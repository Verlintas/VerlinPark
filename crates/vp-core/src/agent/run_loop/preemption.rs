use super::*;

/// Record the terminal preemption after the session is quiescent. The caller
/// must finalize the session status first: checkpoint creation refuses a
/// running/paused session. A checkpoint failure is logged and never changes the
/// cancelled status, never merges incomplete work, and never hides the event.
pub(in crate::agent) async fn record_terminal_preemption(
    engine: &Engine,
    session_id: &str,
    project_id: &str,
    spec: &AgentSpec,
    control: &RunControl,
    snapshot: &crate::preemption::Snapshot,
) -> Result<()> {
    let policy = control.preemption_policy();
    let mut checkpoint_id = None;
    if policy.checkpoint_on_preempt {
        let label = format!(
            "preempted: {} during {}",
            snapshot.reason.as_str(),
            snapshot.phase.as_str()
        );
        match timeout(
            Duration::from_secs(60),
            engine.checkpoint_session_at_preemption(session_id, Some(label)),
        )
        .await
        {
            Ok(Ok(checkpoint)) => checkpoint_id = Some(checkpoint.id),
            Ok(Err(error)) => {
                tracing::warn!(%error, session_id, "preemption checkpoint was not created");
            }
            Err(_) => {
                tracing::warn!(
                    session_id,
                    "preemption checkpoint exceeded its 60s bound and was skipped"
                );
            }
        }
    }
    let attribution = crate::preemption::PreemptAttribution {
        session_id,
        project_id,
        agent_id: &spec.id,
        agent_name: &spec.name,
        agent_role: spec.role.as_str(),
        task_id: spec.task_id.as_deref(),
        department: spec.department.as_deref(),
    };
    let mut task_checkpoint_commit = None;
    if let Some(task_id) = spec.task_id.as_deref()
        && matches!(spec.role, AgentRole::Worker | AgentRole::Labour)
    {
        let label = format!(
            "preempted: {} during {}",
            snapshot.reason.as_str(),
            snapshot.phase.as_str()
        );
        match timeout(
            Duration::from_secs(30),
            engine.snapshot_preempted_task_worktree(session_id, task_id, &spec.worktree, &label),
        )
        .await
        {
            Ok(Ok(commit)) => task_checkpoint_commit = Some(commit),
            Ok(Err(error)) => {
                tracing::warn!(
                    %error,
                    session_id,
                    task_id,
                    "preempted task worktree snapshot was not created"
                );
            }
            Err(_) => {
                tracing::warn!(
                    session_id,
                    task_id,
                    "preempted task worktree snapshot exceeded its 30s bound and was skipped"
                );
            }
        }
    }
    engine
        .append_event(
            session_id,
            "session.preempted",
            serde_json::to_value(crate::preemption::record(
                &attribution,
                snapshot,
                true,
                checkpoint_id,
                task_checkpoint_commit,
                None,
            ))?,
        )
        .await?;
    control
        .finalize_session(engine, session_id, "cancelled")
        .await?;
    Ok(())
}

/// Record a preemption that happened before the run loop started (queued for
/// admission or a global slot). No worktree changes exist yet, so this records
/// the terminal event without a checkpoint.
pub(in crate::agent) async fn record_queued_preemption(
    engine: &Engine,
    session_id: &str,
    project_id: &str,
    control: &RunControl,
    snapshot: &crate::preemption::Snapshot,
) -> Result<()> {
    let attribution = crate::preemption::PreemptAttribution {
        session_id,
        project_id,
        agent_id: "leader",
        agent_name: "Leader",
        agent_role: "leader",
        task_id: None,
        department: None,
    };
    engine
        .append_event(
            session_id,
            "session.preempted",
            serde_json::to_value(crate::preemption::record(
                &attribution,
                snapshot,
                true,
                None,
                None,
                Some("run preempted while waiting for admission".to_string()),
            ))?,
        )
        .await?;
    control
        .finalize_session(engine, session_id, "cancelled")
        .await?;
    Ok(())
}
