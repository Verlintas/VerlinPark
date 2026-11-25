use super::*;

impl AgentService {
    /// Start the daemon-side watcher that auto-resolves conflicts when a
    /// project opts in. Safe to call more than once.
    pub fn start_auto_resolver(&self) {
        if self.auto_resolver_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let service = self.clone();
        tokio::spawn(async move {
            let mut events = service.engine.subscribe();
            service.scan_auto_resolve().await;
            loop {
                match events.recv().await {
                    Ok(event)
                        if event.kind == "session.updated" && event.payload["status"] == "idle" =>
                    {
                        service.auto_resolve_session(&event.session_id).await;
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    pub async fn auto_resolve_project(&self, project_id: &str) {
        let sessions = match self
            .engine
            .list_sessions(Some(project_id.to_string()))
            .await
        {
            Ok(sessions) => sessions,
            Err(error) => {
                tracing::warn!(%error, "auto-resolve could not list project sessions");
                return;
            }
        };
        for session in sessions {
            self.auto_resolve_session(&session.id).await;
        }
    }

    pub async fn auto_resolve_all(&self) {
        self.scan_auto_resolve().await;
    }

    pub(in crate::agent) async fn scan_auto_resolve(&self) {
        let sessions = match self.engine.list_sessions(None).await {
            Ok(sessions) => sessions,
            Err(error) => {
                tracing::warn!(%error, "auto-resolve could not list sessions");
                return;
            }
        };
        for session in sessions {
            self.auto_resolve_session(&session.id).await;
        }
    }

    pub(in crate::agent) async fn auto_resolve_session(&self, session_id: &str) {
        let Ok((session, _)) = load_session_project(self.engine.store().clone(), session_id).await
        else {
            return;
        };
        if session.status != "idle" {
            return;
        }
        let policy = match self
            .engine
            .get_integration_policy(&session.project_id)
            .await
        {
            Ok(policy) => policy,
            Err(error) => {
                tracing::warn!(%error, session_id, "auto-resolve could not load policy");
                return;
            }
        };
        if !policy.auto_resolve || policy.max_auto_attempts == 0 {
            return;
        }
        if !self.info().configured {
            return;
        }
        if self
            .runs
            .lock()
            .map(|runs| runs.contains_key(session_id))
            .unwrap_or(true)
        {
            return;
        }
        let queue = match self.engine.list_merge_queue(session_id).await {
            Ok(queue) => queue,
            Err(error) => {
                tracing::warn!(%error, session_id, "auto-resolve could not list queue");
                return;
            }
        };
        let plan = match self.engine.get_team_plan(session_id).await {
            Ok(plan) => plan,
            Err(error) => {
                tracing::warn!(%error, session_id, "auto-resolve could not read the plan");
                return;
            }
        };
        let store = self.engine.store().clone();
        let session = session_id.to_string();
        let tasks =
            match tokio::task::spawn_blocking(move || store.list_plan_tasks(None, Some(&session)))
                .await
            {
                Ok(Ok(tasks)) => tasks,
                Ok(Err(error)) => {
                    tracing::warn!(%error, session_id, "auto-resolve could not read plan tasks");
                    return;
                }
                Err(error) => {
                    tracing::warn!(%error, session_id, "auto-resolve task read failed");
                    return;
                }
            };
        for entry in queue
            .into_iter()
            .filter(|entry| entry.status == "conflict")
            .filter(|entry| {
                auto_resolve_task_allowed(plan.as_ref(), &tasks, entry.task_id.as_deref())
            })
        {
            let attempted = self
                .engine
                .auto_resolve_attempts(&entry.id)
                .await
                .unwrap_or(policy.max_auto_attempts);
            if attempted >= policy.max_auto_attempts {
                continue;
            }
            let attempted = match self.engine.bump_auto_resolve_attempts(&entry.id).await {
                Ok(attempted) => attempted,
                Err(error) => {
                    tracing::warn!(%error, entry_id = %entry.id, "auto-resolve could not record attempt");
                    continue;
                }
            };
            if attempted > policy.max_auto_attempts {
                continue;
            }
            let _ = self
                .engine
                .append_event(
                    session_id,
                    "merge_queue.auto_resolve_started",
                    serde_json::json!({
                        "entry_id": entry.id,
                        "attempt": attempted,
                        "max_auto_attempts": policy.max_auto_attempts,
                        "max_resolve_tokens": policy.max_resolve_tokens,
                    }),
                )
                .await;
            match self
                .resolve_merge_with_limit(&entry.id, policy.max_resolve_tokens)
                .await
            {
                Ok(resolved) => {
                    let _ = self
                        .engine
                        .append_event(
                            session_id,
                            "merge_queue.auto_resolve_finished",
                            serde_json::json!({
                                "entry_id": entry.id,
                                "status": resolved.status,
                                "detail": resolved.detail,
                            }),
                        )
                        .await;
                }
                Err(error) => {
                    let _ = self
                        .engine
                        .append_event(
                            session_id,
                            "merge_queue.auto_resolve_finished",
                            serde_json::json!({
                                "entry_id": entry.id,
                                "status": "failed",
                                "detail": error.to_string(),
                            }),
                        )
                        .await;
                }
            }
        }
    }
}

/// Auto-resolution may only touch a conflict whose plan task still exists and
/// was not explicitly skipped/cancelled. A plan revision can remove a task, and
/// `task.skip` does not delete its merge-queue row; merging that branch would
/// resurrect work the operator already settled.
pub(in crate::agent) fn auto_resolve_task_allowed(
    plan: Option<&TeamPlan>,
    tasks: &[ScheduledTask],
    task_id: Option<&str>,
) -> bool {
    let Some(task_id) = task_id else {
        return true;
    };
    let Some(plan) = plan else {
        return true;
    };
    if !plan.tasks.iter().any(|task| task.id == task_id) {
        return false;
    }
    if let Some(row) = tasks.iter().find(|task| task.task_id == task_id)
        && matches!(row.status.as_str(), "skipped" | "cancelled")
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(ids: &[&str]) -> TeamPlan {
        serde_json::from_value(serde_json::json!({
            "goal": "auto resolve",
            "departments": [{
                "name": "impl",
                "manager_title": "Implementation",
                "profile": "coding",
                "objective": "build"
            }],
            "tasks": ids.iter().map(|id| serde_json::json!({
                "id": id,
                "department": "impl",
                "title": id,
                "kind": "coding",
                "deps": [],
                "acceptance": ["done"],
                "write_scope": ["src/lib.rs"]
            })).collect::<Vec<_>>()
        }))
        .unwrap()
    }

    fn row(task_id: &str, status: &str) -> ScheduledTask {
        ScheduledTask {
            session_id: "ses-1".into(),
            project_id: "prj-1".into(),
            plan_seq: 1,
            task_id: task_id.into(),
            department: "impl".into(),
            title: task_id.into(),
            priority: 0,
            status: status.into(),
            deps: Vec::new(),
            write_scope: vec!["src/lib.rs".into()],
            attempts: 0,
            enqueued_at: "2026-01-01T00:00:00Z".into(),
            started_at: None,
            finished_at: None,
            last_error: None,
            next_attempt_at: None,
        }
    }

    #[test]
    fn auto_resolve_skips_removed_skipped_and_cancelled_tasks() {
        let plan = plan(&["T1", "T2", "T3"]);
        let rows = vec![
            row("T1", "pending"),
            row("T2", "skipped"),
            row("T3", "cancelled"),
        ];
        assert!(auto_resolve_task_allowed(Some(&plan), &rows, Some("T1")));
        assert!(!auto_resolve_task_allowed(Some(&plan), &rows, Some("T2")));
        assert!(!auto_resolve_task_allowed(Some(&plan), &rows, Some("T3")));
        assert!(!auto_resolve_task_allowed(Some(&plan), &rows, Some("T4")));
        assert!(auto_resolve_task_allowed(None, &rows, Some("T3")));
        assert!(auto_resolve_task_allowed(Some(&plan), &rows, None));
    }
}
