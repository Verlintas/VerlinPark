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
}
