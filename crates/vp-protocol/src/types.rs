use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
        }
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
        Ok(policy)
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
        self
    }

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
    /// vector or embedded text.
    #[serde(default)]
    pub semantic_score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SourceSearchResult {
    pub hits: Vec<SourceSearchHit>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
            volatile_hash: None,
            prompt_prefix_hash: None,
            estimated_savings_usd: 0.0,
            miss_reason: None,
            warm: false,
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
    #[serde(default)]
    pub volatile_hash: Option<String>,
    #[serde(default)]
    pub prompt_prefix_hash: Option<String>,
    #[serde(default)]
    pub estimated_savings_usd: f64,
    #[serde(default)]
    pub miss_reason: Option<String>,
    /// True when this call was an explicit `session.cache.warm` request.
    #[serde(default)]
    pub warm: bool,
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
    #[serde(default)]
    pub core_chars: u64,
    #[serde(default)]
    pub project_chars: u64,
    #[serde(default)]
    pub stable_prefix_chars: u64,
    #[serde(default)]
    pub tool_count: usize,
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
    pub created_at: String,
    pub updated_at: String,
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
    #[serde(default)]
    pub recoveries_match: bool,
    #[serde(default)]
    pub steering_matches: bool,
    #[serde(default)]
    pub continuations_match: bool,
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

        let current: SessionCacheStatus = serde_json::from_value(json!({
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
        }))
        .unwrap();
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
                core_chars: 100,
                project_chars: 50,
                stable_prefix_chars: 151,
                tool_count: 20,
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
        };
        let encoded = serde_json::to_string(&bundle).unwrap();
        let decoded: SessionBundle = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.context_checkpoints, bundle.context_checkpoints);
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
}
