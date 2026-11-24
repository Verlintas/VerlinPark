//! Session lifecycle, context, and transcript commands.

use super::*;

/// Build a typed local CLI failure (no daemon RPC was made). The stable code
/// matches the daemon taxonomy so scripts see the same reason.
fn typed_local_error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    crate::rpc::CliRpcError {
        kind: "local",
        code: None,
        typed_code: Some(code.to_string()),
        message: message.into(),
        hint: None,
        data: None,
    }
    .into()
}

/// Build the `session.delivery.partial` params. `--yes` is required locally so
/// an accidental command cannot write a partial-delivery event before the RPC.
fn partial_delivery_params(
    session: String,
    summary: Option<String>,
    remaining: Vec<String>,
    recorded_by: Option<String>,
    yes: bool,
) -> Result<Value> {
    if !yes {
        return Err(typed_local_error(
            vp_core::partial_delivery_codes::DELIVERY_CONFIRM_REQUIRED,
            "recording a partial delivery requires --yes; it does not merge, verify, or complete work",
        ));
    }
    Ok(json!({
        "session_id": session,
        "summary": summary,
        "remaining_work": remaining,
        "recorded_by": recorded_by,
        "confirm": true,
    }))
}

fn validate_positive_tokens(label: &str, value: Option<u64>, cap: bool) -> Result<()> {
    if value == Some(0) {
        let code = if cap {
            vp_protocol::types::auto_budget_codes::INVALID_BUDGET_CAP
        } else {
            vp_protocol::types::auto_budget_codes::INVALID_BUDGET
        };
        return Err(typed_local_error(
            code,
            format!("{label} must be greater than zero"),
        ));
    }
    Ok(())
}

fn validate_positive_cost(label: &str, value: Option<f64>, cap: bool) -> Result<()> {
    if let Some(value) = value
        && (!value.is_finite() || value <= 0.0)
    {
        let code = if cap {
            vp_protocol::types::auto_budget_codes::INVALID_BUDGET_CAP
        } else {
            vp_protocol::types::auto_budget_codes::INVALID_BUDGET
        };
        return Err(typed_local_error(
            code,
            format!("{label} must be a positive finite USD value"),
        ));
    }
    Ok(())
}

fn hours_to_secs(label: &str, hours: f64, cap: bool) -> Result<u64> {
    if !hours.is_finite() || hours <= 0.0 {
        let code = if cap {
            vp_protocol::types::auto_budget_codes::INVALID_BUDGET_CAP
        } else {
            vp_protocol::types::auto_budget_codes::INVALID_BUDGET
        };
        return Err(typed_local_error(
            code,
            format!("{label} must be a positive finite number of hours"),
        ));
    }
    let secs = hours * 3_600.0;
    if secs > u64::MAX as f64 {
        return Err(typed_local_error(
            vp_protocol::types::auto_budget_codes::INVALID_BUDGET,
            format!("{label} is too large"),
        ));
    }
    Ok(secs.ceil() as u64)
}

fn merge_budget_value(
    label: &str,
    explicit: Option<u64>,
    fallback: Option<u64>,
) -> Result<Option<u64>> {
    match (explicit, fallback) {
        (Some(left), Some(right)) if left != right => Err(typed_local_error(
            vp_protocol::types::auto_budget_codes::INVALID_BUDGET,
            format!("--{label} conflicts with the explicit budget flag; pass only one value"),
        )),
        (Some(value), _) => Ok(Some(value)),
        (None, value) => Ok(value),
    }
}

fn merge_budget_cost(
    label: &str,
    explicit: Option<f64>,
    fallback: Option<f64>,
) -> Result<Option<f64>> {
    match (explicit, fallback) {
        (Some(left), Some(right)) if (left - right).abs() > 1e-9 => Err(typed_local_error(
            vp_protocol::types::auto_budget_codes::INVALID_BUDGET,
            format!("--{label} conflicts with the explicit cost budget flag; pass only one value"),
        )),
        (Some(value), _) => Ok(Some(value)),
        (None, value) => Ok(value),
    }
}

/// Build `session.create` params. `--max-*` flags are hard caps in auto mode
/// and effective budgets in fixed mode. `--auto` without caps deliberately
/// sends no explicit policy so the project auto policy can supply one; the
/// daemon still fails closed with `AUTO_BUDGET_CAP_REQUIRED` when neither has a
/// cap.
#[allow(clippy::too_many_arguments)]
pub(super) fn session_new_params(
    project: String,
    goal: String,
    priority: i32,
    token_budget: Option<u64>,
    cost_budget_usd: Option<f64>,
    time_budget_secs: Option<u64>,
    unlimited: bool,
    budget_mode: Option<BudgetModeArg>,
    auto: bool,
    max_token: Option<u64>,
    max_usd: Option<f64>,
    max_hours: Option<f64>,
) -> Result<Value> {
    let mode = match (auto, budget_mode) {
        (true, None | Some(BudgetModeArg::Auto)) => Some(BudgetModeArg::Auto),
        (true, Some(other)) => {
            return Err(typed_local_error(
                vp_protocol::types::auto_budget_codes::BUDGET_MODE_CONFLICT,
                format!(
                    "--auto conflicts with --budget-mode {}; use --auto alone",
                    other.as_str()
                ),
            ));
        }
        (false, mode) => mode,
    };
    if unlimited && mode.is_some_and(|mode| mode != BudgetModeArg::Unlimited) {
        return Err(typed_local_error(
            vp_protocol::types::auto_budget_codes::BUDGET_MODE_CONFLICT,
            "an unlimited session cannot also select a fixed or auto budget mode",
        ));
    }

    validate_positive_tokens("token budget", token_budget, false)?;
    validate_positive_cost("cost budget", cost_budget_usd, false)?;
    validate_positive_tokens("time budget", time_budget_secs, false)?;

    let auto_mode = mode == Some(BudgetModeArg::Auto);
    let (token_budget, cost_budget_usd, time_budget_secs, auto_budget) = if auto_mode {
        validate_positive_tokens("--max-token", max_token, true)?;
        validate_positive_cost("--max-usd", max_usd, true)?;
        let max_secs = max_hours
            .map(|hours| hours_to_secs("--max-hours", hours, true))
            .transpose()?;
        validate_positive_tokens("--max-hours", max_secs, true)?;
        if let (Some(initial), Some(cap)) = (token_budget, max_token)
            && cap < initial
        {
            return Err(typed_local_error(
                vp_protocol::types::auto_budget_codes::INVALID_BUDGET_CAP,
                format!("--max-token {cap} is below the effective token budget {initial}"),
            ));
        }
        if let (Some(initial), Some(cap)) = (cost_budget_usd, max_usd)
            && cap + 1e-9 < initial
        {
            return Err(typed_local_error(
                vp_protocol::types::auto_budget_codes::INVALID_BUDGET_CAP,
                format!("--max-usd {cap} is below the effective cost budget {initial}"),
            ));
        }
        if let (Some(initial), Some(cap)) = (time_budget_secs, max_secs)
            && cap < initial
        {
            return Err(typed_local_error(
                vp_protocol::types::auto_budget_codes::INVALID_BUDGET_CAP,
                format!("--max-hours {cap}s is below the effective time budget {initial}s"),
            ));
        }
        let mut policy = serde_json::Map::new();
        policy.insert("enabled".into(), Value::Bool(true));
        if let Some(value) = max_token {
            policy.insert("hard_token_cap".into(), json!(value));
        }
        if let Some(value) = max_usd {
            policy.insert("hard_cost_cap_usd".into(), json!(value));
        }
        if let Some(value) = max_secs {
            policy.insert("hard_wall_clock_secs".into(), json!(value));
        }
        let policy = (policy.len() > 1).then_some(Value::Object(policy));
        (token_budget, cost_budget_usd, time_budget_secs, policy)
    } else {
        if unlimited && (max_token.is_some() || max_usd.is_some() || max_hours.is_some()) {
            return Err(typed_local_error(
                vp_protocol::types::auto_budget_codes::BUDGET_MODE_CONFLICT,
                "an unlimited session cannot also declare token, cost, or time budgets",
            ));
        }
        let max_secs = max_hours
            .map(|hours| hours_to_secs("--max-hours", hours, false))
            .transpose()?;
        (
            merge_budget_value("max-token", token_budget, max_token)?,
            merge_budget_cost("max-usd", cost_budget_usd, max_usd)?,
            merge_budget_value("max-hours", time_budget_secs, max_secs)?,
            None,
        )
    };

    Ok(json!({
        "project_id": project,
        "goal": goal,
        "priority": priority,
        "token_budget": token_budget,
        "cost_budget_usd": cost_budget_usd,
        "time_budget_secs": time_budget_secs,
        "unlimited": unlimited,
        "budget_mode": mode.map(BudgetModeArg::as_str),
        "auto_budget": auto_budget,
    }))
}

/// Build `session.budget.set` params without adding any unspecified field.
#[allow(clippy::too_many_arguments)]
pub(super) fn session_budget_update_params(
    session: String,
    mode: Option<BudgetModeArg>,
    token_budget: Option<u64>,
    cost_budget_usd: Option<f64>,
    time_budget_secs: Option<u64>,
    hard_token_cap: Option<u64>,
    hard_cost_cap_usd: Option<f64>,
    hard_wall_clock_secs: Option<u64>,
    clear_token_budget: bool,
    clear_cost_budget: bool,
    clear_time_budget: bool,
    clear_hard_token_cap: bool,
    clear_hard_cost_cap: bool,
    clear_hard_wall_clock_cap: bool,
    unlimited: Option<bool>,
) -> Result<Value> {
    if let (Some(mode), Some(unlimited)) = (mode, unlimited)
        && (mode == BudgetModeArg::Unlimited) != unlimited
    {
        return Err(typed_local_error(
            vp_protocol::types::auto_budget_codes::BUDGET_MODE_CONFLICT,
            "--mode and --unlimited disagree",
        ));
    }
    validate_positive_tokens("token budget", token_budget, false)?;
    validate_positive_cost("cost budget", cost_budget_usd, false)?;
    validate_positive_tokens("time budget", time_budget_secs, false)?;
    validate_positive_tokens("hard token cap", hard_token_cap, true)?;
    validate_positive_cost("hard cost cap", hard_cost_cap_usd, true)?;
    validate_positive_tokens("hard wall-clock cap", hard_wall_clock_secs, true)?;

    for (set, clear, label) in [
        (token_budget.is_some(), clear_token_budget, "token budget"),
        (cost_budget_usd.is_some(), clear_cost_budget, "cost budget"),
        (time_budget_secs.is_some(), clear_time_budget, "time budget"),
        (
            hard_token_cap.is_some(),
            clear_hard_token_cap,
            "hard token cap",
        ),
        (
            hard_cost_cap_usd.is_some(),
            clear_hard_cost_cap,
            "hard cost cap",
        ),
        (
            hard_wall_clock_secs.is_some(),
            clear_hard_wall_clock_cap,
            "hard wall-clock cap",
        ),
    ] {
        if set && clear {
            return Err(typed_local_error(
                vp_protocol::types::auto_budget_codes::INVALID_BUDGET,
                format!("cannot set and clear the same {label} in one update"),
            ));
        }
    }
    let has_update = mode.is_some()
        || unlimited.is_some()
        || token_budget.is_some()
        || cost_budget_usd.is_some()
        || time_budget_secs.is_some()
        || hard_token_cap.is_some()
        || hard_cost_cap_usd.is_some()
        || hard_wall_clock_secs.is_some()
        || clear_token_budget
        || clear_cost_budget
        || clear_time_budget
        || clear_hard_token_cap
        || clear_hard_cost_cap
        || clear_hard_wall_clock_cap;
    if !has_update {
        return Err(typed_local_error(
            vp_protocol::types::auto_budget_codes::INVALID_BUDGET,
            "pass at least one budget field to update",
        ));
    }

    let mut params = json!({"session_id": session});
    if let Some(mode) = mode {
        params["mode"] = json!(mode.as_str());
    }
    if let Some(unlimited) = unlimited {
        params["unlimited"] = json!(unlimited);
    }
    if let Some(value) = token_budget {
        params["token_budget"] = json!(value);
    }
    if let Some(value) = cost_budget_usd {
        params["cost_budget_usd"] = json!(value);
    }
    if let Some(value) = time_budget_secs {
        params["time_budget_secs"] = json!(value);
    }
    if let Some(value) = hard_token_cap {
        params["hard_token_cap"] = json!(value);
    }
    if let Some(value) = hard_cost_cap_usd {
        params["hard_cost_cap_usd"] = json!(value);
    }
    if let Some(value) = hard_wall_clock_secs {
        params["hard_wall_clock_secs"] = json!(value);
    }
    for (enabled, key) in [
        (clear_token_budget, "clear_token_budget"),
        (clear_cost_budget, "clear_cost_budget"),
        (clear_time_budget, "clear_time_budget"),
        (clear_hard_token_cap, "clear_hard_token_cap"),
        (clear_hard_cost_cap, "clear_hard_cost_cap"),
        (clear_hard_wall_clock_cap, "clear_hard_wall_clock_cap"),
    ] {
        if enabled {
            params[key] = json!(true);
        }
    }
    Ok(params)
}

pub(super) async fn dispatch(
    command: Command,
    discovery: Discovery,
    _json: bool,
) -> Result<Option<Value>> {
    let value = match command {
        Command::Sessions { project } => {
            rpc(&discovery, "session.list", json!({"project_id": project})).await?
        }
        Command::SessionNew {
            project,
            goal,
            priority,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            unlimited,
            budget_mode,
            auto,
            max_token,
            max_usd,
            max_hours,
        } => {
            let params = session_new_params(
                project,
                goal,
                priority,
                token_budget,
                cost_budget_usd,
                time_budget_secs,
                unlimited,
                budget_mode,
                auto,
                max_token,
                max_usd,
                max_hours,
            )?;
            rpc(&discovery, "session.create", params).await?
        }
        Command::SessionBudget { session, .. } => {
            rpc(
                &discovery,
                "session.budget.get",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::SessionBudgetForecast { session, .. } => {
            rpc(
                &discovery,
                "session.budget.forecast",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::SessionBudgetSet {
            session,
            mode,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            hard_token_cap,
            hard_cost_cap_usd,
            hard_wall_clock_secs,
            clear_token_budget,
            clear_cost_budget,
            clear_time_budget,
            clear_hard_token_cap,
            clear_hard_cost_cap,
            clear_hard_wall_clock_cap,
            unlimited,
        } => {
            let params = session_budget_update_params(
                session,
                mode,
                token_budget,
                cost_budget_usd,
                time_budget_secs,
                hard_token_cap,
                hard_cost_cap_usd,
                hard_wall_clock_secs,
                clear_token_budget,
                clear_cost_budget,
                clear_time_budget,
                clear_hard_token_cap,
                clear_hard_cost_cap,
                clear_hard_wall_clock_cap,
                unlimited,
            )?;
            rpc(&discovery, "session.budget.set", params).await?
        }
        Command::SessionUnlimited { session, enabled } => {
            let params = session_budget_update_params(
                session,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                false,
                false,
                false,
                false,
                false,
                false,
                Some(enabled.unwrap_or(true)),
            )?;
            rpc(&discovery, "session.budget.set", params).await?
        }
        Command::SessionPriority { session, priority } => {
            rpc(
                &discovery,
                "session.priority.set",
                json!({"session_id": session, "priority": priority}),
            )
            .await?
        }
        Command::Search {
            query,
            project,
            limit,
        } => {
            rpc(
                &discovery,
                "session.search",
                json!({"query": query, "project_id": project, "limit": limit}),
            )
            .await?
        }
        Command::Events {
            session,
            since,
            limit,
        } => {
            rpc(
                &discovery,
                "event.list",
                json!({"session_id": session, "since_seq": since, "limit": limit}),
            )
            .await?
        }
        Command::Prompt {
            session,
            content,
            operation_id,
        } => {
            rpc(
                &discovery,
                "session.prompt",
                json!({"session_id": session, "content": content, "operation_id": operation_id}),
            )
            .await?
        }
        Command::Cancel { session } => {
            rpc(&discovery, "session.cancel", json!({"session_id": session})).await?
        }
        Command::Pause { session } => {
            rpc(&discovery, "session.pause", json!({"session_id": session})).await?
        }
        Command::Resume { session } => {
            rpc(&discovery, "session.resume", json!({"session_id": session})).await?
        }
        Command::ResumePlan { session } => {
            rpc(
                &discovery,
                "session.plan_resume",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Compatibility { session, run } => {
            let mut params = json!({"session_id": session});
            if let Some(run) = run {
                params["run_id"] = json!(run);
            }
            rpc(&discovery, "session.compatibility", params).await?
        }
        Command::Diagnose {
            session,
            task,
            limit,
            graph,
            record,
            history,
        } => {
            if history {
                rpc(
                    &discovery,
                    "session.attributions",
                    json!({"session_id": session, "limit": limit}),
                )
                .await?
            } else {
                let method = if record {
                    "session.diagnosis.record"
                } else {
                    "session.diagnosis"
                };
                rpc(
                    &discovery,
                    method,
                    json!({
                        "session_id": session,
                        "target_task_id": task,
                        "limit": limit,
                        "include_graph": graph,
                    }),
                )
                .await?
            }
        }
        Command::Context { session } => {
            rpc(
                &discovery,
                "session.context",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Compact { session } => {
            rpc(
                &discovery,
                "session.compact",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Todos { session } => {
            rpc(&discovery, "session.todos", json!({"session_id": session})).await?
        }
        Command::CitationAudit { session } => {
            rpc(
                &discovery,
                "session.citation_audit",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Citations { session, message } => {
            rpc(
                &discovery,
                "session.citations",
                json!({"session_id": session, "message_id": message}),
            )
            .await?
        }
        Command::Transcript { session } => {
            rpc(
                &discovery,
                "session.transcript",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Usage { session, csv } => {
            let method = if csv {
                "session.usage_csv"
            } else {
                "session.usage"
            };
            rpc(&discovery, method, json!({"session_id": session})).await?
        }
        Command::Resources { session } => {
            rpc(
                &discovery,
                "session.resources",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Readiness { session } => {
            rpc(
                &discovery,
                "session.readiness",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Delivery { session } => {
            rpc(
                &discovery,
                "session.delivery",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::PartialDelivery {
            session,
            summary,
            remaining,
            recorded_by,
            yes,
        } => {
            let params = partial_delivery_params(session, summary, remaining, recorded_by, yes)?;
            rpc(&discovery, "session.delivery.partial", params).await?
        }
        Command::RunContract { session } => {
            rpc(
                &discovery,
                "session.run_contract.get",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::RunContractSet {
            session,
            profile,
            reserve,
            attention_cap,
            no_progress_turns,
        } => {
            let profile = vp_protocol::types::RunProfile::parse(&profile).ok_or_else(|| {
                anyhow::anyhow!("profile must be quality, balanced, fast, cheap, or unattended")
            })?;
            let stop_policy = no_progress_turns.map(|turns| vp_protocol::types::RunStopPolicy {
                no_progress_turns: turns,
                ..vp_protocol::types::RunStopPolicy::default()
            });
            rpc(
                &discovery,
                "session.run_contract.set",
                json!({
                    "session_id": session,
                    "profile": profile,
                    "completion_reserve_ratio": reserve,
                    "attention_cap": attention_cap,
                    "stop_policy": stop_policy,
                }),
            )
            .await?
        }
        Command::TopUp {
            session,
            tokens,
            cost,
            time,
            resume,
        } => {
            rpc(
                &discovery,
                "session.resources.top_up",
                json!({
                    "session_id": session,
                    "add_tokens": tokens,
                    "add_cost_usd": cost,
                    "add_time_secs": time,
                    "resume": resume,
                }),
            )
            .await?
        }
        _ => unreachable!("command routed to the wrong dispatch family"),
    };
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_new_auto_selects_project_policy_without_max_flags() {
        let params = session_new_params(
            "prj-1".into(),
            "goal".into(),
            0,
            None,
            None,
            None,
            false,
            None,
            true,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(params["budget_mode"], "auto");
        assert!(params["auto_budget"].is_null());
    }

    #[test]
    fn session_new_auto_max_flags_become_hard_caps() {
        let params = session_new_params(
            "prj-1".into(),
            "goal".into(),
            0,
            Some(1_000),
            None,
            None,
            false,
            None,
            true,
            Some(5_000),
            Some(2.5),
            Some(0.5),
        )
        .unwrap();
        assert_eq!(params["budget_mode"], "auto");
        assert_eq!(params["auto_budget"]["enabled"], true);
        assert_eq!(params["auto_budget"]["hard_token_cap"], 5_000);
        assert_eq!(params["auto_budget"]["hard_cost_cap_usd"], 2.5);
        assert_eq!(params["auto_budget"]["hard_wall_clock_secs"], 1_800);
        assert_eq!(params["token_budget"], 1_000);
    }

    #[test]
    fn session_new_fixed_max_flags_are_effective_budgets() {
        let params = session_new_params(
            "prj-1".into(),
            "goal".into(),
            0,
            None,
            None,
            None,
            false,
            Some(BudgetModeArg::Fixed),
            false,
            Some(2_000),
            Some(3.0),
            Some(2.0),
        )
        .unwrap();
        assert_eq!(params["budget_mode"], "fixed");
        assert_eq!(params["token_budget"], 2_000);
        assert_eq!(params["cost_budget_usd"], 3.0);
        assert_eq!(params["time_budget_secs"], 7_200);
        assert!(params["auto_budget"].is_null());
    }

    #[test]
    fn session_new_rejects_conflicting_auto_and_unlimited() {
        let error = session_new_params(
            "prj-1".into(),
            "goal".into(),
            0,
            None,
            None,
            None,
            false,
            None,
            true,
            Some(0),
            None,
            None,
        )
        .unwrap_err();
        let typed = error.downcast_ref::<crate::rpc::CliRpcError>().unwrap();
        assert_eq!(
            typed.typed_code.as_deref(),
            Some(vp_protocol::types::auto_budget_codes::INVALID_BUDGET_CAP)
        );

        let error = session_new_params(
            "prj-1".into(),
            "goal".into(),
            0,
            None,
            None,
            None,
            true,
            Some(BudgetModeArg::Auto),
            false,
            None,
            None,
            None,
        )
        .unwrap_err();
        let typed = error.downcast_ref::<crate::rpc::CliRpcError>().unwrap();
        assert_eq!(
            typed.typed_code.as_deref(),
            Some(vp_protocol::types::auto_budget_codes::BUDGET_MODE_CONFLICT)
        );
    }

    #[test]
    fn session_new_auto_hard_cap_must_cover_effective_budget() {
        let error = session_new_params(
            "prj-1".into(),
            "goal".into(),
            0,
            Some(5_000),
            None,
            None,
            false,
            None,
            true,
            Some(1_000),
            None,
            None,
        )
        .unwrap_err();
        let typed = error.downcast_ref::<crate::rpc::CliRpcError>().unwrap();
        assert_eq!(
            typed.typed_code.as_deref(),
            Some(vp_protocol::types::auto_budget_codes::INVALID_BUDGET_CAP)
        );
    }

    #[test]
    fn budget_update_only_carries_specified_fields() {
        let params = session_budget_update_params(
            "ses-1".into(),
            None,
            Some(2_000),
            None,
            None,
            None,
            None,
            None,
            false,
            false,
            false,
            false,
            false,
            false,
            None,
        )
        .unwrap();
        let object = params.as_object().unwrap();
        assert_eq!(object.len(), 2);
        assert_eq!(object["session_id"], "ses-1");
        assert_eq!(object["token_budget"], 2_000);
    }

    #[test]
    fn budget_update_preserves_mode_and_clear_flags() {
        let params = session_budget_update_params(
            "ses-1".into(),
            Some(BudgetModeArg::Auto),
            None,
            None,
            None,
            Some(10_000),
            None,
            None,
            false,
            false,
            false,
            false,
            true,
            false,
            None,
        )
        .unwrap();
        assert_eq!(params["mode"], "auto");
        assert_eq!(params["hard_token_cap"], 10_000);
        assert_eq!(params["clear_hard_cost_cap"], true);
        assert!(params.get("token_budget").is_none());
    }

    #[test]
    fn budget_update_rejects_zero_hard_caps_and_set_clear_conflicts() {
        let error = session_budget_update_params(
            "ses-1".into(),
            None,
            None,
            None,
            None,
            Some(0),
            None,
            None,
            false,
            false,
            false,
            false,
            false,
            false,
            None,
        )
        .unwrap_err();
        let typed = error.downcast_ref::<crate::rpc::CliRpcError>().unwrap();
        assert_eq!(
            typed.typed_code.as_deref(),
            Some(vp_protocol::types::auto_budget_codes::INVALID_BUDGET_CAP)
        );

        let error = session_budget_update_params(
            "ses-1".into(),
            None,
            Some(1_000),
            None,
            None,
            None,
            None,
            None,
            true,
            false,
            false,
            false,
            false,
            false,
            None,
        )
        .unwrap_err();
        let typed = error.downcast_ref::<crate::rpc::CliRpcError>().unwrap();
        assert_eq!(
            typed.typed_code.as_deref(),
            Some(vp_protocol::types::auto_budget_codes::INVALID_BUDGET)
        );
    }

    #[test]
    fn budget_update_legacy_unlimited_payload_is_unchanged() {
        let params = session_budget_update_params(
            "ses-1".into(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            false,
            false,
            false,
            false,
            false,
            Some(true),
        )
        .unwrap();
        assert_eq!(params, json!({"session_id": "ses-1", "unlimited": true}));
    }

    #[test]
    fn built_session_requests_decode_as_protocol_params() {
        let create = session_new_params(
            "prj-1".into(),
            "goal".into(),
            3,
            None,
            None,
            None,
            false,
            None,
            true,
            Some(50_000),
            Some(5.0),
            None,
        )
        .unwrap();
        let decoded: vp_protocol::types::CreateSessionParams =
            serde_json::from_value(create).unwrap();
        assert_eq!(decoded.project_id, "prj-1");
        assert_eq!(
            decoded.budget_mode,
            Some(vp_protocol::types::BudgetMode::Auto)
        );
        assert_eq!(
            decoded.auto_budget.expect("auto policy").hard_token_cap,
            Some(50_000)
        );

        let update = session_budget_update_params(
            "ses-1".into(),
            Some(BudgetModeArg::Auto),
            Some(1_000),
            None,
            None,
            Some(5_000),
            None,
            None,
            false,
            false,
            false,
            false,
            false,
            false,
            None,
        )
        .unwrap();
        let decoded: vp_protocol::types::SetSessionBudgetParams =
            serde_json::from_value(update).unwrap();
        assert_eq!(decoded.session_id, "ses-1");
        assert_eq!(decoded.mode, Some(vp_protocol::types::BudgetMode::Auto));
        assert_eq!(decoded.token_budget, Some(1_000));
        assert_eq!(decoded.hard_token_cap, Some(5_000));
        assert!(decoded.unlimited.is_none());
        assert!(!decoded.clear_token_budget);
    }

    #[test]
    fn delivery_and_partial_delivery_flags_parse_and_require_yes() {
        use clap::Parser;

        let cli = crate::cli::Cli::try_parse_from(["vp", "delivery", "ses-1"]).unwrap();
        match cli.command {
            Command::Delivery { session } => assert_eq!(session, "ses-1"),
            other => panic!("expected delivery command, got {other:?}"),
        }

        let cli = crate::cli::Cli::try_parse_from([
            "vp",
            "partial-delivery",
            "ses-1",
            "--summary",
            "stopped after the parser",
            "--remaining",
            "wire the CLI",
            "--remaining",
            "run the verifier",
            "--recorded-by",
            "operator",
            "--yes",
        ])
        .unwrap();
        match cli.command {
            Command::PartialDelivery {
                session,
                summary,
                remaining,
                recorded_by,
                yes,
            } => {
                assert_eq!(session, "ses-1");
                assert_eq!(summary.as_deref(), Some("stopped after the parser"));
                assert_eq!(
                    remaining,
                    vec!["wire the CLI".to_string(), "run the verifier".to_string()]
                );
                assert_eq!(recorded_by.as_deref(), Some("operator"));
                assert!(yes);
                let params =
                    partial_delivery_params(session, summary, remaining, recorded_by, yes).unwrap();
                let decoded: vp_protocol::types::PartialDeliveryParams =
                    serde_json::from_value(params).unwrap();
                assert_eq!(decoded.session_id, "ses-1");
                assert!(decoded.confirm);
                assert_eq!(decoded.remaining_work.len(), 2);
            }
            other => panic!("expected partial-delivery command, got {other:?}"),
        }

        let error = partial_delivery_params(
            "ses-1".into(),
            Some("stopped".into()),
            Vec::new(),
            None,
            false,
        )
        .expect_err("missing --yes must fail before the RPC");
        let typed = error
            .downcast_ref::<crate::rpc::CliRpcError>()
            .expect("typed local CLI error");
        assert_eq!(
            typed.typed_code.as_deref(),
            Some(vp_core::partial_delivery_codes::DELIVERY_CONFIRM_REQUIRED)
        );
    }
}
