//! Durable run journal and crash resume.
//!
//! A run is journaled before a provider request or tool invocation (intent)
//! and again after its outcome is durable (completion). The run/step/checkpoint
//! projections are rebuilt from events. Tool outcome rows are deliberately
//! operational: they make resume safe but are not replayed as authoritative
//! state, because a tool may have changed the world even though the daemon died
//! before committing the row.
//!
//! The exactly-once boundary is narrow and explicit:
//! - a provider call is never exactly-once; a completed response is only
//!   replayed when the assistant message itself was persisted;
//! - a completed write tool is hard-blocked by name + canonical input
//!   fingerprint within a resume lineage and returns the journaled result;
//! - shell/process/local_http outcomes are never assumed; an interrupted call
//!   is reported as unknown and the model must verify state;
//! - tool outcomes are not rebuilt by replay/repair. Losing that table loses
//!   the duplicate-write gate for old runs, so the daemon fails closed on an
//!   unreadable journal before resuming a run.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;
use vp_protocol::now_rfc3339;
use vp_protocol::types::{
    Event, RunBudgetSnapshot, RunCheckpoint, RunFailure, RunStep, RunToolCall, Session, SessionRun,
    SessionRunPolicy,
};
use vp_providers::ChatRequest;

use crate::Engine;
use crate::tools::ToolOutput;

/// These tools mutate files or worktree metadata. A completed journal entry is
/// never silently executed a second time within the same resume lineage.
pub(crate) const WRITE_TOOLS: &[&str] = &["write_file", "edit_file", "apply_patch", "file_manage"];

/// Side-effecting or ambiguous tools that cannot be safely classified as pure
/// reads. They are not hard-blocked (the journal cannot prove what happened),
/// but interrupted/missing outcomes are surfaced as unknown.
pub(crate) const AMBIGUOUS_TOOLS: &[&str] = &["shell", "process", "local_http", "browser"];

const MAX_OBJECTIVE_CHARS: usize = 500;
const MAX_RESULT_CHARS: usize = 8_000;
const MAX_NOTE_CHARS: usize = 4_000;
const MAX_NOTE_TOOLS: usize = 24;
const MAX_ERROR_CHARS: usize = 2_000;
const STEP_KINDS: &[&str] = &[
    "model_request",
    "tool_call",
    "compaction",
    "approval",
    "question",
];

/// The in-memory handle for one running attempt. It owns the durable run row
/// identity and, for a resumed run, the duplicate-write gate.
pub(crate) struct RunJournal {
    pub run: SessionRun,
    worktree: PathBuf,
    gate: Option<ResumeGate>,
    finished: std::sync::atomic::AtomicBool,
}

impl RunJournal {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn begin(
        engine: &Engine,
        session: &Session,
        project_id: &str,
        agent_id: &str,
        agent_name: &str,
        agent_role: &str,
        task_id: Option<&str>,
        department: Option<&str>,
        objective: &str,
        provider: &str,
        model: &str,
        worktree: &Path,
        budget: RunBudgetSnapshot,
        top_level: bool,
    ) -> Result<Self> {
        let run_id = Uuid::now_v7().to_string();
        let now = now_rfc3339();
        let head = worktree_head(worktree.to_path_buf()).await;
        let plan_revision = engine.current_plan_revision(&session.id).await?;
        let run = SessionRun {
            run_id: run_id.clone(),
            session_id: session.id.clone(),
            project_id: project_id.to_string(),
            agent_id: agent_id.to_string(),
            agent_name: agent_name.chars().take(120).collect(),
            agent_role: agent_role.to_string(),
            task_id: task_id.map(str::to_string),
            department: department.map(str::to_string),
            objective: truncate_chars(objective, MAX_OBJECTIVE_CHARS),
            provider: provider.to_string(),
            model: model.to_string(),
            status: "running".to_string(),
            attempt: 1,
            resumed_from_run_id: None,
            resume_root_id: run_id,
            top_level,
            started_at: now.clone(),
            updated_at: now,
            ended_at: None,
            budget,
            worktree_head: head,
            plan_revision,
            last_event_seq: 0,
            failure: None,
        };
        let store = engine.store().clone();
        let stored = tokio::task::spawn_blocking(move || store.start_session_run(&run)).await??;
        engine.broadcast_event(stored.1);
        Ok(Self {
            run: stored.0,
            worktree: worktree.to_path_buf(),
            gate: None,
            finished: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Build the handle for a successor attempt after `mark_session_run_resumed`
    /// has already inserted the new run row and its events.
    pub(crate) fn resumed(run: SessionRun, worktree: PathBuf, gate: Option<ResumeGate>) -> Self {
        Self {
            run,
            worktree,
            gate,
            finished: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(crate) fn id(&self) -> &str {
        &self.run.run_id
    }

    pub(crate) async fn checkpoint(
        &self,
        engine: &Engine,
        turn: u32,
        budget: RunBudgetSnapshot,
    ) -> Result<RunCheckpoint> {
        let (event_cursor, message_cursor) = journal_cursors(engine, &self.run.session_id).await?;
        let checkpoint = RunCheckpoint {
            checkpoint_id: Uuid::now_v7().to_string(),
            run_id: self.run.run_id.clone(),
            session_id: self.run.session_id.clone(),
            seq: 0,
            turn,
            message_cursor,
            event_cursor,
            budget,
            worktree_head: worktree_head(self.worktree.clone()).await,
            plan_revision: engine.current_plan_revision(&self.run.session_id).await?,
            created_at: now_rfc3339(),
        };
        let store = engine.store().clone();
        let stored =
            tokio::task::spawn_blocking(move || store.record_run_checkpoint(&checkpoint)).await??;
        engine.broadcast_event(stored.1.clone());
        Ok(stored.0)
    }

    pub(crate) async fn model_intent(
        &self,
        engine: &Engine,
        turn: u32,
        request: &ChatRequest,
    ) -> Result<RunStep> {
        let input_hash = hash_request(request)?;
        let step = RunStep {
            step_id: Uuid::now_v7().to_string(),
            run_id: self.run.run_id.clone(),
            session_id: self.run.session_id.clone(),
            seq: 0,
            turn,
            kind: "model_request".to_string(),
            status: "intent".to_string(),
            input_hash,
            output_hash: String::new(),
            payload: serde_json::json!({
                "turn": turn,
                "model": request.model,
                "messages": request.messages.len(),
                "tools": request.tools.len(),
                "max_tokens": request.max_tokens,
                "system_chars": request.system.as_ref().map(|system| system.chars().count()),
            }),
            created_at: now_rfc3339(),
            updated_at: now_rfc3339(),
        };
        let store = engine.store().clone();
        let stored = tokio::task::spawn_blocking(move || store.record_run_step_intent(&step, None))
            .await??;
        for event in &stored.1 {
            engine.broadcast_event(event.clone());
        }
        Ok(stored.0)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn model_completed(
        &self,
        engine: &Engine,
        step: &RunStep,
        assistant_message_id: Option<&str>,
        text: &str,
        tool_calls: &[vp_providers::ToolCall],
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_write_tokens: u64,
        estimated: bool,
    ) -> Result<()> {
        let mut completed = step.clone();
        completed.status = "completed".to_string();
        completed.output_hash = hash_json(&serde_json::json!({
            "text": text,
            "tool_calls": tool_calls,
        }))?;
        completed.payload = serde_json::json!({
            "turn": step.turn,
            "assistant_message_id": assistant_message_id,
            "text_chars": text.chars().count(),
            "tool_calls": tool_calls.iter().map(|call| call.name.clone()).collect::<Vec<_>>(),
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "cache_read_tokens": cache_read_tokens,
            "cache_write_tokens": cache_write_tokens,
            "estimated": estimated,
        });
        completed.updated_at = now_rfc3339();
        self.complete_step(engine, completed, None).await?;
        Ok(())
    }

    pub(crate) async fn model_failed(
        &self,
        engine: &Engine,
        step: &RunStep,
        error: &str,
    ) -> Result<()> {
        let mut completed = step.clone();
        completed.status = "failed".to_string();
        completed.payload = serde_json::json!({
            "turn": step.turn,
            "error": truncate_chars(error, MAX_ERROR_CHARS),
        });
        completed.updated_at = now_rfc3339();
        self.complete_step(engine, completed, None).await?;
        Ok(())
    }

    pub(crate) async fn tool_intent(
        &self,
        engine: &Engine,
        turn: u32,
        tool_call_id: &str,
        tool_name: &str,
        input: &Value,
    ) -> Result<(RunStep, RunToolCall)> {
        let fingerprint = tool_fingerprint(tool_name, input)?;
        let now = now_rfc3339();
        let step = RunStep {
            step_id: Uuid::now_v7().to_string(),
            run_id: self.run.run_id.clone(),
            session_id: self.run.session_id.clone(),
            seq: 0,
            turn,
            kind: "tool_call".to_string(),
            status: "intent".to_string(),
            input_hash: fingerprint.clone(),
            output_hash: String::new(),
            payload: serde_json::json!({
                "turn": turn,
                "tool_call_id": tool_call_id,
                "tool_name": tool_name,
            }),
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        let tool = RunToolCall {
            run_id: self.run.run_id.clone(),
            session_id: self.run.session_id.clone(),
            step_id: step.step_id.clone(),
            tool_call_id: tool_call_id.to_string(),
            tool_name: tool_name.to_string(),
            input_fingerprint: fingerprint,
            status: "intent".to_string(),
            result: None,
            error: None,
            consumed_by_run_id: None,
            consumed_at: None,
            created_at: now.clone(),
            updated_at: now,
        };
        let store = engine.store().clone();
        let tool_for_store = tool.clone();
        let stored = tokio::task::spawn_blocking(move || {
            store.record_run_step_intent(&step, Some(&tool_for_store))
        })
        .await??;
        for event in &stored.1 {
            engine.broadcast_event(event.clone());
        }
        // The store assigned a sequence; return the tool row that matches the
        // stored step identity (the input tool row is unchanged apart from seq).
        Ok((stored.0, tool))
    }

    pub(crate) async fn tool_completed(
        &self,
        engine: &Engine,
        step: &RunStep,
        tool: &RunToolCall,
        output: &ToolOutput,
    ) -> Result<()> {
        let mut completed_step = step.clone();
        completed_step.status = if output.is_error {
            "failed".to_string()
        } else {
            "completed".to_string()
        };
        completed_step.output_hash = hash_json(&serde_json::json!({
            "content": output.content,
            "is_error": output.is_error,
        }))?;
        completed_step.payload = serde_json::json!({
            "turn": step.turn,
            "tool_call_id": tool.tool_call_id,
            "tool_name": tool.tool_name,
            "is_error": output.is_error,
            "error_code": output
                .failure
                .as_ref()
                .map(|failure| failure.code.as_str()),
            "retryable": output
                .failure
                .as_ref()
                .is_some_and(|failure| failure.retryable),
        });
        completed_step.updated_at = now_rfc3339();
        let mut completed_tool = tool.clone();
        completed_tool.status = completed_step.status.clone();
        completed_tool.result = Some(bounded_tool_result(output));
        completed_tool.error = output
            .is_error
            .then(|| truncate_chars(&output.content, MAX_ERROR_CHARS));
        completed_tool.updated_at = completed_step.updated_at.clone();
        self.complete_step(engine, completed_step, Some(completed_tool))
            .await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn record_compaction(
        &self,
        engine: &Engine,
        turn: u32,
        before_event_cursor: u64,
        after_event_cursor: u64,
        messages: usize,
    ) -> Result<()> {
        let step = self
            .simple_intent(
                engine,
                turn,
                "compaction",
                serde_json::json!({
                    "turn": turn,
                    "before_event_cursor": before_event_cursor,
                    "after_event_cursor": after_event_cursor,
                    "messages": messages,
                }),
            )
            .await?;
        self.simple_completed(engine, &step, "compacted").await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn approval_intent(
        &self,
        engine: &Engine,
        turn: u32,
        approval_id: &str,
        tool_name: &str,
        tool_call_id: &str,
        risk: &str,
        reason: &str,
    ) -> Result<RunStep> {
        self.simple_intent(
            engine,
            turn,
            "approval",
            serde_json::json!({
                "turn": turn,
                "approval_id": approval_id,
                "tool_name": tool_name,
                "tool_call_id": tool_call_id,
                "risk": risk,
                "reason": truncate_chars(reason, 500),
            }),
        )
        .await
    }

    pub(crate) async fn approval_completed(
        &self,
        engine: &Engine,
        step: &RunStep,
        outcome: &str,
    ) -> Result<()> {
        self.simple_completed(engine, step, outcome).await
    }

    pub(crate) async fn question_intent(
        &self,
        engine: &Engine,
        turn: u32,
        question_id: &str,
        question: &str,
        options: &[String],
    ) -> Result<RunStep> {
        self.simple_intent(
            engine,
            turn,
            "question",
            serde_json::json!({
                "turn": turn,
                "question_id": question_id,
                "question": truncate_chars(question, 500),
                "options": options,
            }),
        )
        .await
    }

    pub(crate) async fn question_completed(
        &self,
        engine: &Engine,
        step: &RunStep,
        outcome: &str,
    ) -> Result<()> {
        self.simple_completed(engine, step, outcome).await
    }

    async fn simple_intent(
        &self,
        engine: &Engine,
        turn: u32,
        kind: &str,
        payload: Value,
    ) -> Result<RunStep> {
        validate_step(kind, "intent")?;
        let now = now_rfc3339();
        let step = RunStep {
            step_id: Uuid::now_v7().to_string(),
            run_id: self.run.run_id.clone(),
            session_id: self.run.session_id.clone(),
            seq: 0,
            turn,
            kind: kind.to_string(),
            status: "intent".to_string(),
            input_hash: hash_json(&payload)?,
            output_hash: String::new(),
            payload,
            created_at: now.clone(),
            updated_at: now,
        };
        let store = engine.store().clone();
        let stored = tokio::task::spawn_blocking(move || store.record_run_step_intent(&step, None))
            .await??;
        for event in &stored.1 {
            engine.broadcast_event(event.clone());
        }
        Ok(stored.0)
    }

    async fn simple_completed(&self, engine: &Engine, step: &RunStep, outcome: &str) -> Result<()> {
        let mut completed = step.clone();
        completed.status = "completed".to_string();
        completed.output_hash = hash_json(&serde_json::json!({"outcome": outcome}))?;
        if let Some(object) = completed.payload.as_object_mut() {
            object.insert("outcome".to_string(), Value::String(outcome.to_string()));
        }
        completed.updated_at = now_rfc3339();
        self.complete_step(engine, completed, None).await
    }

    async fn complete_step(
        &self,
        engine: &Engine,
        step: RunStep,
        tool: Option<RunToolCall>,
    ) -> Result<()> {
        let store = engine.store().clone();
        let stored = tokio::task::spawn_blocking(move || {
            store.record_run_step_completed(&step, tool.as_ref())
        })
        .await??;
        for event in &stored.2 {
            engine.broadcast_event(event.clone());
        }
        Ok(())
    }

    pub(crate) async fn finish_completed(&self, engine: &Engine) -> Result<()> {
        self.finish(engine, "completed", None).await
    }

    pub(crate) async fn finish_failed(
        &self,
        engine: &Engine,
        kind: &str,
        message: &str,
    ) -> Result<()> {
        let failure = RunFailure {
            kind: kind.to_string(),
            message: truncate_chars(message, MAX_ERROR_CHARS),
        };
        self.finish(engine, "failed", Some(failure)).await
    }

    pub(crate) async fn finish_interrupted(
        &self,
        engine: &Engine,
        kind: &str,
        message: &str,
    ) -> Result<()> {
        let failure = RunFailure {
            kind: kind.to_string(),
            message: truncate_chars(message, MAX_ERROR_CHARS),
        };
        self.finish(engine, "interrupted", Some(failure)).await
    }

    async fn finish(
        &self,
        engine: &Engine,
        status: &str,
        failure: Option<RunFailure>,
    ) -> Result<()> {
        if self
            .finished
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(());
        }
        let store = engine.store().clone();
        let run_id = self.run.run_id.clone();
        let failure_for_store = failure.clone();
        let status = status.to_string();
        let stored = tokio::task::spawn_blocking(move || {
            store.finish_session_run(&run_id, &status, failure_for_store.as_ref())
        })
        .await??;
        engine.broadcast_event(stored.1);
        Ok(())
    }

    /// Ensure a reconstructed resume conversation is provider-valid: every
    /// assistant tool call in the persisted history must be followed by a tool
    /// result. Missing results are filled from the operational journal when
    /// possible, otherwise with an explicit unknown-outcome message. Synthetic
    /// results are for the provider request only; they are not persisted as
    /// session messages.
    pub(crate) async fn repair_resumed_conversation(
        &self,
        engine: &Engine,
        messages: &mut Vec<vp_providers::ChatMessage>,
    ) -> Result<()> {
        let store = engine.store().clone();
        let run_id = self.run.run_id.clone();
        let tools = tokio::task::spawn_blocking(move || store.list_run_lineage_tool_calls(&run_id))
            .await??;
        let mut by_call = std::collections::HashMap::new();
        for tool in tools {
            by_call.insert(tool.tool_call_id.clone(), tool);
        }
        let mut rebuilt = Vec::with_capacity(messages.len());
        let mut index = 0usize;
        while index < messages.len() {
            let message = messages[index].clone();
            rebuilt.push(message.clone());
            index += 1;
            let Some(calls) = message
                .tool_calls
                .as_ref()
                .filter(|_| message.role == "assistant")
            else {
                continue;
            };
            let mut end = index;
            let mut existing = std::collections::HashMap::new();
            while end < messages.len() && messages[end].role == "tool" {
                if let Some(id) = &messages[end].tool_call_id {
                    existing.insert(id.clone(), messages[end].clone());
                }
                end += 1;
            }
            for call in calls {
                if let Some(message) = existing.remove(&call.id) {
                    rebuilt.push(message);
                    continue;
                }
                let content = match by_call
                    .get(&call.id)
                    .filter(|tool| tool.tool_name == call.name)
                {
                    Some(tool) if tool.status == "completed" => {
                        let result = tool
                            .result
                            .as_ref()
                            .and_then(|result| result.get("content"))
                            .and_then(Value::as_str)
                            .unwrap_or("(journal result unavailable)");
                        format!(
                            "{}\n\n[durable journal] This tool call already completed in an interrupted attempt. The result above is recovered from the journal; the tool was not re-run.",
                            truncate_chars(result, MAX_RESULT_CHARS)
                        )
                    }
                    Some(tool) if tool.status == "failed" => format!(
                        "{}\n\n[durable journal] This tool call failed in an interrupted attempt and was not re-run.",
                        tool.error
                            .as_deref()
                            .map(|error| truncate_chars(error, MAX_ERROR_CHARS))
                            .unwrap_or_else(|| "tool failed".to_string())
                    ),
                    Some(_) => "Unknown outcome: the daemon stopped before this tool call committed a result. Verify the workspace/process state before assuming success. The tool was not re-run.".to_string(),
                    None => "Unknown outcome: no durable journal row exists for this tool call. Verify the workspace/process state before assuming success.".to_string(),
                };
                rebuilt.push(vp_providers::ChatMessage::tool(call.id.clone(), content));
            }
            index = end;
        }
        *messages = rebuilt;
        Ok(())
    }

    /// Check the duplicate-write gate before executing a tool. Returns a
    /// journaled result when this exact write already completed in a prior
    /// attempt. Each prior row can be consumed once per attempt; interrupting
    /// or failing the attempt releases its claims so a later resume can return
    /// the result again instead of re-running the write.
    pub(crate) async fn check_duplicate_write(
        &self,
        engine: &Engine,
        tool_name: &str,
        input: &Value,
    ) -> Result<Option<ToolOutput>> {
        let Some(gate) = &self.gate else {
            return Ok(None);
        };
        if !WRITE_TOOLS.contains(&tool_name) {
            return Ok(None);
        }
        let fingerprint = tool_fingerprint(tool_name, input)?;
        let store = engine.store().clone();
        let run_id = self.run.run_id.clone();
        let tool_name = tool_name.to_string();
        let gate_run_id = gate.run_id.clone();
        let previous = tokio::task::spawn_blocking(move || {
            store.claim_completed_tool_result(&run_id, &tool_name, &fingerprint, &gate_run_id)
        })
        .await??;
        let Some(previous) = previous else {
            return Ok(None);
        };
        let result = previous.result.as_ref();
        let content = result
            .and_then(|result| result.get("content"))
            .and_then(Value::as_str)
            .unwrap_or("(the journal recorded a completed write with no stored result)");
        let is_error = result
            .and_then(|result| result.get("is_error"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let note = format!(
            "\n\n[durable journal] This exact {} call already completed in interrupted run {}. It was not re-run. The journaled result is returned instead. Do not repeat the same write; inspect the file if you need its current content.",
            previous.tool_name, previous.run_id
        );
        let mut output = if is_error {
            ToolOutput::error(format!("{content}{note}"))
        } else {
            ToolOutput::ok(format!("{content}{note}"))
        };
        output.structured = Some(serde_json::json!({
            "journal_replay": true,
            "source_run_id": previous.run_id,
            "tool_name": previous.tool_name,
        }));
        Ok(Some(output))
    }
}

#[derive(Clone)]
pub(crate) struct ResumeGate {
    pub run_id: String,
}

/// Everything needed to create and launch a resumed attempt after preflight.
#[derive(Debug)]
pub(crate) struct ResumePlan {
    pub previous: SessionRun,
    pub note: String,
}

/// Validate every durable precondition before a new provider call. This
/// function never mutates the worktree and never resets user changes.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn preflight(
    engine: &Engine,
    session: &Session,
    run: &SessionRun,
    policy: &SessionRunPolicy,
    provider: &str,
    model: &str,
    now: OffsetDateTime,
) -> Result<ResumePlan> {
    if !matches!(session.status.as_str(), "idle" | "failed" | "cancelled") {
        return Err(anyhow!(
            "session {} is {}; only idle, failed, or cancelled sessions may resume a run",
            session.id,
            session.status
        ));
    }
    let imported_store = engine.store().clone();
    let imported_session = session.id.clone();
    let imported = tokio::task::spawn_blocking(move || {
        imported_store.session_has_imported_event(&imported_session)
    })
    .await??;
    if imported {
        return Err(anyhow!(
            "session {} was imported or rehydrated from an archive; operational tool outcomes are not part of a bundle, so a run cannot be resumed safely. Start a new prompt instead.",
            session.id
        ));
    }
    if !matches!(run.status.as_str(), "interrupted" | "failed") {
        return Err(anyhow!(
            "run {} is {}; only interrupted or failed runs can be resumed",
            run.run_id,
            run.status
        ));
    }
    if run.task_id.is_some() || !run.top_level {
        return Err(anyhow!(
            "run {} is task-scoped; retry it through the plan task/retry path, not session.run.resume",
            run.run_id
        ));
    }
    if run
        .failure
        .as_ref()
        .is_some_and(|failure| failure.kind == "cancelled")
    {
        return Err(anyhow!(
            "run {} was cancelled by an operator and is not resumable",
            run.run_id
        ));
    }
    if run
        .failure
        .as_ref()
        .is_some_and(|failure| failure.kind == "superseded")
    {
        return Err(anyhow!(
            "run {} was superseded by a newer prompt in the same session",
            run.run_id
        ));
    }
    if run.attempt >= policy.max_attempts {
        return Err(anyhow!(
            "run {} has used {}/{} attempts",
            run.run_id,
            run.attempt,
            policy.max_attempts
        ));
    }
    let started = OffsetDateTime::parse(&run.started_at, &Rfc3339)
        .map_err(|error| anyhow!("run {} start time is invalid: {error}", run.run_id))?;
    let age = now - started;
    if age > time::Duration::hours(i64::from(policy.max_age_hours)) {
        return Err(anyhow!(
            "run {} is {} hour(s) old; the policy allows at most {}",
            run.run_id,
            age.whole_hours().max(0),
            policy.max_age_hours
        ));
    }
    if run.provider != provider {
        return Err(anyhow!(
            "run {} was recorded with provider '{}', but the configured provider is '{}'",
            run.run_id,
            run.provider,
            provider
        ));
    }
    if run.model != model {
        return Err(anyhow!(
            "run {} was recorded with model '{}', but the configured model for this role is '{}'",
            run.run_id,
            run.model,
            model
        ));
    }
    if !session.unlimited {
        let spent = session.tokens_in.saturating_add(session.tokens_out);
        if session.token_budget.is_some_and(|limit| spent >= limit) {
            return Err(anyhow!(
                "session token budget is exhausted ({spent} spent); refusing to resume"
            ));
        }
        if session
            .cost_budget_usd
            .is_some_and(|limit| session.cost_spent_usd >= limit)
        {
            return Err(anyhow!(
                "session USD budget is exhausted ({:.6} spent); refusing to resume",
                session.cost_spent_usd
            ));
        }
    }

    // Load the durable journal. Any read/decode failure fails closed before a
    // provider call or side effect.
    let store = engine.store().clone();
    let run_id = run.run_id.clone();
    let (steps, tools, checkpoints) = tokio::task::spawn_blocking(move || {
        Ok::<_, anyhow::Error>((
            store.list_run_steps(&run_id)?,
            store.list_run_lineage_tool_calls(&run_id)?,
            store.list_lineage_run_checkpoints(&run_id)?,
        ))
    })
    .await??;
    let last_checkpoint = checkpoints.last();
    let effective_plan_revision = last_checkpoint
        .map(|checkpoint| checkpoint.plan_revision)
        .unwrap_or(run.plan_revision);
    let current_plan_revision = engine.current_plan_revision(&session.id).await?;
    if current_plan_revision != effective_plan_revision {
        return Err(anyhow!(
            "plan revision changed after the run's last safe checkpoint (recorded {}, current {}); restore the plan or start a new prompt",
            effective_plan_revision,
            current_plan_revision
        ));
    }

    let worktree = session
        .worktree
        .clone()
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("session has no worktree"))?;
    if !worktree.is_dir() {
        return Err(anyhow!(
            "session worktree is missing: {}",
            worktree.display()
        ));
    }
    let recorded_head = last_checkpoint
        .map(|checkpoint| checkpoint.worktree_head.clone())
        .filter(|head| !head.is_empty())
        .unwrap_or_else(|| run.worktree_head.clone());
    let current_head = tokio::task::spawn_blocking({
        let worktree = worktree.clone();
        move || crate::worktree::head_commit(&worktree)
    })
    .await??;
    if recorded_head.is_empty() {
        let clean = tokio::task::spawn_blocking({
            let worktree = worktree.clone();
            move || crate::worktree::is_clean(&worktree)
        })
        .await??;
        if !clean {
            return Err(anyhow!(
                "run {} has no recorded worktree HEAD and the worktree has uncommitted changes; commit or discard them before resuming",
                run.run_id
            ));
        }
    } else if recorded_head != current_head {
        let clean = tokio::task::spawn_blocking({
            let worktree = worktree.clone();
            move || crate::worktree::is_clean(&worktree)
        })
        .await??;
        let compatible = clean
            && tokio::task::spawn_blocking({
                let worktree = worktree.clone();
                let recorded = recorded_head.clone();
                let current = current_head.clone();
                move || crate::worktree::is_ancestor(&worktree, &recorded, &current)
            })
            .await??;
        if !compatible {
            return Err(anyhow!(
                "worktree HEAD {} does not match the safe checkpoint {} and is not a clean descendant; commit or restore the worktree before resuming",
                current_head,
                recorded_head
            ));
        }
    }

    let last_turn = steps
        .iter()
        .filter(|step| step.kind == "model_request")
        .map(|step| step.turn)
        .max()
        .or_else(|| last_checkpoint.map(|checkpoint| checkpoint.turn))
        .unwrap_or(0);
    let note = build_journal_note(run, &tools, last_checkpoint, last_turn);
    Ok(ResumePlan {
        previous: run.clone(),
        note,
    })
}

/// Create the new linked run row and its events. The caller must already have
/// passed `preflight` and must hold the session run slot.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn begin_resumed_run(
    engine: &Engine,
    session: &Session,
    previous: &SessionRun,
    provider: &str,
    model: &str,
    worktree: &Path,
    budget: RunBudgetSnapshot,
) -> Result<RunJournal> {
    let run_id = Uuid::now_v7().to_string();
    let now = now_rfc3339();
    let head = worktree_head(worktree.to_path_buf()).await;
    let plan_revision = engine.current_plan_revision(&session.id).await?;
    let root = if previous.resume_root_id.is_empty() {
        previous.run_id.clone()
    } else {
        previous.resume_root_id.clone()
    };
    let run = SessionRun {
        run_id,
        session_id: session.id.clone(),
        project_id: session.project_id.clone(),
        agent_id: previous.agent_id.clone(),
        agent_name: previous.agent_name.clone(),
        agent_role: previous.agent_role.clone(),
        task_id: None,
        department: previous.department.clone(),
        objective: previous.objective.clone(),
        provider: provider.to_string(),
        model: model.to_string(),
        status: "running".to_string(),
        attempt: previous.attempt.saturating_add(1),
        resumed_from_run_id: Some(previous.run_id.clone()),
        resume_root_id: root,
        top_level: true,
        started_at: now.clone(),
        updated_at: now,
        ended_at: None,
        budget,
        worktree_head: head,
        plan_revision,
        last_event_seq: 0,
        failure: None,
    };
    let store = engine.store().clone();
    let previous_id = previous.run_id.clone();
    let new_run = run.clone();
    let new_run_id = run.run_id.clone();
    let stored =
        tokio::task::spawn_blocking(move || store.mark_session_run_resumed(&previous_id, &new_run))
            .await??;
    engine.broadcast_event(stored.1);
    engine.broadcast_event(stored.2);
    let gate = ResumeGate { run_id: new_run_id };
    Ok(RunJournal::resumed(run, worktree.to_path_buf(), Some(gate)))
}

fn build_journal_note(
    run: &SessionRun,
    tools: &[RunToolCall],
    last_checkpoint: Option<&RunCheckpoint>,
    last_turn: u32,
) -> String {
    let mut note = String::new();
    note.push_str("[Durable run journal] This session is continuing a previous attempt after a daemon interruption.\n");
    note.push_str(&format!(
        "Previous attempt: {} (run {}, attempt {}). Status: {}. Do not assume state beyond the recorded boundary.\n",
        truncate_chars(&run.objective, 200),
        run.run_id,
        run.attempt,
        run.status
    ));
    if let Some(checkpoint) = last_checkpoint {
        note.push_str(&format!(
            "Last safe checkpoint: turn {}, event #{}, message cursor {}.\n",
            checkpoint.turn, checkpoint.event_cursor, checkpoint.message_cursor
        ));
    } else {
        note.push_str(&format!(
            "No checkpoint was recorded; the last provider turn was {}.\n",
            last_turn
        ));
    }
    let completed_writes = tools
        .iter()
        .filter(|tool| tool.status == "completed" && WRITE_TOOLS.contains(&tool.tool_name.as_str()))
        .collect::<Vec<_>>();
    if !completed_writes.is_empty() {
        note.push_str("Completed write tools (duplicate calls will be blocked and return these journaled results):\n");
        for tool in completed_writes.iter().take(MAX_NOTE_TOOLS) {
            let summary = tool_result_summary(tool);
            note.push_str(&format!(
                "- {} {}: {} [run {}]\n",
                tool.tool_name, tool.tool_call_id, summary, tool.run_id
            ));
        }
    }
    let completed_other = tools
        .iter()
        .filter(|tool| {
            tool.status == "completed" && !WRITE_TOOLS.contains(&tool.tool_name.as_str())
        })
        .collect::<Vec<_>>();
    if !completed_other.is_empty() {
        note.push_str("Completed tool calls (short results):\n");
        for tool in completed_other.iter().take(MAX_NOTE_TOOLS) {
            note.push_str(&format!(
                "- {} {}: {}\n",
                tool.tool_name,
                tool.tool_call_id,
                tool_result_summary(tool)
            ));
        }
    }
    let unknown = tools
        .iter()
        .filter(|tool| tool.status == "unknown" || tool.status == "intent")
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        note.push_str(
            "Unknown or interrupted tool calls (verify state before assuming success):\n",
        );
        for tool in unknown.iter().take(MAX_NOTE_TOOLS) {
            note.push_str(&format!(
                "- {} {}: outcome unknown; {}.\n",
                tool.tool_name,
                tool.tool_call_id,
                if AMBIGUOUS_TOOLS.contains(&tool.tool_name.as_str()) {
                    "inspect the workspace/process state before retrying"
                } else {
                    "do not assume it landed"
                }
            ));
        }
    }
    if tools.is_empty() {
        note.push_str("No tool calls were durably recorded before the interruption.\n");
    }
    note.push_str(
        "Continue from the persisted conversation. Do not repeat completed writes; verify unknown state first. This note is bounded journal metadata, not a replacement for reading the workspace.\n",
    );
    truncate_chars(&note, MAX_NOTE_CHARS)
}

fn tool_result_summary(tool: &RunToolCall) -> String {
    if let Some(error) = &tool.error {
        return format!("error: {}", truncate_chars(error, 160));
    }
    let content = tool
        .result
        .as_ref()
        .and_then(|result| result.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("(no stored result)");
    truncate_chars(content, 160).replace('\n', " ")
}

fn bounded_tool_result(output: &ToolOutput) -> Value {
    let truncated = output.content.chars().count() > MAX_RESULT_CHARS;
    serde_json::json!({
        "content": truncate_chars(&output.content, MAX_RESULT_CHARS),
        "is_error": output.is_error,
        "truncated": truncated,
        "error_code": output
            .failure
            .as_ref()
            .map(|failure| failure.code.as_str()),
        "retryable": output
            .failure
            .as_ref()
            .is_some_and(|failure| failure.retryable),
        "structured": crate::tools::bounded_structured(output.structured.clone()),
    })
}

pub(crate) fn tool_fingerprint(tool_name: &str, input: &Value) -> Result<String> {
    crate::operation::fingerprint(tool_name, input)
}

fn hash_request(request: &ChatRequest) -> Result<String> {
    hash_json(&serde_json::json!({
        "model": request.model,
        "messages": request.messages,
        "system": request.system,
        "tools": request.tools,
        "max_tokens": request.max_tokens,
        "temperature": request.temperature,
    }))
}

fn hash_json(value: &Value) -> Result<String> {
    let canonical = canonical_value(value);
    let encoded = serde_json::to_vec(&canonical).context("serializing journal hash input")?;
    let digest = Sha256::digest(&encoded);
    Ok(format!("{digest:x}"))
}

fn canonical_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries = map.iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            let mut sorted = serde_json::Map::new();
            for (key, value) in entries {
                sorted.insert(key.clone(), canonical_value(value));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical_value).collect()),
        other => other.clone(),
    }
}

async fn worktree_head(worktree: PathBuf) -> String {
    tokio::task::spawn_blocking(move || crate::worktree::head_commit(&worktree))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default()
}

pub(crate) async fn supersede_unresolved_runs(
    engine: &Engine,
    session_id: &str,
    new_run_id: &str,
) -> Result<usize> {
    let store = engine.store().clone();
    let session_id = session_id.to_string();
    let new_run_id = new_run_id.to_string();
    let events = tokio::task::spawn_blocking(move || {
        store.supersede_resumable_session_runs(&session_id, &new_run_id)
    })
    .await??;
    let count = events.len();
    for event in events {
        engine.broadcast_event(event);
    }
    Ok(count)
}

pub(crate) async fn journal_cursors(engine: &Engine, session_id: &str) -> Result<(u64, u64)> {
    let store = engine.store().clone();
    let session_id = session_id.to_string();
    tokio::task::spawn_blocking(move || store.journal_cursors(&session_id)).await?
}

pub(crate) async fn interrupt_running_runs(engine: &Engine) -> Result<usize> {
    let store = engine.store().clone();
    let (runs, events) =
        tokio::task::spawn_blocking(move || store.interrupt_running_session_runs()).await??;
    for event in events {
        engine.broadcast_event(event);
    }
    Ok(runs.len())
}

/// Store-level helper used by RPC/view. Kept here so the store stays free of
/// protocol-specific resume policy.
pub(crate) async fn session_runs(
    engine: &Engine,
    session_id: &str,
    limit: u32,
) -> Result<Vec<SessionRun>> {
    let store = engine.store().clone();
    let session_id = session_id.to_string();
    tokio::task::spawn_blocking(move || store.list_session_runs(&session_id, limit)).await?
}

pub(crate) async fn run_detail(
    engine: &Engine,
    run_id: &str,
) -> Result<(
    SessionRun,
    Vec<RunStep>,
    Vec<RunToolCall>,
    Vec<RunCheckpoint>,
)> {
    let store = engine.store().clone();
    let id = run_id.to_string();
    tokio::task::spawn_blocking(move || {
        let run = store
            .get_session_run(&id)?
            .ok_or_else(|| anyhow!("run not found: {id}"))?;
        Ok::<_, anyhow::Error>((
            run.clone(),
            store.list_run_steps(&id)?,
            store.list_run_tool_calls(&id)?,
            store.list_run_checkpoints(&id)?,
        ))
    })
    .await?
}

pub(crate) async fn policy(engine: &Engine, project_id: &str) -> Result<SessionRunPolicy> {
    let store = engine.store().clone();
    let project_id = project_id.to_string();
    tokio::task::spawn_blocking(move || {
        if store.get_project(&project_id)?.is_none() {
            return Err(anyhow!("project not found: {project_id}"));
        }
        store.get_session_run_policy(&project_id)
    })
    .await?
}

pub(crate) async fn set_policy(
    engine: &Engine,
    project_id: &str,
    policy_value: &SessionRunPolicy,
) -> Result<SessionRunPolicy> {
    validate_policy(policy_value)?;
    let store = engine.store().clone();
    let project_id_owned = project_id.to_string();
    let policy_owned = policy_value.clone();
    tokio::task::spawn_blocking(move || {
        if store.get_project(&project_id_owned)?.is_none() {
            return Err(anyhow!("project not found: {project_id_owned}"));
        }
        store.set_session_run_policy(&project_id_owned, &policy_owned)
    })
    .await??;
    engine.record_project_manifest().await?;
    policy(engine, project_id).await
}

pub(crate) fn validate_policy(policy: &SessionRunPolicy) -> Result<()> {
    if !(1..=3).contains(&policy.max_attempts) {
        return Err(anyhow!("session run max_attempts must be between 1 and 3"));
    }
    if !(1..=168).contains(&policy.max_age_hours) {
        return Err(anyhow!(
            "session run max_age_hours must be between 1 and 168"
        ));
    }
    Ok(())
}

pub(crate) fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut result = value.chars().take(max).collect::<String>();
    result.push('…');
    result
}

/// The set of prior statuses that make a run resumable. Kept as a helper so
/// tests and RPC output agree.
pub(crate) fn is_resumable_status(status: &str) -> bool {
    matches!(status, "interrupted" | "failed")
}

/// Replay helper: derive run/step/checkpoint projections from events. Tool
/// outcome events are deliberately ignored. Returns `None` when a run journal
/// event cannot be decoded; repair then leaves the projection untouched rather
/// than destroying operational tool rows.
pub(crate) fn projections_from_events(
    events: &[Event],
) -> Option<(Vec<SessionRun>, Vec<RunStep>, Vec<RunCheckpoint>)> {
    use std::collections::HashMap;
    let mut runs: HashMap<String, SessionRun> = HashMap::new();
    let mut steps: HashMap<String, RunStep> = HashMap::new();
    let mut checkpoints: HashMap<String, RunCheckpoint> = HashMap::new();
    for event in events {
        match event.kind.as_str() {
            "session.run.started" => {
                let run: SessionRun =
                    serde_json::from_value(event.payload.get("run")?.clone()).ok()?;
                runs.insert(run.run_id.clone(), run);
            }
            "session.run.resumed" => {
                if let Some(previous) = event.payload.get("previous_run")
                    && let Ok(previous) = serde_json::from_value::<SessionRun>(previous.clone())
                {
                    runs.insert(previous.run_id.clone(), previous);
                }
                if let Some(run) = event.payload.get("run") {
                    let run: SessionRun = serde_json::from_value(run.clone()).ok()?;
                    runs.insert(run.run_id.clone(), run);
                }
            }
            "session.run.completed" | "session.run.failed" | "session.run.interrupted" => {
                let run: SessionRun =
                    serde_json::from_value(event.payload.get("run")?.clone()).ok()?;
                runs.insert(run.run_id.clone(), run);
            }
            "session.run.step_intent" | "session.run.step_completed" => {
                let step: RunStep =
                    serde_json::from_value(event.payload.get("step")?.clone()).ok()?;
                steps.insert(step.step_id.clone(), step);
            }
            "session.run.checkpointed" => {
                let checkpoint: RunCheckpoint =
                    serde_json::from_value(event.payload.get("checkpoint")?.clone()).ok()?;
                checkpoints.insert(checkpoint.checkpoint_id.clone(), checkpoint);
            }
            _ => {}
        }
    }
    let mut runs = runs.into_values().collect::<Vec<_>>();
    runs.sort_by(|left, right| {
        left.started_at
            .cmp(&right.started_at)
            .then_with(|| left.run_id.cmp(&right.run_id))
    });
    let mut steps = steps.into_values().collect::<Vec<_>>();
    steps.sort_by(|left, right| {
        left.run_id
            .cmp(&right.run_id)
            .then_with(|| left.seq.cmp(&right.seq))
    });
    let mut checkpoints = checkpoints.into_values().collect::<Vec<_>>();
    checkpoints.sort_by(|left, right| {
        left.run_id
            .cmp(&right.run_id)
            .then_with(|| left.seq.cmp(&right.seq))
    });
    Some((runs, steps, checkpoints))
}

/// Compare projected rows to replay-derived rows. `last_event_seq` is
/// operational bookkeeping updated by checkpoint/step/tool events and is not
/// part of the status/attempt replay contract.
pub(crate) fn runs_match(projected: &[SessionRun], expected: &[SessionRun]) -> bool {
    use std::collections::HashMap;
    if projected.len() != expected.len() {
        return false;
    }
    let projected = projected
        .iter()
        .map(|run| (run.run_id.as_str(), run))
        .collect::<HashMap<_, _>>();
    expected.iter().all(|right| {
        projected.get(right.run_id.as_str()).is_some_and(|left| {
            left.session_id == right.session_id
                && left.project_id == right.project_id
                && left.agent_id == right.agent_id
                && left.agent_role == right.agent_role
                && left.task_id == right.task_id
                && left.department == right.department
                && left.provider == right.provider
                && left.model == right.model
                && left.status == right.status
                && left.attempt == right.attempt
                && left.resumed_from_run_id == right.resumed_from_run_id
                && left.resume_root_id == right.resume_root_id
                && left.top_level == right.top_level
                && left.started_at == right.started_at
                && left.updated_at == right.updated_at
                && left.ended_at == right.ended_at
                && left.budget == right.budget
                && left.worktree_head == right.worktree_head
                && left.plan_revision == right.plan_revision
                && left.failure == right.failure
        })
    })
}

pub(crate) fn steps_match(projected: &[RunStep], expected: &[RunStep]) -> bool {
    if projected.len() != expected.len() {
        return false;
    }
    projected.iter().zip(expected.iter()).all(|(left, right)| {
        left.step_id == right.step_id
            && left.run_id == right.run_id
            && left.session_id == right.session_id
            && left.seq == right.seq
            && left.turn == right.turn
            && left.kind == right.kind
            && left.status == right.status
            && left.input_hash == right.input_hash
            && left.output_hash == right.output_hash
            && left.payload == right.payload
            && left.created_at == right.created_at
            && left.updated_at == right.updated_at
    })
}

pub(crate) fn checkpoints_match(projected: &[RunCheckpoint], expected: &[RunCheckpoint]) -> bool {
    if projected.len() != expected.len() {
        return false;
    }
    projected.iter().zip(expected.iter()).all(|(left, right)| {
        left.checkpoint_id == right.checkpoint_id
            && left.run_id == right.run_id
            && left.session_id == right.session_id
            && left.seq == right.seq
            && left.turn == right.turn
            && left.message_cursor == right.message_cursor
            && left.event_cursor == right.event_cursor
            && left.budget == right.budget
            && left.worktree_head == right.worktree_head
            && left.plan_revision == right.plan_revision
            && left.created_at == right.created_at
    })
}

/// Test helpers and RPC helpers need to know which tools are side-effecting.
#[cfg(test)]
pub(crate) fn is_write_tool(name: &str) -> bool {
    WRITE_TOOLS.contains(&name)
}

#[cfg(test)]
pub(crate) fn is_ambiguous_tool(name: &str) -> bool {
    AMBIGUOUS_TOOLS.contains(&name)
}

/// Store access used by `Engine::recover_interrupted_sessions`.
pub(crate) async fn all_resumable_runs(engine: &Engine) -> Result<Vec<SessionRun>> {
    let store = engine.store().clone();
    tokio::task::spawn_blocking(move || store.list_resumable_session_runs(500)).await?
}

/// Validate a step kind/status pair before writing. The store repeats the
/// status check; this prevents a typo from producing an unreadable journal.
pub(crate) fn validate_step(kind: &str, status: &str) -> Result<()> {
    if !STEP_KINDS.contains(&kind) {
        return Err(anyhow!("unsupported run step kind {kind}"));
    }
    if !matches!(status, "intent" | "completed" | "failed" | "unknown") {
        return Err(anyhow!("unsupported run step status {status}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vp_store::Store;

    fn sample_run(session_id: &str) -> SessionRun {
        SessionRun {
            run_id: "run-1".to_string(),
            session_id: session_id.to_string(),
            project_id: "project-1".to_string(),
            agent_id: "agent-1".to_string(),
            agent_name: "Leader".to_string(),
            agent_role: "leader".to_string(),
            task_id: None,
            department: None,
            objective: "test".to_string(),
            provider: "fake".to_string(),
            model: "fake-model".to_string(),
            status: "running".to_string(),
            attempt: 1,
            resumed_from_run_id: None,
            resume_root_id: "run-1".to_string(),
            top_level: true,
            started_at: now_rfc3339(),
            updated_at: now_rfc3339(),
            ended_at: None,
            budget: RunBudgetSnapshot::default(),
            worktree_head: String::new(),
            plan_revision: 0,
            last_event_seq: 0,
            failure: None,
        }
    }

    #[test]
    fn bounded_tool_result_keeps_failure_contract_for_resume() {
        let output = ToolOutput::error_coded(
            "edit_file",
            crate::tooling::errors::ToolErrorCode::PATCH_CONFLICT,
            "old_string was not found",
        )
        .with_failure_evidence("path=src/lib.rs");
        let value = bounded_tool_result(&output);
        assert_eq!(value["is_error"], true);
        assert_eq!(value["error_code"], "PATCH_CONFLICT");
        assert_eq!(value["retryable"], true);
        assert_eq!(value["structured"]["code"], "PATCH_CONFLICT");
        assert_eq!(value["structured"]["tool"], "edit_file");
        assert_eq!(value["structured"]["evidence"], "path=src/lib.rs");
        assert!(
            value["content"]
                .as_str()
                .unwrap()
                .starts_with("error[PATCH_CONFLICT]")
        );
    }

    #[test]
    fn canonical_fingerprint_ignores_object_key_order() {
        let first = serde_json::json!({"path": "src/a.rs", "content": "x"});
        let second = serde_json::json!({"content": "x", "path": "src/a.rs"});
        assert_eq!(
            tool_fingerprint("write_file", &first).unwrap(),
            tool_fingerprint("write_file", &second).unwrap()
        );
        assert_ne!(
            tool_fingerprint("write_file", &first).unwrap(),
            tool_fingerprint("edit_file", &first).unwrap()
        );
    }

    #[test]
    fn policy_bounds_are_enforced() {
        let mut policy = SessionRunPolicy::default();
        assert!(validate_policy(&policy).is_ok());
        policy.max_attempts = 0;
        assert!(validate_policy(&policy).is_err());
        policy.max_attempts = 4;
        assert!(validate_policy(&policy).is_err());
        policy.max_attempts = 3;
        policy.max_age_hours = 169;
        assert!(validate_policy(&policy).is_err());
    }

    #[test]
    fn note_names_completed_writes_and_unknown_calls() {
        let run = sample_run("ses-1");
        let tools = vec![
            RunToolCall {
                run_id: "run-1".into(),
                session_id: "ses-1".into(),
                step_id: "step-1".into(),
                tool_call_id: "call-1".into(),
                tool_name: "write_file".into(),
                input_fingerprint: "fp".into(),
                status: "completed".into(),
                result: Some(serde_json::json!({"content": "wrote src/a.rs"})),
                error: None,
                consumed_by_run_id: None,
                consumed_at: None,
                created_at: now_rfc3339(),
                updated_at: now_rfc3339(),
            },
            RunToolCall {
                run_id: "run-1".into(),
                session_id: "ses-1".into(),
                step_id: "step-2".into(),
                tool_call_id: "call-2".into(),
                tool_name: "shell".into(),
                input_fingerprint: "fp2".into(),
                status: "unknown".into(),
                result: None,
                error: Some("daemon stopped".into()),
                consumed_by_run_id: None,
                consumed_at: None,
                created_at: now_rfc3339(),
                updated_at: now_rfc3339(),
            },
        ];
        let note = build_journal_note(&run, &tools, None, 3);
        assert!(note.contains("Completed write tools"));
        assert!(note.contains("write_file call-1"));
        assert!(note.contains("Unknown or interrupted"));
        assert!(note.contains("verify state"));
        assert!(note.contains("last provider turn was 3"));
    }

    #[test]
    fn note_is_bounded() {
        let run = sample_run("ses-1");
        let tools = (0..200)
            .map(|index| RunToolCall {
                run_id: "run-1".into(),
                session_id: "ses-1".into(),
                step_id: format!("step-{index}"),
                tool_call_id: format!("call-{index}"),
                tool_name: "shell".into(),
                input_fingerprint: format!("fp-{index}"),
                status: "completed".into(),
                result: Some(serde_json::json!({"content": "x".repeat(500)})),
                error: None,
                consumed_by_run_id: None,
                consumed_at: None,
                created_at: now_rfc3339(),
                updated_at: now_rfc3339(),
            })
            .collect::<Vec<_>>();
        let note = build_journal_note(&run, &tools, None, 9);
        assert!(note.chars().count() <= MAX_NOTE_CHARS + 1);
    }

    #[tokio::test]
    async fn replay_derives_runs_steps_and_checkpoints_and_ignores_tool_events() {
        let event = |seq: u64, kind: &str, payload: Value| Event {
            session_id: "ses-1".into(),
            seq,
            kind: kind.into(),
            payload,
            created_at: now_rfc3339(),
        };
        let run = sample_run("ses-1");
        let step = RunStep {
            step_id: "step-1".into(),
            run_id: run.run_id.clone(),
            session_id: "ses-1".into(),
            seq: 1,
            turn: 1,
            kind: "model_request".into(),
            status: "completed".into(),
            input_hash: "in".into(),
            output_hash: "out".into(),
            payload: serde_json::json!({}),
            created_at: now_rfc3339(),
            updated_at: now_rfc3339(),
        };
        let checkpoint = RunCheckpoint {
            checkpoint_id: "cp-1".into(),
            run_id: run.run_id.clone(),
            session_id: "ses-1".into(),
            seq: 1,
            turn: 1,
            message_cursor: 1,
            event_cursor: 1,
            budget: RunBudgetSnapshot::default(),
            worktree_head: "abc".into(),
            plan_revision: 0,
            created_at: now_rfc3339(),
        };
        let events = vec![
            event(
                1,
                "session.run.started",
                serde_json::json!({"run": run.clone()}),
            ),
            event(
                2,
                "session.run.step_intent",
                serde_json::json!({"step": step.clone()}),
            ),
            event(
                3,
                "session.run.checkpointed",
                serde_json::json!({"checkpoint": checkpoint.clone()}),
            ),
            event(
                4,
                "session.run.tool_completed",
                serde_json::json!({"tool": {"tool_name": "shell"}}),
            ),
        ];
        let (runs, steps, checkpoints) = projections_from_events(&events).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(steps.len(), 1);
        assert_eq!(checkpoints.len(), 1);
        assert_eq!(steps[0].step_id, "step-1");
        assert_eq!(checkpoints[0].checkpoint_id, "cp-1");
    }

    #[tokio::test]
    async fn journal_cursors_count_message_events() {
        let data = tempfile::TempDir::new().unwrap();
        let repo = tempfile::TempDir::new().unwrap();
        std::fs::write(repo.path().join("README.md"), "x").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "test@verlinpark.local"],
            vec!["config", "user.name", "VerlinPark Test"],
            vec!["add", "."],
            vec!["commit", "-q", "-m", "initial"],
        ] {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(repo.path())
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let store = Store::open(data.path()).unwrap();
        let engine = Engine::new(store);
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("journal".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id.clone(), "goal".into(), None)
            .await
            .unwrap();
        engine
            .append_event(&session.id, "x.one", serde_json::json!({}))
            .await
            .unwrap();
        engine
            .append_message(&session.id, "user", "hello", serde_json::json!([]), None)
            .await
            .unwrap();
        let (event_cursor, message_cursor) = journal_cursors(&engine, &session.id).await.unwrap();
        assert_eq!(event_cursor, 3);
        assert_eq!(message_cursor, 1);
    }

    #[test]
    fn tool_classification_is_explicit() {
        for tool in WRITE_TOOLS {
            assert!(is_write_tool(tool));
            assert!(!is_ambiguous_tool(tool));
        }
        for tool in AMBIGUOUS_TOOLS {
            assert!(is_ambiguous_tool(tool));
            assert!(!is_write_tool(tool));
        }
        assert!(is_resumable_status("failed"));
        assert!(is_resumable_status("interrupted"));
        assert!(!is_resumable_status("completed"));
        assert!(!is_resumable_status("resumed"));
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::Engine;
    use crate::tools::ToolOutput;
    use std::process::Command;
    use tempfile::TempDir;
    use vp_protocol::types::{RunFailure, SessionRunPolicy, SessionRunPolicyResult, TeamPlan};
    use vp_store::Store;

    fn init_repo(path: &Path) {
        std::fs::write(path.join("README.md"), "hello\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "test@verlinpark.local"],
            vec!["config", "user.name", "VerlinPark Test"],
            vec!["add", "."],
            vec!["commit", "-q", "-m", "initial"],
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(path)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }

    struct Fixture {
        _data: TempDir,
        _repo: TempDir,
        engine: Engine,
        session: vp_protocol::types::Session,
    }

    async fn fixture() -> Fixture {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("journal".to_string()),
            )
            .await
            .unwrap();
        let mut session = engine
            .create_session(project.id, "test goal".into(), None)
            .await
            .unwrap();
        engine
            .update_session_status(&session.id, "idle")
            .await
            .unwrap();
        session.status = "idle".to_string();
        Fixture {
            _data: data,
            _repo: repo,
            engine,
            session,
        }
    }

    async fn begin_journal(
        fixture: &Fixture,
        objective: &str,
        top_level: bool,
        task_id: Option<&str>,
    ) -> RunJournal {
        RunJournal::begin(
            &fixture.engine,
            &fixture.session,
            &fixture.session.project_id,
            "leader-id",
            "Leader",
            "leader",
            task_id,
            None,
            objective,
            "test",
            "test-model",
            Path::new(fixture.session.worktree.as_deref().unwrap()),
            RunBudgetSnapshot::default(),
            top_level,
        )
        .await
        .unwrap()
    }

    async fn interrupt(engine: &Engine) -> usize {
        crate::run_journal::interrupt_running_runs(engine)
            .await
            .unwrap()
    }

    async fn finish_interrupted(fixture: &Fixture, journal: &RunJournal) {
        journal
            .finish_interrupted(&fixture.engine, "interrupted", "test crash")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn start_checkpoint_step_tool_finish_and_detail_round_trip() {
        let fixture = fixture().await;
        let journal = begin_journal(&fixture, "write then finish", true, None).await;
        let checkpoint = journal
            .checkpoint(&fixture.engine, 1, RunBudgetSnapshot::default())
            .await
            .unwrap();
        assert_eq!(checkpoint.turn, 1);
        let request = ChatRequest::new(
            "test-model",
            vec![vp_providers::ChatMessage::user("do the thing")],
        );
        let step = journal
            .model_intent(&fixture.engine, 1, &request)
            .await
            .unwrap();
        journal
            .model_completed(
                &fixture.engine,
                &step,
                None,
                "done",
                &[],
                10,
                4,
                0,
                0,
                false,
            )
            .await
            .unwrap();
        let (tool_step, tool) = journal
            .tool_intent(
                &fixture.engine,
                1,
                "call-1",
                "write_file",
                &serde_json::json!({"path": "a.txt", "content": "x"}),
            )
            .await
            .unwrap();
        journal
            .tool_completed(
                &fixture.engine,
                &tool_step,
                &tool,
                &ToolOutput::ok("wrote a.txt"),
            )
            .await
            .unwrap();
        journal.finish_completed(&fixture.engine).await.unwrap();

        let runs = crate::run_journal::session_runs(&fixture.engine, &fixture.session.id, 20)
            .await
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "completed");
        assert_eq!(runs[0].attempt, 1);
        let (run, steps, tools, checkpoints) =
            crate::run_journal::run_detail(&fixture.engine, &runs[0].run_id)
                .await
                .unwrap();
        assert_eq!(run.status, "completed");
        assert_eq!(steps.len(), 2);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].status, "completed");
        assert!(tools[0].result.is_some());
        assert_eq!(checkpoints.len(), 1);
        let report = fixture
            .engine
            .replay_session(&fixture.session.id)
            .await
            .unwrap();
        assert!(report.consistent, "{:?}", report.mismatches);
        assert!(report.runs_match);
        assert!(report.run_steps_match);
        assert!(report.run_checkpoints_match);
        let events = fixture
            .engine
            .list_events(fixture.session.id.clone(), 0, 100)
            .await
            .unwrap();
        for kind in [
            "session.run.started",
            "session.run.checkpointed",
            "session.run.step_intent",
            "session.run.step_completed",
            "session.run.tool_intent",
            "session.run.tool_completed",
            "session.run.completed",
        ] {
            assert!(
                events.iter().any(|event| event.kind == kind),
                "missing {kind}"
            );
        }
    }

    #[tokio::test]
    async fn crash_recovery_classifies_model_intents_retryable_and_tool_intents_unknown() {
        let fixture = fixture().await;
        let journal = begin_journal(&fixture, "crash", true, None).await;
        journal
            .checkpoint(&fixture.engine, 1, RunBudgetSnapshot::default())
            .await
            .unwrap();
        let request = ChatRequest::new("test-model", vec![vp_providers::ChatMessage::user("go")]);
        let model_step = journal
            .model_intent(&fixture.engine, 1, &request)
            .await
            .unwrap();
        let (_tool_step, tool) = journal
            .tool_intent(
                &fixture.engine,
                1,
                "call-crash",
                "shell",
                &serde_json::json!({"command": "touch marker"}),
            )
            .await
            .unwrap();
        assert_eq!(tool.status, "intent");
        let interrupted = interrupt(&fixture.engine).await;
        assert_eq!(interrupted, 1);
        let runs = crate::run_journal::session_runs(&fixture.engine, &fixture.session.id, 20)
            .await
            .unwrap();
        assert_eq!(runs[0].status, "interrupted");
        assert_eq!(
            runs[0]
                .failure
                .as_ref()
                .map(|failure| failure.kind.as_str()),
            Some("interrupted")
        );
        let (_, steps, tools, checkpoints) =
            crate::run_journal::run_detail(&fixture.engine, &runs[0].run_id)
                .await
                .unwrap();
        let model = steps
            .iter()
            .find(|step| step.step_id == model_step.step_id)
            .unwrap();
        assert_eq!(model.status, "intent");
        assert_eq!(tools[0].status, "unknown");
        assert!(tools[0].error.is_some());
        assert_eq!(checkpoints.len(), 1);
        let report = fixture
            .engine
            .replay_session(&fixture.session.id)
            .await
            .unwrap();
        assert!(report.consistent, "{:?}", report.mismatches);
    }

    #[tokio::test]
    async fn records_compaction_approval_and_question_steps() {
        let fixture = fixture().await;
        let journal = begin_journal(&fixture, "aux steps", true, None).await;
        journal
            .record_compaction(&fixture.engine, 1, 10, 12, 7)
            .await
            .unwrap();
        let approval = journal
            .approval_intent(
                &fixture.engine,
                1,
                "approval-1",
                "shell",
                "call-1",
                "host_state",
                "runs outside the workspace",
            )
            .await
            .unwrap();
        journal
            .approval_completed(&fixture.engine, &approval, "denied")
            .await
            .unwrap();
        let question = journal
            .question_intent(
                &fixture.engine,
                1,
                "question-1",
                "Which branch?",
                &["main".into(), "dev".into()],
            )
            .await
            .unwrap();
        journal
            .question_completed(&fixture.engine, &question, "answered")
            .await
            .unwrap();
        journal
            .finish_failed(&fixture.engine, "budget_exceeded", "test stop")
            .await
            .unwrap();
        let run = crate::run_journal::session_runs(&fixture.engine, &fixture.session.id, 10)
            .await
            .unwrap()
            .remove(0);
        assert_eq!(run.status, "failed");
        let (_, steps, _, _) = crate::run_journal::run_detail(&fixture.engine, &run.run_id)
            .await
            .unwrap();
        for kind in ["compaction", "approval", "question"] {
            assert!(
                steps
                    .iter()
                    .any(|step| step.kind == kind && step.status == "completed"),
                "missing completed {kind} step"
            );
        }
        let report = fixture
            .engine
            .replay_session(&fixture.session.id)
            .await
            .unwrap();
        assert!(report.consistent, "{:?}", report.mismatches);
        assert!(report.run_steps_match);
    }

    #[tokio::test]
    async fn duplicate_write_gate_consumes_once_and_releases_after_failure() {
        let fixture = fixture().await;
        let journal = begin_journal(&fixture, "write", true, None).await;
        let (step, tool) = journal
            .tool_intent(
                &fixture.engine,
                1,
                "call-write",
                "write_file",
                &serde_json::json!({"path": "dup.txt", "content": "one"}),
            )
            .await
            .unwrap();
        journal
            .tool_completed(
                &fixture.engine,
                &step,
                &tool,
                &ToolOutput::ok("wrote dup.txt"),
            )
            .await
            .unwrap();
        finish_interrupted(&fixture, &journal).await;
        let old = crate::run_journal::session_runs(&fixture.engine, &fixture.session.id, 10)
            .await
            .unwrap()
            .remove(0);
        let resumed = crate::run_journal::begin_resumed_run(
            &fixture.engine,
            &fixture.session,
            &old,
            "test",
            "test-model",
            Path::new(fixture.session.worktree.as_deref().unwrap()),
            RunBudgetSnapshot::default(),
        )
        .await
        .unwrap();
        let input = serde_json::json!({"path": "dup.txt", "content": "one"});
        let first = resumed
            .check_duplicate_write(&fixture.engine, "write_file", &input)
            .await
            .unwrap();
        assert!(first.is_some());
        assert!(first.unwrap().content.contains("durable journal"));
        let second = resumed
            .check_duplicate_write(&fixture.engine, "write_file", &input)
            .await
            .unwrap();
        assert!(second.is_none(), "one row must not satisfy two calls");
        let different = serde_json::json!({"path": "dup.txt", "content": "two"});
        assert!(
            resumed
                .check_duplicate_write(&fixture.engine, "write_file", &different)
                .await
                .unwrap()
                .is_none(),
            "a different canonical input is a fresh write"
        );
        // A read-only tool is never blocked by the write gate.
        assert!(
            resumed
                .check_duplicate_write(
                    &fixture.engine,
                    "read_file",
                    &serde_json::json!({"path": "README.md"})
                )
                .await
                .unwrap()
                .is_none()
        );
        resumed
            .finish_failed(&fixture.engine, "error", "simulated failure")
            .await
            .unwrap();
        let third = resumed
            .check_duplicate_write(&fixture.engine, "write_file", &input)
            .await
            .unwrap();
        assert!(
            third.is_some(),
            "a failed resume must release the claim for the next attempt"
        );

        let failed_journal = begin_journal(&fixture, "failed write", true, None).await;
        let (failed_step, failed_tool) = failed_journal
            .tool_intent(
                &fixture.engine,
                1,
                "call-failed-write",
                "write_file",
                &serde_json::json!({"path": "failed.txt", "content": "x"}),
            )
            .await
            .unwrap();
        failed_journal
            .tool_completed(
                &fixture.engine,
                &failed_step,
                &failed_tool,
                &ToolOutput::error("disk full"),
            )
            .await
            .unwrap();
        finish_interrupted(&fixture, &failed_journal).await;
        let failed_old = crate::run_journal::session_runs(&fixture.engine, &fixture.session.id, 10)
            .await
            .unwrap()
            .into_iter()
            .find(|run| run.run_id == failed_journal.id())
            .unwrap();
        let failed_resume = crate::run_journal::begin_resumed_run(
            &fixture.engine,
            &fixture.session,
            &failed_old,
            "test",
            "test-model",
            Path::new(fixture.session.worktree.as_deref().unwrap()),
            RunBudgetSnapshot::default(),
        )
        .await
        .unwrap();
        assert!(
            failed_resume
                .check_duplicate_write(
                    &fixture.engine,
                    "write_file",
                    &serde_json::json!({"path": "failed.txt", "content": "x"})
                )
                .await
                .unwrap()
                .is_none(),
            "a failed write was not applied and may be retried"
        );
    }

    #[tokio::test]
    async fn preflight_rejects_attempt_age_and_cancelled_runs() {
        let fixture = fixture().await;
        let journal = begin_journal(&fixture, "limits", true, None).await;
        finish_interrupted(&fixture, &journal).await;
        let run = crate::run_journal::session_runs(&fixture.engine, &fixture.session.id, 10)
            .await
            .unwrap()
            .remove(0);
        let mut policy = SessionRunPolicy::default();
        let mismatch = preflight(
            &fixture.engine,
            &fixture.session,
            &run,
            &policy,
            "test",
            "test-model",
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap_err();
        assert!(format!("{mismatch:#}").contains("attempts"));
        policy.max_attempts = 2;
        let old_start = (time::OffsetDateTime::now_utc() - time::Duration::hours(48))
            .format(&Rfc3339)
            .unwrap();
        let mut old_run = run.clone();
        old_run.started_at = old_start;
        let mismatch = preflight(
            &fixture.engine,
            &fixture.session,
            &old_run,
            &policy,
            "test",
            "test-model",
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap_err();
        assert!(format!("{mismatch:#}").contains("hour(s) old"));

        let mut running_session = fixture.session.clone();
        running_session.status = "running".into();
        let mut fresh_run = run.clone();
        fresh_run.started_at = time::OffsetDateTime::now_utc().format(&Rfc3339).unwrap();
        let mismatch = preflight(
            &fixture.engine,
            &running_session,
            &fresh_run,
            &policy,
            "test",
            "test-model",
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap_err();
        assert!(format!("{mismatch:#}").contains("only idle"));

        let mut cancelled = run.clone();
        cancelled.failure = Some(RunFailure {
            kind: "cancelled".into(),
            message: "operator".into(),
        });
        let mismatch = preflight(
            &fixture.engine,
            &fixture.session,
            &cancelled,
            &policy,
            "test",
            "test-model",
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap_err();
        assert!(format!("{mismatch:#}").contains("cancelled"));
    }

    #[tokio::test]
    async fn preflight_rejects_plan_provider_budget_and_task_scope() {
        let fixture = fixture().await;
        let journal = begin_journal(&fixture, "checks", true, None).await;
        finish_interrupted(&fixture, &journal).await;
        let run = crate::run_journal::session_runs(&fixture.engine, &fixture.session.id, 10)
            .await
            .unwrap()
            .remove(0);
        let mut policy = SessionRunPolicy {
            auto_resume: false,
            max_attempts: 2,
            max_age_hours: 24,
        };
        // Provider mismatch.
        let error = preflight(
            &fixture.engine,
            &fixture.session,
            &run,
            &policy,
            "other-provider",
            "test-model",
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("provider"));
        // Model mismatch.
        let error = preflight(
            &fixture.engine,
            &fixture.session,
            &run,
            &policy,
            "test",
            "other-model",
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("model"));
        // Plan changed after the checkpoint.
        let plan: TeamPlan = serde_json::from_value(serde_json::json!({
            "goal": "test goal",
            "departments": [{
                "name": "impl",
                "manager_title": "Implementation",
                "profile": "coding",
                "objective": "build"
            }],
            "tasks": [{
                "id": "T1",
                "department": "impl",
                "title": "task",
                "kind": "coding",
                "deps": [],
                "acceptance": ["done"]
            }]
        }))
        .unwrap();
        fixture
            .engine
            .set_team_plan(&fixture.session.id, &plan)
            .await
            .unwrap();
        let error = preflight(
            &fixture.engine,
            &fixture.session,
            &run,
            &policy,
            "test",
            "test-model",
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("plan revision"));
        // Task-scoped runs are not resumed through session.run.resume.
        let mut task_run = run.clone();
        task_run.task_id = Some("T1".into());
        let error = preflight(
            &fixture.engine,
            &fixture.session,
            &task_run,
            &policy,
            "test",
            "test-model",
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("task-scoped"));
        // Budget exhaustion.
        let mut budget_session = fixture
            .engine
            .create_session(
                fixture.session.project_id.clone(),
                "budget".into(),
                Some(10),
            )
            .await
            .unwrap();
        fixture
            .engine
            .record_agent_usage_with_cache(
                &budget_session.id,
                "agent",
                "Agent",
                "leader",
                None,
                None,
                "test-model",
                10,
                0,
                0,
                0,
                false,
                0.0,
            )
            .await
            .unwrap();
        budget_session.tokens_in = 10;
        fixture
            .engine
            .update_session_status(&budget_session.id, "idle")
            .await
            .unwrap();
        budget_session.status = "idle".to_string();
        let mut budget_run = run.clone();
        budget_run.session_id = budget_session.id.clone();
        policy.max_attempts = 2;
        let error = preflight(
            &fixture.engine,
            &budget_session,
            &budget_run,
            &policy,
            "test",
            "test-model",
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("budget"));
    }

    #[tokio::test]
    async fn preflight_accepts_recorded_head_and_clean_descendant_but_rejects_divergence() {
        let fixture = fixture().await;
        let journal = begin_journal(&fixture, "head", true, None).await;
        journal
            .checkpoint(&fixture.engine, 1, RunBudgetSnapshot::default())
            .await
            .unwrap();
        finish_interrupted(&fixture, &journal).await;
        let run = crate::run_journal::session_runs(&fixture.engine, &fixture.session.id, 10)
            .await
            .unwrap()
            .remove(0);
        let policy = SessionRunPolicy {
            auto_resume: false,
            max_attempts: 2,
            max_age_hours: 24,
        };
        preflight(
            &fixture.engine,
            &fixture.session,
            &run,
            &policy,
            "test",
            "test-model",
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
        let worktree = PathBuf::from(fixture.session.worktree.as_deref().unwrap());
        std::fs::write(worktree.join("README.md"), "advanced\n").unwrap();
        let status = Command::new("git")
            .arg("-C")
            .arg(&worktree)
            .args(["add", "."])
            .status()
            .unwrap();
        assert!(status.success());
        let status = Command::new("git")
            .arg("-C")
            .arg(&worktree)
            .args(["commit", "-q", "-m", "advance"])
            .status()
            .unwrap();
        assert!(status.success());
        preflight(
            &fixture.engine,
            &fixture.session,
            &run,
            &policy,
            "test",
            "test-model",
            time::OffsetDateTime::now_utc(),
        )
        .await
        .expect("a clean descendant is compatible");
        let status = Command::new("git")
            .arg("-C")
            .arg(&worktree)
            .args(["checkout", "--orphan", "unrelated"])
            .output()
            .unwrap();
        assert!(status.status.success());
        let status = Command::new("git")
            .arg("-C")
            .arg(&worktree)
            .args(["add", "-A"])
            .status()
            .unwrap();
        assert!(status.success());
        let status = Command::new("git")
            .arg("-C")
            .arg(&worktree)
            .args(["commit", "-q", "-m", "orphan"])
            .status()
            .unwrap();
        assert!(status.success());
        let error = preflight(
            &fixture.engine,
            &fixture.session,
            &run,
            &policy,
            "test",
            "test-model",
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("does not match"));
    }

    #[tokio::test]
    async fn repair_rebuilds_missing_run_journal_projections() {
        let fixture = fixture().await;
        let journal = begin_journal(&fixture, "repair", true, None).await;
        journal
            .checkpoint(&fixture.engine, 1, RunBudgetSnapshot::default())
            .await
            .unwrap();
        let (step, tool) = journal
            .tool_intent(
                &fixture.engine,
                1,
                "call-r",
                "read_file",
                &serde_json::json!({"path": "README.md"}),
            )
            .await
            .unwrap();
        journal
            .tool_completed(&fixture.engine, &step, &tool, &ToolOutput::ok("hello"))
            .await
            .unwrap();
        journal.finish_completed(&fixture.engine).await.unwrap();
        // Simulate a damaged projection. Operational tool rows are preserved;
        // overwrite the event-backed run/step/checkpoint rows instead.
        let store = fixture.engine.store().clone();
        let session_id = fixture.session.id.clone();
        let mut runs = crate::run_journal::session_runs(&fixture.engine, &session_id, 10)
            .await
            .unwrap();
        runs[0].status = "failed".to_string();
        store
            .replace_run_journal_projections(&session_id, &runs, &[], &[])
            .unwrap();
        let report = fixture
            .engine
            .replay_session(&fixture.session.id)
            .await
            .unwrap();
        assert!(!report.runs_match);
        assert!(!report.consistent);
        let repaired = fixture
            .engine
            .repair_session(&fixture.session.id)
            .await
            .unwrap();
        assert!(repaired.repaired);
        assert!(repaired.runs_match || repaired.consistent);
        let runs = crate::run_journal::session_runs(&fixture.engine, &fixture.session.id, 10)
            .await
            .unwrap();
        assert_eq!(runs.len(), 1);
        let (_, steps, tools, checkpoints) =
            crate::run_journal::run_detail(&fixture.engine, &runs[0].run_id)
                .await
                .unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(tools.len(), 1);
        assert_eq!(checkpoints.len(), 1);
    }

    #[tokio::test]
    async fn export_and_import_preserve_the_replayable_run_journal() {
        let fixture = fixture().await;
        let journal = begin_journal(&fixture, "bundle", true, None).await;
        journal
            .checkpoint(&fixture.engine, 1, RunBudgetSnapshot::default())
            .await
            .unwrap();
        journal.finish_completed(&fixture.engine).await.unwrap();
        let bundle = fixture
            .engine
            .export_session(&fixture.session.id)
            .await
            .unwrap();
        assert_eq!(bundle.session_runs.len(), 1);
        assert_eq!(bundle.run_checkpoints.len(), 1);
        let other_data = TempDir::new().unwrap();
        let other = Engine::new(Store::open(other_data.path()).unwrap());
        let imported = other.import_session(bundle).await.unwrap();
        let runs = crate::run_journal::session_runs(&other, &imported.session_id, 10)
            .await
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "completed");
        let detail = other.session_run_detail(&runs[0].run_id).await.unwrap();
        assert_eq!(detail.checkpoints.len(), 1);
    }

    #[tokio::test]
    async fn imported_archives_are_not_resumed_without_operational_tool_outcomes() {
        let fixture = fixture().await;
        let journal = begin_journal(&fixture, "imported", true, None).await;
        journal
            .finish_interrupted(&fixture.engine, "interrupted", "crash")
            .await
            .unwrap();
        let bundle = fixture
            .engine
            .export_session(&fixture.session.id)
            .await
            .unwrap();
        let other_data = TempDir::new().unwrap();
        let other = Engine::new(Store::open(other_data.path()).unwrap());
        let imported = other.import_session(bundle).await.unwrap();
        other
            .update_session_status(&imported.session_id, "idle")
            .await
            .unwrap();
        let session = other
            .list_sessions(None)
            .await
            .unwrap()
            .into_iter()
            .find(|session| session.id == imported.session_id)
            .unwrap();
        let run = crate::run_journal::session_runs(&other, &session.id, 10)
            .await
            .unwrap()
            .remove(0);
        let policy = SessionRunPolicy {
            auto_resume: false,
            max_attempts: 2,
            max_age_hours: 24,
        };
        let error = preflight(
            &other,
            &session,
            &run,
            &policy,
            "test",
            "test-model",
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("imported or rehydrated"));
    }

    #[tokio::test]
    async fn policy_round_trips_through_engine_and_manifest() {
        let fixture = fixture().await;
        let policy = SessionRunPolicy {
            auto_resume: true,
            max_attempts: 3,
            max_age_hours: 12,
        };
        let result: SessionRunPolicyResult = fixture
            .engine
            .set_session_run_policy(&fixture.session.project_id, policy.clone())
            .await
            .unwrap();
        assert_eq!(result.policy, policy);
        let loaded = fixture
            .engine
            .session_run_policy(&fixture.session.project_id)
            .await
            .unwrap();
        assert_eq!(loaded.policy, policy);
        assert!(
            fixture
                .engine
                .session_run_policy("missing-project")
                .await
                .is_err()
        );
        let bad = SessionRunPolicy {
            max_attempts: 4,
            ..policy
        };
        assert!(
            fixture
                .engine
                .set_session_run_policy(&fixture.session.project_id, bad)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn run_journal_events_do_not_disturb_operation_receipts() {
        let fixture = fixture().await;
        let begin = crate::operation::begin(
            fixture.engine.store(),
            &fixture.session.id,
            Some("op-journal"),
            "session.run.resume",
            &serde_json::json!({"session_id": fixture.session.id, "run_id": null}),
        )
        .await
        .unwrap();
        let crate::operation::OperationStart::Proceed(receipt) = begin else {
            panic!("expected a new operation");
        };
        let journal = begin_journal(&fixture, "op", true, None).await;
        journal.finish_completed(&fixture.engine).await.unwrap();
        crate::operation::complete(&receipt, &serde_json::json!({"resumed": true}))
            .await
            .unwrap();
        let replay = crate::operation::begin(
            fixture.engine.store(),
            &fixture.session.id,
            Some("op-journal"),
            "session.run.resume",
            &serde_json::json!({"session_id": fixture.session.id, "run_id": null}),
        )
        .await
        .unwrap();
        let crate::operation::OperationStart::Replay(result) = replay else {
            panic!("completed receipt must replay");
        };
        let value: serde_json::Value = crate::operation::replay(&result).unwrap();
        assert_eq!(value["resumed"], true);
    }
}

#[cfg(test)]
mod queued_recovery_tests {
    use super::*;
    use crate::Engine;
    use std::process::Command;
    use tempfile::TempDir;
    use vp_store::Store;

    fn init_repo(path: &Path) {
        std::fs::write(path.join("README.md"), "hello\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "test@verlinpark.local"],
            vec!["config", "user.name", "VerlinPark Test"],
            vec!["add", "."],
            vec!["commit", "-q", "-m", "initial"],
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(path)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }

    #[tokio::test]
    async fn a_crash_while_a_resume_is_queued_does_not_start_a_plain_prompt() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("queued".to_string()),
            )
            .await
            .unwrap();
        let mut session = engine
            .create_session(project.id, "queued resume".into(), None)
            .await
            .unwrap();
        session.status = "idle".into();
        engine
            .update_session_status(&session.id, "idle")
            .await
            .unwrap();
        let worktree = PathBuf::from(session.worktree.as_deref().unwrap());
        let journal = crate::run_journal::RunJournal::begin(
            &engine,
            &session,
            &session.project_id,
            "leader",
            "Leader",
            "leader",
            None,
            None,
            "queued resume",
            "test",
            "test-model",
            &worktree,
            RunBudgetSnapshot::default(),
            true,
        )
        .await
        .unwrap();
        journal
            .finish_interrupted(&engine, "interrupted", "first crash")
            .await
            .unwrap();
        let old = crate::run_journal::session_runs(&engine, &session.id, 10)
            .await
            .unwrap()
            .remove(0);
        let resumed = crate::run_journal::begin_resumed_run(
            &engine,
            &session,
            &old,
            "test",
            "test-model",
            &worktree,
            RunBudgetSnapshot::default(),
        )
        .await
        .unwrap();
        let new_run_id = resumed.id().to_string();
        drop(resumed);
        engine
            .update_session_status(&session.id, "queued")
            .await
            .unwrap();
        engine
            .append_event(
                &session.id,
                "agent.queued",
                serde_json::json!({
                    "prompt": "[Durable run journal] resume",
                    "resumed_run_id": new_run_id,
                    "resumed_from_run_id": old.run_id,
                    "attempt": 2,
                }),
            )
            .await
            .unwrap();
        let recovered = engine.recover_interrupted_sessions().await.unwrap();
        assert_eq!(recovered, 1);
        let fresh = engine
            .list_sessions(Some(session.project_id.clone()))
            .await
            .unwrap()
            .into_iter()
            .find(|candidate| candidate.id == session.id)
            .unwrap();
        assert_eq!(fresh.status, "failed");
        let runs = crate::run_journal::session_runs(&engine, &session.id, 10)
            .await
            .unwrap();
        let new = runs.iter().find(|run| run.run_id == new_run_id).unwrap();
        assert_eq!(new.status, "interrupted");
        assert!(
            engine.store().queued_session_prompts().unwrap().is_empty(),
            "a queued journal note must not be replayed as a plain prompt"
        );
    }
}

#[cfg(test)]
mod journal_store_edge_tests {
    use super::*;
    use crate::Engine;
    use crate::tools::ToolOutput;
    use std::process::Command;
    use tempfile::TempDir;
    use vp_store::Store;

    fn init_repo(path: &Path) {
        std::fs::write(path.join("README.md"), "hello\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "test@verlinpark.local"],
            vec!["config", "user.name", "VerlinPark Test"],
            vec!["add", "."],
            vec!["commit", "-q", "-m", "initial"],
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(path)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }

    struct Fixture {
        _data: TempDir,
        _repo: TempDir,
        engine: Engine,
        session: vp_protocol::types::Session,
    }

    async fn fixture() -> Fixture {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("edge".into()),
            )
            .await
            .unwrap();
        let mut session = engine
            .create_session(project.id, "edge".into(), None)
            .await
            .unwrap();
        engine
            .update_session_status(&session.id, "idle")
            .await
            .unwrap();
        session.status = "idle".into();
        Fixture {
            _data: data,
            _repo: repo,
            engine,
            session,
        }
    }

    async fn journal(fixture: &Fixture, objective: &str) -> RunJournal {
        RunJournal::begin(
            &fixture.engine,
            &fixture.session,
            &fixture.session.project_id,
            "leader",
            "Leader",
            "leader",
            None,
            None,
            objective,
            "test",
            "test-model",
            Path::new(fixture.session.worktree.as_deref().unwrap()),
            RunBudgetSnapshot::default(),
            true,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn terminal_run_status_cannot_be_overwritten() {
        let fixture = fixture().await;
        let journal = journal(&fixture, "terminal").await;
        journal.finish_completed(&fixture.engine).await.unwrap();
        let store = fixture.engine.store().clone();
        let run_id = journal.id().to_string();
        let error = store
            .finish_session_run(&run_id, "failed", None)
            .unwrap_err();
        assert!(format!("{error:#}").contains("not running"));
        let runs = crate::run_journal::session_runs(&fixture.engine, &fixture.session.id, 10)
            .await
            .unwrap();
        assert_eq!(runs[0].status, "completed");
    }

    #[tokio::test]
    async fn a_durable_cancel_request_survives_a_crash_and_blocks_auto_resume() {
        let fixture = fixture().await;
        let journal = journal(&fixture, "cancel then crash").await;
        let run_id = journal.id().to_string();
        fixture
            .engine
            .append_event(
                &fixture.session.id,
                "session.run.cancel_requested",
                serde_json::json!({"run_id": run_id}),
            )
            .await
            .unwrap();
        drop(journal);
        let interrupted = crate::run_journal::interrupt_running_runs(&fixture.engine)
            .await
            .unwrap();
        assert_eq!(interrupted, 1);
        let run = crate::run_journal::session_runs(&fixture.engine, &fixture.session.id, 10)
            .await
            .unwrap()
            .remove(0);
        assert_eq!(run.status, "interrupted");
        assert_eq!(
            run.failure.as_ref().map(|failure| failure.kind.as_str()),
            Some("cancelled")
        );
        let store = fixture.engine.store().clone();
        assert!(
            store.list_resumable_session_runs(10).unwrap().is_empty(),
            "a cancelled run must not be picked up by auto resume"
        );
    }

    #[tokio::test]
    async fn recovery_ignores_completed_runs() {
        let fixture = fixture().await;
        let journal = journal(&fixture, "already done").await;
        journal.finish_completed(&fixture.engine).await.unwrap();
        let interrupted = crate::run_journal::interrupt_running_runs(&fixture.engine)
            .await
            .unwrap();
        assert_eq!(interrupted, 0);
        let runs = crate::run_journal::session_runs(&fixture.engine, &fixture.session.id, 10)
            .await
            .unwrap();
        assert_eq!(runs[0].status, "completed");
    }

    #[tokio::test]
    async fn start_rejects_a_project_that_does_not_own_the_session() {
        let fixture = fixture().await;
        let now = now_rfc3339();
        let run = SessionRun {
            run_id: "bad-run".into(),
            session_id: fixture.session.id.clone(),
            project_id: "other-project".into(),
            agent_id: "agent".into(),
            agent_name: "Agent".into(),
            agent_role: "leader".into(),
            task_id: None,
            department: None,
            objective: "bad".into(),
            provider: "test".into(),
            model: "test".into(),
            status: "running".into(),
            attempt: 1,
            resumed_from_run_id: None,
            resume_root_id: "bad-run".into(),
            top_level: true,
            started_at: now.clone(),
            updated_at: now,
            ended_at: None,
            budget: RunBudgetSnapshot::default(),
            worktree_head: String::new(),
            plan_revision: 0,
            last_event_seq: 0,
            failure: None,
        };
        let store = fixture.engine.store().clone();
        let error = store.start_session_run(&run).unwrap_err();
        assert!(format!("{error:#}").contains("does not match"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_claim_allows_exactly_one_consumer() {
        let fixture = fixture().await;
        let journal = journal(&fixture, "claim race").await;
        let (step, tool) = journal
            .tool_intent(
                &fixture.engine,
                1,
                "call-race",
                "write_file",
                &serde_json::json!({"path": "race.txt", "content": "x"}),
            )
            .await
            .unwrap();
        journal
            .tool_completed(
                &fixture.engine,
                &step,
                &tool,
                &ToolOutput::ok("wrote race.txt"),
            )
            .await
            .unwrap();
        journal
            .finish_interrupted(&fixture.engine, "interrupted", "crash")
            .await
            .unwrap();
        let old = crate::run_journal::session_runs(&fixture.engine, &fixture.session.id, 10)
            .await
            .unwrap()
            .remove(0);
        let resumed = crate::run_journal::begin_resumed_run(
            &fixture.engine,
            &fixture.session,
            &old,
            "test",
            "test-model",
            Path::new(fixture.session.worktree.as_deref().unwrap()),
            RunBudgetSnapshot::default(),
        )
        .await
        .unwrap();
        let new_run_id = resumed.id().to_string();
        let store = fixture.engine.store().clone();
        let fingerprint = tool_fingerprint(
            "write_file",
            &serde_json::json!({"path": "race.txt", "content": "x"}),
        )
        .unwrap();
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let store = store.clone();
            let run_id = new_run_id.clone();
            let fingerprint = fingerprint.clone();
            let new_run_id = new_run_id.clone();
            tasks.push(tokio::task::spawn_blocking(move || {
                store.claim_completed_tool_result(&run_id, "write_file", &fingerprint, &new_run_id)
            }));
        }
        let results = futures_util::future::join_all(tasks).await;
        let claimed = results
            .into_iter()
            .map(|result| result.unwrap().unwrap())
            .filter(Option::is_some)
            .count();
        assert_eq!(claimed, 1);
    }

    #[tokio::test]
    async fn run_list_order_and_limit_are_deterministic() {
        let fixture = fixture().await;
        let first = journal(&fixture, "first").await;
        first.finish_completed(&fixture.engine).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let second = journal(&fixture, "second").await;
        second.finish_completed(&fixture.engine).await.unwrap();
        let all = crate::run_journal::session_runs(&fixture.engine, &fixture.session.id, 10)
            .await
            .unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].run_id, second.id());
        assert_eq!(all[1].run_id, first.id());
        let limited = crate::run_journal::session_runs(&fixture.engine, &fixture.session.id, 1)
            .await
            .unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].run_id, second.id());
    }

    #[tokio::test]
    async fn lineage_note_includes_tools_from_an_earlier_attempt() {
        let fixture = fixture().await;
        let first = journal(&fixture, "lineage").await;
        let (step, tool) = first
            .tool_intent(
                &fixture.engine,
                1,
                "call-first",
                "write_file",
                &serde_json::json!({"path": "first.txt", "content": "1"}),
            )
            .await
            .unwrap();
        first
            .tool_completed(
                &fixture.engine,
                &step,
                &tool,
                &ToolOutput::ok("wrote first.txt"),
            )
            .await
            .unwrap();
        first
            .finish_interrupted(&fixture.engine, "interrupted", "crash")
            .await
            .unwrap();
        let old = crate::run_journal::session_runs(&fixture.engine, &fixture.session.id, 10)
            .await
            .unwrap()
            .remove(0);
        let second = crate::run_journal::begin_resumed_run(
            &fixture.engine,
            &fixture.session,
            &old,
            "test",
            "test-model",
            Path::new(fixture.session.worktree.as_deref().unwrap()),
            RunBudgetSnapshot::default(),
        )
        .await
        .unwrap();
        second
            .finish_interrupted(&fixture.engine, "interrupted", "second crash")
            .await
            .unwrap();
        let second_run = crate::run_journal::session_runs(&fixture.engine, &fixture.session.id, 10)
            .await
            .unwrap()
            .into_iter()
            .find(|run| run.run_id == second.id())
            .unwrap();
        let policy = SessionRunPolicy {
            auto_resume: false,
            max_attempts: 3,
            max_age_hours: 24,
        };
        let plan = preflight(
            &fixture.engine,
            &fixture.session,
            &second_run,
            &policy,
            "test",
            "test-model",
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
        assert!(plan.note.contains("first.txt"));
        assert!(plan.note.contains("wrote first.txt"));
    }

    #[tokio::test]
    async fn repair_reverts_corrupted_step_and_checkpoint_rows() {
        let fixture = fixture().await;
        let journal = journal(&fixture, "repair rows").await;
        journal
            .checkpoint(&fixture.engine, 1, RunBudgetSnapshot::default())
            .await
            .unwrap();
        let request = ChatRequest::new("test-model", vec![vp_providers::ChatMessage::user("go")]);
        let step = journal
            .model_intent(&fixture.engine, 1, &request)
            .await
            .unwrap();
        journal
            .model_completed(&fixture.engine, &step, None, "done", &[], 1, 1, 0, 0, false)
            .await
            .unwrap();
        journal.finish_completed(&fixture.engine).await.unwrap();
        let runs = crate::run_journal::session_runs(&fixture.engine, &fixture.session.id, 10)
            .await
            .unwrap();
        let mut steps = fixture
            .engine
            .session_run_detail(&runs[0].run_id)
            .await
            .unwrap()
            .steps;
        let mut checkpoints = fixture
            .engine
            .session_run_detail(&runs[0].run_id)
            .await
            .unwrap()
            .checkpoints;
        steps[0].status = "failed".into();
        checkpoints[0].turn = 99;
        fixture
            .engine
            .store()
            .replace_run_journal_projections(&fixture.session.id, &runs, &steps, &checkpoints)
            .unwrap();
        let report = fixture
            .engine
            .replay_session(&fixture.session.id)
            .await
            .unwrap();
        assert!(!report.run_steps_match);
        assert!(!report.run_checkpoints_match);
        fixture
            .engine
            .repair_session(&fixture.session.id)
            .await
            .unwrap();
        let repaired = fixture
            .engine
            .replay_session(&fixture.session.id)
            .await
            .unwrap();
        assert!(repaired.run_steps_match, "{:?}", repaired.mismatches);
        assert!(repaired.run_checkpoints_match, "{:?}", repaired.mismatches);
    }
}
