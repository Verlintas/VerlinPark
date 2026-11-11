//! Test-result cache and deterministic test-impact wire types (ADR-0178).
//!
//! The cache is content-addressed: a result is reusable only when the command,
//! resolved argv, working directory, relevant environment subset, project
//! profile, tool version, input content, and impact-map generation all match.
//! A commit hash is never used as the key because uncommitted changes and
//! dependency-closure edits must invalidate a cached pass.
//!
//! Every struct is additive and defaulted so older manifests, bundles, CLI
//! clients, and Swift decoders keep working across a protocol upgrade.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Version prefix for the cache key. Changing this invalidates every existing
/// row safely (old rows remain stored until retention/prune, but never match).
pub const TEST_CACHE_KEY_VERSION: u32 = 1;

/// The policy default is on for explicit `run_project_task` calls. It is never
/// applied to the Integrator command unless `include_integration` is explicit.
pub const DEFAULT_TEST_CACHE_MAX_ENTRIES: u32 = 512;
pub const HARD_MAX_TEST_CACHE_ENTRIES: u32 = 10_000;
pub const DEFAULT_TEST_CACHE_RETENTION_DAYS: u32 = 14;
pub const HARD_MAX_TEST_CACHE_RETENTION_DAYS: u32 = 365;
pub const DEFAULT_TEST_CACHE_MAX_OUTPUT_BYTES: u64 = 256 * 1024;
pub const MIN_TEST_CACHE_MAX_OUTPUT_BYTES: u64 = 4 * 1024;
pub const HARD_MAX_TEST_CACHE_MAX_OUTPUT_BYTES: u64 = 8 * 1024 * 1024;
pub const DEFAULT_TEST_CACHE_FAILED_RETENTION_SECS: u64 = 900;
pub const MIN_TEST_CACHE_FAILED_RETENTION_SECS: u64 = 60;
pub const HARD_MAX_TEST_CACHE_FAILED_RETENTION_SECS: u64 = 86_400;
/// Bound on the number of test targets returned by one impact map.
pub const HARD_MAX_IMPACT_TARGETS: u32 = 32;
/// Bound on changed paths accepted by one impact request.
pub const HARD_MAX_IMPACT_CHANGED_PATHS: u32 = 500;

fn default_true() -> bool {
    true
}

fn default_max_entries() -> u32 {
    DEFAULT_TEST_CACHE_MAX_ENTRIES
}

fn default_retention_days() -> u32 {
    DEFAULT_TEST_CACHE_RETENTION_DAYS
}

fn default_max_output_bytes() -> u64 {
    DEFAULT_TEST_CACHE_MAX_OUTPUT_BYTES
}

fn default_failed_retention_secs() -> u64 {
    DEFAULT_TEST_CACHE_FAILED_RETENTION_SECS
}

/// Project-scoped test-result reuse policy.
///
/// `include_integration` is deliberately false by default: the deterministic
/// Integrator must execute its command unless an operator explicitly opts in.
/// `max_output_bytes` bounds the body persisted through the existing
/// content-addressed tool-output store; metadata rows never inline a body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestCachePolicy {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_max_entries")]
    pub max_entries: u32,
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: u64,
    /// Reuse lint results. Disabling this keeps lint always-fresh while still
    /// caching tests and checks.
    #[serde(default = "default_true")]
    pub reuse_lint: bool,
    /// Allow the Integrator command to use the cache. Off by default and never
    /// inferred from `enabled`.
    #[serde(default)]
    pub include_integration: bool,
    /// Failed/error results are reusable only for this short window. They are
    /// always reported as failures, never as passes.
    #[serde(default = "default_failed_retention_secs")]
    pub failed_retention_secs: u64,
}

impl Default for TestCachePolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            max_entries: default_max_entries(),
            retention_days: default_retention_days(),
            max_output_bytes: default_max_output_bytes(),
            reuse_lint: true,
            include_integration: false,
            failed_retention_secs: default_failed_retention_secs(),
        }
    }
}

impl TestCachePolicy {
    /// Clamp every field to the documented hard bounds. Stored policies are
    /// always normalized so a hand-edited manifest cannot create unbounded
    /// retention or an oversized body.
    pub fn normalized(mut self) -> Self {
        self.max_entries = self.max_entries.clamp(1, HARD_MAX_TEST_CACHE_ENTRIES);
        self.retention_days = self
            .retention_days
            .clamp(1, HARD_MAX_TEST_CACHE_RETENTION_DAYS);
        self.max_output_bytes = self.max_output_bytes.clamp(
            MIN_TEST_CACHE_MAX_OUTPUT_BYTES,
            HARD_MAX_TEST_CACHE_MAX_OUTPUT_BYTES,
        );
        self.failed_retention_secs = self.failed_retention_secs.clamp(
            MIN_TEST_CACHE_FAILED_RETENTION_SECS,
            HARD_MAX_TEST_CACHE_FAILED_RETENTION_SECS,
        );
        self
    }

    /// Strict validation used by `test_cache.policy.set`. Unlike
    /// `normalized()`, an explicit out-of-range value is rejected with a
    /// stable `TEST_CACHE_POLICY_INVALID` prefix instead of being silently
    /// changed.
    pub fn validate(&self) -> Result<(), String> {
        if self.max_entries == 0 || self.max_entries > HARD_MAX_TEST_CACHE_ENTRIES {
            return Err(format!(
                "max_entries must be 1..={HARD_MAX_TEST_CACHE_ENTRIES}"
            ));
        }
        if self.retention_days == 0 || self.retention_days > HARD_MAX_TEST_CACHE_RETENTION_DAYS {
            return Err(format!(
                "retention_days must be 1..={HARD_MAX_TEST_CACHE_RETENTION_DAYS}"
            ));
        }
        if self.max_output_bytes < MIN_TEST_CACHE_MAX_OUTPUT_BYTES
            || self.max_output_bytes > HARD_MAX_TEST_CACHE_MAX_OUTPUT_BYTES
        {
            return Err(format!(
                "max_output_bytes must be {MIN_TEST_CACHE_MAX_OUTPUT_BYTES}..={HARD_MAX_TEST_CACHE_MAX_OUTPUT_BYTES}"
            ));
        }
        if self.failed_retention_secs < MIN_TEST_CACHE_FAILED_RETENTION_SECS
            || self.failed_retention_secs > HARD_MAX_TEST_CACHE_FAILED_RETENTION_SECS
        {
            return Err(format!(
                "failed_retention_secs must be {MIN_TEST_CACHE_FAILED_RETENTION_SECS}..={HARD_MAX_TEST_CACHE_FAILED_RETENTION_SECS}"
            ));
        }
        Ok(())
    }

    /// Whether a task kind may use the cache under this policy. Destructive or
    /// long-running kinds (`format`, `dev`, `build`, `custom`) are not cached;
    /// `typecheck` is stored as `check`.
    pub fn allows_kind(&self, kind: &str) -> bool {
        if !self.enabled {
            return false;
        }
        match kind {
            "test" => true,
            "lint" => self.reuse_lint,
            "check" | "typecheck" => true,
            _ => false,
        }
    }
}

fn default_key_version() -> u32 {
    TEST_CACHE_KEY_VERSION
}

/// One stored test result. `output_ref` is an opaque handle into the existing
/// content-addressed tool-output store; the body is never returned by list or
/// stats calls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestCacheEntry {
    pub id: String,
    pub project_id: String,
    pub session_id: String,
    /// Normalized kind: `test`, `lint`, or `check`.
    pub kind: String,
    pub command: String,
    #[serde(default)]
    pub command_source: String,
    #[serde(default = "default_key_version")]
    pub key_version: u32,
    pub cache_key: String,
    pub inputs_hash: String,
    pub impact_generation: String,
    /// `passed`, `failed`, or `error`. A cached failure is always surfaced as a
    /// failure, with `cache_hit=true`; it is never converted into a pass.
    pub status: String,
    pub exit_code: i32,
    pub duration_ms: u64,
    #[serde(default)]
    pub output_ref: Option<String>,
    #[serde(default)]
    pub output_bytes: u64,
    #[serde(default)]
    pub covered_paths: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub warning_count: u64,
    #[serde(default)]
    pub failure_count: u64,
    pub created_at: String,
    pub expires_at: String,
    #[serde(default)]
    pub hit_count: u64,
    #[serde(default)]
    pub last_hit_at: Option<String>,
}

impl TestCacheEntry {
    pub fn is_expired_at(&self, now: &str) -> bool {
        self.expires_at.as_str() <= now
    }

    pub fn is_pass(&self) -> bool {
        self.status == "passed"
    }
}

/// Provenance attached to a `run_project_task` / `affected_tests` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestCacheProvenance {
    /// `hit`, `miss`, `refresh`, `bypass`, `disabled`, `uncacheable`, `error`.
    pub decision: String,
    pub cache_hit: bool,
    #[serde(default)]
    pub result_id: Option<String>,
    #[serde(default)]
    pub cache_key: Option<String>,
    #[serde(default)]
    pub key_version: u32,
    #[serde(default)]
    pub inputs_hash: Option<String>,
    #[serde(default)]
    pub impact_generation: Option<String>,
    #[serde(default)]
    pub cached_at: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub origin_session_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    pub note: String,
}

impl TestCacheProvenance {
    pub fn miss(note: impl Into<String>) -> Self {
        Self {
            decision: "miss".to_string(),
            cache_hit: false,
            result_id: None,
            cache_key: None,
            key_version: TEST_CACHE_KEY_VERSION,
            inputs_hash: None,
            impact_generation: None,
            cached_at: None,
            expires_at: None,
            origin_session_id: None,
            status: None,
            note: note.into(),
        }
    }

    pub fn bypass(note: impl Into<String>) -> Self {
        Self {
            decision: "bypass".to_string(),
            cache_hit: false,
            note: note.into(),
            ..Self::miss(String::new())
        }
    }
}

/// Cross-project / project-scoped cache counters. Counts are derived from the
/// metadata rows; output bodies are not inspected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestCacheStats {
    pub project_id: String,
    pub entries: u64,
    pub passed: u64,
    pub failed: u64,
    pub error: u64,
    pub expired: u64,
    pub total_hits: u64,
    pub output_bytes: u64,
    pub by_kind: BTreeMap<String, u64>,
    #[serde(default)]
    pub oldest_created_at: Option<String>,
    #[serde(default)]
    pub newest_created_at: Option<String>,
    pub policy: TestCachePolicy,
    pub generated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestCachePolicyResult {
    pub project_id: String,
    pub policy: TestCachePolicy,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TestCachePolicyParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetTestCachePolicyParams {
    pub project_id: String,
    pub policy: TestCachePolicy,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TestCacheStatsParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TestCacheEntriesParams {
    pub project_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub include_expired: Option<bool>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCacheEntriesResult {
    pub project_id: String,
    pub entries: Vec<TestCacheEntry>,
    pub total: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TestCachePruneParams {
    pub project_id: String,
    /// Remove rows older than this many seconds. `None` means policy retention.
    #[serde(default)]
    pub older_than_secs: Option<u64>,
    /// Restrict to rows whose `expires_at` is in the past.
    #[serde(default)]
    pub expired_only: Option<bool>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub dry_run: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCachePruneResult {
    pub project_id: String,
    pub pruned: u64,
    pub remaining: u64,
    pub dry_run: bool,
    pub affected_sessions: u64,
    /// Bounded sample of removed ids for audit/CLI display.
    #[serde(default)]
    pub removed_ids: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TestCacheClearParams {
    pub project_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    /// Explicit confirmation. A false value is rejected by the daemon.
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCacheClearResult {
    pub project_id: String,
    pub cleared: u64,
    pub remaining: u64,
    pub affected_sessions: u64,
}

/// Test-impact map provenance sources. A target always carries exactly one
/// best source label so callers can judge confidence without treating the map
/// as a completeness proof.
pub const IMPACT_SOURCE_PROFILE: &str = "profile";
pub const IMPACT_SOURCE_LEXICAL: &str = "lexical";
pub const IMPACT_SOURCE_LSP: &str = "lsp";
pub const IMPACT_SOURCE_HISTORY: &str = "history";
pub const IMPACT_SOURCE_FULL: &str = "full";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestImpactTarget {
    pub path: String,
    pub reason: String,
    pub confidence: f64,
    /// `profile`, `lexical`, `lsp`, or `history`.
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestImpactMap {
    pub changed_paths: Vec<String>,
    pub targets: Vec<TestImpactTarget>,
    /// Deterministic generation over the changed paths, profile hash, and
    /// lexical dependency closure. Historical rows never change this value,
    /// so recording a result cannot invalidate the key that produced it.
    pub generation: String,
    pub profile_hash: String,
    #[serde(default)]
    pub sources: BTreeMap<String, u64>,
    #[serde(default)]
    pub dependency_paths: Vec<String>,
    pub truncated: bool,
    pub lsp_available: bool,
    /// True when the map is the legacy lexical-only mapping.
    pub lexical_only: bool,
    #[serde(default)]
    pub notes: Vec<String>,
}

impl TestImpactMap {
    pub fn full(profile_hash: &str, generation: String) -> Self {
        Self {
            changed_paths: Vec::new(),
            targets: Vec::new(),
            generation,
            profile_hash: profile_hash.to_string(),
            sources: BTreeMap::new(),
            dependency_paths: Vec::new(),
            truncated: false,
            lsp_available: false,
            lexical_only: false,
            notes: vec!["no changed paths; the full suite is the affected scope".to_string()],
        }
    }

    pub fn source_count(&self, source: &str) -> u64 {
        self.targets
            .iter()
            .filter(|target| target.source == source)
            .count() as u64
    }

    pub fn paths(&self) -> Vec<String> {
        let mut paths = self
            .targets
            .iter()
            .map(|target| target.path.clone())
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        paths
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TestCacheImpactParams {
    pub session_id: String,
    #[serde(default)]
    pub changed_paths: Vec<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    /// Exclude historical cache rows from the mapping.
    #[serde(default)]
    pub include_history: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCacheImpactResult {
    pub session_id: String,
    pub project_id: String,
    pub map: TestImpactMap,
    pub test_command: Option<String>,
}

/// Parse the public `affected_tests` mode. The legacy default is lexical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AffectedTestsMode {
    Lexical,
    Impact,
}

impl AffectedTestsMode {
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.unwrap_or("lexical") {
            "lexical" => Ok(Self::Lexical),
            "impact" => Ok(Self::Impact),
            other => Err(format!("mode must be lexical or impact, got {other:?}")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lexical => "lexical",
            Self::Impact => "impact",
        }
    }
}

/// Parse the `run_project_task` cache strategy. The legacy default is `auto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestCacheMode {
    Auto,
    Bypass,
    Refresh,
}

impl TestCacheMode {
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.unwrap_or("auto") {
            "auto" => Ok(Self::Auto),
            "bypass" => Ok(Self::Bypass),
            "refresh" => Ok(Self::Refresh),
            other => Err(format!(
                "cache must be auto, bypass, or refresh, got {other:?}"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Bypass => "bypass",
            Self::Refresh => "refresh",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_is_on_for_explicit_runs_but_not_the_integrator() {
        let policy = TestCachePolicy::default();
        assert!(policy.enabled);
        assert!(policy.reuse_lint);
        assert!(!policy.include_integration);
        assert_eq!(policy.max_entries, DEFAULT_TEST_CACHE_MAX_ENTRIES);
        assert!(policy.allows_kind("test"));
        assert!(policy.allows_kind("check"));
        assert!(policy.allows_kind("typecheck"));
        assert!(!policy.allows_kind("build"));
        assert!(!policy.allows_kind("dev"));
    }

    #[test]
    fn policy_normalizes_and_validates_bounds() {
        let normalized = TestCachePolicy {
            max_entries: 0,
            retention_days: 0,
            max_output_bytes: 1,
            failed_retention_secs: 1,
            ..TestCachePolicy::default()
        }
        .normalized();
        assert_eq!(normalized.max_entries, 1);
        assert_eq!(normalized.retention_days, 1);
        assert_eq!(normalized.max_output_bytes, MIN_TEST_CACHE_MAX_OUTPUT_BYTES);
        assert_eq!(
            normalized.failed_retention_secs,
            MIN_TEST_CACHE_FAILED_RETENTION_SECS
        );

        let invalid = TestCachePolicy {
            max_entries: HARD_MAX_TEST_CACHE_ENTRIES + 1,
            ..TestCachePolicy::default()
        };
        assert!(invalid.validate().unwrap_err().contains("max_entries"));
        assert!(TestCachePolicy::default().validate().is_ok());
    }

    #[test]
    fn older_policy_json_decodes_with_safe_defaults() {
        let policy: TestCachePolicy = serde_json::from_str("{}").unwrap();
        assert!(policy.enabled);
        assert!(!policy.include_integration);
        assert_eq!(policy.retention_days, DEFAULT_TEST_CACHE_RETENTION_DAYS);
        let partial: TestCachePolicy = serde_json::from_str(r#"{"enabled":false}"#).unwrap();
        assert!(!partial.enabled);
        assert_eq!(
            partial.max_output_bytes,
            DEFAULT_TEST_CACHE_MAX_OUTPUT_BYTES
        );
    }

    #[test]
    fn entry_expiry_and_pass_checks_are_explicit() {
        let entry = TestCacheEntry {
            id: "tr-1".into(),
            project_id: "p".into(),
            session_id: "s".into(),
            kind: "test".into(),
            command: "cargo test".into(),
            command_source: "Cargo.toml".into(),
            key_version: TEST_CACHE_KEY_VERSION,
            cache_key: "v1:abc".into(),
            inputs_hash: "abc".into(),
            impact_generation: "g".into(),
            status: "passed".into(),
            exit_code: 0,
            duration_ms: 10,
            output_ref: None,
            output_bytes: 0,
            covered_paths: vec!["src/lib.rs".into()],
            warnings: Vec::new(),
            warning_count: 0,
            failure_count: 0,
            created_at: "2026-11-12T00:00:00Z".into(),
            expires_at: "2026-11-13T00:00:00Z".into(),
            hit_count: 0,
            last_hit_at: None,
        };
        assert!(entry.is_pass());
        assert!(!entry.is_expired_at("2026-11-12T12:00:00Z"));
        assert!(entry.is_expired_at("2026-11-13T00:00:00Z"));
    }

    #[test]
    fn modes_parse_with_backward_compatible_defaults() {
        assert_eq!(
            AffectedTestsMode::parse(None).unwrap(),
            AffectedTestsMode::Lexical
        );
        assert_eq!(
            AffectedTestsMode::parse(Some("impact")).unwrap(),
            AffectedTestsMode::Impact
        );
        assert!(AffectedTestsMode::parse(Some("all")).is_err());
        assert_eq!(TestCacheMode::parse(None).unwrap(), TestCacheMode::Auto);
        assert_eq!(
            TestCacheMode::parse(Some("refresh")).unwrap(),
            TestCacheMode::Refresh
        );
        assert!(TestCacheMode::parse(Some("force")).is_err());
    }

    #[test]
    fn impact_map_counts_sources_and_dedupes_paths() {
        let map = TestImpactMap {
            changed_paths: vec!["src/a.rs".into()],
            targets: vec![
                TestImpactTarget {
                    path: "tests/a.rs".into(),
                    reason: "profile".into(),
                    confidence: 0.8,
                    source: IMPACT_SOURCE_PROFILE.into(),
                },
                TestImpactTarget {
                    path: "tests/b.rs".into(),
                    reason: "lexical".into(),
                    confidence: 0.9,
                    source: IMPACT_SOURCE_LEXICAL.into(),
                },
                TestImpactTarget {
                    path: "tests/a.rs".into(),
                    reason: "history".into(),
                    confidence: 0.7,
                    source: IMPACT_SOURCE_HISTORY.into(),
                },
            ],
            generation: "g".into(),
            profile_hash: "p".into(),
            sources: BTreeMap::new(),
            dependency_paths: Vec::new(),
            truncated: false,
            lsp_available: false,
            lexical_only: false,
            notes: Vec::new(),
        };
        assert_eq!(map.source_count(IMPACT_SOURCE_PROFILE), 1);
        assert_eq!(map.paths(), vec!["tests/a.rs", "tests/b.rs"]);
    }

    #[test]
    fn session_bundle_decodes_without_test_results() {
        let bundle: crate::types::SessionBundle = serde_json::from_value(serde_json::json!({
            "version": 1,
            "exported_at": "2026-11-12T00:00:00Z",
            "project": {
                "id": "prj-1",
                "name": "p",
                "root": "/tmp/p",
                "default_branch": "main",
                "created_at": "2026-11-12T00:00:00Z"
            },
            "session": {
                "id": "ses-1",
                "project_id": "prj-1",
                "goal": "g",
                "status": "idle",
                "created_at": "2026-11-12T00:00:00Z"
            },
            "events": [],
            "agents": [],
            "messages": [],
            "todos": [],
            "merge_queue": []
        }))
        .unwrap();
        assert!(bundle.test_results.is_empty());
    }

    #[test]
    fn entry_round_trips_bounded_metadata_without_a_body() {
        let entry = TestCacheEntry {
            id: "tr-1".into(),
            project_id: "prj-1".into(),
            session_id: "ses-1".into(),
            kind: "check".into(),
            command: "just test".into(),
            command_source: "justfile".into(),
            key_version: TEST_CACHE_KEY_VERSION,
            cache_key: "v1:abc".into(),
            inputs_hash: "inputs".into(),
            impact_generation: "impact-v1:x".into(),
            status: "error".into(),
            exit_code: 1,
            duration_ms: 42,
            output_ref: Some("out_1".into()),
            output_bytes: 128,
            covered_paths: vec!["tests/a.rs".into()],
            warnings: vec!["w".into()],
            warning_count: 1,
            failure_count: 1,
            created_at: "2026-11-12T00:00:00Z".into(),
            expires_at: "2026-11-12T00:15:00Z".into(),
            hit_count: 2,
            last_hit_at: Some("2026-11-12T00:05:00Z".into()),
        };
        let encoded = serde_json::to_value(&entry).unwrap();
        assert!(encoded.get("result_json").is_none());
        let decoded: TestCacheEntry = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, entry);
        assert!(!decoded.is_pass());
    }

    #[test]
    fn params_decode_additively_for_old_clients() {
        let params: TestCachePruneParams =
            serde_json::from_value(serde_json::json!({"project_id": "p"})).unwrap();
        assert_eq!(params.project_id, "p");
        assert!(params.older_than_secs.is_none());
        assert!(params.dry_run.is_none());

        let clear: TestCacheClearParams =
            serde_json::from_value(serde_json::json!({"project_id": "p", "confirm": true}))
                .unwrap();
        assert!(clear.confirm);
        assert!(clear.session_id.is_none());
    }
}
