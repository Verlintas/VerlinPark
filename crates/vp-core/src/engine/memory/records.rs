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
}
