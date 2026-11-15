//! Artifacts, artifact versions, and artifact policy.

use super::*;

impl Store {
    #[allow(clippy::too_many_arguments)]
    pub fn create_artifact_with_event(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<&str>,
        name: &str,
        kind: &str,
        mime_type: &str,
        content: &str,
        storage: &str,
        size_bytes: u64,
        sha256: &str,
        path: Option<&str>,
    ) -> Result<(Artifact, Event)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let artifact = Artifact {
            id: Uuid::now_v7().to_string(),
            session_id: session_id.to_string(),
            agent_id: agent_id.to_string(),
            agent_name: agent_name.to_string(),
            task_id: task_id.map(str::to_string),
            name: name.to_string(),
            kind: kind.to_string(),
            mime_type: mime_type.to_string(),
            content: content.to_string(),
            storage: storage.to_string(),
            compression: "none".to_string(),
            size_bytes,
            sha256: sha256.to_string(),
            path: path.map(str::to_string),
            data_base64: None,
            version: 1,
            provenance: None,
            created_at: now_rfc3339(),
        };
        tx.execute(
            "INSERT INTO artifacts
             (id, session_id, agent_id, agent_name, task_id, name, kind, mime_type,
              content, storage, compression, size_bytes, sha256, path, created_at, version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, 1)",
            params![
                artifact.id,
                artifact.session_id,
                artifact.agent_id,
                artifact.agent_name,
                artifact.task_id,
                artifact.name,
                artifact.kind,
                artifact.mime_type,
                artifact.content,
                artifact.storage,
                artifact.compression,
                artifact.size_bytes as i64,
                artifact.sha256,
                artifact.path,
                artifact.created_at
            ],
        )?;
        insert_artifact_version_tx(&tx, &artifact)?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "artifact.published",
            serde_json::json!({
                "artifact_id": artifact.id,
                "name": artifact.name,
                "kind": artifact.kind,
                "agent_id": artifact.agent_id,
                "agent_name": artifact.agent_name,
                "task_id": artifact.task_id,
                "mime_type": artifact.mime_type,
                "storage": artifact.storage,
                "compression": artifact.compression,
                "size_bytes": artifact.size_bytes,
                "sha256": artifact.sha256,
                "path": artifact.path,
                "content": artifact.content,
                "version": artifact.version,
                "provenance": artifact.provenance,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((artifact, event))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_artifact_file_with_event(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<&str>,
        name: &str,
        kind: &str,
        mime_type: &str,
        data: &[u8],
        sha256: &str,
        provenance: Option<&ArtifactProvenance>,
    ) -> Result<(Artifact, Event)> {
        let id = Uuid::now_v7().to_string();
        let dir = self.data_dir.join("artifacts").join("blobs");
        paths::ensure_private_dir(&dir)?;
        let path = dir.join(format!("{sha256}.zst"));
        let created_file = !path.is_file();
        if created_file {
            let compressed = zstd::stream::encode_all(data, 3)
                .with_context(|| format!("compressing artifact {}", id))?;
            std::fs::write(&path, compressed)
                .with_context(|| format!("writing artifact blob {}", path.display()))?;
            paths::ensure_private_file(&path)?;
        }
        let artifact = Artifact {
            id: id.clone(),
            session_id: session_id.to_string(),
            agent_id: agent_id.to_string(),
            agent_name: agent_name.to_string(),
            task_id: task_id.map(str::to_string),
            name: name.to_string(),
            kind: kind.to_string(),
            mime_type: mime_type.to_string(),
            content: String::new(),
            storage: "file".to_string(),
            compression: "zstd".to_string(),
            size_bytes: data.len() as u64,
            sha256: sha256.to_string(),
            path: Some(path.to_string_lossy().to_string()),
            data_base64: None,
            version: 1,
            provenance: provenance.cloned(),
            created_at: now_rfc3339(),
        };
        let provenance_encoded = encode_artifact_provenance(provenance)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO artifacts
             (id, session_id, agent_id, agent_name, task_id, name, kind, mime_type,
              content, storage, compression, size_bytes, sha256, path, provenance,
              created_at, version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, '', 'file', 'zstd', ?9, ?10, ?11, ?12, ?13, 1)",
            params![
                artifact.id,
                artifact.session_id,
                artifact.agent_id,
                artifact.agent_name,
                artifact.task_id,
                artifact.name,
                artifact.kind,
                artifact.mime_type,
                artifact.size_bytes as i64,
                artifact.sha256,
                artifact.path,
                provenance_encoded,
                artifact.created_at
            ],
        )?;
        insert_artifact_version_tx(&tx, &artifact)?;
        let event = insert_event_tx(
            &tx,
            session_id,
            "artifact.published",
            serde_json::json!({
                "artifact_id": artifact.id,
                "name": artifact.name,
                "kind": artifact.kind,
                "agent_id": artifact.agent_id,
                "agent_name": artifact.agent_name,
                "task_id": artifact.task_id,
                "mime_type": artifact.mime_type,
                "storage": artifact.storage,
                "compression": artifact.compression,
                "size_bytes": artifact.size_bytes,
                "sha256": artifact.sha256,
                "path": artifact.path,
                "content": "",
                "version": artifact.version,
                "provenance": artifact.provenance,
            }),
        )?;
        if let Err(error) = tx.commit() {
            if created_file {
                let _ = std::fs::remove_file(&path);
            }
            return Err(error.into());
        }
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok((artifact, event))
    }

    /// Publish a new version of an inline text artifact. File-backed artifacts
    /// are intentionally immutable here because their blobs also participate
    /// in content-addressed GC.
    pub fn update_artifact_with_event(
        &self,
        artifact_id: &str,
        expected_version: Option<u32>,
        name: &str,
        kind: &str,
        mime_type: &str,
        content: &str,
    ) -> Result<Option<(Artifact, Event)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let current = tx
            .query_row(
                "SELECT id, session_id, agent_id, agent_name, task_id, name, kind,
                        mime_type, content, storage, compression, size_bytes, sha256, path,
                        created_at, version, provenance
                 FROM artifacts WHERE id = ?1",
                params![artifact_id],
                map_artifact,
            )
            .optional()?;
        let Some(current) = current else {
            return Ok(None);
        };
        if current.storage != "inline" {
            anyhow::bail!("binary artifact versions are not editable");
        }
        if let Some(expected) = expected_version
            && expected != current.version
        {
            anyhow::bail!(
                "artifact version conflict: expected {expected}, current {}",
                current.version
            );
        }
        // Preserve the current version for legacy artifacts that predate the
        // version table before moving the row forward.
        insert_artifact_version_tx(&tx, &current)?;
        let sha256 = format!("{:x}", Sha256::digest(content.as_bytes()));
        let updated = Artifact {
            name: name.to_string(),
            kind: kind.to_string(),
            mime_type: mime_type.to_string(),
            content: content.to_string(),
            size_bytes: content.len() as u64,
            sha256,
            version: current.version.saturating_add(1),
            created_at: current.created_at.clone(),
            ..current.clone()
        };
        tx.execute(
            "UPDATE artifacts
             SET name = ?2, kind = ?3, mime_type = ?4, content = ?5, size_bytes = ?6,
                 sha256 = ?7, version = ?8
             WHERE id = ?1",
            params![
                artifact_id,
                updated.name,
                updated.kind,
                updated.mime_type,
                updated.content,
                updated.size_bytes as i64,
                updated.sha256,
                updated.version as i64,
            ],
        )?;
        insert_artifact_version_tx(&tx, &updated)?;
        let event = insert_event_tx(
            &tx,
            &updated.session_id,
            "artifact.version_published",
            serde_json::json!({
                "artifact_id": updated.id,
                "name": updated.name,
                "kind": updated.kind,
                "agent_id": updated.agent_id,
                "agent_name": updated.agent_name,
                "task_id": updated.task_id,
                "mime_type": updated.mime_type,
                "storage": updated.storage,
                "compression": updated.compression,
                "size_bytes": updated.size_bytes,
                "sha256": updated.sha256,
                "path": updated.path,
                "content": updated.content,
                "version": updated.version,
                "previous_version": current.version,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((updated, event)))
    }

    pub fn list_artifact_versions(&self, artifact_id: &str) -> Result<Vec<ArtifactVersion>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT artifact_id, version, name, kind, mime_type, content, storage,
                    compression, size_bytes, sha256, path, created_at, provenance
             FROM artifact_versions WHERE artifact_id = ?1 ORDER BY version ASC",
        )?;
        let rows = stmt.query_map(params![artifact_id], map_artifact_version)?;
        let versions = rows.collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        drop(conn);
        if !versions.is_empty() {
            return Ok(versions);
        }
        // Artifacts created before version rows existed still have a valid
        // current version. Expose that snapshot rather than an empty history.
        Ok(self
            .get_artifact(artifact_id)?
            .map(|artifact| vec![artifact_version_from_artifact(&artifact)])
            .unwrap_or_default())
    }

    pub fn get_artifact_version(
        &self,
        artifact_id: &str,
        version: u32,
    ) -> Result<Option<ArtifactVersion>> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT artifact_id, version, name, kind, mime_type, content, storage,
                        compression, size_bytes, sha256, path, created_at, provenance
                 FROM artifact_versions WHERE artifact_id = ?1 AND version = ?2",
                params![artifact_id, version as i64],
                map_artifact_version,
            )
            .optional()?;
        drop(conn);
        if stored.is_some() {
            return Ok(stored);
        }
        let Some(artifact) = self.get_artifact(artifact_id)? else {
            return Ok(None);
        };
        if artifact.version != version {
            return Ok(None);
        }
        Ok(Some(artifact_version_from_artifact(&artifact)))
    }

    pub fn restore_artifact_version(
        &self,
        artifact_id: &str,
        version: u32,
    ) -> Result<Option<(Artifact, Event)>> {
        let Some(version_record) = self.get_artifact_version(artifact_id, version)? else {
            return Ok(None);
        };
        if version_record.storage != "inline" {
            anyhow::bail!("binary artifact versions cannot be restored");
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let current = tx
            .query_row(
                "SELECT id, session_id, agent_id, agent_name, task_id, name, kind,
                        mime_type, content, storage, compression, size_bytes, sha256, path,
                        created_at, version, provenance
                 FROM artifacts WHERE id = ?1",
                params![artifact_id],
                map_artifact,
            )
            .optional()?;
        let Some(current) = current else {
            return Ok(None);
        };
        if current.storage != "inline" {
            anyhow::bail!("binary artifact versions cannot be restored");
        }
        insert_artifact_version_tx(&tx, &current)?;
        let updated = Artifact {
            name: version_record.name,
            kind: version_record.kind,
            mime_type: version_record.mime_type,
            content: version_record.content,
            size_bytes: version_record.size_bytes,
            sha256: version_record.sha256,
            version: current.version.saturating_add(1),
            ..current.clone()
        };
        tx.execute(
            "UPDATE artifacts
             SET name = ?2, kind = ?3, mime_type = ?4, content = ?5, size_bytes = ?6,
                 sha256 = ?7, version = ?8
             WHERE id = ?1",
            params![
                artifact_id,
                updated.name,
                updated.kind,
                updated.mime_type,
                updated.content,
                updated.size_bytes as i64,
                updated.sha256,
                updated.version as i64,
            ],
        )?;
        insert_artifact_version_tx(&tx, &updated)?;
        let event = insert_event_tx(
            &tx,
            &updated.session_id,
            "artifact.version_restored",
            serde_json::json!({
                "artifact_id": updated.id,
                "name": updated.name,
                "kind": updated.kind,
                "agent_id": updated.agent_id,
                "agent_name": updated.agent_name,
                "task_id": updated.task_id,
                "mime_type": updated.mime_type,
                "storage": updated.storage,
                "compression": updated.compression,
                "size_bytes": updated.size_bytes,
                "sha256": updated.sha256,
                "path": updated.path,
                "content": updated.content,
                "version": updated.version,
                "restored_from_version": version,
                "previous_version": current.version,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((updated, event)))
    }

    pub fn get_artifact_by_request_key(
        &self,
        session_id: &str,
        request_key: &str,
    ) -> Result<Option<Artifact>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT a.id, a.session_id, a.agent_id, a.agent_name, a.task_id, a.name,
                    a.kind, a.mime_type, a.content, a.storage, a.compression,
                    a.size_bytes, a.sha256, a.path, a.created_at, a.version, a.provenance
             FROM artifact_requests r
             JOIN artifacts a ON a.id = r.artifact_id
             WHERE r.session_id = ?1 AND r.request_key = ?2",
            params![session_id, request_key],
            map_artifact,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn register_artifact_request(
        &self,
        session_id: &str,
        request_key: &str,
        artifact_id: &str,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO artifact_requests (session_id, request_key, artifact_id, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(session_id, request_key) DO NOTHING",
            params![session_id, request_key, artifact_id, now_rfc3339()],
        )?;
        Ok(())
    }

    pub fn list_artifacts(&self, session_id: &str) -> Result<Vec<Artifact>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, agent_id, agent_name, task_id, name, kind,
                    mime_type, content, storage, compression, size_bytes, sha256, path,
                    created_at, version, provenance
             FROM artifacts WHERE session_id = ?1 ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![session_id], map_artifact)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_artifact(&self, artifact_id: &str) -> Result<Option<Artifact>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, session_id, agent_id, agent_name, task_id, name, kind,
                    mime_type, content, storage, compression, size_bytes, sha256, path,
                    created_at, version, provenance
             FROM artifacts WHERE id = ?1",
            params![artifact_id],
            map_artifact,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn materialize_artifact(&self, artifact_id: &str) -> Result<Option<std::path::PathBuf>> {
        let Some(artifact) = self.get_artifact(artifact_id)? else {
            return Ok(None);
        };
        if artifact.storage != "file" {
            return Ok(None);
        }
        let Some(source) = artifact.path.as_deref() else {
            return Err(anyhow!("artifact {artifact_id} has no stored path"));
        };
        if artifact.compression != "zstd" {
            return Ok(Some(std::path::PathBuf::from(source)));
        }
        let compressed = std::fs::read(source)
            .with_context(|| format!("reading compressed artifact {source}"))?;
        let data = zstd::stream::decode_all(&compressed[..])
            .with_context(|| format!("decompressing artifact {artifact_id}"))?;
        use sha2::{Digest, Sha256};
        let sha256 = format!("{:x}", Sha256::digest(&data));
        if !artifact.sha256.is_empty() && sha256 != artifact.sha256 {
            return Err(anyhow!(
                "artifact {artifact_id} content does not match its SHA-256"
            ));
        }
        let dir = self.data_dir.join("artifacts").join("materialized");
        paths::ensure_private_dir(&dir)?;
        let path = dir.join(&sha256);
        if !path.is_file() {
            std::fs::write(&path, data)
                .with_context(|| format!("writing materialized artifact {}", path.display()))?;
            paths::ensure_private_file(&path)?;
        }
        Ok(Some(path))
    }

    // ------------------------------------------------------------------
    // Tool output store
    //
    // Full tool outputs are operational state. They are content-addressed,
    // daemon-owned, and never addressed by a user-supplied path. Metadata rows
    // are intentionally not part of replay/repair or bundle content.
    // ------------------------------------------------------------------

    pub fn get_artifact_policy(&self, project_id: &str) -> Result<ArtifactPolicy> {
        let conn = self.lock()?;
        let stored = conn
            .query_row(
                "SELECT auto_gc, max_bytes, max_age_secs, max_count
                 FROM project_artifact_policies WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok(ArtifactPolicy {
                        auto_gc: row.get::<_, i64>(0)? != 0,
                        max_bytes: row.get::<_, Option<i64>>(1)?.map(|value| value as u64),
                        max_age_secs: row.get::<_, Option<i64>>(2)?.map(|value| value as u64),
                        max_count: row.get::<_, Option<i64>>(3)?.map(|value| value as u64),
                    })
                },
            )
            .optional()?;
        Ok(stored.unwrap_or_default())
    }

    pub fn set_artifact_policy(&self, project_id: &str, policy: &ArtifactPolicy) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_artifact_policies
                (project_id, auto_gc, max_bytes, max_age_secs, max_count, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(project_id) DO UPDATE SET
                auto_gc = excluded.auto_gc,
                max_bytes = excluded.max_bytes,
                max_age_secs = excluded.max_age_secs,
                max_count = excluded.max_count,
                updated_at = excluded.updated_at",
            params![
                project_id,
                if policy.auto_gc { 1 } else { 0 },
                policy.max_bytes.map(|value| value as i64),
                policy.max_age_secs.map(|value| value as i64),
                policy.max_count.map(|value| value as i64),
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn list_artifacts_for_project(&self, project_id: &str) -> Result<Vec<Artifact>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT a.id, a.session_id, a.agent_id, a.agent_name, a.task_id, a.name,
                    a.kind, a.mime_type, a.content, a.storage, a.compression,
                    a.size_bytes, a.sha256, a.path, a.created_at, a.version, a.provenance
             FROM artifacts a
             JOIN sessions s ON s.id = a.session_id
             WHERE s.project_id = ?1
             ORDER BY a.created_at ASC, a.id ASC",
        )?;
        let rows = stmt.query_map(params![project_id], map_artifact)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn delete_artifact(&self, artifact_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let changed = conn.execute("DELETE FROM artifacts WHERE id = ?1", params![artifact_id])?;
        Ok(changed > 0)
    }

    pub fn delete_artifact_with_event(
        &self,
        artifact_id: &str,
    ) -> Result<Option<(Event, Option<String>)>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let artifact = tx
            .query_row(
                "SELECT session_id, name, kind, storage, size_bytes, sha256, path
                 FROM artifacts WHERE id = ?1",
                params![artifact_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((session_id, name, kind, storage, size_bytes, sha256, path)) = artifact else {
            return Ok(None);
        };
        tx.execute("DELETE FROM artifacts WHERE id = ?1", params![artifact_id])?;
        let remove_path = if let Some(path) = &path {
            let remaining: i64 = tx.query_row(
                "SELECT COUNT(*) FROM artifacts WHERE path = ?1",
                params![path],
                |row| row.get(0),
            )?;
            (remaining == 0).then(|| path.clone())
        } else {
            None
        };
        let event = insert_event_tx(
            &tx,
            &session_id,
            "artifact.pruned",
            serde_json::json!({
                "artifact_id": artifact_id,
                "name": name,
                "kind": kind,
                "storage": storage,
                "size_bytes": size_bytes,
                "sha256": sha256,
                "path": path,
            }),
        )?;
        tx.commit()?;
        if let Err(error) = self.append_event_mirror(&event) {
            tracing::warn!(%error, "failed to append event mirror");
        }
        Ok(Some((event, remove_path)))
    }
}
