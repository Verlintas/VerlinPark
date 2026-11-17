use super::*;

impl AgentService {
    /// Opt-in global task-DAG resume scheduler. Every tick it picks one
    /// idle/failed/cancelled session with retryable tasks, ordered by effective
    /// priority and session round-robin, then resumes its stored plan. This is
    /// intentionally not default-on: resuming a plan spends the session budget.
    pub fn start_task_scheduler(&self) {
        if self.task_scheduler_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let interval_secs = env::var("VERLINPARK_TASK_SCHEDULER_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(5)
            .clamp(1, 3600);
        let service = self.clone();
        tokio::spawn(async move {
            if let Err(error) = service.reconcile_intake_tasks().await {
                tracing::warn!(%error, "intake reconciler failed at startup");
            }
            if let Err(error) = service.reconcile_session_continuations().await {
                tracing::warn!(%error, "continuation reconciler failed at startup");
            }
            if let Err(error) = service.reconcile_programs(None).await {
                tracing::warn!(%error, "program controller failed at startup");
            }
            let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
            ticker.tick().await;
            let mut last_session: Option<String> = None;
            let mut last_project: Option<String> = None;
            let mut cooldowns: HashMap<String, Instant> = HashMap::new();
            let mut intake_cooldowns: HashMap<String, Instant> = HashMap::new();
            loop {
                ticker.tick().await;
                if let Err(error) = service.reconcile_session_continuations().await {
                    tracing::warn!(%error, "global task scheduler could not reconcile continuations");
                }
                if let Err(error) = service.reconcile_intake_tasks().await {
                    tracing::warn!(%error, "global task scheduler could not reconcile intake tasks");
                }
                if let Err(error) = service.reconcile_programs(None).await {
                    tracing::warn!(%error, "global task scheduler could not reconcile programs");
                }
                let now = Instant::now();
                cooldowns.retain(|_, until| *until > now);
                intake_cooldowns.retain(|_, until| *until > now);

                let excluded = cooldowns.keys().cloned().collect::<Vec<_>>();
                let candidate = match service
                    .engine
                    .scheduler()
                    .next_pending_session(last_session.as_deref(), &excluded)
                    .await
                {
                    Ok(candidate) => candidate,
                    Err(error) => {
                        tracing::warn!(%error, "global task scheduler could not read the queue");
                        None
                    }
                };
                if let Some(candidate) = candidate {
                    cooldowns.insert(
                        candidate.session_id.clone(),
                        now + Duration::from_secs(candidate.cooldown_secs),
                    );
                    let dispatch_tasks = service
                        .engine
                        .get_task_scheduler_policy(&candidate.project_id)
                        .await
                        .map(|policy| policy.policy.dispatch_tasks)
                        .unwrap_or(false);
                    tracing::info!(
                        session_id = %candidate.session_id,
                        project_id = %candidate.project_id,
                        pending = candidate.pending,
                        session_priority = candidate.priority,
                        task_priority = candidate.task_priority,
                        effective_priority = candidate.effective_priority,
                        cooldown_secs = candidate.cooldown_secs,
                        dispatch_tasks,
                        "global task scheduler dispatching work"
                    );
                    let result = if dispatch_tasks {
                        service.dispatch_next_task(&candidate.session_id).await
                    } else {
                        service.resume_plan(&candidate.session_id).await
                    };
                    match result {
                        Ok(true) => {
                            last_session = Some(candidate.session_id);
                        }
                        Ok(false) => {}
                        Err(error) => {
                            tracing::warn!(
                                session_id = %candidate.session_id,
                                dispatch_tasks,
                                %error,
                                "global task scheduler could not dispatch work"
                            );
                        }
                    }
                }

                if !service.info().configured {
                    continue;
                }
                let excluded_intake = intake_cooldowns.keys().cloned().collect::<Vec<_>>();
                let intake = match service
                    .engine
                    .scheduler()
                    .next_pending_intake(last_project.as_deref(), &excluded_intake)
                    .await
                {
                    Ok(intake) => intake,
                    Err(error) => {
                        tracing::warn!(%error, "global task scheduler could not read intake tasks");
                        None
                    }
                };
                let Some(intake) = intake else {
                    continue;
                };
                let policy = service
                    .engine
                    .get_task_scheduler_policy(&intake.task.project_id)
                    .await
                    .ok()
                    .map(|result| result.policy)
                    .unwrap_or_default();
                intake_cooldowns.insert(
                    intake.task.id.clone(),
                    now + Duration::from_secs(policy.cooldown_secs),
                );
                tracing::info!(
                    intake_task_id = %intake.task.id,
                    project_id = %intake.task.project_id,
                    effective_priority = intake.effective_priority,
                    attempt = intake.task.attempts,
                    "global task scheduler dispatching standalone intake work"
                );
                match service.dispatch_intake_task(&intake.task.id).await {
                    Ok(result) if result.started => {
                        last_project = result.task.project_id.clone().into();
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(
                            intake_task_id = %intake.task.id,
                            %error,
                            "global task scheduler could not dispatch intake work"
                        );
                    }
                }
            }
        });
    }
}
