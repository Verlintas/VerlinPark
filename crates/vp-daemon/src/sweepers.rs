use std::time::Duration;

use vp_core::Engine;
use vp_core::agent::AgentService;

/// Expire stale pending approvals for projects with attention enabled. The
/// sweep never approves; it denies the waiting agents and emits
/// `approval.expired` for each row.
pub(crate) fn spawn_approval_sweeper(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_APPROVAL_SWEEP_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30);
    if seconds == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        loop {
            interval.tick().await;
            match engine.sweep_expired_approvals().await {
                Ok(0) => {}
                Ok(expired) => tracing::info!(expired, "approval attention sweep"),
                Err(error) => tracing::warn!(%error, "approval attention sweep failed"),
            }
        }
    });
}

pub(crate) fn spawn_gui_timeline_retention_sweeper(engine: Engine) {
    let interval_secs = std::env::var("VERLINPARK_GUI_RETENTION_SWEEP_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value >= 60)
        .unwrap_or(6 * 3_600);
    let batch = std::env::var("VERLINPARK_GUI_RETENTION_BATCH")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(1_000)
        .clamp(1, 10_000);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
        ticker.tick().await;
        loop {
            ticker.tick().await;
            match engine.sweep_gui_timeline_retention(batch).await {
                Ok(results) if !results.is_empty() => {
                    let pruned = results.iter().map(|result| result.pruned).sum::<u64>();
                    let skipped = results
                        .iter()
                        .map(|result| result.skipped_events)
                        .sum::<u64>();
                    tracing::info!(
                        projects = results.len(),
                        pruned,
                        skipped,
                        "GUI timeline retention sweep"
                    );
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "GUI timeline retention sweep failed"),
            }
        }
    });
}

/// Move stale effect `intent` rows to `unknown` after the project grace
/// period. The first tick runs immediately so a daemon restart cannot leave a
/// crash-between-intent-and-finish row silently retryable.
pub(crate) fn spawn_effect_sweeper(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_EFFECT_SWEEP_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30)
        .clamp(5, 3_600);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        loop {
            interval.tick().await;
            match engine.sweep_stale_effect_intents(100).await {
                Ok(0) => {}
                Ok(swept) => tracing::warn!(
                    swept,
                    "moved stale effect intents to unknown; reconcile before retrying or merging"
                ),
                Err(error) => tracing::warn!(%error, "effect intent sweep failed"),
            }
        }
    });
}

/// Expire open external gates whose deadline passed. Expiry is terminal and
/// wakes the watching continuation; it never resolves an approval or retries
/// an external action. The first tick runs immediately so a restart catches up.
pub(crate) fn spawn_gate_sweeper(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_GATE_SWEEP_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30);
    if seconds == 0 {
        return;
    }
    let seconds = seconds.clamp(5, 3_600);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        loop {
            interval.tick().await;
            match engine.sweep_expired_gates(100).await {
                Ok(0) => {}
                Ok(expired) => {
                    tracing::info!(
                        expired,
                        "expired external gates and woke their continuations"
                    );
                }
                Err(error) => tracing::warn!(%error, "external gate expiry sweep failed"),
            }
        }
    });
}

/// Assignment leases and inbox TTLs are project-scoped, so one bounded sweep
/// covers every project. It never reassigns an active identity and never
/// removes an inbox row; expiry is a status transition with one event.
pub(crate) fn spawn_coordination_sweeper(engine: Engine) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(30));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match engine.reconcile_session_agent_assignments().await {
                Ok(expired) if expired > 0 => {
                    tracing::info!(expired, "identity lease expiry sweep");
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "identity lease expiry sweep failed"),
            }
            match engine.sweep_agent_inbox_expiry().await {
                Ok(expired) if expired > 0 => {
                    tracing::info!(expired, "project inbox expiry sweep");
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "project inbox expiry sweep failed"),
            }
            match engine.sweep_agent_workspaces(Some(1_000)).await {
                Ok(released) if released > 0 => {
                    tracing::info!(released, "Worker workspace lease sweep");
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "Worker workspace lease sweep failed"),
            }
        }
    });
}

pub(crate) fn spawn_process_sweeper(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_PROCESS_SWEEP_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(60);
    if seconds == 0 {
        return;
    }
    let idle_enabled = std::env::var("VERLINPARK_PROCESS_IDLE_SWEEP_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|value| value > 0)
        .unwrap_or(true);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        interval.tick().await;
        loop {
            interval.tick().await;
            match engine.sweep_supervised_processes(idle_enabled).await {
                Ok(report)
                    if report.idle_stopped > 0
                        || report.pruned > 0
                        || report.lost > 0
                        || report.unknown > 0 =>
                {
                    tracing::info!(
                        idle_stopped = report.idle_stopped,
                        lost = report.lost,
                        unknown = report.unknown,
                        pruned = report.pruned,
                        "supervised-process sweep"
                    );
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "supervised-process sweep failed"),
            }
        }
    });
}

pub(crate) fn spawn_worktree_sweeper(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_WORKTREE_SWEEP_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(3600);
    if seconds == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        interval.tick().await;
        loop {
            interval.tick().await;
            match engine.sweep_worktree_retention().await {
                Ok(removed) if removed > 0 => {
                    tracing::info!(removed, "automatic worktree retention removed worktrees");
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(error = %error, "automatic worktree retention failed");
                }
            }
        }
    });
}

pub(crate) fn spawn_checkpoint_sweeper(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_CHECKPOINT_SWEEP_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(3600);
    if seconds == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        interval.tick().await;
        loop {
            interval.tick().await;
            match engine.sweep_checkpoint_retention().await {
                Ok(removed) if removed > 0 => {
                    tracing::info!(removed, "automatic checkpoint retention pruned checkpoints");
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "automatic checkpoint retention failed");
                }
            }
            match engine.sweep_context_checkpoint_retention().await {
                Ok(removed) if removed > 0 => {
                    tracing::info!(
                        removed,
                        "automatic context checkpoint retention pruned checkpoints"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "automatic context checkpoint retention failed");
                }
            }
        }
    });
}

pub(crate) fn spawn_tool_output_sweeper(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_TOOL_OUTPUT_SWEEP_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(3600);
    if seconds == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        interval.tick().await;
        loop {
            interval.tick().await;
            match engine.sweep_tool_output_retention().await {
                Ok(removed) if removed > 0 => {
                    tracing::info!(
                        removed,
                        "tool-output retention removed expired or excess handles"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "tool-output retention sweep failed");
                }
            }
        }
    });
}

pub(crate) fn spawn_terminal_sweeper(engine: Engine) {
    let idle_secs = std::env::var("VERLINPARK_TERMINAL_IDLE_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(3600);
    if idle_secs == 0 {
        return;
    }
    // Sweep several times per idle window, but keep the check cheap.
    let interval_secs = (idle_secs / 4).clamp(15, 300);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        interval.tick().await;
        loop {
            interval.tick().await;
            let (consoles, terminals) = engine
                .sweep_idle_terminals(Duration::from_secs(idle_secs))
                .await;
            if consoles + terminals > 0 {
                tracing::info!(
                    consoles,
                    terminals,
                    "closed idle console/terminal processes"
                );
            }
        }
    });
}

pub(crate) fn spawn_browser_sweeper(engine: Engine) {
    let idle = vp_core::browser::BrowserManager::idle_default();
    if idle.is_zero() {
        return;
    }
    let interval_secs = (idle.as_secs() / 4).clamp(15, 300);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        interval.tick().await;
        loop {
            interval.tick().await;
            let browsers = engine.sweep_idle_browsers(idle).await;
            if browsers > 0 {
                tracing::info!(browsers, "closed idle browser helpers");
            }
        }
    });
}

/// Release the logical GUI lease shortly after its grace expires. Helpers stay
/// warm for their own idle window; this sweeper only releases ownership so the
/// next session can take the GUI without waiting for the 120s lease TTL.
pub(crate) fn spawn_gui_grace_sweeper(engine: Engine) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval.tick().await;
        loop {
            interval.tick().await;
            let released = engine.sweep_gui_grace().await;
            if released > 0 {
                tracing::debug!(released, "released GUI lease after grace");
            }
        }
    });
}

pub(crate) fn spawn_computer_sweeper(engine: Engine) {
    let idle = vp_core::computer::ComputerManager::idle_default();
    if idle.is_zero() {
        return;
    }
    let interval_secs = (idle.as_secs() / 4).clamp(15, 300);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        interval.tick().await;
        loop {
            interval.tick().await;
            let computers = engine.sweep_idle_computers(idle).await;
            if computers > 0 {
                tracing::info!(computers, "closed idle computer helpers");
            }
        }
    });
}

/// Stall observation sweep. It always emits `orchestration.stall` events and
/// refreshes the observation window. When a project opts into `escalate` or
/// `retry`, the same call may record one bounded recovery transition; the
/// default `observe` mode remains telemetry-only.
pub(crate) fn spawn_task_stall_sweeper(engine: Engine) {
    let threshold_secs = std::env::var("VERLINPARK_TASK_STALL_SECS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(1_800);
    if threshold_secs == 0 {
        return;
    }
    let interval_secs = (threshold_secs / 4).clamp(1, 300);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        interval.tick().await;
        loop {
            interval.tick().await;
            match engine
                .sweep_task_stalls(Duration::from_secs(threshold_secs))
                .await
            {
                Ok(stalled) if stalled > 0 => {
                    tracing::warn!(stalled, threshold_secs, "plan task stall observed");
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "task stall sweep failed");
                }
            }
        }
    });
}

/// Advance pending stall-recovery requests separately from the task scheduler.
/// A request waits for cooperative cancellation and session quiescence, then
/// reuses the normal retry + plan-resume path at most once per attempt.
pub(crate) fn spawn_stall_recovery_reconciler(agent: AgentService) {
    let seconds = std::env::var("VERLINPARK_STALL_RECOVERY_TICK_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30);
    if seconds == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds.max(5)));
        interval.tick().await;
        loop {
            interval.tick().await;
            match agent.reconcile_stall_recoveries().await {
                Ok(advanced) if advanced > 0 => {
                    tracing::info!(advanced, "stall recovery reconciler advanced");
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "stall recovery reconciler failed");
                }
            }
        }
    });
}

/// Run the deterministic controller for sessions with failed or blocked plan
/// tasks. Task finishes already trigger a pass; this catches retry gates that
/// become due later and failures that survived a daemon restart.
pub(crate) fn spawn_orchestration_controller(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_ORCHESTRATION_TICK_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(20);
    if seconds == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds.max(5)));
        interval.tick().await;
        loop {
            interval.tick().await;
            let candidates = match engine.orchestration_candidates(100).await {
                Ok(candidates) => candidates,
                Err(error) => {
                    tracing::warn!(%error, "orchestration candidate scan failed");
                    continue;
                }
            };
            for session_id in candidates {
                match engine.advance_orchestration(&session_id).await {
                    Ok(actions) if !actions.is_empty() => {
                        tracing::info!(
                            %session_id,
                            actions = actions.len(),
                            "orchestration controller advanced a plan"
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(%error, %session_id, "orchestration controller pass failed");
                    }
                }
            }
            if let Err(error) = engine.sweep_runtime_leases().await {
                tracing::warn!(%error, "runtime lease sweep failed");
            }
            let audit_retention_days = std::env::var("VERLINPARK_LEASE_AUDIT_DAYS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(30);
            if let Err(error) = engine.prune_runtime_lease_audit(audit_retention_days).await {
                tracing::warn!(%error, "runtime lease audit prune failed");
            }
            if let Err(error) = engine.sweep_computer_leases().await {
                tracing::warn!(%error, "computer lease sweep failed");
            }
        }
    });
}

pub(crate) fn spawn_memory_sweeper(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_MEMORY_SWEEP_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(3600);
    if seconds == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        interval.tick().await;
        loop {
            interval.tick().await;
            match engine.sweep_memory_retention().await {
                Ok(removed) if removed > 0 => {
                    tracing::info!(removed, "automatic memory retention pruned memories");
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "automatic memory retention failed");
                }
            }
        }
    });
}

pub(crate) fn spawn_memory_stale_sweeper(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_MEMORY_STALE_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(3600);
    if seconds == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        interval.tick().await;
        loop {
            interval.tick().await;
            match engine.sweep_memory_staleness().await {
                Ok((projects, marked)) if marked > 0 => {
                    tracing::info!(
                        projects,
                        marked,
                        "automatic memory staleness sweep marked memories stale"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "automatic memory staleness sweep failed");
                }
            }
        }
    });
}

/// Bounded semantic indexing. One batch per enabled project per tick; the
/// reconciler performs no request when no project has an enabled embedding
/// policy and never touches session budgets.
pub(crate) fn spawn_memory_embedding_reconciler(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_MEMORY_EMBEDDING_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30);
    if seconds == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        interval.tick().await;
        loop {
            interval.tick().await;
            match engine.reconcile_memory_embeddings().await {
                Ok((_projects, indexed, failed, skipped))
                    if indexed > 0 || failed > 0 || skipped > 0 =>
                {
                    tracing::info!(
                        indexed,
                        failed,
                        skipped,
                        "memory embedding reconciler processed a batch"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "memory embedding reconciler failed");
                }
            }
        }
    });
}

/// Bounded semantic source indexing. One batch per enabled project per tick;
/// no request is made while no project opted in, and embedding failures never
/// touch source writes, lexical search, or session budgets.
pub(crate) fn spawn_source_embedding_reconciler(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_SOURCE_EMBEDDING_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(45);
    if seconds == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        interval.tick().await;
        loop {
            interval.tick().await;
            match engine.reconcile_source_embeddings().await {
                Ok((_projects, indexed, failed, done)) if indexed > 0 || failed > 0 || done > 0 => {
                    tracing::info!(
                        indexed,
                        failed,
                        done,
                        "source embedding reconciler processed a batch"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "source embedding reconciler failed");
                }
            }
        }
    });
}

/// Deterministic same-scope conflict detection. This tick never resolves a
/// conflict and never mutates memory status; operator resolution stays explicit.
pub(crate) fn spawn_memory_conflict_scanner(engine: Engine) {
    let seconds = std::env::var("VERLINPARK_MEMORY_CONFLICT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(3600);
    if seconds == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(seconds));
        interval.tick().await;
        loop {
            interval.tick().await;
            match engine.scan_all_memory_conflicts().await {
                Ok((projects, detected, open)) if detected > 0 => {
                    tracing::info!(
                        projects,
                        detected,
                        open,
                        "automatic memory conflict scan detected new conflicts"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "automatic memory conflict scan failed");
                }
            }
        }
    });
}
