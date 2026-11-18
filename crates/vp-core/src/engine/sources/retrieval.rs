use super::*;

impl Engine {
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

    /// Project-scoped source read for agent tools. A source from another
    /// project is indistinguishable from a missing source so an agent cannot
    /// probe or read another project's source namespace.
    pub async fn get_source_in_project(
        &self,
        project_id: &str,
        source_id: String,
        offset: Option<u32>,
        limit: Option<u32>,
    ) -> Result<Option<SourceGetResult>> {
        let store = self.store.clone();
        let id = source_id.clone();
        let source = tokio::task::spawn_blocking(move || store.get_source(&id)).await??;
        let Some(source) = source else {
            return Ok(None);
        };
        if source.project_id != project_id {
            return Ok(None);
        }
        let store = self.store.clone();
        let id = source_id.clone();
        let chunks = tokio::task::spawn_blocking(move || {
            store.source_chunks(&id, offset.unwrap_or(0), limit.unwrap_or(50))
        })
        .await??;
        let store = self.store.clone();
        let total =
            tokio::task::spawn_blocking(move || store.source_chunk_count(&source_id)).await??;
        Ok(Some(SourceGetResult {
            source,
            chunks,
            total_chunks: total,
        }))
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
