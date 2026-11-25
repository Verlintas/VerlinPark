use super::*;

impl AgentService {
    /// Dispatch a standalone intake item immediately. Manual dispatch is a
    /// deliberate user action, so it bypasses the project's schedule toggle
    /// and parallel cap but still atomically claims the row and uses the same
    /// one-task plan, Verifier, and Integrator path as every other Worker.
    pub async fn dispatch_intake_task(&self, id: &str) -> Result<IntakeDispatchResult> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned())
            .ok_or_else(|| anyhow!("agent provider is not configured"))?;
        let Some(mut task) = self.engine.get_intake_task(id.to_string()).await? else {
            anyhow::bail!("intake task not found: {id}");
        };
        if !task.deps.is_empty() {
            let peers = self
                .engine
                .list_intake_tasks(Some(task.project_id.clone()), None, None)
                .await?
                .tasks;
            for dep in &task.deps {
                match peers.iter().find(|row| &row.id == dep) {
                    Some(row) if matches!(row.status.as_str(), "completed" | "skipped") => {}
                    Some(row) => anyhow::bail!(
                        "intake task {id} is waiting for dependency {} ({})",
                        row.title,
                        row.status
                    ),
                    None => anyhow::bail!("intake task {id} has unknown dependency {dep}"),
                }
            }
        }
        if !matches!(
            task.status.as_str(),
            "queued" | "failed" | "interrupted" | "cancelled"
        ) {
            anyhow::bail!("intake task {id} is not runnable (status: {})", task.status);
        }
        if task.status == "cancelled"
            || (task.attempts >= task.max_attempts
                && matches!(task.status.as_str(), "failed" | "interrupted"))
        {
            task = self
                .engine
                .retry_intake_task(task.id.clone())
                .await?
                .ok_or_else(|| anyhow!("intake task {id} could not be reset for a manual run"))?;
        }
        let Some(claimed) = self.engine.claim_intake_task(task.id.clone()).await? else {
            anyhow::bail!("intake task {id} was claimed by another dispatcher");
        };
        let plan = self.engine.intake_plan(&claimed);
        if let Err(error) = validate_team_plan(&plan, config.max_managers) {
            let next = self.intake_retry_at(&claimed).await;
            let _ = self
                .engine
                .settle_intake_task(
                    claimed.id.clone(),
                    "failed".to_string(),
                    None,
                    Some(format!("plan validation failed: {error:#}")),
                    next,
                )
                .await;
            return Err(error);
        }
        let goal = format!(
            "{} · {}",
            claimed.title,
            claimed
                .instructions
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("")
                .trim()
                .chars()
                .take(180)
                .collect::<String>()
        );
        let session = match self
            .engine
            .create_session_with_options(
                claimed.project_id.clone(),
                goal,
                claimed.token_budget,
                claimed.cost_budget_usd,
                claimed.time_budget_secs,
                claimed.priority,
            )
            .await
        {
            Ok(session) => session,
            Err(error) => {
                let next = self.intake_retry_at(&claimed).await;
                self.engine
                    .settle_intake_task(
                        claimed.id.clone(),
                        "failed".to_string(),
                        None,
                        Some(format!("could not create session: {error:#}")),
                        next,
                    )
                    .await?;
                return Err(error);
            }
        };
        let setup = async {
            validate_team_plan(&plan, config.max_managers)?;
            self.engine.set_team_plan(&session.id, &plan).await?;
            self.engine
                .start_intake_task(claimed.id.clone(), session.id.clone(), "T1".to_string())
                .await?;
            self.engine
                .append_event(
                    &session.id,
                    "intake.dispatched",
                    serde_json::json!({
                        "intake_task_id": claimed.id,
                        "title": claimed.title,
                        "profile": claimed.profile,
                        "priority": claimed.priority,
                        "attempt": claimed.attempts,
                    }),
                )
                .await?;
            Ok::<(), anyhow::Error>(())
        }
        .await;
        if let Err(error) = setup {
            let next = self.intake_retry_at(&claimed).await;
            let _ = self
                .engine
                .update_session_status(&session.id, "failed")
                .await;
            let _ = self
                .engine
                .append_event(
                    &session.id,
                    "intake.dispatch_failed",
                    serde_json::json!({
                        "intake_task_id": claimed.id,
                        "error": format!("{error:#}"),
                    }),
                )
                .await;
            let _ = self
                .engine
                .settle_intake_task(
                    claimed.id.clone(),
                    "failed".to_string(),
                    None,
                    Some(format!("could not prepare the task plan: {error:#}")),
                    next,
                )
                .await;
            return Err(error);
        }
        if let Err(error) = self.resume_plan(&session.id).await {
            let next = self.intake_retry_at(&claimed).await;
            let _ = self
                .engine
                .update_session_status(&session.id, "failed")
                .await;
            let _ = self
                .engine
                .append_event(
                    &session.id,
                    "intake.dispatch_failed",
                    serde_json::json!({
                        "intake_task_id": claimed.id,
                        "error": format!("{error:#}"),
                    }),
                )
                .await;
            let updated = self
                .engine
                .settle_intake_task(
                    claimed.id.clone(),
                    "failed".to_string(),
                    None,
                    Some(format!("could not start the task run: {error:#}")),
                    next,
                )
                .await?;
            return Ok(IntakeDispatchResult {
                started: false,
                task: updated.unwrap_or(claimed),
                session_id: Some(session.id),
            });
        }
        let updated = self.engine.get_intake_task(claimed.id.clone()).await?;
        Ok(IntakeDispatchResult {
            started: true,
            task: updated.unwrap_or(claimed),
            session_id: Some(session.id),
        })
    }

    /// Settle intake rows whose session reached a terminal plan status. This
    /// runs on every scheduler tick, so a daemon crash or a provider failure
    /// never leaves an intake item stuck in `running`.
    pub async fn reconcile_intake_tasks(&self) -> Result<u32> {
        let dependency_blocked = self.reconcile_intake_dependencies().await?;
        let tasks = self
            .engine
            .list_intake_tasks(None, Some("running".to_string()), Some(1_000))
            .await?
            .tasks;
        let mut settled = dependency_blocked;
        for task in tasks {
            let Some(session_id) = task.session_id.clone() else {
                let next = self.intake_retry_at(&task).await;
                let _ = self
                    .engine
                    .settle_intake_task(
                        task.id.clone(),
                        "failed".to_string(),
                        None,
                        Some("session id was not recorded".to_string()),
                        next,
                    )
                    .await;
                settled += 1;
                continue;
            };
            let (session, _) =
                match load_session_project(self.engine.store().clone(), &session_id).await {
                    Ok(value) => value,
                    Err(_) => {
                        let next = self.intake_retry_at(&task).await;
                        let _ = self
                            .engine
                            .settle_intake_task(
                                task.id.clone(),
                                "failed".to_string(),
                                None,
                                Some("the linked session no longer exists".to_string()),
                                next,
                            )
                            .await;
                        settled += 1;
                        continue;
                    }
                };
            let queue = self
                .engine
                .task_queue(None, Some(session.id.clone()))
                .await?;
            let row = queue
                .tasks
                .iter()
                .find(|row| row.task_id == task.task_id.clone().unwrap_or_default());
            let summary = latest_plan_results(&self.engine, &session.id)
                .await
                .ok()
                .and_then(|results| {
                    task.task_id
                        .as_ref()
                        .and_then(|task_id| results.get(task_id).cloned())
                });
            let terminal: Option<(&str, Option<String>, Option<String>)> =
                match row.map(|row| row.status.as_str()) {
                    Some("completed") | Some("skipped") => Some((
                        "completed",
                        Some(summary.unwrap_or_else(|| "completed".into())),
                        None,
                    )),
                    Some("failed") | Some("blocked") | Some("interrupted") => {
                        let detail = row.and_then(|row| row.last_error.clone());
                        Some((
                            "failed",
                            detail
                                .clone()
                                .or_else(|| summary.clone())
                                .or(Some("the worker did not complete".into())),
                            Some(detail.unwrap_or_else(|| "the worker did not complete".into())),
                        ))
                    }
                    Some("cancelled") => Some(("cancelled", summary, None)),
                    Some("pending") | Some("running") => match session.status.as_str() {
                        "idle" | "draft" | "landed" | "imported" => Some((
                            "failed",
                            Some("the session ended before the task settled".into()),
                            Some("the session ended before the task settled".into()),
                        )),
                        _ => None,
                    },
                    _ => match session.status.as_str() {
                        "failed" => Some((
                            "failed",
                            Some("the session failed before the task settled".into()),
                            Some("the session failed before the task settled".into()),
                        )),
                        "cancelled" => Some(("cancelled", summary, None)),
                        "idle" | "draft" | "landed" | "imported" => Some((
                            "failed",
                            Some("the plan task was not found".into()),
                            Some("the plan task was not found".into()),
                        )),
                        _ => None,
                    },
                };
            let Some((status, result_summary, last_error)) = terminal else {
                continue;
            };
            let next = if status == "failed" {
                self.intake_retry_at(&task).await
            } else {
                None
            };
            let blocked_next = next.clone();
            let _ = self
                .engine
                .settle_intake_task(
                    task.id.clone(),
                    status.to_string(),
                    result_summary,
                    last_error,
                    next,
                )
                .await?;
            if status == "cancelled" || (status == "failed" && blocked_next.is_none()) {
                self.block_intake_dependents(
                    &task.project_id,
                    &task.id,
                    &format!("dependency \"{}\" is {status}", task.title),
                )
                .await;
            }
            let kind = match status {
                "completed" => "intake.completed",
                "cancelled" => "intake.cancelled",
                _ => "intake.failed",
            };
            let _ = self
                .engine
                .append_event(
                    &session.id,
                    kind,
                    serde_json::json!({
                        "intake_task_id": task.id,
                        "task_id": task.task_id,
                        "status": status,
                        "attempt": task.attempts,
                    }),
                )
                .await;
            settled += 1;
        }
        Ok(settled)
    }

    /// Read-only scheduler observability for the dashboard and CLI. Counts
    /// are derived from the persisted projections; this never changes state.
    pub async fn scheduler_status(
        &self,
        project_id: Option<String>,
    ) -> Result<SchedulerStatusResult> {
        let sessions = self.engine.list_sessions(project_id.clone()).await?;
        let tasks = self
            .engine
            .task_queue(project_id.clone(), None)
            .await?
            .tasks;
        let intake = self
            .engine
            .list_intake_tasks(project_id.clone(), None, Some(2_000))
            .await?
            .tasks;
        let all_projects = self.engine.list_projects().await?;
        let projects = all_projects
            .into_iter()
            .filter(|project| project_id.as_deref().is_none_or(|id| id == project.id))
            .collect::<Vec<_>>();

        let session_is_runnable =
            |session: &Session| matches!(session.status.as_str(), "idle" | "failed" | "cancelled");
        let task_is_retryable =
            |status: &str| matches!(status, "pending" | "interrupted" | "failed" | "cancelled");
        let task_is_running =
            |status: &str| matches!(status, "running" | "claimed" | "pausing" | "paused");
        let mut pending_sessions = std::collections::HashSet::new();
        for task in &tasks {
            if !task_is_retryable(&task.status) {
                continue;
            }
            if sessions
                .iter()
                .find(|session| session.id == task.session_id)
                .is_some_and(session_is_runnable)
            {
                pending_sessions.insert(task.session_id.clone());
            }
        }
        let active_sessions = sessions
            .iter()
            .filter(|session| {
                matches!(
                    session.status.as_str(),
                    "running" | "paused" | "waiting_input"
                )
            })
            .count();
        let queued_sessions = sessions
            .iter()
            .filter(|session| session.status == "queued")
            .count();
        let paused_sessions = sessions
            .iter()
            .filter(|session| session.status == "paused")
            .count();
        let pending_plan_tasks = tasks
            .iter()
            .filter(|task| task_is_retryable(&task.status))
            .count();
        let running_plan_tasks = tasks
            .iter()
            .filter(|task| task_is_running(&task.status))
            .count();
        let queued_intake = intake.iter().filter(|task| task.status == "queued").count();
        let active_intake = intake
            .iter()
            .filter(|task| task.status == "running" || task.status == "dispatching")
            .count();
        let failed_intake = intake
            .iter()
            .filter(|task| task.status == "failed" || task.status == "interrupted")
            .count();

        let mut project_statuses = Vec::new();
        for project in projects {
            let policy = self
                .engine
                .get_task_scheduler_policy(&project.id)
                .await?
                .policy;
            let project_tasks = tasks
                .iter()
                .filter(|task| task.project_id == project.id)
                .collect::<Vec<_>>();
            let project_intake = intake
                .iter()
                .filter(|task| task.project_id == project.id)
                .collect::<Vec<_>>();
            let mut pending = std::collections::HashSet::new();
            for task in &project_tasks {
                if task_is_retryable(&task.status)
                    && sessions
                        .iter()
                        .find(|session| session.id == task.session_id)
                        .is_some_and(session_is_runnable)
                {
                    pending.insert(task.session_id.clone());
                }
            }
            project_statuses.push(SchedulerProjectStatus {
                project_id: project.id.clone(),
                project_name: project.name.clone(),
                enabled: policy.enabled,
                cooldown_secs: policy.cooldown_secs,
                dispatch_tasks: policy.dispatch_tasks,
                include_intake: policy.include_intake,
                max_intake_parallel: policy.max_intake_parallel,
                pending_sessions: pending.len(),
                pending_plan_tasks: project_tasks
                    .iter()
                    .filter(|task| task_is_retryable(&task.status))
                    .count(),
                running_plan_tasks: project_tasks
                    .iter()
                    .filter(|task| task_is_running(&task.status))
                    .count(),
                queued_intake: project_intake
                    .iter()
                    .filter(|task| task.status == "queued")
                    .count(),
                active_intake: project_intake
                    .iter()
                    .filter(|task| task.status == "running" || task.status == "dispatching")
                    .count(),
                failed_intake: project_intake
                    .iter()
                    .filter(|task| task.status == "failed" || task.status == "interrupted")
                    .count(),
            });
        }

        Ok(SchedulerStatusResult {
            active_sessions,
            queued_sessions,
            paused_sessions,
            pending_plan_tasks,
            running_plan_tasks,
            queued_intake,
            active_intake,
            failed_intake,
            global_session_cap: global_session_cap(),
            global_worker_cap: global_worker_cap(),
            global_labour_cap: global_labour_cap(),
            projects: project_statuses,
        })
    }

    /// Block queued dependents whose dependency can no longer make progress.
    /// This also repairs rows after a daemon restart, where recovery marked a
    /// running intake task failed without touching its dependents.
    pub(in crate::agent) async fn reconcile_intake_dependencies(&self) -> Result<u32> {
        let tasks = self.engine.list_intake_tasks(None, None, None).await?.tasks;
        let by_id = tasks
            .iter()
            .map(|task| (task.id.clone(), task))
            .collect::<HashMap<_, _>>();
        let mut policies = HashMap::new();
        let mut blocked = 0;
        for task in tasks.iter().filter(|task| task.status == "queued") {
            if task.deps.is_empty() {
                continue;
            }
            if !policies.contains_key(&task.project_id) {
                let policy = self
                    .engine
                    .orchestration_policy(&task.project_id)
                    .await
                    .unwrap_or_default();
                policies.insert(task.project_id.clone(), policy);
            }
            let policy = policies.get(&task.project_id).cloned().unwrap_or_default();
            let mut waiting = false;
            let mut failure = None;
            for dep in &task.deps {
                let Some(dependency) = by_id.get(dep) else {
                    failure = Some(format!("dependency {dep} no longer exists"));
                    break;
                };
                match dependency.status.as_str() {
                    "completed" | "skipped" => {}
                    "failed" | "interrupted"
                        if dependency.next_attempt_at.is_some()
                            || (policy.auto_retry_failed
                                && dependency.attempts < dependency.max_attempts) =>
                    {
                        // A retry is scheduled or still available; keep
                        // waiting, but continue scanning for a permanent
                        // failure in a sibling dependency.
                        waiting = true;
                    }
                    "failed" | "interrupted" | "cancelled" | "blocked" => {
                        failure = Some(format!(
                            "dependency \"{}\" is {}",
                            dependency.title, dependency.status
                        ));
                        break;
                    }
                    _ => {
                        // Dispatching/running/queued dependency: wait.
                        waiting = true;
                    }
                }
            }
            if waiting && failure.is_none() {
                continue;
            }
            let Some(reason) = failure else {
                continue;
            };
            self.engine
                .settle_intake_task(
                    task.id.clone(),
                    "blocked".to_string(),
                    None,
                    Some(format!("blocked: {reason}")),
                    None,
                )
                .await?;
            blocked += 1;
        }
        Ok(blocked)
    }

    pub async fn cancel_intake_task(&self, id: &str) -> Result<IntakeTask> {
        let Some(task) = self.engine.get_intake_task(id.to_string()).await? else {
            anyhow::bail!("intake task not found: {id}");
        };
        match task.status.as_str() {
            "running" | "dispatching" => {
                if let Some(session_id) = task.session_id.as_deref() {
                    let _ = self.cancel(session_id);
                    let _ = self
                        .engine
                        .append_event(
                            session_id,
                            "intake.cancelled",
                            serde_json::json!({
                                "intake_task_id": task.id,
                                "reason": "cancelled by user",
                            }),
                        )
                        .await;
                }
                let cancelled = self
                    .engine
                    .settle_intake_task(
                        task.id.clone(),
                        "cancelled".to_string(),
                        None,
                        Some("cancelled by user".to_string()),
                        None,
                    )
                    .await?
                    .ok_or_else(|| anyhow!("intake task disappeared while cancelling"))?;
                self.block_intake_dependents(
                    &cancelled.project_id,
                    &cancelled.id,
                    &format!("dependency \"{}\" was cancelled", cancelled.title),
                )
                .await;
                Ok(cancelled)
            }
            "queued" | "failed" | "interrupted" | "blocked" => {
                let cancelled = self
                    .engine
                    .cancel_intake_task(id.to_string())
                    .await?
                    .ok_or_else(|| {
                        anyhow!("intake task {id} cannot be cancelled from {}", task.status)
                    })?;
                self.block_intake_dependents(
                    &cancelled.project_id,
                    &cancelled.id,
                    &format!("dependency \"{}\" was cancelled", cancelled.title),
                )
                .await;
                Ok(cancelled)
            }
            "completed" | "cancelled" => Ok(task),
            other => anyhow::bail!("intake task {id} cannot be cancelled from {other}"),
        }
    }

    pub async fn retry_intake_task(&self, id: &str) -> Result<IntakeTask> {
        self.engine
            .retry_intake_task(id.to_string())
            .await?
            .ok_or_else(|| {
                anyhow!("intake task {id} is not failed, interrupted, cancelled, or blocked")
            })
    }

    pub async fn delete_intake_task(&self, id: &str) -> Result<()> {
        let dependents = self
            .engine
            .list_intake_tasks(None, None, None)
            .await?
            .tasks
            .into_iter()
            .filter(|task| task.deps.iter().any(|dep| dep == id))
            .collect::<Vec<_>>();
        if let Some(dependent) = dependents
            .iter()
            .find(|task| !matches!(task.status.as_str(), "completed" | "cancelled"))
        {
            anyhow::bail!(
                "intake task {id} is still a dependency of \"{}\"",
                dependent.title
            );
        }
        if !self.engine.delete_intake_task(id.to_string()).await? {
            anyhow::bail!("intake task {id} is active or does not exist");
        }
        Ok(())
    }

    pub(in crate::agent) async fn block_intake_dependents(
        &self,
        project_id: &str,
        dependency_id: &str,
        reason: &str,
    ) {
        let tasks = match self
            .engine
            .list_intake_tasks(Some(project_id.to_string()), None, None)
            .await
        {
            Ok(tasks) => tasks.tasks,
            Err(error) => {
                tracing::warn!(%error, project_id, "could not block intake dependents");
                return;
            }
        };
        for task in tasks {
            if task.status != "queued" || !task.deps.iter().any(|dep| dep == dependency_id) {
                continue;
            }
            let error = format!("blocked: {reason}");
            if let Err(error) = self
                .engine
                .settle_intake_task(
                    task.id.clone(),
                    "blocked".to_string(),
                    None,
                    Some(error.clone()),
                    None,
                )
                .await
            {
                tracing::warn!(%error, task_id = %task.id, "could not block dependent intake task");
            }
        }
    }

    pub(in crate::agent) async fn intake_retry_at(&self, task: &IntakeTask) -> Option<String> {
        if task.attempts >= task.max_attempts {
            return None;
        }
        let policy = self
            .engine
            .orchestration_policy(&task.project_id)
            .await
            .ok()?;
        if !policy.auto_retry_failed {
            return None;
        }
        let delay = crate::orchestration::retry_delay(&policy, task.attempts.max(1));
        (time::OffsetDateTime::now_utc() + delay)
            .format(&time::format_description::well_known::Rfc3339)
            .ok()
    }
}
