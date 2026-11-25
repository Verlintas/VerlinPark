use super::*;

impl Engine {
    pub async fn list_mcp_servers(&self) -> Result<McpServerListResult> {
        let store = self.store.clone();
        let servers = tokio::task::spawn_blocking(move || store.list_mcp_servers()).await??;
        Ok(McpServerListResult { servers })
    }

    pub async fn probe_mcp_server(&self, server_id: &str) -> Result<McpProbeResult> {
        let store = self.store.clone();
        let id = server_id.to_string();
        let server = tokio::task::spawn_blocking(move || store.get_mcp_server(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("MCP server not found: {server_id}"))?;
        self.mcp.probe(&server).await
    }

    pub async fn add_mcp_server(&self, config: McpServerConfig) -> Result<McpServer> {
        let config = crate::mcp::normalize_config(config)?;
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.add_mcp_server(&config)).await?
    }

    pub async fn update_mcp_server(
        &self,
        id: &str,
        config: McpServerConfig,
    ) -> Result<Option<McpServer>> {
        let config = crate::mcp::normalize_config(config)?;
        let store = self.store.clone();
        let id = id.to_string();
        let updated =
            tokio::task::spawn_blocking(move || store.update_mcp_server(&id, &config)).await??;
        if let Some(server) = &updated {
            self.mcp.invalidate(&server.id).await;
        }
        Ok(updated)
    }

    pub async fn remove_mcp_server(&self, id: &str) -> Result<bool> {
        let store = self.store.clone();
        let id = id.to_string();
        let lookup = id.clone();
        let removed =
            tokio::task::spawn_blocking(move || store.remove_mcp_server(&lookup)).await??;
        self.mcp.invalidate(&id).await;
        Ok(removed)
    }

    pub async fn list_acp_agents(&self) -> Result<AcpAgentListResult> {
        let store = self.store.clone();
        let agents = tokio::task::spawn_blocking(move || store.list_acp_agents()).await??;
        Ok(AcpAgentListResult { agents })
    }

    /// Resolve an ACP agent by id or by its unique display name. Stored rows
    /// are revalidated before use so a pre-hardening configuration cannot
    /// bypass the cleartext/credential checks.
    pub async fn resolve_acp_agent(&self, identifier: &str) -> Result<Option<AcpAgent>> {
        let store = self.store.clone();
        let identifier = identifier.to_string();
        let agent =
            tokio::task::spawn_blocking(move || store.find_acp_agent(&identifier)).await??;
        if let Some(agent) = &agent {
            validate_acp_agent_config(AcpAgentConfig {
                name: agent.name.clone(),
                command: agent.command.clone(),
                args: agent.args.clone(),
                env: agent.env.clone(),
                enabled: agent.enabled,
                auto_approve: agent.auto_approve,
                timeout_secs: agent.timeout_secs,
                transport: agent.transport,
                endpoint: agent.endpoint.clone(),
                auth_header_env: agent.auth_header_env.clone(),
                headers: agent.headers.clone(),
                connect_timeout_secs: agent.connect_timeout_secs,
                allow_remote: agent.allow_remote,
            })?;
        }
        Ok(agent)
    }

    /// Add an ACP agent using the full transport-aware configuration.
    pub async fn add_acp_agent_config(&self, config: AcpAgentConfig) -> Result<AcpAgent> {
        let config = validate_acp_agent_config(config)?;
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.add_acp_agent_config(&config)).await?
    }

    /// Backward-compatible stdio-only add used by older internal callers.
    #[allow(clippy::too_many_arguments)]
    pub async fn add_acp_agent(
        &self,
        name: &str,
        command: &str,
        args: Vec<String>,
        env: std::collections::BTreeMap<String, String>,
        enabled: bool,
        auto_approve: bool,
        timeout_secs: u64,
    ) -> Result<AcpAgent> {
        self.add_acp_agent_config(AcpAgentConfig {
            name: name.to_string(),
            command: command.to_string(),
            args,
            env,
            enabled,
            auto_approve,
            timeout_secs,
            transport: AcpTransport::Stdio,
            endpoint: None,
            auth_header_env: None,
            headers: Default::default(),
            connect_timeout_secs: 30,
            allow_remote: false,
        })
        .await
    }

    /// Update an ACP agent using the full transport-aware configuration.
    pub async fn update_acp_agent_config(
        &self,
        id: &str,
        config: AcpAgentConfig,
    ) -> Result<Option<AcpAgent>> {
        let config = validate_acp_agent_config(config)?;
        let store = self.store.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || store.update_acp_agent_config(&id, &config)).await?
    }

    /// Backward-compatible stdio-only update used by older internal callers.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_acp_agent(
        &self,
        id: &str,
        name: &str,
        command: &str,
        args: Vec<String>,
        env: std::collections::BTreeMap<String, String>,
        enabled: bool,
        auto_approve: bool,
        timeout_secs: u64,
    ) -> Result<Option<AcpAgent>> {
        self.update_acp_agent_config(
            id,
            AcpAgentConfig {
                name: name.to_string(),
                command: command.to_string(),
                args,
                env,
                enabled,
                auto_approve,
                timeout_secs,
                transport: AcpTransport::Stdio,
                endpoint: None,
                auth_header_env: None,
                headers: Default::default(),
                connect_timeout_secs: 30,
                allow_remote: false,
            },
        )
        .await
    }

    pub async fn remove_acp_agent(&self, id: &str) -> Result<bool> {
        let store = self.store.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || store.remove_acp_agent(&id)).await?
    }

    /// Spawn an external ACP agent, initialize it, and create a session without
    /// sending a prompt. This validates the executable and protocol handshake.
    pub async fn probe_acp_agent(
        &self,
        agent_id: &str,
        project_id: Option<String>,
    ) -> Result<AcpProbeResult> {
        self.probe_acp_agent_options(agent_id, project_id, true)
            .await
    }

    /// Probe an agent, optionally skipping session creation.
    pub async fn probe_acp_agent_options(
        &self,
        agent_id: &str,
        project_id: Option<String>,
        create_session: bool,
    ) -> Result<AcpProbeResult> {
        let agent = self
            .resolve_acp_agent(agent_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("ACP agent not found: {agent_id}"))?;
        let store = self.store.clone();
        let pid = project_id.clone();
        let project = tokio::task::spawn_blocking(move || match pid {
            Some(project_id) => store.get_project(&project_id),
            None => Ok(None),
        })
        .await??;
        let policy = match &project {
            Some(project) => self.get_shell_policy(&project.id).await?,
            None => ShellPolicy::default(),
        };
        let mut secret_roots = crate::agent::shell_secret_roots(self.store.data_dir());
        secret_roots.extend(policy.read_deny.iter().map(PathBuf::from));
        let store = self.store.clone();
        let mcp_servers = tokio::task::spawn_blocking(move || store.list_mcp_servers())
            .await??
            .into_iter()
            .filter(|server| server.enabled)
            .map(|server| acp::mcp_server(&server))
            .collect::<Result<Vec<_>>>()?;
        // Probe in an isolated scratch directory. The agent is initialized and
        // gets a session, but must not be able to write into the project, the
        // project's Git metadata, or the daemon data directory.
        // A unique probe directory avoids sharing HOME/cwd between
        // concurrent probes and lets us remove all probe state afterwards.
        let probe_root = self
            .store
            .data_dir()
            .join("sandbox")
            .join("acp-probe")
            .join(&agent.id)
            .join(Uuid::now_v7().to_string());
        let cwd = probe_root.join("cwd");
        tokio::fs::create_dir_all(&cwd).await?;
        let launch = acp::AcpLaunch {
            name: agent.name.clone(),
            command: agent.command.clone(),
            args: agent.args.clone(),
            env: agent.env.clone(),
            cwd: cwd.clone(),
            project_root: cwd.clone(),
            sandbox_root: probe_root.join("scratch"),
            secret_roots,
            network_allowed: policy.network != "deny",
            home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default(),
            restrict_home: policy.restrict_home,
            read_jail: policy.read_jail,
            git_branch: None,
            mcp_servers,
            timeout: std::time::Duration::from_secs(agent.timeout_secs.min(120)),
            transport: agent.transport,
            endpoint: agent.endpoint.clone(),
            auth_header_env: agent.auth_header_env.clone(),
            headers: agent.headers.clone(),
            connect_timeout: std::time::Duration::from_secs(agent.connect_timeout_secs),
            allow_remote: agent.allow_remote,
            resume_session_id: None,
            diagnostics: None,
        };
        let result = acp::probe_with_options(launch, create_session).await;
        if let Err(error) = tokio::fs::remove_dir_all(&probe_root).await {
            tracing::warn!(%error, path = %probe_root.display(), "failed to remove ACP probe scratch");
        }
        let result = result?;
        Ok(AcpProbeResult {
            agent_id: agent.id,
            agent_name: result.agent_name,
            version: result.agent_version,
            protocol_version: result.protocol_version,
            session_created: result.session_created,
            transport: agent.transport,
            endpoint: agent.endpoint,
            remote: agent.transport.is_remote(),
            load_session: result.load_session,
            capabilities: result.capabilities,
            http_version: result.http_version,
            messages_sent: result.messages_sent,
            messages_received: result.messages_received,
            streams_opened: result.streams_opened,
        })
    }

    pub async fn mcp_tools(&self, server_id: &str) -> Result<McpToolListResult> {
        let store = self.store.clone();
        let id = server_id.to_string();
        let server = tokio::task::spawn_blocking(move || store.get_mcp_server(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("MCP server not found: {server_id}"))?;
        let tools = self.mcp.list_tools(&server).await?;
        Ok(McpToolListResult {
            server_id: server_id.to_string(),
            tools,
        })
    }

    pub async fn mcp_call(
        &self,
        server_id: &str,
        tool: &str,
        arguments: serde_json::Value,
    ) -> Result<McpCallResult> {
        self.mcp_call_with_context(
            server_id,
            tool,
            arguments,
            crate::mcp::tasks::McpTaskContext::default(),
        )
        .await
    }

    pub async fn mcp_resources(&self, server_id: &str) -> Result<McpResourceListResult> {
        let store = self.store.clone();
        let id = server_id.to_string();
        let server = tokio::task::spawn_blocking(move || store.get_mcp_server(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("MCP server not found: {server_id}"))?;
        self.mcp.list_resources(&server).await
    }

    pub async fn mcp_read_resource(
        &self,
        server_id: &str,
        uri: &str,
    ) -> Result<McpReadResourceResult> {
        let store = self.store.clone();
        let id = server_id.to_string();
        let server = tokio::task::spawn_blocking(move || store.get_mcp_server(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("MCP server not found: {server_id}"))?;
        self.mcp.read_resource(&server, uri).await
    }

    pub async fn mcp_prompts(&self, server_id: &str) -> Result<McpPromptListResult> {
        let store = self.store.clone();
        let id = server_id.to_string();
        let server = tokio::task::spawn_blocking(move || store.get_mcp_server(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("MCP server not found: {server_id}"))?;
        self.mcp.list_prompts(&server).await
    }

    pub async fn mcp_prompt(
        &self,
        server_id: &str,
        name: &str,
        arguments: std::collections::BTreeMap<String, String>,
    ) -> Result<McpGetPromptResult> {
        let store = self.store.clone();
        let id = server_id.to_string();
        let server = tokio::task::spawn_blocking(move || store.get_mcp_server(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("MCP server not found: {server_id}"))?;
        self.mcp.get_prompt(&server, name, arguments).await
    }

    pub async fn sync_mcp_pins(
        &self,
        session_id: &str,
        project_id: &str,
        server: &McpServer,
        tools: &[vp_protocol::types::McpTool],
    ) -> Result<Vec<McpToolPin>> {
        let _pin_guard = self.pin_sync.lock().await;
        let store = self.store.clone();
        let project = project_id.to_string();
        let server_id = server.id.clone();
        let existing = tokio::task::spawn_blocking({
            let store = store.clone();
            let project = project.clone();
            let server_id = server_id.clone();
            move || store.list_mcp_tool_pins(&project, Some(&server_id))
        })
        .await??;
        let now = vp_protocol::now_rfc3339();
        let result = crate::mcp_pins::sync_pins(project_id, server, tools, &existing, &now);
        // Pin lifecycle events are appended before the project rows are
        // updated. The rows are project configuration, not a session replay
        // projection, so a crash after the event leaves a stale row that the
        // next sync repairs and re-emits.
        for change in &result.changes {
            let payload = crate::mcp_pins::change_payload(change, result.first_sync);
            self.append_event(session_id, change.kind.event_kind(), payload)
                .await?;
        }
        for pin in &result.pins {
            let store = store.clone();
            let pin = pin.clone();
            tokio::task::spawn_blocking(move || store.upsert_mcp_tool_pin(&pin)).await??;
        }
        Ok(result.pins)
    }

    pub async fn project_mcp_pins(
        &self,
        project_id: &str,
        server_id: Option<&str>,
    ) -> Result<ProjectMcpPinsResult> {
        let store = self.store.clone();
        let project = project_id.to_string();
        let server = server_id.map(str::to_string);
        let pins = tokio::task::spawn_blocking(move || {
            store.list_mcp_tool_pins(&project, server.as_deref())
        })
        .await??;
        let mut result = ProjectMcpPinsResult {
            project_id: project_id.to_string(),
            pins,
            ..ProjectMcpPinsResult::default()
        };
        for pin in &result.pins {
            match pin.status.as_str() {
                "pinned" => result.pinned += 1,
                "drift" => result.drift += 1,
                "unverified" => result.unverified += 1,
                "removed" => result.removed += 1,
                _ => {}
            }
        }
        Ok(result)
    }

    pub async fn approve_mcp_pin(
        &self,
        params: &McpPinApproveParams,
        session_id: Option<&str>,
    ) -> Result<McpPinActionResult> {
        let store = self.store.clone();
        let project = params.project_id.clone();
        let server = params.server_id.clone();
        let pins =
            tokio::task::spawn_blocking(move || store.list_mcp_tool_pins(&project, Some(&server)))
                .await??;
        let mut pin = pins
            .into_iter()
            .find(|pin| pin.tool_name == params.tool_name)
            .ok_or_else(|| anyhow::anyhow!("MCP tool pin not found"))?;
        if pin.observed_schema_hash != params.expected_schema_hash
            || pin.observed_description_hash != params.expected_description_hash
        {
            anyhow::bail!(
                "expected pin hashes do not match the last observation; rediscover the tool and retry"
            );
        }
        let now = vp_protocol::now_rfc3339();
        pin.status = "pinned".to_string();
        pin.schema_hash = params.expected_schema_hash.clone();
        pin.description_hash = params.expected_description_hash.clone();
        pin.server_fingerprint = pin.observed_server_fingerprint.clone();
        pin.approved_at = Some(now.clone());
        pin.approved_by = Some(
            params
                .approved_by
                .clone()
                .unwrap_or_else(|| "operator".to_string()),
        );
        pin.last_seen_at = now;
        if let Some(session_id) = self
            .resolve_pin_audit_session(session_id, &params.project_id)
            .await?
        {
            self.append_event(
                &session_id,
                "mcp.tool.approved",
                serde_json::json!({
                    "project_id": pin.project_id,
                    "server_id": pin.server_id,
                    "server_name": pin.server_name,
                    "tool_name": pin.tool_name,
                    "schema_hash": pin.schema_hash,
                    "description_hash": pin.description_hash,
                    "approved_by": pin.approved_by,
                    "approved_at": pin.approved_at,
                }),
            )
            .await?;
        }
        let store = self.store.clone();
        let for_store = pin.clone();
        tokio::task::spawn_blocking(move || store.upsert_mcp_tool_pin(&for_store)).await??;
        Ok(McpPinActionResult {
            pin: Some(pin),
            status: "pinned".to_string(),
            message: "MCP tool pin approved".to_string(),
        })
    }

    pub async fn reject_mcp_pin(
        &self,
        params: &McpPinRejectParams,
        session_id: Option<&str>,
    ) -> Result<McpPinActionResult> {
        let store = self.store.clone();
        let project = params.project_id.clone();
        let server = params.server_id.clone();
        let pins =
            tokio::task::spawn_blocking(move || store.list_mcp_tool_pins(&project, Some(&server)))
                .await??;
        let pin = pins
            .into_iter()
            .find(|pin| pin.tool_name == params.tool_name)
            .ok_or_else(|| anyhow::anyhow!("MCP tool pin not found"))?;
        if let Some(session_id) = self
            .resolve_pin_audit_session(session_id, &params.project_id)
            .await?
        {
            self.append_event(
                &session_id,
                "mcp.tool.rejected",
                serde_json::json!({
                    "project_id": pin.project_id,
                    "server_id": pin.server_id,
                    "server_name": pin.server_name,
                    "tool_name": pin.tool_name,
                    "status": pin.status,
                    "schema_hash": pin.schema_hash,
                    "description_hash": pin.description_hash,
                    "reason": params.reason.as_deref().unwrap_or("operator rejected the observed pin"),
                }),
            )
            .await?;
        }
        Ok(McpPinActionResult {
            status: pin.status.clone(),
            message: "MCP tool pin was not approved; the observed status remains visible"
                .to_string(),
            pin: Some(pin),
        })
    }

    async fn resolve_pin_audit_session(
        &self,
        session_id: Option<&str>,
        project_id: &str,
    ) -> Result<Option<String>> {
        if let Some(session_id) = session_id {
            return Ok(Some(session_id.to_string()));
        }
        let store = self.store.clone();
        let project = project_id.to_string();
        tokio::task::spawn_blocking(move || store.latest_session_for_project(&project)).await?
    }

    pub async fn mcp_pin_trust_for_tool(
        &self,
        project_id: &str,
        server_id: &str,
        tool_name: &str,
    ) -> Result<crate::contract::McpPinTrust> {
        let store = self.store.clone();
        let project = project_id.to_string();
        let server = server_id.to_string();
        let pins =
            tokio::task::spawn_blocking(move || store.list_mcp_tool_pins(&project, Some(&server)))
                .await??;
        let Some(pin) = pins.into_iter().find(|pin| pin.tool_name == tool_name) else {
            return Ok(crate::contract::McpPinTrust::Unverified);
        };
        Ok(match pin.status.as_str() {
            "pinned" if crate::mcp_pins::pin_is_callable(&pin) => {
                crate::contract::McpPinTrust::Trusted
            }
            "pinned" => crate::contract::McpPinTrust::Drift,
            "drift" => crate::contract::McpPinTrust::Drift,
            "removed" => crate::contract::McpPinTrust::Removed,
            _ => crate::contract::McpPinTrust::Unverified,
        })
    }

    pub async fn find_mcp_server(&self, selector: &str) -> Result<Option<McpServer>> {
        let servers = self.list_mcp_servers().await?.servers;
        Ok(servers
            .into_iter()
            .find(|server| server.id == selector || server.name == selector))
    }
}
