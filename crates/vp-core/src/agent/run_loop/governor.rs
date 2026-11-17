use super::*;

pub(in crate::agent) async fn emit_scope_budget_stop(
    engine: &Engine,
    session_id: &str,
    spec: &AgentSpec,
    stop: &BudgetStop,
) -> Result<()> {
    emit(
        engine,
        session_id,
        spec,
        "agent.budget_exceeded",
        stop.payload(),
    )
    .await
}

pub(in crate::agent) struct GovernorDeferralEvent<'a> {
    pub(in crate::agent) engine: &'a Engine,
    pub(in crate::agent) session: &'a Session,
    pub(in crate::agent) spec: &'a AgentSpec,
    pub(in crate::agent) context: Option<&'a GovernorRunContext>,
    pub(in crate::agent) gate: Option<&'a GovernorGate>,
    pub(in crate::agent) reason: BudgetReason,
    pub(in crate::agent) requested_tokens: u64,
    pub(in crate::agent) remaining_tokens: Option<u64>,
    pub(in crate::agent) limit_tokens: Option<u64>,
    pub(in crate::agent) spent_tokens: u64,
    pub(in crate::agent) reserved_tokens: u64,
}

/// Persist the provider-call deferral that the budget governor rejected. This
/// is the bridge between the in-flight reservation registry and the event-backed
/// decision history; the task itself remains retryable/queued.
pub(in crate::agent) async fn emit_governor_deferral(
    input: GovernorDeferralEvent<'_>,
) -> Result<()> {
    let reason = match input.reason {
        BudgetReason::Throttled => GovernorReason::Throttle,
        _ => GovernorReason::Reserve,
    };
    let now = vp_protocol::now_rfc3339();
    let decision_id = format!("gov-{}", uuid::Uuid::now_v7());
    let path = input
        .context
        .map(|context| context.path_task_ids.as_slice())
        .unwrap_or(&[]);
    let protected = input.gate.map(|gate| gate.protected_tokens).unwrap_or(0);
    let detail = format!(
        "non-critical request for {} tokens deferred by {}; remaining {} · protected {}",
        input.requested_tokens,
        reason.as_str(),
        input
            .remaining_tokens
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unbounded".to_string()),
        protected
    );
    let payload = denial_payload(GovernorDenial {
        session_id: &input.session.id,
        decision_id: &decision_id,
        plan_revision: input
            .context
            .map(|context| context.plan_revision)
            .unwrap_or(0),
        mode: input
            .context
            .map(|context| context.policy.mode)
            .unwrap_or_default(),
        task_id: input.spec.task_id.as_deref().unwrap_or("unknown"),
        task_critical: input
            .context
            .map(|context| context.critical)
            .unwrap_or(false),
        reason,
        requested_tokens: input.requested_tokens,
        limit_tokens: input.limit_tokens,
        spent_tokens: input.spent_tokens,
        reserved_tokens: input.reserved_tokens,
        remaining_tokens: input.remaining_tokens,
        protected_tokens: protected,
        path,
        path_weight_tokens: input
            .context
            .map(|context| context.path_weight_tokens)
            .unwrap_or(0),
        inputs_hash: input
            .context
            .map(|context| context.inputs_hash.as_str())
            .unwrap_or(""),
        detail,
        created_at: &now,
    });
    emit(
        input.engine,
        &input.session.id,
        input.spec,
        DECISION_EVENT_KIND,
        payload,
    )
    .await
}
