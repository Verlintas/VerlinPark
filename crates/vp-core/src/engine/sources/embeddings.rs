use super::*;

// ---- Semantic source index -----------------------------------------

impl Engine {
    /// Read the opt-in source embedding policy. No request is made.
    pub async fn get_source_embedding_policy(
        &self,
        project_id: &str,
    ) -> Result<SourceEmbeddingPolicyResult> {
        self.ensure_project_exists(project_id).await?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let policy =
            tokio::task::spawn_blocking(move || store.get_source_embedding_policy(&project))
                .await??;
        Ok(SourceEmbeddingPolicyResult {
            project_id: project_id.to_string(),
            policy,
        })
    }

    /// Persist a validated policy. Enabling is explicit: this never makes a
    /// network call and never queues a backlog. Operators call
    /// `source_embedding_reindex` after enabling to index existing sources.
    pub async fn set_source_embedding_policy(
        &self,
        project_id: &str,
        mut policy: SourceEmbeddingPolicy,
    ) -> Result<SourceEmbeddingPolicyResult> {
        self.ensure_project_exists(project_id).await?;
        crate::source_embedding::normalize_source_embedding_policy(&mut policy);
        crate::source_embedding::validate_source_embedding_policy(&policy)?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let stored = policy.clone();
        tokio::task::spawn_blocking(move || store.set_source_embedding_policy(&project, &stored))
            .await??;
        Ok(SourceEmbeddingPolicyResult {
            project_id: project_id.to_string(),
            policy,
        })
    }

    /// Bounded operational status for the source semantic lane. It contains
    /// counts, stable fallback reasons, and environment *names*, never vectors,
    /// embedded text, endpoint values, or credentials.
    pub async fn source_embedding_status(
        &self,
        project_id: &str,
    ) -> Result<SourceEmbeddingStatusResult> {
        self.ensure_project_exists(project_id).await?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let policy =
            tokio::task::spawn_blocking(move || store.get_source_embedding_policy(&project))
                .await??;
        let store = self.store.clone();
        let project = project_id.to_string();
        let policy_for_store = policy.clone();
        let mut status = tokio::task::spawn_blocking(move || {
            store.source_embedding_status(&project, Some(&policy_for_store))
        })
        .await??;
        status.endpoint_env_present = policy.endpoint_env_name().is_some_and(|name| {
            std::env::var(name)
                .ok()
                .is_some_and(|value| !value.trim().is_empty())
        });
        status.api_key_env_present = policy
            .api_key_env_name()
            .map(|name| {
                std::env::var(name)
                    .ok()
                    .is_some_and(|value| !value.trim().is_empty())
            })
            .unwrap_or(true);
        status.semantic_unavailable = crate::source_embedding::source_semantic_unavailable(
            Some(&policy),
            status.indexed_chunks,
            status.stale_chunks,
        );
        Ok(status)
    }

    /// Read the additive retrieval v2 policy. No request or index work occurs.
    pub async fn get_source_retrieval_policy(
        &self,
        project_id: &str,
    ) -> Result<SourceRetrievalPolicyResult> {
        self.ensure_project_exists(project_id).await?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let policy =
            tokio::task::spawn_blocking(move || store.get_source_retrieval_policy(&project))
                .await??;
        Ok(SourceRetrievalPolicyResult {
            project_id: project_id.to_string(),
            policy,
        })
    }

    /// Persist a normalized, validated retrieval policy. Disabling ANN keeps
    /// the derived index on disk but never uses it; a later enable can reuse it
    /// once maintenance validates the generation. Rerank and expansion changes
    /// take effect on the next search and never rebuild vectors.
    pub async fn set_source_retrieval_policy(
        &self,
        project_id: &str,
        mut policy: SourceRetrievalPolicy,
    ) -> Result<SourceRetrievalPolicyResult> {
        self.ensure_project_exists(project_id).await?;
        crate::source_retrieval::normalize_source_retrieval_policy(&mut policy);
        crate::source_retrieval::validate_source_retrieval_policy(&policy)?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let stored = policy.clone();
        tokio::task::spawn_blocking(move || store.set_source_retrieval_policy(&project, &stored))
            .await??;
        Ok(SourceRetrievalPolicyResult {
            project_id: project_id.to_string(),
            policy,
        })
    }

    /// Bounded operational status for the derived source IVF index. It contains
    /// counts, generations, configuration, and stable reasons only; centroid
    /// bytes and vectors are never serialized.
    pub async fn source_index_status(&self, project_id: &str) -> Result<SourceIndexStatusResult> {
        self.ensure_project_exists(project_id).await?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let (embedding, retrieval) = tokio::task::spawn_blocking(move || {
            let embedding = store.get_source_embedding_policy(&project)?;
            let retrieval = store.get_source_retrieval_policy(&project)?;
            Ok::<_, anyhow::Error>((embedding, retrieval))
        })
        .await??;
        let model = embedding
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let dimensions = embedding.dimensions;
        let embedding_enabled = embedding.enabled;
        let embedding_configured = embedding.is_configured();
        let max_chunks = embedding
            .max_chunks_per_source
            .clamp(1, crate::source_embedding::MAX_SOURCE_EMBEDDING_CHUNKS);
        let store = self.store.clone();
        let project = project_id.to_string();
        let retrieval_for_status = retrieval.clone();
        let model_for_status = model.clone();
        let status = tokio::task::spawn_blocking(move || {
            crate::source_ann::source_ann_status(
                &store,
                &project,
                &retrieval_for_status,
                embedding_enabled,
                embedding_configured,
                model_for_status.as_deref(),
                dimensions,
                max_chunks,
            )
        })
        .await??;
        let ann_enabled = retrieval.ann_enabled;
        Ok(SourceIndexStatusResult {
            project_id: project_id.to_string(),
            policy: retrieval,
            embedding_enabled,
            embedding_configured,
            model,
            dimensions,
            ann_enabled,
            ready: status.ready,
            path: status.path,
            generation: status.generation,
            vector_count: status.vector_count,
            assigned_vectors: status.assigned_vectors,
            invalid_assignments: status.invalid_assignments,
            centroid_count: status.centroid_count,
            nlist: status.nlist,
            nprobe: status.nprobe,
            min_vectors: status.min_vectors,
            coverage: status.coverage,
            bytes: status.bytes,
            fallback_reason: status.fallback_reason,
            last_rebuilt_at: status.last_rebuilt_at,
            last_error: status.last_error,
        })
    }

    /// Explicit operator rebuild. Generation-fenced and idempotent for
    /// unchanged vector data. It never makes an embedding request and never
    /// writes source content.
    pub async fn rebuild_source_index(&self, project_id: &str) -> Result<SourceIndexRebuildResult> {
        self.ensure_project_exists(project_id).await?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let (embedding, retrieval) = tokio::task::spawn_blocking(move || {
            let embedding = store.get_source_embedding_policy(&project)?;
            let retrieval = store.get_source_retrieval_policy(&project)?;
            Ok::<_, anyhow::Error>((embedding, retrieval))
        })
        .await??;
        let model = embedding
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_default()
            .to_string();
        let dimensions = embedding.dimensions;
        let max_chunks = embedding
            .max_chunks_per_source
            .clamp(1, crate::source_embedding::MAX_SOURCE_EMBEDDING_CHUNKS);
        let store = self.store.clone();
        let project = project_id.to_string();
        let retrieval_for_rebuild = retrieval.clone();
        let result = tokio::task::spawn_blocking(move || {
            crate::source_ann::rebuild_source_ann(
                &store,
                &project,
                &retrieval_for_rebuild,
                &model,
                dimensions,
                max_chunks,
            )
        })
        .await?;
        match result {
            Ok(report) => Ok(SourceIndexRebuildResult {
                project_id: project_id.to_string(),
                rebuilt: report.rebuilt,
                reason: report.reason,
                generation: report.generation,
                vector_count: report.vector_count,
                assigned_vectors: report.assigned_vectors,
                centroid_count: report.centroid_count,
                nlist: report.nlist,
                nprobe: report.nprobe,
                path: report.path,
            }),
            Err(error) => {
                let store = self.store.clone();
                let project = project_id.to_string();
                let sanitized =
                    crate::source_embedding::sanitized_source_embedding_error(&error.to_string());
                let _ = tokio::task::spawn_blocking(move || {
                    store.record_source_ann_error(&project, &sanitized)
                })
                .await;
                Err(error)
            }
        }
    }

    /// Explicit operator reindex. Derived vectors/jobs are cleared first. When
    /// the lane is active, every indexed source is queued for a fresh
    /// generation; when it is disabled or incomplete this is a cleanup action.
    /// No embedding request is made here.
    pub async fn reindex_source_embeddings(
        &self,
        project_id: &str,
    ) -> Result<SourceEmbeddingReindexResult> {
        self.ensure_project_exists(project_id).await?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let mut policy =
            tokio::task::spawn_blocking(move || store.get_source_embedding_policy(&project))
                .await??;
        crate::source_embedding::normalize_source_embedding_policy(&mut policy);
        if policy.is_configured() {
            crate::source_embedding::validate_source_embedding_policy(&policy)?;
        }
        let store = self.store.clone();
        let project = project_id.to_string();
        let (cleared_vectors, cleared_jobs) =
            tokio::task::spawn_blocking(move || store.clear_source_embeddings(&project)).await??;
        let enabled = policy.is_active();
        let model = policy.model.clone();
        let max_chunks = policy
            .max_chunks_per_source
            .clamp(1, crate::source_embedding::MAX_SOURCE_EMBEDDING_CHUNKS);
        let enqueued = if let (true, Some(model)) = (enabled, model.as_deref()) {
            let store = self.store.clone();
            let project = project_id.to_string();
            let model = model.to_string();
            tokio::task::spawn_blocking(move || {
                store.enqueue_source_embedding_backlog(&project, &model, max_chunks)
            })
            .await??
        } else {
            0
        };
        Ok(SourceEmbeddingReindexResult {
            project_id: project_id.to_string(),
            enabled,
            model,
            cleared_vectors,
            cleared_jobs,
            enqueued_sources: enqueued,
            max_chunks_per_source: max_chunks,
        })
    }

    /// Daemon reconciler over every explicitly enabled project. Returns
    /// `(projects, indexed_chunks, failed_chunks, completed_jobs)`.
    pub async fn reconcile_source_embeddings(&self) -> Result<(usize, usize, usize, usize)> {
        let store = self.store.clone();
        let policies =
            tokio::task::spawn_blocking(move || store.list_enabled_source_embedding_policies())
                .await??;
        let mut projects = 0usize;
        let mut indexed = 0usize;
        let mut failed = 0usize;
        let mut done = 0usize;
        for (project_id, policy) in policies {
            match self
                .reconcile_project_source_embeddings(&project_id, policy)
                .await
            {
                Ok((did_index, did_fail, did_done)) => {
                    projects += 1;
                    indexed += did_index;
                    failed += did_fail;
                    done += did_done;
                }
                Err(error) => {
                    tracing::warn!(%error, %project_id, "source embedding reconciliation failed");
                }
            }
        }
        Ok((projects, indexed, failed, done))
    }

    /// One bounded project tick. Configuration failures (missing env vars) are
    /// returned without burning a job's retry budget. Content/network failures
    /// advance the job with exponential backoff up to a dead letter.
    pub async fn reconcile_project_source_embeddings(
        &self,
        project_id: &str,
        policy: SourceEmbeddingPolicy,
    ) -> Result<(usize, usize, usize)> {
        if !policy.is_active() {
            return Ok((0, 0, 0));
        }
        let client = crate::source_embedding::SourceEmbeddingClient::from_policy(&policy)
            .map_err(|error| anyhow!("source embedding unavailable: {}", error.reason))?;
        let model = client.model().to_string();
        let dimensions = client.dimensions();
        let max_chunks = policy
            .max_chunks_per_source
            .clamp(1, crate::source_embedding::MAX_SOURCE_EMBEDDING_CHUNKS);
        let batch_size = policy
            .batch_size
            .clamp(1, crate::source_embedding::MAX_SOURCE_EMBEDDING_BATCH);
        let now = now_rfc3339();
        let stale_before = crate::source_embedding::SOURCE_EMBEDDING_JOB_STALE_SECS;
        let stale_before = source_embedding_time_before(stale_before);
        let store = self.store.clone();
        let project = project_id.to_string();
        let model_for_jobs = model.clone();
        let jobs = tokio::task::spawn_blocking(move || {
            store.due_source_embedding_jobs(
                &project,
                &model_for_jobs,
                crate::source_embedding::MAX_SOURCE_EMBEDDING_ATTEMPTS,
                &now,
                &stale_before,
                crate::source_embedding::MAX_SOURCE_EMBEDDING_JOBS_PER_TICK,
            )
        })
        .await??;
        let mut indexed = 0usize;
        let mut failed = 0usize;
        let mut done = 0usize;
        for job in jobs {
            let store = self.store.clone();
            let source_id = job.source_id.clone();
            let chunks = tokio::task::spawn_blocking(move || {
                store.source_embedding_chunks(&source_id, max_chunks)
            })
            .await??;
            if chunks.is_empty() {
                let store = self.store.clone();
                let source_id = job.source_id.clone();
                let generation = job.generation;
                let _ = tokio::task::spawn_blocking(move || {
                    store.record_source_embedding_job_done(&source_id, generation, 0)
                })
                .await?;
                done += 1;
                continue;
            }
            let mut current = 0u32;
            let mut missing: Vec<(SourceEmbeddingChunkRow, String, String)> = Vec::new();
            for row in &chunks {
                let text = crate::source_embedding::source_chunk_embedding_text(
                    &row.source_title,
                    &SourceChunk {
                        id: row.chunk_id.clone(),
                        source_id: row.source_id.clone(),
                        chunk_no: row.chunk_no,
                        heading: row.heading.clone(),
                        content: row.content.clone(),
                        start_offset: 0,
                        end_offset: 0,
                        token_count: row.token_count,
                    },
                );
                let hash = crate::source_embedding::source_chunk_embedding_hash(&text);
                let valid = row.generation == job.generation
                    && row.vector_model.as_deref() == Some(model.as_str())
                    && row.vector_dimensions == Some(dimensions)
                    && row.vector_generation == Some(job.generation)
                    && row.vector_status.as_deref() == Some("current")
                    && row.vector_content_hash.as_deref() == Some(hash.as_str());
                if valid {
                    current += 1;
                } else {
                    missing.push((row.clone(), text, hash));
                }
            }
            let chunks_total = u32::try_from(chunks.len()).unwrap_or(u32::MAX);
            if missing.is_empty() {
                let store = self.store.clone();
                let source_id = job.source_id.clone();
                let generation = job.generation;
                let _ = tokio::task::spawn_blocking(move || {
                    store.record_source_embedding_job_done(&source_id, generation, chunks_total)
                })
                .await?;
                done += 1;
                continue;
            }
            let batch_len = missing.len().min(batch_size as usize);
            let has_more = missing.len() > batch_len;
            let batch = missing.into_iter().take(batch_len).collect::<Vec<_>>();
            let texts = batch
                .iter()
                .map(|(_, text, _)| text.clone())
                .collect::<Vec<_>>();
            let store = self.store.clone();
            let source_id = job.source_id.clone();
            let generation = job.generation;
            let marked_running = tokio::task::spawn_blocking(move || {
                store.mark_source_embedding_job_running(&source_id, generation)
            })
            .await??;
            if !marked_running {
                continue;
            }
            let result = client.embed(&texts).await;
            let mut chunks_done = current;
            let mut item_failures: Vec<(bool, String)> = Vec::new();
            let mut obsolete = false;
            match result {
                Ok(batch_result) => {
                    let store = self.store.clone();
                    let project = project_id.to_string();
                    let usage = batch_result.usage.clone();
                    let model_for_usage = model.clone();
                    if usage.calls > 0 {
                        let _ = tokio::task::spawn_blocking(move || {
                            store.record_source_embedding_usage(
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
                    for (index, (row, _text, hash)) in batch.iter().enumerate() {
                        let item = batch_result.items.get(index);
                        let vector = item.and_then(|item| item.vector.as_ref());
                        let item_error = item.and_then(|item| item.error.clone());
                        match vector {
                            Some(vector) => {
                                match crate::source_embedding::encode_source_vector(vector) {
                                    Ok(encoded) => {
                                        let store = self.store.clone();
                                        let source_id = row.source_id.clone();
                                        let chunk_id = row.chunk_id.clone();
                                        let chunk_no = row.chunk_no;
                                        let generation = job.generation;
                                        let model = model.clone();
                                        let hash = hash.clone();
                                        match tokio::task::spawn_blocking(move || {
                                            store.record_source_embedding_success(
                                                &source_id, &chunk_id, chunk_no, generation,
                                                &model, dimensions, &hash, &encoded,
                                            )
                                        })
                                        .await
                                        {
                                            Ok(Ok(true)) => {
                                                chunks_done = chunks_done.saturating_add(1);
                                                indexed += 1;
                                            }
                                            Ok(Ok(false)) => {
                                                obsolete = true;
                                                break;
                                            }
                                            Ok(Err(error)) => {
                                                item_failures.push((
                                                    false,
                                                    format!("storage_failed: {error}"),
                                                ));
                                            }
                                            Err(error) => {
                                                item_failures.push((
                                                    false,
                                                    format!("storage_task_failed: {error}"),
                                                ));
                                            }
                                        }
                                    }
                                    Err(error) => item_failures.push((
                                        false,
                                        crate::source_embedding::source_embedding_item_error_code(
                                            &error.to_string(),
                                        ),
                                    )),
                                }
                            }
                            None => {
                                let error = item_error
                                    .unwrap_or_else(|| "endpoint returned no vector".to_string());
                                item_failures.push((
                                    crate::source_embedding::source_embedding_item_error_retryable(
                                        &error,
                                    ),
                                    crate::source_embedding::source_embedding_item_error_code(
                                        &error,
                                    ),
                                ));
                            }
                        }
                    }
                    if obsolete {
                        continue;
                    }
                    let store = self.store.clone();
                    let source_id = job.source_id.clone();
                    let generation = job.generation;
                    let _ = tokio::task::spawn_blocking(move || {
                        store.record_source_embedding_progress(&source_id, generation, chunks_done)
                    })
                    .await;
                    if item_failures.is_empty() {
                        if has_more {
                            let store = self.store.clone();
                            let source_id = job.source_id.clone();
                            let generation = job.generation;
                            let _ = tokio::task::spawn_blocking(move || {
                                store.mark_source_embedding_job_pending(
                                    &source_id,
                                    generation,
                                    chunks_done,
                                )
                            })
                            .await;
                        } else {
                            let store = self.store.clone();
                            let source_id = job.source_id.clone();
                            let generation = job.generation;
                            let _ = tokio::task::spawn_blocking(move || {
                                store.record_source_embedding_job_done(
                                    &source_id,
                                    generation,
                                    chunks_total,
                                )
                            })
                            .await;
                            done += 1;
                        }
                    } else {
                        failed += item_failures.len();
                        let attempts = job.attempts.saturating_add(1);
                        let terminal = item_failures.iter().any(|(retryable, _)| !retryable)
                            || attempts >= crate::source_embedding::MAX_SOURCE_EMBEDDING_ATTEMPTS;
                        let next_attempt_at = if terminal {
                            None
                        } else {
                            Some(source_embedding_retry_at(attempts))
                        };
                        let error = item_failures
                            .iter()
                            .take(3)
                            .map(|(_, code)| code.as_str())
                            .collect::<Vec<_>>()
                            .join("; ");
                        let store = self.store.clone();
                        let source_id = job.source_id.clone();
                        let generation = job.generation;
                        let _ = tokio::task::spawn_blocking(move || {
                            store.record_source_embedding_failure(
                                &source_id,
                                generation,
                                attempts,
                                &error,
                                next_attempt_at.as_deref(),
                                terminal,
                            )
                        })
                        .await;
                    }
                }
                Err(error) => {
                    let attempts = job.attempts.saturating_add(1);
                    let terminal = !error.retryable_http()
                        || attempts >= crate::source_embedding::MAX_SOURCE_EMBEDDING_ATTEMPTS;
                    let next_attempt_at = if terminal {
                        None
                    } else {
                        Some(source_embedding_retry_at(attempts))
                    };
                    let code = crate::source_embedding::source_embedding_error_code(&error);
                    let store = self.store.clone();
                    let source_id = job.source_id.clone();
                    let generation = job.generation;
                    let _ = tokio::task::spawn_blocking(move || {
                        store.record_source_embedding_failure(
                            &source_id,
                            generation,
                            attempts,
                            &code,
                            next_attempt_at.as_deref(),
                            terminal,
                        )
                    })
                    .await;
                    failed += batch_len;
                }
            }
        }
        // Retention is cheap metadata cleanup and intentionally best-effort.
        let store = self.store.clone();
        let project = project_id.to_string();
        let retention = policy.retention_days;
        if let Ok(Ok((vectors, jobs))) = tokio::task::spawn_blocking(move || {
            store.sweep_source_embedding_retention(&project, retention)
        })
        .await
            && (vectors > 0 || jobs > 0)
        {
            tracing::debug!(
                vectors,
                jobs,
                %project_id,
                "pruned stale source embedding metadata"
            );
        }
        // Derived ANN maintenance is best-effort and never changes the
        // embedding result. It runs only when the project explicitly opted in,
        // so a default project does no extra SQL or CPU work.
        let store = self.store.clone();
        let project = project_id.to_string();
        if let Ok(Ok(retrieval)) =
            tokio::task::spawn_blocking(move || store.get_source_retrieval_policy(&project)).await
            && retrieval.ann_enabled
        {
            let store = self.store.clone();
            let project = project_id.to_string();
            let model = model.clone();
            match tokio::task::spawn_blocking(move || {
                crate::source_ann::maintain_source_ann(
                    &store, &project, &retrieval, &model, dimensions, max_chunks,
                )
            })
            .await
            {
                Ok(Ok(maintenance)) => {
                    if maintenance.rebuilt_generation.is_some()
                        || maintenance.assigned_vectors > 0
                        || maintenance.repaired_assignments > 0
                    {
                        tracing::debug!(
                            rebuilt = ?maintenance.rebuilt_generation,
                            assigned = maintenance.assigned_vectors,
                            repaired = maintenance.repaired_assignments,
                            %project_id,
                            "maintained derived source ANN index"
                        );
                    }
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, %project_id, "source ANN maintenance failed");
                }
                Err(error) => {
                    tracing::warn!(%error, %project_id, "source ANN maintenance task failed");
                }
            }
        }
        Ok((indexed, failed, done))
    }
}

fn source_embedding_retry_at(attempts: u32) -> String {
    let delay = crate::memory_embedding::index_retry_delay(attempts);
    (time::OffsetDateTime::now_utc()
        + time::Duration::seconds(i64::try_from(delay.as_secs()).unwrap_or(3_600)))
    .format(&time::format_description::well_known::Rfc3339)
    .unwrap_or_else(|_| now_rfc3339())
}

fn source_embedding_time_before(seconds: u64) -> String {
    (time::OffsetDateTime::now_utc()
        - time::Duration::seconds(i64::try_from(seconds).unwrap_or(900)))
    .format(&time::format_description::well_known::Rfc3339)
    .unwrap_or_else(|_| now_rfc3339())
}
