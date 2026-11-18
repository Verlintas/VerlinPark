//! Project source, retrieval, and embedding commands.

use super::*;

pub(super) async fn dispatch(
    command: Command,
    discovery: Discovery,
    _json: bool,
) -> Result<Option<Value>> {
    let value = match command {
        Command::Sources {
            project,
            query,
            status,
            limit,
        } => {
            rpc(
                &discovery,
                "source.list",
                json!({
                    "project_id": project,
                    "query": query,
                    "status": status,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::SourceGet {
            source_id,
            offset,
            limit,
        } => {
            rpc(
                &discovery,
                "source.get",
                json!({"source_id": source_id, "offset": offset, "limit": limit}),
            )
            .await?
        }
        Command::SourceNote {
            project,
            title,
            content,
            collection,
        } => {
            let content = if content == "-" {
                let mut buffer = String::new();
                std::io::stdin()
                    .read_to_string(&mut buffer)
                    .context("reading source note from stdin")?;
                buffer
            } else {
                content
            };
            rpc(
                &discovery,
                "source.add_note",
                json!({
                    "project_id": project,
                    "collection": collection,
                    "title": title,
                    "content": content
                }),
            )
            .await?
        }
        Command::SourceAddFile {
            project,
            path,
            title,
            collection,
        } => {
            rpc(
                &discovery,
                "source.add_file",
                json!({
                    "project_id": project,
                    "collection": collection,
                    "path": path,
                    "title": title
                }),
            )
            .await?
        }
        Command::SourceAddUrl {
            project,
            url,
            title,
            collection,
        } => {
            rpc(
                &discovery,
                "source.add_url",
                json!({
                    "project_id": project,
                    "collection": collection,
                    "url": url,
                    "title": title
                }),
            )
            .await?
        }
        Command::SourceImport {
            project,
            path,
            max_files,
            collection,
        } => {
            rpc(
                &discovery,
                "source.import",
                json!({
                    "project_id": project,
                    "collection": collection,
                    "path": path,
                    "max_files": max_files
                }),
            )
            .await?
        }
        Command::SourceRefresh { source_id } => {
            rpc(
                &discovery,
                "source.refresh",
                json!({"source_id": source_id}),
            )
            .await?
        }
        Command::SourceReindex { source_id } => {
            rpc(
                &discovery,
                "source.reindex",
                json!({"source_id": source_id}),
            )
            .await?
        }
        Command::SourceDelete { source_id } => {
            rpc(&discovery, "source.delete", json!({"source_id": source_id})).await?
        }
        Command::SourceUpdate {
            source_id,
            title,
            collection,
            labels,
        } => {
            rpc(
                &discovery,
                "source.update",
                json!({
                    "source_id": source_id,
                    "title": title,
                    "collection": collection,
                    "labels": labels
                }),
            )
            .await?
        }
        Command::SourceCollections { project } => {
            rpc(
                &discovery,
                "source.collections",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::SourceMove {
            source_id,
            collection,
        } => {
            rpc(
                &discovery,
                "source.set_collection",
                json!({"source_id": source_id, "collection": collection}),
            )
            .await?
        }
        Command::SourceLabels { source_id, labels } => {
            rpc(
                &discovery,
                "source.set_labels",
                json!({"source_id": source_id, "labels": labels}),
            )
            .await?
        }
        Command::SourceSearch {
            project,
            query,
            collections,
            labels,
            limit,
            mode,
        } => {
            rpc(
                &discovery,
                "source.search",
                json!({
                    "project_id": project,
                    "query": query,
                    "collections": collections,
                    "labels": labels,
                    "limit": limit,
                    "mode": mode
                }),
            )
            .await?
        }
        Command::SourceEmbedding { project } => {
            rpc(
                &discovery,
                "source.embedding.status",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::SourceEmbeddingReindex { project } => {
            rpc(
                &discovery,
                "source.embedding.reindex",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::SourceEmbeddingPolicy { project } => {
            rpc(
                &discovery,
                "source.embedding.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::SourceEmbeddingPolicySet {
            project,
            enable,
            disable,
            endpoint_env,
            model,
            dimensions,
            batch_size,
            max_chunks_per_source,
            retention_days,
            timeout_secs,
            api_key_env,
            clear,
        } => {
            if enable && disable {
                bail!("--enable and --disable cannot be combined");
            }
            if clear
                && (enable
                    || disable
                    || endpoint_env.is_some()
                    || model.is_some()
                    || dimensions.is_some()
                    || batch_size.is_some()
                    || max_chunks_per_source.is_some()
                    || retention_days.is_some()
                    || timeout_secs.is_some()
                    || api_key_env.is_some())
            {
                bail!("--clear cannot be combined with other policy flags");
            }
            let current = if clear {
                json!({})
            } else {
                rpc(
                    &discovery,
                    "source.embedding.policy.get",
                    json!({"project_id": project}),
                )
                .await?
                .get("policy")
                .cloned()
                .unwrap_or_default()
            };
            let mut policy = if current.is_object() {
                current
            } else {
                json!({})
            };
            let defaults = [
                ("enabled", json!(false)),
                ("dimensions", json!(1536)),
                ("batch_size", json!(8)),
                ("max_chunks_per_source", json!(200)),
                ("retention_days", json!(30)),
                ("timeout_secs", json!(30)),
            ];
            for (key, value) in defaults {
                if policy.get(key).is_none() {
                    policy[key] = value;
                }
            }
            if enable {
                policy["enabled"] = json!(true);
            }
            if disable {
                policy["enabled"] = json!(false);
            }
            if let Some(value) = endpoint_env {
                policy["endpoint_env"] = json!(value);
            }
            if clear {
                policy["endpoint_env"] = Value::Null;
                policy["api_key_env"] = Value::Null;
            }
            if let Some(value) = model {
                policy["model"] = json!(value);
            }
            if clear {
                policy["model"] = Value::Null;
            }
            if let Some(value) = dimensions {
                policy["dimensions"] = json!(value);
            }
            if let Some(value) = batch_size {
                policy["batch_size"] = json!(value);
            }
            if let Some(value) = max_chunks_per_source {
                policy["max_chunks_per_source"] = json!(value);
            }
            if let Some(value) = retention_days {
                policy["retention_days"] = json!(value);
            }
            if let Some(value) = timeout_secs {
                policy["timeout_secs"] = json!(value);
            }
            if let Some(value) = api_key_env {
                policy["api_key_env"] = json!(value);
            }
            rpc(
                &discovery,
                "source.embedding.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::SourceRetrievalPolicy { project } => {
            rpc(
                &discovery,
                "source.retrieval.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::SourceRetrievalPolicySet {
            project,
            enable_ann,
            disable_ann,
            ann_min_vectors,
            ann_nprobe,
            rerank,
            query_expansion,
            max_expanded_terms,
            clear,
        } => {
            let current = if clear {
                json!({})
            } else {
                rpc(
                    &discovery,
                    "source.retrieval.policy.get",
                    json!({"project_id": project}),
                )
                .await?
                .get("policy")
                .cloned()
                .unwrap_or_default()
            };
            let policy = merge_source_retrieval_policy(
                current,
                enable_ann,
                disable_ann,
                ann_min_vectors,
                ann_nprobe,
                rerank,
                query_expansion,
                max_expanded_terms,
                clear,
            )?;
            rpc(
                &discovery,
                "source.retrieval.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::SourceIndex { project } => {
            rpc(
                &discovery,
                "source.embedding.index_status",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::SourceIndexRebuild { project } => {
            rpc(
                &discovery,
                "source.embedding.index_rebuild",
                json!({"project_id": project}),
            )
            .await?
        }
        _ => unreachable!("command routed to the wrong dispatch family"),
    };
    Ok(Some(value))
}
