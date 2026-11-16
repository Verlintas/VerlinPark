use super::*;

impl Engine {
    #[allow(clippy::too_many_arguments)]
    pub async fn publish_artifact(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<String>,
        name: &str,
        kind: &str,
        mime_type: &str,
        content: &str,
    ) -> Result<Artifact> {
        let name = name.trim();
        if name.is_empty() || name.len() > 200 {
            anyhow::bail!("artifact name must be 1-200 characters");
        }
        if content.len() > 100_000 {
            anyhow::bail!("artifact content exceeds 100000 bytes");
        }
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let agent_id = agent_id.to_string();
        let agent_name = agent_name.to_string();
        let name = name.to_string();
        let kind = kind.to_string();
        let mime_type = mime_type.to_string();
        let content = content.to_string();
        let size_bytes = content.len() as u64;
        let sha256 = sha256_hex(content.as_bytes());
        let (artifact, event) = tokio::task::spawn_blocking(move || {
            store.create_artifact_with_event(
                &session_id,
                &agent_id,
                &agent_name,
                task_id.as_deref(),
                &name,
                &kind,
                &mime_type,
                &content,
                "inline",
                size_bytes,
                &sha256,
                None,
            )
        })
        .await??;
        let _ = self.events.send(event);
        self.auto_gc_restarted_artifact(&artifact.session_id).await;
        Ok(artifact)
    }

    /// Publish an artifact with an idempotency key scoped to the session. A
    /// repeated key returns the first committed artifact instead of creating a
    /// duplicate. The pre-check is atomic enough for the local single-daemon
    /// runtime; a cross-process race can still create an orphan row but never
    /// causes the caller to receive a different artifact than the registered
    /// winner.
    #[allow(clippy::too_many_arguments)]
    pub async fn publish_artifact_keyed(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<String>,
        name: &str,
        kind: &str,
        mime_type: &str,
        content: &str,
        request_key: &str,
    ) -> Result<Artifact> {
        let request_key = request_key.trim();
        if request_key.is_empty() || request_key.len() > 160 {
            anyhow::bail!("artifact request key must be 1-160 characters");
        }
        if !request_key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_:./".contains(character))
        {
            anyhow::bail!("artifact request key contains unsupported characters");
        }
        let store = self.store.clone();
        let session_id_owned = session_id.to_string();
        let key = request_key.to_string();
        if let Some(existing) = tokio::task::spawn_blocking(move || {
            store.get_artifact_by_request_key(&session_id_owned, &key)
        })
        .await??
        {
            return Ok(existing);
        }
        let artifact = self
            .publish_artifact(
                session_id, agent_id, agent_name, task_id, name, kind, mime_type, content,
            )
            .await?;
        let store = self.store.clone();
        let session_id_owned = session_id.to_string();
        let key = request_key.to_string();
        let artifact_id = artifact.id.clone();
        tokio::task::spawn_blocking(move || {
            store.register_artifact_request(&session_id_owned, &key, &artifact_id)
        })
        .await??;
        Ok(artifact)
    }

    /// Publish a new version of an inline artifact. `expected_version` is an
    /// optimistic concurrency guard so two writers cannot silently overwrite
    /// each other.
    #[allow(clippy::too_many_arguments)]
    pub async fn publish_artifact_version(
        &self,
        artifact_id: &str,
        expected_version: Option<u32>,
        name: &str,
        kind: &str,
        mime_type: &str,
        content: &str,
    ) -> Result<Artifact> {
        let name = name.trim();
        if name.is_empty() || name.len() > 200 {
            anyhow::bail!("artifact name must be 1-200 characters");
        }
        if content.len() > 100_000 {
            anyhow::bail!("artifact content exceeds 100000 bytes");
        }
        let store = self.store.clone();
        let artifact_id = artifact_id.to_string();
        let artifact_id_for_error = artifact_id.clone();
        let name = name.to_string();
        let kind = kind.to_string();
        let mime_type = mime_type.to_string();
        let content = content.to_string();
        let Some((artifact, event)) = tokio::task::spawn_blocking(move || {
            store.update_artifact_with_event(
                &artifact_id,
                expected_version,
                &name,
                &kind,
                &mime_type,
                &content,
            )
        })
        .await??
        else {
            anyhow::bail!("artifact not found: {artifact_id_for_error}");
        };
        let _ = self.events.send(event);
        self.auto_gc_restarted_artifact(&artifact.session_id).await;
        Ok(artifact)
    }

    /// Compare two inline versions (or a version against the current artifact)
    /// with a bounded deterministic line diff.
    pub async fn artifact_diff(&self, params: ArtifactDiffParams) -> Result<ArtifactDiffResult> {
        let store = self.store.clone();
        let id = params.artifact_id.clone();
        let current = tokio::task::spawn_blocking(move || store.get_artifact(&id))
            .await??
            .ok_or_else(|| anyhow!("artifact not found: {}", params.artifact_id))?;
        if current.storage != "inline" {
            anyhow::bail!("artifact diff is only available for inline text artifacts");
        }
        let to_version = params.to_version.unwrap_or(current.version);
        let store = self.store.clone();
        let id = params.artifact_id.clone();
        let from = tokio::task::spawn_blocking(move || {
            store.get_artifact_version(&id, params.from_version)
        })
        .await??
        .ok_or_else(|| {
            anyhow!(
                "artifact version not found: {}@{}",
                params.artifact_id,
                params.from_version
            )
        })?;
        if from.storage != "inline" {
            anyhow::bail!("artifact diff is only available for inline text artifacts");
        }
        let to = if to_version == current.version {
            let mut version = from.clone();
            version.version = current.version;
            version.name = current.name.clone();
            version.kind = current.kind.clone();
            version.mime_type = current.mime_type.clone();
            version.content = current.content.clone();
            version.storage = current.storage.clone();
            version.compression = current.compression.clone();
            version.size_bytes = current.size_bytes;
            version.sha256 = current.sha256.clone();
            version.path = current.path.clone();
            version.created_at = current.created_at.clone();
            version
        } else {
            let store = self.store.clone();
            let id = params.artifact_id.clone();
            tokio::task::spawn_blocking(move || store.get_artifact_version(&id, to_version))
                .await??
                .ok_or_else(|| {
                    anyhow!(
                        "artifact version not found: {}@{}",
                        params.artifact_id,
                        to_version
                    )
                })?
        };
        if to.storage != "inline" {
            anyhow::bail!("artifact diff is only available for inline text artifacts");
        }
        Ok(ArtifactDiffResult {
            artifact_id: current.id,
            name: current.name,
            from_version: from.version,
            to_version: to.version,
            diff: crate::textdiff::diff_text(
                &from.content,
                &to.content,
                params.context.unwrap_or(3) as usize,
                crate::textdiff::DEFAULT_MAX_DIFF_LINES,
            ),
        })
    }

    pub async fn artifact_versions(&self, artifact_id: &str) -> Result<ArtifactVersionsResult> {
        let store = self.store.clone();
        let artifact_id = artifact_id.to_string();
        let versions =
            tokio::task::spawn_blocking(move || store.list_artifact_versions(&artifact_id))
                .await??;
        Ok(ArtifactVersionsResult { versions })
    }

    pub async fn restore_artifact_version(
        &self,
        artifact_id: &str,
        version: u32,
    ) -> Result<Artifact> {
        let store = self.store.clone();
        let artifact_id = artifact_id.to_string();
        let artifact_id_for_error = artifact_id.clone();
        let Some((artifact, event)) = tokio::task::spawn_blocking(move || {
            store.restore_artifact_version(&artifact_id, version)
        })
        .await??
        else {
            anyhow::bail!("artifact version not found: {artifact_id_for_error}@{version}");
        };
        let _ = self.events.send(event);
        self.auto_gc_restarted_artifact(&artifact.session_id).await;
        Ok(artifact)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn publish_artifact_file(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<String>,
        name: &str,
        kind: &str,
        mime_type: &str,
        source_path: &Path,
        allowed_root: &Path,
    ) -> Result<Artifact> {
        let root = allowed_root.canonicalize()?;
        let source = source_path.canonicalize()?;
        if !source.starts_with(&root) {
            anyhow::bail!("artifact source is outside the agent worktree");
        }
        let data = tokio::fs::read(&source).await?;
        if data.len() > 5 * 1024 * 1024 {
            anyhow::bail!("binary artifact exceeds 5 MiB");
        }
        let name = name.trim();
        if name.is_empty() || name.len() > 200 {
            anyhow::bail!("artifact name must be 1-200 characters");
        }
        let sha256 = sha256_hex(&data);
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let agent_id = agent_id.to_string();
        let agent_name = agent_name.to_string();
        let name = name.to_string();
        let kind = kind.to_string();
        let mime_type = mime_type.to_string();
        let (artifact, event) = tokio::task::spawn_blocking(move || {
            store.create_artifact_file_with_event(
                &session_id,
                &agent_id,
                &agent_name,
                task_id.as_deref(),
                &name,
                &kind,
                &mime_type,
                &data,
                &sha256,
                None,
            )
        })
        .await??;
        let _ = self.events.send(event);
        self.auto_gc_restarted_artifact(&artifact.session_id).await;
        Ok(artifact)
    }

    /// Publish in-memory bytes as a binary artifact. This is the sanctioned path
    /// for daemon-produced screenshots: the source lives in daemon scratch, not
    /// in the agent worktree, and the 5 MiB cap matches `publish_artifact_file`.
    #[allow(clippy::too_many_arguments)]
    pub async fn publish_artifact_binary(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<String>,
        name: &str,
        kind: &str,
        mime_type: &str,
        data: &[u8],
    ) -> Result<Artifact> {
        self.publish_artifact_binary_with_provenance(
            session_id, agent_id, agent_name, task_id, name, kind, mime_type, data, None,
        )
        .await
    }

    /// Publish binary bytes with explicit derivation metadata. Archive
    /// extraction and media transcoding use this path so the resulting
    /// artifact records its source artifact, entry path, and operation without
    /// a separate mutable metadata table.
    #[allow(clippy::too_many_arguments)]
    pub async fn publish_artifact_binary_with_provenance(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<String>,
        name: &str,
        kind: &str,
        mime_type: &str,
        data: &[u8],
        provenance: Option<vp_protocol::types::ArtifactProvenance>,
    ) -> Result<Artifact> {
        if data.is_empty() {
            anyhow::bail!("binary artifact is empty");
        }
        if data.len() > 5 * 1024 * 1024 {
            anyhow::bail!("binary artifact exceeds 5 MiB");
        }
        let name = name.trim();
        if name.is_empty() || name.len() > 200 {
            anyhow::bail!("artifact name must be 1-200 characters");
        }
        let sha256 = crate::tooling::fs::sha256_hex(data);
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let agent_id = agent_id.to_string();
        let agent_name = agent_name.to_string();
        let name = name.to_string();
        let kind = kind.to_string();
        let mime_type = mime_type.to_string();
        let data = data.to_vec();
        let (artifact, event) = tokio::task::spawn_blocking(move || {
            store.create_artifact_file_with_event(
                &session_id,
                &agent_id,
                &agent_name,
                task_id.as_deref(),
                &name,
                &kind,
                &mime_type,
                &data,
                &sha256,
                provenance.as_ref(),
            )
        })
        .await??;
        let _ = self.events.send(event);
        self.auto_gc_restarted_artifact(&artifact.session_id).await;
        Ok(artifact)
    }

    async fn auto_gc_restarted_artifact(&self, session_id: &str) {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let project_id =
            match tokio::task::spawn_blocking(move || store.get_session(&session_id)).await {
                Ok(Ok(Some(session))) => session.project_id,
                _ => return,
            };
        let policy = match self.get_artifact_policy(&project_id).await {
            Ok(policy) if policy.auto_gc => policy,
            _ => return,
        };
        let _ = self.gc_artifacts(&project_id, &policy).await;
    }

    pub async fn get_artifact_policy(&self, project_id: &str) -> Result<ArtifactPolicy> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_artifact_policy(&project_id)).await?
    }

    pub async fn set_artifact_policy(
        &self,
        project_id: &str,
        policy: ArtifactPolicy,
    ) -> Result<ArtifactPolicy> {
        if policy.max_bytes.is_some_and(|value| value == 0) {
            anyhow::bail!("max_bytes must be greater than zero");
        }
        if policy.max_age_secs.is_some_and(|value| value == 0) {
            anyhow::bail!("max_age_secs must be greater than zero");
        }
        if policy.max_count.is_some_and(|value| value == 0) {
            anyhow::bail!("max_count must be greater than zero");
        }
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.get_project(&project_id)
        })
        .await??
        .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let for_store = policy.clone();
        tokio::task::spawn_blocking(move || store.set_artifact_policy(&project_id, &for_store))
            .await??;
        Ok(policy)
    }

    pub async fn gc_artifacts(
        &self,
        project_id: &str,
        policy: &ArtifactPolicy,
    ) -> Result<ArtifactGcResult> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let artifacts = tokio::task::spawn_blocking({
            let store = store.clone();
            let project_id = project_id.clone();
            move || store.list_artifacts_for_project(&project_id)
        })
        .await??;
        let now = time::OffsetDateTime::now_utc();
        let mut remove = Vec::new();
        let mut total_bytes: u64 = artifacts.iter().map(|artifact| artifact.size_bytes).sum();
        if let Some(max_age) = policy.max_age_secs {
            for artifact in &artifacts {
                if let Ok(created) = time::OffsetDateTime::parse(
                    &artifact.created_at,
                    &time::format_description::well_known::Rfc3339,
                ) && (now - created).whole_seconds().max(0) as u64 > max_age
                {
                    remove.push(artifact.id.clone());
                }
            }
        }
        if let Some(max_count) = policy.max_count
            && artifacts.len() as u64 > max_count
        {
            let keep = artifacts.len() - max_count as usize;
            remove.extend(
                artifacts
                    .iter()
                    .take(keep)
                    .map(|artifact| artifact.id.clone()),
            );
        }
        if let Some(max_bytes) = policy.max_bytes {
            for artifact in &artifacts {
                if total_bytes <= max_bytes {
                    break;
                }
                if !remove.contains(&artifact.id) {
                    remove.push(artifact.id.clone());
                    total_bytes = total_bytes.saturating_sub(artifact.size_bytes);
                } else {
                    total_bytes = total_bytes.saturating_sub(artifact.size_bytes);
                }
            }
        }
        remove.sort();
        remove.dedup();
        let mut bytes_freed = 0u64;
        for id in &remove {
            let sha256 = artifacts
                .iter()
                .find(|artifact| &artifact.id == id)
                .map(|artifact| artifact.sha256.clone())
                .unwrap_or_default();
            let store = self.store.clone();
            let id = id.clone();
            let pruned = tokio::task::spawn_blocking(move || store.delete_artifact_with_event(&id))
                .await??;
            if let Some((event, remove_path)) = pruned {
                let _ = self.events.send(event);
                if let Some(path) = remove_path {
                    let physical_size = tokio::fs::metadata(&path)
                        .await
                        .map(|metadata| metadata.len())
                        .unwrap_or(0);
                    match tokio::fs::remove_file(&path).await {
                        Ok(()) => bytes_freed = bytes_freed.saturating_add(physical_size),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            tracing::warn!(%error, path, "failed to remove artifact blob");
                        }
                    }
                    if !sha256.is_empty() {
                        let materialized = self
                            .store()
                            .data_dir()
                            .join("artifacts")
                            .join("materialized")
                            .join(&sha256);
                        let _ = tokio::fs::remove_file(materialized).await;
                    }
                }
            }
        }
        Ok(ArtifactGcResult {
            removed: remove.len(),
            bytes_freed,
        })
    }

    pub async fn backfill_artifact_events(
        &self,
        session_id: &str,
    ) -> Result<ArtifactBackfillResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let (artifacts, events) = tokio::task::spawn_blocking(move || {
            let artifacts = store.list_artifacts(&id)?;
            let events = store.list_all_events(&id)?;
            Ok::<_, anyhow::Error>((artifacts, events))
        })
        .await??;
        let mut full = std::collections::HashSet::new();
        for event in &events {
            if event.kind == "artifact.published"
                && event.payload.get("mime_type").is_some()
                && event.payload.get("sha256").is_some()
                && event.payload.get("content").is_some()
                && let Some(artifact_id) = event
                    .payload
                    .get("artifact_id")
                    .and_then(serde_json::Value::as_str)
            {
                full.insert(artifact_id.to_string());
            }
        }
        let mut backfilled = 0;
        for artifact in &artifacts {
            if full.contains(&artifact.id) {
                continue;
            }
            self.append_event(
                session_id,
                "artifact.published",
                artifact_event_payload(artifact),
            )
            .await?;
            backfilled += 1;
        }
        Ok(ArtifactBackfillResult {
            session_id: session_id.to_string(),
            scanned: artifacts.len(),
            backfilled,
        })
    }

    pub async fn list_artifacts(&self, session_id: &str) -> Result<Vec<Artifact>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.list_artifacts(&session_id)).await?
    }

    pub async fn get_artifact(&self, artifact_id: &str) -> Result<Option<Artifact>> {
        let store = self.store.clone();
        let artifact_id = artifact_id.to_string();
        tokio::task::spawn_blocking(move || store.get_artifact(&artifact_id)).await?
    }

    pub async fn materialize_artifact(
        &self,
        artifact_id: &str,
    ) -> Result<Option<ArtifactMaterialization>> {
        let store = self.store.clone();
        let id = artifact_id.to_string();
        let path = tokio::task::spawn_blocking(move || store.materialize_artifact(&id)).await??;
        Ok(path.map(|path| ArtifactMaterialization {
            artifact_id: artifact_id.to_string(),
            path: path.to_string_lossy().to_string(),
        }))
    }
}

fn artifact_event_payload(artifact: &Artifact) -> serde_json::Value {
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
        "provenance": artifact.provenance,
    })
}
