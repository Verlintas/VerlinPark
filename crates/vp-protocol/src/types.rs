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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceSearchResult {
    pub hits: Vec<SourceSearchHit>,
    pub degraded: bool,
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryMatch {
    pub memory: MemoryItem,
    pub score: f64,
    pub reasons: Vec<String>,
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
}

#[derive(Debug, Clone, Serialize)]
pub struct MemorySearchResult {
    pub memories: Vec<MemoryMatch>,
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

/// A configured external agent that speaks the Agent Client Protocol over stdio.
///
/// ACP agents run as user-installed executables, are spawned inside the same
/// Seatbelt write sandbox as shell work, and are recorded as Worker agents when
/// they are delegated a task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcpAgent {
    pub id: String,
    pub name: String,
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
    pub created_at: String,
}

fn default_acp_timeout() -> u64 {
    1_800
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddAcpAgentParams {
    pub name: String,
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
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateAcpAgentParams {
    pub id: String,
    pub name: String,
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
}

#[derive(Debug, Clone, Serialize)]
pub struct AcpAgentListResult {
    pub agents: Vec<AcpAgent>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AcpProbeResult {
    pub agent_id: String,
    pub agent_name: String,
    pub version: String,
    pub protocol_version: u16,
    pub session_created: bool,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
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
    #[serde(default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionContextStatus {
    pub session_id: String,
    pub usage: Option<ContextUsage>,
    pub last_compaction: Option<ContextCompactionSummary>,
    pub last_measurement: Option<ContextMeasurement>,
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
    /// Latest prompt-prefix hash emitted by `context.prompt_prefix` (or the
    /// `prompt_prefix_hash` recorded on `context.measured`).
    #[serde(default)]
    pub prompt_prefix: Option<String>,
    /// Best-effort estimate of the system-prompt and tool-schema preamble.
    /// Heuristic, not provider-reported usage.
    #[serde(default)]
    pub preamble_tokens_estimate: Option<u64>,
    #[serde(default)]
    pub last_call_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionCacheParams {
    pub session_id: String,
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
    pub kind: String,
    pub session_id: Option<String>,
    pub acquired_at: String,
    pub heartbeat_at: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeLeaseListResult {
    pub runtime_id: String,
    pub leases: Vec<RuntimeLeaseInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeLeaseSweepResult {
    pub removed: usize,
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
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ComputerStatusParams {
    #[serde(default)]
    pub session_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
            lease_surface: "computer".into(),
            error: None,
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
}
