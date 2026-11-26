//! Read-only Custos wire and view types.
//!
//! The protocol is deliberately small: status, health, charter, and ping.
//! There is no `act`, `policy.set`, `pause`, or `resume` method.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CUSTOS_PROTOCOL_VERSION: u32 = 1;
pub const CUSTOS_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const STATUS_SCHEMA: &str = "verlinpark.custos_status/v1";
pub const HEALTH_SCHEMA: &str = "verlinpark.custos_health/v1";
pub const PING_SCHEMA: &str = "verlinpark.custos_ping/v1";
pub const RPC_SCHEMA: &str = "verlinpark.custos_rpc/v1";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CustosRole {
    Core,
    Warden,
    Anchor,
    Sentinel,
}

impl CustosRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Warden => "warden",
            Self::Anchor => "anchor",
            Self::Sentinel => "sentinel",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "core" => Some(Self::Core),
            "warden" => Some(Self::Warden),
            "anchor" => Some(Self::Anchor),
            "sentinel" => Some(Self::Sentinel),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CustosState {
    Healthy,
    Watch,
    Degraded,
    Critical,
    Quarantined,
    SignalLost,
}

impl CustosState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Watch => "watch",
            Self::Degraded => "degraded",
            Self::Critical => "critical",
            Self::Quarantined => "quarantined",
            Self::SignalLost => "signal_lost",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DimensionState {
    Ok,
    Watch,
    Degraded,
    Critical,
    Unknown,
}

impl DimensionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Watch => "watch",
            Self::Degraded => "degraded",
            Self::Critical => "critical",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Counts {
    pub sessions_total: u64,
    pub sessions_running: u64,
    pub sessions_paused: u64,
    pub sessions_failed: u64,
    pub events_total: u64,
    pub event_age_secs: Option<u64>,
    pub leases_live: u64,
    pub leases_expired: u64,
    pub runs_running: u64,
    pub runs_interrupted: u64,
    pub tasks_pending: u64,
    pub tasks_running: u64,
    pub tasks_blocked: u64,
    pub merges_queued: u64,
    pub merges_conflict: u64,
    pub effects_unknown: u64,
    pub approvals_pending: u64,
    pub processes_live: u64,
    pub db_bytes: u64,
    pub wal_bytes: u64,
    pub disk_free_bytes: u64,
    pub disk_total_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Dimension {
    pub key: String,
    pub state: DimensionState,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RingRoleView {
    pub role: String,
    pub state: String,
    pub age_secs: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RingView {
    pub mode: String,
    pub fresh: u32,
    pub stale: u32,
    pub missing: u32,
    pub quorum: bool,
    pub roles: Vec<RingRoleView>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionSummary {
    pub id: String,
    pub class: String,
    pub at: String,
    pub result: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    pub schema: String,
    pub generated_at: String,
    pub state: CustosState,
    pub observe_only: bool,
    pub role: String,
    pub protocol: u32,
    pub version: String,
    pub charter_hash: String,
    pub charter_status: String,
    pub dimensions: Vec<Dimension>,
    pub counts: Counts,
    pub ring: RingView,
    pub last_action: Option<ActionSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProbeInfo {
    pub db_present: bool,
    pub db_open: bool,
    pub user_version: Option<i64>,
    pub journal_mode: Option<String>,
    pub events_total: u64,
    pub event_age_secs: Option<u64>,
    pub table_counts: BTreeMap<String, BTreeMap<String, u64>>,
    pub missing_tables: Vec<String>,
    pub errors: Vec<String>,
    pub probe_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Health {
    pub schema: String,
    pub status: Status,
    pub probe: ProbeInfo,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PingResult {
    pub schema: String,
    pub version: String,
    pub protocol: u32,
    pub pid: u32,
    pub role: String,
    pub observe_only: bool,
    pub started_at: String,
    pub uptime_secs: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RpcRequest {
    #[serde(default)]
    pub id: Option<u64>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RpcError {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RpcResponse {
    pub schema: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl RpcResponse {
    pub fn ok(id: Option<u64>, result: Value) -> Self {
        Self {
            schema: RPC_SCHEMA.to_string(),
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<u64>, code: &str, message: impl Into<String>) -> Self {
        Self {
            schema: RPC_SCHEMA.to_string(),
            id,
            ok: false,
            result: None,
            error: Some(RpcError {
                code: code.to_string(),
                message: message.into(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_round_trips() {
        for role in [
            CustosRole::Core,
            CustosRole::Warden,
            CustosRole::Anchor,
            CustosRole::Sentinel,
        ] {
            assert_eq!(CustosRole::parse(role.as_str()), Some(role));
        }
        assert_eq!(CustosRole::parse("nope"), None);
    }

    #[test]
    fn error_response_has_no_result() {
        let response = RpcResponse::error(Some(7), "METHOD_NOT_FOUND", "no such method");
        assert!(!response.ok);
        assert!(response.result.is_none());
        assert_eq!(response.id, Some(7));
        assert_eq!(response.error.unwrap().code, "METHOD_NOT_FOUND");
    }
}
