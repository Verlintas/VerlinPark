use super::*;

impl Engine {
    /// Derive citation evidence from persisted `agent.message` events. The
    /// event log remains the source of truth; no second citation projection can
    /// drift from it.
    pub async fn session_citations(
        &self,
        session_id: &str,
        message_id: Option<String>,
    ) -> Result<SessionCitationsResult> {
        let events = self.recent_events(session_id, 10_000).await?;
        let mut citations = Vec::new();
        for event in events {
            let Some(parsed) = parse_message_citation_event(&event) else {
                continue;
            };
            if message_id
                .as_deref()
                .is_some_and(|wanted| wanted != parsed.message_id)
            {
                continue;
            }
            for citation in parsed.citations {
                citations.push(MessageCitation {
                    message_id: parsed.message_id.clone(),
                    citation,
                });
            }
        }
        Ok(SessionCitationsResult { citations })
    }

    /// Deterministic citation coverage report. It exposes dangling `[cN]`
    /// markers and citation records the model never referenced so the UI can
    /// surface evidence problems instead of silently trusting prose.
    pub async fn session_citation_audit(
        &self,
        session_id: &str,
    ) -> Result<SessionCitationAuditResult> {
        let events = self.recent_events(session_id, 10_000).await?;
        let mut messages = Vec::new();
        let mut messages_with_citations = 0usize;
        let mut messages_without_citations = 0usize;
        let mut total_citations = 0usize;
        let mut unique_sources = std::collections::HashSet::new();
        let mut dangling_total = 0usize;
        let mut unused_total = 0usize;
        for event in events {
            let Some(parsed) = parse_message_citation_event(&event) else {
                continue;
            };
            if parsed.text.trim().is_empty() {
                continue;
            }
            let referenced = citation_keys_in_text(&parsed.text);
            let available = parsed
                .citations
                .iter()
                .map(|citation| citation.key.clone())
                .collect::<Vec<_>>();
            let dangling = referenced
                .iter()
                .filter(|key| !available.iter().any(|available| available == *key))
                .cloned()
                .collect::<Vec<_>>();
            let unused = available
                .iter()
                .filter(|key| !referenced.iter().any(|referenced| referenced == *key))
                .cloned()
                .collect::<Vec<_>>();
            if parsed.citations.is_empty() {
                messages_without_citations += 1;
            } else {
                messages_with_citations += 1;
            }
            total_citations += parsed.citations.len();
            for citation in &parsed.citations {
                unique_sources.insert(citation.source_id.clone());
            }
            dangling_total += dangling.len();
            unused_total += unused.len();
            messages.push(CitationAuditMessage {
                message_id: parsed.message_id,
                agent_name: parsed.agent_name,
                excerpt: truncate_chars(&parsed.text, 240),
                citation_count: parsed.citations.len(),
                referenced_keys: referenced,
                dangling_keys: dangling,
                unused_keys: unused,
                created_at: parsed.created_at,
            });
        }
        Ok(SessionCitationAuditResult {
            audit: CitationAudit {
                messages_with_citations,
                messages_without_citations,
                total_citations,
                unique_sources: unique_sources.len(),
                dangling_key_count: dangling_total,
                unused_citation_count: unused_total,
                messages,
            },
        })
    }

    /// Skill activations derived from the persistent `skill.activated` events.
    /// Events remain the source of truth; no second usage table can drift.
    pub async fn session_skill_activations(
        &self,
        session_id: &str,
    ) -> Result<SessionSkillActivationsResult> {
        let events = self.recent_events(session_id, 10_000).await?;
        let mut activations = Vec::new();
        for event in events {
            if event.kind != "skill.activated" {
                continue;
            }
            let payload = &event.payload;
            let Some(name) = payload.get("name").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let string = |key: &str| {
                payload
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            let list = |key: &str| {
                payload
                    .get(key)
                    .and_then(serde_json::Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(ToOwned::to_owned)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            };
            activations.push(SkillActivation {
                name: name.to_string(),
                slug: string("slug"),
                version: string("version"),
                role: string("role"),
                profile: string("profile"),
                tool_allow: list("tool_allow"),
                tool_deny: list("tool_deny"),
                agent_id: payload
                    .get("agent_id")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                agent_name: payload
                    .get("agent_name")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                task_id: payload
                    .get("task_id")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                activated_at: event.created_at.clone(),
            });
        }
        Ok(SessionSkillActivationsResult { activations })
    }

    // ---- Project knowledge sources -------------------------------------

    pub async fn list_sources(
        &self,
        project_id: String,
        query: Option<String>,
        status: Option<String>,
        limit: Option<u32>,
    ) -> Result<SourceListResult> {
        let store = self.store.clone();
        let project_id_for_query = project_id.clone();
        let sources = tokio::task::spawn_blocking(move || {
            store.list_sources(
                &project_id_for_query,
                query.as_deref(),
                status.as_deref(),
                limit,
            )
        })
        .await??;
        let store = self.store.clone();
        let stats = tokio::task::spawn_blocking(move || store.source_stats(&project_id)).await??;
        Ok(SourceListResult { sources, stats })
    }

    pub async fn get_source(
        &self,
        source_id: String,
        offset: Option<u32>,
        limit: Option<u32>,
    ) -> Result<SourceGetResult> {
        let store = self.store.clone();
        let id = source_id.clone();
        let source = tokio::task::spawn_blocking(move || store.get_source(&id))
            .await??
            .ok_or_else(|| anyhow!("source not found: {source_id}"))?;
        let store = self.store.clone();
        let id = source_id.clone();
        let chunks = tokio::task::spawn_blocking(move || {
            store.source_chunks(&id, offset.unwrap_or(0), limit.unwrap_or(50))
        })
        .await??;
        let store = self.store.clone();
        let total =
            tokio::task::spawn_blocking(move || store.source_chunk_count(&source_id)).await??;
        Ok(SourceGetResult {
            source,
            chunks,
            total_chunks: total,
        })
    }

    pub async fn add_source_note(&self, params: SourceCreateNoteParams) -> Result<Source> {
        let project = self.project_for_sources(&params.project_id).await?;
        validate_source_title(&params.title)?;
        let labels = validate_source_labels(params.labels)?;
        self.ingest_source(
            &project,
            "note",
            normalize_source_collection(params.collection.as_deref())?,
            params.title,
            None,
            Some("text/markdown".to_string()),
            params.content,
            labels,
        )
        .await
    }

    pub async fn add_source_file(&self, params: SourceCreateFileParams) -> Result<Source> {
        let project = self.project_for_sources(&params.project_id).await?;
        validate_source_title(params.title.as_deref().unwrap_or("source"))?;
        let labels = validate_source_labels(params.labels)?;
        let root = PathBuf::from(&project.root);
        let relative = params.path.clone();
        let file =
            tokio::task::spawn_blocking(move || read_source_file(&root, &relative)).await??;
        let title = params.title.unwrap_or_else(|| file.relative.clone());
        validate_source_title(&title)?;
        self.ingest_source(
            &project,
            "file",
            normalize_source_collection(params.collection.as_deref())?,
            title,
            Some(file.relative),
            Some(file.mime_type),
            file.content,
            labels,
        )
        .await
    }

    pub async fn add_source_url(&self, params: SourceCreateUrlParams) -> Result<Source> {
        let project = self.project_for_sources(&params.project_id).await?;
        let url = params.url.trim();
        if url.is_empty() || url.len() > 2_000 {
            anyhow::bail!("source URL must be 1-2000 characters");
        }
        let fetched = crate::tools::fetch_public_url(
            url,
            1_000_000,
            crate::sources::MAX_SOURCE_BYTES as usize,
        )
        .await?;
        if fetched.truncated {
            tracing::warn!(url, "source URL response was truncated by the fetch limit");
        }
        let is_html = fetched.content_type.to_ascii_lowercase().contains("html")
            || fetched.text.trim_start().starts_with("<!doctype")
            || fetched.text.trim_start().starts_with("<html");
        let content = if is_html {
            crate::sources::html_to_text(&fetched.text)
        } else {
            fetched.text
        };
        if content.trim().is_empty() {
            anyhow::bail!("the URL returned no readable text");
        }
        let title = params
            .title
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| source_title_from_url(url));
        validate_source_title(&title)?;
        let labels = validate_source_labels(params.labels)?;
        self.ingest_source(
            &project,
            "url",
            normalize_source_collection(params.collection.as_deref())?,
            title,
            Some(url.to_string()),
            Some(fetched.content_type),
            content,
            labels,
        )
        .await
    }

    /// Index text-like files under the project root. Existing source URIs are
    /// skipped; binary, oversized, malformed, and ignored files are counted as
    /// skipped rather than silently becoming partial sources.
    pub async fn import_sources(&self, params: SourceImportParams) -> Result<SourceImportResult> {
        let project = self.project_for_sources(&params.project_id).await?;
        let labels = validate_source_labels(params.labels)?;
        let root = PathBuf::from(&project.root);
        let existing = {
            let store = self.store.clone();
            let project_id = project.id.clone();
            tokio::task::spawn_blocking(move || {
                store.list_sources(&project_id, None, None, Some(1_000))
            })
            .await??
        };
        let existing_uris = existing
            .iter()
            .filter_map(|source| source.uri.clone())
            .collect::<std::collections::HashSet<_>>();
        let relative = params.path.clone();
        let max_files = params.max_files.unwrap_or(100).clamp(1, 500) as usize;
        let scan = tokio::task::spawn_blocking(move || {
            collect_import_files(&root, relative.as_deref(), max_files)
        })
        .await??;
        let mut imported = Vec::new();
        let mut skipped = scan.skipped;
        let mut errors = Vec::new();
        for file in scan.files {
            if existing_uris.contains(&file.relative) {
                skipped += 1;
                continue;
            }
            match self
                .ingest_source(
                    &project,
                    "file",
                    normalize_source_collection(params.collection.as_deref())?,
                    file.title.clone(),
                    Some(file.relative.clone()),
                    Some(file.mime_type.clone()),
                    file.content,
                    labels.clone(),
                )
                .await
            {
                Ok(source) => imported.push(source),
                Err(error) => errors.push(SourceImportError {
                    path: file.relative,
                    error: format!("{error:#}"),
                }),
            }
        }
        Ok(SourceImportResult {
            imported,
            skipped,
            errors,
        })
    }

    pub async fn delete_source(&self, source_id: String) -> Result<SourceDeleteResult> {
        let store = self.store.clone();
        let id = source_id.clone();
        let source = tokio::task::spawn_blocking(move || store.get_source(&id)).await??;
        let Some(source) = source else {
            return Ok(SourceDeleteResult { deleted: false });
        };
        // Remove the durable snapshot first. If the row delete then fails the
        // source remains visible but cannot be rebuilt from a stale snapshot;
        // the reverse order could resurrect a deleted source in `db-rebuild`.
        let store = self.store.clone();
        let project_id = source.project_id.clone();
        let id = source_id.clone();
        let deleted = tokio::task::spawn_blocking(move || {
            store.delete_source_snapshot(&project_id, &id)?;
            store.delete_source(&id)
        })
        .await??;
        Ok(SourceDeleteResult { deleted })
    }

    pub async fn reindex_source(&self, source_id: String) -> Result<SourceReindexResult> {
        let store = self.store.clone();
        let id = source_id.clone();
        let source = tokio::task::spawn_blocking(move || store.get_source(&id))
            .await??
            .ok_or_else(|| anyhow!("source not found: {source_id}"))?;
        let store = self.store.clone();
        let project_id = source.project_id.clone();
        let id = source_id.clone();
        let snapshot =
            tokio::task::spawn_blocking(move || store.read_source_snapshot(&project_id, &id))
                .await??
                .ok_or_else(|| {
                    anyhow!("source snapshot is missing; delete and re-add the source")
                })?;
        self.replace_source_content(source, snapshot.content, None)
            .await
    }

    /// Re-read a file or public URL and replace the indexed content. Notes are
    /// reindexed from their durable snapshot.
    pub async fn refresh_source(&self, source_id: String) -> Result<SourceReindexResult> {
        let store = self.store.clone();
        let id = source_id.clone();
        let source = tokio::task::spawn_blocking(move || store.get_source(&id))
            .await??
            .ok_or_else(|| anyhow!("source not found: {source_id}"))?;
        let project = self.project_for_sources(&source.project_id).await?;
        let (content, mime_type) = match source.kind.as_str() {
            "file" => {
                let relative = source
                    .uri
                    .clone()
                    .ok_or_else(|| anyhow!("file source has no path"))?;
                let root = PathBuf::from(&project.root);
                let file = tokio::task::spawn_blocking(move || read_source_file(&root, &relative))
                    .await??;
                (file.content, Some(file.mime_type))
            }
            "url" => {
                let url = source
                    .uri
                    .clone()
                    .ok_or_else(|| anyhow!("URL source has no URL"))?;
                let fetched = crate::tools::fetch_public_url(
                    &url,
                    1_000_000,
                    crate::sources::MAX_SOURCE_BYTES as usize,
                )
                .await?;
                let is_html = fetched.content_type.to_ascii_lowercase().contains("html")
                    || fetched.text.trim_start().starts_with("<!doctype")
                    || fetched.text.trim_start().starts_with("<html");
                let content = if is_html {
                    crate::sources::html_to_text(&fetched.text)
                } else {
                    fetched.text
                };
                (content, Some(fetched.content_type))
            }
            _ => {
                let store = self.store.clone();
                let project_id = source.project_id.clone();
                let id = source.id.clone();
                let snapshot = tokio::task::spawn_blocking(move || {
                    store.read_source_snapshot(&project_id, &id)
                })
                .await??
                .ok_or_else(|| anyhow!("source snapshot is missing"))?;
                (snapshot.content, None)
            }
        };
        if content.trim().is_empty() {
            anyhow::bail!("the refreshed source returned no readable text");
        }
        self.replace_source_content(source, content, mime_type)
            .await
    }

    async fn replace_source_content(
        &self,
        source: Source,
        content: String,
        mime_type: Option<String>,
    ) -> Result<SourceReindexResult> {
        let content = crate::sources::normalize_source_text(&content);
        if content.chars().count() > crate::sources::MAX_SOURCE_CHARS
            || content.len() > crate::sources::MAX_SOURCE_BYTES as usize
        {
            anyhow::bail!(
                "source is too large (limit {} bytes)",
                crate::sources::MAX_SOURCE_BYTES
            );
        }
        let chunks = build_source_chunks(&source.id, &content);
        if chunks.is_empty() {
            anyhow::bail!("source produced no readable chunks");
        }
        let now = now_rfc3339();
        let mut updated = source;
        updated.revision = updated.revision.saturating_add(1);
        updated.chunk_count = chunks.len();
        updated.char_count = content.chars().count();
        updated.token_count = chunks
            .iter()
            .map(|chunk| u64::from(chunk.token_count))
            .sum();
        updated.status = "indexed".to_string();
        updated.error = None;
        updated.size_bytes = content.len() as u64;
        updated.sha256 = sha256_hex(content.as_bytes());
        updated.updated_at = now.clone();
        updated.indexed_at = Some(now);
        if let Some(mime_type) = mime_type {
            updated.mime_type = Some(mime_type);
        }
        let snapshot = SourceSnapshot {
            version: 1,
            source: updated.clone(),
            content,
        };
        let store = self.store.clone();
        let task = updated.clone();
        let chunks_for_db = chunks.clone();
        tokio::task::spawn_blocking(move || {
            store.write_source_snapshot(&snapshot)?;
            store.insert_source(&task, &chunks_for_db)
        })
        .await??;
        Ok(SourceReindexResult {
            source: updated,
            chunks: chunks.len(),
        })
    }

    pub async fn search_sources(
        &self,
        project_id: String,
        query: String,
        source_ids: Vec<String>,
        limit: Option<u32>,
    ) -> Result<SourceSearchResult> {
        self.search_sources_with_mode(
            project_id,
            query,
            source_ids,
            Vec::new(),
            Vec::new(),
            limit,
            None,
        )
        .await
    }

    /// Search with explicit collection/label filters. An empty collection list
    /// means all collections; the empty string selects uncollected sources.
    pub async fn search_sources_scoped(
        &self,
        project_id: String,
        query: String,
        source_ids: Vec<String>,
        collections: Vec<String>,
        labels: Vec<String>,
        limit: Option<u32>,
    ) -> Result<SourceSearchResult> {
        self.search_sources_with_mode(
            project_id,
            query,
            source_ids,
            collections,
            labels,
            limit,
            None,
        )
        .await
    }

    /// Source retrieval with an explicit lane. `None` selects the
    /// backward-compatible dynamic default: hybrid when the project opted into
    /// source embeddings, otherwise lexical.
    ///
    /// Retrieval v2 layers on top of the unchanged lexical and exact-cosine
    /// lanes: optional deterministic IVF candidate selection, deterministic
    /// rerank, and optional lexical query expansion. The exact scan remains the
    /// fallback for every ANN condition, and every fallback reason is reported.
    #[allow(clippy::too_many_arguments)]
    pub async fn search_sources_with_mode(
        &self,
        project_id: String,
        query: String,
        source_ids: Vec<String>,
        collections: Vec<String>,
        labels: Vec<String>,
        limit: Option<u32>,
        mode: Option<SourceSearchMode>,
    ) -> Result<SourceSearchResult> {
        let store = self.store.clone();
        let policy_project = project_id.clone();
        let (embedding_policy, retrieval_policy) = tokio::task::spawn_blocking(move || {
            let embedding = store.get_source_embedding_policy(&policy_project)?;
            let retrieval = store.get_source_retrieval_policy(&policy_project)?;
            Ok::<_, anyhow::Error>((embedding, retrieval))
        })
        .await??;
        let (requested, wants_semantic) =
            crate::source_retrieval::source_effective_mode(mode, Some(&embedding_policy));
        let requested_was_default = mode.is_none();
        let requested_label = requested.as_str().to_string();
        let Some(parsed) = crate::sources::source_query(&query) else {
            return Ok(SourceSearchResult {
                hits: Vec::new(),
                degraded: false,
                mode: SourceSearchMode::Lexical.as_str().to_string(),
                requested_mode: requested_label,
                semantic_unavailable: wants_semantic.then(|| "empty_query".to_string()),
                candidates_scanned: 0,
                index_path: "exact".to_string(),
                rerank: retrieval_policy.rerank.as_str().to_string(),
                expansion: retrieval_policy.query_expansion.as_str().to_string(),
                ..SourceSearchResult::default()
            });
        };
        let search = SourceSearchQuery {
            all: parsed.all,
            any: parsed.any,
            terms: parsed.terms,
            raw: parsed.raw.clone(),
            prefer_like: parsed.prefer_like,
            collections: normalize_source_filters(collections, 24, 80, "collection")?,
            labels: normalize_source_filters(labels, 24, 40, "label")?,
        };
        let final_limit = limit.unwrap_or(8).clamp(1, 50);
        let request = crate::source_retrieval::SourceRetrievalRequest {
            project_id: project_id.clone(),
            query: search,
            source_ids,
            limit: final_limit,
            policy: retrieval_policy.clone(),
        };

        if !wants_semantic {
            let semantic_unavailable = if requested_was_default && !embedding_policy.is_active() {
                crate::source_embedding::source_semantic_unavailable(Some(&embedding_policy), 0, 0)
            } else {
                None
            };
            let mut result = self
                .lexical_source_result(request, semantic_unavailable)
                .await?;
            result.requested_mode = requested_label;
            return Ok(result);
        }

        // The dynamic default asks for hybrid only when the lane is active; an
        // explicit semantic/hybrid request still reports why it fell back.
        if !embedding_policy.is_active() {
            let reason =
                crate::source_embedding::source_semantic_unavailable(Some(&embedding_policy), 0, 0)
                    .unwrap_or_else(|| "disabled".to_string());
            return self
                .lexical_source_fallback(request, reason, requested_label)
                .await;
        }
        let client =
            match crate::source_embedding::SourceEmbeddingClient::from_policy(&embedding_policy) {
                Ok(client) => client,
                Err(error) => {
                    return self
                        .lexical_source_fallback(request, error.reason, requested_label)
                        .await;
                }
            };
        let store = self.store.clone();
        let project = project_id.clone();
        let model = client.model().to_string();
        let dimensions = client.dimensions();
        let max_chunks = embedding_policy.max_chunks_per_source;
        let ready = tokio::task::spawn_blocking(move || {
            store.source_embedding_ready_count(&project, &model, dimensions, max_chunks)
        })
        .await??;
        if ready == 0 {
            let store = self.store.clone();
            let project = project_id.clone();
            let stale =
                tokio::task::spawn_blocking(move || store.source_embedding_stale_count(&project))
                    .await??;
            let reason = crate::source_embedding::source_semantic_unavailable(
                Some(&embedding_policy),
                ready,
                stale,
            )
            .unwrap_or_else(|| "no_indexed_vectors".to_string());
            return self
                .lexical_source_fallback(request, reason, requested_label)
                .await;
        }

        // Query expansion is planned from a bounded first lexical pass before
        // the query is embedded. The expanded query is embedded in the same
        // bounded batch as the original; no second network round trip is made.
        let expansion_terms = if retrieval_policy.query_expansion
            == vp_protocol::types::SourceQueryExpansionMode::Lexical
        {
            let store = self.store.clone();
            let plan_request = request.clone();
            match tokio::task::spawn_blocking(move || {
                crate::source_retrieval::plan_expansion(&store, &plan_request, requested)
            })
            .await?
            {
                Ok(terms) => terms,
                Err(error) => {
                    tracing::warn!(%error, "source query expansion planning failed");
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };
        let mut texts = vec![crate::source_embedding::source_embedding_query_text(
            &parsed.raw,
        )];
        if !expansion_terms.is_empty() {
            let expanded_text = crate::source_embedding::source_embedding_query_text(&format!(
                "{} {}",
                parsed.raw,
                expansion_terms.join(" ")
            ));
            if expanded_text != texts[0] {
                texts.push(expanded_text);
            }
        }
        let batch = match client.embed(&texts).await {
            Ok(batch) => batch,
            Err(error) => {
                let code = crate::source_embedding::source_embedding_error_code(&error);
                return self
                    .lexical_source_fallback(
                        request,
                        format!("request_failed:{code}"),
                        requested_label,
                    )
                    .await;
            }
        };
        let mut items = batch.items.into_iter();
        let original = items.next();
        let Some(original) = original else {
            return self
                .lexical_source_fallback(
                    request,
                    "request_failed:invalid_response".to_string(),
                    requested_label,
                )
                .await;
        };
        if let Some(error) = original.error {
            let code = crate::source_embedding::source_embedding_item_error_code(&error);
            return self
                .lexical_source_fallback(request, format!("request_failed:{code}"), requested_label)
                .await;
        }
        let Some(query_vector) = original
            .vector
            .filter(|vector| vector.len() == dimensions as usize)
        else {
            return self
                .lexical_source_fallback(
                    request,
                    "request_failed:invalid_dimensions".to_string(),
                    requested_label,
                )
                .await;
        };
        let expanded_vector = if texts.len() > 1 {
            items.next().and_then(|item| {
                if item.error.is_some() {
                    return None;
                }
                item.vector
                    .filter(|vector| vector.len() == dimensions as usize)
            })
        } else {
            None
        };
        let context = crate::source_retrieval::SourceSemanticContext {
            model: client.model().to_string(),
            dimensions,
            max_chunks,
            query_vector,
            expanded_vector,
            expansion_terms,
        };
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            crate::source_retrieval::search_semantic(&store, &request, &context, requested)
        })
        .await?
    }

    async fn lexical_source_result(
        &self,
        request: crate::source_retrieval::SourceRetrievalRequest,
        semantic_unavailable: Option<String>,
    ) -> Result<SourceSearchResult> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            crate::source_retrieval::search_lexical(&store, &request, semantic_unavailable)
        })
        .await?
    }

    /// Every semantic preflight failure funnels through one deterministic
    /// lexical path so the fallback shape and reason cannot drift from the
    /// successful lexical path.
    async fn lexical_source_fallback(
        &self,
        request: crate::source_retrieval::SourceRetrievalRequest,
        reason: String,
        requested_label: String,
    ) -> Result<SourceSearchResult> {
        let mut result = self
            .lexical_source_result(request, Some(reason.clone()))
            .await?;
        result.requested_mode = requested_label;
        result.index_fallback_reason = Some(reason);
        Ok(result)
    }

    pub async fn source_stats(&self, project_id: String) -> Result<SourceStats> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.source_stats(&project_id)).await?
    }

    // ---- Semantic source index -----------------------------------------

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

    pub async fn source_collections(&self, project_id: String) -> Result<SourceCollectionsResult> {
        let store = self.store.clone();
        let project_for_count = project_id.clone();
        let collections =
            tokio::task::spawn_blocking(move || store.list_source_collections(&project_id))
                .await??;
        let store = self.store.clone();
        let stats =
            tokio::task::spawn_blocking(move || store.source_stats(&project_for_count)).await??;
        Ok(SourceCollectionsResult {
            total_sources: stats.sources,
            collections,
        })
    }

    /// Replace user-editable source metadata and keep the durable snapshot in
    /// sync in the same blocking section. Content/chunks are untouched.
    pub async fn update_source_metadata(
        &self,
        params: UpdateSourceMetadataParams,
    ) -> Result<Source> {
        let title = params.title.trim();
        validate_source_title(title)?;
        let collection = normalize_source_collection(params.collection.as_deref())?;
        let labels = validate_source_labels(params.labels)?;
        let store = self.store.clone();
        let id = params.source_id.clone();
        let current = tokio::task::spawn_blocking(move || store.get_source(&id))
            .await??
            .ok_or_else(|| anyhow!("source not found: {}", params.source_id))?;
        let mut updated = current;
        updated.title = title.to_string();
        updated.collection = collection;
        updated.labels = labels;
        updated.updated_at = now_rfc3339();
        let store = self.store.clone();
        let task = updated.clone();
        let stored = tokio::task::spawn_blocking(move || {
            let updated = store.update_source_metadata(&task)?;
            if let Some(mut snapshot) =
                store.read_source_snapshot(&updated.project_id, &updated.id)?
            {
                snapshot.source = updated.clone();
                store.write_source_snapshot(&snapshot)?;
            }
            Ok::<_, anyhow::Error>(updated)
        })
        .await??;
        Ok(stored)
    }

    pub async fn set_source_collection(
        &self,
        source_id: String,
        collection: Option<String>,
    ) -> Result<Source> {
        let collection = normalize_source_collection(collection.as_deref())?;
        let store = self.store.clone();
        let id = source_id.clone();
        let current = tokio::task::spawn_blocking(move || store.get_source(&id))
            .await??
            .ok_or_else(|| anyhow!("source not found: {source_id}"))?;
        let mut updated = current;
        updated.collection = collection;
        updated.updated_at = now_rfc3339();
        let store = self.store.clone();
        let task = updated.clone();
        let stored = tokio::task::spawn_blocking(move || {
            let updated = store.update_source_metadata(&task)?;
            if let Some(mut snapshot) =
                store.read_source_snapshot(&updated.project_id, &updated.id)?
            {
                snapshot.source = updated.clone();
                store.write_source_snapshot(&snapshot)?;
            }
            Ok::<_, anyhow::Error>(updated)
        })
        .await??;
        Ok(stored)
    }

    pub async fn set_source_labels(
        &self,
        source_id: String,
        labels: Vec<String>,
    ) -> Result<Source> {
        let labels = validate_source_labels(labels)?;
        let store = self.store.clone();
        let id = source_id.clone();
        let current = tokio::task::spawn_blocking(move || store.get_source(&id))
            .await??
            .ok_or_else(|| anyhow!("source not found: {source_id}"))?;
        let mut updated = current;
        updated.labels = labels;
        updated.updated_at = now_rfc3339();
        let store = self.store.clone();
        let task = updated.clone();
        let stored = tokio::task::spawn_blocking(move || {
            let updated = store.update_source_metadata(&task)?;
            if let Some(mut snapshot) =
                store.read_source_snapshot(&updated.project_id, &updated.id)?
            {
                snapshot.source = updated.clone();
                store.write_source_snapshot(&snapshot)?;
            }
            Ok::<_, anyhow::Error>(updated)
        })
        .await??;
        Ok(stored)
    }

    /// Short source catalog for agent system prompts. Only indexed titles are
    /// exposed; the agent still has to call `search_sources` for content.
    pub async fn source_brief(&self, project_id: &str) -> Result<String> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let sources = tokio::task::spawn_blocking(move || {
            store.list_sources(&project_id, None, Some("indexed"), Some(8))
        })
        .await??;
        if sources.is_empty() {
            return Ok(String::new());
        }
        Ok(sources
            .iter()
            .map(|source| {
                let collection = source
                    .collection
                    .as_deref()
                    .map(|value| format!("[{value}] "))
                    .unwrap_or_default();
                format!("- {collection}{} ({})", source.title, source.kind)
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    /// Rebuild the SQLite source projection from durable JSON snapshots. Used
    /// by `db-rebuild`; malformed snapshots are skipped and counted by the
    /// caller's rebuild report.
    pub async fn restore_sources_from_disk(&self) -> Result<usize> {
        let store = self.store.clone();
        let snapshots =
            tokio::task::spawn_blocking(move || store.list_source_snapshots()).await??;
        let mut restored = 0usize;
        for snapshot in snapshots {
            let project = {
                let store = self.store.clone();
                let id = snapshot.source.project_id.clone();
                tokio::task::spawn_blocking(move || store.get_project(&id)).await??
            };
            if project.is_none() {
                tracing::warn!(
                    source_id = %snapshot.source.id,
                    "skipping source snapshot for a missing project"
                );
                continue;
            }
            let content = crate::sources::normalize_source_text(&snapshot.content);
            let chunks = build_source_chunks(&snapshot.source.id, &content);
            if chunks.is_empty() {
                continue;
            }
            let now = now_rfc3339();
            let mut source = snapshot.source.clone();
            source.chunk_count = chunks.len();
            source.char_count = content.chars().count();
            source.token_count = chunks
                .iter()
                .map(|chunk| u64::from(chunk.token_count))
                .sum();
            source.status = "indexed".to_string();
            source.error = None;
            source.updated_at = now.clone();
            source.indexed_at = Some(now);
            let store = self.store.clone();
            let source_for_db = source.clone();
            let chunks_for_db = chunks.clone();
            tokio::task::spawn_blocking(move || {
                store.insert_source(&source_for_db, &chunks_for_db)
            })
            .await??;
            restored += 1;
        }
        Ok(restored)
    }

    async fn project_for_sources(&self, project_id: &str) -> Result<Project> {
        let store = self.store.clone();
        let id = project_id.to_string();
        tokio::task::spawn_blocking(move || store.get_project(&id))
            .await??
            .ok_or_else(|| anyhow!("project not found: {project_id}"))
    }

    #[allow(clippy::too_many_arguments)]
    async fn ingest_source(
        &self,
        project: &Project,
        kind: &str,
        collection: Option<String>,
        title: String,
        uri: Option<String>,
        mime_type: Option<String>,
        content: String,
        labels: Vec<String>,
    ) -> Result<Source> {
        validate_source_title(&title)?;
        let content = crate::sources::normalize_source_text(&content);
        if content.trim().is_empty() {
            anyhow::bail!("source content must not be empty");
        }
        if content.chars().count() > crate::sources::MAX_SOURCE_CHARS
            || content.len() > crate::sources::MAX_SOURCE_BYTES as usize
        {
            anyhow::bail!(
                "source is too large (limit {} bytes)",
                crate::sources::MAX_SOURCE_BYTES
            );
        }
        let id = Uuid::now_v7().to_string();
        let chunks = build_source_chunks(&id, &content);
        if chunks.is_empty() {
            anyhow::bail!("source produced no readable chunks");
        }
        let now = now_rfc3339();
        let source = Source {
            id: id.clone(),
            project_id: project.id.clone(),
            kind: kind.to_string(),
            collection,
            title,
            uri,
            mime_type,
            status: "indexed".to_string(),
            size_bytes: content.len() as u64,
            sha256: sha256_hex(content.as_bytes()),
            revision: 1,
            chunk_count: chunks.len(),
            char_count: content.chars().count(),
            token_count: chunks
                .iter()
                .map(|chunk| u64::from(chunk.token_count))
                .sum(),
            error: None,
            labels,
            created_at: now.clone(),
            updated_at: now.clone(),
            indexed_at: Some(now),
        };
        let snapshot = SourceSnapshot {
            version: 1,
            source: source.clone(),
            content,
        };
        let store = self.store.clone();
        let source_for_db = source.clone();
        let chunks_for_db = chunks.clone();
        tokio::task::spawn_blocking(move || {
            store.write_source_snapshot(&snapshot)?;
            store.insert_source(&source_for_db, &chunks_for_db)
        })
        .await??;
        Ok(source)
    }

    // ---- Project skill packs -------------------------------------------

    pub async fn list_skills(
        &self,
        project_id: String,
        include_disabled: bool,
    ) -> Result<SkillListResult> {
        self.project_for_sources(&project_id).await?;
        let store = self.store.clone();
        let skills = tokio::task::spawn_blocking(move || {
            store.list_skills(Some(&project_id), include_disabled)
        })
        .await??;
        Ok(SkillListResult { skills })
    }

    pub async fn create_skill(&self, params: SkillCreateParams) -> Result<Skill> {
        let project = self.project_for_sources(&params.project_id).await?;
        let skill = build_skill(
            &project.id,
            &params.name,
            &params.description,
            &params.when_to_use,
            params.version.as_deref(),
            &params.instructions,
            params.tool_allow,
            params.tool_deny,
            params.profiles,
            params.roles,
            "user",
            true,
        )?;
        self.ensure_skill_slug_free(&project.id, &skill.slug, None)
            .await?;
        let store = self.store.clone();
        let stored = tokio::task::spawn_blocking(move || store.insert_skill(&skill)).await??;
        self.record_project_manifest_best_effort().await;
        Ok(stored)
    }

    pub async fn import_skill(&self, params: SkillImportParams) -> Result<Skill> {
        let project = self.project_for_sources(&params.project_id).await?;
        let root = PathBuf::from(&project.root);
        let path = params.path.clone();
        let (content, fallback_name) = tokio::task::spawn_blocking(move || {
            read_project_text_file(&root, &path, crate::skills::MAX_SKILL_FILE_BYTES).map(
                |(content, relative)| {
                    (
                        content,
                        crate::skills::skill_file_name(Path::new(&relative)),
                    )
                },
            )
        })
        .await??;
        let parsed = crate::skills::parse_skill_markdown(&content, &fallback_name)?;
        let skill = build_skill(
            &project.id,
            &parsed.name,
            &parsed.description,
            &parsed.when_to_use,
            Some(&parsed.version),
            &parsed.instructions,
            parsed.tool_allow,
            parsed.tool_deny,
            parsed.profiles,
            parsed.roles,
            "import",
            true,
        )?;
        self.ensure_skill_slug_free(&project.id, &skill.slug, None)
            .await?;
        let store = self.store.clone();
        let stored = tokio::task::spawn_blocking(move || store.insert_skill(&skill)).await??;
        self.record_project_manifest_best_effort().await;
        Ok(stored)
    }

    pub async fn update_skill(&self, params: SkillUpdateParams) -> Result<Skill> {
        let store = self.store.clone();
        let id = params.id.clone();
        let current = tokio::task::spawn_blocking(move || store.get_skill(&id))
            .await??
            .ok_or_else(|| anyhow!("skill not found: {}", params.id))?;
        let skill = build_skill(
            &current.project_id,
            &params.name,
            &params.description,
            &params.when_to_use,
            params.version.as_deref(),
            &params.instructions,
            params.tool_allow,
            params.tool_deny,
            params.profiles,
            params.roles,
            &current.source,
            current.enabled,
        )?;
        let mut updated = skill;
        updated.id = current.id;
        updated.created_at = current.created_at;
        self.ensure_skill_slug_free(&updated.project_id, &updated.slug, Some(&updated.id))
            .await?;
        let store = self.store.clone();
        let stored = tokio::task::spawn_blocking(move || store.insert_skill(&updated)).await??;
        self.record_project_manifest_best_effort().await;
        Ok(stored)
    }

    pub async fn set_skill_enabled(&self, params: SkillEnabledParams) -> Result<Skill> {
        let store = self.store.clone();
        let id = params.id.clone();
        let skill =
            tokio::task::spawn_blocking(move || store.set_skill_enabled(&id, params.enabled))
                .await??
                .ok_or_else(|| anyhow!("skill not found: {}", params.id))?;
        self.record_project_manifest_best_effort().await;
        Ok(skill)
    }

    pub async fn delete_skill(&self, id: String) -> Result<SkillDeleteResult> {
        let store = self.store.clone();
        let deleted = tokio::task::spawn_blocking(move || store.delete_skill(&id)).await??;
        self.record_project_manifest_best_effort().await;
        Ok(SkillDeleteResult { deleted })
    }

    /// Enabled skills applicable to this role/profile.
    pub async fn skills_for(
        &self,
        project_id: &str,
        role: &str,
        profile: &str,
    ) -> Result<Vec<Skill>> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let skills =
            tokio::task::spawn_blocking(move || store.list_skills(Some(&project_id), false))
                .await??;
        Ok(skills
            .into_iter()
            .filter(|skill| skill_applies(skill, role, profile))
            .collect())
    }

    /// Enabled-skill catalog for agent system prompts. Only a compact index is
    /// injected; the full instructions are loaded by the `use_skill` tool.
    pub async fn skill_catalog(
        &self,
        project_id: &str,
        role: &str,
        profile: &str,
    ) -> Result<String> {
        let skills = self.skills_for(project_id, role, profile).await?;
        let mut output = String::new();
        for skill in skills.iter().take(24) {
            let when = if skill.when_to_use.trim().is_empty() {
                String::new()
            } else {
                format!(" — {}", truncate_chars(&skill.when_to_use, 220))
            };
            let line = format!(
                "- {} ({}){}: {}\n",
                skill.name,
                skill.version,
                when,
                truncate_chars(&skill.description, 300)
            );
            if output.len() + line.len() > 4_000 {
                break;
            }
            output.push_str(&line);
        }
        Ok(output)
    }

    async fn ensure_skill_slug_free(
        &self,
        project_id: &str,
        slug: &str,
        excluding_id: Option<&str>,
    ) -> Result<()> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let slug = slug.to_string();
        let existing =
            tokio::task::spawn_blocking(move || store.get_skill_by_slug(&project_id, &slug))
                .await??;
        if let Some(existing) = existing
            && excluding_id != Some(existing.id.as_str())
        {
            anyhow::bail!("a skill with slug '{}' already exists", existing.slug);
        }
        Ok(())
    }
}

struct SourceFileInput {
    relative: String,
    title: String,
    mime_type: String,
    content: String,
}

struct ImportScan {
    files: Vec<SourceFileInput>,
    skipped: usize,
}

fn validate_source_title(title: &str) -> Result<()> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 200 || title.chars().any(char::is_control) {
        anyhow::bail!("source title must be 1-200 characters without control characters");
    }
    Ok(())
}

fn validate_source_labels(labels: Vec<String>) -> Result<Vec<String>> {
    if labels.len() > 12 {
        anyhow::bail!("a source supports at most 12 labels");
    }
    let mut output = Vec::new();
    for label in labels {
        let label = label.trim();
        if label.is_empty() || label.chars().count() > 40 || label.chars().any(char::is_control) {
            anyhow::bail!("source labels must be 1-40 characters without control characters");
        }
        if !output.iter().any(|existing| existing == label) {
            output.push(label.to_string());
        }
    }
    Ok(output)
}

fn build_source_chunks(source_id: &str, content: &str) -> Vec<SourceChunk> {
    crate::sources::chunk_text(content)
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            let digest = sha256_hex(
                format!(
                    "{}:{}:{}:{}",
                    source_id, index, chunk.start_offset, chunk.content
                )
                .as_bytes(),
            );
            SourceChunk {
                id: format!("{source_id}:{:04}:{}", index + 1, &digest[..12]),
                source_id: source_id.to_string(),
                chunk_no: (index + 1) as u32,
                heading: chunk.heading,
                content: chunk.content,
                start_offset: chunk.start_offset,
                end_offset: chunk.end_offset,
                token_count: chunk.token_count,
            }
        })
        .collect()
}

fn read_source_file(root: &Path, relative: &str) -> Result<SourceFileInput> {
    let root = root.canonicalize().context("resolving project root")?;
    let relative_path = Path::new(relative.trim());
    if relative.trim().is_empty()
        || relative_path.is_absolute()
        || relative_path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
        || relative_path.components().any(|component| {
            component
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(".git")
        })
    {
        anyhow::bail!("source path must be relative, inside the project, and outside .git");
    }
    let path = root
        .join(relative_path)
        .canonicalize()
        .with_context(|| format!("resolving source file {relative}"))?;
    if !path.starts_with(&root) {
        anyhow::bail!("source path escapes the project root");
    }
    let metadata = std::fs::metadata(&path)?;
    if !metadata.is_file() {
        anyhow::bail!("source path is not a file: {relative}");
    }
    if metadata.len() > crate::sources::MAX_SOURCE_BYTES {
        anyhow::bail!(
            "source file is {} bytes; the limit is {} bytes",
            metadata.len(),
            crate::sources::MAX_SOURCE_BYTES
        );
    }
    let bytes = std::fs::read(&path)?;
    if bytes.contains(&0) {
        anyhow::bail!("source file looks binary");
    }
    let content = String::from_utf8(bytes).context("source file is not UTF-8")?;
    let relative = path
        .strip_prefix(&root)
        .unwrap_or(&path)
        .to_string_lossy()
        .to_string();
    Ok(SourceFileInput {
        title: relative.clone(),
        mime_type: source_mime_type(&relative),
        relative,
        content,
    })
}

fn collect_import_files(
    root: &Path,
    relative: Option<&str>,
    max_files: usize,
) -> Result<ImportScan> {
    let root = root.canonicalize().context("resolving project root")?;
    let search_root = match relative.map(str::trim).filter(|value| !value.is_empty()) {
        Some(relative) => {
            let path = Path::new(relative);
            if path.is_absolute()
                || path.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                })
            {
                anyhow::bail!("source import path must be relative and inside the project");
            }
            let resolved = root
                .join(path)
                .canonicalize()
                .with_context(|| format!("resolving source import path {relative}"))?;
            if !resolved.starts_with(&root) {
                anyhow::bail!("source import path escapes the project root");
            }
            if !resolved.is_dir() {
                anyhow::bail!("source import path must be a directory");
            }
            resolved
        }
        None => root.clone(),
    };
    let walker = WalkBuilder::new(&search_root)
        .hidden(false)
        .max_depth(Some(20))
        .filter_entry(|entry| {
            !entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(".git")
        })
        .build();
    let mut files = Vec::new();
    let mut skipped = 0usize;
    for (scanned, entry) in walker.enumerate() {
        if files.len() >= max_files || scanned >= MAX_SOURCE_IMPORT_SCAN {
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() || file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let relative_path = match path.strip_prefix(&root) {
            Ok(relative) => relative.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        if !is_source_extension(path) {
            skipped += 1;
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if metadata.len() > crate::sources::MAX_SOURCE_BYTES {
            skipped += 1;
            continue;
        }
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if bytes.contains(&0) {
            skipped += 1;
            continue;
        }
        let content = match String::from_utf8(bytes) {
            Ok(content) => content,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if content.trim().is_empty() {
            skipped += 1;
            continue;
        }
        files.push(SourceFileInput {
            title: relative_path.clone(),
            mime_type: source_mime_type(&relative_path),
            relative: relative_path,
            content,
        });
    }
    Ok(ImportScan { files, skipped })
}

fn is_source_extension(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "md" | "markdown"
            | "mdx"
            | "txt"
            | "rst"
            | "org"
            | "adoc"
            | "json"
            | "jsonl"
            | "yaml"
            | "yml"
            | "toml"
            | "csv"
            | "tsv"
            | "rs"
            | "swift"
            | "py"
            | "js"
            | "jsx"
            | "ts"
            | "tsx"
            | "go"
            | "java"
            | "kt"
            | "kts"
            | "c"
            | "h"
            | "cc"
            | "cpp"
            | "hpp"
            | "m"
            | "mm"
            | "cs"
            | "rb"
            | "php"
            | "sh"
            | "bash"
            | "zsh"
            | "fish"
            | "html"
            | "htm"
            | "css"
            | "scss"
            | "less"
            | "sql"
            | "graphql"
            | "proto"
            | "xml"
            | "svg"
    )
}

fn source_mime_type(path: &str) -> String {
    match Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "md" | "markdown" | "mdx" => "text/markdown",
        "json" | "jsonl" => "application/json",
        "yaml" | "yml" => "application/yaml",
        "html" | "htm" => "text/html",
        "css" | "scss" | "less" => "text/css",
        "xml" | "svg" => "application/xml",
        "csv" | "tsv" => "text/csv",
        _ => "text/plain",
    }
    .to_string()
}

fn normalize_source_filters(
    values: Vec<String>,
    max: usize,
    max_chars: usize,
    label: &str,
) -> Result<Vec<String>> {
    if values.len() > max {
        anyhow::bail!("at most {max} source {label} filters are supported");
    }
    let mut output = Vec::new();
    for value in values {
        let value = value.trim();
        if value.chars().count() > max_chars || value.chars().any(char::is_control) {
            anyhow::bail!(
                "source {label} filters must be at most {max_chars} characters without control characters"
            );
        }
        if !output.iter().any(|existing| existing == value) {
            output.push(value.to_string());
        }
    }
    Ok(output)
}

fn normalize_source_collection(value: Option<&str>) -> Result<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.chars().count() > 80 || value.chars().any(char::is_control) {
        anyhow::bail!("source collection must be at most 80 characters without control characters");
    }
    Ok(Some(value.to_string()))
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

fn source_title_from_url(url: &str) -> String {
    let parsed = reqwest::Url::parse(url).ok();
    let host = parsed
        .as_ref()
        .and_then(|url| url.host_str())
        .unwrap_or("web");
    let tail = parsed
        .as_ref()
        .and_then(|url| url.path_segments())
        .and_then(|mut segments| segments.next_back())
        .filter(|segment| !segment.is_empty())
        .unwrap_or("");
    let title = if tail.is_empty() {
        host.to_string()
    } else {
        format!("{host} / {tail}")
    };
    title.chars().take(200).collect()
}

fn skill_applies(skill: &Skill, role: &str, profile: &str) -> bool {
    (skill.roles.is_empty()
        || skill
            .roles
            .iter()
            .any(|value| value.eq_ignore_ascii_case(role)))
        && (skill.profiles.is_empty()
            || skill
                .profiles
                .iter()
                .any(|value| value.eq_ignore_ascii_case(profile)))
}

#[allow(clippy::too_many_arguments)]
fn build_skill(
    project_id: &str,
    name: &str,
    description: &str,
    when_to_use: &str,
    version: Option<&str>,
    instructions: &str,
    tool_allow: Vec<String>,
    tool_deny: Vec<String>,
    profiles: Vec<String>,
    roles: Vec<String>,
    source: &str,
    enabled: bool,
) -> Result<Skill> {
    let name = name.trim();
    let description = description.trim();
    let when_to_use = when_to_use.trim();
    let instructions = instructions.trim();
    let version = version.unwrap_or("1.0.0").trim();
    crate::skills::validate_skill_fields(name, description, when_to_use, instructions, version)?;
    let slug = crate::skills::slugify(name);
    if slug.is_empty() {
        anyhow::bail!("skill name must contain a letter or number");
    }
    let tool_allow = normalize_skill_list(tool_allow, 64, 80, "tool")?;
    let tool_deny = normalize_skill_list(tool_deny, 64, 80, "tool")?;
    if tool_allow
        .iter()
        .any(|tool| tool_deny.iter().any(|denied| denied == tool))
    {
        anyhow::bail!("a skill tool cannot be both allowed and denied");
    }
    let profiles = normalize_skill_list(profiles, 4, 32, "profile")?;
    for profile in &profiles {
        if crate::agent::AgentProfile::parse(profile).is_none() {
            anyhow::bail!("unknown skill profile: {profile}");
        }
    }
    let roles = normalize_skill_list(roles, 5, 32, "role")?;
    for role in &roles {
        if !matches!(
            role.to_ascii_lowercase().as_str(),
            "leader" | "manager" | "worker" | "verifier" | "labour"
        ) {
            anyhow::bail!("unknown skill role: {role}");
        }
    }
    let now = now_rfc3339();
    Ok(Skill {
        id: Uuid::now_v7().to_string(),
        project_id: project_id.to_string(),
        name: name.to_string(),
        slug,
        description: description.to_string(),
        when_to_use: when_to_use.to_string(),
        version: version.to_string(),
        instructions: instructions.to_string(),
        tool_allow,
        tool_deny,
        profiles,
        roles,
        enabled,
        source: source.to_string(),
        sha256: sha256_hex(
            format!("{name}\n{description}\n{when_to_use}\n{instructions}\n{version}").as_bytes(),
        ),
        created_at: now.clone(),
        updated_at: now,
    })
}

fn normalize_skill_list(
    values: Vec<String>,
    max: usize,
    max_chars: usize,
    label: &str,
) -> Result<Vec<String>> {
    if values.len() > max {
        anyhow::bail!("skill supports at most {max} {label} entries");
    }
    let mut output = Vec::new();
    for value in values {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > max_chars {
            anyhow::bail!("skill {label} entries must be 1-{max_chars} characters");
        }
        if !output.iter().any(|existing| existing == value) {
            output.push(value.to_string());
        }
    }
    Ok(output)
}

fn read_project_text_file(root: &Path, relative: &str, max_bytes: u64) -> Result<(String, String)> {
    let root = root.canonicalize().context("resolving project root")?;
    let relative_path = Path::new(relative.trim());
    if relative.trim().is_empty()
        || relative_path.is_absolute()
        || relative_path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
        || relative_path.components().any(|component| {
            component
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(".git")
        })
    {
        anyhow::bail!("skill path must be relative, inside the project, and outside .git");
    }
    let path = root
        .join(relative_path)
        .canonicalize()
        .with_context(|| format!("resolving skill file {relative}"))?;
    if !path.starts_with(&root) {
        anyhow::bail!("skill path escapes the project root");
    }
    let metadata = std::fs::metadata(&path)?;
    if !metadata.is_file() {
        anyhow::bail!("skill path is not a file");
    }
    if metadata.len() > max_bytes {
        anyhow::bail!("skill file is larger than {max_bytes} bytes");
    }
    let content = std::fs::read_to_string(&path).context("skill file is not UTF-8")?;
    let relative = path
        .strip_prefix(&root)
        .unwrap_or(&path)
        .to_string_lossy()
        .to_string();
    Ok((content, relative))
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut output = value
        .chars()
        .take(limit.saturating_sub(1))
        .collect::<String>();
    output.push('…');
    output
}

struct ParsedCitationMessage {
    message_id: String,
    agent_name: Option<String>,
    text: String,
    citations: Vec<SourceCitation>,
    created_at: String,
}

fn parse_message_citation_event(event: &Event) -> Option<ParsedCitationMessage> {
    if event.kind != "agent.message" {
        return None;
    }
    let payload = &event.payload;
    let message_id = payload
        .get("message_id")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("{}:{}", event.session_id, event.seq));
    let text = payload
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let citations = payload
        .get("citations")
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| serde_json::from_value::<SourceCitation>(value.clone()).ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let agent_name = payload
        .get("agent_name")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            payload
                .get("agent_title")
                .and_then(serde_json::Value::as_str)
        })
        .map(ToOwned::to_owned);
    Some(ParsedCitationMessage {
        message_id,
        agent_name,
        text,
        citations,
        created_at: event.created_at.clone(),
    })
}

/// Extract `[cN]` markers without a regex. Markers are ASCII even when the
/// surrounding prose is not, so byte scanning is safe and deterministic.
fn citation_keys_in_text(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut keys = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'[' || index + 3 >= bytes.len() {
            index += 1;
            continue;
        }
        let prefix = bytes[index + 1];
        if prefix != b'c' && prefix != b'C' {
            index += 1;
            continue;
        }
        let mut end = index + 2;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end > index + 2 && end < bytes.len() && bytes[end] == b']' {
            let key = format!("c{}", &text[index + 2..end]);
            if !keys.iter().any(|existing| existing == &key) {
                keys.push(key);
            }
            index = end + 1;
        } else {
            index += 1;
        }
    }
    keys
}

const MAX_SOURCE_IMPORT_SCAN: usize = 20_000;
