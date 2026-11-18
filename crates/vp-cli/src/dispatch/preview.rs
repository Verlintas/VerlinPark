//! Preview console, terminal, and workspace file commands.

use super::*;

const STREAM_TIMEOUT_SECS: u64 = 300;

fn stream_text(json: bool, text: &str) {
    if json {
        eprint!("{text}");
        let _ = std::io::stderr().flush();
    } else {
        print!("{text}");
        let _ = std::io::stdout().flush();
    }
}

fn stream_note(json: bool, text: &str) {
    if json {
        eprintln!("{text}");
    } else {
        println!("{text}");
    }
}

/// Close/signal calls are best-effort cleanup: never hold the CLI after the
/// stream already failed or timed out.
async fn best_effort_cleanup(discovery: &Discovery, method: &str, params: Value) {
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        rpc(discovery, method, params),
    )
    .await;
}

/// A remote command that exits nonzero, is killed, or ends without an exit
/// code must not be reported as a successful CLI invocation.
pub(crate) fn streamed_exit_error(
    kind: &str,
    status: &str,
    exit_code: Option<i64>,
) -> Option<String> {
    match exit_code {
        Some(0) => None,
        Some(code) => Some(format!("{kind} command exited with code {code}")),
        None if status == "running" => Some(format!("{kind} command did not finish")),
        None => Some(format!(
            "{kind} command ended with status {status} and no exit code"
        )),
    }
}

async fn console_stream(
    discovery: &Discovery,
    session: &str,
    command: &str,
    json: bool,
) -> Result<Option<Value>> {
    let opened = rpc(discovery, "console.open", json!({"session_id": session})).await?;
    let console_id = opened["console_id"]
        .as_str()
        .context("console.open returned no console_id")?
        .to_string();
    stream_note(
        json,
        &format!(
            "console ready in {}",
            opened["worktree"].as_str().unwrap_or("?")
        ),
    );
    let outcome = async {
        rpc(
            discovery,
            "console.write",
            json!({
                "console_id": console_id,
                "data": format!("{command}\nexit\n")
            }),
        )
        .await?;
        let mut after_seq = 0_u64;
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(STREAM_TIMEOUT_SECS);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                best_effort_cleanup(
                    discovery,
                    "console.signal",
                    json!({"console_id": console_id, "signal": "kill"}),
                )
                .await;
                bail!("console command timed out after {STREAM_TIMEOUT_SECS}s");
            }
            let result = match tokio::time::timeout(
                remaining,
                rpc(
                    discovery,
                    "console.read",
                    json!({"console_id": console_id, "after_seq": after_seq}),
                ),
            )
            .await
            {
                Ok(result) => result?,
                Err(_) => {
                    best_effort_cleanup(
                        discovery,
                        "console.signal",
                        json!({"console_id": console_id, "signal": "kill"}),
                    )
                    .await;
                    bail!("console command timed out after {STREAM_TIMEOUT_SECS}s");
                }
            };
            if let Some(chunks) = result["chunks"].as_array() {
                for chunk in chunks {
                    if let Some(text) = chunk["text"].as_str() {
                        stream_text(json, text);
                    }
                }
            }
            after_seq = result["next_seq"].as_u64().unwrap_or(after_seq);
            let status = result["status"].as_str().unwrap_or("running");
            if status != "running" {
                return Ok((status.to_string(), result["exit_code"].as_i64(), after_seq));
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
    .await;
    best_effort_cleanup(
        discovery,
        "console.close",
        json!({"console_id": console_id}),
    )
    .await;
    let (status, exit_code, _) = outcome?;
    stream_note(
        json,
        &format!(
            "[console {status}, exit {}]",
            exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "?".into())
        ),
    );
    if let Some(error) = streamed_exit_error("console", &status, exit_code) {
        bail!(error);
    }
    if json {
        Ok(Some(json!({
            "console_id": console_id,
            "streamed": true,
            "status": status,
            "exit_code": exit_code,
        })))
    } else {
        Ok(None)
    }
}

async fn terminal_stream(
    discovery: &Discovery,
    session: &str,
    command: &str,
    json: bool,
) -> Result<Option<Value>> {
    let opened = rpc(
        discovery,
        "terminal.open",
        json!({"session_id": session, "cols": 100, "rows": 30}),
    )
    .await?;
    let terminal_id = opened["terminal_id"]
        .as_str()
        .context("terminal.open returned no terminal_id")?
        .to_string();
    stream_note(
        json,
        &format!(
            "terminal ready in {}",
            opened["worktree"].as_str().unwrap_or("?")
        ),
    );
    let outcome = async {
        let data = BASE64.encode(format!("{command}\nexit\n"));
        rpc(
            discovery,
            "terminal.write",
            json!({"terminal_id": terminal_id, "data": data}),
        )
        .await?;
        let mut after_seq = 0_u64;
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(STREAM_TIMEOUT_SECS);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                bail!("terminal command timed out after {STREAM_TIMEOUT_SECS}s");
            }
            let result = match tokio::time::timeout(
                remaining,
                rpc(
                    discovery,
                    "terminal.read",
                    json!({"terminal_id": terminal_id, "after_seq": after_seq}),
                ),
            )
            .await
            {
                Ok(result) => result?,
                Err(_) => bail!("terminal command timed out after {STREAM_TIMEOUT_SECS}s"),
            };
            if let Some(chunks) = result["chunks"].as_array() {
                for chunk in chunks {
                    if let Some(data) = chunk["data"].as_str()
                        && let Ok(bytes) = BASE64.decode(data)
                    {
                        stream_text(json, &String::from_utf8_lossy(&bytes));
                    }
                }
            }
            after_seq = result["next_seq"].as_u64().unwrap_or(after_seq);
            let status = result["status"].as_str().unwrap_or("running");
            if status != "running" {
                return Ok((status.to_string(), result["exit_code"].as_i64()));
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
    .await;
    best_effort_cleanup(
        discovery,
        "terminal.close",
        json!({"terminal_id": terminal_id}),
    )
    .await;
    let (status, exit_code) = outcome?;
    stream_note(
        json,
        &format!(
            "[terminal {status}, exit {}]",
            exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "?".into())
        ),
    );
    if let Some(error) = streamed_exit_error("terminal", &status, exit_code) {
        bail!(error);
    }
    if json {
        Ok(Some(json!({
            "terminal_id": terminal_id,
            "streamed": true,
            "status": status,
            "exit_code": exit_code,
        })))
    } else {
        Ok(None)
    }
}

pub(super) async fn dispatch(
    command: Command,
    discovery: Discovery,
    json: bool,
) -> Result<Option<Value>> {
    let value = match command {
        Command::Console { session, command } => {
            return console_stream(&discovery, &session, &command, json).await;
        }
        Command::Files { session } => {
            rpc(&discovery, "workspace.tree", json!({"session_id": session})).await?
        }
        Command::File { session, path } => {
            rpc(
                &discovery,
                "workspace.file",
                json!({"session_id": session, "path": path}),
            )
            .await?
        }
        Command::Grep {
            session,
            query,
            path,
            case_sensitive,
            limit,
        } => {
            rpc(
                &discovery,
                "workspace.grep",
                json!({
                    "session_id": session,
                    "query": query,
                    "path": path,
                    "case_sensitive": case_sensitive,
                    "max_results": limit,
                }),
            )
            .await?
        }
        Command::WriteFile {
            session,
            path,
            content,
        } => {
            let content = if let Some(content) = content {
                content
            } else {
                let mut content = String::new();
                std::io::stdin()
                    .read_to_string(&mut content)
                    .context("reading file content from stdin")?;
                content
            };
            rpc(
                &discovery,
                "workspace.write",
                json!({"session_id": session, "path": path, "content": content}),
            )
            .await?
        }
        Command::History { session, limit } => {
            rpc(
                &discovery,
                "git.log",
                json!({"session_id": session, "limit": limit}),
            )
            .await?
        }
        Command::Show { session, commit } => {
            rpc(
                &discovery,
                "git.show",
                json!({"session_id": session, "commit": commit}),
            )
            .await?
        }
        Command::Terminal { session, command } => {
            return terminal_stream(&discovery, &session, &command, json).await;
        }
        _ => unreachable!("command routed to the wrong dispatch family"),
    };
    Ok(Some(value))
}
