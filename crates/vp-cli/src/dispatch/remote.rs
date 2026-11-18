//! Remote workspace host and execution commands.

use super::*;

pub(super) async fn dispatch(
    command: Command,
    discovery: Discovery,
    _json: bool,
) -> Result<Option<Value>> {
    let value = match command {
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
        _ => unreachable!("command routed to the wrong dispatch family"),
    };
    Ok(Some(value))
}
