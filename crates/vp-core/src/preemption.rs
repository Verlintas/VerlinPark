//! Precise preemption and deadline policy for long-running sessions.
//!
//! Cooperative pause/cancel remains the default operator control. This module
//! adds the narrow, typed hard-stop path described by ADR-0179:
//!
//! - an exhausted session wall-clock budget;
//! - an explicit `session.preempt`;
//! - an optional project per-tool deadline.
//!
//! The token and provider error primitives live in `vp-providers` because the
//! provider trait accepts them directly. This module owns the runtime policy,
//! tool-result mapping, attribution, and event-shaped records.
//!
//! Invariants:
//!
//! - a preempted provider stream is persisted as partial output with honest
//!   provider or estimated usage and is never resumed mid-stream;
//! - a preempted tool reports whether side effects are unknown, and an unknown
//!   shell/browser/computer action is never retried automatically;
//! - shell/process cancellation terminates the process group with a bounded
//!   TERM → KILL escalation;
//! - a tool deadline aborts only that tool, not the run;
//! - nothing in this module merges work or bypasses approval, Seatbelt, path,
//!   Verifier, capacity, or budget gates.

use std::time::{Duration, Instant};

use vp_protocol::types::{MetricCount, PreemptionRecord};
use vp_providers::preempt::{
    PreemptPhase, PreemptReason, PreemptSnapshot, PreemptToken, ToolGrace,
};

pub use vp_providers::preempt::{
    PreemptPhase as Phase, PreemptReason as Reason, PreemptSnapshot as Snapshot,
    PreemptToken as Token, ToolGrace as Grace, current as current_token, scoped as scope_token,
};

/// A bounded wall-clock deadline derived from a session time budget.
///
/// The deadline is advisory data: the [`PreemptToken`] enforces it. Keeping the
/// arithmetic in one place makes the budget, run journal, and preemption event
/// agree on when the run ran out of wall-clock time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunDeadline {
    pub started: Instant,
    pub wall_clock: Option<Instant>,
}

impl RunDeadline {
    pub fn new(started: Instant, time_budget_secs: Option<u64>) -> Self {
        Self {
            started,
            wall_clock: time_budget_secs.map(|secs| started + Duration::from_secs(secs.max(1))),
        }
    }

    pub fn remaining(&self) -> Option<Duration> {
        self.wall_clock
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    pub fn expired(&self) -> bool {
        self.wall_clock
            .is_some_and(|deadline| Instant::now() >= deadline)
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
    }

    /// Build the run token for one session. A disabled policy still gets a
    /// token because an operator can always call `session.preempt`; it just has
    /// no automatic wall-clock deadline.
    pub fn token(&self, enabled: bool) -> PreemptToken {
        PreemptToken::new(self.wall_clock.filter(|_| enabled))
    }
}

/// Build a run token from a session budget and project policy.
pub fn run_token(
    time_budget_secs: Option<u64>,
    policy: &vp_protocol::types::PreemptionPolicy,
) -> PreemptToken {
    let deadline = RunDeadline::new(Instant::now(), time_budget_secs);
    let token = deadline.token(policy.enabled);
    apply_policy(&token, policy);
    token
}

/// Copy bounded grace durations from a project policy onto a token. Child
/// tokens created later inherit these values.
pub fn apply_policy(token: &PreemptToken, policy: &vp_protocol::types::PreemptionPolicy) {
    token.set_grace(ToolGrace {
        grace: policy.tool_grace(),
        kill_escalation: policy.kill_escalation(),
        provider_grace: policy.provider_grace(),
    });
}

/// Effective per-tool deadline. `None` leaves the run token in charge.
pub fn tool_deadline(policy: &vp_protocol::types::PreemptionPolicy) -> Option<Duration> {
    if policy.enabled {
        policy.tool_deadline()
    } else {
        None
    }
}

/// A bounded attribution shared by session, provider, and tool events.
#[derive(Debug, Clone)]
pub struct PreemptAttribution<'a> {
    pub session_id: &'a str,
    pub project_id: &'a str,
    pub agent_id: &'a str,
    pub agent_name: &'a str,
    pub agent_role: &'a str,
    pub task_id: Option<&'a str>,
    pub department: Option<&'a str>,
}

/// A conservative snapshot for the rare case where a select branch observes a
/// trigger but the token snapshot is unavailable (for example a poisoned lock).
/// It never claims success or exactly-once side effects.
pub fn fallback_snapshot(phase: PreemptPhase, tool: Option<&str>) -> PreemptSnapshot {
    PreemptSnapshot {
        reason: PreemptReason::Explicit,
        phase,
        tool: tool.map(str::to_string),
        elapsed_ms: 0,
        side_effects_unknown: phase == PreemptPhase::Tool,
        partial_input_tokens: 0,
        partial_output_tokens: 0,
        partial_output_bytes: 0,
        usage_estimated: false,
        detail: Some("preemption state was unavailable".to_string()),
    }
}

/// Convert a token snapshot into the stable event/CLI/UI record.
pub fn record(
    attribution: &PreemptAttribution<'_>,
    snapshot: &PreemptSnapshot,
    terminal: bool,
    checkpoint_id: Option<String>,
    task_checkpoint_commit: Option<String>,
    detail: Option<String>,
) -> PreemptionRecord {
    PreemptionRecord {
        session_id: attribution.session_id.to_string(),
        project_id: attribution.project_id.to_string(),
        agent_id: attribution.agent_id.to_string(),
        agent_name: attribution.agent_name.to_string(),
        agent_role: attribution.agent_role.to_string(),
        task_id: attribution.task_id.map(str::to_string),
        department: attribution.department.map(str::to_string),
        reason: snapshot.reason.as_str().to_string(),
        phase: snapshot.phase.as_str().to_string(),
        tool: snapshot.tool.clone(),
        elapsed_ms: snapshot.elapsed_ms,
        side_effects_unknown: snapshot.side_effects_unknown,
        partial_input_tokens: snapshot.partial_input_tokens,
        partial_output_tokens: snapshot.partial_output_tokens,
        partial_output_bytes: snapshot.partial_output_bytes,
        usage_estimated: snapshot.usage_estimated,
        checkpoint_id,
        task_checkpoint_commit,
        detail: snapshot
            .detail
            .clone()
            .or(detail)
            .map(|text| text.chars().take(300).collect()),
        terminal,
        created_at: vp_protocol::now_rfc3339(),
    }
}

/// Tools whose preemption cannot have changed workspace or remote state. The
/// list is intentionally conservative: an unknown tool defaults to `true`
/// (side effects unknown), and the runtime fails closed rather than assuming a
/// write did not land.
pub const READ_ONLY_TOOLS: &[&str] = &[
    "read_file",
    "read_files",
    "list_dir",
    "find_files",
    "file_info",
    "grep",
    "read_tool_output",
    "git",
    "repo_map",
    "find_symbol",
    "find_references",
    "dependency_graph",
    "affected_tests",
    "code_diagnostics",
    "build_context",
    "project_profile",
    "tool_help",
    "tool_search",
    "budget_status",
    "task_detail",
    "task_control",
    "team_status",
    "list_artifacts",
    "read_artifact",
    "read_mailbox",
    "recall",
    "list_sources",
    "read_source",
    "search_sources",
    "fetch_url",
    "read_pdf",
    "read_image",
    "data_query",
    "read_document",
];

/// Whether a preempted tool may have produced an external or workspace side
/// effect. `local_http` is not read-only because it can issue arbitrary
/// methods, while `fetch_url` only performs a GET.
pub fn tool_side_effects_unknown(tool: &str) -> bool {
    !READ_ONLY_TOOLS.contains(&tool)
}

fn error_code(snapshot: &PreemptSnapshot) -> crate::tooling::errors::ToolErrorCode {
    match snapshot.reason {
        PreemptReason::ToolDeadline => crate::tooling::errors::ToolErrorCode::DEADLINE_EXCEEDED,
        PreemptReason::WallClock | PreemptReason::Explicit => {
            crate::tooling::errors::ToolErrorCode::PREEMPTED
        }
    }
}

/// Build the typed tool result for a preemption. The provider-facing text is
/// concise; the structured object carries reason, phase, elapsed, partial
/// output metadata, and the unknown-side-effects flag.
pub fn tool_output(tool: &str, snapshot: &PreemptSnapshot) -> crate::tools::ToolOutput {
    let code = error_code(snapshot);
    let message = match snapshot.reason {
        PreemptReason::ToolDeadline => format!(
            "tool '{tool}' exceeded its {}ms deadline and was stopped",
            snapshot.elapsed_ms
        ),
        PreemptReason::WallClock => {
            "the session wall-clock budget was exhausted while this tool was running".to_string()
        }
        PreemptReason::Explicit => {
            "an operator preempted the session while this tool was running".to_string()
        }
    };
    let failure = crate::tooling::errors::ToolFailure::new(code, message)
        .with_tool(tool)
        .with_evidence(format!(
            "phase={} elapsed_ms={} side_effects_unknown={}",
            snapshot.phase.as_str(),
            snapshot.elapsed_ms,
            snapshot.side_effects_unknown
        ));
    let output = crate::tools::ToolOutput::error_failure(failure);
    output.with_structured(serde_json::json!({
        "preempted": true,
        "code": code.as_str(),
        "reason": snapshot.reason.as_str(),
        "phase": snapshot.phase.as_str(),
        "tool": tool,
        "elapsed_ms": snapshot.elapsed_ms,
        "side_effects_unknown": snapshot.side_effects_unknown,
        "partial_input_tokens": snapshot.partial_input_tokens,
        "partial_output_tokens": snapshot.partial_output_tokens,
        "partial_output_bytes": snapshot.partial_output_bytes,
        "usage_estimated": snapshot.usage_estimated,
        "detail": snapshot.detail,
        "auto_retry": false,
    }))
}

/// Read back the typed preemption marker from a tool result. Tools that handle
/// cancellation internally (shell, browser, computer, MCP, LSP) return this
/// shape instead of relying on the dispatcher to drop their future.
pub fn output_snapshot(output: &crate::tools::ToolOutput) -> Option<PreemptSnapshot> {
    let structured = output.structured.as_ref()?;
    if structured
        .get("preempted")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return None;
    }
    Some(PreemptSnapshot {
        reason: structured
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .and_then(PreemptReason::parse)
            .unwrap_or(PreemptReason::Explicit),
        phase: structured
            .get("phase")
            .and_then(serde_json::Value::as_str)
            .and_then(PreemptPhase::parse)
            .unwrap_or(PreemptPhase::Tool),
        tool: structured
            .get("tool")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        elapsed_ms: structured
            .get("elapsed_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        side_effects_unknown: structured
            .get("side_effects_unknown")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        partial_input_tokens: structured
            .get("partial_input_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        partial_output_tokens: structured
            .get("partial_output_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        partial_output_bytes: structured
            .get("partial_output_bytes")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        usage_estimated: structured
            .get("usage_estimated")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        detail: structured
            .get("detail")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
    })
}

/// Convert bounded metric maps into the protocol row shape.
pub fn metric_rows(counts: &std::collections::BTreeMap<String, u64>) -> Vec<MetricCount> {
    counts
        .iter()
        .map(|(key, count)| MetricCount {
            key: key.clone(),
            count: *count,
        })
        .collect()
}

/// True when a tool is in the protocol-aware list whose implementation observes
/// the task-local token. Other tools are dispatched under a bounded outer
/// `select!`, which drops the future and aborts the in-flight request.
pub fn tool_observes_token(tool: &str) -> bool {
    matches!(
        tool,
        "shell"
            | "process"
            | "browser"
            | "computer"
            | "mcp"
            | "fetch_url"
            | "local_http"
            | "find_symbol"
            | "find_references"
            | "code_diagnostics"
            | "build_context"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_deadline_is_bounded_and_reports_expiry() {
        let deadline = RunDeadline::new(Instant::now(), Some(1));
        assert!(deadline.wall_clock.is_some());
        assert!(!deadline.expired());
        assert!(deadline.remaining().is_some());
        let past = RunDeadline::new(Instant::now() - Duration::from_secs(5), Some(1));
        assert!(past.expired());
        assert_eq!(past.remaining(), Some(Duration::ZERO));
    }

    #[test]
    fn disabled_policy_still_accepts_explicit_preemption() {
        let policy = vp_protocol::types::PreemptionPolicy {
            enabled: false,
            ..Default::default()
        };
        let token = run_token(Some(1), &policy);
        assert!(token.deadline().is_none());
        token.trigger(Reason::Explicit, Phase::Queue, false);
        assert_eq!(token.reason(), Some(Reason::Explicit));
    }

    #[test]
    fn policy_clamps_graces_to_finite_bounds() {
        let policy = vp_protocol::types::PreemptionPolicy {
            provider_grace_ms: u64::MAX,
            tool_grace_ms: u64::MAX,
            kill_escalation_ms: u64::MAX,
            tool_deadline_secs: Some(99_999),
            ..Default::default()
        }
        .normalized()
        .unwrap();
        assert_eq!(policy.provider_grace_ms, 30_000);
        assert_eq!(policy.tool_grace_ms, 60_000);
        assert_eq!(policy.kill_escalation_ms, 30_000);
        assert_eq!(policy.tool_deadline_secs, Some(3_600));
    }

    #[test]
    fn read_only_and_unknown_tools_fail_closed_differently() {
        assert!(!tool_side_effects_unknown("read_file"));
        assert!(!tool_side_effects_unknown("fetch_url"));
        assert!(tool_side_effects_unknown("shell"));
        assert!(tool_side_effects_unknown("local_http"));
        assert!(tool_side_effects_unknown("an-unlisted-tool"));
    }

    #[test]
    fn tool_output_round_trips_the_typed_marker() {
        let mut snapshot = PreemptSnapshot {
            reason: Reason::ToolDeadline,
            phase: Phase::Tool,
            tool: Some("shell".to_string()),
            elapsed_ms: 42,
            side_effects_unknown: true,
            partial_input_tokens: 0,
            partial_output_tokens: 0,
            partial_output_bytes: 0,
            usage_estimated: false,
            detail: Some("process_group_terminated".to_string()),
        };
        let output = tool_output("shell", &snapshot);
        assert!(output.is_error);
        assert!(output.content.contains("DEADLINE_EXCEEDED"));
        let parsed = output_snapshot(&output).expect("typed marker");
        assert_eq!(parsed.reason, Reason::ToolDeadline);
        assert_eq!(parsed.phase, Phase::Tool);
        assert_eq!(parsed.tool.as_deref(), Some("shell"));
        assert_eq!(parsed.detail.as_deref(), Some("process_group_terminated"));
        snapshot.elapsed_ms = 99;
        assert_eq!(output_snapshot(&output).unwrap().elapsed_ms, 42);
    }

    #[test]
    fn explicit_preemption_uses_the_preempted_code() {
        let snapshot = PreemptSnapshot {
            reason: Reason::Explicit,
            phase: Phase::Provider,
            tool: None,
            elapsed_ms: 7,
            side_effects_unknown: false,
            partial_input_tokens: 11,
            partial_output_tokens: 3,
            partial_output_bytes: 24,
            usage_estimated: true,
            detail: None,
        };
        let output = tool_output("fetch_url", &snapshot);
        assert!(output.content.contains("PREEMPTED"));
        assert_eq!(
            output.structured.as_ref().unwrap()["partial_input_tokens"],
            serde_json::json!(11)
        );
    }

    #[test]
    fn tool_catalog_lists_protocol_aware_tools() {
        assert!(tool_observes_token("shell"));
        assert!(tool_observes_token("mcp"));
        assert!(!tool_observes_token("read_file"));
    }

    #[test]
    fn record_maps_attribution_and_bounds_detail() {
        let attribution = PreemptAttribution {
            session_id: "s1",
            project_id: "p1",
            agent_id: "a1",
            agent_name: "Worker",
            agent_role: "worker",
            task_id: Some("T1"),
            department: Some("impl"),
        };
        let snapshot = PreemptSnapshot {
            reason: Reason::Explicit,
            phase: Phase::Tool,
            tool: Some("shell".to_string()),
            elapsed_ms: 99,
            side_effects_unknown: true,
            partial_input_tokens: 1,
            partial_output_tokens: 2,
            partial_output_bytes: 3,
            usage_estimated: true,
            detail: Some("d".repeat(1_000)),
        };
        let record = record(
            &attribution,
            &snapshot,
            true,
            Some("cp1".to_string()),
            Some("abc123".to_string()),
            None,
        );
        assert_eq!(record.session_id, "s1");
        assert_eq!(record.project_id, "p1");
        assert_eq!(record.agent_role, "worker");
        assert_eq!(record.task_id.as_deref(), Some("T1"));
        assert_eq!(record.reason, "explicit");
        assert_eq!(record.phase, "tool");
        assert!(record.side_effects_unknown);
        assert!(record.usage_estimated);
        assert_eq!(record.checkpoint_id.as_deref(), Some("cp1"));
        assert_eq!(record.task_checkpoint_commit.as_deref(), Some("abc123"));
        assert!(record.detail.as_deref().unwrap().len() <= 300);
        assert_eq!(
            record.summary(),
            "explicit preempted during tool after 99ms (shell); side effects unknown"
        );
    }

    #[test]
    fn record_without_snapshot_detail_uses_the_fallback_detail() {
        let attribution = PreemptAttribution {
            session_id: "s1",
            project_id: "p1",
            agent_id: "a1",
            agent_name: "Leader",
            agent_role: "leader",
            task_id: None,
            department: None,
        };
        let snapshot = PreemptSnapshot {
            reason: Reason::WallClock,
            phase: Phase::Provider,
            tool: None,
            elapsed_ms: 5,
            side_effects_unknown: false,
            partial_input_tokens: 0,
            partial_output_tokens: 0,
            partial_output_bytes: 0,
            usage_estimated: false,
            detail: None,
        };
        let record = record(
            &attribution,
            &snapshot,
            false,
            None,
            None,
            Some("queue wait".into()),
        );
        assert_eq!(record.detail.as_deref(), Some("queue wait"));
        assert!(!record.terminal);
    }

    #[test]
    fn fallback_snapshot_fails_closed_for_tools() {
        let tool = fallback_snapshot(Phase::Tool, Some("shell"));
        assert!(tool.side_effects_unknown);
        assert_eq!(tool.reason, Reason::Explicit);
        let queue = fallback_snapshot(Phase::Queue, None);
        assert!(!queue.side_effects_unknown);
        assert_eq!(queue.phase, Phase::Queue);
    }

    #[test]
    fn metric_rows_preserve_empty_and_non_empty_maps() {
        let mut counts = std::collections::BTreeMap::new();
        assert!(metric_rows(&counts).is_empty());
        counts.insert("provider".to_string(), 2);
        counts.insert("tool".to_string(), 1);
        let rows = metric_rows(&counts);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].key, "provider");
        assert_eq!(rows[0].count, 2);
    }

    #[test]
    fn tool_output_marks_wall_clock_and_explicit_as_preempted() {
        for reason in [Reason::WallClock, Reason::Explicit] {
            let snapshot = PreemptSnapshot {
                reason,
                phase: Phase::Provider,
                tool: None,
                elapsed_ms: 1,
                side_effects_unknown: false,
                partial_input_tokens: 0,
                partial_output_tokens: 0,
                partial_output_bytes: 0,
                usage_estimated: false,
                detail: None,
            };
            let output = tool_output("read_file", &snapshot);
            assert!(output.content.contains("PREEMPTED"));
            let parsed = output_snapshot(&output).unwrap();
            assert_eq!(parsed.reason, reason);
            assert!(!parsed.side_effects_unknown);
        }
    }

    #[test]
    fn output_snapshot_ignores_non_preemption_errors() {
        let output = crate::tools::ToolOutput::error_coded(
            "read_file",
            crate::tooling::errors::ToolErrorCode::FILE_NOT_FOUND,
            "missing",
        );
        assert!(output_snapshot(&output).is_none());
    }
}
