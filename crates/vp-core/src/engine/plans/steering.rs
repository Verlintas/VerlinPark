use super::*;

impl Engine {
    /// Create an operator steering request for one running agent/run. When
    /// `agent_id` is omitted the session Leader is targeted; there is no
    /// implicit broadcast to Workers. An optional operation id replays the
    /// first steering row instead of creating a duplicate.
    pub async fn steer_session(
        &self,
        session_id: &str,
        body: &str,
        agent_id: Option<&str>,
        task_id: Option<&str>,
        operation_id: Option<&str>,
    ) -> Result<SessionSteerResult> {
        let payload = serde_json::json!({
            "session_id": session_id,
            "body": body,
            "agent_id": agent_id,
            "task_id": task_id,
        });
        let receipt = match crate::operation::begin(
            self.store(),
            session_id,
            operation_id,
            "session.steer",
            &payload,
        )
        .await?
        {
            crate::operation::OperationStart::Replay(result) => {
                return crate::operation::replay(&result);
            }
            crate::operation::OperationStart::Proceed(receipt) => receipt,
        };
        let result = self
            .steer_session_inner(session_id, body, agent_id, task_id)
            .await;
        match &result {
            Ok(value) => {
                if let Err(error) = crate::operation::complete(&receipt, value).await {
                    return Err(error.context(
                        "steering was created but the session operation receipt could not be completed",
                    ));
                }
            }
            Err(error) => {
                crate::operation::fail(&receipt, &format!("{error:#}")).await;
            }
        }
        result
    }

    pub(crate) async fn steer_session_inner(
        &self,
        session_id: &str,
        body: &str,
        agent_id: Option<&str>,
        task_id: Option<&str>,
    ) -> Result<SessionSteerResult> {
        let body = body.trim();
        if body.is_empty() || body.chars().count() > 4_000 {
            anyhow::bail!("steering body must be 1-4000 chars");
        }
        let task_id = task_id.map(str::trim).filter(|task| !task.is_empty());
        if let Some(task_id) = task_id {
            crate::worktree::validate_task_id(task_id)?;
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        let agents = self.list_agents(session_id).await?;
        let target = match agent_id {
            Some(agent_id) => agents
                .iter()
                .find(|agent| agent.id == agent_id)
                .ok_or_else(|| anyhow!("target agent is not in this session: {agent_id}"))?,
            None => agents
                .iter()
                .find(|agent| {
                    agent.role == "leader"
                        && matches!(agent.status.as_str(), "running" | "waiting_input")
                })
                .ok_or_else(|| {
                    anyhow!(
                        "no running Leader in this session; pass --agent <id> or start the session"
                    )
                })?,
        };
        if !matches!(target.status.as_str(), "running" | "waiting_input") {
            anyhow::bail!(
                "target agent {} is {}; steering requires a running or waiting agent",
                target.id,
                target.status
            );
        }
        if let Some(task_id) = task_id {
            if target.task_id.as_deref() != Some(task_id) {
                anyhow::bail!(
                    "target agent {} is not working on task {task_id}",
                    target.id
                );
            }
            if let Some(plan) = self.get_team_plan(session_id).await?
                && !plan.tasks.iter().any(|task| task.id == task_id)
            {
                anyhow::bail!("task {task_id} is not in the stored TeamPlan");
            }
        }
        let target_id = target.id.clone();
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let body = body.to_string();
        let task_id = task_id.map(str::to_string);
        let (steering, event) = tokio::task::spawn_blocking(move || {
            store.create_steering_with_event(&session_id, &target_id, task_id.as_deref(), &body)
        })
        .await??;
        let _ = self.events.send(event);
        Ok(SessionSteerResult { steering })
    }

    pub async fn session_steering(
        &self,
        session_id: &str,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<SessionSteeringListResult> {
        if let Some(status) = status
            && !matches!(status, "pending" | "delivered" | "cancelled")
        {
            anyhow::bail!("steering status must be pending, delivered, or cancelled");
        }
        let store = self.store.clone();
        let id = session_id.to_string();
        let status = status.map(str::to_string);
        let steering =
            tokio::task::spawn_blocking(move || store.list_steering(&id, status.as_deref(), limit))
                .await??;
        Ok(SessionSteeringListResult {
            session_id: session_id.to_string(),
            steering,
        })
    }

    pub async fn cancel_steering(&self, id: &str) -> Result<SessionSteeringCancelResult> {
        let store = self.store.clone();
        let id = id.to_string();
        let (steering, event) =
            tokio::task::spawn_blocking(move || store.cancel_steering_with_event(&id)).await??;
        let _ = self.events.send(event);
        Ok(SessionSteeringCancelResult {
            cancelled: true,
            steering,
        })
    }

    /// Drain pending steering for the current turn. Called by the agent loop
    /// before the next provider request; top-level runs also persist a visible
    /// user message while child runs keep the injected turn in memory and the
    /// event log.
    pub async fn deliver_pending_steering(
        &self,
        session_id: &str,
        agent_id: &str,
        task_id: Option<&str>,
        persist_messages: bool,
    ) -> Result<Vec<DeliveredSteering>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let agent_id = agent_id.to_string();
        let task_id = task_id.map(str::to_string);
        let (delivered, events) = tokio::task::spawn_blocking(move || {
            store.deliver_pending_steering_with_events(
                &session_id,
                &agent_id,
                task_id.as_deref(),
                persist_messages,
            )
        })
        .await??;
        for event in events {
            let _ = self.events.send(event);
        }
        Ok(delivered)
    }

    /// Terminal-run cleanup: cancel anything still pending for this agent.
    pub async fn cancel_pending_steering_for_agent(
        &self,
        session_id: &str,
        agent_id: &str,
        reason: &str,
    ) -> Result<usize> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let agent_id = agent_id.to_string();
        let reason = reason.to_string();
        let events = tokio::task::spawn_blocking(move || {
            store.cancel_pending_steering_for_agent_with_events(&session_id, &agent_id, &reason)
        })
        .await??;
        let count = events.len();
        for event in events {
            let _ = self.events.send(event);
        }
        Ok(count)
    }
}
