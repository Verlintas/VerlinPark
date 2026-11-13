//! OpenTelemetry GenAI projection wire types (ADR-0197).
//!
//! OTel is an **export projection** of the existing event log, not a new source
//! of truth. Nothing in this module mutates events, usage, budgets, approvals,
//! plans, or merge state. Every field is `serde(default)` so an older client or
//! bundle decodes without changing behavior.
//!
//! The projection deliberately carries identifiers, roles, provider/model
//! names, tool names, counts, durations, and token usage only. It never carries
//! prompts, message text, tool arguments, tool results, file content, raw
//! errors, or secrets. See `vp_core::otel` for the mapping and the loopback-only
//! endpoint policy.

use serde::{Deserialize, Serialize};

/// Default local OTLP/HTTP collector endpoint. Export is disabled by default,
/// so this value is metadata until a project explicitly enables it.
pub const DEFAULT_OTEL_ENDPOINT: &str = "http://127.0.0.1:4318";
/// Default service name for the exported resource.
pub const DEFAULT_OTEL_SERVICE_NAME: &str = "verlinpark";
/// Default maximum spans in one explicit export request.
pub const DEFAULT_OTEL_BATCH_LIMIT: u32 = 200;
/// Default HTTP timeout for one explicit export request.
pub const DEFAULT_OTEL_TIMEOUT_SECS: u64 = 5;
/// Hard maximum spans accepted by one export request.
pub const MAX_OTEL_BATCH_LIMIT: u32 = 1_000;
/// Hard maximum HTTP timeout accepted by the policy.
pub const MAX_OTEL_TIMEOUT_SECS: u64 = 60;
/// Maximum service-name characters accepted by the policy.
pub const MAX_OTEL_SERVICE_NAME_CHARS: usize = 64;
/// Maximum endpoint characters accepted by the policy.
pub const MAX_OTEL_ENDPOINT_CHARS: usize = 512;

/// Stable error codes surfaced through RPC error data, CLI diagnostics, and
/// audit-free status text. They are intentionally coarse and never contain
/// endpoint credentials.
pub mod otel_codes {
    pub const OTEL_DISABLED: &str = "OTEL_DISABLED";
    pub const OTEL_ENDPOINT_INVALID: &str = "OTEL_ENDPOINT_INVALID";
    pub const OTEL_ENDPOINT_NOT_LOOPBACK: &str = "OTEL_ENDPOINT_NOT_LOOPBACK";
    pub const OTEL_POLICY_INVALID: &str = "OTEL_POLICY_INVALID";
    pub const OTEL_EXPORT_FAILED: &str = "OTEL_EXPORT_FAILED";
    pub const OTEL_EMPTY: &str = "OTEL_EMPTY";
    pub const OTEL_SESSION_NOT_FOUND: &str = "OTEL_SESSION_NOT_FOUND";
    pub const OTEL_PROJECT_NOT_FOUND: &str = "OTEL_PROJECT_NOT_FOUND";
}

/// Project-scoped OTel export policy. Export is never background and never
/// automatic: `telemetry.otel.export` / `vp otel-export` is the only producer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OtelPolicy {
    /// Master switch. Default false; a disabled policy refuses export.
    #[serde(default)]
    pub enabled: bool,
    /// Base OTLP/HTTP JSON endpoint. Loopback only. Export appends
    /// `/v1/traces`.
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
    /// `service.name` resource attribute.
    #[serde(default = "default_service_name")]
    pub service_name: String,
    /// Maximum spans in one export request.
    #[serde(default = "default_batch_limit")]
    pub batch_limit: u32,
    /// HTTP connect/response timeout for one explicit export.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_endpoint() -> String {
    DEFAULT_OTEL_ENDPOINT.to_string()
}

fn default_service_name() -> String {
    DEFAULT_OTEL_SERVICE_NAME.to_string()
}

fn default_batch_limit() -> u32 {
    DEFAULT_OTEL_BATCH_LIMIT
}

fn default_timeout_secs() -> u64 {
    DEFAULT_OTEL_TIMEOUT_SECS
}

impl Default for OtelPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: default_endpoint(),
            service_name: default_service_name(),
            batch_limit: default_batch_limit(),
            timeout_secs: default_timeout_secs(),
        }
    }
}

impl OtelPolicy {
    /// Validate and normalize the persisted policy. Validation is strict: an
    /// invalid policy is never silently clamped because that would hide a
    /// typo in a collector endpoint.
    pub fn normalized(&self) -> Result<Self, String> {
        let endpoint = self.endpoint.trim();
        if endpoint.is_empty() || endpoint.chars().count() > MAX_OTEL_ENDPOINT_CHARS {
            return Err(format!(
                "endpoint must be 1-{MAX_OTEL_ENDPOINT_CHARS} non-empty characters"
            ));
        }
        let service_name = self.service_name.trim();
        if service_name.is_empty()
            || service_name.chars().count() > MAX_OTEL_SERVICE_NAME_CHARS
            || service_name
                .chars()
                .any(|value| value.is_control() || value == '\n' || value == '\r')
        {
            return Err(format!(
                "service_name must be 1-{MAX_OTEL_SERVICE_NAME_CHARS} non-control characters"
            ));
        }
        if self.batch_limit == 0 || self.batch_limit > MAX_OTEL_BATCH_LIMIT {
            return Err(format!("batch_limit must be 1-{MAX_OTEL_BATCH_LIMIT}"));
        }
        if self.timeout_secs == 0 || self.timeout_secs > MAX_OTEL_TIMEOUT_SECS {
            return Err(format!("timeout_secs must be 1-{MAX_OTEL_TIMEOUT_SECS}"));
        }
        Ok(Self {
            enabled: self.enabled,
            endpoint: endpoint.to_string(),
            service_name: service_name.to_string(),
            batch_limit: self.batch_limit,
            timeout_secs: self.timeout_secs,
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct OtelPolicyParams {
    #[serde(default)]
    pub project_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetOtelPolicyParams {
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub policy: OtelPolicy,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OtelPolicyResult {
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub policy: OtelPolicy,
    #[serde(default)]
    pub endpoint_valid: bool,
    #[serde(default)]
    pub endpoint_loopback: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Bounded event-to-span projection counts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OtelProjectionCounts {
    #[serde(default)]
    pub session_spans: u64,
    #[serde(default)]
    pub agent_spans: u64,
    #[serde(default)]
    pub tool_spans: u64,
    #[serde(default)]
    pub usage_spans: u64,
    /// Spans that could be built. This may exceed `exported_spans`.
    #[serde(default)]
    pub total_spans: u64,
    #[serde(default)]
    pub events_examined: u64,
    #[serde(default)]
    pub events_mapped: u64,
    #[serde(default)]
    pub events_skipped: u64,
    /// True when the bounded event window ended before the session log did.
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct OtelStatusParams {
    #[serde(default)]
    pub project_id: String,
    /// Optional session whose projection counts are reported.
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OtelStatusResult {
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub policy: OtelPolicy,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub endpoint_valid: bool,
    #[serde(default)]
    pub endpoint_loopback: bool,
    #[serde(default)]
    pub traces_path: String,
    #[serde(default)]
    pub counts: OtelProjectionCounts,
    #[serde(default)]
    pub last_event_seq: u64,
    #[serde(default)]
    pub generated_at: String,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct OtelExportParams {
    #[serde(default)]
    pub session_id: String,
    /// Span cap for this request. Defaults to the policy batch limit and is
    /// always clamped to 1..=1000.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Zero-based span offset for paging. The result returns `next_offset`
    /// when more spans remain.
    #[serde(default)]
    pub offset: Option<u32>,
}

/// Status of one explicit export attempt. `exported` is true only for a 2xx
/// response from the configured local collector.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OtelExportResult {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub exported: bool,
    /// `exported`, `failed`, or `empty`.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub span_count: u64,
    #[serde(default)]
    pub available_spans: u64,
    #[serde(default)]
    pub truncated: bool,
    /// First span index selected for this page.
    #[serde(default)]
    pub offset: u64,
    /// Offset for the next page, or `None` when this page reached the end.
    #[serde(default)]
    pub next_offset: Option<u64>,
    /// True when retrying the same request may succeed (transport/5xx/429).
    #[serde(default)]
    pub retryable: bool,
    #[serde(default)]
    pub http_status: Option<u16>,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub request_bytes: u64,
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub generated_at: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct OtelTestParams {
    #[serde(default)]
    pub project_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OtelTestResult {
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub http_status: Option<u16>,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub request_bytes: u64,
    #[serde(default)]
    pub span_count: u64,
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub generated_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn policy_defaults_are_disabled_and_loopback() {
        let policy: OtelPolicy = serde_json::from_value(json!({})).unwrap();
        assert!(!policy.enabled);
        assert_eq!(policy.endpoint, DEFAULT_OTEL_ENDPOINT);
        assert_eq!(policy.service_name, DEFAULT_OTEL_SERVICE_NAME);
        assert_eq!(policy.batch_limit, DEFAULT_OTEL_BATCH_LIMIT);
        assert_eq!(policy.timeout_secs, DEFAULT_OTEL_TIMEOUT_SECS);
        assert_eq!(OtelPolicy::default(), policy);
    }

    #[test]
    fn policy_rejects_empty_hostile_and_out_of_range_values() {
        assert!(OtelPolicy::default().normalized().is_ok());
        let policy = OtelPolicy {
            endpoint: "  ".into(),
            ..Default::default()
        };
        assert!(policy.normalized().is_err());
        let policy = OtelPolicy {
            service_name: "\n".into(),
            ..Default::default()
        };
        assert!(policy.normalized().is_err());
        let policy = OtelPolicy {
            batch_limit: 0,
            ..Default::default()
        };
        assert!(policy.normalized().is_err());
        let policy = OtelPolicy {
            batch_limit: MAX_OTEL_BATCH_LIMIT + 1,
            ..Default::default()
        };
        assert!(policy.normalized().is_err());
        let policy = OtelPolicy {
            timeout_secs: 0,
            ..Default::default()
        };
        assert!(policy.normalized().is_err());
        let policy = OtelPolicy {
            timeout_secs: MAX_OTEL_TIMEOUT_SECS + 1,
            ..Default::default()
        };
        assert!(policy.normalized().is_err());
    }

    #[test]
    fn legacy_policy_wire_shape_decodes_with_defaults() {
        let policy: OtelPolicy =
            serde_json::from_value(json!({"enabled": true, "endpoint": "http://[::1]:4318"}))
                .unwrap();
        assert!(policy.enabled);
        assert_eq!(policy.service_name, DEFAULT_OTEL_SERVICE_NAME);
        assert_eq!(policy.batch_limit, DEFAULT_OTEL_BATCH_LIMIT);
    }

    #[test]
    fn export_result_defaults_are_conservative() {
        let result: OtelExportResult =
            serde_json::from_value(json!({"session_id": "ses-1"})).unwrap();
        assert!(!result.exported);
        assert_eq!(result.status, "");
        assert_eq!(result.span_count, 0);
        assert_eq!(result.http_status, None);
        assert_eq!(result.error_code, None);
    }

    #[test]
    fn status_and_test_results_decode_unknown_fields() {
        let status: OtelStatusResult = serde_json::from_value(json!({
            "project_id": "proj-1",
            "enabled": true,
            "counts": {"total_spans": 4, "agent_spans": 2, "future": 1},
            "warnings": ["bounded"],
        }))
        .unwrap();
        assert!(status.enabled);
        assert_eq!(status.counts.total_spans, 4);
        assert_eq!(status.counts.agent_spans, 2);
        let test: OtelTestResult = serde_json::from_value(json!({
            "project_id": "proj-1",
            "ok": true,
            "http_status": 200
        }))
        .unwrap();
        assert!(test.ok);
        assert_eq!(test.http_status, Some(200));
    }

    #[test]
    fn params_decode_with_optional_fields_absent() {
        let status: OtelStatusParams =
            serde_json::from_value(json!({"project_id": "proj-1"})).unwrap();
        assert!(status.session_id.is_none());
        let export: OtelExportParams =
            serde_json::from_value(json!({"session_id": "ses-1"})).unwrap();
        assert!(export.limit.is_none());
        assert!(export.offset.is_none());
    }

    #[test]
    fn export_result_decodes_paging_and_retryability() {
        let result: OtelExportResult = serde_json::from_value(json!({
            "session_id": "ses-1",
            "status": "failed",
            "offset": 20,
            "next_offset": 40,
            "retryable": true,
            "http_status": 503,
            "truncated": true
        }))
        .unwrap();
        assert_eq!(result.offset, 20);
        assert_eq!(result.next_offset, Some(40));
        assert!(result.retryable);
        assert_eq!(result.http_status, Some(503));
    }
}
