//! Deterministic auto-budget projections and forecast math.
//!
//! This module is deliberately provider-free and run-loop-free. The runtime
//! auto-extension policy consumes these helpers; they only read the session
//! row, the ledger, the event stream, and the persisted policy.

use vp_protocol::now_rfc3339;
use vp_protocol::types::{
    AutoBudgetCaps, AutoBudgetPolicy, BudgetForecast, BudgetForecastSource, BudgetMode,
    BudgetValidationError, Event, Session, SessionAutoBudgetRecord, SessionAutoBudgetState,
    SessionBudgetStatus, UsageEntry, auto_budget_codes,
};

/// Event-derived session budget/policy projection. Used by repair/rebuild so a
/// `session.budget.updated` event restores effective budgets and the auto row.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionBudgetProjection {
    pub mode: BudgetMode,
    pub unlimited: bool,
    pub token_budget: Option<u64>,
    pub cost_budget_usd: Option<f64>,
    pub time_budget_secs: Option<u64>,
    pub policy: Option<AutoBudgetPolicy>,
    pub caps: AutoBudgetCaps,
    pub state: Option<SessionAutoBudgetState>,
    pub auto_created_at: Option<String>,
    pub auto_updated_at: Option<String>,
    /// True when the event stream created an auto row (even if the row has
    /// since been switched to `fixed`).
    pub has_auto_row: bool,
}

impl SessionBudgetProjection {
    pub fn auto_record(&self, session_id: &str) -> Option<SessionAutoBudgetRecord> {
        if !self.has_auto_row {
            return None;
        }
        Some(SessionAutoBudgetRecord {
            session_id: session_id.to_string(),
            mode: self.mode,
            policy: self.policy.clone().unwrap_or_default(),
            state: self.state.clone().unwrap_or_default(),
            caps: self.caps.clone(),
            created_at: self.auto_created_at.clone().unwrap_or_else(now_rfc3339),
            updated_at: self.auto_updated_at.clone().unwrap_or_else(now_rfc3339),
        })
    }
}

/// Fold `session.created`, `session.budget.updated`,
/// `session.budget.auto_extended`, and the compatibility
/// `session.budget_mode_changed` events into the latest budget projection.
pub fn budget_projection_from_events(
    events: &[Event],
) -> Result<Option<SessionBudgetProjection>, BudgetValidationError> {
    let Some(created) = events.iter().find(|event| event.kind == "session.created") else {
        return Ok(None);
    };
    let mut projection = SessionBudgetProjection {
        mode: created
            .payload
            .get("budget_mode")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| BudgetMode::parse(value).ok())
            .unwrap_or(BudgetMode::Fixed),
        unlimited: created
            .payload
            .get("unlimited")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        token_budget: created
            .payload
            .get("token_budget")
            .and_then(serde_json::Value::as_u64),
        cost_budget_usd: created
            .payload
            .get("cost_budget_usd")
            .and_then(serde_json::Value::as_f64),
        time_budget_secs: created
            .payload
            .get("time_budget_secs")
            .and_then(serde_json::Value::as_u64),
        policy: None,
        caps: AutoBudgetCaps::default(),
        state: None,
        auto_created_at: None,
        auto_updated_at: None,
        has_auto_row: false,
    };
    if let Some(value) = created.payload.get("auto_budget").cloned() {
        let record: SessionAutoBudgetRecord = serde_json::from_value(value).map_err(|error| {
            invalid_state(format!(
                "session.created auto-budget row is malformed: {error}"
            ))
        })?;
        record.state.validate()?;
        record.policy.validate()?;
        record.caps.validate()?;
        projection.mode = record.mode;
        projection.policy = Some(record.policy);
        projection.caps = record.caps;
        projection.state = Some(record.state);
        projection.auto_created_at = Some(record.created_at);
        projection.auto_updated_at = Some(record.updated_at);
        projection.has_auto_row = true;
    }
    if projection.unlimited {
        // `unlimited` is the compatibility authority; the stored mode remains
        // visible for a later toggle back.
        projection.unlimited = true;
    }

    for event in events {
        match event.kind.as_str() {
            "session.budget.updated" => {
                if let Some(mode) = event
                    .payload
                    .get("mode")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|value| BudgetMode::parse(value).ok())
                {
                    projection.mode = mode;
                }
                projection.unlimited = event
                    .payload
                    .get("unlimited")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(projection.unlimited);
                projection.token_budget = event
                    .payload
                    .get("token_budget")
                    .and_then(serde_json::Value::as_u64);
                projection.cost_budget_usd = event
                    .payload
                    .get("cost_budget_usd")
                    .and_then(serde_json::Value::as_f64);
                projection.time_budget_secs = event
                    .payload
                    .get("time_budget_secs")
                    .and_then(serde_json::Value::as_u64);
                projection.caps = AutoBudgetCaps {
                    token: event
                        .payload
                        .get("hard_token_cap")
                        .and_then(serde_json::Value::as_u64),
                    cost_usd: event
                        .payload
                        .get("hard_cost_cap_usd")
                        .and_then(serde_json::Value::as_f64),
                    wall_clock_secs: event
                        .payload
                        .get("hard_wall_clock_secs")
                        .and_then(serde_json::Value::as_u64),
                };
                if let Some(value) = event.payload.get("policy").cloned() {
                    let policy: AutoBudgetPolicy =
                        serde_json::from_value(value).map_err(|error| {
                            invalid_state(format!(
                                "session.budget.updated policy is malformed: {error}"
                            ))
                        })?;
                    policy.validate()?;
                    projection.policy = Some(policy);
                }
                if let Some(value) = event.payload.get("state").cloned() {
                    let state: SessionAutoBudgetState =
                        serde_json::from_value(value).map_err(|error| {
                            invalid_state(format!(
                                "session.budget.updated state is malformed: {error}"
                            ))
                        })?;
                    state.validate()?;
                    projection.state = Some(state);
                }
                projection.caps.validate()?;
                projection.auto_created_at = projection
                    .auto_created_at
                    .or_else(|| Some(event.created_at.clone()));
                projection.auto_updated_at = Some(
                    event
                        .payload
                        .get("updated_at")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| event.created_at.clone()),
                );
                projection.has_auto_row = true;
            }
            "session.budget.auto_extended" => {
                if let Some(new) = event.payload.get("new") {
                    projection.token_budget =
                        new.get("token_budget").and_then(serde_json::Value::as_u64);
                    projection.cost_budget_usd = new
                        .get("cost_budget_usd")
                        .and_then(serde_json::Value::as_f64);
                    projection.time_budget_secs = new
                        .get("time_budget_secs")
                        .and_then(serde_json::Value::as_u64);
                }
                if let Some(value) = event.payload.get("state").cloned() {
                    let state: SessionAutoBudgetState =
                        serde_json::from_value(value).map_err(|error| {
                            invalid_state(format!(
                                "session.budget.auto_extended state is malformed: {error}"
                            ))
                        })?;
                    state.validate()?;
                    projection.state = Some(state);
                }
                projection.auto_created_at = projection
                    .auto_created_at
                    .or_else(|| Some(event.created_at.clone()));
                projection.auto_updated_at = Some(
                    event
                        .payload
                        .get("updated_at")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| event.created_at.clone()),
                );
                projection.has_auto_row = true;
            }
            "session.auto_state" => {
                if let Some(value) = event.payload.get("state").cloned() {
                    let state: SessionAutoBudgetState =
                        serde_json::from_value(value).map_err(|error| {
                            invalid_state(format!("session.auto_state state is malformed: {error}"))
                        })?;
                    state.validate()?;
                    projection.state = Some(state);
                }
                projection.auto_updated_at = Some(
                    event
                        .payload
                        .get("updated_at")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| event.created_at.clone()),
                );
                projection.has_auto_row = true;
            }
            "session.budget_mode_changed" => {
                let unlimited = event
                    .payload
                    .get("unlimited")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(projection.unlimited);
                projection.unlimited = unlimited;
                if !unlimited && projection.mode == BudgetMode::Unlimited {
                    projection.mode = BudgetMode::Fixed;
                }
            }
            _ => {}
        }
    }
    Ok(Some(projection))
}

fn invalid_state(message: String) -> BudgetValidationError {
    BudgetValidationError::new(auto_budget_codes::INVALID_AUTO_BUDGET_STATE, message)
}

/// Resolve the externally visible mode. `Session.unlimited` always wins for
/// wire compatibility; a stored `auto` row is otherwise preserved across a
/// temporary unlimited toggle.
pub fn effective_budget_mode(
    session: &Session,
    record: Option<&SessionAutoBudgetRecord>,
) -> BudgetMode {
    if session.unlimited {
        return BudgetMode::Unlimited;
    }
    record.map(|row| row.mode).unwrap_or(BudgetMode::Fixed)
}

pub fn budget_status(
    session: &Session,
    record: Option<&SessionAutoBudgetRecord>,
    usage: &[UsageEntry],
    project_policy: &AutoBudgetPolicy,
    now: &str,
) -> SessionBudgetStatus {
    let mode = effective_budget_mode(session, record);
    let policy = record
        .map(|row| row.policy.clone())
        .unwrap_or_else(|| project_policy.clone());
    let state = record.map(|row| row.state.clone());
    let caps = record.map(|row| row.caps.clone()).unwrap_or_default();
    let spent_tokens = session.tokens_in.saturating_add(session.tokens_out);
    let remaining_tokens = session
        .token_budget
        .map(|budget| budget.saturating_sub(spent_tokens));
    let remaining_cost_usd = session
        .cost_budget_usd
        .map(|budget| (budget - session.cost_spent_usd).max(0.0));
    let elapsed_secs = state.as_ref().map(|state| state.elapsed_secs).unwrap_or(0);
    let remaining_secs = session
        .time_budget_secs
        .map(|budget| budget.saturating_sub(elapsed_secs));
    let forecast = budget_forecast(
        session,
        record,
        usage,
        &policy,
        remaining_tokens,
        remaining_cost_usd,
        remaining_secs,
        now,
    );
    SessionBudgetStatus {
        session_id: session.id.clone(),
        mode,
        unlimited: session.unlimited,
        effective_token_budget: session.token_budget,
        effective_cost_budget_usd: session.cost_budget_usd,
        effective_time_budget_secs: session.time_budget_secs,
        hard_token_cap: caps.token,
        hard_cost_cap_usd: caps.cost_usd,
        hard_wall_clock_secs: caps.wall_clock_secs,
        spent_tokens,
        spent_cost_usd: session.cost_spent_usd,
        elapsed_secs,
        remaining_tokens,
        remaining_cost_usd,
        remaining_secs,
        policy: Some(policy),
        state,
        forecast,
        updated_at: record.map(|row| row.updated_at.clone()),
    }
}

#[allow(clippy::too_many_arguments)]
fn budget_forecast(
    session: &Session,
    record: Option<&SessionAutoBudgetRecord>,
    usage: &[UsageEntry],
    policy: &AutoBudgetPolicy,
    remaining_tokens: Option<u64>,
    remaining_cost_usd: Option<f64>,
    remaining_secs: Option<u64>,
    now: &str,
) -> BudgetForecast {
    let sample_limit = policy.forecast_calls.clamp(1, 1_000) as usize;
    let sample = usage.iter().rev().take(sample_limit).collect::<Vec<_>>();
    let sample_calls = sample.len() as u32;
    let total_input = sample.iter().fold(0u64, |total, entry| {
        total.saturating_add(entry.input_tokens)
    });
    let total_output = sample.iter().fold(0u64, |total, entry| {
        total.saturating_add(entry.output_tokens)
    });
    let total_cost = sample.iter().map(|entry| entry.cost_usd).sum::<f64>();
    let divisor = sample_calls.max(1) as u64;
    let avg_input_tokens_per_call = total_input / divisor;
    let avg_output_tokens_per_call = total_output / divisor;
    let avg_cost_usd_per_call = if sample_calls == 0 {
        0.0
    } else {
        total_cost / sample_calls as f64
    };
    let source = if sample.is_empty() {
        BudgetForecastSource::Unavailable
    } else if sample.iter().all(|entry| entry.estimated) {
        BudgetForecastSource::Estimate
    } else if sample.iter().any(|entry| entry.estimated) {
        BudgetForecastSource::Mixed
    } else {
        BudgetForecastSource::ProviderLedger
    };
    let avg_tokens_per_call = avg_input_tokens_per_call.saturating_add(avg_output_tokens_per_call);
    let mut runway_turns: Option<u32> = None;
    if avg_tokens_per_call > 0
        && let Some(remaining) = remaining_tokens
    {
        runway_turns = Some((remaining / avg_tokens_per_call).min(u32::MAX as u64) as u32);
    }
    if avg_cost_usd_per_call > 0.0
        && let Some(remaining) = remaining_cost_usd
    {
        let cost_turns = (remaining / avg_cost_usd_per_call).max(0.0) as u64;
        let cost_turns = cost_turns.min(u32::MAX as u64) as u32;
        runway_turns = Some(match runway_turns {
            Some(turns) => turns.min(cost_turns),
            None => cost_turns,
        });
    }
    let runway_cost_usd = match (remaining_cost_usd, runway_turns) {
        (Some(remaining), Some(turns)) => Some(remaining.min(avg_cost_usd_per_call * turns as f64)),
        (Some(remaining), None) => Some(remaining),
        (None, _) => None,
    };
    // Record-only dimensions are intentionally included in the status but do
    // not fabricate a turn estimate. Runtime may add plan critical-path turns.
    let _ = (session, record);
    BudgetForecast {
        sample_calls,
        avg_input_tokens_per_call,
        avg_output_tokens_per_call,
        avg_cost_usd_per_call,
        remaining_tokens,
        remaining_cost_usd,
        remaining_secs,
        runway_turns,
        runway_secs: remaining_secs,
        runway_cost_usd,
        critical_path_turns: None,
        source,
        generated_at: now.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vp_protocol::types::Project;

    fn session() -> Session {
        Session {
            id: "ses-1".into(),
            project_id: "prj-1".into(),
            goal: "forecast".into(),
            status: "idle".into(),
            priority: 0,
            worktree: None,
            branch: None,
            token_budget: Some(1_000),
            cost_budget_usd: Some(2.0),
            time_budget_secs: Some(60),
            unlimited: false,
            cost_spent_usd: 0.5,
            base_commit: None,
            tokens_in: 300,
            tokens_out: 100,
            prompt_layout: 3,
            created_at: "2026-11-21T00:00:00Z".into(),
        }
    }

    fn usage(input: u64, output: u64, cost: f64, estimated: bool) -> UsageEntry {
        UsageEntry {
            id: format!("use-{input}-{output}"),
            session_id: "ses-1".into(),
            agent_id: "agent".into(),
            agent_name: "Agent".into(),
            role: "leader".into(),
            task_id: None,
            department: None,
            model: "model".into(),
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            estimated,
            cost_usd: cost,
            created_at: "2026-11-21T00:00:00Z".into(),
        }
    }

    #[test]
    fn forecast_uses_the_bounded_ledger_window() {
        let session = session();
        let usage = vec![
            usage(100, 50, 0.25, false),
            usage(300, 150, 0.75, false),
            usage(900, 900, 9.0, false),
        ];
        let policy = AutoBudgetPolicy {
            forecast_calls: 2,
            ..AutoBudgetPolicy::default()
        };
        let forecast = budget_forecast(
            &session,
            None,
            &usage,
            &policy,
            Some(400),
            Some(1.5),
            Some(30),
            "2026-11-21T01:00:00Z",
        );
        assert_eq!(forecast.sample_calls, 2);
        assert_eq!(forecast.avg_input_tokens_per_call, 600);
        assert_eq!(forecast.avg_output_tokens_per_call, 525);
        assert_eq!(forecast.source, BudgetForecastSource::ProviderLedger);
        assert_eq!(forecast.runway_turns, Some(0));
    }

    #[test]
    fn status_prefers_unlimited_flag_over_stored_auto_mode() {
        let mut session = session();
        session.unlimited = true;
        let record = SessionAutoBudgetRecord {
            session_id: session.id.clone(),
            mode: BudgetMode::Auto,
            policy: AutoBudgetPolicy::default(),
            state: SessionAutoBudgetState::default(),
            caps: AutoBudgetCaps {
                token: Some(5_000),
                ..AutoBudgetCaps::default()
            },
            created_at: "2026-11-21T00:00:00Z".into(),
            updated_at: "2026-11-21T00:00:00Z".into(),
        };
        let status = budget_status(
            &session,
            Some(&record),
            &[],
            &AutoBudgetPolicy::default(),
            "2026-11-21T01:00:00Z",
        );
        assert_eq!(status.mode, BudgetMode::Unlimited);
        assert!(status.unlimited);
        assert_eq!(status.hard_token_cap, Some(5_000));
        assert_eq!(status.remaining_tokens, Some(600));
    }

    #[test]
    fn projection_restores_auto_row_and_budget_updates() {
        let created = Event {
            session_id: "ses-1".into(),
            seq: 1,
            kind: "session.created".into(),
            payload: serde_json::json!({
                "project_id": "prj-1",
                "goal": "auto",
                "token_budget": 1000,
                "cost_budget_usd": 1.0,
                "time_budget_secs": null,
                "unlimited": false,
                "budget_mode": "auto",
                "auto_budget": {
                    "session_id": "ses-1",
                    "mode": "auto",
                    "policy": {"enabled": true, "hard_token_cap": 5000},
                    "state": {"extensions": 1},
                    "caps": {"token": 5000},
                    "created_at": "2026-11-21T00:00:00Z",
                    "updated_at": "2026-11-21T00:00:00Z"
                },
                "created_at": "2026-11-21T00:00:00Z"
            }),
            created_at: "2026-11-21T00:00:00Z".into(),
        };
        let updated = Event {
            session_id: "ses-1".into(),
            seq: 2,
            kind: "session.budget.updated".into(),
            payload: serde_json::json!({
                "mode": "auto",
                "unlimited": false,
                "token_budget": 2000,
                "cost_budget_usd": 2.0,
                "time_budget_secs": 60,
                "hard_token_cap": 5000,
                "hard_cost_cap_usd": 3.0,
                "hard_wall_clock_secs": 120,
                "policy": {"enabled": true, "hard_token_cap": 5000},
                "state": {"extensions": 2, "elapsed_secs": 12},
                "updated_at": "2026-11-21T01:00:00Z"
            }),
            created_at: "2026-11-21T01:00:00Z".into(),
        };
        let projection = budget_projection_from_events(&[created, updated])
            .unwrap()
            .unwrap();
        assert_eq!(projection.mode, BudgetMode::Auto);
        assert_eq!(projection.token_budget, Some(2000));
        assert_eq!(projection.caps.cost_usd, Some(3.0));
        let record = projection.auto_record("ses-1").unwrap();
        assert_eq!(record.state.extensions, 2);
        assert_eq!(record.state.elapsed_secs, 12);
        assert_eq!(record.updated_at, "2026-11-21T01:00:00Z");
    }

    #[test]
    fn projection_restores_auto_extension_and_opaque_progress_state() {
        let events = vec![
            Event {
                session_id: "ses-1".into(),
                seq: 1,
                kind: "session.created".into(),
                payload: serde_json::json!({
                    "project_id": "prj-1",
                    "goal": "auto",
                    "token_budget": 1000,
                    "unlimited": false,
                    "budget_mode": "auto",
                    "auto_budget": {
                        "session_id": "ses-1",
                        "mode": "auto",
                        "policy": {"enabled": true, "initial_token_budget": 1000, "hard_token_cap": 5000},
                        "state": {"progress": {"turn": 1}},
                        "caps": {"token": 5000},
                        "created_at": "2026-11-21T00:00:00Z",
                        "updated_at": "2026-11-21T00:00:00Z"
                    },
                    "created_at": "2026-11-21T00:00:00Z"
                }),
                created_at: "2026-11-21T00:00:00Z".into(),
            },
            Event {
                session_id: "ses-1".into(),
                seq: 2,
                kind: "session.budget.auto_extended".into(),
                payload: serde_json::json!({
                    "new": {"token_budget": 1250, "cost_budget_usd": null, "time_budget_secs": null},
                    "state": {"extensions": 1, "progress": {"turn": 2}},
                    "updated_at": "2026-11-21T01:00:00Z"
                }),
                created_at: "2026-11-21T01:00:00Z".into(),
            },
        ];
        let projection = budget_projection_from_events(&events).unwrap().unwrap();
        assert_eq!(projection.token_budget, Some(1_250));
        assert_eq!(projection.state.as_ref().unwrap().extensions, 1);
        assert_eq!(
            projection.state.unwrap().progress,
            Some(serde_json::json!({"turn": 2}))
        );
    }

    #[test]
    fn oversized_progress_state_fails_closed() {
        let state = SessionAutoBudgetState {
            progress: Some(serde_json::json!({"blob": "x".repeat(70_000)})),
            ..SessionAutoBudgetState::default()
        };
        let error = state.validate().unwrap_err();
        assert_eq!(error.code, "INVALID_AUTO_BUDGET_STATE");
    }

    #[test]
    fn project_fixture_is_not_used_for_forecast_content() {
        // Keep the Project import exercised so fixture drift is obvious.
        let project = Project {
            id: "prj-1".into(),
            name: "p".into(),
            root: "/tmp/p".into(),
            default_branch: "main".into(),
            created_at: "2026-11-21T00:00:00Z".into(),
        };
        assert_eq!(project.id, "prj-1");
    }
}
