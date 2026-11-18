//! Preview console, terminal, and workspace file commands.

use super::*;

pub(super) async fn dispatch(
    command: Command,
    discovery: Discovery,
    _json: bool,
) -> Result<Option<Value>> {
    let value = match command {
        Command::Console { session, command } => {
            let opened = rpc(&discovery, "console.open", json!({"session_id": session})).await?;
            let console_id = opened["console_id"]
                .as_str()
                .context("console.open returned no console_id")?
                .to_string();
            println!(
                "console ready in {}",
                opened["worktree"].as_str().unwrap_or("?")
            );
            rpc(
                &discovery,
                "console.write",
                json!({
                    "console_id": console_id,
                    "data": format!("{command}\nexit\n")
                }),
            )
            .await?;
            let mut after_seq = 0_u64;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
            loop {
                let result = rpc(
                    &discovery,
                    "console.read",
                    json!({"console_id": console_id, "after_seq": after_seq}),
                )
                .await?;
                if let Some(chunks) = result["chunks"].as_array() {
                    for chunk in chunks {
                        if let Some(text) = chunk["text"].as_str() {
                            print!("{text}");
                        }
                    }
                    let _ = std::io::stdout().flush();
                }
                after_seq = result["next_seq"].as_u64().unwrap_or(after_seq);
                let status = result["status"].as_str().unwrap_or("running");
                if status != "running" {
                    if let Some(exit_code) = result["exit_code"].as_i64() {
                        println!("[console {status}, exit {exit_code}]");
                    }
                    break;
                }
                if std::time::Instant::now() > deadline {
                    let _ = rpc(
                        &discovery,
                        "console.signal",
                        json!({"console_id": console_id, "signal": "kill"}),
                    )
                    .await;
                    eprintln!("console command timed out after 300s");
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            let _ = rpc(
                &discovery,
                "console.close",
                json!({"console_id": console_id}),
            )
            .await;
            json!({"console_streamed": true})
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
            let opened = rpc(
                &discovery,
                "terminal.open",
                json!({"session_id": session, "cols": 100, "rows": 30}),
            )
            .await?;
            let terminal_id = opened["terminal_id"]
                .as_str()
                .context("terminal.open returned no terminal_id")?
                .to_string();
            println!(
                "terminal ready in {}",
                opened["worktree"].as_str().unwrap_or("?")
            );
            let data = BASE64.encode(format!("{command}\nexit\n"));
            rpc(
                &discovery,
                "terminal.write",
                json!({"terminal_id": terminal_id, "data": data}),
            )
            .await?;
            let mut after_seq = 0_u64;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
            loop {
                let result = rpc(
                    &discovery,
                    "terminal.read",
                    json!({"terminal_id": terminal_id, "after_seq": after_seq}),
                )
                .await?;
                if let Some(chunks) = result["chunks"].as_array() {
                    for chunk in chunks {
                        if let Some(data) = chunk["data"].as_str()
                            && let Ok(bytes) = BASE64.decode(data)
                        {
                            print!("{}", String::from_utf8_lossy(&bytes));
                        }
                    }
                    let _ = std::io::stdout().flush();
                }
                after_seq = result["next_seq"].as_u64().unwrap_or(after_seq);
                let status = result["status"].as_str().unwrap_or("running");
                if status != "running" {
                    if let Some(exit_code) = result["exit_code"].as_i64() {
                        println!("[terminal {status}, exit {exit_code}]");
                    }
                    break;
                }
                if std::time::Instant::now() > deadline {
                    let _ = rpc(
                        &discovery,
                        "terminal.close",
                        json!({"terminal_id": terminal_id}),
                    )
                    .await;
                    eprintln!("terminal command timed out after 300s");
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            let _ = rpc(
                &discovery,
                "terminal.close",
                json!({"terminal_id": terminal_id}),
            )
            .await;
            json!({"terminal_streamed": true})
        }
        _ => unreachable!("command routed to the wrong dispatch family"),
    };
    Ok(Some(value))
}
