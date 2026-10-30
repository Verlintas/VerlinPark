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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub project_id: String,
    pub goal: String,
    pub status: String,
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub token_budget: Option<u64>,
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
pub struct CreateSessionParams {
    pub project_id: String,
    pub goal: String,
    #[serde(default)]
    pub token_budget: Option<u64>,
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
pub struct SubscribeParams {
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthParams {
    pub token: String,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub configured: bool,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PromptParams {
    pub session_id: String,
    pub content: String,
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
pub struct ReplaySessionParams {
    pub session_id: String,
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
    pub queue_matches: bool,
    pub consistent: bool,
    #[serde(default)]
    pub mismatches: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRecord {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    pub parent_id: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanDepartment {
    pub name: String,
    pub manager_title: String,
    pub profile: String,
    pub objective: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanTask {
    pub id: String,
    pub department: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub write_scope: Vec<String>,
    #[serde(default)]
    pub acceptance: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamPlan {
    pub goal: String,
    pub departments: Vec<PlanDepartment>,
    pub tasks: Vec<PlanTask>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TeamPlanParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TeamPlanResult {
    pub plan: Option<TeamPlan>,
}
