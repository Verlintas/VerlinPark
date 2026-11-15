//! MCP tool pins plus MCP task and server persistence.

use super::*;

impl Store {
    pub fn list_mcp_tool_pins(
        &self,
        project_id: &str,
        server_id: Option<&str>,
    ) -> Result<Vec<McpToolPin>> {
        let conn = self.lock()?;
        let mut rows = Vec::new();
        match server_id {
            Some(server_id) => {
                let mut statement = conn.prepare(
                    "SELECT project_id, server_id, server_name, tool_name,
                            server_fingerprint, schema_hash, description_hash,
                            observed_schema_hash, observed_description_hash,
                            observed_server_fingerprint, status, approved_at, approved_by,
                            last_seen_at, description_preview
                     FROM mcp_tool_pins
                     WHERE project_id = ?1 AND server_id = ?2
                     ORDER BY server_id, tool_name",
                )?;
                let mapped =
                    statement.query_map(params![project_id, server_id], map_mcp_tool_pin)?;
                for row in mapped {
                    rows.push(row?);
                }
            }
            None => {
                let mut statement = conn.prepare(
                    "SELECT project_id, server_id, server_name, tool_name,
                            server_fingerprint, schema_hash, description_hash,
                            observed_schema_hash, observed_description_hash,
                            observed_server_fingerprint, status, approved_at, approved_by,
                            last_seen_at, description_preview
                     FROM mcp_tool_pins
                     WHERE project_id = ?1
                     ORDER BY server_id, tool_name",
                )?;
                let mapped = statement.query_map(params![project_id], map_mcp_tool_pin)?;
                for row in mapped {
                    rows.push(row?);
                }
            }
        }
        Ok(rows)
    }

    pub fn upsert_mcp_tool_pin(&self, pin: &McpToolPin) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO mcp_tool_pins
             (project_id, server_id, server_name, tool_name, server_fingerprint,
              schema_hash, description_hash, observed_schema_hash,
              observed_description_hash, observed_server_fingerprint, status,
              approved_at, approved_by, last_seen_at, description_preview)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             ON CONFLICT(project_id, server_id, tool_name) DO UPDATE SET
                server_name = excluded.server_name,
                server_fingerprint = excluded.server_fingerprint,
                schema_hash = excluded.schema_hash,
                description_hash = excluded.description_hash,
                observed_schema_hash = excluded.observed_schema_hash,
                observed_description_hash = excluded.observed_description_hash,
                observed_server_fingerprint = excluded.observed_server_fingerprint,
                status = excluded.status,
                approved_at = excluded.approved_at,
                approved_by = excluded.approved_by,
                last_seen_at = excluded.last_seen_at,
                description_preview = excluded.description_preview",
            params![
                pin.project_id,
                pin.server_id,
                pin.server_name,
                pin.tool_name,
                pin.server_fingerprint,
                pin.schema_hash,
                pin.description_hash,
                pin.observed_schema_hash,
                pin.observed_description_hash,
                pin.observed_server_fingerprint,
                pin.status,
                pin.approved_at,
                pin.approved_by,
                pin.last_seen_at,
                pin.description_preview
            ],
        )?;
        Ok(())
    }

    pub fn replace_mcp_tool_pins(&self, project_id: &str, pins: &[McpToolPin]) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM mcp_tool_pins WHERE project_id = ?1",
            params![project_id],
        )?;
        for pin in pins {
            tx.execute(
                "INSERT INTO mcp_tool_pins
                 (project_id, server_id, server_name, tool_name, server_fingerprint,
                  schema_hash, description_hash, observed_schema_hash,
                  observed_description_hash, observed_server_fingerprint, status,
                  approved_at, approved_by, last_seen_at, description_preview)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    pin.project_id,
                    pin.server_id,
                    pin.server_name,
                    pin.tool_name,
                    pin.server_fingerprint,
                    pin.schema_hash,
                    pin.description_hash,
                    pin.observed_schema_hash,
                    pin.observed_description_hash,
                    pin.observed_server_fingerprint,
                    pin.status,
                    pin.approved_at,
                    pin.approved_by,
                    pin.last_seen_at,
                    pin.description_preview
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn find_mcp_task_by_remote(
        &self,
        session_id: &str,
        server_id: &str,
        remote_task_id: &str,
    ) -> Result<Option<McpTask>> {
        let conn = self.lock()?;
        conn.query_row(
            &format!(
                "{MCP_TASK_SELECT} WHERE session_id = ?1 AND server_id = ?2 AND remote_task_id = ?3"
            ),
            params![session_id, server_id, remote_task_id],
            map_mcp_task,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn get_mcp_task(&self, task_id: &str) -> Result<Option<McpTask>> {
        let conn = self.lock()?;
        conn.query_row(
            &format!("{MCP_TASK_SELECT} WHERE id = ?1"),
            params![task_id],
            map_mcp_task,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_mcp_tasks(
        &self,
        project_id: Option<&str>,
        session_id: Option<&str>,
        server_id: Option<&str>,
        status: Option<&str>,
        limit: u32,
    ) -> Result<(Vec<McpTask>, bool)> {
        let limit = limit.clamp(1, 500) as i64;
        let conn = self.lock()?;
        let mut statement = conn.prepare(&format!(
            "{MCP_TASK_SELECT}
             WHERE (?1 IS NULL OR project_id = ?1)
               AND (?2 IS NULL OR session_id = ?2)
               AND (?3 IS NULL OR server_id = ?3)
               AND (?4 IS NULL OR status = ?4)
             ORDER BY updated_at DESC, id DESC
             LIMIT ?5"
        ))?;
        let rows = statement
            .query_map(
                params![project_id, session_id, server_id, status, limit + 1],
                map_mcp_task,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let truncated = rows.len() as i64 > limit;
        Ok((rows.into_iter().take(limit as usize).collect(), truncated))
    }

    pub fn insert_mcp_task_with_event(&self, task: &McpTask) -> Result<(McpTask, Option<Event>)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        insert_mcp_task_row_tx(&tx, task)?;
        let event = if task.session_id.trim().is_empty() {
            None
        } else {
            Some(insert_event_tx(
                &tx,
                &task.session_id,
                "mcp.task.created",
                serde_json::to_value(task)?,
            )?)
        };
        tx.commit()?;
        if let Some(event) = &event
            && let Err(error) = self.append_event_mirror(event)
        {
            tracing::warn!(%error, "failed to append MCP task event mirror");
        }
        Ok((task.clone(), event))
    }

    pub fn update_mcp_task_with_event(
        &self,
        task: &McpTask,
        kind: &str,
        extra: Value,
    ) -> Result<(McpTask, Option<Event>)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE mcp_tasks
             SET status = ?2, ttl_secs = ?3, poll_interval_ms = ?4,
                 updated_at = ?5, expires_at = ?6, last_error = ?7,
                 effect_receipt_id = COALESCE(?8, effect_receipt_id)
             WHERE id = ?1",
            params![
                task.id,
                task.status,
                task.ttl_secs as i64,
                task.poll_interval_ms as i64,
                task.updated_at,
                task.expires_at,
                task.last_error,
                task.effect_receipt_id,
            ],
        )?;
        if changed == 0 {
            return Err(anyhow!("MCP task not found: {}", task.id));
        }
        let event = if task.session_id.trim().is_empty() {
            None
        } else {
            let mut payload = serde_json::to_value(task)?;
            if let (Some(object), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
                for (key, value) in extra {
                    object.insert(key.clone(), value.clone());
                }
            }
            Some(insert_event_tx(&tx, &task.session_id, kind, payload)?)
        };
        tx.commit()?;
        if let Some(event) = &event
            && let Err(error) = self.append_event_mirror(event)
        {
            tracing::warn!(%error, "failed to append MCP task event mirror");
        }
        Ok((task.clone(), event))
    }

    pub fn set_mcp_task_result_with_event(
        &self,
        task: &McpTask,
        result_json: &str,
        result_hash: &str,
        result_bytes: u64,
    ) -> Result<(McpTask, Option<Event>)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE mcp_tasks
             SET status = ?2, updated_at = ?3, result_json = ?4,
                 result_hash = ?5, result_bytes = ?6,
                 last_error = ?7, expires_at = ?8
             WHERE id = ?1",
            params![
                task.id,
                task.status,
                task.updated_at,
                result_json,
                result_hash,
                result_bytes as i64,
                task.last_error,
                task.expires_at,
            ],
        )?;
        if changed == 0 {
            return Err(anyhow!("MCP task not found: {}", task.id));
        }
        let mut stored = task.clone();
        stored.result_available = true;
        let event = if task.session_id.trim().is_empty() {
            None
        } else {
            Some(insert_event_tx(
                &tx,
                &task.session_id,
                "mcp.task.result",
                serde_json::json!({
                    "task": stored,
                    "result_hash": result_hash,
                    "result_bytes": result_bytes,
                    "effect_receipt_id": task.effect_receipt_id,
                    "note": "task result is evidence; an unknown effect receipt still requires explicit reconciliation",
                }),
            )?)
        };
        tx.commit()?;
        if let Some(event) = &event
            && let Err(error) = self.append_event_mirror(event)
        {
            tracing::warn!(%error, "failed to append MCP task result mirror");
        }
        Ok((stored, event))
    }

    pub fn mcp_task_result(&self, task_id: &str) -> Result<Option<(String, String, u64)>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT COALESCE(result_json, ''), COALESCE(result_hash, ''), result_bytes
             FROM mcp_tasks WHERE id = ?1",
            params![task_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?.max(0) as u64,
                ))
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_mcp_servers(&self) -> Result<Vec<McpServer>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, command, args, env, enabled, created_at, transport, url, headers
             FROM mcp_servers ORDER BY name ASC",
        )?;
        let rows = stmt.query_map([], map_mcp_server)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_mcp_server(&self, server_id: &str) -> Result<Option<McpServer>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, name, command, args, env, enabled, created_at, transport, url, headers
             FROM mcp_servers WHERE id = ?1",
            params![server_id],
            map_mcp_server,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn add_mcp_server(&self, config: &McpServerConfig) -> Result<McpServer> {
        let server = McpServer {
            id: Uuid::now_v7().to_string(),
            name: config.name.clone(),
            transport: config.transport,
            command: config.command.clone(),
            args: config.args.clone(),
            env: config.env.clone(),
            url: config.url.clone(),
            headers: config.headers.clone(),
            enabled: config.enabled,
            created_at: now_rfc3339(),
        };
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO mcp_servers
                (id, name, command, args, env, enabled, created_at, transport, url, headers)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                server.id,
                server.name,
                server.command,
                serde_json::to_string(&server.args)?,
                serde_json::to_string(&server.env)?,
                if server.enabled { 1 } else { 0 },
                server.created_at,
                transport_name(server.transport),
                server.url,
                serde_json::to_string(&server.headers)?,
            ],
        )?;
        Ok(server)
    }

    pub fn update_mcp_server(
        &self,
        server_id: &str,
        config: &McpServerConfig,
    ) -> Result<Option<McpServer>> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE mcp_servers
             SET name = ?1, command = ?2, args = ?3, env = ?4, enabled = ?5,
                 transport = ?6, url = ?7, headers = ?8
             WHERE id = ?9",
            params![
                config.name,
                config.command,
                serde_json::to_string(&config.args)?,
                serde_json::to_string(&config.env)?,
                if config.enabled { 1 } else { 0 },
                transport_name(config.transport),
                config.url,
                serde_json::to_string(&config.headers)?,
                server_id
            ],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get_mcp_server(server_id)
    }

    pub fn remove_mcp_server(&self, server_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        Ok(conn.execute("DELETE FROM mcp_servers WHERE id = ?1", params![server_id])? > 0)
    }
}
