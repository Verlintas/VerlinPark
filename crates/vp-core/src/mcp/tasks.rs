//! Durable MCP asynchronous task handles.
//!
//! The daemon never polls on its own. `tasks/get`, `tasks/result`, and
//! `tasks/cancel` run only when an agent or operator explicitly asks. Form
//! elicitation during those requests is routed through the normal question
//! inbox by `mcp/elicitation.rs`.

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;
use vp_protocol::types::{
    McpCallParams, McpCallResult, McpServer, McpTask, McpTaskActionResult, McpTaskListParams,
    McpTaskListResult, McpTaskResult, McpTaskResultParams,
};

use crate::Engine;

pub const DEFAULT_TTL_SECS: u64 = 600;
pub const MIN_TTL_SECS: u64 = 30;
pub const MAX_TTL_SECS: u64 = 86_400;
pub const DEFAULT_POLL_INTERVAL_MS: u64 = 1_000;
pub const MIN_POLL_INTERVAL_MS: u64 = 100;
pub const MAX_POLL_INTERVAL_MS: u64 = 60_000;
pub const MAX_REMOTE_TASK_ID_CHARS: usize = 200;
pub const MAX_TASK_RESULT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Default)]
pub struct McpTaskContext {
    pub project_id: String,
    pub session_id: String,
    pub agent_id: String,
    pub agent_name: String,
    pub task_id: Option<String>,
    pub effect_receipt_id: Option<String>,
}

impl McpTaskContext {
    pub fn from_session(
        project_id: &str,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<&str>,
        effect_receipt_id: Option<&str>,
    ) -> Self {
        Self {
            project_id: project_id.to_string(),
            session_id: session_id.to_string(),
            agent_id: agent_id.to_string(),
            agent_name: agent_name.to_string(),
            task_id: task_id.map(str::to_string),
            effect_receipt_id: effect_receipt_id.map(str::to_string),
        }
    }

    fn has_session(&self) -> bool {
        !self.session_id.trim().is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskHandle {
    pub remote_task_id: String,
    pub status: String,
    pub ttl_secs: u64,
    pub poll_interval_ms: u64,
    pub last_error: Option<String>,
}

/// Detect an asynchronous task handle in a `tools/call` result. Supports the
/// common nested `{"task": {...}}` shape and a top-level `taskId`/`status`
/// object. Unknown status strings are treated as `working` rather than trusted.
pub fn parse_task_handle(result: &Value) -> Option<TaskHandle> {
    let object = result
        .get("task")
        .filter(|value| value.is_object())
        .unwrap_or(result);
    let explicit = result.get("task").is_some()
        || object.get("taskId").is_some()
        || object.get("task_id").is_some()
        || object.get("ttl").is_some()
        || object.get("ttlSecs").is_some()
        || object.get("pollInterval").is_some()
        || object.get("pollIntervalMs").is_some();
    if !explicit {
        return None;
    }
    let remote_task_id = object
        .get("taskId")
        .or_else(|| object.get("task_id"))
        .or_else(|| object.get("id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty() && id.chars().count() <= MAX_REMOTE_TASK_ID_CHARS)?
        .to_string();
    let status = normalize_status(
        object
            .get("status")
            .or_else(|| object.get("state"))
            .and_then(Value::as_str)
            .unwrap_or("working"),
    );
    let ttl_secs = object
        .get("ttlSecs")
        .or_else(|| object.get("ttl_seconds"))
        .or_else(|| object.get("ttl"))
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_TTL_SECS)
        .clamp(MIN_TTL_SECS, MAX_TTL_SECS);
    let poll_interval_ms = object
        .get("pollInterval")
        .or_else(|| object.get("pollIntervalMs"))
        .or_else(|| object.get("poll_interval_ms"))
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_POLL_INTERVAL_MS)
        .clamp(MIN_POLL_INTERVAL_MS, MAX_POLL_INTERVAL_MS);
    Some(TaskHandle {
        remote_task_id,
        status,
        ttl_secs,
        poll_interval_ms,
        last_error: object
            .get("error")
            .and_then(Value::as_str)
            .map(|error| error.chars().take(1_000).collect()),
    })
}

pub fn normalize_status(value: &str) -> String {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "working" | "running" | "pending" | "queued" | "in_progress" => "working",
        "input_required" | "needs_input" | "requires_input" => "input_required",
        "completed" | "complete" | "succeeded" | "success" => "completed",
        "failed" | "error" => "failed",
        "cancelled" | "canceled" => "cancelled",
        "expired" | "timeout" | "timed_out" => "expired",
        _ => "working",
    }
    .to_string()
}

pub fn is_pending(status: &str) -> bool {
    matches!(status, "working" | "input_required")
}

pub fn is_terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "expired")
}

fn task_status_event(status: &str) -> &'static str {
    match status {
        "completed" => "mcp.task.completed",
        "failed" => "mcp.task.failed",
        "cancelled" => "mcp.task.cancelled",
        "expired" => "mcp.task.expired",
        _ => "mcp.task.updated",
    }
}

fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

fn format_time(value: OffsetDateTime) -> String {
    value
        .format(&Rfc3339)
        .unwrap_or_else(|_| vp_protocol::now_rfc3339())
}

fn expires_after(ttl_secs: u64) -> String {
    format_time(now() + time::Duration::seconds(ttl_secs as i64))
}

fn task_from_handle(
    handle: &TaskHandle,
    server: &McpServer,
    tool_name: &str,
    context: &McpTaskContext,
) -> McpTask {
    let created = vp_protocol::now_rfc3339();
    McpTask {
        id: format!("mcpt_{}", Uuid::new_v4().simple()),
        project_id: context.project_id.clone(),
        session_id: context.session_id.clone(),
        agent_id: context.agent_id.clone(),
        agent_name: context.agent_name.clone(),
        task_id: context.task_id.clone(),
        server_id: server.id.clone(),
        server_name: server.name.clone(),
        tool_name: tool_name.to_string(),
        remote_task_id: handle.remote_task_id.clone(),
        status: handle.status.clone(),
        ttl_secs: handle.ttl_secs,
        poll_interval_ms: handle.poll_interval_ms,
        created_at: created.clone(),
        updated_at: created,
        expires_at: Some(expires_after(handle.ttl_secs)),
        result_available: false,
        effect_receipt_id: context.effect_receipt_id.clone(),
        last_error: handle.last_error.clone(),
    }
}

fn apply_remote_object(task: &mut McpTask, value: &Value) {
    let object = value
        .get("task")
        .filter(|value| value.is_object())
        .unwrap_or(value);
    if let Some(status) = object
        .get("status")
        .or_else(|| object.get("state"))
        .and_then(Value::as_str)
    {
        task.status = normalize_status(status);
    }
    if let Some(ttl) = object
        .get("ttlSecs")
        .or_else(|| object.get("ttl_seconds"))
        .or_else(|| object.get("ttl"))
        .and_then(Value::as_u64)
    {
        task.ttl_secs = ttl.clamp(MIN_TTL_SECS, MAX_TTL_SECS);
        task.expires_at = Some(expires_after(task.ttl_secs));
    }
    if let Some(poll) = object
        .get("pollInterval")
        .or_else(|| object.get("pollIntervalMs"))
        .or_else(|| object.get("poll_interval_ms"))
        .and_then(Value::as_u64)
    {
        task.poll_interval_ms = poll.clamp(MIN_POLL_INTERVAL_MS, MAX_POLL_INTERVAL_MS);
    }
    if let Some(expires) = object
        .get("expiresAt")
        .or_else(|| object.get("expires_at"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        task.expires_at = Some(expires.to_string());
    }
    task.last_error = object
        .get("error")
        .and_then(Value::as_str)
        .map(|error| error.chars().take(1_000).collect())
        .or_else(|| task.last_error.clone());
    task.updated_at = vp_protocol::now_rfc3339();
}

fn extract_task_result(value: &Value) -> Option<Value> {
    value.get("result").cloned().or_else(|| {
        let object = value
            .get("task")
            .filter(|value| value.is_object())
            .unwrap_or(value);
        object.get("result").cloned()
    })
}

fn task_result_payload(value: &Value) -> Result<(String, String, u64)> {
    let encoded = serde_json::to_vec(value)?;
    if encoded.len() > MAX_TASK_RESULT_BYTES {
        bail!(
            "MCP task result exceeds {} KiB",
            MAX_TASK_RESULT_BYTES / 1024
        );
    }
    let hash = crate::sha256_hex(&encoded);
    Ok((serde_json::to_string(value)?, hash, encoded.len() as u64))
}

async fn expire_if_needed(engine: &Engine, task: McpTask) -> McpTask {
    if !is_pending(&task.status) {
        return task;
    }
    let Some(expires_at) = task.expires_at.as_deref() else {
        return task;
    };
    let Ok(expires) = OffsetDateTime::parse(expires_at, &Rfc3339) else {
        return task;
    };
    if expires > now() {
        return task;
    }
    let mut expired = task;
    expired.status = "expired".to_string();
    expired.updated_at = vp_protocol::now_rfc3339();
    expired.last_error = Some("local MCP task deadline expired".to_string());
    let store = engine.store().clone();
    let row = expired.clone();
    let updated = tokio::task::spawn_blocking(move || {
        store.update_mcp_task_with_event(&row, "mcp.task.expired", json!({}))
    })
    .await
    .ok()
    .and_then(Result::ok)
    .map(|(task, _)| task);
    updated.unwrap_or(expired)
}

impl Engine {
    pub async fn mcp_call_for_params(&self, params: McpCallParams) -> Result<McpCallResult> {
        let mut context = McpTaskContext::default();
        if let Some(session_id) = params.session_id.as_deref() {
            let store = self.store.clone();
            let id = session_id.to_string();
            let session = tokio::task::spawn_blocking(move || store.get_session(&id))
                .await??
                .ok_or_else(|| anyhow!("session not found: {session_id}"))?;
            context = McpTaskContext::from_session(
                &session.project_id,
                &session.id,
                params.agent_id.as_deref().unwrap_or("operator"),
                params.agent_name.as_deref().unwrap_or("Operator"),
                params.task_id.as_deref(),
                params.effect_receipt_id.as_deref(),
            );
        }
        self.mcp_call_with_context(&params.server_id, &params.tool, params.arguments, context)
            .await
    }

    pub async fn mcp_call_with_context(
        &self,
        server_id: &str,
        tool: &str,
        arguments: Value,
        context: McpTaskContext,
    ) -> Result<McpCallResult> {
        let server = self.mcp_server_or_error(server_id).await?;
        let handler = context.has_session().then(|| {
            crate::mcp::elicitation::request_handler(
                self.clone(),
                context.session_id.clone(),
                context.agent_id.clone(),
                context.agent_name.clone(),
                server.id.clone(),
                server.name.clone(),
                tool.to_string(),
            )
        });
        let result = self
            .mcp
            .call_tool_with_handler(&server, tool, arguments, handler)
            .await?;
        let mut task = None;
        if let Some(handle) = parse_task_handle(&result)
            && context.has_session()
        {
            {
                let store = self.store.clone();
                let server_id = server.id.clone();
                let remote_task_id = handle.remote_task_id.clone();
                let session_id = context.session_id.clone();
                let existing = tokio::task::spawn_blocking(move || {
                    store.find_mcp_task_by_remote(&session_id, &server_id, &remote_task_id)
                })
                .await??;
                let row = if let Some(existing) = existing {
                    existing
                } else {
                    let row = task_from_handle(&handle, &server, tool, &context);
                    let store = self.store.clone();
                    let row_clone = row.clone();
                    let (stored, _) = tokio::task::spawn_blocking(move || {
                        store.insert_mcp_task_with_event(&row_clone)
                    })
                    .await??;
                    stored
                };
                task = Some(row);
            }
        }
        Ok(McpCallResult {
            server_id: server_id.to_string(),
            tool: tool.to_string(),
            result,
            task,
        })
    }

    pub async fn mcp_task_list(&self, params: McpTaskListParams) -> Result<McpTaskListResult> {
        let store = self.store.clone();
        let mut rows = tokio::task::spawn_blocking(move || {
            store.list_mcp_tasks(
                params.project_id.as_deref(),
                params.session_id.as_deref(),
                params.server_id.as_deref(),
                params.status.as_deref(),
                params.limit.unwrap_or(100),
            )
        })
        .await??;
        let mut expired = Vec::new();
        for task in rows.0.drain(..) {
            expired.push(expire_if_needed(self, task).await);
        }
        Ok(McpTaskListResult {
            tasks: expired,
            truncated: rows.1,
        })
    }

    pub async fn mcp_task_get(&self, task_id: &str, refresh: bool) -> Result<McpTask> {
        let task = self.mcp_task_row(task_id).await?;
        let task = expire_if_needed(self, task).await;
        if !refresh || is_terminal(&task.status) {
            return Ok(task);
        }
        let server = self.mcp_server_or_error(&task.server_id).await?;
        let handler = self.task_handler(&task, &server);
        let result = self
            .mcp
            .task_request(
                &server,
                "tasks/get",
                json!({"taskId": task.remote_task_id}),
                handler,
            )
            .await?;
        let mut next = task;
        apply_remote_object(&mut next, &result);
        self.persist_task_update(next, json!({"source": "tasks/get"}))
            .await
    }

    pub async fn mcp_task_result(&self, params: McpTaskResultParams) -> Result<McpTaskResult> {
        let task = self.mcp_task_row(&params.task_id).await?;
        let task = expire_if_needed(self, task).await;
        let refresh = params.refresh.unwrap_or(true);
        if task.result_available {
            let cached = self.cached_task_result(&task.id).await?;
            return Ok(task_result(&task, cached, false));
        }
        if !refresh || (is_terminal(&task.status) && task.status != "completed") {
            return Ok(task_result(&task, None, is_pending(&task.status)));
        }
        let server = self.mcp_server_or_error(&task.server_id).await?;
        let handler = self.task_handler(&task, &server);
        let value = self
            .mcp
            .task_request(
                &server,
                "tasks/result",
                json!({"taskId": task.remote_task_id}),
                handler,
            )
            .await?;
        let mut next = task;
        apply_remote_object(&mut next, &value);
        if let Some(result) = extract_task_result(&value) {
            if is_pending(&next.status) {
                next.status = "completed".to_string();
            }
            let (json, hash, bytes) = task_result_payload(&result)?;
            let store = self.store.clone();
            let row = next.clone();
            let (stored, _) = tokio::task::spawn_blocking(move || {
                store.set_mcp_task_result_with_event(&row, &json, &hash, bytes)
            })
            .await??;
            return Ok(task_result(&stored, Some(result), false));
        }
        let stored = self
            .persist_task_update(next, json!({"source": "tasks/result"}))
            .await?;
        Ok(task_result(&stored, None, is_pending(&stored.status)))
    }

    pub async fn mcp_task_cancel(
        &self,
        task_id: &str,
        reason: Option<&str>,
    ) -> Result<McpTaskActionResult> {
        let task = self.mcp_task_row(task_id).await?;
        if is_terminal(&task.status) {
            return Ok(McpTaskActionResult {
                task,
                cancelled: false,
                message: "task is already terminal".to_string(),
            });
        }
        let server = self.mcp_server_or_error(&task.server_id).await?;
        let handler = self.task_handler(&task, &server);
        let value = self
            .mcp
            .task_request(
                &server,
                "tasks/cancel",
                json!({"taskId": task.remote_task_id, "reason": reason}),
                handler,
            )
            .await?;
        let mut next = task;
        apply_remote_object(&mut next, &value);
        if next.status == "working" || next.status == "input_required" {
            next.status = "cancelled".to_string();
        }
        next.last_error = reason.map(|reason| reason.chars().take(1_000).collect());
        let kind = task_status_event(&next.status);
        let stored = self
            .persist_task_update_with_kind(next, json!({"source": "tasks/cancel"}), kind)
            .await?;
        Ok(McpTaskActionResult {
            cancelled: stored.status == "cancelled",
            task: stored,
            message: "MCP task cancellation requested".to_string(),
        })
    }

    async fn mcp_task_row(&self, task_id: &str) -> Result<McpTask> {
        let store = self.store.clone();
        let requested = task_id.trim().to_string();
        if requested.is_empty() {
            bail!("task_id is required");
        }
        let lookup = requested.clone();
        tokio::task::spawn_blocking(move || store.get_mcp_task(&lookup))
            .await??
            .ok_or_else(|| anyhow!("MCP task not found: {requested}"))
    }

    async fn mcp_server_or_error(&self, server_id: &str) -> Result<McpServer> {
        let store = self.store.clone();
        let requested = server_id.to_string();
        let lookup = requested.clone();
        tokio::task::spawn_blocking(move || store.get_mcp_server(&lookup))
            .await??
            .ok_or_else(|| anyhow!("MCP server not found: {requested}"))
    }

    fn task_handler(
        &self,
        task: &McpTask,
        server: &McpServer,
    ) -> Option<crate::mcp::elicitation::ServerRequestHandler> {
        if task.session_id.trim().is_empty() {
            return None;
        }
        Some(crate::mcp::elicitation::request_handler(
            self.clone(),
            task.session_id.clone(),
            task.agent_id.clone(),
            task.agent_name.clone(),
            server.id.clone(),
            server.name.clone(),
            task.tool_name.clone(),
        ))
    }

    async fn persist_task_update(&self, task: McpTask, extra: Value) -> Result<McpTask> {
        let kind = task_status_event(&task.status);
        self.persist_task_update_with_kind(task, extra, kind).await
    }

    async fn persist_task_update_with_kind(
        &self,
        task: McpTask,
        extra: Value,
        kind: &'static str,
    ) -> Result<McpTask> {
        let store = self.store.clone();
        let (stored, _) = tokio::task::spawn_blocking(move || {
            store.update_mcp_task_with_event(&task, kind, extra)
        })
        .await??;
        Ok(stored)
    }

    async fn cached_task_result(&self, task_id: &str) -> Result<Option<Value>> {
        let store = self.store.clone();
        let task_id = task_id.to_string();
        let row = tokio::task::spawn_blocking(move || store.mcp_task_result(&task_id)).await??;
        let Some((json, _hash, _bytes)) = row else {
            return Ok(None);
        };
        if json.trim().is_empty() {
            return Ok(None);
        }
        Ok(serde_json::from_str(&json).ok())
    }
}

fn task_result(task: &McpTask, result: Option<Value>, pending: bool) -> McpTaskResult {
    McpTaskResult {
        task: task.clone(),
        result,
        pending,
        result_truncated: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_handle_supports_nested_and_top_level_forms() {
        let nested = parse_task_handle(&json!({
            "task": {"taskId": "remote-1", "status": "input-required", "ttl": 120, "pollInterval": 500}
        }))
        .unwrap();
        assert_eq!(nested.remote_task_id, "remote-1");
        assert_eq!(nested.status, "input_required");
        assert_eq!(nested.ttl_secs, 120);
        assert_eq!(nested.poll_interval_ms, 500);
        let top = parse_task_handle(&json!({"taskId": "remote-2", "state": "completed"})).unwrap();
        assert_eq!(top.remote_task_id, "remote-2");
        assert_eq!(top.status, "completed");
        assert!(parse_task_handle(&json!({"content": []})).is_none());
    }

    #[test]
    fn status_normalization_is_bounded() {
        for (raw, expected) in [
            ("RUNNING", "working"),
            ("needs-input", "input_required"),
            ("succeeded", "completed"),
            ("canceled", "cancelled"),
            ("timed-out", "expired"),
            ("surprising", "working"),
        ] {
            assert_eq!(normalize_status(raw), expected);
        }
    }

    #[test]
    fn task_result_is_bounded_and_hashed() {
        let (json, hash, bytes) =
            task_result_payload(&json!({"content": [{"type": "text", "text": "ok"}]})).unwrap();
        assert!(json.contains("\"ok\""));
        assert_eq!(hash.len(), 64);
        assert!(bytes > 0);
        let oversized = Value::String("x".repeat(MAX_TASK_RESULT_BYTES + 1));
        assert!(task_result_payload(&oversized).is_err());
    }
}
