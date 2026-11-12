//! Durable process tool for long-lived sandboxed children.
//!
//! The tool is a thin protocol layer over [`crate::process_supervisor`]. The
//! daemon owns the row, log file, lifecycle events, restart reconciliation, and
//! idle sweep; a provider turn only starts, inspects, waits for, or stops one.
//! No command is ever restarted automatically.

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use regex::Regex;
use serde_json::{Value, json};

use crate::process_supervisor::{
    MAX_OUTPUT_BYTES, MAX_OUTPUT_LINES, MIN_OUTPUT_BYTES, MIN_OUTPUT_LINES, StartSpec,
};
use crate::tooling::errors::ToolErrorCode;
use crate::tools::{AgentTool, ToolContext, ToolOutput, truncate};

pub struct ProcessTool;
pub struct WaitFor;

#[async_trait]
impl AgentTool for ProcessTool {
    fn name(&self) -> &'static str {
        "process"
    }

    fn description(&self) -> &'static str {
        "Start, inspect, read logs from, wait for, or stop a durable sandboxed process. Processes survive a daemon restart when their identity can be verified. Boot restart is opt-in per project policy, bounded, and never silent."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["start", "list", "status", "logs", "read", "wait", "stop", "signal", "write"]},
                "command": {"type": "string", "description": "Shell command for action=start; max 16 KiB"},
                "name": {"type": "string", "description": "Human label; default is the first command word"},
                "cwd": {"type": "string", "description": "Worktree-relative working directory"},
                "env": {"type": "object", "description": "Explicit environment variables; max 32 keys, VERLINPARK_* reserved"},
                "process_id": {"type": "string", "description": "Required for status/logs/read/wait/stop/signal/write"},
                "offset": {"type": "integer", "minimum": 0, "description": "Byte offset for action=logs"},
                "from_end": {"type": "boolean", "description": "Read the newest retained bytes for action=logs"},
                "max_bytes": {"type": "integer", "minimum": 1, "maximum": 1048576, "description": "Log page byte cap; default 65536"},
                "max_lines": {"type": "integer", "minimum": 1, "maximum": 20000, "description": "Log page line cap; default 20000"},
                "max_output_bytes": {"type": "integer", "minimum": 4096, "maximum": 4194304, "description": "Retained log byte cap; default 1 MiB"},
                "max_output_lines": {"type": "integer", "minimum": 100, "maximum": 50000, "description": "Retained log line cap; default 20000"},
                "pattern": {"type": "string", "description": "Regex pattern for action=wait"},
                "timeout_secs": {"type": "integer", "minimum": 1, "maximum": 3600, "description": "Wait deadline; default 30. For start it is the idle TTL; default 3600"},
                "signal": {"type": "string", "enum": ["TERM", "KILL", "INT", "HUP"], "description": "Default TERM"},
                "grace_secs": {"type": "integer", "minimum": 1, "maximum": 30, "description": "TERM-to-KILL grace for action=stop; default 5"},
                "include_finished": {"type": "boolean", "description": "Include exited/stopped/lost rows in action=list; default true"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 200, "description": "List cap; default 50"},
                "reset": {"type": "boolean", "description": "Legacy alias for from_end=false with offset=0"}
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        if let Some(snapshot) =
            crate::preemption::current_token().and_then(|token| token.snapshot())
        {
            return Ok(crate::preemption::tool_output("process", &snapshot));
        }
        let action = input
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("action is required"))?;
        let (session_id, supervisor) = match (
            context.session_id.as_deref(),
            context.process_supervisor.as_ref(),
        ) {
            (Some(session_id), Some(supervisor)) => (session_id, supervisor),
            _ => {
                return Ok(ToolOutput::error_coded(
                    "process",
                    ToolErrorCode::PROCESS_FAILED,
                    "durable process supervision is unavailable in this context",
                ));
            }
        };
        match action {
            "start" => start_process(context, session_id, supervisor, &input).await,
            "list" => list_processes(session_id, supervisor, &input),
            "status" => status_process(session_id, supervisor, &input),
            "logs" => logs_process(session_id, supervisor, &input),
            "read" => legacy_read_process(session_id, supervisor, &input),
            "wait" => wait_process(session_id, supervisor, &input).await,
            "stop" => stop_process(session_id, supervisor, &input).await,
            "signal" => signal_process(session_id, supervisor, &input),
            "write" => Ok(ToolOutput::error_coded(
                "process",
                ToolErrorCode::PROCESS_IO_FAILED,
                "durable processes run with detached stdin; use files, arguments, or a network protocol instead",
            )),
            _ => Ok(ToolOutput::error_coded(
                "process",
                ToolErrorCode::INVALID_ARGUMENT,
                format!("unsupported process action: {action}"),
            )),
        }
    }
}

async fn start_process(
    context: &ToolContext,
    session_id: &str,
    supervisor: &std::sync::Arc<crate::process_supervisor::ProcessSupervisor>,
    input: &Value,
) -> Result<ToolOutput> {
    let command = input
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .ok_or_else(|| anyhow!("command is required for action=start"))?
        .to_string();
    let name = input
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string);
    let cwd = input.get("cwd").and_then(Value::as_str).map(str::to_string);
    let mut env = std::collections::BTreeMap::new();
    if let Some(values) = input.get("env").and_then(Value::as_object) {
        if values.len() > 32 {
            return Ok(ToolOutput::error_coded(
                "process",
                ToolErrorCode::INVALID_ARGUMENT,
                "env accepts at most 32 entries",
            ));
        }
        for (key, value) in values {
            let Some(value) = value.as_str() else {
                return Ok(ToolOutput::error_coded(
                    "process",
                    ToolErrorCode::INVALID_ARGUMENT,
                    format!("environment variable {key} must be a string"),
                ));
            };
            env.insert(key.clone(), value.to_string());
        }
    }
    let idle_ttl_secs = input
        .get("timeout_secs")
        .and_then(Value::as_u64)
        .unwrap_or(3_600)
        .clamp(1, 86_400);
    let max_output_bytes = input
        .get("max_output_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(1_048_576)
        .clamp(MIN_OUTPUT_BYTES, MAX_OUTPUT_BYTES);
    let max_output_lines = input
        .get("max_output_lines")
        .and_then(Value::as_u64)
        .unwrap_or(20_000)
        .clamp(MIN_OUTPUT_LINES, MAX_OUTPUT_LINES);
    let spec = StartSpec {
        command: command.clone(),
        name: name.clone(),
        cwd,
        env,
        idle_ttl_secs,
        max_output_bytes,
        max_output_lines,
    };
    match supervisor.start(session_id, context, &spec).await {
        Ok(row) => Ok(ToolOutput::ok_structured(
            format!(
                "started {} ({}) · running · pid {}",
                row.id,
                row.name,
                row.pid.unwrap_or(0)
            ),
            json!({
                "process_id": row.id,
                "name": row.name,
                "command": row.command,
                "pid": row.pid,
                "status": row.state,
                "state": row.state,
                "started_at": row.started_at,
                "idle_ttl_secs": row.idle_ttl_secs,
                "max_output_bytes": row.max_output_bytes,
                "max_output_lines": row.max_output_lines,
                "process": row,
                "next_actions": [
                    format!("process action=logs process_id={}", row.id),
                    format!("process action=wait process_id={} pattern=<ready>", row.id),
                ],
            }),
        )),
        Err(error) => {
            let message = error.to_string();
            if message.contains("process limit") {
                Ok(ToolOutput::error_coded(
                    "process",
                    ToolErrorCode::PROCESS_LIMIT,
                    message,
                ))
            } else {
                Ok(ToolOutput::error_coded(
                    "process",
                    ToolErrorCode::PROCESS_SPAWN_FAILED,
                    message,
                ))
            }
        }
    }
}

fn list_processes(
    session_id: &str,
    supervisor: &crate::process_supervisor::ProcessSupervisor,
    input: &Value,
) -> Result<ToolOutput> {
    let include_finished = input
        .get("include_finished")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .clamp(1, 200) as u32;
    let rows = supervisor.list(session_id, include_finished, limit)?;
    let active = supervisor.active_count(session_id)?;
    let rendered = rows
        .iter()
        .map(|row| {
            format!(
                "{} {} · {} · pid {} · {} bytes{}",
                row.id,
                row.name,
                row.state,
                row.pid.unwrap_or(0),
                row.output_bytes,
                row.exit_code
                    .map(|code| format!(" · exit {code}"))
                    .unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(ToolOutput::ok_structured(
        truncate(if rendered.is_empty() {
            "(no supervised processes)".to_string()
        } else {
            rendered
        }),
        json!({
            "processes": rows,
            "count": rows.len(),
            "active": active,
            "limit": supervisor.policy_for_session(session_id).max_processes,
            "include_finished": include_finished,
        }),
    ))
}

fn status_process(
    session_id: &str,
    supervisor: &crate::process_supervisor::ProcessSupervisor,
    input: &Value,
) -> Result<ToolOutput> {
    let id = process_id(input)?;
    let mut row = supervisor.get(session_id, id)?;
    supervisor.touch(&mut row);
    let restart = if row.restart_attempts > 0 || row.state == "blocked" {
        format!(
            " · restart {}{}",
            row.restart_attempts,
            row.restart_blocked_reason
                .as_deref()
                .map(|reason| format!(" ({reason})"))
                .unwrap_or_default()
        )
    } else {
        String::new()
    };
    let text = format!(
        "{} {} · {}{}{}{}",
        row.id,
        row.name,
        row.state,
        row.pid
            .map(|pid| format!(" · pid {pid}"))
            .unwrap_or_default(),
        row.exit_code
            .map(|code| format!(" · exit {code}"))
            .unwrap_or_default(),
        restart
    );
    Ok(ToolOutput::ok_structured(
        text,
        json!({
            "process_id": row.id,
            "process": row,
        }),
    ))
}

fn logs_process(
    session_id: &str,
    supervisor: &crate::process_supervisor::ProcessSupervisor,
    input: &Value,
) -> Result<ToolOutput> {
    let id = process_id(input)?;
    let offset = input.get("offset").and_then(Value::as_u64);
    let max_bytes = input.get("max_bytes").and_then(Value::as_u64);
    let max_lines = input.get("max_lines").and_then(Value::as_u64);
    let from_end = input
        .get("from_end")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let result = supervisor.read_logs(
        session_id,
        id,
        offset,
        max_bytes.map(|value| value.min(u32::MAX as u64) as u32),
        max_lines.map(|value| value.min(u32::MAX as u64) as u32),
        from_end,
    )?;
    let mut text = String::new();
    if result.rotated {
        text.push_str("[older output was dropped by bounded rotation]\n");
    }
    if !result.content.is_empty() {
        text.push_str(&result.content);
        if !text.ends_with('\n') {
            text.push('\n');
        }
    }
    if text.is_empty() {
        text.push_str("(no output)");
    }
    Ok(ToolOutput::ok_structured(
        truncate(text),
        json!({
            "process_id": result.process_id,
            "state": result.state,
            "content": result.content,
            "offset": result.offset,
            "next_offset": result.next_offset,
            "total_bytes": result.total_bytes,
            "eof": result.eof,
            "truncated": result.truncated,
            "rotated": result.rotated,
            "text": result.content,
            "stdout": result.content,
        }),
    ))
}

fn legacy_read_process(
    session_id: &str,
    supervisor: &crate::process_supervisor::ProcessSupervisor,
    input: &Value,
) -> Result<ToolOutput> {
    let id = process_id(input)?;
    let reset = input.get("reset").and_then(Value::as_bool).unwrap_or(false);
    let offset = if reset {
        Some(0)
    } else {
        input.get("offset").and_then(Value::as_u64)
    };
    let max_bytes = input
        .get("max_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(65_536)
        .min(u32::MAX as u64) as u32;
    let from_end = !reset && offset.is_none();
    let result = supervisor.read_logs(session_id, id, offset, Some(max_bytes), None, from_end)?;
    let text = if result.content.is_empty() {
        "(no new output)".to_string()
    } else {
        result.content.clone()
    };
    Ok(ToolOutput::ok_structured(
        truncate(format!(
            "{}{}",
            if result.rotated {
                "[older output was dropped by bounded rotation]\n"
            } else {
                ""
            },
            text
        )),
        json!({
            "process_id": result.process_id,
            "state": result.state,
            "done": result.state != "running" && result.state != "starting",
            "content": result.content,
            "stdout": result.content,
            "offset": result.offset,
            "next_offset": result.next_offset,
            "total_bytes": result.total_bytes,
            "eof": result.eof,
            "truncated": result.truncated,
            "tail": result.content,
        }),
    ))
}

async fn wait_process(
    session_id: &str,
    supervisor: &crate::process_supervisor::ProcessSupervisor,
    input: &Value,
) -> Result<ToolOutput> {
    let id = process_id(input)?;
    let timeout_secs = input
        .get("timeout_secs")
        .and_then(Value::as_u64)
        .unwrap_or(30)
        .clamp(1, 3_600);
    let pattern = input
        .get("pattern")
        .and_then(Value::as_str)
        .map(|pattern| {
            Regex::new(pattern).map_err(|error| anyhow!("invalid wait pattern: {error}"))
        })
        .transpose()?;
    let outcome = match crate::preemption::current_token() {
        Some(token) => {
            tokio::select! {
                _ = token.cancelled() => {
                    let mut snapshot = token.snapshot().unwrap_or_else(|| {
                        crate::preemption::fallback_snapshot(
                            crate::preemption::Phase::Tool,
                            Some("process"),
                        )
                    });
                    // Waiting for a process cannot mutate it; only the start
                    // and stop actions have side effects.
                    snapshot.side_effects_unknown = false;
                    return Ok(crate::preemption::tool_output("process", &snapshot));
                }
                outcome = supervisor.wait(session_id, id, timeout_secs, pattern.as_ref()) => outcome?,
            }
        }
        None => {
            supervisor
                .wait(session_id, id, timeout_secs, pattern.as_ref())
                .await?
        }
    };
    if outcome.matched {
        return Ok(ToolOutput::ok_structured(
            format!("pattern matched after {}ms", outcome.elapsed_ms),
            json!({
                "process_id": outcome.process.id,
                "matched": true,
                "elapsed_ms": outcome.elapsed_ms,
                "evidence": outcome.evidence,
                "process": outcome.process,
            }),
        ));
    }
    if outcome.timed_out {
        return Ok(ToolOutput::error_coded(
            "process",
            ToolErrorCode::WAIT_TIMEOUT,
            format!("wait timed out after {timeout_secs}s"),
        )
        .with_structured(json!({
            "process_id": outcome.process.id,
            "timeout_secs": timeout_secs,
            "state": outcome.process.state,
        })));
    }
    if outcome.process.state == "exited" || outcome.process.state == "stopped" {
        return Ok(ToolOutput::error_coded(
            "process",
            ToolErrorCode::PROCESS_EXITED,
            format!(
                "{} {} {} before the condition matched",
                outcome.process.id,
                outcome.process.state,
                outcome
                    .process
                    .exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "without an exit code".to_string())
            ),
        )
        .with_structured(json!({
            "process_id": outcome.process.id,
            "exit_code": outcome.process.exit_code,
            "state": outcome.process.state,
            "tail": outcome.evidence,
        })));
    }
    Ok(ToolOutput::error_coded(
        "process",
        ToolErrorCode::PROCESS_LOST,
        format!(
            "{} entered state {} before the condition matched",
            outcome.process.id, outcome.process.state
        ),
    )
    .with_structured(json!({
        "process_id": outcome.process.id,
        "state": outcome.process.state,
        "tail": outcome.evidence,
    })))
}

async fn stop_process(
    session_id: &str,
    supervisor: &crate::process_supervisor::ProcessSupervisor,
    input: &Value,
) -> Result<ToolOutput> {
    let id = process_id(input)?;
    let signal = input.get("signal").and_then(Value::as_str);
    let grace_secs = input.get("grace_secs").and_then(Value::as_u64);
    match supervisor
        .stop(session_id, id, signal, grace_secs, "stopped by operator")
        .await
    {
        Ok(result) => Ok(ToolOutput::ok_structured(
            format!(
                "{} stopped{} after {}ms",
                result.process.id,
                if result.forced { " (forced)" } else { "" },
                result.waited_ms
            ),
            json!({
                "process_id": result.process.id,
                "done": true,
                "state": result.process.state,
                "exit_code": result.process.exit_code,
                "signal": result.process.signal,
                "forced": result.forced,
                "waited_ms": result.waited_ms,
                "process": result.process,
            }),
        )),
        Err(error) => Ok(ToolOutput::error_coded(
            "process",
            ToolErrorCode::PROCESS_STOP_TIMEOUT,
            error.to_string(),
        )),
    }
}

fn signal_process(
    session_id: &str,
    supervisor: &crate::process_supervisor::ProcessSupervisor,
    input: &Value,
) -> Result<ToolOutput> {
    let id = process_id(input)?;
    let signal = input
        .get("signal")
        .and_then(Value::as_str)
        .unwrap_or("TERM");
    let sent = supervisor.signal(session_id, id, signal)?;
    Ok(ToolOutput::ok_structured(
        format!("sent signal {signal} to {id}"),
        json!({
            "process_id": id,
            "signal": signal,
            "sent": sent,
        }),
    ))
}

fn process_id(input: &Value) -> Result<&str> {
    input
        .get("process_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow!("process_id is required"))
}

#[async_trait]
impl AgentTool for WaitFor {
    fn name(&self) -> &'static str {
        "wait_for"
    }

    fn description(&self) -> &'static str {
        "Wait deterministically for a local TCP port, a localhost URL, or a durable process log pattern. No sleep guessing."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "port": {"type": "integer", "minimum": 1, "maximum": 65535, "description": "Local TCP port"},
                "url": {"type": "string", "description": "Localhost http(s) URL"},
                "process_id": {"type": "string", "description": "Durable process to inspect for a log pattern"},
                "pattern": {"type": "string", "description": "Regex pattern when process_id is used"},
                "timeout_secs": {"type": "integer", "minimum": 1, "maximum": 3600, "description": "Default 30"},
                "interval_ms": {"type": "integer", "minimum": 50, "maximum": 5000, "description": "Default 200"}
            }
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let timeout_secs = input
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(30)
            .clamp(1, 3_600);
        let interval = input
            .get("interval_ms")
            .and_then(Value::as_u64)
            .unwrap_or(200)
            .clamp(50, 5_000);
        let port = input
            .get("port")
            .and_then(Value::as_u64)
            .map(|port| port as u16);
        let url = input.get("url").and_then(Value::as_str);
        let process_id = input.get("process_id").and_then(Value::as_str);
        let pattern = input.get("pattern").and_then(Value::as_str);
        if port.is_none() && url.is_none() && process_id.is_none() {
            return Ok(ToolOutput::error_coded(
                "wait_for",
                ToolErrorCode::INVALID_ARGUMENT,
                "wait_for requires port, url, or process_id with pattern",
            ));
        }
        if process_id.is_some() && pattern.is_none() {
            return Ok(ToolOutput::error_coded(
                "wait_for",
                ToolErrorCode::INVALID_ARGUMENT,
                "process_id requires a regex pattern",
            ));
        }
        let compiled = pattern
            .map(|pattern| {
                Regex::new(pattern).map_err(|error| anyhow!("invalid wait pattern: {error}"))
            })
            .transpose()?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        let started = std::time::Instant::now();
        let mut last_error = String::new();
        loop {
            if let Some(port) = port {
                match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
                    Ok(_) => {
                        return Ok(ToolOutput::ok_structured(
                            format!("port {port} is accepting connections"),
                            json!({
                                "ready": true,
                                "kind": "port",
                                "port": port,
                                "elapsed_ms": started.elapsed().as_millis() as u64,
                            }),
                        ));
                    }
                    Err(error) => last_error = error.to_string(),
                }
            }
            if let Some(url) = url {
                if !is_local_url(url) {
                    return Ok(ToolOutput::error_coded(
                        "wait_for",
                        ToolErrorCode::NETWORK_DENIED,
                        "wait_for url must be localhost, 127.0.0.1, or [::1]",
                    ));
                }
                match reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(std::time::Duration::from_secs(2))
                    .build()
                {
                    Ok(client) => match client.get(url).send().await {
                        Ok(response) => {
                            let status = response.status().as_u16();
                            return Ok(ToolOutput::ok_structured(
                                format!("{url} responded {status}"),
                                json!({
                                    "ready": true,
                                    "kind": "url",
                                    "url": url,
                                    "status": status,
                                    "elapsed_ms": started.elapsed().as_millis() as u64,
                                }),
                            ));
                        }
                        Err(error) => last_error = error.to_string(),
                    },
                    Err(error) => last_error = error.to_string(),
                }
            }
            if let (Some(process_id), Some(pattern)) = (process_id, compiled.as_ref()) {
                let (session_id, supervisor) = match (
                    context.session_id.as_deref(),
                    context.process_supervisor.as_ref(),
                ) {
                    (Some(session_id), Some(supervisor)) => (session_id, supervisor),
                    _ => {
                        return Ok(ToolOutput::error_coded(
                            "wait_for",
                            ToolErrorCode::PROCESS_FAILED,
                            "durable process supervision is unavailable in this context",
                        ));
                    }
                };
                let mut row = supervisor.get(session_id, process_id)?;
                let logs = supervisor.read_logs(
                    session_id,
                    process_id,
                    None,
                    Some(256 * 1024),
                    None,
                    true,
                )?;
                if pattern.is_match(&logs.content) {
                    supervisor.touch(&mut row);
                    return Ok(ToolOutput::ok_structured(
                        format!(
                            "process pattern matched after {}ms",
                            started.elapsed().as_millis()
                        ),
                        json!({
                            "ready": true,
                            "kind": "process_pattern",
                            "process_id": process_id,
                            "elapsed_ms": started.elapsed().as_millis() as u64,
                            "evidence": logs.content.chars().take(4_000).collect::<String>(),
                        }),
                    ));
                }
                if matches!(row.state.as_str(), "exited" | "stopped" | "lost") {
                    return Ok(ToolOutput::error_coded(
                        "wait_for",
                        ToolErrorCode::PROCESS_EXITED,
                        format!("{} {} before the pattern matched", row.id, row.state),
                    )
                    .with_structured(json!({
                        "process_id": row.id,
                        "state": row.state,
                        "exit_code": row.exit_code,
                        "tail": logs.content.chars().take(2_000).collect::<String>(),
                    })));
                }
                supervisor.touch(&mut row);
            }
            if std::time::Instant::now() >= deadline {
                return Ok(ToolOutput::error_coded(
                    "wait_for",
                    ToolErrorCode::WAIT_TIMEOUT,
                    format!("wait_for timed out after {timeout_secs}s"),
                )
                .with_structured(json!({
                    "timeout_secs": timeout_secs,
                    "last_error": last_error,
                })));
            }
            tokio::time::sleep(std::time::Duration::from_millis(interval)).await;
        }
    }
}

fn is_local_url(url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    matches!(
        host.trim_matches(['[', ']']).to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "::1"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_supervisor::ProcessSupervisor;
    use std::path::Path;
    use std::sync::Arc;
    use tempfile::TempDir;
    use vp_store::Store;

    fn context_for(root: &Path, data_dir: &Path, store: &Store, session_id: &str) -> ToolContext {
        ToolContext {
            root: root.to_path_buf(),
            project_root: root.to_path_buf(),
            sandbox_root: data_dir.join("sandbox").join(session_id),
            secret_roots: Vec::new(),
            network_allowed: true,
            home: root.join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
            remote: None,
            session_id: Some(session_id.to_string()),
            process_supervisor: Some(Arc::new(ProcessSupervisor::new(store.clone()))),
            test_cache: None,
        }
    }

    fn store_with_session(root: &Path) -> (TempDir, Store, String) {
        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(root.to_string_lossy().as_ref(), None)
            .unwrap();
        let session = store
            .create_session(&project.id, "durable process tool test")
            .unwrap();
        (data, store, session.id)
    }

    #[tokio::test]
    async fn starts_reads_and_stops_a_process() {
        let dir = TempDir::new().unwrap();
        let (data, store, session_id) = store_with_session(dir.path());
        let context = context_for(dir.path(), data.path(), &store, &session_id);
        let started = ProcessTool
            .execute(
                &context,
                json!({"action": "start", "command": "printf 'ready\\n'; sleep 30"}),
            )
            .await
            .unwrap();
        assert!(!started.is_error, "{}", started.content);
        let id = started.structured.unwrap()["process_id"]
            .as_str()
            .unwrap()
            .to_string();
        let waited = WaitFor
            .execute(
                &context,
                json!({"process_id": id, "pattern": "ready", "timeout_secs": 5}),
            )
            .await
            .unwrap();
        assert!(!waited.is_error, "{}", waited.content);
        let read = ProcessTool
            .execute(&context, json!({"action": "logs", "process_id": id}))
            .await
            .unwrap();
        assert!(!read.is_error, "{}", read.content);
        assert!(read.content.contains("ready"));
        let stopped = ProcessTool
            .execute(&context, json!({"action": "stop", "process_id": id}))
            .await
            .unwrap();
        assert!(!stopped.is_error, "{}", stopped.content);
    }

    #[tokio::test]
    async fn rejects_cwd_escape_and_unknown_processes() {
        let dir = TempDir::new().unwrap();
        let (data, store, session_id) = store_with_session(dir.path());
        let context = context_for(dir.path(), data.path(), &store, &session_id);
        let escape = ProcessTool
            .execute(
                &context,
                json!({"action": "start", "command": "true", "cwd": "../"}),
            )
            .await;
        assert!(escape.is_err() || escape.unwrap().is_error);
        let unknown = ProcessTool
            .execute(
                &context,
                json!({"action": "status", "process_id": "proc_missing"}),
            )
            .await;
        assert!(unknown.is_err());
    }

    #[tokio::test]
    async fn preemption_aborts_a_process_wait_without_mutating_the_process() {
        let dir = TempDir::new().unwrap();
        let (data, store, session_id) = store_with_session(dir.path());
        let context = context_for(dir.path(), data.path(), &store, &session_id);
        let started = ProcessTool
            .execute(
                &context,
                json!({"action": "start", "command": "printf 'ready\\n'; sleep 30"}),
            )
            .await
            .unwrap();
        assert!(!started.is_error, "{}", started.content);
        let id = started.structured.unwrap()["process_id"]
            .as_str()
            .unwrap()
            .to_string();
        let token = crate::preemption::Token::disabled();
        let trigger = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            trigger.trigger(
                crate::preemption::Reason::Explicit,
                crate::preemption::Phase::Tool,
                false,
            );
        });
        let started_at = std::time::Instant::now();
        let waited = crate::preemption::scope_token(
            token,
            ProcessTool.execute(
                &context,
                json!({
                    "action": "wait",
                    "process_id": id,
                    "pattern": "never-matches",
                    "timeout_secs": 60
                }),
            ),
        )
        .await
        .unwrap();
        assert!(waited.is_error, "{}", waited.content);
        assert!(started_at.elapsed() < std::time::Duration::from_secs(3));
        let snapshot = crate::preemption::output_snapshot(&waited).expect("typed preemption");
        assert!(!snapshot.side_effects_unknown, "waiting is read-only");
        let status = ProcessTool
            .execute(&context, json!({"action": "status", "process_id": id}))
            .await
            .unwrap();
        assert!(!status.is_error, "{}", status.content);
        assert!(status.content.contains("running") || status.content.contains("ready"));
        let stopped = ProcessTool
            .execute(&context, json!({"action": "stop", "process_id": id}))
            .await
            .unwrap();
        assert!(!stopped.is_error, "{}", stopped.content);
    }
}
