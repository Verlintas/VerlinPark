use super::*;

/// Deterministic state derived from the stored TeamPlan and task scheduler
/// rows. This block is authoritative; the model summary is advisory free text.
pub(in crate::agent) async fn load_compaction_structured_state(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
) -> ContextStructuredState {
    let plan = engine.get_team_plan(&session.id).await.ok().flatten();
    let store = engine.store().clone();
    let session_id = session.id.clone();
    let tasks = tokio::task::spawn_blocking(move || store.list_plan_tasks(None, Some(&session_id)))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    let store = engine.store().clone();
    let session_id = session.id.clone();
    let plan_revision = tokio::task::spawn_blocking(move || store.plan_revision_count(&session_id))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or(0);
    let statuses = tasks
        .iter()
        .map(|task| (task.task_id.clone(), task.status.clone()))
        .collect::<HashMap<_, _>>();
    let active_id = spec
        .task_id
        .clone()
        .or_else(|| {
            tasks
                .iter()
                .filter(|task| {
                    matches!(
                        task.status.as_str(),
                        "running" | "claimed" | "paused" | "cancelling" | "pausing"
                    )
                })
                .min_by(|left, right| {
                    left.plan_seq
                        .cmp(&right.plan_seq)
                        .then_with(|| left.task_id.cmp(&right.task_id))
                })
                .map(|task| task.task_id.clone())
        })
        .or_else(|| {
            plan.as_ref().and_then(|plan| {
                plan.tasks
                    .iter()
                    .find(|task| {
                        !matches!(
                            statuses.get(&task.id).map(String::as_str),
                            Some("completed" | "failed" | "blocked" | "cancelled" | "skipped")
                        )
                    })
                    .map(|task| task.id.clone())
            })
        });
    let active_task = active_id.as_ref().and_then(|active_id| {
        plan.as_ref()
            .and_then(|plan| plan.tasks.iter().find(|task| &task.id == active_id))
    });
    let mut completed = tasks
        .iter()
        .filter(|task| task.status == "completed")
        .collect::<Vec<_>>();
    completed.sort_by(|left, right| {
        right
            .finished_at
            .cmp(&left.finished_at)
            .then_with(|| left.task_id.cmp(&right.task_id))
    });
    let mut recent_completed_task_ids = Vec::new();
    for task in completed {
        if recent_completed_task_ids.len() >= 8 {
            break;
        }
        if !recent_completed_task_ids.contains(&task.task_id) {
            recent_completed_task_ids.push(task.task_id.clone());
        }
    }
    ContextStructuredState {
        goal: plan
            .as_ref()
            .map(|plan| plan.goal.clone())
            .filter(|goal| !goal.trim().is_empty())
            .unwrap_or_else(|| session.goal.clone()),
        plan_revision,
        active_task_id: active_id,
        active_task_title: active_task.map(|task| task.title.clone()),
        active_task_department: active_task.map(|task| task.department.clone()),
        active_task_acceptance: active_task
            .map(|task| task.acceptance.clone())
            .unwrap_or_default(),
        active_task_write_scope: active_task
            .map(|task| task.write_scope.clone())
            .unwrap_or_default(),
        recent_completed_task_ids,
        generated_at: vp_protocol::now_rfc3339(),
    }
}

pub(in crate::agent) fn render_structured_state(state: &ContextStructuredState) -> String {
    let mut rendered = String::new();
    rendered.push_str(STRUCTURED_STATE_START);
    rendered.push('\n');
    rendered.push_str(&format!("Goal: {}\n", structured_state_line(&state.goal)));
    rendered.push_str(&format!("Plan revision: {}\n", state.plan_revision));
    match &state.active_task_id {
        Some(task_id) => {
            rendered.push_str(&format!(
                "Active task: {task_id} - {}\n",
                structured_state_line(
                    state
                        .active_task_title
                        .as_deref()
                        .unwrap_or("(untitled task)")
                )
            ));
            rendered.push_str(&format!(
                "Department: {}\n",
                structured_state_line(state.active_task_department.as_deref().unwrap_or("(none)"))
            ));
            if state.active_task_acceptance.is_empty() {
                rendered.push_str("Acceptance: (none recorded)\n");
            } else {
                rendered.push_str("Acceptance:\n");
                for item in &state.active_task_acceptance {
                    rendered.push_str(&format!("- {}\n", structured_state_line(item)));
                }
            }
            if state.active_task_write_scope.is_empty() {
                rendered.push_str("Write scope: (none recorded)\n");
            } else {
                rendered.push_str("Write scope:\n");
                for item in &state.active_task_write_scope {
                    rendered.push_str(&format!("- {}\n", structured_state_line(item)));
                }
            }
        }
        None => {
            rendered.push_str("Active task: (none)\n");
        }
    }
    if state.recent_completed_task_ids.is_empty() {
        rendered.push_str("Recently completed tasks: (none)\n");
    } else {
        rendered.push_str(&format!(
            "Recently completed tasks: {}\n",
            state.recent_completed_task_ids.join(", ")
        ));
    }
    rendered.push_str(&format!("Generated at: {}\n", state.generated_at));
    rendered.push_str(STRUCTURED_STATE_END);
    rendered
}

/// Replace a previous structured block with the current one while preserving
/// the surrounding model summary. If no block exists, prepend one.
pub(in crate::agent) fn refresh_structured_state(existing: &str, state_block: &str) -> String {
    if let Some(start) = existing.find(STRUCTURED_STATE_START)
        && let Some(relative_end) = existing[start..].find(STRUCTURED_STATE_END)
    {
        let end = start + relative_end + STRUCTURED_STATE_END.len();
        let mut refreshed = String::new();
        refreshed.push_str(&existing[..start]);
        refreshed.push_str(state_block);
        refreshed.push_str(&existing[end..]);
        return refreshed;
    }
    format!("{state_block}\n\n{existing}")
}

pub(in crate::agent) async fn project_memory_brief(
    engine: &Engine,
    session_id: &str,
    task_id: Option<&str>,
    department: Option<&str>,
) -> Result<String> {
    let brief = engine
        .memory_brief_with_department(session_id, task_id, department)
        .await?;
    Ok(brief.text)
}

pub(in crate::agent) async fn coordination_brief_for_agent(
    engine: &Engine,
    session_id: &str,
    agent_id: &str,
    task_id: Option<&str>,
    limit: usize,
) -> Result<String> {
    let notes = engine
        .coordination_notes_for_agent(session_id, agent_id, task_id, limit.clamp(1, 50) as u32)
        .await?;
    Ok(notes
        .iter()
        .map(format_coordination_note)
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Inject unread mailbox messages into a Worker brief and mark them read.
pub(in crate::agent) async fn mailbox_brief(
    engine: &Engine,
    session_id: &str,
    agent_id: &str,
    limit: usize,
) -> Result<String> {
    let messages = engine
        .list_agent_messages(session_id, Some(agent_id.to_string()), true)
        .await?;
    if messages.is_empty() {
        return Ok(String::new());
    }
    let selected = messages
        .iter()
        .rev()
        .take(limit.clamp(1, 20))
        .cloned()
        .collect::<Vec<_>>();
    for message in selected.iter().rev() {
        let _ = engine.mark_agent_message_read(&message.id).await;
    }
    Ok(selected
        .iter()
        .rev()
        .map(|message| {
            format!(
                "[{}] {} → {} · {}",
                message.kind,
                message.from_name,
                message.to_name,
                truncate_to(&message.body, 500)
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

pub(in crate::agent) fn task_dependency_brief(
    task: &PlanTask,
    statuses: &HashMap<String, String>,
    results: &HashMap<String, String>,
) -> String {
    if task.deps.is_empty() {
        return "No upstream dependencies.".to_string();
    }
    task.deps
        .iter()
        .take(8)
        .map(|dep| {
            let status = statuses.get(dep).map(String::as_str).unwrap_or("pending");
            let result = results
                .get(dep)
                .map(String::as_str)
                .unwrap_or("no completed result yet");
            format!("- {dep} [{status}] {result}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(in crate::agent) async fn load_session_project(
    store: Store,
    session_id: &str,
) -> Result<(Session, Project)> {
    let session_id = session_id.to_string();
    tokio::task::spawn_blocking(move || -> Result<(Session, Project)> {
        let session = store
            .get_session(&session_id)?
            .ok_or_else(|| anyhow!("session not found"))?;
        let project = store
            .get_project(&session.project_id)?
            .ok_or_else(|| anyhow!("project not found"))?;
        Ok((session, project))
    })
    .await?
}

pub(in crate::agent) struct LoadedConversation {
    pub(in crate::agent) messages: Vec<ChatMessage>,
    pub(in crate::agent) ids: Vec<Option<String>>,
}

/// Keep provider tool-call history valid after an interrupted or cancelled
/// turn. OpenAI-compatible providers reject an assistant `tool_calls` message
/// unless every id is followed by a matching `tool` message. A cancelled run
/// can leave that pair incomplete, so synthesize a bounded interrupted result
/// in memory and drop orphan tool messages. This is a resume-time projection;
/// it does not rewrite the session's stored messages.
fn normalize_tool_history(
    messages: Vec<ChatMessage>,
    ids: Vec<Option<String>>,
) -> (Vec<ChatMessage>, Vec<Option<String>>) {
    let mut normalized = Vec::with_capacity(messages.len());
    let mut normalized_ids = Vec::with_capacity(ids.len());
    let mut index = 0usize;
    while index < messages.len() {
        let message = &messages[index];
        if message.role == "assistant"
            && message
                .tool_calls
                .as_ref()
                .is_some_and(|calls| !calls.is_empty())
        {
            let calls = message.tool_calls.clone().unwrap_or_default();
            let mut results = HashMap::new();
            let mut next = index + 1;
            while next < messages.len() && messages[next].role == "tool" {
                if let Some(call_id) = messages[next].tool_call_id.clone() {
                    results.entry(call_id).or_insert(next);
                }
                next += 1;
            }
            normalized.push(message.clone());
            normalized_ids.push(ids[index].clone());
            for call in calls {
                if let Some(source) = results.remove(&call.id) {
                    normalized.push(messages[source].clone());
                    normalized_ids.push(ids[source].clone());
                } else {
                    normalized.push(ChatMessage::tool(
                        call.id,
                        format!(
                            "error[TOOL_FAILED] tool '{}' did not complete because the previous run ended before its result was recorded. Re-issue it if the work is still needed.",
                            call.name
                        ),
                    ));
                    normalized_ids.push(None);
                }
            }
            index = next;
            continue;
        }
        if message.role == "tool" {
            // No preceding assistant call owned this result.
            index += 1;
            continue;
        }
        normalized.push(message.clone());
        normalized_ids.push(ids[index].clone());
        index += 1;
    }
    (normalized, normalized_ids)
}

/// Build the active conversation from `message.created` events. This is the
/// resume path: it deliberately does not trust a live in-memory vector or an
/// imported archive's convenience projection.
pub(crate) fn conversation_from_events(
    events: &[vp_protocol::types::Event],
    session_id: &str,
) -> Result<(Vec<ChatMessage>, Vec<Option<String>>)> {
    let mut stored = Vec::new();
    let mut compaction: Option<&vp_protocol::types::Event> = None;
    for event in events {
        if event.session_id != session_id {
            anyhow::bail!("event stream contains an event for another session");
        }
        match event.kind.as_str() {
            "message.created" => {
                let message: StoredMessage = serde_json::from_value(event.payload.clone())
                    .context("decoding a durable message event")?;
                stored.push(message);
            }
            "context.compacted" => {
                compaction = Some(event);
            }
            _ => {}
        }
    }
    let mut start = 0usize;
    let mut summary = None;
    if let Some(event) = compaction {
        let mode = event
            .payload
            .get("mode")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("summary");
        let through = event
            .payload
            .get("through_message_id")
            .and_then(serde_json::Value::as_str);
        let text = event
            .payload
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .filter(|text| !text.trim().is_empty());
        if let Some(through) = through
            && let Some(index) = stored.iter().position(|message| message.id == through)
        {
            start = index.saturating_add(1);
            if mode == "summary" {
                summary = text.map(str::to_string);
            }
        }
    }
    let mut ids = stored
        .iter()
        .skip(start)
        .map(|message| Some(message.id.clone()))
        .collect::<Vec<_>>();
    let mut messages = stored
        .into_iter()
        .skip(start)
        .map(|message| {
            let tool_calls: Vec<ToolCall> =
                serde_json::from_value(message.tool_calls).unwrap_or_default();
            ChatMessage {
                role: message.role,
                content: message.content,
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                },
                tool_call_id: message.tool_call_id,
            }
        })
        .collect::<Vec<_>>();
    if let Some(summary) = summary {
        messages.insert(0, ChatMessage::user(summary));
        ids.insert(0, None);
    }
    Ok((messages, ids))
}

/// Best-effort worktree identity for a context checkpoint. A missing or
/// unreadable worktree records the empty string and restore refuses it.
pub(in crate::agent) fn context_worktree_head(worktree: Option<&str>) -> String {
    let Some(worktree) = worktree else {
        return String::new();
    };
    let path = Path::new(worktree);
    if !path.is_dir() {
        return String::new();
    }
    worktree::head_commit(path).unwrap_or_default()
}

/// Reconstruct the conversation a checkpoint restore would expose. Returns
/// `(messages, through_found)`. The summary is injected whenever the
/// checkpoint carries one, matching the restored compaction mode.
pub(in crate::agent) fn restored_active_messages(
    messages: &[StoredMessage],
    checkpoint: &ContextCheckpoint,
) -> (Vec<ChatMessage>, bool) {
    let start = match checkpoint.through_message_id.as_deref() {
        Some(through) => match messages.iter().position(|message| message.id == through) {
            Some(index) => index.saturating_add(1),
            None => return (Vec::new(), false),
        },
        None => 0,
    };
    let mut restored = messages
        .iter()
        .skip(start)
        .map(|message| {
            let tool_calls: Vec<ToolCall> =
                serde_json::from_value(message.tool_calls.clone()).unwrap_or_default();
            ChatMessage {
                role: message.role.clone(),
                content: message.content.clone(),
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                },
                tool_call_id: message.tool_call_id.clone(),
            }
        })
        .collect::<Vec<_>>();
    if crate::context::context_checkpoint_restore_mode(checkpoint) == "summary"
        && let Some(summary) = &checkpoint.summary
    {
        restored.insert(0, ChatMessage::user(summary.clone()));
    }
    let ids = vec![None; restored.len()];
    let (restored, _) = normalize_tool_history(restored, ids);
    (restored, true)
}

pub(in crate::agent) async fn load_conversation(
    engine: &Engine,
    session_id: &str,
) -> Result<LoadedConversation> {
    let messages: Vec<StoredMessage> = engine.list_messages(session_id).await?;
    let mut start = 0usize;
    let mut summary = None;
    // A context.compacted event points at the last summarized message. The
    // summary text lives in the event; the active conversation becomes
    // [summary] + messages after that point. Older messages stay in the
    // database for search, replay, and transcript.
    if let Some(event) = engine
        .latest_event_of_kind(session_id, "context.compacted")
        .await?
    {
        let mode = event
            .payload
            .get("mode")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("summary");
        let through = event
            .payload
            .get("through_message_id")
            .and_then(serde_json::Value::as_str);
        let text = event
            .payload
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .filter(|text| !text.trim().is_empty());
        if let Some(through) = through
            && let Some(index) = messages.iter().position(|message| message.id == through)
        {
            start = index.saturating_add(1);
            if mode == "summary" {
                summary = text.map(str::to_string);
            }
        }
    }
    let mut ids = messages
        .iter()
        .skip(start)
        .map(|message| Some(message.id.clone()))
        .collect::<Vec<_>>();
    let mut messages = messages
        .into_iter()
        .skip(start)
        .map(|message| {
            let tool_calls: Vec<ToolCall> =
                serde_json::from_value(message.tool_calls).unwrap_or_default();
            ChatMessage {
                role: message.role,
                content: message.content,
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                },
                tool_call_id: message.tool_call_id,
            }
        })
        .collect::<Vec<_>>();
    if let Some(summary) = summary {
        // The event stores the already-prefixed summary text.
        messages.insert(0, ChatMessage::user(summary));
        ids.insert(0, None);
    }
    let (messages, ids) = normalize_tool_history(messages, ids);
    Ok(LoadedConversation { messages, ids })
}

pub(in crate::agent) fn parse_todos(input: &serde_json::Value) -> Result<Vec<TodoItem>> {
    let items = input
        .get("items")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow!("items is required"))?;
    if items.len() > 50 {
        return Err(anyhow!("at most 50 todo items are allowed"));
    }
    let mut todos = Vec::with_capacity(items.len());
    for item in items {
        let title = item
            .get("title")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|title| !title.is_empty() && title.len() <= 200)
            .ok_or_else(|| anyhow!("each item needs a non-empty title (max 200 chars)"))?;
        let status = item
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("pending");
        if !matches!(status, "pending" | "in_progress" | "completed") {
            return Err(anyhow!("status must be pending, in_progress, or completed"));
        }
        todos.push(TodoItem {
            title: title.to_string(),
            status: status.to_string(),
        });
    }
    Ok(todos)
}

#[cfg(test)]
mod tool_history_tests {
    use super::*;

    fn call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: "{}".into(),
        }
    }

    #[test]
    fn synthesizes_a_missing_result_and_drops_orphans() {
        let messages = vec![
            ChatMessage::user("go"),
            ChatMessage::assistant_tool_calls(
                "",
                vec![call("a", "read_file"), call("b", "write_file")],
            ),
            ChatMessage::tool("a", "ok"),
            ChatMessage::tool("orphan", "stale"),
            ChatMessage::user("continue"),
        ];
        let ids = vec![None; messages.len()];
        let (normalized, normalized_ids) = normalize_tool_history(messages, ids);
        assert_eq!(normalized.len(), 5);
        assert_eq!(normalized[2].tool_call_id.as_deref(), Some("a"));
        assert_eq!(normalized[3].tool_call_id.as_deref(), Some("b"));
        assert!(normalized[3].content.contains("did not complete"));
        assert_eq!(normalized[4].role, "user");
        assert_eq!(normalized_ids.len(), 5);
    }

    #[test]
    fn preserves_a_complete_tool_turn_and_its_message_ids() {
        let messages = vec![
            ChatMessage::user("go"),
            ChatMessage::assistant_tool_calls("", vec![call("a", "read_file")]),
            ChatMessage::tool("a", "ok"),
        ];
        let ids = vec![Some("m1".into()), Some("m2".into()), Some("m3".into())];
        let (normalized, normalized_ids) = normalize_tool_history(messages, ids);
        assert_eq!(normalized.len(), 3);
        assert_eq!(normalized[2].content, "ok");
        assert_eq!(normalized_ids[2].as_deref(), Some("m3"));
    }
}
