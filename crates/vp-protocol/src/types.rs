use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::runtime::{PolicySnapshot, RuntimeSnapshot};
use crate::test_cache::TestCacheEntry;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub root: String,
    pub default_branch: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellPolicy {
    #[serde(default = "default_shell_network")]
    pub network: String,
    #[serde(default)]
    pub read_deny: Vec<String>,
    /// Deny reads under the user's home directory except the worktree, scratch,
    /// Git metadata, and explicitly allowed toolchain/cache paths.
    #[serde(default)]
    pub restrict_home: bool,
    /// Deny reads under the user's home-parent (normally `/Users`) and
    /// `/Volumes` outside the worktree, scratch, Git metadata, and toolchain
    /// allowlist. System paths remain readable.
    #[serde(default)]
    pub read_jail: bool,
}

impl Default for ShellPolicy {
    fn default() -> Self {
        Self {
            network: default_shell_network(),
            read_deny: Vec::new(),
            restrict_home: false,
            read_jail: false,
        }
    }
}

fn default_shell_network() -> String {
    "allow".to_string()
}

/// Project-scoped prompt-cache policy. Values are explicit; the runtime never
/// infers a provider TTL or enables automatic warming silently.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachePolicy {
    /// Master switch. Disabled means the adapter sends no cache markers and
    /// the runtime reports zero breakpoints.
    #[serde(default = "default_cache_enabled")]
    pub enabled: bool,
    /// Default TTL pool for Anthropic-compatible requests: `5m` (default) or
    /// `1h` (explicit extended pool). OpenAI-compatible endpoints receive no
    /// TTL field and use provider-managed prefix caching.
    #[serde(default = "default_cache_ttl")]
    pub ttl: String,
    /// Request the provider-managed moving breakpoint (Anthropic top-level
    /// `cache_control`). When false, the adapter may mark the last cacheable
    /// message block instead. Default true.
    #[serde(default = "default_cache_automatic")]
    pub automatic_caching: bool,
    /// Opt-in one-shot warm before resuming a journaled run. Default false;
    /// no provider call is added unless the operator explicitly enables this.
    #[serde(default)]
    pub warm_on_resume: bool,
    /// Record compaction cache generations and an estimated invalidation
    /// cost. Default true. This never delays or blocks compaction.
    #[serde(default = "default_cache_compact_aware")]
    pub compact_aware: bool,
    /// Optional per-role TTL overrides. Keys are `leader`, `manager`, `worker`,
    /// `verifier`, `labour`, or `scout`; values are `5m` or `1h`.
    #[serde(default)]
    pub role_ttl: BTreeMap<String, String>,
    /// Instruction/prompt layout used for newly created sessions. `3` is the
    /// content-addressed section layout; `2` is the legacy combined layout and
    /// remains available for rollback. Existing sessions keep their own
    /// persisted layout regardless of this project default.
    #[serde(default = "default_prompt_layout")]
    pub prompt_layout: u32,
}

impl Default for CachePolicy {
    fn default() -> Self {
        Self {
            enabled: default_cache_enabled(),
            ttl: default_cache_ttl(),
            automatic_caching: default_cache_automatic(),
            warm_on_resume: false,
            compact_aware: default_cache_compact_aware(),
            role_ttl: BTreeMap::new(),
            prompt_layout: default_prompt_layout(),
        }
    }
}

pub fn default_prompt_layout() -> u32 {
    3
}

/// Session-scoped prompt layout. Legacy sessions deserialize as `2`; a newly
/// created session is written with the project default (normally `3`).
pub fn default_session_prompt_layout() -> u32 {
    2
}

pub fn normalize_prompt_layout(value: u32) -> u32 {
    match value {
        2 => 2,
        _ => 3,
    }
}

fn default_cache_enabled() -> bool {
    true
}

fn default_cache_ttl() -> String {
    "5m".to_string()
}

fn default_cache_automatic() -> bool {
    true
}

fn default_cache_compact_aware() -> bool {
    true
}

impl CachePolicy {
    pub const FIVE_MINUTES: &'static str = "5m";
    pub const ONE_HOUR: &'static str = "1h";

    /// Validate and normalize the policy. Unknown TTLs and roles are rejected
    /// so a typo cannot silently change cache cost.
    pub fn normalized(&self) -> std::result::Result<Self, String> {
        let mut policy = self.clone();
        policy.ttl = policy.ttl.trim().to_ascii_lowercase();
        if !matches!(policy.ttl.as_str(), Self::FIVE_MINUTES | Self::ONE_HOUR) {
            return Err(format!(
                "cache ttl must be '5m' or '1h', got '{}'",
                self.ttl
            ));
        }
        let mut roles = BTreeMap::new();
        for (role, ttl) in &policy.role_ttl {
            let role = role.trim().to_ascii_lowercase();
            if !matches!(
                role.as_str(),
                "leader" | "manager" | "worker" | "verifier" | "labour" | "scout"
            ) {
                return Err(format!("unknown role in cache role_ttl: '{role}'"));
            }
            let ttl = ttl.trim().to_ascii_lowercase();
            if !matches!(ttl.as_str(), Self::FIVE_MINUTES | Self::ONE_HOUR) {
                return Err(format!(
                    "cache role_ttl for '{role}' must be '5m' or '1h', got '{ttl}'"
                ));
            }
            roles.insert(role, ttl);
        }
        policy.role_ttl = roles;
        if !matches!(policy.prompt_layout, 2 | 3) {
            return Err(format!(
                "prompt_layout must be 2 or 3, got {}",
                policy.prompt_layout
            ));
        }
        Ok(policy)
    }

    /// Layout used for new sessions. Values outside `2|3` fail closed to the
    /// current layout rather than silently enabling an unknown prompt shape.
    pub fn session_prompt_layout(&self) -> u32 {
        normalize_prompt_layout(self.prompt_layout)
    }

    /// TTL selected for one role. A missing override uses the project default.
    pub fn ttl_for(&self, role: &str) -> &str {
        self.role_ttl
            .get(role)
            .map(String::as_str)
            .unwrap_or(&self.ttl)
    }

    pub fn ttl_is_one_hour(&self, role: &str) -> bool {
        self.ttl_for(role) == Self::ONE_HOUR
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CachePolicyParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetCachePolicyParams {
    pub project_id: String,
    pub policy: CachePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CachePolicyResult {
    pub project_id: String,
    pub policy: CachePolicy,
}

/// How the runtime decides whether an elevated tool call needs a human.
///
/// `Smart` is the default: routine workspace writes and ordinary shell
/// commands run immediately, while commands that can change host state ask
/// first. `Strict` restores the older ask-before-every-write behavior.
/// `Unattended` never blocks on a human; host-level operations are denied.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    #[default]
    Smart,
    Strict,
    Unattended,
}

impl ApprovalMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Smart => "smart",
            Self::Strict => "strict",
            Self::Unattended => "unattended",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "smart" => Some(Self::Smart),
            "strict" => Some(Self::Strict),
            "unattended" => Some(Self::Unattended),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskSchedulerPolicy {
    /// Default-on. New projects begin with the autonomous dispatcher enabled;
    /// an explicit false row (or `VERLINPARK_TASK_SCHEDULER=0`) opts out.
    #[serde(default = "default_task_scheduler_enabled")]
    pub enabled: bool,
    #[serde(default = "default_task_scheduler_cooldown")]
    pub cooldown_secs: u64,
    /// When enabled, the scheduler dispatches one ready task per tick instead
    /// of resuming the whole stored plan.
    #[serde(default)]
    pub dispatch_tasks: bool,
    /// Dispatch standalone task-inbox items that are not attached to a stored
    /// TeamPlan. Intake tasks always run through a synthesized one-task plan.
    #[serde(default = "default_intake_enabled")]
    pub include_intake: bool,
    /// Maximum intake sessions that may be dispatching/running per project.
    #[serde(default = "default_intake_parallel")]
    pub max_intake_parallel: u32,
}

impl Default for TaskSchedulerPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            cooldown_secs: default_task_scheduler_cooldown(),
            dispatch_tasks: false,
            include_intake: true,
            max_intake_parallel: default_intake_parallel(),
        }
    }
}

fn default_task_scheduler_enabled() -> bool {
    true
}

fn default_task_scheduler_cooldown() -> u64 {
    60
}

fn default_intake_enabled() -> bool {
    true
}

fn default_intake_parallel() -> u32 {
    2
}

#[derive(Debug, Clone, Deserialize)]
pub struct TaskSchedulerPolicyParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetTaskSchedulerPolicyParams {
    pub project_id: String,
    pub policy: TaskSchedulerPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskSchedulerPolicyResult {
    pub project_id: String,
    pub policy: TaskSchedulerPolicy,
}

/// Whether a `TaskContract` is enforced or only observed.
///
/// `advisory` records every `intent.decision` and its rule but never blocks a
/// call or asks for approval because of the contract. Existing approval,
/// sandbox, Verifier, and merge gates still apply unchanged. `enforce` is the
/// default and is deliberately fail-closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContractMode {
    Advisory,
    #[default]
    Enforce,
}

impl ContractMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Advisory => "advisory",
            Self::Enforce => "enforce",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "advisory" | "observe" => Some(Self::Advisory),
            "enforce" | "enforced" => Some(Self::Enforce),
            _ => None,
        }
    }
}

/// Project-scoped task-contract policy.
///
/// The contract is derived from trusted Leader-authored plan text and this
/// policy only. It can narrow the existing tool/path/network policy; it can
/// never widen an approval, sandbox, path-jail, or merge gate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContractPolicy {
    /// Master switch. Disabled means no contract is derived or enforced.
    /// Default true for new and upgraded projects.
    #[serde(default = "default_contract_enabled")]
    pub enabled: bool,
    /// `advisory` records decisions only; `enforce` escalates or denies.
    #[serde(default)]
    pub mode: ContractMode,
    /// When false, shell argument mismatches are recorded as advisory even in
    /// enforce mode. The existing shell risk classifier still prompts for
    /// host-state/destructive commands. Default false.
    #[serde(default)]
    pub enforce_shell: bool,
    /// Worktree-relative paths that no role may mutate through a contracted
    /// call. A match is a contract denial, not an approval prompt.
    #[serde(default)]
    pub forbidden_paths: Vec<String>,
    /// Hosts or `*.suffix` entries accepted by the contract egress policy.
    /// Empty means no host-level allowlist; the project `ShellPolicy.network`
    /// mode still applies.
    #[serde(default)]
    pub egress_allowlist: Vec<String>,
    /// Optional per-contract mutation caps. Exceeding a cap escalates to a
    /// human; it is never silently allowed.
    #[serde(default)]
    pub max_writes: Option<u64>,
    #[serde(default)]
    pub max_bytes: Option<u64>,
    #[serde(default)]
    pub max_commits: Option<u64>,
}

impl Default for ContractPolicy {
    fn default() -> Self {
        Self {
            enabled: default_contract_enabled(),
            mode: ContractMode::Enforce,
            enforce_shell: false,
            forbidden_paths: Vec::new(),
            egress_allowlist: Vec::new(),
            max_writes: None,
            max_bytes: None,
            max_commits: None,
        }
    }
}

fn default_contract_enabled() -> bool {
    true
}

impl ContractPolicy {
    /// Normalize allowlist/path entries and validate bounds. Unknown values are
    /// rejected rather than silently changing enforcement.
    pub fn normalized(&self) -> std::result::Result<Self, String> {
        let mut policy = self.clone();
        policy.forbidden_paths = policy
            .forbidden_paths
            .iter()
            .map(|path| path.trim().to_string())
            .filter(|path| !path.is_empty())
            .collect();
        for path in &policy.forbidden_paths {
            if path.starts_with('/') || path.starts_with('~') || path.contains("..") {
                return Err(format!(
                    "forbidden path must be worktree-relative without '..': {path}"
                ));
            }
        }
        policy.egress_allowlist = policy
            .egress_allowlist
            .iter()
            .map(|host| host.trim().to_ascii_lowercase())
            .filter(|host| !host.is_empty())
            .collect();
        for host in &policy.egress_allowlist {
            if !valid_egress_entry(host) {
                return Err(format!("invalid egress allowlist entry: {host}"));
            }
        }
        Ok(policy)
    }
}

fn valid_egress_entry(entry: &str) -> bool {
    if entry == "*" {
        return true;
    }
    let value = entry
        .strip_prefix("*.")
        .or_else(|| entry.strip_prefix("https://"))
        .or_else(|| entry.strip_prefix("http://"))
        .unwrap_or(entry);
    let host = value.split('/').next().unwrap_or(value);
    !host.is_empty()
        && host
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | ':' | '[' | ']'))
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContractPolicyParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetContractPolicyParams {
    pub project_id: String,
    pub policy: ContractPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContractPolicyResult {
    pub project_id: String,
    pub policy: ContractPolicy,
}

/// Bounded, text-free contract metadata for RPC/CLI/UI. The canonical
/// contract body stays in the daemon store and is never rendered as prompt
/// text through this surface.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskContractMetadata {
    #[serde(default)]
    pub contract_id: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub plan_revision: u64,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub source_hash: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub allowed_tools_count: u64,
    #[serde(default)]
    pub allowed_paths_count: u64,
    #[serde(default)]
    pub forbidden_paths_count: u64,
    #[serde(default)]
    pub allowed_paths_preview: Vec<String>,
    #[serde(default)]
    pub egress: String,
    #[serde(default)]
    pub unrestricted_scope: bool,
    #[serde(default)]
    pub acceptance_count: u64,
    #[serde(default)]
    pub max_writes: Option<u64>,
    #[serde(default)]
    pub max_bytes: Option<u64>,
    #[serde(default)]
    pub max_commits: Option<u64>,
    #[serde(default)]
    pub writes_used: u64,
    #[serde(default)]
    pub bytes_used: u64,
    #[serde(default)]
    pub commits_used: u64,
    #[serde(default)]
    pub enforcement_mode: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionContractParams {
    pub session_id: String,
    #[serde(default)]
    pub task_id: Option<String>,
}

/// Stable effect event kinds. Each payload contains
/// `{"receipt": <EffectReceipt>}`; a reconcile adds a bounded `operator`.
pub const EFFECT_INTENT_EVENT: &str = "effect.intent";
pub const EFFECT_SETTLED_EVENT: &str = "effect.settled";
pub const EFFECT_FAILED_EVENT: &str = "effect.failed";
pub const EFFECT_UNKNOWN_EVENT: &str = "effect.unknown";
pub const EFFECT_RECONCILED_EVENT: &str = "effect.reconciled";

/// Lifecycle of one local effect receipt.
///
/// - `intent` is written in the same transaction as the tool intent, before
///   the effect can happen.
/// - `done` means the local runtime observed the declared postcondition.
/// - `failed` means the postcondition was not observed and the call reported a
///   failure; it may be retried as a new attempt row or reconciled.
/// - `unknown` means the runtime cannot prove whether the effect happened.
///   It requires explicit operator reconciliation and never auto-retries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectState {
    #[default]
    Intent,
    Done,
    Failed,
    Unknown,
}

impl EffectState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "intent" | "pending" => Some(Self::Intent),
            "done" | "settled" | "succeeded" => Some(Self::Done),
            "failed" | "error" => Some(Self::Failed),
            "unknown" | "ambiguous" => Some(Self::Unknown),
            _ => None,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed)
    }

    pub fn blocks_merge(self) -> bool {
        matches!(self, Self::Intent | Self::Unknown)
    }
}

/// Secret-free canonical effect target. `key` is stable across retries and is
/// derived from trusted arguments, never from model prose or tool output.
/// Examples: `workspace_write:src/lib.rs`, `git_push:origin/feature/x`,
/// `forge_pr:github/owner/repo:feature/x->main`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EffectTarget {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub key: String,
}

impl EffectTarget {
    pub fn new(kind: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            key: key.into(),
        }
    }

    /// Canonical single-column representation used by the receipt table and
    /// its unique index. The first `:` separates kind from key.
    pub fn canonical(&self) -> String {
        format!("{}:{}", self.kind, self.key)
    }

    pub fn parse(value: &str) -> Self {
        match value.split_once(':') {
            Some((kind, key)) => Self::new(kind, key),
            None => Self::new(value, ""),
        }
    }

    pub fn label(&self) -> String {
        if self.key.is_empty() {
            self.kind.clone()
        } else {
            format!("{}:{}", self.kind, self.key)
        }
    }
}

/// One observed path in a postcondition probe. `status` is one of `created`,
/// `modified`, `deleted`, `unchanged`, `missing`, `unexpected`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectPathEvidence {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub exists: bool,
    #[serde(default)]
    pub sha256: Option<String>,
}

/// Bounded postcondition evidence. It never contains command output, file
/// content, URLs with credentials, provider keys, or free-form model text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectEvidence {
    /// Probe identifier, for example `workspace_sha256` or `git_ls_remote`.
    #[serde(default)]
    pub probe: String,
    /// `observed`, `mismatch`, `unverifiable`, `declared`, `tool_error`, or
    /// `reconciled`.
    #[serde(default)]
    pub outcome: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub paths: Vec<EffectPathEvidence>,
    #[serde(default)]
    pub exit_code: Option<i64>,
    #[serde(default)]
    pub http_status: Option<u16>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub external_id: Option<String>,
    /// Forge change name (`pull request`, `merge request`, `PR`) or the
    /// remote integration id. Bounded and never secret.
    #[serde(default)]
    pub change: Option<String>,
    /// Host/provider repository identifier. Not a filesystem path.
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    /// Honest boundary statement. For HTTP it says the response does not prove
    /// a remote side effect; for remote writes it says the helper reported the
    /// postcondition.
    #[serde(default)]
    pub caveat: Option<String>,
    /// True when a stored receipt was returned instead of executing again.
    #[serde(default)]
    pub replayed: bool,
    #[serde(default)]
    pub tool_error_code: Option<String>,
}

impl EffectEvidence {
    pub fn declared(probe: impl Into<String>) -> Self {
        Self {
            probe: probe.into(),
            outcome: "declared".to_string(),
            ..Self::default()
        }
    }

    pub fn observed(probe: impl Into<String>) -> Self {
        Self {
            probe: probe.into(),
            outcome: "observed".to_string(),
            ..Self::default()
        }
    }

    pub fn unverifiable(probe: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            probe: probe.into(),
            outcome: "unverifiable".to_string(),
            reason: Some(reason.into()),
            ..Self::default()
        }
    }

    pub fn has_resolution_evidence(&self) -> bool {
        self.sha256
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            || self
                .commit
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            || self
                .external_id
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            || self.http_status.is_some()
            || self
                .note
                .as_deref()
                .is_some_and(|value| value.trim().chars().count() >= 8)
    }
}

/// One persisted effect receipt. `done` is terminal and replays the stored
/// evidence; `unknown` requires `effect.reconcile`; `failed` may retry only as
/// a new attempt row or an explicit reconcile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectReceipt {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub attempt: u64,
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub target: EffectTarget,
    #[serde(default)]
    pub request_hash: String,
    #[serde(default)]
    pub state: EffectState,
    #[serde(default)]
    pub provider_key: Option<String>,
    #[serde(default)]
    pub external_id: Option<String>,
    #[serde(default)]
    pub response_hash: Option<String>,
    #[serde(default)]
    pub evidence: EffectEvidence,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub resolved_at: Option<String>,
}

impl EffectReceipt {
    pub fn target_label(&self) -> String {
        self.target.label()
    }

    pub fn blocks_merge(&self) -> bool {
        self.state.blocks_merge()
    }
}

/// How the runtime treats raw shell effects.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectShellMode {
    /// Only shell commands whose risk classifier is `external` or higher are
    /// recorded as effects. Workspace-write shell stays covered by the
    /// worktree sandbox and the Verifier/Integrator diff, not by a receipt.
    #[default]
    ExternalOnly,
    /// Every shell command that the classifier does not mark read-only is
    /// recorded. A raw shell effect is `unknown` unless the call declares an
    /// explicit postcondition probe.
    All,
}

impl EffectShellMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExternalOnly => "external_only",
            Self::All => "all",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "external_only" | "external" => Some(Self::ExternalOnly),
            "all" | "every" => Some(Self::All),
            _ => None,
        }
    }
}

/// Project-scoped effect-receipt policy. Every field defaults to the safe,
/// narrow behavior so an older project decodes without changing its gates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectPolicy {
    /// Master switch. Disabled means no receipts are created and the merge
    /// gate is not applied. Default true for new and upgraded projects.
    #[serde(default = "default_effect_enabled")]
    pub enabled: bool,
    /// When true (default), `intent`/`unknown` receipts block merge queue
    /// integration and local landing. A project may observe without blocking
    /// only by explicitly turning this off.
    #[serde(default = "default_effect_unknown_blocks_merge")]
    pub unknown_blocks_merge: bool,
    /// Grace period before a stale `intent` moves to `unknown` after a daemon
    /// restart. Bounded 1..=86_400 seconds.
    #[serde(default = "default_effect_stale_grace")]
    pub stale_intent_grace_secs: u64,
    #[serde(default)]
    pub shell_mode: EffectShellMode,
    /// Record mutating `local_http` calls. Default true; read-only methods are
    /// never recorded by this flag.
    #[serde(default = "default_effect_track_mutating_http")]
    pub track_mutating_http: bool,
    /// Record `fetch_url` GET observations. Default false: a GET is a read
    /// observation, not an effect. When enabled the receipt is `done` with an
    /// explicit "no remote side effect proven" caveat.
    #[serde(default)]
    pub track_read_http: bool,
    /// Record MCP calls as `unknown` effects. Default true; a mutating MCP tool
    /// has no stable local postcondition probe.
    #[serde(default = "default_effect_track_mcp")]
    pub track_mcp_mutations: bool,
    /// Record workspace writes. Default true so a daemon restart can replay a
    /// completed write from the receipt even when the run journal is gone.
    #[serde(default = "default_effect_track_workspace")]
    pub track_workspace_writes: bool,
}

impl Default for EffectPolicy {
    fn default() -> Self {
        Self {
            enabled: default_effect_enabled(),
            unknown_blocks_merge: default_effect_unknown_blocks_merge(),
            stale_intent_grace_secs: default_effect_stale_grace(),
            shell_mode: EffectShellMode::ExternalOnly,
            track_mutating_http: default_effect_track_mutating_http(),
            track_read_http: false,
            track_mcp_mutations: default_effect_track_mcp(),
            track_workspace_writes: default_effect_track_workspace(),
        }
    }
}

fn default_effect_enabled() -> bool {
    true
}

fn default_effect_unknown_blocks_merge() -> bool {
    true
}

fn default_effect_stale_grace() -> u64 {
    120
}

fn default_effect_track_mutating_http() -> bool {
    true
}

fn default_effect_track_mcp() -> bool {
    true
}

fn default_effect_track_workspace() -> bool {
    true
}

impl EffectPolicy {
    pub const MAX_STALE_GRACE_SECS: u64 = 86_400;

    pub fn normalized(&self) -> std::result::Result<Self, String> {
        let policy = self.clone();
        if policy.stale_intent_grace_secs == 0
            || policy.stale_intent_grace_secs > Self::MAX_STALE_GRACE_SECS
        {
            return Err(format!(
                "stale_intent_grace_secs must be 1..={}",
                Self::MAX_STALE_GRACE_SECS
            ));
        }
        Ok(policy)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EffectPolicyParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetEffectPolicyParams {
    pub project_id: String,
    pub policy: EffectPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffectPolicyResult {
    pub project_id: String,
    pub policy: EffectPolicy,
}

/// Per-state receipt counts. `blocking` counts `intent` plus `unknown` rows
/// that the active policy treats as blocking.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectCounts {
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub intent: u64,
    #[serde(default)]
    pub done: u64,
    #[serde(default)]
    pub failed: u64,
    #[serde(default)]
    pub unknown: u64,
    #[serde(default)]
    pub blocking: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionEffectsParams {
    pub session_id: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    /// Include `done`/`failed` rows. Default true. When false only unresolved
    /// `intent`/`unknown` rows are returned.
    #[serde(default)]
    pub include_settled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEffectsResult {
    pub session_id: String,
    pub policy: EffectPolicy,
    pub receipts: Vec<EffectReceipt>,
    pub counts: EffectCounts,
    /// True when at least one receipt blocks merge under the active policy.
    pub blocked: bool,
    #[serde(default)]
    pub block_reason: Option<String>,
    #[serde(default)]
    pub truncated: bool,
}

/// Operator-supplied evidence for `effect.reconcile`. At least one bounded
/// field must be present for a `done` decision.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectReconcileEvidence {
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub external_id: Option<String>,
    #[serde(default)]
    pub http_status: Option<u16>,
    #[serde(default)]
    pub note: Option<String>,
    /// The probe identifier the operator used, e.g. `remote_stat` or
    /// `git_ls_remote`. Defaults to `operator_reconcile`.
    #[serde(default)]
    pub probe: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EffectReconcileParams {
    #[serde(default)]
    pub session_id: Option<String>,
    pub effect_id: String,
    /// `done` or `failed`; `intent`/`unknown` are not valid decisions.
    pub decision: EffectState,
    #[serde(default)]
    pub evidence: EffectReconcileEvidence,
    #[serde(default)]
    pub operator: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectReconcileResult {
    pub receipt: EffectReceipt,
    /// False when the receipt already had the requested terminal state.
    pub changed: bool,
}

/// Kinds of external gate. The runtime deliberately has no provider-specific
/// logic: a gate is an opaque, authenticated wait.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateKind {
    Ci,
    PrReview,
    Deploy,
    Human,
    #[default]
    Custom,
}

impl GateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ci => "ci",
            Self::PrReview => "pr_review",
            Self::Deploy => "deploy",
            Self::Human => "human",
            Self::Custom => "custom",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "ci" => Some(Self::Ci),
            "pr_review" | "pr-review" | "review" => Some(Self::PrReview),
            "deploy" => Some(Self::Deploy),
            "human" => Some(Self::Human),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateState {
    #[default]
    Open,
    Resolved,
    Expired,
    Cancelled,
}

impl GateState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Resolved => "resolved",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "open" => Some(Self::Open),
            "resolved" => Some(Self::Resolved),
            "expired" => Some(Self::Expired),
            "cancelled" | "canceled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Open)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateOutcome {
    Resolved,
    Rejected,
    Failed,
}

impl GateOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "resolved" | "approved" | "passed" => Some(Self::Resolved),
            "rejected" | "denied" => Some(Self::Rejected),
            "failed" | "error" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// Public gate projection. It deliberately has no token or token hash: the
/// one-time token is returned only by `gate.open`, and the hash stays inside
/// the server-side event/store record.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExternalGate {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub kind: GateKind,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub state: GateState,
    #[serde(default)]
    pub outcome: Option<GateOutcome>,
    /// Bounded, secret-free caller context (for example a PR number or CI
    /// workflow label). It is never used for authorization.
    #[serde(default)]
    pub payload: Value,
    #[serde(default)]
    pub resolution_note: Option<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub resolved_at: Option<String>,
    #[serde(default)]
    pub resolved_by: Option<String>,
}

impl ExternalGate {
    pub fn is_open(&self) -> bool {
        self.state == GateState::Open
    }

    pub fn is_terminal(&self) -> bool {
        self.state.is_terminal()
    }

    pub fn kind_label(&self) -> &'static str {
        self.kind.as_str()
    }
}

/// Server-side gate record used by event payloads, SQLite projections, and
/// bundle/rebuild restore. The hash verifies the one-time token; it is not the
/// token and is never returned by an RPC or decoded by the Swift client.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GateRecord {
    #[serde(default)]
    pub gate: ExternalGate,
    #[serde(default)]
    pub token_hash: String,
}

pub const GATE_OPENED_EVENT: &str = "gate.opened";
pub const GATE_RESOLVED_EVENT: &str = "gate.resolved";
pub const GATE_EXPIRED_EVENT: &str = "gate.expired";
pub const GATE_CANCELLED_EVENT: &str = "gate.cancelled";
pub const GATE_CONTINUATION_READY_EVENT: &str = "gate.continuation_ready";

/// Project-scoped gate policy. The default is local-only and disables the
/// optional loopback resolve endpoint. The daemon was already loopback-bound;
/// this policy is an additional explicit opt-in, not a public listener.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatePolicy {
    #[serde(default)]
    pub loopback_resolve: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GatePolicyParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetGatePolicyParams {
    pub project_id: String,
    pub policy: GatePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GatePolicyResult {
    pub project_id: String,
    pub policy: GatePolicy,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GateOpenParams {
    pub session_id: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub payload: Option<Value>,
    /// Absolute RFC3339 expiry.
    #[serde(default)]
    pub expires_at: Option<String>,
    /// Relative expiry in seconds. Mutually exclusive with `expires_at`.
    #[serde(default)]
    pub after_secs: Option<u64>,
}

/// Result of opening a gate. `token` is the only occurrence of the one-time
/// secret; retry/duplicate calls cannot recover it.
#[derive(Clone, PartialEq, Serialize)]
pub struct GateOpenResult {
    pub gate: ExternalGate,
    pub token: String,
    /// Non-secret hint for the operator or same-host CI.
    pub resolve_hint: String,
    /// True only when the project opted into the loopback resolve endpoint.
    /// The daemon remains loopback-bound; this is an additional policy gate,
    /// not a public listener.
    #[serde(default)]
    pub loopback_resolve: bool,
}

impl std::fmt::Debug for GateOpenResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GateOpenResult")
            .field("gate", &self.gate)
            .field("token", &"redacted")
            .field("resolve_hint", &self.resolve_hint)
            .field("loopback_resolve", &self.loopback_resolve)
            .finish()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GateResolveParams {
    pub gate_id: String,
    /// One-time scoped token from `gate.open`. The daemon Bearer token is not
    /// a substitute and cannot bypass this check.
    pub token: String,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub resolved_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateResolveResult {
    pub gate: ExternalGate,
    /// False when the same token/outcome replay observed an already resolved
    /// gate. A different outcome or token fails closed.
    pub changed: bool,
    #[serde(default)]
    pub continuation_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GateCancelParams {
    pub gate_id: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub cancelled_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateCancelResult {
    pub gate: ExternalGate,
    pub changed: bool,
    #[serde(default)]
    pub continuation_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GateListParams {
    pub session_id: String,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateCounts {
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub open: u64,
    #[serde(default)]
    pub resolved: u64,
    #[serde(default)]
    pub expired: u64,
    #[serde(default)]
    pub cancelled: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateListResult {
    pub session_id: String,
    #[serde(default)]
    pub gates: Vec<ExternalGate>,
    #[serde(default)]
    pub counts: GateCounts,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GateStatusParams {
    pub gate_id: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GateSweepParams {
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateSweepResult {
    #[serde(default)]
    pub expired_gates: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateStatusResult {
    pub gate: ExternalGate,
    #[serde(default)]
    pub continuation_id: Option<String>,
    #[serde(default)]
    pub continuation_status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionContractResult {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub contract: Option<TaskContractMetadata>,
    #[serde(default)]
    pub policy: ContractPolicy,
    #[serde(default)]
    pub recent_decisions: Vec<IntentDecisionRecord>,
}

/// One bounded `intent.decision` audit row. It contains no argument values,
/// prompt text, or secrets; only tool name, rule, decision, and risk.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentDecisionRecord {
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub contract_hash: String,
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub rule: String,
    #[serde(default)]
    pub decision: String,
    #[serde(default)]
    pub risk: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpToolPin {
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub server_id: String,
    #[serde(default)]
    pub server_name: String,
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub server_fingerprint: String,
    #[serde(default)]
    pub schema_hash: String,
    #[serde(default)]
    pub description_hash: String,
    /// Last observed hashes, which may diverge from the approved hashes when
    /// `status` is `drift`. They never overwrite the approved values.
    #[serde(default)]
    pub observed_schema_hash: String,
    #[serde(default)]
    pub observed_description_hash: String,
    #[serde(default)]
    pub observed_server_fingerprint: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub approved_at: Option<String>,
    #[serde(default)]
    pub approved_by: Option<String>,
    #[serde(default)]
    pub last_seen_at: String,
    #[serde(default)]
    pub description_preview: String,
}

impl McpToolPin {
    pub fn callable(&self) -> bool {
        self.status == "pinned"
    }

    pub fn trust(&self) -> &'static str {
        match self.status.as_str() {
            "pinned" => "approved",
            "drift" => "drift",
            "unverified" => "unverified",
            "removed" => "removed",
            _ => "unknown",
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProjectMcpPinsParams {
    pub project_id: String,
    #[serde(default)]
    pub server_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectMcpPinsResult {
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub pins: Vec<McpToolPin>,
    #[serde(default)]
    pub pinned: u64,
    #[serde(default)]
    pub drift: u64,
    #[serde(default)]
    pub unverified: u64,
    #[serde(default)]
    pub removed: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpPinApproveParams {
    pub project_id: String,
    pub server_id: String,
    pub tool_name: String,
    pub expected_schema_hash: String,
    pub expected_description_hash: String,
    #[serde(default)]
    pub approved_by: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpPinRejectParams {
    pub project_id: String,
    pub server_id: String,
    pub tool_name: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpPinActionResult {
    #[serde(default)]
    pub pin: Option<McpToolPin>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SchedulerStatusParams {
    #[serde(default)]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchedulerProjectStatus {
    pub project_id: String,
    pub project_name: String,
    pub enabled: bool,
    pub cooldown_secs: u64,
    pub dispatch_tasks: bool,
    pub include_intake: bool,
    pub max_intake_parallel: u32,
    pub pending_sessions: usize,
    pub pending_plan_tasks: usize,
    pub running_plan_tasks: usize,
    pub queued_intake: usize,
    pub active_intake: usize,
    pub failed_intake: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchedulerStatusResult {
    pub active_sessions: usize,
    pub queued_sessions: usize,
    pub paused_sessions: usize,
    pub pending_plan_tasks: usize,
    pub running_plan_tasks: usize,
    pub queued_intake: usize,
    pub active_intake: usize,
    pub failed_intake: usize,
    pub global_session_cap: usize,
    pub global_worker_cap: usize,
    pub global_labour_cap: usize,
    pub projects: Vec<SchedulerProjectStatus>,
}

/// How the budget governor protects remaining plan work.
///
/// `single` is the original F5 behavior: only the heaviest remaining chain is
/// protected. `multi` enumerates bounded root-to-leaf paths and water-fills
/// the reserve across the top paths with per-department floors. The default is
/// `single`, so an upgraded daemon with an existing policy changes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CriticalPathMode {
    #[default]
    Single,
    Multi,
}

impl CriticalPathMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Multi => "multi",
        }
    }

    /// Tolerant parser used by CLI/manual configuration. Unknown values are
    /// rejected instead of silently falling back to a mode that would change
    /// admission behavior.
    pub fn parse(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
        match normalized.as_str() {
            "single" | "single_path" | "critical_path" => Some(Self::Single),
            "multi" | "multi_path" | "fair" | "fairness" => Some(Self::Multi),
            _ => None,
        }
    }
}

/// Project-scoped critical-path budget governor policy. The governor is
/// deliberately opt-in: a missing row or an older manifest decodes as
/// `enabled = false`, and only an explicit operator opt-in changes task
/// admission. Policy changes never alter model/provider routing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CriticalPathPolicy {
    #[serde(default)]
    pub enabled: bool,
    /// Fraction of the remaining session token budget held back for the
    /// longest remaining plan path, in addition to the path's own weight.
    #[serde(default = "default_critical_path_reserve_ratio")]
    pub reserve_ratio: f64,
    /// Remaining-budget fraction at which non-critical provider calls are
    /// throttled. `0.0` disables the throttle zone; `1.0` throttles whenever
    /// the critical path still has unfinished work.
    #[serde(default = "default_critical_path_throttle_at_ratio")]
    pub throttle_at_ratio: f64,
    /// Added to a critical task's effective scheduling priority while the
    /// governor is enabled. Range 0..100; it never changes the plan itself.
    #[serde(default = "default_critical_path_boost")]
    pub critical_boost: i32,
    /// Hard floor: below this remaining session budget no task is admitted.
    #[serde(default = "default_critical_path_min_task_budget")]
    pub min_task_budget: u64,
    /// `single` (default) protects the heaviest chain exactly like F5.
    /// `multi` protects up to `max_paths` bounded root-to-leaf paths.
    #[serde(default)]
    pub mode: CriticalPathMode,
    /// Maximum number of root-to-leaf paths in the multi-path table. The
    /// protocol range is 2..=16; single mode does not use it.
    #[serde(default = "default_critical_path_max_paths")]
    pub max_paths: u32,
    /// Per-department minimum share of the protected reserve, range 0.0..=1.0.
    /// Floors are applied before weighted water-filling. `0.0` disables floors.
    #[serde(default)]
    pub department_min_ratio: f64,
    /// Blend of observed usage-ledger spend into path weights, range 0.0..=1.0.
    /// `0.0` (default) uses declared task budgets only.
    #[serde(default)]
    pub observed_weight_ratio: f64,
    /// After this many seconds queued, a deferred task receives deterministic
    /// aging credit. `0` disables starvation aging.
    #[serde(default = "default_critical_path_starvation_after_secs")]
    pub starvation_after_secs: u64,
}

impl Default for CriticalPathPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            reserve_ratio: default_critical_path_reserve_ratio(),
            throttle_at_ratio: default_critical_path_throttle_at_ratio(),
            critical_boost: default_critical_path_boost(),
            min_task_budget: default_critical_path_min_task_budget(),
            mode: CriticalPathMode::Single,
            max_paths: default_critical_path_max_paths(),
            department_min_ratio: 0.0,
            observed_weight_ratio: 0.0,
            starvation_after_secs: default_critical_path_starvation_after_secs(),
        }
    }
}

fn default_critical_path_reserve_ratio() -> f64 {
    0.25
}

fn default_critical_path_throttle_at_ratio() -> f64 {
    0.35
}

fn default_critical_path_boost() -> i32 {
    25
}

fn default_critical_path_min_task_budget() -> u64 {
    1_000
}

fn default_critical_path_max_paths() -> u32 {
    4
}

fn default_critical_path_starvation_after_secs() -> u64 {
    1_800
}

fn default_governor_mode_name() -> String {
    CriticalPathMode::Single.as_str().to_string()
}

impl CriticalPathPolicy {
    pub const MAX_CRITICAL_BOOST: i32 = 100;
    pub const MAX_MIN_TASK_BUDGET: u64 = 10_000_000;
    pub const MIN_MAX_PATHS: u32 = 2;
    pub const MAX_MAX_PATHS: u32 = 16;
    pub const MAX_STARVATION_AFTER_SECS: u64 = 7 * 86_400;

    /// Clamp persisted or user-provided values into the daemon's effective
    /// ranges. Validation is separate so an RPC with an invalid value is
    /// rejected instead of silently changing admission behavior.
    pub fn normalized(mut self) -> Self {
        if !self.reserve_ratio.is_finite() {
            self.reserve_ratio = default_critical_path_reserve_ratio();
        }
        self.reserve_ratio = self.reserve_ratio.clamp(0.0, 1.0);
        if !self.throttle_at_ratio.is_finite() {
            self.throttle_at_ratio = default_critical_path_throttle_at_ratio();
        }
        self.throttle_at_ratio = self.throttle_at_ratio.clamp(0.0, 1.0);
        self.critical_boost = self.critical_boost.clamp(0, Self::MAX_CRITICAL_BOOST);
        self.min_task_budget = self.min_task_budget.min(Self::MAX_MIN_TASK_BUDGET);
        self.max_paths = self
            .max_paths
            .clamp(Self::MIN_MAX_PATHS, Self::MAX_MAX_PATHS);
        if !self.department_min_ratio.is_finite() {
            self.department_min_ratio = 0.0;
        }
        self.department_min_ratio = self.department_min_ratio.clamp(0.0, 1.0);
        if !self.observed_weight_ratio.is_finite() {
            self.observed_weight_ratio = 0.0;
        }
        self.observed_weight_ratio = self.observed_weight_ratio.clamp(0.0, 1.0);
        self.starvation_after_secs = self
            .starvation_after_secs
            .min(Self::MAX_STARVATION_AFTER_SECS);
        self
    }

    pub fn validate(&self) -> std::result::Result<(), String> {
        if !self.reserve_ratio.is_finite() || !(0.0..=1.0).contains(&self.reserve_ratio) {
            return Err("reserve_ratio must be a finite number between 0.0 and 1.0".to_string());
        }
        if !self.throttle_at_ratio.is_finite() || !(0.0..=1.0).contains(&self.throttle_at_ratio) {
            return Err(
                "throttle_at_ratio must be a finite number between 0.0 and 1.0".to_string(),
            );
        }
        if !(0..=Self::MAX_CRITICAL_BOOST).contains(&self.critical_boost) {
            return Err(format!(
                "critical_boost must be between 0 and {}",
                Self::MAX_CRITICAL_BOOST
            ));
        }
        if self.min_task_budget > Self::MAX_MIN_TASK_BUDGET {
            return Err(format!(
                "min_task_budget must be at most {}",
                Self::MAX_MIN_TASK_BUDGET
            ));
        }
        if !(Self::MIN_MAX_PATHS..=Self::MAX_MAX_PATHS).contains(&self.max_paths) {
            return Err(format!(
                "max_paths must be between {} and {}",
                Self::MIN_MAX_PATHS,
                Self::MAX_MAX_PATHS
            ));
        }
        if !self.department_min_ratio.is_finite()
            || !(0.0..=1.0).contains(&self.department_min_ratio)
        {
            return Err(
                "department_min_ratio must be a finite number between 0.0 and 1.0".to_string(),
            );
        }
        if !self.observed_weight_ratio.is_finite()
            || !(0.0..=1.0).contains(&self.observed_weight_ratio)
        {
            return Err(
                "observed_weight_ratio must be a finite number between 0.0 and 1.0".to_string(),
            );
        }
        if self.starvation_after_secs > Self::MAX_STARVATION_AFTER_SECS {
            return Err(format!(
                "starvation_after_secs must be at most {}",
                Self::MAX_STARVATION_AFTER_SECS
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CriticalPathPolicyParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetCriticalPathPolicyParams {
    pub project_id: String,
    pub policy: CriticalPathPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CriticalPathPolicyResult {
    pub project_id: String,
    pub policy: CriticalPathPolicy,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GovernorStatusParams {
    pub session_id: String,
    /// Bounded decision history size. Clamped by the daemon.
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GovernorPreviewParams {
    pub session_id: String,
    /// Deterministic preview override. When absent the daemon uses the live
    /// session ledger and in-flight reservations.
    #[serde(default)]
    pub remaining_tokens: Option<u64>,
    #[serde(default)]
    pub limit: Option<u32>,
    /// Optional `single`/`multi` mode override. Preview only: the stored
    /// project policy is never changed. Unknown values are rejected.
    #[serde(default)]
    pub mode: Option<String>,
}

/// Deterministic longest-remaining-path result for one plan revision.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CriticalPathInfo {
    pub plan_revision: u32,
    #[serde(default)]
    pub task_ids: Vec<String>,
    #[serde(default)]
    pub weight_tokens: u64,
    #[serde(default)]
    pub remaining_budget_tokens: Option<u64>,
    #[serde(default)]
    pub complete: bool,
    #[serde(default)]
    pub task_count: u32,
    #[serde(default)]
    pub edge_count: u32,
    #[serde(default)]
    pub inputs_hash: String,
}

/// Explainable reserve math for one decision. Every number is derived from the
/// persisted session ledger, the plan DAG, and the project policy.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GovernorReserveInfo {
    /// `disabled`, `no_budget`, `complete`, `active`, or `throttled`.
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub reserve_ratio: f64,
    #[serde(default)]
    pub throttle_at_ratio: f64,
    #[serde(default)]
    pub critical_boost: i32,
    #[serde(default)]
    pub min_task_budget: u64,
    #[serde(default)]
    pub limit_tokens: Option<u64>,
    #[serde(default)]
    pub spent_tokens: u64,
    #[serde(default)]
    pub reserved_tokens: u64,
    #[serde(default)]
    pub remaining_tokens: Option<u64>,
    #[serde(default)]
    pub path_weight_tokens: u64,
    #[serde(default)]
    pub ratio_reserve_tokens: u64,
    #[serde(default)]
    pub protected_tokens: u64,
    #[serde(default)]
    pub headroom_tokens: Option<u64>,
    #[serde(default)]
    pub throttled: bool,
    /// Effective governor mode: `single` or `multi`. Older payloads decode as
    /// `single`, matching the F5 compatibility default.
    #[serde(default = "default_governor_mode_name")]
    pub mode: String,
    /// Number of protected paths in the multi-path table (0 or 1 in single).
    #[serde(default)]
    pub path_count: u32,
    /// Sum of path allocations distributed from the reserve pool.
    #[serde(default)]
    pub allocated_tokens: u64,
    /// Sum of per-department/path floors applied before water-filling.
    #[serde(default)]
    pub floor_tokens: u64,
    /// Number of maximal paths enumerated before selection.
    #[serde(default)]
    pub enumerated_paths: u32,
    /// Enumeration hit its bound before exhausting the DAG; the table is a
    /// deterministic prefix, not a claim of completeness.
    #[serde(default)]
    pub paths_truncated: bool,
}

/// One bounded root-to-leaf path in the multi-path fairness table. Every
/// number is derived from the plan, the usage ledger, and the policy; the row
/// never contains provider or secret data.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GovernorPathInfo {
    /// Rank in the deterministic path order, 0 = heaviest path.
    #[serde(default)]
    pub path_id: u32,
    #[serde(default)]
    pub task_ids: Vec<String>,
    #[serde(default)]
    pub departments: Vec<String>,
    /// Blended weight (declared budget plus the optional observed-spend share).
    #[serde(default)]
    pub weight_tokens: u64,
    #[serde(default)]
    pub declared_weight_tokens: u64,
    #[serde(default)]
    pub observed_tokens: u64,
    /// Minimum reserve granted by department floors before water-filling.
    #[serde(default)]
    pub floor_tokens: u64,
    /// Reserve actually protected for this path.
    #[serde(default)]
    pub allocation_tokens: u64,
    /// Unfunded demand: `weight_tokens - allocation_tokens`.
    #[serde(default)]
    pub deficit_tokens: u64,
}

/// One department's share of the multi-path reserve before it is split across
/// paths. `weight_tokens` counts each selected path touching the department
/// once; `path_ids` names those paths. The table is bounded and read-only.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GovernorDepartmentInfo {
    #[serde(default)]
    pub department: String,
    #[serde(default)]
    pub weight_tokens: u64,
    #[serde(default)]
    pub floor_tokens: u64,
    #[serde(default)]
    pub path_ids: Vec<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GovernorTaskDecisionInfo {
    pub task_id: String,
    #[serde(default)]
    pub department: String,
    #[serde(default)]
    pub critical: bool,
    #[serde(default)]
    pub weight_tokens: u64,
    #[serde(default)]
    pub order: u32,
    /// `admit` or `defer`.
    pub action: String,
    /// Stable lowercase reason (`critical_admit`, `reserve`, `throttle`, ...).
    pub reason: String,
    /// Stable Q3 error-contract code for deferrals.
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub remaining_after_tokens: Option<u64>,
    /// Protected path that made the task critical, when applicable.
    #[serde(default)]
    pub path_id: Option<u32>,
    /// Every protected path containing the task (shared prefixes can belong
    /// to several paths), in table order.
    #[serde(default)]
    pub path_ids: Vec<u32>,
    /// Unfunded demand of the task's least-funded protected path.
    #[serde(default)]
    pub path_deficit_tokens: u64,
    /// Deterministic priority credit applied after `starvation_after_secs`.
    #[serde(default)]
    pub starvation_credit: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GovernorDeferralInfo {
    pub task_id: String,
    #[serde(default)]
    pub department: String,
    #[serde(default)]
    pub critical: bool,
    #[serde(default)]
    pub weight_tokens: u64,
    pub reason: String,
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub remaining_tokens: Option<u64>,
    #[serde(default)]
    pub protected_tokens: u64,
    #[serde(default)]
    pub path_id: Option<u32>,
    #[serde(default)]
    pub path_ids: Vec<u32>,
    #[serde(default)]
    pub path_deficit_tokens: u64,
    #[serde(default)]
    pub starvation_credit: i64,
}

/// One persisted `budget.governor.decision` event. Admission events carry the
/// ordered admitted/deferred lists; provider-call denial events carry a single
/// task and requested token estimate.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BudgetGovernorDecision {
    #[serde(default)]
    pub decision_id: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub plan_revision: u32,
    /// `admission` or `defer`.
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub critical: bool,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub path: Vec<String>,
    #[serde(default)]
    pub path_weight_tokens: u64,
    #[serde(default)]
    pub limit_tokens: Option<u64>,
    #[serde(default)]
    pub spent_tokens: u64,
    #[serde(default)]
    pub reserved_tokens: u64,
    #[serde(default)]
    pub remaining_tokens: Option<u64>,
    #[serde(default)]
    pub protected_tokens: u64,
    #[serde(default)]
    pub requested_tokens: Option<u64>,
    #[serde(default)]
    pub admitted: Vec<String>,
    #[serde(default)]
    pub deferred: Vec<GovernorDeferralInfo>,
    #[serde(default)]
    pub inputs_hash: String,
    #[serde(default)]
    pub created_at: String,
    /// Effective governor mode (`single` or `multi`). Older decisions decode
    /// as `single`.
    #[serde(default = "default_governor_mode_name")]
    pub mode: String,
    /// Bounded multi-path allocation table carried by the decision.
    #[serde(default)]
    pub paths: Vec<GovernorPathInfo>,
    /// Bounded per-department floor table carried by the decision.
    #[serde(default)]
    pub departments: Vec<GovernorDepartmentInfo>,
    #[serde(default)]
    pub allocated_tokens: u64,
    /// Deferred task ids in deterministic reconsideration order.
    #[serde(default)]
    pub deferred_order: Vec<String>,
}

/// Status for `session.budget.governor` and `governor.preview`. Preview differs
/// only in that it is computed with an optional remaining-token override and
/// never writes an event.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GovernorSessionStatus {
    pub session_id: String,
    pub project_id: String,
    #[serde(default)]
    pub session_status: String,
    #[serde(default)]
    pub plan_revision: u32,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub unlimited: bool,
    #[serde(default)]
    pub plan_present: bool,
    #[serde(default)]
    pub policy: CriticalPathPolicy,
    #[serde(default)]
    pub critical_path: Option<CriticalPathInfo>,
    #[serde(default)]
    pub reserve: GovernorReserveInfo,
    #[serde(default)]
    pub inputs_hash: String,
    #[serde(default)]
    pub decisions: Vec<GovernorTaskDecisionInfo>,
    #[serde(default)]
    pub admitted: Vec<String>,
    #[serde(default)]
    pub deferred: Vec<GovernorDeferralInfo>,
    #[serde(default)]
    pub last_decisions: Vec<BudgetGovernorDecision>,
    /// Deterministic multi-path allocation table. Single mode carries at most
    /// one row (the F5 critical path) and is entirely additive.
    #[serde(default)]
    pub paths: Vec<GovernorPathInfo>,
    /// Bounded per-department floor table in multi mode.
    #[serde(default)]
    pub departments: Vec<GovernorDepartmentInfo>,
    /// Deferred task ids in deterministic reconsideration order.
    #[serde(default)]
    pub deferred_order: Vec<String>,
    /// Present when the governor could not evaluate the plan (for example a
    /// cycle or an unknown dependency). It is a typed Q3 code, not free text.
    #[serde(default)]
    pub error: Option<GovernorFailureInfo>,
    pub generated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GovernorFailureInfo {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub hint: String,
}

fn default_gui_grace_secs() -> u64 {
    45
}

fn default_gui_wait_ms() -> u64 {
    15_000
}

fn default_gui_status_ttl_secs() -> u64 {
    30
}

/// How the daemon handles native GUI targets that commonly expose credentials
/// or security decisions. The default is a hard denial: an agent never
/// silently automates System Settings, SecurityAgent, or a password manager.
/// `require_approval` relies on the existing HostState/Destructive approval
/// decision for input actions and still denies read capture of a sensitive
/// target unless the call names that target explicitly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuiSensitiveMode {
    #[default]
    Deny,
    RequireApproval,
}

impl GuiSensitiveMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::RequireApproval => "require_approval",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "deny" => Some(Self::Deny),
            "require_approval" | "require-approval" | "approval" => Some(Self::RequireApproval),
            _ => None,
        }
    }
}

/// Project-scoped GUI session policy: grace, bounded wait, helper status cache,
/// and the sensitive-target list. All fields are additive and defaulted so an
/// older daemon payload decodes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GuiPolicy {
    /// Retain the logical `gui` lease this long after the last action. Helpers
    /// stay warm for their own idle window and do not hold the lease.
    #[serde(default = "default_gui_grace_secs")]
    pub grace_secs: u64,
    /// Bounded wait before a second session receives a typed `GUI_BUSY`.
    #[serde(default = "default_gui_wait_ms")]
    pub wait_ms: u64,
    /// Cache helper/TCC status for this long so dashboard refreshes do not
    /// spawn a temporary helper.
    #[serde(default = "default_gui_status_ttl_secs")]
    pub status_ttl_secs: u64,
    #[serde(default)]
    pub sensitive_mode: GuiSensitiveMode,
    /// Additional bundle identifiers appended to the built-in sensitive list.
    #[serde(default)]
    pub sensitive_bundles: Vec<String>,
    /// Additional case-insensitive title/application patterns appended to the
    /// built-in sensitive list.
    #[serde(default)]
    pub sensitive_titles: Vec<String>,
    /// Retain `gui.action` timeline events for this many days. `0` keeps them
    /// forever. Only `gui.action` events are eligible; the daemon never prunes
    /// another event kind for timeline retention. Default 30 days.
    #[serde(default = "default_gui_timeline_retention_days")]
    pub timeline_retention_days: u32,
}

fn default_gui_timeline_retention_days() -> u32 {
    30
}

impl Default for GuiPolicy {
    fn default() -> Self {
        Self {
            grace_secs: default_gui_grace_secs(),
            wait_ms: default_gui_wait_ms(),
            status_ttl_secs: default_gui_status_ttl_secs(),
            sensitive_mode: GuiSensitiveMode::Deny,
            sensitive_bundles: Vec::new(),
            sensitive_titles: Vec::new(),
            timeline_retention_days: default_gui_timeline_retention_days(),
        }
    }
}

impl GuiPolicy {
    pub const MAX_GRACE_SECS: u64 = 600;
    pub const MAX_WAIT_MS: u64 = 60_000;
    pub const MIN_STATUS_TTL_SECS: u64 = 5;
    pub const MAX_STATUS_TTL_SECS: u64 = 600;
    pub const MAX_SENSITIVE_ENTRIES: usize = 64;
    pub const MAX_SENSITIVE_ENTRY_CHARS: usize = 200;

    /// Clamp user input into the ranges the daemon enforces. Validation is
    /// separate so an invalid RPC value is rejected instead of silently
    /// changing behavior.
    pub fn normalized(mut self) -> Self {
        self.grace_secs = self.grace_secs.min(Self::MAX_GRACE_SECS);
        self.wait_ms = self.wait_ms.min(Self::MAX_WAIT_MS);
        self.status_ttl_secs = self
            .status_ttl_secs
            .clamp(Self::MIN_STATUS_TTL_SECS, Self::MAX_STATUS_TTL_SECS);
        self.sensitive_bundles = self
            .sensitive_bundles
            .into_iter()
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty())
            .take(Self::MAX_SENSITIVE_ENTRIES)
            .collect();
        self.sensitive_titles = self
            .sensitive_titles
            .into_iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .take(Self::MAX_SENSITIVE_ENTRIES)
            .collect();
        self.timeline_retention_days = self
            .timeline_retention_days
            .min(Self::MAX_TIMELINE_RETENTION_DAYS);
        self
    }

    /// `0` means keep forever. A non-zero policy is clamped to the documented
    /// maximum; validation rejects an out-of-range explicit update instead of
    /// silently changing it.
    pub const MAX_TIMELINE_RETENTION_DAYS: u32 = 3_650;

    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.grace_secs > Self::MAX_GRACE_SECS {
            return Err(format!(
                "gui grace_secs must be at most {}",
                Self::MAX_GRACE_SECS
            ));
        }
        if self.wait_ms > Self::MAX_WAIT_MS {
            return Err(format!("gui wait_ms must be at most {}", Self::MAX_WAIT_MS));
        }
        if !(Self::MIN_STATUS_TTL_SECS..=Self::MAX_STATUS_TTL_SECS).contains(&self.status_ttl_secs)
        {
            return Err(format!(
                "gui status_ttl_secs must be between {} and {}",
                Self::MIN_STATUS_TTL_SECS,
                Self::MAX_STATUS_TTL_SECS
            ));
        }
        if self.timeline_retention_days > Self::MAX_TIMELINE_RETENTION_DAYS {
            return Err(format!(
                "gui timeline_retention_days must be 0..={}",
                Self::MAX_TIMELINE_RETENTION_DAYS
            ));
        }
        for (name, values) in [
            ("sensitive_bundles", &self.sensitive_bundles),
            ("sensitive_titles", &self.sensitive_titles),
        ] {
            if values.len() > Self::MAX_SENSITIVE_ENTRIES {
                return Err(format!(
                    "{name} supports at most {} entries",
                    Self::MAX_SENSITIVE_ENTRIES
                ));
            }
            if values.iter().any(|value| {
                value.trim().is_empty() || value.chars().count() > Self::MAX_SENSITIVE_ENTRY_CHARS
            }) {
                return Err(format!(
                    "{name} entries must be 1-{} characters",
                    Self::MAX_SENSITIVE_ENTRY_CHARS
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GuiPolicyParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetGuiPolicyParams {
    pub project_id: String,
    pub policy: GuiPolicy,
}

#[derive(Debug, Clone, Serialize)]
pub struct GuiPolicyResult {
    pub project_id: String,
    pub policy: GuiPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GuiLeaseInfo {
    pub lease_id: String,
    pub owner_id: String,
    pub session_id: String,
    pub agent_id: String,
    pub agent_name: String,
    pub surface: String,
    pub lease_generation: u64,
    pub acquired_at: String,
    pub last_action_at: String,
    pub expires_at: String,
    #[serde(default)]
    pub queue_position: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GuiQueueInfo {
    pub session_id: String,
    pub agent_id: String,
    pub agent_name: String,
    pub surface: String,
    pub position: u32,
    pub waiting_ms: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GuiStatusParams {
    #[serde(default)]
    pub project_id: Option<String>,
}

/// Unified GUI surface status. `computer` keeps the original status payload;
/// `holder`/`queue` describe the single `gui` lease, not a worktree write
/// scope. A bounded `gui` queue is local to one daemon and is never claimed
/// across hosts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GuiStatusResult {
    pub runtime_id: String,
    pub lease_surface: String,
    #[serde(default)]
    pub holder: Option<GuiLeaseInfo>,
    #[serde(default)]
    pub queue: Vec<GuiQueueInfo>,
    pub policy: GuiPolicy,
    pub computer: ComputerStatusResult,
    pub browser_helper_available: bool,
    #[serde(default)]
    pub browser_helper_path: Option<String>,
    pub browser_sandbox_available: bool,
    pub warm_computer_helpers: usize,
    pub warm_browser_helpers: usize,
    pub stale_ref_supported: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GuiActionInfo {
    pub id: String,
    pub session_id: String,
    pub agent_id: String,
    pub agent_name: String,
    pub surface: String,
    pub action: String,
    #[serde(default)]
    pub target_app: Option<String>,
    #[serde(default)]
    pub target_bundle_id: Option<String>,
    #[serde(default)]
    pub target_window: Option<String>,
    #[serde(default)]
    pub sensitive_target: bool,
    pub snapshot_generation: u64,
    pub lease_generation: u64,
    pub queued_ms: u64,
    pub latency_ms: u64,
    pub result: String,
    #[serde(default)]
    pub error_code: Option<String>,
    pub occurred_at: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GuiTimelineParams {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GuiTimelineResult {
    pub actions: Vec<GuiActionInfo>,
    pub limit: u32,
}

/// Read-only GUI action retention statistics. `keep_forever` is true when the
/// effective policy is `0`; `cutoff` is absent in that case and `eligible` is
/// zero. No event payload or screen content is returned.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GuiTimelineRetentionResult {
    pub project_id: String,
    pub retention_days: u32,
    pub keep_forever: bool,
    #[serde(default)]
    pub cutoff: Option<String>,
    pub total: u64,
    pub eligible: u64,
    #[serde(default)]
    pub oldest_at: Option<String>,
    #[serde(default)]
    pub newest_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GuiTimelinePrunedInfo {
    pub session_id: String,
    pub seq: u64,
    pub retention_days: u32,
    pub cutoff: String,
    pub pruned: u64,
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GuiTimelineAuditParams {
    pub project_id: String,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GuiTimelineAuditResult {
    pub project_id: String,
    pub entries: Vec<GuiTimelinePrunedInfo>,
    pub total: u64,
    pub limit: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GuiTimelineRetentionParams {
    pub project_id: String,
    #[serde(default)]
    pub older_than_days: Option<u32>,
}

/// Explicit operator request to prune the GUI action timeline. `older_than_days`
/// overrides the project policy for this call; `0` is rejected because an
/// explicit prune of everything is never a retention operation.
#[derive(Debug, Clone, Deserialize)]
pub struct GuiTimelinePruneParams {
    pub project_id: String,
    #[serde(default)]
    pub older_than_days: Option<u32>,
    #[serde(default)]
    pub dry_run: Option<bool>,
    /// Explicit confirmation for a non-dry-run prune. A false value is
    /// rejected by the daemon; the automatic sweep bypasses this RPC.
    #[serde(default)]
    pub confirm: bool,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Outcome of a bounded `gui.action` retention prune. Only `gui.action` events
/// are eligible; `other_kinds_touched` stays zero and is reported for audit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GuiTimelinePruneResult {
    pub project_id: String,
    pub retention_days: u32,
    /// RFC3339 UTC cutoff. Events strictly older than this are eligible.
    pub cutoff: String,
    pub dry_run: bool,
    pub pruned: u64,
    pub remaining: u64,
    pub sessions: u64,
    /// Sessions skipped because their mirror was being rewritten or unreadable.
    /// A skipped row is never deleted from SQLite.
    pub skipped_sessions: u64,
    pub skipped_events: u64,
    pub other_kinds_touched: u64,
    pub limit: u32,
    pub truncated: bool,
    pub reason: String,
    /// Bounded sample of pruned session ids, no event payloads.
    #[serde(default)]
    pub affected_sessions: Vec<String>,
}

/// Daemon LaunchAgent supervision status. `state` is one of `not_installed`,
/// `installed`, `loaded`, `disabled`, `running`, or `unknown`. The status call
/// never spawns the daemon and never writes the plist.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonAgentStatusResult {
    pub label: String,
    pub plist_path: String,
    pub installed: bool,
    /// The launchd service is loaded for the current user.
    pub loaded: bool,
    /// The label is present in `launchctl print-disabled` as disabled.
    pub disabled: bool,
    /// `launchctl print` reported a running process for this label.
    pub running: bool,
    /// The running pid belongs to this installation and matches discovery.
    pub supervised: bool,
    #[serde(default)]
    pub pid: Option<u32>,
    pub state: String,
    pub detail: String,
    /// True when `launchctl` was queried successfully. False means the state is
    /// `unknown`; no negative conclusion is drawn from a failed query.
    pub launchctl_available: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DaemonAgentStatusParams {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IntegrationPolicy {
    #[serde(default)]
    pub auto_resolve: bool,
    #[serde(default = "default_auto_resolve_attempts")]
    pub max_auto_attempts: u32,
    #[serde(default)]
    pub max_resolve_tokens: Option<u64>,
}

impl Default for IntegrationPolicy {
    fn default() -> Self {
        Self {
            auto_resolve: false,
            max_auto_attempts: default_auto_resolve_attempts(),
            max_resolve_tokens: None,
        }
    }
}

fn default_auto_resolve_attempts() -> u32 {
    1
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RoleModelConfig {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub input_usd_per_million: Option<f64>,
    #[serde(default)]
    pub output_usd_per_million: Option<f64>,
    /// Optional price for input tokens served from the provider prompt cache.
    /// Unset falls back to the full input price (conservative).
    #[serde(default)]
    pub cache_read_usd_per_million: Option<f64>,
    /// Optional price for input tokens written to the provider prompt cache.
    /// Unset falls back to the full input price (conservative).
    #[serde(default)]
    pub cache_write_usd_per_million: Option<f64>,
    /// Optional context window for this role's model, in tokens.
    #[serde(default)]
    pub context_window: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RoleModels {
    #[serde(default)]
    pub leader: RoleModelConfig,
    #[serde(default)]
    pub manager: RoleModelConfig,
    #[serde(default)]
    pub worker: RoleModelConfig,
    #[serde(default)]
    pub verifier: RoleModelConfig,
    #[serde(default)]
    pub labour: RoleModelConfig,
    #[serde(default)]
    pub scout: RoleModelConfig,
}

impl RoleModels {
    pub fn for_role(&self, role: &str) -> &RoleModelConfig {
        match role {
            "manager" => &self.manager,
            "worker" => &self.worker,
            "verifier" => &self.verifier,
            "labour" => &self.labour,
            "scout" => &self.scout,
            _ => &self.leader,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub project_id: String,
    pub goal: String,
    pub status: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub cost_budget_usd: Option<f64>,
    #[serde(default)]
    pub time_budget_secs: Option<u64>,
    /// Explicit opt-in that bypasses session, plan-task, department, and
    /// project economic budget gates for this session. Capacity, approvals,
    /// verification, context, and turn limits still apply.
    #[serde(default)]
    pub unlimited: bool,
    #[serde(default)]
    pub cost_spent_usd: f64,
    #[serde(default)]
    pub base_commit: Option<String>,
    #[serde(default)]
    pub tokens_in: u64,
    #[serde(default)]
    pub tokens_out: u64,
    /// Prompt layout persisted at session creation. `2` keeps the legacy
    /// combined system prompt; `3` uses the content-addressed instruction
    /// store. Legacy sessions decode as `2` so a daemon upgrade never changes
    /// their prompt bytes.
    #[serde(default = "default_session_prompt_layout")]
    pub prompt_layout: u32,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub session_id: String,
    pub seq: u64,
    #[serde(rename = "type")]
    pub kind: String,
    pub payload: Value,
    pub created_at: String,
}

/// One content-addressed instruction section body. The hash is
/// `sha256(text)`; the text is never copied into cache telemetry, only into
/// the store and an explicit export bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionBlob {
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub created_at: String,
}

/// The ordered instruction state persisted for one instruction scope. The
/// scope id is normally the session id for the Leader, or
/// `<session_id>#<agent_id>` for a task-scoped agent. Keys remain deterministic
/// (`core/charter`, `run/task`, ...) so the same schema is inspectable for
/// every agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInstructionState {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub ordinal: u32,
    #[serde(default)]
    pub tail: bool,
    #[serde(default)]
    pub updated_at: String,
}

/// One ordered section reference recorded on `context.prompt_prefix` and
/// `context.instruction` events. It contains no prompt text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptSectionRef {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub ordinal: u32,
    #[serde(default)]
    pub tail: bool,
}

impl PromptSectionRef {
    pub fn new(key: impl Into<String>, hash: impl Into<String>, ordinal: u32, tail: bool) -> Self {
        Self {
            key: key.into(),
            hash: hash.into(),
            ordinal,
            tail,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionInstructionsParams {
    pub session_id: String,
    /// Optional exact agent id whose scoped instruction state should be
    /// inspected. Omit to inspect every state attached to the session.
    #[serde(default)]
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInstructionSection {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub ordinal: u32,
    #[serde(default)]
    pub tail: bool,
    #[serde(default)]
    pub chars: u64,
    /// True when the referenced blob exists in the local store and its hash
    /// verifies. Text is deliberately never returned by this RPC.
    #[serde(default)]
    pub available: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInstructionStateReport {
    #[serde(default)]
    pub state_id: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub prompt_layout: u32,
    #[serde(default)]
    pub head_hash: String,
    #[serde(default)]
    pub tail_hash: String,
    /// Hash of this state's `agent/identity` section, when present. Text is
    /// never returned by this RPC.
    #[serde(default)]
    pub identity_hash: Option<String>,
    #[serde(default)]
    pub sections: Vec<SessionInstructionSection>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInstructionsReport {
    #[serde(default)]
    pub session_id: String,
    #[serde(default = "default_prompt_layout")]
    pub prompt_layout: u32,
    /// Latest `context.prompt_prefix` hash observed for the session, if any.
    #[serde(default)]
    pub latest_prefix_hash: Option<String>,
    /// Finalized L4 cache epoch from the latest prompt-prefix event. Legacy L1
    /// placeholder events decode as `None`.
    #[serde(default)]
    pub latest_cache_epoch: Option<String>,
    /// Hash of the exact ordered head text from the latest prompt-prefix
    /// event. It can be compared with each state's `head_hash`.
    #[serde(default)]
    pub latest_stable_prefix_hash: Option<String>,
    /// Latest project identity id/generation/hash from the prompt-prefix
    /// event. Absent for legacy sessions and for Labour/Verifier/Scout runs.
    #[serde(default)]
    pub latest_identity_id: Option<String>,
    #[serde(default)]
    pub latest_identity_generation: Option<u64>,
    #[serde(default)]
    pub latest_identity_hash: Option<String>,
    #[serde(default)]
    pub states: Vec<SessionInstructionStateReport>,
    /// Bounded diagnostic surfaced by the last materialization attempt.
    #[serde(default)]
    pub last_rebuilt: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetSessionPromptLayoutParams {
    pub session_id: String,
    pub prompt_layout: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetSessionPromptLayoutResult {
    pub session_id: String,
    pub prompt_layout: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetProjectPromptLayoutParams {
    pub project_id: String,
    pub prompt_layout: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetProjectPromptLayoutResult {
    pub project_id: String,
    pub prompt_layout: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub pid: u32,
    pub version: String,
    pub protocol: u32,
    pub data_dir: String,
    pub started_at: String,
    pub sessions: usize,
    pub projects: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PingResult {
    pub pong: bool,
    pub version: String,
    pub protocol: u32,
    pub uptime_secs: u64,
}

/// One bounded low-cardinality count, grouped by a status or kind label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricCount {
    pub key: String,
    pub count: u64,
}

/// Daemon metrics exposed through `daemon.metrics` and the Prometheus
/// `/metrics` endpoint. No session/task ids, prompts, paths, model names, or
/// provider secrets are included; the only labels are bounded status/kind
/// vocabularies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonMetrics {
    pub version: String,
    pub protocol: u32,
    pub uptime_secs: u64,
    pub projects: u64,
    pub sessions: Vec<MetricCount>,
    pub plan_tasks: Vec<MetricCount>,
    pub merge_queue: Vec<MetricCount>,
    pub intake: Vec<MetricCount>,
    pub continuations: Vec<MetricCount>,
    pub recoveries: Vec<MetricCount>,
    pub memories: Vec<MetricCount>,
    pub memory_conflicts: Vec<MetricCount>,
    pub runtime_leases: Vec<MetricCount>,
    /// Durable supervised processes by state (`starting`, `running`,
    /// `exited`, `stopped`, `lost`, `unknown`).
    #[serde(default)]
    pub supervised_processes: Vec<MetricCount>,
    /// Retained bytes across daemon-owned supervised-process logs.
    #[serde(default)]
    pub supervised_process_output_bytes: u64,
    /// Terminal preemptions by phase (`provider`, `tool`, `queue`).
    #[serde(default)]
    pub preemptions_by_phase: Vec<MetricCount>,
    /// Terminal preemptions by reason (`wall_clock`, `explicit`).
    #[serde(default)]
    pub preemptions_by_reason: Vec<MetricCount>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// Derived cache-read share over the ledger buckets; see
    /// [`cache_hit_ratio`]. Never persisted separately.
    #[serde(default)]
    pub cache_hit_ratio: f64,
    /// Most recent agent-run preamble estimate (system prompt + tool schemas).
    /// `None` when no run has emitted a prompt-prefix event yet.
    #[serde(default)]
    pub preamble_tokens_estimate: Option<u64>,
    pub cost_spent_usd: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonMetricsResult {
    pub metrics: DaemonMetrics,
    pub prometheus: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddProjectParams {
    pub root: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectListResult {
    pub projects: Vec<Project>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ShellPolicyParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetShellPolicyParams {
    pub project_id: String,
    pub policy: ShellPolicy,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShellPolicyResult {
    pub project_id: String,
    pub policy: ShellPolicy,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IntegrationPolicyParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetIntegrationPolicyParams {
    pub project_id: String,
    pub policy: IntegrationPolicy,
}

#[derive(Debug, Clone, Serialize)]
pub struct IntegrationPolicyResult {
    pub project_id: String,
    pub policy: IntegrationPolicy,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateSessionParams {
    pub project_id: String,
    pub goal: String,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub cost_budget_usd: Option<f64>,
    #[serde(default)]
    pub time_budget_secs: Option<u64>,
    /// Ignore all economic budget gates for this session. Cannot be combined
    /// with explicit token/cost/time budgets.
    #[serde(default)]
    pub unlimited: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetSessionPriorityParams {
    pub session_id: String,
    pub priority: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetSessionUnlimitedParams {
    pub session_id: String,
    pub unlimited: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionUnlimitedResult {
    pub session_id: String,
    pub unlimited: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionPriorityResult {
    pub session_id: String,
    pub priority: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitLogParams {
    pub session_id: String,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitCommit {
    pub hash: String,
    pub short_hash: String,
    pub author: String,
    pub email: String,
    pub author_date: String,
    pub subject: String,
    #[serde(default)]
    pub refs: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GitLogResult {
    pub session_id: String,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub base_commit: Option<String>,
    pub commits: Vec<GitCommit>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitShowParams {
    pub session_id: String,
    pub commit: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GitShowResult {
    pub session_id: String,
    pub commit: String,
    pub output: String,
    pub truncated: bool,
}

/// Project-scoped knowledge source. The source body lives in a private JSON
/// snapshot on disk; SQLite carries the queryable metadata and chunk index.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Skill {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub slug: String,
    pub description: String,
    #[serde(default)]
    pub when_to_use: String,
    #[serde(default = "default_skill_version")]
    pub version: String,
    pub instructions: String,
    #[serde(default)]
    pub tool_allow: Vec<String>,
    #[serde(default)]
    pub tool_deny: Vec<String>,
    #[serde(default)]
    pub profiles: Vec<String>,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub enabled: bool,
    pub source: String,
    pub sha256: String,
    pub created_at: String,
    pub updated_at: String,
}

fn default_skill_version() -> String {
    "1.0.0".to_string()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SkillListParams {
    pub project_id: String,
    #[serde(default)]
    pub include_disabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillListResult {
    pub skills: Vec<Skill>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SkillCreateParams {
    pub project_id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub when_to_use: String,
    #[serde(default)]
    pub version: Option<String>,
    pub instructions: String,
    #[serde(default)]
    pub tool_allow: Vec<String>,
    #[serde(default)]
    pub tool_deny: Vec<String>,
    #[serde(default)]
    pub profiles: Vec<String>,
    #[serde(default)]
    pub roles: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SkillImportParams {
    pub project_id: String,
    pub path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SkillUpdateParams {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub when_to_use: String,
    #[serde(default)]
    pub version: Option<String>,
    pub instructions: String,
    #[serde(default)]
    pub tool_allow: Vec<String>,
    #[serde(default)]
    pub tool_deny: Vec<String>,
    #[serde(default)]
    pub profiles: Vec<String>,
    #[serde(default)]
    pub roles: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SkillIdParams {
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SkillEnabledParams {
    pub id: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillDeleteResult {
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillActivation {
    pub name: String,
    pub slug: String,
    pub version: String,
    pub role: String,
    pub profile: String,
    #[serde(default)]
    pub tool_allow: Vec<String>,
    #[serde(default)]
    pub tool_deny: Vec<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    pub activated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionSkillActivationsParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionSkillActivationsResult {
    pub activations: Vec<SkillActivation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Source {
    pub id: String,
    pub project_id: String,
    pub kind: String,
    #[serde(default)]
    pub collection: Option<String>,
    pub title: String,
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    pub status: String,
    #[serde(default)]
    pub size_bytes: u64,
    pub sha256: String,
    #[serde(default = "default_source_revision")]
    pub revision: u32,
    #[serde(default)]
    pub chunk_count: usize,
    #[serde(default)]
    pub char_count: usize,
    #[serde(default)]
    pub token_count: u64,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub indexed_at: Option<String>,
}

fn default_source_revision() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceCollection {
    /// `None` groups sources that have no collection assigned.
    #[serde(default)]
    pub name: Option<String>,
    pub sources: usize,
    pub chunks: usize,
    pub bytes: u64,
    pub tokens: u64,
    #[serde(default)]
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SourceCollectionsParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceCollectionsResult {
    pub collections: Vec<SourceCollection>,
    pub total_sources: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateSourceMetadataParams {
    pub source_id: String,
    pub title: String,
    #[serde(default)]
    pub collection: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetSourceCollectionParams {
    pub source_id: String,
    /// An empty string or null clears the collection.
    #[serde(default)]
    pub collection: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetSourceLabelsParams {
    pub source_id: String,
    #[serde(default)]
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceChunk {
    pub id: String,
    pub source_id: String,
    pub chunk_no: u32,
    #[serde(default)]
    pub heading: Option<String>,
    pub content: String,
    #[serde(default)]
    pub start_offset: usize,
    #[serde(default)]
    pub end_offset: usize,
    #[serde(default)]
    pub token_count: u32,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SourceListParams {
    pub project_id: String,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceStats {
    pub sources: usize,
    pub indexed: usize,
    pub failed: usize,
    pub chunks: usize,
    pub bytes: u64,
    pub tokens: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceListResult {
    pub sources: Vec<Source>,
    pub stats: SourceStats,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceGetParams {
    pub source_id: String,
    #[serde(default)]
    pub offset: Option<u32>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceGetResult {
    pub source: Source,
    pub chunks: Vec<SourceChunk>,
    pub total_chunks: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceCreateNoteParams {
    pub project_id: String,
    #[serde(default)]
    pub collection: Option<String>,
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceCreateFileParams {
    pub project_id: String,
    #[serde(default)]
    pub collection: Option<String>,
    pub path: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceCreateUrlParams {
    pub project_id: String,
    #[serde(default)]
    pub collection: Option<String>,
    pub url: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceImportParams {
    pub project_id: String,
    #[serde(default)]
    pub collection: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub max_files: Option<u32>,
    #[serde(default)]
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceImportError {
    pub path: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceImportResult {
    pub imported: Vec<Source>,
    pub skipped: usize,
    pub errors: Vec<SourceImportError>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceActionParams {
    pub source_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceDeleteResult {
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceReindexResult {
    pub source: Source,
    pub chunks: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceSearchParams {
    pub project_id: String,
    pub query: String,
    #[serde(default)]
    pub source_ids: Vec<String>,
    #[serde(default)]
    pub collections: Vec<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    /// `lexical`, `semantic`, or `hybrid`. Absent keeps the backward-compatible
    /// dynamic default: hybrid when the project opted into source embeddings,
    /// otherwise lexical. An unavailable semantic lane always falls back to
    /// lexical with a documented reason.
    #[serde(default)]
    pub mode: Option<SourceSearchMode>,
}

/// Source retrieval lane. This is deliberately not a model selector: it only
/// chooses whether the deterministic lexical lane, the configured embedding
/// lane, or their RRF fusion is used.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SourceSearchMode {
    #[default]
    Lexical,
    Semantic,
    Hybrid,
}

impl SourceSearchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lexical => "lexical",
            Self::Semantic => "semantic",
            Self::Hybrid => "hybrid",
        }
    }
}

impl std::str::FromStr for SourceSearchMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "lexical" => Ok(Self::Lexical),
            "semantic" => Ok(Self::Semantic),
            "hybrid" => Ok(Self::Hybrid),
            other => Err(format!(
                "source search mode must be lexical, semantic, or hybrid (got {other})"
            )),
        }
    }
}

fn default_source_label() -> String {
    "lexical".to_string()
}

fn default_source_origins() -> Vec<String> {
    vec!["lexical".to_string()]
}

/// One ranked source chunk. Additive semantic fields keep older clients
/// decoding: `source` defaults to `lexical`, confidence/origins default to a
/// plain lexical result, and both per-lane scores are optional. Vectors are
/// never serialized through this type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SourceSearchHit {
    pub source_id: String,
    pub source_title: String,
    #[serde(default)]
    pub collection: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    pub chunk_id: String,
    pub chunk_no: u32,
    #[serde(default)]
    pub heading: Option<String>,
    pub content: String,
    pub score: f64,
    /// Effective per-candidate origin label: `lexical`, `semantic`, or
    /// `hybrid`. Legacy payloads had only the lexical path.
    #[serde(default = "default_source_label")]
    pub source: String,
    /// Calibrated 0..=1 confidence derived from the effective lane. It never
    /// changes lexical ordering and never invents semantic evidence.
    #[serde(default)]
    pub confidence: f64,
    /// Deterministic lane membership, ordered `lexical, semantic`.
    #[serde(default = "default_source_origins")]
    pub origins: Vec<String>,
    /// Raw lexical relevance retained for audit/debugging. Never a vector.
    #[serde(default)]
    pub lexical_score: Option<f64>,
    /// Cosine similarity in 0..=1 retained for audit/debugging. Never a
    /// vector or embedded text. With deterministic reranking this remains the
    /// raw cosine component; `rerank_score` is the blended ordering value.
    #[serde(default)]
    pub semantic_score: Option<f64>,
    /// Effective rerank label for this hit: `off` or `deterministic`. Older
    /// payloads decode as `off`.
    #[serde(default = "default_source_rerank_label")]
    pub rerank: String,
    /// Blended deterministic rerank score in 0..=1. Absent when rerank is off.
    #[serde(default)]
    pub rerank_score: Option<f64>,
    /// Fraction of query terms present in the chunk body in 0..=1.
    #[serde(default)]
    pub lexical_overlap: Option<f64>,
    /// Fraction of query terms present in the source title and heading path.
    #[serde(default)]
    pub title_match: Option<f64>,
    /// Recency component in 0..=1 derived from the source `updated_at`. Absent
    /// when the timestamp is unavailable or unparsable.
    #[serde(default)]
    pub freshness: Option<f64>,
    /// True only for a hit introduced by the bounded lexical expansion pass.
    /// Expansion never creates a chunk; it can only re-surface a stored one.
    #[serde(default)]
    pub expanded: bool,
    /// Expansion terms that actually occur in this hit, in deterministic plan
    /// order. Empty for first-pass hits and for all hits when expansion is off.
    #[serde(default)]
    pub expansion_terms: Vec<String>,
    /// Source `updated_at` used for the freshness component. Operational
    /// metadata only; no content or vector is added.
    #[serde(default)]
    pub source_updated_at: Option<String>,
}

fn default_source_rerank_label() -> String {
    "off".to_string()
}

fn default_source_index_path() -> String {
    "exact".to_string()
}

fn default_source_expansion_label() -> String {
    "off".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SourceSearchResult {
    pub hits: Vec<SourceSearchHit>,
    #[serde(default)]
    pub degraded: bool,
    /// Effective lane after fallback. This can differ from `requested_mode`.
    #[serde(default = "default_source_label")]
    pub mode: String,
    /// Lane requested by the caller or selected by the dynamic default.
    #[serde(default = "default_source_label")]
    pub requested_mode: String,
    /// Present only when semantic retrieval was requested/selected but could
    /// not contribute. Lexical hits are still returned.
    #[serde(default)]
    pub semantic_unavailable: Option<String>,
    /// Bounded count of candidates examined by the semantic scan. Operational
    /// telemetry only; no chunk text is duplicated here.
    #[serde(default)]
    pub candidates_scanned: usize,
    /// Candidate path actually used for semantic retrieval: `exact` or `ann`.
    /// Lexical-only searches always report `exact`.
    #[serde(default = "default_source_index_path")]
    pub index_path: String,
    /// Derived ANN generation used for an ANN probe. `None` for exact scans.
    #[serde(default)]
    pub index_generation: Option<u64>,
    /// Explicit reason the exact scan was used when ANN was requested. Reasons
    /// are stable codes such as `ann_disabled`, `below_min_vectors`,
    /// `not_built`, `index_corrupt: ...`, or `no_assignments`.
    #[serde(default)]
    pub index_fallback_reason: Option<String>,
    /// Number of centroids probed by an ANN search. Zero for exact scans.
    #[serde(default)]
    pub probed_clusters: u32,
    /// Bounded count of vectors examined by an ANN probe before cosine scoring.
    #[serde(default)]
    pub ann_candidates: usize,
    /// Effective global rerank label: `off` or `deterministic`.
    #[serde(default = "default_source_rerank_label")]
    pub rerank: String,
    /// Effective global query-expansion label: `off` or `lexical`.
    #[serde(default = "default_source_expansion_label")]
    pub expansion: String,
    /// Bounded expansion terms used for the second pass. No model-generated
    /// terms are ever present.
    #[serde(default)]
    pub expansion_terms: Vec<String>,
}

/// Opt-in embedding lane for project knowledge sources.
///
/// This is deliberately separate from [`MemoryEmbeddingPolicy`]: source
/// embeddings index bounded chunks of immutable snapshots, have their own job
/// queue, retention, usage table, and generation-invalidated vectors. The
/// endpoint is named by `endpoint_env`, never stored as a URL value, so an
/// operator can point one daemon at a different endpoint without rewriting
/// project state. `api_key_env` follows the same name-only rule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceEmbeddingPolicy {
    /// Off by default. A disabled policy performs no automatic indexing and no
    /// query embedding calls.
    #[serde(default)]
    pub enabled: bool,
    /// Environment variable name containing the OpenAI-compatible embeddings
    /// base URL (or full `/embeddings` URL). The value is read at call time.
    #[serde(default)]
    pub endpoint_env: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default = "default_source_embedding_dimensions")]
    pub dimensions: u32,
    /// Chunks embedded per HTTP batch and per reconciler tick.
    #[serde(default = "default_source_embedding_batch_size")]
    pub batch_size: u32,
    /// Hard per-source chunk cap for the derived index. Chunks above this cap
    /// remain lexically searchable and are counted as not indexed.
    #[serde(default = "default_source_embedding_max_chunks")]
    pub max_chunks_per_source: u32,
    /// Older-generation vectors and dead jobs are pruned after this many days.
    /// `0` keeps derived rows until an explicit reindex/clear.
    #[serde(default = "default_source_embedding_retention_days")]
    pub retention_days: u32,
    #[serde(default = "default_source_embedding_timeout_secs")]
    pub timeout_secs: u64,
    /// Environment variable name for the bearer token, if the endpoint needs
    /// one. The value is never persisted, returned, or logged.
    #[serde(default)]
    pub api_key_env: Option<String>,
}

fn default_source_embedding_dimensions() -> u32 {
    1536
}

fn default_source_embedding_batch_size() -> u32 {
    8
}

fn default_source_embedding_max_chunks() -> u32 {
    200
}

fn default_source_embedding_retention_days() -> u32 {
    30
}

fn default_source_embedding_timeout_secs() -> u64 {
    30
}

impl Default for SourceEmbeddingPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint_env: None,
            model: None,
            dimensions: default_source_embedding_dimensions(),
            batch_size: default_source_embedding_batch_size(),
            max_chunks_per_source: default_source_embedding_max_chunks(),
            retention_days: default_source_embedding_retention_days(),
            timeout_secs: default_source_embedding_timeout_secs(),
            api_key_env: None,
        }
    }
}

impl SourceEmbeddingPolicy {
    /// Both endpoint and model are required before any request can be made.
    /// An object may be configured but disabled.
    pub fn is_configured(&self) -> bool {
        self.endpoint_env
            .as_deref()
            .is_some_and(|name| !name.trim().is_empty())
            && self
                .model
                .as_deref()
                .is_some_and(|model| !model.trim().is_empty())
    }

    /// True only when the lane is both enabled and configured. This is the
    /// single gate for daemon indexing and query embedding calls.
    pub fn is_active(&self) -> bool {
        self.enabled && self.is_configured()
    }

    pub fn endpoint_env_name(&self) -> Option<&str> {
        self.endpoint_env
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
    }

    pub fn api_key_env_name(&self) -> Option<&str> {
        self.api_key_env
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceEmbeddingPolicyParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetSourceEmbeddingPolicyParams {
    pub project_id: String,
    pub policy: SourceEmbeddingPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct SourceEmbeddingPolicyResult {
    pub project_id: String,
    pub policy: SourceEmbeddingPolicy,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceEmbeddingStatusParams {
    pub project_id: String,
}

/// Bounded operational state for one source embedding job. Contains no
/// vectors, embedded text, endpoint URL values, or credentials.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct SourceEmbeddingJobStatus {
    pub source_id: String,
    pub source_title: String,
    /// `pending`, `running`, `done`, `failed`, or `dead`.
    pub status: String,
    pub attempts: u32,
    pub generation: u64,
    pub chunks_total: u32,
    pub chunks_done: u32,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub next_attempt_at: Option<String>,
    pub updated_at: String,
}

/// Source embedding spend is project infrastructure accounting, separate from
/// memory embeddings and session usage. Nothing here is charged to a session
/// token or USD budget.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct SourceEmbeddingUsage {
    pub calls: u64,
    pub input_tokens: u64,
    pub estimated_tokens: u64,
    pub bytes: u64,
    pub provider_reported_calls: u64,
}

/// Structural health of derived source vectors. These counts are operational,
/// rebuildable metadata, not an audit projection. The engine re-checks the
/// exact embedding-text hash during retrieval, so a stale row is skipped even
/// when its structural header is valid.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct SourceEmbeddingIntegrity {
    pub valid_vectors: u32,
    pub invalid_vectors: u32,
    pub stale_vectors: u32,
    pub orphan_vectors: u32,
    pub generation_mismatched: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct SourceEmbeddingStatusResult {
    pub project_id: String,
    pub enabled: bool,
    pub configured: bool,
    /// Environment variable *names* only. Booleans describe whether a value is
    /// currently present; values are never returned.
    pub endpoint_env: Option<String>,
    pub endpoint_env_present: bool,
    pub api_key_env: Option<String>,
    pub api_key_env_present: bool,
    pub model: Option<String>,
    pub dimensions: u32,
    pub batch_size: u32,
    pub max_chunks_per_source: u32,
    pub retention_days: u32,
    /// Chunks eligible under `max_chunks_per_source`.
    pub indexable_chunks: u32,
    /// Current-generation, structurally valid vectors.
    pub indexed_chunks: u32,
    /// Older-generation vectors retained until retention pruning.
    pub stale_chunks: u32,
    pub invalid_chunks: u32,
    pub pending_jobs: u32,
    pub running_jobs: u32,
    pub failed_jobs: u32,
    pub dead_jobs: u32,
    pub done_jobs: u32,
    /// `indexed_chunks / indexable_chunks`, zero when nothing is indexable.
    pub coverage: f64,
    /// Why semantic retrieval cannot contribute right now, if applicable.
    pub semantic_unavailable: Option<String>,
    pub last_error: Option<String>,
    pub usage: SourceEmbeddingUsage,
    pub recent_jobs: Vec<SourceEmbeddingJobStatus>,
    pub integrity: SourceEmbeddingIntegrity,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceEmbeddingReindexParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct SourceEmbeddingReindexResult {
    pub project_id: String,
    pub enabled: bool,
    pub model: Option<String>,
    pub cleared_vectors: u32,
    pub cleared_jobs: u32,
    pub enqueued_sources: u32,
    pub max_chunks_per_source: u32,
}

/// Deterministic source rerank mode. `deterministic` blends fixed components
/// with fixed weights and performs no model call.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SourceRerankMode {
    Off,
    #[default]
    Deterministic,
}

impl SourceRerankMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Deterministic => "deterministic",
        }
    }
}

impl std::str::FromStr for SourceRerankMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "deterministic" => Ok(Self::Deterministic),
            other => Err(format!(
                "source rerank must be off or deterministic (got {other})"
            )),
        }
    }
}

/// Lexical-only query expansion mode. `lexical` extracts bounded terms from
/// the first pass with the deterministic tokenizer and performs one second
/// pass. No LLM or remote service is involved.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SourceQueryExpansionMode {
    #[default]
    Off,
    Lexical,
}

impl SourceQueryExpansionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Lexical => "lexical",
        }
    }
}

impl std::str::FromStr for SourceQueryExpansionMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "lexical" => Ok(Self::Lexical),
            other => Err(format!(
                "source query expansion must be off or lexical (got {other})"
            )),
        }
    }
}

/// Project-scoped retrieval controls layered on top of the opt-in source
/// embedding policy. Every field is additive and serde-defaulted so older
/// manifests, clients, and events decode unchanged. ANN and expansion are off
/// by default; rerank defaults to the deterministic, model-free blend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceRetrievalPolicy {
    /// Use the derived source IVF index for semantic candidate selection.
    /// Disabled by default; the exact cosine scan remains the fallback.
    #[serde(default)]
    pub ann_enabled: bool,
    /// Minimum number of current-generation vectors before an ANN index is
    /// considered useful. Smaller corpora use the exact scan.
    #[serde(default = "default_source_ann_min_vectors")]
    pub ann_min_vectors: u32,
    /// Centroids probed per query. Bounded to 1..=32.
    #[serde(default = "default_source_ann_nprobe")]
    pub ann_nprobe: u32,
    /// Deterministic rerank mode. No hidden model routing is possible.
    #[serde(default)]
    pub rerank: SourceRerankMode,
    /// Lexical query expansion mode. Off by default.
    #[serde(default)]
    pub query_expansion: SourceQueryExpansionMode,
    /// Maximum expansion terms extracted from the first pass. Bounded 1..=16.
    #[serde(default = "default_source_max_expanded_terms")]
    pub max_expanded_terms: u32,
}

fn default_source_ann_min_vectors() -> u32 {
    256
}

fn default_source_ann_nprobe() -> u32 {
    8
}

fn default_source_max_expanded_terms() -> u32 {
    8
}

impl Default for SourceRetrievalPolicy {
    fn default() -> Self {
        Self {
            ann_enabled: false,
            ann_min_vectors: default_source_ann_min_vectors(),
            ann_nprobe: default_source_ann_nprobe(),
            rerank: SourceRerankMode::Deterministic,
            query_expansion: SourceQueryExpansionMode::Off,
            max_expanded_terms: default_source_max_expanded_terms(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceRetrievalPolicyParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetSourceRetrievalPolicyParams {
    pub project_id: String,
    pub policy: SourceRetrievalPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct SourceRetrievalPolicyResult {
    pub project_id: String,
    pub policy: SourceRetrievalPolicy,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceIndexStatusParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceIndexRebuildParams {
    pub project_id: String,
}

/// Bounded operational state for the derived source IVF index. Contains only
/// counts, generation numbers, configuration, and stable fallback reasons.
/// Centroid bytes, vectors, and embedded text are never serialized.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct SourceIndexStatusResult {
    pub project_id: String,
    pub policy: SourceRetrievalPolicy,
    pub embedding_enabled: bool,
    pub embedding_configured: bool,
    pub model: Option<String>,
    pub dimensions: u32,
    pub ann_enabled: bool,
    /// True only when the ANN index can serve the next semantic query.
    pub ready: bool,
    /// Effective candidate path for the next semantic query: `exact` or `ann`.
    pub path: String,
    pub generation: Option<u64>,
    pub vector_count: u32,
    pub assigned_vectors: u32,
    pub invalid_assignments: u32,
    pub centroid_count: u32,
    pub nlist: u32,
    pub nprobe: u32,
    pub min_vectors: u32,
    pub coverage: f64,
    pub bytes: u64,
    /// Stable reason the ANN path is unavailable. `None` when `ready` is true.
    pub fallback_reason: Option<String>,
    pub last_rebuilt_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct SourceIndexRebuildResult {
    pub project_id: String,
    pub rebuilt: bool,
    pub reason: Option<String>,
    pub generation: Option<u64>,
    pub vector_count: u32,
    pub assigned_vectors: u32,
    pub centroid_count: u32,
    pub nlist: u32,
    pub nprobe: u32,
    /// `exact` when the rebuild did not produce a usable ANN index.
    pub path: String,
}

/// Citation evidence attached to an assistant message. The excerpt is a
/// snapshot so a deleted or refreshed source does not erase the audit trail.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceCitation {
    pub key: String,
    pub source_id: String,
    pub source_title: String,
    pub chunk_id: String,
    pub chunk_no: u32,
    #[serde(default)]
    pub heading: Option<String>,
    pub excerpt: String,
    pub score: f64,
    pub origin: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MessageCitation {
    pub message_id: String,
    pub citation: SourceCitation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CitationAuditMessage {
    pub message_id: String,
    #[serde(default)]
    pub agent_name: Option<String>,
    pub excerpt: String,
    pub citation_count: usize,
    pub referenced_keys: Vec<String>,
    pub dangling_keys: Vec<String>,
    pub unused_keys: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CitationAudit {
    pub messages: Vec<CitationAuditMessage>,
    pub messages_with_citations: usize,
    pub messages_without_citations: usize,
    pub total_citations: usize,
    pub unique_sources: usize,
    pub dangling_key_count: usize,
    pub unused_citation_count: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionCitationAuditParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionCitationAuditResult {
    pub audit: CitationAudit,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionCitationsParams {
    pub session_id: String,
    #[serde(default)]
    pub message_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionCitationsResult {
    pub citations: Vec<MessageCitation>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceTreeParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceEntry {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceTreeResult {
    pub root: String,
    pub entries: Vec<WorkspaceEntry>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceListParams {
    pub session_id: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub offset: Option<u32>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceListResult {
    pub root: String,
    pub path: String,
    pub entries: Vec<WorkspaceEntry>,
    pub total: usize,
    pub has_more: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceSearchParams {
    pub session_id: String,
    pub query: String,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceSearchResult {
    pub entries: Vec<WorkspaceEntry>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceGrepParams {
    pub session_id: String,
    pub query: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub case_sensitive: Option<bool>,
    #[serde(default)]
    pub max_results: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceGrepMatch {
    pub path: String,
    /// 1-based line number.
    pub line: u32,
    /// 1-based character column.
    pub column: u32,
    pub preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceGrepResult {
    pub query: String,
    pub matches: Vec<WorkspaceGrepMatch>,
    pub files_scanned: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceFileParams {
    pub session_id: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceFileResult {
    pub path: String,
    pub size: u64,
    pub truncated: bool,
    pub binary: bool,
    pub content: String,
    pub sha256: String,
    pub editable: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceWriteParams {
    pub session_id: String,
    pub path: String,
    pub content: String,
    #[serde(default)]
    pub expected_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWriteResult {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TerminalOpenParams {
    pub session_id: String,
    #[serde(default)]
    pub cols: Option<u16>,
    #[serde(default)]
    pub rows: Option<u16>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TerminalOpenResult {
    pub terminal_id: String,
    pub worktree: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TerminalWriteParams {
    pub terminal_id: String,
    /// Base64-encoded bytes, because PTY input may contain arbitrary control bytes.
    pub data: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TerminalWriteResult {
    pub written: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TerminalReadParams {
    pub terminal_id: String,
    #[serde(default)]
    pub after_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalChunk {
    pub seq: u64,
    /// Base64-encoded PTY output bytes.
    pub data: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TerminalReadResult {
    pub terminal_id: String,
    pub chunks: Vec<TerminalChunk>,
    pub next_seq: u64,
    pub status: String,
    #[serde(default)]
    pub exit_code: Option<i32>,
    pub dropped: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TerminalResizeParams {
    pub terminal_id: String,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct TerminalResizeResult {
    pub resized: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TerminalCloseParams {
    pub terminal_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TerminalCloseResult {
    pub closed: bool,
    #[serde(default)]
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConsoleOpenParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConsoleOpenResult {
    pub console_id: String,
    pub worktree: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConsoleWriteParams {
    pub console_id: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConsoleWriteResult {
    pub written: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConsoleReadParams {
    pub console_id: String,
    #[serde(default)]
    pub after_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleChunk {
    pub seq: u64,
    pub stream: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConsoleReadResult {
    pub console_id: String,
    pub chunks: Vec<ConsoleChunk>,
    pub next_seq: u64,
    pub status: String,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub dropped: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConsoleSignalParams {
    pub console_id: String,
    pub signal: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConsoleSignalResult {
    pub signaled: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConsoleCloseParams {
    pub console_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConsoleCloseResult {
    pub closed: bool,
    #[serde(default)]
    pub exit_code: Option<i32>,
}

/// A durable process supervised by the daemon.
///
/// Lifecycle transitions are event-backed (`process.starting`,
/// `process.started`, `process.adopted`, `process.restarting`,
/// `process.restarted`, `process.restart_blocked`, `process.exited`,
/// `process.lost`, `process.unknown`, `process.stopped`, `process.pruned`).
/// Heartbeat, last-access, and output byte/line counters are operational
/// observation state and are not replay content; the columns are updated
/// without an event so a chatty build cannot flood the event log. Output bodies
/// never enter events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SupervisedProcess {
    pub id: String,
    pub session_id: String,
    #[serde(default)]
    pub name: String,
    pub command: String,
    /// Canonical argv stored for inspection. The actual shell is
    /// `/bin/sh -c <command>` under the Seatbelt sandbox.
    #[serde(default)]
    pub argv: Vec<String>,
    /// Absolute working directory inside the session worktree.
    pub cwd: String,
    /// Canonical worktree root the process belongs to.
    pub worktree: String,
    /// SHA-256 over the effective allowlisted environment (names and values).
    pub env_hash: String,
    /// SHA-256 over the immutable spawn description. The same value is placed
    /// in the child environment and re-read for restart adoption.
    pub command_hash: String,
    /// `starting`, `running`, `restarting`, `exited`, `stopped`, `lost`,
    /// `unknown`, or `blocked`.
    pub state: String,
    #[serde(default)]
    pub pid: Option<i64>,
    #[serde(default)]
    pub pgid: Option<i64>,
    /// Per-spawn random nonce placed in the child environment.
    pub nonce: String,
    pub started_at: String,
    /// `ps -o lstart=` observation captured at spawn. Reattachment requires an
    /// exact match, which rejects PID reuse.
    #[serde(default)]
    pub process_started_at: Option<String>,
    pub heartbeat_at: String,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub signal: Option<i32>,
    /// Daemon-owned append-only log file. Output bodies are operational state.
    #[serde(default)]
    pub output_path: String,
    #[serde(default)]
    pub output_bytes: u64,
    #[serde(default)]
    pub output_lines: u64,
    #[serde(default)]
    pub total_output_bytes: u64,
    #[serde(default)]
    pub output_rotations: u64,
    /// Hard cap for retained output bytes.
    #[serde(default = "default_process_output_bytes")]
    pub max_output_bytes: u64,
    /// Hard cap for retained output lines.
    #[serde(default = "default_process_output_lines")]
    pub max_output_lines: u64,
    /// `instance_id:pid` of the daemon incarnation that currently supervises
    /// the row. A restart with matching evidence adopts the row and updates it.
    #[serde(default)]
    pub owner_instance: String,
    #[serde(default)]
    pub retention_deadline: Option<String>,
    #[serde(default = "default_process_idle_ttl")]
    pub idle_ttl_secs: u64,
    #[serde(default)]
    pub last_access_at: String,
    /// Consecutive boot-restart attempts for this row. It is incremented by the
    /// startup reconciler before a respawn and is bounded by the project policy
    /// so a crashing command cannot create an unbounded restart loop.
    #[serde(default)]
    pub restart_attempts: u32,
    /// Stable machine-readable reason a row entered `blocked`. `None` for a
    /// row that was never blocked by boot-restart policy.
    #[serde(default)]
    pub restart_blocked_reason: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
}

fn default_process_idle_ttl() -> u64 {
    3_600
}

fn default_process_output_bytes() -> u64 {
    1_048_576
}

fn default_process_output_lines() -> u64 {
    20_000
}

/// Project-scoped durable-process policy. Defaults preserve the historical
/// per-session cap and one-hour idle TTL; a project can opt out of idle stops
/// or tighten caps without changing agent tool schemas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessPolicy {
    /// Stop live processes idle beyond their per-process TTL. Default true.
    #[serde(default = "default_process_idle_sweep")]
    pub idle_sweep_enabled: bool,
    /// Active supervised processes per session.
    #[serde(default = "default_process_max")]
    pub max_processes: u32,
    /// Upper bound applied to a process's requested idle TTL.
    #[serde(default = "default_process_policy_idle_ttl")]
    pub idle_ttl_secs: u64,
    /// Per-process retained log byte cap.
    #[serde(default = "default_process_output_bytes")]
    pub max_output_bytes: u64,
    /// Per-process retained log line cap.
    #[serde(default = "default_process_output_lines")]
    pub max_output_lines: u64,
    /// Opt in to restarting a supervised process that was `running` when the
    /// daemon stopped but whose original process is gone at the next boot.
    /// Default false: a restart is explicit policy, never a silent behavior.
    #[serde(default)]
    pub restart_on_boot: bool,
    /// Consecutive boot-restart attempts allowed for one row. Once exhausted,
    /// the row becomes `blocked` and is never respawned automatically.
    #[serde(default = "default_process_restart_max_attempts")]
    pub restart_max_attempts: u32,
    /// Base exponential backoff between boot-restart attempts. Attempt N
    /// waits `base * 2^(N-1)`, capped by the daemon's hard ceiling.
    #[serde(default = "default_process_restart_backoff_secs")]
    pub restart_backoff_secs: u64,
}

fn default_process_idle_sweep() -> bool {
    true
}

fn default_process_max() -> u32 {
    8
}

fn default_process_policy_idle_ttl() -> u64 {
    3_600
}

fn default_process_restart_max_attempts() -> u32 {
    3
}

fn default_process_restart_backoff_secs() -> u64 {
    5
}

pub const MAX_PROCESS_RESTART_ATTEMPTS: u32 = 10;
pub const MAX_PROCESS_RESTART_BACKOFF_SECS: u64 = 300;

impl Default for ProcessPolicy {
    fn default() -> Self {
        Self {
            idle_sweep_enabled: true,
            max_processes: default_process_max(),
            idle_ttl_secs: default_process_policy_idle_ttl(),
            max_output_bytes: default_process_output_bytes(),
            max_output_lines: default_process_output_lines(),
            restart_on_boot: false,
            restart_max_attempts: default_process_restart_max_attempts(),
            restart_backoff_secs: default_process_restart_backoff_secs(),
        }
    }
}

impl ProcessPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=32).contains(&self.max_processes) {
            return Err("max_processes must be 1..32".to_string());
        }
        if !(60..=86_400).contains(&self.idle_ttl_secs) {
            return Err("idle_ttl_secs must be 60..86400".to_string());
        }
        if !(4_096..=16 * 1024 * 1024).contains(&self.max_output_bytes) {
            return Err("max_output_bytes must be 4096..16777216".to_string());
        }
        if !(100..=1_000_000).contains(&self.max_output_lines) {
            return Err("max_output_lines must be 100..1000000".to_string());
        }
        if self.restart_max_attempts > MAX_PROCESS_RESTART_ATTEMPTS {
            return Err(format!(
                "restart_max_attempts must be 0..={MAX_PROCESS_RESTART_ATTEMPTS}"
            ));
        }
        if self.restart_backoff_secs > MAX_PROCESS_RESTART_BACKOFF_SECS {
            return Err(format!(
                "restart_backoff_secs must be 0..={MAX_PROCESS_RESTART_BACKOFF_SECS}"
            ));
        }
        Ok(())
    }

    /// Effective exponential backoff for one attempt (1-based). The value is
    /// bounded by the policy's hard ceiling so a crashed process can never
    /// create an unbounded or integer-overflowing wait.
    pub fn restart_backoff(&self, attempt: u32) -> std::time::Duration {
        let exponent = attempt.saturating_sub(1).min(8);
        let multiplier = 1u64 << exponent;
        let secs = self
            .restart_backoff_secs
            .saturating_mul(multiplier)
            .min(MAX_PROCESS_RESTART_BACKOFF_SECS);
        std::time::Duration::from_secs(secs)
    }

    /// Clamp a process request to this project's limits. A caller may always
    /// ask for a shorter TTL or smaller log; it cannot widen the policy.
    pub fn clamp(
        &self,
        idle_ttl_secs: u64,
        max_output_bytes: u64,
        max_output_lines: u64,
    ) -> (u64, u64, u64) {
        (
            idle_ttl_secs.min(self.idle_ttl_secs),
            max_output_bytes.min(self.max_output_bytes),
            max_output_lines.min(self.max_output_lines),
        )
    }
}

/// Project-scoped hard-preemption policy. This is separate from cooperative
/// pause/cancel: it bounds the grace the runtime gives in-flight provider and
/// tool work after a wall-clock, explicit, or per-tool deadline fires. Every
/// grace has a finite upper bound, so no configuration can create an infinite
/// shutdown window.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreemptionPolicy {
    /// Enable automatic wall-clock preemption. Default true. An explicit
    /// `session.preempt` is an operator action and still works when this is
    /// false; a per-tool deadline only applies when `tool_deadline_secs` is
    /// set and this switch is true.
    #[serde(default = "default_preemption_enabled")]
    pub enabled: bool,
    /// Time allowed for an in-flight provider read to close and report usage
    /// after a preemption. The adapter never starts another request.
    #[serde(default = "default_provider_grace_ms")]
    pub provider_grace_ms: u64,
    /// Time allowed for a tool to observe cancellation before termination.
    #[serde(default = "default_tool_grace_ms")]
    pub tool_grace_ms: u64,
    /// Time between the graceful termination signal and the forced kill for a
    /// process group. Also used as the bounded wait after a forced kill.
    #[serde(default = "default_kill_escalation_ms")]
    pub kill_escalation_ms: u64,
    /// Optional per-tool deadline. `None` disables tool deadlines.
    #[serde(default)]
    pub tool_deadline_secs: Option<u64>,
    /// Create a session checkpoint after a safe preemption when the worktree is
    /// available. Default true; a checkpoint failure never changes the
    /// preempted status and never merges incomplete work.
    #[serde(default = "default_preemption_checkpoint")]
    pub checkpoint_on_preempt: bool,
}

fn default_preemption_enabled() -> bool {
    true
}

fn default_provider_grace_ms() -> u64 {
    2_000
}

fn default_tool_grace_ms() -> u64 {
    5_000
}

fn default_kill_escalation_ms() -> u64 {
    2_000
}

fn default_preemption_checkpoint() -> bool {
    true
}

impl Default for PreemptionPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            provider_grace_ms: default_provider_grace_ms(),
            tool_grace_ms: default_tool_grace_ms(),
            kill_escalation_ms: default_kill_escalation_ms(),
            tool_deadline_secs: None,
            checkpoint_on_preempt: true,
        }
    }
}

impl PreemptionPolicy {
    /// Validate and clamp to the documented bounds. A request may narrow a
    /// grace but cannot widen it past the hard upper bound.
    pub fn normalized(mut self) -> Result<Self, String> {
        self.provider_grace_ms = self.provider_grace_ms.min(30_000);
        self.tool_grace_ms = self.tool_grace_ms.min(60_000);
        self.kill_escalation_ms = self.kill_escalation_ms.min(30_000);
        self.tool_deadline_secs = self.tool_deadline_secs.map(|secs| secs.clamp(1, 3_600));
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.provider_grace_ms > 30_000 {
            return Err("provider_grace_ms must be at most 30000".to_string());
        }
        if self.tool_grace_ms > 60_000 {
            return Err("tool_grace_ms must be at most 60000".to_string());
        }
        if self.kill_escalation_ms > 30_000 {
            return Err("kill_escalation_ms must be at most 30000".to_string());
        }
        if let Some(secs) = self.tool_deadline_secs
            && !(1..=3_600).contains(&secs)
        {
            return Err("tool_deadline_secs must be 1..3600".to_string());
        }
        Ok(())
    }

    pub fn provider_grace(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.provider_grace_ms.min(30_000))
    }

    pub fn tool_grace(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.tool_grace_ms.min(60_000))
    }

    pub fn kill_escalation(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.kill_escalation_ms.min(30_000))
    }

    pub fn tool_deadline(&self) -> Option<std::time::Duration> {
        self.tool_deadline_secs
            .map(|secs| std::time::Duration::from_secs(secs.clamp(1, 3_600)))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PreemptionPolicyParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetPreemptionPolicyParams {
    pub project_id: String,
    pub policy: PreemptionPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreemptionPolicyResult {
    pub project_id: String,
    pub policy: PreemptionPolicy,
}

/// One bounded preemption observation. It never contains prompt text, tool
/// input, file contents, command output, or credentials.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreemptionRecord {
    pub session_id: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub agent_id: String,
    #[serde(default)]
    pub agent_name: String,
    #[serde(default)]
    pub agent_role: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub department: Option<String>,
    /// `wall_clock`, `explicit`, or `tool_deadline`.
    pub reason: String,
    /// `provider`, `tool`, or `queue`.
    pub phase: String,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub elapsed_ms: u64,
    #[serde(default)]
    pub side_effects_unknown: bool,
    #[serde(default)]
    pub partial_input_tokens: u64,
    #[serde(default)]
    pub partial_output_tokens: u64,
    #[serde(default)]
    pub partial_output_bytes: u64,
    #[serde(default)]
    pub usage_estimated: bool,
    #[serde(default)]
    pub checkpoint_id: Option<String>,
    /// Snapshot commit of the preempted Worker/Labour task worktree. It is
    /// never merged automatically; it only preserves incomplete work for
    /// inspection or an explicit retry.
    #[serde(default)]
    pub task_checkpoint_commit: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    /// True for the authoritative `session.preempted` terminal record. Tool
    /// deadline sub-events are false.
    #[serde(default)]
    pub terminal: bool,
    #[serde(default)]
    pub created_at: String,
}

impl PreemptionRecord {
    pub fn summary(&self) -> String {
        let mut text = format!(
            "{} preempted during {} after {}ms",
            self.reason, self.phase, self.elapsed_ms
        );
        if let Some(tool) = &self.tool {
            text.push_str(&format!(" ({tool})"));
        }
        if self.side_effects_unknown {
            text.push_str("; side effects unknown");
        }
        text
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionPreemptParams {
    pub session_id: String,
    /// Operator-supplied bounded reason. It is persisted in the audit event.
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionPreemptResult {
    pub session_id: String,
    /// True when an active run received the preemption signal. It is not a
    /// claim that the run finished instantly.
    pub preempted: bool,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionPreemptionParams {
    pub session_id: String,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionPreemptionResult {
    pub session_id: String,
    pub policy: PreemptionPolicy,
    /// The run currently holds an in-memory preempt token and that token has
    /// fired. False once the run finishes and is removed.
    #[serde(default)]
    pub active: bool,
    /// True when any terminal preemption record exists for the session.
    #[serde(default)]
    pub preempted: bool,
    #[serde(default)]
    pub records: Vec<PreemptionRecord>,
    #[serde(default)]
    pub by_phase: Vec<MetricCount>,
    #[serde(default)]
    pub by_reason: Vec<MetricCount>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PreemptionOverviewParams {
    pub project_id: String,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreemptionSessionSummary {
    pub session_id: String,
    #[serde(default)]
    pub goal: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub terminal_count: u64,
    #[serde(default)]
    pub last_reason: String,
    #[serde(default)]
    pub last_phase: String,
    #[serde(default)]
    pub last_tool: Option<String>,
    #[serde(default)]
    pub last_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreemptionOverviewResult {
    pub project_id: String,
    pub policy: PreemptionPolicy,
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub sessions: Vec<PreemptionSessionSummary>,
    #[serde(default)]
    pub by_phase: Vec<MetricCount>,
    #[serde(default)]
    pub by_reason: Vec<MetricCount>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProcessPolicyParams {
    pub project_id: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct SetProcessPolicyParams {
    pub project_id: String,
    pub policy: ProcessPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessPolicyResult {
    pub project_id: String,
    pub policy: ProcessPolicy,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProcessListParams {
    pub session_id: String,
    #[serde(default)]
    pub include_finished: Option<bool>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessListResult {
    pub session_id: String,
    pub processes: Vec<SupervisedProcess>,
    #[serde(default)]
    pub active: u32,
    #[serde(default)]
    pub limit: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProcessLogsParams {
    pub session_id: String,
    pub process_id: String,
    /// Zero-based byte offset. Ignored when `from_end` is set.
    #[serde(default)]
    pub offset: Option<u64>,
    #[serde(default)]
    pub max_bytes: Option<u32>,
    #[serde(default)]
    pub max_lines: Option<u32>,
    /// Return the newest retained bytes instead of a forward page.
    #[serde(default)]
    pub from_end: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessLogsResult {
    pub session_id: String,
    pub process_id: String,
    pub state: String,
    pub content: String,
    #[serde(default)]
    pub offset: u64,
    #[serde(default)]
    pub next_offset: u64,
    #[serde(default)]
    pub total_bytes: u64,
    #[serde(default)]
    pub eof: bool,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub rotated: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProcessStopParams {
    pub session_id: String,
    pub process_id: String,
    /// `TERM` (default), `KILL`, `INT`, or `HUP`.
    #[serde(default)]
    pub signal: Option<String>,
    #[serde(default)]
    pub grace_secs: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessStopResult {
    pub process: SupervisedProcess,
    #[serde(default)]
    pub forced: bool,
    #[serde(default)]
    pub waited_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProcessRestartParams {
    pub session_id: String,
    pub process_id: String,
    /// Explicit operator confirmation. A false value is rejected; this action
    /// is never an automatic retry path.
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessRestartResult {
    /// The row after entering `restarting`. `scheduled` is false only when the
    /// row was blocked immediately (for example, an exhausted attempt budget).
    pub process: SupervisedProcess,
    pub scheduled: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessSupervisorStatus {
    pub instance: String,
    #[serde(default)]
    pub running: u32,
    #[serde(default)]
    pub starting: u32,
    #[serde(default)]
    pub unknown: u32,
    #[serde(default)]
    pub exited: u32,
    #[serde(default)]
    pub stopped: u32,
    #[serde(default)]
    pub lost: u32,
    /// Rows inside an opted-in boot-restart backoff window.
    #[serde(default)]
    pub restarting: u32,
    /// Rows whose boot restart was refused or exhausted.
    #[serde(default)]
    pub blocked: u32,
    #[serde(default)]
    pub output_bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListSessionsParams {
    #[serde(default)]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionListResult {
    pub sessions: Vec<Session>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListEventsParams {
    pub session_id: String,
    #[serde(default)]
    pub since_seq: u64,
    #[serde(default = "default_event_limit")]
    pub limit: u32,
}

fn default_event_limit() -> u32 {
    200
}

#[derive(Debug, Clone, Serialize)]
pub struct EventListResult {
    pub events: Vec<Event>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionTranscriptParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionTranscriptResult {
    pub session_id: String,
    pub markdown: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchParams {
    pub query: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    /// Stable record identifier: message ID, or `session:seq` for events.
    pub id: String,
    pub session_id: String,
    pub project_id: String,
    pub session_goal: String,
    /// `message` or `event`.
    pub source: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub seq: Option<u64>,
    pub snippet: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub query: String,
    pub hits: Vec<SearchHit>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubscribeParams {
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthParams {
    pub token: String,
    /// Optional client protocol major version. Older clients omit it and are
    /// treated as protocol 1.
    #[serde(default)]
    pub protocol: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthResult {
    pub ok: bool,
    pub server: ServerInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub tool_calls: Value,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: String,
    pub session_id: String,
    pub from_agent_id: String,
    pub from_name: String,
    pub to_agent_id: String,
    pub to_name: String,
    pub kind: String,
    pub body: String,
    #[serde(default)]
    pub payload: Value,
    pub status: String,
    pub created_at: String,
    #[serde(default)]
    pub read_at: Option<String>,
}

fn default_memory_status() -> String {
    "active".to_string()
}

fn default_memory_source() -> String {
    "agent".to_string()
}

fn default_task_result_verify_days() -> Option<u32> {
    Some(90)
}

fn default_memory_verify_days() -> Option<u32> {
    Some(365)
}

fn default_brief_max_chars() -> Option<u32> {
    Some(4000)
}

/// One provenance reference attached to a memory. Kinds are validated strings
/// (`commit`, `path`, `task`, `session`, `event`, `url`) so unknown future
/// values still decode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryEvidence {
    pub kind: String,
    pub reference: String,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryItem {
    pub id: String,
    pub session_id: String,
    pub project_id: String,
    pub agent_id: String,
    pub agent_name: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub created_at: String,
    /// `active`, `stale`, `superseded`, or `archived`.
    #[serde(default = "default_memory_status")]
    pub status: String,
    /// `agent`, `task_result`, `operator`, `orchestrator`, or `import`.
    #[serde(default = "default_memory_source")]
    pub source: String,
    #[serde(default)]
    pub evidence: Vec<MemoryEvidence>,
    #[serde(default)]
    pub supersedes: Vec<String>,
    #[serde(default)]
    pub superseded_by: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub use_count: u32,
    #[serde(default)]
    pub last_used_at: Option<String>,
    #[serde(default)]
    pub last_verified_at: Option<String>,
    #[serde(default)]
    pub verify_after: Option<String>,
    #[serde(default)]
    pub stale_reason: Option<String>,
}

/// A ranked memory plus the deterministic reasons it surfaced.
///
/// The score fields are additive: lexical-only clients can ignore them, and
/// they are absent in responses produced before hybrid recall existed. Raw
/// vectors are never part of this type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryMatch {
    pub memory: MemoryItem,
    pub score: f64,
    pub reasons: Vec<String>,
    /// Clamped lexical relevance in `0..=1` used for this match.
    #[serde(default)]
    pub lexical_score: Option<f64>,
    /// Cosine similarity in `0..=1` when a current embedding matched.
    #[serde(default)]
    pub semantic_score: Option<f64>,
    /// `lexical`, `semantic`, `hybrid`, or `structural`.
    #[serde(default)]
    pub source: Option<String>,
    /// Populated for every returned row when hybrid recall fell back to the
    /// lexical lane. Never contains endpoint credentials.
    #[serde(default)]
    pub semantic_unavailable: Option<String>,
}

/// Result-level hybrid recall status. `fallback` is set when semantic recall
/// was requested but could not run (disabled, unhealthy, timeout, bad
/// response); ranking then stays lexical and the reason is also copied onto
/// each match.
///
/// `index_source` is `scan` (the default exact lane), `ivf` (a valid ANN probe
/// contributed candidates), or absent on older daemons. `index_fallback` is
/// independent from `fallback`: an IVF index can fail its validation while the
/// exact semantic scan still succeeds, in which case semantic recall remains
/// active and only the index fallback is reported. Neither field carries
/// vectors, centroids, or user text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemorySemanticSummary {
    pub enabled: bool,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub lexical_candidates: u32,
    #[serde(default)]
    pub semantic_candidates: u32,
    #[serde(default)]
    pub fallback: Option<String>,
    /// `scan` or `ivf`. Absent means the daemon predates the ANN index.
    #[serde(default)]
    pub index_source: Option<String>,
    /// Derived index generation used for this recall, when the index was
    /// built and selected.
    #[serde(default)]
    pub index_generation: Option<u64>,
    /// Why the ANN probe was not used. Semantic recall still ran through the
    /// exact scan unless `fallback` is also set.
    #[serde(default)]
    pub index_fallback: Option<String>,
    /// Number of centroids probed for this recall (0 for a scan).
    #[serde(default)]
    pub probed_clusters: u32,
    /// Number of stored vectors whose cosine was computed after probing.
    #[serde(default)]
    pub candidate_vectors: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryListParams {
    pub project_id: String,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    /// Exact status filter. Takes precedence over `include_stale`.
    #[serde(default)]
    pub status: Option<String>,
    /// Include `stale` rows when no exact status is requested.
    #[serde(default)]
    pub include_stale: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryListResult {
    pub memories: Vec<MemoryItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemorySearchParams {
    pub project_id: String,
    #[serde(default)]
    pub query: String,
    /// Legacy single-kind filter, kept for symmetry with `memory.list`.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub department: Option<String>,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub include_stale: bool,
    /// Optional total brief text budget. When set, low-ranked matches are
    /// dropped instead of returning more text than requested.
    #[serde(default)]
    pub max_chars: Option<u32>,
    /// Force the lexical lane for this request even when the project has an
    /// enabled embedding policy. Performs no embedding request.
    #[serde(default)]
    pub lexical_only: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemorySearchResult {
    pub memories: Vec<MemoryMatch>,
    /// Hybrid recall status. Absent on older daemons.
    #[serde(default)]
    pub semantic: Option<MemorySemanticSummary>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryBriefParams {
    pub session_id: String,
    #[serde(default)]
    pub task_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryBriefResult {
    pub text: String,
    pub memories: Vec<MemoryMatch>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryUpdateParams {
    pub id: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub pinned: Option<bool>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryUpdateResult {
    pub memory: MemoryItem,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryVerifyParams {
    pub id: String,
}

/// Per-evidence outcome for `memory.verify`. `status` is `verified`, `missing`,
/// or `unchecked`; unchecked evidence is reported, never silently approved as
/// fact-checked.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryVerificationCheck {
    pub evidence: MemoryEvidence,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryVerifyResult {
    pub memory: MemoryItem,
    pub checks: Vec<MemoryVerificationCheck>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemorySweepParams {
    pub project_id: String,
    #[serde(default)]
    pub older_than_days: Option<u32>,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemorySweepResult {
    pub project_id: String,
    pub candidates: usize,
    pub marked_stale: usize,
    pub memory_ids: Vec<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryStatsParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryStatCount {
    pub key: String,
    pub count: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryStatsResult {
    pub project_id: String,
    pub total: u32,
    pub by_status: Vec<MemoryStatCount>,
    pub by_source: Vec<MemoryStatCount>,
    pub by_kind: Vec<MemoryStatCount>,
}

fn default_memory_conflict_status() -> String {
    "open".to_string()
}

/// A deterministic, explainable review row for two active memories that cover
/// the same path or task evidence but have different bodies. Detection never
/// mutates memory; an explicit operator resolution does.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryConflict {
    pub id: String,
    pub project_id: String,
    /// Session that owns the detection event (memory A's session).
    pub session_id: String,
    pub memory_a_id: String,
    pub memory_b_id: String,
    pub memory_a_title: String,
    pub memory_b_title: String,
    pub memory_a_kind: String,
    pub memory_b_kind: String,
    pub memory_a_excerpt: String,
    pub memory_b_excerpt: String,
    /// `path` or `task`.
    pub scope_kind: String,
    pub scope_value: String,
    pub reason: String,
    /// `open` or `resolved`.
    #[serde(default = "default_memory_conflict_status")]
    pub status: String,
    pub detected_at: String,
    #[serde(default)]
    pub resolved_at: Option<String>,
    /// `keep_a`, `keep_b`, `keep_both`, or `archive_both`.
    #[serde(default)]
    pub resolution: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryConflictsParams {
    pub project_id: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryConflictsResult {
    pub project_id: String,
    pub conflicts: Vec<MemoryConflict>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryConflictScanParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryConflictScanResult {
    pub project_id: String,
    /// Number of conflicts inserted by this scan.
    pub detected: u32,
    /// Number of open conflicts after the scan.
    pub open: u32,
    pub conflicts: Vec<MemoryConflict>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryConflictResolveParams {
    pub id: String,
    /// `keep_a`, `keep_b`, `keep_both`, or `archive_both`.
    pub action: String,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryConflictResolveResult {
    pub conflict: MemoryConflict,
    pub memories: Vec<MemoryItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryHealthParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryHealthResult {
    pub project_id: String,
    pub total: u32,
    pub active: u32,
    pub stale: u32,
    pub superseded: u32,
    pub archived: u32,
    pub pinned: u32,
    /// Active rows with at least one evidence entry.
    pub evidence_covered: u32,
    /// Rows with a recorded `last_verified_at`.
    pub verified: u32,
    /// Active, non-pinned rows whose `verify_after` is in the past.
    pub verification_due: u32,
    pub open_conflicts: u32,
    pub by_kind: Vec<MemoryStatCount>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryCreateParams {
    pub session_id: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryCreateResult {
    pub memory: MemoryItem,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryDeleteParams {
    pub id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryDeleteResult {
    pub memory_id: String,
    pub removed: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryPruneParams {
    pub project_id: String,
    #[serde(default)]
    pub older_than_days: Option<u32>,
    #[serde(default)]
    pub keep_latest: Option<u32>,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryPruneResult {
    pub candidates: usize,
    pub removed: usize,
    pub dry_run: bool,
}

/// Retrieval index mode for the optional semantic memory lane.
///
/// `Scan` (the default) scores a bounded exact candidate window. `Ivf` is an
/// explicit, project-scoped opt-in to a derived inverted-file index. The mode
/// never changes the embedding provider, model, or budget, and disabling
/// embeddings performs zero index work in either mode.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryIndexMode {
    #[default]
    Scan,
    Ivf,
}

impl MemoryIndexMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scan => "scan",
            Self::Ivf => "ivf",
        }
    }
}

/// Opt-in semantic lane for project memory.
///
/// The endpoint is an explicit OpenAI-compatible `/embeddings` base URL. The
/// API key is *never* stored: `api_key_env` names a daemon environment
/// variable that is read at call time. Disabled (the default) performs no
/// network calls and leaves lexical recall byte-for-byte unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryEmbeddingPolicy {
    #[serde(default)]
    pub enabled: bool,
    /// Base URL or full embeddings URL. When it does not end in `/embeddings`
    /// the runtime appends it. Query strings and userinfo are rejected.
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default = "default_embedding_dimensions")]
    pub dimensions: u32,
    #[serde(default = "default_embedding_batch_size")]
    pub batch_size: u32,
    #[serde(default = "default_embedding_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_embedding_max_candidates")]
    pub max_candidates: u32,
    /// Minimum cosine similarity required to contribute a semantic signal.
    #[serde(default = "default_embedding_min_similarity")]
    pub min_similarity: f32,
    /// Environment variable name only; the value is read in the daemon and
    /// never persisted, returned by status, or logged.
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// `scan` keeps exact bounded recall. `ivf` opts into a derived,
    /// rebuildable inverted-file index with exact cosine scoring of probed
    /// candidates and an automatic scan fallback.
    #[serde(default)]
    pub index: MemoryIndexMode,
    /// Target number of k-means centroids. Bounded 4–1024.
    #[serde(default = "default_ivf_nlist")]
    pub ivf_nlist: u32,
    /// Number of nearest centroids probed per recall. Bounded 1..=nlist.
    #[serde(default = "default_ivf_nprobe")]
    pub ivf_nprobe: u32,
    /// Below this many valid vectors, recall uses the exact scan.
    #[serde(default = "default_ivf_min_vectors")]
    pub ivf_min_vectors: u32,
    /// Growth threshold that triggers a deterministic centroid rebuild.
    #[serde(default = "default_ivf_rebuild_vectors")]
    pub ivf_rebuild_vectors: u32,
}

fn default_ivf_nlist() -> u32 {
    64
}

fn default_ivf_nprobe() -> u32 {
    8
}

fn default_ivf_min_vectors() -> u32 {
    256
}

fn default_ivf_rebuild_vectors() -> u32 {
    4096
}

fn default_embedding_dimensions() -> u32 {
    1536
}

fn default_embedding_batch_size() -> u32 {
    16
}

fn default_embedding_timeout_secs() -> u64 {
    30
}

fn default_embedding_max_candidates() -> u32 {
    64
}

fn default_embedding_min_similarity() -> f32 {
    0.20
}

impl Default for MemoryEmbeddingPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: None,
            model: None,
            dimensions: default_embedding_dimensions(),
            batch_size: default_embedding_batch_size(),
            timeout_secs: default_embedding_timeout_secs(),
            max_candidates: default_embedding_max_candidates(),
            min_similarity: default_embedding_min_similarity(),
            api_key_env: None,
            index: MemoryIndexMode::Scan,
            ivf_nlist: default_ivf_nlist(),
            ivf_nprobe: default_ivf_nprobe(),
            ivf_min_vectors: default_ivf_min_vectors(),
            ivf_rebuild_vectors: default_ivf_rebuild_vectors(),
        }
    }
}

impl MemoryEmbeddingPolicy {
    /// Configured means endpoint and model are both non-empty. A policy object
    /// may be configured but disabled; it then still performs no automatic
    /// work and no recall calls.
    pub fn is_configured(&self) -> bool {
        self.endpoint
            .as_deref()
            .is_some_and(|endpoint| !endpoint.trim().is_empty())
            && self
                .model
                .as_deref()
                .is_some_and(|model| !model.trim().is_empty())
    }

    /// True only when the semantic lane is enabled, configured, and the
    /// operator explicitly selected the derived IVF index.
    pub fn uses_ivf_index(&self) -> bool {
        self.enabled && self.is_configured() && self.index == MemoryIndexMode::Ivf
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryPolicy {
    /// Auto-prune retention toggle. Staleness sweeping is `sweep_stale`.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub max_items: Option<u32>,
    #[serde(default)]
    pub max_age_days: Option<u32>,
    /// TTL applied to task-result memories after auto-capture.
    #[serde(default = "default_task_result_verify_days")]
    pub task_result_verify_days: Option<u32>,
    /// TTL applied to explicitly created memories and successful verifies.
    #[serde(default = "default_memory_verify_days")]
    pub default_verify_days: Option<u32>,
    /// Allow a same-key create with different content to supersede the previous
    /// active row instead of creating a second active row.
    #[serde(default = "default_true")]
    pub auto_supersede: bool,
    /// Character budget for an injected memory brief.
    #[serde(default = "default_brief_max_chars")]
    pub brief_max_chars: Option<u32>,
    /// Opt-in daemon sweep that marks expired memories stale. Never deletes.
    #[serde(default)]
    pub sweep_stale: bool,
    /// Optional hybrid retrieval lane. Absent means lexical-only with zero
    /// embedding network calls.
    #[serde(default)]
    pub embedding: Option<MemoryEmbeddingPolicy>,
}

impl Default for MemoryPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            max_items: Some(1000),
            max_age_days: Some(180),
            task_result_verify_days: Some(90),
            default_verify_days: Some(365),
            auto_supersede: true,
            brief_max_chars: Some(4000),
            sweep_stale: false,
            embedding: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryPolicyParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetMemoryPolicyParams {
    pub project_id: String,
    pub policy: MemoryPolicy,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryPolicyResult {
    pub project_id: String,
    pub policy: MemoryPolicy,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryEmbeddingsStatusParams {
    pub project_id: String,
}

/// Bounded operational state for one embedding job. Contains no vectors.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryEmbeddingJobStatus {
    pub memory_id: String,
    pub title: String,
    /// `pending`, `indexed`, `failed`, or `skipped`.
    pub status: String,
    pub attempts: u32,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub next_attempt_at: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct MemoryEmbeddingUsage {
    /// Number of `/embeddings` HTTP call batches.
    pub calls: u64,
    /// Tokens reported by the endpoint, or estimated when absent.
    pub input_tokens: u64,
    /// Portion of `input_tokens` that is a bytes/4 estimate.
    pub estimated_tokens: u64,
    /// Exact UTF-8 bytes sent as embedding text.
    pub bytes: u64,
    /// Calls where the endpoint reported a token count.
    pub provider_reported_calls: u64,
}

/// Bounded health of the derived IVF index. This is *operational* state: it
/// can be deleted and rebuilt from embeddings at any time, is not a replay
/// projection, and never contains centroids, vectors, or embedded text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MemoryEmbeddingIndexStatus {
    /// `scan` or `ivf` (the configured mode).
    pub mode: String,
    /// True when a structurally valid, generation-matched index can serve
    /// recall right now.
    pub ready: bool,
    /// Monotonic derived index generation; absent before the first build.
    pub generation: Option<u64>,
    /// Model and dimensions the stored index was built from.
    pub model: Option<String>,
    pub dimensions: u32,
    /// Current valid vectors eligible for semantic recall.
    pub vector_count: u32,
    /// Vectors with an assignment in the current generation.
    pub assigned_vectors: u32,
    /// Assignments that are stale, out of range, or lack a current valid
    /// vector. Maintenance removes them; a nonzero value is diagnostic.
    pub invalid_assignments: u32,
    pub cluster_count: u32,
    pub nlist: u32,
    pub nprobe: u32,
    /// `assigned_vectors / vector_count`, or 0 when there are no vectors.
    pub coverage: f64,
    pub min_vectors: u32,
    pub rebuild_vectors: u32,
    /// Centroid blob size. Never the blob itself.
    pub bytes: u64,
    pub last_rebuilt_at: Option<String>,
    /// Last automatic or explicit build error, sanitized by the builder.
    pub last_error: Option<String>,
    /// Recalls that fell back to the exact scan because the index could not
    /// be used. This counter is diagnostic only.
    pub fallback_count: u64,
    pub last_fallback_at: Option<String>,
    pub last_fallback_reason: Option<String>,
    /// Why the index is not ready now (`not_built`, `below_min_vectors`,
    /// `model_mismatch`, `dimensions_mismatch`, `nlist_mismatch`,
    /// `index_corrupt: ...`, `no_assignments`, `scan_mode`, ...).
    pub fallback_reason: Option<String>,
}

impl Default for MemoryEmbeddingIndexStatus {
    fn default() -> Self {
        Self {
            mode: "scan".to_string(),
            ready: false,
            generation: None,
            model: None,
            dimensions: default_embedding_dimensions(),
            vector_count: 0,
            assigned_vectors: 0,
            invalid_assignments: 0,
            cluster_count: 0,
            nlist: default_ivf_nlist(),
            nprobe: default_ivf_nprobe(),
            coverage: 0.0,
            min_vectors: default_ivf_min_vectors(),
            rebuild_vectors: default_ivf_rebuild_vectors(),
            bytes: 0,
            last_rebuilt_at: None,
            last_error: None,
            fallback_count: 0,
            last_fallback_at: None,
            last_fallback_reason: None,
            fallback_reason: None,
        }
    }
}

impl MemoryEmbeddingIndexStatus {
    pub fn coverage_label(&self) -> String {
        format!("{:.1}%", self.coverage.clamp(0.0, 1.0) * 100.0)
    }
}

/// Health and coverage snapshot. Deliberately excludes vectors, request
/// bodies, endpoint query strings, and credentials.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryEmbeddingsStatusResult {
    pub project_id: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub configured: bool,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default = "default_embedding_dimensions")]
    pub dimensions: u32,
    /// Memories eligible for semantic retrieval (active and stale).
    #[serde(default)]
    pub indexable: u32,
    /// Eligible memories with a current, structurally valid model/dimension
    /// embedding row.
    #[serde(default)]
    pub indexed: u32,
    /// Current-model rows whose vector blob fails the header/length checks.
    /// They are not counted as indexed and are re-queued by the reconciler.
    #[serde(default)]
    pub invalid: u32,
    /// Jobs waiting for the next reconciler tick.
    #[serde(default)]
    pub pending: u32,
    /// Jobs whose retry budget is exhausted or whose last call failed.
    #[serde(default)]
    pub failed: u32,
    /// Memories intentionally not indexed (archived/superseded or unchanged).
    #[serde(default)]
    pub skipped: u32,
    /// `indexed / indexable`, or 0 when there is nothing to index.
    #[serde(default)]
    pub coverage: f64,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub usage: MemoryEmbeddingUsage,
    /// At most 20 recent job rows, newest first.
    #[serde(default)]
    pub recent_jobs: Vec<MemoryEmbeddingJobStatus>,
    /// Derived ANN index health. Absent on older daemons and when the policy
    /// has no embedding object.
    #[serde(default)]
    pub index: Option<MemoryEmbeddingIndexStatus>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryEmbeddingsIndexStatusParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryEmbeddingsIndexStatusResult {
    pub project_id: String,
    pub index: MemoryEmbeddingIndexStatus,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryEmbeddingsIndexRebuildParams {
    pub project_id: String,
}

/// Explicit derived-index rebuild outcome. `rebuilt = false` is not an error:
/// the index may be disabled, in scan mode, or below the policy minimum.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryEmbeddingsIndexRebuildResult {
    pub project_id: String,
    #[serde(default)]
    pub rebuilt: bool,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub generation: Option<u64>,
    #[serde(default)]
    pub vector_count: u32,
    #[serde(default)]
    pub assigned_vectors: u32,
    #[serde(default)]
    pub cluster_count: u32,
    #[serde(default)]
    pub nlist: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryEmbeddingsRebuildParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryEmbeddingsRebuildResult {
    pub project_id: String,
    pub enabled: bool,
    pub model: Option<String>,
    pub dimensions: u32,
    /// Existing derived rows removed for the project.
    pub cleared: u32,
    /// Eligible memories queued for re-indexing.
    pub enqueued: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryEmbeddingsTestParams {
    pub project_id: String,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryEmbeddingsTestResult {
    pub project_id: String,
    pub ok: bool,
    pub model: Option<String>,
    pub dimensions: u32,
    pub latency_ms: u64,
    pub input_tokens: u64,
    pub estimated_tokens: u64,
    /// True when the endpoint reported `usage`.
    pub provider_reported: bool,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Question {
    pub id: String,
    pub session_id: String,
    pub agent_id: String,
    pub agent_name: String,
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
    pub status: String,
    #[serde(default)]
    pub answer: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub answered_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QuestionListParams {
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QuestionListResult {
    pub questions: Vec<Question>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnswerQuestionParams {
    pub question_id: String,
    pub answer: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnswerQuestionResult {
    pub answered: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpTransport {
    #[default]
    Stdio,
    /// MCP Streamable HTTP: POST JSON-RPC to one endpoint, optionally receiving
    /// a JSON or SSE response, with an `Mcp-Session-Id` for session affinity.
    Http,
    /// Legacy HTTP+SSE transport: a long-lived SSE stream plus a POST endpoint
    /// announced by the stream's `endpoint` event.
    Sse,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerConfig {
    pub name: String,
    #[serde(default)]
    pub transport: McpTransport,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServer {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub transport: McpTransport,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub enabled: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddMcpServerParams {
    #[serde(flatten)]
    pub config: McpServerConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateMcpServerParams {
    pub id: String,
    #[serde(flatten)]
    pub config: McpServerConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpProbeParams {
    pub server_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpProbeResult {
    pub server_id: String,
    pub name: String,
    pub transport: McpTransport,
    #[serde(default)]
    pub server_name: String,
    #[serde(default)]
    pub server_version: String,
    #[serde(default)]
    pub protocol_version: String,
    pub tools: usize,
    pub resources: usize,
    pub prompts: usize,
    pub resources_supported: bool,
    pub prompts_supported: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpServerIdParams {
    pub server_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpServerListResult {
    pub servers: Vec<McpServer>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpToolListResult {
    pub server_id: String,
    pub tools: Vec<McpTool>,
}

/// How VerlinPark reaches an external Agent Client Protocol (ACP) agent.
///
/// `stdio` launches a user-installed executable; the two remote profiles speak
/// ACP over the RFD Streamable HTTP / WebSocket transport. Remote agents are
/// trusted external services and are not sandboxed by VerlinPark.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpTransport {
    #[default]
    Stdio,
    StreamableHttp,
    Websocket,
}

impl AcpTransport {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::StreamableHttp => "streamable_http",
            Self::Websocket => "websocket",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "stdio" => Some(Self::Stdio),
            "streamable_http" | "streamable-http" | "http" => Some(Self::StreamableHttp),
            "websocket" | "ws" | "wss" => Some(Self::Websocket),
            _ => None,
        }
    }

    pub fn is_remote(self) -> bool {
        !matches!(self, Self::Stdio)
    }
}

/// A configured external agent that speaks the Agent Client Protocol.
///
/// A stdio ACP agent runs as a user-installed executable, is spawned inside the
/// same Seatbelt write sandbox as shell work, and is recorded as a Worker agent
/// when it is delegated a task. A remote agent is reached over Streamable HTTP
/// or WebSocket, is always an explicitly opted-in trusted external service, and
/// is **not** sandboxed or confined to the local worktree.
///
/// Remote credentials never live in these fields: `auth_header_env` names an
/// environment variable and header values may reference `${ENV_VAR}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcpAgent {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub enabled: bool,
    /// Skip VerlinPark approval prompts for this agent's own permission requests.
    #[serde(default)]
    pub auto_approve: bool,
    #[serde(default = "default_acp_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub transport: AcpTransport,
    /// Remote endpoint (`http`/`https` for Streamable HTTP, `ws`/`wss` for
    /// WebSocket). Always empty for stdio agents.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Environment variable name holding the `Authorization` header value.
    /// The value is read from the daemon environment and never persisted.
    #[serde(default)]
    pub auth_header_env: Option<String>,
    /// Additional request headers. Values may contain `${ENV_VAR}` references;
    /// expanded values are never persisted or logged.
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
    #[serde(default = "default_acp_connect_timeout")]
    pub connect_timeout_secs: u64,
    /// Explicit opt-in required before VerlinPark contacts a remote agent.
    #[serde(default)]
    pub allow_remote: bool,
    pub created_at: String,
}

fn default_acp_timeout() -> u64 {
    1_800
}

fn default_acp_connect_timeout() -> u64 {
    30
}

/// Validated settings for creating or updating an ACP agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpAgentConfig {
    pub name: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    pub enabled: bool,
    #[serde(default)]
    pub auto_approve: bool,
    #[serde(default = "default_acp_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub transport: AcpTransport,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub auth_header_env: Option<String>,
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
    #[serde(default = "default_acp_connect_timeout")]
    pub connect_timeout_secs: u64,
    #[serde(default)]
    pub allow_remote: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddAcpAgentParams {
    pub name: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub auto_approve: bool,
    #[serde(default = "default_acp_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub transport: AcpTransport,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub auth_header_env: Option<String>,
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
    #[serde(default = "default_acp_connect_timeout")]
    pub connect_timeout_secs: u64,
    #[serde(default)]
    pub allow_remote: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateAcpAgentParams {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub auto_approve: bool,
    #[serde(default = "default_acp_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub transport: AcpTransport,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub auth_header_env: Option<String>,
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
    #[serde(default = "default_acp_connect_timeout")]
    pub connect_timeout_secs: u64,
    #[serde(default)]
    pub allow_remote: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AcpAgentIdParams {
    pub agent_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AcpProbeParams {
    pub agent_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    /// Create a session as part of the probe. Disable this for a pure
    /// initialize/transport health check that leaves no server-side session.
    #[serde(default = "default_true")]
    pub create_session: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AcpAgentListResult {
    pub agents: Vec<AcpAgent>,
}

/// Negotiated ACP capabilities observed during a probe.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcpProbeCapabilities {
    #[serde(default)]
    pub load_session: bool,
    #[serde(default)]
    pub mcp_http: bool,
    #[serde(default)]
    pub mcp_sse: bool,
    #[serde(default)]
    pub prompt_image: bool,
    #[serde(default)]
    pub prompt_audio: bool,
    #[serde(default)]
    pub prompt_embedded_context: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpProbeResult {
    pub agent_id: String,
    pub agent_name: String,
    pub version: String,
    pub protocol_version: u16,
    pub session_created: bool,
    #[serde(default)]
    pub transport: AcpTransport,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub remote: bool,
    #[serde(default)]
    pub load_session: bool,
    #[serde(default)]
    pub capabilities: AcpProbeCapabilities,
    /// Negotiated HTTP version for Streamable HTTP probes (`HTTP/1.1`,
    /// `HTTP/2`, …); `None` for stdio and WebSocket.
    #[serde(default)]
    pub http_version: Option<String>,
    #[serde(default)]
    pub messages_sent: u64,
    #[serde(default)]
    pub messages_received: u64,
    #[serde(default)]
    pub streams_opened: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpResource {
    pub uri: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpResourceTemplate {
    pub uri_template: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResourceListResult {
    pub server_id: String,
    pub resources: Vec<McpResource>,
    #[serde(default)]
    pub templates: Vec<McpResourceTemplate>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpReadResourceParams {
    pub server_id: String,
    pub uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResourceContent {
    pub uri: String,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub blob: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpReadResourceResult {
    pub server_id: String,
    pub uri: String,
    pub contents: Vec<McpResourceContent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpPromptArgument {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpPrompt {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub arguments: Vec<McpPromptArgument>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPromptListResult {
    pub server_id: String,
    pub prompts: Vec<McpPrompt>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpGetPromptParams {
    pub server_id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPromptMessage {
    pub role: String,
    pub content: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpGetPromptResult {
    pub server_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub messages: Vec<McpPromptMessage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpCallParams {
    pub server_id: String,
    pub tool: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpCallResult {
    pub server_id: String,
    pub tool: String,
    pub result: Value,
}

/// Explicit derivation metadata for an artifact produced from another
/// artifact. It is additive: older artifacts decode with `None`, and every new
/// field is optional so a mixed-version client never fails to decode an
/// artifact row.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactProvenance {
    /// `archive_extract` or `media_transcode`. Empty for an unknown producer.
    #[serde(default)]
    pub operation: String,
    #[serde(default)]
    pub source_artifact_id: Option<String>,
    #[serde(default)]
    pub source_artifact_name: Option<String>,
    #[serde(default)]
    pub source_sha256: Option<String>,
    #[serde(default)]
    pub source_mime_type: Option<String>,
    /// Normalized archive entry path selected during extraction.
    #[serde(default)]
    pub entry_path: Option<String>,
    /// `zip`, `tar`, or `tar.gz`.
    #[serde(default)]
    pub archive_format: Option<String>,
    /// Extraction operation id; all entries from one extraction share it.
    #[serde(default)]
    pub extraction_id: Option<String>,
    /// SHA-256 of the exact entry bytes before they were stored as an artifact.
    #[serde(default)]
    pub entry_sha256: Option<String>,
    /// Source/input format for a media transcode (for example `mov`).
    #[serde(default)]
    pub source_format: Option<String>,
    /// Requested output format for a media transcode.
    #[serde(default)]
    pub output_format: Option<String>,
    /// Host tool used for an explicit media operation (`ffmpeg`/`ffprobe`).
    #[serde(default)]
    pub tool: Option<String>,
    /// Bounded transcode duration cap in seconds.
    #[serde(default)]
    pub max_duration_secs: Option<u64>,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub id: String,
    pub session_id: String,
    pub agent_id: String,
    pub agent_name: String,
    #[serde(default)]
    pub task_id: Option<String>,
    pub name: String,
    pub kind: String,
    pub mime_type: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub storage: String,
    #[serde(default)]
    pub compression: String,
    #[serde(default)]
    pub size_bytes: u64,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_base64: Option<String>,
    #[serde(default = "default_artifact_version")]
    pub version: u32,
    /// Derivation metadata for archive entries and transcoded media. `None`
    /// means the artifact was authored directly (agent publish, tool output).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ArtifactProvenance>,
    pub created_at: String,
}

fn default_artifact_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArtifactVersion {
    pub artifact_id: String,
    pub version: u32,
    pub name: String,
    pub kind: String,
    pub mime_type: String,
    #[serde(default)]
    pub content: String,
    pub storage: String,
    pub compression: String,
    pub size_bytes: u64,
    pub sha256: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ArtifactProvenance>,
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtifactVersionsParams {
    pub artifact_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactVersionsResult {
    pub versions: Vec<ArtifactVersion>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TextDiffLine {
    /// `context`, `add`, or `delete`.
    pub kind: String,
    #[serde(default)]
    pub old_line: Option<u32>,
    #[serde(default)]
    pub new_line: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TextDiffHunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub lines: Vec<TextDiffLine>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TextDiff {
    pub hunks: Vec<TextDiffHunk>,
    pub additions: u32,
    pub deletions: u32,
    pub unchanged: u32,
    pub identical: bool,
    /// True when the line budget or edit-distance cap forced a bounded result.
    pub truncated: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtifactDiffParams {
    pub artifact_id: String,
    pub from_version: u32,
    /// `None` compares against the current artifact.
    #[serde(default)]
    pub to_version: Option<u32>,
    /// Context lines around each change; clamped by the engine.
    #[serde(default)]
    pub context: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactDiffResult {
    pub artifact_id: String,
    pub name: String,
    pub from_version: u32,
    pub to_version: u32,
    pub diff: TextDiff,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtifactRestoreParams {
    pub artifact_id: String,
    pub version: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactPolicy {
    #[serde(default)]
    pub auto_gc: bool,
    #[serde(default)]
    pub max_bytes: Option<u64>,
    #[serde(default)]
    pub max_age_secs: Option<u64>,
    #[serde(default)]
    pub max_count: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtifactPolicyParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetArtifactPolicyParams {
    pub project_id: String,
    pub policy: ArtifactPolicy,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactPolicyResult {
    pub project_id: String,
    pub policy: ArtifactPolicy,
}

/// Project-scoped policy for spilling oversized tool output into the local
/// content-addressed store and replacing it with a bounded, page-readable
/// preview.
///
/// The policy is deliberately additive: older clients decode defaults, and the
/// runtime never treats a tool-output handle as a workspace path. Thresholds
/// are bytes, not characters, so a multi-byte UTF-8 output never gets a
/// provider-facing preview larger than the configured bound by accident.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutputPolicy {
    #[serde(default = "default_tool_output_enabled")]
    pub enabled: bool,
    #[serde(default = "default_tool_output_spill_threshold")]
    pub spill_threshold_bytes: u64,
    #[serde(default = "default_tool_output_preview_bytes")]
    pub preview_bytes: u64,
    #[serde(default = "default_tool_output_max_turn_bytes")]
    pub max_total_bytes_per_turn: u64,
    #[serde(default = "default_tool_output_retention_secs")]
    pub retention_secs: u64,
    #[serde(default = "default_tool_output_max_entries")]
    pub max_entries: u64,
    /// Optional per-tool spill thresholds. Recognized tools are
    /// `read_file`, `read_files`, `grep`, `shell`, `run_project_task`, `git`,
    /// `remote_exec`, `code_diagnostics`, and `browser`; unknown names are
    /// retained but ignored until a matching tool call appears.
    #[serde(default)]
    pub tool_thresholds: BTreeMap<String, u64>,
}

impl Default for ToolOutputPolicy {
    fn default() -> Self {
        Self {
            enabled: default_tool_output_enabled(),
            spill_threshold_bytes: default_tool_output_spill_threshold(),
            preview_bytes: default_tool_output_preview_bytes(),
            max_total_bytes_per_turn: default_tool_output_max_turn_bytes(),
            retention_secs: default_tool_output_retention_secs(),
            max_entries: default_tool_output_max_entries(),
            tool_thresholds: BTreeMap::new(),
        }
    }
}

fn default_tool_output_enabled() -> bool {
    true
}

fn default_tool_output_spill_threshold() -> u64 {
    16 * 1024
}

fn default_tool_output_preview_bytes() -> u64 {
    4 * 1024
}

fn default_tool_output_max_turn_bytes() -> u64 {
    64 * 1024
}

fn default_tool_output_retention_secs() -> u64 {
    7 * 24 * 60 * 60
}

fn default_tool_output_max_entries() -> u64 {
    2_000
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolOutputPolicyParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetToolOutputPolicyParams {
    pub project_id: String,
    pub policy: ToolOutputPolicy,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolOutputPolicyResult {
    pub project_id: String,
    pub policy: ToolOutputPolicy,
}

/// Metadata for one spilled tool output. It never contains the output body:
/// callers that need content must go through the daemon-owned
/// `read_tool_output` tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutputEntry {
    pub id: String,
    pub session_id: String,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    pub tool_name: String,
    pub sha256: String,
    pub bytes: u64,
    pub lines: u64,
    pub preview_bytes: u64,
    pub created_at: String,
    #[serde(default)]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutputToolStat {
    pub tool_name: String,
    pub entries: u64,
    pub bytes: u64,
    pub preview_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutputStatus {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    pub policy: ToolOutputPolicy,
    pub entries: u64,
    pub total_bytes: u64,
    pub preview_bytes: u64,
    pub active_entries: u64,
    pub expired_entries: u64,
    #[serde(default)]
    pub oldest_created_at: Option<String>,
    #[serde(default)]
    pub newest_created_at: Option<String>,
    /// Per-tool spill counts and bytes, sorted by tool name. Metadata only.
    #[serde(default)]
    pub by_tool: Vec<ToolOutputToolStat>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ToolOutputStatusParams {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ToolOutputListParams {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    /// Exclusive creation-time cursor for older entries.
    #[serde(default)]
    pub before_created_at: Option<String>,
    /// Include rows whose `expires_at` has passed but which the sweep has not
    /// removed yet. Defaults to false so operator lists match paging behavior.
    #[serde(default)]
    pub include_expired: bool,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolOutputListResult {
    pub entries: Vec<ToolOutputEntry>,
    pub total: u64,
    pub truncated: bool,
    /// Pass as `before_created_at` to fetch the next page.
    #[serde(default)]
    pub next_before: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ToolOutputPruneParams {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    /// Override the project retention age for this sweep. Values below 60 are
    /// rejected rather than silently deleting live handles.
    #[serde(default)]
    pub older_than_secs: Option<u64>,
    /// Override the project entry cap for this sweep.
    #[serde(default)]
    pub max_entries: Option<u64>,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ToolOutputAuditParams {
    #[serde(default)]
    pub project_id: Option<String>,
    /// Re-hash each selected blob. This is O(stored bytes) and is off by
    /// default for routine operator checks.
    #[serde(default)]
    pub verify_hashes: bool,
    /// Remove content-addressed files with no metadata row. Metadata is never
    /// removed by an audit.
    #[serde(default)]
    pub clean_orphans: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ToolOutputAuditResult {
    #[serde(default)]
    pub project_id: Option<String>,
    pub rows: u64,
    pub blob_files: u64,
    pub missing_blobs: u64,
    pub hash_mismatches: u64,
    pub unsafe_blob_files: u64,
    pub orphan_blobs: u64,
    pub orphan_bytes: u64,
    pub cleaned_orphans: u64,
    pub cleaned_bytes: u64,
    pub checked_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolOutputPruneResult {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    pub candidates: u64,
    pub removed: u64,
    pub removed_bytes: u64,
    pub expired_removed: u64,
    pub overflow_removed: u64,
    pub protected_active: u64,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailboxPolicy {
    #[serde(default)]
    pub max_age_secs: Option<u64>,
    #[serde(default)]
    pub max_count: Option<u64>,
    #[serde(default)]
    pub delete_unread: bool,
    #[serde(default)]
    pub auto_gc: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MailboxPolicyParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetMailboxPolicyParams {
    pub project_id: String,
    pub policy: MailboxPolicy,
}

#[derive(Debug, Clone, Serialize)]
pub struct MailboxPolicyResult {
    pub project_id: String,
    pub policy: MailboxPolicy,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MailboxGcParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MailboxGcResult {
    pub removed: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtifactGcParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactGcResult {
    pub removed: usize,
    pub bytes_freed: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MailboxListParams {
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub unread_only: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MailboxListResult {
    pub messages: Vec<AgentMessage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MarkMessageReadParams {
    pub message_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentMessageResult {
    pub message: AgentMessage,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtifactListParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactListResult {
    pub artifacts: Vec<Artifact>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtifactBackfillParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactBackfillResult {
    pub session_id: String,
    pub scanned: usize,
    pub backfilled: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtifactMaterializeParams {
    pub artifact_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactMaterialization {
    pub artifact_id: String,
    pub path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtifactGetParams {
    pub artifact_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactResult {
    pub artifact: Artifact,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ArtifactProvenance>,
}

// ---------------------------------------------------------------------------
// Safe artifact archives
// ---------------------------------------------------------------------------

/// Hard bounds applied to every archive listing and extraction. The values are
/// explicit in the response so a UI can explain why an entry is marked unsafe
/// or why an archive was rejected instead of silently truncating it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchiveLimits {
    #[serde(default = "default_archive_max_entries")]
    pub max_entries: usize,
    #[serde(default = "default_archive_max_entry_bytes")]
    pub max_entry_bytes: u64,
    #[serde(default = "default_archive_max_total_bytes")]
    pub max_total_bytes: u64,
    #[serde(default = "default_archive_max_archive_bytes")]
    pub max_archive_bytes: u64,
    #[serde(default = "default_archive_max_compression_ratio")]
    pub max_compression_ratio: u64,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            max_entries: default_archive_max_entries(),
            max_entry_bytes: default_archive_max_entry_bytes(),
            max_total_bytes: default_archive_max_total_bytes(),
            max_archive_bytes: default_archive_max_archive_bytes(),
            max_compression_ratio: default_archive_max_compression_ratio(),
        }
    }
}

fn default_archive_max_entries() -> usize {
    2_000
}

fn default_archive_max_entry_bytes() -> u64 {
    32 * 1024 * 1024
}

fn default_archive_max_total_bytes() -> u64 {
    128 * 1024 * 1024
}

fn default_archive_max_archive_bytes() -> u64 {
    64 * 1024 * 1024
}

fn default_archive_max_compression_ratio() -> u64 {
    100
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchiveEntry {
    /// Zero-based position in the archive. It is stable for one listing and
    /// keeps duplicate-path rows distinguishable in a client list.
    #[serde(default)]
    pub index: usize,
    /// Normalized relative path with `/` separators. Unsafe paths are retained
    /// for display but can never be selected for extraction.
    pub path: String,
    /// `file`, `directory`, `symlink`, `hardlink`, `device`, `fifo`, or `other`.
    pub kind: String,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compressed_size: Option<u64>,
    /// Unix mode from the archive, when present. It is display metadata only;
    /// the extractor never applies archive modes to the host filesystem.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    /// False when path normalization, the entry kind, duplicate detection, a
    /// size cap, or a compression-ratio cap makes the entry ineligible.
    pub safe: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsafe_reason: Option<String>,
    /// True when an earlier normalized entry already used this path.
    #[serde(default)]
    pub duplicate: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtifactArchiveListParams {
    pub artifact_id: String,
    #[serde(default)]
    pub max_entries: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactArchiveListResult {
    pub artifact_id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub sha256: String,
    /// `zip`, `tar`, or `tar.gz`.
    pub format: String,
    pub entries: Vec<ArchiveEntry>,
    /// Total entries found (including entries beyond the list cap).
    pub entry_count: usize,
    /// True when `max_entries` cut the returned list. Extraction refuses a
    /// truncated listing unless only already-listed entries are selected.
    pub truncated: bool,
    pub total_uncompressed_bytes: u64,
    pub total_compressed_bytes: u64,
    pub limits: ArchiveLimits,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtifactArchiveExtractParams {
    pub artifact_id: String,
    /// Normalized entry paths returned by `artifact.archive.list`. The request
    /// is rejected when any selected entry is missing or unsafe.
    #[serde(default)]
    pub entries: Vec<String>,
    /// Select every safe regular file instead of naming entries explicitly.
    #[serde(default)]
    pub all: bool,
    /// Reduce the hard entry cap for this request; it can never raise the
    /// default policy cap.
    #[serde(default)]
    pub max_entries: Option<usize>,
    #[serde(default)]
    pub include_previews: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtractedArchiveEntry {
    pub path: String,
    pub kind: String,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Content-addressed artifact published for this entry, when the bytes fit
    /// the 5 MiB artifact cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_artifact_id: Option<String>,
    /// Bounded preview. Text entries carry UTF-8; binary entries carry a hex
    /// preview. The preview is never the full unbounded file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default)]
    pub preview_encoding: String,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactArchiveExtractResult {
    pub artifact_id: String,
    pub extraction_id: String,
    pub format: String,
    pub entries: Vec<ExtractedArchiveEntry>,
    /// Published content-addressed artifacts, in selection order.
    pub published_artifacts: Vec<Artifact>,
    pub total_bytes: u64,
    /// The daemon-owned scratch directory is removed after publishing; it is
    /// never exposed as a workspace path or retained indefinitely.
    pub scratch_retained: bool,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Explicit media inspection and transcoding
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct MediaInfoParams {
    pub artifact_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MediaStreamInfo {
    pub index: u32,
    #[serde(default)]
    pub codec_type: String,
    #[serde(default)]
    pub codec_name: String,
    #[serde(default)]
    pub codec_long_name: Option<String>,
    #[serde(default)]
    pub width: Option<u64>,
    #[serde(default)]
    pub height: Option<u64>,
    #[serde(default)]
    pub duration_secs: Option<f64>,
    #[serde(default)]
    pub bit_rate: Option<u64>,
    #[serde(default)]
    pub sample_rate: Option<u64>,
    #[serde(default)]
    pub channels: Option<u64>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub frames: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MediaInfo {
    #[serde(default)]
    pub container: Option<String>,
    #[serde(default)]
    pub format_long_name: Option<String>,
    #[serde(default)]
    pub duration_secs: Option<f64>,
    #[serde(default)]
    pub bit_rate: Option<u64>,
    #[serde(default)]
    pub width: Option<u64>,
    #[serde(default)]
    pub height: Option<u64>,
    #[serde(default)]
    pub streams: Vec<MediaStreamInfo>,
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
    /// `ffprobe` when a probe ran, `image-metadata` when still-image metadata
    /// was inspected as a fallback. Never a fabricated value.
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MediaInfoResult {
    pub artifact_id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub info: MediaInfo,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MediaTranscodeParams {
    pub artifact_id: String,
    /// `mp4`, `webm`, `mp3`, `wav`, `png`, or `jpeg`.
    pub format: String,
    /// Requested duration cap. It is clamped to the daemon bound and passed to
    /// ffmpeg as `-t`; the output is rejected if it exceeds the cap.
    #[serde(default)]
    pub max_duration_secs: Option<u64>,
    /// Requested output byte cap. The daemon applies its own hard cap as well.
    #[serde(default)]
    pub max_bytes: Option<u64>,
    /// Explicit confirmation. `false` is rejected with
    /// `APPROVAL_REQUIRED`; the CLI requires `--yes` and the UI confirms.
    #[serde(default)]
    pub approved: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MediaTranscodeResult {
    pub source_artifact_id: String,
    pub artifact: Artifact,
    pub format: String,
    pub duration_secs: Option<f64>,
    pub size_bytes: u64,
    pub elapsed_ms: u64,
    /// Redacted command identity: the tool path/name and codec choice, never
    /// the full argv, source path, or environment.
    pub command_summary: String,
    pub max_duration_secs: u64,
    pub max_bytes: u64,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApprovalGrant {
    pub id: String,
    pub project_id: String,
    pub tool: String,
    pub path_prefix: String,
    pub include_children: bool,
    pub created_by: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApprovalGrantListParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApprovalGrantListResult {
    pub grants: Vec<ApprovalGrant>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApprovalGrantCreateParams {
    pub project_id: String,
    pub tool: String,
    pub path_prefix: String,
    pub include_children: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApprovalGrantCreateResult {
    pub grant: ApprovalGrant,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApprovalGrantDeleteParams {
    pub id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApprovalGrantDeleteResult {
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Approval {
    pub id: String,
    pub session_id: String,
    pub tool_call_id: String,
    pub tool: String,
    pub input: Value,
    pub status: String,
    pub created_at: String,
    #[serde(default)]
    pub resolved_at: Option<String>,
    /// Risk classification captured when the approval was requested.
    #[serde(default)]
    pub approval_risk: Option<String>,
    /// Human-readable explanation of why the approval was requested.
    #[serde(default)]
    pub approval_reason: Option<String>,
    /// Agent that requested the approval. Older rows and events decode as
    /// `None`; the event payload still carries the agent context at top level
    /// for compatibility with the pre-H1 UI.
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub agent_role: Option<String>,
    #[serde(default)]
    pub agent_title: Option<String>,
    /// Plan task blocked by this approval, when the requester supplied one.
    #[serde(default)]
    pub task_id: Option<String>,
    /// SHA-256 of the canonical input scope. This is an identity fingerprint,
    /// not a replacement for the stored input; it never contains command text
    /// or file contents.
    #[serde(default)]
    pub scope_hash: Option<String>,
    /// Deterministic deduplication key: session + agent + tool + scope + risk.
    #[serde(default)]
    pub dedup_key: Option<String>,
    /// Number of current and historical waiters coalesced onto this row. One
    /// operator decision resolves every waiter registered at resolution time.
    #[serde(default)]
    pub waiter_count: u32,
    /// Template that produced the original decision, when one matched.
    #[serde(default)]
    pub template_id: Option<String>,
    /// Snapshot of the plan-task priority at request time, followed by the
    /// attention policy boost when the request is listed.
    #[serde(default)]
    pub priority: i32,
    /// Expiry computed from the attention policy at creation time. Expired
    /// rows are denied, never approved.
    #[serde(default)]
    pub expires_at: Option<String>,
    /// Whether this row was allowed to create an inbox notification. Rows
    /// inside the session/tool/risk coalescing window carry `false` while
    /// remaining fully reviewable in the approval queue.
    #[serde(default)]
    pub notify: bool,
    /// Stable session/tool/risk/window key used by notification coalescing.
    #[serde(default)]
    pub notify_key: Option<String>,
}

/// Project-scoped bounded auto-approval/deny rule. Templates are deliberately
/// smaller than a permission system: they never cover Destructive or HostState
/// risks and never replace the approval queue for high-risk operations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalTemplate {
    pub id: String,
    pub project_id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Exact tool name, prefix glob (`write_*`), or bounded `*`/`?` glob.
    pub tool_pattern: String,
    /// Known risk names. Empty decodes to the low-risk default
    /// (`external`, `workspace_write`); `allow` templates reject HostState and
    /// Destructive risks even if a hand-edited row names them.
    #[serde(default)]
    pub risk_levels: Vec<String>,
    /// Relative path globs. Empty means the whole tool, not an unrestricted
    /// filesystem; absolute paths and `..` are rejected when the template is
    /// created. `*` does not cross `/`; `**` does.
    #[serde(default)]
    pub path_globs: Vec<String>,
    /// `allow`, `deny`, or `escalate`.
    #[serde(default = "default_approval_action")]
    pub action: String,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub max_uses: Option<u32>,
    #[serde(default)]
    pub used_count: u32,
    pub created_by: String,
    pub created_at: String,
    #[serde(default)]
    pub updated_at: Option<String>,
}

impl ApprovalTemplate {
    /// Remaining uses, capped at zero. `None` means unlimited.
    pub fn remaining_uses(&self) -> Option<u32> {
        self.max_uses.map(|max| max.saturating_sub(self.used_count))
    }
}

fn default_approval_action() -> String {
    "allow".to_string()
}

/// Project-scoped approval attention policy. Values are normalized on write and
/// on read so a hand-edited manifest cannot create an unbounded queue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttentionPolicy {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_max_pending_per_session")]
    pub max_pending_per_session: u32,
    #[serde(default = "default_coalesce_window_secs")]
    pub coalesce_window_secs: u64,
    #[serde(default = "default_stale_after_secs")]
    pub stale_after_secs: u64,
    /// Risk threshold whose approvals bypass notification coalescing and get a
    /// longer stale window. They never bypass operator approval.
    #[serde(default = "default_priority_boost_risk")]
    pub priority_boost_risk: String,
    #[serde(default = "default_notify_coalesce_secs")]
    pub notify_coalesce_secs: u64,
}

fn default_max_pending_per_session() -> u32 {
    64
}

fn default_coalesce_window_secs() -> u64 {
    120
}

fn default_stale_after_secs() -> u64 {
    86_400
}

fn default_priority_boost_risk() -> String {
    "host_state".to_string()
}

fn default_notify_coalesce_secs() -> u64 {
    60
}

impl Default for AttentionPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            max_pending_per_session: default_max_pending_per_session(),
            coalesce_window_secs: default_coalesce_window_secs(),
            stale_after_secs: default_stale_after_secs(),
            priority_boost_risk: default_priority_boost_risk(),
            notify_coalesce_secs: default_notify_coalesce_secs(),
        }
    }
}

impl AttentionPolicy {
    /// Clamp all numeric bounds and normalize the boost risk to a known name.
    /// Unknown names fail closed to `host_state` (the conservative middle).
    pub fn normalized(mut self) -> Self {
        self.max_pending_per_session = self.max_pending_per_session.clamp(1, 500);
        self.coalesce_window_secs = self.coalesce_window_secs.min(3_600);
        self.stale_after_secs = self.stale_after_secs.clamp(60, 7 * 24 * 60 * 60);
        self.notify_coalesce_secs = self.notify_coalesce_secs.min(3_600);
        self.priority_boost_risk = match self.priority_boost_risk.as_str() {
            "read_only" | "workspace_write" | "external" | "host_state" | "destructive" => {
                self.priority_boost_risk
            }
            _ => default_priority_boost_risk(),
        };
        self
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApprovalTemplateListParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApprovalTemplateListResult {
    pub templates: Vec<ApprovalTemplate>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApprovalTemplateCreateParams {
    pub project_id: String,
    pub name: String,
    pub tool_pattern: String,
    #[serde(default)]
    pub risk_levels: Vec<String>,
    #[serde(default)]
    pub path_globs: Vec<String>,
    pub action: String,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub max_uses: Option<u32>,
    #[serde(default)]
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApprovalTemplateResult {
    pub template: ApprovalTemplate,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApprovalTemplateUpdateParams {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub tool_pattern: String,
    #[serde(default)]
    pub risk_levels: Vec<String>,
    #[serde(default)]
    pub path_globs: Vec<String>,
    pub action: String,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub max_uses: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApprovalTemplateDeleteParams {
    pub id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApprovalTemplateDeleteResult {
    pub deleted: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApprovalTemplateFromApprovalParams {
    pub approval_id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub max_uses: Option<u32>,
    #[serde(default)]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AttentionPolicyGetParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttentionPolicyResult {
    pub project_id: String,
    pub policy: AttentionPolicy,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AttentionPolicySetParams {
    pub project_id: String,
    pub policy: AttentionPolicy,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApprovalQueueParams {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApprovalQueueResult {
    /// Pending rows in deterministic attention order: risk, blocked-task
    /// criticality, age, then id.
    pub approvals: Vec<Approval>,
    pub policy: AttentionPolicy,
    pub pending_count: u32,
    pub generated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApprovalBatchDecision {
    pub approval_id: String,
    pub approved: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApprovalBatchResolveParams {
    pub decisions: Vec<ApprovalBatchDecision>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApprovalBatchItemResult {
    pub approval_id: String,
    pub resolved: bool,
    #[serde(default)]
    pub status: Option<String>,
    /// Stable Q3-style code (`APPROVAL_NOT_FOUND`, `APPROVAL_NOT_PENDING`).
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub waiter_count: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApprovalBatchResolveResult {
    pub results: Vec<ApprovalBatchItemResult>,
    pub resolved_count: u32,
    pub failed_count: u32,
}

/// One project-scoped approval audit row. Template matches and template
/// mutations are recorded here rather than in a session event stream because
/// they are project configuration. Session-scoped lifecycle events remain in
/// the normal event table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalAuditEvent {
    pub id: String,
    pub project_id: String,
    pub kind: String,
    pub payload: Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApprovalAuditParams {
    pub project_id: String,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApprovalAuditResult {
    pub events: Vec<ApprovalAuditEvent>,
}

/// Cache-read share of the provider input buckets.
///
/// Provider-normalized `input_tokens` is the total input and already contains
/// the cache read/write subsets (ADR-0138). The E1 formula treats the
/// denominator as disjoint buckets:
///
/// ```text
/// uncached = input - cache_read - cache_write
/// ratio    = cache_read / (uncached + cache_read + cache_write)
/// ```
///
/// which simplifies to `cache_read / input` for a consistent provider report.
/// A report where the cache buckets exceed input is clamped to `1.0`, and a
/// zero denominator yields `0.0`. The value is observational only; it is not a
/// billing reconstruction.
pub fn cache_hit_ratio(input_tokens: u64, cache_read_tokens: u64, cache_write_tokens: u64) -> f64 {
    let uncached = input_tokens
        .saturating_sub(cache_read_tokens)
        .saturating_sub(cache_write_tokens);
    let denominator = uncached
        .saturating_add(cache_read_tokens)
        .saturating_add(cache_write_tokens);
    if denominator == 0 {
        0.0
    } else {
        (cache_read_tokens as f64 / denominator as f64).clamp(0.0, 1.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageEntry {
    pub id: String,
    pub session_id: String,
    pub agent_id: String,
    pub agent_name: String,
    pub role: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub department: Option<String>,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Subset of `input_tokens` served from the provider prompt cache.
    #[serde(default)]
    pub cache_read_tokens: u64,
    /// Subset of `input_tokens` written to the provider prompt cache.
    #[serde(default)]
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub estimated: bool,
    #[serde(default)]
    pub cost_usd: f64,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageBucket {
    pub key: String,
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    /// Derived cache-read share for this bucket; see [`cache_hit_ratio`].
    #[serde(default)]
    pub cache_hit_ratio: f64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageTotals {
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    /// Derived cache-read share over all ledger entries; see
    /// [`cache_hit_ratio`]. Not persisted separately.
    #[serde(default)]
    pub cache_hit_ratio: f64,
    pub cost_usd: f64,
    pub by_role: Vec<UsageBucket>,
    pub by_model: Vec<UsageBucket>,
    pub by_agent: Vec<UsageBucket>,
    pub by_task: Vec<UsageBucket>,
    pub by_department: Vec<UsageBucket>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UsageLedgerParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageCsvResult {
    pub session_id: String,
    pub csv: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageLedgerResult {
    pub session_id: String,
    pub entries: Vec<UsageEntry>,
    pub totals: UsageTotals,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectBudget {
    #[serde(default)]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub cost_budget_usd: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectBudgetParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetProjectBudgetParams {
    pub project_id: String,
    pub budget: ProjectBudget,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectUsageParams {
    pub project_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectUsageSession {
    pub session_id: String,
    pub goal: String,
    pub status: String,
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    /// Derived cache-read share for this session; see [`cache_hit_ratio`].
    #[serde(default)]
    pub cache_hit_ratio: f64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectUsageResult {
    pub project_id: String,
    pub sessions: Vec<ProjectUsageSession>,
    pub totals: UsageTotals,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectBudgetResult {
    pub project_id: String,
    pub budget: ProjectBudget,
    pub spent_tokens: u64,
    pub spent_cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProbeResult {
    pub provider: String,
    pub model: String,
    pub latency_ms: u64,
    #[serde(default)]
    pub first_token_ms: u64,
    #[serde(default)]
    pub chunks: u32,
    #[serde(default)]
    pub done: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityInfo {
    #[serde(default)]
    pub worker_limit: usize,
    #[serde(default)]
    pub worker_available: usize,
    #[serde(default)]
    pub labour_limit: usize,
    #[serde(default)]
    pub labour_available: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub configured: bool,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    #[serde(default)]
    pub role_models: RoleModels,
    #[serde(default)]
    pub capacity: CapacityInfo,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub approval_mode: ApprovalMode,
    /// Static tool-bundle matrix. Additive in protocol v1; older clients that
    /// do not know the field ignore it.
    #[serde(default)]
    pub tool_bundles: Vec<ToolBundleCatalogRow>,
}

fn default_context_calibration() -> f64 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContextUsage {
    #[serde(default)]
    pub estimated_tokens: u64,
    #[serde(default)]
    pub context_window: u64,
    #[serde(default)]
    pub threshold_tokens: u64,
    #[serde(default)]
    pub hard_limit_tokens: u64,
    /// `window_ratio`, `environment_override`, or `disabled`.
    #[serde(default)]
    pub threshold_source: Option<String>,
    /// Ratio of the model window used before compaction, in basis points
    /// (9000 = 90%). Zero when the threshold came from an explicit override.
    #[serde(default)]
    pub threshold_ratio_basis_points: u32,
    #[serde(default)]
    pub messages: usize,
    /// Multiplier applied to the heuristic active-context estimate. 1.0 means
    /// raw heuristic; values above 1.0 are evidence-based safety scaling.
    #[serde(default = "default_context_calibration")]
    pub calibration: f64,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContextMeasurement {
    #[serde(default)]
    pub estimated_tokens: u64,
    #[serde(default)]
    pub actual_input_tokens: u64,
    #[serde(default)]
    pub actual_output_tokens: u64,
    /// Raw provider-reported input divided by the heuristic request estimate.
    #[serde(default)]
    pub ratio: f64,
    /// Clamped multiplier actually applied to future context estimates.
    #[serde(default = "default_context_calibration")]
    pub calibration: f64,
    #[serde(default)]
    pub samples: u64,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub messages: usize,
    #[serde(default)]
    pub created_at: String,
}

impl Default for ContextUsage {
    fn default() -> Self {
        Self {
            estimated_tokens: 0,
            context_window: 0,
            threshold_tokens: 0,
            hard_limit_tokens: 0,
            threshold_source: None,
            threshold_ratio_basis_points: 0,
            messages: 0,
            calibration: 1.0,
            created_at: String::new(),
        }
    }
}

impl Default for ContextMeasurement {
    fn default() -> Self {
        Self {
            estimated_tokens: 0,
            actual_input_tokens: 0,
            actual_output_tokens: 0,
            ratio: 1.0,
            calibration: 1.0,
            samples: 0,
            model: String::new(),
            messages: 0,
            created_at: String::new(),
        }
    }
}

/// Deterministic task state included in every compaction summary. The model's
/// free-text summary is advisory; this block is generated from the stored
/// TeamPlan and is authoritative for recovery.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextStructuredState {
    #[serde(default)]
    pub goal: String,
    #[serde(default)]
    pub plan_revision: u32,
    #[serde(default)]
    pub active_task_id: Option<String>,
    #[serde(default)]
    pub active_task_title: Option<String>,
    #[serde(default)]
    pub active_task_department: Option<String>,
    #[serde(default)]
    pub active_task_acceptance: Vec<String>,
    #[serde(default)]
    pub active_task_write_scope: Vec<String>,
    #[serde(default)]
    pub recent_completed_task_ids: Vec<String>,
    #[serde(default)]
    pub generated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ContextCompactionSummary {
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub summarized_messages: usize,
    #[serde(default)]
    pub dropped_messages: usize,
    #[serde(default)]
    pub kept_messages: usize,
    #[serde(default)]
    pub estimated_tokens_before: u64,
    #[serde(default)]
    pub estimated_tokens_after: u64,
    /// Wall-clock duration of this compaction, including a prepared-summary
    /// reuse (which is nearly free) or a synchronous summarizer call.
    #[serde(default)]
    pub latency_ms: u64,
    /// True when this compaction reused a summary prepared below the
    /// compaction threshold.
    #[serde(default)]
    pub background_reuse: bool,
    /// True when a prepared summary existed but could not be reused, so the
    /// synchronous path ran instead.
    #[serde(default)]
    pub fallback_to_sync: bool,
    /// Cumulative successful compaction count for the session, including this
    /// event.
    #[serde(default)]
    pub compaction_count: u64,
    #[serde(default)]
    pub structured_state: Option<ContextStructuredState>,
    /// Prompt-cache generation before the active conversation was rewritten.
    /// The stable system/tool prefix may be unchanged; the cached conversation
    /// prefix is not.
    #[serde(default)]
    pub cache_generation_before: Option<String>,
    /// Prompt-cache generation after compaction completed.
    #[serde(default)]
    pub cache_generation_after: Option<String>,
    /// Why the conversation cache generation changed. Currently
    /// `compaction`; `None` for legacy events.
    #[serde(default)]
    pub cache_reset_reason: Option<String>,
    /// Estimated conversation tokens whose cached prefix was invalidated.
    #[serde(default)]
    pub cache_invalidation_tokens: u64,
    /// Estimated USD of lost cache-read savings. This is an estimate from the
    /// configured role prices, not a provider invoice.
    #[serde(default)]
    pub cache_invalidation_estimated_usd: f64,
    /// Whether the project cache policy asked for compaction cache metrics.
    #[serde(default)]
    pub cache_compact_aware: bool,
    /// Stable cache epoch before the boundary. The instruction prefix does not
    /// change, so `cache_epoch_after` normally equals this value.
    #[serde(default)]
    pub cache_epoch_before: Option<String>,
    #[serde(default)]
    pub cache_epoch_after: Option<String>,
    /// True only when a stable instruction/tool change also moved the epoch.
    /// A compaction boundary itself does not change the shareable epoch.
    #[serde(default)]
    pub cache_epoch_changed: bool,
    /// Always-computed estimate of cache-read savings lost at the boundary.
    /// Independent of the legacy `cache_compact_aware` accounting switch.
    #[serde(default)]
    pub estimated_lost_cache_read_usd: f64,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionContextStatus {
    pub session_id: String,
    pub usage: Option<ContextUsage>,
    pub last_compaction: Option<ContextCompactionSummary>,
    pub last_measurement: Option<ContextMeasurement>,
    /// Latest cumulative compaction count. This mirrors
    /// `last_compaction.compaction_count` and is surfaced separately so older
    /// clients can still read the summary object.
    pub compaction_count: u64,
    pub last_compaction_latency_ms: u64,
    pub last_compaction_tokens_before: u64,
    pub last_compaction_tokens_after: u64,
    pub compaction_background_reuse: bool,
    pub compaction_fallback_to_sync: bool,
    /// Number of live (non-deleted) durable context checkpoints.
    #[serde(default)]
    pub checkpoint_count: u64,
    #[serde(default)]
    pub latest_checkpoint_id: Option<String>,
    #[serde(default)]
    pub latest_checkpoint_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionContextParams {
    pub session_id: String,
}

/// Bounded cache telemetry for one session. Token values come from the usage
/// ledger; `prompt_prefix` is a hash, never prompt text.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionCacheStatus {
    pub session_id: String,
    /// Finalized L4 cache epoch for the latest prompt prefix. L1 recorded a
    /// deterministic placeholder; L4 replaces it with the stable
    /// `(model, tools, core/project)` identity.
    #[serde(default)]
    pub cache_epoch: Option<String>,
    #[serde(default)]
    pub cache_epoch_source: Option<String>,
    /// First/last provider observation for this session's current epoch.
    #[serde(default)]
    pub cache_epoch_first_seen: Option<String>,
    #[serde(default)]
    pub cache_epoch_last_seen: Option<String>,
    /// First/last observation across every session in the project. The
    /// session-scoped fields above intentionally differ for a new session.
    #[serde(default)]
    pub cache_epoch_project_first_seen: Option<String>,
    #[serde(default)]
    pub cache_epoch_project_last_seen: Option<String>,
    /// Ordered core/project section hashes for the current epoch.
    #[serde(default)]
    pub cache_epoch_core_sections: Vec<PromptSectionRef>,
    #[serde(default)]
    pub cache_epoch_project_sections: Vec<PromptSectionRef>,
    /// Stable identity component for the current epoch, when the latest
    /// prompt-prefix event was produced by an assigned Leader/Manager/Worker.
    /// Legacy, v2, and Labour/Verifier/Scout runs leave these empty.
    #[serde(default)]
    pub cache_epoch_identity_hash: Option<String>,
    #[serde(default)]
    pub cache_epoch_identity_id: Option<String>,
    #[serde(default)]
    pub cache_epoch_identity_generation: Option<u64>,
    #[serde(default)]
    pub cache_epoch_identity_sections: Vec<PromptSectionRef>,
    /// Project-wide distinct sessions that have used the current epoch.
    #[serde(default)]
    pub cache_epoch_session_count: u64,
    /// Project-wide calls and tokens for the current epoch, not just this
    /// session. Session-scoped counts are exposed by `session.cache.epoch`.
    #[serde(default)]
    pub cache_epoch_call_count: u64,
    #[serde(default)]
    pub cache_epoch_observed_input_tokens: u64,
    #[serde(default)]
    pub cache_epoch_observed_cache_read_tokens: u64,
    /// Distinct epochs observed in this session's prompt-prefix history.
    #[serde(default)]
    pub cache_epochs_seen: u64,
    /// Other sessions that had already used this epoch before the latest
    /// ordinary call. Derived from persisted observations, never inferred.
    #[serde(default)]
    pub prefix_reused_from_sessions: u64,
    /// Provider-reported cache reads on the first call of a session that
    /// reused an epoch already observed by another session.
    #[serde(default)]
    pub cross_session_hit_tokens: u64,
    #[serde(default)]
    pub cross_session_first_calls: u64,
    /// Calls attributed to a provider cache miss with an unchanged epoch.
    #[serde(default)]
    pub provider_cache_misses: u64,
    /// Calls attributed to a local model/tools/project/prefix change.
    #[serde(default)]
    pub prefix_changes: u64,
    /// Compaction boundaries that invalidated the conversation cache.
    #[serde(default)]
    pub compaction_cache_resets: u64,
    /// Estimated cache-read savings lost at those boundaries. This is an
    /// estimate from configured role prices, never a provider invoice.
    #[serde(default)]
    pub compaction_lost_cache_usd: f64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    /// Derived cache-read share; see [`cache_hit_ratio`].
    #[serde(default)]
    pub cache_hit_ratio: f64,
    /// Cache-read share of total provider input, including cache writes.
    #[serde(default)]
    pub cache_read_ratio: f64,
    /// Cache-write share of total provider input.
    #[serde(default)]
    pub cache_write_ratio: f64,
    /// Sum of per-call estimated savings from `context.cache` events. Negative
    /// values are possible when cache writes cost more than the reused reads.
    #[serde(default)]
    pub estimated_savings_usd: f64,
    /// Number of recorded per-call cache observations.
    #[serde(default)]
    pub cache_calls: u64,
    /// Latest prompt-prefix hash emitted by `context.prompt_prefix` (or the
    /// `prompt_prefix_hash` recorded on `context.measured`).
    #[serde(default)]
    pub prompt_prefix: Option<String>,
    /// Latest cache generation. This equals the stable prompt prefix hash; the
    /// conversation generation is recorded separately on compaction events.
    #[serde(default)]
    pub cache_generation: Option<String>,
    #[serde(default)]
    pub core_hash: Option<String>,
    #[serde(default)]
    pub project_hash: Option<String>,
    #[serde(default)]
    pub tools_hash: Option<String>,
    /// Latest frozen built-in tool bundle name (`leader`, `coding`, `research`,
    /// `verification`, `minimal`, or `full`).
    #[serde(default)]
    pub tool_bundle: Option<String>,
    /// Hash of the selected built-in bundle before dynamic MCP schemas.
    #[serde(default)]
    pub tool_bundle_hash: Option<String>,
    /// Built-in and dynamic MCP tool counts from the latest prompt prefix.
    #[serde(default)]
    pub tool_bundle_tool_count: Option<u64>,
    #[serde(default)]
    pub tool_bundle_mcp_count: Option<u64>,
    #[serde(default)]
    pub volatile_hash: Option<String>,
    /// Best-effort estimate of the system-prompt and tool-schema preamble.
    /// Heuristic, not provider-reported usage.
    #[serde(default)]
    pub preamble_tokens_estimate: Option<u64>,
    #[serde(default)]
    pub last_call_at: Option<String>,
    /// Latest breakpoint count the adapter was asked to emit.
    #[serde(default)]
    pub last_breakpoints: u32,
    /// Latest cache TTL pool (`5m` or `1h`).
    #[serde(default)]
    pub last_cache_ttl: Option<String>,
    /// Latest derived miss reason, when the provider reported no cache reads.
    #[serde(default)]
    pub last_miss_reason: Option<String>,
    /// Latest first-token latency for a cache-observed call.
    #[serde(default)]
    pub last_first_token_ms: Option<u64>,
    /// Append-only continuity for the latest ordinary provider call.
    #[serde(default)]
    pub last_message_prefix_hash: Option<String>,
    #[serde(default)]
    pub last_previous_message_prefix_hash: Option<String>,
    #[serde(default)]
    pub last_append_only: Option<bool>,
    #[serde(default)]
    pub append_only_calls: u64,
    #[serde(default)]
    pub non_append_only_calls: u64,
    /// Conversation message count of the latest ordinary call.
    #[serde(default)]
    pub last_message_count: u64,
    /// How the latest call placed tail memory (`none`, `trailing_system`,
    /// `trailing_user`, `embedded`).
    #[serde(default)]
    pub last_tail_memory_mode: Option<String>,
    /// Whether the provider accepts a mid-history system message.
    #[serde(default)]
    pub provider_tail_system: Option<bool>,
    #[serde(default)]
    pub last_tail_system_capability: Option<String>,
    /// Counting buckets for structural cache breaks and provider-side misses.
    #[serde(default)]
    pub cache_break_counts: Vec<CacheCountBucket>,
    /// Bounded newest-first structural break records.
    #[serde(default)]
    pub recent_breaks: Vec<SessionCacheBreak>,
    #[serde(default)]
    pub last_cache_break_at: Option<String>,
    /// Deterministic tool-call/tool-result ordering observations from the
    /// latest ordinary call and the bounded window.
    #[serde(default)]
    pub last_message_sequence_issue_count: u64,
    #[serde(default)]
    pub last_message_sequence_issues: Vec<String>,
    #[serde(default)]
    pub message_sequence_issue_counts: Vec<CacheCountBucket>,
    /// Bounded newest-first list of per-call cache records.
    #[serde(default)]
    pub recent_calls: Vec<SessionCacheCall>,
    /// Counting buckets derived from the bounded observation window.
    #[serde(default)]
    pub miss_reason_counts: Vec<CacheCountBucket>,
    #[serde(default)]
    pub ttl_counts: Vec<CacheCountBucket>,
    #[serde(default)]
    pub breakpoint_counts: Vec<CacheCountBucket>,
    /// Estimated savings grouped by the role that made the call.
    #[serde(default)]
    pub savings_by_role: Vec<CacheSavingsBucket>,
    /// Nearest-rank first-token latency percentiles over observed calls.
    #[serde(default)]
    pub first_token_p50_ms: Option<u64>,
    #[serde(default)]
    pub first_token_p95_ms: Option<u64>,
    /// Number of observed compaction cache resets in the bounded window.
    #[serde(default)]
    pub compaction_resets: u64,
    /// Sum of compaction invalidation token estimates in the same window.
    #[serde(default)]
    pub compaction_invalidation_tokens: u64,
    /// Sum of compaction invalidation USD estimates in the same window.
    #[serde(default)]
    pub compaction_invalidation_estimated_usd: f64,
    /// Project policy that produced the latest request plan.
    #[serde(default)]
    pub policy: Option<CachePolicy>,
    // ------------------------------------------------------------------
    // L5 telemetry. Every field is additive and defaults to zero/empty so a
    // pre-L5 daemon or a legacy event payload still decodes.
    // ------------------------------------------------------------------
    /// `input_tokens - cache_read_tokens`, clamped at zero. This is the
    /// provider-reported prompt portion that was not served from cache.
    #[serde(default)]
    pub cache_miss_tokens: u64,
    /// Best achievable in-session hit ratio from the previous prompt size:
    /// `min(previous_input_tokens, input_tokens) / input_tokens`, clamped.
    /// It measures append-only conversation headroom, not a provider promise.
    #[serde(default)]
    pub theoretical_max_hit_ratio: f64,
    /// Calls where the canonical message list was not an append-only
    /// extension. The first observed call in a session is not counted: there
    /// is no previous list to compare against.
    #[serde(default)]
    pub append_only_breaks: u64,
    /// Nearest-rank percentiles over ordinary per-call hit ratios.
    #[serde(default)]
    pub hit_ratio_p50: Option<f64>,
    #[serde(default)]
    pub hit_ratio_p95: Option<f64>,
    /// Nearest-rank percentiles over per-call theoretical maxima.
    #[serde(default)]
    pub theoretical_max_p50: Option<f64>,
    #[serde(default)]
    pub theoretical_max_p95: Option<f64>,
    /// Total ledger cost for the session. Cache reads are still billed; this
    /// is the same value `session.usage` reports, repeated here for the cache
    /// card so it does not need a second RPC.
    #[serde(default)]
    pub cost_usd: f64,
    /// Bounded cache-epoch timeline (newest last, at most 20 entries).
    #[serde(default)]
    pub cache_epoch_timeline: Vec<CacheEpochBucket>,
    /// Per-role cache accounting over the bounded observation window.
    #[serde(default)]
    pub per_role: Vec<CacheRoleBucket>,
    /// Most frequent miss reasons, sorted by call count then key (max 5).
    #[serde(default)]
    pub top_miss_reasons: Vec<CacheCountBucket>,
    /// Distinct other sessions that reported the same cache epoch with at
    /// least one cache read. Bounded scan; never a guaranteed provider hit.
    #[serde(default)]
    pub cross_session_reuse_sessions: u64,
    /// Number of bounded cross-session cache events inspected.
    #[serde(default)]
    pub cross_session_calls_observed: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheCountBucket {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub calls: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CacheSavingsBucket {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub calls: u64,
    #[serde(default)]
    pub estimated_savings_usd: f64,
}

/// One bounded `context.cache_break` observation. It records why the canonical
/// message list stopped being append-only, but never message text: only the
/// first differing index, its role, and a tool-call id when that message is a
/// tool result or tool-call turn.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCacheBreak {
    #[serde(default)]
    pub at: String,
    /// `first_call`, `system_changed`, `tool_bundle_changed`, `message_edited`,
    /// `message_reordered`, `compaction_reset`, `provider_cache_miss`, or
    /// `unknown`.
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub first_differing_index: Option<u64>,
    #[serde(default)]
    pub first_differing_role: Option<String>,
    #[serde(default)]
    pub first_differing_tool_call_id: Option<String>,
    #[serde(default)]
    pub previous_message_count: u64,
    #[serde(default)]
    pub message_count: u64,
    #[serde(default)]
    pub previous_message_prefix_hash: Option<String>,
    #[serde(default)]
    pub message_prefix_hash: Option<String>,
    /// True when this event came from an explicit cache-warm request.
    #[serde(default)]
    pub warm: bool,
}

/// One session's contribution to a stable cache epoch. This is the
/// event-derived projection row used for cross-session reuse; it contains no
/// prompt text and no provider secret.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptPrefixObservation {
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub cache_epoch: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub tool_bundle: String,
    #[serde(default)]
    pub tool_bundle_hash: String,
    #[serde(default)]
    pub core_hash: String,
    #[serde(default)]
    pub project_hash: String,
    #[serde(default)]
    pub core_sections: Vec<PromptSectionRef>,
    #[serde(default)]
    pub project_sections: Vec<PromptSectionRef>,
    #[serde(default)]
    pub first_seen: String,
    #[serde(default)]
    pub last_seen: String,
    #[serde(default)]
    pub observed_input_tokens: u64,
    #[serde(default)]
    pub observed_cache_read_tokens: u64,
    #[serde(default)]
    pub calls: u64,
}

/// Aggregate of [`PromptPrefixObservation`] rows for one epoch in one
/// project. The aggregate is a projection rebuilt from observations; it is
/// never computed from a session's current configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptPrefixAggregate {
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub cache_epoch: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub tool_bundle: String,
    #[serde(default)]
    pub tool_bundle_hash: String,
    #[serde(default)]
    pub core_hash: String,
    #[serde(default)]
    pub project_hash: String,
    #[serde(default)]
    pub core_sections: Vec<PromptSectionRef>,
    #[serde(default)]
    pub project_sections: Vec<PromptSectionRef>,
    #[serde(default)]
    pub first_seen: String,
    #[serde(default)]
    pub last_seen: String,
    #[serde(default)]
    pub observed_input_tokens: u64,
    #[serde(default)]
    pub observed_cache_read_tokens: u64,
    #[serde(default)]
    pub session_count: u64,
    #[serde(default)]
    pub call_count: u64,
}

/// Result of one epoch observation. `prior_sessions` excludes the observing
/// session, so a brand-new session sees how many other sessions already used
/// the same bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptPrefixRecord {
    #[serde(default)]
    pub first_session_call: bool,
    #[serde(default)]
    pub prior_sessions: u64,
    #[serde(default)]
    pub aggregate: PromptPrefixAggregate,
}

/// Read-only cache-epoch inspection. It never calls a provider, starts a run,
/// or changes the epoch.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionCacheEpochParams {
    pub session_id: String,
    /// Maximum timeline entries and aggregate epochs to return. Clamped by the
    /// runtime to a bounded window.
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheEpochTimelineEntry {
    #[serde(default)]
    pub at: String,
    /// `prefix` or `compaction_reset`.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub cache_epoch: Option<String>,
    /// `first`, `model`, `tools`, `project`, `core`, `sections`, or `unknown`.
    #[serde(default)]
    pub change_reason: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub tool_bundle: Option<String>,
    #[serde(default)]
    pub prefix_hash: Option<String>,
    #[serde(default)]
    pub compaction_count: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CacheEpochSummary {
    #[serde(default)]
    pub cache_epoch: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub tool_bundle: String,
    #[serde(default)]
    pub tool_bundle_hash: String,
    #[serde(default)]
    pub core_hash: String,
    #[serde(default)]
    pub project_hash: String,
    #[serde(default)]
    pub core_sections: Vec<PromptSectionRef>,
    #[serde(default)]
    pub project_sections: Vec<PromptSectionRef>,
    #[serde(default)]
    pub first_seen: String,
    #[serde(default)]
    pub last_seen: String,
    /// Distinct sessions in the project that observed this epoch.
    #[serde(default)]
    pub session_count: u64,
    #[serde(default)]
    pub call_count: u64,
    #[serde(default)]
    pub observed_input_tokens: u64,
    #[serde(default)]
    pub observed_cache_read_tokens: u64,
    #[serde(default)]
    pub cache_hit_ratio: f64,
    /// This session's contribution, zero when the session never observed it.
    #[serde(default)]
    pub this_session_calls: u64,
    #[serde(default)]
    pub this_session_input_tokens: u64,
    #[serde(default)]
    pub this_session_cache_read_tokens: u64,
    /// Other sessions that used this epoch. Provider-reported hits are the
    /// only evidence of actual provider reuse.
    #[serde(default)]
    pub reused_from_sessions: u64,
}

/// One other session that observed the current epoch. The title is included
/// only because operators need to recognize the session; no prompt text,
/// message, or artifact is returned.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CacheEpochReuseSession {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub first_seen: String,
    #[serde(default)]
    pub last_seen: String,
    #[serde(default)]
    pub calls: u64,
    #[serde(default)]
    pub observed_input_tokens: u64,
    #[serde(default)]
    pub observed_cache_read_tokens: u64,
    #[serde(default)]
    pub cache_hit_ratio: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionCacheEpochResult {
    pub session_id: String,
    pub project_id: String,
    #[serde(default = "default_prompt_layout")]
    pub prompt_layout: u32,
    /// Latest epoch observed for this session, with project/session counts.
    #[serde(default)]
    pub current_epoch: Option<CacheEpochSummary>,
    /// Bounded project epochs, newest last-seen first.
    #[serde(default)]
    pub epochs: Vec<CacheEpochSummary>,
    /// Bounded chronological prefix/compaction timeline.
    #[serde(default)]
    pub timeline: Vec<CacheEpochTimelineEntry>,
    /// Bounded other sessions that observed the current epoch.
    #[serde(default)]
    pub reused_sessions: Vec<CacheEpochReuseSession>,
    #[serde(default)]
    pub total_epochs: u64,
    #[serde(default)]
    pub timeline_truncated: bool,
    /// Other sessions that already used the current epoch.
    #[serde(default)]
    pub prefix_reused_from_sessions: u64,
    /// Provider-reported first-call cache reads attributed to another session.
    #[serde(default)]
    pub cross_session_hit_tokens: u64,
    #[serde(default)]
    pub cross_session_first_calls: u64,
    #[serde(default)]
    pub provider_cache_misses: u64,
    #[serde(default)]
    pub compaction_resets: u64,
    #[serde(default)]
    pub compaction_lost_cache_usd: f64,
    #[serde(default)]
    pub generated_at: String,
}

/// One tool-bundle bucket in a project epoch report. Epochs are already
/// model/tool/core/project-specific, so this groups them for operators who
/// want to know which frozen bundle is paying for cache reuse.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CacheEpochBundleBucket {
    #[serde(default)]
    pub tool_bundle: String,
    #[serde(default)]
    pub epochs: u64,
    #[serde(default)]
    pub sessions: u64,
    #[serde(default)]
    pub calls: u64,
    #[serde(default)]
    pub observed_input_tokens: u64,
    #[serde(default)]
    pub observed_cache_read_tokens: u64,
    #[serde(default)]
    pub cache_hit_ratio: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProjectCacheEpochParams {
    pub project_id: String,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Project-scoped retention request for cache-epoch observations. The newest
/// `keep_epochs` epochs are always retained regardless of age.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProjectCacheEpochPruneParams {
    pub project_id: String,
    /// Remove observations whose last provider call is older than this many
    /// days. Clamped to 1..=3650. Defaults to 90.
    #[serde(default)]
    pub older_than_days: Option<u32>,
    /// Always retain this many newest epochs. Clamped to 1..=1000. Defaults
    /// to 50.
    #[serde(default)]
    pub keep_epochs: Option<u32>,
    /// Count only; no rows are deleted.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectCacheEpochPruneResult {
    pub project_id: String,
    #[serde(default)]
    pub dry_run: bool,
    /// RFC3339 cutoff used for age comparison.
    #[serde(default)]
    pub cutoff: String,
    #[serde(default)]
    pub removed_epochs: u64,
    #[serde(default)]
    pub removed_observations: u64,
    #[serde(default)]
    pub kept_epochs: u64,
    /// For a dry run this is the hypothetical count after pruning; the
    /// database still contains the pre-prune rows.
    #[serde(default)]
    pub remaining_epochs: u64,
    /// Epochs retained because an active session still references them.
    #[serde(default)]
    pub protected_epochs: u64,
}

/// Read-only project-level epoch report. It never calls a provider and never
/// mutates a projection.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectCacheEpochResult {
    pub project_id: String,
    #[serde(default)]
    pub epochs: Vec<CacheEpochSummary>,
    #[serde(default)]
    pub bundles: Vec<CacheEpochBundleBucket>,
    #[serde(default)]
    pub total_epochs: u64,
    #[serde(default)]
    pub total_sessions: u64,
    #[serde(default)]
    pub total_calls: u64,
    #[serde(default)]
    pub observed_input_tokens: u64,
    #[serde(default)]
    pub observed_cache_read_tokens: u64,
    #[serde(default)]
    pub cache_hit_ratio: f64,
    #[serde(default)]
    pub generated_at: String,
}

/// Per-role cache accounting over a bounded event window. Token counts are
/// provider-reported totals; `cache_miss_tokens` is the uncached remainder.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CacheRoleBucket {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub calls: u64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_miss_tokens: u64,
    #[serde(default)]
    pub hit_ratio: f64,
    #[serde(default)]
    pub theoretical_max_hit_ratio: f64,
    #[serde(default)]
    pub cost_usd: f64,
}

/// One cache epoch observed in the session. The timeline is derived from
/// telemetry, not from provider behavior; repeated epochs are possible when a
/// provider evicts a prefix without our bytes changing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CacheEpochBucket {
    #[serde(default)]
    pub epoch: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub calls: u64,
    #[serde(default)]
    pub first_at: String,
    #[serde(default)]
    pub last_at: String,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_miss_tokens: u64,
    #[serde(default)]
    pub hit_ratio: f64,
    #[serde(default)]
    pub append_only_breaks: u64,
}

impl Default for SessionCacheCall {
    fn default() -> Self {
        Self {
            at: String::new(),
            agent_name: None,
            role: None,
            model: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cache_hit_ratio: 0.0,
            cache_breakpoints: 0,
            cache_ttl: None,
            first_token_ms: None,
            core_hash: None,
            project_hash: None,
            tools_hash: None,
            tool_bundle: None,
            tool_bundle_hash: None,
            volatile_hash: None,
            prompt_prefix_hash: None,
            canonical_version: 0,
            previous_message_prefix_hash: None,
            message_prefix_hash: None,
            append_only: None,
            message_count: 0,
            head_prefix_hash: None,
            tail_prefix_hash: None,
            request_prefix_hash: None,
            tail_memory_mode: None,
            provider_tail_system: None,
            tail_system_capability: None,
            cache_break_reason: None,
            message_sequence_issue_count: 0,
            message_sequence_issues: Vec::new(),
            cache_epoch: None,
            cache_epoch_reused_from_sessions: 0,
            cross_session_cache_read_tokens: 0,
            cross_session_first_call: false,
            estimated_savings_usd: 0.0,
            miss_reason: None,
            warm: false,
            cache_miss_tokens: 0,
            theoretical_max_hit_ratio: 0.0,
            message_prefix_count: 0,
            previous_message_prefix_count: 0,
            cache_epoch_source: None,
            previous_input_tokens: 0,
        }
    }
}

/// One bounded per-call cache observation. Token fields come from the provider
/// usage report (or the runtime estimate when the provider omitted usage).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionCacheCall {
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub cache_hit_ratio: f64,
    #[serde(default)]
    pub cache_breakpoints: u32,
    #[serde(default)]
    pub cache_ttl: Option<String>,
    #[serde(default)]
    pub first_token_ms: Option<u64>,
    #[serde(default)]
    pub core_hash: Option<String>,
    #[serde(default)]
    pub project_hash: Option<String>,
    #[serde(default)]
    pub tools_hash: Option<String>,
    /// Frozen bundle name and built-in bundle hash for this call.
    #[serde(default)]
    pub tool_bundle: Option<String>,
    #[serde(default)]
    pub tool_bundle_hash: Option<String>,
    #[serde(default)]
    pub volatile_hash: Option<String>,
    #[serde(default)]
    pub prompt_prefix_hash: Option<String>,
    /// Canonical conversation continuity for this call. These fields are
    /// additive; older events decode with empty/default values.
    #[serde(default)]
    pub canonical_version: u32,
    #[serde(default)]
    pub previous_message_prefix_hash: Option<String>,
    #[serde(default)]
    pub message_prefix_hash: Option<String>,
    #[serde(default)]
    pub append_only: Option<bool>,
    #[serde(default)]
    pub message_count: u64,
    #[serde(default)]
    pub head_prefix_hash: Option<String>,
    #[serde(default)]
    pub tail_prefix_hash: Option<String>,
    #[serde(default)]
    pub request_prefix_hash: Option<String>,
    #[serde(default)]
    pub tail_memory_mode: Option<String>,
    #[serde(default)]
    pub provider_tail_system: Option<bool>,
    #[serde(default)]
    pub tail_system_capability: Option<String>,
    /// Structural break reason recorded by the runtime before this call, if
    /// any. `provider_cache_miss` is post-call and means the provider did not
    /// reuse an otherwise append-only prefix.
    #[serde(default)]
    pub cache_break_reason: Option<String>,
    /// Deterministic tool-call/tool-result ordering observations. Diagnostic
    /// only; the runtime never reorders messages.
    #[serde(default)]
    pub message_sequence_issue_count: u64,
    #[serde(default)]
    pub message_sequence_issues: Vec<String>,
    /// Stable cache epoch for this call. Volatile/session/run/tail content is
    /// deliberately excluded from the identity.
    #[serde(default)]
    pub cache_epoch: Option<String>,
    /// Other sessions that had already observed this epoch before the call.
    #[serde(default)]
    pub cache_epoch_reused_from_sessions: u64,
    /// Provider-reported cache reads attributed to cross-session reuse. Only
    /// set on the first call of a session for an epoch another session used.
    #[serde(default)]
    pub cross_session_cache_read_tokens: u64,
    #[serde(default)]
    pub cross_session_first_call: bool,
    #[serde(default)]
    pub estimated_savings_usd: f64,
    #[serde(default)]
    pub miss_reason: Option<String>,
    /// True when this call was an explicit `session.cache.warm` request.
    #[serde(default)]
    pub warm: bool,
    // ------------------------------------------------------------------
    // L5 additive per-call metrics.
    // ------------------------------------------------------------------
    /// `input_tokens - cache_read_tokens`, clamped at zero.
    #[serde(default)]
    pub cache_miss_tokens: u64,
    /// `min(previous_input_tokens, input_tokens) / input_tokens`; zero on the
    /// first call or when no prior prompt size is known.
    #[serde(default)]
    pub theoretical_max_hit_ratio: f64,
    /// Number of canonical messages covered by `message_prefix_hash`.
    #[serde(default)]
    pub message_prefix_count: u64,
    /// Number of canonical messages covered by
    /// `previous_message_prefix_hash`.
    #[serde(default)]
    pub previous_message_prefix_count: u64,
    /// Provenance of `cache_epoch` (`prompt_prefix_v2`, `prompt_prefix_v3`,
    /// or a future finalized source). Never a provider cache key.
    #[serde(default)]
    pub cache_epoch_source: Option<String>,
    /// Previous ordinary call's provider-reported input token total.
    #[serde(default)]
    pub previous_input_tokens: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionCacheWarmParams {
    pub session_id: String,
    /// Optional role whose stable prompt/tool bundle to warm. Defaults to the
    /// Leader. No volatile section or memory is included.
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionCacheAuditParams {
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionCacheAuditFinding {
    pub code: String,
    pub severity: String,
    pub message: String,
    #[serde(default)]
    pub evidence: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionCacheAuditResult {
    pub session_id: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub calls_checked: u64,
    #[serde(default)]
    pub warm_calls_checked: u64,
    #[serde(default)]
    pub compactions_checked: u64,
    #[serde(default)]
    pub hit_ratio: f64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub findings: Vec<SessionCacheAuditFinding>,
    /// True when every finding is informational.
    #[serde(default)]
    pub clean: bool,
    /// L5 aggregate fields. They are additive so the legacy audit payload
    /// decodes with defaults.
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cache_miss_tokens: u64,
    #[serde(default)]
    pub theoretical_max_hit_ratio: f64,
    #[serde(default)]
    pub hit_ratio_p50: Option<f64>,
    #[serde(default)]
    pub hit_ratio_p95: Option<f64>,
    #[serde(default)]
    pub append_only_calls: u64,
    #[serde(default)]
    pub append_only_breaks: u64,
    #[serde(default)]
    pub cache_epochs: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionCachePlanParams {
    pub session_id: String,
    /// Roles to preview. Empty means `leader` only.
    #[serde(default)]
    pub roles: Vec<String>,
}

/// One role's cache plan and component hashes. This is a read-only preview: it
/// never calls a provider and never creates a worktree or session run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionCachePlanRole {
    pub role: String,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub cache_breakpoints: u32,
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl: String,
    #[serde(default)]
    pub ttl_explicit: bool,
    #[serde(default)]
    pub automatic: bool,
    #[serde(default)]
    pub tail_message: bool,
    #[serde(default)]
    pub tool_breakpoint: bool,
    #[serde(default)]
    pub core_breakpoint: bool,
    #[serde(default)]
    pub project_breakpoint: bool,
    #[serde(default)]
    pub core_hash: String,
    #[serde(default)]
    pub project_hash: String,
    #[serde(default)]
    pub tools_hash: String,
    #[serde(default)]
    pub volatile_hash: String,
    #[serde(default)]
    pub prefix_hash: String,
    /// Stable `agent/identity` section metadata for this role. Empty/absent
    /// for legacy sessions and Labour/Verifier/Scout plans.
    #[serde(default)]
    pub identity_hash: String,
    #[serde(default)]
    pub identity_id: Option<String>,
    #[serde(default)]
    pub identity_generation: Option<u64>,
    #[serde(default)]
    pub prompt_layout: u32,
    #[serde(default)]
    pub stable_prefix_hash: String,
    #[serde(default)]
    pub head_sections: Vec<PromptSectionRef>,
    #[serde(default)]
    pub tail_sections: Vec<PromptSectionRef>,
    #[serde(default)]
    pub section_chars: BTreeMap<String, u64>,
    #[serde(default)]
    pub core_chars: u64,
    #[serde(default)]
    pub project_chars: u64,
    #[serde(default)]
    pub stable_prefix_chars: u64,
    #[serde(default)]
    pub tool_count: usize,
    /// Frozen bundle selected for the role and optional task kind.
    #[serde(default)]
    pub tool_bundle: String,
    #[serde(default)]
    pub tool_bundle_hash: String,
    #[serde(default)]
    pub tool_bundle_tool_count: usize,
    #[serde(default)]
    pub tool_bundle_mcp_count: usize,
    #[serde(default)]
    pub tool_bundle_target_limit: Option<usize>,
    #[serde(default)]
    pub tool_bundle_within_target: Option<bool>,
    #[serde(default)]
    pub tool_bundle_fallback: bool,
    #[serde(default)]
    pub system_tokens_estimate: u64,
    #[serde(default)]
    pub tool_tokens_estimate: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionCachePlanResult {
    pub session_id: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub policy: Option<CachePolicy>,
    #[serde(default)]
    pub roles: Vec<SessionCachePlanRole>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionCacheWarmResult {
    pub session_id: String,
    pub warmed: bool,
    pub provider: String,
    pub model: String,
    /// Role whose stable prompt/tool bundle was warmed.
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub source: String,
    pub cache_breakpoints: u32,
    pub cache_ttl: String,
    #[serde(default)]
    pub automatic: bool,
    #[serde(default)]
    pub core_hash: Option<String>,
    #[serde(default)]
    pub project_hash: Option<String>,
    #[serde(default)]
    pub tools_hash: Option<String>,
    /// Stable identity component included in the warmed head sections, when
    /// the role has an active project identity assignment.
    #[serde(default)]
    pub identity_hash: Option<String>,
    #[serde(default)]
    pub identity_id: Option<String>,
    #[serde(default)]
    pub identity_generation: Option<u64>,
    /// Stable epoch covered by the warm request. A warm never changes it.
    #[serde(default)]
    pub cache_epoch: Option<String>,
    #[serde(default)]
    pub cache_epoch_unchanged: bool,
    /// Other sessions that had already observed the same epoch. Informational;
    /// the warm call's own usage is billed and recorded separately.
    #[serde(default)]
    pub cache_epoch_reused_from_sessions: u64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub estimated: bool,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub latency_ms: u64,
    #[serde(default)]
    pub first_token_ms: u64,
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionCacheParams {
    pub session_id: String,
}

/// Read-only structural cache-break history. It never calls a provider and
/// never returns message text.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionCacheBreaksParams {
    pub session_id: String,
    /// Newest-first cap. Defaults to 50, maximum 200.
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectCacheParams {
    pub project_id: String,
    /// Bounded recent cache-event scan. Defaults to 5,000; clamped 1..=50,000.
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionCacheBreaksResult {
    pub session_id: String,
    #[serde(default)]
    pub append_only_calls: u64,
    #[serde(default)]
    pub non_append_only_calls: u64,
    /// Total break events in the scanned window before the cap.
    #[serde(default)]
    pub total_breaks: u64,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub breaks: Vec<SessionCacheBreak>,
}

/// Read-only project-wide cache overview over a bounded scan of recent
/// `context.cache` events. It is aggregate-only and never returns prompt text.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectCacheOverview {
    pub project_id: String,
    #[serde(default)]
    pub sessions_observed: u64,
    #[serde(default)]
    pub calls: u64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_miss_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub hit_ratio: f64,
    #[serde(default)]
    pub hit_ratio_p50: Option<f64>,
    #[serde(default)]
    pub hit_ratio_p95: Option<f64>,
    #[serde(default)]
    pub append_only_calls: u64,
    #[serde(default)]
    pub append_only_breaks: u64,
    #[serde(default)]
    pub cache_epochs: u64,
    #[serde(default)]
    pub top_miss_reasons: Vec<CacheCountBucket>,
    #[serde(default)]
    pub cache_epoch_timeline: Vec<CacheEpochBucket>,
    #[serde(default)]
    pub scan_limit: u32,
    #[serde(default)]
    pub truncated: bool,
}

/// Bounded per-call cache table request. Filters are applied before the limit;
/// the result reports whether older matching calls were truncated.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionCacheCallsParams {
    pub session_id: String,
    /// Newest-first limit. Defaults to 20, clamped to 1..=100.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Exact role filter (`leader`, `worker`, `cache_warm`, ...).
    #[serde(default)]
    pub role: Option<String>,
    /// Exact miss-reason filter (`tail_delta`, `provider_cache_miss`, ...).
    #[serde(default)]
    pub miss_reason: Option<String>,
    /// Include explicit warm calls. Default false: the table describes agent
    /// calls, while warm cost remains visible in `session.cache`.
    #[serde(default)]
    pub include_warm: bool,
}

/// Bounded newest-first per-call cache table. It contains no prompt text.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionCacheCallsResult {
    pub session_id: String,
    #[serde(default)]
    pub calls: Vec<SessionCacheCall>,
    /// Matching calls after filters, before the limit.
    #[serde(default)]
    pub observed: u64,
    /// Number of rows returned.
    #[serde(default)]
    pub returned: u64,
    /// True when `observed > returned`.
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub limit: u32,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub miss_reason: Option<String>,
    #[serde(default)]
    pub include_warm: bool,
}

/// Read-only tool-bundle inspection. It never calls a provider, starts a run,
/// or creates a worktree.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ToolBundleParams {
    pub session_id: String,
    /// Roles to inspect. Empty means every role currently present in the
    /// session, plus the Leader.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Optional plan task kind used to partition a Worker/Labour bundle. When
    /// omitted, the agent's stored plan task kind is used if one exists.
    #[serde(default)]
    pub task_kind: Option<String>,
    /// Include per-tool metadata rows. Defaults to true; schemas are never
    /// returned.
    #[serde(default)]
    pub include_tools: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolBundleWarningInfo {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub message: String,
}

/// One bounded per-tool row. It contains hashes and counts, never schema text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolBundleToolInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub workspace_write: bool,
    #[serde(default)]
    pub delegation: bool,
    #[serde(default)]
    pub schema_hash: String,
    #[serde(default)]
    pub description_chars: u64,
    #[serde(default)]
    pub parameter_fields: u64,
}

/// Latest observed `context.prompt_prefix` for one role, if any. It is derived
/// from persisted events, not recomputed from current configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolBundleLastRun {
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub bundle: String,
    #[serde(default)]
    pub bundle_hash: String,
    #[serde(default)]
    pub tools_hash: String,
    #[serde(default)]
    pub tool_count: u64,
    #[serde(default)]
    pub mcp_tool_count: u64,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolBundleCategoryCount {
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tools: usize,
    #[serde(default)]
    pub schema_bytes: u64,
}

/// One role's frozen bundle. `requested` is the explicit template; `tools` is
/// what the runtime actually produced after role policy and delegation rules.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolBundleRoleResult {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub profile: String,
    #[serde(default)]
    pub task_kind: Option<String>,
    #[serde(default)]
    pub bundle: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub fallback: bool,
    #[serde(default)]
    pub bundle_hash: String,
    #[serde(default)]
    pub tools_hash: String,
    #[serde(default)]
    pub requested: Vec<String>,
    #[serde(default)]
    pub omitted: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<ToolBundleWarningInfo>,
    #[serde(default)]
    pub builtin_count: usize,
    #[serde(default)]
    pub mcp_count: usize,
    #[serde(default)]
    pub total_count: usize,
    /// Exact canonical schema bytes and a diagnostic token estimate.
    #[serde(default)]
    pub schema_bytes: u64,
    #[serde(default)]
    pub estimated_tool_tokens: u64,
    #[serde(default)]
    pub target_limit: Option<usize>,
    #[serde(default)]
    pub within_target: Option<bool>,
    #[serde(default)]
    pub categories: Vec<ToolBundleCategoryCount>,
    #[serde(default)]
    pub tools: Vec<ToolBundleToolInfo>,
    #[serde(default)]
    pub last_run: Option<ToolBundleLastRun>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolBundleCatalogRow {
    #[serde(default)]
    pub bundle: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub target_limit: Option<usize>,
    #[serde(default)]
    pub requested_count: usize,
    #[serde(default)]
    pub representative_role: String,
    #[serde(default)]
    pub representative_profile: String,
    #[serde(default)]
    pub allows_dynamic_mcp: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolBundleResult {
    pub session_id: String,
    #[serde(default)]
    pub roles: Vec<ToolBundleRoleResult>,
    #[serde(default)]
    pub catalog: Vec<ToolBundleCatalogRow>,
}

/// Static bundle matrix for `tool_bundle.catalog` / `vp tool-bundles`. No
/// session is needed and no provider call is made.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolBundleCatalogResult {
    #[serde(default)]
    pub bundles: Vec<ToolBundleCatalogRow>,
    /// Programming-error findings from the static matrix audit. Empty in a
    /// healthy build.
    #[serde(default)]
    pub warnings: Vec<ToolBundleWarningInfo>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ToolBundleMatrixParams {
    /// Task kinds to evaluate. Empty uses the built-in deterministic set:
    /// no kind, implementation, verification, research, trivial, unknown.
    #[serde(default)]
    pub task_kinds: Vec<String>,
}

/// One actual role/profile/task-kind combination, resolved against the live
/// tool registry. This is the test-gated partition as the runtime would build
/// it, not the static request list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolBundleMatrixRow {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub profile: String,
    #[serde(default)]
    pub task_kind: Option<String>,
    #[serde(default)]
    pub bundle: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub fallback: bool,
    #[serde(default)]
    pub bundle_hash: String,
    #[serde(default)]
    pub tools_hash: String,
    #[serde(default)]
    pub builtin_count: usize,
    #[serde(default)]
    pub mcp_count: usize,
    #[serde(default)]
    pub total_count: usize,
    #[serde(default)]
    pub schema_bytes: u64,
    #[serde(default)]
    pub estimated_tool_tokens: u64,
    #[serde(default)]
    pub requested_count: usize,
    #[serde(default)]
    pub omitted_count: usize,
    #[serde(default)]
    pub target_limit: Option<usize>,
    #[serde(default)]
    pub within_target: Option<bool>,
    #[serde(default)]
    pub warning_count: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolBundleMatrixResult {
    #[serde(default)]
    pub task_kinds: Vec<String>,
    #[serde(default)]
    pub rows: Vec<ToolBundleMatrixRow>,
    #[serde(default)]
    pub warnings: Vec<ToolBundleWarningInfo>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ToolBundleHistoryParams {
    pub session_id: String,
    #[serde(default)]
    pub role: Option<String>,
    /// Newest-first cap. Defaults to 50 and is clamped to 200.
    #[serde(default)]
    pub limit: Option<u64>,
}

/// One persisted `context.prompt_prefix` observation. History is derived from
/// the event stream and never recomputed from current configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolBundleHistoryEntry {
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub bundle: String,
    #[serde(default)]
    pub bundle_hash: String,
    #[serde(default)]
    pub tools_hash: String,
    #[serde(default)]
    pub tool_count: u64,
    #[serde(default)]
    pub mcp_tool_count: u64,
    #[serde(default)]
    pub task_kind: Option<String>,
    #[serde(default)]
    pub fallback: bool,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolBundleHistoryResult {
    pub session_id: String,
    #[serde(default)]
    pub entries: Vec<ToolBundleHistoryEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ToolBundleDiffParams {
    pub from_bundle: String,
    pub to_bundle: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub task_kind: Option<String>,
}

/// Deterministic diff between two bundle previews for one role/profile. Tool
/// lists are sorted and bounded; no schema text is returned.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolBundleDiffResult {
    pub from_bundle: String,
    pub to_bundle: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub profile: String,
    #[serde(default)]
    pub task_kind: Option<String>,
    #[serde(default)]
    pub from_hash: String,
    #[serde(default)]
    pub to_hash: String,
    #[serde(default)]
    pub from_count: usize,
    #[serde(default)]
    pub to_count: usize,
    #[serde(default)]
    pub added: Vec<String>,
    #[serde(default)]
    pub removed: Vec<String>,
    #[serde(default)]
    pub shared: Vec<String>,
    #[serde(default)]
    pub changed: bool,
    #[serde(default)]
    pub warnings: Vec<ToolBundleWarningInfo>,
}

/// Bounded per-tool activity rows derived from the append-only
/// `tool.finished` event stream. No provider content, command text, or
/// structured payload is copied here; only operational metrics and the stable
/// failure contract.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolActivityEntry {
    #[serde(default)]
    pub tool_call_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default)]
    pub hint: Option<String>,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub output_bytes: u64,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub approval_risk: Option<String>,
    #[serde(default)]
    pub approval_mode: Option<String>,
    #[serde(default)]
    pub approval_wait_ms: Option<u64>,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolActivityToolStat {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub calls: u64,
    #[serde(default)]
    pub errors: u64,
    #[serde(default)]
    pub total_duration_ms: u64,
    #[serde(default)]
    pub output_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolActivityCodeStat {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub calls: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolActivitySummary {
    #[serde(default)]
    pub calls: u64,
    #[serde(default)]
    pub errors: u64,
    #[serde(default)]
    pub retryable_errors: u64,
    #[serde(default)]
    pub truncated_results: u64,
    #[serde(default)]
    pub total_duration_ms: u64,
    #[serde(default)]
    pub max_duration_ms: u64,
    #[serde(default)]
    pub average_duration_ms: u64,
    #[serde(default)]
    pub output_bytes: u64,
    #[serde(default)]
    pub approval_waits: u64,
    #[serde(default)]
    pub total_approval_wait_ms: u64,
    #[serde(default)]
    pub max_approval_wait_ms: u64,
    #[serde(default)]
    pub by_tool: Vec<ToolActivityToolStat>,
    #[serde(default)]
    pub error_codes: Vec<ToolActivityCodeStat>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolActivityResult {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub entries: Vec<ToolActivityEntry>,
    /// Total `tool.finished` events in this session, independent of the
    /// returned window.
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub limit: u32,
    /// True when older events exist outside the returned newest-first window.
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub summary: ToolActivitySummary,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolActivityParams {
    pub session_id: String,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionCompactResult {
    pub session_id: String,
    pub compacted: bool,
    pub compaction: Option<ContextCompactionSummary>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionCompactParams {
    pub session_id: String,
}

/// A durable, inspectable snapshot of the active-context boundary. It records
/// exactly what the latest `context.compacted` event used: mode, summary,
/// through-message cursor, plan revision, provider/model/prefix identity,
/// worktree head, and the authoritative structured task state. Restoring a
/// checkpoint rewrites the active boundary; it never rewrites stored messages
/// or worktree content.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCheckpoint {
    pub id: String,
    pub session_id: String,
    pub project_id: String,
    #[serde(default)]
    pub label: String,
    /// `summary`, `trim`, or `manual`.
    #[serde(default)]
    pub mode: String,
    /// Prefixed summary injected at the head of the active conversation.
    /// Bounded; no additional prompt text is captured beyond the summary that
    /// already exists in `context.compacted`.
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub through_message_id: Option<String>,
    /// Number of stored messages at or before the boundary. Kept as durable
    /// evidence even when the referenced message id is later unavailable.
    #[serde(default)]
    pub message_cursor: usize,
    #[serde(default)]
    pub plan_revision: u32,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    /// Hash only; never prompt text.
    #[serde(default)]
    pub prompt_prefix_hash: String,
    #[serde(default)]
    pub worktree_head: String,
    #[serde(default)]
    pub tokens_before: u64,
    #[serde(default)]
    pub tokens_after: u64,
    #[serde(default)]
    pub structured_state: Option<ContextStructuredState>,
    /// Sequence of the compaction event this checkpoint was captured from.
    #[serde(default)]
    pub source_compaction_seq: Option<u64>,
    /// True when created automatically after a successful compaction.
    #[serde(default)]
    pub automatic: bool,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContextCheckpointListParams {
    pub session_id: String,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContextCheckpointListResult {
    pub session_id: String,
    pub checkpoints: Vec<ContextCheckpoint>,
    pub latest_id: Option<String>,
    pub policy: ContextCheckpointPolicy,
    pub limit: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContextCheckpointDetailParams {
    pub session_id: String,
    pub checkpoint_id: String,
}

/// Deterministic restore preflight. Errors are safety refusals; warnings are
/// token-estimate advisories that `force` may bypass without weakening a
/// model, plan, worktree, import, or budget check.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextRestorePreflight {
    pub restorable: bool,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub estimated_tokens: u64,
    #[serde(default)]
    pub hard_limit_tokens: u64,
    #[serde(default)]
    pub threshold_tokens: u64,
    #[serde(default)]
    pub active_messages: usize,
    #[serde(default)]
    pub plan_revision: u32,
    #[serde(default)]
    pub checkpoint_plan_revision: u32,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub worktree_head: String,
    #[serde(default)]
    pub current_worktree_head: Option<String>,
    #[serde(default)]
    pub worktree_descendant: bool,
    #[serde(default)]
    pub budget_exhausted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContextCheckpointDetailResult {
    pub checkpoint: ContextCheckpoint,
    pub preflight: ContextRestorePreflight,
    /// True when this checkpoint is the boundary the latest compaction event
    /// points at (a restore would be a no-op rewrite of the same boundary).
    pub is_active: bool,
    /// One-line, bounded preview of the stored summary for UI/CLI.
    #[serde(default)]
    pub summary_preview: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContextCheckpointCreateParams {
    pub session_id: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContextRestoreParams {
    pub session_id: String,
    pub checkpoint_id: String,
    /// Bypasses only the token-threshold advisory. Hard-limit, model, plan,
    /// worktree, import, and budget safety checks always apply.
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContextRestoreResult {
    pub session_id: String,
    pub checkpoint_id: String,
    pub mode: String,
    pub through_message_id: Option<String>,
    pub active_messages: usize,
    pub estimated_tokens: u64,
    pub hard_limit_tokens: u64,
    pub warning: Option<String>,
    pub compaction_event_seq: u64,
    pub restored_event_seq: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContextCheckpointDeleteParams {
    pub session_id: String,
    pub checkpoint_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContextCheckpointDeleteResult {
    pub session_id: String,
    pub checkpoint_id: String,
    pub removed: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContextCheckpointPruneParams {
    pub session_id: String,
    #[serde(default)]
    pub older_than_days: Option<u32>,
    #[serde(default)]
    pub keep_latest: Option<u32>,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContextCheckpointPruneResult {
    pub session_id: String,
    pub candidates: usize,
    pub removed: usize,
    pub dry_run: bool,
    pub checkpoints: Vec<ContextCheckpoint>,
}

/// Project-scoped retention for context checkpoints. The daemon sweep is
/// opt-in; manual prune is always available. `max_age_days = 0` disables the
/// age arm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCheckpointPolicy {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_context_checkpoint_max")]
    pub max_checkpoints: u32,
    #[serde(default = "default_context_checkpoint_age")]
    pub max_age_days: u32,
}

fn default_context_checkpoint_max() -> u32 {
    20
}

fn default_context_checkpoint_age() -> u32 {
    30
}

impl Default for ContextCheckpointPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            max_checkpoints: default_context_checkpoint_max(),
            max_age_days: default_context_checkpoint_age(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContextCheckpointPolicyParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetContextCheckpointPolicyParams {
    pub project_id: String,
    pub policy: ContextCheckpointPolicy,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContextCheckpointPolicyResult {
    pub project_id: String,
    pub policy: ContextCheckpointPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionCheckpoint {
    pub id: String,
    pub session_id: String,
    pub label: String,
    pub commit: String,
    pub branch: String,
    #[serde(default)]
    pub start_message_id: Option<String>,
    #[serde(default)]
    pub through_message_id: Option<String>,
    #[serde(default)]
    pub message_count: usize,
    /// Prefixed summary injected at the head of the active conversation, if
    /// the source had already been compacted.
    #[serde(default)]
    pub summary: Option<String>,
    /// True when the checkpoint commit includes uncommitted worktree changes.
    #[serde(default)]
    pub dirty: bool,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionCheckpointParams {
    pub session_id: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionCheckpointListParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionCheckpointListResult {
    pub checkpoints: Vec<SessionCheckpoint>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionCheckpointStatsParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionCheckpointStatsResult {
    pub session_id: String,
    pub checkpoints: usize,
    pub dirty: usize,
    pub refs: usize,
    pub commit_bytes: u64,
    pub snapshot_bytes: u64,
    pub unique_blobs: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionCheckpointRestoreParams {
    pub session_id: String,
    pub checkpoint_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionCheckpointRestoreResult {
    pub session_id: String,
    pub checkpoint_id: String,
    pub previous_commit: String,
    pub restored_commit: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionCheckpointDiffParams {
    pub session_id: String,
    pub checkpoint_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionCheckpointDiffResult {
    pub session_id: String,
    pub checkpoint_id: String,
    pub current_commit: String,
    pub checkpoint_commit: String,
    pub current_branch: String,
    pub clean: bool,
    pub files: Vec<SessionCompareFile>,
    pub patch: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionCheckpointDeleteParams {
    pub session_id: String,
    pub checkpoint_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionCheckpointDeleteResult {
    pub session_id: String,
    pub checkpoint_id: String,
    pub removed: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionCheckpointPruneParams {
    pub session_id: String,
    #[serde(default)]
    pub older_than_days: Option<u32>,
    #[serde(default)]
    pub keep_latest: Option<u32>,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionCheckpointPruneResult {
    pub session_id: String,
    pub candidates: usize,
    pub removed: usize,
    pub dry_run: bool,
    pub checkpoints: Vec<SessionCheckpoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckpointRetentionPolicy {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_checkpoint_older_secs")]
    pub older_than_secs: u64,
    #[serde(default = "default_checkpoint_keep_latest")]
    pub keep_latest: u32,
}

impl Default for CheckpointRetentionPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            older_than_secs: default_checkpoint_older_secs(),
            keep_latest: default_checkpoint_keep_latest(),
        }
    }
}

fn default_checkpoint_older_secs() -> u64 {
    30 * 24 * 60 * 60
}

fn default_checkpoint_keep_latest() -> u32 {
    5
}

#[derive(Debug, Clone, Deserialize)]
pub struct CheckpointPolicyParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetCheckpointPolicyParams {
    pub project_id: String,
    pub policy: CheckpointRetentionPolicy,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckpointPolicyResult {
    pub project_id: String,
    pub policy: CheckpointRetentionPolicy,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionForkParams {
    pub session_id: String,
    #[serde(default)]
    pub checkpoint_id: Option<String>,
    #[serde(default)]
    pub goal: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionForkResult {
    pub source_session_id: String,
    pub checkpoint_id: String,
    pub session: Session,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionLineageEntry {
    pub session: Session,
    #[serde(default)]
    pub checkpoint_id: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionLineageResult {
    pub session_id: String,
    pub parent: Option<SessionLineageEntry>,
    pub siblings: Vec<SessionLineageEntry>,
    pub children: Vec<SessionLineageEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionLineageParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionForkTreeNode {
    pub session: Session,
    #[serde(default)]
    pub checkpoint_id: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub children: Vec<SessionForkTreeNode>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionForkTreeResult {
    pub session_id: String,
    pub root: SessionForkTreeNode,
    pub total: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionForkTreeParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectForkForestParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectForkForestResult {
    pub project_id: String,
    pub roots: Vec<SessionForkTreeNode>,
    pub total: usize,
    pub checkpoints: usize,
    pub dirty_checkpoints: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ForkAuditParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ForkAuditIssue {
    pub kind: String,
    pub session_id: String,
    #[serde(default)]
    pub related_session_id: Option<String>,
    #[serde(default)]
    pub checkpoint_id: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    pub detail: String,
    pub repairable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ForkAuditResult {
    pub project_id: String,
    pub sessions: usize,
    pub checkpoints: usize,
    pub issues: Vec<ForkAuditIssue>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ForkRepairParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ForkRepairResult {
    pub project_id: String,
    pub repaired: usize,
    pub failed: usize,
    pub details: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DashboardCount {
    pub label: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardProjectSummary {
    pub project_id: String,
    pub name: String,
    pub sessions: usize,
    pub running_sessions: usize,
    pub queued_sessions: usize,
    pub failed_sessions: usize,
    pub pending_approvals: usize,
    pub pending_questions: usize,
    pub tasks: usize,
    pub running_tasks: usize,
    pub checkpoints: usize,
    pub dirty_checkpoints: usize,
    pub fork_roots: usize,
    pub fork_sessions: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardSummary {
    pub generated_at: String,
    pub project_id: Option<String>,
    pub projects: usize,
    pub sessions: usize,
    pub sessions_by_status: Vec<DashboardCount>,
    pub pending_approvals: usize,
    pub pending_questions: usize,
    pub running_sessions: usize,
    pub queued_sessions: usize,
    pub tasks: usize,
    pub tasks_by_status: Vec<DashboardCount>,
    pub running_tasks: usize,
    pub write_leases: usize,
    pub checkpoints: usize,
    pub dirty_checkpoints: usize,
    pub fork_roots: usize,
    pub fork_sessions: usize,
    pub project_summaries: Vec<DashboardProjectSummary>,
    #[serde(default)]
    pub agent: Option<AgentInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardAlert {
    pub id: String,
    pub kind: String,
    pub severity: String,
    pub title: String,
    pub detail: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    /// UI section hint: inbox, team, git, usage, diagnostics.
    pub action: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardResult {
    pub summary: DashboardSummary,
    pub alerts: Vec<DashboardAlert>,
    pub alerts_truncated: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DashboardParams {
    #[serde(default)]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelPreset {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub input_usd_per_million: Option<f64>,
    #[serde(default)]
    pub output_usd_per_million: Option<f64>,
    #[serde(default)]
    pub notes: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelPresetListResult {
    pub presets: Vec<ModelPreset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateModelPresetParams {
    pub name: String,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub input_usd_per_million: Option<f64>,
    #[serde(default)]
    pub output_usd_per_million: Option<f64>,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateModelPresetParams {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub input_usd_per_million: Option<f64>,
    #[serde(default)]
    pub output_usd_per_million: Option<f64>,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeleteModelPresetParams {
    pub id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeleteModelPresetResult {
    pub removed: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct UpdateCheckParams {
    #[serde(default)]
    pub repo: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateCheckResult {
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
    pub repo: String,
    #[serde(default)]
    pub release_name: Option<String>,
    #[serde(default)]
    pub release_url: Option<String>,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub dmg_name: Option<String>,
    #[serde(default)]
    pub dmg_url: Option<String>,
    #[serde(default)]
    pub sha256_name: Option<String>,
    #[serde(default)]
    pub sha256_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct UpdateDownloadParams {
    #[serde(default)]
    pub repo: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateDownloadResult {
    pub version: String,
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub verified: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionCompareParams {
    pub session_id: String,
    pub other_session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionCompareFile {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
    pub binary: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionCompareResult {
    pub session_id: String,
    pub other_session_id: String,
    pub left_commit: String,
    pub right_commit: String,
    pub files: Vec<SessionCompareFile>,
    pub patch: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionMergeForkParams {
    pub source_session_id: String,
    pub fork_session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionMergeForkResult {
    pub source_session_id: String,
    pub fork_session_id: String,
    /// merged | conflict | failed
    pub status: String,
    pub pre_merge_commit: String,
    pub fork_commit: String,
    #[serde(default)]
    pub merge_commit: Option<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    pub detail: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PromptParams {
    pub session_id: String,
    pub content: String,
    /// Optional caller-supplied idempotency key. Repeating the same id with
    /// the same payload replays the first accepted result instead of starting
    /// a second run.
    #[serde(default)]
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PromptResult {
    pub accepted: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListApprovalsParams {
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApprovalListResult {
    pub approvals: Vec<Approval>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResolveApprovalParams {
    pub approval_id: String,
    pub approved: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolveApprovalResult {
    pub resolved: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConfigureAgentParams {
    pub provider: String,
    #[serde(default)]
    pub base_url: Option<String>,
    pub model: String,
    #[serde(default)]
    pub role_models: Option<RoleModels>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub max_turns: Option<u32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub max_managers: Option<u32>,
    #[serde(default)]
    pub max_workers: Option<u32>,
    #[serde(default)]
    pub max_labour: Option<u32>,
    #[serde(default)]
    pub auto_expand_workers: Option<bool>,
    /// Context window for the base model, in tokens. 0 disables automatic
    /// context management.
    #[serde(default)]
    pub context_window: Option<u64>,
    /// Risk-based approval policy. Omitted preserves `smart`.
    #[serde(default)]
    pub approval_mode: Option<ApprovalMode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionDiffParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDiff {
    pub session_id: String,
    pub branch: String,
    pub worktree: String,
    #[serde(default)]
    pub base_commit: Option<String>,
    pub status: String,
    pub patch: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CommitSessionParams {
    pub session_id: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCommit {
    pub session_id: String,
    pub commit: String,
    pub message: String,
    pub summary: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiscardSessionParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CancelSessionParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CancelSessionResult {
    pub cancelled: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionPauseParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionPauseResult {
    pub session_id: String,
    pub paused: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionResumeParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionResumeResult {
    pub session_id: String,
    pub resumed: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionPlanResumeParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionPlanResumeResult {
    pub session_id: String,
    pub started: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub title: String,
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionTodosParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionTodosResult {
    pub session_id: String,
    pub items: Vec<TodoItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeQueueEntry {
    pub id: String,
    pub session_id: String,
    pub agent_id: String,
    pub agent_name: String,
    #[serde(default)]
    pub task_id: Option<String>,
    pub branch: String,
    pub commit: String,
    pub base_commit: String,
    /// Session HEAD recorded immediately before the Integrator starts. This is
    /// the exact rollback target if the process crashes mid-merge.
    #[serde(default)]
    pub pre_merge_commit: Option<String>,
    pub status: String,
    pub attempt: u32,
    #[serde(default)]
    pub detail: Option<String>,
    /// `local` for an ordinary Worker branch, `remote` for an imported bundle.
    /// Older rows decode as `local`; this is additive within protocol v1.
    #[serde(default = "merge_source_local")]
    pub source: String,
    #[serde(default)]
    pub remote_host: Option<String>,
    #[serde(default)]
    pub remote_branch: Option<String>,
    #[serde(default)]
    pub bundle_sha256: Option<String>,
    /// Temporary ref (`refs/verlinpark/remote/...`) that holds the exact
    /// imported commit. Retained while a remote entry is retryable.
    #[serde(default)]
    pub imported_ref: Option<String>,
    /// Verifier verdict attached to a remote entry. Local entries leave these
    /// empty because their approval is already part of the Worker run.
    #[serde(default)]
    pub verifier_approved: Option<bool>,
    #[serde(default)]
    pub verifier_summary: Option<String>,
    #[serde(default)]
    pub environment_caveat: Option<String>,
    /// Supplemental remote-test evidence attached by `remote.test`. These
    /// fields never change the Verifier verdict or the merge gate; older rows
    /// decode as `None`.
    #[serde(default)]
    pub remote_test_evidence_id: Option<String>,
    #[serde(default)]
    pub remote_test_digest: Option<String>,
    #[serde(default)]
    pub remote_test_status: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

pub const MERGE_SOURCE_LOCAL: &str = "local";
pub const MERGE_SOURCE_REMOTE: &str = "remote";

fn merge_source_local() -> String {
    MERGE_SOURCE_LOCAL.to_string()
}

/// Provenance supplied when an entry enters the merge queue. Local Worker
/// enqueues use [`MergeQueueSource::local`]; remote integration uses
/// [`MergeQueueSource::remote`] with the exported host, branch, bundle hash,
/// and temporary imported ref.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeQueueSource {
    #[serde(default = "merge_source_local")]
    pub source: String,
    #[serde(default)]
    pub remote_host: Option<String>,
    #[serde(default)]
    pub remote_branch: Option<String>,
    #[serde(default)]
    pub bundle_sha256: Option<String>,
    #[serde(default)]
    pub imported_ref: Option<String>,
}

impl Default for MergeQueueSource {
    fn default() -> Self {
        Self::local()
    }
}

impl MergeQueueSource {
    pub fn local() -> Self {
        Self {
            source: MERGE_SOURCE_LOCAL.to_string(),
            remote_host: None,
            remote_branch: None,
            bundle_sha256: None,
            imported_ref: None,
        }
    }

    pub fn remote(
        host: impl Into<String>,
        branch: impl Into<String>,
        bundle_sha256: impl Into<String>,
        imported_ref: impl Into<String>,
    ) -> Self {
        Self {
            source: MERGE_SOURCE_REMOTE.to_string(),
            remote_host: Some(host.into()),
            remote_branch: Some(branch.into()),
            bundle_sha256: Some(bundle_sha256.into()),
            imported_ref: Some(imported_ref.into()),
        }
    }

    pub fn is_remote(&self) -> bool {
        self.source == MERGE_SOURCE_REMOTE
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct MergeQueueListParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MergeQueueListResult {
    pub entries: Vec<MergeQueueEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RetryMergeParams {
    pub entry_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetryMergeResult {
    pub entry: MergeQueueEntry,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResolveMergeParams {
    pub entry_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolveMergeResult {
    pub entry: MergeQueueEntry,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LandSessionParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionLandResult {
    pub session_id: String,
    pub branch: String,
    pub target_branch: String,
    pub commit: String,
    pub detail: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpenPullRequestParams {
    pub session_id: String,
    /// Explicit provider (`github`, `gitlab`, `gitea`/`forgejo`, `bitbucket`).
    /// Older clients omit this and the daemon uses `VERLINPARK_PR_PROVIDER` or
    /// origin detection.
    #[serde(default)]
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequestResult {
    pub session_id: String,
    pub branch: String,
    pub base_branch: String,
    pub pushed: bool,
    #[serde(default)]
    pub url: Option<String>,
    pub detail: String,
    /// Stable forge id (`github`, `gitlab`, `gitea`, `bitbucket`). Older
    /// daemons/clients omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// `owner/repository` when the origin was parseable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// `PR` or `MR`, matching the provider's terminology.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<String>,
}

/// Optional project context for `forge.status`. Omitting it reports the global
/// provider/credential inventory without an origin inspection.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ForgeStatusParams {
    #[serde(default)]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgeProviderStatus {
    pub provider: String,
    pub change_name: String,
    pub transport: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default)]
    pub command_available: bool,
    /// `Some` only when the daemon can decide from configuration alone
    /// (Bitbucket Cloud). CLI providers report `None` until preflight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_configured: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_source: Option<String>,
    #[serde(default)]
    pub supported: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgeOriginStatus {
    /// Sanitized origin identity (host and repository only, never userinfo).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default)]
    pub supported: bool,
    /// True when the origin looks like Bitbucket Server/Data Center.
    #[serde(default)]
    pub server_detected: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgeStatusResult {
    /// Raw `VERLINPARK_PR_PROVIDER` selection, if set. This is a provider name,
    /// never a credential.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_override: Option<String>,
    /// Provider that would be used for the detected origin.
    pub default_provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<ForgeOriginStatus>,
    pub providers: Vec<ForgeProviderStatus>,
    pub detail: String,
}

/// Explicit read-only credential/repository probe for one project. It never
/// pushes a branch or creates a pull request.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ForgePreflightParams {
    pub project_id: String,
    /// Optional explicit provider; otherwise the daemon env/origin decides.
    #[serde(default)]
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgePreflightResult {
    pub provider: String,
    pub change_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// `basic`, `bearer`, or `cli`. This is an auth mode, never a secret.
    pub auth: String,
    pub detail: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionExportParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionImportParams {
    pub bundle: SessionBundle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionImportResult {
    pub project_id: String,
    pub session_id: String,
    pub events: usize,
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RehydrateSessionParams {
    pub session_id: String,
    #[serde(default)]
    pub worktree: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRehydrateResult {
    pub session_id: String,
    pub worktree: String,
    pub branch: String,
    pub commit: String,
    pub status: String,
    #[serde(default)]
    pub agents_remapped: usize,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionLogStatus {
    pub session_id: String,
    pub mirror_events: usize,
    pub database_events: usize,
    pub matches: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeRetentionPolicy {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_worktree_retention_secs")]
    pub older_than_secs: u64,
    #[serde(default)]
    pub delete_branches: bool,
    #[serde(default = "default_true")]
    pub keep_failed: bool,
}

impl Default for WorktreeRetentionPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            older_than_secs: default_worktree_retention_secs(),
            delete_branches: false,
            keep_failed: true,
        }
    }
}

fn default_worktree_retention_secs() -> u64 {
    7 * 24 * 60 * 60
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorktreePolicyParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetWorktreePolicyParams {
    pub project_id: String,
    pub policy: WorktreeRetentionPolicy,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorktreePolicyResult {
    pub project_id: String,
    pub policy: WorktreeRetentionPolicy,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CleanupWorktreesParams {
    pub project_id: String,
    #[serde(default)]
    pub older_than_secs: u64,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub delete_branches: bool,
    #[serde(default)]
    pub keep_failed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeCleanupEntry {
    pub session_id: String,
    pub status: String,
    pub worktree: String,
    pub branch: String,
    pub removed: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeCleanupResult {
    pub project_id: String,
    pub scanned: usize,
    pub removed: usize,
    pub skipped: usize,
    pub entries: Vec<WorktreeCleanupEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionLogParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReplaySessionParams {
    pub session_id: String,
    #[serde(default)]
    pub repair: bool,
    #[serde(default)]
    pub rebuild: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionBundle {
    pub version: u32,
    pub exported_at: String,
    pub project: Project,
    pub session: Session,
    pub events: Vec<Event>,
    pub agents: Vec<AgentRecord>,
    pub messages: Vec<StoredMessage>,
    #[serde(default)]
    pub team_plan: Option<TeamPlan>,
    pub todos: Vec<TodoItem>,
    #[serde(default)]
    pub approvals: Vec<Approval>,
    pub merge_queue: Vec<MergeQueueEntry>,
    #[serde(default)]
    pub agent_messages: Vec<AgentMessage>,
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    #[serde(default)]
    pub usage: Vec<UsageEntry>,
    #[serde(default)]
    pub memories: Vec<MemoryItem>,
    /// Deterministic memory conflicts owned by this session's event stream.
    #[serde(default)]
    pub memory_conflicts: Vec<MemoryConflict>,
    #[serde(default)]
    pub questions: Vec<Question>,
    /// Persistent task-DAG rows. Older bundles decode as an empty queue.
    #[serde(default)]
    pub tasks: Vec<ScheduledTask>,
    /// Operator steering records. Older bundles decode as an empty list.
    #[serde(default)]
    pub steering: Vec<SteeringMessage>,
    /// Deferred continuations. Older bundles decode as an empty list.
    #[serde(default)]
    pub continuations: Vec<SessionContinuation>,
    /// Event-backed external gates. The event stream remains authoritative;
    /// this snapshot carries the same non-secret gate records (including the
    /// verifier hash, never a raw token).
    #[serde(default)]
    pub gates: Vec<GateRecord>,
    /// Immutable project policy bundles referenced by this archive's plan/run
    /// snapshots. No secrets: policies only.
    #[serde(default)]
    pub policy_snapshots: Vec<PolicySnapshot>,
    /// Event-backed stall recovery rows. Older bundles decode as an empty
    /// list; importing the archive replays the recovery events it carries.
    #[serde(default)]
    pub recoveries: Vec<OrchestrationRecovery>,
    /// Program ledgers linked to this session. Exported as an archive
    /// snapshot; imported programs are marked archived and never auto-run.
    #[serde(default)]
    pub programs: Vec<Program>,
    #[serde(default)]
    pub program_milestones: Vec<ProgramMilestone>,
    #[serde(default)]
    pub program_events: Vec<ProgramEvent>,
    /// Event-backed durable run rows. Older bundles decode as an empty list.
    #[serde(default)]
    pub session_runs: Vec<SessionRun>,
    /// Event-backed run step rows. Tool outcomes are deliberately not part of
    /// a replayable bundle; they are operational audit state.
    #[serde(default)]
    pub run_steps: Vec<RunStep>,
    #[serde(default)]
    pub run_checkpoints: Vec<RunCheckpoint>,
    /// Durable active-context boundaries. Older bundles decode as empty.
    #[serde(default)]
    pub context_checkpoints: Vec<ContextCheckpoint>,
    /// Event-backed durable supervised-process lifecycle rows. Output bodies
    /// are operational state and are not part of the archive; imported rows are
    /// metadata-only and can never be reattached or read as local files.
    #[serde(default)]
    pub supervised_processes: Vec<SupervisedProcess>,
    /// Event-backed test-cache metadata. Output bodies are operational state
    /// and are not part of the archive; imported rows can be inspected but their
    /// `output_ref` cannot resolve a local body.
    #[serde(default)]
    pub test_results: Vec<TestCacheEntry>,
    /// Content-addressed instruction bodies referenced by
    /// `session_instruction_state`. Older bundles decode as an empty list and
    /// remain readable; a missing blob is rebuilt deterministically only on an
    /// explicit run, never during archive import.
    #[serde(default)]
    pub instruction_blobs: Vec<InstructionBlob>,
    /// Ordered per-scope instruction state. Imported archives are read-only;
    /// these rows are preserved for inspection and never trigger generation.
    #[serde(default)]
    pub instructions: Vec<SessionInstructionState>,
    /// Event-derived cache-epoch observations for this session. Older bundles
    /// decode as an empty list; replay can rebuild the rows from the archived
    /// cache events.
    #[serde(default)]
    pub prompt_prefix_observations: Vec<PromptPrefixObservation>,
    /// Event-backed local effect receipts. Older bundles decode as an empty
    /// list; the archived `effect.*` events remain authoritative and replay
    /// rebuilds the rows.
    #[serde(default)]
    pub effects: Vec<EffectReceipt>,
    /// Project-scoped identities for this archive's project. Older bundles
    /// decode as an empty list. Imported identities are read-only until an
    /// explicit rehome; their project ids are never silently remapped.
    #[serde(default)]
    pub agent_identities: Vec<AgentIdentity>,
    /// Append-only identity lifecycle events. These are project-scoped and are
    /// exported alongside the session archive so projections can be rebuilt
    /// without inventing rows.
    #[serde(default)]
    pub agent_identity_events: Vec<AgentIdentityEvent>,
    /// Session→identity assignments for this archive's project. Older bundles
    /// decode as an empty list. Leases are evidence, not live local ownership;
    /// the daemon startup sweep expires any lease whose timestamp has passed.
    #[serde(default)]
    pub session_agent_assignments: Vec<AgentAssignment>,
    /// Project-scoped agent inbox rows for this archive's project. Older
    /// bundles decode as an empty list and imported rows remain inert.
    #[serde(default)]
    pub agent_inbox: Vec<AgentInboxMessage>,
    /// Append-only assignment/inbox event stream. These are project-scoped and
    /// portable by event id, so an archive import can be idempotent without
    /// colliding with an unrelated local sequence.
    #[serde(default)]
    pub agent_coordination_events: Vec<AgentCoordinationEvent>,
    /// Project-scoped Worker component workspaces. Older bundles decode as an
    /// empty list. Imported workspaces are read-only evidence; their branches
    /// and worktrees are never attached or landed locally.
    #[serde(default)]
    pub agent_workspaces: Vec<AgentWorkspace>,
    /// Session attachments of project workspaces. Older bundles decode as an
    /// empty list and imported rows are released evidence.
    #[serde(default)]
    pub session_worker_workspaces: Vec<SessionWorkerWorkspace>,
    /// Append-only workspace event stream. Portable by event id; a same-id
    /// event must be byte-identical on import.
    #[serde(default)]
    pub agent_workspace_events: Vec<AgentWorkspaceEvent>,
}

/// Project policy for automatic crash resume. Manual `session.run.resume`
/// always remains available within the same bounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRunPolicy {
    /// Opt-in. Default false: a daemon restart marks runs interrupted and
    /// leaves them for an operator.
    #[serde(default)]
    pub auto_resume: bool,
    /// Total attempts, including the first run. Bounded 1..=3.
    #[serde(default = "default_run_max_attempts")]
    pub max_attempts: u32,
    /// A run older than this is not resumed automatically or manually.
    /// Bounded 1..=168 hours.
    #[serde(default = "default_run_max_age_hours")]
    pub max_age_hours: u32,
}

impl Default for SessionRunPolicy {
    fn default() -> Self {
        Self {
            auto_resume: false,
            max_attempts: default_run_max_attempts(),
            max_age_hours: default_run_max_age_hours(),
        }
    }
}

pub fn default_run_max_attempts() -> u32 {
    1
}

pub fn default_run_max_age_hours() -> u32 {
    24
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunBudgetSnapshot {
    #[serde(default)]
    pub token_limit: Option<u64>,
    #[serde(default)]
    pub tokens_spent: u64,
    #[serde(default)]
    pub cost_limit_usd: Option<f64>,
    #[serde(default)]
    pub cost_spent_usd: f64,
    #[serde(default)]
    pub time_limit_secs: Option<u64>,
    #[serde(default)]
    pub elapsed_secs: u64,
    #[serde(default)]
    pub unlimited: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFailure {
    /// Stable classification: `error`, `cancelled`, `budget_exceeded`,
    /// `max_turns`, or `interrupted`.
    pub kind: String,
    pub message: String,
}

/// One durable agent run. Status and attempt transitions are event-backed;
/// step tool outcomes are operational audit state and are exempt from replay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRun {
    pub run_id: String,
    pub session_id: String,
    pub project_id: String,
    pub agent_id: String,
    #[serde(default)]
    pub agent_name: String,
    #[serde(default)]
    pub agent_role: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub department: Option<String>,
    #[serde(default)]
    pub objective: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    /// `running`, `completed`, `failed`, `interrupted`, or `resumed`.
    pub status: String,
    #[serde(default = "default_run_attempt")]
    pub attempt: u32,
    #[serde(default)]
    pub resumed_from_run_id: Option<String>,
    #[serde(default)]
    pub resume_root_id: String,
    #[serde(default)]
    pub top_level: bool,
    pub started_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub ended_at: Option<String>,
    #[serde(default)]
    pub budget: RunBudgetSnapshot,
    #[serde(default)]
    pub worktree_head: String,
    #[serde(default)]
    pub plan_revision: u32,
    /// Runtime/policy/tool-bundle/layout identity captured when this run
    /// started. Older rows and bundles decode as `None` (legacy drift).
    #[serde(default)]
    pub runtime_snapshot: Option<RuntimeSnapshot>,
    #[serde(default)]
    pub last_event_seq: u64,
    #[serde(default)]
    pub failure: Option<RunFailure>,
}

pub fn default_run_attempt() -> u32 {
    1
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunStep {
    pub step_id: String,
    pub run_id: String,
    pub session_id: String,
    pub seq: u64,
    pub turn: u32,
    /// `model_request`, `tool_call`, `compaction`, `approval`, or `question`.
    pub kind: String,
    /// `intent`, `completed`, `failed`, or `unknown`.
    pub status: String,
    #[serde(default)]
    pub input_hash: String,
    #[serde(default)]
    pub output_hash: String,
    #[serde(default)]
    pub payload: Value,
    pub created_at: String,
    pub updated_at: String,
}

/// Operational audit row for one tool invocation. It is durable so a resume can
/// detect completed write tools and interrupted calls, but it is intentionally
/// not rebuilt by replay/repair and is not exported as replayable state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunToolCall {
    pub run_id: String,
    pub session_id: String,
    pub step_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub input_fingerprint: String,
    /// `intent`, `completed`, `failed`, or `unknown`.
    pub status: String,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub consumed_by_run_id: Option<String>,
    #[serde(default)]
    pub consumed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunCheckpoint {
    pub checkpoint_id: String,
    pub run_id: String,
    pub session_id: String,
    pub seq: u64,
    /// Agent turn at which the checkpoint was taken.
    #[serde(default)]
    pub turn: u32,
    #[serde(default)]
    pub message_cursor: u64,
    #[serde(default)]
    pub event_cursor: u64,
    #[serde(default)]
    pub budget: RunBudgetSnapshot,
    #[serde(default)]
    pub worktree_head: String,
    #[serde(default)]
    pub plan_revision: u32,
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionRunsParams {
    pub session_id: String,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRunsResult {
    pub session_id: String,
    pub runs: Vec<SessionRun>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionRunDetailParams {
    pub run_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRunDetailResult {
    pub run: SessionRun,
    pub steps: Vec<RunStep>,
    pub tool_calls: Vec<RunToolCall>,
    pub checkpoints: Vec<RunCheckpoint>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionRunResumeParams {
    pub session_id: String,
    /// Omit to resume the newest resumable top-level run.
    #[serde(default)]
    pub run_id: Option<String>,
    /// Optional idempotency key for this explicit resume request. This is a
    /// separate receipt from session prompt admission.
    #[serde(default)]
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRunResumeResult {
    pub session_id: String,
    pub resumed: bool,
    pub run_id: String,
    pub resumed_from_run_id: Option<String>,
    pub attempt: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionRunCancelParams {
    pub session_id: String,
    #[serde(default)]
    pub run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRunCancelResult {
    pub session_id: String,
    pub run_id: Option<String>,
    pub cancelled: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionRunPolicyParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetSessionRunPolicyParams {
    pub project_id: String,
    pub policy: SessionRunPolicy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRunPolicyResult {
    pub project_id: String,
    pub policy: SessionRunPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionReplayReport {
    pub session_id: String,
    pub event_count: usize,
    pub status_matches: bool,
    pub usage_matches: bool,
    pub plan_matches: bool,
    pub todos_matches: bool,
    pub messages_matches: bool,
    pub agents_matches: bool,
    pub queue_matches: bool,
    #[serde(default)]
    pub mailbox_matches: bool,
    #[serde(default)]
    pub artifacts_matches: bool,
    #[serde(default)]
    pub approvals_matches: bool,
    #[serde(default)]
    pub ledger_matches: bool,
    #[serde(default)]
    pub memories_matches: bool,
    #[serde(default)]
    pub memory_conflicts_match: bool,
    #[serde(default)]
    pub questions_matches: bool,
    #[serde(default)]
    pub task_queue_matches: bool,
    #[serde(default)]
    pub notes_match: bool,
    #[serde(default)]
    pub decisions_match: bool,
    /// Event-backed deterministic scaling decision projection.
    #[serde(default)]
    pub scaling_decisions_match: bool,
    #[serde(default)]
    pub recoveries_match: bool,
    #[serde(default)]
    pub steering_matches: bool,
    #[serde(default)]
    pub continuations_match: bool,
    /// Event-backed external-gate projection (`gate.*` events).
    #[serde(default)]
    pub gates_match: bool,
    /// Event-backed durable run status/attempt projections.
    #[serde(default)]
    pub runs_match: bool,
    #[serde(default)]
    pub run_steps_match: bool,
    #[serde(default)]
    pub run_checkpoints_match: bool,
    /// Event-backed durable context checkpoint projection.
    #[serde(default)]
    pub context_checkpoints_match: bool,
    /// Event-backed durable supervised-process lifecycle projection. Operational
    /// heartbeat/output telemetry is deliberately excluded from the comparison.
    #[serde(default)]
    pub processes_match: bool,
    /// Event-backed supplemental remote-test evidence projection.
    #[serde(default)]
    pub remote_test_evidence_matches: bool,
    /// Event-backed task-contract projection (`contract.derived` events).
    #[serde(default)]
    pub contracts_match: bool,
    /// Event-backed local effect receipt projection (`effect.*` events).
    #[serde(default)]
    pub effects_match: bool,
    /// Immutable policy-bundle rows referenced by this session's plan/run
    /// snapshots and `policy.snapshot` events.
    #[serde(default)]
    pub policy_snapshots_match: bool,
    pub consistent: bool,
    #[serde(default)]
    pub repaired: bool,
    #[serde(default)]
    pub rebuilt: bool,
    #[serde(default)]
    pub mismatches: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentRecord {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    /// Project-scoped identity behind this run, when one was explicitly
    /// assigned. S1 leaves this `None` for every existing run record; session
    /// assignment is a separate phase.
    #[serde(default)]
    pub identity_id: Option<String>,
    /// Project workspace this Worker run is explicitly bound to. `None` means
    /// the legacy task-worktree flow.
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// Parent session workspace for a Labour run. Labour worktrees branch from
    /// the Worker workspace HEAD and merge back into it.
    #[serde(default)]
    pub parent_workspace_id: Option<String>,
    pub parent_id: Option<String>,
    /// Plan task this agent is currently responsible for, when applicable.
    #[serde(default)]
    pub task_id: Option<String>,
    /// Plan department this agent belongs to, when applicable.
    #[serde(default)]
    pub department: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    pub status: String,
    pub model: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentListParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentListResult {
    pub agents: Vec<AgentRecord>,
}

// ---------------------------------------------------------------------------
// Project-scoped agent identities (S1)
// ---------------------------------------------------------------------------

/// Roles that may own a durable project identity. Labour, Verifier, and Scout
/// are deliberately absent: Labour is hired per task, and the other two are
/// session/task gates rather than project employees.
pub const AGENT_IDENTITY_ROLES: [&str; 3] = ["leader", "manager", "worker"];

/// Stored lifecycle states. `idle` is reserved for the session-assignment
/// phase and is never produced by the S1 lifecycle API.
pub const AGENT_IDENTITY_STATUSES: [&str; 4] = ["active", "idle", "paused", "retired"];

pub const AGENT_IDENTITY_CREATED_EVENT: &str = "agent.identity.created";
pub const AGENT_IDENTITY_UPDATED_EVENT: &str = "agent.identity.updated";
pub const AGENT_IDENTITY_PAUSED_EVENT: &str = "agent.identity.paused";
pub const AGENT_IDENTITY_RESUMED_EVENT: &str = "agent.identity.resumed";
pub const AGENT_IDENTITY_RETIRED_EVENT: &str = "agent.identity.retired";

pub const AGENT_IDENTITY_EVENT_KINDS: [&str; 5] = [
    AGENT_IDENTITY_CREATED_EVENT,
    AGENT_IDENTITY_UPDATED_EVENT,
    AGENT_IDENTITY_PAUSED_EVENT,
    AGENT_IDENTITY_RESUMED_EVENT,
    AGENT_IDENTITY_RETIRED_EVENT,
];

pub const AGENT_IDENTITY_PROFILE_VALUES: [&str; 4] =
    ["coding", "research", "verification", "generic"];

/// Runtime hard caps. A project policy may only narrow these; it can never
/// enable more Managers or Workers than the existing team runtime will admit.
pub const AGENT_IDENTITY_MAX_MANAGERS_HARD_CAP: u32 = 5;
pub const AGENT_IDENTITY_MAX_WORKERS_HARD_CAP: u32 = 10;

/// Bounded per-identity metadata. This is configuration, not prompt text or
/// model output, and it is never interpreted as a credential.
pub const AGENT_IDENTITY_MAX_POLICY_BYTES: usize = 8_192;
pub const AGENT_IDENTITY_MAX_DEPARTMENT_CHARS: usize = 120;
pub const AGENT_IDENTITY_MAX_COMPONENT_CHARS: usize = 200;
pub const AGENT_IDENTITY_MAX_TITLE_CHARS: usize = 200;
pub const AGENT_IDENTITY_MAX_MODEL_CHARS: usize = 200;
pub const AGENT_IDENTITY_MAX_ID_CHARS: usize = 128;
pub const AGENT_IDENTITY_MAX_ROLE_CHARS: usize = 32;
pub const AGENT_IDENTITY_MAX_STATUS_CHARS: usize = 32;

/// Stable, bounded error codes for identity RPCs. These are wire values, not
/// message-text classifications.
pub mod agent_identity_codes {
    pub const IDENTITY_NOT_FOUND: &str = "IDENTITY_NOT_FOUND";
    pub const IDENTITY_INVALID: &str = "IDENTITY_INVALID";
    pub const IDENTITY_ROLE_INVALID: &str = "IDENTITY_ROLE_INVALID";
    pub const IDENTITY_PROJECT_MISMATCH: &str = "IDENTITY_PROJECT_MISMATCH";
    pub const IDENTITY_CONFLICT: &str = "IDENTITY_CONFLICT";
    pub const IDENTITY_RETIRED: &str = "IDENTITY_RETIRED";
    pub const IDENTITY_CAP_EXCEEDED: &str = "IDENTITY_CAP_EXCEEDED";
    pub const IDENTITY_IMPORTED_READONLY: &str = "IDENTITY_IMPORTED_READONLY";
    pub const IDENTITY_POLICY_INVALID: &str = "IDENTITY_POLICY_INVALID";
}

/// Typed identity failure. It carries only a stable code and a bounded message;
/// prompts, project source text, and secrets never enter it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentIdentityError {
    pub code: &'static str,
    pub message: String,
}

impl AgentIdentityError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for AgentIdentityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for AgentIdentityError {}

pub fn agent_identity_role(value: &str) -> Option<&'static str> {
    match value {
        "leader" => Some("leader"),
        "manager" => Some("manager"),
        "worker" => Some("worker"),
        _ => None,
    }
}

pub fn valid_agent_identity_profile(value: &str) -> bool {
    AGENT_IDENTITY_PROFILE_VALUES.contains(&value)
}

fn default_identity_policy_enabled() -> bool {
    false
}

fn default_identity_max_managers() -> u32 {
    4
}

fn default_identity_max_workers() -> u32 {
    10
}

fn default_identity_requires_approval() -> bool {
    true
}

fn default_identity_generation() -> u64 {
    1
}

fn default_identity_origin() -> String {
    "local".to_string()
}

fn default_identity_policy_json() -> Value {
    Value::Object(Default::default())
}

/// Project setting for the project-scoped identity program. The default is
/// deliberately disabled: a project that never opts in keeps the run-scoped
/// roster and the same behavior as before S1.
///
/// S1 only stores and validates this policy. Hire/retire approval workflows
/// and session assignment are separate phases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentIdentityPolicy {
    #[serde(default = "default_identity_policy_enabled")]
    pub enabled: bool,
    #[serde(default = "default_identity_max_managers")]
    pub max_managers: u32,
    #[serde(default = "default_identity_max_workers")]
    pub max_workers: u32,
    #[serde(default = "default_identity_requires_approval")]
    pub hire_requires_approval: bool,
    #[serde(default = "default_identity_requires_approval")]
    pub retirement_requires_approval: bool,
}

impl Default for AgentIdentityPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            max_managers: default_identity_max_managers(),
            max_workers: default_identity_max_workers(),
            hire_requires_approval: true,
            retirement_requires_approval: true,
        }
    }
}

impl AgentIdentityPolicy {
    /// Validate and return a normalized copy. Unknown values fail closed rather
    /// than silently widening the caps.
    pub fn normalized(&self) -> std::result::Result<Self, String> {
        if self.max_managers > AGENT_IDENTITY_MAX_MANAGERS_HARD_CAP {
            return Err(format!(
                "max_managers must be between 0 and {}",
                AGENT_IDENTITY_MAX_MANAGERS_HARD_CAP
            ));
        }
        if self.max_workers > AGENT_IDENTITY_MAX_WORKERS_HARD_CAP {
            return Err(format!(
                "max_workers must be between 0 and {}",
                AGENT_IDENTITY_MAX_WORKERS_HARD_CAP
            ));
        }
        Ok(self.clone())
    }
}

/// One durable project-scoped employee record. It is not a process, a lease, or
/// a credential; a run may reference it later but execution policy is unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentIdentity {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub project_id: String,
    /// `leader`, `manager`, or `worker`. Labour is never a project identity.
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub department: Option<String>,
    #[serde(default)]
    pub component: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    /// Explicit model reference. It is metadata only; the user-configured role
    /// model bindings still decide every provider call.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub parent_identity_id: Option<String>,
    /// `active`, `idle`, `paused`, or `retired`. `idle` is reserved for S2.
    #[serde(default)]
    pub status: String,
    /// Increments once per role/department/component/title/profile/model change.
    #[serde(default = "default_identity_generation")]
    pub generation: u64,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub retired_at: Option<String>,
    #[serde(default)]
    pub successor_id: Option<String>,
    /// Bounded, non-secret identity metadata. Defaults to `{}`.
    #[serde(default = "default_identity_policy_json")]
    pub policy: Value,
    /// `local` or `imported`. Imported identities are read-only until an
    /// explicit rehome phase assigns them to a local project.
    #[serde(default = "default_identity_origin")]
    pub origin: String,
}

impl Default for AgentIdentity {
    /// Defaults exist for forward-compatible decoding of partial payloads.
    /// They are not a valid stored row; the store always writes every field.
    fn default() -> Self {
        Self {
            id: String::new(),
            project_id: String::new(),
            role: String::new(),
            department: None,
            component: None,
            title: None,
            profile: None,
            model: None,
            parent_identity_id: None,
            status: "active".to_string(),
            generation: 1,
            created_at: String::new(),
            updated_at: String::new(),
            retired_at: None,
            successor_id: None,
            policy: default_identity_policy_json(),
            origin: default_identity_origin(),
        }
    }
}

/// One append-only project-scoped identity lifecycle event. Payloads contain a
/// bounded identity snapshot and changed field names, never prompts or secrets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentIdentityEvent {
    #[serde(default)]
    pub identity_id: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub seq: u64,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub payload: Value,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentIdentityListParams {
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentIdentitiesResult {
    #[serde(default)]
    pub identities: Vec<AgentIdentity>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentIdentityParams {
    #[serde(default)]
    pub identity_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentIdentityResult {
    #[serde(default)]
    pub identity: AgentIdentity,
    #[serde(default)]
    pub changed: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateAgentIdentityParams {
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub department: Option<String>,
    #[serde(default)]
    pub component: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub parent_identity_id: Option<String>,
    #[serde(default)]
    pub policy: Option<Value>,
}

/// Distinguish an omitted field (`None`) from an explicit JSON `null`
/// (`Some(None)`) and a value (`Some(Some(value))`).
fn deserialize_present_option<'de, D, T>(
    deserializer: D,
) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateAgentIdentityParams {
    #[serde(default)]
    pub identity_id: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub department: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub component: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub title: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub profile: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub model: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub parent_identity_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub successor_id: Option<Option<String>>,
    #[serde(default)]
    pub policy: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PauseAgentIdentityParams {
    #[serde(default)]
    pub identity_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResumeAgentIdentityParams {
    #[serde(default)]
    pub identity_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RetireAgentIdentityParams {
    #[serde(default)]
    pub identity_id: String,
    #[serde(default)]
    pub successor_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentIdentityPolicyParams {
    #[serde(default)]
    pub project_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetAgentIdentityPolicyParams {
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub policy: AgentIdentityPolicy,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentIdentityPolicyResult {
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub policy: AgentIdentityPolicy,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentIdentityReplayParams {
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub repair: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentIdentityReplayReport {
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub identities: u64,
    #[serde(default)]
    pub events: u64,
    #[serde(default)]
    pub consistent: bool,
    #[serde(default)]
    pub repaired: bool,
    #[serde(default)]
    pub mismatches: Vec<String>,
}

// ---------------------------------------------------------------------------
// Session assignment, identity lease, and project agent inbox (S2)
// ---------------------------------------------------------------------------

/// Assignment row states. `queued` is reserved for a later portfolio phase and
/// is never produced by S2; the only S2 transitions are `active -> released`.
pub const AGENT_ASSIGNMENT_STATUSES: [&str; 3] = ["active", "released", "queued"];

pub const AGENT_ASSIGNMENT_CREATED_EVENT: &str = "agent.assignment.created";
pub const AGENT_ASSIGNMENT_RENEWED_EVENT: &str = "agent.assignment.renewed";
pub const AGENT_ASSIGNMENT_RELEASED_EVENT: &str = "agent.assignment.released";
pub const AGENT_ASSIGNMENT_EXPIRED_EVENT: &str = "agent.assignment.expired";

pub const AGENT_ASSIGNMENT_EVENT_KINDS: [&str; 4] = [
    AGENT_ASSIGNMENT_CREATED_EVENT,
    AGENT_ASSIGNMENT_RENEWED_EVENT,
    AGENT_ASSIGNMENT_RELEASED_EVENT,
    AGENT_ASSIGNMENT_EXPIRED_EVENT,
];

/// Default identity-lease duration: ten minutes, renewed by an explicit
/// heartbeat. The range is deliberately narrow; an assignment is not an
/// unkillable process and a stale lease must not pin an identity for a day.
pub const AGENT_ASSIGNMENT_DEFAULT_LEASE_SECS: u64 = 600;
pub const AGENT_ASSIGNMENT_MIN_LEASE_SECS: u64 = 60;
pub const AGENT_ASSIGNMENT_MAX_LEASE_SECS: u64 = 86_400;

/// Hard row cap for one assignment listing.
pub const AGENT_ASSIGNMENT_MAX_LIST: u32 = 500;

/// Inbox kinds. Labour has no project inbox; these kinds are for durable
/// Leader/Manager/Worker identities only.
pub const AGENT_INBOX_KINDS: [&str; 6] = [
    "task", "message", "question", "handoff", "decision", "review",
];

pub const AGENT_INBOX_STATUSES: [&str; 4] = ["unread", "read", "done", "expired"];

pub const AGENT_INBOX_MESSAGE_EVENT: &str = "agent.inbox.message";
pub const AGENT_INBOX_READ_EVENT: &str = "agent.inbox.read";
pub const AGENT_INBOX_DONE_EVENT: &str = "agent.inbox.done";
pub const AGENT_INBOX_EXPIRED_EVENT: &str = "agent.inbox.expired";

pub const AGENT_INBOX_EVENT_KINDS: [&str; 4] = [
    AGENT_INBOX_MESSAGE_EVENT,
    AGENT_INBOX_READ_EVENT,
    AGENT_INBOX_DONE_EVENT,
    AGENT_INBOX_EXPIRED_EVENT,
];

/// Bounded structured payload. There is no prompt text, file blob, or
/// credential store here; the core and store surfaces reject oversized or
/// obviously sensitive key names before persisting.
pub const AGENT_INBOX_MAX_PAYLOAD_BYTES: usize = 16_384;
pub const AGENT_INBOX_MAX_BODY_CHARS: usize = 4_000;
pub const AGENT_INBOX_MAX_LIST: u32 = 500;
pub const AGENT_INBOX_MIN_TTL_SECS: u64 = 60;
pub const AGENT_INBOX_MAX_TTL_SECS: u64 = 30 * 24 * 60 * 60;
/// Hard cap for one project coordination replay read.
pub const AGENT_COORDINATION_MAX_EVENTS: u32 = 100_000;
/// Maximum number of characters of one message summary that may enter a wake
/// continuation context note.
pub const AGENT_INBOX_MAX_SUMMARY_CHARS: usize = 200;

/// Stable, bounded coordination error codes. These are wire values, not
/// message-text classifications.
pub mod agent_coordination_codes {
    pub const ASSIGNMENT_NOT_FOUND: &str = "ASSIGNMENT_NOT_FOUND";
    pub const ASSIGNMENT_INVALID: &str = "ASSIGNMENT_INVALID";
    pub const ASSIGNMENT_CONFLICT: &str = "ASSIGNMENT_CONFLICT";
    pub const ASSIGNMENT_PROJECT_MISMATCH: &str = "ASSIGNMENT_PROJECT_MISMATCH";
    pub const ASSIGNMENT_ROLE_MISMATCH: &str = "ASSIGNMENT_ROLE_MISMATCH";
    pub const ASSIGNMENT_IDENTITY_RETIRED: &str = "ASSIGNMENT_IDENTITY_RETIRED";
    pub const ASSIGNMENT_IDENTITY_UNAVAILABLE: &str = "ASSIGNMENT_IDENTITY_UNAVAILABLE";
    pub const ASSIGNMENT_LEASE_EXPIRED: &str = "ASSIGNMENT_LEASE_EXPIRED";
    pub const ASSIGNMENT_POLICY_DISABLED: &str = "ASSIGNMENT_POLICY_DISABLED";
    pub const INBOX_NOT_FOUND: &str = "INBOX_NOT_FOUND";
    pub const INBOX_INVALID: &str = "INBOX_INVALID";
    pub const INBOX_CONFLICT: &str = "INBOX_CONFLICT";
    pub const INBOX_PROJECT_MISMATCH: &str = "INBOX_PROJECT_MISMATCH";
    pub const INBOX_RECIPIENT_INVALID: &str = "INBOX_RECIPIENT_INVALID";
    pub const INBOX_SENDER_INVALID: &str = "INBOX_SENDER_INVALID";
    pub const INBOX_RECIPIENT_RETIRED: &str = "INBOX_RECIPIENT_RETIRED";
    pub const INBOX_EXPIRED: &str = "INBOX_EXPIRED";
    pub const INBOX_PAYLOAD_INVALID: &str = "INBOX_PAYLOAD_INVALID";
    pub const COORDINATION_REPLAY_INVALID: &str = "COORDINATION_REPLAY_INVALID";
    pub const INVALID_COORDINATION_EVENT: &str = "INVALID_COORDINATION_EVENT";
}

/// Typed coordination failure. It carries only a stable code and a bounded
/// message; payload text and secrets never enter it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCoordinationError {
    pub code: &'static str,
    pub message: String,
}

impl AgentCoordinationError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for AgentCoordinationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for AgentCoordinationError {}

/// Validate and normalize an identity-lease duration. `None` uses the
/// ten-minute default; explicit values must be in `60..=86400`.
pub fn normalize_assignment_lease_secs(
    value: Option<u64>,
) -> std::result::Result<u64, AgentCoordinationError> {
    let value = value.unwrap_or(AGENT_ASSIGNMENT_DEFAULT_LEASE_SECS);
    if !(AGENT_ASSIGNMENT_MIN_LEASE_SECS..=AGENT_ASSIGNMENT_MAX_LEASE_SECS).contains(&value) {
        return Err(AgentCoordinationError::new(
            agent_coordination_codes::ASSIGNMENT_INVALID,
            format!(
                "lease_secs must be {}..={}",
                AGENT_ASSIGNMENT_MIN_LEASE_SECS, AGENT_ASSIGNMENT_MAX_LEASE_SECS
            ),
        ));
    }
    Ok(value)
}

/// Validate an optional inbox TTL. `None` means the message does not expire by
/// time; explicit values are bounded to `60..=2,592,000` seconds.
pub fn normalize_inbox_ttl_secs(
    value: Option<u64>,
) -> std::result::Result<Option<u64>, AgentCoordinationError> {
    match value {
        None => Ok(None),
        Some(seconds)
            if (AGENT_INBOX_MIN_TTL_SECS..=AGENT_INBOX_MAX_TTL_SECS).contains(&seconds) =>
        {
            Ok(Some(seconds))
        }
        Some(_) => Err(AgentCoordinationError::new(
            agent_coordination_codes::INBOX_INVALID,
            format!(
                "expires_in_secs must be {}..={}",
                AGENT_INBOX_MIN_TTL_SECS, AGENT_INBOX_MAX_TTL_SECS
            ),
        )),
    }
}

fn inbox_key_is_sensitive(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "apikey"
            | "accesstoken"
            | "refreshtoken"
            | "token"
            | "password"
            | "passphrase"
            | "secret"
            | "clientsecret"
            | "privatekey"
            | "authorization"
            | "credential"
            | "credentials"
            | "bearer"
            | "cookie"
            | "sessiontoken"
    )
}

fn inbox_payload_has_sensitive_key(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            inbox_key_is_sensitive(key) || inbox_payload_has_sensitive_key(value)
        }),
        Value::Array(items) => items.iter().any(inbox_payload_has_sensitive_key),
        _ => false,
    }
}

/// Validate one inbox payload. The payload must be a JSON object, fit the
/// bounded size, and must not use an obvious credential key at any depth. This
/// is a defensive shape check, not a content classifier: callers remain
/// responsible for never sending secrets.
pub fn validate_agent_inbox_payload(
    payload: &Value,
) -> std::result::Result<(), AgentCoordinationError> {
    if !payload.is_object() {
        return Err(AgentCoordinationError::new(
            agent_coordination_codes::INBOX_PAYLOAD_INVALID,
            "inbox payload must be a JSON object",
        ));
    }
    let encoded = serde_json::to_string(payload).map_err(|_| {
        AgentCoordinationError::new(
            agent_coordination_codes::INBOX_PAYLOAD_INVALID,
            "inbox payload is not serializable",
        )
    })?;
    if encoded.len() > AGENT_INBOX_MAX_PAYLOAD_BYTES {
        return Err(AgentCoordinationError::new(
            agent_coordination_codes::INBOX_PAYLOAD_INVALID,
            format!(
                "inbox payload exceeds {} bytes",
                AGENT_INBOX_MAX_PAYLOAD_BYTES
            ),
        ));
    }
    if inbox_payload_has_sensitive_key(payload) {
        return Err(AgentCoordinationError::new(
            agent_coordination_codes::INBOX_PAYLOAD_INVALID,
            "inbox payload uses a credential-like key; send a label and use Keychain for secrets",
        ));
    }
    Ok(())
}

/// One assignment of a project identity to a session. The lease token is
/// deliberately absent: it is an internal fencing value and never enters the
/// wire, an event, a bundle, a CLI line, or the UI.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentAssignment {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub identity_id: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub assigned_at: String,
    #[serde(default)]
    pub released_at: Option<String>,
    #[serde(default)]
    pub heartbeat_at: String,
    #[serde(default)]
    pub lease_expires_at: Option<String>,
    /// Derived, time-relative convenience for read surfaces. Events and stored
    /// rows always carry the authoritative status and expiry.
    #[serde(default)]
    pub lease_active: bool,
}

/// One durable project-scoped inbox row. Delivery is pull-based and this is not
/// a task-scoped Labour inbox.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentInboxMessage {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub identity_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub from_identity_id: Option<String>,
    /// `operator` for an operator-authored row, otherwise the sender identity
    /// id. It is a label, not a credential.
    #[serde(default)]
    pub from_label: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default = "default_identity_policy_json")]
    pub payload: Value,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub read_at: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInboxCounts {
    #[serde(default)]
    pub unread: u64,
    #[serde(default)]
    pub read: u64,
    #[serde(default)]
    pub done: u64,
    #[serde(default)]
    pub expired: u64,
}

/// One append-only project-scoped coordination event. `seq` is a per-project
/// ordering value; `id` is portable across databases so an archive import can
/// be idempotent without colliding with an unrelated local stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentCoordinationEvent {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub seq: u64,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub payload: Value,
    #[serde(default)]
    pub created_at: String,
}

/// Optional wake outcome for assignment and inbox commands. A wake is a
/// deferred continuation, never a hidden provider call or a held Worker slot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCoordinationWake {
    #[serde(default)]
    pub requested: bool,
    #[serde(default)]
    pub created: bool,
    #[serde(default)]
    pub continuation_id: Option<String>,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionAssignmentsParams {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub identity_id: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionAssignmentsResult {
    #[serde(default)]
    pub assignments: Vec<AgentAssignment>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionAssignParams {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub identity_id: String,
    /// Optional consistency check. When present it must equal the identity role.
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub lease_secs: Option<u64>,
    /// Create a deferred wake continuation when the session is idle/failed/
    /// cancelled. Never blocks and never holds a Worker slot.
    #[serde(default)]
    pub wake_session: bool,
    /// Bounded operator label for the audit event.
    #[serde(default)]
    pub operator: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionAssignResult {
    #[serde(default)]
    pub assignment: AgentAssignment,
    #[serde(default)]
    pub created: bool,
    #[serde(default)]
    pub wake: AgentCoordinationWake,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionAssignmentRenewParams {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub identity_id: String,
    #[serde(default)]
    pub lease_secs: Option<u64>,
    #[serde(default)]
    pub operator: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionAssignmentRenewResult {
    #[serde(default)]
    pub assignment: AgentAssignment,
    #[serde(default)]
    pub renewed: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionReleaseParams {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub identity_id: String,
    /// Operator-initiated releases are still scoped to the owning session; this
    /// flag is audit metadata, not an authentication claim.
    #[serde(default)]
    pub operator: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionReleaseResult {
    #[serde(default)]
    pub assignment: AgentAssignment,
    #[serde(default)]
    pub released: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentInboxSendParams {
    /// Optional; derived from the recipient identity when absent and required
    /// to match when present.
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub identity_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    /// Sender identity. Omit for the stable `operator` sender.
    #[serde(default)]
    pub from_identity_id: Option<String>,
    /// task, message, question, handoff, decision, or review.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub payload: Option<Value>,
    /// Convenience text; becomes `{"body": ...}` when no structured payload is
    /// supplied. Never a secret store.
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub expires_in_secs: Option<u64>,
    #[serde(default)]
    pub wake_session: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentInboxSendResult {
    #[serde(default)]
    pub message: AgentInboxMessage,
    #[serde(default)]
    pub wake: AgentCoordinationWake,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentInboxListParams {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub identity_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub unread_only: bool,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentInboxListResult {
    #[serde(default)]
    pub messages: Vec<AgentInboxMessage>,
    #[serde(default)]
    pub counts: AgentInboxCounts,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentInboxMessageParams {
    #[serde(default)]
    pub message_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub identity_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentInboxReadResult {
    #[serde(default)]
    pub message: AgentInboxMessage,
    #[serde(default)]
    pub changed: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentCoordinationReplayParams {
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub repair: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentCoordinationReplayReport {
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub assignments: u64,
    #[serde(default)]
    pub inbox: u64,
    #[serde(default)]
    pub events: u64,
    #[serde(default)]
    pub consistent: bool,
    #[serde(default)]
    pub repaired: bool,
    #[serde(default)]
    pub mismatches: Vec<String>,
}

// ---------------------------------------------------------------------------
// Worker workspaces and two-level integration (S4)
// ---------------------------------------------------------------------------

/// A workspace is the durable Workowning checkout for one project identity
/// component. `active` workspaces may be attached to one session at a time;
/// `archived` and `retired` rows are evidence and are never deleted.
pub const AGENT_WORKSPACE_STATUSES: [&str; 3] = ["active", "archived", "retired"];

/// Session binding states. One active binding per `(session_id, identity_id)`.
pub const AGENT_SESSION_WORKSPACE_STATUSES: [&str; 3] = ["active", "released", "archived"];

pub const AGENT_WORKSPACE_CREATED_EVENT: &str = "agent.workspace.created";
pub const AGENT_WORKSPACE_SESSION_ATTACHED_EVENT: &str = "agent.workspace.session_attached";
pub const AGENT_WORKSPACE_LABOUR_MERGED_EVENT: &str = "agent.workspace.labour_merged";
pub const AGENT_WORKSPACE_LANDED_EVENT: &str = "agent.workspace.landed";
pub const AGENT_WORKSPACE_ARCHIVED_EVENT: &str = "agent.workspace.archived";
pub const AGENT_WORKSPACE_RETIRED_EVENT: &str = "agent.workspace.retired";
pub const AGENT_WORKSPACE_RELEASED_EVENT: &str = "agent.workspace.released";
pub const AGENT_WORKSPACE_LEASE_EXPIRED_EVENT: &str = "agent.workspace.lease_expired";

pub const AGENT_WORKSPACE_EVENT_KINDS: [&str; 8] = [
    AGENT_WORKSPACE_CREATED_EVENT,
    AGENT_WORKSPACE_SESSION_ATTACHED_EVENT,
    AGENT_WORKSPACE_LABOUR_MERGED_EVENT,
    AGENT_WORKSPACE_LANDED_EVENT,
    AGENT_WORKSPACE_ARCHIVED_EVENT,
    AGENT_WORKSPACE_RETIRED_EVENT,
    AGENT_WORKSPACE_RELEASED_EVENT,
    AGENT_WORKSPACE_LEASE_EXPIRED_EVENT,
];

/// Default workspace lease: ten minutes, renewed by an explicit operator or
/// engine action. A lease is a same-database coordination claim, not a
/// distributed lock or a security credential.
pub const AGENT_WORKSPACE_DEFAULT_LEASE_SECS: u64 = 600;
pub const AGENT_WORKSPACE_MIN_LEASE_SECS: u64 = 60;
pub const AGENT_WORKSPACE_MAX_LEASE_SECS: u64 = 86_400;
pub const AGENT_WORKSPACE_MAX_LIST: u32 = 500;
pub const AGENT_WORKSPACE_MAX_COMPONENT_CHARS: usize = 120;
pub const AGENT_WORKSPACE_MAX_REASON_CHARS: usize = 500;
pub const AGENT_WORKSPACE_MAX_PATHS: usize = 200;

/// Durable component branch namespace. A branch may only be created through
/// the deterministic helper so an operator-supplied string can never become an
/// arbitrary ref, a child of the Leader session ref, or a path traversal.
pub const AGENT_WORKSPACE_COMPONENT_BRANCH_PREFIX: &str = "verlinpark/components/";
pub const AGENT_WORKSPACE_SESSION_BRANCH_PREFIX: &str = "verlinpark/sessions/";

/// Stable, bounded error codes for workspace RPCs. These are wire values, not
/// message-text classifications.
pub mod agent_workspace_codes {
    pub const WORKSPACE_NOT_FOUND: &str = "WORKSPACE_NOT_FOUND";
    pub const WORKSPACE_INVALID: &str = "WORKSPACE_INVALID";
    pub const WORKSPACE_BUSY: &str = "WORKSPACE_BUSY";
    pub const WORKSPACE_PROJECT_MISMATCH: &str = "WORKSPACE_PROJECT_MISMATCH";
    pub const WORKSPACE_IDENTITY_INVALID: &str = "WORKSPACE_IDENTITY_INVALID";
    pub const WORKSPACE_IDENTITY_RETIRED: &str = "WORKSPACE_IDENTITY_RETIRED";
    pub const WORKSPACE_ASSIGNMENT_REQUIRED: &str = "WORKSPACE_ASSIGNMENT_REQUIRED";
    pub const WORKSPACE_POLICY_DISABLED: &str = "WORKSPACE_POLICY_DISABLED";
    pub const WORKSPACE_IMPORTED_READONLY: &str = "WORKSPACE_IMPORTED_READONLY";
    pub const WORKSPACE_CONFLICT: &str = "WORKSPACE_CONFLICT";
    pub const WORKSPACE_NOT_QUIESCENT: &str = "WORKSPACE_NOT_QUIESCENT";
    pub const WORKSPACE_NOT_ACCEPTED: &str = "WORKSPACE_NOT_ACCEPTED";
    pub const WORKSPACE_NOT_ACTIVE: &str = "WORKSPACE_NOT_ACTIVE";
    pub const WORKSPACE_LAND_CONFLICT: &str = "WORKSPACE_LAND_CONFLICT";
    pub const WORKSPACE_LAND_CHECK_FAILED: &str = "WORKSPACE_LAND_CHECK_FAILED";
    pub const WORKSPACE_CONFIRMATION_REQUIRED: &str = "WORKSPACE_CONFIRMATION_REQUIRED";
    pub const WORKSPACE_LEASE_EXPIRED: &str = "WORKSPACE_LEASE_EXPIRED";
    pub const WORKSPACE_BRANCH_INVALID: &str = "WORKSPACE_BRANCH_INVALID";
    pub const WORKSPACE_WORKTREE_INVALID: &str = "WORKSPACE_WORKTREE_INVALID";
    pub const WORKSPACE_INVALID_EVENT: &str = "WORKSPACE_INVALID_EVENT";
}

/// Typed workspace failure. It carries only a stable code and a bounded message;
/// prompt text, file content, and secrets never enter it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentWorkspaceError {
    pub code: &'static str,
    pub message: String,
}

impl AgentWorkspaceError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for AgentWorkspaceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for AgentWorkspaceError {}

/// One durable project-scoped component workspace. The lease token is an
/// internal fencing value and is never serialized on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentWorkspace {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub identity_id: String,
    #[serde(default)]
    pub component: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub base_commit: String,
    #[serde(default)]
    pub worktree_path: String,
    /// `active`, `archived`, or `retired`.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub last_session_id: Option<String>,
    /// Derived, time-relative convenience for read surfaces. Stored rows carry
    /// the authoritative expiry; the lease token never leaves the store.
    #[serde(default)]
    pub lease_active: bool,
    #[serde(default)]
    pub lease_expires_at: Option<String>,
    /// `local` or `imported`. Imported workspaces are read-only evidence and
    /// can never be attached, merged into, landed, or re-leased locally.
    #[serde(default = "default_identity_origin")]
    pub origin: String,
}

impl Default for AgentWorkspace {
    fn default() -> Self {
        Self {
            id: String::new(),
            project_id: String::new(),
            identity_id: String::new(),
            component: String::new(),
            branch: String::new(),
            base_commit: String::new(),
            worktree_path: String::new(),
            status: "active".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            last_session_id: None,
            lease_active: false,
            lease_expires_at: None,
            origin: "local".to_string(),
        }
    }
}

/// One session attachment of a project workspace. The binding owns the session
/// branch and worktree that a Worker run uses; it is evidence after release and
/// is never silently reassigned to another workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionWorkerWorkspace {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub identity_id: String,
    #[serde(default)]
    pub worktree_path: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub base_commit: String,
    /// `active`, `released`, or `archived`.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub lease_active: bool,
    #[serde(default)]
    pub lease_expires_at: Option<String>,
}

impl Default for SessionWorkerWorkspace {
    fn default() -> Self {
        Self {
            id: String::new(),
            workspace_id: String::new(),
            session_id: String::new(),
            identity_id: String::new(),
            worktree_path: String::new(),
            branch: String::new(),
            base_commit: String::new(),
            status: "active".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            lease_active: false,
            lease_expires_at: None,
        }
    }
}

/// One append-only project-scoped workspace event. `seq` orders replay within
/// one project; `id` is portable across databases so archive import is
/// idempotent without colliding with an unrelated local stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentWorkspaceEvent {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub seq: u64,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub payload: Value,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentWorkspaceListParams {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub identity_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentWorkspaceListResult {
    #[serde(default)]
    pub workspaces: Vec<AgentWorkspace>,
    #[serde(default)]
    pub sessions: Vec<SessionWorkerWorkspace>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentWorkspaceParams {
    #[serde(default)]
    pub workspace_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentWorkspaceGetResult {
    #[serde(default)]
    pub workspace: AgentWorkspace,
    #[serde(default)]
    pub sessions: Vec<SessionWorkerWorkspace>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentWorkspaceEnsureParams {
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub identity_id: String,
    #[serde(default)]
    pub component: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub lease_secs: Option<u64>,
    #[serde(default)]
    pub operator: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentWorkspaceEnsureResult {
    #[serde(default)]
    pub workspace: AgentWorkspace,
    #[serde(default)]
    pub session: Option<SessionWorkerWorkspace>,
    #[serde(default)]
    pub created: bool,
    #[serde(default)]
    pub attached: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentWorkspaceLandParams {
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub operator: Option<String>,
    /// Explicit operator confirmation. A missing/false value fails closed.
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentWorkspaceLandResult {
    #[serde(default)]
    pub workspace: AgentWorkspace,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub previous_component_commit: String,
    #[serde(default)]
    pub landed_commit: String,
    #[serde(default)]
    pub already_up_to_date: bool,
    #[serde(default)]
    pub commits: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentWorkspaceArchiveParams {
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub operator: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentWorkspaceRetireParams {
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub operator: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentWorkspaceResult {
    #[serde(default)]
    pub workspace: AgentWorkspace,
    #[serde(default)]
    pub changed: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionWorkspaceParams {
    #[serde(default)]
    pub session_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionWorkspaceResult {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub bindings: Vec<SessionWorkerWorkspace>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentWorkspaceReplayParams {
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub repair: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentWorkspaceReplayReport {
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub workspaces: u64,
    #[serde(default)]
    pub sessions: u64,
    #[serde(default)]
    pub events: u64,
    #[serde(default)]
    pub consistent: bool,
    #[serde(default)]
    pub repaired: bool,
    #[serde(default)]
    pub mismatches: Vec<String>,
}

/// Validate and normalize a component label. Components are single-line labels,
/// never paths or refs; branch/path construction always goes through
/// [`component_branch_name`] and [`workspace_ref_slug`].
pub fn normalize_workspace_component(
    value: &str,
) -> std::result::Result<String, AgentWorkspaceError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > AGENT_WORKSPACE_MAX_COMPONENT_CHARS {
        return Err(AgentWorkspaceError::new(
            agent_workspace_codes::WORKSPACE_INVALID,
            format!(
                "component must be 1-{} characters",
                AGENT_WORKSPACE_MAX_COMPONENT_CHARS
            ),
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(AgentWorkspaceError::new(
            agent_workspace_codes::WORKSPACE_INVALID,
            "component must not contain control characters",
        ));
    }
    if value.contains('/')
        || value.contains('\\')
        || value == "."
        || value == ".."
        || value.contains("..")
    {
        return Err(AgentWorkspaceError::new(
            agent_workspace_codes::WORKSPACE_INVALID,
            "component must be a label, not a path or ref",
        ));
    }
    Ok(value.to_string())
}

/// Validate an optional workspace lease; defaults to ten minutes.
pub fn normalize_workspace_lease_secs(
    value: Option<u64>,
) -> std::result::Result<u64, AgentWorkspaceError> {
    let value = value.unwrap_or(AGENT_WORKSPACE_DEFAULT_LEASE_SECS);
    if !(AGENT_WORKSPACE_MIN_LEASE_SECS..=AGENT_WORKSPACE_MAX_LEASE_SECS).contains(&value) {
        return Err(AgentWorkspaceError::new(
            agent_workspace_codes::WORKSPACE_INVALID,
            format!(
                "lease_secs must be {}..={}",
                AGENT_WORKSPACE_MIN_LEASE_SECS, AGENT_WORKSPACE_MAX_LEASE_SECS
            ),
        ));
    }
    Ok(value)
}

/// Deterministic, bounded, ref/path-safe slug. Non-ASCII and punctuation
/// collapse to `-`; a stable FNV-1a suffix keeps two long labels from silently
/// sharing one branch or directory.
pub fn workspace_ref_slug(value: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in value.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() || lower == '.' || lower == '_' {
            slug.push(lower);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
        if slug.len() >= 40 {
            break;
        }
    }
    let slug = slug.trim_matches(['-', '.']).to_string();
    let slug = if slug.is_empty() {
        "component".to_string()
    } else {
        slug
    };
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{slug}-{hash:08x}")
}

/// Deterministic component branch for a Worker identity. The identity part
/// must already be a safe path/ref component; callers validate it before this
/// helper so a malformed identity cannot escape the namespace.
pub fn component_branch_name(identity_id: &str, component: &str) -> String {
    format!(
        "{}{}--{}",
        AGENT_WORKSPACE_COMPONENT_BRANCH_PREFIX,
        workspace_ref_slug(component),
        workspace_ref_slug(identity_id)
    )
}

/// Deterministic session worker branch. It is intentionally in a separate ref
/// namespace from both the component branch and `verlinpark/<session-id>` so
/// no branch can be both a file and a directory.
pub fn session_workspace_branch_name(session_id: &str, identity_id: &str) -> String {
    format!(
        "{}worker-{}-{}",
        AGENT_WORKSPACE_SESSION_BRANCH_PREFIX,
        workspace_ref_slug(session_id),
        workspace_ref_slug(identity_id)
    )
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlanDepartment {
    pub name: String,
    pub manager_title: String,
    pub profile: String,
    pub objective: String,
    #[serde(default)]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub cost_budget_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlanTask {
    pub id: String,
    pub department: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub kind: String,
    /// Higher values run first within a ready batch. Range -100..100.
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub write_scope: Vec<String>,
    #[serde(default)]
    pub acceptance: Vec<String>,
    #[serde(default)]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub cost_budget_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TeamPlan {
    pub goal: String,
    pub departments: Vec<PlanDepartment>,
    pub tasks: Vec<PlanTask>,
}

/// A persistent task-DAG node owned by the daemon scheduler. The plan remains
/// the source of truth for task definitions; this row carries live scheduling
/// state across plan runs and daemon restarts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledTask {
    pub session_id: String,
    pub project_id: String,
    pub plan_seq: u64,
    pub task_id: String,
    pub department: String,
    pub title: String,
    pub priority: i32,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub write_scope: Vec<String>,
    #[serde(default)]
    pub attempts: u32,
    pub enqueued_at: String,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    /// Earliest time an auto-retry should be dispatched. `None` means ready now.
    #[serde(default)]
    pub next_attempt_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteLease {
    pub project_id: String,
    pub session_id: String,
    pub agent_id: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    pub acquired_at: String,
}

/// Runtime progress telemetry for a task-scoped agent run. It is deliberately
/// separate from `ScheduledTask`: the schedule row is reconstructed from plan
/// lifecycle events, while these observations are refreshed by live turn/tool
/// boundaries and by the stall sweep.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanTaskProgress {
    pub session_id: String,
    pub project_id: String,
    pub plan_seq: u64,
    pub task_id: String,
    #[serde(default)]
    pub last_progress_at: Option<String>,
    #[serde(default)]
    pub stall_count: u32,
    #[serde(default)]
    pub last_turn: Option<u32>,
    #[serde(default)]
    pub last_tool: Option<String>,
    pub updated_at: String,
}

/// One observability-only stall observation. The sweep never cancels or
/// retries a running task; operators decide what to do with the signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskStall {
    pub session_id: String,
    pub project_id: String,
    pub plan_seq: u64,
    pub task_id: String,
    pub department: String,
    pub title: String,
    pub status: String,
    pub attempts: u32,
    pub age_secs: u64,
    pub stall_count: u32,
    pub last_progress_at: String,
    #[serde(default)]
    pub last_turn: Option<u32>,
    #[serde(default)]
    pub last_tool: Option<String>,
    pub observed_at: String,
}

/// Persisted, event-backed stall recovery intent and outcome for one plan
/// task revision. A row is created only after the project policy opts into
/// `escalate` or `retry`; `observe` never writes one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestrationRecovery {
    pub session_id: String,
    pub project_id: String,
    pub plan_seq: u64,
    pub task_id: String,
    pub department: String,
    /// `escalate` or `retry`.
    pub action: String,
    /// `requested`, `waiting_cancel`, `retried`, `escalated`, `failed`, or
    /// `unnecessary`.
    pub status: String,
    /// Automatic recovery retries issued for this task revision.
    pub attempts: u32,
    /// Stall observation count that triggered the latest transition.
    pub last_stall_count: u32,
    pub requested_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub last_error: Option<String>,
    /// Deadline for cooperative cancellation and session quiescence. `None`
    /// once the recovery reaches a terminal state.
    #[serde(default)]
    pub next_action_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrchestrationRecoveriesParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrchestrationRecoveriesResult {
    pub session_id: String,
    #[serde(default)]
    pub recoveries: Vec<OrchestrationRecovery>,
}

/// Persisted operator steering addressed to one running agent/run. Delivery
/// is cooperative at the next turn boundary; it is never a hidden prompt
/// injection and it never broadcasts to every agent implicitly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteeringMessage {
    pub id: String,
    pub session_id: String,
    pub agent_id: String,
    #[serde(default)]
    pub task_id: Option<String>,
    pub body: String,
    pub status: String,
    pub created_at: String,
    #[serde(default)]
    pub delivered_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionSteerParams {
    pub session_id: String,
    /// Operator text. `text` is accepted as a compatibility alias.
    #[serde(alias = "text")]
    pub body: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    /// Optional caller-supplied idempotency key. Repeating the same id with
    /// the same payload replays the first steering result.
    #[serde(default)]
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionSteeringParams {
    pub session_id: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionSteeringCancelParams {
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSteerResult {
    pub steering: SteeringMessage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSteeringListResult {
    pub session_id: String,
    pub steering: Vec<SteeringMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSteeringCancelResult {
    pub cancelled: bool,
    pub steering: SteeringMessage,
}

/// Compact per-task outcome captured when a deferred continuation wakes. It
/// is frozen at wake time so later plan revisions cannot rewrite the prompt
/// context that justified the wake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuationOutcome {
    pub task_id: String,
    #[serde(default)]
    pub title: String,
    pub status: String,
}

/// A deferred operator continuation. It is session-scoped, replaces no plan
/// dependency, and only starts a normal run through the usual `start` path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionContinuation {
    pub id: String,
    pub session_id: String,
    pub project_id: String,
    pub body: String,
    #[serde(default)]
    pub watch_task_ids: Vec<String>,
    #[serde(default = "default_continuation_watch_mode")]
    pub watch_mode: String,
    /// External gate this continuation waits on. At most one continuation may
    /// watch a gate while it is open. The gate token is never stored here.
    #[serde(default)]
    pub watch_gate_id: Option<String>,
    #[serde(default)]
    pub deadline_at: Option<String>,
    pub status: String,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default = "default_continuation_max_attempts")]
    pub max_attempts: u32,
    /// `watch` or `deadline`, set when the row becomes ready.
    #[serde(default)]
    pub wake_reason: Option<String>,
    #[serde(default)]
    pub outcomes: Vec<ContinuationOutcome>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub next_attempt_at: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub ready_at: Option<String>,
    #[serde(default)]
    pub delivered_at: Option<String>,
    #[serde(default)]
    pub updated_at: String,
    /// Stable id shared by every run in a recurrence series. Empty means the
    /// row predates recurrence and behaves as a one-shot whose series is its
    /// own id.
    #[serde(default)]
    pub series_id: String,
    /// Previous run in the series, if this row was created by a delivery.
    #[serde(default)]
    pub parent_continuation_id: Option<String>,
    /// Interval between delivered runs. Absent means one-shot.
    #[serde(default)]
    pub recurrence_every_secs: Option<u64>,
    /// Maximum number of delivered runs in the series, including the first.
    /// Old rows default to one delivered run.
    #[serde(default = "default_continuation_recurrence_max_runs")]
    pub recurrence_max_runs: u32,
    /// Delivered runs already completed before this row. A pending child starts
    /// at the count of its parent, and delivery increments it.
    #[serde(default)]
    pub run_count: u32,
    /// Process owner currently holding the delivery claim, if any. This is
    /// operational ownership metadata, not an agent identity; it is cleared on
    /// delivery, retry, cancellation, or expiry recovery.
    #[serde(default)]
    pub lease_owner: Option<String>,
    /// Expiry of the delivery claim. A live foreign claim must not be touched.
    #[serde(default)]
    pub lease_expires_at: Option<String>,
    /// Number of delivery claims attempted, including reclaimed expired
    /// claims. It is distinct from `attempts`, which counts failed starts.
    #[serde(default)]
    pub delivery_attempt: u32,
}

fn default_continuation_watch_mode() -> String {
    "all".to_string()
}

fn default_continuation_max_attempts() -> u32 {
    3
}

fn default_continuation_recurrence_max_runs() -> u32 {
    1
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionDeferParams {
    pub session_id: String,
    /// Operator prompt. `text` and `prompt` are accepted as compatibility
    /// aliases so older scripts and alternate clients keep working.
    #[serde(alias = "text", alias = "prompt")]
    pub body: String,
    #[serde(default, alias = "watch")]
    pub watch_task_ids: Vec<String>,
    /// Watch an existing external gate instead of (or in addition to) plan
    /// tasks. The continuation wakes when the gate resolves, expires, or is
    /// cancelled. The operator never needs the gate token to defer.
    #[serde(default)]
    pub watch_gate_id: Option<String>,
    #[serde(default)]
    pub watch_mode: Option<String>,
    #[serde(default)]
    pub deadline_at: Option<String>,
    /// Relative deadline in seconds. Mutually exclusive with `deadline_at`.
    #[serde(default)]
    pub after_secs: Option<u64>,
    /// Small bounded start-attempt cap. Defaults to 3.
    #[serde(default)]
    pub max_attempts: Option<u32>,
    /// Repeat the continuation every N seconds after a successful delivery.
    /// Range 60..=2_592_000 (30 days). The first wake still requires a watched
    /// task or a deadline.
    #[serde(default)]
    pub every_secs: Option<u64>,
    /// Maximum delivered runs in the recurrence series, including the first.
    /// Defaults to 10 when `every_secs` is set and must be 2..=100. One-shot
    /// continuations omit it or pass 1.
    #[serde(default)]
    pub max_runs: Option<u32>,
    /// Optional caller-supplied idempotency key. Repeating the same id with
    /// the same payload replays the first continuation instead of creating a
    /// duplicate.
    #[serde(default)]
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionContinuationsParams {
    pub session_id: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionContinuationCancelParams {
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionDeferResult {
    pub continuation: SessionContinuation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionContinuationsResult {
    pub session_id: String,
    pub continuations: Vec<SessionContinuation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionContinuationCancelResult {
    pub cancelled: bool,
    pub continuation: SessionContinuation,
}

/// Token-free visibility into a continuation delivery claim. `live` is derived
/// from expiry at response time; the owner is diagnostic, not a liveness
/// proof.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionContinuationClaimInfo {
    pub status: String,
    #[serde(default)]
    pub owner_id: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub delivery_attempt: u32,
    #[serde(default)]
    pub live: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionContinuationStatusParams {
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionContinuationStatusResult {
    pub continuation: SessionContinuation,
    #[serde(default)]
    pub claim: Option<SessionContinuationClaimInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionContinuationReclaimParams {
    pub id: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionContinuationReclaimResult {
    pub reclaimed: bool,
    #[serde(default)]
    pub continuation: Option<SessionContinuation>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionStallsParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionStallsResult {
    pub session_id: String,
    /// Configured threshold in seconds; 0 means the sweep is disabled.
    pub threshold_secs: u64,
    #[serde(default)]
    pub progress: Vec<PlanTaskProgress>,
    #[serde(default)]
    pub stalls: Vec<TaskStall>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TaskQueueParams {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskQueueResult {
    pub tasks: Vec<ScheduledTask>,
    pub leases: Vec<WriteLease>,
    /// Operational progress observations joined by session/project. Older
    /// clients simply ignore the additive field.
    #[serde(default)]
    pub progress: Vec<PlanTaskProgress>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TaskActionParams {
    pub session_id: String,
    pub task_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IntakeTask {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub instructions: String,
    pub profile: String,
    pub kind: String,
    pub priority: i32,
    #[serde(default)]
    pub write_scope: Vec<String>,
    #[serde(default)]
    pub acceptance: Vec<String>,
    /// Other intake task ids that must complete before this task is runnable.
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub cost_budget_usd: Option<f64>,
    #[serde(default)]
    pub time_budget_secs: Option<u64>,
    pub status: String,
    pub attempts: u32,
    pub max_attempts: u32,
    #[serde(default)]
    pub next_attempt_at: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub result_summary: Option<String>,
    pub origin: String,
    #[serde(default)]
    pub labels: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct IntakeListParams {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IntakeListResult {
    pub tasks: Vec<IntakeTask>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IntakeCreateParams {
    pub project_id: String,
    pub title: String,
    pub instructions: String,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub write_scope: Vec<String>,
    #[serde(default)]
    pub acceptance: Vec<String>,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub cost_budget_usd: Option<f64>,
    #[serde(default)]
    pub time_budget_secs: Option<u64>,
    #[serde(default)]
    pub max_attempts: Option<u32>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub origin: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IntakeUpdateParams {
    pub id: String,
    pub title: String,
    pub instructions: String,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub write_scope: Vec<String>,
    #[serde(default)]
    pub acceptance: Vec<String>,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub cost_budget_usd: Option<f64>,
    #[serde(default)]
    pub time_budget_secs: Option<u64>,
    #[serde(default)]
    pub max_attempts: Option<u32>,
    #[serde(default)]
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IntakeTaskParams {
    pub id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct IntakeActionResult {
    pub task: IntakeTask,
}

#[derive(Debug, Clone, Serialize)]
pub struct IntakeDispatchResult {
    pub started: bool,
    pub task: IntakeTask,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IntakeDeleteResult {
    pub deleted: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TaskDispatchParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskDispatchResult {
    pub started: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskActionResult {
    pub task: ScheduledTask,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TeamPlanParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TeamPlanResult {
    pub plan: Option<TeamPlan>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DependencyFailurePolicy {
    /// Mark dependents blocked with a precise unmet-dependency reason.
    #[default]
    Block,
    /// Cancel dependents when a dependency fails permanently.
    Cancel,
    /// Keep dependents pending so a retry can unblock them.
    Retry,
}

/// What the daemon may do after a plan task produces repeated stall
/// observations. `Observe` is the default and preserves the telemetry-only
/// behavior: no task, session, retry, or coordination state is mutated.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StallRecoveryMode {
    #[default]
    Observe,
    Escalate,
    Retry,
}

/// Project-level orchestration policy. It is intentionally declarative and
/// deterministic: the runtime applies it without model routing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OrchestrationPolicy {
    pub auto_retry_failed: bool,
    pub max_attempts: u32,
    pub retry_backoff_secs: u64,
    pub retry_priority_boost: i32,
    pub dependency_failure: DependencyFailurePolicy,
    /// Record an escalation note once a task exhausts its retry budget.
    pub escalate_after_retries: bool,
    /// Close coordination blockers automatically when their task completes.
    pub auto_resolve_blockers: bool,
    /// What to do after repeated stall observations. The default keeps the
    /// telemetry-only behavior from ADR-0139.
    pub stall_recovery: StallRecoveryMode,
    /// `orchestration.stall` observations required before an escalation or
    /// recovery request is considered. After an earlier recovery transition,
    /// this many additional observations are required for the next one.
    pub stall_min_observations: u32,
    /// Maximum automatic stall-recovery retries per task/plan revision.
    /// `0` disables recovery retries even when `stall_recovery` is `retry`.
    pub stall_max_attempts: u32,
    /// Time a recovery request may wait for cooperative cancellation and for
    /// the session to become idle before it fails and escalates.
    pub stall_grace_secs: u64,
}

impl Default for OrchestrationPolicy {
    fn default() -> Self {
        Self {
            auto_retry_failed: true,
            max_attempts: 2,
            retry_backoff_secs: 30,
            retry_priority_boost: 10,
            dependency_failure: DependencyFailurePolicy::Block,
            escalate_after_retries: true,
            auto_resolve_blockers: true,
            stall_recovery: StallRecoveryMode::Observe,
            stall_min_observations: 2,
            stall_max_attempts: 1,
            stall_grace_secs: 300,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrchestrationPolicyParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetOrchestrationPolicyParams {
    pub project_id: String,
    pub policy: OrchestrationPolicy,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrchestrationPolicyResult {
    pub project_id: String,
    pub policy: OrchestrationPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlanRevisionInfo {
    pub revision: u32,
    pub seq: u64,
    pub kind: String,
    #[serde(default)]
    pub reason: Option<String>,
    pub author: String,
    pub task_count: usize,
    #[serde(default)]
    pub added: Vec<String>,
    #[serde(default)]
    pub removed: Vec<String>,
    #[serde(default)]
    pub changed: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlanRevisionListParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanRevisionListResult {
    pub revisions: Vec<PlanRevisionInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReviseTeamPlanParams {
    pub session_id: String,
    pub plan: TeamPlan,
    pub reason: String,
    #[serde(default)]
    pub expected_revision: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviseTeamPlanResult {
    pub revision: PlanRevisionInfo,
    pub plan: TeamPlan,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlanRevisionGetParams {
    pub session_id: String,
    pub revision: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanRevisionDetailResult {
    pub revision: PlanRevisionInfo,
    pub plan: TeamPlan,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlanRevisionRestoreParams {
    pub session_id: String,
    pub revision: u32,
    pub reason: String,
    #[serde(default)]
    pub expected_revision: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TaskEditParams {
    pub session_id: String,
    pub task: PlanTask,
    pub reason: String,
    #[serde(default)]
    pub expected_revision: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrchestrationTask {
    pub task_id: String,
    pub department: String,
    pub title: String,
    pub kind: String,
    pub priority: i32,
    pub status: String,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub write_scope: Vec<String>,
    #[serde(default)]
    pub acceptance: Vec<String>,
    pub attempts: u32,
    pub max_attempts: u32,
    #[serde(default)]
    pub owner_agent_id: Option<String>,
    #[serde(default)]
    pub owner_agent_name: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub result_summary: Option<String>,
    #[serde(default)]
    pub verifier_status: Option<String>,
    #[serde(default)]
    pub next_attempt_at: Option<String>,
    pub ready: bool,
    #[serde(default)]
    pub blocked_by: Vec<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    #[serde(default)]
    pub last_progress_at: Option<String>,
    #[serde(default)]
    pub stall_count: u32,
    #[serde(default)]
    pub last_turn: Option<u32>,
    #[serde(default)]
    pub last_tool: Option<String>,
    /// True when a running task's last observed progress is older than the
    /// configured stall threshold.
    #[serde(default)]
    pub stalled: bool,
    /// Event-backed stall recovery state for this task revision, when the
    /// project policy opted into escalation or retry.
    #[serde(default)]
    pub recovery: Option<OrchestrationRecovery>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrchestrationDepartment {
    pub name: String,
    pub manager_title: String,
    pub profile: String,
    pub objective: String,
    pub total_tasks: usize,
    pub completed_tasks: usize,
    pub active_tasks: usize,
    pub blocked_tasks: usize,
    pub failed_tasks: usize,
    #[serde(default)]
    pub manager_agent_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrchestrationNote {
    pub note_id: String,
    pub action: String,
    pub severity: String,
    pub status: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub target_agent_id: Option<String>,
    pub agent_id: String,
    pub agent_name: String,
    pub message: String,
    pub created_at: String,
    #[serde(default)]
    pub resolved_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrchestrationDecision {
    pub decision_id: String,
    pub action: String,
    pub task_id: Option<String>,
    pub detail: String,
    pub attempt: u32,
    pub agent_id: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrchestrationBlocker {
    pub kind: String,
    #[serde(default)]
    pub note_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_name: Option<String>,
    pub message: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct OrchestrationMetrics {
    pub total_agents: usize,
    pub running_agents: usize,
    pub queued_agents: usize,
    pub failed_agents: usize,
    pub total_tasks: usize,
    pub completed_tasks: usize,
    pub active_tasks: usize,
    pub ready_tasks: usize,
    pub blocked_tasks: usize,
    pub failed_tasks: usize,
    pub retrying_tasks: usize,
    pub open_blockers: usize,
    pub merge_queue_depth: usize,
    pub merge_conflicts: usize,
    pub attention_count: usize,
    #[serde(default)]
    pub stalled_tasks: usize,
    #[serde(default)]
    pub recoveries: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrchestrationSnapshot {
    pub session_id: String,
    pub project_id: String,
    pub goal: String,
    pub status: String,
    #[serde(default)]
    pub plan: Option<TeamPlan>,
    pub plan_revision: u32,
    pub policy: OrchestrationPolicy,
    #[serde(default)]
    pub departments: Vec<OrchestrationDepartment>,
    #[serde(default)]
    pub tasks: Vec<OrchestrationTask>,
    #[serde(default)]
    pub agents: Vec<AgentRecord>,
    #[serde(default)]
    pub notes: Vec<OrchestrationNote>,
    #[serde(default)]
    pub decisions: Vec<OrchestrationDecision>,
    #[serde(default)]
    pub revisions: Vec<PlanRevisionInfo>,
    #[serde(default)]
    pub blockers: Vec<OrchestrationBlocker>,
    /// Event-backed stall recovery rows for the current plan revision.
    #[serde(default)]
    pub recoveries: Vec<OrchestrationRecovery>,
    pub metrics: OrchestrationMetrics,
    pub generated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CoordinationNoteParams {
    pub session_id: String,
    pub action: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub target_agent_id: Option<String>,
    #[serde(default)]
    pub target_note_id: Option<String>,
    pub message: String,
    #[serde(default)]
    pub severity: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoordinationNoteResult {
    pub note_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeLeaseInfo {
    pub lease_key: String,
    pub owner_id: String,
    /// Process-scoped instance (`instance_id:pid`) that owns the row.
    #[serde(default)]
    pub owner_instance: String,
    pub kind: String,
    pub session_id: Option<String>,
    /// Monotonic per-key generation. Never reused across reacquisition.
    #[serde(default)]
    pub fencing_token: u64,
    pub acquired_at: String,
    pub heartbeat_at: String,
    #[serde(default)]
    pub renewed_at: String,
    pub expires_at: String,
    #[serde(default)]
    pub released_at: Option<String>,
    /// Derived `live`, `stale`, or `released`.
    #[serde(default)]
    pub state: String,
}

/// Stable, non-secret daemon identity for one data directory. `pid` is the
/// creator's pid for diagnostics; it is not a liveness probe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonInstanceInfo {
    pub instance_id: String,
    pub pid: u32,
    pub started_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeLeaseListResult {
    pub runtime_id: String,
    #[serde(default)]
    pub daemon_instance: Option<DaemonInstanceInfo>,
    pub leases: Vec<RuntimeLeaseInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeLeaseSweepResult {
    pub removed: usize,
}

/// Token-free history row for one runtime lease key. `detail` never includes
/// the acquisition token.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeLeaseAuditInfo {
    pub id: i64,
    pub lease_key: String,
    pub kind: String,
    pub action: String,
    pub owner_id: String,
    pub owner_instance: String,
    pub fencing_token: u64,
    #[serde(default)]
    pub detail: Option<String>,
    pub occurred_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeLeaseHistoryParams {
    #[serde(default)]
    pub lease_key: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeLeaseHistoryResult {
    #[serde(default)]
    pub lease_key: Option<String>,
    pub entries: Vec<RuntimeLeaseAuditInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ComputerLeaseInfo {
    pub lease_id: String,
    pub surface: String,
    pub owner_id: String,
    pub session_id: String,
    pub agent_id: String,
    pub agent_name: String,
    pub acquired_at: String,
    pub heartbeat_at: String,
    pub expires_at: String,
    /// Monotonic acquisition generation; survives daemon restart because it
    /// lives in SQLite. Older daemon payloads decode as zero.
    #[serde(default)]
    pub generation: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComputerLeaseListResult {
    pub runtime_id: String,
    pub leases: Vec<ComputerLeaseInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComputerLeaseSweepResult {
    pub removed: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ComputerLeaseReleaseParams {
    pub lease_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComputerLeaseReleaseResult {
    pub released: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrchestrationAdvanceParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrchestrationPreviewAction {
    pub action: String,
    pub task_id: String,
    pub detail: String,
    pub attempt: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrchestrationPreviewParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrchestrationPreviewResult {
    pub session_id: String,
    pub actions: Vec<OrchestrationPreviewAction>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrchestrationAdvanceResult {
    pub session_id: String,
    pub actions: Vec<OrchestrationDecision>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrchestrationOverviewParams {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrchestrationSessionSummary {
    pub session_id: String,
    pub project_id: String,
    pub project_name: String,
    pub goal: String,
    pub status: String,
    pub priority: i32,
    pub plan_revision: u32,
    pub metrics: OrchestrationMetrics,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub last_decision: Option<String>,
    #[serde(default)]
    pub last_decision_at: Option<String>,
    /// Stall recovery rows for this session's current plan revision.
    #[serde(default)]
    pub recoveries: usize,
    #[serde(default)]
    pub active_recoveries: usize,
    #[serde(default)]
    pub failed_recoveries: usize,
    #[serde(default)]
    pub last_recovery: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct OrchestrationOverviewTotals {
    pub sessions: usize,
    pub running: usize,
    pub attention: usize,
    pub blocked_tasks: usize,
    pub failed_tasks: usize,
    pub retrying_tasks: usize,
    pub open_blockers: usize,
    pub merge_conflicts: usize,
    #[serde(default)]
    pub stalled_tasks: usize,
    #[serde(default)]
    pub recoveries: usize,
    #[serde(default)]
    pub active_recoveries: usize,
    #[serde(default)]
    pub failed_recoveries: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrchestrationOverviewResult {
    pub sessions: Vec<OrchestrationSessionSummary>,
    pub totals: OrchestrationOverviewTotals,
    pub generated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrchestrationSnapshotParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrchestrationSnapshotResult {
    pub snapshot: OrchestrationSnapshot,
}

// ---------------------------------------------------------------------------
// Project programs and milestones (ADR-0157)
// ---------------------------------------------------------------------------

/// Lifecycle of a project-scoped program. Programs outlive sessions and hold
/// a milestone ledger; they are not a second kind of TeamPlan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramStatus {
    #[default]
    Draft,
    Active,
    Paused,
    Completed,
    Failed,
}

impl ProgramStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "draft" => Some(Self::Draft),
            "active" => Some(Self::Active),
            "paused" => Some(Self::Paused),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

impl std::fmt::Display for ProgramStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Lifecycle of one milestone. `ready` is a persisted, event-backed state so a
/// paused program can preserve dispatch eligibility without repeatedly
/// recomputing it, and `verifying` records the acceptance check between a
/// completed session task and terminal completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramMilestoneStatus {
    #[default]
    Pending,
    Ready,
    Running,
    Verifying,
    Completed,
    Failed,
    Blocked,
    Cancelled,
}

impl ProgramMilestoneStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Verifying => "verifying",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "ready" => Some(Self::Ready),
            "running" => Some(Self::Running),
            "verifying" => Some(Self::Verifying),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "blocked" => Some(Self::Blocked),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    pub fn is_terminal(self) -> bool {
        // `failed` is transient while a bounded retry or an explicit operator
        // retry is still possible. Only completed, blocked, and cancelled are
        // final states for program completion accounting.
        matches!(self, Self::Completed | Self::Blocked | Self::Cancelled)
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Running | Self::Verifying)
    }
}

impl std::fmt::Display for ProgramMilestoneStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Declarative, deterministic program policy. The controller never asks a
/// model which milestone to run; it applies these fields directly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ProgramPolicy {
    /// Opt-in autonomous dispatch. Creating an active program with no explicit
    /// policy enables this; a plain draft does not. Paused programs never
    /// dispatch regardless of this flag.
    pub auto_dispatch: bool,
    /// Automatic starts per milestone before it needs operator intervention.
    pub max_attempts: u32,
    /// Delay before an automatic retry becomes ready.
    pub retry_backoff_secs: u64,
    /// What happens to dependents when a milestone ends permanently failed.
    pub dependency_failure: DependencyFailurePolicy,
    /// Default token budget inherited by a milestone that declares none.
    pub token_budget: Option<u64>,
    /// Default USD budget inherited by a milestone that declares none.
    pub cost_budget_usd: Option<f64>,
    /// Maximum milestones in `running` or `verifying` at once.
    pub max_parallel_milestones: u32,
}

impl Default for ProgramPolicy {
    fn default() -> Self {
        Self {
            auto_dispatch: false,
            max_attempts: 2,
            retry_backoff_secs: 60,
            dependency_failure: DependencyFailurePolicy::Block,
            token_budget: None,
            cost_budget_usd: None,
            max_parallel_milestones: 2,
        }
    }
}

/// One project-scoped program. This row is a ledger, not an execution runtime:
/// execution always happens in a normal session with a normal one-task plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Program {
    pub id: String,
    pub project_id: String,
    pub goal: String,
    pub status: ProgramStatus,
    #[serde(default)]
    pub policy: ProgramPolicy,
    #[serde(default)]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub cost_budget_usd: Option<f64>,
    /// Operator-visible pause/failure/block reason. `None` for a clean run.
    #[serde(default)]
    pub reason: Option<String>,
    /// Imported bundle ledgers are archives. The controller never dispatches
    /// them, and `program.resume` refuses to activate them.
    #[serde(default)]
    pub archived: bool,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub completed_at: Option<String>,
}

/// One milestone in a program. `deps` contains milestone ids in the same
/// program; the store validates uniqueness and acyclicity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgramMilestone {
    pub id: String,
    pub program_id: String,
    /// Stable human key such as `1` or `1.2`; unique inside the program.
    pub key: String,
    pub title: String,
    pub objective: String,
    #[serde(default)]
    pub acceptance: Vec<String>,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub priority: i32,
    pub status: ProgramMilestoneStatus,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub plan_revision: Option<u32>,
    #[serde(default)]
    pub attempts: u32,
    pub max_attempts: u32,
    #[serde(default)]
    pub next_attempt_at: Option<String>,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default = "default_program_milestone_profile")]
    pub profile: String,
    #[serde(default)]
    pub write_scope: Vec<String>,
    #[serde(default)]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub cost_budget_usd: Option<f64>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub completed_at: Option<String>,
}

fn default_program_milestone_profile() -> String {
    "coding".to_string()
}

/// Append-only program-scoped event. Program events are separate from the
/// session event stream because a program outlives every one of its sessions.
/// Payloads contain the full program or milestone row so replay can rebuild
/// the projection without joining other tables.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgramEvent {
    pub program_id: String,
    pub project_id: String,
    pub seq: u64,
    #[serde(rename = "type")]
    pub kind: String,
    pub payload: Value,
    pub created_at: String,
}

/// Read-only aggregate of the sessions linked to a program.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProgramUsage {
    #[serde(default)]
    pub token_spent: u64,
    #[serde(default)]
    pub cost_spent_usd: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramMilestoneCounts {
    pub pending: usize,
    pub ready: usize,
    pub running: usize,
    pub verifying: usize,
    pub completed: usize,
    pub failed: usize,
    pub blocked: usize,
    pub cancelled: usize,
}

impl ProgramMilestoneCounts {
    pub fn total(&self) -> usize {
        self.pending
            + self.ready
            + self.running
            + self.verifying
            + self.completed
            + self.failed
            + self.blocked
            + self.cancelled
    }

    pub fn add(&mut self, status: ProgramMilestoneStatus) {
        match status {
            ProgramMilestoneStatus::Pending => self.pending += 1,
            ProgramMilestoneStatus::Ready => self.ready += 1,
            ProgramMilestoneStatus::Running => self.running += 1,
            ProgramMilestoneStatus::Verifying => self.verifying += 1,
            ProgramMilestoneStatus::Completed => self.completed += 1,
            ProgramMilestoneStatus::Failed => self.failed += 1,
            ProgramMilestoneStatus::Blocked => self.blocked += 1,
            ProgramMilestoneStatus::Cancelled => self.cancelled += 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgramListEntry {
    pub program: Program,
    #[serde(default)]
    pub counts: ProgramMilestoneCounts,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgramListResult {
    pub programs: Vec<ProgramListEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProgramListParams {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub status: Option<ProgramStatus>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProgramCreateParams {
    pub project_id: String,
    pub goal: String,
    #[serde(default)]
    pub policy: Option<ProgramPolicy>,
    #[serde(default)]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub cost_budget_usd: Option<f64>,
    /// Draft programs do not dispatch. An explicit activation creates an
    /// `active` row and, when `policy` was omitted, opts into auto-dispatch.
    #[serde(default)]
    pub activate: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProgramUpdateParams {
    pub program_id: String,
    pub goal: String,
    pub policy: ProgramPolicy,
    #[serde(default)]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub cost_budget_usd: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProgramParams {
    pub program_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProgramActionParams {
    pub program_id: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProgramDeleteParams {
    pub program_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProgramStatusParams {
    pub program_id: String,
    #[serde(default)]
    pub event_limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgramResult {
    pub program: Program,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgramStatusResult {
    pub program: Program,
    pub milestones: Vec<ProgramMilestone>,
    #[serde(default)]
    pub counts: ProgramMilestoneCounts,
    #[serde(default)]
    pub usage: ProgramUsage,
    #[serde(default)]
    pub events: Vec<ProgramEvent>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgramDeleteResult {
    pub deleted: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProgramMilestoneCreateParams {
    pub program_id: String,
    #[serde(default)]
    pub key: Option<String>,
    pub title: String,
    pub objective: String,
    #[serde(default)]
    pub acceptance: Vec<String>,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub max_attempts: Option<u32>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub write_scope: Vec<String>,
    #[serde(default)]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub cost_budget_usd: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProgramMilestoneUpdateParams {
    pub milestone_id: String,
    pub title: String,
    pub objective: String,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub acceptance: Vec<String>,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub max_attempts: Option<u32>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub write_scope: Vec<String>,
    #[serde(default)]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub cost_budget_usd: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProgramMilestoneParams {
    pub milestone_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProgramMilestoneActionParams {
    pub milestone_id: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgramMilestoneResult {
    pub milestone: ProgramMilestone,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgramMilestoneListResult {
    pub milestones: Vec<ProgramMilestone>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProgramTickParams {
    #[serde(default)]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgramTickResult {
    pub programs: usize,
    pub milestones_advanced: usize,
    pub sessions_started: usize,
    pub completed: usize,
    pub failed: usize,
    #[serde(default)]
    pub ready: Vec<ProgramMilestone>,
}

/// Result of replaying the program event log into its projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgramReplayReport {
    pub program_id: String,
    pub event_count: usize,
    pub program_matches: bool,
    pub milestones_match: bool,
    pub consistent: bool,
    #[serde(default)]
    pub repaired: bool,
    #[serde(default)]
    pub mismatches: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProgramReplayParams {
    pub program_id: String,
    #[serde(default)]
    pub repair: bool,
}

/// Rebuild the expected projection from the ordered event log. Events carry
/// full rows, so replay is a fold of program rows and milestone upserts plus a
/// delete tombstone.
pub fn rebuild_program_from_events(
    events: &[ProgramEvent],
) -> (Option<Program>, Vec<ProgramMilestone>) {
    let mut program: Option<Program> = None;
    let mut milestones: Vec<ProgramMilestone> = Vec::new();
    for event in events {
        match event.kind.as_str() {
            "program.deleted" => {
                program = None;
                milestones.clear();
            }
            kind if kind.starts_with("program.milestone.") => {
                let Ok(milestone) =
                    serde_json::from_value::<ProgramMilestone>(event.payload.clone())
                else {
                    continue;
                };
                if let Some(existing) = milestones
                    .iter_mut()
                    .find(|existing| existing.id == milestone.id)
                {
                    *existing = milestone;
                } else {
                    milestones.push(milestone);
                }
            }
            kind if kind.starts_with("program.") => {
                if let Ok(row) = serde_json::from_value::<Program>(event.payload.clone()) {
                    program = Some(row);
                }
            }
            _ => {}
        }
    }
    milestones.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.key.cmp(&right.key))
            .then_with(|| left.id.cmp(&right.id))
    });
    (program, milestones)
}

/// Accessibility and Screen Recording state reported by the packaged computer
/// helper. These are permissions, not approval decisions: the daemon approval
/// policy still decides whether a particular host-state action may run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ComputerPermissions {
    pub accessibility: bool,
    pub screen_recording: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Result of `computer.status`. The status path launches the helper only to
/// preflight TCC permissions; it never takes the GUI lease or posts input.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ComputerStatusResult {
    pub platform_supported: bool,
    pub helper_available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub helper_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<ComputerPermissions>,
    pub permissions_checked: bool,
    pub lease_surface: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// True when this payload came from the short-lived status cache and no
    /// helper process was spawned.
    #[serde(default)]
    pub cached: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_age_ms: Option<u64>,
    /// The logical lease surface is `gui`. Kept as a separate field so older
    /// clients that expected `computer` can still read the payload.
    #[serde(default)]
    pub grace_secs: u64,
    #[serde(default)]
    pub wait_ms: u64,
    #[serde(default)]
    pub snapshot_generations: bool,
}

impl Default for ComputerStatusResult {
    fn default() -> Self {
        Self {
            platform_supported: cfg!(target_os = "macos"),
            helper_available: false,
            helper_path: None,
            permissions: None,
            permissions_checked: false,
            lease_surface: "gui".to_string(),
            error: None,
            cached: false,
            cache_age_ms: None,
            grace_secs: 0,
            wait_ms: 0,
            snapshot_generations: true,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ComputerStatusParams {
    #[serde(default)]
    pub session_id: Option<String>,
    /// Bypass the short-lived helper/TCC status cache.
    #[serde(default)]
    pub refresh: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_acp_agent_configuration_decodes_as_stdio() {
        let agent: AcpAgent = serde_json::from_value(json!({
            "id": "agent-1",
            "name": "gemini",
            "command": "npx",
            "args": ["-y", "gemini"],
            "env": {"API_KEY": "hidden"},
            "enabled": true,
            "auto_approve": false,
            "timeout_secs": 900,
            "created_at": "2026-11-11T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(agent.transport, AcpTransport::Stdio);
        assert_eq!(agent.endpoint, None);
        assert_eq!(agent.auth_header_env, None);
        assert!(agent.headers.is_empty());
        assert_eq!(agent.connect_timeout_secs, 30);
        assert!(!agent.allow_remote);
        assert!(!agent.transport.is_remote());

        let params: AddAcpAgentParams = serde_json::from_value(json!({
            "name": "gemini",
            "command": "npx"
        }))
        .unwrap();
        assert_eq!(params.transport, AcpTransport::Stdio);
        assert!(!params.allow_remote);
    }

    #[test]
    fn remote_acp_agent_configuration_decodes_explicitly() {
        let agent: AcpAgent = serde_json::from_value(json!({
            "id": "agent-2",
            "name": "remote",
            "command": "",
            "transport": "streamable_http",
            "endpoint": "https://agent.example.test/acp",
            "auth_header_env": "ACP_TOKEN",
            "headers": {"x-project": "demo"},
            "connect_timeout_secs": 45,
            "allow_remote": true,
            "timeout_secs": 900,
            "created_at": "2026-11-11T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(agent.transport, AcpTransport::StreamableHttp);
        assert!(agent.transport.is_remote());
        assert_eq!(
            agent.endpoint.as_deref(),
            Some("https://agent.example.test/acp")
        );
        assert_eq!(agent.auth_header_env.as_deref(), Some("ACP_TOKEN"));
        assert_eq!(agent.connect_timeout_secs, 45);
        assert!(agent.allow_remote);

        assert_eq!(
            AcpTransport::parse("websocket"),
            Some(AcpTransport::Websocket)
        );
        assert_eq!(
            AcpTransport::parse("streamable-http"),
            Some(AcpTransport::StreamableHttp)
        );
        assert_eq!(AcpTransport::parse("sse"), None);
        assert_eq!(
            serde_json::to_value(AcpTransport::StreamableHttp).unwrap(),
            json!("streamable_http")
        );
    }

    #[test]
    fn acp_probe_capabilities_decode_additively() {
        let legacy: AcpProbeResult = serde_json::from_value(json!({
            "agent_id": "agent-1",
            "agent_name": "gemini",
            "version": "1",
            "protocol_version": 1,
            "session_created": true
        }))
        .unwrap();
        assert_eq!(legacy.transport, AcpTransport::Stdio);
        assert!(!legacy.remote);
        assert!(!legacy.load_session);
        assert!(!legacy.capabilities.load_session);
        assert_eq!(legacy.http_version, None);
        assert_eq!(legacy.messages_sent, 0);

        let current: AcpProbeResult = serde_json::from_value(json!({
            "agent_id": "agent-2",
            "agent_name": "remote",
            "version": "2",
            "protocol_version": 1,
            "session_created": true,
            "transport": "websocket",
            "endpoint": "wss://agent.example.test/acp",
            "remote": true,
            "load_session": true,
            "http_version": "HTTP/2",
            "messages_sent": 5,
            "messages_received": 7,
            "streams_opened": 2,
            "capabilities": {
                "load_session": true,
                "mcp_http": true,
                "mcp_sse": false,
                "prompt_image": true,
                "prompt_audio": false,
                "prompt_embedded_context": true
            }
        }))
        .unwrap();
        assert_eq!(current.transport, AcpTransport::Websocket);
        assert!(current.remote);
        assert!(current.load_session);
        assert!(current.capabilities.prompt_image);
        assert!(!current.capabilities.prompt_audio);
        assert_eq!(current.http_version.as_deref(), Some("HTTP/2"));
        assert_eq!(current.streams_opened, 2);
    }

    #[test]
    fn session_operation_ids_decode_as_optional() {
        let legacy_prompt: PromptParams = serde_json::from_value(json!({
            "session_id": "ses-1",
            "content": "continue"
        }))
        .unwrap();
        assert!(legacy_prompt.operation_id.is_none());
        let prompt: PromptParams = serde_json::from_value(json!({
            "session_id": "ses-1",
            "content": "continue",
            "operation_id": "op-1"
        }))
        .unwrap();
        assert_eq!(prompt.operation_id.as_deref(), Some("op-1"));

        let legacy_steer: SessionSteerParams = serde_json::from_value(json!({
            "session_id": "ses-1",
            "body": "look at tests"
        }))
        .unwrap();
        assert!(legacy_steer.operation_id.is_none());
        let steer: SessionSteerParams = serde_json::from_value(json!({
            "session_id": "ses-1",
            "body": "look at tests",
            "operation_id": "op-2"
        }))
        .unwrap();
        assert_eq!(steer.operation_id.as_deref(), Some("op-2"));

        let legacy_defer: SessionDeferParams = serde_json::from_value(json!({
            "session_id": "ses-1",
            "body": "follow up",
            "watch_task_ids": ["T1"]
        }))
        .unwrap();
        assert!(legacy_defer.operation_id.is_none());
        let defer: SessionDeferParams = serde_json::from_value(json!({
            "session_id": "ses-1",
            "body": "follow up",
            "watch_task_ids": ["T1"],
            "operation_id": "op-3"
        }))
        .unwrap();
        assert_eq!(defer.operation_id.as_deref(), Some("op-3"));
    }

    #[test]
    fn session_continuation_decodes_legacy_rows_as_one_shot() {
        let legacy: SessionContinuation = serde_json::from_value(json!({
            "id": "cont-1",
            "session_id": "ses-1",
            "project_id": "prj-1",
            "body": "follow up",
            "watch_task_ids": ["T1"],
            "watch_mode": "all",
            "deadline_at": null,
            "status": "delivered",
            "attempts": 0,
            "max_attempts": 3,
            "wake_reason": "watch",
            "outcomes": [],
            "last_error": null,
            "next_attempt_at": null,
            "created_at": "2026-11-11T00:00:00Z",
            "ready_at": "2026-11-11T00:01:00Z",
            "delivered_at": "2026-11-11T00:02:00Z",
            "updated_at": "2026-11-11T00:02:00Z"
        }))
        .unwrap();
        assert!(legacy.series_id.is_empty());
        assert!(legacy.parent_continuation_id.is_none());
        assert!(legacy.recurrence_every_secs.is_none());
        assert_eq!(legacy.recurrence_max_runs, 1);
        assert_eq!(legacy.run_count, 0);

        let recurring: SessionContinuation = serde_json::from_value(json!({
            "id": "cont-2",
            "session_id": "ses-1",
            "project_id": "prj-1",
            "body": "follow up",
            "watch_task_ids": [],
            "watch_mode": "all",
            "deadline_at": "2026-11-11T01:00:00Z",
            "status": "pending",
            "attempts": 0,
            "max_attempts": 3,
            "wake_reason": null,
            "outcomes": [],
            "last_error": null,
            "next_attempt_at": null,
            "created_at": "2026-11-11T00:00:00Z",
            "ready_at": null,
            "delivered_at": null,
            "updated_at": "2026-11-11T00:00:00Z",
            "series_id": "cont-1",
            "parent_continuation_id": "cont-1",
            "recurrence_every_secs": 3600,
            "recurrence_max_runs": 10,
            "run_count": 1
        }))
        .unwrap();
        assert_eq!(recurring.series_id, "cont-1");
        assert_eq!(recurring.parent_continuation_id.as_deref(), Some("cont-1"));
        assert_eq!(recurring.recurrence_every_secs, Some(3600));
        assert_eq!(recurring.recurrence_max_runs, 10);
        assert_eq!(recurring.run_count, 1);
        let encoded = serde_json::to_value(&recurring).unwrap();
        let decoded: SessionContinuation = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, recurring);
    }

    #[test]
    fn session_defer_params_decode_legacy_and_recurrence_fields() {
        let legacy: SessionDeferParams = serde_json::from_value(json!({
            "session_id": "ses-1",
            "body": "follow up",
            "watch_task_ids": ["T1"]
        }))
        .unwrap();
        assert!(legacy.after_secs.is_none());
        assert!(legacy.max_attempts.is_none());
        assert!(legacy.every_secs.is_none());
        assert!(legacy.max_runs.is_none());

        let recurring: SessionDeferParams = serde_json::from_value(json!({
            "session_id": "ses-1",
            "body": "follow up",
            "watch_task_ids": ["T1"],
            "every_secs": 3600,
            "max_runs": 24
        }))
        .unwrap();
        assert_eq!(recurring.every_secs, Some(3600));
        assert_eq!(recurring.max_runs, Some(24));
    }

    #[test]
    fn memory_items_decode_legacy_defaults_and_round_trip_provenance() {
        let legacy: MemoryItem = serde_json::from_value(json!({
            "id": "mem-1",
            "session_id": "ses-1",
            "project_id": "prj-1",
            "agent_id": "agent-1",
            "agent_name": "Leader",
            "kind": "decision",
            "title": "Use deterministic merge",
            "body": "The Integrator is deterministic.",
            "tags": ["architecture"],
            "created_at": "2026-11-11T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(legacy.status, "active");
        assert_eq!(legacy.source, "agent");
        assert!(legacy.evidence.is_empty());
        assert!(legacy.supersedes.is_empty());
        assert!(legacy.superseded_by.is_none());
        assert!(!legacy.pinned);
        assert_eq!(legacy.use_count, 0);
        assert!(legacy.verify_after.is_none());

        let current = MemoryItem {
            status: "stale".to_string(),
            source: "task_result".to_string(),
            evidence: vec![
                MemoryEvidence {
                    kind: "task".to_string(),
                    reference: "T3".to_string(),
                    note: Some("completed".to_string()),
                },
                MemoryEvidence {
                    kind: "commit".to_string(),
                    reference: "abcdef1".to_string(),
                    note: None,
                },
            ],
            supersedes: vec!["mem-0".to_string()],
            superseded_by: Some("mem-2".to_string()),
            pinned: true,
            use_count: 4,
            last_used_at: Some("2026-11-11T01:00:00Z".to_string()),
            last_verified_at: Some("2026-11-11T00:30:00Z".to_string()),
            verify_after: Some("2027-02-09T00:00:00Z".to_string()),
            stale_reason: Some("verification expired".to_string()),
            ..legacy
        };
        let encoded = serde_json::to_value(&current).unwrap();
        assert_eq!(encoded["status"], "stale");
        assert_eq!(encoded["source"], "task_result");
        assert_eq!(encoded["evidence"][0]["kind"], "task");
        let decoded: MemoryItem = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, current);

        let matched = MemoryMatch {
            memory: current,
            score: 1.25,
            reasons: vec!["fts".to_string(), "task:T3".to_string()],
            lexical_score: Some(0.5),
            semantic_score: Some(0.75),
            source: Some("hybrid".to_string()),
            semantic_unavailable: None,
        };
        let encoded = serde_json::to_value(&matched).unwrap();
        let decoded: MemoryMatch = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, matched);
    }

    #[test]
    fn memory_policy_decode_defaults_and_round_trips_new_fields() {
        let legacy: MemoryPolicy = serde_json::from_value(json!({
            "enabled": true,
            "max_items": 100,
            "max_age_days": 30
        }))
        .unwrap();
        assert_eq!(legacy.task_result_verify_days, Some(90));
        assert_eq!(legacy.default_verify_days, Some(365));
        assert!(legacy.auto_supersede);
        assert_eq!(legacy.brief_max_chars, Some(4000));
        assert!(!legacy.sweep_stale);

        let current: MemoryPolicy = serde_json::from_value(json!({
            "enabled": false,
            "task_result_verify_days": 14,
            "default_verify_days": null,
            "auto_supersede": false,
            "brief_max_chars": 2048,
            "sweep_stale": true
        }))
        .unwrap();
        assert_eq!(current.task_result_verify_days, Some(14));
        assert_eq!(current.default_verify_days, None);
        assert!(!current.auto_supersede);
        assert_eq!(current.brief_max_chars, Some(2048));
        assert!(current.sweep_stale);
        let encoded = serde_json::to_value(&current).unwrap();
        let decoded: MemoryPolicy = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, current);

        let params: MemoryListParams =
            serde_json::from_value(json!({"project_id": "prj-1"})).unwrap();
        assert!(params.status.is_none());
        assert!(!params.include_stale);
    }

    #[test]
    fn memory_conflict_and_health_round_trip_with_defaults() {
        let conflict: MemoryConflict = serde_json::from_value(json!({
            "id": "conflict-1",
            "project_id": "prj-1",
            "session_id": "ses-1",
            "memory_a_id": "mem-a",
            "memory_b_id": "mem-b",
            "memory_a_title": "Tabs",
            "memory_b_title": "Spaces",
            "memory_a_kind": "decision",
            "memory_b_kind": "fact",
            "memory_a_excerpt": "Use tabs.",
            "memory_b_excerpt": "Use spaces.",
            "scope_kind": "path",
            "scope_value": "src/lib.rs",
            "reason": "Both active memories cite path src/lib.rs and have different bodies.",
            "detected_at": "2026-11-11T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(conflict.status, "open");
        assert!(conflict.resolved_at.is_none());
        assert!(conflict.resolution.is_none());
        assert!(conflict.note.is_none());
        let encoded = serde_json::to_value(&conflict).unwrap();
        let decoded: MemoryConflict = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, conflict);

        let health = MemoryHealthResult {
            project_id: "prj-1".to_string(),
            total: 10,
            active: 6,
            stale: 2,
            superseded: 1,
            archived: 1,
            pinned: 2,
            evidence_covered: 4,
            verified: 3,
            verification_due: 1,
            open_conflicts: 2,
            by_kind: vec![MemoryStatCount {
                key: "fact".to_string(),
                count: 4,
            }],
        };
        let encoded = serde_json::to_value(&health).unwrap();
        let decoded: MemoryHealthResult = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, health);

        let params: MemoryConflictsParams =
            serde_json::from_value(json!({"project_id": "prj-1"})).unwrap();
        assert!(params.status.is_none());
        assert!(params.limit.is_none());
        let resolve: MemoryConflictResolveParams =
            serde_json::from_value(json!({"id": "c1", "action": "keep_a"})).unwrap();
        assert!(resolve.note.is_none());

        // Older bundles have no memory_conflicts field.
        let bundle_json = json!({
            "version": 1,
            "exported_at": "2026-11-11T00:00:00Z",
            "project": {
                "id": "prj-1",
                "name": "Demo",
                "root": "/tmp/demo",
                "default_branch": "main",
                "created_at": "2026-11-11T00:00:00Z"
            },
            "session": {
                "id": "ses-1",
                "project_id": "prj-1",
                "goal": "demo",
                "status": "imported",
                "priority": 0,
                "worktree": null,
                "branch": null,
                "token_budget": null,
                "cost_budget_usd": null,
                "time_budget_secs": null,
                "unlimited": false,
                "cost_spent_usd": 0.0,
                "base_commit": null,
                "tokens_in": 0,
                "tokens_out": 0,
                "created_at": "2026-11-11T00:00:00Z"
            },
            "events": [],
            "agents": [],
            "messages": [],
            "todos": [],
            "merge_queue": []
        });
        let bundle: SessionBundle = serde_json::from_value(bundle_json).unwrap();
        assert!(bundle.memory_conflicts.is_empty());
    }

    #[test]
    fn auth_params_accept_legacy_clients_without_a_protocol() {
        let legacy: AuthParams = serde_json::from_value(json!({"token": "abc"})).unwrap();
        assert_eq!(legacy.token, "abc");
        assert!(legacy.protocol.is_none());
        let current: AuthParams =
            serde_json::from_value(json!({"token": "abc", "protocol": 2})).unwrap();
        assert_eq!(current.protocol, Some(2));
    }

    #[test]
    fn orchestration_policy_stall_recovery_defaults_and_backcompat() {
        let legacy: OrchestrationPolicy = serde_json::from_value(json!({
            "auto_retry_failed": true,
            "max_attempts": 2,
            "retry_backoff_secs": 30,
            "retry_priority_boost": 10,
            "dependency_failure": "block",
            "escalate_after_retries": true,
            "auto_resolve_blockers": true
        }))
        .unwrap();
        assert_eq!(legacy.stall_recovery, StallRecoveryMode::Observe);
        assert_eq!(legacy.stall_min_observations, 2);
        assert_eq!(legacy.stall_max_attempts, 1);
        assert_eq!(legacy.stall_grace_secs, 300);

        let current: OrchestrationPolicy = serde_json::from_value(json!({
            "stall_recovery": "retry",
            "stall_min_observations": 4,
            "stall_max_attempts": 3,
            "stall_grace_secs": 45
        }))
        .unwrap();
        assert_eq!(current.stall_recovery, StallRecoveryMode::Retry);
        assert_eq!(current.stall_min_observations, 4);
        assert_eq!(current.stall_max_attempts, 3);
        assert_eq!(current.stall_grace_secs, 45);

        let encoded = serde_json::to_value(OrchestrationPolicy::default()).unwrap();
        assert_eq!(encoded["stall_recovery"], "observe");
        assert_eq!(encoded["stall_min_observations"], 2);
        assert_eq!(encoded["stall_max_attempts"], 1);
        assert_eq!(encoded["stall_grace_secs"], 300);
    }

    #[test]
    fn usage_entries_and_role_prices_decode_without_cache_fields() {
        let entry: UsageEntry = serde_json::from_value(json!({
            "id": "usage-1",
            "session_id": "session-1",
            "agent_id": "agent-1",
            "agent_name": "Worker",
            "role": "worker",
            "model": "model",
            "input_tokens": 10,
            "output_tokens": 5,
            "estimated": false,
            "cost_usd": 0.1,
            "created_at": "2026-11-11T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(entry.cache_read_tokens, 0);
        assert_eq!(entry.cache_write_tokens, 0);

        let config: RoleModelConfig = serde_json::from_value(json!({
            "model": "claude",
            "input_usd_per_million": 3.0,
            "output_usd_per_million": 15.0
        }))
        .unwrap();
        assert!(config.cache_read_usd_per_million.is_none());
        assert!(config.cache_write_usd_per_million.is_none());
    }

    #[test]
    fn mcp_add_params_preserve_flat_wire_shape() {
        let remote: AddMcpServerParams = serde_json::from_value(json!({
            "name": "remote",
            "transport": "http",
            "url": "https://mcp.example.test/acp",
            "headers": {"authorization": "Bearer ${MCP_TOKEN}"},
            "enabled": true
        }))
        .unwrap();
        assert_eq!(remote.config.name, "remote");
        assert_eq!(remote.config.transport, McpTransport::Http);
        assert_eq!(
            remote.config.url.as_deref(),
            Some("https://mcp.example.test/acp")
        );

        let legacy: AddMcpServerParams = serde_json::from_value(json!({
            "name": "local",
            "command": "npx",
            "args": ["-y", "server"]
        }))
        .unwrap();
        assert_eq!(legacy.config.transport, McpTransport::Stdio);
        assert_eq!(legacy.config.command, "npx");
        assert!(legacy.config.enabled);
        assert!(legacy.config.url.is_none());
    }

    #[test]
    fn tool_activity_decodes_legacy_entries_with_defaults() {
        let result: ToolActivityResult = serde_json::from_value(json!({
            "session_id": "ses-1",
            "entries": [{
                "tool_call_id": "call-1",
                "name": "read_file",
                "seq": 7,
                "is_error": true
            }],
            "total": 1,
            "limit": 50
        }))
        .unwrap();
        assert_eq!(result.entries.len(), 1);
        let entry = &result.entries[0];
        assert_eq!(entry.duration_ms, 0);
        assert_eq!(entry.output_bytes, 0);
        assert!(!entry.truncated);
        assert!(!entry.retryable);
        assert!(entry.error_code.is_none());
        assert!(entry.approval_risk.is_none());
        assert!(!result.truncated);
        assert_eq!(result.summary.calls, 0);

        let params: ToolActivityParams =
            serde_json::from_value(json!({"session_id": "ses-1"})).unwrap();
        assert!(params.limit.is_none());

        let encoded = serde_json::to_value(ToolActivityResult {
            session_id: "ses-1".to_string(),
            limit: 50,
            ..ToolActivityResult::default()
        })
        .unwrap();
        assert_eq!(encoded["entries"], json!([]));
        assert_eq!(encoded["summary"]["calls"], 0);
    }

    #[test]
    fn tool_activity_summary_backfills_new_buckets() {
        let summary: ToolActivitySummary = serde_json::from_value(json!({
            "calls": 3,
            "errors": 1,
            "retryable_errors": 0,
            "truncated_results": 0,
            "total_duration_ms": 30,
            "max_duration_ms": 20,
            "average_duration_ms": 10,
            "output_bytes": 400,
            "by_tool": []
        }))
        .unwrap();
        assert_eq!(summary.approval_waits, 0);
        assert_eq!(summary.total_approval_wait_ms, 0);
        assert_eq!(summary.max_approval_wait_ms, 0);
        assert!(summary.error_codes.is_empty());
        let encoded = serde_json::to_value(&summary).unwrap();
        assert_eq!(encoded["approval_waits"], 0);
        assert_eq!(encoded["error_codes"], json!([]));

        let full: ToolActivitySummary = serde_json::from_value(json!({
            "calls": 3,
            "errors": 1,
            "retryable_errors": 1,
            "truncated_results": 1,
            "total_duration_ms": 30,
            "max_duration_ms": 20,
            "average_duration_ms": 10,
            "output_bytes": 400,
            "approval_waits": 2,
            "total_approval_wait_ms": 700,
            "max_approval_wait_ms": 500,
            "by_tool": [{"name": "shell", "calls": 1, "errors": 1, "total_duration_ms": 20, "output_bytes": 300}],
            "error_codes": [{"code": "COMMAND_FAILED", "calls": 1}]
        }))
        .unwrap();
        assert_eq!(full.approval_waits, 2);
        assert_eq!(full.max_approval_wait_ms, 500);
        assert_eq!(full.by_tool[0].name, "shell");
        assert_eq!(full.error_codes[0].code, "COMMAND_FAILED");
    }

    #[test]
    fn daemon_metrics_round_trip_preserves_bounded_counts() {
        let metrics = DaemonMetrics {
            version: "0.1.0-dev".to_string(),
            protocol: 1,
            uptime_secs: 42,
            projects: 3,
            sessions: vec![
                MetricCount {
                    key: "draft".to_string(),
                    count: 1,
                },
                MetricCount {
                    key: "running".to_string(),
                    count: 2,
                },
            ],
            plan_tasks: vec![MetricCount {
                key: "completed".to_string(),
                count: 5,
            }],
            merge_queue: vec![MetricCount {
                key: "merged".to_string(),
                count: 2,
            }],
            intake: Vec::new(),
            continuations: Vec::new(),
            recoveries: Vec::new(),
            memories: vec![MetricCount {
                key: "active".to_string(),
                count: 7,
            }],
            memory_conflicts: vec![MetricCount {
                key: "open".to_string(),
                count: 1,
            }],
            runtime_leases: vec![MetricCount {
                key: "session".to_string(),
                count: 1,
            }],
            supervised_processes: vec![MetricCount {
                key: "running".to_string(),
                count: 1,
            }],
            supervised_process_output_bytes: 512,
            preemptions_by_phase: vec![MetricCount {
                key: "provider".to_string(),
                count: 1,
            }],
            preemptions_by_reason: vec![MetricCount {
                key: "wall_clock".to_string(),
                count: 1,
            }],
            tokens_in: 1000,
            tokens_out: 250,
            cache_read_tokens: 400,
            cache_write_tokens: 100,
            cache_hit_ratio: 0.4,
            preamble_tokens_estimate: Some(7000),
            cost_spent_usd: 0.125,
        };
        let result = DaemonMetricsResult {
            metrics: metrics.clone(),
            prometheus: "# HELP verlinpark_up Daemon liveness.\nverlinpark_up 1\n".to_string(),
        };
        let encoded = serde_json::to_value(&result).unwrap();
        assert_eq!(encoded["metrics"]["sessions"][1]["key"], "running");
        assert_eq!(encoded["metrics"]["tokens_in"], 1000);
        assert_eq!(encoded["metrics"]["cost_spent_usd"], 0.125);
        let decoded: DaemonMetricsResult = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, result);
    }

    #[test]
    fn cache_hit_ratio_is_zero_safe_and_clamped() {
        assert_eq!(cache_hit_ratio(0, 0, 0), 0.0);
        assert_eq!(cache_hit_ratio(0, 0, 10), 0.0);
        assert_eq!(cache_hit_ratio(0, 10, 0), 1.0);
        assert_eq!(cache_hit_ratio(100, 200, 0), 1.0);
        assert_eq!(cache_hit_ratio(500, 0, 0), 0.0);
        let normal = cache_hit_ratio(1_000, 600, 100);
        assert!((normal - 0.6).abs() < 1e-9, "unexpected ratio: {normal}");
    }

    #[test]
    fn cache_policy_defaults_and_validation_are_explicit() {
        let legacy: CachePolicy = serde_json::from_value(json!({})).unwrap();
        assert!(legacy.enabled);
        assert_eq!(legacy.ttl, "5m");
        assert!(legacy.automatic_caching);
        assert!(!legacy.warm_on_resume);
        assert!(legacy.compact_aware);
        assert!(legacy.role_ttl.is_empty());

        let one_hour: CachePolicy = serde_json::from_value(json!({
            "enabled": false,
            "ttl": "1h",
            "automatic_caching": false,
            "warm_on_resume": true,
            "compact_aware": false,
            "role_ttl": {"worker": "1h", "leader": "5m"}
        }))
        .unwrap();
        assert!(!one_hour.enabled);
        assert_eq!(one_hour.ttl_for("worker"), "1h");
        assert_eq!(one_hour.ttl_for("leader"), "5m");
        assert_eq!(one_hour.ttl_for("verifier"), "1h");
        assert!(one_hour.ttl_is_one_hour("verifier"));
        assert!(one_hour.normalized().is_ok());

        let invalid = CachePolicy {
            ttl: "30m".into(),
            ..CachePolicy::default()
        };
        assert!(invalid.normalized().is_err());
        let invalid_role = CachePolicy {
            role_ttl: BTreeMap::from([("intern".to_string(), "5m".to_string())]),
            ..CachePolicy::default()
        };
        assert!(invalid_role.normalized().is_err());
        let invalid_role_ttl = CachePolicy {
            role_ttl: BTreeMap::from([("worker".to_string(), "2h".to_string())]),
            ..CachePolicy::default()
        };
        assert!(invalid_role_ttl.normalized().is_err());

        // Normalization trims and lowercases without inventing a value.
        let normalized = CachePolicy {
            ttl: " 1H ".into(),
            role_ttl: BTreeMap::from([(" Worker ".to_string(), " 5M ".to_string())]),
            ..CachePolicy::default()
        }
        .normalized()
        .unwrap();
        assert_eq!(normalized.ttl, "1h");
        assert_eq!(
            normalized.role_ttl.get("worker").map(String::as_str),
            Some("5m")
        );
    }

    #[test]
    fn session_cache_status_decodes_legacy_and_new_payloads() {
        let legacy: SessionCacheStatus =
            serde_json::from_value(json!({"session_id": "ses-1"})).unwrap();
        assert_eq!(legacy.session_id, "ses-1");
        assert_eq!(legacy.cache_calls, 0);
        assert!(legacy.recent_calls.is_empty());
        assert!(legacy.policy.is_none());
        assert_eq!(legacy.last_breakpoints, 0);
        assert!(legacy.last_append_only.is_none());
        assert_eq!(legacy.append_only_calls, 0);
        assert_eq!(legacy.non_append_only_calls, 0);
        assert!(legacy.recent_breaks.is_empty());
        assert!(legacy.last_message_prefix_hash.is_none());
        assert!(legacy.provider_tail_system.is_none());

        let mut current_value = json!({
            "session_id": "ses-1",
            "input_tokens": 1000,
            "cache_read_tokens": 600,
            "cache_write_tokens": 100,
            "output_tokens": 50,
            "cache_hit_ratio": 0.6,
            "cache_read_ratio": 0.6,
            "cache_write_ratio": 0.1,
            "estimated_savings_usd": 0.42,
            "cache_calls": 3,
            "prompt_prefix": "prefix",
            "cache_generation": "generation",
            "core_hash": "core",
            "project_hash": "project",
            "tools_hash": "tools",
            "volatile_hash": "volatile",
            "last_breakpoints": 4,
            "last_cache_ttl": "1h",
            "last_miss_reason": "compaction_reset",
            "last_first_token_ms": 123,
            "recent_calls": [{
                "at": "2026-11-11T00:00:00Z",
                "input_tokens": 100,
                "cache_read_tokens": 60,
                "cache_breakpoints": 4,
                "cache_ttl": "1h",
                "miss_reason": "ttl_or_provider_miss"
            }],
            "miss_reason_counts": [{"key": "first_call", "calls": 2}],
            "ttl_counts": [{"key": "1h", "calls": 3}],
            "breakpoint_counts": [{"key": "4", "calls": 3}],
            "savings_by_role": [{"role": "leader", "calls": 3, "estimated_savings_usd": 0.42}],
            "first_token_p50_ms": 120,
            "first_token_p95_ms": 300,
            "compaction_resets": 1,
            "compaction_invalidation_tokens": 900,
            "compaction_invalidation_estimated_usd": 0.0027,
            "policy": {"ttl": "1h", "warm_on_resume": true}
        });
        let continuity = json!({
            "last_message_prefix_hash": "message-chain",
            "last_previous_message_prefix_hash": "previous-chain",
            "last_append_only": true,
            "append_only_calls": 4,
            "non_append_only_calls": 1,
            "last_message_count": 12,
            "last_tail_memory_mode": "embedded",
            "provider_tail_system": true,
            "last_tail_system_capability": "supported",
            "cache_break_counts": [{"key": "first_call", "calls": 1}],
            "recent_breaks": [{
                "at": "2026-11-11T00:00:00Z",
                "reason": "message_reordered",
                "first_differing_index": 3,
                "first_differing_role": "assistant",
                "first_differing_tool_call_id": "call_1",
                "previous_message_count": 8,
                "message_count": 9,
                "previous_message_prefix_hash": "previous-chain",
                "message_prefix_hash": "message-chain"
            }],
            "last_cache_break_at": "2026-11-11T00:00:00Z"
        });
        if let (Some(target), Some(extra)) = (current_value.as_object_mut(), continuity.as_object())
        {
            target.extend(extra.clone());
        }
        if let Some(call) = current_value
            .get_mut("recent_calls")
            .and_then(Value::as_array_mut)
            .and_then(|calls| calls.first_mut())
            .and_then(Value::as_object_mut)
        {
            call.insert("message_prefix_hash".into(), json!("message-chain"));
            call.insert(
                "previous_message_prefix_hash".into(),
                json!("previous-chain"),
            );
            call.insert("append_only".into(), json!(true));
            call.insert("message_count".into(), json!(9));
            call.insert("canonical_version".into(), json!(1));
            call.insert("tail_memory_mode".into(), json!("embedded"));
            call.insert("provider_tail_system".into(), json!(true));
            call.insert("cache_break_reason".into(), json!("message_reordered"));
            call.insert("message_sequence_issue_count".into(), json!(1));
            call.insert(
                "message_sequence_issues".into(),
                json!(["tool_result_reordered"]),
            );
        }
        if let Some(target) = current_value.as_object_mut() {
            target.insert("last_message_sequence_issue_count".into(), json!(1));
            target.insert(
                "last_message_sequence_issues".into(),
                json!(["tool_result_reordered"]),
            );
            target.insert(
                "message_sequence_issue_counts".into(),
                json!([{"key": "tool_result_reordered", "calls": 1}]),
            );
        }
        let current: SessionCacheStatus = serde_json::from_value(current_value).unwrap();
        assert_eq!(current.cache_calls, 3);
        assert_eq!(current.last_breakpoints, 4);
        assert_eq!(current.last_cache_ttl.as_deref(), Some("1h"));
        assert_eq!(
            current.last_miss_reason.as_deref(),
            Some("compaction_reset")
        );
        assert_eq!(current.recent_calls.len(), 1);
        assert_eq!(current.recent_calls[0].input_tokens, 100);
        assert_eq!(current.miss_reason_counts[0].key, "first_call");
        assert_eq!(current.ttl_counts[0].calls, 3);
        assert_eq!(current.breakpoint_counts[0].key, "4");
        assert_eq!(current.savings_by_role[0].role, "leader");
        assert_eq!(current.first_token_p50_ms, Some(120));
        assert_eq!(current.first_token_p95_ms, Some(300));
        assert_eq!(current.compaction_resets, 1);
        assert_eq!(current.compaction_invalidation_tokens, 900);
        assert_eq!(current.policy.as_ref().unwrap().ttl, "1h");
        assert_eq!(
            current.last_message_prefix_hash.as_deref(),
            Some("message-chain")
        );
        assert_eq!(
            current.last_previous_message_prefix_hash.as_deref(),
            Some("previous-chain")
        );
        assert_eq!(current.last_append_only, Some(true));
        assert_eq!(current.append_only_calls, 4);
        assert_eq!(current.non_append_only_calls, 1);
        assert_eq!(current.last_message_count, 12);
        assert_eq!(current.last_tail_memory_mode.as_deref(), Some("embedded"));
        assert_eq!(current.provider_tail_system, Some(true));
        assert_eq!(
            current.last_tail_system_capability.as_deref(),
            Some("supported")
        );
        assert_eq!(current.cache_break_counts[0].key, "first_call");
        assert_eq!(current.recent_breaks.len(), 1);
        assert_eq!(current.recent_breaks[0].reason, "message_reordered");
        assert_eq!(current.recent_breaks[0].first_differing_index, Some(3));
        assert_eq!(
            current.recent_breaks[0]
                .first_differing_tool_call_id
                .as_deref(),
            Some("call_1")
        );
        assert_eq!(current.recent_calls[0].append_only, Some(true));
        assert_eq!(current.recent_calls[0].message_count, 9);
        assert_eq!(current.recent_calls[0].canonical_version, 1);
        assert_eq!(current.last_message_sequence_issue_count, 1);
        assert_eq!(
            current.last_message_sequence_issues,
            vec!["tool_result_reordered"]
        );
        assert_eq!(current.message_sequence_issue_counts[0].calls, 1);
        assert_eq!(current.recent_calls[0].message_sequence_issue_count, 1);
        assert_eq!(
            current.recent_calls[0].message_sequence_issues,
            vec!["tool_result_reordered"]
        );
    }

    #[test]
    fn session_cache_breaks_result_is_additive_and_bounded() {
        let legacy: SessionCacheBreaksResult =
            serde_json::from_value(json!({"session_id": "ses-1"})).unwrap();
        assert_eq!(legacy.session_id, "ses-1");
        assert_eq!(legacy.total_breaks, 0);
        assert!(!legacy.truncated);
        assert!(legacy.breaks.is_empty());
        let current: SessionCacheBreaksResult = serde_json::from_value(json!({
            "session_id": "ses-1",
            "append_only_calls": 4,
            "non_append_only_calls": 1,
            "total_breaks": 2,
            "truncated": true,
            "breaks": [{
                "reason": "compaction_reset",
                "previous_message_count": 8,
                "message_count": 2
            }]
        }))
        .unwrap();
        assert_eq!(current.append_only_calls, 4);
        assert_eq!(current.breaks[0].reason, "compaction_reset");
        assert!(current.truncated);
        let encoded = serde_json::to_value(&current).unwrap();
        assert_eq!(encoded["breaks"][0]["message_count"], 2);
    }

    #[test]
    fn session_cache_break_decodes_legacy_and_all_reasons() {
        let legacy: SessionCacheBreak = serde_json::from_value(json!({})).unwrap();
        assert_eq!(legacy, SessionCacheBreak::default());
        assert_eq!(legacy.reason, "");
        assert_eq!(legacy.message_count, 0);

        for reason in [
            "first_call",
            "system_changed",
            "tool_bundle_changed",
            "message_edited",
            "message_reordered",
            "compaction_reset",
            "provider_cache_miss",
            "unknown",
        ] {
            let decoded: SessionCacheBreak = serde_json::from_value(json!({
                "reason": reason,
                "message_count": 2,
                "previous_message_count": 1,
            }))
            .unwrap();
            assert_eq!(decoded.reason, reason);
            assert_eq!(decoded.message_count, 2);
            assert_eq!(decoded.previous_message_count, 1);
        }
    }

    #[test]
    fn session_cache_call_decodes_legacy_empty_payload() {
        let call: SessionCacheCall = serde_json::from_value(json!({})).unwrap();
        assert_eq!(call, SessionCacheCall::default());
        let warm: SessionCacheCall = serde_json::from_value(json!({
            "at": "now",
            "warm": true,
            "estimated_savings_usd": -0.0003
        }))
        .unwrap();
        assert!(warm.warm);
        assert!(warm.estimated_savings_usd < 0.0);
        assert_eq!(warm.cache_miss_tokens, 0);
        assert_eq!(warm.append_only, None);
        assert_eq!(warm.theoretical_max_hit_ratio, 0.0);
        assert!(warm.message_prefix_hash.is_none());
    }

    #[test]
    fn session_cache_l5_fields_round_trip_and_default_on_legacy_payloads() {
        let call = SessionCacheCall {
            at: "now".into(),
            agent_name: Some("Leader".into()),
            role: Some("leader".into()),
            model: Some("deepseek-chat".into()),
            input_tokens: 200_000,
            output_tokens: 200,
            cache_read_tokens: 199_000,
            cache_write_tokens: 0,
            cache_hit_ratio: 0.995,
            cache_breakpoints: 0,
            cache_ttl: Some("5m".into()),
            first_token_ms: Some(420),
            core_hash: Some("core".into()),
            project_hash: Some("project".into()),
            tools_hash: Some("tools".into()),
            tool_bundle: Some("leader".into()),
            tool_bundle_hash: Some("bundle".into()),
            volatile_hash: Some("volatile".into()),
            prompt_prefix_hash: Some("prefix".into()),
            canonical_version: 0,
            message_count: 0,
            head_prefix_hash: None,
            tail_prefix_hash: None,
            request_prefix_hash: None,
            tail_memory_mode: None,
            provider_tail_system: None,
            tail_system_capability: None,
            cache_break_reason: None,
            message_sequence_issue_count: 0,
            message_sequence_issues: Vec::new(),
            cache_epoch_reused_from_sessions: 0,
            cross_session_cache_read_tokens: 0,
            cross_session_first_call: false,
            estimated_savings_usd: 0.42,
            miss_reason: Some("tail_delta".into()),
            warm: false,
            cache_miss_tokens: 1_000,
            theoretical_max_hit_ratio: 0.997,
            message_prefix_hash: Some("message".into()),
            previous_message_prefix_hash: Some("previous".into()),
            message_prefix_count: 12,
            previous_message_prefix_count: 10,
            append_only: Some(true),
            cache_epoch: Some("epoch".into()),
            cache_epoch_source: Some("prompt_prefix_v3".into()),
            previous_input_tokens: 198_000,
        };
        let encoded = serde_json::to_value(&call).unwrap();
        let decoded: SessionCacheCall = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, call);

        let status: SessionCacheStatus = serde_json::from_value(json!({
            "session_id": "ses-1",
            "cache_hit_ratio": 0.995,
            "cache_miss_tokens": 1000,
            "theoretical_max_hit_ratio": 0.997,
            "cache_epoch": "epoch",
            "cache_epoch_source": "prompt_prefix_v3",
            "append_only_calls": 49,
            "append_only_breaks": 0,
            "last_append_only": true,
            "hit_ratio_p50": 0.996,
            "hit_ratio_p95": 0.999,
            "theoretical_max_p50": 0.997,
            "theoretical_max_p95": 0.999,
            "cost_usd": 0.12,
            "cache_epoch_timeline": [{
                "epoch": "epoch",
                "source": "prompt_prefix_v3",
                "calls": 50,
                "first_at": "a",
                "last_at": "b",
                "input_tokens": 1000,
                "cache_read_tokens": 900,
                "cache_miss_tokens": 100,
                "hit_ratio": 0.9,
                "append_only_breaks": 0
            }],
            "per_role": [{
                "role": "leader",
                "calls": 50,
                "input_tokens": 1000,
                "cache_read_tokens": 900,
                "cache_miss_tokens": 100,
                "hit_ratio": 0.9,
                "theoretical_max_hit_ratio": 0.95,
                "cost_usd": 0.12
            }],
            "top_miss_reasons": [{"key": "tail_delta", "calls": 49}],
            "cross_session_reuse_sessions": 2,
            "cross_session_hit_tokens": 4000,
            "cross_session_calls_observed": 8,
            "compaction_lost_cache_usd": 0.003
        }))
        .unwrap();
        assert_eq!(status.cache_miss_tokens, 1_000);
        assert_eq!(status.theoretical_max_hit_ratio, 0.997);
        assert_eq!(status.cache_epoch.as_deref(), Some("epoch"));
        assert_eq!(status.append_only_calls, 49);
        assert_eq!(status.last_append_only, Some(true));
        assert_eq!(status.cache_epoch_timeline[0].epoch, "epoch");
        assert_eq!(status.cache_epoch_timeline[0].hit_ratio, 0.9);
        assert_eq!(status.per_role[0].role, "leader");
        assert_eq!(status.top_miss_reasons[0].calls, 49);
        assert_eq!(status.cross_session_reuse_sessions, 2);
        assert_eq!(status.compaction_lost_cache_usd, 0.003);

        let legacy: SessionCacheStatus =
            serde_json::from_value(json!({"session_id": "ses-1"})).unwrap();
        assert_eq!(legacy.cache_miss_tokens, 0);
        assert_eq!(legacy.theoretical_max_hit_ratio, 0.0);
        assert!(legacy.cache_epoch_timeline.is_empty());
        assert!(legacy.per_role.is_empty());
        assert_eq!(legacy.last_append_only, None);
    }

    #[test]
    fn cache_buckets_decode_legacy_and_unknown_fields() {
        let role: CacheRoleBucket = serde_json::from_value(json!({
            "role": "worker",
            "calls": 3,
            "input_tokens": 100,
            "cache_read_tokens": 90,
            "cache_miss_tokens": 10,
            "hit_ratio": 0.9,
            "theoretical_max_hit_ratio": 0.95,
            "cost_usd": 0.01,
            "future_field": true
        }))
        .unwrap();
        assert_eq!(role.role, "worker");
        assert_eq!(role.cache_miss_tokens, 10);
        let legacy: CacheRoleBucket = serde_json::from_value(json!({"role": "leader"})).unwrap();
        assert_eq!(legacy.calls, 0);
        assert_eq!(legacy.hit_ratio, 0.0);

        let epoch: CacheEpochBucket = serde_json::from_value(json!({
            "epoch": "e1",
            "source": "prompt_prefix_v3",
            "calls": 2,
            "first_at": "a",
            "last_at": "b",
            "input_tokens": 100,
            "cache_read_tokens": 90,
            "cache_miss_tokens": 10,
            "hit_ratio": 0.9,
            "append_only_breaks": 1
        }))
        .unwrap();
        assert_eq!(epoch.epoch, "e1");
        assert_eq!(epoch.append_only_breaks, 1);
        let legacy: CacheEpochBucket = serde_json::from_value(json!({})).unwrap();
        assert_eq!(legacy.epoch, "");
        assert_eq!(legacy.calls, 0);
        assert!(legacy.source.is_none());
    }

    #[test]
    fn project_cache_params_and_overview_decode_with_defaults() {
        let params: ProjectCacheParams =
            serde_json::from_value(json!({"project_id": "proj-1"})).unwrap();
        assert_eq!(params.project_id, "proj-1");
        assert_eq!(params.limit, None);
        let overview: ProjectCacheOverview = serde_json::from_value(json!({
            "project_id": "proj-1",
            "sessions_observed": 2,
            "calls": 10,
            "input_tokens": 1000,
            "cache_read_tokens": 900,
            "cache_miss_tokens": 100,
            "hit_ratio": 0.9,
            "hit_ratio_p50": 0.91,
            "hit_ratio_p95": 0.99,
            "append_only_calls": 9,
            "append_only_breaks": 0,
            "cache_epochs": 1,
            "top_miss_reasons": [{"key": "tail_delta", "calls": 9}],
            "scan_limit": 5000,
            "truncated": false
        }))
        .unwrap();
        assert_eq!(overview.sessions_observed, 2);
        assert_eq!(overview.cache_miss_tokens, 100);
        assert_eq!(overview.top_miss_reasons[0].key, "tail_delta");
        let legacy: ProjectCacheOverview =
            serde_json::from_value(json!({"project_id": "proj-1"})).unwrap();
        assert_eq!(legacy.calls, 0);
        assert_eq!(legacy.scan_limit, 0);
        assert!(legacy.cache_epoch_timeline.is_empty());
    }

    #[test]
    fn session_cache_calls_params_and_result_defaults_are_bounded() {
        let params: SessionCacheCallsParams =
            serde_json::from_value(json!({"session_id": "ses-1"})).unwrap();
        assert_eq!(params.session_id, "ses-1");
        assert_eq!(params.limit, None);
        assert_eq!(params.role, None);
        assert_eq!(params.miss_reason, None);
        assert!(!params.include_warm);

        let result = SessionCacheCallsResult {
            session_id: "ses-1".into(),
            calls: vec![SessionCacheCall {
                at: "now".into(),
                input_tokens: 100,
                cache_read_tokens: 90,
                cache_miss_tokens: 10,
                ..SessionCacheCall::default()
            }],
            observed: 5,
            returned: 1,
            truncated: true,
            limit: 1,
            role: Some("leader".into()),
            miss_reason: Some("tail_delta".into()),
            include_warm: false,
        };
        let encoded = serde_json::to_value(&result).unwrap();
        let decoded: SessionCacheCallsResult = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, result);
        let legacy: SessionCacheCallsResult =
            serde_json::from_value(json!({"session_id": "ses-1"})).unwrap();
        assert_eq!(legacy.returned, 0);
        assert!(!legacy.truncated);
        assert_eq!(legacy.limit, 0);
    }

    #[test]
    fn cache_audit_result_decodes_l5_fields_and_defaults() {
        let audit: SessionCacheAuditResult = serde_json::from_value(json!({
            "session_id": "ses-1",
            "input_tokens": 200_000,
            "cache_miss_tokens": 1_000,
            "theoretical_max_hit_ratio": 0.997,
            "hit_ratio_p50": 0.996,
            "hit_ratio_p95": 0.999,
            "append_only_calls": 49,
            "append_only_breaks": 0,
            "cache_epochs": 1
        }))
        .unwrap();
        assert_eq!(audit.input_tokens, 200_000);
        assert_eq!(audit.cache_miss_tokens, 1_000);
        assert_eq!(audit.append_only_calls, 49);
        assert_eq!(audit.cache_epochs, 1);
        let legacy: SessionCacheAuditResult =
            serde_json::from_value(json!({"session_id": "ses-1"})).unwrap();
        assert_eq!(legacy.input_tokens, 0);
        assert_eq!(legacy.cache_miss_tokens, 0);
        assert_eq!(legacy.append_only_breaks, 0);
        assert_eq!(legacy.cache_epochs, 0);
    }

    #[test]
    fn cache_warm_result_round_trips_and_decodes_legacy_defaults() {
        let result = SessionCacheWarmResult {
            session_id: "ses-1".into(),
            warmed: true,
            provider: "anthropic".into(),
            model: "claude".into(),
            role: "leader".into(),
            source: "manual".into(),
            cache_breakpoints: 4,
            cache_ttl: "1h".into(),
            automatic: true,
            core_hash: Some("core".into()),
            project_hash: Some("project".into()),
            tools_hash: Some("tools".into()),
            identity_hash: Some("i".repeat(64)),
            identity_id: Some("agt-1".into()),
            identity_generation: Some(2),
            cache_epoch: Some("e".repeat(64)),
            cache_epoch_unchanged: true,
            cache_epoch_reused_from_sessions: 2,
            input_tokens: 1_000,
            output_tokens: 5,
            cache_read_tokens: 0,
            cache_write_tokens: 900,
            estimated: false,
            cost_usd: 0.003,
            latency_ms: 250,
            first_token_ms: 120,
            message: "OK".into(),
        };
        let encoded = serde_json::to_value(&result).unwrap();
        let decoded: SessionCacheWarmResult = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, result);

        let minimal: SessionCacheWarmResult = serde_json::from_value(json!({
            "session_id": "ses-1",
            "warmed": false,
            "provider": "openai-compatible",
            "model": "gpt",
            "cache_breakpoints": 0,
            "cache_ttl": "5m"
        }))
        .unwrap();
        assert!(!minimal.warmed);
        assert_eq!(minimal.source, "");
        assert_eq!(minimal.cost_usd, 0.0);
        assert_eq!(minimal.latency_ms, 0);
    }

    #[test]
    fn cache_audit_result_decodes_legacy_minimal_payload() {
        let audit: SessionCacheAuditResult =
            serde_json::from_value(json!({"session_id": "ses-1"})).unwrap();
        assert_eq!(audit.session_id, "ses-1");
        assert_eq!(audit.calls_checked, 0);
        assert_eq!(audit.hit_ratio, 0.0);
        assert!(audit.findings.is_empty());
        assert!(!audit.clean);
        let finding: SessionCacheAuditFinding = serde_json::from_value(json!({
            "code": "x",
            "severity": "info",
            "message": "y"
        }))
        .unwrap();
        assert!(finding.evidence.is_empty());
    }

    #[test]
    fn instruction_protocol_types_default_and_round_trip() {
        let legacy: Session = serde_json::from_value(json!({
            "id": "ses-1",
            "project_id": "prj-1",
            "goal": "goal",
            "status": "idle",
            "created_at": "2026-11-12T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(legacy.prompt_layout, 2);

        let report: SessionInstructionsReport = serde_json::from_value(json!({
            "session_id": "ses-1"
        }))
        .unwrap();
        assert_eq!(report.prompt_layout, 3);
        assert!(report.states.is_empty());
        assert!(report.latest_identity_id.is_none());
        assert!(report.latest_identity_generation.is_none());
        assert!(report.latest_identity_hash.is_none());

        let with_identity: SessionInstructionsReport = serde_json::from_value(json!({
            "session_id": "ses-1",
            "latest_identity_id": "agt-1",
            "latest_identity_generation": 5,
            "latest_identity_hash": "b".repeat(64),
            "states": [{
                "state_id": "ses-1",
                "role": "worker",
                "identity_hash": "b".repeat(64),
                "sections": [{
                    "key": "agent/identity",
                    "hash": "b".repeat(64),
                    "ordinal": 15,
                    "tail": false,
                    "chars": 900,
                    "available": true
                }]
            }]
        }))
        .unwrap();
        assert_eq!(with_identity.latest_identity_id.as_deref(), Some("agt-1"));
        assert_eq!(with_identity.latest_identity_generation, Some(5));
        assert_eq!(
            with_identity.states[0].identity_hash.as_deref(),
            Some(with_identity.latest_identity_hash.as_deref().unwrap())
        );
        assert_eq!(with_identity.states[0].sections[0].ordinal, 15);
        assert!(!with_identity.states[0].sections[0].tail);

        let full = SessionInstructionsReport {
            session_id: "ses-1".into(),
            prompt_layout: 3,
            latest_prefix_hash: Some("prefix".into()),
            latest_cache_epoch: Some("prefix".into()),
            latest_stable_prefix_hash: Some("stable".into()),
            latest_identity_id: Some("agt-1".into()),
            latest_identity_generation: Some(2),
            latest_identity_hash: Some("b".repeat(64)),
            states: vec![SessionInstructionStateReport {
                state_id: "ses-1".into(),
                role: Some("leader".into()),
                agent_id: None,
                prompt_layout: 3,
                head_hash: "head".into(),
                tail_hash: "tail".into(),
                identity_hash: Some("b".repeat(64)),
                sections: vec![SessionInstructionSection {
                    key: "core/charter".into(),
                    hash: "a".repeat(64),
                    ordinal: 10,
                    tail: false,
                    chars: 120,
                    available: true,
                }],
            }],
            last_rebuilt: 1,
        };
        let encoded = serde_json::to_value(&full).unwrap();
        let decoded: SessionInstructionsReport = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, full);
        assert_eq!(decoded.states[0].sections[0].ordinal, 10);
        assert!(decoded.states[0].sections[0].available);

        let policy = CachePolicy::default();
        assert_eq!(policy.prompt_layout, 3);
        assert_eq!(policy.session_prompt_layout(), 3);
        let legacy_policy = CachePolicy {
            prompt_layout: 2,
            ..CachePolicy::default()
        };
        assert_eq!(legacy_policy.session_prompt_layout(), 2);
        let invalid = CachePolicy {
            prompt_layout: 9,
            ..CachePolicy::default()
        };
        assert!(invalid.normalized().is_err());
        assert_eq!(legacy_policy.normalized().unwrap().prompt_layout, 2);
    }

    #[test]
    fn session_bundle_decodes_legacy_without_instruction_fields() {
        let bundle: SessionBundle = serde_json::from_value(json!({
            "version": 1,
            "exported_at": "2026-11-12T00:00:00Z",
            "project": {
                "id": "prj-1",
                "name": "project",
                "root": "/tmp/project",
                "default_branch": "main",
                "created_at": "2026-11-12T00:00:00Z"
            },
            "session": {
                "id": "ses-1",
                "project_id": "prj-1",
                "goal": "goal",
                "status": "imported",
                "created_at": "2026-11-12T00:00:00Z"
            },
            "events": [],
            "agents": [],
            "messages": [],
            "todos": [],
            "merge_queue": []
        }))
        .unwrap();
        assert!(bundle.instruction_blobs.is_empty());
        assert!(bundle.instructions.is_empty());
        assert_eq!(bundle.session.prompt_layout, 2);
    }

    #[test]
    fn cache_plan_result_round_trips_and_defaults_params() {
        let params: SessionCachePlanParams =
            serde_json::from_value(json!({"session_id": "ses-1"})).unwrap();
        assert!(params.roles.is_empty());
        let result = SessionCachePlanResult {
            session_id: "ses-1".into(),
            provider: "anthropic".into(),
            policy: Some(CachePolicy::default()),
            roles: vec![SessionCachePlanRole {
                role: "leader".into(),
                provider: "anthropic".into(),
                model: "claude".into(),
                cache_breakpoints: 4,
                cache_ttl: "1h".into(),
                ttl_explicit: true,
                automatic: true,
                tail_message: false,
                tool_breakpoint: true,
                core_breakpoint: true,
                project_breakpoint: true,
                core_hash: "core".into(),
                project_hash: "project".into(),
                tools_hash: "tools".into(),
                volatile_hash: "volatile".into(),
                prefix_hash: "prefix".into(),
                identity_hash: "identity".into(),
                identity_id: Some("agt-1".into()),
                identity_generation: Some(3),
                prompt_layout: 2,
                stable_prefix_hash: "stable".into(),
                head_sections: Vec::new(),
                tail_sections: Vec::new(),
                section_chars: std::collections::BTreeMap::new(),
                core_chars: 100,
                project_chars: 50,
                stable_prefix_chars: 151,
                tool_count: 20,
                tool_bundle: "leader".into(),
                tool_bundle_hash: "bundle".into(),
                tool_bundle_tool_count: 46,
                tool_bundle_mcp_count: 2,
                tool_bundle_target_limit: Some(48),
                tool_bundle_within_target: Some(true),
                tool_bundle_fallback: false,
                system_tokens_estimate: 200,
                tool_tokens_estimate: 400,
            }],
        };
        let encoded = serde_json::to_value(&result).unwrap();
        let decoded: SessionCachePlanResult = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, result);
        let minimal: SessionCachePlanResult = serde_json::from_value(json!({
            "session_id": "ses-1",
            "roles": [{"role": "leader", "provider": "x", "model": "y"}]
        }))
        .unwrap();
        assert_eq!(minimal.roles[0].cache_breakpoints, 0);
        assert_eq!(minimal.roles[0].tool_count, 0);
        assert!(minimal.policy.is_none());
        assert!(minimal.roles[0].identity_hash.is_empty());
        assert!(minimal.roles[0].identity_id.is_none());
        assert!(minimal.roles[0].identity_generation.is_none());

        let identity_plan: SessionCachePlanResult = serde_json::from_value(json!({
            "session_id": "ses-1",
            "roles": [{
                "role": "worker",
                "provider": "x",
                "model": "y",
                "identity_hash": "b".repeat(64),
                "identity_id": "agt-worker",
                "identity_generation": 4,
                "head_sections": [{
                    "key": "agent/identity",
                    "hash": "b".repeat(64),
                    "ordinal": 15,
                    "tail": false
                }]
            }]
        }))
        .unwrap();
        assert_eq!(identity_plan.roles[0].identity_generation, Some(4));
        assert_eq!(
            identity_plan.roles[0].identity_id.as_deref(),
            Some("agt-worker")
        );
        assert_eq!(identity_plan.roles[0].identity_hash.len(), 64);
        assert_eq!(identity_plan.roles[0].head_sections[0].ordinal, 15);
    }

    #[test]
    fn tool_bundle_params_default_to_all_session_roles() {
        let params: ToolBundleParams =
            serde_json::from_value(json!({"session_id": "ses-1"})).unwrap();
        assert_eq!(params.session_id, "ses-1");
        assert!(params.roles.is_empty());
        assert!(params.task_kind.is_none());
        assert!(params.include_tools.is_none());
        let explicit: ToolBundleParams = serde_json::from_value(json!({
            "session_id": "ses-1",
            "roles": ["worker"],
            "task_kind": "trivial",
            "include_tools": false
        }))
        .unwrap();
        assert_eq!(explicit.roles, vec!["worker".to_string()]);
        assert_eq!(explicit.task_kind.as_deref(), Some("trivial"));
        assert_eq!(explicit.include_tools, Some(false));
    }

    #[test]
    fn tool_bundle_result_round_trips_and_decodes_minimal_payloads() {
        let result = ToolBundleResult {
            session_id: "ses-1".into(),
            roles: vec![ToolBundleRoleResult {
                role: "worker".into(),
                profile: "coding".into(),
                task_kind: Some("trivial".into()),
                bundle: "minimal".into(),
                title: "Minimal".into(),
                reason: "task_kind".into(),
                fallback: false,
                bundle_hash: "bundle".into(),
                tools_hash: "tools".into(),
                requested: vec!["read_file".into(), "write_file".into()],
                omitted: vec!["apply_patch".into()],
                warnings: vec![ToolBundleWarningInfo {
                    code: "tool_bundle_omitted".into(),
                    message: "missing apply_patch".into(),
                }],
                builtin_count: 6,
                mcp_count: 0,
                total_count: 6,
                schema_bytes: 4096,
                estimated_tool_tokens: 1024,
                target_limit: Some(12),
                within_target: Some(true),
                categories: vec![ToolBundleCategoryCount {
                    category: "files".into(),
                    tools: 1,
                    schema_bytes: 512,
                }],
                tools: vec![ToolBundleToolInfo {
                    name: "read_file".into(),
                    category: "files".into(),
                    workspace_write: false,
                    delegation: false,
                    schema_hash: "schema".into(),
                    description_chars: 120,
                    parameter_fields: 2,
                }],
                last_run: Some(ToolBundleLastRun {
                    agent_id: Some("agt-1".into()),
                    agent_name: Some("Worker".into()),
                    role: "worker".into(),
                    bundle: "minimal".into(),
                    bundle_hash: "bundle".into(),
                    tools_hash: "tools".into(),
                    tool_count: 6,
                    mcp_tool_count: 0,
                    created_at: "2026-11-12T00:00:00Z".into(),
                }),
            }],
            catalog: vec![ToolBundleCatalogRow {
                bundle: "minimal".into(),
                title: "Minimal".into(),
                summary: "small".into(),
                target_limit: Some(12),
                requested_count: 6,
                representative_role: "worker".into(),
                representative_profile: "coding".into(),
                allows_dynamic_mcp: false,
            }],
        };
        let encoded = serde_json::to_value(&result).unwrap();
        let decoded: ToolBundleResult = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, result);

        let minimal: ToolBundleResult =
            serde_json::from_value(json!({"session_id": "ses-1"})).unwrap();
        assert!(minimal.roles.is_empty());
        assert!(minimal.catalog.is_empty());
        let role: ToolBundleRoleResult = serde_json::from_value(json!({"role": "leader"})).unwrap();
        assert_eq!(role.role, "leader");
        assert_eq!(role.bundle, "");
        assert_eq!(role.builtin_count, 0);
        assert!(role.last_run.is_none());
        assert!(role.warnings.is_empty());
    }

    #[test]
    fn tool_bundle_catalog_result_decodes_and_agent_info_defaults() {
        let result: ToolBundleCatalogResult = serde_json::from_value(json!({
            "bundles": [{
                "bundle": "leader",
                "title": "Leader",
                "summary": "orchestration",
                "target_limit": 48,
                "requested_count": 46,
                "representative_role": "leader",
                "representative_profile": "generic",
                "allows_dynamic_mcp": true
            }],
            "warnings": [{"code": "w", "message": "m"}]
        }))
        .unwrap();
        assert_eq!(result.bundles.len(), 1);
        assert_eq!(result.bundles[0].target_limit, Some(48));
        assert_eq!(result.warnings[0].code, "w");
        let empty: ToolBundleCatalogResult = serde_json::from_value(json!({})).unwrap();
        assert!(empty.bundles.is_empty());
        assert!(empty.warnings.is_empty());
        let info: AgentInfo = serde_json::from_value(json!({"configured": false})).unwrap();
        assert!(info.tool_bundles.is_empty());
        assert_eq!(info.approval_mode, ApprovalMode::default());
    }

    #[test]
    fn tool_bundle_history_and_diff_decode_with_defaults() {
        let history: ToolBundleHistoryResult = serde_json::from_value(json!({
            "session_id": "ses-1",
            "entries": [{
                "seq": 4,
                "role": "worker",
                "bundle": "minimal",
                "bundle_hash": "hash",
                "tools_hash": "tools",
                "tool_count": 6,
                "mcp_tool_count": 0,
                "task_kind": "trivial",
                "fallback": false,
                "reason": "task_kind",
                "created_at": "now"
            }]
        }))
        .unwrap();
        assert_eq!(history.entries[0].seq, 4);
        assert_eq!(history.entries[0].bundle, "minimal");
        assert_eq!(history.entries[0].task_kind.as_deref(), Some("trivial"));
        let empty: ToolBundleHistoryResult =
            serde_json::from_value(json!({"session_id": "ses-1"})).unwrap();
        assert!(empty.entries.is_empty());

        let diff: ToolBundleDiffResult = serde_json::from_value(json!({
            "from_bundle": "leader",
            "to_bundle": "minimal",
            "added": ["a"],
            "removed": ["b"],
            "shared": ["c"],
            "changed": true
        }))
        .unwrap();
        assert_eq!(diff.from_count, 0);
        assert!(diff.changed);
        assert_eq!(diff.added, vec!["a"]);
        assert!(diff.warnings.is_empty());
        let params: ToolBundleDiffParams = serde_json::from_value(json!({
            "from_bundle": "leader",
            "to_bundle": "minimal"
        }))
        .unwrap();
        assert!(params.role.is_none());
        assert!(params.task_kind.is_none());
        let history_params: ToolBundleHistoryParams = serde_json::from_value(json!({
            "session_id": "ses-1"
        }))
        .unwrap();
        assert!(history_params.role.is_none());
        assert!(history_params.limit.is_none());
    }

    #[test]
    fn cache_status_decodes_legacy_payload_without_bundle_fields() {
        let status: SessionCacheStatus = serde_json::from_value(json!({
            "session_id": "ses-1",
            "input_tokens": 100,
            "cache_read_tokens": 80
        }))
        .unwrap();
        assert!(status.tool_bundle.is_none());
        assert!(status.tool_bundle_hash.is_none());
        assert!(status.tool_bundle_tool_count.is_none());
        assert!(status.tool_bundle_mcp_count.is_none());
        let call: SessionCacheCall = serde_json::from_value(json!({
            "at": "now",
            "tool_bundle": "leader",
            "tool_bundle_hash": "hash"
        }))
        .unwrap();
        assert_eq!(call.tool_bundle.as_deref(), Some("leader"));
        assert_eq!(call.tool_bundle_hash.as_deref(), Some("hash"));
    }

    #[test]
    fn cache_plan_role_decodes_legacy_payload_without_bundle_fields() {
        let role: SessionCachePlanRole = serde_json::from_value(json!({
            "role": "leader",
            "provider": "deepseek",
            "model": "deepseek-flash"
        }))
        .unwrap();
        assert_eq!(role.tool_bundle, "");
        assert_eq!(role.tool_bundle_hash, "");
        assert_eq!(role.tool_bundle_tool_count, 0);
        assert_eq!(role.tool_bundle_mcp_count, 0);
        assert_eq!(role.tool_bundle_target_limit, None);
        assert_eq!(role.tool_bundle_within_target, None);
        assert!(!role.tool_bundle_fallback);
    }

    #[test]
    fn compaction_summary_decodes_legacy_and_cache_fields() {
        let legacy: ContextCompactionSummary = serde_json::from_value(json!({
            "mode": "summary",
            "summarized_messages": 10,
            "dropped_messages": 0,
            "kept_messages": 4,
            "estimated_tokens_before": 1000,
            "estimated_tokens_after": 300
        }))
        .unwrap();
        assert!(legacy.cache_generation_before.is_none());
        assert!(legacy.cache_generation_after.is_none());
        assert!(legacy.cache_reset_reason.is_none());
        assert_eq!(legacy.cache_invalidation_tokens, 0);
        assert_eq!(legacy.cache_invalidation_estimated_usd, 0.0);
        assert!(!legacy.cache_compact_aware);

        let current: ContextCompactionSummary = serde_json::from_value(json!({
            "mode": "trim",
            "cache_generation_before": "before",
            "cache_generation_after": "after",
            "cache_reset_reason": "compaction",
            "cache_invalidation_tokens": 700,
            "cache_invalidation_estimated_usd": 0.0063,
            "cache_compact_aware": true
        }))
        .unwrap();
        assert_eq!(current.cache_generation_before.as_deref(), Some("before"));
        assert_eq!(current.cache_generation_after.as_deref(), Some("after"));
        assert_eq!(current.cache_reset_reason.as_deref(), Some("compaction"));
        assert_eq!(current.cache_invalidation_tokens, 700);
        assert!((current.cache_invalidation_estimated_usd - 0.0063).abs() < 1e-12);
        assert!(current.cache_compact_aware);
    }

    #[test]
    fn legacy_usage_payloads_decode_derived_cache_fields_as_default() {
        let totals = UsageTotals {
            calls: 1,
            input_tokens: 1_000,
            output_tokens: 100,
            cache_read_tokens: 600,
            cache_write_tokens: 100,
            cache_hit_ratio: 0.6,
            cost_usd: 0.25,
            ..UsageTotals::default()
        };
        let mut value = serde_json::to_value(&totals).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("cache_hit_ratio")
            .expect("new field was serialized");
        let legacy: UsageTotals = serde_json::from_value(value).unwrap();
        assert_eq!(legacy.cache_hit_ratio, 0.0);
        assert_eq!(legacy.cache_read_tokens, 600);

        let bucket = UsageBucket {
            key: "worker".into(),
            calls: 1,
            input_tokens: 100,
            output_tokens: 20,
            cache_read_tokens: 40,
            cache_write_tokens: 10,
            cache_hit_ratio: 0.4,
            cost_usd: 0.01,
        };
        let mut value = serde_json::to_value(&bucket).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("cache_hit_ratio")
            .expect("new field was serialized");
        let legacy: UsageBucket = serde_json::from_value(value).unwrap();
        assert_eq!(legacy.cache_hit_ratio, 0.0);

        let status: SessionCacheStatus = serde_json::from_value(json!({
            "session_id": "ses-legacy"
        }))
        .unwrap();
        assert_eq!(status.input_tokens, 0);
        assert_eq!(status.cache_hit_ratio, 0.0);
        assert!(status.prompt_prefix.is_none());
        assert!(status.preamble_tokens_estimate.is_none());
        assert!(status.last_call_at.is_none());
    }

    #[test]
    fn legacy_daemon_metrics_decode_without_cache_ratio_or_preamble() {
        let metrics = DaemonMetrics {
            version: "0.1.0-dev".to_string(),
            protocol: 1,
            uptime_secs: 1,
            projects: 1,
            sessions: Vec::new(),
            plan_tasks: Vec::new(),
            merge_queue: Vec::new(),
            intake: Vec::new(),
            continuations: Vec::new(),
            recoveries: Vec::new(),
            memories: Vec::new(),
            memory_conflicts: Vec::new(),
            runtime_leases: Vec::new(),
            supervised_processes: Vec::new(),
            supervised_process_output_bytes: 0,
            preemptions_by_phase: Vec::new(),
            preemptions_by_reason: Vec::new(),
            tokens_in: 10,
            tokens_out: 2,
            cache_read_tokens: 4,
            cache_write_tokens: 1,
            cache_hit_ratio: 0.4,
            preamble_tokens_estimate: Some(7000),
            cost_spent_usd: 0.0,
        };
        let mut value = serde_json::to_value(&metrics).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("cache_hit_ratio");
        object.remove("preamble_tokens_estimate");
        let legacy: DaemonMetrics = serde_json::from_value(value).unwrap();
        assert_eq!(legacy.cache_hit_ratio, 0.0);
        assert!(legacy.preamble_tokens_estimate.is_none());
    }

    #[test]
    fn computer_status_serializes_permissions_and_optional_fields() {
        let status = ComputerStatusResult {
            platform_supported: true,
            helper_available: true,
            helper_path: Some("/tmp/VerlinParkComputerHelper".into()),
            permissions: Some(ComputerPermissions {
                accessibility: true,
                screen_recording: false,
                detail: Some("enable Screen Recording".into()),
            }),
            permissions_checked: true,
            lease_surface: "gui".into(),
            error: None,
            cached: true,
            cache_age_ms: Some(1200),
            grace_secs: 45,
            wait_ms: 15_000,
            snapshot_generations: true,
        };
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(value["permissions"]["screen_recording"], false);
        assert!(value.get("error").is_none());
        let decoded: ComputerStatusResult = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, status);
    }

    #[test]
    fn computer_status_tolerates_older_daemon_payloads() {
        let status: ComputerStatusResult = serde_json::from_value(serde_json::json!({
            "platform_supported": true,
            "helper_available": false,
            "permissions_checked": false,
            "lease_surface": "computer"
        }))
        .unwrap();
        assert!(status.permissions.is_none());
        assert!(status.helper_path.is_none());
        assert!(status.error.is_none());
        let params: ComputerStatusParams = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(params.session_id.is_none());
        let permissions: ComputerPermissions = serde_json::from_value(serde_json::json!({
            "accessibility": true,
            "screen_recording": false
        }))
        .unwrap();
        assert!(permissions.detail.is_none());
    }
}

#[cfg(test)]
mod program_type_tests {
    use super::*;
    use serde_json::json;

    fn program_row() -> Program {
        Program {
            id: "prog-1".into(),
            project_id: "project-1".into(),
            goal: "Ship the program".into(),
            status: ProgramStatus::Active,
            policy: ProgramPolicy {
                auto_dispatch: true,
                max_attempts: 3,
                retry_backoff_secs: 30,
                dependency_failure: DependencyFailurePolicy::Retry,
                token_budget: Some(10_000),
                cost_budget_usd: Some(2.0),
                max_parallel_milestones: 3,
            },
            token_budget: Some(100_000),
            cost_budget_usd: Some(20.0),
            reason: None,
            archived: false,
            created_at: "2026-11-11T00:00:00Z".into(),
            updated_at: "2026-11-11T00:00:01Z".into(),
            completed_at: None,
        }
    }

    fn milestone_row() -> ProgramMilestone {
        ProgramMilestone {
            id: "ms-1".into(),
            program_id: "prog-1".into(),
            key: "1.2".into(),
            title: "Milestone".into(),
            objective: "Deliver it".into(),
            acceptance: vec!["accepted".into()],
            deps: Vec::new(),
            priority: 7,
            status: ProgramMilestoneStatus::Ready,
            session_id: None,
            plan_revision: None,
            attempts: 0,
            max_attempts: 3,
            next_attempt_at: None,
            result: None,
            last_error: None,
            profile: "coding".into(),
            write_scope: vec!["src".into()],
            token_budget: None,
            cost_budget_usd: None,
            created_at: "2026-11-11T00:00:00Z".into(),
            updated_at: "2026-11-11T00:00:01Z".into(),
            completed_at: None,
        }
    }

    #[test]
    fn program_rows_decode_with_additive_defaults() {
        let program: Program = serde_json::from_value(json!({
            "id": "prog-legacy",
            "project_id": "project-1",
            "goal": "Legacy program",
            "status": "draft",
            "created_at": "2026-11-11T00:00:00Z",
            "updated_at": "2026-11-11T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(program.policy, ProgramPolicy::default());
        assert!(!program.policy.auto_dispatch);
        assert_eq!(program.token_budget, None);
        assert!(!program.archived);
        assert!(program.completed_at.is_none());

        let milestone: ProgramMilestone = serde_json::from_value(json!({
            "id": "ms-legacy",
            "program_id": "prog-legacy",
            "key": "1",
            "title": "Legacy milestone",
            "objective": "Keep decoding",
            "status": "pending",
            "max_attempts": 2,
            "created_at": "2026-11-11T00:00:00Z",
            "updated_at": "2026-11-11T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(milestone.profile, "coding");
        assert!(milestone.acceptance.is_empty());
        assert!(milestone.deps.is_empty());
        assert_eq!(milestone.attempts, 0);
        assert!(milestone.session_id.is_none());
    }

    #[test]
    fn program_status_and_milestone_status_round_trip() {
        for status in [
            ProgramStatus::Draft,
            ProgramStatus::Active,
            ProgramStatus::Paused,
            ProgramStatus::Completed,
            ProgramStatus::Failed,
        ] {
            let value = serde_json::to_value(status).unwrap();
            assert_eq!(value, json!(status.as_str()));
            assert_eq!(ProgramStatus::parse(status.as_str()), Some(status));
        }
        for status in [
            ProgramMilestoneStatus::Pending,
            ProgramMilestoneStatus::Ready,
            ProgramMilestoneStatus::Running,
            ProgramMilestoneStatus::Verifying,
            ProgramMilestoneStatus::Completed,
            ProgramMilestoneStatus::Failed,
            ProgramMilestoneStatus::Blocked,
            ProgramMilestoneStatus::Cancelled,
        ] {
            let value = serde_json::to_value(status).unwrap();
            assert_eq!(value, json!(status.as_str()));
            assert_eq!(ProgramMilestoneStatus::parse(status.as_str()), Some(status));
        }
        assert!(!ProgramMilestoneStatus::Failed.is_terminal());
        assert!(ProgramMilestoneStatus::Blocked.is_terminal());
    }

    #[test]
    fn event_replay_folds_full_rows_and_honors_delete_tombstones() {
        let mut program = program_row();
        let mut milestone = milestone_row();
        let mut events = vec![ProgramEvent {
            program_id: program.id.clone(),
            project_id: program.project_id.clone(),
            seq: 1,
            kind: "program.created".into(),
            payload: serde_json::to_value(&program).unwrap(),
            created_at: "2026-11-11T00:00:00Z".into(),
        }];
        milestone.status = ProgramMilestoneStatus::Running;
        events.push(ProgramEvent {
            program_id: program.id.clone(),
            project_id: program.project_id.clone(),
            seq: 2,
            kind: "program.milestone.created".into(),
            payload: serde_json::to_value(&milestone).unwrap(),
            created_at: "2026-11-11T00:00:01Z".into(),
        });
        milestone.status = ProgramMilestoneStatus::Completed;
        milestone.result = Some("done".into());
        events.push(ProgramEvent {
            program_id: program.id.clone(),
            project_id: program.project_id.clone(),
            seq: 3,
            kind: "program.milestone.completed".into(),
            payload: serde_json::to_value(&milestone).unwrap(),
            created_at: "2026-11-11T00:00:02Z".into(),
        });
        program.status = ProgramStatus::Completed;
        events.push(ProgramEvent {
            program_id: program.id.clone(),
            project_id: program.project_id.clone(),
            seq: 4,
            kind: "program.completed".into(),
            payload: serde_json::to_value(&program).unwrap(),
            created_at: "2026-11-11T00:00:03Z".into(),
        });
        let (rebuilt, milestones) = rebuild_program_from_events(&events);
        assert_eq!(rebuilt.unwrap().status, ProgramStatus::Completed);
        assert_eq!(milestones.len(), 1);
        assert_eq!(milestones[0].status, ProgramMilestoneStatus::Completed);

        events.push(ProgramEvent {
            program_id: program.id.clone(),
            project_id: program.project_id.clone(),
            seq: 5,
            kind: "program.deleted".into(),
            payload: json!({"id": program.id}),
            created_at: "2026-11-11T00:00:04Z".into(),
        });
        let (rebuilt, milestones) = rebuild_program_from_events(&events);
        assert!(rebuilt.is_none());
        assert!(milestones.is_empty());
    }
}

#[cfg(test)]
mod program_wire_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn program_create_params_accept_legacy_minimal_input() {
        let params: ProgramCreateParams = serde_json::from_value(json!({
            "project_id": "project-1",
            "goal": "Ship it"
        }))
        .unwrap();
        assert!(params.policy.is_none());
        assert!(!params.activate);
        assert!(params.token_budget.is_none());
        assert!(params.cost_budget_usd.is_none());
    }

    #[test]
    fn program_status_result_round_trips_through_json() {
        let detail = ProgramStatusResult {
            program: Program {
                id: "prog-1".into(),
                project_id: "project-1".into(),
                goal: "Status".into(),
                status: ProgramStatus::Paused,
                policy: ProgramPolicy::default(),
                token_budget: Some(10_000),
                cost_budget_usd: Some(2.0),
                reason: Some("operator pause".into()),
                archived: false,
                created_at: "2026-11-11T00:00:00Z".into(),
                updated_at: "2026-11-11T00:00:01Z".into(),
                completed_at: None,
            },
            milestones: Vec::new(),
            counts: ProgramMilestoneCounts::default(),
            usage: ProgramUsage {
                token_spent: 10,
                cost_spent_usd: 0.1,
            },
            events: Vec::new(),
        };
        let value = serde_json::to_value(&detail).unwrap();
        assert_eq!(value["program"]["status"], "paused");
        assert_eq!(value["program"]["policy"]["auto_dispatch"], false);
        assert_eq!(value["usage"]["cost_spent_usd"], 0.1);
        assert_eq!(value["counts"]["pending"], 0);
    }

    #[test]
    fn legacy_session_bundle_decodes_without_a_program_ledger() {
        let bundle: SessionBundle = serde_json::from_value(json!({
            "version": 1,
            "exported_at": "2026-11-11T00:00:00Z",
            "project": {
                "id": "project-1",
                "name": "Legacy",
                "root": "/tmp/legacy",
                "default_branch": "main",
                "created_at": "2026-11-11T00:00:00Z"
            },
            "session": {
                "id": "session-1",
                "project_id": "project-1",
                "goal": "Legacy bundle",
                "status": "imported",
                "created_at": "2026-11-11T00:00:00Z"
            },
            "events": [],
            "agents": [],
            "messages": [],
            "todos": [],
            "merge_queue": []
        }))
        .unwrap();
        assert!(bundle.programs.is_empty());
        assert!(bundle.program_milestones.is_empty());
        assert!(bundle.program_events.is_empty());
        assert!(bundle.recoveries.is_empty());
    }

    #[test]
    fn program_tick_and_milestone_action_params_decode_additive_fields() {
        let tick: ProgramTickParams = serde_json::from_value(json!({})).unwrap();
        assert!(tick.project_id.is_none());
        let action: ProgramMilestoneActionParams =
            serde_json::from_value(json!({"milestone_id": "ms-1"})).unwrap();
        assert!(action.reason.is_none());
        let list: ProgramListParams = serde_json::from_value(json!({"status": "active"})).unwrap();
        assert_eq!(list.status, Some(ProgramStatus::Active));
        assert!(list.project_id.is_none());
    }

    #[test]
    fn durable_run_policy_and_rows_decode_with_safe_defaults() {
        let policy: SessionRunPolicy = serde_json::from_str(r#"{"auto_resume":true}"#).unwrap();
        assert!(policy.auto_resume);
        assert_eq!(policy.max_attempts, 1);
        assert_eq!(policy.max_age_hours, 24);

        let run: SessionRun = serde_json::from_value(serde_json::json!({
            "run_id": "run-1",
            "session_id": "ses-1",
            "project_id": "proj-1",
            "agent_id": "agent-1",
            "status": "running",
            "started_at": "2026-11-11T00:00:00Z",
            "updated_at": "2026-11-11T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(run.attempt, 1);
        assert!(!run.top_level);
        assert!(run.task_id.is_none());
        assert!(run.failure.is_none());
        assert_eq!(run.budget, RunBudgetSnapshot::default());

        let checkpoint: RunCheckpoint = serde_json::from_value(serde_json::json!({
            "checkpoint_id": "cp-1",
            "run_id": "run-1",
            "session_id": "ses-1",
            "seq": 1,
            "created_at": "2026-11-11T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(checkpoint.turn, 0);
        assert_eq!(checkpoint.message_cursor, 0);
        assert_eq!(checkpoint.event_cursor, 0);

        let step: RunStep = serde_json::from_value(serde_json::json!({
            "step_id": "step-1",
            "run_id": "run-1",
            "session_id": "ses-1",
            "seq": 1,
            "turn": 1,
            "kind": "model_request",
            "status": "intent",
            "created_at": "2026-11-11T00:00:00Z",
            "updated_at": "2026-11-11T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(step.input_hash, "");
        assert_eq!(step.payload, Value::Null);
    }

    #[test]
    fn durable_run_policy_bounds_round_trip() {
        let policy = SessionRunPolicy {
            auto_resume: true,
            max_attempts: 3,
            max_age_hours: 168,
        };
        let encoded = serde_json::to_string(&policy).unwrap();
        let decoded: SessionRunPolicy = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, policy);
    }

    #[test]
    fn runtime_lease_info_describes_fencing_without_a_token() {
        let lease = RuntimeLeaseInfo {
            lease_key: "integration:s1".into(),
            owner_id: "instance:42:nonce".into(),
            owner_instance: "instance:42".into(),
            kind: "integration".into(),
            session_id: Some("s1".into()),
            fencing_token: 7,
            acquired_at: "2026-11-11T00:00:00Z".into(),
            heartbeat_at: "2026-11-11T00:01:00Z".into(),
            renewed_at: "2026-11-11T00:01:00Z".into(),
            expires_at: "2026-11-11T00:16:00Z".into(),
            released_at: None,
            state: "live".into(),
        };
        let value = serde_json::to_value(&lease).unwrap();
        assert_eq!(value["fencing_token"], 7);
        assert_eq!(value["owner_instance"], "instance:42");
        assert_eq!(value["state"], "live");
        assert!(value.get("lease_token").is_none());
        assert!(value.get("token").is_none());
        // Legacy clients omit the new fields; the additive protocol still
        // decodes them with safe defaults.
        let legacy: RuntimeLeaseInfo = serde_json::from_value(serde_json::json!({
            "lease_key": "session:s1",
            "owner_id": "daemon-a",
            "kind": "session",
            "session_id": "s1",
            "acquired_at": "2026-11-09T09:00:00Z",
            "heartbeat_at": "2026-11-09T09:01:00Z",
            "expires_at": "2026-11-09T09:03:00Z"
        }))
        .unwrap();
        assert_eq!(legacy.fencing_token, 0);
        assert!(legacy.owner_instance.is_empty());
        assert!(legacy.released_at.is_none());
        assert!(legacy.state.is_empty());
    }

    #[test]
    fn runtime_lease_history_is_token_free_and_additive() {
        let entry = RuntimeLeaseAuditInfo {
            id: 1,
            lease_key: "integration:s1".into(),
            kind: "integration".into(),
            action: "acquired".into(),
            owner_id: "instance:42:nonce".into(),
            owner_instance: "instance:42".into(),
            fencing_token: 3,
            detail: Some("expired predecessor generation 2".into()),
            occurred_at: "2026-11-11T00:00:00Z".into(),
        };
        let value = serde_json::to_value(&entry).unwrap();
        assert_eq!(value["action"], "acquired");
        assert_eq!(value["fencing_token"], 3);
        assert!(value.get("lease_token").is_none());
        assert!(value.get("token").is_none());
        let decoded: RuntimeLeaseAuditInfo = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, entry);
    }

    #[test]
    fn tool_output_policy_decodes_with_safe_defaults() {
        let policy: ToolOutputPolicy = serde_json::from_str("{}").unwrap();
        assert!(policy.enabled);
        assert_eq!(policy.spill_threshold_bytes, 16 * 1024);
        assert_eq!(policy.preview_bytes, 4 * 1024);
        assert_eq!(policy.max_total_bytes_per_turn, 64 * 1024);
        assert_eq!(policy.retention_secs, 7 * 24 * 60 * 60);
        assert_eq!(policy.max_entries, 2_000);
        assert!(policy.tool_thresholds.is_empty());

        let encoded = serde_json::to_string(&policy).unwrap();
        let decoded: ToolOutputPolicy = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, policy);
    }

    #[test]
    fn tool_output_policy_keeps_unknown_per_tool_fields() {
        let policy: ToolOutputPolicy = serde_json::from_value(serde_json::json!({
            "enabled": false,
            "spill_threshold_bytes": 8192,
            "tool_thresholds": {"shell": 131072, "future_tool": 4096}
        }))
        .unwrap();
        assert!(!policy.enabled);
        assert_eq!(policy.spill_threshold_bytes, 8192);
        assert_eq!(policy.tool_thresholds.get("shell"), Some(&131_072));
        assert_eq!(policy.tool_thresholds.get("future_tool"), Some(&4096));
    }

    #[test]
    fn legacy_compaction_summary_decodes_without_metrics() {
        let summary: ContextCompactionSummary = serde_json::from_value(serde_json::json!({
            "mode": "summary",
            "summarized_messages": 8,
            "dropped_messages": 0,
            "kept_messages": 4,
            "estimated_tokens_before": 1000,
            "estimated_tokens_after": 400,
            "created_at": "2026-11-11T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(summary.mode, "summary");
        assert_eq!(summary.latency_ms, 0);
        assert!(!summary.background_reuse);
        assert!(!summary.fallback_to_sync);
        assert_eq!(summary.compaction_count, 0);
        assert!(summary.structured_state.is_none());
    }

    #[test]
    fn tool_output_params_default_additively() {
        let list: ToolOutputListParams = serde_json::from_str("{}").unwrap();
        assert!(list.project_id.is_none());
        assert!(list.session_id.is_none());
        assert!(list.tool_name.is_none());
        assert!(list.before_created_at.is_none());
        assert!(!list.include_expired);
        assert!(list.limit.is_none());
        let prune: ToolOutputPruneParams = serde_json::from_str("{}").unwrap();
        assert!(!prune.dry_run);
        assert!(prune.older_than_secs.is_none());
        let audit: ToolOutputAuditParams = serde_json::from_str("{}").unwrap();
        assert!(!audit.verify_hashes);
        assert!(!audit.clean_orphans);
        let status: ToolOutputStatusParams = serde_json::from_str("{}").unwrap();
        assert!(status.project_id.is_none());
    }

    #[test]
    fn tool_output_entry_and_status_decode_metadata_only() {
        let entry: ToolOutputEntry = serde_json::from_value(serde_json::json!({
            "id": "out_00000000-0000-0000-0000-000000000000",
            "session_id": "ses-1",
            "tool_name": "shell",
            "sha256": "a".repeat(64),
            "bytes": 1000,
            "lines": 10,
            "preview_bytes": 200,
            "created_at": "2026-11-11T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(entry.bytes, 1000);
        assert!(entry.tool_call_id.is_none());
        assert!(entry.expires_at.is_none());
        let status: ToolOutputStatus = serde_json::from_value(serde_json::json!({
            "policy": {},
            "entries": 2,
            "total_bytes": 4000,
            "preview_bytes": 800,
            "active_entries": 1,
            "expired_entries": 0,
            "by_tool": [
                {"tool_name": "shell", "entries": 2, "bytes": 4000, "preview_bytes": 800}
            ]
        }))
        .unwrap();
        assert_eq!(status.entries, 2);
        assert_eq!(status.by_tool.len(), 1);
        assert_eq!(status.by_tool[0].tool_name, "shell");
        assert_eq!(status.policy, ToolOutputPolicy::default());
        assert!(status.project_id.is_none());
    }

    #[test]
    fn legacy_tool_output_status_decodes_without_by_tool() {
        let status: ToolOutputStatus = serde_json::from_value(serde_json::json!({
            "policy": {
                "enabled": true,
                "spill_threshold_bytes": 16384,
                "preview_bytes": 4096,
                "max_total_bytes_per_turn": 65536,
                "retention_secs": 604800,
                "max_entries": 2000
            },
            "entries": 0,
            "total_bytes": 0,
            "preview_bytes": 0,
            "active_entries": 0,
            "expired_entries": 0
        }))
        .unwrap();
        assert!(status.by_tool.is_empty());
        assert!(status.oldest_created_at.is_none());
        assert!(status.project_id.is_none());
    }

    #[test]
    fn tool_output_status_params_reject_a_non_object_payload() {
        assert!(
            serde_json::from_value::<ToolOutputStatusParams>(serde_json::json!("bad")).is_err()
        );
        let params: ToolOutputStatusParams =
            serde_json::from_value(serde_json::json!({"session_id": "ses-1"})).unwrap();
        assert_eq!(params.session_id.as_deref(), Some("ses-1"));
        assert!(params.project_id.is_none());
    }

    #[test]
    fn tool_output_list_result_reports_a_next_cursor() {
        let result = ToolOutputListResult {
            entries: Vec::new(),
            total: 10,
            truncated: true,
            next_before: Some("2026-11-11T00:00:00Z".into()),
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["truncated"], true);
        assert_eq!(value["next_before"], "2026-11-11T00:00:00Z");
        assert_eq!(value["entries"], serde_json::json!([]));
    }

    #[test]
    fn tool_output_prune_and_audit_results_are_stable_json() {
        let prune = ToolOutputPruneResult {
            project_id: Some("proj-1".into()),
            session_id: None,
            candidates: 3,
            removed: 2,
            removed_bytes: 100,
            expired_removed: 1,
            overflow_removed: 1,
            protected_active: 1,
            dry_run: false,
        };
        let value = serde_json::to_value(&prune).unwrap();
        assert_eq!(value["candidates"], 3);
        assert_eq!(value["removed"], 2);
        assert_eq!(value["protected_active"], 1);
        let audit = ToolOutputAuditResult {
            rows: 2,
            orphan_blobs: 1,
            cleaned_orphans: 1,
            ..ToolOutputAuditResult::default()
        };
        let value = serde_json::to_value(&audit).unwrap();
        assert_eq!(value["rows"], 2);
        assert_eq!(value["orphan_blobs"], 1);
        assert_eq!(value["missing_blobs"], 0);
    }

    #[test]
    fn tool_output_policy_full_round_trip_preserves_overrides() {
        let mut policy = ToolOutputPolicy {
            enabled: false,
            spill_threshold_bytes: 2_048,
            preview_bytes: 512,
            max_total_bytes_per_turn: 4_096,
            retention_secs: 600,
            max_entries: 7,
            ..ToolOutputPolicy::default()
        };
        policy.tool_thresholds.insert("shell".into(), 65_536);
        policy.tool_thresholds.insert("grep".into(), 1_024);
        let encoded = serde_json::to_string(&policy).unwrap();
        let decoded: ToolOutputPolicy = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, policy);
        assert_eq!(decoded.tool_thresholds.get("shell"), Some(&65_536));
    }

    #[test]
    fn tool_output_entry_round_trips_every_metadata_field() {
        let entry = ToolOutputEntry {
            id: "out_00000000-0000-0000-0000-000000000000".into(),
            session_id: "ses-1".into(),
            tool_call_id: Some("call-1".into()),
            tool_name: "shell".into(),
            sha256: "a".repeat(64),
            bytes: 1234,
            lines: 12,
            preview_bytes: 200,
            created_at: "2026-11-11T00:00:00Z".into(),
            expires_at: Some("2026-11-18T00:00:00Z".into()),
        };
        let encoded = serde_json::to_string(&entry).unwrap();
        let decoded: ToolOutputEntry = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, entry);
    }

    #[test]
    fn continuation_status_and_reclaim_wire_shapes_are_token_free() {
        let continuation: SessionContinuation = serde_json::from_value(serde_json::json!({
            "id": "cont-1",
            "session_id": "ses-1",
            "project_id": "prj-1",
            "body": "continue",
            "status": "delivering",
            "created_at": "2026-11-11T00:00:00Z",
            "updated_at": "2026-11-11T00:00:00Z"
        }))
        .unwrap();
        let status = SessionContinuationStatusResult {
            continuation,
            claim: Some(SessionContinuationClaimInfo {
                status: "delivering".into(),
                owner_id: Some("instance:42:nonce".into()),
                expires_at: Some("2026-11-11T00:02:00Z".into()),
                delivery_attempt: 2,
                live: true,
            }),
        };
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(value["claim"]["delivery_attempt"], 2);
        assert_eq!(value["claim"]["live"], true);
        assert!(!value.to_string().contains("lease_token"));
        let reclaim = SessionContinuationReclaimResult {
            reclaimed: true,
            continuation: Some(status.continuation),
        };
        assert_eq!(serde_json::to_value(reclaim).unwrap()["reclaimed"], true);
    }

    #[test]
    fn continuation_claim_fields_decode_as_additive_defaults() {
        let legacy: SessionContinuation = serde_json::from_value(serde_json::json!({
            "id": "cont-legacy",
            "session_id": "ses-1",
            "project_id": "prj-1",
            "body": "continue",
            "status": "delivering",
            "created_at": "2026-11-11T00:00:00Z",
            "updated_at": "2026-11-11T00:00:00Z"
        }))
        .unwrap();
        assert!(legacy.lease_owner.is_none());
        assert!(legacy.lease_expires_at.is_none());
        assert_eq!(legacy.delivery_attempt, 0);

        let value = serde_json::to_value(SessionContinuation {
            lease_owner: Some("instance:42:nonce".into()),
            lease_expires_at: Some("2026-11-11T00:02:00Z".into()),
            delivery_attempt: 2,
            ..legacy
        })
        .unwrap();
        assert_eq!(value["delivery_attempt"], 2);
        assert_eq!(value["lease_owner"], "instance:42:nonce");
        assert!(value.get("lease_token").is_none());
    }

    #[test]
    fn hybrid_memory_fields_decode_additively_and_default_safely() {
        let legacy_match: MemoryMatch = serde_json::from_value(json!({
            "memory": {
                "id": "mem-1",
                "session_id": "ses-1",
                "project_id": "prj-1",
                "agent_id": "agent-1",
                "agent_name": "Leader",
                "kind": "fact",
                "title": "Legacy",
                "body": "body",
                "created_at": "2026-11-11T00:00:00Z"
            },
            "score": 0.5,
            "reasons": ["fts"]
        }))
        .unwrap();
        assert!(legacy_match.lexical_score.is_none());
        assert!(legacy_match.semantic_score.is_none());
        assert!(legacy_match.source.is_none());
        assert!(legacy_match.semantic_unavailable.is_none());

        let legacy_policy: MemoryPolicy = serde_json::from_value(json!({
            "enabled": false
        }))
        .unwrap();
        assert!(legacy_policy.embedding.is_none());

        let embedding: MemoryEmbeddingPolicy = serde_json::from_value(json!({
            "enabled": true,
            "endpoint": "https://example.com/v1",
            "model": "embed-test"
        }))
        .unwrap();
        assert_eq!(embedding.dimensions, 1536);
        assert_eq!(embedding.batch_size, 16);
        assert_eq!(embedding.timeout_secs, 30);
        assert_eq!(embedding.max_candidates, 64);
        assert!((embedding.min_similarity - 0.20).abs() < f32::EPSILON);
        assert!(embedding.is_configured());
        assert!(!MemoryEmbeddingPolicy::default().enabled);
        assert!(!MemoryEmbeddingPolicy::default().is_configured());

        let policy = MemoryPolicy {
            embedding: Some(embedding),
            ..MemoryPolicy::default()
        };
        let encoded = serde_json::to_value(&policy).unwrap();
        assert_eq!(encoded["embedding"]["model"], "embed-test");
        let decoded: MemoryPolicy = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, policy);

        let summary: MemorySemanticSummary = serde_json::from_value(json!({
            "enabled": true,
            "model": "embed-test",
            "lexical_candidates": 4,
            "semantic_candidates": 2,
            "fallback": null
        }))
        .unwrap();
        assert!(summary.enabled);
        assert_eq!(summary.semantic_candidates, 2);
        assert!(summary.fallback.is_none());

        let status: MemoryEmbeddingsStatusResult = serde_json::from_value(json!({
            "project_id": "prj-1",
            "enabled": true,
            "configured": true,
            "model": "embed-test",
            "dimensions": 8,
            "indexable": 2,
            "indexed": 1,
            "pending": 1,
            "failed": 0,
            "skipped": 0,
            "coverage": 0.5,
            "usage": {
                "calls": 2,
                "input_tokens": 12,
                "estimated_tokens": 3,
                "bytes": 48,
                "provider_reported_calls": 1
            },
            "recent_jobs": []
        }))
        .unwrap();
        assert_eq!(status.indexed, 1);
        assert_eq!(status.invalid, 0);
        assert_eq!(status.usage.calls, 2);
        assert!(status.last_error.is_none());

        let probe: MemoryEmbeddingsTestResult = serde_json::from_value(json!({
            "project_id": "prj-1",
            "ok": true,
            "model": "embed-test",
            "dimensions": 8,
            "latency_ms": 3,
            "input_tokens": 2,
            "estimated_tokens": 0,
            "provider_reported": true
        }))
        .unwrap();
        assert!(probe.ok);
        assert!(probe.error.is_none());

        let params: MemorySearchParams =
            serde_json::from_value(json!({"project_id": "prj-1", "query": "q"})).unwrap();
        assert!(!params.lexical_only);
        let forced: MemorySearchParams = serde_json::from_value(
            json!({"project_id": "prj-1", "query": "q", "lexical_only": true}),
        )
        .unwrap();
        assert!(forced.lexical_only);
    }

    #[test]
    fn hybrid_embedding_rpc_types_decode_defaults_and_round_trip() {
        let status: MemoryEmbeddingsStatusResult = serde_json::from_value(json!({
            "project_id": "prj-1",
            "indexable": 3,
            "indexed": 2
        }))
        .unwrap();
        assert!(!status.enabled);
        assert!(!status.configured);
        assert!(status.model.is_none());
        assert_eq!(status.dimensions, 1536);
        assert_eq!(status.invalid, 0);
        assert_eq!(status.pending, 0);
        assert_eq!(status.failed, 0);
        assert_eq!(status.skipped, 0);
        assert_eq!(status.coverage, 0.0);
        assert_eq!(status.usage, MemoryEmbeddingUsage::default());
        assert!(status.recent_jobs.is_empty());
        assert!(status.last_error.is_none());

        let rebuild: MemoryEmbeddingsRebuildParams =
            serde_json::from_value(json!({"project_id": "prj-1"})).unwrap();
        assert_eq!(rebuild.project_id, "prj-1");
        let result = MemoryEmbeddingsRebuildResult {
            project_id: "prj-1".to_string(),
            enabled: false,
            model: None,
            dimensions: 1536,
            cleared: 3,
            enqueued: 0,
        };
        let decoded: MemoryEmbeddingsRebuildResult =
            serde_json::from_value(serde_json::to_value(&result).unwrap()).unwrap();
        assert_eq!(decoded, result);

        let params: MemoryEmbeddingsTestParams =
            serde_json::from_value(json!({"project_id": "prj-1"})).unwrap();
        assert!(params.text.is_none());
        let params: MemoryEmbeddingsTestParams =
            serde_json::from_value(json!({"project_id": "prj-1", "text": "probe"})).unwrap();
        assert_eq!(params.text.as_deref(), Some("probe"));

        let matched = MemoryMatch {
            memory: serde_json::from_value(json!({
                "id": "mem-1",
                "session_id": "ses-1",
                "project_id": "prj-1",
                "agent_id": "agent-1",
                "agent_name": "Leader",
                "kind": "fact",
                "title": "Title",
                "body": "Body",
                "created_at": "2026-11-11T00:00:00Z"
            }))
            .unwrap(),
            score: 0.9,
            reasons: vec!["semantic".to_string(), "hybrid".to_string()],
            lexical_score: Some(0.2),
            semantic_score: Some(0.8),
            source: Some("hybrid".to_string()),
            semantic_unavailable: None,
        };
        let encoded = serde_json::to_value(&matched).unwrap();
        assert_eq!(encoded["lexical_score"], 0.2);
        assert_eq!(encoded["semantic_score"], 0.8);
        assert_eq!(encoded["source"], "hybrid");
        assert!(encoded["semantic_unavailable"].is_null());
        let decoded: MemoryMatch = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, matched);

        let summary = MemorySemanticSummary {
            enabled: true,
            model: Some("embed".to_string()),
            lexical_candidates: 3,
            semantic_candidates: 2,
            fallback: Some("endpoint unavailable".to_string()),
            index_source: Some("ivf".to_string()),
            index_generation: Some(4),
            index_fallback: None,
            probed_clusters: 8,
            candidate_vectors: 31,
        };
        let decoded: MemorySemanticSummary =
            serde_json::from_value(serde_json::to_value(&summary).unwrap()).unwrap();
        assert_eq!(decoded, summary);
    }

    #[test]
    fn ivf_index_policy_and_status_are_additive_and_default_off() {
        let minimal: MemoryEmbeddingPolicy = serde_json::from_value(json!({
            "enabled": true,
            "endpoint": "http://127.0.0.1:9/v1",
            "model": "embed-test"
        }))
        .unwrap();
        assert_eq!(minimal.index, MemoryIndexMode::Scan);
        assert_eq!(minimal.ivf_nlist, 64);
        assert_eq!(minimal.ivf_nprobe, 8);
        assert_eq!(minimal.ivf_min_vectors, 256);
        assert_eq!(minimal.ivf_rebuild_vectors, 4096);
        assert!(!minimal.uses_ivf_index());

        let ivf: MemoryEmbeddingPolicy = serde_json::from_value(json!({
            "enabled": true,
            "endpoint": "http://127.0.0.1:9/v1",
            "model": "embed-test",
            "index": "ivf",
            "ivf_nlist": 32,
            "ivf_nprobe": 4
        }))
        .unwrap();
        assert_eq!(ivf.index, MemoryIndexMode::Ivf);
        assert!(ivf.uses_ivf_index());
        assert_eq!(ivf.index.as_str(), "ivf");
        let invalid = serde_json::from_value::<MemoryEmbeddingPolicy>(json!({
            "enabled": true,
            "endpoint": "http://127.0.0.1:9/v1",
            "model": "embed-test",
            "index": "hnsw"
        }));
        assert!(invalid.is_err(), "unknown index modes fail closed");

        let index: MemoryEmbeddingIndexStatus = serde_json::from_value(json!({
            "mode": "ivf",
            "ready": true,
            "generation": 7,
            "model": "embed-test",
            "dimensions": 8,
            "vector_count": 400,
            "assigned_vectors": 380,
            "invalid_assignments": 2,
            "cluster_count": 32,
            "nlist": 32,
            "nprobe": 4,
            "coverage": 0.95,
            "min_vectors": 256,
            "rebuild_vectors": 4096,
            "bytes": 1024,
            "last_rebuilt_at": "2026-11-11T00:00:00Z",
            "fallback_count": 2,
            "last_fallback_reason": "model_mismatch"
        }))
        .unwrap();
        assert!(index.ready);
        assert_eq!(index.generation, Some(7));
        assert_eq!(index.coverage_label(), "95.0%");
        assert_eq!(index.invalid_assignments, 2);
        assert_eq!(index.fallback_count, 2);
        assert!(index.last_error.is_none());
        let encoded = serde_json::to_value(&index).unwrap();
        assert_eq!(encoded["mode"], "ivf");
        assert!(encoded.get("centroids").is_none());
        assert!(encoded.get("vector").is_none());
        let decoded: MemoryEmbeddingIndexStatus = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(decoded, index);

        let defaulted: MemoryEmbeddingIndexStatus = serde_json::from_value(json!({})).unwrap();
        assert_eq!(defaulted.mode, "scan");
        assert!(!defaulted.ready);
        assert_eq!(defaulted.dimensions, 1536);
        assert_eq!(defaulted.nlist, 64);
        assert_eq!(defaulted.nprobe, 8);
        assert_eq!(defaulted.invalid_assignments, 0);
        assert_eq!(defaulted.fallback_count, 0);

        let status = MemoryEmbeddingsStatusResult {
            project_id: "prj-1".to_string(),
            enabled: true,
            configured: true,
            model: Some("embed-test".to_string()),
            dimensions: 8,
            indexable: 400,
            indexed: 400,
            invalid: 0,
            pending: 0,
            failed: 0,
            skipped: 0,
            coverage: 1.0,
            last_error: None,
            usage: MemoryEmbeddingUsage::default(),
            recent_jobs: Vec::new(),
            index: Some(index.clone()),
        };
        let decoded: MemoryEmbeddingsStatusResult =
            serde_json::from_value(serde_json::to_value(&status).unwrap()).unwrap();
        assert_eq!(decoded.index.as_ref().unwrap().generation, Some(7));

        let legacy: MemoryEmbeddingsStatusResult = serde_json::from_value(json!({
            "project_id": "old",
            "indexable": 0
        }))
        .unwrap();
        assert!(legacy.index.is_none());

        let params: MemoryEmbeddingsIndexStatusParams =
            serde_json::from_value(json!({"project_id": "prj-1"})).unwrap();
        assert_eq!(params.project_id, "prj-1");
        let result = MemoryEmbeddingsIndexStatusResult {
            project_id: "prj-1".to_string(),
            index,
        };
        let decoded: MemoryEmbeddingsIndexStatusResult =
            serde_json::from_value(serde_json::to_value(&result).unwrap()).unwrap();
        assert_eq!(decoded, result);

        let replay = MemoryEmbeddingsIndexRebuildResult {
            project_id: "prj-1".to_string(),
            rebuilt: true,
            reason: None,
            generation: Some(8),
            vector_count: 400,
            assigned_vectors: 400,
            cluster_count: 32,
            nlist: 32,
        };
        let decoded: MemoryEmbeddingsIndexRebuildResult =
            serde_json::from_value(serde_json::to_value(&replay).unwrap()).unwrap();
        assert_eq!(decoded, replay);

        let minimal_rebuild: MemoryEmbeddingsIndexRebuildResult =
            serde_json::from_value(json!({"project_id": "prj-1"})).unwrap();
        assert!(!minimal_rebuild.rebuilt);
        assert_eq!(minimal_rebuild.generation, None);
        assert_eq!(minimal_rebuild.vector_count, 0);
        assert_eq!(minimal_rebuild.nlist, 0);

        let summary: MemorySemanticSummary = serde_json::from_value(json!({
            "enabled": true,
            "index_source": "ivf",
            "index_generation": 8,
            "index_fallback": null,
            "probed_clusters": 4,
            "candidate_vectors": 48
        }))
        .unwrap();
        assert_eq!(summary.index_source.as_deref(), Some("ivf"));
        assert_eq!(summary.index_generation, Some(8));
        assert_eq!(summary.probed_clusters, 4);
        assert_eq!(summary.candidate_vectors, 48);
    }

    #[test]
    fn session_context_status_serializes_compaction_metrics() {
        let status = SessionContextStatus {
            session_id: "ses-1".into(),
            usage: None,
            last_compaction: None,
            last_measurement: None,
            compaction_count: 3,
            last_compaction_latency_ms: 25,
            last_compaction_tokens_before: 9_000,
            last_compaction_tokens_after: 3_000,
            compaction_background_reuse: true,
            compaction_fallback_to_sync: false,
            checkpoint_count: 2,
            latest_checkpoint_id: Some("ctx-1".into()),
            latest_checkpoint_at: Some("2026-11-11T00:00:00Z".into()),
        };
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(value["compaction_count"], 3);
        assert_eq!(value["last_compaction_latency_ms"], 25);
        assert_eq!(value["last_compaction_tokens_before"], 9_000);
        assert_eq!(value["last_compaction_tokens_after"], 3_000);
        assert_eq!(value["compaction_background_reuse"], true);
        assert_eq!(value["last_compaction"], Value::Null);
        assert_eq!(value["checkpoint_count"], 2);
        assert_eq!(value["latest_checkpoint_id"], "ctx-1");
    }

    #[test]
    fn modern_compaction_summary_round_trips_metrics() {
        let summary = ContextCompactionSummary {
            mode: "summary".into(),
            summarized_messages: 8,
            dropped_messages: 1,
            kept_messages: 4,
            estimated_tokens_before: 10_000,
            estimated_tokens_after: 4_000,
            latency_ms: 42,
            background_reuse: true,
            fallback_to_sync: false,
            compaction_count: 3,
            structured_state: Some(ContextStructuredState {
                goal: "ship C1".into(),
                plan_revision: 2,
                active_task_id: Some("T1".into()),
                active_task_title: Some("paging".into()),
                active_task_department: Some("runtime".into()),
                active_task_acceptance: vec!["tests".into()],
                active_task_write_scope: vec!["crates/vp-core".into()],
                recent_completed_task_ids: vec!["T0".into()],
                generated_at: "2026-11-11T00:00:00Z".into(),
            }),
            cache_generation_before: Some("before".into()),
            cache_generation_after: Some("after".into()),
            cache_reset_reason: Some("compaction".into()),
            cache_invalidation_tokens: 6_000,
            cache_invalidation_estimated_usd: 0.0162,
            cache_compact_aware: true,
            cache_epoch_before: Some("a".repeat(64)),
            cache_epoch_after: Some("a".repeat(64)),
            cache_epoch_changed: false,
            estimated_lost_cache_read_usd: 0.0162,
            created_at: "2026-11-11T00:00:00Z".into(),
        };
        let encoded = serde_json::to_string(&summary).unwrap();
        let decoded: ContextCompactionSummary = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, summary);
    }

    #[test]
    fn structured_state_round_trips() {
        let state = ContextStructuredState {
            goal: "ship C1".into(),
            plan_revision: 2,
            active_task_id: Some("T2".into()),
            active_task_title: Some("tool output store".into()),
            active_task_department: Some("runtime".into()),
            active_task_acceptance: vec!["tests pass".into()],
            active_task_write_scope: vec!["crates/vp-core".into()],
            recent_completed_task_ids: vec!["T1".into()],
            generated_at: "2026-11-11T00:00:00Z".into(),
        };
        let encoded = serde_json::to_string(&state).unwrap();
        let decoded: ContextStructuredState = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, state);
    }

    #[test]
    fn context_checkpoint_round_trips_and_decodes_with_defaults() {
        let checkpoint = ContextCheckpoint {
            id: "ctx-1".into(),
            session_id: "ses-1".into(),
            project_id: "prj-1".into(),
            label: "before refactor".into(),
            mode: "summary".into(),
            summary: Some("Earlier conversation summary\n\nwork".into()),
            through_message_id: Some("msg-9".into()),
            message_cursor: 10,
            plan_revision: 3,
            provider: "openai-compatible".into(),
            model: "test-model".into(),
            prompt_prefix_hash: "abc123".into(),
            worktree_head: "deadbeef".into(),
            tokens_before: 90_000,
            tokens_after: 12_000,
            structured_state: Some(ContextStructuredState {
                goal: "ship D1".into(),
                plan_revision: 3,
                ..ContextStructuredState::default()
            }),
            source_compaction_seq: Some(42),
            automatic: true,
            created_at: "2026-11-11T00:00:00Z".into(),
        };
        let encoded = serde_json::to_string(&checkpoint).unwrap();
        let decoded: ContextCheckpoint = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, checkpoint);

        let minimal: ContextCheckpoint = serde_json::from_value(json!({
            "id": "ctx-2",
            "session_id": "ses-1",
            "project_id": "prj-1"
        }))
        .unwrap();
        assert_eq!(minimal.id, "ctx-2");
        assert_eq!(minimal.mode, "");
        assert_eq!(minimal.message_cursor, 0);
        assert_eq!(minimal.tokens_after, 0);
        assert!(minimal.summary.is_none());
        assert!(!minimal.automatic);
    }

    #[test]
    fn context_checkpoint_policy_defaults_and_clamps_are_typed() {
        let policy = ContextCheckpointPolicy::default();
        assert!(!policy.enabled);
        assert_eq!(policy.max_checkpoints, 20);
        assert_eq!(policy.max_age_days, 30);
        let decoded: ContextCheckpointPolicy =
            serde_json::from_value(json!({"enabled": true})).unwrap();
        assert!(decoded.enabled);
        assert_eq!(decoded.max_checkpoints, 20);
        assert_eq!(decoded.max_age_days, 30);
        let custom: ContextCheckpointPolicy = serde_json::from_value(json!({
            "enabled": true,
            "max_checkpoints": 5,
            "max_age_days": 0
        }))
        .unwrap();
        assert_eq!(custom.max_checkpoints, 5);
        assert_eq!(custom.max_age_days, 0);
    }

    #[test]
    fn context_restore_params_and_results_are_backward_compatible() {
        let params: ContextRestoreParams = serde_json::from_value(json!({
            "session_id": "ses-1",
            "checkpoint_id": "ctx-1"
        }))
        .unwrap();
        assert!(!params.force);
        let forced: ContextRestoreParams = serde_json::from_value(json!({
            "session_id": "ses-1",
            "checkpoint_id": "ctx-1",
            "force": true
        }))
        .unwrap();
        assert!(forced.force);
        let result = ContextRestoreResult {
            session_id: "ses-1".into(),
            checkpoint_id: "ctx-1".into(),
            mode: "summary".into(),
            through_message_id: Some("msg-9".into()),
            active_messages: 4,
            estimated_tokens: 3_000,
            hard_limit_tokens: 32_000,
            warning: None,
            compaction_event_seq: 43,
            restored_event_seq: 44,
        };
        assert_eq!(result.active_messages, 4);
        assert_eq!(result.compaction_event_seq, 43);
        assert_eq!(result.restored_event_seq, 44);
    }

    #[test]
    fn older_session_bundle_decodes_with_empty_context_checkpoints() {
        let value = json!({
            "version": 1,
            "exported_at": "2026-11-11T00:00:00Z",
            "project": {
                "id": "prj-1",
                "name": "p",
                "root": "/tmp/p",
                "default_branch": "main",
                "created_at": "2026-11-11T00:00:00Z"
            },
            "session": {
                "id": "ses-1",
                "project_id": "prj-1",
                "goal": "g",
                "status": "idle",
                "priority": 0,
                "worktree": null,
                "branch": null,
                "token_budget": null,
                "cost_budget_usd": null,
                "time_budget_secs": null,
                "unlimited": false,
                "cost_spent_usd": 0.0,
                "base_commit": null,
                "tokens_in": 0,
                "tokens_out": 0,
                "created_at": "2026-11-11T00:00:00Z"
            },
            "events": [],
            "agents": [],
            "messages": [],
            "todos": [],
            "merge_queue": []
        });
        let bundle: SessionBundle = serde_json::from_value(value).unwrap();
        assert!(bundle.context_checkpoints.is_empty());
        assert!(bundle.session_runs.is_empty());
    }
}

#[cfg(test)]
mod context_checkpoint_protocol_tests {
    use super::*;
    use serde_json::{Value, json};

    fn checkpoint(id: &str) -> ContextCheckpoint {
        ContextCheckpoint {
            id: id.into(),
            session_id: "ses-1".into(),
            project_id: "prj-1".into(),
            label: format!("checkpoint {id}"),
            mode: "summary".into(),
            summary: Some("Earlier conversation summary".into()),
            through_message_id: Some("msg-1".into()),
            message_cursor: 1,
            plan_revision: 2,
            provider: "queue".into(),
            model: "test".into(),
            prompt_prefix_hash: "prefix".into(),
            worktree_head: "head".into(),
            tokens_before: 9_000,
            tokens_after: 900,
            structured_state: Some(ContextStructuredState::default()),
            source_compaction_seq: Some(4),
            automatic: true,
            created_at: "2026-11-11T00:00:00Z".into(),
        }
    }

    #[test]
    fn context_checkpoint_list_serializes_policy_and_latest_id() {
        let result = ContextCheckpointListResult {
            session_id: "ses-1".into(),
            checkpoints: vec![checkpoint("ctx-1")],
            latest_id: Some("ctx-1".into()),
            policy: ContextCheckpointPolicy {
                enabled: true,
                max_checkpoints: 5,
                max_age_days: 7,
            },
            limit: 50,
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["session_id"], "ses-1");
        assert_eq!(value["latest_id"], "ctx-1");
        assert_eq!(value["checkpoints"][0]["mode"], "summary");
        assert_eq!(value["checkpoints"][0]["tokens_before"], 9_000);
        assert_eq!(value["policy"]["max_checkpoints"], 5);
        assert_eq!(value["limit"], 50);
    }

    #[test]
    fn context_checkpoint_detail_serializes_preflight_and_preview() {
        let result = ContextCheckpointDetailResult {
            checkpoint: checkpoint("ctx-1"),
            preflight: ContextRestorePreflight {
                restorable: false,
                errors: vec!["plan revision changed".into()],
                warnings: vec!["above threshold".into()],
                estimated_tokens: 1_200,
                hard_limit_tokens: 32_000,
                threshold_tokens: 1_000,
                active_messages: 4,
                plan_revision: 3,
                checkpoint_plan_revision: 2,
                provider: "queue".into(),
                model: "test".into(),
                worktree_head: "head".into(),
                current_worktree_head: Some("head".into()),
                worktree_descendant: true,
                budget_exhausted: false,
            },
            is_active: false,
            summary_preview: "Earlier conversation summary".into(),
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["preflight"]["restorable"], false);
        assert_eq!(value["preflight"]["errors"][0], "plan revision changed");
        assert_eq!(value["preflight"]["checkpoint_plan_revision"], 2);
        assert_eq!(value["is_active"], false);
        assert_eq!(value["summary_preview"], "Earlier conversation summary");

        let decoded: ContextRestorePreflight = serde_json::from_value(json!({
            "restorable": true,
            "errors": [],
            "warnings": []
        }))
        .unwrap();
        assert!(decoded.restorable);
        assert_eq!(decoded.estimated_tokens, 0);
        assert_eq!(decoded.active_messages, 0);
        assert!(!decoded.worktree_descendant);
        assert!(decoded.current_worktree_head.is_none());
    }

    #[test]
    fn context_checkpoint_action_params_decode_with_defaults() {
        let list: ContextCheckpointListParams =
            serde_json::from_value(json!({"session_id": "ses-1"})).unwrap();
        assert!(list.limit.is_none());
        let create: ContextCheckpointCreateParams =
            serde_json::from_value(json!({"session_id": "ses-1"})).unwrap();
        assert!(create.label.is_none());

        let detail: ContextCheckpointDetailParams = serde_json::from_value(json!({
            "session_id": "ses-1",
            "checkpoint_id": "ctx-1"
        }))
        .unwrap();
        assert_eq!(detail.checkpoint_id, "ctx-1");

        let delete: ContextCheckpointDeleteParams = serde_json::from_value(json!({
            "session_id": "ses-1",
            "checkpoint_id": "ctx-1"
        }))
        .unwrap();
        assert_eq!(delete.session_id, "ses-1");

        let prune: ContextCheckpointPruneParams = serde_json::from_value(json!({
            "session_id": "ses-1"
        }))
        .unwrap();
        assert!(prune.older_than_days.is_none());
        assert!(prune.keep_latest.is_none());
        assert!(!prune.dry_run);
        let prune: ContextCheckpointPruneParams = serde_json::from_value(json!({
            "session_id": "ses-1",
            "older_than_days": 7,
            "keep_latest": 2,
            "dry_run": true
        }))
        .unwrap();
        assert_eq!(prune.older_than_days, Some(7));
        assert_eq!(prune.keep_latest, Some(2));
        assert!(prune.dry_run);

        let policy: SetContextCheckpointPolicyParams = serde_json::from_value(json!({
            "project_id": "prj-1",
            "policy": {"enabled": true, "max_checkpoints": 4, "max_age_days": 0}
        }))
        .unwrap();
        assert_eq!(policy.project_id, "prj-1");
        assert!(policy.policy.enabled);
        assert_eq!(policy.policy.max_age_days, 0);
    }

    #[test]
    fn context_checkpoint_prune_and_delete_results_are_stable_shapes() {
        let prune = ContextCheckpointPruneResult {
            session_id: "ses-1".into(),
            candidates: 3,
            removed: 2,
            dry_run: false,
            checkpoints: vec![checkpoint("ctx-1")],
        };
        let value: Value = serde_json::to_value(&prune).unwrap();
        assert_eq!(value["candidates"], 3);
        assert_eq!(value["removed"], 2);
        assert_eq!(value["checkpoints"][0]["id"], "ctx-1");
        let delete = ContextCheckpointDeleteResult {
            session_id: "ses-1".into(),
            checkpoint_id: "ctx-1".into(),
            removed: true,
        };
        let value = serde_json::to_value(&delete).unwrap();
        assert_eq!(value["checkpoint_id"], "ctx-1");
        assert_eq!(value["removed"], true);
    }

    #[test]
    fn context_restore_result_is_additive_and_typed() {
        let result = ContextRestoreResult {
            session_id: "ses-1".into(),
            checkpoint_id: "ctx-1".into(),
            mode: "summary".into(),
            through_message_id: None,
            active_messages: 2,
            estimated_tokens: 500,
            hard_limit_tokens: 32_000,
            warning: Some("threshold".into()),
            compaction_event_seq: 11,
            restored_event_seq: 12,
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["through_message_id"], Value::Null);
        assert_eq!(value["warning"], "threshold");
        assert_eq!(value["compaction_event_seq"], 11);
        assert_eq!(value["restored_event_seq"], 12);
    }
}

#[cfg(test)]
mod context_checkpoint_bundle_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn session_bundle_round_trips_context_checkpoints() {
        let bundle = SessionBundle {
            version: 1,
            exported_at: "2026-11-11T00:00:00Z".into(),
            project: Project {
                id: "prj-1".into(),
                name: "p".into(),
                root: "/tmp/p".into(),
                default_branch: "main".into(),
                created_at: "2026-11-11T00:00:00Z".into(),
            },
            session: Session {
                id: "ses-1".into(),
                project_id: "prj-1".into(),
                goal: "g".into(),
                status: "idle".into(),
                priority: 0,
                worktree: None,
                branch: None,
                token_budget: None,
                cost_budget_usd: None,
                time_budget_secs: None,
                unlimited: false,
                cost_spent_usd: 0.0,
                base_commit: None,
                tokens_in: 0,
                tokens_out: 0,
                prompt_layout: 2,
                created_at: "2026-11-11T00:00:00Z".into(),
            },
            events: Vec::new(),
            agents: Vec::new(),
            messages: Vec::new(),
            team_plan: None,
            todos: Vec::new(),
            approvals: Vec::new(),
            merge_queue: Vec::new(),
            agent_messages: Vec::new(),
            artifacts: Vec::new(),
            usage: Vec::new(),
            memories: Vec::new(),
            memory_conflicts: Vec::new(),
            questions: Vec::new(),
            tasks: Vec::new(),
            steering: Vec::new(),
            continuations: Vec::new(),
            gates: Vec::new(),
            policy_snapshots: Vec::new(),
            recoveries: Vec::new(),
            programs: Vec::new(),
            program_milestones: Vec::new(),
            program_events: Vec::new(),
            session_runs: Vec::new(),
            run_steps: Vec::new(),
            run_checkpoints: Vec::new(),
            context_checkpoints: vec![ContextCheckpoint {
                id: "ctx-1".into(),
                session_id: "ses-1".into(),
                project_id: "prj-1".into(),
                label: "bundle".into(),
                mode: "summary".into(),
                summary: Some("summary".into()),
                through_message_id: Some("msg-1".into()),
                message_cursor: 1,
                plan_revision: 1,
                provider: "queue".into(),
                model: "test".into(),
                prompt_prefix_hash: "prefix".into(),
                worktree_head: "head".into(),
                tokens_before: 10,
                tokens_after: 1,
                structured_state: None,
                source_compaction_seq: Some(3),
                automatic: true,
                created_at: "2026-11-11T00:00:00Z".into(),
            }],
            supervised_processes: Vec::new(),
            test_results: Vec::new(),
            instruction_blobs: Vec::new(),
            instructions: Vec::new(),
            prompt_prefix_observations: Vec::new(),
            effects: vec![EffectReceipt {
                id: "effect-1".into(),
                session_id: "ses-1".into(),
                state: EffectState::Done,
                target: EffectTarget::new("workspace_write", "src/lib.rs"),
                evidence: EffectEvidence::observed("workspace_sha256"),
                ..Default::default()
            }],
            agent_identities: Vec::new(),
            agent_identity_events: Vec::new(),
            session_agent_assignments: Vec::new(),
            agent_inbox: Vec::new(),
            agent_coordination_events: Vec::new(),
            agent_workspaces: Vec::new(),
            session_worker_workspaces: Vec::new(),
            agent_workspace_events: Vec::new(),
        };
        let encoded = serde_json::to_string(&bundle).unwrap();
        let decoded: SessionBundle = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.context_checkpoints, bundle.context_checkpoints);
        assert_eq!(decoded.effects, bundle.effects);
        assert!(decoded.agent_identities.is_empty());
        let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(value["context_checkpoints"][0]["id"], "ctx-1");
        assert_eq!(value["context_checkpoints"][0]["source_compaction_seq"], 3);
        assert_eq!(value["context_checkpoints"][0]["automatic"], true);
    }

    #[test]
    fn session_bundle_defaults_absent_context_checkpoints() {
        let mut value = json!({
            "version": 1,
            "exported_at": "2026-11-11T00:00:00Z",
            "project": {
                "id": "prj-1",
                "name": "p",
                "root": "/tmp/p",
                "default_branch": "main",
                "created_at": "2026-11-11T00:00:00Z"
            },
            "session": {
                "id": "ses-1",
                "project_id": "prj-1",
                "goal": "g",
                "status": "idle",
                "priority": 0,
                "unlimited": false,
                "cost_spent_usd": 0.0,
                "tokens_in": 0,
                "tokens_out": 0,
                "created_at": "2026-11-11T00:00:00Z"
            },
            "events": [],
            "agents": [],
            "messages": [],
            "todos": [],
            "merge_queue": []
        });
        value.as_object_mut().unwrap().remove("context_checkpoints");
        let bundle: SessionBundle = serde_json::from_value(value).unwrap();
        assert!(bundle.context_checkpoints.is_empty());
        assert!(bundle.agent_identities.is_empty());
        assert!(bundle.agent_identity_events.is_empty());
    }

    #[test]
    fn gui_policy_defaults_and_legacy_payloads_decode() {
        let policy = GuiPolicy::default();
        assert_eq!(policy.grace_secs, 45);
        assert_eq!(policy.wait_ms, 15_000);
        assert_eq!(policy.status_ttl_secs, 30);
        assert_eq!(policy.sensitive_mode, GuiSensitiveMode::Deny);
        let legacy: GuiPolicy = serde_json::from_value(json!({})).unwrap();
        assert_eq!(legacy, policy);
        let decoded: GuiPolicy = serde_json::from_value(json!({
            "grace_secs": 0,
            "wait_ms": 2500,
            "status_ttl_secs": 10,
            "sensitive_mode": "require_approval",
            "sensitive_bundles": ["com.example.Vault"],
            "sensitive_titles": ["Vault"]
        }))
        .unwrap();
        assert_eq!(decoded.sensitive_mode, GuiSensitiveMode::RequireApproval);
        assert_eq!(decoded.grace_secs, 0);
        assert_eq!(decoded.wait_ms, 2500);
        assert_eq!(decoded.sensitive_bundles, vec!["com.example.Vault"]);
        assert!(GuiSensitiveMode::parse("APPROVAL").is_some());
        assert!(GuiSensitiveMode::parse("unknown").is_none());
    }

    #[test]
    fn gui_status_and_timeline_round_trip_with_defaults() {
        let status = GuiStatusResult {
            runtime_id: "runtime-1".into(),
            lease_surface: "gui".into(),
            holder: Some(GuiLeaseInfo {
                lease_id: "lease-1".into(),
                owner_id: "owner-1".into(),
                session_id: "ses-1".into(),
                agent_id: "agent-1".into(),
                agent_name: "Agent One".into(),
                surface: "computer".into(),
                lease_generation: 4,
                acquired_at: "2026-11-11T00:00:00Z".into(),
                last_action_at: "2026-11-11T00:00:01Z".into(),
                expires_at: "2026-11-11T00:00:46Z".into(),
                queue_position: None,
            }),
            queue: vec![GuiQueueInfo {
                session_id: "ses-2".into(),
                agent_id: "agent-2".into(),
                agent_name: "Agent Two".into(),
                surface: "browser".into(),
                position: 1,
                waiting_ms: 1200,
            }],
            policy: GuiPolicy::default(),
            computer: ComputerStatusResult {
                lease_surface: "gui".into(),
                ..ComputerStatusResult::default()
            },
            browser_helper_available: true,
            browser_helper_path: Some("/tmp/VerlinParkBrowserHelper".into()),
            browser_sandbox_available: true,
            warm_computer_helpers: 1,
            warm_browser_helpers: 2,
            stale_ref_supported: true,
        };
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(value["holder"]["lease_generation"], 4);
        assert_eq!(value["queue"][0]["position"], 1);
        let decoded: GuiStatusResult = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, status);

        let action = GuiActionInfo {
            id: "action-1".into(),
            session_id: "ses-1".into(),
            agent_id: "agent-1".into(),
            agent_name: "Agent One".into(),
            surface: "computer".into(),
            action: "snapshot".into(),
            target_app: Some("Example".into()),
            target_bundle_id: Some("com.example.App".into()),
            target_window: Some("Main".into()),
            sensitive_target: false,
            snapshot_generation: 7,
            lease_generation: 4,
            queued_ms: 0,
            latency_ms: 18,
            result: "ok".into(),
            error_code: None,
            occurred_at: "2026-11-11T00:00:02Z".into(),
        };
        let value = serde_json::to_value(&action).unwrap();
        assert_eq!(value["snapshot_generation"], 7);
        let legacy: GuiActionInfo = serde_json::from_value(json!({
            "id": "a",
            "session_id": "s",
            "agent_id": "g",
            "agent_name": "A",
            "surface": "computer",
            "action": "apps",
            "snapshot_generation": 0,
            "lease_generation": 1,
            "queued_ms": 0,
            "latency_ms": 2,
            "result": "ok",
            "occurred_at": "2026-11-11T00:00:00Z"
        }))
        .unwrap();
        assert!(legacy.target_app.is_none());
        assert!(!legacy.sensitive_target);
    }

    #[test]
    fn computer_status_tolerates_legacy_fields_and_reports_gui_defaults() {
        let status: ComputerStatusResult = serde_json::from_value(json!({
            "platform_supported": true,
            "helper_available": true,
            "permissions_checked": false,
            "lease_surface": "computer"
        }))
        .unwrap();
        assert!(!status.cached);
        assert!(status.cache_age_ms.is_none());
        assert_eq!(status.grace_secs, 0);
        assert_eq!(status.wait_ms, 0);
        assert!(!status.snapshot_generations);
    }

    #[test]
    fn computer_lease_info_tolerates_a_missing_generation() {
        let info: ComputerLeaseInfo = serde_json::from_value(json!({
            "lease_id": "lease-1",
            "surface": "computer",
            "owner_id": "owner-1",
            "session_id": "ses-1",
            "agent_id": "agent-1",
            "agent_name": "Agent One",
            "acquired_at": "2026-11-11T00:00:00Z",
            "heartbeat_at": "2026-11-11T00:00:01Z",
            "expires_at": "2026-11-11T00:00:30Z"
        }))
        .unwrap();
        assert_eq!(info.generation, 0);
        let current = ComputerLeaseInfo {
            generation: 9,
            ..info
        };
        let value = serde_json::to_value(&current).unwrap();
        assert_eq!(value["generation"], 9);
    }

    #[test]
    fn gui_policy_validation_rejects_out_of_range_values_and_long_entries() {
        let mut policy = GuiPolicy {
            grace_secs: 601,
            ..GuiPolicy::default()
        };
        assert!(policy.validate().is_err());
        policy.grace_secs = 0;
        policy.wait_ms = 60_001;
        assert!(policy.validate().is_err());
        policy.wait_ms = 0;
        policy.status_ttl_secs = 4;
        assert!(policy.validate().is_err());
        policy.status_ttl_secs = 601;
        assert!(policy.validate().is_err());
        policy.status_ttl_secs = 30;
        policy.sensitive_titles = vec!["x".repeat(201)];
        assert!(policy.validate().is_err());
        policy.sensitive_titles = vec!["ok".into()];
        policy.sensitive_bundles = (0..65).map(|index| format!("b{index}")).collect();
        assert!(policy.validate().is_err());
        policy.sensitive_bundles = vec![" ".into()];
        assert!(policy.validate().is_err());
        policy.sensitive_bundles.clear();
        assert!(policy.validate().is_ok());
        let normalized = policy.normalized();
        assert_eq!(normalized.status_ttl_secs, 30);
    }

    #[test]
    fn merge_queue_entries_decode_legacy_rows_as_local() {
        let entry: MergeQueueEntry = serde_json::from_value(json!({
            "id": "e1",
            "session_id": "s1",
            "agent_id": "a1",
            "agent_name": "Worker",
            "branch": "verlinpark/s1--T1",
            "commit": "abcdef1234567890",
            "base_commit": "1234567890abcdef",
            "status": "merged",
            "attempt": 1,
            "created_at": "2026-11-11T00:00:00Z",
            "updated_at": "2026-11-11T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(entry.source, MERGE_SOURCE_LOCAL);
        assert!(entry.remote_host.is_none());
        assert!(entry.remote_branch.is_none());
        assert!(entry.bundle_sha256.is_none());
        assert!(entry.imported_ref.is_none());
        assert!(entry.verifier_approved.is_none());
        assert!(entry.verifier_summary.is_none());
        assert!(entry.environment_caveat.is_none());
        let remote = MergeQueueEntry {
            source: MERGE_SOURCE_REMOTE.into(),
            remote_host: Some("build.example.com".into()),
            remote_branch: Some("feature/x".into()),
            bundle_sha256: Some("a".repeat(64)),
            imported_ref: Some("refs/verlinpark/remote/s1/i1".into()),
            ..entry.clone()
        };
        let encoded = serde_json::to_value(&remote).unwrap();
        let decoded: MergeQueueEntry = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.source, MERGE_SOURCE_REMOTE);
        assert_eq!(decoded.remote_host.as_deref(), Some("build.example.com"));
        assert_eq!(
            decoded.imported_ref.as_deref(),
            Some("refs/verlinpark/remote/s1/i1")
        );
        let source = MergeQueueSource::remote(
            "build.example.com",
            "feature/x",
            "b".repeat(64),
            "refs/verlinpark/remote/s1/i1",
        );
        assert!(source.is_remote());
        assert_eq!(
            serde_json::from_value::<MergeQueueSource>(json!({})).unwrap(),
            MergeQueueSource::local()
        );
    }

    #[test]
    fn gui_request_params_default_to_no_scope() {
        let status: GuiStatusParams = serde_json::from_value(json!({})).unwrap();
        assert!(status.project_id.is_none());
        let timeline: GuiTimelineParams = serde_json::from_value(json!({})).unwrap();
        assert!(timeline.session_id.is_none());
        assert!(timeline.limit.is_none());
        let status: GuiStatusParams = serde_json::from_value(json!({
            "project_id": "prj-1"
        }))
        .unwrap();
        assert_eq!(status.project_id.as_deref(), Some("prj-1"));
    }

    #[test]
    fn source_search_mode_defaults_and_serializes_lowercase() {
        let params: SourceSearchParams = serde_json::from_value(json!({
            "project_id": "prj-1",
            "query": "leases"
        }))
        .unwrap();
        assert!(
            params.mode.is_none(),
            "absent mode keeps the dynamic default"
        );
        let semantic: SourceSearchParams = serde_json::from_value(json!({
            "project_id": "prj-1",
            "query": "leases",
            "mode": "semantic"
        }))
        .unwrap();
        assert_eq!(semantic.mode, Some(SourceSearchMode::Semantic));
        assert_eq!(SourceSearchMode::Hybrid.as_str(), "hybrid");
        assert_eq!(
            serde_json::to_value(SourceSearchMode::Hybrid).unwrap(),
            json!("hybrid")
        );
        assert_eq!(
            "lexical"
                .parse::<SourceSearchMode>()
                .expect("lexical mode parses"),
            SourceSearchMode::Lexical
        );
        assert!("vector".parse::<SourceSearchMode>().is_err());
    }

    #[test]
    fn legacy_source_search_hit_decodes_without_semantic_fields() {
        let hit: SourceSearchHit = serde_json::from_value(json!({
            "source_id": "src-1",
            "source_title": "Runtime",
            "chunk_id": "src-1:0001:abcdef",
            "chunk_no": 1,
            "content": "Deterministic leases.",
            "score": 1.25
        }))
        .unwrap();
        assert_eq!(hit.source, "lexical");
        assert_eq!(hit.confidence, 0.0);
        assert_eq!(hit.origins, vec!["lexical"]);
        assert!(hit.lexical_score.is_none());
        assert!(hit.semantic_score.is_none());
        let result: SourceSearchResult = serde_json::from_value(json!({
            "hits": [{
                "source_id": "src-1",
                "source_title": "Runtime",
                "chunk_id": "src-1:0001:abcdef",
                "chunk_no": 1,
                "content": "Deterministic leases.",
                "score": 1.25
            }],
            "degraded": false
        }))
        .unwrap();
        assert!(!result.degraded);
        assert_eq!(result.mode, "lexical");
        assert!(result.semantic_unavailable.is_none());
        assert_eq!(result.candidates_scanned, 0);
    }

    #[test]
    fn source_search_result_serializes_semantic_lineage() {
        let hit = SourceSearchHit {
            source_id: "src-1".into(),
            source_title: "Runtime".into(),
            chunk_id: "src-1:0001:abcdef".into(),
            chunk_no: 1,
            content: "Deterministic leases.".into(),
            score: 0.87,
            source: "hybrid".into(),
            confidence: 0.87,
            origins: vec!["lexical".into(), "semantic".into()],
            lexical_score: Some(1.4),
            semantic_score: Some(0.81),
            ..SourceSearchHit::default()
        };
        let result = SourceSearchResult {
            hits: vec![hit],
            degraded: false,
            mode: "hybrid".into(),
            requested_mode: "hybrid".into(),
            semantic_unavailable: Some("no_indexed_vectors".into()),
            candidates_scanned: 12,
            ..SourceSearchResult::default()
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["mode"], "hybrid");
        assert_eq!(value["hits"][0]["source"], "hybrid");
        assert_eq!(value["hits"][0]["origins"][0], "lexical");
        assert_eq!(value["candidates_scanned"], 12);
        assert!(value["hits"][0].get("vector").is_none());
        assert!(value["hits"][0].get("embedding").is_none());
    }

    #[test]
    fn source_embedding_policy_defaults_are_off_and_bounded() {
        let policy = SourceEmbeddingPolicy::default();
        assert!(!policy.enabled);
        assert!(!policy.is_configured());
        assert!(!policy.is_active());
        assert_eq!(policy.dimensions, 1536);
        assert_eq!(policy.batch_size, 8);
        assert_eq!(policy.max_chunks_per_source, 200);
        assert_eq!(policy.retention_days, 30);
        assert_eq!(policy.timeout_secs, 30);
        assert!(policy.endpoint_env.is_none());
        assert!(policy.api_key_env.is_none());

        let decoded: SourceEmbeddingPolicy = serde_json::from_value(json!({
            "enabled": true,
            "endpoint_env": "SOURCE_EMBED_ENDPOINT",
            "model": "embed-small"
        }))
        .unwrap();
        assert!(decoded.is_active());
        assert_eq!(decoded.batch_size, 8);
        assert_eq!(decoded.max_chunks_per_source, 200);
        assert_eq!(decoded.endpoint_env_name(), Some("SOURCE_EMBED_ENDPOINT"));
        assert_eq!(decoded.api_key_env_name(), None);
        assert_eq!(decoded.endpoint_env_name(), Some("SOURCE_EMBED_ENDPOINT"));
    }

    #[test]
    fn source_embedding_status_decodes_counts_and_jobs_without_vectors() {
        let status: SourceEmbeddingStatusResult = serde_json::from_value(json!({
            "project_id": "prj-1",
            "enabled": true,
            "configured": true,
            "endpoint_env": "SOURCE_EMBED_ENDPOINT",
            "endpoint_env_present": true,
            "model": "embed-small",
            "dimensions": 512,
            "indexable_chunks": 20,
            "indexed_chunks": 16,
            "stale_chunks": 2,
            "pending_jobs": 1,
            "failed_jobs": 1,
            "coverage": 0.8,
            "semantic_unavailable": "no_indexed_vectors",
            "usage": {
                "calls": 3,
                "input_tokens": 90,
                "estimated_tokens": 10,
                "bytes": 360,
                "provider_reported_calls": 2
            },
            "recent_jobs": [{
                "source_id": "src-1",
                "source_title": "Runtime",
                "status": "failed",
                "attempts": 2,
                "generation": 4,
                "chunks_total": 10,
                "chunks_done": 8,
                "last_error": "HTTP 503: busy",
                "next_attempt_at": "2026-11-11T00:10:00Z",
                "updated_at": "2026-11-11T00:00:00Z"
            }]
        }))
        .unwrap();
        assert_eq!(status.dimensions, 512);
        assert_eq!(status.indexed_chunks, 16);
        assert_eq!(status.pending_jobs, 1);
        assert_eq!(status.failed_jobs, 1);
        assert_eq!(status.coverage, 0.8);
        assert_eq!(status.recent_jobs[0].generation, 4);
        assert_eq!(status.recent_jobs[0].chunks_done, 8);
        assert_eq!(status.usage.provider_reported_calls, 2);
        assert_eq!(status.batch_size, 0, "absent additive fields default");
        assert_eq!(status.integrity.valid_vectors, 0);
        let encoded = serde_json::to_value(&status).unwrap();
        assert!(encoded.get("vectors").is_none());
        assert!(encoded.get("endpoint").is_none());
    }

    #[test]
    fn source_embedding_reindex_result_round_trips() {
        let result = SourceEmbeddingReindexResult {
            project_id: "prj-1".into(),
            enabled: true,
            model: Some("embed-small".into()),
            cleared_vectors: 8,
            cleared_jobs: 2,
            enqueued_sources: 3,
            max_chunks_per_source: 200,
        };
        let value = serde_json::to_value(&result).unwrap();
        let decoded: SourceEmbeddingReindexResult = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, result);
        let legacy: SourceEmbeddingReindexResult = serde_json::from_value(json!({
            "project_id": "prj-1"
        }))
        .unwrap();
        assert!(!legacy.enabled);
        assert_eq!(legacy.enqueued_sources, 0);
    }

    #[test]
    fn source_embedding_policy_round_trips_without_a_secret() {
        let policy = SourceEmbeddingPolicy {
            enabled: true,
            endpoint_env: Some("SOURCE_EMBED_ENDPOINT".into()),
            model: Some("embed-small".into()),
            dimensions: 512,
            batch_size: 4,
            max_chunks_per_source: 120,
            retention_days: 14,
            timeout_secs: 20,
            api_key_env: Some("SOURCE_EMBED_KEY".into()),
        };
        let value = serde_json::to_value(&policy).unwrap();
        assert_eq!(value["endpoint_env"], "SOURCE_EMBED_ENDPOINT");
        assert_eq!(value["api_key_env"], "SOURCE_EMBED_KEY");
        assert!(value.get("endpoint").is_none());
        assert!(value.get("api_key").is_none());
        let decoded: SourceEmbeddingPolicy = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, policy);
        assert!(decoded.is_active());
        assert_eq!(decoded.endpoint_env_name(), Some("SOURCE_EMBED_ENDPOINT"));
        assert_eq!(decoded.api_key_env_name(), Some("SOURCE_EMBED_KEY"));
    }

    #[test]
    fn legacy_source_embedding_status_decodes_additive_defaults() {
        let status: SourceEmbeddingStatusResult = serde_json::from_value(json!({
            "project_id": "prj-1",
            "enabled": false,
            "configured": false
        }))
        .unwrap();
        assert!(!status.enabled);
        assert_eq!(status.indexable_chunks, 0);
        assert_eq!(status.pending_jobs, 0);
        assert_eq!(status.usage.calls, 0);
        assert!(status.recent_jobs.is_empty());
        assert_eq!(status.integrity, SourceEmbeddingIntegrity::default());
        assert!(status.semantic_unavailable.is_none());
    }

    #[test]
    fn source_retrieval_policy_defaults_are_bounded_and_off() {
        let policy = SourceRetrievalPolicy::default();
        assert!(!policy.ann_enabled);
        assert_eq!(policy.ann_min_vectors, 256);
        assert_eq!(policy.ann_nprobe, 8);
        assert_eq!(policy.rerank, SourceRerankMode::Deterministic);
        assert_eq!(policy.query_expansion, SourceQueryExpansionMode::Off);
        assert_eq!(policy.max_expanded_terms, 8);

        let decoded: SourceRetrievalPolicy = serde_json::from_value(json!({
            "ann_enabled": true,
            "ann_nprobe": 4
        }))
        .unwrap();
        assert!(decoded.ann_enabled);
        assert_eq!(decoded.ann_nprobe, 4);
        assert_eq!(decoded.ann_min_vectors, 256);
        assert_eq!(decoded.rerank, SourceRerankMode::Deterministic);

        let legacy: SourceRetrievalPolicy = serde_json::from_value(json!({})).unwrap();
        assert_eq!(legacy, SourceRetrievalPolicy::default());
        assert_eq!(SourceRerankMode::Off.as_str(), "off");
        assert_eq!(SourceRerankMode::Deterministic.as_str(), "deterministic");
        assert_eq!(
            "off".parse::<SourceRerankMode>().unwrap(),
            SourceRerankMode::Off
        );
        assert!("model".parse::<SourceRerankMode>().is_err());
        assert_eq!(
            "lexical".parse::<SourceQueryExpansionMode>().unwrap(),
            SourceQueryExpansionMode::Lexical
        );
        assert!("semantic".parse::<SourceQueryExpansionMode>().is_err());
    }

    #[test]
    fn source_retrieval_policy_decodes_closed_modes_and_rejects_unknown_values() {
        let policy: SourceRetrievalPolicy = serde_json::from_value(json!({
            "ann_enabled": true,
            "ann_min_vectors": 4,
            "ann_nprobe": 2,
            "rerank": "off",
            "query_expansion": "lexical",
            "max_expanded_terms": 3
        }))
        .unwrap();
        assert_eq!(policy.rerank, SourceRerankMode::Off);
        assert_eq!(policy.query_expansion, SourceQueryExpansionMode::Lexical);
        assert_eq!(policy.ann_min_vectors, 4);
        assert_eq!(policy.max_expanded_terms, 3);
        assert!(
            serde_json::from_value::<SourceRetrievalPolicy>(json!({
                "rerank": "learned"
            }))
            .is_err(),
            "rerank is a closed enum; no hidden model routing"
        );
        assert!(
            serde_json::from_value::<SourceRetrievalPolicy>(json!({
                "query_expansion": "semantic"
            }))
            .is_err()
        );
        let result = SourceRetrievalPolicyResult {
            project_id: "prj-1".into(),
            policy: policy.clone(),
        };
        let decoded: SourceRetrievalPolicyResult =
            serde_json::from_value(serde_json::to_value(&result).unwrap()).unwrap();
        assert_eq!(decoded, result);
        let legacy: SourceRetrievalPolicyResult =
            serde_json::from_value(json!({"project_id": "prj-1"})).unwrap();
        assert_eq!(legacy.policy, SourceRetrievalPolicy::default());
    }

    #[test]
    fn source_index_status_and_rebuild_round_trip_without_vector_data() {
        let policy = SourceRetrievalPolicy {
            ann_enabled: true,
            ann_min_vectors: 16,
            ann_nprobe: 3,
            rerank: SourceRerankMode::Deterministic,
            query_expansion: SourceQueryExpansionMode::Lexical,
            max_expanded_terms: 4,
        };
        let status = SourceIndexStatusResult {
            project_id: "prj-1".into(),
            policy: policy.clone(),
            embedding_enabled: true,
            embedding_configured: true,
            model: Some("embed-small".into()),
            dimensions: 8,
            ann_enabled: true,
            ready: true,
            path: "ann".into(),
            generation: Some(3),
            vector_count: 40,
            assigned_vectors: 40,
            invalid_assignments: 0,
            centroid_count: 6,
            nlist: 6,
            nprobe: 3,
            min_vectors: 16,
            coverage: 1.0,
            bytes: 1_024,
            fallback_reason: None,
            last_rebuilt_at: Some("2026-11-12T00:00:00Z".into()),
            last_error: None,
        };
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(value["path"], "ann");
        assert_eq!(value["policy"]["ann_enabled"], true);
        assert!(value.get("centroids").is_none());
        assert!(value.get("vectors").is_none());
        let decoded: SourceIndexStatusResult = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, status);

        let legacy: SourceIndexStatusResult = serde_json::from_value(json!({
            "project_id": "prj-1"
        }))
        .unwrap();
        assert!(!legacy.ready);
        assert_eq!(legacy.path, "");
        assert_eq!(legacy.policy, SourceRetrievalPolicy::default());

        let rebuild = SourceIndexRebuildResult {
            project_id: "prj-1".into(),
            rebuilt: true,
            reason: None,
            generation: Some(4),
            vector_count: 40,
            assigned_vectors: 40,
            centroid_count: 6,
            nlist: 6,
            nprobe: 2,
            path: "ann".into(),
        };
        let decoded: SourceIndexRebuildResult =
            serde_json::from_value(serde_json::to_value(&rebuild).unwrap()).unwrap();
        assert_eq!(decoded, rebuild);
        let legacy: SourceIndexRebuildResult = serde_json::from_value(json!({})).unwrap();
        assert!(!legacy.rebuilt);
        assert_eq!(legacy.nprobe, 0);
    }

    #[test]
    fn source_search_result_decodes_retrieval_v2_defaults() {
        let result: SourceSearchResult = serde_json::from_value(json!({
            "hits": [{
                "source_id": "src-1",
                "source_title": "Runtime",
                "chunk_id": "src-1:0001:abcdef",
                "chunk_no": 1,
                "content": "Deterministic leases.",
                "score": 0.75
            }]
        }))
        .unwrap();
        assert_eq!(result.index_path, "exact");
        assert_eq!(result.rerank, "off");
        assert_eq!(result.expansion, "off");
        assert!(result.index_generation.is_none());
        assert!(result.index_fallback_reason.is_none());
        assert_eq!(result.probed_clusters, 0);
        assert_eq!(result.ann_candidates, 0);
        assert!(result.expansion_terms.is_empty());
        assert_eq!(result.hits[0].rerank, "off");
        assert!(!result.hits[0].expanded);
        assert!(result.hits[0].rerank_score.is_none());
        assert!(result.hits[0].lexical_overlap.is_none());
        assert!(result.hits[0].title_match.is_none());
        assert!(result.hits[0].freshness.is_none());
        assert!(result.hits[0].expansion_terms.is_empty());
        assert!(result.hits[0].source_updated_at.is_none());

        let hit = SourceSearchHit {
            rerank: "deterministic".into(),
            rerank_score: Some(0.71),
            lexical_overlap: Some(1.0),
            title_match: Some(0.5),
            freshness: Some(0.9),
            expanded: true,
            expansion_terms: vec!["leases".into()],
            source_updated_at: Some("2026-11-12T00:00:00Z".into()),
            ..SourceSearchHit::default()
        };
        let value = serde_json::to_value(&hit).unwrap();
        assert_eq!(value["rerank"], "deterministic");
        assert_eq!(value["expanded"], true);
        assert_eq!(value["expansion_terms"][0], "leases");
        assert!(value.get("vector").is_none());
    }
}

#[cfg(test)]
mod process_supervisor_protocol_tests {
    use super::*;
    use serde_json::json;

    fn row() -> SupervisedProcess {
        SupervisedProcess {
            id: "psup_1".to_string(),
            session_id: "ses_1".to_string(),
            name: "server".to_string(),
            command: "cargo run".to_string(),
            argv: vec!["/bin/sh".into(), "-c".into(), "cargo run".into()],
            cwd: "/tmp/work".to_string(),
            worktree: "/tmp/work".to_string(),
            env_hash: "env".to_string(),
            command_hash: "command".to_string(),
            state: "running".to_string(),
            pid: Some(42),
            pgid: Some(42),
            nonce: "nonce".to_string(),
            started_at: "2026-11-11T00:00:00Z".to_string(),
            process_started_at: Some("Wed Nov 11 00:00:00 2026".to_string()),
            heartbeat_at: "2026-11-11T00:00:01Z".to_string(),
            finished_at: None,
            exit_code: None,
            signal: None,
            output_path: "/data/processes/ses_1/psup_1/output.log".to_string(),
            output_bytes: 12,
            output_lines: 2,
            total_output_bytes: 44,
            output_rotations: 1,
            max_output_bytes: 1_048_576,
            max_output_lines: 20_000,
            owner_instance: "instance:9".to_string(),
            retention_deadline: None,
            idle_ttl_secs: 3_600,
            last_access_at: "2026-11-11T00:00:01Z".to_string(),
            restart_attempts: 0,
            restart_blocked_reason: None,
            detail: None,
        }
    }

    #[test]
    fn gui_retention_audit_prune_and_scope_preview_decode_additively() {
        let prune: GuiTimelinePruneParams = serde_json::from_value(json!({
            "project_id": "prj-1",
            "limit": 10
        }))
        .unwrap();
        assert!(!prune.confirm);
        assert_eq!(prune.limit, Some(10));
        let audit = GuiTimelineAuditResult {
            project_id: "prj-1".to_string(),
            entries: vec![GuiTimelinePrunedInfo {
                session_id: "ses-1".to_string(),
                seq: 4,
                retention_days: 30,
                cutoff: "2026-10-13T00:00:00Z".to_string(),
                pruned: 2,
                created_at: "2026-11-12T00:00:00Z".to_string(),
            }],
            total: 1,
            limit: 50,
        };
        let decoded: GuiTimelineAuditResult =
            serde_json::from_value(serde_json::to_value(&audit).unwrap()).unwrap();
        assert_eq!(decoded, audit);
        let params: GuiTimelineAuditParams =
            serde_json::from_value(json!({"project_id": "prj-1"})).unwrap();
        assert!(params.limit.is_none());
        let preview: crate::test_cache::TestCacheScopePreviewResult =
            serde_json::from_value(json!({
                "project_id": "prj-1",
                "enabled": true,
                "extra_env": 0,
                "extra_paths": 0,
                "complete": true,
                "entries": []
            }))
            .unwrap();
        assert!(preview.entries.is_empty());
        let retention: GuiTimelineRetentionParams =
            serde_json::from_value(json!({"project_id": "prj-1"})).unwrap();
        assert!(retention.older_than_days.is_none());
    }

    #[test]
    fn process_restart_and_daemon_agent_results_round_trip() {
        let result = ProcessRestartResult {
            process: row(),
            scheduled: true,
            detail: "explicit restart scheduled".to_string(),
        };
        let decoded: ProcessRestartResult =
            serde_json::from_value(serde_json::to_value(&result).unwrap()).unwrap();
        assert_eq!(decoded, result);
        let params: ProcessRestartParams = serde_json::from_value(json!({
            "session_id": "ses_1",
            "process_id": "psup_1"
        }))
        .unwrap();
        assert!(!params.confirm);
        let status = DaemonAgentStatusResult {
            label: "com.verlinpark.daemon".to_string(),
            plist_path: "/Users/me/Library/LaunchAgents/com.verlinpark.daemon.plist".to_string(),
            installed: true,
            loaded: true,
            disabled: false,
            running: true,
            supervised: true,
            pid: Some(42),
            state: "running".to_string(),
            detail: "launchd reports pid 42".to_string(),
            launchctl_available: true,
        };
        let decoded: DaemonAgentStatusResult =
            serde_json::from_value(serde_json::to_value(&status).unwrap()).unwrap();
        assert_eq!(decoded, status);
        let retention = GuiTimelineRetentionResult {
            project_id: "prj-1".to_string(),
            retention_days: 30,
            keep_forever: false,
            cutoff: Some("2026-10-13T00:00:00Z".to_string()),
            total: 4,
            eligible: 1,
            oldest_at: Some("2026-01-01T00:00:00Z".to_string()),
            newest_at: Some("2026-11-01T00:00:00Z".to_string()),
        };
        let decoded: GuiTimelineRetentionResult =
            serde_json::from_value(serde_json::to_value(&retention).unwrap()).unwrap();
        assert_eq!(decoded, retention);
    }

    #[test]
    fn supervised_process_round_trips_and_tolerates_old_fields() {
        let value = serde_json::to_value(row()).unwrap();
        let decoded: SupervisedProcess = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(decoded, row());
        let mut minimal = value.clone();
        let object = minimal.as_object_mut().unwrap();
        for key in [
            "argv",
            "pid",
            "pgid",
            "process_started_at",
            "output_bytes",
            "output_lines",
            "total_output_bytes",
            "output_rotations",
            "max_output_bytes",
            "max_output_lines",
            "owner_instance",
            "retention_deadline",
            "idle_ttl_secs",
            "last_access_at",
        ] {
            object.remove(key);
        }
        let decoded: SupervisedProcess = serde_json::from_value(minimal).unwrap();
        assert!(decoded.argv.is_empty());
        assert_eq!(decoded.pid, None);
        assert_eq!(decoded.idle_ttl_secs, 3_600);
        assert_eq!(decoded.max_output_bytes, 1_048_576);
        assert_eq!(decoded.max_output_lines, 20_000);
    }

    #[test]
    fn process_params_are_forward_compatible() {
        let list: ProcessListParams = serde_json::from_value(json!({
            "session_id": "ses_1"
        }))
        .unwrap();
        assert!(list.include_finished.is_none());
        assert!(list.limit.is_none());
        let logs: ProcessLogsParams = serde_json::from_value(json!({
            "session_id": "ses_1",
            "process_id": "psup_1"
        }))
        .unwrap();
        assert!(!logs.from_end);
        assert!(logs.offset.is_none());
        let stop: ProcessStopParams = serde_json::from_value(json!({
            "session_id": "ses_1",
            "process_id": "psup_1"
        }))
        .unwrap();
        assert!(stop.signal.is_none());
        assert!(stop.grace_secs.is_none());
    }

    #[test]
    fn logs_result_round_trips() {
        let result = ProcessLogsResult {
            session_id: "ses_1".to_string(),
            process_id: "psup_1".to_string(),
            state: "running".to_string(),
            content: "hello\n".to_string(),
            offset: 0,
            next_offset: 6,
            total_bytes: 6,
            eof: true,
            truncated: false,
            rotated: true,
        };
        let decoded: ProcessLogsResult =
            serde_json::from_value(serde_json::to_value(&result).unwrap()).unwrap();
        assert_eq!(decoded, result);
    }

    #[test]
    fn process_metrics_decode_with_defaults() {
        let metrics: DaemonMetrics = serde_json::from_value(json!({
            "version": "0.1",
            "protocol": 1,
            "uptime_secs": 5,
            "projects": 1,
            "sessions": [],
            "plan_tasks": [],
            "merge_queue": [],
            "intake": [],
            "continuations": [],
            "recoveries": [],
            "memories": [],
            "memory_conflicts": [],
            "runtime_leases": [],
            "tokens_in": 0,
            "tokens_out": 0,
            "cache_read_tokens": 0,
            "cache_write_tokens": 0,
            "cost_spent_usd": 0.0
        }))
        .unwrap();
        assert!(metrics.supervised_processes.is_empty());
        assert_eq!(metrics.supervised_process_output_bytes, 0);
    }
}

#[cfg(test)]
mod process_policy_protocol_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn process_policy_defaults_preserve_historical_limits() {
        let policy: ProcessPolicy = serde_json::from_value(json!({})).unwrap();
        assert_eq!(policy, ProcessPolicy::default());
        assert!(policy.idle_sweep_enabled);
        assert_eq!(policy.max_processes, 8);
        assert_eq!(policy.idle_ttl_secs, 3_600);
        assert_eq!(policy.max_output_bytes, 1_048_576);
        assert_eq!(policy.max_output_lines, 20_000);
    }

    #[test]
    fn process_policy_validation_bounds_and_clamps() {
        assert!(ProcessPolicy::default().validate().is_ok());
        let too_many = ProcessPolicy {
            max_processes: 33,
            ..ProcessPolicy::default()
        };
        assert!(too_many.validate().is_err());
        let short_idle = ProcessPolicy {
            idle_ttl_secs: 59,
            ..ProcessPolicy::default()
        };
        assert!(short_idle.validate().is_err());
        let small_log = ProcessPolicy {
            max_output_bytes: 1_024,
            ..ProcessPolicy::default()
        };
        assert!(small_log.validate().is_err());
        let policy = ProcessPolicy {
            idle_ttl_secs: 900,
            max_output_bytes: 65_536,
            max_output_lines: 2_000,
            ..ProcessPolicy::default()
        };
        assert_eq!(
            policy.clamp(86_400, 4_194_304, 50_000),
            (900, 65_536, 2_000)
        );
        assert_eq!(policy.clamp(60, 4_096, 100), (60, 4_096, 100));
    }

    #[test]
    fn process_policy_restart_defaults_are_off_and_bounded() {
        let policy: ProcessPolicy = serde_json::from_value(json!({})).unwrap();
        assert!(!policy.restart_on_boot);
        assert_eq!(policy.restart_max_attempts, 3);
        assert_eq!(policy.restart_backoff_secs, 5);
        let invalid = ProcessPolicy {
            restart_max_attempts: MAX_PROCESS_RESTART_ATTEMPTS + 1,
            ..ProcessPolicy::default()
        };
        assert!(invalid.validate().is_err());
        let invalid = ProcessPolicy {
            restart_backoff_secs: MAX_PROCESS_RESTART_BACKOFF_SECS + 1,
            ..ProcessPolicy::default()
        };
        assert!(invalid.validate().is_err());
        let policy = ProcessPolicy {
            restart_backoff_secs: 5,
            ..ProcessPolicy::default()
        };
        assert_eq!(policy.restart_backoff(1), std::time::Duration::from_secs(5));
        assert_eq!(
            policy.restart_backoff(2),
            std::time::Duration::from_secs(10)
        );
        assert_eq!(
            policy.restart_backoff(99),
            std::time::Duration::from_secs(300)
        );
        assert_eq!(policy.restart_backoff(0), std::time::Duration::from_secs(5));
        let zero = ProcessPolicy {
            restart_backoff_secs: 0,
            ..ProcessPolicy::default()
        };
        assert_eq!(zero.restart_backoff(3), std::time::Duration::from_secs(0));
        let legacy: ProcessPolicy = serde_json::from_value(json!({
            "idle_sweep_enabled": true,
            "max_processes": 4
        }))
        .unwrap();
        assert!(!legacy.restart_on_boot);
    }

    #[test]
    fn gui_timeline_retention_defaults_and_normalization_are_bounded() {
        let policy: GuiPolicy = serde_json::from_value(json!({})).unwrap();
        assert_eq!(policy.timeline_retention_days, 30);
        assert_eq!(policy.normalized().timeline_retention_days, 30);
        let forever = GuiPolicy {
            timeline_retention_days: 0,
            ..GuiPolicy::default()
        };
        assert_eq!(forever.clone().normalized().timeline_retention_days, 0);
        let huge = GuiPolicy {
            timeline_retention_days: 100_000,
            ..GuiPolicy::default()
        };
        assert_eq!(
            huge.normalized().timeline_retention_days,
            GuiPolicy::MAX_TIMELINE_RETENTION_DAYS
        );
        let invalid = GuiPolicy {
            timeline_retention_days: GuiPolicy::MAX_TIMELINE_RETENTION_DAYS + 1,
            ..GuiPolicy::default()
        };
        assert!(invalid.validate().is_err());
        assert!(forever.validate().is_ok());
        let legacy: GuiPolicy = serde_json::from_value(json!({
            "grace_secs": 20
        }))
        .unwrap();
        assert_eq!(legacy.timeline_retention_days, 30);
    }

    #[test]
    fn process_policy_result_round_trips() {
        let result = ProcessPolicyResult {
            project_id: "prj-1".to_string(),
            policy: ProcessPolicy {
                idle_sweep_enabled: false,
                max_processes: 2,
                idle_ttl_secs: 600,
                max_output_bytes: 8_192,
                max_output_lines: 500,
                restart_on_boot: false,
                restart_max_attempts: 3,
                restart_backoff_secs: 5,
            },
        };
        let decoded: ProcessPolicyResult =
            serde_json::from_value(serde_json::to_value(&result).unwrap()).unwrap();
        assert_eq!(decoded, result);
        let set: SetProcessPolicyParams = serde_json::from_value(json!({
            "project_id": "prj-1",
            "policy": {"max_processes": 4}
        }))
        .unwrap();
        assert_eq!(set.policy.max_processes, 4);
        assert!(set.policy.idle_sweep_enabled);
    }
}

#[cfg(test)]
mod critical_path_governor_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn critical_path_policy_defaults_are_off_and_bounded() {
        let policy = CriticalPathPolicy::default();
        assert!(!policy.enabled);
        assert_eq!(policy.reserve_ratio, 0.25);
        assert_eq!(policy.throttle_at_ratio, 0.35);
        assert_eq!(policy.critical_boost, 25);
        assert_eq!(policy.min_task_budget, 1_000);
        assert_eq!(policy.mode, CriticalPathMode::Single);
        assert_eq!(policy.max_paths, 4);
        assert_eq!(policy.department_min_ratio, 0.0);
        assert_eq!(policy.observed_weight_ratio, 0.0);
        assert_eq!(policy.starvation_after_secs, 1_800);
        assert!(policy.validate().is_ok());
    }

    #[test]
    fn critical_path_mode_parses_aliases_and_rejects_unknown() {
        assert_eq!(
            CriticalPathMode::parse("SINGLE"),
            Some(CriticalPathMode::Single)
        );
        assert_eq!(
            CriticalPathMode::parse("multi-path"),
            Some(CriticalPathMode::Multi)
        );
        assert_eq!(
            CriticalPathMode::parse("fairness"),
            Some(CriticalPathMode::Multi)
        );
        assert_eq!(CriticalPathMode::parse("weighted"), None);
        assert_eq!(CriticalPathMode::Single.as_str(), "single");
        assert_eq!(CriticalPathMode::Multi.as_str(), "multi");
        assert_eq!(
            serde_json::to_value(CriticalPathMode::Multi).unwrap(),
            json!("multi")
        );
        assert_eq!(
            serde_json::from_value::<CriticalPathMode>(json!("single")).unwrap(),
            CriticalPathMode::Single
        );
    }

    #[test]
    fn critical_path_policy_decodes_and_normalizes_fairness_fields() {
        let policy: CriticalPathPolicy = serde_json::from_value(json!({
            "enabled": true,
            "mode": "multi",
            "max_paths": 99,
            "department_min_ratio": 4.0,
            "observed_weight_ratio": -2.0,
            "starvation_after_secs": u64::MAX
        }))
        .unwrap();
        assert_eq!(policy.mode, CriticalPathMode::Multi);
        assert_eq!(policy.max_paths, 99);
        assert_eq!(policy.department_min_ratio, 4.0);
        // Invalid values are rejected before normalization.
        assert!(policy.validate().is_err());
        let normalized = policy.normalized();
        assert_eq!(normalized.max_paths, 16);
        assert_eq!(normalized.department_min_ratio, 1.0);
        assert_eq!(normalized.observed_weight_ratio, 0.0);
        assert_eq!(
            normalized.starvation_after_secs,
            CriticalPathPolicy::MAX_STARVATION_AFTER_SECS
        );
        assert!(normalized.validate().is_ok());

        let too_few: CriticalPathPolicy = serde_json::from_value(json!({
            "mode": "multi",
            "max_paths": 1
        }))
        .unwrap();
        assert!(too_few.validate().is_err());
        assert_eq!(too_few.normalized().max_paths, 2);

        let nan: CriticalPathPolicy = serde_json::from_value(json!({
            "department_min_ratio": 0.5
        }))
        .unwrap();
        let mut nan = nan;
        nan.department_min_ratio = f64::NAN;
        assert!(nan.validate().is_err());
        assert_eq!(nan.normalized().department_min_ratio, 0.0);
    }

    #[test]
    fn critical_path_policy_decodes_missing_fields_with_defaults() {
        let policy: CriticalPathPolicy = serde_json::from_value(json!({})).unwrap();
        assert_eq!(policy, CriticalPathPolicy::default());
        let policy: CriticalPathPolicy =
            serde_json::from_value(json!({ "enabled": true })).unwrap();
        assert!(policy.enabled);
        assert_eq!(policy.reserve_ratio, 0.25);
    }

    #[test]
    fn critical_path_policy_normalizes_and_validates_ranges() {
        let mut policy = CriticalPathPolicy {
            enabled: true,
            reserve_ratio: 3.0,
            throttle_at_ratio: -1.0,
            critical_boost: 500,
            min_task_budget: u64::MAX,
            ..CriticalPathPolicy::default()
        };
        let normalized = policy.clone().normalized();
        assert_eq!(normalized.reserve_ratio, 1.0);
        assert_eq!(normalized.throttle_at_ratio, 0.0);
        assert_eq!(normalized.critical_boost, 100);
        assert_eq!(normalized.min_task_budget, 10_000_000);
        assert!(policy.validate().is_err());

        policy.reserve_ratio = f64::NAN;
        assert!(policy.validate().is_err());
        assert_eq!(policy.clone().normalized().reserve_ratio, 0.25);
        policy.reserve_ratio = 0.5;
        policy.throttle_at_ratio = 0.0;
        policy.critical_boost = 0;
        policy.min_task_budget = 0;
        assert!(policy.validate().is_ok());
    }

    #[test]
    fn governor_status_decodes_with_defaults_and_round_trips() {
        let status: GovernorSessionStatus = serde_json::from_value(json!({
            "session_id": "s1",
            "project_id": "p1",
            "generated_at": "2026-11-11T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(status.session_id, "s1");
        assert!(!status.enabled);
        assert!(!status.plan_present);
        assert_eq!(status.policy, CriticalPathPolicy::default());
        assert!(status.critical_path.is_none());
        assert!(status.decisions.is_empty());
        assert!(status.paths.is_empty());
        assert!(status.deferred_order.is_empty());
        assert!(status.error.is_none());
        let encoded = serde_json::to_value(&status).unwrap();
        let decoded: GovernorSessionStatus = serde_json::from_value(encoded).unwrap();
        assert_eq!(status, decoded);
    }

    #[test]
    fn governor_status_decodes_a_multi_path_table() {
        let status: GovernorSessionStatus = serde_json::from_value(json!({
            "session_id": "s1",
            "project_id": "p1",
            "enabled": true,
            "policy": {"enabled": true, "mode": "multi", "max_paths": 3},
            "reserve": {
                "state": "active",
                "mode": "multi",
                "path_count": 2,
                "protected_tokens": 900,
                "allocated_tokens": 700,
                "floor_tokens": 100
            },
            "paths": [
                {
                    "path_id": 0,
                    "task_ids": ["a", "b"],
                    "departments": ["impl"],
                    "weight_tokens": 600,
                    "floor_tokens": 60,
                    "allocation_tokens": 400,
                    "deficit_tokens": 200
                },
                {
                    "path_id": 1,
                    "task_ids": ["c"],
                    "departments": ["qa"],
                    "weight_tokens": 300,
                    "floor_tokens": 40,
                    "allocation_tokens": 300,
                    "deficit_tokens": 0
                }
            ],
            "departments": [
                {"department": "impl", "weight_tokens": 900, "floor_tokens": 90, "path_ids": [0]},
                {"department": "qa", "weight_tokens": 300, "floor_tokens": 30, "path_ids": [1]}
            ],
            "deferred_order": ["side"],
            "generated_at": "2026-11-12T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(status.policy.mode, CriticalPathMode::Multi);
        assert_eq!(status.reserve.path_count, 2);
        assert_eq!(status.reserve.allocated_tokens, 700);
        assert_eq!(status.paths.len(), 2);
        assert_eq!(status.paths[0].task_ids, vec!["a", "b"]);
        assert_eq!(status.paths[1].deficit_tokens, 0);
        assert_eq!(status.departments.len(), 2);
        assert_eq!(status.departments[0].department, "impl");
        assert_eq!(status.departments[0].path_ids, vec![0]);
        assert_eq!(status.deferred_order, vec!["side"]);
    }

    #[test]
    fn governor_decision_payload_decodes_partial_history() {
        let decision: BudgetGovernorDecision = serde_json::from_value(json!({
            "decision_id": "gov-1",
            "session_id": "s1",
            "action": "defer",
            "reason": "reserve",
            "code": "BUDGET_RESERVED",
            "created_at": "2026-11-11T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(decision.action, "defer");
        assert_eq!(decision.code, "BUDGET_RESERVED");
        assert_eq!(decision.mode, "single");
        assert!(!decision.critical);
        assert!(decision.path.is_empty());
        assert!(decision.deferred.is_empty());
        assert_eq!(decision.plan_revision, 0);
    }

    #[test]
    fn governor_decision_round_trips_admission_and_deferral_lists() {
        let decision = BudgetGovernorDecision {
            decision_id: "gov-9".to_string(),
            session_id: "s1".to_string(),
            plan_revision: 4,
            action: "admission".to_string(),
            task_id: None,
            critical: false,
            reason: "reserve".to_string(),
            code: "BUDGET_RESERVED".to_string(),
            detail: "path [a] weight 5000".to_string(),
            path: vec!["a".to_string(), "b".to_string()],
            path_weight_tokens: 5_000,
            limit_tokens: Some(10_000),
            spent_tokens: 2_000,
            reserved_tokens: 100,
            remaining_tokens: Some(7_900),
            protected_tokens: 6_000,
            requested_tokens: Some(1_500),
            admitted: vec!["a".to_string()],
            deferred: vec![GovernorDeferralInfo {
                task_id: "side".to_string(),
                department: "impl".to_string(),
                critical: false,
                weight_tokens: 2_500,
                reason: "reserve".to_string(),
                code: "BUDGET_RESERVED".to_string(),
                remaining_tokens: Some(7_900),
                protected_tokens: 6_000,
                path_id: None,
                path_ids: Vec::new(),
                path_deficit_tokens: 0,
                starvation_credit: 0,
            }],
            inputs_hash: "hash".to_string(),
            created_at: "2026-11-11T00:00:00Z".to_string(),
            mode: "multi".to_string(),
            paths: vec![GovernorPathInfo {
                path_id: 0,
                task_ids: vec!["a".to_string(), "b".to_string()],
                departments: vec!["impl".to_string()],
                weight_tokens: 5_000,
                declared_weight_tokens: 5_000,
                observed_tokens: 0,
                floor_tokens: 500,
                allocation_tokens: 4_000,
                deficit_tokens: 1_000,
            }],
            departments: vec![GovernorDepartmentInfo {
                department: "impl".to_string(),
                weight_tokens: 5_000,
                floor_tokens: 500,
                path_ids: vec![0],
            }],
            allocated_tokens: 4_000,
            deferred_order: vec!["side".to_string()],
        };
        let value = serde_json::to_value(&decision).unwrap();
        assert_eq!(value["deferred"][0]["code"], "BUDGET_RESERVED");
        assert_eq!(value["path"][1], "b");
        assert_eq!(value["mode"], "multi");
        assert_eq!(value["paths"][0]["allocation_tokens"], 4_000);
        assert_eq!(value["departments"][0]["floor_tokens"], 500);
        assert_eq!(value["deferred_order"][0], "side");
        let decoded: BudgetGovernorDecision = serde_json::from_value(value).unwrap();
        assert_eq!(decision, decoded);
    }

    #[test]
    fn governor_path_table_decodes_partial_rows_with_defaults() {
        let path: GovernorPathInfo = serde_json::from_value(json!({
            "task_ids": ["a", "b"]
        }))
        .unwrap();
        assert_eq!(path.path_id, 0);
        assert_eq!(path.task_ids, vec!["a", "b"]);
        assert!(path.departments.is_empty());
        assert_eq!(path.weight_tokens, 0);
        assert_eq!(path.allocation_tokens, 0);
        assert_eq!(path.deficit_tokens, 0);
    }

    #[test]
    fn critical_path_policy_rejects_unknown_mode_values() {
        let error = serde_json::from_value::<CriticalPathPolicy>(json!({
            "enabled": true,
            "mode": "weighted"
        }));
        assert!(error.is_err());
        let legacy: CriticalPathPolicy = serde_json::from_value(json!({
            "enabled": true,
            "reserve_ratio": 0.2
        }))
        .unwrap();
        assert_eq!(legacy.mode, CriticalPathMode::Single);
        assert_eq!(legacy.max_paths, 4);
    }

    #[test]
    fn governor_department_rows_decode_with_defaults_and_bounds() {
        let row: GovernorDepartmentInfo = serde_json::from_value(json!({
            "department": "impl"
        }))
        .unwrap();
        assert_eq!(row.department, "impl");
        assert_eq!(row.weight_tokens, 0);
        assert_eq!(row.floor_tokens, 0);
        assert!(row.path_ids.is_empty());
        let row: GovernorDepartmentInfo = serde_json::from_value(json!({
            "department": "qa",
            "weight_tokens": 500,
            "floor_tokens": 50,
            "path_ids": [0, 2]
        }))
        .unwrap();
        assert_eq!(row.path_ids, vec![0, 2]);
        let value = serde_json::to_value(&row).unwrap();
        assert_eq!(value["department"], "qa");
        assert_eq!(value["path_ids"][1], 2);
    }

    #[test]
    fn critical_path_policy_normalization_is_idempotent_for_fairness() {
        let policy = CriticalPathPolicy {
            enabled: true,
            mode: CriticalPathMode::Multi,
            max_paths: 6,
            department_min_ratio: 0.25,
            observed_weight_ratio: 0.5,
            starvation_after_secs: 1_200,
            ..CriticalPathPolicy::default()
        };
        let once = policy.clone().normalized();
        let twice = once.clone().normalized();
        assert_eq!(once, twice);
        assert_eq!(once, policy);
        assert_eq!(
            serde_json::from_value::<CriticalPathPolicy>(serde_json::to_value(&once).unwrap())
                .unwrap(),
            once
        );
    }

    #[test]
    fn governor_decision_decodes_task_membership_path_ids() {
        let decision: BudgetGovernorDecision = serde_json::from_value(json!({
            "decision_id": "gov-1",
            "session_id": "s1",
            "action": "admission",
            "mode": "multi",
            "paths": [{
                "path_id": 0,
                "task_ids": ["root", "a"],
                "allocation_tokens": 500
            }],
            "deferred": [{
                "task_id": "side",
                "reason": "reserve",
                "path_ids": []
            }],
            "created_at": "2026-11-12T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(decision.mode, "multi");
        assert_eq!(decision.paths[0].task_ids, vec!["root", "a"]);
        assert!(decision.deferred[0].path_ids.is_empty());
        assert!(decision.departments.is_empty());
    }

    #[test]
    fn governor_task_decision_info_carries_path_membership() {
        let info = GovernorTaskDecisionInfo {
            task_id: "root".to_string(),
            department: "impl".to_string(),
            critical: true,
            weight_tokens: 1_000,
            order: 0,
            action: "admit".to_string(),
            reason: "critical_admit".to_string(),
            code: String::new(),
            remaining_after_tokens: Some(9_000),
            path_id: Some(0),
            path_ids: vec![0, 1],
            path_deficit_tokens: 500,
            starvation_credit: 5,
        };
        let value = serde_json::to_value(&info).unwrap();
        assert_eq!(value["path_ids"][1], 1);
        let decoded: GovernorTaskDecisionInfo = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, info);
    }

    #[test]
    fn governor_preview_params_are_optional() {
        let params: GovernorPreviewParams =
            serde_json::from_value(json!({"session_id": "s1"})).unwrap();
        assert_eq!(params.session_id, "s1");
        assert!(params.remaining_tokens.is_none());
        assert!(params.limit.is_none());
        assert!(params.mode.is_none());
        let status: GovernorStatusParams =
            serde_json::from_value(json!({"session_id": "s1", "limit": 5})).unwrap();
        assert_eq!(status.limit, Some(5));
        let multi: GovernorPreviewParams = serde_json::from_value(json!({
            "session_id": "s1",
            "mode": "multi",
            "remaining_tokens": 500
        }))
        .unwrap();
        assert_eq!(multi.mode.as_deref(), Some("multi"));
        assert_eq!(multi.remaining_tokens, Some(500));
    }

    #[test]
    fn governor_reserve_and_path_info_have_stable_shapes() {
        let path = CriticalPathInfo {
            plan_revision: 2,
            task_ids: vec!["a".to_string(), "b".to_string()],
            weight_tokens: 5_000,
            remaining_budget_tokens: Some(10_000),
            complete: false,
            task_count: 2,
            edge_count: 1,
            inputs_hash: "hash".to_string(),
        };
        let value = serde_json::to_value(&path).unwrap();
        assert_eq!(value["plan_revision"], 2);
        assert_eq!(value["task_ids"][0], "a");
        let decoded: CriticalPathInfo = serde_json::from_value(value).unwrap();
        assert_eq!(path, decoded);

        let reserve = GovernorReserveInfo {
            state: "active".to_string(),
            reserve_ratio: 0.25,
            throttle_at_ratio: 0.35,
            critical_boost: 25,
            min_task_budget: 1_000,
            limit_tokens: Some(10_000),
            spent_tokens: 2_000,
            reserved_tokens: 100,
            remaining_tokens: Some(7_900),
            path_weight_tokens: 5_000,
            ratio_reserve_tokens: 1_975,
            protected_tokens: 6_975,
            headroom_tokens: Some(925),
            throttled: false,
            mode: "multi".to_string(),
            path_count: 2,
            allocated_tokens: 5_000,
            floor_tokens: 500,
            enumerated_paths: 7,
            paths_truncated: true,
        };
        let value = serde_json::to_value(&reserve).unwrap();
        assert_eq!(value["state"], "active");
        assert_eq!(value["protected_tokens"], 6_975);
        assert_eq!(value["mode"], "multi");
        assert_eq!(value["path_count"], 2);
        let decoded: GovernorReserveInfo = serde_json::from_value(value).unwrap();
        assert_eq!(reserve, decoded);
    }

    #[test]
    fn multi_path_status_round_trips_with_departments_and_membership() {
        let status = GovernorSessionStatus {
            session_id: "s1".to_string(),
            project_id: "p1".to_string(),
            enabled: true,
            plan_present: true,
            policy: CriticalPathPolicy {
                enabled: true,
                mode: CriticalPathMode::Multi,
                ..CriticalPathPolicy::default()
            },
            critical_path: Some(CriticalPathInfo {
                plan_revision: 1,
                task_ids: vec!["a".to_string()],
                weight_tokens: 100,
                ..CriticalPathInfo::default()
            }),
            reserve: GovernorReserveInfo {
                state: "active".to_string(),
                mode: "multi".to_string(),
                path_count: 1,
                protected_tokens: 100,
                allocated_tokens: 100,
                floor_tokens: 25,
                ..GovernorReserveInfo::default()
            },
            paths: vec![GovernorPathInfo {
                path_id: 0,
                task_ids: vec!["a".to_string()],
                departments: vec!["impl".to_string()],
                weight_tokens: 100,
                floor_tokens: 25,
                allocation_tokens: 100,
                ..GovernorPathInfo::default()
            }],
            departments: vec![GovernorDepartmentInfo {
                department: "impl".to_string(),
                weight_tokens: 100,
                floor_tokens: 25,
                path_ids: vec![0],
            }],
            deferred_order: vec!["side".to_string()],
            decisions: vec![GovernorTaskDecisionInfo {
                task_id: "a".to_string(),
                critical: true,
                action: "admit".to_string(),
                reason: "critical_admit".to_string(),
                path_id: Some(0),
                path_ids: vec![0],
                ..GovernorTaskDecisionInfo::default()
            }],
            generated_at: "2026-11-12T00:00:00Z".to_string(),
            ..GovernorSessionStatus::default()
        };
        let value = serde_json::to_value(&status).unwrap();
        let decoded: GovernorSessionStatus = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, status);
        assert_eq!(decoded.departments[0].path_ids, vec![0]);
        assert_eq!(decoded.decisions[0].path_ids, vec![0]);
    }

    #[test]
    fn governor_deferral_round_trips_path_membership() {
        let deferral = GovernorDeferralInfo {
            task_id: "side".to_string(),
            department: "impl".to_string(),
            critical: false,
            weight_tokens: 500,
            reason: "reserve".to_string(),
            code: "BUDGET_RESERVED".to_string(),
            remaining_tokens: Some(100),
            protected_tokens: 900,
            path_id: Some(1),
            path_ids: vec![1],
            path_deficit_tokens: 250,
            starvation_credit: 10,
        };
        let value = serde_json::to_value(&deferral).unwrap();
        assert_eq!(value["path_ids"][0], 1);
        let decoded: GovernorDeferralInfo = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, deferral);
    }

    #[test]
    fn critical_path_policy_result_is_wire_compatible() {
        let result = CriticalPathPolicyResult {
            project_id: "p1".to_string(),
            policy: CriticalPathPolicy {
                enabled: true,
                ..CriticalPathPolicy::default()
            },
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["project_id"], "p1");
        assert_eq!(value["policy"]["enabled"], true);
        assert_eq!(value["policy"]["reserve_ratio"], 0.25);
    }

    #[test]
    fn preemption_policy_defaults_are_enabled_and_bounded() {
        let policy = PreemptionPolicy::default();
        assert!(policy.enabled);
        assert_eq!(policy.provider_grace_ms, 2_000);
        assert_eq!(policy.tool_grace_ms, 5_000);
        assert_eq!(policy.kill_escalation_ms, 2_000);
        assert_eq!(policy.tool_deadline_secs, None);
        assert!(policy.checkpoint_on_preempt);
        let decoded: PreemptionPolicy = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(decoded, policy);
        assert!(policy.validate().is_ok());
    }

    #[test]
    fn preemption_policy_clamps_every_grace_and_deadline() {
        let policy = PreemptionPolicy {
            enabled: false,
            provider_grace_ms: 9_999_999,
            tool_grace_ms: 9_999_999,
            kill_escalation_ms: 9_999_999,
            tool_deadline_secs: Some(999_999),
            checkpoint_on_preempt: false,
        };
        assert!(policy.validate().is_err());
        let normalized = policy.clone().normalized().unwrap();
        assert!(!normalized.enabled);
        assert_eq!(normalized.provider_grace_ms, 30_000);
        assert_eq!(normalized.tool_grace_ms, 60_000);
        assert_eq!(normalized.kill_escalation_ms, 30_000);
        assert_eq!(normalized.tool_deadline_secs, Some(3_600));
        assert!(!normalized.checkpoint_on_preempt);
        assert!(policy.validate().is_err());
    }

    #[test]
    fn preemption_record_wire_shape_defaults_optional_fields() {
        let record = PreemptionRecord {
            session_id: "s1".to_string(),
            project_id: "p1".to_string(),
            agent_id: "a1".to_string(),
            agent_name: "Leader".to_string(),
            agent_role: "leader".to_string(),
            task_id: None,
            department: None,
            reason: "wall_clock".to_string(),
            phase: "provider".to_string(),
            tool: None,
            elapsed_ms: 1_005,
            side_effects_unknown: false,
            partial_input_tokens: 40,
            partial_output_tokens: 5,
            partial_output_bytes: 14,
            usage_estimated: false,
            checkpoint_id: Some("cp1".to_string()),
            task_checkpoint_commit: None,
            detail: None,
            terminal: true,
            created_at: "2026-11-12T00:00:00Z".to_string(),
        };
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(value["reason"], "wall_clock");
        assert_eq!(value["terminal"], true);
        let decoded: PreemptionRecord = serde_json::from_value(serde_json::json!({
            "session_id": "s1",
            "reason": "explicit",
            "phase": "tool"
        }))
        .unwrap();
        assert_eq!(decoded.elapsed_ms, 0);
        assert!(!decoded.side_effects_unknown);
        assert!(!decoded.terminal);
        assert_eq!(
            record.summary(),
            "wall_clock preempted during provider after 1005ms"
        );
    }

    #[test]
    fn session_preemption_result_is_backward_compatible() {
        let result = SessionPreemptionResult {
            session_id: "s1".to_string(),
            policy: PreemptionPolicy::default(),
            active: true,
            preempted: true,
            records: Vec::new(),
            by_phase: vec![MetricCount {
                key: "provider".to_string(),
                count: 1,
            }],
            by_reason: vec![MetricCount {
                key: "wall_clock".to_string(),
                count: 1,
            }],
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["by_phase"][0]["key"], "provider");
        let decoded: SessionPreemptionResult = serde_json::from_value(serde_json::json!({
            "session_id": "s1",
            "policy": {}
        }))
        .unwrap();
        assert!(!decoded.active);
        assert!(!decoded.preempted);
        assert!(decoded.records.is_empty());
    }

    #[test]
    fn daemon_metrics_decode_older_payloads_without_preemption_groups() {
        let value = serde_json::json!({
            "version": "0.1",
            "protocol": 1,
            "uptime_secs": 1,
            "projects": 1,
            "sessions": [],
            "plan_tasks": [],
            "merge_queue": [],
            "intake": [],
            "continuations": [],
            "recoveries": [],
            "memories": [],
            "memory_conflicts": [],
            "runtime_leases": [],
            "tokens_in": 0,
            "tokens_out": 0,
            "cache_read_tokens": 0,
            "cache_write_tokens": 0,
            "cost_spent_usd": 0.0
        });
        let metrics: DaemonMetrics = serde_json::from_value(value).unwrap();
        assert!(metrics.preemptions_by_phase.is_empty());
        assert!(metrics.preemptions_by_reason.is_empty());
    }
}

#[cfg(test)]
mod approval_optimizer_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_approval_json_decodes_with_attention_defaults() {
        let value = json!({
            "id": "a1",
            "session_id": "s1",
            "tool_call_id": "call-1",
            "tool": "write_file",
            "input": {"path": "README.md"},
            "status": "pending",
            "created_at": "2026-11-12T00:00:00Z",
            "approval_risk": "workspace_write",
            "approval_reason": "workspace write"
        });
        let approval: Approval = serde_json::from_value(value).unwrap();
        assert_eq!(approval.id, "a1");
        assert_eq!(approval.waiter_count, 0);
        assert!(approval.agent_id.is_none());
        assert!(approval.dedup_key.is_none());
        assert!(!approval.notify);
        assert_eq!(approval.priority, 0);
    }

    #[test]
    fn approval_round_trips_all_attention_fields() {
        let approval = Approval {
            id: "a1".into(),
            session_id: "s1".into(),
            tool_call_id: "call-1".into(),
            tool: "write_file".into(),
            input: json!({"path": "src/lib.rs"}),
            status: "pending".into(),
            created_at: "2026-11-12T00:00:00Z".into(),
            resolved_at: None,
            approval_risk: Some("workspace_write".into()),
            approval_reason: Some("workspace write".into()),
            agent_id: Some("worker-1".into()),
            agent_name: Some("Worker".into()),
            agent_role: Some("worker".into()),
            agent_title: Some("API".into()),
            task_id: Some("task-1".into()),
            scope_hash: Some("abc".into()),
            dedup_key: Some("key".into()),
            waiter_count: 3,
            template_id: Some("t1".into()),
            priority: 42,
            expires_at: Some("2026-11-13T00:00:00Z".into()),
            notify: true,
            notify_key: Some("s1|write_file|workspace_write".into()),
        };
        let value = serde_json::to_value(&approval).unwrap();
        assert_eq!(value["waiter_count"], 3);
        assert_eq!(value["notify"], true);
        let decoded: Approval = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, approval);
    }

    #[test]
    fn attention_policy_defaults_and_clamps_are_stable() {
        let policy = AttentionPolicy::default();
        assert!(policy.enabled);
        assert_eq!(policy.max_pending_per_session, 64);
        assert_eq!(policy.coalesce_window_secs, 120);
        assert_eq!(policy.stale_after_secs, 86_400);
        assert_eq!(policy.priority_boost_risk, "host_state");
        assert_eq!(policy.notify_coalesce_secs, 60);

        let clamped = AttentionPolicy {
            enabled: false,
            max_pending_per_session: 0,
            coalesce_window_secs: 99_999,
            stale_after_secs: 1,
            priority_boost_risk: "not-a-risk".into(),
            notify_coalesce_secs: 99_999,
        }
        .normalized();
        assert_eq!(clamped.max_pending_per_session, 1);
        assert_eq!(clamped.coalesce_window_secs, 3_600);
        assert_eq!(clamped.stale_after_secs, 60);
        assert_eq!(clamped.priority_boost_risk, "host_state");
        assert_eq!(clamped.notify_coalesce_secs, 3_600);
    }

    #[test]
    fn approval_template_defaults_are_low_risk_allow() {
        let value = json!({
            "id": "t1",
            "project_id": "p1",
            "name": "writes",
            "tool_pattern": "write_file",
            "created_by": "operator",
            "created_at": "2026-11-12T00:00:00Z"
        });
        let template: ApprovalTemplate = serde_json::from_value(value).unwrap();
        assert!(template.enabled);
        assert_eq!(template.action, "allow");
        assert!(template.risk_levels.is_empty());
        assert!(template.path_globs.is_empty());
        assert_eq!(template.used_count, 0);
        assert_eq!(template.remaining_uses(), None);

        let capped = ApprovalTemplate {
            max_uses: Some(3),
            used_count: 2,
            ..template
        };
        assert_eq!(capped.remaining_uses(), Some(1));
    }

    #[test]
    fn batch_and_queue_params_decode_explicitly() {
        let params: ApprovalBatchResolveParams = serde_json::from_value(json!({
            "decisions": [
                {"approval_id": "a1", "approved": true},
                {"approval_id": "a2", "approved": false}
            ]
        }))
        .unwrap();
        assert_eq!(params.decisions.len(), 2);
        assert!(params.decisions[0].approved);

        let queue: ApprovalQueueParams = serde_json::from_value(json!({})).unwrap();
        assert!(queue.session_id.is_none());
        assert!(queue.project_id.is_none());

        let result = ApprovalBatchResolveResult {
            results: vec![ApprovalBatchItemResult {
                approval_id: "a1".into(),
                resolved: false,
                status: Some("approved".into()),
                code: Some("APPROVAL_NOT_PENDING".into()),
                message: Some("already resolved".into()),
                waiter_count: 2,
            }],
            resolved_count: 0,
            failed_count: 1,
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["results"][0]["code"], "APPROVAL_NOT_PENDING");
        assert_eq!(value["failed_count"], 1);
    }

    #[test]
    fn audit_events_round_trip() {
        let event = ApprovalAuditEvent {
            id: "e1".into(),
            project_id: "p1".into(),
            kind: "approval.template.matched".into(),
            payload: json!({"template_id": "t1", "remaining_uses": 2}),
            created_at: "2026-11-12T00:00:00Z".into(),
        };
        let decoded: ApprovalAuditEvent =
            serde_json::from_value(serde_json::to_value(&event).unwrap()).unwrap();
        assert_eq!(decoded, event);
    }
}

/// Parameters for `backup.status` (none today).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BackupStatusParams {}

/// One repository's local anti-rollback high-water mark.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupAnchorStatus {
    pub repository_id: String,
    pub repository_path: String,
    pub highest_generation: u64,
    pub last_verified_generation: u64,
    pub last_snapshot_id: Option<String>,
    pub last_operation: String,
    pub first_seen_at: String,
    pub last_verified_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

/// One public backup signing key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupSigningKeyStatus {
    pub key_id: String,
    pub algorithm: String,
    pub public_key_b64: String,
    pub created_at: String,
    pub retired_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<String>,
    pub active: bool,
}

/// Secret-free backup signing and anti-rollback status for the desktop client.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupStatusResult {
    pub anchor_path: String,
    pub anchor_present: bool,
    pub anchor_updated_at: Option<String>,
    pub repositories: Vec<BackupAnchorStatus>,
    pub keyring_path: String,
    pub keyring_present: bool,
    pub active_key_id: Option<String>,
    pub key_count: usize,
    pub keys: Vec<BackupSigningKeyStatus>,
    pub signed_repository_formats: Vec<u32>,
    /// Additive: number of public keys whose signatures are refused by default.
    #[serde(default)]
    pub revoked_key_count: usize,
    /// Additive: portable anchor document format version accepted by this build.
    #[serde(default)]
    pub portable_anchor_format_version: u32,
    /// Additive: repositories whose high-water mark came from a portable import.
    #[serde(default)]
    pub imported_repositories: usize,
    /// Always present: explains local-anchor and no-escrow limits.
    pub note: String,
}

#[cfg(test)]
mod backup_status_revocation_tests {
    use super::*;

    #[test]
    fn backup_status_round_trips_revocation_and_import_fields() {
        let status = BackupStatusResult {
            anchor_path: "/tmp/backup-anchor.json".into(),
            anchor_present: true,
            anchor_updated_at: Some("2026-11-12T03:00:00Z".into()),
            repositories: vec![BackupAnchorStatus {
                repository_id: "01930000-0000-7000-8000-000000000001".into(),
                repository_path: "(portable import)".into(),
                highest_generation: 9,
                last_verified_generation: 9,
                last_snapshot_id: None,
                last_operation: "anchor_import".into(),
                first_seen_at: "2026-11-12T00:00:00Z".into(),
                last_verified_at: "2026-11-12T03:00:00Z".into(),
                origin: Some("portable_import".into()),
            }],
            keyring_path: "/tmp/keys/keyring.json".into(),
            keyring_present: true,
            active_key_id: Some("new".into()),
            key_count: 2,
            keys: vec![
                BackupSigningKeyStatus {
                    key_id: "old".into(),
                    algorithm: "ed25519".into(),
                    public_key_b64: "AAAA".into(),
                    created_at: "2026-11-01T00:00:00Z".into(),
                    retired_at: Some("2026-11-10T00:00:00Z".into()),
                    revoked_at: Some("2026-11-12T02:00:00Z".into()),
                    active: false,
                },
                BackupSigningKeyStatus {
                    key_id: "new".into(),
                    algorithm: "ed25519".into(),
                    public_key_b64: "BBBB".into(),
                    created_at: "2026-11-10T00:00:00Z".into(),
                    retired_at: None,
                    revoked_at: None,
                    active: true,
                },
            ],
            signed_repository_formats: vec![3],
            revoked_key_count: 1,
            portable_anchor_format_version: 1,
            imported_repositories: 1,
            note: "Local anchor, no escrow.".into(),
        };
        let encoded = serde_json::to_value(&status).unwrap();
        let decoded: BackupStatusResult = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, status);
        assert_eq!(decoded.revoked_key_count, 1);
        assert_eq!(
            decoded.keys[0].revoked_at.as_deref(),
            Some("2026-11-12T02:00:00Z")
        );
        assert!(decoded.keys[1].revoked_at.is_none());
        assert_eq!(
            decoded.repositories[0].origin.as_deref(),
            Some("portable_import")
        );
    }

    #[test]
    fn revoked_key_and_portable_anchor_fields_are_additive() {
        let key: BackupSigningKeyStatus = serde_json::from_value(serde_json::json!({
            "key_id": "primary",
            "algorithm": "ed25519",
            "public_key_b64": "AAAA",
            "created_at": "2026-11-12T00:00:00Z",
            "retired_at": null,
            "active": true
        }))
        .unwrap();
        assert!(key.revoked_at.is_none());

        let key: BackupSigningKeyStatus = serde_json::from_value(serde_json::json!({
            "key_id": "old",
            "algorithm": "ed25519",
            "public_key_b64": "AAAA",
            "created_at": "2026-11-12T00:00:00Z",
            "retired_at": "2026-11-12T01:00:00Z",
            "revoked_at": "2026-11-12T02:00:00Z",
            "active": false
        }))
        .unwrap();
        assert_eq!(key.revoked_at.as_deref(), Some("2026-11-12T02:00:00Z"));

        let anchor: BackupAnchorStatus = serde_json::from_value(serde_json::json!({
            "repository_id": "01930000-0000-7000-8000-000000000001",
            "repository_path": "(portable import)",
            "highest_generation": 9,
            "last_verified_generation": 9,
            "last_snapshot_id": null,
            "last_operation": "anchor_import",
            "first_seen_at": "2026-11-12T00:00:00Z",
            "last_verified_at": "2026-11-12T00:00:00Z"
        }))
        .unwrap();
        assert!(anchor.origin.is_none());
        assert_eq!(anchor.last_operation, "anchor_import");
    }
}

#[cfg(test)]
mod forge_protocol_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn open_pull_request_params_keep_legacy_decode() {
        let legacy: OpenPullRequestParams =
            serde_json::from_value(json!({"session_id": "ses-1"})).unwrap();
        assert_eq!(legacy.session_id, "ses-1");
        assert_eq!(legacy.provider, None);

        let explicit: OpenPullRequestParams =
            serde_json::from_value(json!({"session_id": "ses-1", "provider": "bitbucket"}))
                .unwrap();
        assert_eq!(explicit.provider.as_deref(), Some("bitbucket"));
    }

    #[test]
    fn pull_request_result_decodes_without_new_fields() {
        let legacy = json!({
            "session_id": "ses-1",
            "branch": "feature",
            "base_branch": "main",
            "pushed": true,
            "url": "https://example.test/pr/1",
            "detail": "opened"
        });
        let result: PullRequestResult = serde_json::from_value(legacy).unwrap();
        assert_eq!(result.session_id, "ses-1");
        assert_eq!(result.provider, None);
        assert_eq!(result.repository, None);
        assert_eq!(result.change, None);

        let current = PullRequestResult {
            session_id: "ses-1".into(),
            branch: "feature".into(),
            base_branch: "main".into(),
            pushed: true,
            url: Some("https://bitbucket.org/acme/widgets/pull-requests/7".into()),
            detail: "opened".into(),
            provider: Some("bitbucket".into()),
            repository: Some("acme/widgets".into()),
            change: Some("PR".into()),
        };
        let encoded = serde_json::to_value(&current).unwrap();
        assert_eq!(encoded["provider"], "bitbucket");
        assert_eq!(encoded["repository"], "acme/widgets");
        assert_eq!(encoded["change"], "PR");
        let decoded: PullRequestResult = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.provider.as_deref(), Some("bitbucket"));
    }

    #[test]
    fn forge_status_params_default_without_project() {
        let params: ForgeStatusParams = serde_json::from_value(json!({})).unwrap();
        assert_eq!(params.project_id, None);
        let params: ForgeStatusParams =
            serde_json::from_value(json!({"project_id": "prj-1"})).unwrap();
        assert_eq!(params.project_id.as_deref(), Some("prj-1"));
    }

    #[test]
    fn forge_status_result_round_trips_and_defaults_missing_fields() {
        let status = ForgeStatusResult {
            provider_override: Some("bitbucket".into()),
            default_provider: "bitbucket".into(),
            origin: Some(ForgeOriginStatus {
                host: Some("bitbucket.org".into()),
                provider: Some("bitbucket".into()),
                repository: Some("acme/widgets".into()),
                supported: true,
                server_detected: false,
                detail: "Bitbucket Cloud origin detected".into(),
            }),
            providers: vec![ForgeProviderStatus {
                provider: "bitbucket".into(),
                change_name: "PR".into(),
                transport: "rest".into(),
                command: None,
                command_available: true,
                auth_configured: Some(true),
                credential_source: Some("BITBUCKET_TOKEN".into()),
                supported: true,
                detail: "credentials configured".into(),
            }],
            detail: "selected bitbucket".into(),
        };
        let value = serde_json::to_value(&status).unwrap();
        let decoded: ForgeStatusResult = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.default_provider, "bitbucket");
        assert_eq!(decoded.providers.len(), 1);
        assert_eq!(decoded.providers[0].auth_configured, Some(true));

        let minimal: ForgeStatusResult = serde_json::from_value(json!({
            "default_provider": "github",
            "providers": [],
            "detail": "fallback"
        }))
        .unwrap();
        assert_eq!(minimal.provider_override, None);
        assert!(minimal.origin.is_none());
    }

    #[test]
    fn forge_preflight_params_and_result_round_trip() {
        let params: ForgePreflightParams =
            serde_json::from_value(json!({"project_id": "prj-1"})).unwrap();
        assert_eq!(params.project_id, "prj-1");
        assert_eq!(params.provider, None);
        let params: ForgePreflightParams =
            serde_json::from_value(json!({"project_id": "prj-1", "provider": "bitbucket"}))
                .unwrap();
        assert_eq!(params.provider.as_deref(), Some("bitbucket"));

        let result = ForgePreflightResult {
            provider: "bitbucket".into(),
            change_name: "PR".into(),
            repository: Some("acme/widgets".into()),
            auth: "bearer".into(),
            detail: "Bitbucket Cloud credentials accepted".into(),
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["auth"], "bearer");
        assert_eq!(value["repository"], "acme/widgets");
        let decoded: ForgePreflightResult = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.provider, "bitbucket");

        let minimal: ForgePreflightResult = serde_json::from_value(json!({
            "provider": "github",
            "change_name": "PR",
            "auth": "cli",
            "detail": "gh auth check passed"
        }))
        .unwrap();
        assert!(minimal.repository.is_none());
    }

    #[test]
    fn forge_origin_status_defaults_conservatively() {
        let minimal: ForgeOriginStatus = serde_json::from_value(json!({
            "detail": "not inspected"
        }))
        .unwrap();
        assert_eq!(minimal.host, None);
        assert_eq!(minimal.provider, None);
        assert_eq!(minimal.repository, None);
        assert!(!minimal.supported);
        assert!(!minimal.server_detected);

        let server: ForgeOriginStatus = serde_json::from_value(json!({
            "host": "bitbucket.example.com",
            "supported": false,
            "server_detected": true,
            "detail": "Bitbucket Server/Data Center is not supported"
        }))
        .unwrap();
        assert!(server.server_detected);
        assert!(!server.supported);
        let encoded = serde_json::to_value(&server).unwrap();
        assert_eq!(encoded["server_detected"], true);
    }

    #[test]
    fn forge_status_rows_are_bounded_and_secret_free_by_shape() {
        let row = ForgeProviderStatus {
            provider: "bitbucket".into(),
            change_name: "PR".into(),
            transport: "rest".into(),
            command: None,
            command_available: true,
            auth_configured: Some(false),
            credential_source: None,
            supported: true,
            detail: "Bitbucket credentials are not configured".into(),
        };
        let value = serde_json::to_value(row).unwrap();
        assert!(value.get("command").is_none());
        assert!(value.get("credential_source").is_none());
        assert!(value.get("auth_configured").is_some());
    }
}

#[cfg(test)]
mod archive_media_protocol_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_artifact_json_decodes_without_provenance() {
        let value = json!({
            "id": "art-1",
            "session_id": "s1",
            "agent_id": "a1",
            "agent_name": "Agent",
            "name": "notes.md",
            "kind": "note",
            "mime_type": "text/markdown",
            "content": "hello",
            "created_at": "2026-11-12T00:00:00Z"
        });
        let artifact: Artifact = serde_json::from_value(value).unwrap();
        assert!(artifact.provenance.is_none());
        assert_eq!(artifact.storage, "");
        assert_eq!(artifact.version, 1);
        let encoded = serde_json::to_value(&artifact).unwrap();
        assert!(encoded.get("provenance").is_none());
    }

    #[test]
    fn archive_requests_are_additive_and_defaulted() {
        let list: ArtifactArchiveListParams =
            serde_json::from_value(json!({"artifact_id": "art-1"})).unwrap();
        assert!(list.max_entries.is_none());

        let extract: ArtifactArchiveExtractParams =
            serde_json::from_value(json!({"artifact_id": "art-1"})).unwrap();
        assert!(extract.entries.is_empty());
        assert!(!extract.all);
        assert!(extract.max_entries.is_none());
        assert!(extract.include_previews.is_none());

        let transcode: MediaTranscodeParams =
            serde_json::from_value(json!({"artifact_id": "art-1", "format": "mp4"})).unwrap();
        assert!(!transcode.approved);
        assert!(transcode.max_duration_secs.is_none());
        assert!(transcode.max_bytes.is_none());
    }

    #[test]
    fn archive_limits_and_provenance_round_trip() {
        let limits = ArchiveLimits::default();
        let encoded = serde_json::to_value(&limits).unwrap();
        assert_eq!(encoded["max_entries"], 2000);
        assert_eq!(encoded["max_archive_bytes"], 67_108_864);
        let decoded: ArchiveLimits = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, limits);

        let provenance = ArtifactProvenance {
            operation: "archive_extract".to_string(),
            source_artifact_id: Some("art-1".to_string()),
            entry_path: Some("docs/a.txt".to_string()),
            archive_format: Some("zip".to_string()),
            extraction_id: Some("extract-1".to_string()),
            ..Default::default()
        };
        let artifact = Artifact {
            id: "art-2".to_string(),
            session_id: "s1".to_string(),
            agent_id: "daemon".to_string(),
            agent_name: "Archive".to_string(),
            task_id: None,
            name: "a.txt".to_string(),
            kind: "archive-entry".to_string(),
            mime_type: "text/plain".to_string(),
            content: String::new(),
            storage: "file".to_string(),
            compression: "zstd".to_string(),
            size_bytes: 4,
            sha256: "a".repeat(64),
            path: None,
            data_base64: None,
            version: 1,
            provenance: Some(provenance.clone()),
            created_at: "2026-11-12T00:00:00Z".to_string(),
        };
        let decoded: Artifact =
            serde_json::from_value(serde_json::to_value(&artifact).unwrap()).unwrap();
        assert_eq!(decoded.provenance, Some(provenance));
    }

    #[test]
    fn media_info_defaults_streams_tags_and_warnings() {
        let value = json!({
            "container": "mp4",
            "source": "ffprobe"
        });
        let info: MediaInfo = serde_json::from_value(value).unwrap();
        assert!(info.streams.is_empty());
        assert!(info.tags.is_empty());
        assert!(info.warnings.is_empty());
        assert_eq!(info.source, "ffprobe");
    }
}

#[cfg(test)]
mod cache_epoch_protocol_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cache_epoch_fields_default_on_legacy_status_and_call_payloads() {
        let status: SessionCacheStatus = serde_json::from_value(json!({
            "session_id": "ses-1",
            "input_tokens": 10,
        }))
        .unwrap();
        assert!(status.cache_epoch.is_none());
        assert_eq!(status.cache_epoch_session_count, 0);
        assert_eq!(status.prefix_reused_from_sessions, 0);
        assert_eq!(status.compaction_lost_cache_usd, 0.0);

        let call: SessionCacheCall = serde_json::from_value(json!({
            "at": "now",
            "input_tokens": 100,
        }))
        .unwrap();
        assert!(call.cache_epoch.is_none());
        assert_eq!(call.cache_epoch_reused_from_sessions, 0);
        assert_eq!(call.cross_session_cache_read_tokens, 0);
        assert!(!call.cross_session_first_call);
    }

    #[test]
    fn cache_epoch_status_round_trips_final_identity_and_reuse() {
        let status = SessionCacheStatus {
            session_id: "ses-1".into(),
            cache_epoch: Some("a".repeat(64)),
            cache_epoch_source: Some("l4".into()),
            cache_epoch_first_seen: Some("2026-11-12T00:00:00Z".into()),
            cache_epoch_last_seen: Some("2026-11-12T00:05:00Z".into()),
            cache_epoch_session_count: 3,
            cache_epoch_call_count: 12,
            cache_epoch_observed_input_tokens: 90_000,
            cache_epoch_observed_cache_read_tokens: 88_000,
            cache_epoch_identity_hash: Some("b".repeat(64)),
            cache_epoch_identity_id: Some("agt-1".into()),
            cache_epoch_identity_generation: Some(3),
            cache_epoch_identity_sections: vec![PromptSectionRef::new(
                "agent/identity",
                "b".repeat(64),
                15,
                false,
            )],
            cache_epochs_seen: 1,
            prefix_reused_from_sessions: 2,
            cross_session_hit_tokens: 1_200,
            cross_session_first_calls: 1,
            provider_cache_misses: 1,
            prefix_changes: 0,
            compaction_cache_resets: 1,
            compaction_lost_cache_usd: 0.01,
            cache_calls: 10,
            ..SessionCacheStatus::default()
        };
        let decoded: SessionCacheStatus =
            serde_json::from_value(serde_json::to_value(&status).unwrap()).unwrap();
        assert_eq!(decoded, status);
        assert_eq!(decoded.cache_epoch_session_count, 3);
        assert_eq!(decoded.cross_session_hit_tokens, 1_200);
        let expected_identity_hash = "b".repeat(64);
        assert_eq!(
            decoded.cache_epoch_identity_hash.as_deref(),
            Some(expected_identity_hash.as_str())
        );
        assert_eq!(decoded.cache_epoch_identity_id.as_deref(), Some("agt-1"));
        assert_eq!(decoded.cache_epoch_identity_generation, Some(3));
        assert_eq!(decoded.cache_epoch_identity_sections.len(), 1);
        assert_eq!(decoded.cache_epoch_identity_sections[0].ordinal, 15);

        let legacy: SessionCacheStatus = serde_json::from_value(json!({
            "session_id": "ses-legacy"
        }))
        .unwrap();
        assert!(legacy.cache_epoch_identity_hash.is_none());
        assert!(legacy.cache_epoch_identity_id.is_none());
        assert!(legacy.cache_epoch_identity_generation.is_none());
        assert!(legacy.cache_epoch_identity_sections.is_empty());
    }

    #[test]
    fn session_cache_epoch_result_and_timeline_round_trip() {
        let result = SessionCacheEpochResult {
            session_id: "ses-1".into(),
            project_id: "prj-1".into(),
            prompt_layout: 3,
            current_epoch: Some(CacheEpochSummary {
                cache_epoch: "b".repeat(64),
                model: "deepseek-flash".into(),
                session_count: 2,
                call_count: 4,
                observed_input_tokens: 4_000,
                observed_cache_read_tokens: 3_600,
                cache_hit_ratio: 0.9,
                this_session_calls: 2,
                reused_from_sessions: 1,
                ..CacheEpochSummary::default()
            }),
            epochs: Vec::new(),
            timeline: vec![
                CacheEpochTimelineEntry {
                    at: "2026-11-12T00:00:00Z".into(),
                    kind: "prefix".into(),
                    cache_epoch: Some("b".repeat(64)),
                    change_reason: Some("first".into()),
                    model: Some("deepseek-flash".into()),
                    tool_bundle: Some("leader".into()),
                    prefix_hash: Some("c".repeat(64)),
                    compaction_count: None,
                },
                CacheEpochTimelineEntry {
                    at: "2026-11-12T00:01:00Z".into(),
                    kind: "compaction_reset".into(),
                    cache_epoch: Some("b".repeat(64)),
                    change_reason: Some("compaction".into()),
                    compaction_count: Some(1),
                    ..CacheEpochTimelineEntry::default()
                },
            ],
            reused_sessions: vec![CacheEpochReuseSession {
                session_id: "ses-2".into(),
                title: "earlier run".into(),
                calls: 2,
                observed_input_tokens: 2_000,
                observed_cache_read_tokens: 1_800,
                cache_hit_ratio: 0.9,
                ..CacheEpochReuseSession::default()
            }],
            total_epochs: 1,
            timeline_truncated: false,
            prefix_reused_from_sessions: 1,
            cross_session_hit_tokens: 900,
            cross_session_first_calls: 1,
            provider_cache_misses: 0,
            compaction_resets: 1,
            compaction_lost_cache_usd: 0.002,
            generated_at: "2026-11-12T00:02:00Z".into(),
        };
        let encoded = serde_json::to_string(&result).unwrap();
        let decoded: SessionCacheEpochResult = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, result);
        assert_eq!(decoded.timeline[1].kind, "compaction_reset");
    }

    #[test]
    fn prompt_prefix_rows_and_records_round_trip() {
        let observation = PromptPrefixObservation {
            project_id: "prj".into(),
            cache_epoch: "d".repeat(64),
            session_id: "ses".into(),
            model: "model".into(),
            tool_bundle: "leader".into(),
            tool_bundle_hash: "bundle".into(),
            core_hash: "core".into(),
            project_hash: "project".into(),
            core_sections: vec![PromptSectionRef::new("core/charter", "hash", 10, false)],
            project_sections: vec![PromptSectionRef::new("project/sources", "hash2", 30, false)],
            first_seen: "a".into(),
            last_seen: "b".into(),
            observed_input_tokens: 10,
            observed_cache_read_tokens: 9,
            calls: 1,
        };
        let decoded: PromptPrefixObservation =
            serde_json::from_str(&serde_json::to_string(&observation).unwrap()).unwrap();
        assert_eq!(decoded, observation);
        assert_eq!(decoded.core_sections[0].ordinal, 10);

        let record = PromptPrefixRecord {
            first_session_call: true,
            prior_sessions: 2,
            aggregate: PromptPrefixAggregate {
                project_id: "prj".into(),
                cache_epoch: "d".repeat(64),
                session_count: 3,
                ..PromptPrefixAggregate::default()
            },
        };
        let decoded: PromptPrefixRecord =
            serde_json::from_str(&serde_json::to_string(&record).unwrap()).unwrap();
        assert_eq!(decoded, record);
    }

    #[test]
    fn compaction_summary_epoch_fields_default_and_round_trip() {
        let legacy: ContextCompactionSummary = serde_json::from_value(json!({
            "mode": "summary",
            "compaction_count": 1,
        }))
        .unwrap();
        assert!(legacy.cache_epoch_before.is_none());
        assert!(!legacy.cache_epoch_changed);
        assert_eq!(legacy.estimated_lost_cache_read_usd, 0.0);

        let mut current = legacy;
        current.cache_epoch_before = Some("e".repeat(64));
        current.cache_epoch_after = Some("e".repeat(64));
        current.cache_epoch_changed = false;
        current.estimated_lost_cache_read_usd = 0.0123;
        let decoded: ContextCompactionSummary =
            serde_json::from_str(&serde_json::to_string(&current).unwrap()).unwrap();
        assert_eq!(decoded, current);
    }

    #[test]
    fn project_epoch_prune_params_and_result_decode_with_defaults() {
        let params: ProjectCacheEpochPruneParams =
            serde_json::from_value(json!({"project_id": "prj"})).unwrap();
        assert_eq!(params.project_id, "prj");
        assert!(params.older_than_days.is_none());
        assert!(params.keep_epochs.is_none());
        assert!(!params.dry_run);

        let result = ProjectCacheEpochPruneResult {
            project_id: "prj".into(),
            dry_run: true,
            cutoff: "2026-01-01T00:00:00Z".into(),
            removed_epochs: 2,
            removed_observations: 3,
            kept_epochs: 50,
            remaining_epochs: 48,
            protected_epochs: 1,
        };
        let decoded: ProjectCacheEpochPruneResult =
            serde_json::from_str(&serde_json::to_string(&result).unwrap()).unwrap();
        assert_eq!(decoded, result);
        let minimal: ProjectCacheEpochPruneResult =
            serde_json::from_value(json!({"project_id": "prj"})).unwrap();
        assert_eq!(minimal.kept_epochs, 0);
        assert!(!minimal.dry_run);
    }

    #[test]
    fn warm_on_resume_remains_opted_out_by_default() {
        let policy = CachePolicy::default();
        assert!(!policy.warm_on_resume);
        assert!(policy.compact_aware);
        let decoded: CachePolicy = serde_json::from_value(json!({})).unwrap();
        assert!(!decoded.warm_on_resume);
    }

    #[test]
    fn context_usage_threshold_policy_fields_decode_with_defaults() {
        let legacy: ContextUsage = serde_json::from_value(json!({
            "estimated_tokens": 10,
            "threshold_tokens": 5,
        }))
        .unwrap();
        assert!(legacy.threshold_source.is_none());
        assert_eq!(legacy.threshold_ratio_basis_points, 0);
        let current: ContextUsage = serde_json::from_value(json!({
            "estimated_tokens": 10,
            "threshold_tokens": 5,
            "threshold_source": "window_ratio",
            "threshold_ratio_basis_points": 9000,
        }))
        .unwrap();
        assert_eq!(current.threshold_source.as_deref(), Some("window_ratio"));
        assert_eq!(current.threshold_ratio_basis_points, 9000);
    }

    #[test]
    fn session_cache_epoch_params_default_to_optional_limit() {
        let params: SessionCacheEpochParams =
            serde_json::from_value(json!({"session_id": "ses-1"})).unwrap();
        assert_eq!(params.session_id, "ses-1");
        assert_eq!(params.limit, None);
    }

    #[test]
    fn project_cache_epoch_report_round_trips_bundle_buckets() {
        let result = ProjectCacheEpochResult {
            project_id: "prj-1".into(),
            epochs: vec![CacheEpochSummary {
                cache_epoch: "f".repeat(64),
                model: "deepseek-flash".into(),
                tool_bundle: "coding".into(),
                session_count: 4,
                call_count: 9,
                observed_input_tokens: 9_000,
                observed_cache_read_tokens: 8_100,
                cache_hit_ratio: 0.9,
                reused_from_sessions: 3,
                ..CacheEpochSummary::default()
            }],
            bundles: vec![CacheEpochBundleBucket {
                tool_bundle: "coding".into(),
                epochs: 2,
                sessions: 5,
                calls: 12,
                observed_input_tokens: 12_000,
                observed_cache_read_tokens: 10_800,
                cache_hit_ratio: 0.9,
            }],
            total_epochs: 2,
            total_sessions: 5,
            total_calls: 12,
            observed_input_tokens: 12_000,
            observed_cache_read_tokens: 10_800,
            cache_hit_ratio: 0.9,
            generated_at: "2026-11-12T00:00:00Z".into(),
        };
        let decoded: ProjectCacheEpochResult =
            serde_json::from_str(&serde_json::to_string(&result).unwrap()).unwrap();
        assert_eq!(decoded, result);
        assert_eq!(decoded.bundles[0].tool_bundle, "coding");
        assert_eq!(decoded.epochs[0].reused_from_sessions, 3);
    }

    #[test]
    fn project_cache_epoch_params_decode_without_limit() {
        let params: ProjectCacheEpochParams =
            serde_json::from_value(json!({"project_id": "prj-1"})).unwrap();
        assert_eq!(params.project_id, "prj-1");
        assert_eq!(params.limit, None);
    }

    #[test]
    fn reuse_session_decodes_legacy_defaults_without_title() {
        let row: CacheEpochReuseSession = serde_json::from_value(json!({
            "session_id": "ses-1",
            "calls": 2,
        }))
        .unwrap();
        assert_eq!(row.title, "");
        assert_eq!(row.calls, 2);
        assert_eq!(row.cache_hit_ratio, 0.0);
    }

    #[test]
    fn epoch_family_decodes_minimal_and_legacy_payloads() {
        let summary: CacheEpochSummary = serde_json::from_value(json!({})).unwrap();
        assert_eq!(summary.cache_epoch, "");
        assert_eq!(summary.session_count, 0);
        assert_eq!(summary.cache_hit_ratio, 0.0);

        let entry: CacheEpochTimelineEntry = serde_json::from_value(json!({
            "at": "2026-11-12T00:00:00Z",
            "kind": "prefix",
        }))
        .unwrap();
        assert!(entry.cache_epoch.is_none());
        assert!(entry.change_reason.is_none());
        assert_eq!(entry.at, "2026-11-12T00:00:00Z");

        let bucket: CacheEpochBundleBucket = serde_json::from_value(json!({
            "tool_bundle": "minimal",
        }))
        .unwrap();
        assert_eq!(bucket.tool_bundle, "minimal");
        assert_eq!(bucket.epochs, 0);
        assert_eq!(bucket.cache_hit_ratio, 0.0);

        let aggregate: PromptPrefixAggregate = serde_json::from_value(json!({})).unwrap();
        assert!(aggregate.cache_epoch.is_empty());
        assert!(aggregate.core_sections.is_empty());
        assert_eq!(aggregate.call_count, 0);

        let observation: PromptPrefixObservation = serde_json::from_value(json!({
            "cache_epoch": "a".repeat(64),
        }))
        .unwrap();
        assert_eq!(observation.calls, 0);
        assert_eq!(observation.observed_input_tokens, 0);
    }

    #[test]
    fn session_bundle_legacy_payload_defaults_epoch_observations() {
        let bundle: SessionBundle = serde_json::from_value(json!({
            "version": 1,
            "exported_at": "2026-11-12T00:00:00Z",
            "project": {
                "id": "prj",
                "name": "p",
                "root": "/tmp/p",
                "default_branch": "main",
                "created_at": "2026-11-12T00:00:00Z"
            },
            "session": {
                "id": "ses",
                "project_id": "prj",
                "goal": "g",
                "status": "idle",
                "priority": 0,
                "tokens_in": 0,
                "tokens_out": 0,
                "unlimited": false,
                "cost_spent_usd": 0.0,
                "created_at": "2026-11-12T00:00:00Z"
            },
            "events": [],
            "agents": [],
            "messages": [],
            "todos": [],
            "merge_queue": []
        }))
        .unwrap();
        assert!(bundle.prompt_prefix_observations.is_empty());
        assert!(bundle.instructions.is_empty());
    }
}

#[cfg(test)]
mod contract_protocol_tests {
    use super::*;

    #[test]
    fn contract_policy_defaults_and_mode_decode() {
        let policy: ContractPolicy = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(policy.enabled);
        assert_eq!(policy.mode, ContractMode::Enforce);
        assert!(!policy.enforce_shell);
        assert!(policy.max_writes.is_none());

        let policy: ContractPolicy = serde_json::from_value(serde_json::json!({
            "mode": "advisory",
            "enforce_shell": true,
            "forbidden_paths": ["secrets"],
            "egress_allowlist": ["example.com"],
            "max_writes": 3
        }))
        .unwrap();
        assert_eq!(policy.mode, ContractMode::Advisory);
        assert_eq!(policy.normalized().unwrap().max_writes, Some(3));
    }

    #[test]
    fn session_contract_result_old_shapes_decode_with_defaults() {
        let result: SessionContractResult = serde_json::from_value(serde_json::json!({
            "session_id": "ses-1"
        }))
        .unwrap();
        assert_eq!(result.session_id, "ses-1");
        assert!(result.contract.is_none());
        assert!(result.policy.enabled);
        assert!(result.recent_decisions.is_empty());

        let metadata: TaskContractMetadata = serde_json::from_value(serde_json::json!({
            "hash": "abc",
            "role": "worker"
        }))
        .unwrap();
        assert_eq!(metadata.hash, "abc");
        assert_eq!(metadata.enforcement_mode, "");
    }

    #[test]
    fn mcp_tool_pin_status_and_trust_decode() {
        let pin: McpToolPin = serde_json::from_value(serde_json::json!({
            "project_id": "prj",
            "server_id": "srv",
            "tool_name": "echo",
            "status": "drift"
        }))
        .unwrap();
        assert_eq!(pin.trust(), "drift");
        assert!(!pin.callable());
        let pinned: McpToolPin = serde_json::from_value(serde_json::json!({
            "status": "pinned"
        }))
        .unwrap();
        assert!(pinned.callable());
    }

    #[test]
    fn effect_receipt_fields_decode_with_safe_defaults() {
        let receipt: EffectReceipt = serde_json::from_value(serde_json::json!({
            "id": "effect-1",
            "session_id": "ses",
            "tool": "write_file",
            "target": {"kind": "workspace_write", "key": "src/lib.rs"},
            "request_hash": "abc",
            "state": "unknown"
        }))
        .unwrap();
        assert_eq!(receipt.state, EffectState::Unknown);
        assert_eq!(receipt.attempt, 0);
        assert!(receipt.task_id.is_none());
        assert!(receipt.evidence.paths.is_empty());
        assert!(receipt.evidence.sha256.is_none());
        assert!(receipt.evidence.caveat.is_none());

        let settled: EffectReceipt = serde_json::from_value(serde_json::json!({
            "state": "done",
            "target": {"kind": "git_push", "key": "origin/feature"},
            "evidence": {"sha256": "deadbeef", "http_status": 201}
        }))
        .unwrap();
        assert_eq!(settled.state, EffectState::Done);
        assert_eq!(settled.evidence.http_status, Some(201));
        assert_eq!(settled.target.label(), "git_push:origin/feature");
        assert_eq!(
            EffectTarget::parse(&settled.target.canonical()),
            settled.target
        );
    }

    #[test]
    fn effect_policy_and_state_wire_defaults_are_narrow() {
        let policy: EffectPolicy = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(policy.enabled);
        assert!(policy.unknown_blocks_merge);
        assert_eq!(policy.stale_intent_grace_secs, 120);
        assert_eq!(policy.shell_mode, EffectShellMode::ExternalOnly);
        assert!(!policy.track_read_http);
        assert!(policy.track_mutating_http);
        assert!(policy.track_mcp_mutations);
        assert!(policy.track_workspace_writes);
        assert!(EffectState::Intent.blocks_merge());
        assert!(EffectState::Unknown.blocks_merge());
        assert!(!EffectState::Done.blocks_merge());
        assert!(!EffectState::Failed.blocks_merge());
        assert!(EffectState::Done.is_terminal());
        assert!(EffectState::parse("UNKNOWN").is_some());
        assert!(EffectState::parse("not-a-state").is_none());
    }

    #[test]
    fn effect_reconcile_decision_decodes_and_counts_are_stable() {
        let params: EffectReconcileParams = serde_json::from_value(serde_json::json!({
            "effect_id": "effect-1",
            "decision": "done",
            "evidence": {"note": "verified on the remote host"}
        }))
        .unwrap();
        assert_eq!(params.decision, EffectState::Done);
        assert!(params.session_id.is_none());
        assert!(params.operator.is_none());
        let counts = EffectCounts {
            total: 4,
            intent: 1,
            done: 1,
            failed: 1,
            unknown: 1,
            blocking: 2,
        };
        let encoded = serde_json::to_value(counts).unwrap();
        assert_eq!(encoded["blocking"], 2);
    }
}

#[cfg(test)]
mod gate_protocol_tests {
    use super::*;

    #[test]
    fn gate_kind_state_and_outcome_parse_stably() {
        assert_eq!(GateKind::parse("CI"), Some(GateKind::Ci));
        assert_eq!(GateKind::parse("pr-review"), Some(GateKind::PrReview));
        assert_eq!(GateKind::parse("deploy"), Some(GateKind::Deploy));
        assert_eq!(GateKind::parse("human"), Some(GateKind::Human));
        assert_eq!(GateKind::parse("custom"), Some(GateKind::Custom));
        assert!(GateKind::parse("jenkins").is_none());
        assert_eq!(GateState::parse("canceled"), Some(GateState::Cancelled));
        assert!(!GateState::Open.is_terminal());
        assert!(GateState::Expired.is_terminal());
        assert_eq!(GateOutcome::parse("approved"), Some(GateOutcome::Resolved));
        assert_eq!(GateOutcome::parse("denied"), Some(GateOutcome::Rejected));
        assert_eq!(GateOutcome::parse("error"), Some(GateOutcome::Failed));
    }

    #[test]
    fn gate_policy_defaults_to_local_only() {
        let policy: GatePolicy = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(!policy.loopback_resolve);
        let params: GateOpenParams = serde_json::from_value(serde_json::json!({
            "session_id": "ses-1"
        }))
        .unwrap();
        assert!(params.kind.is_none());
        assert!(params.task_id.is_none());
        assert!(params.payload.is_none());
        assert!(params.expires_at.is_none());
        assert!(params.after_secs.is_none());
    }

    #[test]
    fn gate_public_projection_has_no_token_hash_field() {
        let gate = ExternalGate {
            id: "gate-1".into(),
            session_id: "ses-1".into(),
            kind: GateKind::Ci,
            label: "unit tests".into(),
            state: GateState::Open,
            ..ExternalGate::default()
        };
        let encoded = serde_json::to_value(&gate).unwrap();
        assert!(encoded.get("token").is_none());
        assert!(encoded.get("token_hash").is_none());
        assert_eq!(encoded["kind"], "ci");
        assert_eq!(encoded["state"], "open");

        let record = GateRecord {
            gate,
            token_hash: "a".repeat(64),
        };
        let encoded = serde_json::to_value(&record).unwrap();
        assert_eq!(encoded["token_hash"], "a".repeat(64));
    }

    #[test]
    fn continuation_gate_watch_and_bundle_gates_are_additive() {
        let continuation: SessionContinuation = serde_json::from_value(serde_json::json!({
            "id": "cont-1",
            "session_id": "ses-1",
            "project_id": "prj-1",
            "body": "resume",
            "status": "pending",
            "created_at": "2026-11-12T00:00:00Z",
            "updated_at": "2026-11-12T00:00:00Z"
        }))
        .unwrap();
        assert!(continuation.watch_gate_id.is_none());
        assert!(continuation.watch_task_ids.is_empty());

        let params: SessionDeferParams = serde_json::from_value(serde_json::json!({
            "session_id": "ses-1",
            "body": "resume after CI",
            "watch_gate_id": "gate-1"
        }))
        .unwrap();
        assert_eq!(params.watch_gate_id.as_deref(), Some("gate-1"));
    }

    #[test]
    fn gate_kinds_and_outcomes_round_trip_on_the_wire() {
        for (kind, wire) in [
            (GateKind::Ci, "ci"),
            (GateKind::PrReview, "pr_review"),
            (GateKind::Deploy, "deploy"),
            (GateKind::Human, "human"),
            (GateKind::Custom, "custom"),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), wire);
            assert_eq!(GateKind::parse(wire), Some(kind));
        }
        for (outcome, wire) in [
            (GateOutcome::Resolved, "resolved"),
            (GateOutcome::Rejected, "rejected"),
            (GateOutcome::Failed, "failed"),
        ] {
            assert_eq!(serde_json::to_value(outcome).unwrap(), wire);
            assert_eq!(GateOutcome::parse(wire), Some(outcome));
        }
        for (state, wire) in [
            (GateState::Open, "open"),
            (GateState::Resolved, "resolved"),
            (GateState::Expired, "expired"),
            (GateState::Cancelled, "cancelled"),
        ] {
            assert_eq!(serde_json::to_value(state).unwrap(), wire);
            assert_eq!(GateState::parse(wire), Some(state));
        }
    }

    #[test]
    fn gate_sweep_result_uses_a_specific_key() {
        let result = GateSweepResult { expired_gates: 3 };
        let encoded = serde_json::to_value(result).unwrap();
        assert_eq!(encoded["expired_gates"], 3);
        assert!(encoded.get("expired").is_none());
        let params: GateSweepParams = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(params.limit.is_none());
    }

    #[test]
    fn gate_results_serialize_without_private_hash() {
        let result = GateResolveResult {
            gate: ExternalGate {
                id: "gate-1".into(),
                session_id: "ses-1".into(),
                state: GateState::Resolved,
                outcome: Some(GateOutcome::Resolved),
                ..ExternalGate::default()
            },
            changed: true,
            continuation_id: Some("cont-1".into()),
        };
        let encoded = serde_json::to_value(&result).unwrap();
        assert!(encoded.get("token").is_none());
        assert!(encoded.get("token_hash").is_none());
        assert_eq!(encoded["continuation_id"], "cont-1");

        let open = GateOpenResult {
            gate: ExternalGate::default(),
            token: "vpg_once".into(),
            resolve_hint: "vp gate resolve".into(),
            loopback_resolve: false,
        };
        let encoded = serde_json::to_value(&open).unwrap();
        assert_eq!(encoded["token"], "vpg_once");
        assert_eq!(encoded["loopback_resolve"], false);
        let debug = format!("{open:?}");
        assert!(!debug.contains("vpg_once"));
        assert!(debug.contains("redacted"));

        let public_gate = serde_json::to_value(ExternalGate {
            id: "g1".into(),
            session_id: "s1".into(),
            kind: GateKind::Deploy,
            label: "staging".into(),
            state: GateState::Cancelled,
            resolution_note: Some("operator cancelled".into()),
            ..ExternalGate::default()
        })
        .unwrap();
        assert_eq!(public_gate["kind"], "deploy");
        assert_eq!(public_gate["state"], "cancelled");
        assert!(public_gate.get("token_hash").is_none());
        assert!(public_gate.get("token").is_none());
    }

    #[test]
    fn gate_resolve_defaults_outcome_and_operator_label() {
        let params: GateResolveParams = serde_json::from_value(serde_json::json!({
            "gate_id": "gate-1",
            "token": "vpg_secret"
        }))
        .unwrap();
        assert!(params.outcome.is_none());
        assert!(params.note.is_none());
        assert!(params.resolved_by.is_none());
        let status: GateStatusParams = serde_json::from_value(serde_json::json!({
            "gate_id": "gate-1"
        }))
        .unwrap();
        assert_eq!(status.gate_id, "gate-1");
        let list: GateListParams = serde_json::from_value(serde_json::json!({
            "session_id": "ses-1"
        }))
        .unwrap();
        assert!(list.state.is_none());
        assert!(list.limit.is_none());
        let cancel: GateCancelParams = serde_json::from_value(serde_json::json!({
            "gate_id": "gate-1"
        }))
        .unwrap();
        assert!(cancel.reason.is_none());
        assert!(cancel.cancelled_by.is_none());
    }
}

#[cfg(test)]
mod agent_identity_protocol_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn policy_defaults_disabled_and_decodes_partial_objects() {
        let policy: AgentIdentityPolicy = serde_json::from_value(json!({})).unwrap();
        assert!(!policy.enabled);
        assert_eq!(policy.max_managers, 4);
        assert_eq!(policy.max_workers, 10);
        assert!(policy.hire_requires_approval);
        assert!(policy.retirement_requires_approval);
        let policy: AgentIdentityPolicy =
            serde_json::from_value(json!({"enabled": true, "max_managers": 1})).unwrap();
        assert!(policy.enabled);
        assert_eq!(policy.max_managers, 1);
        assert_eq!(policy.max_workers, 10);
        assert!(policy.normalized().is_ok());
        let invalid = AgentIdentityPolicy {
            max_workers: 100,
            ..AgentIdentityPolicy::default()
        };
        assert!(invalid.normalized().is_err());
    }

    #[test]
    fn identity_decodes_with_additive_defaults_and_old_agent_records_still_decode() {
        let identity: AgentIdentity = serde_json::from_value(json!({
            "id": "id-1",
            "project_id": "prj-1",
            "role": "worker",
            "status": "active"
        }))
        .unwrap();
        assert_eq!(identity.id, "id-1");
        assert_eq!(identity.generation, 1);
        assert_eq!(identity.policy, json!({}));
        assert_eq!(identity.origin, "local");
        assert!(identity.department.is_none());

        let legacy_agent: AgentRecord = serde_json::from_value(json!({
            "id": "run-1",
            "session_id": "ses-1",
            "role": "leader",
            "name": "Leader",
            "parent_id": null,
            "status": "running",
            "model": "m",
            "created_at": "2026-11-12T00:00:00Z"
        }))
        .unwrap();
        assert!(legacy_agent.identity_id.is_none());
    }

    #[test]
    fn update_params_distinguish_absent_null_and_value() {
        let absent: UpdateAgentIdentityParams =
            serde_json::from_value(json!({"identity_id": "id-1"})).unwrap();
        assert!(absent.department.is_none());
        let null: UpdateAgentIdentityParams =
            serde_json::from_value(json!({"identity_id": "id-1", "department": null})).unwrap();
        assert_eq!(null.department, Some(None));
        let value: UpdateAgentIdentityParams =
            serde_json::from_value(json!({"identity_id": "id-1", "department": "platform"}))
                .unwrap();
        assert_eq!(value.department, Some(Some("platform".to_string())));
    }

    #[test]
    fn results_and_replay_reports_decode_with_bounded_defaults() {
        let result: AgentIdentityResult = serde_json::from_value(json!({})).unwrap();
        assert!(!result.changed);
        assert!(result.identity.id.is_empty());
        assert_eq!(result.identity.generation, 1);
        let list: AgentIdentitiesResult = serde_json::from_value(json!({})).unwrap();
        assert!(list.identities.is_empty());
        let report: AgentIdentityReplayReport = serde_json::from_value(json!({})).unwrap();
        assert!(!report.consistent);
        assert!(!report.repaired);
        assert!(report.mismatches.is_empty());
        let policy: AgentIdentityPolicyResult = serde_json::from_value(json!({
            "project_id": "prj-1"
        }))
        .unwrap();
        assert!(!policy.policy.enabled);
        assert_eq!(policy.project_id, "prj-1");
    }

    #[test]
    fn event_payload_and_code_values_are_stable() {
        let event: AgentIdentityEvent = serde_json::from_value(json!({
            "identity_id": "id-1",
            "project_id": "prj-1",
            "seq": 3,
            "type": "agent.identity.paused",
            "payload": {"identity": {"id": "id-1"}, "changed_fields": ["status"]},
            "created_at": "2026-11-12T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(event.kind, "agent.identity.paused");
        assert_eq!(event.payload["changed_fields"], json!(["status"]));
        assert_eq!(
            agent_identity_codes::IDENTITY_ROLE_INVALID,
            "IDENTITY_ROLE_INVALID"
        );
        assert!(AGENT_IDENTITY_ROLES.contains(&"leader"));
        assert!(!AGENT_IDENTITY_ROLES.contains(&"labour"));
    }
}

#[cfg(test)]
mod agent_coordination_protocol_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn assignment_and_inbox_decoders_are_additive() {
        let assignment: AgentAssignment = serde_json::from_value(json!({
            "session_id": "ses-1",
            "project_id": "prj-1",
            "identity_id": "id-1",
            "role": "worker",
            "status": "active",
            "assigned_at": "2026-11-13T00:00:00Z",
            "heartbeat_at": "2026-11-13T00:00:00Z",
            "lease_expires_at": "2026-11-13T00:10:00Z"
        }))
        .unwrap();
        assert!(!assignment.lease_active);
        assert!(assignment.released_at.is_none());

        let message: AgentInboxMessage = serde_json::from_value(json!({
            "id": "msg-1",
            "project_id": "prj-1",
            "identity_id": "id-1",
            "kind": "message",
            "status": "unread"
        }))
        .unwrap();
        assert_eq!(message.payload, json!({}));
        assert_eq!(message.from_label, "");
        assert!(message.session_id.is_none());

        let list: AgentInboxListResult = serde_json::from_value(json!({})).unwrap();
        assert!(list.messages.is_empty());
        assert_eq!(list.counts.unread, 0);
        let params: AgentInboxSendParams = serde_json::from_value(json!({
            "identity_id": "id-1"
        }))
        .unwrap();
        assert!(params.project_id.is_none());
        assert!(!params.wake_session);
        assert!(params.kind.is_empty());
        let assign: SessionAssignParams = serde_json::from_value(json!({
            "session_id": "ses-1",
            "identity_id": "id-1",
            "wake_session": true
        }))
        .unwrap();
        assert!(assign.lease_secs.is_none());
        assert!(assign.wake_session);
    }

    #[test]
    fn lease_and_ttl_bounds_fail_closed() {
        assert_eq!(
            normalize_assignment_lease_secs(None).unwrap(),
            AGENT_ASSIGNMENT_DEFAULT_LEASE_SECS
        );
        assert!(normalize_assignment_lease_secs(Some(59)).is_err());
        assert!(normalize_assignment_lease_secs(Some(86_401)).is_err());
        assert_eq!(
            normalize_assignment_lease_secs(Some(60)).unwrap(),
            AGENT_ASSIGNMENT_MIN_LEASE_SECS
        );
        assert_eq!(normalize_inbox_ttl_secs(None).unwrap(), None);
        assert!(normalize_inbox_ttl_secs(Some(59)).is_err());
        assert_eq!(normalize_inbox_ttl_secs(Some(60)).unwrap(), Some(60));
    }

    #[test]
    fn inbox_payload_is_bounded_object_and_rejects_credential_keys() {
        assert!(validate_agent_inbox_payload(&json!({"summary": "review T1"})).is_ok());
        assert!(validate_agent_inbox_payload(&json!("plain text")).is_err());
        assert!(validate_agent_inbox_payload(&json!({"api_key": "x"})).is_err());
        assert!(validate_agent_inbox_payload(&json!({"nested": [{"password": "x"}]})).is_err());
        let oversized = json!({"body": "x".repeat(AGENT_INBOX_MAX_PAYLOAD_BYTES)});
        assert!(validate_agent_inbox_payload(&oversized).is_err());
        assert!(validate_agent_inbox_payload(&json!({"context_tokens": 12})).is_ok());
    }

    #[test]
    fn event_kinds_and_codes_are_stable_wire_values() {
        assert!(AGENT_ASSIGNMENT_EVENT_KINDS.contains(&"agent.assignment.expired"));
        assert!(AGENT_INBOX_EVENT_KINDS.contains(&"agent.inbox.done"));
        assert_eq!(
            agent_coordination_codes::ASSIGNMENT_CONFLICT,
            "ASSIGNMENT_CONFLICT"
        );
        assert!(!AGENT_IDENTITY_ROLES.contains(&"labour"));
        assert!(AGENT_INBOX_KINDS.contains(&"handoff"));
    }

    #[test]
    fn bundle_new_fields_decode_as_empty_for_older_archives() {
        let bundle: SessionBundle = serde_json::from_value(json!({
            "version": 1,
            "exported_at": "2026-11-13T00:00:00Z",
            "project": {"id": "prj-1", "name": "p", "root": "/tmp/p", "default_branch": "main", "created_at": "2026-11-13T00:00:00Z"},
            "session": {"id": "ses-1", "project_id": "prj-1", "goal": "g", "status": "idle", "created_at": "2026-11-13T00:00:00Z"},
            "events": [],
            "agents": [],
            "messages": [],
            "todos": [],
            "merge_queue": []
        }))
        .unwrap();
        assert!(bundle.session_agent_assignments.is_empty());
        assert!(bundle.agent_inbox.is_empty());
        assert!(bundle.agent_coordination_events.is_empty());
    }
}

#[cfg(test)]
mod workspace_protocol_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn workspace_rows_and_bindings_decode_with_additive_defaults() {
        let workspace: AgentWorkspace = serde_json::from_value(json!({
            "id": "wsp-1",
            "project_id": "prj-1",
            "identity_id": "agt-1",
            "component": "auth",
            "branch": "verlinpark/components/auth-1234--agt-1",
            "base_commit": "abc",
            "worktree_path": "/data/workspaces/prj-1/agt-1",
            "status": "active"
        }))
        .unwrap();
        assert_eq!(workspace.status, "active");
        assert!(!workspace.lease_active);
        assert_eq!(workspace.origin, "local");
        assert!(workspace.last_session_id.is_none());

        let binding: SessionWorkerWorkspace = serde_json::from_value(json!({
            "id": "sww-1",
            "workspace_id": "wsp-1",
            "session_id": "ses-1",
            "identity_id": "agt-1",
            "worktree_path": "/data/workspaces/prj-1/ses-1",
            "branch": "verlinpark/sessions/ses-1/worker-agt-1",
            "base_commit": "abc",
            "status": "active"
        }))
        .unwrap();
        assert_eq!(binding.status, "active");
        assert!(!binding.lease_active);

        let params: AgentWorkspaceEnsureParams = serde_json::from_value(json!({
            "project_id": "prj-1",
            "identity_id": "agt-1"
        }))
        .unwrap();
        assert!(params.session_id.is_none());
        assert!(params.lease_secs.is_none());
    }

    #[test]
    fn workspace_branch_names_are_deterministic_and_ref_safe() {
        let branch = component_branch_name("agt-1", "Auth / API");
        assert!(branch.starts_with(AGENT_WORKSPACE_COMPONENT_BRANCH_PREFIX));
        assert!(!branch.contains(' '));
        assert!(!branch.contains("//"));
        assert_eq!(branch, component_branch_name("agt-1", "Auth / API"));
        assert_ne!(branch, component_branch_name("agt-2", "Auth / API"));
        assert_ne!(branch, component_branch_name("agt-1", "Auth API"));
        let session = session_workspace_branch_name("ses-1", "agt-1");
        assert!(session.starts_with(AGENT_WORKSPACE_SESSION_BRANCH_PREFIX));
        assert_ne!(session, branch);
        assert!(workspace_ref_slug("").starts_with("component-"));
        assert!(!workspace_ref_slug("../escape/..").contains('/'));
        assert!(!workspace_ref_slug("../escape/..").contains(".."));
    }

    #[test]
    fn component_and_lease_validation_is_bounded() {
        assert_eq!(normalize_workspace_component(" auth ").unwrap(), "auth");
        for invalid in ["", "a/b", "..", "a\\b", "line\nbreak"] {
            assert!(
                normalize_workspace_component(invalid).is_err(),
                "{invalid:?} should be rejected"
            );
        }
        assert_eq!(
            normalize_workspace_lease_secs(None).unwrap(),
            AGENT_WORKSPACE_DEFAULT_LEASE_SECS
        );
        assert!(normalize_workspace_lease_secs(Some(59)).is_err());
        assert_eq!(normalize_workspace_lease_secs(Some(60)).unwrap(), 60);
        assert!(normalize_workspace_lease_secs(Some(86_401)).is_err());
    }

    #[test]
    fn workspace_event_kinds_and_codes_are_stable() {
        assert!(AGENT_WORKSPACE_EVENT_KINDS.contains(&"agent.workspace.created"));
        assert!(AGENT_WORKSPACE_EVENT_KINDS.contains(&"agent.workspace.session_attached"));
        assert!(AGENT_WORKSPACE_EVENT_KINDS.contains(&"agent.workspace.labour_merged"));
        assert!(AGENT_WORKSPACE_EVENT_KINDS.contains(&"agent.workspace.landed"));
        assert!(AGENT_WORKSPACE_EVENT_KINDS.contains(&"agent.workspace.archived"));
        assert!(AGENT_WORKSPACE_EVENT_KINDS.contains(&"agent.workspace.retired"));
        assert!(AGENT_WORKSPACE_EVENT_KINDS.contains(&"agent.workspace.lease_expired"));
        assert_eq!(agent_workspace_codes::WORKSPACE_BUSY, "WORKSPACE_BUSY");
    }

    #[test]
    fn session_bundle_decodes_workspace_fields_as_empty() {
        let bundle: SessionBundle = serde_json::from_value(json!({
            "version": 1,
            "exported_at": "2026-11-13T00:00:00Z",
            "project": {"id": "prj-1", "name": "p", "root": "/tmp/p", "default_branch": "main", "created_at": "2026-11-13T00:00:00Z"},
            "session": {"id": "ses-1", "project_id": "prj-1", "goal": "g", "status": "idle", "created_at": "2026-11-13T00:00:00Z"},
            "events": [],
            "agents": [],
            "messages": [],
            "todos": [],
            "merge_queue": []
        }))
        .unwrap();
        assert!(bundle.agent_workspaces.is_empty());
        assert!(bundle.session_worker_workspaces.is_empty());
        assert!(bundle.agent_workspace_events.is_empty());
    }
}
