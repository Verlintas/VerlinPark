//! Project detection and structured build/test execution.
//!
//! The model should not guess how to build, test, lint, or format a repository.
//! These tools resolve commands from checked-in configuration, run them under
//! the existing shell sandbox, and parse the common failure formats into
//! machine-readable locations.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use ignore::WalkBuilder;
use regex::Regex;
use serde::Serialize;
use serde_json::{Value, json};

use crate::test_cache::{
    TestCacheKeyMaterial, build_impact_map, compute_cache_key, compute_input_fingerprint,
    full_impact_generation, impact_targets_as_affected, incomplete_input_note, profile_hash,
    resolve_external_scope_material, spill_cache_output,
};
use crate::tools::{
    AgentTool, ToolContext, ToolOutput, run_sandboxed_shell_captured, safe_existing_path,
};
use vp_protocol::test_cache::{AffectedTestsMode, TestCacheMode, TestCacheProvenance};

const MAX_SCAN_FILES: usize = 30_000;
const MAX_FAILURES: usize = 60;
const MAX_RAW_TAIL_BYTES: usize = 12_000;
const MAX_AFFECTED_TESTS: usize = 12;

pub struct ProjectProfile;
pub struct RunProjectTask;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CommandCandidate {
    pub command: String,
    pub source: String,
    pub confidence: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LanguageStat {
    pub name: String,
    pub files: usize,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Convention {
    pub path: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CiFile {
    pub path: String,
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct TestLayout {
    pub roots: Vec<String>,
    pub patterns: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProjectProfileData {
    pub root: String,
    pub languages: Vec<LanguageStat>,
    pub package_managers: Vec<String>,
    pub commands: BTreeMap<String, CommandCandidate>,
    pub test_layout: TestLayout,
    pub conventions: Vec<Convention>,
    pub ci: Vec<CiFile>,
    pub warnings: Vec<String>,
    pub truncated: bool,
}

#[async_trait]
impl AgentTool for ProjectProfile {
    fn name(&self) -> &'static str {
        "project_profile"
    }

    fn description(&self) -> &'static str {
        "Detect the project's languages, build/test/lint/format commands, test layout, conventions, and CI files. Use this instead of guessing commands."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Directory relative to the worktree root; default ."},
                "refresh": {"type": "boolean", "description": "Accepted for symmetry; detection is always current"}
            }
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let relative = input.get("path").and_then(Value::as_str).unwrap_or(".");
        let root = safe_existing_path(&context.root, relative)?;
        if !root.is_dir() {
            return Ok(ToolOutput::error_structured(
                "project_profile path must be a directory",
                json!({"code": "INVALID_ARGUMENT", "path": relative}),
            ));
        }
        let profile = detect_profile(&root);
        let languages = if profile.languages.is_empty() {
            "no recognized source files".to_string()
        } else {
            profile
                .languages
                .iter()
                .take(5)
                .map(|language| format!("{} ({} files)", language.name, language.files))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let commands = profile
            .commands
            .iter()
            .map(|(kind, candidate)| {
                format!("{kind}: `{}` ({})", candidate.command, candidate.source)
            })
            .collect::<Vec<_>>()
            .join("\n");
        let text = format!(
            "project profile for {}\nlanguages: {languages}\n{}",
            profile.root,
            if commands.is_empty() {
                "no supported commands detected".to_string()
            } else {
                commands
            }
        );
        let structured = serde_json::to_value(&profile).unwrap_or_else(|_| json!({}));
        Ok(ToolOutput::ok_structured_preserving(
            text, structured, false,
        ))
    }
}

#[async_trait]
impl AgentTool for RunProjectTask {
    fn name(&self) -> &'static str {
        "run_project_task"
    }

    fn description(&self) -> &'static str {
        "Run the project's detected build, test, lint, format, typecheck, or check task under the shell sandbox. Returns parsed failures and a bounded raw tail. Test/lint/check results may be reused from the content-addressed cache when every input matches."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "kind": {"type": "string", "enum": ["build", "test", "lint", "format", "typecheck", "check", "dev", "custom"]},
                "package": {"type": "string", "description": "Package or workspace member to focus, when supported"},
                "filter": {"type": "string", "description": "Native test-name filter, when supported"},
                "paths": {"type": "array", "items": {"type": "string"}, "description": "Changed source paths used to select affected tests"},
                "changed_only": {"type": "boolean", "description": "Select tests affected by current Git changes"},
                "command": {"type": "string", "description": "Explicit command, required only for kind=custom"},
                "mode": {"type": "string", "enum": ["lexical", "impact"], "description": "Affected-test mapping source; default lexical for compatibility"},
                "cache": {"type": "string", "enum": ["auto", "bypass", "refresh"], "description": "auto reuses an exact key, bypass never reads/writes, refresh executes and replaces; default auto"},
                "timeout_secs": {"type": "integer", "minimum": 1, "maximum": 1800, "description": "Default 300"},
                "dry_run": {"type": "boolean", "description": "Resolve and report the command without running it"}
            },
            "required": ["kind"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let kind = input
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("kind is required"))?;
        let allowed = [
            "build",
            "test",
            "lint",
            "format",
            "typecheck",
            "check",
            "dev",
            "custom",
        ];
        if !allowed.contains(&kind) {
            return Ok(ToolOutput::error_structured(
                format!("unsupported task kind: {kind}"),
                json!({"code": "INVALID_ARGUMENT", "kind": kind}),
            ));
        }
        let cache_mode = match TestCacheMode::parse(input.get("cache").and_then(Value::as_str)) {
            Ok(mode) => mode,
            Err(message) => {
                return Ok(ToolOutput::error_structured(
                    message,
                    json!({"code": "INVALID_ARGUMENT", "field": "cache"}),
                ));
            }
        };
        let affected_mode =
            match AffectedTestsMode::parse(input.get("mode").and_then(Value::as_str)) {
                Ok(mode) => mode,
                Err(message) => {
                    return Ok(ToolOutput::error_structured(
                        message,
                        json!({"code": "INVALID_ARGUMENT", "field": "mode"}),
                    ));
                }
            };
        let package = input.get("package").and_then(Value::as_str);
        let filter = input.get("filter").and_then(Value::as_str);
        let dry_run = input
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let changed_only = input
            .get("changed_only")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let timeout_secs = input
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(300)
            .clamp(1, 1_800);
        let profile = detect_profile(&context.root);
        let profile_digest = profile_hash(&profile);
        let mut warnings = Vec::new();
        let mut affected = Vec::new();
        let mut changed_paths = input
            .get("paths")
            .and_then(Value::as_array)
            .map(|paths| {
                paths
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if changed_only && changed_paths.is_empty() {
            changed_paths = git_changed_paths(&context.root);
        }
        changed_paths = changed_paths
            .into_iter()
            .filter_map(|path| crate::test_cache::normalize_changed_path(&path))
            .collect();
        let access = context.test_cache.clone();
        let policy = if cache_mode != TestCacheMode::Bypass {
            match access.as_ref() {
                Some(access) => {
                    let access = access.clone();
                    match tokio::task::spawn_blocking(move || access.policy()).await {
                        Ok(Ok(policy)) => Some(policy),
                        Ok(Err(error)) => {
                            warnings.push(format!(
                                "test-cache policy unavailable; caching disabled: {error}"
                            ));
                            None
                        }
                        Err(error) => {
                            warnings.push(format!(
                                "test-cache policy task failed; caching disabled: {error}"
                            ));
                            None
                        }
                    }
                }
                None => None,
            }
        } else {
            None
        };
        let policy_allows_kind = policy
            .as_ref()
            .is_some_and(|policy| policy.allows_kind(kind));
        let external_scopes = policy
            .as_ref()
            .map(|policy| resolve_external_scope_material(&context.root, policy))
            .unwrap_or_else(crate::test_cache::ExternalScopeMaterial::empty);
        let mut impact_map: Option<vp_protocol::test_cache::TestImpactMap> = None;
        let mut impact_generation_value = full_impact_generation(&profile_digest);
        let mut impact_available = true;
        if !changed_paths.is_empty() {
            let want_impact = affected_mode == AffectedTestsMode::Impact
                || (cache_mode != TestCacheMode::Bypass && policy_allows_kind);
            if want_impact {
                match build_impact_map(
                    context,
                    access.as_deref(),
                    &changed_paths,
                    32,
                    cache_mode != TestCacheMode::Bypass,
                )
                .await
                {
                    Ok(map) => {
                        impact_generation_value = map.generation.clone();
                        if affected_mode == AffectedTestsMode::Impact {
                            affected = impact_targets_as_affected(&map);
                        } else {
                            affected = affected_tests(&context.root, &changed_paths);
                        }
                        impact_map = Some(map);
                    }
                    Err(error) => {
                        if affected_mode == AffectedTestsMode::Impact {
                            return Ok(ToolOutput::error_coded(
                                "run_project_task",
                                crate::tooling::errors::ToolErrorCode::IMPACT_MAP_UNAVAILABLE,
                                format!("impact mapping failed: {error}"),
                            )
                            .with_failure_evidence(format!(
                                "kind={kind}; changed_paths={}",
                                changed_paths.len()
                            )));
                        }
                        warnings.push(format!(
                            "impact mapping failed; lexical mapping was used and caching is disabled: {error}"
                        ));
                        impact_available = false;
                        affected = affected_tests(&context.root, &changed_paths);
                    }
                }
            } else {
                affected = affected_tests(&context.root, &changed_paths);
            }
        }

        let mut resolved = if kind == "custom" {
            let command = input
                .get("command")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|command| !command.is_empty())
                .ok_or_else(|| anyhow!("command is required when kind=custom"))?;
            ResolvedCommand {
                command: command.to_string(),
                source: "explicit".to_string(),
                confidence: 1.0,
                warnings: Vec::new(),
            }
        } else {
            let mut found = profile.commands.get(kind).cloned().or_else(|| {
                if kind == "check" {
                    profile
                        .commands
                        .get("typecheck")
                        .or_else(|| profile.commands.get("lint"))
                        .cloned()
                } else {
                    None
                }
            });
            if found.is_none() && kind == "test" {
                warnings.push(
                    "no test command detected; run project_profile to inspect supported ecosystems"
                        .to_string(),
                );
            }
            let Some(found) = found.take() else {
                return Ok(ToolOutput::error_structured(
                    format!("no {kind} command detected for this project"),
                    json!({
                        "code": "PROJECT_UNSUPPORTED",
                        "kind": kind,
                        "languages": profile.languages.iter().map(|language| language.name.clone()).collect::<Vec<_>>(),
                        "detected_commands": profile.commands.keys().collect::<Vec<_>>(),
                        "warnings": warnings,
                    }),
                ));
            };
            ResolvedCommand {
                command: found.command,
                source: found.source,
                confidence: found.confidence,
                warnings: found.note.into_iter().collect(),
            }
        };

        if let Some(package) = package {
            let (updated, package_warning) = apply_package(&resolved.command, package);
            resolved.command = updated;
            if let Some(warning) = package_warning {
                resolved.warnings.push(warning);
            }
        }
        if let Some(filter) = filter {
            resolved.command = apply_filter(&resolved.command, filter);
        }
        if kind == "test" && !affected.is_empty() {
            let (updated, test_warning) = apply_affected_tests(&resolved.command, &affected);
            resolved.command = updated;
            if let Some(warning) = test_warning {
                resolved.warnings.push(warning);
            }
        } else if changed_only && affected.is_empty() {
            resolved.warnings.push(
                "no affected tests matched the changed paths; the full suite will run".to_string(),
            );
        }
        warnings.extend(resolved.warnings.clone());

        let summary = format!("{kind}: `{}`", resolved.command);
        let mut affected_sources = impact_map
            .as_ref()
            .map(|map| map.sources.clone())
            .unwrap_or_default();
        if affected_sources.is_empty() {
            for test in &affected {
                *affected_sources.entry(test.source.clone()).or_insert(0) += 1;
            }
        }
        if dry_run {
            return Ok(ToolOutput::ok_structured(
                format!(
                    "{summary}\nsource: {}\ncache: dry_run (no lookup, no command)\n(dry run)",
                    resolved.source
                ),
                json!({
                    "ok": true,
                    "kind": kind,
                    "command": resolved.command,
                    "source": resolved.source,
                    "confidence": resolved.confidence,
                    "dry_run": true,
                    "changed_paths": changed_paths,
                    "affected_tests": affected,
                    "affected_sources": affected_sources,
                    "impact_generation": impact_generation_value,
                    "cache_hit": false,
                    "cache": {
                        "decision": "dry_run",
                        "cache_hit": false,
                        "key_version": vp_protocol::test_cache::TEST_CACHE_KEY_VERSION,
                        "note": "dry run: no cache lookup and no command execution"
                    },
                    "warnings": warnings,
                }),
            ));
        }

        let mut cacheable = cache_mode != TestCacheMode::Bypass
            && access.is_some()
            && policy_allows_kind
            && impact_available;
        let mut cache_key: Option<String> = None;
        let mut inputs_hash: Option<String> = None;
        let mut provenance = if cache_mode == TestCacheMode::Bypass {
            TestCacheProvenance::bypass("cache=bypass: no lookup, no write")
        } else if access.is_none() {
            crate::test_cache::unavailable_provenance()
        } else if let Some(policy) = &policy {
            if !policy.enabled {
                TestCacheProvenance {
                    decision: "disabled".to_string(),
                    note: "project test-cache policy is disabled".to_string(),
                    ..TestCacheProvenance::miss(String::new())
                }
            } else if !policy.allows_kind(kind) {
                TestCacheProvenance {
                    decision: "uncacheable".to_string(),
                    note: format!("kind={kind} is not cacheable by policy"),
                    ..TestCacheProvenance::miss(String::new())
                }
            } else if !impact_available {
                TestCacheProvenance {
                    decision: "uncacheable".to_string(),
                    note: "impact generation was unavailable; cache reuse is disabled for this run"
                        .to_string(),
                    ..TestCacheProvenance::miss(String::new())
                }
            } else {
                TestCacheProvenance::miss("no cached result for these exact inputs")
            }
        } else {
            TestCacheProvenance::miss("no test-cache policy is available")
        };
        if cacheable {
            let dependency_paths = impact_map
                .as_ref()
                .map(|map| map.dependency_paths.clone())
                .unwrap_or_default();
            let fingerprint =
                compute_input_fingerprint(&context.root, &changed_paths, &dependency_paths);
            if !fingerprint.complete {
                cacheable = false;
                if let Some(note) = incomplete_input_note(&fingerprint) {
                    warnings.push(note.clone());
                    provenance = TestCacheProvenance {
                        decision: "uncacheable".to_string(),
                        inputs_hash: Some(fingerprint.hash.clone()),
                        impact_generation: Some(impact_generation_value.clone()),
                        note,
                        ..TestCacheProvenance::miss(String::new())
                    };
                }
            } else if !external_scopes.complete {
                cacheable = false;
                let note = external_scopes.notes.first().cloned().unwrap_or_else(|| {
                    "an external cache input is unavailable; reuse is disabled".to_string()
                });
                warnings.extend(external_scopes.notes.iter().cloned());
                provenance = TestCacheProvenance {
                    decision: "uncacheable".to_string(),
                    inputs_hash: Some(fingerprint.hash.clone()),
                    impact_generation: Some(impact_generation_value.clone()),
                    note,
                    ..TestCacheProvenance::miss(String::new())
                };
            } else {
                inputs_hash = Some(fingerprint.hash.clone());
                let material = TestCacheKeyMaterial::resolved(
                    kind,
                    &resolved.command,
                    &profile_digest,
                    &fingerprint.hash,
                    &impact_generation_value,
                )
                .with_external_scopes(&external_scopes);
                cache_key = Some(compute_cache_key(&material));
                provenance.inputs_hash = inputs_hash.clone();
                provenance.impact_generation = Some(impact_generation_value.clone());
                provenance.cache_key = cache_key.clone();
                provenance.key_version = vp_protocol::test_cache::TEST_CACHE_KEY_VERSION;
                provenance.note = format!(
                    "{} external env name(s), {} external path(s) included in the key",
                    external_scopes.env_names, external_scopes.paths
                );
            }
        }

        if cacheable
            && cache_mode == TestCacheMode::Auto
            && let (Some(access), Some(key)) = (access.clone(), cache_key.clone())
        {
            let session_id = context.session_id.clone().unwrap_or_default();
            if session_id.is_empty() {
                cacheable = false;
                provenance = TestCacheProvenance {
                    decision: "uncacheable".to_string(),
                    note: "no session id is attached to this tool context".to_string(),
                    ..TestCacheProvenance::miss(String::new())
                };
            } else {
                let key_for_lookup = key.clone();
                let lookup = tokio::task::spawn_blocking(move || {
                    access.lookup(&key_for_lookup, &session_id, &vp_protocol::now_rfc3339())
                })
                .await;
                match lookup {
                    Ok(Ok(Some(hit))) => {
                        return Ok(cached_task_output(
                            hit,
                            &summary,
                            kind,
                            &resolved.command,
                            &resolved.source,
                            changed_paths,
                            affected,
                            warnings,
                            affected_sources,
                        ));
                    }
                    Ok(Ok(None)) => {
                        provenance = TestCacheProvenance {
                            decision: "miss".to_string(),
                            note: "no cached result for these exact inputs".to_string(),
                            inputs_hash: inputs_hash.clone(),
                            impact_generation: Some(impact_generation_value.clone()),
                            cache_key: cache_key.clone(),
                            key_version: vp_protocol::test_cache::TEST_CACHE_KEY_VERSION,
                            ..TestCacheProvenance::miss(String::new())
                        };
                    }
                    Ok(Err(error)) => {
                        warnings.push(format!(
                            "test-cache lookup failed; the command ran fresh: {error}"
                        ));
                        provenance = TestCacheProvenance {
                            decision: "error".to_string(),
                            cache_hit: false,
                            inputs_hash: inputs_hash.clone(),
                            impact_generation: Some(impact_generation_value.clone()),
                            cache_key: cache_key.clone(),
                            key_version: vp_protocol::test_cache::TEST_CACHE_KEY_VERSION,
                            note: format!("cache lookup failed; no result was reused: {error}"),
                            ..TestCacheProvenance::miss(String::new())
                        };
                    }
                    Err(error) => {
                        warnings.push(format!(
                            "test-cache lookup task failed; the command ran fresh: {error}"
                        ));
                    }
                }
            }
        }

        let captured = match run_sandboxed_shell_captured(
            context,
            &resolved.command,
            Duration::from_secs(timeout_secs),
        )
        .await
        {
            Ok(captured) => captured,
            Err(error) => {
                let code = if error.to_string().contains("timed out") {
                    crate::tooling::errors::ToolErrorCode::COMMAND_TIMEOUT
                } else {
                    crate::tooling::errors::ToolErrorCode::COMMAND_FAILED
                };
                return Ok(ToolOutput::error_coded(
                    "run_project_task",
                    code,
                    format!("{summary}\n{error}"),
                )
                .with_failure_evidence(format!(
                    "kind={kind}; timeout_secs={timeout_secs}; affected_tests={}",
                    affected.len()
                )));
            }
        };
        let combined = if captured.stderr.is_empty() {
            captured.stdout.clone()
        } else if captured.stdout.is_empty() {
            captured.stderr.clone()
        } else {
            format!("{}\n{}", captured.stdout, captured.stderr)
        };
        let (failures, warning_count) = parse_task_output(&combined);
        let ok = captured.exit_code == 0;
        let tail = tail_bytes(&combined, MAX_RAW_TAIL_BYTES);
        let mut next_actions = Vec::new();
        if !ok {
            for failure in failures.iter().take(3) {
                if let (Some(path), Some(line)) = (&failure.path, failure.line) {
                    let offset = line.saturating_sub(10);
                    next_actions.push(format!("read_file path={path} offset={offset} limit=40"));
                }
                if let Some(test) = &failure.test {
                    next_actions.push(format!("rerun run_project_task kind=test filter={test}"));
                }
            }
            if next_actions.is_empty() {
                next_actions
                    .push("inspect the raw_tail and rerun with a narrower filter".to_string());
            }
        } else if kind == "test" {
            next_actions.push("run_project_task kind=lint".to_string());
        } else if kind == "lint" {
            next_actions.push("run_project_task kind=test".to_string());
        }
        let failure_lines = failures
            .iter()
            .take(12)
            .map(|failure| {
                let location = failure
                    .path
                    .as_deref()
                    .map(|path| {
                        format!(
                            "{path}:{}{}",
                            failure.line.unwrap_or(0),
                            failure
                                .column
                                .map(|column| format!(":{column}"))
                                .unwrap_or_default()
                        )
                    })
                    .unwrap_or_else(|| "project".to_string());
                format!(
                    "{}: {}{}",
                    location,
                    failure.message,
                    failure
                        .test
                        .as_ref()
                        .map(|test| format!(" [{test}]"))
                        .unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let text = if ok {
            format!(
                "{summary}\nexit 0 in {}ms{}",
                captured.duration_ms,
                if warning_count > 0 {
                    format!(", {warning_count} warning(s)")
                } else {
                    String::new()
                }
            )
        } else {
            format!(
                "{summary}\nexit {} in {}ms\n{}{}",
                captured.exit_code,
                captured.duration_ms,
                if failure_lines.is_empty() {
                    String::new()
                } else {
                    format!("{failure_lines}\n")
                },
                tail
            )
        };

        if cacheable
            && let (Some(access), Some(policy), Some(key)) =
                (access.clone(), policy.clone(), cache_key.clone())
            && let Some(inputs_hash_value) = inputs_hash.clone()
        {
            let session_id = context.session_id.clone().unwrap_or_default();
            let retention_secs = if ok {
                u64::from(policy.retention_days).saturating_mul(86_400)
            } else {
                policy.failed_retention_secs
            };
            let spill = spill_cache_output(
                &access,
                &session_id,
                &combined,
                policy.max_output_bytes,
                retention_secs,
            );
            if let Some(note) = spill.note.clone() {
                warnings.push(note);
            }
            let mut covered_paths = changed_paths.clone();
            covered_paths.extend(affected.iter().map(|test| test.path.clone()));
            if let Some(map) = &impact_map {
                covered_paths.extend(map.dependency_paths.clone());
                covered_paths.extend(map.targets.iter().map(|target| target.path.clone()));
            }
            covered_paths.sort();
            covered_paths.dedup();
            covered_paths.truncate(crate::test_cache::MAX_COVERED_PATHS);
            let result_json = json!({
                "ok": ok,
                "kind": kind,
                "command": resolved.command,
                "source": resolved.source,
                "exit_code": captured.exit_code,
                "duration_ms": captured.duration_ms,
                "failures": failures,
                "warnings": warnings,
                "warning_count": warning_count,
                "raw_tail": tail,
                "truncated": captured.stdout_truncated || captured.stderr_truncated,
                "changed_paths": changed_paths,
                "affected_tests": affected,
                "next_actions": next_actions,
            });
            let spec = crate::test_cache::TestResultSpec {
                session_id: session_id.clone(),
                kind: normalized_cache_kind(kind).to_string(),
                command: resolved.command.clone(),
                command_source: resolved.source.clone(),
                cache_key: key.clone(),
                inputs_hash: inputs_hash_value.clone(),
                impact_generation: impact_generation_value.clone(),
                status: if ok { "passed" } else { "failed" }.to_string(),
                exit_code: captured.exit_code,
                duration_ms: captured.duration_ms,
                output_ref: spill.output_ref.clone(),
                output_bytes: spill.bytes,
                covered_paths,
                warnings: warnings.clone(),
                warning_count: warning_count as u64,
                failure_count: failures.len() as u64,
                result_json: result_json.clone(),
            };
            let persist = tokio::task::spawn_blocking(move || access.persist(spec, &policy)).await;
            provenance = match persist {
                Ok(Ok(stored)) => TestCacheProvenance {
                    decision: if cache_mode == TestCacheMode::Refresh {
                        "refresh".to_string()
                    } else {
                        "miss".to_string()
                    },
                    cache_hit: false,
                    result_id: Some(stored.id),
                    cache_key: Some(key),
                    key_version: vp_protocol::test_cache::TEST_CACHE_KEY_VERSION,
                    inputs_hash: Some(inputs_hash_value),
                    impact_generation: Some(impact_generation_value.clone()),
                    cached_at: Some(stored.created_at),
                    expires_at: Some(stored.expires_at),
                    origin_session_id: Some(session_id),
                    status: Some(stored.status),
                    note: if cache_mode == TestCacheMode::Refresh {
                        "cache=refresh: command executed and the result replaced the prior entry"
                            .to_string()
                    } else {
                        "command executed; result stored for identical future inputs".to_string()
                    },
                },
                Ok(Err(error)) => {
                    warnings.push(format!("test-cache write failed: {error}"));
                    TestCacheProvenance {
                        decision: "error".to_string(),
                        cache_hit: false,
                        inputs_hash: Some(inputs_hash_value),
                        impact_generation: Some(impact_generation_value.clone()),
                        cache_key: Some(key),
                        key_version: vp_protocol::test_cache::TEST_CACHE_KEY_VERSION,
                        note: format!("cache write failed; the fresh result is returned: {error}"),
                        ..TestCacheProvenance::miss(String::new())
                    }
                }
                Err(error) => {
                    warnings.push(format!("test-cache write task failed: {error}"));
                    TestCacheProvenance {
                        decision: "error".to_string(),
                        cache_hit: false,
                        note: format!("cache write task failed: {error}"),
                        ..TestCacheProvenance::miss(String::new())
                    }
                }
            };
        } else if cache_mode == TestCacheMode::Refresh {
            provenance.decision = "refresh".to_string();
            provenance.note = "cache=refresh: command executed; result was not stored".to_string();
        }

        let structured = json!({
            "ok": ok,
            "kind": kind,
            "command": resolved.command,
            "source": resolved.source,
            "confidence": resolved.confidence,
            "exit_code": captured.exit_code,
            "duration_ms": captured.duration_ms,
            "failures": failures,
            "warnings": warnings,
            "warning_count": warning_count,
            "raw_tail": tail,
            "truncated": captured.stdout_truncated || captured.stderr_truncated,
            "changed_paths": changed_paths,
            "affected_tests": affected,
            "affected_sources": affected_sources,
            "impact_generation": impact_generation_value,
            "inputs_hash": inputs_hash,
            "cache_hit": false,
            "cached_at": Value::Null,
            "cache": provenance,
            "next_actions": next_actions,
        });
        let output = ToolOutput::ok_structured_preserving(text, structured, false);
        if ok {
            Ok(output)
        } else {
            Ok(output.mark_failure(
                "run_project_task",
                crate::tooling::errors::ToolErrorCode::TEST_FAILED,
                format!(
                    "{} task failed with exit code {}; fix the first reported failure and rerun the focused check",
                    kind, captured.exit_code
                ),
            ))
        }
    }
}

fn normalized_cache_kind(kind: &str) -> &str {
    match kind {
        "test" => "test",
        "lint" => "lint",
        _ => "check",
    }
}

fn render_cached_failures(failures: &Value) -> String {
    let Some(failures) = failures.as_array() else {
        return String::new();
    };
    failures
        .iter()
        .take(12)
        .map(|failure| {
            let path = failure
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("project");
            let line = failure.get("line").and_then(Value::as_u64).unwrap_or(0);
            let message = failure
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("failure");
            let test = failure
                .get("test")
                .and_then(Value::as_str)
                .map(|test| format!(" [{test}]"))
                .unwrap_or_default();
            format!("{path}:{line}: {message}{test}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[allow(clippy::too_many_arguments)]
fn cached_task_output(
    hit: crate::test_cache::CachedTestResult,
    summary: &str,
    kind: &str,
    command: &str,
    source: &str,
    changed_paths: Vec<String>,
    affected: Vec<AffectedTest>,
    warnings: Vec<String>,
    affected_sources: BTreeMap<String, u64>,
) -> ToolOutput {
    let ok = hit.entry.status == "passed";
    let failures = hit
        .result
        .get("failures")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let mut warnings = warnings;
    if let Some(stored_warnings) = hit.result.get("warnings").and_then(Value::as_array) {
        for warning in stored_warnings.iter().filter_map(Value::as_str) {
            if warnings.len() >= 32 {
                break;
            }
            if !warnings.iter().any(|existing| existing == warning) {
                warnings.push(warning.to_string());
            }
        }
    }
    for warning in &hit.entry.warnings {
        if warnings.len() >= 32 {
            break;
        }
        if !warnings.iter().any(|existing| existing == warning) {
            warnings.push(warning.clone());
        }
    }
    let warning_count = hit
        .result
        .get("warning_count")
        .and_then(Value::as_u64)
        .unwrap_or(hit.entry.warning_count);
    let raw_tail = hit
        .result
        .get("raw_tail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let failure_lines = render_cached_failures(&failures);
    let cached_note = format!(
        "cache hit: no command ran; cached at {} ({}); inputs {}; impact {}",
        hit.entry.created_at,
        if ok { "pass" } else { "failure" },
        hit.entry.inputs_hash,
        hit.entry.impact_generation
    );
    let text = if ok {
        format!(
            "{summary}\n{cached_note}\nexit 0 in {}ms{}",
            hit.entry.duration_ms,
            if warning_count > 0 {
                format!(", {warning_count} warning(s)")
            } else {
                String::new()
            }
        )
    } else {
        format!(
            "{summary}\n{cached_note}\nexit {} in {}ms\n{}{}",
            hit.entry.exit_code,
            hit.entry.duration_ms,
            if failure_lines.is_empty() {
                String::new()
            } else {
                format!("{failure_lines}\n")
            },
            raw_tail
        )
    };
    let exit_code = hit.entry.exit_code;
    let duration_ms = hit.entry.duration_ms;
    let cached_at = hit.entry.created_at.clone();
    let inputs_hash = hit.entry.inputs_hash.clone();
    let impact_generation = hit.entry.impact_generation.clone();
    let result_id = hit.entry.id.clone();
    let status = hit.entry.status.clone();
    let provenance = hit.provenance;
    let structured = json!({
        "ok": ok,
        "kind": kind,
        "command": command,
        "source": source,
        "exit_code": exit_code,
        "duration_ms": duration_ms,
        "failures": failures,
        "warnings": warnings,
        "warning_count": warning_count,
        "raw_tail": raw_tail,
        "truncated": hit.result.get("truncated").and_then(Value::as_bool).unwrap_or(false),
        "changed_paths": changed_paths,
        "affected_tests": affected,
        "affected_sources": affected_sources,
        "impact_generation": impact_generation,
        "inputs_hash": inputs_hash,
        "cache_hit": true,
        "cached_at": cached_at,
        "cache_result_id": result_id,
        "cache_status": status,
        "cache": provenance,
        "next_actions": hit.result.get("next_actions").cloned().unwrap_or_else(|| json!([])),
    });
    let output = ToolOutput::ok_structured_preserving(text, structured, false);
    if ok {
        output
    } else {
        output.mark_failure(
            "run_project_task",
            if status == "error" {
                crate::tooling::errors::ToolErrorCode::COMMAND_FAILED
            } else {
                crate::tooling::errors::ToolErrorCode::TEST_FAILED
            },
            "a cached failure was reused because every input matches; no command ran and no pass is claimed",
        )
    }
}

struct ResolvedCommand {
    command: String,
    source: String,
    confidence: f64,
    warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct DetectedTaskRun {
    pub command: String,
    pub source: String,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub output: String,
    pub failures: Vec<Failure>,
    pub warning_count: usize,
}

/// Run a detected project task for internal tools such as diagnostics. This
/// shares the same resolution, sandbox, timeout, and parser as
/// `run_project_task`; it only omits the agent-facing presentation.
pub(crate) async fn run_detected_task(
    context: &ToolContext,
    kind: &str,
    timeout_secs: u64,
) -> Result<Option<DetectedTaskRun>> {
    let profile = detect_profile(&context.root);
    let Some(candidate) = profile
        .commands
        .get(kind)
        .or_else(|| {
            (kind == "check")
                .then(|| profile.commands.get("typecheck"))
                .flatten()
        })
        .or_else(|| {
            (kind == "check")
                .then(|| profile.commands.get("lint"))
                .flatten()
        })
        .cloned()
    else {
        return Ok(None);
    };
    let captured = run_sandboxed_shell_captured(
        context,
        &candidate.command,
        Duration::from_secs(timeout_secs.clamp(1, 1_800)),
    )
    .await?;
    let output = if captured.stderr.is_empty() {
        captured.stdout.clone()
    } else if captured.stdout.is_empty() {
        captured.stderr.clone()
    } else {
        format!("{}\n{}", captured.stdout, captured.stderr)
    };
    let (failures, warning_count) = parse_task_output(&output);
    Ok(Some(DetectedTaskRun {
        command: candidate.command,
        source: candidate.source,
        exit_code: captured.exit_code,
        duration_ms: captured.duration_ms,
        output,
        failures,
        warning_count,
    }))
}

pub(crate) fn detect_profile(root: &Path) -> ProjectProfileData {
    let mut warnings = Vec::new();
    let (languages, mut test_roots, truncated) = scan_languages(root);
    let mut commands = BTreeMap::new();
    let mut package_managers = BTreeSet::new();

    if let Some(justfile) = find_justfile(root) {
        let recipes = parse_justfile(&justfile);
        for (recipe, kind) in [
            ("build", "build"),
            ("test", "test"),
            ("lint", "lint"),
            ("clippy", "lint"),
            ("fmt", "format"),
            ("format", "format"),
            ("check", "check"),
            ("typecheck", "typecheck"),
            ("types", "typecheck"),
            ("dev", "dev"),
            ("serve", "dev"),
        ] {
            if recipes.contains(recipe) {
                insert_command(
                    &mut commands,
                    kind,
                    CommandCandidate {
                        command: format!("just {recipe}"),
                        source: "justfile".to_string(),
                        confidence: 0.99,
                        note: None,
                    },
                );
            }
        }
    }

    if let Some(makefile) = find_named_file(root, &["Makefile", "makefile", "GNUmakefile"]) {
        let targets = parse_makefile(&makefile);
        for (target, kind) in [
            ("build", "build"),
            ("test", "test"),
            ("lint", "lint"),
            ("fmt", "format"),
            ("format", "format"),
            ("check", "check"),
        ] {
            if targets.contains(target) {
                insert_command(
                    &mut commands,
                    kind,
                    CommandCandidate {
                        command: format!("make {target}"),
                        source: "Makefile".to_string(),
                        confidence: 0.8,
                        note: None,
                    },
                );
            }
        }
    }

    if let Some(cargo) = read_text(root.join("Cargo.toml")) {
        package_managers.insert("cargo".to_string());
        let workspace = cargo.lines().any(|line| line.trim() == "[workspace]");
        let suffix = if workspace { " --workspace" } else { "" };
        insert_command(
            &mut commands,
            "build",
            CommandCandidate {
                command: format!("cargo build{suffix}"),
                source: "Cargo.toml".to_string(),
                confidence: 0.95,
                note: None,
            },
        );
        insert_command(
            &mut commands,
            "test",
            CommandCandidate {
                command: format!("cargo test{suffix}"),
                source: "Cargo.toml".to_string(),
                confidence: 0.95,
                note: None,
            },
        );
        insert_command(
            &mut commands,
            "lint",
            CommandCandidate {
                command: if workspace {
                    "cargo clippy --workspace --all-targets -- -D warnings".to_string()
                } else {
                    "cargo clippy --all-targets -- -D warnings".to_string()
                },
                source: "Cargo.toml".to_string(),
                confidence: 0.9,
                note: None,
            },
        );
        insert_command(
            &mut commands,
            "format",
            CommandCandidate {
                command: "cargo fmt --all".to_string(),
                source: "Cargo.toml".to_string(),
                confidence: 0.95,
                note: None,
            },
        );
        insert_command(
            &mut commands,
            "check",
            CommandCandidate {
                command: format!("cargo check{suffix}"),
                source: "Cargo.toml".to_string(),
                confidence: 0.9,
                note: None,
            },
        );
        if workspace {
            test_roots.push("crates".to_string());
        }
        test_roots.push("tests".to_string());
    }

    if let Some(package) = read_text(root.join("package.json"))
        && let Ok(parsed) = serde_json::from_str::<Value>(&package)
    {
        let package_manager = detect_node_package_manager(root);
        package_managers.insert(package_manager.clone());
        let scripts = parsed.get("scripts").and_then(Value::as_object);
        if let Some(scripts) = scripts {
            for (script, kind) in [
                ("build", "build"),
                ("test", "test"),
                ("lint", "lint"),
                ("format", "format"),
                ("fmt", "format"),
                ("typecheck", "typecheck"),
                ("check", "check"),
                ("dev", "dev"),
                ("start", "dev"),
            ] {
                if scripts.contains_key(script) {
                    insert_command(
                        &mut commands,
                        kind,
                        CommandCandidate {
                            command: format!(
                                "{} {}",
                                package_manager_command(&package_manager),
                                script
                            ),
                            source: "package.json".to_string(),
                            confidence: 0.95,
                            note: None,
                        },
                    );
                }
            }
        }
        test_roots.push("tests".to_string());
        test_roots.push("__tests__".to_string());
    }

    if root.join("pyproject.toml").exists()
        || root.join("requirements.txt").exists()
        || root.join("setup.py").exists()
        || root.join("tox.ini").exists()
        || root.join("pytest.ini").exists()
    {
        let pyproject = read_text(root.join("pyproject.toml")).unwrap_or_default();
        let package_manager = if root.join("uv.lock").exists() {
            "uv"
        } else if root.join("poetry.lock").exists() {
            "poetry"
        } else {
            "pip"
        };
        package_managers.insert(package_manager.to_string());
        let prefix = match package_manager {
            "uv" => "uv run ",
            "poetry" => "poetry run ",
            _ => "",
        };
        if root.join("tests").exists()
            || root.join("pytest.ini").exists()
            || root.join("tox.ini").exists()
            || pyproject.contains("pytest")
        {
            insert_command(
                &mut commands,
                "test",
                CommandCandidate {
                    command: format!("{prefix}python -m pytest"),
                    source: "python project".to_string(),
                    confidence: 0.85,
                    note: None,
                },
            );
        }
        if pyproject.contains("[tool.ruff") || pyproject.contains("ruff") {
            insert_command(
                &mut commands,
                "lint",
                CommandCandidate {
                    command: format!("{prefix}ruff check ."),
                    source: "pyproject.toml".to_string(),
                    confidence: 0.9,
                    note: None,
                },
            );
            insert_command(
                &mut commands,
                "format",
                CommandCandidate {
                    command: format!("{prefix}ruff format ."),
                    source: "pyproject.toml".to_string(),
                    confidence: 0.85,
                    note: None,
                },
            );
        }
        if pyproject.contains("mypy") {
            insert_command(
                &mut commands,
                "typecheck",
                CommandCandidate {
                    command: format!("{prefix}mypy ."),
                    source: "pyproject.toml".to_string(),
                    confidence: 0.85,
                    note: None,
                },
            );
        }
        test_roots.push("tests".to_string());
    }

    if root.join("go.mod").exists() {
        package_managers.insert("go".to_string());
        insert_command(
            &mut commands,
            "build",
            CommandCandidate {
                command: "go build ./...".to_string(),
                source: "go.mod".to_string(),
                confidence: 0.98,
                note: None,
            },
        );
        insert_command(
            &mut commands,
            "test",
            CommandCandidate {
                command: "go test ./...".to_string(),
                source: "go.mod".to_string(),
                confidence: 0.98,
                note: None,
            },
        );
        insert_command(
            &mut commands,
            "lint",
            CommandCandidate {
                command: "go vet ./...".to_string(),
                source: "go.mod".to_string(),
                confidence: 0.9,
                note: None,
            },
        );
        insert_command(
            &mut commands,
            "format",
            CommandCandidate {
                command: "gofmt -w .".to_string(),
                source: "go.mod".to_string(),
                confidence: 0.9,
                note: None,
            },
        );
    }

    if root.join("Package.swift").exists() {
        package_managers.insert("swift".to_string());
        insert_command(
            &mut commands,
            "build",
            CommandCandidate {
                command: "swift build".to_string(),
                source: "Package.swift".to_string(),
                confidence: 0.95,
                note: None,
            },
        );
        insert_command(
            &mut commands,
            "test",
            CommandCandidate {
                command: "swift test".to_string(),
                source: "Package.swift".to_string(),
                confidence: 0.95,
                note: None,
            },
        );
        test_roots.push("Tests".to_string());
    }

    if let Some(xcode_project) = find_xcode_project(root) {
        package_managers.insert("xcodebuild".to_string());
        let (flag, path) = if xcode_project
            .extension()
            .is_some_and(|ext| ext == "xcworkspace")
        {
            ("-workspace", xcode_project)
        } else {
            ("-project", xcode_project)
        };
        let scheme = xcode_scheme(root, &path);
        let scheme_flag = scheme
            .as_ref()
            .map(|scheme| format!(" -scheme {scheme}"))
            .unwrap_or_default();
        let base = format!(
            "xcodebuild {flag} {} {}",
            shell_quote(&path.to_string_lossy()),
            scheme_flag.trim()
        );
        insert_command(
            &mut commands,
            "build",
            CommandCandidate {
                command: format!("{base} build").replace("  ", " "),
                source: path.to_string_lossy().to_string(),
                confidence: if scheme.is_some() { 0.85 } else { 0.55 },
                note: if scheme.is_some() {
                    None
                } else {
                    Some("no shared scheme detected; pass a scheme explicitly".to_string())
                },
            },
        );
        insert_command(
            &mut commands,
            "test",
            CommandCandidate {
                command: format!("{base} test").replace("  ", " "),
                source: path.to_string_lossy().to_string(),
                confidence: if scheme.is_some() { 0.85 } else { 0.55 },
                note: if scheme.is_some() {
                    None
                } else {
                    Some("no shared scheme detected; pass a scheme explicitly".to_string())
                },
            },
        );
        test_roots.push("Tests".to_string());
    }

    if root.join("CMakeLists.txt").exists() {
        package_managers.insert("cmake".to_string());
        insert_command(
            &mut commands,
            "build",
            CommandCandidate {
                command: "cmake -S . -B build && cmake --build build".to_string(),
                source: "CMakeLists.txt".to_string(),
                confidence: 0.9,
                note: None,
            },
        );
        insert_command(
            &mut commands,
            "test",
            CommandCandidate {
                command: "ctest --test-dir build --output-on-failure".to_string(),
                source: "CMakeLists.txt".to_string(),
                confidence: 0.85,
                note: None,
            },
        );
    }

    if root.join("gradlew").exists() {
        package_managers.insert("gradle".to_string());
        for (gradle_kind, kind) in [("build", "build"), ("test", "test"), ("check", "lint")] {
            insert_command(
                &mut commands,
                kind,
                CommandCandidate {
                    command: format!("./gradlew {gradle_kind}"),
                    source: "gradlew".to_string(),
                    confidence: 0.9,
                    note: None,
                },
            );
        }
    }
    if root.join("pom.xml").exists() {
        package_managers.insert("maven".to_string());
        insert_command(
            &mut commands,
            "build",
            CommandCandidate {
                command: "mvn -q package".to_string(),
                source: "pom.xml".to_string(),
                confidence: 0.85,
                note: None,
            },
        );
        insert_command(
            &mut commands,
            "test",
            CommandCandidate {
                command: "mvn -q test".to_string(),
                source: "pom.xml".to_string(),
                confidence: 0.9,
                note: None,
            },
        );
    }

    let test_layout = TestLayout {
        roots: dedupe(test_roots),
        patterns: test_patterns(&languages),
    };
    let conventions = conventions(root);
    let ci = ci_files(root);
    if commands.is_empty() && !languages.is_empty() {
        warnings.push(
            "source files were detected but no supported build/test command was found".to_string(),
        );
    }
    if truncated {
        warnings.push(format!(
            "source scan stopped after {MAX_SCAN_FILES} files; counts are lower bounds"
        ));
    }
    ProjectProfileData {
        root: root.to_string_lossy().to_string(),
        languages,
        package_managers: package_managers.into_iter().collect(),
        commands,
        test_layout,
        conventions,
        ci,
        warnings,
        truncated,
    }
}

fn insert_command(
    commands: &mut BTreeMap<String, CommandCandidate>,
    kind: &str,
    candidate: CommandCandidate,
) {
    match commands.get(kind) {
        Some(existing) if existing.confidence >= candidate.confidence => {}
        _ => {
            commands.insert(kind.to_string(), candidate);
        }
    }
}

fn scan_languages(root: &Path) -> (Vec<LanguageStat>, Vec<String>, bool) {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut test_dirs = BTreeSet::new();
    let mut total = 0usize;
    let mut truncated = false;
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .max_depth(Some(10))
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            !matches!(
                name.as_ref(),
                ".git"
                    | "target"
                    | "node_modules"
                    | "dist"
                    | "build"
                    | ".build"
                    | "DerivedData"
                    | "Pods"
                    | "vendor"
                    | ".venv"
                    | "venv"
            )
        });
    for entry in builder.build().flatten() {
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let path = entry.path();
        if let Some(language) = language_for(path) {
            *counts.entry(language).or_default() += 1;
            total += 1;
            if total >= MAX_SCAN_FILES {
                truncated = true;
                break;
            }
            if let Ok(relative) = path.strip_prefix(root) {
                for component in relative.components() {
                    let name = component.as_os_str().to_string_lossy();
                    if matches!(
                        name.as_ref(),
                        "tests" | "test" | "spec" | "__tests__" | "Tests"
                    ) {
                        test_dirs.insert(name.to_string());
                    }
                }
            }
        }
    }
    let mut languages = counts
        .into_iter()
        .map(|(name, files)| LanguageStat {
            name: name.to_string(),
            files,
            confidence: if total == 0 {
                0.0
            } else {
                (files as f64 / total as f64 * 100.0).round() / 100.0
            },
        })
        .collect::<Vec<_>>();
    languages.sort_by(|left, right| {
        right
            .files
            .cmp(&left.files)
            .then_with(|| left.name.cmp(&right.name))
    });
    (languages, test_dirs.into_iter().collect(), truncated)
}

fn language_for(path: &Path) -> Option<&'static str> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "rs" => Some("Rust"),
        "swift" => Some("Swift"),
        "ts" | "tsx" | "mts" | "cts" => Some("TypeScript"),
        "js" | "jsx" | "mjs" | "cjs" => Some("JavaScript"),
        "py" | "pyi" => Some("Python"),
        "go" => Some("Go"),
        "java" => Some("Java"),
        "kt" | "kts" => Some("Kotlin"),
        "c" | "h" => Some("C"),
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => Some("C++"),
        "cs" => Some("C#"),
        "rb" => Some("Ruby"),
        "php" => Some("PHP"),
        "sh" | "bash" | "zsh" => Some("Shell"),
        "sql" => Some("SQL"),
        "lua" => Some("Lua"),
        "dart" => Some("Dart"),
        "ex" | "exs" => Some("Elixir"),
        "hs" => Some("Haskell"),
        "scala" => Some("Scala"),
        _ => None,
    }
}

fn test_patterns(languages: &[LanguageStat]) -> Vec<String> {
    let mut patterns = Vec::new();
    for language in languages {
        match language.name.as_str() {
            "Rust" => patterns.extend(["crates/*/tests/*.rs", "tests/*.rs"].map(str::to_string)),
            "Swift" => patterns.push("Tests/**/*.swift".to_string()),
            "TypeScript" => patterns.push("**/*.{test,spec}.{ts,tsx}".to_string()),
            "JavaScript" => patterns.push("**/*.{test,spec}.{js,jsx}".to_string()),
            "Python" => patterns.push("tests/test_*.py".to_string()),
            "Go" => patterns.push("*_test.go".to_string()),
            "Java" | "Kotlin" => {
                patterns.push("src/test/**/*.{java,kt}".to_string());
            }
            _ => {}
        }
    }
    dedupe(patterns)
}

fn conventions(root: &Path) -> Vec<Convention> {
    let mut result = Vec::new();
    for name in [
        "AGENTS.md",
        "CONTRIBUTING.md",
        "README.md",
        "README",
        ".editorconfig",
    ] {
        let path = root.join(name);
        let Some(text) = read_text(&path) else {
            continue;
        };
        let summary = text
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or(name)
            .trim_start_matches('#')
            .trim_start_matches("//")
            .trim()
            .chars()
            .take(160)
            .collect::<String>();
        result.push(Convention {
            path: name.to_string(),
            summary,
        });
    }
    result
}

fn ci_files(root: &Path) -> Vec<CiFile> {
    let mut paths = Vec::new();
    let workflows = root.join(".github/workflows");
    if let Ok(entries) = std::fs::read_dir(&workflows) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| matches!(ext, "yml" | "yaml"))
            {
                paths.push(path);
            }
        }
    }
    for name in [
        ".gitlab-ci.yml",
        "azure-pipelines.yml",
        ".circleci/config.yml",
        "Jenkinsfile",
    ] {
        let path = root.join(name);
        if path.exists() {
            paths.push(path);
        }
    }
    paths.sort();
    paths
        .into_iter()
        .take(8)
        .filter_map(|path| {
            let text = read_text(&path)?;
            let mut commands = Vec::new();
            for line in text.lines() {
                let trimmed = line.trim();
                let command = trimmed
                    .strip_prefix("- run:")
                    .or_else(|| trimmed.strip_prefix("run:"))
                    .map(str::trim)
                    .filter(|command| !command.is_empty());
                if let Some(command) = command {
                    commands.push(command.chars().take(300).collect::<String>());
                    if commands.len() >= 20 {
                        break;
                    }
                }
            }
            Some(CiFile {
                path: path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string(),
                commands,
            })
        })
        .collect()
}

fn find_justfile(root: &Path) -> Option<PathBuf> {
    find_named_file(root, &["justfile", "Justfile", ".justfile"])
}

fn find_named_file(root: &Path, names: &[&str]) -> Option<PathBuf> {
    names
        .iter()
        .map(|name| root.join(name))
        .find(|path| path.is_file())
}

fn read_text(path: impl AsRef<Path>) -> Option<String> {
    let path = path.as_ref();
    let metadata = std::fs::metadata(path).ok()?;
    if metadata.len() > 2 * 1024 * 1024 {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

fn parse_justfile(path: &Path) -> BTreeSet<String> {
    let mut recipes = BTreeSet::new();
    let Some(text) = read_text(path) else {
        return recipes;
    };
    for line in text.lines() {
        if line.starts_with(char::is_whitespace) || line.trim_start().starts_with('#') {
            continue;
        }
        let Some((header, _)) = line.split_once(':') else {
            continue;
        };
        if header.contains('=') || header.trim().is_empty() {
            continue;
        }
        let name = header
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .trim_start_matches('@');
        if !name.is_empty()
            && name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
        {
            recipes.insert(name.to_string());
        }
    }
    recipes
}

fn parse_makefile(path: &Path) -> BTreeSet<String> {
    let mut targets = BTreeSet::new();
    let Some(text) = read_text(path) else {
        return targets;
    };
    for line in text.lines() {
        if line.starts_with(char::is_whitespace) || line.trim_start().starts_with('#') {
            continue;
        }
        let Some((header, _)) = line.split_once(':') else {
            continue;
        };
        if header.contains('=') {
            continue;
        }
        for target in header.split_whitespace() {
            if !target.is_empty()
                && target
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
            {
                targets.insert(target.to_string());
            }
        }
    }
    targets
}

fn detect_node_package_manager(root: &Path) -> String {
    if root.join("pnpm-lock.yaml").exists() {
        "pnpm".to_string()
    } else if root.join("yarn.lock").exists() {
        "yarn".to_string()
    } else if root.join("bun.lockb").exists() || root.join("bun.lock").exists() {
        "bun".to_string()
    } else {
        "npm".to_string()
    }
}

fn package_manager_command(package_manager: &str) -> &'static str {
    match package_manager {
        "pnpm" => "pnpm",
        "yarn" => "yarn",
        "bun" => "bun run",
        _ => "npm run",
    }
}

fn find_xcode_project(root: &Path) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| matches!(ext, "xcodeproj" | "xcworkspace"))
            {
                candidates.push(path);
            }
        }
    }
    candidates.sort();
    candidates
        .into_iter()
        .find(|path| path.extension().is_some_and(|ext| ext == "xcworkspace"))
        .or_else(|| {
            let mut candidates = Vec::new();
            if let Ok(entries) = std::fs::read_dir(root) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|ext| ext == "xcodeproj") {
                        candidates.push(path);
                    }
                }
            }
            candidates.sort();
            candidates.into_iter().next()
        })
}

fn xcode_scheme(root: &Path, project: &Path) -> Option<String> {
    let mut schemes = Vec::new();
    if project.extension().is_some_and(|ext| ext == "xcodeproj") {
        let directory = project.join("xcshareddata/xcschemes");
        if let Ok(entries) = std::fs::read_dir(&directory) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "xcscheme")
                    && let Some(stem) = path.file_stem()
                {
                    schemes.push(stem.to_string_lossy().to_string());
                }
            }
        }
    }
    schemes.sort();
    if let Some(scheme) = schemes.into_iter().next() {
        return Some(scheme);
    }
    let project_yml = root.join("project.yml");
    let text = read_text(project_yml)?;
    text.lines()
        .find_map(|line| line.trim().strip_prefix("name:"))
        .map(|name| name.trim().trim_matches('"').trim_matches('\'').to_string())
}

fn dedupe(values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | ':'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn apply_package(command: &str, package: &str) -> (String, Option<String>) {
    if command.starts_with("cargo ") {
        let mut parts = command.splitn(2, ' ');
        let cargo = parts.next().unwrap_or("cargo");
        let rest = parts.next().unwrap_or_default();
        let mut rest_parts = rest.splitn(2, ' ');
        let subcommand = rest_parts.next().unwrap_or_default();
        let arguments = rest_parts.next().unwrap_or_default();
        (
            format!(
                "{cargo} {subcommand} -p {} {arguments}",
                shell_quote(package)
            )
            .trim()
            .to_string(),
            None,
        )
    } else {
        (
            command.to_string(),
            Some(format!(
                "package selection is only applied to cargo commands; ignored package={package}"
            )),
        )
    }
}

fn apply_filter(command: &str, filter: &str) -> String {
    if command.starts_with("cargo test") || command.starts_with("just test") {
        format!("{command} {}", shell_quote(filter))
    } else if command.contains("python -m pytest") || command.ends_with("pytest") {
        format!("{command} -k {}", shell_quote(filter))
    } else if command.starts_with("go test") {
        format!("{command} -run {}", shell_quote(filter))
    } else if command.starts_with("swift test") {
        format!("{command} --filter {}", shell_quote(filter))
    } else if command.starts_with("xcodebuild") {
        format!("{command} -only-testing:{}", shell_quote(filter))
    } else {
        format!("{command} {}", shell_quote(filter))
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AffectedTest {
    pub path: String,
    pub reason: String,
    pub confidence: f64,
    /// `profile`, `lexical`, `lsp`, or `history`. Legacy lexical results label
    /// their source instead of implying completeness.
    #[serde(default)]
    pub source: String,
}

pub(crate) fn affected_tests(root: &Path, changed: &[String]) -> Vec<AffectedTest> {
    let mut found: Vec<AffectedTest> = Vec::new();
    for path in changed {
        let path = path.trim_start_matches("./");
        let candidate_paths = affected_candidates(root, path);
        for (candidate, confidence) in candidate_paths {
            if found.iter().any(|entry| entry.path == candidate) {
                continue;
            }
            if root.join(&candidate).exists() {
                found.push(AffectedTest {
                    path: candidate,
                    reason: format!("derived from {path}"),
                    confidence,
                    source: "profile".to_string(),
                });
            }
            if found.len() >= MAX_AFFECTED_TESTS {
                return found;
            }
        }
    }
    found
}

/// Profile-convention candidates that actually exist in the worktree. The
/// impact map labels these `profile` and does not execute anything.
pub(crate) fn profile_affected_candidates(root: &Path, path: &str) -> Vec<(String, f64)> {
    affected_candidates(root, path)
        .into_iter()
        .filter(|(candidate, _)| root.join(candidate).exists())
        .collect()
}

fn affected_candidates(root: &Path, path: &str) -> Vec<(String, f64)> {
    let parsed = Path::new(path);
    let extension = parsed
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let stem = parsed
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let mut result = Vec::new();
    if stem.contains("test") || stem.contains("spec") || path.contains("/tests/") {
        result.push((path.to_string(), 0.99));
    }
    match extension.as_str() {
        "rs" => {
            let components = parsed.components().collect::<Vec<_>>();
            if let Some(position) = components
                .iter()
                .position(|component| component.as_os_str().to_string_lossy() == "crates")
            {
                let members = components
                    .iter()
                    .skip(position + 1)
                    .map(|component| component.as_os_str().to_string_lossy().to_string())
                    .collect::<Vec<_>>();
                if members.len() >= 3 {
                    let crate_name = &members[0];
                    let tests_dir = root.join("crates").join(crate_name).join("tests");
                    if tests_dir.is_dir()
                        && let Ok(entries) = std::fs::read_dir(tests_dir)
                    {
                        for entry in entries.flatten() {
                            let candidate = entry.path();
                            if candidate.extension().is_some_and(|ext| ext == "rs")
                                && let Some(candidate_stem) = candidate.file_stem()
                            {
                                let candidate_stem = candidate_stem.to_string_lossy();
                                let confidence = if candidate_stem == stem { 0.9 } else { 0.5 };
                                result.push((
                                    candidate
                                        .strip_prefix(root)
                                        .unwrap_or(&candidate)
                                        .to_string_lossy()
                                        .to_string(),
                                    confidence,
                                ));
                            }
                        }
                    }
                }
            }
        }
        "py" => {
            let parent = parsed.parent().unwrap_or_else(|| Path::new(""));
            let candidates = [
                parent.join("tests").join(format!("test_{stem}.py")),
                PathBuf::from("tests").join(format!("test_{stem}.py")),
                parent.join(format!("test_{stem}.py")),
            ];
            for candidate in candidates {
                result.push((candidate.to_string_lossy().to_string(), 0.85));
            }
        }
        "go" => {
            let parent = parsed.parent().unwrap_or_else(|| Path::new(""));
            if let Ok(entries) = std::fs::read_dir(root.join(parent)) {
                for entry in entries.flatten() {
                    let candidate = entry.path();
                    if candidate
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.ends_with("_test.go"))
                    {
                        result.push((
                            candidate
                                .strip_prefix(root)
                                .unwrap_or(&candidate)
                                .to_string_lossy()
                                .to_string(),
                            0.75,
                        ));
                    }
                }
            }
        }
        "swift" => {
            let parent = parsed
                .parent()
                .and_then(|parent| parent.file_name())
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default();
            for candidate in [
                format!("Tests/{parent}Tests/{stem}Tests.swift"),
                format!("Tests/{parent}Tests/{stem}Test.swift"),
            ] {
                result.push((candidate, 0.8));
            }
        }
        "ts" | "tsx" | "js" | "jsx" => {
            let parent = parsed.parent().unwrap_or_else(|| Path::new(""));
            for candidate in [
                parent.join(format!("{stem}.test.{extension}")),
                parent.join(format!("{stem}.spec.{extension}")),
                parent
                    .join("__tests__")
                    .join(format!("{stem}.test.{extension}")),
                PathBuf::from("tests").join(format!("{stem}.test.{extension}")),
            ] {
                result.push((candidate.to_string_lossy().to_string(), 0.75));
            }
        }
        _ => {}
    }
    result
}

fn apply_affected_tests(command: &str, affected: &[AffectedTest]) -> (String, Option<String>) {
    if affected.is_empty() {
        return (command.to_string(), None);
    }
    if command.starts_with("cargo test") {
        let targets = affected
            .iter()
            .filter_map(|test| {
                let path = Path::new(&test.path);
                if path
                    .components()
                    .any(|component| component.as_os_str().to_string_lossy() == "tests")
                {
                    path.file_stem()
                        .map(|stem| stem.to_string_lossy().to_string())
                } else {
                    None
                }
            })
            .collect::<BTreeSet<_>>();
        if !targets.is_empty() {
            let flags = targets
                .iter()
                .take(6)
                .map(|target| format!("--test {}", shell_quote(target)))
                .collect::<Vec<_>>()
                .join(" ");
            return (format!("{command} {flags}"), None);
        }
    }
    if command.contains("pytest") {
        let paths = affected
            .iter()
            .map(|test| shell_quote(&test.path))
            .collect::<Vec<_>>()
            .join(" ");
        return (format!("{command} {paths}"), None);
    }
    if command.starts_with("go test") {
        let packages = affected
            .iter()
            .filter_map(|test| {
                Path::new(&test.path)
                    .parent()
                    .map(|parent| format!("./{}", parent.to_string_lossy()))
            })
            .collect::<BTreeSet<_>>();
        if !packages.is_empty() {
            let packages = packages
                .iter()
                .take(6)
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            return (format!("{command} {packages}"), None);
        }
    }
    if command.starts_with("swift test") {
        let filters = affected
            .iter()
            .filter_map(|test| {
                Path::new(&test.path)
                    .file_stem()
                    .map(|stem| format!("--filter {}", shell_quote(&stem.to_string_lossy())))
            })
            .collect::<Vec<_>>()
            .join(" ");
        if !filters.is_empty() {
            return (format!("{command} {filters}"), None);
        }
    }
    (
        command.to_string(),
        Some("affected tests were found, but this command cannot select them; the full suite will run".to_string()),
    )
}

pub(crate) fn git_changed_paths(root: &Path) -> Vec<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain=v1", "-z"])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut paths = Vec::new();
    let mut entries = text.split('\0').filter(|entry| !entry.is_empty());
    while let Some(entry) = entries.next() {
        if entry.len() < 4 {
            continue;
        }
        let status = &entry[..2];
        let path = entry[3..].to_string();
        if status.contains('R') || status.contains('C') {
            // The rename source follows in the next NUL-separated field.
            let _ = entries.next();
        }
        paths.push(path);
        if paths.len() >= 500 {
            break;
        }
    }
    paths
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Failure {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
    pub severity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test: Option<String>,
}

pub(crate) fn parse_task_output(output: &str) -> (Vec<Failure>, usize) {
    let compiler =
        Regex::new(r"^(.+?):(\d+):(?:(\d+):)?\s*(error|warning|note|fatal error):\s*(.+)$")
            .expect("compiler diagnostic regex is valid");
    let cargo_test = Regex::new(r"^test (.+) \.\.\. FAILED$").expect("cargo test regex is valid");
    let panic = Regex::new(r"panicked at (.+?):(\d+):(\d+)").expect("panic regex is valid");
    let pytest = Regex::new(r"^FAILED (.+?)::(.+?)(?: - (.+))?$").expect("pytest regex is valid");
    let go_fail = Regex::new(r"^--- FAIL: (\S+)").expect("go test regex is valid");
    let go_location =
        Regex::new(r"^\s*(.+?\.go):(\d+):\s*(.+)$").expect("go location regex is valid");
    let jest_failure = Regex::new(r"^FAIL (.+)$").expect("jest failure regex is valid");
    let jest_case = Regex::new(r"^●\s+(.+)$").expect("jest case regex is valid");
    let swift_case = Regex::new(r"^Test Case '(.+)' failed").expect("swift test regex is valid");
    let simple_diagnostic = Regex::new(r"^(error|warning)(?:\[([A-Za-z0-9_]+)\])?:\s*(.+)$")
        .expect("simple diagnostic regex is valid");
    let arrow = Regex::new(r"^\s*-->\s+(.+?):(\d+):(?:(\d+):)?").expect("arrow regex is valid");
    let mut failures = Vec::new();
    let mut warnings = 0usize;
    let mut current_test: Option<String> = None;
    let mut pending: Option<(String, Option<String>, String)> = None;
    for line in output.lines() {
        let trimmed = line.trim_end();
        if let Some(captures) = arrow.captures(trimmed)
            && let Some((severity, code, message)) = pending.take()
        {
            push_failure(
                &mut failures,
                Failure {
                    path: captures.get(1).map(|value| value.as_str().to_string()),
                    line: captures
                        .get(2)
                        .and_then(|value| value.as_str().parse().ok()),
                    column: captures
                        .get(3)
                        .and_then(|value| value.as_str().parse().ok()),
                    severity,
                    code,
                    message,
                    test: current_test.clone(),
                },
            );
            continue;
        }
        if let Some(captures) = simple_diagnostic.captures(trimmed) {
            let severity = captures
                .get(1)
                .map(|value| value.as_str().to_string())
                .unwrap_or_else(|| "error".to_string());
            if severity == "warning" {
                warnings = warnings.saturating_add(1);
            } else {
                pending = Some((
                    severity,
                    captures.get(2).map(|value| value.as_str().to_string()),
                    captures
                        .get(3)
                        .map(|value| value.as_str().chars().take(500).collect())
                        .unwrap_or_default(),
                ));
            }
            continue;
        }
        if compiler.is_match(trimmed) || trimmed.starts_with("warning:") {
            warnings = warnings.saturating_add(usize::from(
                trimmed.contains("warning:") || trimmed.contains(" warning:"),
            ));
        }
        if let Some(captures) = compiler.captures(trimmed) {
            let severity = captures
                .get(4)
                .map(|value| value.as_str())
                .unwrap_or("error");
            if severity == "warning" {
                continue;
            }
            failures.push(Failure {
                path: captures.get(1).map(|value| value.as_str().to_string()),
                line: captures
                    .get(2)
                    .and_then(|value| value.as_str().parse().ok()),
                column: captures
                    .get(3)
                    .and_then(|value| value.as_str().parse().ok()),
                severity: severity.to_string(),
                code: None,
                message: captures
                    .get(5)
                    .map(|value| value.as_str().chars().take(500).collect())
                    .unwrap_or_default(),
                test: current_test.clone(),
            });
            continue;
        }
        if let Some(captures) = cargo_test.captures(trimmed) {
            let test = captures
                .get(1)
                .map(|value| value.as_str().to_string())
                .unwrap_or_default();
            current_test = Some(test.clone());
            push_failure(
                &mut failures,
                Failure {
                    path: None,
                    line: None,
                    column: None,
                    severity: "error".to_string(),
                    code: None,
                    message: "test failed".to_string(),
                    test: Some(test),
                },
            );
            continue;
        }
        if let Some(captures) = panic.captures(trimmed) {
            push_failure(
                &mut failures,
                Failure {
                    path: captures.get(1).map(|value| value.as_str().to_string()),
                    line: captures
                        .get(2)
                        .and_then(|value| value.as_str().parse().ok()),
                    column: captures
                        .get(3)
                        .and_then(|value| value.as_str().parse().ok()),
                    severity: "error".to_string(),
                    code: None,
                    message: "panic".to_string(),
                    test: current_test.clone(),
                },
            );
            continue;
        }
        if let Some(captures) = pytest.captures(trimmed) {
            push_failure(
                &mut failures,
                Failure {
                    path: captures.get(1).map(|value| value.as_str().to_string()),
                    line: None,
                    column: None,
                    severity: "error".to_string(),
                    code: None,
                    message: captures
                        .get(3)
                        .map(|value| value.as_str().chars().take(500).collect())
                        .unwrap_or_else(|| "test failed".to_string()),
                    test: captures.get(2).map(|value| value.as_str().to_string()),
                },
            );
            continue;
        }
        if let Some(captures) = go_fail.captures(trimmed) {
            let test = captures
                .get(1)
                .map(|value| value.as_str().to_string())
                .unwrap_or_default();
            current_test = Some(test.clone());
            push_failure(
                &mut failures,
                Failure {
                    path: None,
                    line: None,
                    column: None,
                    severity: "error".to_string(),
                    code: None,
                    message: "test failed".to_string(),
                    test: Some(test),
                },
            );
            continue;
        }
        if let Some(captures) = go_location.captures(trimmed) {
            push_failure(
                &mut failures,
                Failure {
                    path: captures.get(1).map(|value| value.as_str().to_string()),
                    line: captures
                        .get(2)
                        .and_then(|value| value.as_str().parse().ok()),
                    column: None,
                    severity: "error".to_string(),
                    code: None,
                    message: captures
                        .get(3)
                        .map(|value| value.as_str().chars().take(500).collect())
                        .unwrap_or_default(),
                    test: current_test.clone(),
                },
            );
            continue;
        }
        if let Some(captures) = jest_failure.captures(trimmed) {
            current_test = None;
            push_failure(
                &mut failures,
                Failure {
                    path: captures.get(1).map(|value| value.as_str().to_string()),
                    line: None,
                    column: None,
                    severity: "error".to_string(),
                    code: None,
                    message: "test file failed".to_string(),
                    test: None,
                },
            );
            continue;
        }
        if let Some(captures) = jest_case.captures(trimmed) {
            let test: String = captures
                .get(1)
                .map(|value| value.as_str().chars().take(300).collect())
                .unwrap_or_default();
            current_test = Some(test.clone());
            push_failure(
                &mut failures,
                Failure {
                    path: None,
                    line: None,
                    column: None,
                    severity: "error".to_string(),
                    code: None,
                    message: "test failed".to_string(),
                    test: Some(test),
                },
            );
            continue;
        }
        if let Some(captures) = swift_case.captures(trimmed) {
            let test = captures
                .get(1)
                .map(|value| value.as_str().to_string())
                .unwrap_or_default();
            current_test = Some(test.clone());
            push_failure(
                &mut failures,
                Failure {
                    path: None,
                    line: None,
                    column: None,
                    severity: "error".to_string(),
                    code: None,
                    message: "test case failed".to_string(),
                    test: Some(test),
                },
            );
        }
    }
    (failures, warnings)
}

fn push_failure(failures: &mut Vec<Failure>, failure: Failure) {
    if failures.len() >= MAX_FAILURES {
        return;
    }
    let duplicate = failures.iter().any(|existing| {
        existing.path == failure.path
            && existing.line == failure.line
            && existing.message == failure.message
            && existing.test == failure.test
    });
    if !duplicate {
        failures.push(failure);
    }
}

fn tail_bytes(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut start = text.len().saturating_sub(limit);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    let boundary = text[start..]
        .find('\n')
        .map(|offset| start + offset + 1)
        .unwrap_or(start);
    format!("… (earlier output omitted)\n{}", &text[boundary..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolContext;
    use serde_json::json;
    use tempfile::TempDir;

    fn context(root: &Path) -> ToolContext {
        ToolContext {
            root: root.to_path_buf(),
            project_root: root.to_path_buf(),
            sandbox_root: root.join(".sandbox"),
            secret_roots: Vec::new(),
            network_allowed: false,
            home: root.join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
            remote: None,
            session_id: None,
            process_supervisor: None,
            test_cache: None,
        }
    }

    #[test]
    fn parses_cargo_test_failures() {
        let output = "\
running 2 tests
test a ... ok
test b ... FAILED

failures:

---- b stdout ----
thread 'b' panicked at src/lib.rs:12:3:
assertion failed
error[E0425]: cannot find value `x` in this scope
 --> src/main.rs:7:9
";
        let (failures, _) = parse_task_output(output);
        assert!(
            failures
                .iter()
                .any(|failure| failure.test.as_deref() == Some("b"))
        );
        assert!(failures.iter().any(
            |failure| failure.path.as_deref() == Some("src/main.rs") && failure.line == Some(7)
        ));
    }

    #[test]
    fn parses_pytest_and_go_failures() {
        let output = "FAILED tests/test_a.py::test_one - AssertionError: nope\n\
--- FAIL: TestTwo (0.00s)\n    foo_test.go:14: expected true\n";
        let (failures, _) = parse_task_output(output);
        assert!(
            failures
                .iter()
                .any(|failure| failure.path.as_deref() == Some("tests/test_a.py"))
        );
        assert!(
            failures
                .iter()
                .any(|failure| failure.path.as_deref() == Some("foo_test.go")
                    && failure.line == Some(14))
        );
    }

    #[tokio::test]
    async fn detects_rust_project_commands() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/a\"]\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("crates/a/src")).unwrap();
        std::fs::write(dir.path().join("crates/a/src/lib.rs"), "pub fn a() {}").unwrap();
        let output = ProjectProfile
            .execute(&context(dir.path()), json!({}))
            .await
            .unwrap();
        assert!(output.content.contains("cargo test --workspace"));
        assert!(!output.is_error);
    }

    #[tokio::test]
    async fn detects_justfile_and_node_projects() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/a\"]\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("justfile"),
            "build:\n    echo build\ntest:\n    echo test\nlint:\n    echo lint\nfmt:\n    echo fmt\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("crates/a/src")).unwrap();
        std::fs::write(dir.path().join("crates/a/src/lib.rs"), "pub fn a() {}").unwrap();
        let profile = detect_profile(dir.path());
        assert_eq!(profile.commands["test"].command, "just test");
        assert_eq!(profile.commands["build"].command, "just build");
        assert_eq!(profile.commands["format"].command, "just fmt");

        let node = TempDir::new().unwrap();
        std::fs::write(
            node.path().join("package.json"),
            r#"{"scripts":{"test":"vitest run","build":"vite build","lint":"eslint ."}}"#,
        )
        .unwrap();
        std::fs::write(node.path().join("pnpm-lock.yaml"), "").unwrap();
        std::fs::create_dir_all(node.path().join("src")).unwrap();
        std::fs::write(node.path().join("src/a.ts"), "export const a = 1;\n").unwrap();
        let profile = detect_profile(node.path());
        assert_eq!(profile.commands["test"].command, "pnpm test");
        assert_eq!(profile.commands["build"].command, "pnpm build");
    }

    #[tokio::test]
    async fn detects_python_and_go_projects() {
        let python = TempDir::new().unwrap();
        std::fs::write(
            python.path().join("pyproject.toml"),
            "[tool.pytest.ini_options]\n[tool.ruff]\nline-length = 100\n[tool.mypy]\nstrict = true\n",
        )
        .unwrap();
        std::fs::create_dir_all(python.path().join("tests")).unwrap();
        std::fs::write(
            python.path().join("tests/test_a.py"),
            "def test_a():\n    pass\n",
        )
        .unwrap();
        let profile = detect_profile(python.path());
        assert_eq!(profile.commands["test"].command, "python -m pytest");
        assert_eq!(profile.commands["lint"].command, "ruff check .");
        assert_eq!(profile.commands["typecheck"].command, "mypy .");

        let go = TempDir::new().unwrap();
        std::fs::write(go.path().join("go.mod"), "module example.com/x\n").unwrap();
        std::fs::write(go.path().join("x.go"), "package x\n").unwrap();
        let profile = detect_profile(go.path());
        assert_eq!(profile.commands["test"].command, "go test ./...");
        assert_eq!(profile.commands["build"].command, "go build ./...");
    }

    #[tokio::test]
    async fn dry_run_resolves_the_detected_command() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn a() {}").unwrap();
        let output = RunProjectTask
            .execute(
                &context(dir.path()),
                json!({"kind": "test", "dry_run": true}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let structured = output.structured.unwrap();
        assert_eq!(structured["command"], "cargo test");
        assert!(output.content.contains("cargo test"));
    }

    #[tokio::test]
    async fn runs_custom_commands_under_the_sandbox() {
        let dir = TempDir::new().unwrap();
        let output = RunProjectTask
            .execute(
                &context(dir.path()),
                json!({"kind": "custom", "command": "printf 'hello\n'"}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        assert!(output.content.contains("hello"));
        assert_eq!(output.structured.unwrap()["exit_code"], 0);
    }

    #[test]
    fn maps_changed_paths_to_affected_tests() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("crates/a/src")).unwrap();
        std::fs::create_dir_all(dir.path().join("crates/a/tests")).unwrap();
        std::fs::write(dir.path().join("crates/a/src/lib.rs"), "pub fn a() {}").unwrap();
        std::fs::write(
            dir.path().join("crates/a/tests/smoke.rs"),
            "#[test] fn t() {}\n",
        )
        .unwrap();
        let tests = affected_tests(dir.path(), &["crates/a/src/lib.rs".to_string()]);
        assert!(
            tests
                .iter()
                .any(|test| test.path == "crates/a/tests/smoke.rs")
        );

        std::fs::create_dir_all(dir.path().join("tests")).unwrap();
        std::fs::write(dir.path().join("pkg.py"), "x = 1\n").unwrap();
        std::fs::write(
            dir.path().join("tests/test_pkg.py"),
            "def test_pkg(): pass\n",
        )
        .unwrap();
        let tests = affected_tests(dir.path(), &["pkg.py".to_string()]);
        assert!(tests.iter().any(|test| test.path == "tests/test_pkg.py"));

        std::fs::write(dir.path().join("main.go"), "package main\n").unwrap();
        std::fs::write(
            dir.path().join("main_test.go"),
            "package main\nimport \"testing\"\nfunc TestMain(t *testing.T) {}\n",
        )
        .unwrap();
        let tests = affected_tests(dir.path(), &["main.go".to_string()]);
        assert!(tests.iter().any(|test| test.path == "main_test.go"));
    }

    #[test]
    fn parses_swift_and_compile_diagnostics() {
        let output = "Sources/A/Foo.swift:12:7: error: cannot find 'bar' in scope\n\
Test Case 'ATests.testFoo' failed (0.01 seconds).\n";
        let (failures, _) = parse_task_output(output);
        assert!(failures.iter().any(|failure| failure.path.as_deref()
            == Some("Sources/A/Foo.swift")
            && failure.line == Some(12)));
        assert!(
            failures
                .iter()
                .any(|failure| failure.test.as_deref() == Some("ATests.testFoo"))
        );
    }

    #[tokio::test]
    async fn run_project_task_reuses_an_exact_cache_hit_and_honors_refresh_and_bypass() {
        use std::sync::Arc;
        use vp_store::Store;

        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let worktree = TempDir::new().unwrap();
        let project = store
            .add_project(worktree.path().to_str().unwrap(), None)
            .unwrap();
        let session = store.create_session(&project.id, "cache test").unwrap();
        std::fs::write(
            worktree.path().join("Makefile"),
            "test:\n\t@echo x >> runs.log\n",
        )
        .unwrap();
        std::fs::write(worktree.path().join(".gitignore"), "runs.log\n").unwrap();
        let access = Arc::new(crate::test_cache::TestCacheAccess::new(
            store.clone(),
            project.id.clone(),
        ));
        let context = ToolContext {
            root: worktree.path().to_path_buf(),
            project_root: worktree.path().to_path_buf(),
            sandbox_root: data.path().join("sandbox"),
            secret_roots: Vec::new(),
            network_allowed: false,
            home: worktree.path().join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
            remote: None,
            session_id: Some(session.id.clone()),
            process_supervisor: None,
            test_cache: Some(access),
        };
        let first = RunProjectTask
            .execute(&context, json!({"kind": "test"}))
            .await
            .unwrap();
        assert!(!first.is_error, "{}", first.content);
        let first_structured = first.structured.unwrap();
        assert_eq!(first_structured["cache_hit"], false);
        assert_eq!(
            std::fs::read_to_string(worktree.path().join("runs.log"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        let second = RunProjectTask
            .execute(&context, json!({"kind": "test"}))
            .await
            .unwrap();
        assert!(!second.is_error, "{}", second.content);
        let second_structured = second.structured.unwrap();
        assert_eq!(second_structured["cache_hit"], true);
        assert_eq!(second_structured["cache"]["decision"], "hit");
        assert!(second.content.contains("no command ran"));
        assert_eq!(
            second_structured["cache_status"], "passed",
            "a cached pass must be labeled, not inferred"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.path().join("runs.log"))
                .unwrap()
                .lines()
                .count(),
            1,
            "a cache hit must not execute the command"
        );
        let hit_stats = store
            .test_cache_stats(&project.id, &vp_protocol::now_rfc3339())
            .unwrap();
        assert_eq!(hit_stats.entries, 1);
        assert_eq!(hit_stats.total_hits, 1);
        let refreshed = RunProjectTask
            .execute(&context, json!({"kind": "test", "cache": "refresh"}))
            .await
            .unwrap();
        assert!(!refreshed.is_error, "{}", refreshed.content);
        assert_eq!(
            refreshed.structured.unwrap()["cache"]["decision"],
            "refresh"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.path().join("runs.log"))
                .unwrap()
                .lines()
                .count(),
            2
        );
        let bypassed = RunProjectTask
            .execute(&context, json!({"kind": "test", "cache": "bypass"}))
            .await
            .unwrap();
        assert!(!bypassed.is_error, "{}", bypassed.content);
        assert_eq!(bypassed.structured.unwrap()["cache"]["decision"], "bypass");
        assert_eq!(
            std::fs::read_to_string(worktree.path().join("runs.log"))
                .unwrap()
                .lines()
                .count(),
            3
        );
        let stats = store
            .test_cache_stats(&project.id, &vp_protocol::now_rfc3339())
            .unwrap();
        assert_eq!(stats.entries, 1);
    }

    #[tokio::test]
    async fn run_project_task_never_returns_a_pass_for_changed_inputs() {
        use std::sync::Arc;
        use vp_store::Store;

        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let worktree = TempDir::new().unwrap();
        let project = store
            .add_project(worktree.path().to_str().unwrap(), None)
            .unwrap();
        let session = store
            .create_session(&project.id, "cache invalidation")
            .unwrap();
        std::fs::write(
            worktree.path().join("Makefile"),
            "test:\n\t@test ! -f fail\n",
        )
        .unwrap();
        let access = Arc::new(crate::test_cache::TestCacheAccess::new(
            store.clone(),
            project.id.clone(),
        ));
        let context = ToolContext {
            root: worktree.path().to_path_buf(),
            project_root: worktree.path().to_path_buf(),
            sandbox_root: data.path().join("sandbox"),
            secret_roots: Vec::new(),
            network_allowed: false,
            home: worktree.path().join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
            remote: None,
            session_id: Some(session.id.clone()),
            process_supervisor: None,
            test_cache: Some(access),
        };
        let first = RunProjectTask
            .execute(&context, json!({"kind": "test"}))
            .await
            .unwrap();
        assert!(!first.is_error, "{}", first.content);
        std::fs::write(worktree.path().join("fail"), "").unwrap();
        let second = RunProjectTask
            .execute(&context, json!({"kind": "test"}))
            .await
            .unwrap();
        assert!(second.is_error, "{}", second.content);
        let structured = second.structured.unwrap();
        assert_eq!(structured["cache_hit"], false);
        assert_eq!(structured["cache"]["decision"], "miss");
        assert!(
            structured["inputs_hash"]
                .as_str()
                .is_some_and(|hash| !hash.is_empty())
        );
    }

    #[tokio::test]
    async fn cached_failure_is_returned_as_a_failure_and_never_cached_as_a_pass() {
        use std::sync::Arc;
        use vp_store::Store;

        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let worktree = TempDir::new().unwrap();
        let project = store
            .add_project(worktree.path().to_str().unwrap(), None)
            .unwrap();
        let session = store.create_session(&project.id, "cached failure").unwrap();
        std::fs::write(worktree.path().join("Makefile"), "test:\n\t@test -f pass\n").unwrap();
        let context = ToolContext {
            root: worktree.path().to_path_buf(),
            project_root: worktree.path().to_path_buf(),
            sandbox_root: data.path().join("sandbox"),
            secret_roots: Vec::new(),
            network_allowed: false,
            home: worktree.path().join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
            remote: None,
            session_id: Some(session.id.clone()),
            process_supervisor: None,
            test_cache: Some(Arc::new(crate::test_cache::TestCacheAccess::new(
                store.clone(),
                project.id.clone(),
            ))),
        };
        let first = RunProjectTask
            .execute(&context, json!({"kind": "test"}))
            .await
            .unwrap();
        assert!(first.is_error, "{}", first.content);
        assert_eq!(first.structured.unwrap()["cache_hit"], false);
        let cached = RunProjectTask
            .execute(&context, json!({"kind": "test"}))
            .await
            .unwrap();
        assert!(cached.is_error, "{}", cached.content);
        let structured = cached.structured.unwrap();
        assert_eq!(structured["cache_hit"], true);
        assert_eq!(structured["cache_status"], "failed");
        assert_eq!(structured["ok"], false);
        assert!(cached.content.contains("no command ran"));
        // A changed input must miss and execute again instead of reusing the
        // cached failure as if it were a pass.
        std::fs::write(worktree.path().join("pass"), "").unwrap();
        let fixed = RunProjectTask
            .execute(&context, json!({"kind": "test"}))
            .await
            .unwrap();
        assert!(!fixed.is_error, "{}", fixed.content);
        assert_eq!(fixed.structured.unwrap()["cache_hit"], false);
    }

    #[tokio::test]
    async fn dry_run_in_impact_mode_labels_sources_without_executing() {
        use std::sync::Arc;
        use vp_store::Store;

        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let worktree = TempDir::new().unwrap();
        let project = store
            .add_project(worktree.path().to_str().unwrap(), None)
            .unwrap();
        let session = store.create_session(&project.id, "impact dry run").unwrap();
        std::fs::write(worktree.path().join("pyproject.toml"), "[tool.pytest]\n").unwrap();
        std::fs::create_dir_all(worktree.path().join("tests")).unwrap();
        std::fs::write(worktree.path().join("pkg.py"), "def a():\n    return 1\n").unwrap();
        std::fs::write(
            worktree.path().join("tests/test_pkg.py"),
            "from pkg import a\n\ndef test_a():\n    assert a() == 1\n",
        )
        .unwrap();
        let context = ToolContext {
            root: worktree.path().to_path_buf(),
            project_root: worktree.path().to_path_buf(),
            sandbox_root: data.path().join("sandbox"),
            secret_roots: Vec::new(),
            network_allowed: false,
            home: worktree.path().join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
            remote: None,
            session_id: Some(session.id.clone()),
            process_supervisor: None,
            test_cache: Some(Arc::new(crate::test_cache::TestCacheAccess::new(
                store.clone(),
                project.id.clone(),
            ))),
        };
        let output = RunProjectTask
            .execute(
                &context,
                json!({
                    "kind": "test",
                    "paths": ["pkg.py"],
                    "mode": "impact",
                    "dry_run": true
                }),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let structured = output.structured.unwrap();
        assert_eq!(structured["cache"]["decision"], "dry_run");
        assert_eq!(structured["cache_hit"], false);
        assert!(
            structured["impact_generation"]
                .as_str()
                .is_some_and(|generation| generation.starts_with("impact-v1:"))
        );
        let affected = structured["affected_tests"].as_array().unwrap();
        assert!(!affected.is_empty());
        assert!(affected.iter().all(|test| {
            test["source"]
                .as_str()
                .is_some_and(|source| matches!(source, "profile" | "lexical" | "lsp" | "history"))
        }));
        assert_eq!(
            store
                .test_cache_stats(&project.id, &vp_protocol::now_rfc3339())
                .unwrap()
                .entries,
            0,
            "a dry run must never write a cache row"
        );
    }

    #[tokio::test]
    async fn applies_unified_patch_and_preserves_crlf() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\r\ntwo\r\nthree\r\n").unwrap();
        let patch = "\
--- a/a.txt
+++ b/a.txt
@@ -1,3 +1,3 @@
 one
-two
+TWO
 three
";
        let output = crate::tooling::files::ApplyPatch
            .execute(
                &context(dir.path()),
                json!({"patch": patch, "dry_run": false}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let text = std::fs::read_to_string(dir.path().join("a.txt")).unwrap();
        assert_eq!(text, "one\r\nTWO\r\nthree\r\n");
    }

    #[tokio::test]
    async fn run_project_task_external_scopes_invalidate_and_fail_closed() {
        use std::sync::Arc;
        use vp_store::Store;

        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let worktree = TempDir::new().unwrap();
        let project = store
            .add_project(worktree.path().to_str().unwrap(), None)
            .unwrap();
        let session = store
            .create_session(&project.id, "external scope cache")
            .unwrap();
        std::fs::write(
            worktree.path().join("Makefile"),
            "test:\n\t@echo x >> runs.log\n",
        )
        .unwrap();
        std::fs::write(worktree.path().join(".gitignore"), "runs.log\nlocal.env\n").unwrap();
        std::fs::write(worktree.path().join("local.env"), "MODE=one\n").unwrap();
        let env_name = format!("VP_RUN_PROJECT_SCOPE_{}", std::process::id());
        unsafe { std::env::set_var(&env_name, "first") };
        store
            .set_test_cache_policy(
                &project.id,
                &vp_protocol::test_cache::TestCachePolicy {
                    enabled: true,
                    extra_env: vec![env_name.clone()],
                    extra_paths: vec!["local.env".into()],
                    ..vp_protocol::test_cache::TestCachePolicy::default()
                },
            )
            .unwrap();
        let access = Arc::new(crate::test_cache::TestCacheAccess::new(
            store.clone(),
            project.id.clone(),
        ));
        let context = ToolContext {
            root: worktree.path().to_path_buf(),
            project_root: worktree.path().to_path_buf(),
            sandbox_root: data.path().join("sandbox"),
            secret_roots: Vec::new(),
            network_allowed: false,
            home: worktree.path().join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
            remote: None,
            session_id: Some(session.id.clone()),
            process_supervisor: None,
            test_cache: Some(access),
        };
        let run = |context: ToolContext| async move {
            RunProjectTask
                .execute(&context, json!({"kind": "test"}))
                .await
                .unwrap()
        };
        let first = run(context.clone()).await;
        assert_eq!(first.structured.as_ref().unwrap()["cache_hit"], false);
        let second = run(context.clone()).await;
        assert_eq!(second.structured.as_ref().unwrap()["cache_hit"], true);
        assert_eq!(
            std::fs::read_to_string(worktree.path().join("runs.log"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        std::fs::write(worktree.path().join("local.env"), "MODE=two\n").unwrap();
        let path_changed = run(context.clone()).await;
        assert_eq!(
            path_changed.structured.as_ref().unwrap()["cache_hit"],
            false,
            "{}",
            path_changed.content
        );
        assert_eq!(
            std::fs::read_to_string(worktree.path().join("runs.log"))
                .unwrap()
                .lines()
                .count(),
            2
        );
        unsafe { std::env::set_var(&env_name, "second") };
        let env_changed = run(context.clone()).await;
        assert_eq!(env_changed.structured.as_ref().unwrap()["cache_hit"], false);
        assert_eq!(
            std::fs::read_to_string(worktree.path().join("runs.log"))
                .unwrap()
                .lines()
                .count(),
            3
        );
        std::fs::remove_file(worktree.path().join("local.env")).unwrap();
        let missing = run(context.clone()).await;
        assert_eq!(missing.structured.as_ref().unwrap()["cache_hit"], false);
        assert_eq!(
            missing.structured.as_ref().unwrap()["cache"]["decision"],
            "uncacheable"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.path().join("runs.log"))
                .unwrap()
                .lines()
                .count(),
            4
        );
        unsafe { std::env::remove_var(&env_name) };
    }
}
