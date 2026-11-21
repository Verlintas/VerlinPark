use super::*;

use vp_protocol::types::{
    AgentInboxSendParams, AgentInboxView, AgentMessageDelivery, AgentMessageSendParams,
    AgentMessageSendResult, agent_operator_codes,
};

/// A task mismatch is a client error but there is no protocol constant for it.
/// The wire string is stable; the daemon maps it like the other operator codes.
const AGENT_TASK_INVALID: &str = "AGENT_TASK_INVALID";
const AGENT_IMPORTED_READONLY: &str = "SESSION_IMPORTED_READONLY";
const AGENT_MESSAGE_BODY_MAX_CHARS: usize = 4_000;
const AGENT_INBOX_VIEW_TEXT_CHARS: usize = 1_200;

impl Engine {
    /// Direct operator-to-Agent message router for the Collab inspector.
    ///
    /// This is a router, not a chat engine. A running or waiting Agent uses the
    /// existing cooperative steering path; an idle Agent with a persisted
    /// project identity uses the durable project inbox; an idle ephemeral run
    /// fails closed with `AGENT_NOT_ACTIVE`. The message cannot approve an
    /// approval, resolve a gate, submit a verdict, merge, or change budgets,
    /// capacity, contracts, sandbox, or model routing.
    pub async fn send_agent_operator_message(
        &self,
        params: AgentMessageSendParams,
    ) -> Result<AgentMessageSendResult> {
        let session_id = params.session_id.trim().to_string();
        let agent_id = params.agent_id.trim().to_string();
        let body = params.body.trim().to_string();
        if session_id.is_empty() || agent_id.is_empty() {
            return Err(operator_error(
                agent_operator_codes::AGENT_TARGET_INVALID,
                "session_id and agent_id are required",
            ));
        }
        if body.is_empty() || body.chars().count() > AGENT_MESSAGE_BODY_MAX_CHARS {
            return Err(operator_error(
                agent_operator_codes::AGENT_MESSAGE_INVALID,
                format!("body must be 1-{AGENT_MESSAGE_BODY_MAX_CHARS} characters"),
            ));
        }
        let task_id = params
            .task_id
            .as_deref()
            .map(str::trim)
            .filter(|task| !task.is_empty())
            .map(str::to_string);
        if let Some(task_id) = &task_id
            && let Err(error) = crate::worktree::validate_task_id(task_id)
        {
            return Err(operator_error(
                AGENT_TASK_INVALID,
                format!("task_id is invalid: {error}"),
            ));
        }

        // Canonical idempotency payload. The operation id itself is never part
        // of the fingerprint. `body` is the normalized operator text.
        let payload = serde_json::json!({
            "session_id": session_id,
            "agent_id": agent_id,
            "body": body,
            "task_id": task_id,
            "wake_session": params.wake_session,
        });
        let receipt = match crate::operation::begin(
            self.store(),
            &session_id,
            params.operation_id.as_deref(),
            "agent.message.send",
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
            .send_agent_message_inner(
                &session_id,
                &agent_id,
                &body,
                task_id.as_deref(),
                params.wake_session,
            )
            .await;
        match &result {
            Ok(value) => {
                if let Err(error) = crate::operation::complete(&receipt, value).await {
                    return Err(error.context(
                        "message was routed but the session operation receipt could not be completed",
                    ));
                }
            }
            Err(error) => {
                crate::operation::fail(&receipt, &format!("{error:#}")).await;
            }
        }
        result
    }

    async fn send_agent_message_inner(
        &self,
        session_id: &str,
        agent_id: &str,
        body: &str,
        task_id: Option<&str>,
        wake_session: bool,
    ) -> Result<AgentMessageSendResult> {
        let store = self.store.clone();
        let lookup = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&lookup))
            .await??
            .ok_or_else(|| {
                operator_error(agent_operator_codes::AGENT_NOT_FOUND, "session not found")
            })?;
        if session.status == "imported" {
            return Err(operator_error(
                AGENT_IMPORTED_READONLY,
                "imported sessions are read-only archives; message a live session instead",
            ));
        }

        let agents = self.list_agents(session_id).await?;
        let target = agents
            .iter()
            .find(|agent| agent.id == agent_id)
            .cloned()
            .ok_or_else(|| {
                operator_error(
                    agent_operator_codes::AGENT_NOT_FOUND,
                    "agent is not a member of this session",
                )
            })?;

        if let Some(task_id) = task_id {
            // The route never silently retargets a message to another task.
            if target.task_id.as_deref() != Some(task_id) {
                return Err(operator_error(
                    AGENT_TASK_INVALID,
                    format!("agent {} is not working on task {task_id}", target.id),
                ));
            }
            if let Some(plan) = self.get_team_plan(session_id).await?
                && !plan.tasks.iter().any(|task| task.id == task_id)
            {
                return Err(operator_error(
                    AGENT_TASK_INVALID,
                    format!("task {task_id} is not in the stored TeamPlan"),
                ));
            }
        }

        if matches!(target.status.as_str(), "running" | "waiting_input") {
            // Existing cooperative steering path. Delivery happens at the
            // target run's next provider boundary and is cancelled if the run
            // ends first; this router must not claim instant delivery.
            let result = self
                .steer_session_inner(session_id, body, Some(&target.id), task_id)
                .await?;
            let steering = result.steering;
            return Ok(AgentMessageSendResult {
                delivery: AgentMessageDelivery {
                    kind: "steering".to_string(),
                    status: steering.status.clone(),
                    message: format!(
                        "Queued as steering for agent {}. Delivery happens at the target run's next provider boundary and is cancelled if the run ends first; this message grants no approval, gate, budget, or merge authority.",
                        target.id
                    ),
                    agent_id: target.id,
                    identity_id: target.identity_id,
                    steering: Some(steering),
                    inbox: None,
                    wake: None,
                },
            });
        }

        let Some(identity_id) = target.identity_id.clone() else {
            return Err(operator_error(
                agent_operator_codes::AGENT_NOT_ACTIVE,
                format!(
                    "agent {} is {} and has no persisted project identity, so it cannot receive a durable inbox message; start or retry its task, then message it while it is running",
                    target.id, target.status
                ),
            ));
        };

        // Idle project identity: durable, pull-based inbox. The optional wake is
        // the existing deferred continuation, never a provider call or a run.
        let inbox_result = self
            .send_agent_inbox(AgentInboxSendParams {
                project_id: Some(session.project_id.clone()),
                identity_id: identity_id.clone(),
                session_id: Some(session.id.clone()),
                from_identity_id: None,
                kind: "message".to_string(),
                payload: None,
                body: Some(body.to_string()),
                expires_in_secs: None,
                wake_session,
            })
            .await?;
        let inbox = agent_inbox_view(inbox_result.message);
        let message = if wake_session {
            format!(
                "Stored in the project inbox for agent {} and a deferred wake was requested. The Agent is not running, so this is queued/unread, not instant delivery.",
                target.id
            )
        } else {
            format!(
                "Stored in the project inbox for agent {}. The inbox is pull-based, so this remains unread until the Agent reads it or an operator explicitly wakes the session.",
                target.id
            )
        };
        Ok(AgentMessageSendResult {
            delivery: AgentMessageDelivery {
                kind: "inbox".to_string(),
                status: inbox.status.clone(),
                message,
                agent_id: target.id,
                identity_id: Some(identity_id),
                steering: None,
                inbox: Some(inbox),
                wake: Some(inbox_result.wake),
            },
        })
    }
}

fn operator_error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    vp_protocol::types::AgentIdentityError::new(code, message).into()
}

fn agent_inbox_view(message: vp_protocol::types::AgentInboxMessage) -> AgentInboxView {
    AgentInboxView {
        id: message.id,
        from_label: operator_text(&message.from_label, 200),
        kind: operator_text(&message.kind, 80),
        status: operator_text(&message.status, 40),
        body: inbox_display_body(&message.payload),
        created_at: message.created_at,
        read_at: message.read_at,
        expires_at: message.expires_at,
    }
}

fn inbox_display_body(payload: &serde_json::Value) -> String {
    for key in ["body", "message", "summary", "detail"] {
        if let Some(value) = payload
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return operator_text(value, AGENT_INBOX_VIEW_TEXT_CHARS);
        }
    }
    if payload.is_null() || payload.as_object().is_some_and(|object| object.is_empty()) {
        return String::new();
    }
    let summary = serde_json::to_string(payload).unwrap_or_default();
    operator_text(&summary, 600)
}

fn operator_text(value: &str, max_chars: usize) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let redacted = crate::tooling::errors::redact_secrets_for_display(trimmed);
    let mut chars = redacted.chars();
    let mut bounded = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        if max_chars > 0 {
            bounded.pop();
        }
        bounded.push('…');
    }
    bounded
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;
    use vp_protocol::types::{
        AgentInboxListParams, CreateAgentIdentityParams, PlanDepartment, PlanTask, TeamPlan,
    };

    fn setup() -> (TempDir, TempDir, Engine, Project, Session) {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("operator"))
            .unwrap();
        let session = store
            .create_session(&project.id, "operator message")
            .unwrap();
        let engine = Engine::new(store);
        (data, repo, engine, project, session)
    }

    async fn leader(engine: &Engine, session: &Session) -> AgentRecord {
        let id = engine
            .create_leader_agent(&session.id, "leader-model")
            .await
            .unwrap();
        engine
            .list_agents(&session.id)
            .await
            .unwrap()
            .into_iter()
            .find(|agent| agent.id == id)
            .expect("leader row")
    }

    fn code_of(error: &anyhow::Error) -> Option<&'static str> {
        error
            .downcast_ref::<vp_protocol::types::AgentIdentityError>()
            .map(|typed| typed.code)
    }

    fn operator_plan() -> TeamPlan {
        TeamPlan {
            goal: "route operator messages".into(),
            departments: vec![PlanDepartment {
                name: "impl".into(),
                manager_title: "Implementation".into(),
                profile: "coding".into(),
                objective: "build it".into(),
                token_budget: None,
                cost_budget_usd: None,
            }],
            tasks: vec![PlanTask {
                id: "T1".into(),
                department: "impl".into(),
                title: "Build it".into(),
                kind: "coding".into(),
                priority: 5,
                profile: Some("coding".into()),
                deps: Vec::new(),
                write_scope: vec!["src/operator".into()],
                acceptance: vec!["tests pass".into()],
                token_budget: None,
                cost_budget_usd: None,
                parent_task_id: None,
                fanout_key: None,
                fanout_join_for: None,
            }],
        }
    }

    #[tokio::test]
    async fn running_agent_uses_steering_and_replays_the_operation() {
        let (_data, _repo, engine, _project, session) = setup();
        let agent = leader(&engine, &session).await;
        engine
            .update_agent_status(&agent.id, "running")
            .await
            .unwrap();

        let params = AgentMessageSendParams {
            session_id: session.id.clone(),
            agent_id: agent.id.clone(),
            body: "please check the parser".into(),
            task_id: None,
            operation_id: Some("msg-op-1".into()),
            wake_session: false,
        };
        let first = engine
            .send_agent_operator_message(params.clone())
            .await
            .expect("steering route");
        assert_eq!(first.delivery.kind, "steering");
        assert_eq!(first.delivery.status, "pending");
        assert!(first.delivery.inbox.is_none());
        assert!(first.delivery.wake.is_none());
        assert_eq!(
            first
                .delivery
                .steering
                .as_ref()
                .map(|row| row.body.as_str()),
            Some("please check the parser")
        );
        let steering_id = first.delivery.steering.as_ref().unwrap().id.clone();

        let replay = engine
            .send_agent_operator_message(params)
            .await
            .expect("operation replay");
        assert_eq!(
            replay.delivery.steering.as_ref().map(|row| row.id.as_str()),
            Some(steering_id.as_str())
        );
        let rows = engine
            .session_steering(&session.id, None, None)
            .await
            .unwrap();
        assert_eq!(rows.steering.len(), 1);
    }

    #[tokio::test]
    async fn idle_identity_uses_the_project_inbox_and_reports_the_wake() {
        let (_data, _repo, engine, project, session) = setup();
        let agent = leader(&engine, &session).await;
        let identity = engine
            .create_agent_identity(CreateAgentIdentityParams {
                project_id: project.id.clone(),
                role: "worker".into(),
                department: Some("impl".into()),
                title: Some("Implementation".into()),
                profile: Some("coding".into()),
                ..CreateAgentIdentityParams::default()
            })
            .await
            .expect("identity")
            .identity;
        engine
            .set_agent_identity_binding(&agent.id, Some(&identity.id))
            .await
            .unwrap();
        engine.update_agent_status(&agent.id, "idle").await.unwrap();
        engine
            .store()
            .update_session_status(&session.id, "idle")
            .unwrap();

        let result = engine
            .send_agent_operator_message(AgentMessageSendParams {
                session_id: session.id.clone(),
                agent_id: agent.id.clone(),
                body: "pick this up when you return".into(),
                task_id: None,
                operation_id: None,
                wake_session: true,
            })
            .await
            .expect("inbox route");
        assert_eq!(result.delivery.kind, "inbox");
        assert_eq!(result.delivery.status, "unread");
        assert_eq!(
            result.delivery.identity_id.as_deref(),
            Some(identity.id.as_str())
        );
        assert_eq!(
            result.delivery.inbox.as_ref().map(|row| row.body.as_str()),
            Some("pick this up when you return")
        );
        assert!(
            !result
                .delivery
                .message
                .to_ascii_lowercase()
                .contains("delivered immediately")
        );
        let wake = result.delivery.wake.expect("wake result");
        assert!(wake.requested);
        assert!(
            wake.created,
            "wake should create a deferred continuation: {wake:?}"
        );
        assert!(wake.continuation_id.is_some());

        let inbox = engine
            .list_agent_inbox(AgentInboxListParams {
                project_id: Some(project.id.clone()),
                identity_id: Some(identity.id.clone()),
                session_id: Some(session.id.clone()),
                ..AgentInboxListParams::default()
            })
            .await
            .expect("inbox list");
        assert_eq!(inbox.messages.len(), 1);
        assert_eq!(inbox.messages[0].status, "unread");
        assert_eq!(inbox.counts.unread, 1);
        assert!(
            engine
                .session_steering(&session.id, None, None)
                .await
                .unwrap()
                .steering
                .is_empty()
        );
    }

    #[tokio::test]
    async fn idle_ephemeral_agent_fails_closed_without_writing_a_note() {
        let (_data, _repo, engine, project, session) = setup();
        let agent = leader(&engine, &session).await;
        engine.update_agent_status(&agent.id, "idle").await.unwrap();

        let error = engine
            .send_agent_operator_message(AgentMessageSendParams {
                session_id: session.id.clone(),
                agent_id: agent.id,
                body: "hello".into(),
                ..AgentMessageSendParams::default()
            })
            .await
            .expect_err("ephemeral idle target must fail");
        assert_eq!(
            code_of(&error),
            Some(agent_operator_codes::AGENT_NOT_ACTIVE)
        );

        let inbox = engine
            .list_agent_inbox(AgentInboxListParams {
                project_id: Some(project.id),
                session_id: Some(session.id.clone()),
                ..AgentInboxListParams::default()
            })
            .await
            .expect("inbox list");
        assert!(inbox.messages.is_empty());
        assert!(
            engine
                .session_steering(&session.id, None, None)
                .await
                .unwrap()
                .steering
                .is_empty()
        );
    }

    #[tokio::test]
    async fn message_validation_rejects_imported_cross_session_and_task_mismatch() {
        let (_data, _repo, engine, _project, session) = setup();
        let agent = leader(&engine, &session).await;

        let empty = engine
            .send_agent_operator_message(AgentMessageSendParams {
                session_id: session.id.clone(),
                agent_id: agent.id.clone(),
                body: "   ".into(),
                ..AgentMessageSendParams::default()
            })
            .await
            .expect_err("empty body");
        assert_eq!(
            code_of(&empty),
            Some(agent_operator_codes::AGENT_MESSAGE_INVALID)
        );

        engine
            .update_agent_status(&agent.id, "running")
            .await
            .unwrap();
        let task_error = engine
            .send_agent_operator_message(AgentMessageSendParams {
                session_id: session.id.clone(),
                agent_id: agent.id.clone(),
                body: "wrong task".into(),
                task_id: Some("T9".into()),
                ..AgentMessageSendParams::default()
            })
            .await
            .expect_err("task mismatch");
        assert_eq!(code_of(&task_error), Some("AGENT_TASK_INVALID"));

        engine
            .submit_team_plan(&session.id, &operator_plan())
            .await
            .unwrap();
        engine
            .set_agent_task(&agent.id, Some("T1".into()), Some("impl".into()))
            .await
            .unwrap();
        let plan_error = engine
            .send_agent_operator_message(AgentMessageSendParams {
                session_id: session.id.clone(),
                agent_id: agent.id.clone(),
                body: "missing task".into(),
                task_id: Some("T9".into()),
                ..AgentMessageSendParams::default()
            })
            .await
            .expect_err("task not in plan");
        assert_eq!(code_of(&plan_error), Some("AGENT_TASK_INVALID"));

        let other = engine
            .store()
            .create_session(&session.project_id, "other")
            .unwrap();
        let other_agent = leader(&engine, &other).await;
        let cross = engine
            .send_agent_operator_message(AgentMessageSendParams {
                session_id: session.id.clone(),
                agent_id: other_agent.id,
                body: "cross session".into(),
                ..AgentMessageSendParams::default()
            })
            .await
            .expect_err("cross-session target");
        assert_eq!(code_of(&cross), Some(agent_operator_codes::AGENT_NOT_FOUND));

        engine
            .store()
            .update_session_status(&session.id, "imported")
            .unwrap();
        let imported = engine
            .send_agent_operator_message(AgentMessageSendParams {
                session_id: session.id.clone(),
                agent_id: agent.id,
                body: "archive".into(),
                ..AgentMessageSendParams::default()
            })
            .await
            .expect_err("imported target");
        assert_eq!(code_of(&imported), Some("SESSION_IMPORTED_READONLY"));
    }

    #[tokio::test]
    async fn redacted_inbox_view_never_returns_raw_payload_objects() {
        let view = agent_inbox_view(vp_protocol::types::AgentInboxMessage {
            id: "msg-1".into(),
            from_label: "operator".into(),
            kind: "message".into(),
            status: "unread".into(),
            payload: json!({"body": "token=supersecret123 review this"}),
            created_at: "2026-11-22T00:00:00Z".into(),
            ..vp_protocol::types::AgentInboxMessage::default()
        });
        assert!(!view.body.contains("supersecret123"));
        assert!(view.body.contains("review this"));

        let unknown = agent_inbox_view(vp_protocol::types::AgentInboxMessage {
            id: "msg-2".into(),
            from_label: "operator".into(),
            kind: "message".into(),
            status: "unread".into(),
            payload: json!({"credential": "abc123", "detail": "safe summary"}),
            created_at: "2026-11-22T00:00:00Z".into(),
            ..vp_protocol::types::AgentInboxMessage::default()
        });
        assert_eq!(unknown.body, "safe summary");
    }
}
