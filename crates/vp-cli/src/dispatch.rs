//! Command dispatch for the `vp` binary.
//!
//! The match arms are kept verbatim from the historical single-file entrypoint.
//! `None` means the arm already printed its own output and the caller must not
//! print the returned value again.

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};
use std::path::Path;
use vp_protocol::discovery::Discovery;
use vp_protocol::remote::{
    RemoteIntegrationPolicy, RemoteTestKind, RemoteTestPolicy, RemoteTestProfile,
};

use crate::cli::*;
use crate::render::*;
use crate::rpc::*;
use crate::*;

/// Run one parsed command against the daemon and return the JSON value to
/// render. Arms that emit streaming/verbatim output return `None`.
pub(crate) async fn run_command(
    command: Command,
    discovery: Discovery,
    json: bool,
) -> Result<Option<Value>> {
    let value = match command {
        Command::DbRebuild { .. } => unreachable!("db rebuild is handled before discovery"),
        Command::Backup { .. } => unreachable!("backup is handled before discovery"),
        Command::Restore { .. } => unreachable!("restore is handled before discovery"),
        Command::BackupList { .. } => unreachable!("backup-list is handled before discovery"),
        Command::BackupPrune { .. } => unreachable!("backup-prune is handled before discovery"),
        Command::BackupVerify { .. } => unreachable!("backup-verify is handled before discovery"),
        Command::BackupKeyGenerate { .. } => {
            unreachable!("backup-key-generate is handled before discovery")
        }
        Command::BackupKeyList { .. } => {
            unreachable!("backup-key-list is handled before discovery")
        }
        Command::BackupKeyEvents { .. } => {
            unreachable!("backup-key-events is handled before discovery")
        }
        Command::BackupKeyRotate { .. } => {
            unreachable!("backup-key-rotate is handled before discovery")
        }
        Command::BackupKeyRevoke { .. } => {
            unreachable!("backup-key-revoke is handled before discovery")
        }
        Command::BackupRekey { .. } => {
            unreachable!("backup-rekey is handled before discovery")
        }
        Command::BackupKeyExportPublic { .. } => {
            unreachable!("backup-key-export-public is handled before discovery")
        }
        Command::BackupKeyImport { .. } => {
            unreachable!("backup-key-import is handled before discovery")
        }
        Command::BackupKeyring { .. } => unreachable!("backup-keyring is handled before discovery"),
        Command::BackupAnchor { .. } => unreachable!("backup-anchor is handled before discovery"),
        Command::BackupAnchorExport { .. } => {
            unreachable!("backup-anchor-export is handled before discovery")
        }
        Command::BackupAnchorVerify { .. } => {
            unreachable!("backup-anchor-verify is handled before discovery")
        }
        Command::BackupAnchorImport { .. } => {
            unreachable!("backup-anchor-import is handled before discovery")
        }
        Command::Bench { .. } => unreachable!("bench is handled before discovery"),
        Command::CacheBench { .. } => {
            unreachable!("cache-bench is handled before discovery")
        }
        Command::DaemonAgent { .. } => {
            unreachable!("daemon-agent is handled before discovery")
        }
        Command::ToolCodes { .. } => unreachable!("tool-codes is handled before discovery"),
        Command::Ping => {
            let result = rpc(&discovery, "ping", json!({})).await?;
            if json {
                print_json(&result)?;
            } else {
                println!(
                    "pong · v{} · protocol {} · up {}s",
                    result["version"].as_str().unwrap_or("?"),
                    result["protocol"].as_u64().unwrap_or(0),
                    result["uptime_secs"].as_u64().unwrap_or(0)
                );
            }
            return Ok(None);
        }
        Command::OtelStatus { project, session } => {
            rpc(
                &discovery,
                "telemetry.otel.status",
                json!({"project_id": project, "session_id": session}),
            )
            .await?
        }
        Command::OtelPolicy { project } => {
            rpc(
                &discovery,
                "project.otel_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::OtelPolicySet {
            project,
            enable,
            disable,
            endpoint,
            service_name,
            batch_limit,
            timeout_secs,
        } => {
            if enable && disable {
                bail!("--enable and --disable are mutually exclusive");
            }
            let current = rpc(
                &discovery,
                "project.otel_policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            if let Some(object) = policy.as_object_mut() {
                if enable || disable {
                    object.insert("enabled".into(), json!(enable));
                }
                if let Some(endpoint) = endpoint {
                    object.insert("endpoint".into(), json!(endpoint));
                }
                if let Some(service_name) = service_name {
                    object.insert("service_name".into(), json!(service_name));
                }
                if let Some(batch_limit) = batch_limit {
                    object.insert("batch_limit".into(), json!(batch_limit));
                }
                if let Some(timeout_secs) = timeout_secs {
                    object.insert("timeout_secs".into(), json!(timeout_secs));
                }
            }
            rpc(
                &discovery,
                "project.otel_policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::OtelExport {
            session,
            limit,
            offset,
        } => {
            rpc(
                &discovery,
                "telemetry.otel.export",
                json!({"session_id": session, "limit": limit, "offset": offset}),
            )
            .await?
        }
        Command::OtelTest { project } => {
            rpc(
                &discovery,
                "telemetry.otel.test",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::Metrics { prometheus } => {
            let result = rpc(&discovery, "daemon.metrics", json!({})).await?;
            if prometheus {
                let text = result
                    .get("prometheus")
                    .and_then(Value::as_str)
                    .context("daemon.metrics returned no Prometheus text")?;
                print!("{text}");
                if !text.ends_with('\n') {
                    println!();
                }
            } else {
                print_json(result.get("metrics").unwrap_or(&result))?;
            }
            return Ok(None);
        }
        Command::Info => rpc(&discovery, "server.info", json!({})).await?,
        Command::AcpAgents => rpc(&discovery, "acp.agent.list", json!({})).await?,
        Command::AcpAdd {
            name,
            command,
            transport,
            endpoint,
            auth_header_env,
            headers,
            allow_remote,
            connect_timeout,
            args,
            env,
            auto_approve,
            timeout,
        } => {
            let env = parse_env_pairs(&env)?;
            let headers = parse_pairs(&headers, "--header")?;
            rpc(
                &discovery,
                "acp.agent.add",
                json!({
                    "name": name,
                    "command": command.unwrap_or_default(),
                    "args": args,
                    "env": env,
                    "enabled": true,
                    "auto_approve": auto_approve,
                    "timeout_secs": timeout,
                    "transport": transport.as_str(),
                    "endpoint": endpoint,
                    "auth_header_env": auth_header_env,
                    "headers": headers,
                    "connect_timeout_secs": connect_timeout,
                    "allow_remote": allow_remote
                }),
            )
            .await?
        }
        Command::AcpUpdate {
            agent,
            name,
            command,
            transport,
            endpoint,
            clear_endpoint,
            auth_header_env,
            clear_auth_header_env,
            headers,
            clear_headers,
            allow_remote,
            no_allow_remote,
            connect_timeout,
            args,
            env,
            auto_approve,
            no_auto_approve,
            enabled,
            disabled,
            timeout,
        } => {
            if clear_endpoint && endpoint.is_some() {
                bail!("use either --endpoint or --clear-endpoint, not both");
            }
            if clear_auth_header_env && auth_header_env.is_some() {
                bail!("use either --auth-header-env or --clear-auth-header-env, not both");
            }
            if clear_headers && !headers.is_empty() {
                bail!("use either --header or --clear-headers, not both");
            }
            if allow_remote && no_allow_remote {
                bail!("use either --allow-remote or --no-allow-remote, not both");
            }
            if auto_approve && no_auto_approve {
                bail!("use either --auto-approve or --no-auto-approve, not both");
            }
            if enabled && disabled {
                bail!("use either --enabled or --disabled, not both");
            }
            let current = rpc(&discovery, "acp.agent.list", json!({})).await?;
            let agent_json = current
                .get("agents")
                .and_then(Value::as_array)
                .and_then(|agents| {
                    agents.iter().find(|candidate| {
                        candidate.get("id").and_then(Value::as_str) == Some(agent.as_str())
                            || candidate.get("name").and_then(Value::as_str) == Some(agent.as_str())
                    })
                })
                .cloned()
                .with_context(|| format!("ACP agent not found: {agent}"))?;
            let existing_transport = agent_json
                .get("transport")
                .and_then(Value::as_str)
                .unwrap_or("stdio");
            let transport = transport
                .map(AcpTransportArg::as_str)
                .unwrap_or(existing_transport)
                .to_string();
            let existing_args = agent_json.get("args").cloned().unwrap_or_else(|| json!([]));
            let existing_env = agent_json.get("env").cloned().unwrap_or_else(|| json!({}));
            let existing_headers = agent_json
                .get("headers")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let env = if env.is_empty() {
                existing_env
            } else {
                json!(parse_env_pairs(&env)?)
            };
            let headers = if !headers.is_empty() {
                json!(parse_pairs(&headers, "--header")?)
            } else if clear_headers {
                json!({})
            } else {
                existing_headers
            };
            let endpoint = if clear_endpoint {
                Value::Null
            } else if let Some(endpoint) = endpoint {
                json!(endpoint)
            } else {
                agent_json.get("endpoint").cloned().unwrap_or(Value::Null)
            };
            let auth_header_env = if clear_auth_header_env {
                Value::Null
            } else if let Some(name) = auth_header_env {
                json!(name)
            } else {
                agent_json
                    .get("auth_header_env")
                    .cloned()
                    .unwrap_or(Value::Null)
            };
            let allow_remote = if allow_remote {
                true
            } else if no_allow_remote {
                false
            } else {
                agent_json
                    .get("allow_remote")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            };
            let auto_approve = if auto_approve {
                true
            } else if no_auto_approve {
                false
            } else {
                agent_json
                    .get("auto_approve")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            };
            let enabled = if enabled {
                true
            } else if disabled {
                false
            } else {
                agent_json
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
            };
            rpc(
                &discovery,
                "acp.agent.update",
                json!({
                    "id": agent_json.get("id").cloned().unwrap_or(Value::Null),
                    "name": name.unwrap_or_else(|| {
                        agent_json
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    }),
                    "command": command.unwrap_or_else(|| {
                        agent_json
                            .get("command")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    }),
                    "args": if args.is_empty() { existing_args } else { json!(args) },
                    "env": env,
                    "enabled": enabled,
                    "auto_approve": auto_approve,
                    "timeout_secs": timeout.unwrap_or_else(|| {
                        agent_json
                            .get("timeout_secs")
                            .and_then(Value::as_u64)
                            .unwrap_or(1800)
                    }),
                    "transport": transport,
                    "endpoint": endpoint,
                    "auth_header_env": auth_header_env,
                    "headers": headers,
                    "connect_timeout_secs": connect_timeout.unwrap_or_else(|| {
                        agent_json
                            .get("connect_timeout_secs")
                            .and_then(Value::as_u64)
                            .unwrap_or(30)
                    }),
                    "allow_remote": allow_remote
                }),
            )
            .await?
        }
        Command::AcpRemove { agent_id } => {
            rpc(
                &discovery,
                "acp.agent.remove",
                json!({"agent_id": agent_id}),
            )
            .await?
        }
        Command::AcpProbe {
            agent,
            project,
            no_session,
        } => {
            rpc(
                &discovery,
                "acp.agent.probe",
                json!({
                    "agent_id": agent,
                    "project_id": project,
                    "create_session": !no_session
                }),
            )
            .await?
        }
        Command::McpServers => rpc(&discovery, "mcp.server.list", json!({})).await?,
        Command::McpAdd {
            name,
            command,
            transport,
            args,
            env,
            url,
            headers,
        } => {
            let env = parse_env_pairs(&env)?;
            let headers = parse_env_pairs(&headers)?;
            rpc(
                &discovery,
                "mcp.server.add",
                json!({
                    "name": name,
                    "transport": transport.as_str(),
                    "command": command.unwrap_or_default(),
                    "args": args,
                    "env": env,
                    "url": url,
                    "headers": headers,
                }),
            )
            .await?
        }
        Command::McpUpdate {
            server_id,
            name,
            transport,
            command,
            args,
            env,
            url,
            headers,
            enabled,
        } => {
            let env = parse_env_pairs(&env)?;
            let headers = parse_env_pairs(&headers)?;
            let enabled = match enabled {
                Some(enabled) => enabled,
                None => mcp_server_enabled(&discovery, &server_id).await?,
            };
            rpc(
                &discovery,
                "mcp.server.update",
                json!({
                    "id": server_id,
                    "name": name,
                    "transport": transport.as_str(),
                    "command": command.unwrap_or_default(),
                    "args": args,
                    "env": env,
                    "url": url,
                    "headers": headers,
                    "enabled": enabled,
                }),
            )
            .await?
        }
        Command::McpProbe { server_id } => {
            rpc(
                &discovery,
                "mcp.server.probe",
                json!({"server_id": server_id}),
            )
            .await?
        }
        Command::McpRemove { server_id } => {
            rpc(
                &discovery,
                "mcp.server.remove",
                json!({"server_id": server_id}),
            )
            .await?
        }
        Command::McpTools { server } => {
            rpc(&discovery, "mcp.tools", json!({"server_id": server})).await?
        }
        Command::McpCall {
            server,
            tool,
            arguments,
            session,
            agent_id,
            agent_name,
        } => {
            let arguments: serde_json::Value =
                serde_json::from_str(&arguments).context("parsing --arguments as JSON")?;
            rpc(
                &discovery,
                "mcp.call",
                json!({
                    "server_id": server,
                    "tool": tool,
                    "arguments": arguments,
                    "session_id": session,
                    "agent_id": agent_id,
                    "agent_name": agent_name,
                }),
            )
            .await?
        }
        Command::McpTasks {
            project,
            session,
            server,
            status,
            limit,
        } => {
            rpc(
                &discovery,
                "mcp.task.list",
                json!({
                    "project_id": project,
                    "session_id": session,
                    "server_id": server,
                    "status": status,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::McpTask { task_id } => {
            rpc(&discovery, "mcp.task.get", json!({"task_id": task_id})).await?
        }
        Command::McpTaskResult {
            task_id,
            no_refresh,
        } => {
            rpc(
                &discovery,
                "mcp.task.result",
                json!({"task_id": task_id, "refresh": !no_refresh}),
            )
            .await?
        }
        Command::McpTaskCancel { task_id, reason } => {
            rpc(
                &discovery,
                "mcp.task.cancel",
                json!({"task_id": task_id, "reason": reason}),
            )
            .await?
        }
        Command::McpResources { server } => {
            rpc(&discovery, "mcp.resources", json!({"server_id": server})).await?
        }
        Command::McpReadResource { server, uri } => {
            rpc(
                &discovery,
                "mcp.resource.read",
                json!({"server_id": server, "uri": uri}),
            )
            .await?
        }
        Command::McpPrompts { server } => {
            rpc(&discovery, "mcp.prompts", json!({"server_id": server})).await?
        }
        Command::McpPrompt { server, name, args } => {
            let arguments = parse_env_pairs(&args)?;
            rpc(
                &discovery,
                "mcp.prompt.get",
                json!({"server_id": server, "name": name, "arguments": arguments}),
            )
            .await?
        }
        Command::Doctor => rpc(&discovery, "server.doctor", json!({})).await?,
        Command::ProviderTest => rpc(&discovery, "agent.probe", json!({})).await?,
        Command::Projects => rpc(&discovery, "project.list", json!({})).await?,
        Command::ProjectAdd { path, name } => {
            let root = path
                .canonicalize()
                .with_context(|| format!("resolving {}", path.display()))?;
            rpc(
                &discovery,
                "project.add",
                json!({"root": root, "name": name}),
            )
            .await?
        }
        Command::HostList => rpc(&discovery, "host.list", json!({})).await?,
        Command::HostSet { project, config } => {
            let text = if config == "-" {
                let mut text = String::new();
                std::io::stdin().read_to_string(&mut text)?;
                text
            } else if Path::new(&config).is_file() {
                std::fs::read_to_string(&config)
                    .with_context(|| format!("reading execution host config {config}"))?
            } else {
                config.clone()
            };
            let config: Value = serde_json::from_str(&text)
                .context("execution host config must be a JSON object")?;
            rpc(
                &discovery,
                "host.update",
                json!({"project_id": project, "config": config}),
            )
            .await?
        }
        Command::HostRemove { project } => {
            rpc(&discovery, "host.remove", json!({"project_id": project})).await?
        }
        Command::HostProbe { project, timeout } => {
            rpc(
                &discovery,
                "host.probe",
                json!({"project_id": project, "timeout_secs": timeout}),
            )
            .await?
        }
        Command::RemoteStatus { project } => {
            rpc(&discovery, "remote.status", json!({"project_id": project})).await?
        }
        Command::Lsp { project } => {
            rpc(&discovery, "lsp.status", json!({"project_id": project})).await?
        }
        Command::RemoteWorktrees { project } => {
            rpc(
                &discovery,
                "remote.worktree_list",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::RemoteLedger { project, limit } => {
            rpc(
                &discovery,
                "remote.ledger",
                json!({"project_id": project, "limit": limit}),
            )
            .await?
        }
        Command::RemoteIntegrate {
            project,
            session,
            branch,
            host,
            expected_commit,
            base_commit,
            task,
            objective,
            confirm,
            keep_import_refs,
        } => {
            rpc(
                &discovery,
                "remote.integrate",
                json!({
                    "project_id": project,
                    "session_id": session,
                    "branch": branch,
                    "host": host,
                    "expected_commit": expected_commit,
                    "base_commit": base_commit,
                    "task_id": task,
                    "objective": objective,
                    "confirm": confirm,
                    "keep_import_refs": if keep_import_refs { Some(true) } else { None },
                }),
            )
            .await?
        }
        Command::RemoteBootstrap {
            project,
            install_path,
            expected_sha256,
            expected_version,
            force,
            confirm,
            timeout,
            session,
        } => {
            rpc(
                &discovery,
                "remote.bootstrap",
                json!({
                    "project_id": project,
                    "install_path": install_path,
                    "expected_sha256": expected_sha256,
                    "expected_version": expected_version,
                    "force": force,
                    "confirm": confirm,
                    "timeout_secs": timeout,
                    "session_id": session,
                }),
            )
            .await?
        }
        Command::RemoteBootstrapStatus { project } => {
            rpc(
                &discovery,
                "remote.bootstrap.status",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::RemoteTest {
            project,
            session,
            entry,
            profile,
            command,
            cwd,
            timeout,
            output_cap,
            expected_commit,
        } => {
            rpc(
                &discovery,
                "remote.test",
                json!({
                    "project_id": project,
                    "session_id": session.unwrap_or_default(),
                    "entry_id": entry,
                    "profile": profile,
                    "command": command,
                    "cwd": cwd,
                    "timeout_secs": timeout,
                    "output_cap_bytes": output_cap,
                    "expected_commit": expected_commit,
                }),
            )
            .await?
        }
        Command::RemoteTestStatus {
            project,
            session,
            entry,
            limit,
        } => {
            rpc(
                &discovery,
                "remote.test.status",
                json!({
                    "project_id": project,
                    "session_id": session,
                    "entry_id": entry,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::RemoteTestOutput {
            project,
            evidence,
            offset,
            limit,
        } => {
            rpc(
                &discovery,
                "remote.test.output",
                json!({
                    "project_id": project,
                    "evidence_id": evidence,
                    "offset": offset,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::RemoteTestPolicy { project } => {
            rpc(
                &discovery,
                "remote.test.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::RemoteTestPolicySet {
            project,
            enable,
            disable,
            default_profile,
            timeout,
            output_cap,
            profiles,
            remove_profiles,
            clear_profiles,
        } => {
            if enable && disable {
                bail!("use either --enable or --disable, not both");
            }
            let current = rpc(
                &discovery,
                "remote.test.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy: RemoteTestPolicy = serde_json::from_value(
                current
                    .get("policy")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("policy response is missing `policy`"))?,
            )
            .context("decoding the current remote test policy")?;
            if enable {
                policy.enabled = true;
            }
            if disable {
                policy.enabled = false;
            }
            if let Some(default) = default_profile {
                policy.default_profile = Some(default);
            }
            if let Some(timeout) = timeout {
                policy.timeout_secs = timeout;
            }
            if let Some(output_cap) = output_cap {
                policy.output_cap_bytes = output_cap;
            }
            if clear_profiles {
                policy.profiles.clear();
            }
            for name in remove_profiles {
                policy.profiles.retain(|profile| profile.name != name);
            }
            for raw in profiles {
                let mut parts = raw.splitn(3, ':');
                let name = parts.next().unwrap_or_default().trim();
                let kind = parts.next().unwrap_or("other").trim();
                let command = parts.next().unwrap_or_default();
                if name.is_empty() || command.trim().is_empty() {
                    bail!("profile {raw:?} must be name:kind:command");
                }
                let kind = match kind {
                    "check" => RemoteTestKind::Check,
                    "lint" => RemoteTestKind::Lint,
                    "test" => RemoteTestKind::Test,
                    "other" => RemoteTestKind::Other,
                    other => bail!("unknown remote test kind {other:?}; use check|lint|test|other"),
                };
                let profile = RemoteTestProfile {
                    name: name.to_string(),
                    kind,
                    command: command.to_string(),
                    cwd: None,
                    timeout_secs: None,
                };
                profile
                    .validate()
                    .map_err(|message| anyhow::anyhow!("invalid remote test profile: {message}"))?;
                if let Some(existing) = policy
                    .profiles
                    .iter_mut()
                    .find(|existing| existing.name == profile.name)
                {
                    *existing = profile;
                } else {
                    policy.profiles.push(profile);
                }
            }
            policy
                .validate()
                .map_err(|message| anyhow::anyhow!("invalid remote test policy: {message}"))?;
            rpc(
                &discovery,
                "remote.test.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::RemoteIntegration { project, session } => {
            rpc(
                &discovery,
                "remote.integration.status",
                json!({"project_id": project, "session_id": session}),
            )
            .await?
        }
        Command::RemoteIntegrationCleanup {
            project,
            session,
            entry,
            force,
        } => {
            rpc(
                &discovery,
                "remote.integration.cleanup",
                json!({
                    "project_id": project,
                    "session_id": session,
                    "entry_id": entry,
                    "force": force,
                }),
            )
            .await?
        }
        Command::RemoteIntegrationPolicy { project } => {
            rpc(
                &discovery,
                "remote.integration.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::RemoteIntegrationPolicySet {
            project,
            enable,
            disable,
            max_bundle_bytes,
            require_verifier,
            no_require_verifier,
            allowed_hosts,
            clear_hosts,
            keep_import_refs,
            no_keep_import_refs,
            confirm,
        } => {
            if enable && disable {
                bail!("use either --enable or --disable, not both");
            }
            if require_verifier && no_require_verifier {
                bail!("use either --require-verifier or --no-require-verifier, not both");
            }
            if keep_import_refs && no_keep_import_refs {
                bail!("use either --keep-import-refs or --no-keep-import-refs, not both");
            }
            let current = rpc(
                &discovery,
                "remote.integration.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy: RemoteIntegrationPolicy = serde_json::from_value(
                current
                    .get("policy")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("policy response is missing `policy`"))?,
            )
            .context("decoding the current remote integration policy")?;
            let mut changed = false;
            if enable {
                policy.enabled = true;
                changed = true;
            }
            if disable {
                policy.enabled = false;
                changed = true;
            }
            if let Some(bytes) = max_bundle_bytes {
                policy.max_bundle_bytes = bytes;
                changed = true;
            }
            if require_verifier {
                policy.require_verifier = true;
                changed = true;
            }
            if no_require_verifier {
                policy.require_verifier = false;
                changed = true;
            }
            if clear_hosts {
                policy.allowed_hosts.clear();
                changed = true;
            }
            if !allowed_hosts.is_empty() {
                policy.allowed_hosts = allowed_hosts;
                changed = true;
            }
            if keep_import_refs {
                policy.keep_import_refs = true;
                changed = true;
            }
            if no_keep_import_refs {
                policy.keep_import_refs = false;
                changed = true;
            }
            if !changed {
                bail!(
                    "no policy change requested; pass --enable/--disable, --max-bundle-bytes, verifier flags, --allow-host, or keep-ref flags"
                );
            }
            policy
                .validate()
                .map_err(|error| anyhow::anyhow!("invalid remote integration policy: {error}"))?;
            rpc(
                &discovery,
                "remote.integration.policy.set",
                json!({
                    "project_id": project,
                    "policy": policy,
                    "confirm": confirm,
                }),
            )
            .await?
        }
        Command::ProjectBudget { project } => {
            rpc(
                &discovery,
                "project.budget.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::Skills { project, all } => {
            rpc(
                &discovery,
                "skill.list",
                json!({"project_id": project, "include_disabled": all}),
            )
            .await?
        }
        Command::SkillCreate {
            project,
            name,
            description,
            instructions,
            when_to_use,
            version,
            tool_allow,
            tool_deny,
            profiles,
            roles,
        } => {
            let instructions = if instructions == "-" {
                let mut buffer = String::new();
                std::io::stdin()
                    .read_to_string(&mut buffer)
                    .context("reading skill instructions from stdin")?;
                buffer
            } else {
                instructions
            };
            rpc(
                &discovery,
                "skill.create",
                json!({
                    "project_id": project,
                    "name": name,
                    "description": description,
                    "when_to_use": when_to_use,
                    "version": version,
                    "instructions": instructions,
                    "tool_allow": tool_allow,
                    "tool_deny": tool_deny,
                    "profiles": profiles,
                    "roles": roles,
                }),
            )
            .await?
        }
        Command::SkillActivations { session } => {
            rpc(
                &discovery,
                "skill.activations",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::SkillImport { project, path } => {
            rpc(
                &discovery,
                "skill.import",
                json!({"project_id": project, "path": path}),
            )
            .await?
        }
        Command::SkillEnabled { id, enabled } => {
            rpc(
                &discovery,
                "skill.set_enabled",
                json!({"id": id, "enabled": enabled.unwrap_or(true)}),
            )
            .await?
        }
        Command::SkillDelete { id } => rpc(&discovery, "skill.delete", json!({"id": id})).await?,
        Command::Sources {
            project,
            query,
            status,
            limit,
        } => {
            rpc(
                &discovery,
                "source.list",
                json!({
                    "project_id": project,
                    "query": query,
                    "status": status,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::SourceGet {
            source_id,
            offset,
            limit,
        } => {
            rpc(
                &discovery,
                "source.get",
                json!({"source_id": source_id, "offset": offset, "limit": limit}),
            )
            .await?
        }
        Command::SourceNote {
            project,
            title,
            content,
            collection,
        } => {
            let content = if content == "-" {
                let mut buffer = String::new();
                std::io::stdin()
                    .read_to_string(&mut buffer)
                    .context("reading source note from stdin")?;
                buffer
            } else {
                content
            };
            rpc(
                &discovery,
                "source.add_note",
                json!({
                    "project_id": project,
                    "collection": collection,
                    "title": title,
                    "content": content
                }),
            )
            .await?
        }
        Command::SourceAddFile {
            project,
            path,
            title,
            collection,
        } => {
            rpc(
                &discovery,
                "source.add_file",
                json!({
                    "project_id": project,
                    "collection": collection,
                    "path": path,
                    "title": title
                }),
            )
            .await?
        }
        Command::SourceAddUrl {
            project,
            url,
            title,
            collection,
        } => {
            rpc(
                &discovery,
                "source.add_url",
                json!({
                    "project_id": project,
                    "collection": collection,
                    "url": url,
                    "title": title
                }),
            )
            .await?
        }
        Command::SourceImport {
            project,
            path,
            max_files,
            collection,
        } => {
            rpc(
                &discovery,
                "source.import",
                json!({
                    "project_id": project,
                    "collection": collection,
                    "path": path,
                    "max_files": max_files
                }),
            )
            .await?
        }
        Command::SourceRefresh { source_id } => {
            rpc(
                &discovery,
                "source.refresh",
                json!({"source_id": source_id}),
            )
            .await?
        }
        Command::SourceReindex { source_id } => {
            rpc(
                &discovery,
                "source.reindex",
                json!({"source_id": source_id}),
            )
            .await?
        }
        Command::SourceDelete { source_id } => {
            rpc(&discovery, "source.delete", json!({"source_id": source_id})).await?
        }
        Command::SourceUpdate {
            source_id,
            title,
            collection,
            labels,
        } => {
            rpc(
                &discovery,
                "source.update",
                json!({
                    "source_id": source_id,
                    "title": title,
                    "collection": collection,
                    "labels": labels
                }),
            )
            .await?
        }
        Command::SourceCollections { project } => {
            rpc(
                &discovery,
                "source.collections",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::SourceMove {
            source_id,
            collection,
        } => {
            rpc(
                &discovery,
                "source.set_collection",
                json!({"source_id": source_id, "collection": collection}),
            )
            .await?
        }
        Command::SourceLabels { source_id, labels } => {
            rpc(
                &discovery,
                "source.set_labels",
                json!({"source_id": source_id, "labels": labels}),
            )
            .await?
        }
        Command::SourceSearch {
            project,
            query,
            collections,
            labels,
            limit,
            mode,
        } => {
            rpc(
                &discovery,
                "source.search",
                json!({
                    "project_id": project,
                    "query": query,
                    "collections": collections,
                    "labels": labels,
                    "limit": limit,
                    "mode": mode
                }),
            )
            .await?
        }
        Command::SourceEmbedding { project } => {
            rpc(
                &discovery,
                "source.embedding.status",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::SourceEmbeddingReindex { project } => {
            rpc(
                &discovery,
                "source.embedding.reindex",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::SourceEmbeddingPolicy { project } => {
            rpc(
                &discovery,
                "source.embedding.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::SourceEmbeddingPolicySet {
            project,
            enable,
            disable,
            endpoint_env,
            model,
            dimensions,
            batch_size,
            max_chunks_per_source,
            retention_days,
            timeout_secs,
            api_key_env,
            clear,
        } => {
            if enable && disable {
                bail!("--enable and --disable cannot be combined");
            }
            if clear
                && (enable
                    || disable
                    || endpoint_env.is_some()
                    || model.is_some()
                    || dimensions.is_some()
                    || batch_size.is_some()
                    || max_chunks_per_source.is_some()
                    || retention_days.is_some()
                    || timeout_secs.is_some()
                    || api_key_env.is_some())
            {
                bail!("--clear cannot be combined with other policy flags");
            }
            let current = if clear {
                json!({})
            } else {
                rpc(
                    &discovery,
                    "source.embedding.policy.get",
                    json!({"project_id": project}),
                )
                .await?
                .get("policy")
                .cloned()
                .unwrap_or_default()
            };
            let mut policy = if current.is_object() {
                current
            } else {
                json!({})
            };
            let defaults = [
                ("enabled", json!(false)),
                ("dimensions", json!(1536)),
                ("batch_size", json!(8)),
                ("max_chunks_per_source", json!(200)),
                ("retention_days", json!(30)),
                ("timeout_secs", json!(30)),
            ];
            for (key, value) in defaults {
                if policy.get(key).is_none() {
                    policy[key] = value;
                }
            }
            if enable {
                policy["enabled"] = json!(true);
            }
            if disable {
                policy["enabled"] = json!(false);
            }
            if let Some(value) = endpoint_env {
                policy["endpoint_env"] = json!(value);
            }
            if clear {
                policy["endpoint_env"] = Value::Null;
                policy["api_key_env"] = Value::Null;
            }
            if let Some(value) = model {
                policy["model"] = json!(value);
            }
            if clear {
                policy["model"] = Value::Null;
            }
            if let Some(value) = dimensions {
                policy["dimensions"] = json!(value);
            }
            if let Some(value) = batch_size {
                policy["batch_size"] = json!(value);
            }
            if let Some(value) = max_chunks_per_source {
                policy["max_chunks_per_source"] = json!(value);
            }
            if let Some(value) = retention_days {
                policy["retention_days"] = json!(value);
            }
            if let Some(value) = timeout_secs {
                policy["timeout_secs"] = json!(value);
            }
            if let Some(value) = api_key_env {
                policy["api_key_env"] = json!(value);
            }
            rpc(
                &discovery,
                "source.embedding.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::SourceRetrievalPolicy { project } => {
            rpc(
                &discovery,
                "source.retrieval.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::SourceRetrievalPolicySet {
            project,
            enable_ann,
            disable_ann,
            ann_min_vectors,
            ann_nprobe,
            rerank,
            query_expansion,
            max_expanded_terms,
            clear,
        } => {
            let current = if clear {
                json!({})
            } else {
                rpc(
                    &discovery,
                    "source.retrieval.policy.get",
                    json!({"project_id": project}),
                )
                .await?
                .get("policy")
                .cloned()
                .unwrap_or_default()
            };
            let policy = merge_source_retrieval_policy(
                current,
                enable_ann,
                disable_ann,
                ann_min_vectors,
                ann_nprobe,
                rerank,
                query_expansion,
                max_expanded_terms,
                clear,
            )?;
            rpc(
                &discovery,
                "source.retrieval.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::SourceIndex { project } => {
            rpc(
                &discovery,
                "source.embedding.index_status",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::SourceIndexRebuild { project } => {
            rpc(
                &discovery,
                "source.embedding.index_rebuild",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ProjectUsage { project } => {
            rpc(&discovery, "project.usage", json!({"project_id": project})).await?
        }
        Command::ProjectCache { project, limit } => {
            rpc(
                &discovery,
                "project.cache",
                json!({"project_id": project, "limit": limit}),
            )
            .await?
        }
        Command::ProjectBudgetSet {
            project,
            token_budget,
            cost_budget_usd,
        } => {
            rpc(
                &discovery,
                "project.budget.set",
                json!({
                    "project_id": project,
                    "budget": {
                        "token_budget": token_budget,
                        "cost_budget_usd": cost_budget_usd,
                    }
                }),
            )
            .await?
        }
        Command::Governor { session, limit } => {
            rpc(
                &discovery,
                "session.budget.governor",
                json!({"session_id": session, "limit": limit}),
            )
            .await?
        }
        Command::CriticalPath {
            session,
            remaining_tokens,
            mode,
        } => {
            let mode = parse_governor_mode(mode.as_deref())?;
            rpc(
                &discovery,
                "governor.preview",
                json!({
                    "session_id": session,
                    "remaining_tokens": remaining_tokens,
                    "limit": 1,
                    "mode": mode,
                }),
            )
            .await?
        }
        Command::GovernorPreview {
            session,
            remaining_tokens,
            limit,
            mode,
        } => {
            let mode = parse_governor_mode(mode.as_deref())?;
            rpc(
                &discovery,
                "governor.preview",
                json!({
                    "session_id": session,
                    "remaining_tokens": remaining_tokens,
                    "limit": limit,
                    "mode": mode,
                }),
            )
            .await?
        }
        Command::GovernorPolicy { project } => {
            rpc(
                &discovery,
                "critical_path.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::GovernorPolicySet {
            project,
            enabled,
            reserve_ratio,
            throttle_at_ratio,
            critical_boost,
            min_task_budget,
            mode,
            max_paths,
            department_min_ratio,
            observed_weight_ratio,
            starvation_after_secs,
        } => {
            let current = rpc(
                &discovery,
                "critical_path.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            if let Some(value) = enabled {
                policy["enabled"] = json!(value);
            }
            if let Some(value) = reserve_ratio {
                policy["reserve_ratio"] = json!(value);
            }
            if let Some(value) = throttle_at_ratio {
                policy["throttle_at_ratio"] = json!(value);
            }
            if let Some(value) = critical_boost {
                policy["critical_boost"] = json!(value);
            }
            if let Some(value) = min_task_budget {
                policy["min_task_budget"] = json!(value);
            }
            if let Some(value) = mode {
                let parsed =
                    vp_protocol::types::CriticalPathMode::parse(&value).ok_or_else(|| {
                        anyhow::anyhow!("invalid --mode {value:?}; use `single` or `multi`")
                    })?;
                policy["mode"] = json!(parsed.as_str());
            }
            if let Some(value) = max_paths {
                policy["max_paths"] = json!(value);
            }
            if let Some(value) = department_min_ratio {
                policy["department_min_ratio"] = json!(value);
            }
            if let Some(value) = observed_weight_ratio {
                policy["observed_weight_ratio"] = json!(value);
            }
            if let Some(value) = starvation_after_secs {
                policy["starvation_after_secs"] = json!(value);
            }
            rpc(
                &discovery,
                "critical_path.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::CleanupWorktrees {
            project,
            older_than_days,
            dry_run,
            force,
            delete_branches,
            drop_failed,
        } => {
            rpc(
                &discovery,
                "project.cleanup_worktrees",
                json!({
                    "project_id": project,
                    "older_than_secs": older_than_days.saturating_mul(86_400),
                    "dry_run": dry_run,
                    "force": force,
                    "delete_branches": delete_branches,
                    "keep_failed": !drop_failed,
                }),
            )
            .await?
        }
        Command::TaskScheduler { project } => {
            rpc(
                &discovery,
                "project.task_scheduler.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::TaskSchedulerSet {
            project,
            enabled,
            cooldown_secs,
            dispatch_tasks,
            include_intake,
            max_intake_parallel,
        } => {
            // Merge with the stored policy so a one-field CLI edit does not
            // silently reset intake or per-task behavior to defaults.
            let current = rpc(
                &discovery,
                "project.task_scheduler.get",
                json!({"project_id": project}),
            )
            .await?;
            let policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            rpc(
                &discovery,
                "project.task_scheduler.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "enabled": enabled
                            .unwrap_or_else(|| policy["enabled"].as_bool().unwrap_or(true)),
                        "cooldown_secs": cooldown_secs
                            .unwrap_or_else(|| policy["cooldown_secs"].as_u64().unwrap_or(60)),
                        "dispatch_tasks": dispatch_tasks
                            .unwrap_or_else(|| policy["dispatch_tasks"].as_bool().unwrap_or(false)),
                        "include_intake": include_intake
                            .unwrap_or_else(|| policy["include_intake"].as_bool().unwrap_or(true)),
                        "max_intake_parallel": max_intake_parallel
                            .unwrap_or_else(|| {
                                policy["max_intake_parallel"].as_u64().unwrap_or(2) as u32
                            }),
                    }
                }),
            )
            .await?
        }
        Command::Scaling {
            project,
            session,
            limit,
        } => {
            rpc(
                &discovery,
                "scaling.status",
                json!({
                    "project_id": project,
                    "session_id": session,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::ScalingPreview { session } => {
            rpc(
                &discovery,
                "scaling.preview",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::ScalingPolicy { project } => {
            rpc(
                &discovery,
                "scaling.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ScalingPolicySet {
            project,
            enabled,
            min_workers,
            max_workers,
            min_labour,
            max_labour,
            expand_ready_threshold,
            expand_critical_delay_secs,
            scale_down_idle_secs,
            cooldown_secs,
            max_step,
            budget_headroom_ratio,
            no_budget_check,
        } => {
            // Merge with the stored policy so a one-field edit does not reset
            // the rest to defaults.
            let current = rpc(
                &discovery,
                "scaling.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            let budget_headroom_ratio = if no_budget_check {
                Value::Null
            } else if let Some(ratio) = budget_headroom_ratio {
                json!(ratio)
            } else {
                policy
                    .get("budget_headroom_ratio")
                    .cloned()
                    .unwrap_or(Value::Null)
            };
            rpc(
                &discovery,
                "scaling.policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "enabled": enabled
                            .unwrap_or_else(|| policy["enabled"].as_bool().unwrap_or(false)),
                        "min_workers": min_workers
                            .unwrap_or_else(|| policy["min_workers"].as_u64().unwrap_or(1) as u32),
                        "max_workers": max_workers
                            .unwrap_or_else(|| policy["max_workers"].as_u64().unwrap_or(4) as u32),
                        "min_labour": min_labour
                            .unwrap_or_else(|| policy["min_labour"].as_u64().unwrap_or(1) as u32),
                        "max_labour": max_labour
                            .unwrap_or_else(|| policy["max_labour"].as_u64().unwrap_or(8) as u32),
                        "expand_ready_threshold": expand_ready_threshold.unwrap_or_else(|| {
                            policy["expand_ready_threshold"].as_u64().unwrap_or(2) as u32
                        }),
                        "expand_critical_delay_secs": expand_critical_delay_secs.unwrap_or_else(
                            || policy["expand_critical_delay_secs"].as_u64().unwrap_or(120),
                        ),
                        "scale_down_idle_secs": scale_down_idle_secs.unwrap_or_else(|| {
                            policy["scale_down_idle_secs"].as_u64().unwrap_or(600)
                        }),
                        "cooldown_secs": cooldown_secs.unwrap_or_else(|| {
                            policy["cooldown_secs"].as_u64().unwrap_or(120)
                        }),
                        "max_step": max_step
                            .unwrap_or_else(|| policy["max_step"].as_u64().unwrap_or(2) as u32),
                        "budget_headroom_ratio": budget_headroom_ratio,
                    }
                }),
            )
            .await?
        }
        Command::OrchestrationPolicy { project } => {
            rpc(
                &discovery,
                "orchestration.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::OrchestrationPolicySet {
            project,
            auto_retry,
            max_attempts,
            retry_backoff_secs,
            retry_priority_boost,
            dependency_failure,
            escalate_after_retries,
            auto_resolve_blockers,
            stall_recovery,
            stall_min_observations,
            stall_max_attempts,
            stall_grace_secs,
        } => {
            let current = rpc(
                &discovery,
                "orchestration.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            if let Some(value) = auto_retry {
                policy["auto_retry_failed"] = json!(value);
            }
            if let Some(value) = max_attempts {
                policy["max_attempts"] = json!(value);
            }
            if let Some(value) = retry_backoff_secs {
                policy["retry_backoff_secs"] = json!(value);
            }
            if let Some(value) = retry_priority_boost {
                policy["retry_priority_boost"] = json!(value);
            }
            if let Some(value) = dependency_failure {
                policy["dependency_failure"] = json!(value);
            }
            if let Some(value) = escalate_after_retries {
                policy["escalate_after_retries"] = json!(value);
            }
            if let Some(value) = auto_resolve_blockers {
                policy["auto_resolve_blockers"] = json!(value);
            }
            if let Some(value) = stall_recovery {
                policy["stall_recovery"] = json!(value);
            }
            if let Some(value) = stall_min_observations {
                policy["stall_min_observations"] = json!(value);
            }
            if let Some(value) = stall_max_attempts {
                policy["stall_max_attempts"] = json!(value);
            }
            if let Some(value) = stall_grace_secs {
                policy["stall_grace_secs"] = json!(value);
            }
            rpc(
                &discovery,
                "orchestration.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::Orchestration { session } => {
            rpc(
                &discovery,
                "orchestration.snapshot",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::PlanRevisions { session } => {
            rpc(
                &discovery,
                "team_plan.revisions",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::OrchestrationOverview { project, limit } => {
            let mut params = json!({"limit": limit});
            if let Some(project) = project {
                params["project_id"] = json!(project);
            }
            rpc(&discovery, "orchestration.overview", params).await?
        }
        Command::Orchestrate { session, dry_run } => {
            rpc(
                &discovery,
                if dry_run {
                    "orchestration.preview"
                } else {
                    "orchestration.advance"
                },
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Coordination {
            session,
            action,
            task,
            agent,
            note,
            severity,
            message,
        } => {
            rpc(
                &discovery,
                "coordination.note",
                json!({
                    "session_id": session,
                    "action": action,
                    "task_id": task,
                    "target_agent_id": agent,
                    "target_note_id": note,
                    "severity": severity,
                    "message": message,
                }),
            )
            .await?
        }
        Command::Unblock {
            session,
            task,
            note,
            message,
        } => {
            rpc(
                &discovery,
                "coordination.note",
                json!({
                    "session_id": session,
                    "action": "unblock",
                    "task_id": task,
                    "target_note_id": note,
                    "severity": "info",
                    "message": message,
                }),
            )
            .await?
        }
        Command::PlanRevision { session, revision } => {
            rpc(
                &discovery,
                "team_plan.revision",
                json!({"session_id": session, "revision": revision}),
            )
            .await?
        }
        Command::PlanRestore {
            session,
            revision,
            reason,
            expected_revision,
        } => {
            rpc(
                &discovery,
                "team_plan.restore",
                json!({
                    "session_id": session,
                    "revision": revision,
                    "reason": reason,
                    "expected_revision": expected_revision,
                }),
            )
            .await?
        }
        Command::TaskEdit {
            session,
            file,
            reason,
            expected_revision,
        } => {
            let raw = read_json_input(file.as_deref())?;
            let task: Value = serde_json::from_str(&raw).context("parsing task JSON")?;
            rpc(
                &discovery,
                "team_plan.task_edit",
                json!({
                    "session_id": session,
                    "task": task,
                    "reason": reason,
                    "expected_revision": expected_revision,
                }),
            )
            .await?
        }
        Command::PlanFanout {
            session,
            template,
            items,
            file,
            artifact,
            join_task,
            join_title,
            join_acceptance,
            reason,
            expected_revision,
            yes,
        } => {
            let items: Vec<Value> = match (items, file) {
                (Some(raw), None) => serde_json::from_str(&raw).context("parsing fan-out items")?,
                (None, Some(path)) => {
                    let raw = read_json_input(Some(&path))?;
                    serde_json::from_str(&raw).context("parsing fan-out items")?
                }
                (None, None) => Vec::new(),
                (Some(_), Some(_)) => bail!("pass either --items or --file, not both"),
            };
            rpc(
                &discovery,
                "plan.fanout",
                json!({
                    "session_id": session,
                    "template_task_id": template,
                    "items": items,
                    "artifact_id": artifact,
                    "join_task_id": join_task,
                    "join_title": join_title,
                    "join_acceptance": join_acceptance,
                    "reason": reason,
                    "expected_revision": expected_revision,
                    "approved": yes,
                    "author": "operator",
                }),
            )
            .await?
        }
        Command::TeamPlanRevise {
            session,
            reason,
            file,
            expected_revision,
        } => {
            let raw = read_json_input(file.as_deref())?;
            let plan: Value = serde_json::from_str(&raw).context("parsing plan JSON")?;
            rpc(
                &discovery,
                "team_plan.revise",
                json!({
                    "session_id": session,
                    "plan": plan,
                    "reason": reason,
                    "expected_revision": expected_revision,
                }),
            )
            .await?
        }
        Command::Leases => rpc(&discovery, "runtime.leases", json!({})).await?,
        Command::LeasesSweep => rpc(&discovery, "runtime.leases.sweep", json!({})).await?,
        Command::LeaseHistory { key, limit } => {
            rpc(
                &discovery,
                "runtime.leases.history",
                json!({"lease_key": key, "limit": limit}),
            )
            .await?
        }
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
        Command::Dashboard { project } => {
            rpc(
                &discovery,
                "dashboard.snapshot",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ModelPresets => rpc(&discovery, "model.preset.list", json!({})).await?,
        Command::ModelPresetCreate {
            name,
            provider,
            model,
            context_window,
            input_price,
            output_price,
            notes,
        } => {
            rpc(
                &discovery,
                "model.preset.create",
                json!({
                    "name": name,
                    "provider": provider,
                    "model": model,
                    "context_window": context_window,
                    "input_usd_per_million": input_price,
                    "output_usd_per_million": output_price,
                    "notes": notes
                }),
            )
            .await?
        }
        Command::ModelPresetUpdate {
            id,
            name,
            provider,
            model,
            context_window,
            input_price,
            output_price,
            notes,
        } => {
            rpc(
                &discovery,
                "model.preset.update",
                json!({
                    "id": id,
                    "name": name,
                    "provider": provider,
                    "model": model,
                    "context_window": context_window,
                    "input_usd_per_million": input_price,
                    "output_usd_per_million": output_price,
                    "notes": notes
                }),
            )
            .await?
        }
        Command::ModelPresetDelete { id } => {
            rpc(&discovery, "model.preset.delete", json!({"id": id})).await?
        }
        Command::UpdateCheck { repo } => {
            rpc(&discovery, "update.check", json!({"repo": repo})).await?
        }
        Command::CheckpointPolicy { project } => {
            rpc(
                &discovery,
                "project.checkpoint_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::CheckpointPolicySet {
            project,
            enabled,
            older_than_days,
            keep_latest,
        } => {
            let policy = json!({
                "enabled": enabled,
                "older_than_secs": older_than_days.saturating_mul(86_400),
                "keep_latest": keep_latest
            });
            rpc(
                &discovery,
                "project.checkpoint_policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::WorktreePolicy { project } => {
            rpc(
                &discovery,
                "project.worktree_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::WorktreePolicySet {
            project,
            enabled,
            older_than_days,
            delete_branches,
            drop_failed,
        } => {
            let policy = json!({
                "enabled": enabled,
                "older_than_secs": older_than_days.saturating_mul(86_400),
                "delete_branches": delete_branches,
                "keep_failed": !drop_failed
            });
            rpc(
                &discovery,
                "project.worktree_policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::IntegrationPolicy { project } => {
            rpc(
                &discovery,
                "project.integration_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::IntegrationPolicySet {
            project,
            auto_resolve,
            max_auto_attempts,
            max_resolve_tokens,
        } => {
            rpc(
                &discovery,
                "project.integration_policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "auto_resolve": auto_resolve,
                        "max_auto_attempts": max_auto_attempts,
                        "max_resolve_tokens": max_resolve_tokens,
                    }
                }),
            )
            .await?
        }
        Command::Sessions { project } => {
            rpc(&discovery, "session.list", json!({"project_id": project})).await?
        }
        Command::Scheduler { project } => {
            rpc(
                &discovery,
                "scheduler.status",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::Programs {
            project,
            status,
            limit,
        } => {
            rpc(
                &discovery,
                "program.list",
                json!({"project_id": project, "status": status, "limit": limit}),
            )
            .await?
        }
        Command::ProgramStatus { id, events } => {
            rpc(
                &discovery,
                "program.status",
                json!({"program_id": id, "event_limit": events}),
            )
            .await?
        }
        Command::ProgramCreate {
            project,
            goal,
            activate,
            auto_dispatch,
            token_budget,
            cost_budget_usd,
            max_attempts,
            retry_backoff_secs,
            max_parallel,
            dependency_failure,
        } => {
            let dependency_failure = parse_dependency_failure(&dependency_failure)?;
            rpc(
                &discovery,
                "program.create",
                json!({
                    "project_id": project,
                    "goal": goal,
                    "activate": activate,
                    "token_budget": token_budget,
                    "cost_budget_usd": cost_budget_usd,
                    "policy": {
                        "auto_dispatch": auto_dispatch || activate,
                        "max_attempts": max_attempts,
                        "retry_backoff_secs": retry_backoff_secs,
                        "dependency_failure": dependency_failure,
                        "max_parallel_milestones": max_parallel,
                    },
                }),
            )
            .await?
        }
        Command::ProgramUpdate {
            id,
            goal,
            auto_dispatch,
            token_budget,
            cost_budget_usd,
            max_attempts,
            retry_backoff_secs,
            max_parallel,
            dependency_failure,
        } => {
            let dependency_failure = parse_dependency_failure(&dependency_failure)?;
            rpc(
                &discovery,
                "program.update",
                json!({
                    "program_id": id,
                    "goal": goal,
                    "token_budget": token_budget,
                    "cost_budget_usd": cost_budget_usd,
                    "policy": {
                        "auto_dispatch": auto_dispatch,
                        "max_attempts": max_attempts,
                        "retry_backoff_secs": retry_backoff_secs,
                        "dependency_failure": dependency_failure,
                        "max_parallel_milestones": max_parallel,
                    },
                }),
            )
            .await?
        }
        Command::ProgramPause { id, reason } => {
            rpc(
                &discovery,
                "program.pause",
                json!({"program_id": id, "reason": reason}),
            )
            .await?
        }
        Command::ProgramResume { id } => {
            rpc(&discovery, "program.resume", json!({"program_id": id})).await?
        }
        Command::ProgramCancel { id, reason } => {
            rpc(
                &discovery,
                "program.cancel",
                json!({"program_id": id, "reason": reason}),
            )
            .await?
        }
        Command::ProgramDelete { id } => {
            rpc(&discovery, "program.delete", json!({"program_id": id})).await?
        }
        Command::ProgramMilestoneAdd {
            program,
            title,
            objective,
            key,
            acceptance,
            depends_on,
            priority,
            max_attempts,
            profile,
            scope,
            token_budget,
            cost_budget_usd,
        } => {
            let objective = if objective == "-" {
                let mut buffer = String::new();
                std::io::stdin()
                    .read_to_string(&mut buffer)
                    .context("reading milestone objective from stdin")?;
                buffer
            } else {
                objective
            };
            rpc(
                &discovery,
                "program.milestone.create",
                json!({
                    "program_id": program,
                    "key": key,
                    "title": title,
                    "objective": objective,
                    "acceptance": acceptance,
                    "deps": depends_on,
                    "priority": priority,
                    "max_attempts": max_attempts,
                    "profile": profile,
                    "write_scope": scope,
                    "token_budget": token_budget,
                    "cost_budget_usd": cost_budget_usd,
                }),
            )
            .await?
        }
        Command::ProgramMilestoneRetry { id } => {
            rpc(
                &discovery,
                "program.milestone.retry",
                json!({"milestone_id": id}),
            )
            .await?
        }
        Command::ProgramMilestoneSkip { id, reason } => {
            rpc(
                &discovery,
                "program.milestone.skip",
                json!({"milestone_id": id, "reason": reason}),
            )
            .await?
        }
        Command::ProgramRun { milestone } => {
            rpc(
                &discovery,
                "program.milestone.start",
                json!({"milestone_id": milestone}),
            )
            .await?
        }
        Command::ProgramTick { project } => {
            rpc(&discovery, "program.tick", json!({"project_id": project})).await?
        }
        Command::ProgramReplay { id, repair } => {
            rpc(
                &discovery,
                "program.replay",
                json!({"program_id": id, "repair": repair}),
            )
            .await?
        }
        Command::Tasks { project, session } => {
            rpc(
                &discovery,
                "task.queue",
                json!({"project_id": project, "session_id": session}),
            )
            .await?
        }
        Command::Runs { session, limit } => {
            rpc(
                &discovery,
                "session.runs",
                json!({"session_id": session, "limit": limit}),
            )
            .await?
        }
        Command::RunDetail { run } => {
            rpc(&discovery, "session.run.detail", json!({"run_id": run})).await?
        }
        Command::RunResume {
            session,
            run,
            operation_id,
        } => {
            rpc(
                &discovery,
                "session.run.resume",
                json!({
                    "session_id": session,
                    "run_id": run,
                    "operation_id": operation_id,
                }),
            )
            .await?
        }
        Command::RunCancel { session, run } => {
            rpc(
                &discovery,
                "session.run.cancel",
                json!({"session_id": session, "run_id": run}),
            )
            .await?
        }
        Command::RunPolicy { project } => {
            rpc(
                &discovery,
                "session.run.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::RunPolicySet {
            project,
            auto_resume,
            max_attempts,
            max_age_hours,
        } => {
            let current = rpc(
                &discovery,
                "session.run.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            if let Some(object) = policy.as_object_mut() {
                if let Some(auto_resume) = auto_resume {
                    object.insert("auto_resume".into(), json!(auto_resume));
                }
                if let Some(max_attempts) = max_attempts {
                    object.insert("max_attempts".into(), json!(max_attempts));
                }
                if let Some(max_age_hours) = max_age_hours {
                    object.insert("max_age_hours".into(), json!(max_age_hours));
                }
            }
            rpc(
                &discovery,
                "session.run.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::Intake {
            project,
            status,
            limit,
        } => {
            rpc(
                &discovery,
                "intake.list",
                json!({"project_id": project, "status": status, "limit": limit}),
            )
            .await?
        }
        Command::IntakeAdd {
            project,
            title,
            instructions,
            profile,
            kind,
            priority,
            scope,
            acceptance,
            depends_on,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            max_attempts,
            labels,
        } => {
            let instructions = if instructions == "-" {
                let mut buffer = String::new();
                std::io::stdin()
                    .read_to_string(&mut buffer)
                    .context("reading intake instructions from stdin")?;
                buffer
            } else {
                instructions
            };
            rpc(
                &discovery,
                "intake.create",
                json!({
                    "project_id": project,
                    "title": title,
                    "instructions": instructions,
                    "profile": profile,
                    "kind": kind,
                    "priority": priority,
                    "write_scope": scope,
                    "acceptance": acceptance,
                    "deps": depends_on,
                    "token_budget": token_budget,
                    "cost_budget_usd": cost_budget_usd,
                    "time_budget_secs": time_budget_secs,
                    "max_attempts": max_attempts,
                    "labels": labels,
                    "origin": "cli",
                }),
            )
            .await?
        }
        Command::IntakeEdit {
            id,
            title,
            instructions,
            profile,
            kind,
            priority,
            scope,
            acceptance,
            depends_on,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            max_attempts,
            labels,
        } => {
            let instructions = if instructions == "-" {
                let mut buffer = String::new();
                std::io::stdin()
                    .read_to_string(&mut buffer)
                    .context("reading intake instructions from stdin")?;
                buffer
            } else {
                instructions
            };
            rpc(
                &discovery,
                "intake.update",
                json!({
                    "id": id,
                    "title": title,
                    "instructions": instructions,
                    "profile": profile,
                    "kind": kind,
                    "priority": priority,
                    "write_scope": scope,
                    "acceptance": acceptance,
                    "deps": depends_on,
                    "token_budget": token_budget,
                    "cost_budget_usd": cost_budget_usd,
                    "time_budget_secs": time_budget_secs,
                    "max_attempts": max_attempts,
                    "labels": labels,
                }),
            )
            .await?
        }
        Command::IntakeRun { id } => rpc(&discovery, "intake.dispatch", json!({"id": id})).await?,
        Command::IntakeCancel { id } => rpc(&discovery, "intake.cancel", json!({"id": id})).await?,
        Command::IntakeRetry { id } => rpc(&discovery, "intake.retry", json!({"id": id})).await?,
        Command::IntakeDelete { id } => rpc(&discovery, "intake.delete", json!({"id": id})).await?,
        Command::TaskDispatch { session } => {
            rpc(&discovery, "task.dispatch", json!({"session_id": session})).await?
        }
        Command::TaskRetry { session, task } => {
            rpc(
                &discovery,
                "task.retry",
                json!({"session_id": session, "task_id": task}),
            )
            .await?
        }
        Command::TaskSkip { session, task } => {
            rpc(
                &discovery,
                "task.skip",
                json!({"session_id": session, "task_id": task}),
            )
            .await?
        }
        Command::TaskCancel { session, task } => {
            rpc(
                &discovery,
                "task.cancel",
                json!({"session_id": session, "task_id": task}),
            )
            .await?
        }
        Command::TaskPause { session, task } => {
            rpc(
                &discovery,
                "task.pause",
                json!({"session_id": session, "task_id": task}),
            )
            .await?
        }
        Command::TaskResume { session, task } => {
            rpc(
                &discovery,
                "task.resume",
                json!({"session_id": session, "task_id": task}),
            )
            .await?
        }
        Command::Steer {
            session,
            text,
            agent,
            task,
            operation_id,
        } => {
            rpc(
                &discovery,
                "session.steer",
                json!({
                    "session_id": session,
                    "body": text,
                    "agent_id": agent,
                    "task_id": task,
                    "operation_id": operation_id,
                }),
            )
            .await?
        }
        Command::Steering {
            session,
            status,
            limit,
        } => {
            rpc(
                &discovery,
                "session.steering",
                json!({"session_id": session, "status": status, "limit": limit}),
            )
            .await?
        }
        Command::SteerCancel { id } => {
            rpc(&discovery, "session.steering.cancel", json!({"id": id})).await?
        }
        Command::Defer {
            session,
            prompt,
            watch,
            gate,
            watch_mode,
            after_secs,
            at,
            every_secs,
            max_runs,
            operation_id,
        } => {
            rpc(
                &discovery,
                "session.defer",
                json!({
                    "session_id": session,
                    "body": prompt,
                    "watch_task_ids": watch,
                    "watch_gate_id": gate,
                    "watch_mode": watch_mode,
                    "after_secs": after_secs,
                    "deadline_at": at,
                    "every_secs": every_secs,
                    "max_runs": max_runs,
                    "operation_id": operation_id,
                }),
            )
            .await?
        }
        Command::Continuations {
            session,
            status,
            limit,
        } => {
            rpc(
                &discovery,
                "session.continuations",
                json!({"session_id": session, "status": status, "limit": limit}),
            )
            .await?
        }
        Command::ContinuationCancel { id } => {
            rpc(&discovery, "session.continuation.cancel", json!({"id": id})).await?
        }
        Command::ContinuationStatus { id } => {
            rpc(&discovery, "session.continuation.status", json!({"id": id})).await?
        }
        Command::ContinuationReclaim { id, reason } => {
            rpc(
                &discovery,
                "session.continuation.reclaim",
                json!({"id": id, "reason": reason}),
            )
            .await?
        }
        Command::TaskStalls { session } => {
            rpc(&discovery, "session.stalls", json!({"session_id": session})).await?
        }
        Command::ToolActivity {
            session,
            limit,
            csv: _,
        } => {
            rpc(
                &discovery,
                "session.tool_activity",
                json!({"session_id": session, "limit": limit}),
            )
            .await?
        }
        Command::Recoveries { session } => {
            rpc(
                &discovery,
                "orchestration.recoveries",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::SessionNew {
            project,
            goal,
            priority,
            token_budget,
            cost_budget_usd,
            time_budget_secs,
            unlimited,
        } => {
            rpc(
                &discovery,
                "session.create",
                json!({
                    "project_id": project,
                    "goal": goal,
                    "priority": priority,
                    "token_budget": token_budget,
                    "cost_budget_usd": cost_budget_usd,
                    "time_budget_secs": time_budget_secs,
                    "unlimited": unlimited,
                }),
            )
            .await?
        }
        Command::SessionUnlimited { session, enabled } => {
            rpc(
                &discovery,
                "session.budget.set",
                json!({"session_id": session, "unlimited": enabled.unwrap_or(true)}),
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
        Command::SessionPriority { session, priority } => {
            rpc(
                &discovery,
                "session.priority.set",
                json!({"session_id": session, "priority": priority}),
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
        Command::Search {
            query,
            project,
            limit,
        } => {
            rpc(
                &discovery,
                "session.search",
                json!({"query": query, "project_id": project, "limit": limit}),
            )
            .await?
        }
        Command::Events {
            session,
            since,
            limit,
        } => {
            rpc(
                &discovery,
                "event.list",
                json!({"session_id": session, "since_seq": since, "limit": limit}),
            )
            .await?
        }
        Command::Prompt {
            session,
            content,
            operation_id,
        } => {
            rpc(
                &discovery,
                "session.prompt",
                json!({"session_id": session, "content": content, "operation_id": operation_id}),
            )
            .await?
        }
        Command::Cancel { session } => {
            rpc(&discovery, "session.cancel", json!({"session_id": session})).await?
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
        Command::Pause { session } => {
            rpc(&discovery, "session.pause", json!({"session_id": session})).await?
        }
        Command::Resume { session } => {
            rpc(&discovery, "session.resume", json!({"session_id": session})).await?
        }
        Command::ResumePlan { session } => {
            rpc(
                &discovery,
                "session.plan_resume",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Compatibility { session, run } => {
            let mut params = json!({"session_id": session});
            if let Some(run) = run {
                params["run_id"] = json!(run);
            }
            rpc(&discovery, "session.compatibility", params).await?
        }
        Command::Diagnose {
            session,
            task,
            limit,
            graph,
            record,
            history,
        } => {
            if history {
                rpc(
                    &discovery,
                    "session.attributions",
                    json!({"session_id": session, "limit": limit}),
                )
                .await?
            } else {
                let method = if record {
                    "session.diagnosis.record"
                } else {
                    "session.diagnosis"
                };
                rpc(
                    &discovery,
                    method,
                    json!({
                        "session_id": session,
                        "target_task_id": task,
                        "limit": limit,
                        "include_graph": graph,
                    }),
                )
                .await?
            }
        }
        Command::PlanMigrate {
            session,
            reason,
            expected_revision,
            yes,
            approved_by,
        } => {
            rpc(
                &discovery,
                "plan.migrate",
                json!({
                    "session_id": session,
                    "reason": reason,
                    "expected_revision": expected_revision,
                    "approved": yes,
                    "approved_by": approved_by,
                }),
            )
            .await?
        }
        Command::OrchestrateReplay { session, limit } => {
            rpc(
                &discovery,
                "orchestration.replay",
                json!({"session_id": session, "limit": limit}),
            )
            .await?
        }
        Command::Context { session } => {
            rpc(
                &discovery,
                "session.context",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Progress {
            session,
            plan_revision,
            limit,
        } => {
            rpc(
                &discovery,
                "session.progress",
                json!({
                    "session_id": session,
                    "plan_revision": plan_revision,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::ProgressReport { session, payload } => {
            let round: serde_json::Value =
                serde_json::from_str(&payload).context("parsing --payload as JSON")?;
            rpc(
                &discovery,
                "session.progress.report",
                json!({"session_id": session, "round": round}),
            )
            .await?
        }
        Command::A2aStatus => rpc(&discovery, "a2a.status", json!({})).await?,
        Command::A2aCard => rpc(&discovery, "a2a.card", json!({})).await?,
        Command::A2aPolicySet {
            enable,
            disable,
            allow_inbound,
            port,
            max_tasks,
            task_ttl_secs,
        } => {
            let mut policy = vp_protocol::a2a::A2aPolicy {
                enabled: enable && !disable,
                allow_inbound,
                ..Default::default()
            };
            if let Some(port) = port {
                policy.port = port;
            }
            if let Some(max_tasks) = max_tasks {
                policy.max_tasks = max_tasks;
            }
            if let Some(task_ttl_secs) = task_ttl_secs {
                policy.task_ttl_secs = task_ttl_secs;
            }
            rpc(&discovery, "a2a.policy.set", json!({"policy": policy})).await?
        }
        Command::A2aTasks { limit, state } => {
            rpc(
                &discovery,
                "a2a.tasks",
                json!({"limit": limit, "state": state}),
            )
            .await?
        }
        Command::A2aStart {
            approved,
            port,
            max_tasks,
            task_ttl_secs,
            allow_inbound,
            enable,
        } => {
            let mut policy = vp_protocol::a2a::A2aPolicy {
                enabled: enable,
                allow_inbound,
                ..Default::default()
            };
            if let Some(port) = port {
                policy.port = port;
            }
            if let Some(max_tasks) = max_tasks {
                policy.max_tasks = max_tasks;
            }
            if let Some(task_ttl_secs) = task_ttl_secs {
                policy.task_ttl_secs = task_ttl_secs;
            }
            rpc(
                &discovery,
                "a2a.start",
                json!({"approved": approved, "policy": policy}),
            )
            .await?
        }
        Command::A2aStop { reason } => {
            rpc(&discovery, "a2a.stop", json!({"reason": reason})).await?
        }
        Command::Compact { session } => {
            rpc(
                &discovery,
                "session.compact",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Checkpoint { session, label } => {
            rpc(
                &discovery,
                "session.checkpoint",
                json!({"session_id": session, "label": label}),
            )
            .await?
        }
        Command::Checkpoints { session } => {
            rpc(
                &discovery,
                "session.checkpoints",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::CheckpointStats { session } => {
            rpc(
                &discovery,
                "session.checkpoint.stats",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::CheckpointRestore {
            session,
            checkpoint,
        } => {
            rpc(
                &discovery,
                "session.checkpoint.restore",
                json!({"session_id": session, "checkpoint_id": checkpoint}),
            )
            .await?
        }
        Command::CheckpointDelete {
            session,
            checkpoint,
        } => {
            rpc(
                &discovery,
                "session.checkpoint.delete",
                json!({"session_id": session, "checkpoint_id": checkpoint}),
            )
            .await?
        }
        Command::CheckpointPrune {
            session,
            older_than_days,
            keep_latest,
            dry_run,
        } => {
            rpc(
                &discovery,
                "session.checkpoint.prune",
                json!({
                    "session_id": session,
                    "older_than_days": older_than_days,
                    "keep_latest": keep_latest,
                    "dry_run": dry_run
                }),
            )
            .await?
        }
        Command::ContextCheckpoints { session, limit } => {
            rpc(
                &discovery,
                "session.context.checkpoints",
                json!({"session_id": session, "limit": limit}),
            )
            .await?
        }
        Command::ContextCheckpoint { session, label } => {
            rpc(
                &discovery,
                "session.context.checkpoint",
                json!({"session_id": session, "label": label}),
            )
            .await?
        }
        Command::ContextCheckpointDetail {
            session,
            checkpoint,
        } => {
            rpc(
                &discovery,
                "session.context.checkpoint.detail",
                json!({"session_id": session, "checkpoint_id": checkpoint}),
            )
            .await?
        }
        Command::ContextRestore {
            session,
            checkpoint,
            force,
        } => {
            rpc(
                &discovery,
                "session.context.restore",
                json!({
                    "session_id": session,
                    "checkpoint_id": checkpoint,
                    "force": force
                }),
            )
            .await?
        }
        Command::ContextCheckpointDelete {
            session,
            checkpoint,
        } => {
            rpc(
                &discovery,
                "session.context.checkpoint.delete",
                json!({"session_id": session, "checkpoint_id": checkpoint}),
            )
            .await?
        }
        Command::ContextCheckpointPrune {
            session,
            older_than_days,
            keep_latest,
            dry_run,
        } => {
            rpc(
                &discovery,
                "session.context.checkpoint.prune",
                json!({
                    "session_id": session,
                    "older_than_days": older_than_days,
                    "keep_latest": keep_latest,
                    "dry_run": dry_run
                }),
            )
            .await?
        }
        Command::ContextCheckpointPolicy { project } => {
            rpc(
                &discovery,
                "session.context.policy",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ContextCheckpointPolicySet {
            project,
            enabled,
            max_checkpoints,
            max_age_days,
        } => {
            rpc(
                &discovery,
                "session.context.policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "enabled": enabled,
                        "max_checkpoints": max_checkpoints,
                        "max_age_days": max_age_days
                    }
                }),
            )
            .await?
        }
        Command::Fork {
            session,
            checkpoint,
            goal,
        } => {
            rpc(
                &discovery,
                "session.fork",
                json!({
                    "session_id": session,
                    "checkpoint_id": checkpoint,
                    "goal": goal
                }),
            )
            .await?
        }
        Command::Lineage { session } => {
            rpc(
                &discovery,
                "session.lineage",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::ForkTree { session } => {
            rpc(
                &discovery,
                "session.fork.tree",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::ForkForest { project } => {
            rpc(
                &discovery,
                "project.fork.forest",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ForkAudit { project } => {
            rpc(
                &discovery,
                "project.fork.audit",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ForkRepair { project } => {
            rpc(
                &discovery,
                "project.fork.repair",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::Compare { session, other } => {
            rpc(
                &discovery,
                "session.compare",
                json!({"session_id": session, "other_session_id": other}),
            )
            .await?
        }
        Command::ForkMerge { session, fork } => {
            rpc(
                &discovery,
                "session.fork.merge",
                json!({"source_session_id": session, "fork_session_id": fork}),
            )
            .await?
        }
        Command::Todos { session } => {
            rpc(&discovery, "session.todos", json!({"session_id": session})).await?
        }
        Command::TeamPlan { session } => {
            rpc(&discovery, "team_plan.get", json!({"session_id": session})).await?
        }
        Command::CitationAudit { session } => {
            rpc(
                &discovery,
                "session.citation_audit",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Citations { session, message } => {
            rpc(
                &discovery,
                "session.citations",
                json!({"session_id": session, "message_id": message}),
            )
            .await?
        }
        Command::Transcript { session } => {
            rpc(
                &discovery,
                "session.transcript",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Usage { session, csv } => {
            let method = if csv {
                "session.usage_csv"
            } else {
                "session.usage"
            };
            rpc(&discovery, method, json!({"session_id": session})).await?
        }
        Command::Cache { session } => {
            rpc(&discovery, "session.cache", json!({"session_id": session})).await?
        }
        Command::CacheEpoch { session, limit } => {
            rpc(
                &discovery,
                "session.cache.epoch",
                json!({"session_id": session, "limit": limit}),
            )
            .await?
        }
        Command::CacheEpochs { project, limit } => {
            rpc(
                &discovery,
                "project.cache_epochs",
                json!({"project_id": project, "limit": limit}),
            )
            .await?
        }
        Command::CacheEpochsPrune {
            project,
            older_than_days,
            keep_epochs,
            dry_run,
        } => {
            rpc(
                &discovery,
                "project.cache_epochs.prune",
                json!({
                    "project_id": project,
                    "older_than_days": older_than_days,
                    "keep_epochs": keep_epochs,
                    "dry_run": dry_run,
                }),
            )
            .await?
        }
        Command::CacheCalls {
            session,
            limit,
            role,
            miss_reason,
            include_warm,
        } => {
            rpc(
                &discovery,
                "session.cache.calls",
                json!({
                    "session_id": session,
                    "limit": limit,
                    "role": role,
                    "miss_reason": miss_reason,
                    "include_warm": include_warm,
                }),
            )
            .await?
        }
        Command::Contract { session, task } => {
            rpc(
                &discovery,
                "session.contract",
                json!({"session_id": session, "task_id": task}),
            )
            .await?
        }
        Command::ContractPolicy { project } => {
            rpc(
                &discovery,
                "project.contract_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ContractPolicySet {
            project,
            enabled,
            mode,
            enforce_shell,
            forbidden_paths,
            egress_allowlist,
            max_writes,
            max_bytes,
            max_commits,
        } => {
            let current = rpc(
                &discovery,
                "project.contract_policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            if let Some(enabled) = enabled {
                policy["enabled"] = json!(enabled);
            }
            if let Some(mode) = mode {
                if vp_protocol::types::ContractMode::parse(&mode).is_none() {
                    bail!("contract mode must be `advisory` or `enforce`, got {mode}");
                }
                policy["mode"] = json!(mode.to_ascii_lowercase());
            }
            if let Some(enforce_shell) = enforce_shell {
                policy["enforce_shell"] = json!(enforce_shell);
            }
            if !forbidden_paths.is_empty() {
                policy["forbidden_paths"] = json!(forbidden_paths);
            }
            if !egress_allowlist.is_empty() {
                policy["egress_allowlist"] = json!(egress_allowlist);
            }
            if let Some(max_writes) = max_writes {
                policy["max_writes"] = json!(max_writes);
            }
            if let Some(max_bytes) = max_bytes {
                policy["max_bytes"] = json!(max_bytes);
            }
            if let Some(max_commits) = max_commits {
                policy["max_commits"] = json!(max_commits);
            }
            rpc(
                &discovery,
                "project.contract_policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::Agents {
            project,
            session,
            role,
            status,
        } => {
            if let Some(session) = session {
                rpc(&discovery, "agent.list", json!({"session_id": session})).await?
            } else {
                let projects = rpc(&discovery, "project.list", json!({})).await?;
                let is_project = projects["projects"].as_array().is_some_and(|rows| {
                    rows.iter().any(|row| row["id"].as_str() == Some(&project))
                });
                if is_project {
                    rpc(
                        &discovery,
                        "agent.identities.list",
                        json!({"project_id": project, "role": role, "status": status}),
                    )
                    .await?
                } else {
                    // Backward-compatible `vp agents <session>` behavior.
                    rpc(&discovery, "agent.list", json!({"session_id": project})).await?
                }
            }
        }
        Command::AgentOrg {
            project,
            include_retired,
            limit,
        } => {
            rpc(
                &discovery,
                "agent.org.snapshot",
                json!({
                    "project_id": project,
                    "include_retired": include_retired,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::AgentDetail {
            identity,
            project,
            include_inbox,
            inbox_limit,
            memory_limit,
            assignment_limit,
        } => {
            rpc(
                &discovery,
                "agent.identity.detail",
                json!({
                    "identity_id": identity,
                    "project_id": project,
                    "include_inbox_content": include_inbox,
                    "inbox_limit": inbox_limit,
                    "memory_limit": memory_limit,
                    "assignment_limit": assignment_limit,
                }),
            )
            .await?
        }
        Command::TeamExport {
            project,
            name,
            version,
            description,
            out,
        } => {
            let value = rpc(
                &discovery,
                "team.template.export",
                json!({
                    "project_id": project,
                    "name": name,
                    "version": version,
                    "description": description,
                }),
            )
            .await?;
            let json_body = value
                .get("json")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("team export returned no JSON body"))?;
            let markdown = value
                .get("markdown")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("team export returned no Markdown body"))?;
            std::fs::create_dir_all(&out).with_context(|| format!("creating {}", out.display()))?;
            let json_path = out.join("team.json");
            let markdown_path = out.join("TEAM.md");
            std::fs::write(&json_path, json_body)
                .with_context(|| format!("writing {}", json_path.display()))?;
            std::fs::write(&markdown_path, markdown)
                .with_context(|| format!("writing {}", markdown_path.display()))?;
            json!({
                "out": out.display().to_string(),
                "json_path": json_path.display().to_string(),
                "markdown_path": markdown_path.display().to_string(),
                "template_id": value["template"]["template_id"],
                "name": value["template"]["name"],
                "version": value["template"]["version"],
                "content_hash": value["template"]["content_hash"],
                "warnings": value["warnings"],
            })
        }
        Command::TeamImport {
            project,
            file,
            dry_run,
            apply,
            approved,
            operator,
            resolution_json,
        } => {
            let json_body = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let resolutions: Vec<vp_protocol::types::TeamTemplateResolution> = match resolution_json
            {
                Some(raw) => serde_json::from_str(&raw)
                    .context("--resolution-json must be a JSON array of resolutions")?,
                None => Vec::new(),
            };
            let method = if dry_run {
                "team.template.preview"
            } else {
                "team.template.import"
            };
            rpc(
                &discovery,
                method,
                json!({
                    "project_id": project,
                    "json": json_body,
                    "dry_run": dry_run,
                    "apply": apply && !dry_run,
                    "approved": approved,
                    "operator": operator,
                    "resolutions": resolutions,
                }),
            )
            .await?
        }
        Command::TeamTemplates { project, limit } => {
            rpc(
                &discovery,
                "team.template.list",
                json!({"project_id": project, "limit": limit}),
            )
            .await?
        }
        Command::TeamDiff { project, template } => {
            rpc(
                &discovery,
                "team.template.diff",
                json!({"project_id": project, "template_id": template}),
            )
            .await?
        }
        Command::TeamTemplateShow { template, project } => {
            rpc(
                &discovery,
                "team.template.get",
                json!({"template_id": template, "project_id": project}),
            )
            .await?
        }
        Command::TeamReplay { project, repair } => {
            rpc(
                &discovery,
                "team.template.replay",
                json!({"project_id": project, "repair": repair}),
            )
            .await?
        }
        Command::AgentCreate {
            project,
            role,
            department,
            component,
            title,
            profile,
            model,
            parent,
            policy,
            approved,
        } => {
            let policy = parse_agent_identity_policy_json(policy.as_deref())?;
            if role.trim().eq_ignore_ascii_case("leader") {
                rpc(
                    &discovery,
                    "agent.identity.create",
                    json!({
                        "project_id": project,
                        "role": role,
                        "department": department,
                        "component": component,
                        "title": title,
                        "profile": profile,
                        "model": model,
                        "parent_identity_id": parent,
                        "policy": policy,
                    }),
                )
                .await?
            } else {
                rpc(
                    &discovery,
                    "agent.hire",
                    json!({
                        "project_id": project,
                        "role": role,
                        "department": department,
                        "component": component,
                        "title": title,
                        "profile": profile,
                        "model": model,
                        "parent_identity_id": parent,
                        "policy": policy,
                        "approved": approved,
                    }),
                )
                .await?
            }
        }
        Command::AgentUpdate {
            identity,
            role,
            department,
            clear_department,
            component,
            clear_component,
            title,
            clear_title,
            profile,
            clear_profile,
            model,
            clear_model,
            parent,
            clear_parent,
            successor,
            clear_successor,
            policy,
        } => {
            if role.is_some()
                || department.is_some()
                || clear_department
                || component.is_some()
                || clear_component
                || parent.is_some()
                || clear_parent
            {
                bail!(
                    "role/department/component/parent changes require `vp agent-promote` or `vp agent-transfer` approval"
                );
            }
            if clear_department && department.is_some() {
                bail!("pass only one of --department or --clear-department");
            }
            if clear_component && component.is_some() {
                bail!("pass only one of --component or --clear-component");
            }
            if clear_title && title.is_some() {
                bail!("pass only one of --title or --clear-title");
            }
            if clear_profile && profile.is_some() {
                bail!("pass only one of --profile or --clear-profile");
            }
            if clear_model && model.is_some() {
                bail!("pass only one of --model or --clear-model");
            }
            if clear_parent && parent.is_some() {
                bail!("pass only one of --parent or --clear-parent");
            }
            if clear_successor && successor.is_some() {
                bail!("pass only one of --successor or --clear-successor");
            }
            let mut params = serde_json::Map::new();
            params.insert("identity_id".into(), json!(identity));
            if let Some(role) = role {
                params.insert("role".into(), json!(role));
            }
            insert_agent_identity_optional(&mut params, "department", department, clear_department);
            insert_agent_identity_optional(&mut params, "component", component, clear_component);
            insert_agent_identity_optional(&mut params, "title", title, clear_title);
            insert_agent_identity_optional(&mut params, "profile", profile, clear_profile);
            insert_agent_identity_optional(&mut params, "model", model, clear_model);
            insert_agent_identity_optional(&mut params, "parent_identity_id", parent, clear_parent);
            insert_agent_identity_optional(&mut params, "successor_id", successor, clear_successor);
            if let Some(policy) = policy {
                params.insert(
                    "policy".into(),
                    parse_agent_identity_policy_json(Some(&policy))?.unwrap_or_else(|| json!({})),
                );
            }
            rpc(&discovery, "agent.identity.update", Value::Object(params)).await?
        }
        Command::AgentPause { identity } => {
            rpc(
                &discovery,
                "agent.identity.pause",
                json!({"identity_id": identity}),
            )
            .await?
        }
        Command::AgentResume { identity } => {
            rpc(
                &discovery,
                "agent.identity.resume",
                json!({"identity_id": identity}),
            )
            .await?
        }
        Command::AgentRetire {
            identity,
            successor,
            proposal,
            approved,
            force,
            reason,
            operator,
        } => {
            rpc(
                &discovery,
                "agent.retire",
                json!({
                    "identity_id": identity,
                    "successor_id": successor,
                    "proposal_id": proposal,
                    "approved": approved,
                    "force": force,
                    "reason": reason,
                    "operator": operator,
                }),
            )
            .await?
        }
        Command::AgentHire {
            project,
            role,
            department,
            component,
            title,
            profile,
            model,
            parent,
            policy,
            approved,
            operator,
        } => {
            let policy = parse_agent_identity_policy_json(policy.as_deref())?;
            rpc(
                &discovery,
                "agent.hire",
                json!({
                    "project_id": project,
                    "role": role,
                    "department": department,
                    "component": component,
                    "title": title,
                    "profile": profile,
                    "model": model,
                    "parent_identity_id": parent,
                    "policy": policy,
                    "approved": approved,
                    "operator": operator,
                }),
            )
            .await?
        }
        Command::AgentHires {
            project,
            status,
            limit,
        } => {
            rpc(
                &discovery,
                "agent.hire.list",
                json!({"project_id": project, "status": status, "limit": limit}),
            )
            .await?
        }
        Command::AgentHireApprove { request, operator } => {
            rpc(
                &discovery,
                "agent.hire.approve",
                json!({"request_id": request, "operator": operator, "approval_risk": "host_state"}),
            )
            .await?
        }
        Command::AgentHireReject {
            request,
            reason,
            operator,
        } => {
            rpc(
                &discovery,
                "agent.hire.reject",
                json!({"request_id": request, "reason": reason, "operator": operator}),
            )
            .await?
        }
        Command::AgentPromote {
            identity,
            role,
            parent,
            title,
            force,
            operator,
        } => {
            rpc(
                &discovery,
                "agent.promote",
                json!({
                    "identity_id": identity,
                    "target_role": role,
                    "parent_identity_id": parent,
                    "title": title,
                    "approved": true,
                    "force": force,
                    "operator": operator,
                }),
            )
            .await?
        }
        Command::AgentTransfer {
            identity,
            department,
            clear_department,
            component,
            clear_component,
            title,
            clear_title,
            parent,
            clear_parent,
            force,
            operator,
        } => {
            if clear_department && department.is_some() {
                bail!("pass only one of --department or --clear-department");
            }
            if clear_component && component.is_some() {
                bail!("pass only one of --component or --clear-component");
            }
            if clear_title && title.is_some() {
                bail!("pass only one of --title or --clear-title");
            }
            if clear_parent && parent.is_some() {
                bail!("pass only one of --parent or --clear-parent");
            }
            if department.is_none()
                && !clear_department
                && component.is_none()
                && !clear_component
                && title.is_none()
                && !clear_title
                && parent.is_none()
                && !clear_parent
            {
                bail!("pass at least one transfer field");
            }
            let mut params = serde_json::Map::new();
            params.insert("identity_id".into(), json!(identity));
            params.insert("approved".into(), json!(true));
            params.insert("force".into(), json!(force));
            if let Some(operator) = operator {
                params.insert("operator".into(), json!(operator));
            }
            insert_agent_identity_optional(&mut params, "department", department, clear_department);
            insert_agent_identity_optional(&mut params, "component", component, clear_component);
            insert_agent_identity_optional(&mut params, "title", title, clear_title);
            insert_agent_identity_optional(&mut params, "parent_identity_id", parent, clear_parent);
            rpc(&discovery, "agent.transfer", Value::Object(params)).await?
        }
        Command::AgentPerformance {
            project,
            identity,
            lookback_days,
            limit,
        } => {
            if let Some(identity) = identity {
                rpc(
                    &discovery,
                    "agent.performance",
                    json!({
                        "project_id": project,
                        "identity_id": identity,
                        "lookback_days": lookback_days,
                    }),
                )
                .await?
            } else {
                rpc(
                    &discovery,
                    "agent.performance.summary",
                    json!({
                        "project_id": project,
                        "lookback_days": lookback_days,
                        "limit": limit,
                    }),
                )
                .await?
            }
        }
        Command::AgentRetirement {
            project,
            candidates,
            proposals,
            propose,
            approve,
            reject,
            reason,
            identity,
            status,
            operator,
            limit,
        } => {
            let selected = [
                candidates,
                proposals,
                propose.is_some(),
                approve.is_some(),
                reject.is_some(),
            ]
            .into_iter()
            .filter(|value| *value)
            .count();
            if selected > 1 {
                bail!("pass only one retirement action");
            }
            if let Some(identity) = propose {
                rpc(
                    &discovery,
                    "agent.retirement.propose",
                    json!({"identity_id": identity, "operator": operator}),
                )
                .await?
            } else if let Some(proposal) = approve {
                rpc(
                    &discovery,
                    "agent.retirement.approve",
                    json!({"proposal_id": proposal, "operator": operator}),
                )
                .await?
            } else if let Some(proposal) = reject {
                rpc(
                    &discovery,
                    "agent.retirement.reject",
                    json!({"proposal_id": proposal, "reason": reason, "operator": operator}),
                )
                .await?
            } else if proposals {
                rpc(
                    &discovery,
                    "agent.retirement.proposals",
                    json!({
                        "project_id": project,
                        "identity_id": identity,
                        "status": status,
                        "limit": limit,
                    }),
                )
                .await?
            } else {
                rpc(
                    &discovery,
                    "agent.retirement.candidates",
                    json!({"project_id": project, "limit": limit}),
                )
                .await?
            }
        }
        Command::RetirementPolicy { project } => {
            rpc(
                &discovery,
                "project.retirement_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::RetirementPolicySet {
            project,
            enabled,
            lookback_days,
            min_completed_tasks,
            max_consecutive_failures,
            max_verifier_rejections,
            max_unsubstantiated_handoffs,
            max_false_consensus,
            cooldown_secs,
            require_approval,
        } => {
            let current = rpc(
                &discovery,
                "project.retirement_policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            if let Some(enabled) = enabled {
                policy["enabled"] = json!(enabled);
            }
            if let Some(value) = lookback_days {
                policy["lookback_days"] = json!(value);
            }
            if let Some(value) = min_completed_tasks {
                policy["min_completed_tasks"] = json!(value);
            }
            if let Some(value) = max_consecutive_failures {
                policy["max_consecutive_failures"] = json!(value);
            }
            if let Some(value) = max_verifier_rejections {
                policy["max_verifier_rejections"] = json!(value);
            }
            if let Some(value) = max_unsubstantiated_handoffs {
                policy["max_unsubstantiated_handoffs"] = json!(value);
            }
            if let Some(value) = max_false_consensus {
                policy["max_false_consensus"] = json!(value);
            }
            if let Some(value) = cooldown_secs {
                policy["cooldown_secs"] = json!(value);
            }
            if let Some(value) = require_approval {
                policy["require_approval"] = json!(value);
            }
            rpc(
                &discovery,
                "project.retirement_policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::AgentPolicy { project } => {
            rpc(
                &discovery,
                "project.agent_identity_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::AgentPolicySet {
            project,
            enabled,
            max_managers,
            max_workers,
            hire_requires_approval,
            retirement_requires_approval,
        } => {
            let current = rpc(
                &discovery,
                "project.agent_identity_policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            if let Some(enabled) = enabled {
                policy["enabled"] = json!(enabled);
            }
            if let Some(max_managers) = max_managers {
                policy["max_managers"] = json!(max_managers);
            }
            if let Some(max_workers) = max_workers {
                policy["max_workers"] = json!(max_workers);
            }
            if let Some(hire_requires_approval) = hire_requires_approval {
                policy["hire_requires_approval"] = json!(hire_requires_approval);
            }
            if let Some(retirement_requires_approval) = retirement_requires_approval {
                policy["retirement_requires_approval"] = json!(retirement_requires_approval);
            }
            rpc(
                &discovery,
                "project.agent_identity_policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::AgentMemory {
            project,
            identity,
            scope,
            status,
            department,
            query,
            limit,
            include_expired,
        } => {
            rpc(
                &discovery,
                "agent.memory.list",
                json!({
                    "project_id": project,
                    "identity_id": identity,
                    "scope": scope,
                    "status": status,
                    "department": department,
                    "query": query,
                    "limit": limit,
                    "include_expired": include_expired,
                }),
            )
            .await?
        }
        Command::AgentMemoryRemember {
            project,
            scope,
            content,
            identity,
            department,
            component,
            source,
            evidence,
            expires_at,
            pinned,
        } => {
            let evidence = evidence
                .iter()
                .map(|entry| parse_memory_evidence(entry))
                .collect::<Result<Vec<_>>>()?;
            rpc(
                &discovery,
                "agent.memory.remember",
                json!({
                    "project_id": project,
                    "scope": scope,
                    "content": content,
                    "identity_id": identity,
                    "department": department,
                    "component": component,
                    "source": source.unwrap_or_else(|| "operator".to_string()),
                    "evidence": evidence,
                    "expires_at": expires_at,
                    "pinned": pinned,
                }),
            )
            .await?
        }
        Command::AgentMemorySupersede {
            memory,
            replacement,
            content,
            source,
            expected_hash,
        } => {
            if replacement.is_none() && content.is_none() {
                bail!("pass --replacement or --content");
            }
            rpc(
                &discovery,
                "agent.memory.supersede",
                json!({
                    "memory_id": memory,
                    "replacement_memory_id": replacement,
                    "content": content,
                    "source": source,
                    "expected_content_hash": expected_hash,
                }),
            )
            .await?
        }
        Command::AgentMemoryForget { memory, prune } => {
            rpc(
                &discovery,
                "agent.memory.forget",
                json!({"memory_id": memory, "prune": prune}),
            )
            .await?
        }
        Command::AgentMemoryErase {
            memory,
            project,
            reason,
            yes,
        } => {
            rpc(
                &discovery,
                "agent.memory.erase",
                json!({
                    "memory_id": memory,
                    "project_id": project,
                    "reason": reason,
                    "approved": yes,
                }),
            )
            .await?
        }
        Command::AgentMemorySweep {
            project,
            older_than_days,
            limit,
            prune,
        } => {
            rpc(
                &discovery,
                "agent.memory.sweep",
                json!({
                    "project_id": project,
                    "older_than_days": older_than_days,
                    "limit": limit,
                    "prune": prune,
                }),
            )
            .await?
        }
        Command::Assignments {
            session,
            identity,
            project,
            status,
            limit,
        } => {
            if session.is_none() && identity.is_none() && project.is_none() {
                bail!("pass a session, --identity, or --project");
            }
            rpc(
                &discovery,
                "session.assignments",
                json!({
                    "session_id": session,
                    "identity_id": identity,
                    "project_id": project,
                    "status": status,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::Assign {
            session,
            identity,
            role,
            lease_secs,
            wake,
            operator,
        } => {
            rpc(
                &discovery,
                "session.assign",
                json!({
                    "session_id": session,
                    "identity_id": identity,
                    "role": role,
                    "lease_secs": lease_secs,
                    "wake_session": wake,
                    "operator": operator,
                }),
            )
            .await?
        }
        Command::AssignmentRenew {
            session,
            identity,
            lease_secs,
            operator,
        } => {
            rpc(
                &discovery,
                "session.assignment.renew",
                json!({
                    "session_id": session,
                    "identity_id": identity,
                    "lease_secs": lease_secs,
                    "operator": operator,
                }),
            )
            .await?
        }
        Command::Release {
            session,
            identity,
            operator,
            reason,
        } => {
            rpc(
                &discovery,
                "session.release",
                json!({
                    "session_id": session,
                    "identity_id": identity,
                    "operator": operator,
                    "reason": reason,
                }),
            )
            .await?
        }
        Command::Workspaces {
            project,
            session,
            identity,
            status,
            limit,
        } => {
            if project.is_none() && session.is_none() && identity.is_none() {
                bail!("pass --project, --session, or --identity");
            }
            rpc(
                &discovery,
                "agent.workspace.list",
                json!({
                    "project_id": project,
                    "session_id": session,
                    "identity_id": identity,
                    "status": status,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::Workspace { workspace, session } => match (workspace, session) {
            (Some(workspace_id), None) => {
                rpc(
                    &discovery,
                    "agent.workspace.get",
                    json!({"workspace_id": workspace_id}),
                )
                .await?
            }
            (None, Some(session_id)) => {
                rpc(
                    &discovery,
                    "session.workspace",
                    json!({"session_id": session_id}),
                )
                .await?
            }
            (Some(_), Some(_)) => bail!("pass either a workspace id or --session, not both"),
            (None, None) => bail!("pass a workspace id or --session"),
        },
        Command::WorkspaceLand {
            workspace,
            session,
            operator,
            yes,
        } => {
            if !yes {
                bail!("workspace-land requires --yes to confirm the component-branch merge");
            }
            rpc(
                &discovery,
                "agent.workspace.land",
                json!({
                    "workspace_id": workspace,
                    "session_id": session,
                    "operator": operator,
                    "confirm": true,
                }),
            )
            .await?
        }
        Command::WorkspaceArchive {
            workspace,
            retire,
            operator,
            reason,
        } => {
            let method = if retire {
                "agent.workspace.retire"
            } else {
                "agent.workspace.archive"
            };
            rpc(
                &discovery,
                method,
                json!({
                    "workspace_id": workspace,
                    "operator": operator,
                    "reason": reason,
                }),
            )
            .await?
        }
        Command::Inbox {
            identity,
            project,
            session,
            status,
            unread,
            limit,
        } => {
            if identity.is_none() && project.is_none() && session.is_none() {
                bail!("pass an identity, --project, or --session");
            }
            rpc(
                &discovery,
                "agent.inbox.list",
                json!({
                    "identity_id": identity,
                    "project_id": project,
                    "session_id": session,
                    "status": status,
                    "unread_only": unread,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::InboxSend {
            identity,
            project,
            session,
            from_identity,
            kind,
            body,
            payload,
            expires_in_secs,
            wake,
        } => {
            if body.is_some() && payload.is_some() {
                bail!("pass either --body or --payload-json, not both");
            }
            let payload = match payload {
                Some(raw) => {
                    let value: Value =
                        serde_json::from_str(&raw).context("--payload-json must be valid JSON")?;
                    if !value.is_object() {
                        bail!("--payload-json must be a JSON object");
                    }
                    Some(value)
                }
                None => None,
            };
            let project = match project {
                Some(project) => Some(project),
                None => {
                    let identity = rpc(
                        &discovery,
                        "agent.identities.get",
                        json!({"identity_id": identity}),
                    )
                    .await?;
                    identity
                        .get("identity")
                        .and_then(|identity| identity.get("project_id"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                }
            };
            rpc(
                &discovery,
                "agent.inbox.send",
                json!({
                    "project_id": project,
                    "identity_id": identity,
                    "session_id": session,
                    "from_identity_id": from_identity,
                    "kind": kind,
                    "body": body,
                    "payload": payload,
                    "expires_in_secs": expires_in_secs,
                    "wake_session": wake,
                }),
            )
            .await?
        }
        Command::InboxRead {
            message,
            project,
            identity,
        } => {
            rpc(
                &discovery,
                "agent.inbox.read",
                json!({
                    "message_id": message,
                    "project_id": project,
                    "identity_id": identity,
                }),
            )
            .await?
        }
        Command::InboxAck {
            message,
            project,
            identity,
        } => {
            rpc(
                &discovery,
                "agent.inbox.ack",
                json!({
                    "message_id": message,
                    "project_id": project,
                    "identity_id": identity,
                }),
            )
            .await?
        }
        Command::CoordinationReplay { project, repair } => {
            rpc(
                &discovery,
                "agent.coordination.replay",
                json!({"project_id": project, "repair": repair}),
            )
            .await?
        }
        Command::Effects {
            session,
            task,
            limit,
            unresolved,
        } => {
            rpc(
                &discovery,
                "session.effects",
                json!({
                    "session_id": session,
                    "task_id": task,
                    "limit": limit,
                    "include_settled": !unresolved,
                }),
            )
            .await?
        }
        Command::EffectReconcile {
            effect,
            session,
            done,
            failed,
            sha256,
            commit,
            external_id,
            http_status,
            note,
            operator,
        } => {
            if done == failed {
                bail!("pass exactly one of --done or --failed");
            }
            let decision = if done { "done" } else { "failed" };
            rpc(
                &discovery,
                "effect.reconcile",
                json!({
                    "session_id": session,
                    "effect_id": effect,
                    "decision": decision,
                    "evidence": {
                        "sha256": sha256,
                        "commit": commit,
                        "external_id": external_id,
                        "http_status": http_status,
                        "note": note,
                    },
                    "operator": operator,
                }),
            )
            .await?
        }
        Command::EffectPolicy { project } => {
            rpc(
                &discovery,
                "project.effect_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::EffectPolicySet {
            project,
            enabled,
            unknown_blocks_merge,
            stale_intent_grace_secs,
            shell_mode,
            track_mutating_http,
            track_read_http,
            track_mcp_mutations,
            track_workspace_writes,
        } => {
            let current = rpc(
                &discovery,
                "project.effect_policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            if let Some(enabled) = enabled {
                policy["enabled"] = json!(enabled);
            }
            if let Some(value) = unknown_blocks_merge {
                policy["unknown_blocks_merge"] = json!(value);
            }
            if let Some(value) = stale_intent_grace_secs {
                policy["stale_intent_grace_secs"] = json!(value);
            }
            if let Some(mode) = shell_mode {
                if vp_protocol::types::EffectShellMode::parse(&mode).is_none() {
                    bail!("shell mode must be `external_only` or `all`, got {mode}");
                }
                policy["shell_mode"] = json!(mode.to_ascii_lowercase());
            }
            if let Some(value) = track_mutating_http {
                policy["track_mutating_http"] = json!(value);
            }
            if let Some(value) = track_read_http {
                policy["track_read_http"] = json!(value);
            }
            if let Some(value) = track_mcp_mutations {
                policy["track_mcp_mutations"] = json!(value);
            }
            if let Some(value) = track_workspace_writes {
                policy["track_workspace_writes"] = json!(value);
            }
            rpc(
                &discovery,
                "project.effect_policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::EffectSweep { limit } => {
            rpc(&discovery, "effect.sweep", json!({"limit": limit})).await?
        }
        Command::Gate { command } => match command {
            GateCommand::Open {
                session,
                task,
                kind,
                label,
                payload,
                after_secs,
                at,
            } => {
                let payload = match payload {
                    Some(payload) => Some(serde_json::from_str::<Value>(&payload)?),
                    None => None,
                };
                rpc(
                    &discovery,
                    "gate.open",
                    json!({
                        "session_id": session,
                        "task_id": task,
                        "kind": kind,
                        "label": label,
                        "payload": payload,
                        "after_secs": after_secs,
                        "expires_at": at,
                    }),
                )
                .await?
            }
            GateCommand::Resolve {
                gate,
                token,
                token_stdin,
                token_file,
                outcome,
                note,
                operator,
            } => {
                let token = read_gate_token(token, token_stdin, token_file.as_deref())?;
                rpc(
                    &discovery,
                    "gate.resolve",
                    json!({
                        "gate_id": gate,
                        "token": token,
                        "outcome": outcome,
                        "note": note,
                        "resolved_by": operator,
                    }),
                )
                .await?
            }
            GateCommand::Cancel {
                gate,
                reason,
                operator,
            } => {
                rpc(
                    &discovery,
                    "gate.cancel",
                    json!({
                        "gate_id": gate,
                        "reason": reason,
                        "cancelled_by": operator,
                    }),
                )
                .await?
            }
            GateCommand::List {
                session,
                state,
                limit,
            } => {
                rpc(
                    &discovery,
                    "gate.list",
                    json!({"session_id": session, "state": state, "limit": limit}),
                )
                .await?
            }
            GateCommand::Status { gate } => {
                rpc(&discovery, "gate.status", json!({"gate_id": gate})).await?
            }
            GateCommand::Sweep { limit } => {
                rpc(&discovery, "gate.sweep", json!({"limit": limit})).await?
            }
            GateCommand::Policy { project } => {
                rpc(
                    &discovery,
                    "project.gate_policy.get",
                    json!({"project_id": project}),
                )
                .await?
            }
            GateCommand::PolicySet { project, loopback } => {
                rpc(
                    &discovery,
                    "project.gate_policy.set",
                    json!({"project_id": project, "policy": {"loopback_resolve": loopback}}),
                )
                .await?
            }
        },
        Command::McpPins { project, server } => {
            rpc(
                &discovery,
                "project.mcp_pins",
                json!({"project_id": project, "server_id": server}),
            )
            .await?
        }
        Command::McpPinApprove {
            project,
            server,
            tool,
            schema_hash,
            description_hash,
            approved_by,
        } => {
            rpc(
                &discovery,
                "mcp.pin.approve",
                json!({
                    "project_id": project,
                    "server_id": server,
                    "tool_name": tool,
                    "expected_schema_hash": schema_hash,
                    "expected_description_hash": description_hash,
                    "approved_by": approved_by,
                }),
            )
            .await?
        }
        Command::McpPinReject {
            project,
            server,
            tool,
            reason,
        } => {
            rpc(
                &discovery,
                "mcp.pin.reject",
                json!({
                    "project_id": project,
                    "server_id": server,
                    "tool_name": tool,
                    "reason": reason,
                }),
            )
            .await?
        }
        Command::Instructions { session, agent } => {
            rpc(
                &discovery,
                "session.instructions",
                json!({"session_id": session, "agent_id": agent}),
            )
            .await?
        }
        Command::PromptLayout { session, layout } => {
            if !matches!(layout, 2 | 3) {
                bail!("prompt layout must be 2 or 3, got {layout}");
            }
            rpc(
                &discovery,
                "session.prompt_layout.set",
                json!({"session_id": session, "prompt_layout": layout}),
            )
            .await?
        }
        Command::CacheAudit { session } => {
            rpc(
                &discovery,
                "session.cache.audit",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::CacheBreaks { session, limit } => {
            rpc(
                &discovery,
                "session.cache.breaks",
                json!({"session_id": session, "limit": limit}),
            )
            .await?
        }
        Command::CachePlan { session, role } => {
            rpc(
                &discovery,
                "session.cache.plan",
                json!({"session_id": session, "roles": role}),
            )
            .await?
        }
        Command::CacheWarm { session, role } => {
            rpc(
                &discovery,
                "session.cache.warm",
                json!({"session_id": session, "role": role}),
            )
            .await?
        }
        Command::ToolBundles {
            matrix,
            task_kind,
            diff,
            role,
            profile,
        } => {
            if let Some(diff) = diff {
                let Some((from_bundle, to_bundle)) = diff.split_once(',') else {
                    bail!("--diff expects FROM,TO");
                };
                rpc(
                    &discovery,
                    "tool_bundle.diff",
                    json!({
                        "from_bundle": from_bundle.trim(),
                        "to_bundle": to_bundle.trim(),
                        "role": role,
                        "profile": profile,
                        "task_kind": task_kind.first(),
                    }),
                )
                .await?
            } else {
                let method = if matrix {
                    "tool_bundle.matrix"
                } else {
                    "tool_bundle.catalog"
                };
                rpc(
                    &discovery,
                    method,
                    json!({"task_kinds": if matrix { task_kind } else { Vec::new() } }),
                )
                .await?
            }
        }
        Command::ToolBundle {
            session,
            role,
            task_kind,
            tools,
            history,
        } => {
            if history {
                rpc(
                    &discovery,
                    "session.tool_bundle.history",
                    json!({"session_id": session, "role": role.first()}),
                )
                .await?
            } else {
                rpc(
                    &discovery,
                    "session.tool_bundle",
                    json!({
                        "session_id": session,
                        "roles": role,
                        "task_kind": task_kind,
                        "include_tools": tools,
                    }),
                )
                .await?
            }
        }
        Command::CachePolicy { project } => {
            rpc(
                &discovery,
                "project.cache_policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::CachePolicySet {
            project,
            disabled,
            ttl,
            no_automatic,
            warm_on_resume,
            no_compact_aware,
            role_ttl,
            prompt_layout,
        } => {
            // Read-modify-write preserves role overrides that the operator did
            // not mention instead of silently clearing them.
            let existing = rpc(
                &discovery,
                "project.cache_policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut roles = existing["policy"]["role_ttl"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            for entry in role_ttl {
                let Some((role, value)) = entry.split_once('=') else {
                    bail!("--role-ttl must be ROLE=TTL, got '{entry}'");
                };
                if role.trim().is_empty() || value.trim().is_empty() {
                    bail!("--role-ttl must be ROLE=TTL, got '{entry}'");
                }
                roles.insert(
                    role.trim().to_string(),
                    Value::String(value.trim().to_string()),
                );
            }
            let prompt_layout = prompt_layout.unwrap_or_else(|| {
                existing["policy"]["prompt_layout"]
                    .as_u64()
                    .map(|value| value as u32)
                    .unwrap_or(3)
            });
            if !matches!(prompt_layout, 2 | 3) {
                bail!("--prompt-layout must be 2 or 3, got {prompt_layout}");
            }
            rpc(
                &discovery,
                "project.cache_policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "enabled": !disabled,
                        "ttl": ttl,
                        "automatic_caching": !no_automatic,
                        "warm_on_resume": warm_on_resume,
                        "compact_aware": !no_compact_aware,
                        "role_ttl": roles,
                        "prompt_layout": prompt_layout,
                    }
                }),
            )
            .await?
        }
        Command::Mailbox {
            session,
            agent,
            unread,
        } => {
            rpc(
                &discovery,
                "mailbox.list",
                json!({
                    "session_id": session,
                    "agent_id": agent,
                    "unread_only": unread,
                }),
            )
            .await?
        }
        Command::ReadMessage { message } => {
            rpc(&discovery, "mailbox.read", json!({"message_id": message})).await?
        }
        Command::Artifacts { session } => {
            rpc(&discovery, "artifact.list", json!({"session_id": session})).await?
        }
        Command::ArtifactVersions { artifact } => {
            rpc(
                &discovery,
                "artifact.versions",
                json!({"artifact_id": artifact}),
            )
            .await?
        }
        Command::ArtifactDiff {
            artifact,
            from_version,
            to_version,
            context,
        } => {
            rpc(
                &discovery,
                "artifact.diff",
                json!({
                    "artifact_id": artifact,
                    "from_version": from_version,
                    "to_version": to_version,
                    "context": context
                }),
            )
            .await?
        }
        Command::ArtifactRestore { artifact, version } => {
            rpc(
                &discovery,
                "artifact.restore",
                json!({"artifact_id": artifact, "version": version}),
            )
            .await?
        }
        Command::Artifact { artifact } => {
            let value = rpc(&discovery, "artifact.get", json!({"artifact_id": artifact})).await?;
            if value["artifact"]["storage"].as_str() == Some("file") {
                rpc(
                    &discovery,
                    "artifact.materialize",
                    json!({"artifact_id": artifact}),
                )
                .await?
            } else {
                value
            }
        }
        Command::ArtifactArchiveList {
            artifact,
            max_entries,
        } => {
            rpc(
                &discovery,
                "artifact.archive.list",
                json!({"artifact_id": artifact, "max_entries": max_entries}),
            )
            .await?
        }
        Command::ArtifactArchiveExtract {
            artifact,
            entries,
            all,
            max_entries,
            no_previews,
        } => {
            if !all && entries.is_empty() {
                bail!("artifact-archive-extract requires at least one --entry or --all");
            }
            rpc(
                &discovery,
                "artifact.archive.extract",
                json!({
                    "artifact_id": artifact,
                    "entries": entries,
                    "all": all,
                    "max_entries": max_entries,
                    "include_previews": !no_previews,
                }),
            )
            .await?
        }
        Command::MediaInfo { artifact } => {
            rpc(&discovery, "media.info", json!({"artifact_id": artifact})).await?
        }
        Command::MediaTranscode {
            artifact,
            format,
            max_duration_secs,
            max_bytes,
            yes,
        } => {
            if !yes {
                bail!(
                    "media-transcode is an explicit host operation; pass --yes after reviewing the source and bounds"
                );
            }
            rpc(
                &discovery,
                "media.transcode",
                json!({
                    "artifact_id": artifact,
                    "format": format,
                    "max_duration_secs": max_duration_secs,
                    "max_bytes": max_bytes,
                    "approved": true,
                }),
            )
            .await?
        }
        Command::BackfillArtifacts { session } => {
            rpc(
                &discovery,
                "artifact.backfill",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Memory {
            project,
            query,
            kind,
            limit,
            include_stale,
            status,
        } => {
            rpc(
                &discovery,
                "memory.list",
                json!({
                    "project_id": project,
                    "query": query,
                    "kind": kind,
                    "limit": limit,
                    "include_stale": include_stale,
                    "status": status
                }),
            )
            .await?
        }
        Command::Remember {
            session,
            kind,
            title,
            body,
            tags,
        } => {
            rpc(
                &discovery,
                "memory.create",
                json!({
                    "session_id": session,
                    "kind": kind,
                    "title": title,
                    "body": body,
                    "tags": tags
                }),
            )
            .await?
        }
        Command::MemoryDelete { memory } => {
            rpc(&discovery, "memory.delete", json!({"id": memory})).await?
        }
        Command::MemorySearch {
            project,
            query,
            task,
            department,
            paths,
            kinds,
            limit,
            include_stale,
            lexical_only,
        } => {
            rpc(
                &discovery,
                "memory.search",
                json!({
                    "project_id": project,
                    "query": query,
                    "task_id": task,
                    "department": department,
                    "paths": paths,
                    "kinds": kinds,
                    "limit": limit,
                    "include_stale": include_stale,
                    "lexical_only": lexical_only
                }),
            )
            .await?
        }
        Command::MemoryBrief { session, task } => {
            rpc(
                &discovery,
                "memory.brief",
                json!({"session_id": session, "task_id": task}),
            )
            .await?
        }
        Command::MemoryUpdate {
            memory,
            status,
            pin,
            unpin,
            tags,
        } => {
            if pin && unpin {
                bail!("--pin and --unpin cannot be combined");
            }
            let pinned = if pin {
                Some(true)
            } else if unpin {
                Some(false)
            } else {
                None
            };
            let tags = if tags.is_empty() { None } else { Some(tags) };
            if status.is_none() && pinned.is_none() && tags.is_none() {
                bail!("memory update requires --status, --pin/--unpin, or --tag");
            }
            rpc(
                &discovery,
                "memory.update",
                json!({
                    "id": memory,
                    "status": status,
                    "pinned": pinned,
                    "tags": tags
                }),
            )
            .await?
        }
        Command::MemoryVerify { memory } => {
            rpc(&discovery, "memory.verify", json!({"id": memory})).await?
        }
        Command::MemorySweep {
            project,
            days,
            dry_run,
        } => {
            rpc(
                &discovery,
                "memory.sweep",
                json!({
                    "project_id": project,
                    "older_than_days": days,
                    "dry_run": dry_run
                }),
            )
            .await?
        }
        Command::MemoryStats { project } => {
            rpc(&discovery, "memory.stats", json!({"project_id": project})).await?
        }
        Command::MemoryConflicts {
            project,
            status,
            limit,
        } => {
            rpc(
                &discovery,
                "memory.conflicts",
                json!({"project_id": project, "status": status, "limit": limit}),
            )
            .await?
        }
        Command::MemoryConflictScan { project } => {
            rpc(
                &discovery,
                "memory.conflict.scan",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::MemoryConflictResolve { id, action, note } => {
            let action = action.replace('-', "_");
            rpc(
                &discovery,
                "memory.conflict.resolve",
                json!({"id": id, "action": action, "note": note}),
            )
            .await?
        }
        Command::MemoryHealth { project } => {
            rpc(&discovery, "memory.health", json!({"project_id": project})).await?
        }
        Command::MemoryPrune {
            project,
            days,
            keep_latest,
            dry_run,
        } => {
            rpc(
                &discovery,
                "memory.prune",
                json!({
                    "project_id": project,
                    "older_than_days": days,
                    "keep_latest": keep_latest,
                    "dry_run": dry_run
                }),
            )
            .await?
        }
        Command::MemoryPolicy { project } => {
            rpc(
                &discovery,
                "memory.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::MemoryPolicySet {
            project,
            disable,
            max_items,
            max_age_days,
            clear,
            embedding_enable,
            embedding_disable,
            embedding_endpoint,
            embedding_model,
            embedding_dimensions,
            embedding_batch_size,
            embedding_timeout_secs,
            embedding_max_candidates,
            embedding_min_similarity,
            embedding_api_key_env,
            embedding_index,
            ivf_nlist,
            ivf_nprobe,
            ivf_min_vectors,
            ivf_rebuild_vectors,
            embedding_clear,
        } => {
            if embedding_enable && embedding_disable {
                bail!("--embedding-enable and --embedding-disable cannot be combined");
            }
            if embedding_clear
                && (embedding_enable
                    || embedding_disable
                    || embedding_endpoint.is_some()
                    || embedding_model.is_some()
                    || embedding_dimensions.is_some()
                    || embedding_batch_size.is_some()
                    || embedding_timeout_secs.is_some()
                    || embedding_max_candidates.is_some()
                    || embedding_min_similarity.is_some()
                    || embedding_api_key_env.is_some()
                    || embedding_index.is_some()
                    || ivf_nlist.is_some()
                    || ivf_nprobe.is_some()
                    || ivf_min_vectors.is_some()
                    || ivf_rebuild_vectors.is_some())
            {
                bail!("--embedding-clear cannot be combined with other embedding flags");
            }
            let (max_items, max_age_days) = if clear {
                (None, None)
            } else if max_items.is_none() && max_age_days.is_none() {
                (Some(1000), Some(180))
            } else {
                (max_items, max_age_days)
            };
            // Preserve v2 lifecycle settings that this command does not edit.
            let current = rpc(
                &discovery,
                "memory.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let policy = current.get("policy").cloned().unwrap_or_default();
            let current_embedding = policy.get("embedding").cloned().unwrap_or(Value::Null);
            let embedding_flags = embedding_enable
                || embedding_disable
                || embedding_endpoint.is_some()
                || embedding_model.is_some()
                || embedding_dimensions.is_some()
                || embedding_batch_size.is_some()
                || embedding_timeout_secs.is_some()
                || embedding_max_candidates.is_some()
                || embedding_min_similarity.is_some()
                || embedding_api_key_env.is_some()
                || embedding_index.is_some()
                || ivf_nlist.is_some()
                || ivf_nprobe.is_some()
                || ivf_min_vectors.is_some()
                || ivf_rebuild_vectors.is_some();
            let embedding = if embedding_clear {
                Value::Null
            } else if !embedding_flags {
                current_embedding
            } else {
                let mut object = if current_embedding.is_object() {
                    current_embedding
                } else {
                    json!({})
                };
                if object.get("enabled").is_none() {
                    object["enabled"] = json!(false);
                }
                if object.get("dimensions").is_none() {
                    object["dimensions"] = json!(1536);
                }
                if object.get("batch_size").is_none() {
                    object["batch_size"] = json!(16);
                }
                if object.get("timeout_secs").is_none() {
                    object["timeout_secs"] = json!(30);
                }
                if object.get("max_candidates").is_none() {
                    object["max_candidates"] = json!(64);
                }
                if object.get("min_similarity").is_none() {
                    object["min_similarity"] = json!(0.20);
                }
                if object.get("index").is_none() {
                    object["index"] = json!("scan");
                }
                if object.get("ivf_nlist").is_none() {
                    object["ivf_nlist"] = json!(64);
                }
                if object.get("ivf_nprobe").is_none() {
                    object["ivf_nprobe"] = json!(8);
                }
                if object.get("ivf_min_vectors").is_none() {
                    object["ivf_min_vectors"] = json!(256);
                }
                if object.get("ivf_rebuild_vectors").is_none() {
                    object["ivf_rebuild_vectors"] = json!(4096);
                }
                if embedding_enable {
                    object["enabled"] = json!(true);
                }
                if embedding_disable {
                    object["enabled"] = json!(false);
                }
                if let Some(endpoint) = embedding_endpoint {
                    object["endpoint"] = json!(endpoint);
                }
                if let Some(model) = embedding_model {
                    object["model"] = json!(model);
                }
                if let Some(dimensions) = embedding_dimensions {
                    object["dimensions"] = json!(dimensions);
                }
                if let Some(batch_size) = embedding_batch_size {
                    object["batch_size"] = json!(batch_size);
                }
                if let Some(timeout_secs) = embedding_timeout_secs {
                    object["timeout_secs"] = json!(timeout_secs);
                }
                if let Some(max_candidates) = embedding_max_candidates {
                    object["max_candidates"] = json!(max_candidates);
                }
                if let Some(min_similarity) = embedding_min_similarity {
                    object["min_similarity"] = json!(min_similarity);
                }
                if let Some(env_name) = embedding_api_key_env {
                    object["api_key_env"] = json!(env_name);
                }
                if let Some(index) = embedding_index {
                    object["index"] = json!(index);
                }
                if let Some(nlist) = ivf_nlist {
                    object["ivf_nlist"] = json!(nlist);
                }
                if let Some(nprobe) = ivf_nprobe {
                    object["ivf_nprobe"] = json!(nprobe);
                }
                if let Some(min_vectors) = ivf_min_vectors {
                    object["ivf_min_vectors"] = json!(min_vectors);
                }
                if let Some(rebuild_vectors) = ivf_rebuild_vectors {
                    object["ivf_rebuild_vectors"] = json!(rebuild_vectors);
                }
                object
            };
            rpc(
                &discovery,
                "memory.policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "enabled": !clear && !disable,
                        "max_items": max_items,
                        "max_age_days": max_age_days,
                        "task_result_verify_days": policy.get("task_result_verify_days").cloned().unwrap_or(json!(90)),
                        "default_verify_days": policy.get("default_verify_days").cloned().unwrap_or(json!(365)),
                        "auto_supersede": policy.get("auto_supersede").cloned().unwrap_or(json!(true)),
                        "brief_max_chars": policy.get("brief_max_chars").cloned().unwrap_or(json!(4000)),
                        "sweep_stale": policy.get("sweep_stale").cloned().unwrap_or(json!(false)),
                        "embedding": embedding
                    }
                }),
            )
            .await?
        }
        Command::MemoryEmbeddings { project } => {
            rpc(
                &discovery,
                "memory.embeddings.status",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ToolOutputPolicy { project } => {
            rpc(
                &discovery,
                "tool_output.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::MemoryEmbeddingsRebuild { project } => {
            rpc(
                &discovery,
                "memory.embeddings.rebuild",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::MemoryEmbeddingsTest { project, text } => {
            rpc(
                &discovery,
                "memory.embeddings.test",
                json!({"project_id": project, "text": text}),
            )
            .await?
        }
        Command::MemoryEmbeddingsIndex { project } => {
            rpc(
                &discovery,
                "memory.embeddings.index.status",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::MemoryEmbeddingsIndexRebuild { project } => {
            rpc(
                &discovery,
                "memory.embeddings.index.rebuild",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ToolOutputPolicySet {
            project,
            disable,
            enable,
            spill_threshold,
            preview_bytes,
            max_turn_bytes,
            retention_secs,
            max_entries,
            tool_thresholds,
        } => {
            if disable && enable {
                bail!("--disable and --enable are mutually exclusive");
            }
            let current = rpc(
                &discovery,
                "tool_output.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_default();
            if !policy.is_object() {
                policy = json!({});
            }
            if let Some(object) = policy.as_object_mut() {
                if disable {
                    object.insert("enabled".into(), json!(false));
                }
                if enable {
                    object.insert("enabled".into(), json!(true));
                }
                if let Some(value) = spill_threshold {
                    object.insert("spill_threshold_bytes".into(), json!(value));
                }
                if let Some(value) = preview_bytes {
                    object.insert("preview_bytes".into(), json!(value));
                }
                if let Some(value) = max_turn_bytes {
                    object.insert("max_total_bytes_per_turn".into(), json!(value));
                }
                if let Some(value) = retention_secs {
                    object.insert("retention_secs".into(), json!(value));
                }
                if let Some(value) = max_entries {
                    object.insert("max_entries".into(), json!(value));
                }
                if !tool_thresholds.is_empty() {
                    let mut thresholds = object
                        .get("tool_thresholds")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    for entry in parse_tool_thresholds(&tool_thresholds)? {
                        thresholds.insert(entry.0, json!(entry.1));
                    }
                    object.insert("tool_thresholds".into(), Value::Object(thresholds));
                }
            }
            rpc(
                &discovery,
                "tool_output.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::ToolOutputs {
            project,
            session,
            tool,
            before,
            include_expired,
            limit,
        } => {
            rpc(
                &discovery,
                "tool_output.list",
                json!({
                    "project_id": project,
                    "session_id": session,
                    "tool_name": tool,
                    "before_created_at": before,
                    "include_expired": include_expired,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::ToolOutputStatus { project, session } => {
            rpc(
                &discovery,
                "tool_output.status",
                json!({"project_id": project, "session_id": session}),
            )
            .await?
        }
        Command::ToolOutputPrune {
            project,
            session,
            older_than_secs,
            max_entries,
            dry_run,
        } => {
            rpc(
                &discovery,
                "tool_output.prune",
                json!({
                    "project_id": project,
                    "session_id": session,
                    "older_than_secs": older_than_secs,
                    "max_entries": max_entries,
                    "dry_run": dry_run,
                }),
            )
            .await?
        }
        Command::ToolOutputAudit {
            project,
            verify_hashes,
            clean_orphans,
        } => {
            rpc(
                &discovery,
                "tool_output.audit",
                json!({
                    "project_id": project,
                    "verify_hashes": verify_hashes,
                    "clean_orphans": clean_orphans,
                }),
            )
            .await?
        }
        Command::TestCache { project } => {
            rpc(
                &discovery,
                "test_cache.stats",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::TestCacheEntries {
            project,
            session,
            kind,
            status,
            include_expired,
            limit,
        } => {
            rpc(
                &discovery,
                "test_cache.entries",
                json!({
                    "project_id": project,
                    "session_id": session,
                    "kind": kind,
                    "status": status,
                    "include_expired": include_expired,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::TestCachePolicySet {
            project,
            enabled,
            max_entries,
            retention_days,
            max_output_bytes,
            reuse_lint,
            include_integration,
            failed_retention_secs,
            extra_env,
            extra_path,
            replace_extra_scopes,
        } => {
            let current = rpc(
                &discovery,
                "test_cache.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_default();
            if !policy.is_object() {
                policy = json!({});
            }
            if let Some(object) = policy.as_object_mut() {
                if let Some(value) = enabled {
                    object.insert("enabled".into(), json!(value));
                }
                if let Some(value) = max_entries {
                    object.insert("max_entries".into(), json!(value));
                }
                if let Some(value) = retention_days {
                    object.insert("retention_days".into(), json!(value));
                }
                if let Some(value) = max_output_bytes {
                    object.insert("max_output_bytes".into(), json!(value));
                }
                if let Some(value) = reuse_lint {
                    object.insert("reuse_lint".into(), json!(value));
                }
                if let Some(value) = include_integration {
                    object.insert("include_integration".into(), json!(value));
                }
                if let Some(value) = failed_retention_secs {
                    object.insert("failed_retention_secs".into(), json!(value));
                }
                if replace_extra_scopes {
                    object.insert("extra_env".into(), json!([]));
                    object.insert("extra_paths".into(), json!([]));
                }
                if !extra_env.is_empty() {
                    let mut values = object
                        .get("extra_env")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    values.extend(extra_env.iter().cloned().map(Value::String));
                    object.insert("extra_env".into(), json!(values));
                }
                if !extra_path.is_empty() {
                    let mut values = object
                        .get("extra_paths")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    values.extend(extra_path.iter().cloned().map(Value::String));
                    object.insert("extra_paths".into(), json!(values));
                }
            }
            rpc(
                &discovery,
                "test_cache.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::TestCacheScopes { project } => {
            rpc(
                &discovery,
                "test_cache.scope.preview",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::TestCachePrune {
            project,
            older_than_secs,
            expired_only,
            kind,
            dry_run,
        } => {
            rpc(
                &discovery,
                "test_cache.prune",
                json!({
                    "project_id": project,
                    "older_than_secs": older_than_secs,
                    "expired_only": expired_only,
                    "kind": kind,
                    "dry_run": dry_run,
                }),
            )
            .await?
        }
        Command::TestCacheClear {
            project,
            session,
            kind,
            yes,
        } => {
            if !yes {
                bail!("refusing to clear test-cache metadata without --yes");
            }
            rpc(
                &discovery,
                "test_cache.clear",
                json!({
                    "project_id": project,
                    "session_id": session,
                    "kind": kind,
                    "confirm": yes,
                }),
            )
            .await?
        }
        Command::TestImpact {
            session,
            paths,
            limit,
            no_history,
        } => {
            rpc(
                &discovery,
                "test_cache.impact",
                json!({
                    "session_id": session,
                    "changed_paths": paths,
                    "limit": limit,
                    "include_history": !no_history,
                }),
            )
            .await?
        }
        Command::ArtifactPolicy { project } => {
            rpc(
                &discovery,
                "artifact.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ArtifactPolicySet {
            project,
            auto_gc,
            max_bytes,
            max_age_secs,
            max_count,
        } => {
            rpc(
                &discovery,
                "artifact.policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "auto_gc": auto_gc,
                        "max_bytes": max_bytes,
                        "max_age_secs": max_age_secs,
                        "max_count": max_count,
                    }
                }),
            )
            .await?
        }
        Command::Gc { project } => {
            rpc(&discovery, "artifact.gc", json!({"project_id": project})).await?
        }
        Command::MailboxPolicy { project } => {
            rpc(
                &discovery,
                "mailbox.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::MailboxPolicySet {
            project,
            max_age_secs,
            max_count,
            delete_unread,
            auto_gc,
        } => {
            rpc(
                &discovery,
                "mailbox.policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "max_age_secs": max_age_secs,
                        "max_count": max_count,
                        "delete_unread": delete_unread,
                        "auto_gc": auto_gc,
                    }
                }),
            )
            .await?
        }
        Command::MailboxGc { project } => {
            rpc(&discovery, "mailbox.gc", json!({"project_id": project})).await?
        }
        Command::Queue { session } => {
            rpc(
                &discovery,
                "merge_queue.list",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Retry { entry } => {
            rpc(&discovery, "merge_queue.retry", json!({"entry_id": entry})).await?
        }
        Command::Resolve { entry } => {
            rpc(
                &discovery,
                "merge_queue.resolve",
                json!({"entry_id": entry}),
            )
            .await?
        }
        Command::Land { session } => {
            rpc(&discovery, "session.land", json!({"session_id": session})).await?
        }
        Command::Pr { session, provider } => {
            let mut params = json!({"session_id": session});
            if let Some(provider) = provider {
                params["provider"] = json!(provider.as_str());
            }
            rpc(&discovery, "session.pr", params).await?
        }
        Command::Forge { action } => match action {
            ForgeAction::Status { project } => {
                let project = match project {
                    Some(project) => Some(resolve_project(&discovery, Some(&project)).await?),
                    None => None,
                };
                rpc(&discovery, "forge.status", json!({"project_id": project})).await?
            }
            ForgeAction::Check { project, provider } => {
                let project = resolve_project(&discovery, project.as_deref()).await?;
                let mut params = json!({"project_id": project});
                if let Some(provider) = provider {
                    params["provider"] = json!(provider.as_str());
                }
                rpc(&discovery, "forge.preflight", params).await?
            }
        },
        Command::Export { session, output } => {
            let value = rpc(&discovery, "session.export", json!({"session_id": session})).await?;
            if let Some(path) = output {
                std::fs::write(&path, serde_json::to_string_pretty(&value)?)?;
                println!("exported {}", path.display());
                return Ok(None);
            }
            value
        }
        Command::Import { bundle } => {
            let data =
                std::fs::read(&bundle).with_context(|| format!("reading {}", bundle.display()))?;
            let parsed: Value = serde_json::from_slice(&data).context("parsing session bundle")?;
            rpc(&discovery, "session.import", json!({"bundle": parsed})).await?
        }
        Command::Rehydrate { session, worktree } => {
            rpc(
                &discovery,
                "session.rehydrate",
                json!({"session_id": session, "worktree": worktree}),
            )
            .await?
        }
        Command::LogStatus { session } => {
            rpc(
                &discovery,
                "session.log_status",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::RestoreLog { session } => {
            rpc(
                &discovery,
                "session.restore_log",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Replay {
            session,
            repair,
            rebuild,
        } => {
            rpc(
                &discovery,
                "session.replay",
                json!({"session_id": session, "repair": repair, "rebuild": rebuild}),
            )
            .await?
        }
        Command::Discard { session } => {
            rpc(
                &discovery,
                "session.discard",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::Commit { session, message } => {
            rpc(
                &discovery,
                "session.commit",
                json!({"session_id": session, "message": message}),
            )
            .await?
        }
        Command::Diff { session } => {
            rpc(&discovery, "session.diff", json!({"session_id": session})).await?
        }
        Command::Approvals { session, project } => {
            let project = match project {
                Some(project) => Some(resolve_project(&discovery, Some(&project)).await?),
                None => None,
            };
            rpc(
                &discovery,
                "approval.queue",
                json!({"session_id": session, "project_id": project}),
            )
            .await?
        }
        Command::ApprovalBatch { approve, deny } => {
            if approve.is_empty() && deny.is_empty() {
                bail!("approval-batch needs at least one --approve or --deny id");
            }
            if let Some(id) = approve.iter().find(|id| deny.contains(id)) {
                bail!("approval id '{id}' appears in both --approve and --deny");
            }
            let mut decisions = Vec::new();
            decisions.extend(
                approve
                    .into_iter()
                    .map(|id| json!({"approval_id": id, "approved": true})),
            );
            decisions.extend(
                deny.into_iter()
                    .map(|id| json!({"approval_id": id, "approved": false})),
            );
            rpc(
                &discovery,
                "approval.batch.resolve",
                json!({"decisions": decisions}),
            )
            .await?
        }
        Command::ApprovalTemplate { project } => {
            let project = resolve_project(&discovery, project.as_deref()).await?;
            rpc(
                &discovery,
                "approval.templates",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ApprovalTemplateSet {
            project,
            id,
            name,
            tool_pattern,
            risk,
            path_glob,
            action,
            expires_at,
            max_uses,
            disabled,
        } => {
            let project = resolve_project(&discovery, project.as_deref()).await?;
            if let Some(id) = id {
                let current = rpc(
                    &discovery,
                    "approval.templates",
                    json!({"project_id": project}),
                )
                .await?;
                let existing = current
                    .get("templates")
                    .and_then(Value::as_array)
                    .and_then(|templates| {
                        templates.iter().find(|template| {
                            template.get("id").and_then(Value::as_str) == Some(&id)
                        })
                    })
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("approval template not found: {id}"))?;
                let name = name.unwrap_or_else(|| {
                    existing
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("template")
                        .to_string()
                });
                let tool_pattern = tool_pattern.unwrap_or_else(|| {
                    existing
                        .get("tool_pattern")
                        .and_then(Value::as_str)
                        .unwrap_or("write_file")
                        .to_string()
                });
                let action = action.unwrap_or_else(|| {
                    existing
                        .get("action")
                        .and_then(Value::as_str)
                        .unwrap_or("allow")
                        .to_string()
                });
                let risk = if risk.is_empty() {
                    existing
                        .get("risk_levels")
                        .and_then(Value::as_array)
                        .map(|levels| {
                            levels
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                } else {
                    risk
                };
                let path_glob = if path_glob.is_empty() {
                    existing
                        .get("path_globs")
                        .and_then(Value::as_array)
                        .map(|globs| {
                            globs
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                } else {
                    path_glob
                };
                rpc(
                    &discovery,
                    "approval.template.update",
                    json!({
                        "id": id,
                        "name": name,
                        "enabled": !disabled,
                        "tool_pattern": tool_pattern,
                        "risk_levels": risk,
                        "path_globs": path_glob,
                        "action": action,
                        "expires_at": expires_at.or_else(|| {
                            existing.get("expires_at").cloned().and_then(|value| value.as_str().map(str::to_string))
                        }),
                        "max_uses": max_uses.or_else(|| {
                            existing.get("max_uses").and_then(Value::as_u64).map(|value| value as u32)
                        }),
                    }),
                )
                .await?
            } else {
                let name = name.ok_or_else(|| anyhow::anyhow!("--name is required"))?;
                let tool_pattern =
                    tool_pattern.ok_or_else(|| anyhow::anyhow!("--tool-pattern is required"))?;
                let action = action.ok_or_else(|| anyhow::anyhow!("--action is required"))?;
                rpc(
                    &discovery,
                    "approval.template.create",
                    json!({
                        "project_id": project,
                        "name": name,
                        "tool_pattern": tool_pattern,
                        "risk_levels": risk,
                        "path_globs": path_glob,
                        "action": action,
                        "expires_at": expires_at,
                        "max_uses": max_uses,
                    }),
                )
                .await?
            }
        }
        Command::ApprovalTemplateDelete { id } => {
            rpc(&discovery, "approval.template.delete", json!({"id": id})).await?
        }
        Command::ApprovalPolicy { project } => {
            rpc(
                &discovery,
                "approval.policy",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ApprovalPolicySet {
            project,
            enabled,
            max_pending,
            coalesce_secs,
            stale_secs,
            priority_boost_risk,
            notify_coalesce_secs,
        } => {
            let current = rpc(
                &discovery,
                "approval.policy",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_else(|| json!({}));
            if let Some(enabled) = enabled {
                policy["enabled"] = json!(enabled);
            }
            if let Some(max_pending) = max_pending {
                policy["max_pending_per_session"] = json!(max_pending);
            }
            if let Some(coalesce_secs) = coalesce_secs {
                policy["coalesce_window_secs"] = json!(coalesce_secs);
            }
            if let Some(stale_secs) = stale_secs {
                policy["stale_after_secs"] = json!(stale_secs);
            }
            if let Some(priority_boost_risk) = priority_boost_risk {
                policy["priority_boost_risk"] = json!(priority_boost_risk);
            }
            if let Some(notify_coalesce_secs) = notify_coalesce_secs {
                policy["notify_coalesce_secs"] = json!(notify_coalesce_secs);
            }
            rpc(
                &discovery,
                "approval.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::ApprovalAudit { project, limit } => {
            let project = resolve_project(&discovery, Some(&project)).await?;
            rpc(
                &discovery,
                "approval.audit",
                json!({"project_id": project, "limit": limit}),
            )
            .await?
        }
        Command::Questions { session } => {
            rpc(&discovery, "question.list", json!({"session_id": session})).await?
        }
        Command::Answer { question, answer } => {
            rpc(
                &discovery,
                "question.answer",
                json!({"question_id": question, "answer": answer}),
            )
            .await?
        }
        Command::Approve { approval } => {
            rpc(
                &discovery,
                "approval.resolve",
                json!({"approval_id": approval, "approved": true}),
            )
            .await?
        }
        Command::Deny { approval } => {
            rpc(
                &discovery,
                "approval.resolve",
                json!({"approval_id": approval, "approved": false}),
            )
            .await?
        }
    };
    Ok(Some(value))
}
