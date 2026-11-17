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
        for entry in queue.into_iter().filter(|entry| entry.status == "conflict") {
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
