use super::*;

pub(super) async fn handle(
    state: &AppState,
    id: Option<serde_json::Value>,
    request: RpcRequest,
) -> RpcResponse {
    match request.method.as_str() {
        "skill.list" => match parse_params::<SkillListParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_skills(params.project_id, params.include_disabled.unwrap_or(true))
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "skill.create" => match parse_params::<SkillCreateParams>(request.params) {
            Ok(params) => match state.engine.create_skill(params).await {
                Ok(skill) => RpcResponse::success(id, skill),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "skill.import" => match parse_params::<SkillImportParams>(request.params) {
            Ok(params) => match state.engine.import_skill(params).await {
                Ok(skill) => RpcResponse::success(id, skill),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "skill.update" => match parse_params::<SkillUpdateParams>(request.params) {
            Ok(params) => match state.engine.update_skill(params).await {
                Ok(skill) => RpcResponse::success(id, skill),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "skill.set_enabled" => match parse_params::<SkillEnabledParams>(request.params) {
            Ok(params) => match state.engine.set_skill_enabled(params).await {
                Ok(skill) => RpcResponse::success(id, skill),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "skill.delete" => match parse_params::<SkillIdParams>(request.params) {
            Ok(params) => match state.engine.delete_skill(params.id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.list" => match parse_params::<SourceListParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_sources(params.project_id, params.query, params.status, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.update" => match parse_params::<UpdateSourceMetadataParams>(request.params) {
            Ok(params) => match state.engine.update_source_metadata(params).await {
                Ok(source) => RpcResponse::success(id, source),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.collections" => match parse_params::<SourceCollectionsParams>(request.params) {
            Ok(params) => match state.engine.source_collections(params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.set_collection" => {
            match parse_params::<SetSourceCollectionParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_source_collection(params.source_id, params.collection)
                    .await
                {
                    Ok(source) => RpcResponse::success(id, source),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "source.set_labels" => match parse_params::<SetSourceLabelsParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_source_labels(params.source_id, params.labels)
                .await
            {
                Ok(source) => RpcResponse::success(id, source),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.get" => match parse_params::<SourceGetParams>(request.params) {
            Ok(params) => match state
                .engine
                .get_source(params.source_id, params.offset, params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.add_note" => match parse_params::<SourceCreateNoteParams>(request.params) {
            Ok(params) => match state.engine.add_source_note(params).await {
                Ok(source) => RpcResponse::success(id, source),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.add_file" => match parse_params::<SourceCreateFileParams>(request.params) {
            Ok(params) => match state.engine.add_source_file(params).await {
                Ok(source) => RpcResponse::success(id, source),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.add_url" => match parse_params::<SourceCreateUrlParams>(request.params) {
            Ok(params) => match state.engine.add_source_url(params).await {
                Ok(source) => RpcResponse::success(id, source),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.import" => match parse_params::<SourceImportParams>(request.params) {
            Ok(params) => match state.engine.import_sources(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.delete" => match parse_params::<SourceActionParams>(request.params) {
            Ok(params) => match state.engine.delete_source(params.source_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.refresh" => match parse_params::<SourceActionParams>(request.params) {
            Ok(params) => match state.engine.refresh_source(params.source_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.reindex" => match parse_params::<SourceActionParams>(request.params) {
            Ok(params) => match state.engine.reindex_source(params.source_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.search" => match parse_params::<SourceSearchParams>(request.params) {
            Ok(params) => match state
                .engine
                .search_sources_with_mode(
                    params.project_id,
                    params.query,
                    params.source_ids,
                    params.collections,
                    params.labels,
                    params.limit,
                    params.mode,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "source.embedding.status" => {
            match parse_params::<SourceEmbeddingStatusParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .source_embedding_status(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "source.embedding.reindex" => {
            match parse_params::<SourceEmbeddingReindexParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .reindex_source_embeddings(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "source.embedding.policy.get" => {
            match parse_params::<SourceEmbeddingPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .get_source_embedding_policy(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "source.embedding.policy.set" => {
            match parse_params::<SetSourceEmbeddingPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_source_embedding_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "source.retrieval.policy.get" => {
            match parse_params::<SourceRetrievalPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .get_source_retrieval_policy(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "source.retrieval.policy.set" => {
            match parse_params::<SetSourceRetrievalPolicyParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .set_source_retrieval_policy(&params.project_id, params.policy)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => retrieval_policy_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "source.embedding.index_status" => {
            match parse_params::<SourceIndexStatusParams>(request.params) {
                Ok(params) => match state.engine.source_index_status(&params.project_id).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "source.embedding.index_rebuild" => {
            match parse_params::<SourceIndexRebuildParams>(request.params) {
                Ok(params) => match state.engine.rebuild_source_index(&params.project_id).await {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => source_index_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "skill.activations" => {
            match parse_params::<SessionSkillActivationsParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .session_skill_activations(&params.session_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "memory.list" => match parse_params::<MemoryListParams>(request.params) {
            Ok(params) => match state
                .engine
                .list_memories_filtered(
                    &params.project_id,
                    params.query,
                    params.kind,
                    params.status,
                    params.include_stale,
                    params.limit.unwrap_or(100),
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.search" => match parse_params::<MemorySearchParams>(request.params) {
            Ok(params) => match state.engine.memory_search(params).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.brief" => match parse_params::<MemoryBriefParams>(request.params) {
            Ok(params) => match state
                .engine
                .memory_brief(&params.session_id, params.task_id.as_deref())
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.update" => match parse_params::<MemoryUpdateParams>(request.params) {
            Ok(params) => match state
                .engine
                .memory_update(
                    &params.id,
                    params.status.as_deref(),
                    params.pinned,
                    params.tags,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.verify" => match parse_params::<MemoryVerifyParams>(request.params) {
            Ok(params) => match state.engine.memory_verify(&params.id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.sweep" => match parse_params::<MemorySweepParams>(request.params) {
            Ok(params) => match state
                .engine
                .memory_sweep(&params.project_id, params.older_than_days, params.dry_run)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.stats" => match parse_params::<MemoryStatsParams>(request.params) {
            Ok(params) => match state.engine.memory_stats(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.conflicts" => match parse_params::<MemoryConflictsParams>(request.params) {
            Ok(params) => match state
                .engine
                .memory_conflicts(&params.project_id, params.status.as_deref(), params.limit)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.conflict.scan" => match parse_params::<MemoryConflictScanParams>(request.params) {
            Ok(params) => match state.engine.memory_conflict_scan(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.conflict.resolve" => {
            match parse_params::<MemoryConflictResolveParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .memory_conflict_resolve(&params.id, &params.action, params.note.as_deref())
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "memory.health" => match parse_params::<MemoryHealthParams>(request.params) {
            Ok(params) => match state.engine.memory_health(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.embeddings.status" => {
            match parse_params::<MemoryEmbeddingsStatusParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .memory_embedding_status(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "memory.embeddings.rebuild" => {
            match parse_params::<MemoryEmbeddingsRebuildParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .memory_embedding_rebuild(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "memory.embeddings.test" => {
            match parse_params::<MemoryEmbeddingsTestParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .memory_embedding_test(&params.project_id, params.text.as_deref())
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "memory.embeddings.index.status" => {
            match parse_params::<MemoryEmbeddingsIndexStatusParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .memory_embedding_index_status(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "memory.embeddings.index.rebuild" => {
            match parse_params::<MemoryEmbeddingsIndexRebuildParams>(request.params) {
                Ok(params) => match state
                    .engine
                    .memory_embedding_index_rebuild(&params.project_id)
                    .await
                {
                    Ok(result) => RpcResponse::success(id, result),
                    Err(error) => internal_error(id, error),
                },
                Err(response) => response_with_id(id, response),
            }
        }
        "memory.create" => match parse_params::<MemoryCreateParams>(request.params) {
            Ok(params) => match state
                .engine
                .create_memory(
                    &params.session_id,
                    &params.kind,
                    &params.title,
                    &params.body,
                    params.tags,
                )
                .await
            {
                Ok(memory) => RpcResponse::success(id, MemoryCreateResult { memory }),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.delete" => match parse_params::<MemoryDeleteParams>(request.params) {
            Ok(params) => match state.engine.delete_memory(&params.id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.prune" => match parse_params::<MemoryPruneParams>(request.params) {
            Ok(params) => match state
                .engine
                .prune_memories(
                    &params.project_id,
                    params.older_than_days,
                    params.keep_latest,
                    params.dry_run,
                )
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.policy.get" => match parse_params::<MemoryPolicyParams>(request.params) {
            Ok(params) => match state.engine.get_memory_policy(&params.project_id).await {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        "memory.policy.set" => match parse_params::<SetMemoryPolicyParams>(request.params) {
            Ok(params) => match state
                .engine
                .set_memory_policy(&params.project_id, params.policy)
                .await
            {
                Ok(result) => RpcResponse::success(id, result),
                Err(error) => internal_error(id, error),
            },
            Err(response) => response_with_id(id, response),
        },
        _ => RpcResponse::error(
            id,
            RpcError::new(codes::METHOD_NOT_FOUND, "method not found"),
        ),
    }
}
