//! GUI, process, and preemption runtime commands.

use super::*;

pub(super) async fn dispatch(
    command: Command,
    discovery: Discovery,
    _json: bool,
) -> Result<Option<Value>> {
    let value = match command {
        Command::Computer {
            sweep,
            release,
            status,
            refresh,
            session,
        } => {
            if status {
                rpc(
                    &discovery,
                    "computer.status",
                    json!({"session_id": session, "refresh": refresh}),
                )
                .await?
            } else if let Some(lease_id) = release {
                rpc(
                    &discovery,
                    "computer.leases.release",
                    json!({"lease_id": lease_id}),
                )
                .await?
            } else if sweep {
                rpc(&discovery, "computer.leases.sweep", json!({})).await?
            } else {
                rpc(&discovery, "computer.leases", json!({})).await?
            }
        }
        Command::Gui {
            timeline,
            session,
            project,
            limit,
        } => {
            if timeline {
                rpc(
                    &discovery,
                    "gui.timeline",
                    json!({"session_id": session, "limit": limit}),
                )
                .await?
            } else {
                rpc(&discovery, "gui.status", json!({"project_id": project})).await?
            }
        }
        Command::GuiPolicy { project } => {
            rpc(&discovery, "gui.policy.get", json!({"project_id": project})).await?
        }
        Command::GuiPolicySet {
            project,
            grace_secs,
            wait_ms,
            status_ttl_secs,
            sensitive_mode,
            sensitive_bundle,
            sensitive_title,
            timeline_retention_days,
            replace_sensitive,
        } => {
            let current = rpc(&discovery, "gui.policy.get", json!({"project_id": project})).await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            if let Some(grace_secs) = grace_secs {
                policy["grace_secs"] = json!(grace_secs);
            }
            if let Some(wait_ms) = wait_ms {
                policy["wait_ms"] = json!(wait_ms);
            }
            if let Some(status_ttl_secs) = status_ttl_secs {
                policy["status_ttl_secs"] = json!(status_ttl_secs);
            }
            if let Some(sensitive_mode) = sensitive_mode {
                policy["sensitive_mode"] = json!(sensitive_mode);
            }
            if let Some(days) = timeline_retention_days {
                policy["timeline_retention_days"] = json!(days);
            }
            if replace_sensitive {
                policy["sensitive_bundles"] = json!([]);
                policy["sensitive_titles"] = json!([]);
            }
            if !sensitive_bundle.is_empty() {
                let mut bundles = policy
                    .get("sensitive_bundles")
                    .and_then(|value| value.as_array())
                    .cloned()
                    .unwrap_or_default();
                bundles.extend(sensitive_bundle.into_iter().map(Value::String));
                policy["sensitive_bundles"] = json!(bundles);
            }
            if !sensitive_title.is_empty() {
                let mut titles = policy
                    .get("sensitive_titles")
                    .and_then(|value| value.as_array())
                    .cloned()
                    .unwrap_or_default();
                titles.extend(sensitive_title.into_iter().map(Value::String));
                policy["sensitive_titles"] = json!(titles);
            }
            rpc(
                &discovery,
                "gui.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::GuiTimelineAudit { project, limit } => {
            rpc(
                &discovery,
                "gui.timeline.audit",
                json!({"project_id": project, "limit": limit}),
            )
            .await?
        }
        Command::GuiTimelineStatus {
            project,
            older_than_days,
        } => {
            rpc(
                &discovery,
                "gui.timeline.retention",
                json!({
                    "project_id": project,
                    "older_than_days": older_than_days,
                }),
            )
            .await?
        }
        Command::GuiTimelinePrune {
            project,
            older_than_days,
            limit,
            dry_run,
            yes,
        } => {
            let confirmed = dry_run
                || yes
                || confirm_cli_action("Prune old gui.action events for this project?")?;
            if !confirmed {
                bail!("GUI timeline prune requires explicit confirmation");
            }
            rpc(
                &discovery,
                "gui.timeline.prune",
                json!({
                    "project_id": project,
                    "older_than_days": older_than_days,
                    "limit": limit,
                    "dry_run": dry_run,
                    "confirm": confirmed,
                }),
            )
            .await?
        }
        Command::ShellPolicy { project } => {
            rpc(
                &discovery,
                "project.shell_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ShellPolicySet {
            project,
            network,
            read_deny,
            restrict_home,
            read_jail,
        } => {
            rpc(
                &discovery,
                "project.shell_policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "network": network,
                        "read_deny": read_deny,
                        "restrict_home": restrict_home,
                        "read_jail": read_jail,
                    }
                }),
            )
            .await?
        }
        Command::ProcessPolicy { project } => {
            rpc(
                &discovery,
                "process.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ProcessPolicySet {
            project,
            idle_sweep_enabled,
            max_processes,
            idle_ttl_secs,
            max_output_bytes,
            max_output_lines,
            restart_on_boot,
            restart_max_attempts,
            restart_backoff_secs,
        } => {
            let current = rpc(
                &discovery,
                "process.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current["policy"].clone();
            if let Some(enabled) = idle_sweep_enabled {
                policy["idle_sweep_enabled"] = json!(enabled);
            }
            if let Some(value) = max_processes {
                policy["max_processes"] = json!(value);
            }
            if let Some(value) = idle_ttl_secs {
                policy["idle_ttl_secs"] = json!(value);
            }
            if let Some(value) = max_output_bytes {
                policy["max_output_bytes"] = json!(value);
            }
            if let Some(value) = max_output_lines {
                policy["max_output_lines"] = json!(value);
            }
            if let Some(value) = restart_on_boot {
                policy["restart_on_boot"] = json!(value);
            }
            if let Some(value) = restart_max_attempts {
                policy["restart_max_attempts"] = json!(value);
            }
            if let Some(value) = restart_backoff_secs {
                policy["restart_backoff_secs"] = json!(value);
            }
            rpc(
                &discovery,
                "process.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::Processes {
            session,
            include_finished,
            limit,
        } => {
            rpc(
                &discovery,
                "session.processes",
                json!({
                    "session_id": session,
                    "include_finished": include_finished,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::ProcessLogs {
            session,
            process,
            offset,
            max_bytes,
            max_lines,
            from_end,
            follow,
            timeout_secs,
        } => {
            if follow {
                let deadline =
                    std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs.max(1));
                let mut cursor = offset;
                loop {
                    let result = rpc(
                        &discovery,
                        "session.process.logs",
                        json!({
                            "session_id": session,
                            "process_id": process,
                            "offset": cursor,
                            "max_bytes": max_bytes,
                            "max_lines": max_lines,
                            "from_end": false,
                        }),
                    )
                    .await?;
                    if let Some(content) = result["content"].as_str()
                        && !content.is_empty()
                    {
                        print!("{content}");
                        let _ = std::io::stdout().flush();
                    }
                    cursor = result["next_offset"].as_u64().unwrap_or(cursor);
                    let state = result["state"].as_str().unwrap_or("running");
                    let eof = result["eof"].as_bool().unwrap_or(false);
                    if eof && matches!(state, "exited" | "stopped" | "lost") {
                        break;
                    }
                    if std::time::Instant::now() >= deadline {
                        eprintln!("process log follow timed out after {timeout_secs}s");
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(400));
                }
                json!({"process_streamed": true})
            } else {
                rpc(
                    &discovery,
                    "session.process.logs",
                    json!({
                        "session_id": session,
                        "process_id": process,
                        "offset": offset,
                        "max_bytes": max_bytes,
                        "max_lines": max_lines,
                        "from_end": from_end,
                    }),
                )
                .await?
            }
        }
        Command::ProcessRestart {
            session,
            process,
            yes,
        } => {
            let confirmed = yes || confirm_cli_action("Retry this supervised process now?")?;
            if !confirmed {
                bail!("process restart requires explicit confirmation");
            }
            rpc(
                &discovery,
                "session.process.restart",
                json!({
                    "session_id": session,
                    "process_id": process,
                    "confirm": true,
                }),
            )
            .await?
        }
        Command::ProcessStop {
            session,
            process,
            signal,
            grace_secs,
        } => {
            rpc(
                &discovery,
                "session.process.stop",
                json!({
                    "session_id": session,
                    "process_id": process,
                    "signal": signal,
                    "grace_secs": grace_secs,
                }),
            )
            .await?
        }
        Command::Preempt { session, reason } => {
            rpc(
                &discovery,
                "session.preempt",
                json!({"session_id": session, "reason": reason}),
            )
            .await?
        }
        Command::Preemption { session, limit } => {
            rpc(
                &discovery,
                "session.preemption",
                json!({"session_id": session, "limit": limit}),
            )
            .await?
        }
        Command::PreemptionPolicy { project } => {
            rpc(
                &discovery,
                "project.preemption_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::Preemptions { project, limit } => {
            rpc(
                &discovery,
                "preemption.overview",
                json!({"project_id": project, "limit": limit}),
            )
            .await?
        }
        Command::PreemptionPolicySet {
            project,
            disabled,
            provider_grace_ms,
            tool_grace_ms,
            kill_escalation_ms,
            tool_deadline_secs,
            clear_tool_deadline,
            no_checkpoint,
        } => {
            let current = rpc(
                &discovery,
                "project.preemption_policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            let object = policy
                .as_object_mut()
                .ok_or_else(|| anyhow::anyhow!("preemption policy result is not an object"))?;
            object.insert("enabled".into(), json!(!disabled));
            if let Some(value) = provider_grace_ms {
                object.insert("provider_grace_ms".into(), json!(value));
            }
            if let Some(value) = tool_grace_ms {
                object.insert("tool_grace_ms".into(), json!(value));
            }
            if let Some(value) = kill_escalation_ms {
                object.insert("kill_escalation_ms".into(), json!(value));
            }
            if clear_tool_deadline {
                object.insert("tool_deadline_secs".into(), Value::Null);
            } else if let Some(value) = tool_deadline_secs {
                object.insert("tool_deadline_secs".into(), json!(value));
            }
            if no_checkpoint {
                object.insert("checkpoint_on_preempt".into(), json!(false));
            }
            rpc(
                &discovery,
                "project.preemption_policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        _ => unreachable!("command routed to the wrong dispatch family"),
    };
    Ok(Some(value))
}
