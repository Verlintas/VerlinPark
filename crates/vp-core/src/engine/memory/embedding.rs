use super::*;

impl Engine {
    /// Coverage/health for the optional semantic lane. Reads only derived
    /// tables and the project policy; never returns vectors or credentials.
    pub async fn memory_embedding_status(
        &self,
        project_id: &str,
    ) -> Result<MemoryEmbeddingsStatusResult> {
        self.ensure_project_exists(project_id).await?;
        let policy = self.get_memory_policy(project_id).await?.policy;
        let embedding = policy.embedding;
        let store = self.store.clone();
        let project = project_id.to_string();
        let embedding_for_store = embedding.clone();
        let mut status = tokio::task::spawn_blocking(move || {
            store.memory_embedding_status(&project, embedding_for_store.as_ref())
        })
        .await??;
        if let Some(policy) = embedding.as_ref()
            && policy.enabled
            && policy.is_configured()
            && let Some(name) = policy
                .api_key_env
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
        {
            let missing = std::env::var(name)
                .ok()
                .is_none_or(|value| value.trim().is_empty());
            if missing {
                status.last_error = Some(format!(
                    "embedding API key environment variable {name} is not set"
                ));
            }
        }
        let store = self.store.clone();
        let project = project_id.to_string();
        let index_policy = embedding.clone();
        let index = tokio::task::spawn_blocking(move || {
            crate::memory_index::index_status(&store, &project, index_policy.as_ref())
        })
        .await??;
        status.index = Some(index);
        Ok(status)
    }

    /// Clear derived vectors/jobs and queue every active/stale memory. This is
    /// an explicit operator action; it never deletes memories.
    pub async fn memory_embedding_rebuild(
        &self,
        project_id: &str,
    ) -> Result<MemoryEmbeddingsRebuildResult> {
        self.ensure_project_exists(project_id).await?;
        let policy = self.get_memory_policy(project_id).await?.policy;
        let embedding = policy.embedding;
        let enabled = embedding
            .as_ref()
            .is_some_and(|embedding| embedding.enabled && embedding.is_configured());
        if let Some(embedding) = embedding.as_ref() {
            crate::memory_embedding::validate_embedding_policy(embedding)?;
        }
        // Clearing is always allowed, including while disabled or removed. This
        // is the operator's privacy/cleanup path: it removes derived vectors
        // and jobs without touching memory truth or usage accounting.
        let store = self.store.clone();
        let project = project_id.to_string();
        let cleared =
            tokio::task::spawn_blocking(move || store.clear_memory_embeddings(&project)).await??;
        let model = embedding
            .as_ref()
            .and_then(|embedding| embedding.model.clone())
            .filter(|model| !model.trim().is_empty());
        let dimensions = embedding
            .as_ref()
            .map_or(1536, |embedding| embedding.dimensions);
        let enqueued = if enabled {
            let model_for_queue = model
                .clone()
                .ok_or_else(|| anyhow::anyhow!("embedding model is missing"))?;
            let store = self.store.clone();
            let project = project_id.to_string();
            tokio::task::spawn_blocking(move || {
                store.enqueue_memory_embedding_backlog(&project, &model_for_queue)
            })
            .await??
        } else {
            0
        };
        Ok(MemoryEmbeddingsRebuildResult {
            project_id: project_id.to_string(),
            enabled,
            model,
            dimensions,
            cleared,
            enqueued,
        })
    }

    /// Explicit connectivity probe. Unlike indexing/recall this is allowed for
    /// a configured-but-disabled policy: the operator asked for one request.
    /// Toggle `enabled` when the probe succeeds. No vector is returned.
    pub async fn memory_embedding_test(
        &self,
        project_id: &str,
        text: Option<&str>,
    ) -> Result<MemoryEmbeddingsTestResult> {
        self.ensure_project_exists(project_id).await?;
        let policy = self.get_memory_policy(project_id).await?.policy;
        let Some(embedding) = policy.embedding else {
            anyhow::bail!("embedding policy is not configured");
        };
        if !embedding.is_configured() {
            anyhow::bail!("embedding policy needs endpoint and model");
        }
        crate::memory_embedding::validate_embedding_policy(&embedding)?;
        let client = crate::memory_embedding::EmbeddingClient::from_policy(&embedding)?;
        let probe = text
            .map(crate::memory::embedding_query_text)
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "verlinpark embedding readiness probe".to_string());
        let started = std::time::Instant::now();
        let result = client.embed(std::slice::from_ref(&probe)).await;
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let model = embedding.model.clone();
        let dimensions = embedding.dimensions;
        match result {
            Ok(batch) => {
                let store = self.store.clone();
                let project = project_id.to_string();
                let usage = batch.usage.clone();
                let model_for_usage = model.clone().unwrap_or_default();
                if usage.calls > 0 {
                    let _ = tokio::task::spawn_blocking(move || {
                        store.record_memory_embedding_usage(
                            &project,
                            &model_for_usage,
                            usage.calls,
                            usage.input_tokens,
                            usage.estimated_tokens,
                            usage.bytes,
                            usage.provider_reported,
                        )
                    })
                    .await;
                }
                let item = batch.items.into_iter().next();
                let vector_dimensions = item
                    .as_ref()
                    .and_then(|item| item.vector.as_ref())
                    .map(|vector| u32::try_from(vector.len()).unwrap_or(u32::MAX));
                let error = item.and_then(|item| item.error);
                Ok(MemoryEmbeddingsTestResult {
                    project_id: project_id.to_string(),
                    ok: vector_dimensions.is_some(),
                    model,
                    dimensions: vector_dimensions.unwrap_or(dimensions),
                    latency_ms,
                    input_tokens: batch.usage.input_tokens,
                    estimated_tokens: batch.usage.estimated_tokens,
                    provider_reported: batch.usage.provider_reported,
                    error: error
                        .map(|error| crate::memory_embedding::sanitize_embedding_error(&error)),
                })
            }
            Err(error) => Ok(MemoryEmbeddingsTestResult {
                project_id: project_id.to_string(),
                ok: false,
                model,
                dimensions,
                latency_ms,
                input_tokens: 0,
                estimated_tokens: 0,
                provider_reported: false,
                error: Some(crate::memory_embedding::sanitize_embedding_error(
                    &error.to_string(),
                )),
            }),
        }
    }

    /// Daemon reconciler: process one bounded batch for each enabled project.
    /// Returns `(projects, indexed, failed, skipped)`.
    pub async fn reconcile_memory_embeddings(&self) -> Result<(usize, usize, usize, usize)> {
        let store = self.store.clone();
        let policies =
            tokio::task::spawn_blocking(move || store.list_memory_embedding_policies()).await??;
        let mut projects = 0usize;
        let mut indexed = 0usize;
        let mut failed = 0usize;
        let mut skipped = 0usize;
        for (project_id, policy) in policies {
            if !policy.enabled || !policy.is_configured() {
                continue;
            }
            match self
                .reconcile_project_memory_embeddings(&project_id, policy)
                .await
            {
                Ok((did_index, did_fail, did_skip)) => {
                    projects += 1;
                    indexed += did_index;
                    failed += did_fail;
                    skipped += did_skip;
                }
                Err(error) => {
                    tracing::warn!(%error, %project_id, "memory embedding reconciliation failed");
                }
            }
        }
        Ok((projects, indexed, failed, skipped))
    }

    /// One bounded indexing batch. Content hashes are recomputed before any
    /// network call; unchanged rows transition to `skipped` without a request.
    pub async fn reconcile_project_memory_embeddings(
        &self,
        project_id: &str,
        policy: MemoryEmbeddingPolicy,
    ) -> Result<(usize, usize, usize)> {
        if !policy.enabled {
            return Ok((0, 0, 0));
        }
        // Index maintenance does not depend on the embedding endpoint. Run one
        // bounded pass before the client is built so a corrupt/stale index can
        // still repair itself while the endpoint or API key is unavailable.
        self.maintain_project_memory_index(project_id, &policy)
            .await;
        let runtime = crate::memory_embedding::EmbeddingRuntimePolicy::from_policy(&policy)?;
        let client = match crate::memory_embedding::EmbeddingClient::from_policy(&policy) {
            Ok(client) => client,
            Err(error) => {
                // Configuration failures are retried after the operator fixes
                // them. They are not recorded as per-memory embedding failures,
                // so a missing key does not burn every job's retry budget.
                return Err(anyhow::anyhow!("{error}"));
            }
        };
        let model = runtime.model.clone();
        let dimensions = runtime.dimensions;
        let scan_limit = runtime
            .batch_size
            .saturating_mul(2)
            .clamp(runtime.batch_size, 128);
        let store = self.store.clone();
        let project = project_id.to_string();
        let model_for_scan = model.clone();
        let candidates = tokio::task::spawn_blocking(move || {
            store.memory_embedding_index_candidates(
                &project,
                &model_for_scan,
                dimensions,
                crate::memory_embedding::MAX_EMBEDDING_INDEX_ATTEMPTS,
                scan_limit,
            )
        })
        .await??;

        let mut needs = Vec::new();
        let mut skipped = 0usize;
        for candidate in candidates {
            let text = crate::memory::embedding_text(&candidate.memory);
            let content_hash = crate::memory::embedding_content_hash(&text);
            let current = candidate.embedded_model.as_deref() == Some(model.as_str())
                && candidate.embedded_dimensions == Some(dimensions)
                && candidate.embedded_content_hash.as_deref() == Some(content_hash.as_str());
            if current {
                let store = self.store.clone();
                let memory_id = candidate.memory.id.clone();
                let model = model.clone();
                if let Ok(Ok(Some(event))) = tokio::task::spawn_blocking(move || {
                    store.record_memory_embedding_skip(&memory_id, &model, "content hash unchanged")
                })
                .await
                {
                    let _ = self.events.send(event);
                }
                skipped += 1;
                continue;
            }
            let reset_attempts = candidate
                .job_model
                .as_deref()
                .is_some_and(|job_model| job_model != model)
                || candidate
                    .embedded_model
                    .as_deref()
                    .is_some_and(|embedded_model| embedded_model != model)
                || candidate
                    .embedded_dimensions
                    .is_some_and(|embedded_dimensions| embedded_dimensions != dimensions);
            needs.push((candidate.memory.id, text, content_hash, reset_attempts));
            if needs.len() >= runtime.batch_size as usize {
                break;
            }
        }
        if needs.is_empty() {
            self.maintain_project_memory_index(project_id, &policy)
                .await;
            return Ok((0, 0, skipped));
        }

        let mut attempted = Vec::with_capacity(needs.len());
        for (memory_id, _text, _content_hash, reset_attempts) in &needs {
            let store = self.store.clone();
            let memory_id = memory_id.clone();
            let model = model.clone();
            let reset_attempts = *reset_attempts;
            let attempts = tokio::task::spawn_blocking(move || {
                store.mark_memory_embedding_attempted(&memory_id, &model, reset_attempts)
            })
            .await??;
            attempted.push(attempts);
        }

        let texts = needs
            .iter()
            .map(|(_, text, _, _)| text.clone())
            .collect::<Vec<_>>();
        let result = client.embed(&texts).await;
        let mut indexed = 0usize;
        let mut failed = 0usize;
        let mut failure_events = Vec::new();
        match result {
            Ok(batch) => {
                let store = self.store.clone();
                let project = project_id.to_string();
                let usage = batch.usage.clone();
                let model_for_usage = model.clone();
                if usage.calls > 0 {
                    let _ = tokio::task::spawn_blocking(move || {
                        store.record_memory_embedding_usage(
                            &project,
                            &model_for_usage,
                            usage.calls,
                            usage.input_tokens,
                            usage.estimated_tokens,
                            usage.bytes,
                            usage.provider_reported,
                        )
                    })
                    .await;
                }
                for (index, (memory_id, _, content_hash, _)) in needs.iter().enumerate() {
                    let item = batch.items.get(index);
                    let vector = item.and_then(|item| item.vector.as_ref());
                    let item_error = item.and_then(|item| item.error.clone());
                    match vector {
                        Some(vector) => match crate::memory::encode_embedding_vector(vector) {
                            Ok(encoded) => {
                                let store = self.store.clone();
                                let memory_id = memory_id.clone();
                                let model = model.clone();
                                let content_hash = content_hash.clone();
                                if let Ok(Ok(Some(event))) =
                                    tokio::task::spawn_blocking(move || {
                                        store.record_memory_embedding_success(
                                            &memory_id,
                                            &model,
                                            dimensions,
                                            &content_hash,
                                            &encoded,
                                        )
                                    })
                                    .await
                                {
                                    let _ = self.events.send(event);
                                    indexed += 1;
                                } else {
                                    failed += 1;
                                }
                            }
                            Err(error) => {
                                let error = crate::memory_embedding::sanitize_embedding_error(
                                    &error.to_string(),
                                );
                                if let Some(event) = self
                                    .record_embedding_failure(
                                        memory_id,
                                        &model,
                                        attempted[index],
                                        &error,
                                        false,
                                    )
                                    .await?
                                {
                                    let _ = self.events.send(event);
                                }
                                failed += 1;
                            }
                        },
                        None => {
                            let error = item_error.unwrap_or_else(|| {
                                "embedding endpoint returned no vector".to_string()
                            });
                            let error = crate::memory_embedding::sanitize_embedding_error(&error);
                            if let Some(event) = self
                                .record_embedding_failure(
                                    memory_id,
                                    &model,
                                    attempted[index],
                                    &error,
                                    false,
                                )
                                .await?
                            {
                                failure_events.push(event);
                            }
                            failed += 1;
                        }
                    }
                }
                for event in failure_events {
                    let _ = self.events.send(event);
                }
            }
            Err(error) => {
                let terminal = !error.retryable_http();
                let error = crate::memory_embedding::sanitize_embedding_error(&error.to_string());
                for (index, (memory_id, _, _, _)) in needs.iter().enumerate() {
                    if let Some(event) = self
                        .record_embedding_failure(
                            memory_id,
                            &model,
                            attempted[index],
                            &error,
                            terminal,
                        )
                        .await?
                    {
                        failure_events.push(event);
                    }
                    failed += 1;
                }
                for event in failure_events {
                    let _ = self.events.send(event);
                }
            }
        }
        self.maintain_project_memory_index(project_id, &policy)
            .await;
        Ok((indexed, failed, skipped))
    }

    async fn record_embedding_failure(
        &self,
        memory_id: &str,
        model: &str,
        attempts: u32,
        error: &str,
        terminal: bool,
    ) -> Result<Option<Event>> {
        let next_attempt_at =
            if terminal || attempts >= crate::memory_embedding::MAX_EMBEDDING_INDEX_ATTEMPTS {
                None
            } else {
                let delay = crate::memory_embedding::index_retry_delay(attempts);
                Some(
                    (time::OffsetDateTime::now_utc()
                        + time::Duration::seconds(i64::try_from(delay.as_secs()).unwrap_or(3_600)))
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_else(|_| vp_protocol::now_rfc3339()),
                )
            };
        let store = self.store.clone();
        let memory_id = memory_id.to_string();
        let model = model.to_string();
        let error = error.to_string();
        tokio::task::spawn_blocking(move || {
            store.record_memory_embedding_failure(
                &memory_id,
                &model,
                attempts,
                &error,
                next_attempt_at.as_deref(),
            )
        })
        .await?
    }
}
