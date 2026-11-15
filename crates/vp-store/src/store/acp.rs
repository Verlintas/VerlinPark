//! ACP agent configuration and model presets.

use super::*;

impl Store {
    const ACP_AGENT_COLUMNS: &'static str = "id, name, command, args, env, enabled, auto_approve, \
         timeout_secs, created_at, transport, endpoint, auth_header_env, headers, \
         connect_timeout_secs, allow_remote";

    pub fn list_acp_agents(&self) -> Result<Vec<AcpAgent>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM acp_agents ORDER BY name ASC",
            Self::ACP_AGENT_COLUMNS
        ))?;
        let rows = stmt.query_map([], map_acp_agent)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_acp_agent(&self, agent_id: &str) -> Result<Option<AcpAgent>> {
        let conn = self.lock()?;
        conn.query_row(
            &format!(
                "SELECT {} FROM acp_agents WHERE id = ?1",
                Self::ACP_AGENT_COLUMNS
            ),
            params![agent_id],
            map_acp_agent,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn find_acp_agent(&self, identifier: &str) -> Result<Option<AcpAgent>> {
        let conn = self.lock()?;
        conn.query_row(
            &format!(
                "SELECT {} FROM acp_agents WHERE id = ?1 OR name = ?1",
                Self::ACP_AGENT_COLUMNS
            ),
            params![identifier],
            map_acp_agent,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Insert an ACP agent with the full transport-aware configuration.
    pub fn add_acp_agent_config(&self, config: &AcpAgentConfig) -> Result<AcpAgent> {
        let agent = AcpAgent {
            id: Uuid::now_v7().to_string(),
            name: config.name.clone(),
            command: config.command.clone(),
            args: config.args.clone(),
            env: config.env.clone(),
            enabled: config.enabled,
            auto_approve: config.auto_approve,
            timeout_secs: config.timeout_secs,
            transport: config.transport,
            endpoint: config.endpoint.clone(),
            auth_header_env: config.auth_header_env.clone(),
            headers: config.headers.clone(),
            connect_timeout_secs: config.connect_timeout_secs,
            allow_remote: config.allow_remote,
            created_at: now_rfc3339(),
        };
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO acp_agents
             (id, name, command, args, env, enabled, auto_approve, timeout_secs, created_at,
              transport, endpoint, auth_header_env, headers, connect_timeout_secs, allow_remote)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                agent.id,
                agent.name,
                agent.command,
                serde_json::to_string(&agent.args)?,
                serde_json::to_string(&agent.env)?,
                if agent.enabled { 1 } else { 0 },
                if agent.auto_approve { 1 } else { 0 },
                agent.timeout_secs as i64,
                agent.created_at,
                agent.transport.as_str(),
                agent.endpoint,
                agent.auth_header_env,
                serde_json::to_string(&agent.headers)?,
                agent.connect_timeout_secs as i64,
                if agent.allow_remote { 1 } else { 0 }
            ],
        )?;
        Ok(agent)
    }

    /// Backward-compatible stdio-only insert used by older internal callers.
    #[allow(clippy::too_many_arguments)]
    pub fn add_acp_agent(
        &self,
        name: &str,
        command: &str,
        args: &[String],
        env: &std::collections::BTreeMap<String, String>,
        enabled: bool,
        auto_approve: bool,
        timeout_secs: u64,
    ) -> Result<AcpAgent> {
        self.add_acp_agent_config(&AcpAgentConfig {
            name: name.to_string(),
            command: command.to_string(),
            args: args.to_vec(),
            env: env.clone(),
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
    }

    /// Update every transport-aware field of an ACP agent.
    pub fn update_acp_agent_config(
        &self,
        agent_id: &str,
        config: &AcpAgentConfig,
    ) -> Result<Option<AcpAgent>> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE acp_agents SET name = ?1, command = ?2, args = ?3, env = ?4,
                enabled = ?5, auto_approve = ?6, timeout_secs = ?7, transport = ?8,
                endpoint = ?9, auth_header_env = ?10, headers = ?11,
                connect_timeout_secs = ?12, allow_remote = ?13
             WHERE id = ?14",
            params![
                config.name,
                config.command,
                serde_json::to_string(&config.args)?,
                serde_json::to_string(&config.env)?,
                if config.enabled { 1 } else { 0 },
                if config.auto_approve { 1 } else { 0 },
                config.timeout_secs as i64,
                config.transport.as_str(),
                config.endpoint,
                config.auth_header_env,
                serde_json::to_string(&config.headers)?,
                config.connect_timeout_secs as i64,
                if config.allow_remote { 1 } else { 0 },
                agent_id
            ],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get_acp_agent(agent_id)
    }

    /// Backward-compatible stdio-only update used by older internal callers.
    #[allow(clippy::too_many_arguments)]
    pub fn update_acp_agent(
        &self,
        agent_id: &str,
        name: &str,
        command: &str,
        args: &[String],
        env: &std::collections::BTreeMap<String, String>,
        enabled: bool,
        auto_approve: bool,
        timeout_secs: u64,
    ) -> Result<Option<AcpAgent>> {
        self.update_acp_agent_config(
            agent_id,
            &AcpAgentConfig {
                name: name.to_string(),
                command: command.to_string(),
                args: args.to_vec(),
                env: env.clone(),
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
    }

    pub fn remove_acp_agent(&self, agent_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        Ok(conn.execute("DELETE FROM acp_agents WHERE id = ?1", params![agent_id])? > 0)
    }

    pub fn list_model_presets(&self) -> Result<Vec<ModelPreset>> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT id, name, provider, model, context_window,
                    input_usd_per_million, output_usd_per_million, notes, created_at
             FROM model_presets ORDER BY name COLLATE NOCASE, created_at",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(ModelPreset {
                id: row.get(0)?,
                name: row.get(1)?,
                provider: row.get(2)?,
                model: row.get(3)?,
                context_window: row.get::<_, Option<i64>>(4)?.map(|value| value as u64),
                input_usd_per_million: row.get(5)?,
                output_usd_per_million: row.get(6)?,
                notes: row.get(7)?,
                created_at: row.get(8)?,
            })
        })?;
        let mut presets = Vec::new();
        for preset in rows {
            presets.push(preset?);
        }
        Ok(presets)
    }

    pub fn create_model_preset(&self, preset: &ModelPreset) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO model_presets
             (id, name, provider, model, context_window, input_usd_per_million,
              output_usd_per_million, notes, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                preset.id,
                preset.name,
                preset.provider,
                preset.model,
                preset.context_window.map(|value| value as i64),
                preset.input_usd_per_million,
                preset.output_usd_per_million,
                preset.notes,
                preset.created_at
            ],
        )?;
        Ok(())
    }

    pub fn update_model_preset(&self, preset: &ModelPreset) -> Result<bool> {
        let conn = self.lock()?;
        Ok(conn.execute(
            "UPDATE model_presets SET
                name = ?2, provider = ?3, model = ?4, context_window = ?5,
                input_usd_per_million = ?6, output_usd_per_million = ?7, notes = ?8
             WHERE id = ?1",
            params![
                preset.id,
                preset.name,
                preset.provider,
                preset.model,
                preset.context_window.map(|value| value as i64),
                preset.input_usd_per_million,
                preset.output_usd_per_million,
                preset.notes
            ],
        )? > 0)
    }

    pub fn delete_model_preset(&self, id: &str) -> Result<bool> {
        let conn = self.lock()?;
        Ok(conn.execute("DELETE FROM model_presets WHERE id = ?1", params![id])? > 0)
    }

    pub fn get_model_preset(&self, id: &str) -> Result<Option<ModelPreset>> {
        let presets = self.list_model_presets()?;
        Ok(presets.into_iter().find(|preset| preset.id == id))
    }
}
