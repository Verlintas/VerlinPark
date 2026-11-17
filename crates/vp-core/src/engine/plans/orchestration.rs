use super::*;

impl Engine {
    pub async fn coordination_notes(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<Vec<OrchestrationNote>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.list_coordination_notes(&session_id, limit))
            .await?
    }

    pub async fn orchestration_decisions(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<Vec<OrchestrationDecision>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.list_orchestration_decisions(&session_id, limit))
            .await?
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn record_coordination_note(
        &self,
        session_id: &str,
        action: &str,
        severity: &str,
        task_id: Option<&str>,
        target_agent_id: Option<&str>,
        target_note_id: Option<&str>,
        agent_id: &str,
        agent_name: &str,
        agent_role: Option<&str>,
        message: &str,
    ) -> Result<Event> {
        let store = self.store.clone();
        let session_id_owned = session_id.to_string();
        let action = action.to_string();
        let severity = severity.to_string();
        let task_id = task_id.map(str::to_string);
        let target_agent_id = target_agent_id.map(str::to_string);
        let target_note_id = target_note_id.map(str::to_string);
        let agent_id = agent_id.to_string();
        let agent_name = agent_name.to_string();
        let agent_role = agent_role.map(str::to_string);
        let message = message.to_string();
        let event = tokio::task::spawn_blocking(move || {
            store.record_coordination_note_with_event(
                &session_id_owned,
                &action,
                &severity,
                task_id.as_deref(),
                target_agent_id.as_deref(),
                target_note_id.as_deref(),
                &agent_id,
                &agent_name,
                agent_role.as_deref(),
                &message,
            )
        })
        .await??;
        let _ = self.events.send(event.clone());
        // A targeted note is also delivered to the durable mailbox so the
        // recipient still sees it after the recent-coordination window moves on.
        let target = event
            .payload
            .get("target_agent_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let note_action = event
            .payload
            .get("action")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("note");
        if let Some(target) = target
            && !matches!(note_action, "unblock" | "list")
        {
            let agents = self.list_agents(session_id).await?;
            if let Some(agent) = agents.iter().find(|agent| agent.id == target) {
                let _ = self
                    .send_agent_message(
                        session_id,
                        event
                            .payload
                            .get("agent_id")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("orchestrator"),
                        event
                            .payload
                            .get("agent_name")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("Orchestrator"),
                        &agent.id,
                        &agent.name,
                        "coordination",
                        event
                            .payload
                            .get("message")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or(""),
                        serde_json::json!({
                            "note_id": event.payload.get("note_id"),
                            "action": note_action,
                            "task_id": event.payload.get("task_id"),
                            "severity": event.payload.get("severity"),
                        }),
                    )
                    .await;
            }
        }
        Ok(event)
    }

    pub async fn coordination_notes_for_agent(
        &self,
        session_id: &str,
        agent_id: &str,
        task_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<OrchestrationNote>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let agent_id = agent_id.to_string();
        let task_id = task_id.map(str::to_string);
        tokio::task::spawn_blocking(move || {
            store.list_coordination_notes_for_agent(
                &session_id,
                &agent_id,
                task_id.as_deref(),
                limit,
            )
        })
        .await?
    }

    /// Read-only controller replay. This never calls a provider, executes a
    /// tool, appends an event, or mutates a projection.
    pub async fn replay_orchestration(
        &self,
        session_id: &str,
        limit: Option<u32>,
    ) -> Result<vp_protocol::replay::ControllerReplayReport> {
        crate::orchestration::replay::replay_orchestration(self, session_id, limit).await
    }

    pub async fn orchestration_snapshot(&self, session_id: &str) -> Result<OrchestrationSnapshot> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let plan = self.get_team_plan(session_id).await?;
        let queue = self.task_queue(None, Some(session_id.to_string())).await?;
        let tasks = queue.tasks;
        let progress = queue.progress;
        let store = self.store.clone();
        let id = session_id.to_string();
        let recoveries = tokio::task::spawn_blocking(move || {
            store.list_current_orchestration_recoveries(None, Some(&id))
        })
        .await??;
        let stall_threshold_secs = match task_stall_threshold_secs() {
            0 => None,
            secs => Some(secs),
        };
        let agents = self.list_agents(session_id).await?;
        let merge_queue = self.list_merge_queue(session_id).await?;
        let questions = self
            .list_questions(Some(session_id.to_string()))
            .await?
            .questions;
        let events = self.recent_events(session_id, 5_000).await?;
        let notes = self.coordination_notes(session_id, 2_000).await?;
        let decisions = self.orchestration_decisions(session_id, 2_000).await?;
        let policy = self.orchestration_policy(&session.project_id).await?;
        Ok(crate::orchestration::build_snapshot(
            crate::orchestration::OrchestrationInput {
                session: &session,
                plan: plan.as_ref(),
                scheduled: &tasks,
                agents: &agents,
                events: &events,
                merge_queue: &merge_queue,
                questions: &questions,
                notes: &notes,
                decisions: &decisions,
                progress: &progress,
                recoveries: &recoveries,
                stall_threshold_secs,
                policy: &policy,
            },
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn record_user_coordination_note(
        &self,
        session_id: &str,
        action: &str,
        task_id: Option<&str>,
        target_agent_id: Option<&str>,
        target_note_id: Option<&str>,
        message: &str,
        severity: Option<&str>,
    ) -> Result<String> {
        if !matches!(
            action,
            "note" | "blocker" | "unblock" | "decision" | "escalate"
        ) {
            anyhow::bail!("unsupported coordination action: {action}");
        }
        let message = message.trim();
        if message.is_empty() || message.chars().count() > 2_000 {
            anyhow::bail!("coordination message must be 1-2000 chars");
        }
        let task_id = task_id.map(str::trim).filter(|task| !task.is_empty());
        if let Some(task_id) = task_id {
            crate::worktree::validate_task_id(task_id)?;
        }
        let severity = severity.unwrap_or(if matches!(action, "blocker" | "escalate") {
            "warning"
        } else {
            "info"
        });
        if !matches!(severity, "info" | "warning" | "critical") {
            anyhow::bail!("severity must be info, warning, or critical");
        }
        if let Some(target) = target_agent_id {
            let agents = self.list_agents(session_id).await?;
            if !agents.iter().any(|agent| agent.id == target) {
                anyhow::bail!("target agent is not in this session: {target}");
            }
        }
        if let Some(note) = target_note_id {
            let notes = self.coordination_notes(session_id, 2_000).await?;
            if !notes.iter().any(|candidate| candidate.note_id == note) {
                anyhow::bail!("coordination note not found: {note}");
            }
        }
        let event = self
            .record_coordination_note(
                session_id,
                action,
                severity,
                task_id,
                target_agent_id,
                target_note_id,
                "user",
                "User",
                Some("user"),
                message,
            )
            .await?;
        Ok(event
            .payload
            .get("note_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// Recent TeamPlan sessions across the selected project (or all projects)
    /// with their live metrics and top blockers. The daemon exposes this for
    /// Manage Dash; the per-session snapshot remains the source of truth.
    pub async fn orchestration_overview(
        &self,
        project_id: Option<&str>,
        limit: u32,
    ) -> Result<OrchestrationOverviewResult> {
        let sessions = self.list_sessions(project_id.map(str::to_string)).await?;
        let projects = self.list_projects().await?;
        let project_names = projects
            .into_iter()
            .map(|project| (project.id, project.name))
            .collect::<HashMap<_, _>>();
        let limit = limit.clamp(1, 50) as usize;
        let mut summaries = Vec::new();
        for session in sessions.into_iter().take(limit.saturating_mul(4)) {
            let Ok(snapshot) = self.orchestration_snapshot(&session.id).await else {
                continue;
            };
            if snapshot.plan.is_none() {
                continue;
            }
            let blockers = snapshot
                .blockers
                .iter()
                .take(3)
                .map(|blocker| format!("{}: {}", blocker.kind, blocker.message))
                .collect::<Vec<_>>();
            let last = snapshot.decisions.last().cloned();
            let active_recoveries = snapshot
                .recoveries
                .iter()
                .filter(|recovery| {
                    matches!(recovery.status.as_str(), "requested" | "waiting_cancel")
                })
                .count();
            let failed_recoveries = snapshot
                .recoveries
                .iter()
                .filter(|recovery| recovery.status == "failed")
                .count();
            let last_recovery = snapshot
                .recoveries
                .iter()
                .max_by(|left, right| left.updated_at.cmp(&right.updated_at))
                .map(|recovery| {
                    format!(
                        "{}: {} ({})",
                        recovery.task_id,
                        recovery.status,
                        recovery
                            .last_error
                            .as_deref()
                            .unwrap_or(recovery.action.as_str())
                    )
                });
            summaries.push(OrchestrationSessionSummary {
                session_id: session.id.clone(),
                project_id: session.project_id.clone(),
                project_name: project_names
                    .get(&session.project_id)
                    .cloned()
                    .unwrap_or_else(|| session.project_id.clone()),
                goal: session.goal.clone(),
                status: session.status.clone(),
                priority: session.priority,
                plan_revision: snapshot.plan_revision,
                metrics: snapshot.metrics.clone(),
                blockers,
                last_decision: last.as_ref().map(|decision| decision.detail.clone()),
                last_decision_at: last.map(|decision| decision.created_at),
                recoveries: snapshot.recoveries.len(),
                active_recoveries,
                failed_recoveries,
                last_recovery,
            });
        }
        summaries.sort_by(|lhs, rhs| {
            let left_attention = lhs.metrics.attention_count;
            let right_attention = rhs.metrics.attention_count;
            if left_attention != right_attention {
                return right_attention.cmp(&left_attention);
            }
            let left_running = lhs.status == "running" || lhs.status == "queued";
            let right_running = rhs.status == "running" || rhs.status == "queued";
            if left_running != right_running {
                return right_running.cmp(&left_running);
            }
            rhs.priority.cmp(&lhs.priority)
        });
        summaries.truncate(limit);
        let totals = OrchestrationOverviewTotals {
            sessions: summaries.len(),
            running: summaries
                .iter()
                .filter(|summary| matches!(summary.status.as_str(), "running" | "queued"))
                .count(),
            attention: summaries
                .iter()
                .filter(|summary| summary.metrics.attention_count > 0)
                .count(),
            blocked_tasks: summaries
                .iter()
                .map(|summary| summary.metrics.blocked_tasks)
                .sum(),
            failed_tasks: summaries
                .iter()
                .map(|summary| summary.metrics.failed_tasks)
                .sum(),
            retrying_tasks: summaries
                .iter()
                .map(|summary| summary.metrics.retrying_tasks)
                .sum(),
            open_blockers: summaries
                .iter()
                .map(|summary| summary.metrics.open_blockers)
                .sum(),
            merge_conflicts: summaries
                .iter()
                .map(|summary| summary.metrics.merge_conflicts)
                .sum(),
            stalled_tasks: summaries
                .iter()
                .map(|summary| summary.metrics.stalled_tasks)
                .sum(),
            recoveries: summaries.iter().map(|summary| summary.recoveries).sum(),
            active_recoveries: summaries
                .iter()
                .map(|summary| summary.active_recoveries)
                .sum(),
            failed_recoveries: summaries
                .iter()
                .map(|summary| summary.failed_recoveries)
                .sum(),
        };
        Ok(OrchestrationOverviewResult {
            sessions: summaries,
            totals,
            generated_at: vp_protocol::now_rfc3339(),
        })
    }

    /// Preview the controller decisions without mutating task rows or notes.
    pub async fn preview_orchestration(
        &self,
        session_id: &str,
    ) -> Result<OrchestrationPreviewResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let Some(plan) = self.get_team_plan(session_id).await? else {
            return Ok(OrchestrationPreviewResult {
                session_id: session_id.to_string(),
                actions: Vec::new(),
            });
        };
        let tasks = self
            .task_queue(None, Some(session_id.to_string()))
            .await?
            .tasks;
        let events = self.recent_events(session_id, 5_000).await?;
        let notes = self.coordination_notes(session_id, 2_000).await?;
        let policy = self.orchestration_policy(&session.project_id).await?;
        let actions = crate::orchestration::plan_actions(crate::orchestration::ControllerInput {
            session: &session,
            plan: &plan,
            scheduled: &tasks,
            events: &events,
            notes: &notes,
            policy: &policy,
            budget_exhausted: session_budget_exhausted(&session),
        });
        Ok(OrchestrationPreviewResult {
            session_id: session_id.to_string(),
            actions: actions
                .into_iter()
                .map(|action| OrchestrationPreviewAction {
                    action: action.action().to_string(),
                    task_id: action.task_id().to_string(),
                    detail: action.detail(),
                    attempt: action.attempt(),
                })
                .collect(),
        })
    }

    /// Run one deterministic controller pass for a session. Actions are
    /// idempotent: retries are gated by `next_attempt_at`, blocked rows are
    /// skipped once moved, and escalations are deduplicated by attempt.
    pub async fn advance_orchestration(
        &self,
        session_id: &str,
    ) -> Result<Vec<OrchestrationDecision>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let Some(plan) = self.get_team_plan(session_id).await? else {
            return Ok(Vec::new());
        };
        let tasks = self
            .task_queue(None, Some(session_id.to_string()))
            .await?
            .tasks;
        let events = self.recent_events(session_id, 5_000).await?;
        let notes = self.coordination_notes(session_id, 2_000).await?;
        let policy = self.orchestration_policy(&session.project_id).await?;
        let actions = crate::orchestration::plan_actions(crate::orchestration::ControllerInput {
            session: &session,
            plan: &plan,
            scheduled: &tasks,
            events: &events,
            notes: &notes,
            policy: &policy,
            budget_exhausted: session_budget_exhausted(&session),
        });
        let mut decisions = Vec::new();
        for action in actions {
            let task_id = action.task_id().to_string();
            let Some(row) = tasks.iter().find(|row| row.task_id == task_id) else {
                continue;
            };
            let plan_seq = row.plan_seq;
            match &action {
                crate::orchestration::ControllerAction::Retry {
                    task_id,
                    delay_secs,
                    reason,
                } => {
                    if let Err(error) = self
                        .schedule_plan_task_retry(
                            session_id,
                            plan_seq,
                            task_id,
                            std::time::Duration::from_secs(*delay_secs),
                            reason,
                        )
                        .await
                    {
                        tracing::warn!(%error, task_id, "orchestration retry failed");
                        continue;
                    }
                }
                crate::orchestration::ControllerAction::Block {
                    task_id,
                    dependency,
                    reason,
                } => {
                    let store = self.store.clone();
                    let session_id_owned = session_id.to_string();
                    let task_id_owned = task_id.clone();
                    let dependency = dependency.clone();
                    let reason = reason.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        store.block_plan_task_with_event(
                            &session_id_owned,
                            plan_seq,
                            &task_id_owned,
                            &reason,
                            &dependency,
                        )
                    })
                    .await??;
                    let Some((_task, event)) = result else {
                        continue;
                    };
                    let _ = self.events.send(event);
                }
                crate::orchestration::ControllerAction::Unblock { task_id, reason } => {
                    let store = self.store.clone();
                    let session_id_owned = session_id.to_string();
                    let task_id_owned = task_id.clone();
                    let reason = reason.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        store.unblock_plan_task_with_event(
                            &session_id_owned,
                            plan_seq,
                            &task_id_owned,
                            &reason,
                        )
                    })
                    .await??;
                    let Some((_task, event)) = result else {
                        continue;
                    };
                    let _ = self.events.send(event);
                }
                crate::orchestration::ControllerAction::Cancel {
                    task_id,
                    dependency,
                    reason,
                } => {
                    let store = self.store.clone();
                    let session_id_owned = session_id.to_string();
                    let task_id_owned = task_id.clone();
                    let dependency = dependency.clone();
                    let reason = reason.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        store.cancel_plan_task_with_event(
                            &session_id_owned,
                            plan_seq,
                            &task_id_owned,
                            &reason,
                            &dependency,
                        )
                    })
                    .await??;
                    let Some((_task, event)) = result else {
                        continue;
                    };
                    let _ = self.events.send(event);
                }
                crate::orchestration::ControllerAction::Escalate {
                    task_id,
                    attempt: _,
                    message,
                } => {
                    if let Err(error) = self
                        .record_coordination_note(
                            session_id,
                            "escalate",
                            "critical",
                            Some(task_id),
                            None,
                            None,
                            "orchestrator",
                            "Orchestrator",
                            Some("orchestrator"),
                            message,
                        )
                        .await
                    {
                        tracing::warn!(%error, task_id, "orchestration escalation failed");
                        continue;
                    }
                }
                crate::orchestration::ControllerAction::ResolveBlockers { task_id, count } => {
                    if let Err(error) = self
                        .record_coordination_note(
                            session_id,
                            "unblock",
                            "info",
                            Some(task_id),
                            None,
                            None,
                            "orchestrator",
                            "Orchestrator",
                            Some("orchestrator"),
                            &format!(
                                "{task_id} completed; {count} coordination blocker(s) resolved automatically"
                            ),
                        )
                        .await
                    {
                        tracing::warn!(%error, task_id, "orchestration blocker resolution failed");
                        continue;
                    }
                }
            }
            decisions.push(
                self.record_orchestration_decision(
                    session_id,
                    action.action(),
                    Some(action.task_id()),
                    &action.detail(),
                    action.attempt(),
                )
                .await?,
            );
        }
        Ok(decisions)
    }

    pub async fn record_orchestration_decision(
        &self,
        session_id: &str,
        action: &str,
        task_id: Option<&str>,
        detail: &str,
        attempt: u32,
    ) -> Result<OrchestrationDecision> {
        let store = self.store.clone();
        let session_id_owned = session_id.to_string();
        let action_owned = action.to_string();
        let task_id_owned = task_id.map(str::to_string);
        let detail_owned = detail.to_string();
        let event = tokio::task::spawn_blocking(move || {
            store.record_orchestration_decision_with_event(
                &session_id_owned,
                &action_owned,
                task_id_owned.as_deref(),
                &detail_owned,
                attempt,
                "orchestrator",
            )
        })
        .await??;
        let _ = self.events.send(event.clone());
        Ok(OrchestrationDecision {
            decision_id: event.payload["decision_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            action: action.to_string(),
            task_id: task_id.map(str::to_string),
            detail: detail.to_string(),
            attempt,
            agent_id: "orchestrator".to_string(),
            created_at: event.created_at,
        })
    }

    pub async fn schedule_plan_task_retry(
        &self,
        session_id: &str,
        plan_seq: u64,
        task_id: &str,
        delay: std::time::Duration,
        reason: &str,
    ) -> Result<ScheduledTask> {
        let next = time::OffsetDateTime::now_utc()
            + time::Duration::seconds(delay.as_secs().min(i64::MAX as u64) as i64);
        let next = next
            .format(&time::format_description::well_known::Rfc3339)
            .context("formatting retry time")?;
        let store = self.store.clone();
        let session_id_owned = session_id.to_string();
        let task_id_owned = task_id.to_string();
        let reason_owned = reason.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.schedule_plan_task_retry_with_event(
                &session_id_owned,
                plan_seq,
                &task_id_owned,
                &next,
                &reason_owned,
            )
        })
        .await??;
        let (task, event) =
            result.ok_or_else(|| anyhow::anyhow!("task {task_id} cannot be retried"))?;
        let _ = self.events.send(event);
        self.scheduler.notify_state();
        Ok(task)
    }

    pub async fn write_leases(
        &self,
        project_id: Option<String>,
        session_id: Option<String>,
    ) -> Result<Vec<WriteLease>> {
        self.scheduler.leases(project_id, session_id).await
    }
}
