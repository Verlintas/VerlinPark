//! Remote execution-host config, bootstrap records, and remote test evidence.

use super::*;

impl Store {
    pub fn get_remote_integration_policy(
        &self,
        project_id: &str,
    ) -> Result<RemoteIntegrationPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT enabled, max_bundle_bytes, require_verifier, allowed_hosts, keep_import_refs
                 FROM project_remote_integration_policies WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)? != 0,
                        row.get::<_, i64>(1)? as u64,
                        row.get::<_, i64>(2)? != 0,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)? != 0,
                    ))
                },
            )
            .optional()?;
        match stored {
            Some((
                enabled,
                max_bundle_bytes,
                require_verifier,
                allowed_hosts,
                keep_import_refs,
            )) => Ok(RemoteIntegrationPolicy {
                enabled,
                max_bundle_bytes,
                require_verifier,
                allowed_hosts: serde_json::from_str(&allowed_hosts)
                    .context("parsing stored remote integration allowlist")?,
                keep_import_refs,
            }),
            None => Ok(RemoteIntegrationPolicy::default()),
        }
    }

    pub fn set_remote_integration_policy(
        &self,
        project_id: &str,
        policy: &RemoteIntegrationPolicy,
    ) -> Result<()> {
        policy
            .validate()
            .map_err(|error| anyhow!("invalid remote integration policy: {error}"))?;
        let allowed_hosts = serde_json::to_string(&policy.allowed_hosts)
            .context("serializing remote integration allowlist")?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_remote_integration_policies
             (project_id, enabled, max_bundle_bytes, require_verifier, allowed_hosts,
              keep_import_refs, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(project_id) DO UPDATE SET
                enabled = excluded.enabled,
                max_bundle_bytes = excluded.max_bundle_bytes,
                require_verifier = excluded.require_verifier,
                allowed_hosts = excluded.allowed_hosts,
                keep_import_refs = excluded.keep_import_refs,
                updated_at = excluded.updated_at",
            params![
                project_id,
                if policy.enabled { 1 } else { 0 },
                policy.max_bundle_bytes as i64,
                if policy.require_verifier { 1 } else { 0 },
                allowed_hosts,
                if policy.keep_import_refs { 1 } else { 0 },
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn remote_integration_policy_updated_at(&self, project_id: &str) -> Result<Option<String>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT updated_at FROM project_remote_integration_policies WHERE project_id = ?1",
            params![project_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn get_remote_test_policy(&self, project_id: &str) -> Result<RemoteTestPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT enabled, default_profile, profiles, timeout_secs, output_cap_bytes
                 FROM project_remote_test_policies WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)? != 0,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)? as u64,
                        row.get::<_, i64>(4)? as u64,
                    ))
                },
            )
            .optional()?;
        match stored {
            Some((enabled, default_profile, profiles, timeout_secs, output_cap_bytes)) => {
                let profiles: Vec<vp_protocol::remote::RemoteTestProfile> =
                    serde_json::from_str(&profiles)
                        .context("parsing stored remote test profiles")?;
                Ok(RemoteTestPolicy {
                    enabled,
                    default_profile,
                    profiles,
                    timeout_secs,
                    output_cap_bytes,
                })
            }
            None => Ok(RemoteTestPolicy::default()),
        }
    }

    pub fn set_remote_test_policy(
        &self,
        project_id: &str,
        policy: &RemoteTestPolicy,
    ) -> Result<()> {
        policy
            .validate()
            .map_err(|error| anyhow!("invalid remote test policy: {error}"))?;
        let profiles =
            serde_json::to_string(&policy.profiles).context("serializing remote test profiles")?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_remote_test_policies
             (project_id, enabled, default_profile, profiles, timeout_secs,
              output_cap_bytes, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(project_id) DO UPDATE SET
                enabled = excluded.enabled,
                default_profile = excluded.default_profile,
                profiles = excluded.profiles,
                timeout_secs = excluded.timeout_secs,
                output_cap_bytes = excluded.output_cap_bytes,
                updated_at = excluded.updated_at",
            params![
                project_id,
                if policy.enabled { 1 } else { 0 },
                policy.default_profile,
                profiles,
                policy.timeout_secs as i64,
                policy.output_cap_bytes as i64,
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn remote_test_policy_updated_at(&self, project_id: &str) -> Result<Option<String>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT updated_at FROM project_remote_test_policies WHERE project_id = ?1",
            params![project_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn upsert_remote_bootstrap_record(&self, record: &RemoteBootstrapRecord) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO remote_bootstrap_records
             (project_id, host, install_path, sha256, bytes, helper_version,
              protocol_version, target_os, target_arch, installed_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(project_id, host, install_path) DO UPDATE SET
                sha256 = excluded.sha256,
                bytes = excluded.bytes,
                helper_version = excluded.helper_version,
                protocol_version = excluded.protocol_version,
                target_os = excluded.target_os,
                target_arch = excluded.target_arch,
                installed_at = excluded.installed_at,
                updated_at = excluded.updated_at",
            params![
                record.project_id,
                record.host,
                record.install_path,
                record.sha256,
                record.bytes as i64,
                record.helper_version,
                record.protocol_version as i64,
                record.target_os,
                record.target_arch,
                record.installed_at,
                record.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn list_remote_bootstrap_records(
        &self,
        project_id: &str,
    ) -> Result<Vec<RemoteBootstrapRecord>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT project_id, host, install_path, sha256, bytes, helper_version,
                    protocol_version, target_os, target_arch, installed_at, updated_at
             FROM remote_bootstrap_records
             WHERE project_id = ?1
             ORDER BY updated_at DESC, host ASC, install_path ASC",
        )?;
        let rows = stmt.query_map(params![project_id], map_remote_bootstrap_record)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_remote_bootstrap_record(
        &self,
        project_id: &str,
        host: &str,
        install_path: &str,
    ) -> Result<Option<RemoteBootstrapRecord>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT project_id, host, install_path, sha256, bytes, helper_version,
                    protocol_version, target_os, target_arch, installed_at, updated_at
             FROM remote_bootstrap_records
             WHERE project_id = ?1 AND host = ?2 AND install_path = ?3",
            params![project_id, host, install_path],
            map_remote_bootstrap_record,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Insert one evidence row and its `remote_test.completed` event in the
    /// same transaction. Output bodies stay in daemon-owned storage; this row
    /// carries only the digest and a bounded reference.
    pub fn insert_remote_test_evidence_with_event(
        &self,
        evidence: &RemoteTestEvidence,
    ) -> Result<Event> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO remote_test_evidence
             (id, project_id, session_id, entry_id, profile, kind, host, commit_hash,
              command, cwd, status, exit_code, duration_ms, output_ref, output_sha256,
              output_bytes, output_truncated, timed_out, detail, digest,
              environment_caveat, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
            params![
                evidence.id,
                evidence.project_id,
                evidence.session_id,
                evidence.entry_id,
                evidence.profile,
                evidence.kind,
                evidence.host,
                evidence.commit,
                evidence.command,
                evidence.cwd,
                evidence.status,
                evidence.exit_code,
                evidence.duration_ms as i64,
                evidence.output_ref,
                evidence.output_sha256,
                evidence.output_bytes as i64,
                if evidence.output_truncated { 1 } else { 0 },
                if evidence.timed_out { 1 } else { 0 },
                evidence.detail,
                evidence.digest,
                evidence.environment_caveat,
                evidence.created_at,
            ],
        )?;
        let event = insert_event_tx(
            &tx,
            &evidence.session_id,
            "remote_test.completed",
            serde_json::json!({
                "evidence": evidence,
                "supplemental": true,
                "local_verifier_required": true,
                "merge_gate_unchanged": true,
                "environment_caveat": evidence.environment_caveat,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(event)
    }

    pub fn get_remote_test_evidence(&self, id: &str) -> Result<Option<RemoteTestEvidence>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, project_id, session_id, entry_id, profile, kind, host,
                    commit_hash, command, cwd, status, exit_code, duration_ms,
                    output_ref, output_sha256, output_bytes, output_truncated,
                    timed_out, detail, digest, environment_caveat, created_at
             FROM remote_test_evidence WHERE id = ?1",
            params![id],
            map_remote_test_evidence,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_remote_test_evidence(
        &self,
        project_id: &str,
        session_id: Option<&str>,
        entry_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<RemoteTestEvidence>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, project_id, session_id, entry_id, profile, kind, host,
                    commit_hash, command, cwd, status, exit_code, duration_ms,
                    output_ref, output_sha256, output_bytes, output_truncated,
                    timed_out, detail, digest, environment_caveat, created_at
             FROM remote_test_evidence
             WHERE project_id = ?1
               AND (?2 IS NULL OR session_id = ?2)
               AND (?3 IS NULL OR entry_id = ?3)
             ORDER BY created_at DESC, id DESC
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![
                project_id,
                session_id,
                entry_id,
                i64::from(limit.clamp(1, 100))
            ],
            map_remote_test_evidence,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_remote_test_evidence_for_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<RemoteTestEvidence>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, project_id, session_id, entry_id, profile, kind, host,
                    commit_hash, command, cwd, status, exit_code, duration_ms,
                    output_ref, output_sha256, output_bytes, output_truncated,
                    timed_out, detail, digest, environment_caveat, created_at
             FROM remote_test_evidence
             WHERE session_id = ?1
             ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![session_id], map_remote_test_evidence)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_execution_host(&self, project_id: &str) -> Result<ExecutionHost> {
        let conn = self.lock()?;
        let stored: Option<String> = conn
            .query_row(
                "SELECT config FROM project_execution_hosts WHERE project_id = ?1",
                params![project_id],
                |row| row.get(0),
            )
            .optional()?;
        match stored {
            Some(config) => {
                serde_json::from_str(&config).context("parsing stored project execution host")
            }
            None => Ok(ExecutionHost::default()),
        }
    }

    pub fn execution_host_updated_at(&self, project_id: &str) -> Result<Option<String>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT updated_at FROM project_execution_hosts WHERE project_id = ?1",
            params![project_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn set_execution_host(&self, project_id: &str, config: &ExecutionHost) -> Result<()> {
        let encoded =
            serde_json::to_string(config).context("serializing project execution host")?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_execution_hosts (project_id, config, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id) DO UPDATE SET
                config = excluded.config,
                updated_at = excluded.updated_at",
            params![project_id, encoded, now_rfc3339()],
        )?;
        Ok(())
    }

    pub fn remove_execution_host(&self, project_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let removed = conn.execute(
            "DELETE FROM project_execution_hosts WHERE project_id = ?1",
            params![project_id],
        )?;
        Ok(removed > 0)
    }

    pub fn list_execution_hosts(&self) -> Result<Vec<ExecutionHost>> {
        let projects = self.list_projects()?;
        projects
            .into_iter()
            .map(|project| self.get_execution_host(&project.id))
            .collect()
    }
}
