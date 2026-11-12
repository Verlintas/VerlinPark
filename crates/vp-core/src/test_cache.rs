//! Deterministic test-result cache and changed-path impact mapping (ADR-0178).
//!
//! The cache is deliberately conservative:
//!
//! - the key is content-addressed over command, resolved argv, working
//!   directory, a normalized environment subset, project profile, tool version,
//!   relevant file content, dependency closure, and impact generation;
//! - a commit hash alone never identifies reusable inputs, because uncommitted
//!   edits and ignored build/config files can change a result;
//! - an input hash that cannot be completed (too many files/bytes, unreadable
//!   file) disables reuse instead of risking a false pass;
//! - failed results use a short TTL and are always returned as failures;
//! - output bodies go through the existing content-addressed tool-output store;
//!   metadata rows never inline an unbounded command log;
//! - the deterministic Integrator never uses the cache unless a project policy
//!   explicitly sets `include_integration`.
//!
//! The impact map is a deterministic heuristic: profile conventions, the
//! lexical reverse-import closure, references from an already-running language
//! server, and historical cache coverage. Every target is labeled with its
//! source and confidence. No test is executed during mapping and no result
//! claims completeness.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ignore::WalkBuilder;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use vp_protocol::test_cache::{
    HARD_MAX_IMPACT_TARGETS, IMPACT_SOURCE_HISTORY, IMPACT_SOURCE_LEXICAL, IMPACT_SOURCE_LSP,
    IMPACT_SOURCE_PROFILE, MAX_TEST_CACHE_EXTRA_ENV_VALUE_BYTES, MAX_TEST_CACHE_EXTRA_PATH_BYTES,
    MAX_TEST_CACHE_EXTRA_TOTAL_BYTES, TEST_CACHE_KEY_VERSION, TestCacheEntry, TestCachePolicy,
    TestCacheProvenance, TestCacheStats, TestImpactMap, TestImpactTarget,
};
use vp_store::Store;
use vp_store::store::TestCacheSweep;

use crate::tool_output::ToolOutputStore;
use crate::tooling::errors::ToolErrorCode;
use crate::tooling::project::{ProjectProfileData, detect_profile};
use crate::tools::ToolContext;

/// Stable prefix for every cache key. It is versioned independently of the
/// crate version; changing key semantics must bump both this and
/// `TEST_CACHE_KEY_VERSION`.
pub const TEST_CACHE_KEY_PREFIX: &str = "vp-test-cache-v1";
/// Tool/runtime version folded into every key. It changes when the executor,
/// parser, or key model changes, invalidating old results safely.
pub fn test_cache_tool_version() -> &'static str {
    concat!("vp-core/", env!("CARGO_PKG_VERSION"), "/test-cache/", "1")
}

/// Hard bounds for deterministic input hashing. Reaching any bound disables
/// reuse rather than producing a key that might miss a changed input.
const MAX_INPUT_FILES: usize = 30_000;
const MAX_INPUT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_SINGLE_INPUT_BYTES: u64 = 8 * 1024 * 1024;
/// Bound for dependency-closure hashing and display.
const MAX_DEPENDENCY_PATHS: usize = 300;
/// Bound for historical rows inspected by one impact map.
const MAX_HISTORY_ROWS: usize = 500;
/// Bound for covered paths stored on one result.
pub const MAX_COVERED_PATHS: usize = 500;
/// Bound for the number of warnings stored on one result.
const MAX_STORED_WARNINGS: usize = 32;
/// The environment subset that can change a build/test result. Secrets are
/// deliberately excluded: only a digest of these values is persisted.
const CACHE_ENV_KEYS: &[&str] = &[
    "CI",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    "PATH",
    "CC",
    "CXX",
    "CFLAGS",
    "CXXFLAGS",
    "LDFLAGS",
    "CPPFLAGS",
    "PKG_CONFIG_PATH",
    "MACOSX_DEPLOYMENT_TARGET",
    "SDKROOT",
    "DEVELOPER_DIR",
    "RUSTFLAGS",
    "RUSTDOCFLAGS",
    "RUSTUP_TOOLCHAIN",
    "CARGO_BUILD_TARGET",
    "CARGO_TARGET_DIR",
    "CARGO_INCREMENTAL",
    "CARGO_PROFILE",
    "NODE_ENV",
    "NODE_OPTIONS",
    "PYTHONPATH",
    "PYTHONHASHSEED",
    "VIRTUAL_ENV",
    "CONDA_PREFIX",
    "GOROOT",
    "GOPATH",
    "GOFLAGS",
    "JAVA_HOME",
    "GRADLE_OPTS",
    "MAVEN_OPTS",
    "HOMEBREW_PREFIX",
];

/// Project/session-scoped cache façade used by tool execution. It owns only a
/// cloneable store handle and the project id; the session id is supplied per
/// operation so the same access can serve Leader/Worker/Labour contexts.
#[derive(Clone)]
pub struct TestCacheAccess {
    store: Store,
    project_id: String,
}

impl fmt::Debug for TestCacheAccess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TestCacheAccess")
            .field("project_id", &self.project_id)
            .finish_non_exhaustive()
    }
}

impl TestCacheAccess {
    pub fn new(store: Store, project_id: impl Into<String>) -> Self {
        Self {
            store,
            project_id: project_id.into(),
        }
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn policy(&self) -> Result<TestCachePolicy> {
        self.store.get_test_cache_policy(&self.project_id)
    }

    /// Look up one exact key. Expired rows are swept and never returned. A
    /// successful lookup records a `test_cache.hit` audit event and increments
    /// the owner row's hit counter.
    pub fn lookup(
        &self,
        cache_key: &str,
        requesting_session_id: &str,
        now: &str,
    ) -> Result<Option<CachedTestResult>> {
        if !is_cache_key(cache_key) {
            bail!(
                "{}: invalid test-cache key",
                ToolErrorCode::TEST_CACHE_KEY_INVALID
            );
        }
        let Some(stored) = self
            .store
            .find_test_result_by_key(&self.project_id, cache_key)?
        else {
            return Ok(None);
        };
        if stored.entry.key_version != TEST_CACHE_KEY_VERSION || stored.entry.is_expired_at(now) {
            let _ = self
                .store
                .prune_test_results(&self.project_id, None, true, None, false, now);
            return Ok(None);
        }
        if !matches!(stored.entry.status.as_str(), "passed" | "failed" | "error") {
            bail!(
                "{}: test-cache row has an unknown status",
                ToolErrorCode::TEST_CACHE_CONFLICT
            );
        }
        let _ = self
            .store
            .record_test_result_hit(&stored.entry.id, requesting_session_id, now)?;
        let mut entry = stored.entry;
        entry.hit_count = entry.hit_count.saturating_add(1);
        entry.last_hit_at = Some(now.to_string());
        let result =
            serde_json::from_str::<Value>(&stored.result_json).unwrap_or_else(|_| json!({}));
        let provenance = hit_provenance(&entry);
        Ok(Some(CachedTestResult {
            entry,
            result,
            provenance,
        }))
    }

    /// Persist one executed result. The output body is expected to be spilled
    /// already; this only writes bounded metadata and emits `test_cache.stored`.
    pub fn persist(
        &self,
        spec: TestResultSpec,
        policy: &TestCachePolicy,
    ) -> Result<TestCacheEntry> {
        let now = vp_protocol::now_rfc3339();
        let retention_secs = if spec.status == "passed" {
            u64::from(policy.retention_days).saturating_mul(86_400)
        } else {
            policy.failed_retention_secs
        };
        let expires_at = add_seconds(&now, retention_secs);
        let entry = TestCacheEntry {
            id: String::new(),
            project_id: self.project_id.clone(),
            session_id: spec.session_id,
            kind: spec.kind,
            command: spec.command,
            command_source: spec.command_source,
            key_version: TEST_CACHE_KEY_VERSION,
            cache_key: spec.cache_key,
            inputs_hash: spec.inputs_hash,
            impact_generation: spec.impact_generation,
            status: spec.status,
            exit_code: spec.exit_code,
            duration_ms: spec.duration_ms,
            output_ref: spec.output_ref,
            output_bytes: spec.output_bytes,
            covered_paths: spec
                .covered_paths
                .into_iter()
                .take(MAX_COVERED_PATHS)
                .collect(),
            warnings: spec
                .warnings
                .into_iter()
                .take(MAX_STORED_WARNINGS)
                .collect(),
            warning_count: spec.warning_count,
            failure_count: spec.failure_count,
            created_at: now,
            expires_at,
            hit_count: 0,
            last_hit_at: None,
        };
        let result_json =
            serde_json::to_string(&spec.result_json).context("serializing cached test result")?;
        let (stored, _events) =
            self.store
                .record_test_result_with_events(&entry, &result_json, policy.max_entries)?;
        Ok(stored)
    }

    pub fn stats(&self, now: &str) -> Result<TestCacheStats> {
        self.store.test_cache_stats(&self.project_id, now)
    }

    pub fn list(
        &self,
        session_id: Option<&str>,
        kind: Option<&str>,
        status: Option<&str>,
        include_expired: bool,
        limit: usize,
        now: &str,
    ) -> Result<Vec<TestCacheEntry>> {
        self.store.list_test_results(
            &self.project_id,
            session_id,
            kind,
            status,
            include_expired,
            limit,
            now,
        )
    }

    pub fn history_for_paths(
        &self,
        changed_paths: &[String],
        now: &str,
        limit: usize,
    ) -> Result<Vec<TestCacheEntry>> {
        self.store.test_results_for_paths(
            &self.project_id,
            changed_paths,
            now,
            limit.min(MAX_HISTORY_ROWS),
        )
    }

    pub fn prune(
        &self,
        older_than_secs: Option<u64>,
        expired_only: bool,
        kind: Option<&str>,
        dry_run: bool,
        now: &str,
    ) -> Result<TestCacheSweep> {
        self.store.prune_test_results(
            &self.project_id,
            older_than_secs,
            expired_only,
            kind,
            dry_run,
            now,
        )
    }

    pub fn clear(
        &self,
        session_id: Option<&str>,
        kind: Option<&str>,
        now: &str,
    ) -> Result<TestCacheSweep> {
        self.store
            .clear_test_results(&self.project_id, session_id, kind, now)
    }
}

#[derive(Debug, Clone)]
pub struct CachedTestResult {
    pub entry: TestCacheEntry,
    pub result: Value,
    pub provenance: TestCacheProvenance,
}

#[derive(Debug, Clone)]
pub struct TestResultSpec {
    pub session_id: String,
    pub kind: String,
    pub command: String,
    pub command_source: String,
    pub cache_key: String,
    pub inputs_hash: String,
    pub impact_generation: String,
    pub status: String,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub output_ref: Option<String>,
    pub output_bytes: u64,
    pub covered_paths: Vec<String>,
    pub warnings: Vec<String>,
    pub warning_count: u64,
    pub failure_count: u64,
    pub result_json: Value,
}

fn hit_provenance(entry: &TestCacheEntry) -> TestCacheProvenance {
    TestCacheProvenance {
        decision: "hit".to_string(),
        cache_hit: true,
        result_id: Some(entry.id.clone()),
        cache_key: Some(entry.cache_key.clone()),
        key_version: entry.key_version,
        inputs_hash: Some(entry.inputs_hash.clone()),
        impact_generation: Some(entry.impact_generation.clone()),
        cached_at: Some(entry.created_at.clone()),
        expires_at: Some(entry.expires_at.clone()),
        origin_session_id: Some(entry.session_id.clone()),
        status: Some(entry.status.clone()),
        note: format!(
            "served from the test-result cache ({}); no command ran for this call",
            if entry.status == "passed" {
                "cached pass"
            } else {
                "cached failure"
            }
        ),
    }
}

/// Bound one command body before it enters the content-addressed store. The
/// head retains setup context and the tail retains the failure summary; a
/// middle marker makes the truncation explicit.
pub fn bounded_output_bytes(content: &str, max_bytes: u64) -> (String, bool) {
    let max_bytes = max_bytes as usize;
    if content.len() <= max_bytes {
        return (content.to_string(), false);
    }
    let marker = "\n… (test-cache output truncated) …\n";
    let keep = max_bytes.saturating_sub(marker.len()).max(1);
    let head_target = keep * 2 / 3;
    let tail_target = keep - head_target;
    let mut head_end = head_target.min(content.len());
    while head_end > 0 && !content.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = content.len().saturating_sub(tail_target);
    while tail_start < content.len() && !content.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    (
        format!("{}{marker}{}", &content[..head_end], &content[tail_start..]),
        true,
    )
}

#[derive(Debug, Clone, Default)]
pub struct SpilledCacheOutput {
    pub output_ref: Option<String>,
    pub bytes: u64,
    pub truncated: bool,
    pub note: Option<String>,
}

/// Spill the bounded command body through the existing tool-output store. The
/// cache never stores a body inline. A spill failure is reported as a note and
/// does not make a completed test run fail.
pub fn spill_cache_output(
    access: &TestCacheAccess,
    session_id: &str,
    content: &str,
    max_bytes: u64,
    retention_secs: u64,
) -> SpilledCacheOutput {
    if content.is_empty() {
        return SpilledCacheOutput::default();
    }
    let (bounded, truncated) = bounded_output_bytes(content, max_bytes);
    let preview = bounded_output_bytes(&bounded, 2 * 1024).0;
    let store = ToolOutputStore::new(access.store().clone());
    match store.spill(
        access.project_id(),
        session_id,
        None,
        "test_result_cache",
        &bounded,
        &preview,
        retention_secs,
    ) {
        Ok(spilled) => SpilledCacheOutput {
            output_ref: Some(spilled.handle),
            bytes: spilled.bytes,
            truncated,
            note: None,
        },
        Err(error) => SpilledCacheOutput {
            output_ref: None,
            bytes: 0,
            truncated,
            note: Some(format!(
                "{}: output body was not stored: {error}",
                ToolErrorCode::TEST_CACHE_UNAVAILABLE
            )),
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputFingerprint {
    pub hash: String,
    pub files: usize,
    pub bytes: u64,
    pub complete: bool,
    pub notes: Vec<String>,
}

/// Resolved operator-declared external inputs. The hash covers the configured
/// names/paths themselves plus the observed values/content, so adding an unset
/// variable or removing a path still invalidates a previous key. No value is
/// ever stored or rendered; only digests and bounded notes leave this struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalScopeStatus {
    /// `env` or `path`.
    pub kind: String,
    /// Environment variable name or worktree-relative path. Never a value.
    pub label: String,
    pub present: bool,
    pub bytes: u64,
    pub complete: bool,
    /// Bounded, non-secret diagnostic for this entry.
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExternalScopeMaterial {
    pub hash: String,
    pub env_names: usize,
    pub paths: usize,
    pub bytes: u64,
    pub complete: bool,
    pub notes: Vec<String>,
    /// Per-entry metadata for the scopes preview. Values and file content are
    /// never retained here.
    pub statuses: Vec<ExternalScopeStatus>,
}

impl ExternalScopeMaterial {
    pub fn empty() -> Self {
        Self {
            hash: external_scope_digest(&[]),
            complete: true,
            ..Self::default()
        }
    }
}

/// Resolve `TestCachePolicy.extra_env` and `extra_paths` for one worktree.
///
/// - A missing environment variable is a valid state and is hashed as `unset`;
///   setting it later invalidates the key.
/// - A non-Unicode value, unreadable path, symlink escape, missing file,
///   non-file path, or content bound is `complete=false`. The caller must
///   disable reuse rather than risk a false pass.
pub fn resolve_external_scope_material(
    root: &Path,
    policy: &TestCachePolicy,
) -> ExternalScopeMaterial {
    let mut notes = Vec::new();
    let mut complete = true;
    let mut entries = Vec::new();
    let mut statuses = Vec::new();
    let mut env_names = policy.extra_env.clone();
    env_names.sort();
    env_names.dedup();
    for name in &env_names {
        match std::env::var(name) {
            Ok(value) => {
                if value.len() > MAX_TEST_CACHE_EXTRA_ENV_VALUE_BYTES {
                    complete = false;
                    if notes.len() < 8 {
                        notes.push(format!(
                            "extra_env {name} exceeds {MAX_TEST_CACHE_EXTRA_ENV_VALUE_BYTES} bytes; cache reuse disabled"
                        ));
                    }
                    entries.push(format!("env:{name}:oversized"));
                    statuses.push(ExternalScopeStatus {
                        kind: "env".to_string(),
                        label: name.clone(),
                        present: true,
                        bytes: value.len() as u64,
                        complete: false,
                        note: Some("value exceeds the 8 KiB scope limit".to_string()),
                    });
                } else {
                    entries.push(format!("env:{name}:value:{}", digest_hex(value.as_bytes())));
                    statuses.push(ExternalScopeStatus {
                        kind: "env".to_string(),
                        label: name.clone(),
                        present: true,
                        bytes: value.len() as u64,
                        complete: true,
                        note: None,
                    });
                }
            }
            Err(std::env::VarError::NotPresent) => {
                entries.push(format!("env:{name}:unset"));
                statuses.push(ExternalScopeStatus {
                    kind: "env".to_string(),
                    label: name.clone(),
                    present: false,
                    bytes: 0,
                    complete: true,
                    note: Some("unset; setting it later invalidates the key".to_string()),
                });
            }
            Err(std::env::VarError::NotUnicode(_)) => {
                complete = false;
                if notes.len() < 8 {
                    notes.push(format!(
                        "extra_env {name} is not valid Unicode; cache reuse disabled"
                    ));
                }
                entries.push(format!("env:{name}:invalid"));
                statuses.push(ExternalScopeStatus {
                    kind: "env".to_string(),
                    label: name.clone(),
                    present: false,
                    bytes: 0,
                    complete: false,
                    note: Some("value is not valid Unicode".to_string()),
                });
            }
        }
    }
    let mut paths = policy.extra_paths.clone();
    paths.sort();
    paths.dedup();
    let canonical_root = root.canonicalize().ok();
    let mut total_bytes = 0u64;
    let mut path_count = 0usize;
    for path in &paths {
        if !vp_protocol::test_cache::valid_scope_path(path) {
            complete = false;
            if notes.len() < 8 {
                notes.push(format!(
                    "extra_paths entry is not a safe worktree-relative path: {}",
                    bounded_path_label(path)
                ));
            }
            entries.push(format!("path:{path}:invalid"));
            statuses.push(ExternalScopeStatus {
                kind: "path".to_string(),
                label: path.clone(),
                present: false,
                bytes: 0,
                complete: false,
                note: Some("not a safe worktree-relative path".to_string()),
            });
            continue;
        }
        let Some(root) = canonical_root.as_ref() else {
            complete = false;
            notes.push("worktree root is not canonicalizable; cache reuse disabled".to_string());
            break;
        };
        let candidate = root.join(path);
        let canonical = match candidate.canonicalize() {
            Ok(path) => path,
            Err(error) => {
                complete = false;
                if notes.len() < 8 {
                    notes.push(format!(
                        "extra_paths {path} is unreadable: {error}; cache reuse disabled"
                    ));
                }
                entries.push(format!("path:{path}:unreadable"));
                statuses.push(ExternalScopeStatus {
                    kind: "path".to_string(),
                    label: path.clone(),
                    present: false,
                    bytes: 0,
                    complete: false,
                    note: Some("unreadable".to_string()),
                });
                continue;
            }
        };
        if !canonical.starts_with(root) {
            complete = false;
            if notes.len() < 8 {
                notes.push(format!(
                    "extra_paths {path} escapes the worktree; cache reuse disabled"
                ));
            }
            entries.push(format!("path:{path}:escape"));
            statuses.push(ExternalScopeStatus {
                kind: "path".to_string(),
                label: path.clone(),
                present: false,
                bytes: 0,
                complete: false,
                note: Some("symlink escapes the worktree".to_string()),
            });
            continue;
        }
        let metadata = match std::fs::metadata(&canonical) {
            Ok(metadata) => metadata,
            Err(error) => {
                complete = false;
                if notes.len() < 8 {
                    notes.push(format!(
                        "extra_paths {path} is unreadable: {error}; cache reuse disabled"
                    ));
                }
                entries.push(format!("path:{path}:unreadable"));
                statuses.push(ExternalScopeStatus {
                    kind: "path".to_string(),
                    label: path.clone(),
                    present: false,
                    bytes: 0,
                    complete: false,
                    note: Some("unreadable".to_string()),
                });
                continue;
            }
        };
        if !metadata.is_file() {
            complete = false;
            if notes.len() < 8 {
                notes.push(format!(
                    "extra_paths {path} is not a regular file; cache reuse disabled"
                ));
            }
            entries.push(format!("path:{path}:not_file"));
            statuses.push(ExternalScopeStatus {
                kind: "path".to_string(),
                label: path.clone(),
                present: true,
                bytes: metadata.len(),
                complete: false,
                note: Some("not a regular file".to_string()),
            });
            continue;
        }
        if metadata.len() > MAX_TEST_CACHE_EXTRA_PATH_BYTES {
            complete = false;
            if notes.len() < 8 {
                notes.push(format!(
                    "extra_paths {path} exceeds {MAX_TEST_CACHE_EXTRA_PATH_BYTES} bytes; cache reuse disabled"
                ));
            }
            entries.push(format!("path:{path}:oversized"));
            statuses.push(ExternalScopeStatus {
                kind: "path".to_string(),
                label: path.clone(),
                present: true,
                bytes: metadata.len(),
                complete: false,
                note: Some("exceeds the 8 MiB per-file limit".to_string()),
            });
            continue;
        }
        total_bytes = total_bytes.saturating_add(metadata.len());
        if total_bytes > MAX_TEST_CACHE_EXTRA_TOTAL_BYTES {
            complete = false;
            notes.push(format!(
                "extra_paths exceed {MAX_TEST_CACHE_EXTRA_TOTAL_BYTES} total bytes; cache reuse disabled"
            ));
            break;
        }
        match std::fs::read(&canonical) {
            Ok(content) => {
                path_count += 1;
                entries.push(format!(
                    "path:{path}:{}:{}",
                    content.len(),
                    digest_hex(&content)
                ));
                statuses.push(ExternalScopeStatus {
                    kind: "path".to_string(),
                    label: path.clone(),
                    present: true,
                    bytes: content.len() as u64,
                    complete: true,
                    note: None,
                });
            }
            Err(error) => {
                complete = false;
                if notes.len() < 8 {
                    notes.push(format!(
                        "extra_paths {path} is unreadable: {error}; cache reuse disabled"
                    ));
                }
                entries.push(format!("path:{path}:unreadable"));
                statuses.push(ExternalScopeStatus {
                    kind: "path".to_string(),
                    label: path.clone(),
                    present: false,
                    bytes: 0,
                    complete: false,
                    note: Some("unreadable".to_string()),
                });
            }
        }
    }
    ExternalScopeMaterial {
        hash: external_scope_digest(&entries),
        env_names: env_names.len(),
        paths: path_count,
        bytes: total_bytes,
        complete,
        notes,
        statuses,
    }
}

fn external_scope_digest(entries: &[String]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"test-cache-external-scopes-v1\n");
    hasher.update((entries.len() as u64).to_le_bytes());
    let mut sorted = entries.to_vec();
    sorted.sort();
    for entry in sorted {
        hasher.update((entry.len() as u64).to_le_bytes());
        hasher.update(entry.as_bytes());
    }
    digest_hex(&hasher.finalize())
}

fn bounded_path_label(path: &str) -> String {
    path.chars().take(120).collect()
}

/// Hash every non-ignored file in the worktree plus explicit changed and
/// dependency paths. The walk is sorted, bounded, and fails closed: if any
/// candidate is unreadable or a bound is reached, `complete=false` disables
/// reuse for this run.
pub fn compute_input_fingerprint(
    root: &Path,
    changed_paths: &[String],
    dependency_paths: &[String],
) -> InputFingerprint {
    let mut notes = Vec::new();
    let mut complete = true;
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .follow_links(false)
        .max_depth(Some(30))
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
                    | "coverage"
                    | ".next"
                    | ".turbo"
                    | ".cache"
                    | ".sandbox"
                    | "tool-outputs"
            )
        });
    let mut paths = Vec::new();
    for entry in builder.build() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                complete = false;
                if notes.len() < 8 {
                    notes.push(format!("input walk error: {error}"));
                }
                continue;
            }
        };
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        let path = relative.to_string_lossy().replace('\\', "/");
        if path.is_empty() {
            continue;
        }
        paths.push(path);
        if paths.len() > MAX_INPUT_FILES {
            complete = false;
            notes.push(format!(
                "more than {MAX_INPUT_FILES} files; cache reuse disabled"
            ));
            break;
        }
    }
    paths.sort();
    paths.dedup();

    let mut hasher = Sha256::new();
    hasher.update(b"test-cache-inputs-v1");
    let mut bytes = 0u64;
    let mut hashed = 0usize;
    let mut changed_sorted = changed_paths
        .iter()
        .map(|path| normalize_changed_path(path).unwrap_or_else(|| path.clone()))
        .collect::<Vec<_>>();
    changed_sorted.sort();
    changed_sorted.dedup();
    for path in &changed_sorted {
        hasher.update((path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update([u8::from(root.join(path).exists())]);
    }
    let mut dependency_sorted = dependency_paths
        .iter()
        .map(|path| normalize_changed_path(path).unwrap_or_else(|| path.clone()))
        .collect::<Vec<_>>();
    dependency_sorted.sort();
    dependency_sorted.dedup();
    for path in &dependency_sorted {
        hasher.update((path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
    }
    for path in &paths {
        let absolute = root.join(path);
        let metadata = match std::fs::metadata(&absolute) {
            Ok(metadata) => metadata,
            Err(error) => {
                complete = false;
                if notes.len() < 8 {
                    notes.push(format!("unreadable input {path}: {error}"));
                }
                continue;
            }
        };
        if metadata.len() > MAX_SINGLE_INPUT_BYTES {
            complete = false;
            if notes.len() < 8 {
                notes.push(format!(
                    "input {path} exceeds {MAX_SINGLE_INPUT_BYTES} bytes; cache reuse disabled"
                ));
            }
            continue;
        }
        bytes = bytes.saturating_add(metadata.len());
        if bytes > MAX_INPUT_BYTES {
            complete = false;
            notes.push(format!(
                "inputs exceed {MAX_INPUT_BYTES} bytes; cache reuse disabled"
            ));
            break;
        }
        let content = match std::fs::read(&absolute) {
            Ok(content) => content,
            Err(error) => {
                complete = false;
                if notes.len() < 8 {
                    notes.push(format!("unreadable input {path}: {error}"));
                }
                continue;
            }
        };
        hashed += 1;
        hasher.update((path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update((content.len() as u64).to_le_bytes());
        hasher.update(&content);
    }
    let hash = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    InputFingerprint {
        hash,
        files: hashed,
        bytes,
        complete,
        notes,
    }
}

/// Stable project-profile digest. Root paths and warnings are excluded so two
/// checkout locations with the same detected commands produce the same hash.
pub(crate) fn profile_hash(profile: &ProjectProfileData) -> String {
    let languages = profile
        .languages
        .iter()
        .map(|language| json!({"name": language.name, "files": language.files}))
        .collect::<Vec<_>>();
    let commands = profile
        .commands
        .iter()
        .map(|(kind, candidate)| {
            (
                kind.clone(),
                json!({
                    "command": candidate.command,
                    "source": candidate.source,
                    "confidence": candidate.confidence,
                }),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut roots = profile.test_layout.roots.clone();
    roots.sort();
    roots.dedup();
    let mut patterns = profile.test_layout.patterns.clone();
    patterns.sort();
    patterns.dedup();
    let mut conventions = profile
        .conventions
        .iter()
        .map(|convention| json!({"path": convention.path, "summary": convention.summary}))
        .collect::<Vec<_>>();
    conventions.sort_by(|left, right| {
        left["path"]
            .as_str()
            .unwrap_or_default()
            .cmp(right["path"].as_str().unwrap_or_default())
    });
    let mut ci = profile
        .ci
        .iter()
        .map(|file| json!({"path": file.path, "commands": file.commands}))
        .collect::<Vec<_>>();
    ci.sort_by(|left, right| {
        left["path"]
            .as_str()
            .unwrap_or_default()
            .cmp(right["path"].as_str().unwrap_or_default())
    });
    let mut package_managers = profile.package_managers.clone();
    package_managers.sort();
    package_managers.dedup();
    let canonical = json!({
        "version": 1,
        "languages": languages,
        "package_managers": package_managers,
        "commands": commands,
        "test_layout": {"roots": roots, "patterns": patterns},
        "conventions": conventions,
        "ci": ci,
        "truncated": profile.truncated,
    });
    let encoded = serde_json::to_vec(&canonical).unwrap_or_default();
    digest_hex(&encoded)
}

/// Deterministic environment subset. Values are only hashed into the key; they
/// are never stored in a row, event, CLI output, or log.
pub fn normalized_environment() -> Vec<(String, String)> {
    let mut values = CACHE_ENV_KEYS
        .iter()
        .filter_map(|key| {
            std::env::var(key)
                .ok()
                .map(|value| ((*key).to_string(), value))
        })
        .collect::<Vec<_>>();
    values.sort_by(|left, right| left.0.cmp(&right.0));
    values
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestCacheKeyMaterial {
    pub kind: String,
    pub command: String,
    pub cwd: String,
    pub environment: Vec<(String, String)>,
    pub profile_hash: String,
    pub inputs_hash: String,
    pub impact_generation: String,
    pub tool_version: String,
    /// Digest of operator-declared external environment names and worktree
    /// paths. `empty()` when the project declares no external scopes.
    pub external_scopes_hash: String,
}

impl TestCacheKeyMaterial {
    pub fn resolved(
        kind: &str,
        command: &str,
        profile_hash: &str,
        inputs_hash: &str,
        impact_generation: &str,
    ) -> Self {
        Self {
            kind: kind.to_string(),
            command: command.to_string(),
            cwd: ".".to_string(),
            environment: normalized_environment(),
            profile_hash: profile_hash.to_string(),
            inputs_hash: inputs_hash.to_string(),
            impact_generation: impact_generation.to_string(),
            tool_version: test_cache_tool_version().to_string(),
            external_scopes_hash: ExternalScopeMaterial::empty().hash,
        }
    }

    /// Attach the resolved external-scope digest. Keeping this separate from
    /// `resolved` lets existing callers stay explicit about whether they
    /// resolved scopes or intentionally have none.
    pub fn with_external_scopes(mut self, material: &ExternalScopeMaterial) -> Self {
        self.external_scopes_hash = material.hash.clone();
        self
    }
}

/// Versioned content-addressed key:
/// `v1:<sha256(command, argv, cwd, env, profile, inputs, impact, tool version)>`.
pub fn compute_cache_key(material: &TestCacheKeyMaterial) -> String {
    let mut hasher = Sha256::new();
    write_field(&mut hasher, TEST_CACHE_KEY_PREFIX);
    hasher.update(TEST_CACHE_KEY_VERSION.to_le_bytes());
    write_field(&mut hasher, &material.kind);
    write_field(&mut hasher, &material.command);
    let argv = tokenize_command(&material.command);
    hasher.update((argv.len() as u64).to_le_bytes());
    for argument in &argv {
        write_field(&mut hasher, argument);
    }
    write_field(&mut hasher, &material.cwd);
    let mut environment = material.environment.clone();
    environment.sort_by(|left, right| left.0.cmp(&right.0));
    hasher.update((environment.len() as u64).to_le_bytes());
    for (key, value) in &environment {
        write_field(&mut hasher, key);
        write_field(&mut hasher, value);
    }
    write_field(&mut hasher, &material.profile_hash);
    write_field(&mut hasher, &material.inputs_hash);
    write_field(&mut hasher, &material.impact_generation);
    write_field(&mut hasher, &material.tool_version);
    write_field(&mut hasher, &material.external_scopes_hash);
    format!(
        "v{TEST_CACHE_KEY_VERSION}:{}",
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

pub fn is_cache_key(value: &str) -> bool {
    let Some(rest) = value.strip_prefix(&format!("v{TEST_CACHE_KEY_VERSION}:")) else {
        return false;
    };
    rest.len() == 64 && rest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Small deterministic shell tokenizer used only for key material. It is not an
/// executor: the command itself is still run by `/bin/sh -c`. Quoting changes
/// remain visible because the raw command is hashed as well.
pub fn tokenize_command(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    while let Some(ch) = chars.next() {
        match ch {
            '\\' if !in_single => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            ch if ch.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            ch => current.push(ch),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Full-suite impact generation is stable for a profile; the input hash already
/// distinguishes different file contents in the key.
pub fn full_impact_generation(profile_hash: &str) -> String {
    let mut hasher = Sha256::new();
    write_field(&mut hasher, "impact-v1");
    write_field(&mut hasher, "full");
    write_field(&mut hasher, profile_hash);
    format!("impact-v1:{}", digest_hex(&hasher.finalize()))
}

/// Impact generation covers changed paths, the profile, the lexical dependency
/// digest/closure, and LSP reference paths. Historical cache rows are
/// deliberately excluded so storing a result cannot invalidate its own key.
pub fn impact_generation(
    profile_hash: &str,
    changed_paths: &[String],
    dependency_digest: &str,
    dependency_paths: &[String],
    lsp_paths: &[String],
) -> String {
    let mut hasher = Sha256::new();
    write_field(&mut hasher, "impact-v1");
    write_field(&mut hasher, profile_hash);
    let mut changed = changed_paths.to_vec();
    changed.sort();
    changed.dedup();
    for path in &changed {
        write_field(&mut hasher, path);
    }
    write_field(&mut hasher, dependency_digest);
    let mut dependencies = dependency_paths.to_vec();
    dependencies.sort();
    dependencies.dedup();
    for path in &dependencies {
        write_field(&mut hasher, path);
    }
    let mut lsp = lsp_paths.to_vec();
    lsp.sort();
    lsp.dedup();
    for path in &lsp {
        write_field(&mut hasher, path);
    }
    format!("impact-v1:{}", digest_hex(&hasher.finalize()))
}

/// Build a bounded, deterministic impact map. LSP references are used only when
/// a server is already running; the function never spawns a server and never
/// executes a test. Historical cache rows are an additional hint, not proof.
pub async fn build_impact_map(
    context: &ToolContext,
    access: Option<&TestCacheAccess>,
    changed_paths: &[String],
    limit: usize,
    include_history: bool,
) -> Result<TestImpactMap> {
    let limit = limit.clamp(1, HARD_MAX_IMPACT_TARGETS as usize);
    let mut notes = Vec::new();
    let mut changed = Vec::new();
    for path in changed_paths.iter().take(500) {
        let Some(path) = normalize_changed_path(path) else {
            notes.push(format!("ignored unsafe changed path: {path}"));
            continue;
        };
        changed.push(path);
    }
    changed.sort();
    changed.dedup();
    let profile = detect_profile(&context.root);
    let profile_hash = profile_hash(&profile);
    if changed.is_empty() {
        let mut map = TestImpactMap::full(&profile_hash, full_impact_generation(&profile_hash));
        map.notes.extend(notes);
        map.notes.push(
            "mapping is heuristic and does not prove that every affected test was found"
                .to_string(),
        );
        map.notes
            .push("no tests are executed while building an impact map".to_string());
        return Ok(map);
    }

    let dependency = crate::tooling::code::impact_dependency_closure(&context.root, &changed, 3);
    let mut dependency_paths = dependency.paths;
    dependency_paths.truncate(MAX_DEPENDENCY_PATHS);
    if dependency.truncated {
        notes.push("lexical dependency scan was truncated".to_string());
    }
    let lsp = crate::tooling::code::impact_lsp_targets(context, &changed).await;
    notes.extend(lsp.notes.iter().cloned());
    let generation = impact_generation(
        &profile_hash,
        &changed,
        &dependency.digest,
        &dependency_paths,
        &lsp.paths,
    );

    let mut candidates: BTreeMap<String, TestImpactTarget> = BTreeMap::new();
    let add = |path: String,
               reason: String,
               confidence: f64,
               source: &str,
               candidates: &mut BTreeMap<String, TestImpactTarget>| {
        let Some(path) = normalize_changed_path(&path) else {
            return;
        };
        let confidence = confidence.clamp(0.0, 1.0);
        match candidates.get_mut(&path) {
            Some(existing) => {
                if confidence > existing.confidence
                    || (confidence == existing.confidence
                        && source_rank(source) > source_rank(&existing.source))
                {
                    existing.reason = reason;
                    existing.confidence = confidence;
                    existing.source = source.to_string();
                }
            }
            None => {
                candidates.insert(
                    path.clone(),
                    TestImpactTarget {
                        path,
                        reason,
                        confidence,
                        source: source.to_string(),
                    },
                );
            }
        }
    };

    for path in &changed {
        if crate::tooling::code::is_test_path(path) {
            add(
                path.clone(),
                "changed path is a test file".to_string(),
                0.99,
                IMPACT_SOURCE_PROFILE,
                &mut candidates,
            );
        }
        for (candidate, confidence) in
            crate::tooling::code::impact_profile_candidates(&context.root, path)
        {
            add(
                candidate,
                format!("profile convention derived from {path}"),
                confidence,
                IMPACT_SOURCE_PROFILE,
                &mut candidates,
            );
        }
    }
    for path in &dependency_paths {
        if candidates.len() >= limit.saturating_mul(3) {
            break;
        }
        if crate::tooling::code::is_test_path(path) {
            add(
                path.clone(),
                "test file is in the lexical reverse-import closure".to_string(),
                0.9,
                IMPACT_SOURCE_LEXICAL,
                &mut candidates,
            );
        }
        for (candidate, confidence) in
            crate::tooling::code::impact_profile_candidates(&context.root, path)
        {
            add(
                candidate,
                format!("profile candidate of lexical dependency {path}"),
                (confidence * 0.9).min(0.85),
                IMPACT_SOURCE_LEXICAL,
                &mut candidates,
            );
        }
    }
    for path in &lsp.paths {
        if crate::tooling::code::is_test_path(path) {
            add(
                path.clone(),
                "language-server reference points at a test file".to_string(),
                0.97,
                IMPACT_SOURCE_LSP,
                &mut candidates,
            );
        }
        for (candidate, confidence) in
            crate::tooling::code::impact_profile_candidates(&context.root, path)
        {
            add(
                candidate,
                format!("profile candidate of LSP-referenced path {path}"),
                (confidence * 0.92).min(0.9),
                IMPACT_SOURCE_LSP,
                &mut candidates,
            );
        }
    }
    if include_history && let Some(access) = access {
        let now = vp_protocol::now_rfc3339();
        match access.history_for_paths(&changed, &now, MAX_HISTORY_ROWS) {
            Ok(history) => {
                for entry in history {
                    for covered in &entry.covered_paths {
                        if !crate::tooling::code::is_test_path(covered) {
                            continue;
                        }
                        add(
                            covered.clone(),
                            format!(
                                "covered {} in cached {} result {}",
                                changed.join(", "),
                                entry.kind,
                                entry.id
                            ),
                            history_confidence(&entry, &now),
                            IMPACT_SOURCE_HISTORY,
                            &mut candidates,
                        );
                    }
                }
            }
            Err(error) => notes.push(format!("historical mapping unavailable: {error}")),
        }
    }

    let mut targets = candidates.into_values().collect::<Vec<_>>();
    targets.sort_by(|left, right| {
        right
            .confidence
            .total_cmp(&left.confidence)
            .then_with(|| source_rank(&right.source).cmp(&source_rank(&left.source)))
            .then_with(|| left.path.cmp(&right.path))
    });
    let truncated = targets.len() > limit;
    targets.truncate(limit);
    let mut sources = BTreeMap::new();
    for target in &targets {
        *sources.entry(target.source.clone()).or_insert(0u64) += 1;
    }
    let lexical_only = !targets.iter().any(|target| {
        matches!(
            target.source.as_str(),
            IMPACT_SOURCE_LSP | IMPACT_SOURCE_HISTORY
        )
    });
    notes.push(
        "mapping is heuristic and does not prove that every affected test was found".to_string(),
    );
    notes.push("no tests are executed while building an impact map".to_string());
    if lsp.paths.is_empty() {
        notes.push(
            "no running language server contributed references; lexical/profile sources were used"
                .to_string(),
        );
    }
    Ok(TestImpactMap {
        changed_paths: changed,
        targets,
        generation,
        profile_hash,
        sources,
        dependency_paths,
        truncated,
        lsp_available: lsp.available,
        lexical_only,
        notes,
    })
}

pub(crate) fn impact_targets_as_affected(
    map: &TestImpactMap,
) -> Vec<crate::tooling::project::AffectedTest> {
    map.targets
        .iter()
        .map(|target| crate::tooling::project::AffectedTest {
            path: target.path.clone(),
            reason: target.reason.clone(),
            confidence: target.confidence,
            source: target.source.clone(),
        })
        .collect()
}

fn history_confidence(entry: &TestCacheEntry, now: &str) -> f64 {
    let mut confidence = 0.75;
    if let (Some(created), Some(now_time)) = (parse_time(&entry.created_at), parse_time(now)) {
        let age_days = (now_time - created).whole_seconds().max(0) as f64 / 86_400.0;
        confidence -= (age_days / 30.0).min(0.2);
    }
    confidence += (entry.hit_count as f64 * 0.005).min(0.1);
    confidence.clamp(0.5, 0.95)
}

fn source_rank(source: &str) -> u8 {
    match source {
        IMPACT_SOURCE_LSP => 4,
        IMPACT_SOURCE_LEXICAL => 3,
        IMPACT_SOURCE_PROFILE => 2,
        IMPACT_SOURCE_HISTORY => 1,
        _ => 0,
    }
}

/// Reconstruct cache metadata from session events. Hit counters and output
/// bodies are operational state and are deliberately not required to match;
/// `stored`, `invalidated`, and `pruned` events determine row existence and
/// durable metadata.
pub fn results_from_events(events: &[vp_protocol::types::Event]) -> Vec<TestCacheEntry> {
    let mut rows: BTreeMap<String, TestCacheEntry> = BTreeMap::new();
    for event in events {
        match event.kind.as_str() {
            "test_cache.stored" => {
                let Some(value) = event.payload.get("entry") else {
                    continue;
                };
                if let Ok(mut entry) = serde_json::from_value::<TestCacheEntry>(value.clone()) {
                    if entry.created_at.is_empty() {
                        entry.created_at = event.created_at.clone();
                    }
                    rows.insert(entry.id.clone(), entry);
                }
            }
            "test_cache.invalidated" => {
                if let Some(id) = event.payload.get("result_id").and_then(Value::as_str) {
                    rows.remove(id);
                }
            }
            "test_cache.pruned" => {
                if let Some(ids) = event.payload.get("ids").and_then(Value::as_array) {
                    for id in ids.iter().filter_map(Value::as_str) {
                        rows.remove(id);
                    }
                }
            }
            _ => {}
        }
    }
    rows.into_values().collect()
}

pub fn results_match(projected: &[TestCacheEntry], replayed: &[TestCacheEntry]) -> bool {
    if projected.len() != replayed.len() {
        return false;
    }
    let replayed = replayed
        .iter()
        .map(|entry| (entry.id.clone(), comparable_entry(entry)))
        .collect::<BTreeMap<_, _>>();
    projected.iter().all(|entry| {
        replayed
            .get(&entry.id)
            .is_some_and(|value| value == &comparable_entry(entry))
    })
}

fn comparable_entry(entry: &TestCacheEntry) -> Value {
    let mut value = serde_json::to_value(entry).unwrap_or(Value::Null);
    if let Some(object) = value.as_object_mut() {
        object.remove("hit_count");
        object.remove("last_hit_at");
    }
    value
}

/// Normalize a changed path without touching the filesystem. Absolute paths,
/// parent traversal, control characters, and empty values are rejected so a
/// tool input cannot escape the worktree through the impact map.
pub fn normalize_changed_path(path: &str) -> Option<String> {
    let mut path = path.trim();
    while let Some(rest) = path.strip_prefix("./") {
        path = rest;
    }
    if path.is_empty()
        || path.len() > 500
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains('\\')
        || path.chars().any(char::is_control)
    {
        return None;
    }
    let parsed = Path::new(path);
    if parsed
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return None;
    }
    Some(path.to_string())
}

fn write_field(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn digest_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn add_seconds(now: &str, seconds: u64) -> String {
    let Some(parsed) = parse_time(now) else {
        return now.to_string();
    };
    parsed
        .saturating_add(time::Duration::seconds(seconds.min(i64::MAX as u64) as i64))
        .format(&Rfc3339)
        .unwrap_or_else(|_| now.to_string())
}

fn parse_time(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339).ok()
}

/// Build a `ToolOutput` error for an incomplete hash without hiding that the
/// command itself may have run. Called by `run_project_task` after execution.
pub fn incomplete_input_note(fingerprint: &InputFingerprint) -> Option<String> {
    if fingerprint.complete {
        return None;
    }
    let detail = fingerprint.notes.join("; ");
    Some(format!(
        "{}: {}; the command ran but its result was not cached",
        ToolErrorCode::TEST_CACHE_INPUT_INCOMPLETE,
        if detail.is_empty() {
            "input hashing was incomplete".to_string()
        } else {
            detail
        }
    ))
}

/// Reject an unconfirmed destructive clear at the engine boundary. Kept here so
/// both RPC and CLI tests exercise the same message.
pub fn require_clear_confirmation(confirm: bool) -> Result<()> {
    if !confirm {
        bail!(
            "{}: clearing test-cache metadata requires confirm=true",
            ToolErrorCode::TEST_CACHE_CLEAR_UNCONFIRMED
        );
    }
    Ok(())
}

/// Missing daemon cache access is a normal tool-test context, not a hard error;
/// return a typed bypass note rather than pretending the cache was consulted.
pub fn unavailable_provenance() -> TestCacheProvenance {
    TestCacheProvenance {
        decision: "uncacheable".to_string(),
        cache_hit: false,
        key_version: TEST_CACHE_KEY_VERSION,
        note: format!(
            "{}: no project-scoped cache is attached to this tool context",
            ToolErrorCode::TEST_CACHE_UNAVAILABLE
        ),
        ..TestCacheProvenance::miss(String::new())
    }
}

/// Convenience used by `affected_tests` when the caller asked for impact mode
/// but no cache store is attached: profile + lexical still work.
pub async fn build_impact_map_without_store(
    context: &ToolContext,
    changed_paths: &[String],
    limit: usize,
) -> Result<TestImpactMap> {
    build_impact_map(context, None, changed_paths, limit, false).await
}

impl crate::Engine {
    pub async fn get_test_cache_policy(&self, project_id: &str) -> Result<TestCachePolicy> {
        let store = self.store().clone();
        let project = project_id.to_string();
        let exists = tokio::task::spawn_blocking({
            let store = store.clone();
            let project = project.clone();
            move || store.get_project(&project)
        })
        .await??
        .is_some();
        if !exists {
            bail!("project not found: {project_id}");
        }
        tokio::task::spawn_blocking(move || store.get_test_cache_policy(&project)).await?
    }

    pub async fn set_test_cache_policy(
        &self,
        project_id: &str,
        policy: TestCachePolicy,
    ) -> Result<vp_protocol::test_cache::TestCachePolicyResult> {
        policy.validate().map_err(|message| {
            anyhow::anyhow!("{}: {message}", ToolErrorCode::TEST_CACHE_POLICY_INVALID)
        })?;
        let normalized = policy.clone().normalized();
        let store = self.store().clone();
        let project = project_id.to_string();
        let exists = tokio::task::spawn_blocking({
            let store = store.clone();
            let project = project.clone();
            move || store.get_project(&project)
        })
        .await??
        .is_some();
        if !exists {
            bail!("project not found: {project_id}");
        }
        let stored = normalized.clone();
        let project = project_id.to_string();
        tokio::task::spawn_blocking(move || store.set_test_cache_policy(&project, &stored))
            .await??;
        self.record_project_manifest_best_effort().await;
        Ok(vp_protocol::test_cache::TestCachePolicyResult {
            project_id: project_id.to_string(),
            policy: normalized,
        })
    }

    pub async fn test_cache_stats(
        &self,
        project_id: &str,
    ) -> Result<vp_protocol::test_cache::TestCacheStats> {
        let access = TestCacheAccess::new(self.store().clone(), project_id);
        let now = vp_protocol::now_rfc3339();
        tokio::task::spawn_blocking(move || access.stats(&now)).await?
    }

    /// Read-only preview of `extra_env`/`extra_paths`. It resolves the same
    /// material the key builder uses, so an operator sees the exact
    /// fail-closed reasons without executing a test command or reading content.
    pub async fn preview_test_cache_scopes(
        &self,
        project_id: &str,
    ) -> Result<vp_protocol::test_cache::TestCacheScopePreviewResult> {
        let store = self.store().clone();
        let project = project_id.to_string();
        let (policy, root) = tokio::task::spawn_blocking(move || -> Result<_> {
            let project_row = store
                .get_project(&project)?
                .ok_or_else(|| anyhow::anyhow!("project not found: {project}"))?;
            let policy = store.get_test_cache_policy(&project)?;
            Ok::<_, anyhow::Error>((policy, PathBuf::from(project_row.root)))
        })
        .await??;
        let material = resolve_external_scope_material(&root, &policy);
        Ok(vp_protocol::test_cache::TestCacheScopePreviewResult {
            project_id: project_id.to_string(),
            enabled: policy.enabled,
            extra_env: material.env_names,
            extra_paths: material.paths,
            complete: material.complete,
            notes: material.notes,
            entries: material
                .statuses
                .into_iter()
                .map(|status| vp_protocol::test_cache::TestCacheScopeEntry {
                    kind: status.kind,
                    label: status.label,
                    present: status.present,
                    bytes: status.bytes,
                    complete: status.complete,
                    note: status.note,
                })
                .collect(),
        })
    }

    pub async fn list_test_cache_entries(
        &self,
        params: vp_protocol::test_cache::TestCacheEntriesParams,
    ) -> Result<vp_protocol::test_cache::TestCacheEntriesResult> {
        let access = TestCacheAccess::new(self.store().clone(), params.project_id.clone());
        let session = params.session_id.clone();
        let kind = params.kind.clone();
        let status = params.status.clone();
        let include_expired = params.include_expired.unwrap_or(false);
        let limit = params.limit.unwrap_or(100).clamp(1, 1_000) as usize;
        let now = vp_protocol::now_rfc3339();
        let (entries, total) = tokio::task::spawn_blocking(move || -> Result<_> {
            let entries = access.list(
                session.as_deref(),
                kind.as_deref(),
                status.as_deref(),
                include_expired,
                limit,
                &now,
            )?;
            let total = access.store().count_test_results(
                access.project_id(),
                session.as_deref(),
                kind.as_deref(),
                status.as_deref(),
                include_expired,
                &now,
            )?;
            Ok((entries, total))
        })
        .await??;
        Ok(vp_protocol::test_cache::TestCacheEntriesResult {
            project_id: params.project_id,
            truncated: total > entries.len() as u64,
            entries,
            total,
        })
    }

    pub async fn prune_test_cache(
        &self,
        params: vp_protocol::test_cache::TestCachePruneParams,
    ) -> Result<vp_protocol::test_cache::TestCachePruneResult> {
        let access = TestCacheAccess::new(self.store().clone(), params.project_id.clone());
        let older = params.older_than_secs;
        let expired_only = params.expired_only.unwrap_or(false);
        let kind = params.kind.clone();
        let dry_run = params.dry_run.unwrap_or(false);
        let now = vp_protocol::now_rfc3339();
        let sweep = tokio::task::spawn_blocking(move || {
            access.prune(older, expired_only, kind.as_deref(), dry_run, &now)
        })
        .await??;
        for event in &sweep.events {
            self.broadcast_event(event.clone());
        }
        Ok(vp_protocol::test_cache::TestCachePruneResult {
            project_id: params.project_id,
            pruned: sweep.removed_ids.len() as u64,
            remaining: sweep.remaining,
            dry_run,
            affected_sessions: sweep.affected_sessions,
            removed_ids: sweep.removed_ids.into_iter().take(100).collect(),
            reason: if expired_only {
                "expired".to_string()
            } else {
                "retention".to_string()
            },
        })
    }

    pub async fn clear_test_cache(
        &self,
        params: vp_protocol::test_cache::TestCacheClearParams,
    ) -> Result<vp_protocol::test_cache::TestCacheClearResult> {
        require_clear_confirmation(params.confirm)?;
        let access = TestCacheAccess::new(self.store().clone(), params.project_id.clone());
        let session = params.session_id.clone();
        let kind = params.kind.clone();
        let now = vp_protocol::now_rfc3339();
        let sweep = tokio::task::spawn_blocking(move || {
            access.clear(session.as_deref(), kind.as_deref(), &now)
        })
        .await??;
        for event in &sweep.events {
            self.broadcast_event(event.clone());
        }
        Ok(vp_protocol::test_cache::TestCacheClearResult {
            project_id: params.project_id,
            cleared: sweep.removed_ids.len() as u64,
            remaining: sweep.remaining,
            affected_sessions: sweep.affected_sessions,
        })
    }

    pub async fn test_cache_impact(
        &self,
        params: vp_protocol::test_cache::TestCacheImpactParams,
    ) -> Result<vp_protocol::test_cache::TestCacheImpactResult> {
        let store = self.store().clone();
        let session_id = params.session_id.clone();
        let session = tokio::task::spawn_blocking({
            let store = store.clone();
            let id = session_id.clone();
            move || store.get_session(&id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let project_id = session.project_id.clone();
        let project_store = store.clone();
        let project_lookup = project_id.clone();
        let project =
            tokio::task::spawn_blocking(move || project_store.get_project(&project_lookup))
                .await??
                .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let root = session
            .worktree
            .as_deref()
            .map(PathBuf::from)
            .filter(|path| path.is_dir())
            .unwrap_or_else(|| PathBuf::from(&project.root));
        let shell_policy = tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.get_shell_policy(&project_id)
        })
        .await??;
        let context = ToolContext {
            root: root.clone(),
            project_root: PathBuf::from(&project.root),
            sandbox_root: self.store().data_dir().join("sandbox").join(&session.id),
            secret_roots: Vec::new(),
            network_allowed: false,
            home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default(),
            restrict_home: shell_policy.restrict_home,
            read_jail: shell_policy.read_jail,
            git_branch: session.branch.clone(),
            remote: None,
            session_id: Some(session.id.clone()),
            process_supervisor: None,
            test_cache: Some(std::sync::Arc::new(TestCacheAccess::new(
                store.clone(),
                project_id.clone(),
            ))),
        };
        let mut changed = params.changed_paths;
        if changed.is_empty() {
            let root = root.clone();
            changed = tokio::task::spawn_blocking(move || {
                crate::tooling::project::git_changed_paths(&root)
            })
            .await?;
        }
        let limit = params.limit.unwrap_or(32).clamp(1, HARD_MAX_IMPACT_TARGETS) as usize;
        let include_history = params.include_history.unwrap_or(true);
        let access = context.test_cache.clone();
        let map = build_impact_map(
            &context,
            access.as_deref(),
            &changed,
            limit,
            include_history,
        )
        .await?;
        let profile = detect_profile(&root);
        Ok(vp_protocol::test_cache::TestCacheImpactResult {
            session_id: session.id,
            project_id,
            map,
            test_command: profile
                .commands
                .get("test")
                .map(|command| command.command.clone()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;
    use vp_protocol::test_cache::{
        DEFAULT_TEST_CACHE_MAX_OUTPUT_BYTES, IMPACT_SOURCE_FULL, TEST_CACHE_KEY_VERSION,
    };

    fn material(seed: &str) -> TestCacheKeyMaterial {
        TestCacheKeyMaterial {
            kind: "test".into(),
            command: format!("cargo test {seed}"),
            cwd: ".".into(),
            environment: vec![("CI".into(), "true".into())],
            profile_hash: "profile".into(),
            inputs_hash: "inputs".into(),
            impact_generation: "impact".into(),
            tool_version: test_cache_tool_version().into(),
            external_scopes_hash: ExternalScopeMaterial::empty().hash,
        }
    }

    fn store_fixture() -> (TempDir, Store, String, String) {
        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let worktree = TempDir::new().unwrap();
        let project = store
            .add_project(worktree.path().to_str().unwrap(), None)
            .unwrap();
        let session = store.create_session(&project.id, "cache").unwrap();
        (data, store, project.id, session.id)
    }

    fn entry(key: &str, status: &str, session: &str, project: &str) -> TestCacheEntry {
        TestCacheEntry {
            id: format!("tr-{key}"),
            project_id: project.to_string(),
            session_id: session.to_string(),
            kind: "test".into(),
            command: "cargo test".into(),
            command_source: "Cargo.toml".into(),
            key_version: TEST_CACHE_KEY_VERSION,
            cache_key: key.into(),
            inputs_hash: "inputs".into(),
            impact_generation: "impact".into(),
            status: status.into(),
            exit_code: if status == "passed" { 0 } else { 1 },
            duration_ms: 10,
            output_ref: None,
            output_bytes: 0,
            covered_paths: vec!["tests/a.rs".into(), "src/a.rs".into()],
            warnings: Vec::new(),
            warning_count: 0,
            failure_count: u64::from(status != "passed"),
            created_at: "2026-11-12T00:00:00Z".into(),
            expires_at: "2026-11-13T00:00:00Z".into(),
            hit_count: 0,
            last_hit_at: None,
        }
    }

    #[test]
    fn key_is_stable_for_identical_material() {
        let left = compute_cache_key(&material("a"));
        let right = compute_cache_key(&material("a"));
        assert_eq!(left, right);
        assert!(is_cache_key(&left));
        assert!(left.starts_with(&format!("v{TEST_CACHE_KEY_VERSION}:")));
    }

    #[test]
    fn key_changes_for_every_documented_input() {
        let base = compute_cache_key(&material("a"));
        let mut changed = material("a");
        changed.command = "cargo test b".into();
        assert_ne!(base, compute_cache_key(&changed));
        let mut changed = material("a");
        changed.kind = "lint".into();
        assert_ne!(base, compute_cache_key(&changed));
        let mut changed = material("a");
        changed.cwd = "sub".into();
        assert_ne!(base, compute_cache_key(&changed));
        let mut changed = material("a");
        changed.environment = vec![("CI".into(), "false".into())];
        assert_ne!(base, compute_cache_key(&changed));
        let mut changed = material("a");
        changed.profile_hash = "other".into();
        assert_ne!(base, compute_cache_key(&changed));
        let mut changed = material("a");
        changed.inputs_hash = "other".into();
        assert_ne!(base, compute_cache_key(&changed));
        let mut changed = material("a");
        changed.impact_generation = "other".into();
        assert_ne!(base, compute_cache_key(&changed));
        let mut changed = material("a");
        changed.tool_version = "other".into();
        assert_ne!(base, compute_cache_key(&changed));
    }

    #[test]
    fn argv_tokenization_is_deterministic_and_quote_aware() {
        assert_eq!(
            tokenize_command("cargo test -- --nocapture"),
            ["cargo", "test", "--", "--nocapture"]
        );
        assert_eq!(
            tokenize_command("sh -c 'echo hello world'"),
            ["sh", "-c", "echo hello world"]
        );
        assert_eq!(
            tokenize_command("python -c \"import os\\nprint(1)\""),
            ["python", "-c", "import osnprint(1)"]
        );
        assert!(tokenize_command("   ").is_empty());
    }

    #[test]
    fn key_rejects_invalid_shapes() {
        assert!(!is_cache_key("v1:abc"));
        assert!(!is_cache_key(
            "v2:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
        assert!(is_cache_key(&format!("v1:{}", "a".repeat(64))));
    }

    #[test]
    fn input_fingerprint_changes_with_content_and_deletion() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one").unwrap();
        let first = compute_input_fingerprint(dir.path(), &[], &[]);
        assert!(first.complete);
        assert_eq!(first.files, 1);
        std::fs::write(dir.path().join("a.txt"), "two").unwrap();
        let second = compute_input_fingerprint(dir.path(), &[], &[]);
        assert_ne!(first.hash, second.hash);
        std::fs::remove_file(dir.path().join("a.txt")).unwrap();
        let third = compute_input_fingerprint(dir.path(), &["a.txt".into()], &[]);
        assert_ne!(second.hash, third.hash);
        assert!(third.complete);
    }

    #[test]
    fn input_fingerprint_ignores_build_directories_but_respects_gitignore() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/junk"), "junk").unwrap();
        std::fs::write(dir.path().join("src.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "ignored").unwrap();
        let first = compute_input_fingerprint(dir.path(), &[], &[]);
        assert_eq!(first.files, 2);
        std::fs::write(dir.path().join("target/junk"), "different").unwrap();
        let second = compute_input_fingerprint(dir.path(), &[], &[]);
        assert_eq!(first.hash, second.hash);
    }

    #[test]
    fn profile_hash_is_root_independent() {
        let left = TempDir::new().unwrap();
        let right = TempDir::new().unwrap();
        for root in [left.path(), right.path()] {
            std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
            std::fs::create_dir_all(root.join("src")).unwrap();
            std::fs::write(root.join("src/lib.rs"), "pub fn a() {}").unwrap();
        }
        assert_eq!(
            profile_hash(&detect_profile(left.path())),
            profile_hash(&detect_profile(right.path()))
        );
    }

    #[test]
    fn output_bounding_keeps_head_and_tail_and_respects_utf8() {
        let content = format!("{}{}", "a".repeat(400), "é".repeat(400));
        let (bounded, truncated) = bounded_output_bytes(&content, 120);
        assert!(truncated);
        assert!(bounded.len() <= 120 + 8);
        assert!(bounded.starts_with("aaa"));
        assert!(bounded.ends_with("ééé"));
        assert!(bounded.contains("truncated"));
        let (same, truncated) = bounded_output_bytes("short", 120);
        assert!(!truncated);
        assert_eq!(same, "short");
    }

    #[test]
    fn cache_persists_hits_and_never_reuses_a_different_key() {
        let (_data, store, project_id, session_id) = store_fixture();
        let access = TestCacheAccess::new(store.clone(), project_id.clone());
        let policy = TestCachePolicy::default();
        let row = entry(
            &format!("v1:{}", "a".repeat(64)),
            "passed",
            &session_id,
            &project_id,
        );
        access
            .persist(
                TestResultSpec {
                    session_id: session_id.clone(),
                    kind: row.kind.clone(),
                    command: row.command.clone(),
                    command_source: row.command_source.clone(),
                    cache_key: row.cache_key.clone(),
                    inputs_hash: row.inputs_hash.clone(),
                    impact_generation: row.impact_generation.clone(),
                    status: row.status.clone(),
                    exit_code: 0,
                    duration_ms: 12,
                    output_ref: None,
                    output_bytes: 0,
                    covered_paths: row.covered_paths.clone(),
                    warnings: vec![],
                    warning_count: 0,
                    failure_count: 0,
                    result_json: json!({"ok": true, "raw_tail": "ok"}),
                },
                &policy,
            )
            .unwrap();
        let hit = access
            .lookup(&row.cache_key, &session_id, "2026-11-12T01:00:00Z")
            .unwrap()
            .expect("hit");
        assert!(hit.provenance.cache_hit);
        assert!(hit.provenance.note.contains("no command ran"));
        assert_eq!(hit.entry.hit_count, 1);
        let other_key = format!("v1:{}", "b".repeat(64));
        assert!(
            access
                .lookup(&other_key, &session_id, "2026-11-12T01:00:00Z")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn expired_and_failed_rows_are_never_returned_as_passes() {
        let (_data, store, project_id, session_id) = store_fixture();
        let access = TestCacheAccess::new(store.clone(), project_id.clone());
        let policy = TestCachePolicy::default();
        let key = format!("v1:{}", "c".repeat(64));
        let _row = entry(&key, "failed", &session_id, &project_id);
        access
            .persist(
                TestResultSpec {
                    session_id: session_id.clone(),
                    kind: "test".into(),
                    command: "cargo test".into(),
                    command_source: "Cargo.toml".into(),
                    cache_key: key.clone(),
                    inputs_hash: "inputs".into(),
                    impact_generation: "impact".into(),
                    status: "failed".into(),
                    exit_code: 101,
                    duration_ms: 5,
                    output_ref: None,
                    output_bytes: 0,
                    covered_paths: vec![],
                    warnings: vec![],
                    warning_count: 0,
                    failure_count: 1,
                    result_json: json!({"ok": false}),
                },
                &policy,
            )
            .unwrap();
        let now = vp_protocol::now_rfc3339();
        let hit = access
            .lookup(&key, &session_id, &now)
            .unwrap()
            .expect("failed hit");
        assert!(!hit.entry.is_pass());
        assert_eq!(hit.provenance.status.as_deref(), Some("failed"));
        // Failed rows expire at the short TTL; persisting applies the policy
        // TTL (900s by default), so a later lookup cannot reuse them.
        let later = add_seconds(&now, 86_400);
        assert!(access.lookup(&key, &session_id, &later).unwrap().is_none());
    }

    #[test]
    fn replace_and_replay_metadata_ignore_hit_counters() {
        let (_data, store, project_id, session_id) = store_fixture();
        let rows = vec![
            entry(
                &format!("v1:{}", "d".repeat(64)),
                "passed",
                &session_id,
                &project_id,
            ),
            entry(
                &format!("v1:{}", "e".repeat(64)),
                "failed",
                &session_id,
                &project_id,
            ),
        ];
        store.replace_test_results(&session_id, &rows).unwrap();
        let projected = store
            .list_test_results(
                &project_id,
                Some(&session_id),
                None,
                None,
                true,
                10,
                "2026-11-12T00:00:00Z",
            )
            .unwrap();
        let events = vec![
            vp_protocol::types::Event {
                session_id: session_id.clone(),
                seq: 1,
                kind: "test_cache.stored".into(),
                payload: json!({"entry": rows[0]}),
                created_at: rows[0].created_at.clone(),
            },
            vp_protocol::types::Event {
                session_id: session_id.clone(),
                seq: 2,
                kind: "test_cache.stored".into(),
                payload: json!({"entry": rows[1]}),
                created_at: rows[1].created_at.clone(),
            },
        ];
        let replayed = results_from_events(&events);
        assert!(results_match(&projected, &replayed));

        let pruned = vec![
            events[0].clone(),
            events[1].clone(),
            vp_protocol::types::Event {
                session_id: session_id.clone(),
                seq: 3,
                kind: "test_cache.pruned".into(),
                payload: json!({"ids": [rows[0].id]}),
                created_at: "2026-11-12T02:00:00Z".into(),
            },
        ];
        let replayed = results_from_events(&pruned);
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].id, rows[1].id);
    }

    #[test]
    fn stats_prune_and_clear_are_bounded_and_explicit() {
        let (_data, store, project_id, session_id) = store_fixture();
        let access = TestCacheAccess::new(store.clone(), project_id.clone());
        let policy = TestCachePolicy::default();
        for index in 0..3 {
            let key = format!("v1:{}", format!("{index}").repeat(64));
            access
                .persist(
                    TestResultSpec {
                        session_id: session_id.clone(),
                        kind: "test".into(),
                        command: "cargo test".into(),
                        command_source: "Cargo.toml".into(),
                        cache_key: key,
                        inputs_hash: "inputs".into(),
                        impact_generation: "impact".into(),
                        status: "passed".into(),
                        exit_code: 0,
                        duration_ms: 1,
                        output_ref: None,
                        output_bytes: 4,
                        covered_paths: vec!["tests/a.rs".into()],
                        warnings: vec![],
                        warning_count: 0,
                        failure_count: 0,
                        result_json: json!({"ok": true}),
                    },
                    &policy,
                )
                .unwrap();
        }
        // Persisted rows use the wall clock, so the sweep boundary must be
        // strictly after any plausible test-run time. A fixed past timestamp
        // makes this test expire as soon as the calendar passes it.
        let now = "2100-01-01T00:00:00Z";
        let stats = access.stats(now).unwrap();
        assert_eq!(stats.entries, 3);
        assert_eq!(stats.passed, 3);
        assert_eq!(stats.output_bytes, 12);
        let dry = access.prune(Some(0), false, None, true, now).unwrap();
        assert_eq!(dry.removed_ids.len(), 3);
        assert_eq!(dry.remaining, 3);
        assert!(dry.events.is_empty());
        let swept = access.prune(Some(0), false, None, false, now).unwrap();
        assert_eq!(swept.remaining, 0);
        assert!(!swept.events.is_empty());
        assert!(require_clear_confirmation(false).is_err());
        assert!(require_clear_confirmation(true).is_ok());
    }

    #[test]
    fn maximum_entries_evicts_oldest_rows_with_events() {
        let (_data, store, project_id, session_id) = store_fixture();
        let access = TestCacheAccess::new(store.clone(), project_id.clone());
        let policy = TestCachePolicy {
            max_entries: 2,
            ..TestCachePolicy::default()
        };
        for index in 0..3 {
            access
                .persist(
                    TestResultSpec {
                        session_id: session_id.clone(),
                        kind: "test".into(),
                        command: format!("cargo test {index}"),
                        command_source: "Cargo.toml".into(),
                        cache_key: format!("v1:{}", format!("{index}").repeat(64)),
                        inputs_hash: "inputs".into(),
                        impact_generation: "impact".into(),
                        status: "passed".into(),
                        exit_code: 0,
                        duration_ms: 1,
                        output_ref: None,
                        output_bytes: 0,
                        covered_paths: vec![],
                        warnings: vec![],
                        warning_count: 0,
                        failure_count: 0,
                        result_json: json!({"ok": true}),
                    },
                    &policy,
                )
                .unwrap();
        }
        let stats = access.stats("2026-11-12T01:00:00Z").unwrap();
        assert_eq!(stats.entries, 2);
        let events = store.list_all_events(&session_id).unwrap();
        assert!(
            events.iter().any(|event| event.kind == "test_cache.pruned"
                && event.payload["reason"] == "max_entries")
        );
    }

    #[test]
    fn impact_map_uses_profile_lexical_history_and_bounds_targets() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "[tool.pytest]\n").unwrap();
        std::fs::create_dir_all(dir.path().join("tests")).unwrap();
        std::fs::write(dir.path().join("pkg.py"), "def a():\n    return 1\n").unwrap();
        std::fs::write(
            dir.path().join("tests/test_pkg.py"),
            "from pkg import a\n\ndef test_a():\n    assert a() == 1\n",
        )
        .unwrap();
        let context = context(dir.path());
        let changed = vec!["pkg.py".to_string()];
        let map = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(build_impact_map(&context, None, &changed, 12, true))
            .unwrap();
        assert!(!map.changed_paths.is_empty());
        assert!(!map.generation.is_empty());
        assert!(
            map.targets
                .iter()
                .any(|target| target.path == "tests/test_pkg.py")
        );
        assert!(map.targets.iter().all(|target| {
            matches!(
                target.source.as_str(),
                IMPACT_SOURCE_PROFILE
                    | IMPACT_SOURCE_LEXICAL
                    | IMPACT_SOURCE_LSP
                    | IMPACT_SOURCE_HISTORY
            )
        }));
        assert!(map.targets.len() <= 12);
        assert!(map.notes.iter().any(|note| note.contains("heuristic")));
        assert!(!map.lsp_available);
    }

    #[test]
    fn impact_map_is_deterministic_and_rejects_unsafe_paths() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn a() {}\n").unwrap();
        let context = context(dir.path());
        let changed = vec!["src/lib.rs".to_string(), "../escape".to_string()];
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let left = runtime
            .block_on(build_impact_map(&context, None, &changed, 8, false))
            .unwrap();
        let right = runtime
            .block_on(build_impact_map(&context, None, &changed, 8, false))
            .unwrap();
        assert_eq!(left.generation, right.generation);
        assert_eq!(left.targets, right.targets);
        assert!(!left.changed_paths.iter().any(|path| path.contains("..")));
        assert!(left.notes.iter().any(|note| note.contains("unsafe")));
    }

    #[test]
    fn historical_coverage_adds_a_history_source() {
        let (_data, store, project_id, session_id) = store_fixture();
        let access = TestCacheAccess::new(store.clone(), project_id.clone());
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join("tests")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn a() {}\n").unwrap();
        std::fs::write(
            dir.path().join("tests/history_only.rs"),
            "#[test] fn t() {}\n",
        )
        .unwrap();
        let row = TestCacheEntry {
            session_id: session_id.clone(),
            project_id: project_id.clone(),
            covered_paths: vec!["src/lib.rs".into(), "tests/history_only.rs".into()],
            ..entry(
                &format!("v1:{}", "f".repeat(64)),
                "passed",
                &session_id,
                &project_id,
            )
        };
        access
            .persist(
                TestResultSpec {
                    session_id: session_id.clone(),
                    kind: "test".into(),
                    command: "cargo test".into(),
                    command_source: "Cargo.toml".into(),
                    cache_key: row.cache_key.clone(),
                    inputs_hash: row.inputs_hash.clone(),
                    impact_generation: row.impact_generation.clone(),
                    status: "passed".into(),
                    exit_code: 0,
                    duration_ms: 1,
                    output_ref: None,
                    output_bytes: 0,
                    covered_paths: row.covered_paths.clone(),
                    warnings: vec![],
                    warning_count: 0,
                    failure_count: 0,
                    result_json: json!({"ok": true}),
                },
                &TestCachePolicy::default(),
            )
            .unwrap();
        let context = context(dir.path());
        let map = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(build_impact_map(
                &context,
                Some(&access),
                &["src/lib.rs".into()],
                12,
                true,
            ))
            .unwrap();
        assert!(
            map.targets
                .iter()
                .any(|target| target.path == "tests/history_only.rs"
                    && target.source == IMPACT_SOURCE_HISTORY)
        );
    }

    #[test]
    fn full_impact_map_is_stable_and_explicit() {
        let map = TestImpactMap::full("profile", full_impact_generation("profile"));
        assert!(map.targets.is_empty());
        assert!(map.changed_paths.is_empty());
        assert!(map.notes.iter().any(|note| note.contains("full suite")));
        assert_eq!(map.profile_hash, "profile");
        assert!(!map.generation.is_empty());
    }

    #[test]
    fn input_fingerprint_fails_closed_for_oversized_inputs() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("huge.bin"),
            vec![0u8; (MAX_SINGLE_INPUT_BYTES + 1) as usize],
        )
        .unwrap();
        let fingerprint = compute_input_fingerprint(dir.path(), &[], &[]);
        assert!(!fingerprint.complete);
        assert!(
            fingerprint
                .notes
                .iter()
                .any(|note| note.contains("exceeds") || note.contains("disabled"))
        );
        assert!(incomplete_input_note(&fingerprint).is_some());
    }

    #[test]
    fn impact_generation_tracks_dependency_and_lsp_scope_but_not_history() {
        let base = impact_generation(
            "profile",
            &["src/lib.rs".into()],
            "dep-digest",
            &["tests/smoke.rs".into()],
            &[],
        );
        assert_eq!(
            base,
            impact_generation(
                "profile",
                &["src/lib.rs".into()],
                "dep-digest",
                &["tests/smoke.rs".into()],
                &[],
            ),
            "history is deliberately excluded from the generation"
        );
        assert_ne!(
            base,
            impact_generation(
                "profile",
                &["src/lib.rs".into()],
                "other-digest",
                &["tests/smoke.rs".into()],
                &[],
            )
        );
        assert_ne!(
            base,
            impact_generation(
                "profile",
                &["src/lib.rs".into()],
                "dep-digest",
                &["tests/smoke.rs".into()],
                &["tests/other.rs".into()],
            )
        );
        assert_ne!(
            base,
            impact_generation(
                "profile",
                &["src/other.rs".into()],
                "dep-digest",
                &["tests/smoke.rs".into()],
                &[],
            )
        );
    }

    #[test]
    fn replay_handles_invalidated_and_chunked_pruned_events() {
        let project = "prj";
        let session = "ses";
        let row = entry(
            &format!("v1:{}", "a".repeat(64)),
            "passed",
            session,
            project,
        );
        let events = vec![
            vp_protocol::types::Event {
                session_id: session.into(),
                seq: 1,
                kind: "test_cache.stored".into(),
                payload: json!({"entry": row}),
                created_at: "2026-11-12T00:00:00Z".into(),
            },
            vp_protocol::types::Event {
                session_id: session.into(),
                seq: 2,
                kind: "test_cache.invalidated".into(),
                payload: json!({"result_id": row.id}),
                created_at: "2026-11-12T00:01:00Z".into(),
            },
        ];
        assert!(results_from_events(&events).is_empty());

        let second = entry(
            &format!("v1:{}", "b".repeat(64)),
            "failed",
            session,
            project,
        );
        let events = vec![
            vp_protocol::types::Event {
                session_id: session.into(),
                seq: 1,
                kind: "test_cache.stored".into(),
                payload: json!({"entry": row}),
                created_at: "2026-11-12T00:00:00Z".into(),
            },
            vp_protocol::types::Event {
                session_id: session.into(),
                seq: 2,
                kind: "test_cache.stored".into(),
                payload: json!({"entry": second}),
                created_at: "2026-11-12T00:00:00Z".into(),
            },
            vp_protocol::types::Event {
                session_id: session.into(),
                seq: 3,
                kind: "test_cache.pruned".into(),
                payload: json!({"ids": [row.id], "reason": "max_entries"}),
                created_at: "2026-11-12T00:02:00Z".into(),
            },
            vp_protocol::types::Event {
                session_id: session.into(),
                seq: 4,
                kind: "test_cache.pruned".into(),
                payload: json!({"ids": [second.id], "reason": "max_entries"}),
                created_at: "2026-11-12T00:02:00Z".into(),
            },
        ];
        assert!(results_from_events(&events).is_empty());
    }

    #[test]
    fn normalized_paths_reject_escape_and_control_characters() {
        assert_eq!(
            normalize_changed_path("./src/lib.rs").unwrap(),
            "src/lib.rs"
        );
        assert!(normalize_changed_path("../x").is_none());
        assert!(normalize_changed_path("/abs").is_none());
        assert!(normalize_changed_path("a\\b").is_none());
        assert!(normalize_changed_path("a\nb").is_none());
        assert!(normalize_changed_path("").is_none());
    }

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

    #[allow(dead_code)]
    fn default_output_bound() -> u64 {
        DEFAULT_TEST_CACHE_MAX_OUTPUT_BYTES
    }

    #[allow(dead_code)]
    fn full_source() -> &'static str {
        IMPACT_SOURCE_FULL
    }

    #[test]
    fn external_paths_change_the_key_and_fail_closed() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("config")).unwrap();
        let input = root.path().join("config/local.env");
        std::fs::write(&input, b"first").unwrap();
        let policy = TestCachePolicy {
            extra_paths: vec!["config/local.env".into()],
            ..TestCachePolicy::default()
        };
        let first = resolve_external_scope_material(root.path(), &policy);
        assert!(first.complete, "{first:?}");
        assert_eq!(first.paths, 1);
        let base = material("scope").with_external_scopes(&first);
        let first_key = compute_cache_key(&base);
        std::fs::write(&input, b"second").unwrap();
        let second = resolve_external_scope_material(root.path(), &policy);
        assert!(second.complete);
        assert_ne!(
            first_key,
            compute_cache_key(&material("scope").with_external_scopes(&second))
        );

        let missing = TestCachePolicy {
            extra_paths: vec!["config/missing.env".into()],
            ..TestCachePolicy::default()
        };
        let missing = resolve_external_scope_material(root.path(), &missing);
        assert!(!missing.complete);
        assert!(missing.notes.iter().any(|note| note.contains("unreadable")));

        let directory = TestCachePolicy {
            extra_paths: vec!["config".into()],
            ..TestCachePolicy::default()
        };
        let directory = resolve_external_scope_material(root.path(), &directory);
        assert!(!directory.complete);
        assert!(
            directory
                .notes
                .iter()
                .any(|note| note.contains("regular file"))
        );

        let oversized = root.path().join("config/oversized.bin");
        let file = std::fs::File::create(&oversized).unwrap();
        file.set_len(MAX_TEST_CACHE_EXTRA_PATH_BYTES + 1).unwrap();
        let policy = TestCachePolicy {
            extra_paths: vec!["config/oversized.bin".into()],
            ..TestCachePolicy::default()
        };
        let oversized = resolve_external_scope_material(root.path(), &policy);
        assert!(!oversized.complete);
        assert!(oversized.notes.iter().any(|note| note.contains("exceeds")));

        let outside = TempDir::new().unwrap();
        std::fs::write(outside.path().join("secret"), b"outside").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                outside.path().join("secret"),
                root.path().join("config/leak"),
            )
            .unwrap();
            let policy = TestCachePolicy {
                extra_paths: vec!["config/leak".into()],
                ..TestCachePolicy::default()
            };
            let escaped = resolve_external_scope_material(root.path(), &policy);
            assert!(!escaped.complete);
            assert!(escaped.notes.iter().any(|note| note.contains("escapes")));
        }
    }

    #[test]
    fn external_env_values_are_hashed_and_never_stored() {
        let name = format!("VP_TEST_CACHE_SCOPE_{}", std::process::id());
        unsafe { std::env::set_var(&name, "super-secret-value") };
        let root = TempDir::new().unwrap();
        let policy = TestCachePolicy {
            extra_env: vec![name.clone()],
            ..TestCachePolicy::default()
        };
        let set = resolve_external_scope_material(root.path(), &policy);
        assert!(set.complete, "{set:?}");
        assert_eq!(set.env_names, 1);
        let set_key = compute_cache_key(&material("env").with_external_scopes(&set));
        let rendered = format!("{set:?}");
        assert!(!rendered.contains("super-secret-value"));
        assert_eq!(set.hash.len(), 64);
        assert!(set.hash.chars().all(|ch| ch.is_ascii_hexdigit()));
        unsafe { std::env::remove_var(&name) };
        let unset = resolve_external_scope_material(root.path(), &policy);
        assert!(unset.complete);
        assert_ne!(
            set_key,
            compute_cache_key(&material("env").with_external_scopes(&unset))
        );
        unsafe { std::env::set_var(&name, "x".repeat(MAX_TEST_CACHE_EXTRA_ENV_VALUE_BYTES + 1)) };
        let oversized = resolve_external_scope_material(root.path(), &policy);
        assert!(!oversized.complete);
        assert!(oversized.notes.iter().any(|note| note.contains("exceeds")));
        unsafe { std::env::remove_var(&name) };
    }

    #[test]
    fn external_scope_order_and_duplicates_do_not_change_the_hash() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("config")).unwrap();
        std::fs::write(root.path().join("config/a"), b"a").unwrap();
        std::fs::write(root.path().join("config/b"), b"b").unwrap();
        let first = TestCachePolicy {
            extra_paths: vec!["config/a".into(), "config/b".into()],
            ..TestCachePolicy::default()
        };
        let second = TestCachePolicy {
            extra_paths: vec!["config/b".into(), "config/a".into(), "config/a".into()],
            ..TestCachePolicy::default()
        };
        assert_eq!(
            resolve_external_scope_material(root.path(), &first).hash,
            resolve_external_scope_material(root.path(), &second).hash
        );
    }

    #[test]
    fn external_scope_total_bytes_bound_fails_closed() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("config")).unwrap();
        let mut paths = Vec::new();
        for index in 0..9 {
            let name = format!("config/part-{index}.bin");
            let file = std::fs::File::create(root.path().join(&name)).unwrap();
            file.set_len(MAX_TEST_CACHE_EXTRA_PATH_BYTES).unwrap();
            paths.push(name);
        }
        let policy = TestCachePolicy {
            extra_paths: paths,
            ..TestCachePolicy::default()
        };
        let material = resolve_external_scope_material(root.path(), &policy);
        assert!(!material.complete);
        assert!(
            material
                .notes
                .iter()
                .any(|note| note.contains("total bytes"))
        );
    }

    #[test]
    fn external_scope_preview_reports_presence_without_values() {
        let root = TempDir::new().unwrap();
        std::fs::write(root.path().join("local.env"), b"MODE=one").unwrap();
        let name = format!("VP_TEST_CACHE_PREVIEW_{}", std::process::id());
        unsafe { std::env::set_var(&name, "secret-value") };
        let policy = TestCachePolicy {
            extra_env: vec![name.clone()],
            extra_paths: vec!["local.env".into(), "missing.env".into()],
            ..TestCachePolicy::default()
        };
        let material = resolve_external_scope_material(root.path(), &policy);
        assert!(!material.complete);
        let env = material
            .statuses
            .iter()
            .find(|status| status.kind == "env" && status.label == name)
            .expect("env status");
        assert!(env.present);
        assert!(env.complete);
        assert_eq!(env.bytes, "secret-value".len() as u64);
        assert!(!format!("{env:?}").contains("secret-value"));
        let file = material
            .statuses
            .iter()
            .find(|status| status.kind == "path" && status.label == "local.env")
            .expect("path status");
        assert!(file.present && file.complete);
        let missing = material
            .statuses
            .iter()
            .find(|status| status.kind == "path" && status.label == "missing.env")
            .expect("missing status");
        assert!(!missing.present && !missing.complete);
        assert!(missing.note.is_some());
        unsafe { std::env::remove_var(&name) };
    }

    #[tokio::test]
    async fn test_cache_scope_preview_reports_unset_env_without_failing() {
        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let worktree = TempDir::new().unwrap();
        let project = store
            .add_project(worktree.path().to_str().unwrap(), None)
            .unwrap();
        let engine = crate::Engine::new(store.clone());
        let name = format!("VP_TEST_CACHE_UNSET_{}", std::process::id());
        unsafe { std::env::remove_var(&name) };
        store
            .set_test_cache_policy(
                &project.id,
                &TestCachePolicy {
                    extra_env: vec![name.clone()],
                    ..TestCachePolicy::default()
                },
            )
            .unwrap();
        let preview = engine.preview_test_cache_scopes(&project.id).await.unwrap();
        assert!(preview.complete, "an unset variable is a valid state");
        assert_eq!(preview.extra_env, 1);
        assert_eq!(preview.entries.len(), 1);
        assert_eq!(preview.entries[0].label, name);
        assert!(!preview.entries[0].present);
        assert!(preview.entries[0].complete);
        assert!(preview.entries[0].note.is_some());
    }

    #[tokio::test]
    async fn test_cache_scope_preview_is_read_only_and_project_scoped() {
        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let worktree = TempDir::new().unwrap();
        std::fs::write(worktree.path().join("local.env"), b"one").unwrap();
        let project = store
            .add_project(worktree.path().to_str().unwrap(), None)
            .unwrap();
        let engine = crate::Engine::new(store.clone());
        store
            .set_test_cache_policy(
                &project.id,
                &TestCachePolicy {
                    extra_paths: vec!["local.env".into()],
                    ..TestCachePolicy::default()
                },
            )
            .unwrap();
        let preview = engine.preview_test_cache_scopes(&project.id).await.unwrap();
        assert!(preview.enabled);
        assert!(preview.complete);
        assert_eq!(preview.extra_env, 0);
        assert_eq!(preview.extra_paths, 1);
        assert_eq!(preview.entries.len(), 1);
        assert_eq!(preview.entries[0].label, "local.env");
        assert!(preview.entries[0].present);
        assert_eq!(preview.project_id, project.id);
        let error = engine
            .preview_test_cache_scopes("missing")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("project not found"), "{error}");
    }
}
