//! Loopback-only A2A facade wire types (C11 / ADR-0221).
//!
//! A2A is disabled by default. Enabling inbound work is a host-state decision:
//! the daemon binds `127.0.0.1`/`::1` only, requires a one-time bearer token,
//! and maps inbound tasks to bounded local questions/gates. It never grants
//! local tool authority to a remote agent.

use serde::{Deserialize, Serialize};

/// Hard maximum tasks retained by one facade.
pub const MAX_A2A_TASKS: u32 = 1_024;
/// Default maximum tasks.
pub const DEFAULT_A2A_TASKS: u32 = 128;
/// Minimum/maximum task TTL.
pub const MIN_A2A_TASK_TTL_SECS: u64 = 60;
pub const MAX_A2A_TASK_TTL_SECS: u64 = 86_400;
pub const DEFAULT_A2A_TASK_TTL_SECS: u64 = 3_600;
/// Maximum request body accepted by the facade.
pub const MAX_A2A_BODY_BYTES: u64 = 1_048_576;
/// Maximum task list page.
pub const MAX_A2A_TASK_PAGE: u32 = 200;
/// Stable error codes.
pub mod a2a_codes {
    pub const A2A_DISABLED: &str = "A2A_DISABLED";
    pub const A2A_NOT_RUNNING: &str = "A2A_NOT_RUNNING";
    pub const A2A_ALREADY_RUNNING: &str = "A2A_ALREADY_RUNNING";
    pub const A2A_INVALID_POLICY: &str = "A2A_INVALID_POLICY";
    pub const A2A_BIND_NOT_LOOPBACK: &str = "A2A_BIND_NOT_LOOPBACK";
    pub const A2A_APPROVAL_REQUIRED: &str = "A2A_APPROVAL_REQUIRED";
    pub const A2A_TASK_NOT_FOUND: &str = "A2A_TASK_NOT_FOUND";
    pub const A2A_TASK_BOUND: &str = "A2A_TASK_BOUND";
    pub const A2A_TOKEN_MISSING: &str = "A2A_TOKEN_MISSING";
    pub const A2A_INBOUND_DENIED: &str = "A2A_INBOUND_DENIED";
}

/// Project/daemon-local A2A policy. Disabled by default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct A2aPolicy {
    #[serde(default)]
    pub enabled: bool,
    /// Reserved for a future capability contract. Start currently fails closed
    /// while this is `true`: no inbound execution bridge is implemented, and the
    /// facade never grants local tool, file, shell, network, or model authority.
    #[serde(default)]
    pub allow_inbound: bool,
    /// Loopback host. Only `127.0.0.1` or `::1` are accepted.
    #[serde(default = "default_bind_host")]
    pub bind_host: String,
    /// 0 means an ephemeral port.
    #[serde(default)]
    pub port: u16,
    #[serde(default = "default_max_tasks")]
    pub max_tasks: u32,
    #[serde(default = "default_task_ttl_secs")]
    pub task_ttl_secs: u64,
    /// The facade always requires its single bearer token. This field exists
    /// for forward-compatible policy decoding and must remain `true`.
    #[serde(default = "default_true")]
    pub token_required: bool,
}

fn default_bind_host() -> String {
    "127.0.0.1".to_string()
}
fn default_max_tasks() -> u32 {
    DEFAULT_A2A_TASKS
}
fn default_task_ttl_secs() -> u64 {
    DEFAULT_A2A_TASK_TTL_SECS
}
fn default_true() -> bool {
    true
}

impl Default for A2aPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_inbound: false,
            bind_host: default_bind_host(),
            port: 0,
            max_tasks: DEFAULT_A2A_TASKS,
            task_ttl_secs: DEFAULT_A2A_TASK_TTL_SECS,
            token_required: true,
        }
    }
}

impl A2aPolicy {
    pub fn normalized(&self) -> Result<Self, String> {
        let mut policy = self.clone();
        policy.bind_host = policy.bind_host.trim().to_string();
        if policy.bind_host != "127.0.0.1" && policy.bind_host != "::1" {
            return Err("A2A bind host must be loopback (127.0.0.1 or ::1)".to_string());
        }
        if policy.max_tasks == 0 || policy.max_tasks > MAX_A2A_TASKS {
            return Err(format!("A2A max_tasks must be 1-{MAX_A2A_TASKS}"));
        }
        if !(MIN_A2A_TASK_TTL_SECS..=MAX_A2A_TASK_TTL_SECS).contains(&policy.task_ttl_secs) {
            return Err(format!(
                "A2A task_ttl_secs must be {MIN_A2A_TASK_TTL_SECS}-{MAX_A2A_TASK_TTL_SECS}"
            ));
        }
        if policy.allow_inbound && !policy.enabled {
            return Err("A2A allow_inbound requires enabled=true".to_string());
        }
        if !policy.token_required {
            return Err(
                "A2A always requires its single bearer token; token_required must be true"
                    .to_string(),
            );
        }
        Ok(policy)
    }
}

/// Live facade status. Never contains the bearer token.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct A2aStatus {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub running: bool,
    #[serde(default)]
    pub inbound_enabled: bool,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub card_url: Option<String>,
    #[serde(default)]
    pub task_count: u32,
    #[serde(default)]
    pub max_tasks: u32,
    #[serde(default)]
    pub token_present: bool,
    /// Token was returned exactly once by the start call; status never repeats
    /// it. Clients must store it in their own secret store.
    #[serde(default)]
    pub token_issued: bool,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub policy: A2aPolicy,
}

/// One A2A task summary for the operator surface. No message bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct A2aTaskSummary {
    pub id: String,
    #[serde(default)]
    pub context_id: String,
    pub state: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub message_count: u32,
    #[serde(default)]
    pub artifact_count: u32,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct A2aStatusParams {}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct A2aStartParams {
    /// Explicit host-state approval. Start fails closed without it.
    #[serde(default)]
    pub approved: bool,
    #[serde(default)]
    pub policy: Option<A2aPolicy>,
}

#[derive(Debug, Clone, Serialize)]
pub struct A2aStartResult {
    pub status: A2aStatus,
    /// Returned exactly once by the start call. Never serialized in status.
    #[serde(default)]
    pub token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct A2aPolicySetParams {
    pub policy: A2aPolicy,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct A2aCardParams {}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct A2aStopParams {
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct A2aTaskListParams {
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub state: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct A2aTaskListResult {
    pub tasks: Vec<A2aTaskSummary>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct A2aTaskParams {
    pub task_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct A2aCancelTaskParams {
    pub task_id: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_defaults_disabled_and_loopback_only() {
        let policy = A2aPolicy::default();
        assert!(!policy.enabled);
        assert!(!policy.allow_inbound);
        assert_eq!(policy.bind_host, "127.0.0.1");
        assert!(policy.token_required);
        assert!(policy.normalized().is_ok());
        let mut public = policy.clone();
        public.bind_host = "0.0.0.0".into();
        assert!(public.normalized().is_err());
        let mut inbound = policy;
        inbound.allow_inbound = true;
        assert!(inbound.normalized().is_err());
    }

    #[test]
    fn status_never_has_a_token_field() {
        let status = A2aStatus::default();
        let json = serde_json::to_value(&status).unwrap();
        assert!(json.get("token").is_none());
        assert!(json.get("token_present").is_some());
    }

    #[test]
    fn task_summary_state_is_display_only() {
        let task = A2aTaskSummary {
            id: "t1".into(),
            context_id: "c1".into(),
            state: "input_required".into(),
            updated_at: "2026-11-16T00:00:00Z".into(),
            message_count: 2,
            artifact_count: 0,
        };
        let json = serde_json::to_string(&task).unwrap();
        assert!(json.contains("input_required"));
    }
}

#[cfg(test)]
mod wire_tests {
    use super::*;

    #[test]
    fn policy_normalization_rejects_inbound_without_enable() {
        let mut policy = A2aPolicy {
            allow_inbound: true,
            ..A2aPolicy::default()
        };
        assert!(policy.normalized().is_err());
        policy.enabled = true;
        assert!(policy.normalized().is_ok());
        policy.token_required = false;
        assert!(policy.normalized().is_err());
        policy.token_required = true;
        policy.max_tasks = 0;
        assert!(policy.normalized().is_err());
        policy.max_tasks = MAX_A2A_TASKS + 1;
        assert!(policy.normalized().is_err());
        policy.max_tasks = 1;
        policy.task_ttl_secs = 1;
        assert!(policy.normalized().is_err());
        policy.task_ttl_secs = MAX_A2A_TASK_TTL_SECS + 1;
        assert!(policy.normalized().is_err());
    }

    #[test]
    fn task_summary_round_trips_and_never_carries_a_token() {
        let task = A2aTaskSummary {
            id: "task".into(),
            context_id: "ctx".into(),
            state: "working".into(),
            updated_at: "2026-11-16T00:00:00Z".into(),
            message_count: 1,
            artifact_count: 2,
        };
        let json = serde_json::to_value(&task).unwrap();
        assert!(json.get("token").is_none());
        let decoded: A2aTaskSummary = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, task);
    }

    #[test]
    fn start_params_default_to_unapproved() {
        let params: A2aStartParams = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(!params.approved);
        assert!(params.policy.is_none());
        assert!(
            serde_json::to_value(A2aStatus::default())
                .unwrap()
                .get("token")
                .is_none()
        );
    }

    #[test]
    fn task_list_params_are_bounded_by_the_consumer() {
        let params: A2aTaskListParams = serde_json::from_value(serde_json::json!({
            "limit": 10, "state": "rejected"
        }))
        .unwrap();
        assert_eq!(params.limit, Some(10));
        assert_eq!(params.state.as_deref(), Some("rejected"));
        assert_eq!(MAX_A2A_TASK_PAGE, 200);
    }
}
