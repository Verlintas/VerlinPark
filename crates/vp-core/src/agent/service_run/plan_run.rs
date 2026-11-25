use super::*;

impl AgentService {
    /// Dispatch exactly one ready task from a stored TeamPlan. The session is
    /// released after the task settles so the global scheduler can make fair
    /// progress across sessions instead of running an entire plan at once.
    pub async fn dispatch_next_task(&self, session_id: &str) -> Result<bool> {
        self.start_plan_run(session_id, Some(1)).await
    }

    pub(in crate::agent) async fn start_plan_run(
        &self,
        session_id: &str,
        max_tasks: Option<usize>,
    ) -> Result<bool> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned());
        let Some(config) = config else {
            anyhow::bail!("agent provider is not configured");
        };
        let (session, _) = load_session_project(self.engine.store().clone(), session_id).await?;
        if session.status == "imported" {
            anyhow::bail!(
                "imported sessions are read-only archives; rehydrate before resuming a plan"
            );
        }
        if matches!(
            session.status.as_str(),
            "queued" | "running" | "paused" | "landed"
        ) {
            anyhow::bail!("session must be idle, failed, cancelled, or draft to resume a plan");
        }
        let Some(plan) = self.engine.get_team_plan(session_id).await? else {
            anyhow::bail!("session has no TeamPlan");
        };
        let compatibility = self.engine.plan_compatibility(session_id).await?;
        if compatibility.status == vp_protocol::runtime::CompatibilityStatus::Incompatible {
            return Err(crate::runtime_pin::RuntimePinError::new(
                crate::runtime_pin::codes::RUNTIME_INCOMPATIBLE,
                format!(
                    "plan runtime identity is incompatible with the stored snapshot: {}",
                    compatibility.warnings.join("; ")
                ),
            )
            .with_report(compatibility)
            .into());
        }
        if compatibility.status == vp_protocol::runtime::CompatibilityStatus::Drift {
            self.engine
                .record_compatibility_drift(&compatibility)
                .await?;
        }
        let (plan_seq, scheduled) = self.engine.scheduler().sync_plan(&session, &plan).await?;
        let mut terminal = scheduled
            .iter()
            .filter(|task| matches!(task.status.as_str(), "completed" | "skipped" | "cancelled"))
            .map(|task| task.task_id.clone())
            .collect::<std::collections::HashSet<_>>();
        // Tasks absent from a just-synced queue are treated as pending.
        for task in &plan.tasks {
            if !scheduled.iter().any(|row| row.task_id == task.id) {
                terminal.remove(&task.id);
            }
        }
        if plan.tasks.iter().all(|task| terminal.contains(&task.id)) {
            anyhow::bail!("plan is already complete, skipped, or cancelled");
        }
        // Check the dispatch guard before emitting queued/plan-resume events or
        // taking a lease. The execute path keeps the same guard as a
        // defense-in-depth check, but a terminal resource block must not make
        // every scheduler cooldown append a full plan-resume cycle.
        match self
            .engine
            .resource_governor_decision(session_id, "dispatch")
            .await
        {
            Ok(decision) if decision.should_block() => {
                self.engine.record_resource_decision(&decision).await?;
                tracing::info!(
                    session_id,
                    action = %decision.action,
                    reason = %decision.reason,
                    "plan dispatch deferred by the resource governor"
                );
                return Ok(false);
            }
            Ok(_) => {}
            Err(error) => {
                // Advisory infrastructure: a read failure must not block an
                // otherwise valid resume. The execute path applies the same
                // policy if it can be read there.
                tracing::warn!(%error, session_id, "resource governor dispatch read failed");
            }
        }
        // The critical-path budget governor is also a dispatch gate. Evaluate
        // it before queue/lease events so a deferral is not re-emitted as a
        // full plan-resume cycle on every scheduler cooldown.
        let governor_budget = crate::budget_governor::GovernorBudgetView::new(
            session.token_budget,
            session.tokens_in.saturating_add(session.tokens_out),
            self.engine
                .budget_registry()
                .reserved_for_session(session_id),
        );
        let selection = self
            .engine
            .scheduler()
            .ready_tasks_governed(&session, plan_seq, &plan, 1, Some(governor_budget))
            .await?;
        if selection.ready.is_empty() && !selection.deferred.is_empty() {
            if let Some(admission) = selection.governor.as_ref() {
                self.engine
                    .record_budget_governor_admission(session_id, admission)
                    .await?;
            }
            tracing::info!(
                session_id,
                deferred = selection.deferred.len(),
                "plan dispatch deferred by the critical-path budget governor"
            );
            return Ok(false);
        }

        let control = Arc::new(RunControl::new());
        let preemption_policy = self
            .engine
            .get_preemption_policy(&session.project_id)
            .await?;
        arm_session_preemption(&self.engine, &session, &control, preemption_policy).await?;
        {
            let mut runs = self
                .runs
                .lock()
                .map_err(|_| anyhow!("agent run mutex poisoned"))?;
            if runs.contains_key(session_id) {
                return Err(anyhow!("session is already running"));
            }
            runs.insert(session_id.to_string(), control.clone());
        }
        let lease = match self
            .engine
            .acquire_session_lease(session_id, std::time::Duration::from_secs(180))
            .await
        {
            Ok(lease) => lease,
            Err(error) => {
                if let Ok(mut runs) = self.runs.lock() {
                    runs.remove(session_id);
                }
                return Err(error);
            }
        };

        let prepared: Result<()> = async {
            self.engine
                .update_session_status(session_id, "queued")
                .await?;
            let (event_kind, mut payload) = if max_tasks.is_some() {
                (
                    "agent.task_dispatch_queued",
                    serde_json::json!({
                        "goal": plan.goal,
                        "remaining_tasks": plan.tasks.len().saturating_sub(terminal.len()),
                        "task_limit": 1,
                    }),
                )
            } else {
                (
                    "agent.plan_resume_queued",
                    serde_json::json!({
                        "goal": plan.goal,
                        "remaining_tasks": plan.tasks.len().saturating_sub(terminal.len()),
                    }),
                )
            };
            if let Some(object) = payload.as_object_mut() {
                object.insert("task_limit".into(), serde_json::json!(max_tasks));
            }
            self.engine
                .append_event(session_id, event_kind, payload)
                .await?;
            Ok(())
        }
        .await;
        if let Err(error) = prepared {
            if let Ok(mut runs) = self.runs.lock() {
                runs.remove(session_id);
            }
            let _ = self
                .engine
                .update_session_status(session_id, "failed")
                .await;
            lease.release().await;
            return Err(error);
        }

        let engine = self.engine.clone();
        let session_id = session_id.to_string();
        let project_id = session.project_id.clone();
        let priority = session.priority;
        let runs = self.runs.clone();
        let global_slots = self.global_slots.clone();
        tokio::spawn(async move {
            control
                .preempt
                .set_phase(crate::preemption::Phase::Queue, None);
            let permit = tokio::select! {
                permit = global_slots.acquire(&session_id, &project_id, priority, &control) => permit,
                _ = wait_for_cancel(&control) => None,
            };
            let Some(_permit) = permit else {
                if let Some(snapshot) = control.preempt_report() {
                    let _ = engine
                        .append_event(
                            &session_id,
                            "agent.cancelled",
                            serde_json::json!({
                                "preempted": true,
                                "reason": snapshot.reason.as_str(),
                                "phase": snapshot.phase.as_str(),
                            }),
                        )
                        .await;
                    let _ = record_queued_preemption(
                        &engine,
                        &session_id,
                        &project_id,
                        &control,
                        &snapshot,
                    )
                    .await;
                } else {
                    let _ = engine
                        .append_event(&session_id, "agent.cancelled", serde_json::json!({}))
                        .await;
                    let _ = control
                        .finalize_session(&engine, &session_id, "cancelled")
                        .await;
                }
                if let Ok(mut runs) = runs.lock() {
                    runs.remove(&session_id);
                }
                lease.release().await;
                return;
            };
            if control.is_cancelled() {
                let _ = engine
                    .append_event(&session_id, "agent.cancelled", serde_json::json!({}))
                    .await;
                let _ = control
                    .finalize_session(&engine, &session_id, "cancelled")
                    .await;
                if let Ok(mut runs) = runs.lock() {
                    runs.remove(&session_id);
                }
                lease.release().await;
                return;
            }
            let status = if control.is_paused() {
                "paused"
            } else {
                "running"
            };
            let _ = engine.update_session_status(&session_id, status).await;
            let result = run_plan_resume(
                engine.clone(),
                config,
                &session_id,
                control.clone(),
                max_tasks,
            )
            .await;
            if let Err(error) = &result {
                let _ = engine
                    .append_event(
                        &session_id,
                        "agent.failed",
                        serde_json::json!({"error": format!("{error:#}")}),
                    )
                    .await;
                let _ = control
                    .finalize_session(&engine, &session_id, "failed")
                    .await;
            }
            if let Ok(mut runs) = runs.lock() {
                runs.remove(&session_id);
            }
            lease.release().await;
        });
        Ok(true)
    }
}
