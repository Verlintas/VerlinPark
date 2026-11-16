use super::*;

impl Engine {
    #[allow(clippy::too_many_arguments)]
    pub async fn create_memory_as(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        kind: &str,
        title: &str,
        body: &str,
        tags: Vec<String>,
    ) -> Result<MemoryItem> {
        self.create_memory_with_metadata(
            session_id,
            agent_id,
            agent_name,
            kind,
            title,
            body,
            tags,
            "agent",
            Vec::new(),
            false,
            None,
        )
        .await
    }

    /// Create a memory with explicit provenance. The policy's
    /// `auto_supersede` and verify TTLs apply; duplicate content is idempotent.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_memory_with_metadata(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
        kind: &str,
        title: &str,
        body: &str,
        tags: Vec<String>,
        source: &str,
        evidence: Vec<MemoryEvidence>,
        pinned: bool,
        verify_after: Option<String>,
    ) -> Result<MemoryItem> {
        let kind = kind.trim();
        let title = title.trim();
        if kind.is_empty() || kind.chars().count() > 40 {
            anyhow::bail!("memory kind must be 1-40 characters");
        }
        if title.is_empty() || title.chars().count() > 200 {
            anyhow::bail!("memory title must be 1-200 characters");
        }
        if body.trim().is_empty() || body.chars().count() > 20_000 {
            anyhow::bail!("memory body must be 1-20000 characters");
        }
        if tags.len() > 10 || tags.iter().any(|tag| tag.chars().count() > 40) {
            anyhow::bail!("at most 10 tags of 40 characters are allowed");
        }
        if source.trim().is_empty() || source.chars().count() > 40 {
            anyhow::bail!("memory source must be 1-40 characters");
        }
        if evidence.len() > 64 {
            anyhow::bail!("memory supports at most 64 evidence entries");
        }
        for item in &evidence {
            if item.kind.trim().is_empty()
                || item.kind.chars().count() > 40
                || item.reference.trim().is_empty()
                || item.reference.chars().count() > 4_096
            {
                anyhow::bail!(
                    "memory evidence requires a 1-40 char kind and 1-4096 char reference"
                );
            }
        }
        let store = self.store.clone();
        let session_id_owned = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&session_id_owned))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        let store = self.store.clone();
        let policy_project = session.project_id.clone();
        let policy =
            tokio::task::spawn_blocking(move || store.get_memory_policy(&policy_project)).await??;
        let verify_days = if source == "task_result" {
            policy.task_result_verify_days
        } else {
            policy.default_verify_days
        };
        let verify_after = verify_after.or_else(|| memory_verify_after(verify_days));
        let store = self.store.clone();
        let session_id = session.id;
        let project_id = session.project_id;
        let agent_id = agent_id.to_string();
        let agent_name = agent_name.to_string();
        let kind = kind.to_string();
        let title = title.to_string();
        let body = body.to_string();
        let source = source.to_string();
        let outcome = tokio::task::spawn_blocking(move || {
            store.create_memory_with_metadata_event(NewMemory {
                session_id: &session_id,
                project_id: &project_id,
                agent_id: &agent_id,
                agent_name: &agent_name,
                kind: &kind,
                title: &title,
                body: &body,
                tags: &tags,
                source: &source,
                evidence: &evidence,
                pinned,
                verify_after: verify_after.as_deref(),
                dedupe: true,
                auto_supersede: policy.auto_supersede,
            })
        })
        .await??;
        for event in outcome.events {
            let _ = self.events.send(event);
        }
        Ok(outcome.memory)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_memory(
        &self,
        session_id: &str,
        kind: &str,
        title: &str,
        body: &str,
        tags: Vec<String>,
    ) -> Result<MemoryItem> {
        self.create_memory_as(session_id, "user", "User", kind, title, body, tags)
            .await
    }

    pub async fn list_memories(
        &self,
        project_id: &str,
        query: Option<String>,
        kind: Option<String>,
        limit: u32,
    ) -> Result<MemoryListResult> {
        self.list_memories_filtered(project_id, query, kind, None, false, limit)
            .await
    }

    /// Active memories by default; exact `status` wins over `include_stale`.
    #[allow(clippy::too_many_arguments)]
    pub async fn list_memories_filtered(
        &self,
        project_id: &str,
        query: Option<String>,
        kind: Option<String>,
        status: Option<String>,
        include_stale: bool,
        limit: u32,
    ) -> Result<MemoryListResult> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let memories = tokio::task::spawn_blocking(move || {
            store.list_memories_filtered(
                &project_id,
                query.as_deref(),
                kind.as_deref(),
                status.as_deref(),
                include_stale,
                limit,
            )
        })
        .await??;
        Ok(MemoryListResult { memories })
    }

    pub async fn memory_search(&self, params: MemorySearchParams) -> Result<MemorySearchResult> {
        let query_text = params.query.trim().to_string();
        let store = self.store.clone();
        let project_id = params.project_id.clone();
        let kind = params.kind.clone();
        let kinds = params.kinds.clone();
        let include_stale = params.include_stale;
        let limit = params.limit.unwrap_or(20).clamp(1, 100);
        let lexical_query = query_text.clone();
        let mut candidates = tokio::task::spawn_blocking(move || {
            store.memory_candidates(
                &project_id,
                &lexical_query,
                kind.as_deref(),
                &kinds,
                include_stale,
                limit,
            )
        })
        .await??;
        if let Some(task_id) = params
            .task_id
            .as_deref()
            .map(str::trim)
            .filter(|task_id| !task_id.is_empty())
        {
            let store = self.store.clone();
            let project_id = params.project_id.clone();
            let task_id = task_id.to_string();
            let extra = tokio::task::spawn_blocking(move || {
                store.memory_task_candidates(&project_id, &task_id, include_stale, limit)
            })
            .await??;
            let mut seen = candidates
                .iter()
                .map(|candidate| candidate.memory.id.clone())
                .collect::<HashSet<_>>();
            for memory in extra {
                if seen.insert(memory.id.clone()) {
                    candidates.push(MemoryCandidate {
                        memory,
                        lexical: 0.0,
                        fts: false,
                        semantic: None,
                    });
                }
            }
        }

        // The semantic lane is opt-in twice: the project policy must have an
        // embedding object, and that object must be enabled and configured. A
        // disabled policy never reads the key, queries the store, or opens a
        // socket.
        let memory_policy = self.get_memory_policy(&params.project_id).await?.policy;
        let embedding_policy = if params.lexical_only {
            None
        } else {
            memory_policy.embedding.clone()
        };
        let mut semantic_summary = MemorySemanticSummary {
            enabled: embedding_policy
                .as_ref()
                .is_some_and(|policy| policy.enabled && policy.is_configured()),
            model: embedding_policy
                .as_ref()
                .filter(|policy| policy.enabled)
                .and_then(|policy| policy.model.clone()),
            lexical_candidates: u32::try_from(candidates.len()).unwrap_or(u32::MAX),
            semantic_candidates: 0,
            fallback: None,
            index_source: None,
            index_generation: None,
            index_fallback: None,
            probed_clusters: 0,
            candidate_vectors: 0,
        };
        let mut semantic_ranking = crate::memory::SemanticRanking::Disabled;
        if let Some(policy) = embedding_policy.as_ref().filter(|policy| policy.enabled) {
            let unavailable = if !policy.is_configured() {
                Some("embedding policy is incomplete".to_string())
            } else if query_text.is_empty() {
                Some("query text is empty".to_string())
            } else {
                None
            };
            if let Some(reason) = unavailable {
                semantic_summary.fallback = Some(reason.clone());
                semantic_ranking = crate::memory::SemanticRanking::Unavailable(reason);
            } else {
                match crate::memory_embedding::EmbeddingClient::from_policy(policy) {
                    Err(error) => {
                        let reason =
                            crate::memory_embedding::sanitize_embedding_error(&error.to_string());
                        semantic_summary.fallback = Some(reason.clone());
                        semantic_ranking = crate::memory::SemanticRanking::Unavailable(reason);
                    }
                    Ok(client) => {
                        let query_vector = crate::memory::embedding_query_text(&query_text);
                        match client.embed(std::slice::from_ref(&query_vector)).await {
                            Err(error) => {
                                let reason = crate::memory_embedding::sanitize_embedding_error(
                                    &error.to_string(),
                                );
                                semantic_summary.fallback = Some(reason.clone());
                                semantic_ranking =
                                    crate::memory::SemanticRanking::Unavailable(reason);
                            }
                            Ok(batch) => {
                                let store = self.store.clone();
                                let project = params.project_id.clone();
                                let model = policy.model.clone().unwrap_or_default();
                                let usage = batch.usage.clone();
                                if usage.calls > 0 {
                                    let _ = tokio::task::spawn_blocking(move || {
                                        store.record_memory_embedding_usage(
                                            &project,
                                            &model,
                                            usage.calls,
                                            usage.input_tokens,
                                            usage.estimated_tokens,
                                            usage.bytes,
                                            usage.provider_reported,
                                        )
                                    })
                                    .await;
                                }
                                let query_vector =
                                    batch.items.first().and_then(|item| item.vector.clone());
                                let item_error =
                                    batch.items.first().and_then(|item| item.error.clone());
                                match query_vector {
                                    None => {
                                        let reason = item_error.unwrap_or_else(|| {
                                            "embedding endpoint returned no query vector"
                                                .to_string()
                                        });
                                        let reason =
                                            crate::memory_embedding::sanitize_embedding_error(
                                                &reason,
                                            );
                                        semantic_summary.fallback = Some(reason.clone());
                                        semantic_ranking =
                                            crate::memory::SemanticRanking::Unavailable(reason);
                                    }
                                    Some(query_vector) => {
                                        let model = policy.model.clone().unwrap_or_default();
                                        let dimensions = policy.dimensions;
                                        let max_candidates = policy.max_candidates.clamp(1, 256);
                                        let scored = if policy.uses_ivf_index() {
                                            let store = self.store.clone();
                                            let project = params.project_id.clone();
                                            let probe_policy = policy.clone();
                                            let probe_query = query_vector.clone();
                                            let probe = tokio::task::spawn_blocking(move || {
                                                crate::memory_index::probe_ivf(
                                                    &store,
                                                    &project,
                                                    &probe_policy,
                                                    include_stale,
                                                    &probe_query,
                                                )
                                            })
                                            .await?;
                                            match probe.fallback {
                                                Some(reason) => {
                                                    let store = self.store.clone();
                                                    let project = params.project_id.clone();
                                                    let fallback_model = model.clone();
                                                    let nlist = policy.ivf_nlist;
                                                    let reason_for_log = reason.clone();
                                                    let _ = tokio::task::spawn_blocking(move || {
                                                        store.note_memory_embedding_index_fallback(
                                                            &project,
                                                            &fallback_model,
                                                            dimensions,
                                                            nlist,
                                                            &reason_for_log,
                                                        )
                                                    })
                                                    .await;
                                                    semantic_summary.index_source =
                                                        Some("scan".to_string());
                                                    semantic_summary.index_generation =
                                                        probe.generation;
                                                    semantic_summary.index_fallback = Some(reason);
                                                    semantic_summary.probed_clusters = 0;
                                                    semantic_summary.candidate_vectors = 0;
                                                    self.scan_semantic_candidates(
                                                        &params.project_id,
                                                        &model,
                                                        dimensions,
                                                        include_stale,
                                                        max_candidates,
                                                        &query_vector,
                                                        f64::from(policy.min_similarity),
                                                    )
                                                    .await?
                                                }
                                                None => {
                                                    semantic_summary.index_source =
                                                        Some("ivf".to_string());
                                                    semantic_summary.index_generation =
                                                        probe.generation;
                                                    semantic_summary.index_fallback = None;
                                                    semantic_summary.probed_clusters =
                                                        probe.probed_clusters;
                                                    semantic_summary.candidate_vectors =
                                                        probe.candidate_vectors;
                                                    probe.scored
                                                }
                                            }
                                        } else {
                                            semantic_summary.index_source =
                                                Some("scan".to_string());
                                            semantic_summary.index_generation = None;
                                            semantic_summary.index_fallback = None;
                                            semantic_summary.probed_clusters = 0;
                                            semantic_summary.candidate_vectors = 0;
                                            self.scan_semantic_candidates(
                                                &params.project_id,
                                                &model,
                                                dimensions,
                                                include_stale,
                                                max_candidates,
                                                &query_vector,
                                                f64::from(policy.min_similarity),
                                            )
                                            .await?
                                        };
                                        semantic_summary.semantic_candidates =
                                            u32::try_from(scored.len()).unwrap_or(u32::MAX);
                                        let mut semantic_by_id = scored
                                            .into_iter()
                                            .map(|(memory, score)| {
                                                (memory.id.clone(), (memory, score))
                                            })
                                            .collect::<std::collections::HashMap<_, _>>();
                                        for candidate in &mut candidates {
                                            if let Some((_, score)) =
                                                semantic_by_id.remove(&candidate.memory.id)
                                            {
                                                candidate.semantic = Some(score);
                                            }
                                        }
                                        candidates.extend(semantic_by_id.into_values().map(
                                            |(memory, score)| MemoryCandidate {
                                                memory,
                                                lexical: 0.0,
                                                fts: false,
                                                semantic: Some(score),
                                            },
                                        ));
                                        semantic_ranking = crate::memory::SemanticRanking::Active;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let query = crate::memory::MemoryQuery {
            text: params.query,
            task_id: params.task_id,
            department: params.department,
            paths: params.paths,
        };
        let mut matches = crate::memory::rank_candidates_with_semantics(
            candidates,
            &query,
            time::OffsetDateTime::now_utc(),
            include_stale,
            semantic_ranking,
        );
        if let Some(max_chars) = params.max_chars {
            matches = crate::memory::select_brief(&matches, max_chars).memories;
        }
        matches.truncate(limit as usize);
        Ok(MemorySearchResult {
            memories: matches,
            semantic: Some(semantic_summary),
        })
    }

    /// Exact semantic scan used by the default lane and by every IVF fallback.
    /// The candidate window is bounded by the policy and decoded/scored by the
    /// same function as the ANN probe, so a fallback cannot change cosine
    /// semantics.
    #[allow(clippy::too_many_arguments)]
    async fn scan_semantic_candidates(
        &self,
        project_id: &str,
        model: &str,
        dimensions: u32,
        include_stale: bool,
        max_candidates: u32,
        query_vector: &[f32],
        min_similarity: f64,
    ) -> Result<Vec<(MemoryItem, f64)>> {
        let store = self.store.clone();
        let project = project_id.to_string();
        let model = model.to_string();
        let semantic_candidates = tokio::task::spawn_blocking(move || {
            store.memory_semantic_candidates(
                &project,
                &model,
                dimensions,
                include_stale,
                max_candidates,
            )
        })
        .await??;
        Ok(crate::memory::semantic_scores(
            semantic_candidates,
            query_vector,
            dimensions,
            min_similarity,
        ))
    }

    /// Build the injected project-memory brief. Usage is observed per selected
    /// memory and session, and `memory.used` is idempotent for that pair.
    pub async fn memory_brief(
        &self,
        session_id: &str,
        task_id: Option<&str>,
    ) -> Result<MemoryBriefResult> {
        self.memory_brief_with_department(session_id, task_id, None)
            .await
    }

    /// Same as `memory_brief`, with an explicit department override for agents
    /// that carry a department but no plan-task row.
    pub async fn memory_brief_with_department(
        &self,
        session_id: &str,
        task_id: Option<&str>,
        department_override: Option<&str>,
    ) -> Result<MemoryBriefResult> {
        let store = self.store.clone();
        let lookup = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&lookup))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        let project_id = session.project_id.clone();
        let policy = self.get_memory_policy(&project_id).await?.policy;
        let brief_max_chars = policy.brief_max_chars.unwrap_or(4_000).clamp(200, 100_000);

        let mut text_parts = vec![session.goal.clone()];
        if let Some(task_id) = task_id {
            text_parts.push(task_id.to_string());
        }
        let mut department = department_override
            .map(str::trim)
            .filter(|department| !department.is_empty())
            .map(str::to_string);
        let mut paths = Vec::new();
        if let Some(task_id) = task_id {
            if let Some(plan) = self.get_team_plan(session_id).await?
                && let Some(task) = plan.tasks.iter().find(|task| task.id == task_id)
            {
                if !task.title.trim().is_empty() {
                    text_parts.push(task.title.clone());
                }
                text_parts.push(task.department.clone());
                text_parts.extend(task.write_scope.iter().cloned());
                text_parts.extend(task.acceptance.iter().cloned());
                department = Some(task.department.clone());
                paths = task.write_scope.clone();
            }
            if department.is_none() {
                let store = self.store.clone();
                let lookup = session_id.to_string();
                if let Ok(tasks) =
                    tokio::task::spawn_blocking(move || store.list_plan_tasks(None, Some(&lookup)))
                        .await?
                    && let Some(task) = tasks.into_iter().find(|task| task.task_id == task_id)
                {
                    if !task.title.trim().is_empty() {
                        text_parts.push(task.title);
                    }
                    text_parts.push(task.department.clone());
                    text_parts.extend(task.write_scope.iter().cloned());
                    department = Some(task.department);
                    paths = task.write_scope;
                }
            }
        }

        let search = MemorySearchParams {
            project_id: project_id.clone(),
            query: text_parts.join(" "),
            kind: None,
            kinds: Vec::new(),
            task_id: task_id.map(str::to_string),
            department,
            paths,
            limit: Some(20),
            include_stale: false,
            max_chars: None,
            lexical_only: false,
        };
        let mut matches = self.memory_search(search).await?.memories;

        if matches.is_empty() {
            let store = self.store.clone();
            let project = project_id.clone();
            let recent = tokio::task::spawn_blocking(move || {
                store.list_memories_filtered(&project, None, None, None, false, 8)
            })
            .await??;
            matches = recent
                .into_iter()
                .map(|memory| MemoryMatch {
                    memory,
                    score: 0.0,
                    reasons: vec!["recent-fallback".to_string()],
                    lexical_score: Some(0.0),
                    semantic_score: None,
                    source: Some("structural".to_string()),
                    semantic_unavailable: None,
                })
                .collect();
        }

        let selection = crate::memory::select_brief(&matches, brief_max_chars);
        for matched in &selection.memories {
            let store = self.store.clone();
            let memory_id = matched.memory.id.clone();
            let using_session = session_id.to_string();
            if let Ok(Some((_, event))) = tokio::task::spawn_blocking(move || {
                store.record_memory_use_with_event(&memory_id, &using_session)
            })
            .await
            .unwrap_or(Ok(None))
            {
                let _ = self.events.send(event);
            }
        }
        Ok(MemoryBriefResult {
            text: selection.text,
            memories: selection.memories,
        })
    }

    /// Update lifecycle metadata only. Title and body are immutable so
    /// provenance cannot be rewritten after the fact.
    pub async fn memory_update(
        &self,
        id: &str,
        status: Option<&str>,
        pinned: Option<bool>,
        tags: Option<Vec<String>>,
    ) -> Result<MemoryUpdateResult> {
        if let Some(status) = status
            && !matches!(status, "active" | "stale" | "superseded" | "archived")
        {
            anyhow::bail!("memory status must be active, stale, superseded, or archived");
        }
        if status.is_none() && pinned.is_none() && tags.is_none() {
            anyhow::bail!("memory update requires status, pinned, or tags");
        }
        if let Some(tags) = &tags
            && (tags.len() > 10 || tags.iter().any(|tag| tag.chars().count() > 40))
        {
            anyhow::bail!("at most 10 tags of 40 characters are allowed");
        }
        let store = self.store.clone();
        let memory_id = id.to_string();
        let status = status.map(str::to_string);
        let tags_for_store = tags.clone();
        let updated = tokio::task::spawn_blocking(move || {
            store.update_memory_with_event(
                &memory_id,
                status.as_deref(),
                pinned,
                tags_for_store.as_deref(),
            )
        })
        .await??;
        let Some((memory, event)) = updated else {
            anyhow::bail!("memory not found: {id}");
        };
        let _ = self.events.send(event);
        Ok(MemoryUpdateResult { memory })
    }

    /// Check evidence locally, then explicitly verify or mark stale. This never
    /// asks a model to judge whether a claim is true.
    pub async fn memory_verify(&self, id: &str) -> Result<MemoryVerifyResult> {
        let store = self.store.clone();
        let memory_id = id.to_string();
        let memory = tokio::task::spawn_blocking(move || store.get_memory(&memory_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("memory not found: {id}"))?;
        if matches!(memory.status.as_str(), "superseded" | "archived") {
            anyhow::bail!(
                "memory {id} is {}; restore it with memory.update before verifying",
                memory.status
            );
        }
        let store = self.store.clone();
        let project_id = memory.project_id.clone();
        let project = tokio::task::spawn_blocking(move || store.get_project(&project_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found for memory: {id}"))?;
        let root = PathBuf::from(&project.root);
        let store = self.store.clone();
        let session_id = memory.session_id.clone();
        let project_id = memory.project_id.clone();
        let evidence = memory.evidence.clone();
        let checks = tokio::task::spawn_blocking(move || {
            verify_memory_evidence(&store, &root, &project_id, &session_id, &evidence)
        })
        .await?;

        let failed = checks
            .iter()
            .find(|check| check.status == "missing")
            .map(|check| {
                format!(
                    "evidence verification failed: {} {}: {}",
                    check.evidence.kind, check.evidence.reference, check.detail
                )
            });
        let checked = checks
            .iter()
            .filter(|check| check.status != "unchecked")
            .count();
        let store = self.store.clone();
        let memory_id = id.to_string();
        let mut memory = memory;
        let mut event = None;
        if let Some(reason) = failed {
            let updated = tokio::task::spawn_blocking(move || {
                store.mark_memory_stale_with_event(&memory_id, &reason)
            })
            .await??;
            let Some((stale, stale_event)) = updated else {
                anyhow::bail!("memory not found: {id}");
            };
            memory = stale;
            event = Some(stale_event);
        } else if checked > 0 || checks.is_empty() {
            // A memory with only unchecked evidence (for example a URL) is not
            // silently treated as verified.
            let policy = self.get_memory_policy(&memory.project_id).await?.policy;
            let verified_at = now_rfc3339();
            let verify_after = memory_verify_after(policy.default_verify_days);
            let updated = tokio::task::spawn_blocking(move || {
                store.mark_memory_verified_with_event(
                    &memory_id,
                    &verified_at,
                    verify_after.as_deref(),
                )
            })
            .await??;
            let Some((verified, verified_event)) = updated else {
                anyhow::bail!("memory not found: {id}");
            };
            memory = verified;
            event = Some(verified_event);
        }
        if let Some(event) = event {
            let _ = self.events.send(event);
        }
        Ok(MemoryVerifyResult { memory, checks })
    }

    /// Mark expired active, non-pinned memories stale. Never deletes and never
    /// restores. `dry_run` returns candidates without mutating.
    pub async fn memory_sweep(
        &self,
        project_id: &str,
        older_than_days: Option<u32>,
        dry_run: bool,
    ) -> Result<MemorySweepResult> {
        if let Some(days) = older_than_days
            && !(1..=36_500).contains(&days)
        {
            anyhow::bail!("memory sweep older_than_days must be between 1 and 36500");
        }
        let store = self.store.clone();
        let project = project_id.to_string();
        let candidates = tokio::task::spawn_blocking(move || {
            store.memory_sweep_candidates(&project, older_than_days)
        })
        .await??;
        let memory_ids = candidates
            .iter()
            .map(|memory| memory.id.clone())
            .collect::<Vec<_>>();
        let mut marked_stale = 0;
        if !dry_run {
            for id in &memory_ids {
                let store = self.store.clone();
                let memory_id = id.clone();
                let updated = tokio::task::spawn_blocking(move || {
                    store.mark_memory_stale_with_event(&memory_id, "verification expired")
                })
                .await??;
                if let Some((_, event)) = updated {
                    let _ = self.events.send(event);
                    marked_stale += 1;
                }
            }
        }
        Ok(MemorySweepResult {
            project_id: project_id.to_string(),
            candidates: memory_ids.len(),
            marked_stale,
            memory_ids,
            dry_run,
        })
    }

    pub async fn memory_stats(&self, project_id: &str) -> Result<MemoryStatsResult> {
        let store = self.store.clone();
        let project = project_id.to_string();
        let stats = tokio::task::spawn_blocking(move || store.memory_stats(&project)).await??;
        Ok(MemoryStatsResult {
            project_id: project_id.to_string(),
            total: stats.total,
            by_status: stats.by_status,
            by_source: stats.by_source,
            by_kind: stats.by_kind,
        })
    }

    pub async fn memory_conflicts(
        &self,
        project_id: &str,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<MemoryConflictsResult> {
        if let Some(status) = status
            && !matches!(status, "open" | "resolved")
        {
            anyhow::bail!("memory conflict status must be open or resolved");
        }
        self.ensure_project_exists(project_id).await?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let status = status.map(str::to_string);
        let conflicts = tokio::task::spawn_blocking(move || {
            store.list_memory_conflicts(&project, status.as_deref(), limit)
        })
        .await??;
        Ok(MemoryConflictsResult {
            project_id: project_id.to_string(),
            conflicts,
        })
    }

    /// Deterministic detection pass. Never auto-resolves and never changes a
    /// memory lifecycle status.
    pub async fn memory_conflict_scan(&self, project_id: &str) -> Result<MemoryConflictScanResult> {
        self.ensure_project_exists(project_id).await?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let scan =
            tokio::task::spawn_blocking(move || store.scan_memory_conflicts(&project)).await??;
        for event in &scan.events {
            let _ = self.events.send(event.clone());
        }
        Ok(MemoryConflictScanResult {
            project_id: project_id.to_string(),
            detected: u32::try_from(scan.new.len()).unwrap_or(u32::MAX),
            open: u32::try_from(scan.open.len()).unwrap_or(u32::MAX),
            conflicts: scan.open,
        })
    }

    pub async fn memory_conflict_resolve(
        &self,
        id: &str,
        action: &str,
        note: Option<&str>,
    ) -> Result<MemoryConflictResolveResult> {
        if !matches!(action, "keep_a" | "keep_b" | "keep_both" | "archive_both") {
            anyhow::bail!(
                "memory conflict action must be keep_a, keep_b, keep_both, or archive_both"
            );
        }
        let store = self.store.clone();
        let conflict_id = id.to_string();
        let conflict = tokio::task::spawn_blocking(move || store.get_memory_conflict(&conflict_id))
            .await??
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "memory conflict {id} not found: memory no longer exists or the conflict was removed with a deleted memory"
                )
            })?;
        let project_id = conflict.project_id.clone();
        let store = self.store.clone();
        let conflict_id = id.to_string();
        let action = action.to_string();
        let note = note
            .map(str::trim)
            .filter(|note| !note.is_empty())
            .map(str::to_string);
        let resolution = tokio::task::spawn_blocking(move || {
            store.resolve_memory_conflict(&project_id, &conflict_id, &action, note.as_deref())
        })
        .await??;
        for event in &resolution.events {
            let _ = self.events.send(event.clone());
        }
        Ok(MemoryConflictResolveResult {
            conflict: resolution.conflict,
            memories: resolution.memories,
        })
    }

    pub async fn memory_health(&self, project_id: &str) -> Result<MemoryHealthResult> {
        self.ensure_project_exists(project_id).await?;
        let store = self.store.clone();
        let project = project_id.to_string();
        tokio::task::spawn_blocking(move || store.memory_health(&project)).await?
    }

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

    /// Derived IVF index health for one project. Contains aggregate counts and
    /// generation metadata only; centroids and vectors never leave the store.
    pub async fn memory_embedding_index_status(
        &self,
        project_id: &str,
    ) -> Result<MemoryEmbeddingsIndexStatusResult> {
        self.ensure_project_exists(project_id).await?;
        let policy = self.get_memory_policy(project_id).await?.policy;
        let store = self.store.clone();
        let project = project_id.to_string();
        let index = tokio::task::spawn_blocking(move || {
            crate::memory_index::index_status(&store, &project, policy.embedding.as_ref())
        })
        .await??;
        Ok(MemoryEmbeddingsIndexStatusResult {
            project_id: project_id.to_string(),
            index,
        })
    }

    /// Explicit derived-index rebuild. Never calls the embedding endpoint,
    /// never changes memory truth, and never deletes vectors. Scan mode or a
    /// below-minimum vector count removes stale derived index state and
    /// returns `rebuilt = false` with a reason.
    pub async fn memory_embedding_index_rebuild(
        &self,
        project_id: &str,
    ) -> Result<MemoryEmbeddingsIndexRebuildResult> {
        self.ensure_project_exists(project_id).await?;
        let policy = self.get_memory_policy(project_id).await?.policy;
        let Some(embedding) = policy.embedding else {
            return Ok(MemoryEmbeddingsIndexRebuildResult {
                project_id: project_id.to_string(),
                rebuilt: false,
                reason: Some("not_configured".to_string()),
                generation: None,
                vector_count: 0,
                assigned_vectors: 0,
                cluster_count: 0,
                nlist: 0,
            });
        };
        crate::memory_embedding::validate_embedding_policy(&embedding)?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let embedding_for_build = embedding.clone();
        let report = tokio::task::spawn_blocking(move || {
            crate::memory_index::rebuild_index(&store, &project, &embedding_for_build)
        })
        .await??;
        Ok(MemoryEmbeddingsIndexRebuildResult {
            project_id: project_id.to_string(),
            rebuilt: report.rebuilt,
            reason: report.reason,
            generation: report.generation,
            vector_count: report.vector_count,
            assigned_vectors: report.assigned_vectors,
            cluster_count: report.cluster_count,
            nlist: report.nlist,
        })
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

    /// Look up the project embedding policy and run bounded index
    /// maintenance. Used after delete/prune so assignments and centroid
    /// metadata cannot drift from the live vector set.
    async fn maintain_memory_index_if_configured(&self, project_id: &str) {
        match self.get_memory_policy(project_id).await {
            Ok(policy) => {
                if let Some(embedding) = policy.policy.embedding {
                    self.maintain_project_memory_index(project_id, &embedding)
                        .await;
                }
            }
            Err(error) => {
                tracing::warn!(%error, %project_id, "could not load memory policy for index maintenance");
            }
        }
    }

    /// Bounded derived-index maintenance after an embedding batch. This is
    /// best-effort: a failed index build never fails memory writes or the
    /// embedding reconciler, and recall falls back to the exact scan.
    async fn maintain_project_memory_index(
        &self,
        project_id: &str,
        policy: &MemoryEmbeddingPolicy,
    ) {
        if !policy.uses_ivf_index() {
            return;
        }
        let store = self.store.clone();
        let project = project_id.to_string();
        let policy = policy.clone();
        match tokio::task::spawn_blocking(move || {
            crate::memory_index::maintain_index(&store, &project, &policy)
        })
        .await
        {
            Ok(Ok(report)) => {
                if let Some(generation) = report.rebuilt_generation {
                    tracing::info!(
                        %project_id,
                        generation,
                        assigned = report.assigned_vectors,
                        vectors = report.vector_count,
                        "memory embedding IVF index rebuilt"
                    );
                }
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, %project_id, "memory embedding IVF index maintenance failed");
            }
            Err(error) => {
                tracing::warn!(%error, %project_id, "memory embedding IVF index task failed");
            }
        }
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

    pub(super) async fn ensure_project_exists(&self, project_id: &str) -> Result<()> {
        let store = self.store.clone();
        let project = project_id.to_string();
        let exists = tokio::task::spawn_blocking(move || store.get_project(&project))
            .await??
            .is_some();
        if !exists {
            anyhow::bail!("project not found: {project_id}");
        }
        Ok(())
    }

    /// Daemon tick: detect conflicts for every project. Detection only; it
    /// never resolves a conflict or changes a memory status.
    pub async fn scan_all_memory_conflicts(&self) -> Result<(usize, usize, usize)> {
        let projects = self.list_projects().await?;
        let mut projects_scanned = 0usize;
        let mut detected = 0usize;
        let mut open = 0usize;
        for project in projects {
            match self.memory_conflict_scan(&project.id).await {
                Ok(result) => {
                    projects_scanned += 1;
                    detected += result.detected as usize;
                    open += result.open as usize;
                    if result.detected > 0 {
                        tracing::info!(
                            project_id = %project.id,
                            detected = result.detected,
                            open = result.open,
                            "memory conflict scan detected new same-scope conflicts"
                        );
                    } else {
                        tracing::debug!(
                            project_id = %project.id,
                            detected = result.detected,
                            open = result.open,
                            "memory conflict scan complete"
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, project_id = %project.id, "memory conflict scan failed");
                }
            }
        }
        Ok((projects_scanned, detected, open))
    }

    /// Daemon tick: sweep only projects whose policy opted into `sweep_stale`.
    pub async fn sweep_memory_staleness(&self) -> Result<(usize, usize)> {
        let store = self.store.clone();
        let policies =
            tokio::task::spawn_blocking(move || store.list_memory_sweep_policies()).await??;
        let mut projects = 0;
        let mut marked = 0;
        for (project_id, policy) in policies {
            if !policy.sweep_stale {
                continue;
            }
            match self.memory_sweep(&project_id, None, false).await {
                Ok(result) => {
                    projects += 1;
                    marked += result.marked_stale;
                }
                Err(error) => {
                    tracing::warn!(%error, %project_id, "memory staleness sweep failed");
                }
            }
        }
        Ok((projects, marked))
    }

    pub async fn delete_memory(&self, id: &str) -> Result<MemoryDeleteResult> {
        let store = self.store.clone();
        let memory_id = id.to_string();
        let lookup = memory_id.clone();
        let removed =
            tokio::task::spawn_blocking(move || store.delete_memories_with_events(&[lookup]))
                .await??;
        let project_id = removed.first().map(|(memory, _)| memory.project_id.clone());
        for (_, event) in &removed {
            let _ = self.events.send(event.clone());
        }
        if let Some(project_id) = project_id {
            self.maintain_memory_index_if_configured(&project_id).await;
        }
        Ok(MemoryDeleteResult {
            memory_id,
            removed: !removed.is_empty(),
        })
    }

    pub async fn prune_memories(
        &self,
        project_id: &str,
        older_than_days: Option<u32>,
        keep_latest: Option<u32>,
        dry_run: bool,
    ) -> Result<MemoryPruneResult> {
        if older_than_days.is_none() && keep_latest.is_none() {
            anyhow::bail!("memory prune requires older_than_days or keep_latest");
        }
        let store = self.store.clone();
        let project = project_id.to_string();
        let ids = tokio::task::spawn_blocking(move || {
            store.memory_ids_for_prune(&project, older_than_days, keep_latest)
        })
        .await??;
        let candidates = ids.len();
        if dry_run || ids.is_empty() {
            return Ok(MemoryPruneResult {
                candidates,
                removed: 0,
                dry_run,
            });
        }
        let store = self.store.clone();
        let removed =
            tokio::task::spawn_blocking(move || store.delete_memories_with_events(&ids)).await??;
        for (_, event) in &removed {
            let _ = self.events.send(event.clone());
        }
        if !removed.is_empty() {
            self.maintain_memory_index_if_configured(project_id).await;
        }
        Ok(MemoryPruneResult {
            candidates,
            removed: removed.len(),
            dry_run: false,
        })
    }

    pub async fn get_memory_policy(&self, project_id: &str) -> Result<MemoryPolicyResult> {
        let store = self.store.clone();
        let project = project_id.to_string();
        let policy =
            tokio::task::spawn_blocking(move || store.get_memory_policy(&project)).await??;
        Ok(MemoryPolicyResult {
            project_id: project_id.to_string(),
            policy,
        })
    }

    pub async fn set_memory_policy(
        &self,
        project_id: &str,
        policy: MemoryPolicy,
    ) -> Result<MemoryPolicyResult> {
        validate_memory_policy(&policy)?;
        let store = self.store.clone();
        let project = project_id.to_string();
        let stored = policy.clone();
        tokio::task::spawn_blocking(move || store.set_memory_policy(&project, &stored)).await??;
        Ok(MemoryPolicyResult {
            project_id: project_id.to_string(),
            policy,
        })
    }

    pub async fn sweep_memory_retention(&self) -> Result<usize> {
        let store = self.store.clone();
        let policies =
            tokio::task::spawn_blocking(move || store.list_enabled_memory_policies()).await??;
        let mut removed = 0;
        for (project_id, policy) in policies {
            if policy.max_items.is_none() && policy.max_age_days.is_none() {
                continue;
            }
            match self
                .prune_memories(&project_id, policy.max_age_days, policy.max_items, false)
                .await
            {
                Ok(result) => removed += result.removed,
                Err(error) => {
                    tracing::warn!(%error, %project_id, "memory retention sweep failed");
                }
            }
        }
        Ok(removed)
    }

    pub async fn get_agent_memory(&self, params: AgentMemoryParams) -> Result<AgentMemoryResult> {
        let store = self.store.clone();
        let viewer = match params.identity_id.as_deref() {
            Some(identity_id) => Some(store.get_agent_identity(identity_id)?.ok_or_else(|| {
                vp_protocol::types::AgentMemoryError::new(
                    vp_protocol::types::agent_memory_codes::MEMORY_NOT_FOUND,
                    format!("agent identity not found: {identity_id}"),
                )
            })?),
            None => None,
        };
        let memory_id = params.memory_id.clone();
        let memory = tokio::task::spawn_blocking(move || {
            vp_store::store::identity_memory::get_agent_memory(&store, &memory_id)
        })
        .await??
        .ok_or_else(|| {
            vp_protocol::types::AgentMemoryError::new(
                vp_protocol::types::agent_memory_codes::MEMORY_NOT_FOUND,
                format!("agent memory not found: {}", params.memory_id),
            )
        })?;
        if let Some(project_id) = params.project_id.as_deref()
            && project_id != memory.project_id
        {
            return Err(vp_protocol::types::AgentMemoryError::new(
                vp_protocol::types::agent_memory_codes::MEMORY_PROJECT_MISMATCH,
                "agent memory belongs to another project",
            )
            .into());
        }
        if let Some(viewer) = viewer.as_ref() {
            if viewer.project_id != memory.project_id {
                return Err(vp_protocol::types::AgentMemoryError::new(
                    vp_protocol::types::agent_memory_codes::MEMORY_PROJECT_MISMATCH,
                    "viewer identity belongs to another project",
                )
                .into());
            }
            crate::identity_memory::ensure_memory_visible(&memory, Some(viewer))?;
        }
        Ok(AgentMemoryResult {
            memory,
            changed: false,
        })
    }

    pub async fn remember_agent_memory(
        &self,
        params: CreateAgentMemoryParams,
    ) -> Result<AgentMemoryResult> {
        let store = self.store.clone();
        let (memory, _event) = tokio::task::spawn_blocking(move || {
            vp_store::store::identity_memory::create_agent_memory_with_event(&store, &params)
        })
        .await??;
        Ok(AgentMemoryResult {
            memory,
            changed: true,
        })
    }

    pub async fn update_agent_memory(
        &self,
        params: UpdateAgentMemoryParams,
    ) -> Result<AgentMemoryResult> {
        let store = self.store.clone();
        let memory_id = params.memory_id.clone();
        match tokio::task::spawn_blocking(move || {
            vp_store::store::identity_memory::update_agent_memory_with_event(&store, &params)
        })
        .await??
        {
            Some((memory, _event)) => Ok(AgentMemoryResult {
                memory,
                changed: true,
            }),
            None => {
                let current = self
                    .get_agent_memory(AgentMemoryParams {
                        memory_id,
                        project_id: None,
                        identity_id: None,
                    })
                    .await?;
                Ok(AgentMemoryResult {
                    memory: current.memory,
                    changed: false,
                })
            }
        }
    }

    pub async fn supersede_agent_memory(
        &self,
        params: SupersedeAgentMemoryParams,
    ) -> Result<AgentMemorySupersedeResult> {
        let store = self.store.clone();
        let result = tokio::task::spawn_blocking(move || {
            vp_store::store::identity_memory::supersede_agent_memory_with_events(&store, &params)
        })
        .await??;
        Ok(AgentMemorySupersedeResult {
            previous: result.previous,
            replacement: result.replacement,
        })
    }

    pub async fn forget_agent_memory(
        &self,
        params: ForgetAgentMemoryParams,
    ) -> Result<AgentMemoryForgetResult> {
        let store = self.store.clone();
        let memory_id = params.memory_id.clone();
        let prune = params.prune;
        let outcome = tokio::task::spawn_blocking(move || {
            vp_store::store::identity_memory::forget_agent_memory_with_event(&store, &params)
        })
        .await??;
        Ok(AgentMemoryForgetResult {
            memory_id,
            pruned: prune && outcome.is_some(),
            changed: outcome.is_some(),
        })
    }

    /// Logically erase one memory and its prior event payloads. Destructive,
    /// explicit, and local-only; imported rows fail closed.
    pub async fn erase_agent_memory(
        &self,
        params: vp_protocol::types::EraseAgentMemoryParams,
    ) -> Result<vp_protocol::types::AgentMemoryEraseResult> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            vp_store::store::identity_memory::erase_agent_memory(&store, &params)
        })
        .await?
    }

    pub async fn verify_agent_memory(
        &self,
        params: VerifyAgentMemoryParams,
    ) -> Result<AgentMemoryResult> {
        let store = self.store.clone();
        let memory_id = params.memory_id.clone();
        match tokio::task::spawn_blocking(move || {
            vp_store::store::identity_memory::verify_agent_memory_with_event(&store, &params)
        })
        .await??
        {
            Some((memory, _event)) => Ok(AgentMemoryResult {
                memory,
                changed: true,
            }),
            None => {
                let current = self
                    .get_agent_memory(AgentMemoryParams {
                        memory_id,
                        project_id: None,
                        identity_id: None,
                    })
                    .await?;
                Ok(AgentMemoryResult {
                    memory: current.memory,
                    changed: false,
                })
            }
        }
    }

    pub async fn sweep_agent_memories(
        &self,
        params: SweepAgentMemoriesParams,
    ) -> Result<AgentMemorySweepResult> {
        let store = self.store.clone();
        let sweep = tokio::task::spawn_blocking(move || {
            vp_store::store::identity_memory::sweep_agent_memories_with_events(&store, &params)
        })
        .await??;
        Ok(AgentMemorySweepResult {
            expired: sweep.expired,
            pruned: sweep.pruned,
            remaining: sweep.remaining,
        })
    }

    /// Project-scoped memory replay, or explicit projection repair. It never
    /// appends memory events and never crosses project boundaries.
    pub async fn replay_agent_memories(
        &self,
        params: AgentMemoryReplayParams,
    ) -> Result<AgentMemoryReplayReport> {
        let store = self.store.clone();
        let project_id = params.project_id;
        let repair = params.repair;
        tokio::task::spawn_blocking(move || {
            let events = store.list_agent_memory_events_for_project(&project_id, None)?;
            if repair {
                let total = vp_store::store::identity_memory::count_agent_memory_events(
                    &store,
                    &project_id,
                )?;
                if total > events.len() as u64 {
                    return Err(vp_protocol::types::AgentMemoryError::new(
                        vp_protocol::types::agent_memory_codes::MEMORY_REPLAY_TRUNCATED,
                        format!(
                            "project has {total} memory events but replay reads at most {}; refusing to repair from a truncated stream",
                            events.len()
                        ),
                    )
                    .into());
                }
            }
            let expected = crate::identity_memory::memories_from_events(&events)?;
            let mut actual = store.list_agent_memories_for_replay(&project_id)?;
            let mut mismatches =
                crate::identity_memory::compare_memory_projection(&expected, &actual);
            let mut repaired = false;
            if repair && !mismatches.is_empty() {
                store.replace_agent_memory_projection(&project_id, &expected)?;
                actual = store.list_agent_memories_for_replay(&project_id)?;
                mismatches = crate::identity_memory::compare_memory_projection(&expected, &actual);
                repaired = true;
            }
            Ok(crate::identity_memory::replay_report(
                &project_id,
                &expected,
                events.len(),
                repaired,
                mismatches,
            ))
        })
        .await?
    }

    // -----------------------------------------------------------------
    // S2 session assignment, identity lease, and project inbox
    // -----------------------------------------------------------------
}

fn validate_memory_policy(policy: &MemoryPolicy) -> Result<()> {
    if let Some(max_items) = policy.max_items
        && !(1..=100_000).contains(&max_items)
    {
        anyhow::bail!("memory max_items must be between 1 and 100000");
    }
    if let Some(max_age_days) = policy.max_age_days
        && !(1..=36_500).contains(&max_age_days)
    {
        anyhow::bail!("memory max_age_days must be between 1 and 36500");
    }
    if policy.enabled && policy.max_items.is_none() && policy.max_age_days.is_none() {
        anyhow::bail!("enabled memory policy requires max_items or max_age_days");
    }
    for (name, days) in [
        ("task_result_verify_days", policy.task_result_verify_days),
        ("default_verify_days", policy.default_verify_days),
    ] {
        if let Some(days) = days
            && !(1..=36_500).contains(&days)
        {
            anyhow::bail!("memory {name} must be between 1 and 36500 (or null to disable)");
        }
    }
    if let Some(max_chars) = policy.brief_max_chars
        && !(200..=100_000).contains(&max_chars)
    {
        anyhow::bail!("memory brief_max_chars must be between 200 and 100000");
    }
    if let Some(embedding) = &policy.embedding {
        crate::memory_embedding::validate_embedding_policy(embedding)?;
    }
    Ok(())
}
