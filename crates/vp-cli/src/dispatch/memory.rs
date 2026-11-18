//! Project and identity memory plus tool-output commands.

use super::*;

pub(super) async fn dispatch(
    command: Command,
    discovery: Discovery,
    _json: bool,
) -> Result<Option<Value>> {
    let value = match command {
        Command::AgentMemory {
            project,
            identity,
            scope,
            status,
            department,
            query,
            limit,
            include_expired,
        } => {
            rpc(
                &discovery,
                "agent.memory.list",
                json!({
                    "project_id": project,
                    "identity_id": identity,
                    "scope": scope,
                    "status": status,
                    "department": department,
                    "query": query,
                    "limit": limit,
                    "include_expired": include_expired,
                }),
            )
            .await?
        }
        Command::AgentMemoryRemember {
            project,
            scope,
            content,
            identity,
            department,
            component,
            source,
            evidence,
            expires_at,
            pinned,
        } => {
            let evidence = evidence
                .iter()
                .map(|entry| parse_memory_evidence(entry))
                .collect::<Result<Vec<_>>>()?;
            rpc(
                &discovery,
                "agent.memory.remember",
                json!({
                    "project_id": project,
                    "scope": scope,
                    "content": content,
                    "identity_id": identity,
                    "department": department,
                    "component": component,
                    "source": source.unwrap_or_else(|| "operator".to_string()),
                    "evidence": evidence,
                    "expires_at": expires_at,
                    "pinned": pinned,
                }),
            )
            .await?
        }
        Command::AgentMemorySupersede {
            memory,
            replacement,
            content,
            source,
            expected_hash,
        } => {
            if replacement.is_none() && content.is_none() {
                bail!("pass --replacement or --content");
            }
            rpc(
                &discovery,
                "agent.memory.supersede",
                json!({
                    "memory_id": memory,
                    "replacement_memory_id": replacement,
                    "content": content,
                    "source": source,
                    "expected_content_hash": expected_hash,
                }),
            )
            .await?
        }
        Command::AgentMemoryForget { memory, prune } => {
            rpc(
                &discovery,
                "agent.memory.forget",
                json!({"memory_id": memory, "prune": prune}),
            )
            .await?
        }
        Command::AgentMemoryErase {
            memory,
            project,
            reason,
            yes,
        } => {
            rpc(
                &discovery,
                "agent.memory.erase",
                json!({
                    "memory_id": memory,
                    "project_id": project,
                    "reason": reason,
                    "approved": yes,
                }),
            )
            .await?
        }
        Command::AgentMemorySweep {
            project,
            older_than_days,
            limit,
            prune,
        } => {
            rpc(
                &discovery,
                "agent.memory.sweep",
                json!({
                    "project_id": project,
                    "older_than_days": older_than_days,
                    "limit": limit,
                    "prune": prune,
                }),
            )
            .await?
        }
        Command::Memory {
            project,
            query,
            kind,
            limit,
            include_stale,
            status,
        } => {
            rpc(
                &discovery,
                "memory.list",
                json!({
                    "project_id": project,
                    "query": query,
                    "kind": kind,
                    "limit": limit,
                    "include_stale": include_stale,
                    "status": status
                }),
            )
            .await?
        }
        Command::Remember {
            session,
            kind,
            title,
            body,
            tags,
        } => {
            rpc(
                &discovery,
                "memory.create",
                json!({
                    "session_id": session,
                    "kind": kind,
                    "title": title,
                    "body": body,
                    "tags": tags
                }),
            )
            .await?
        }
        Command::MemoryDelete { memory } => {
            rpc(&discovery, "memory.delete", json!({"id": memory})).await?
        }
        Command::MemorySearch {
            project,
            query,
            task,
            department,
            paths,
            kinds,
            limit,
            include_stale,
            lexical_only,
        } => {
            rpc(
                &discovery,
                "memory.search",
                json!({
                    "project_id": project,
                    "query": query,
                    "task_id": task,
                    "department": department,
                    "paths": paths,
                    "kinds": kinds,
                    "limit": limit,
                    "include_stale": include_stale,
                    "lexical_only": lexical_only
                }),
            )
            .await?
        }
        Command::MemoryBrief { session, task } => {
            rpc(
                &discovery,
                "memory.brief",
                json!({"session_id": session, "task_id": task}),
            )
            .await?
        }
        Command::MemoryUpdate {
            memory,
            status,
            pin,
            unpin,
            tags,
        } => {
            if pin && unpin {
                bail!("--pin and --unpin cannot be combined");
            }
            let pinned = if pin {
                Some(true)
            } else if unpin {
                Some(false)
            } else {
                None
            };
            let tags = if tags.is_empty() { None } else { Some(tags) };
            if status.is_none() && pinned.is_none() && tags.is_none() {
                bail!("memory update requires --status, --pin/--unpin, or --tag");
            }
            rpc(
                &discovery,
                "memory.update",
                json!({
                    "id": memory,
                    "status": status,
                    "pinned": pinned,
                    "tags": tags
                }),
            )
            .await?
        }
        Command::MemoryVerify { memory } => {
            rpc(&discovery, "memory.verify", json!({"id": memory})).await?
        }
        Command::MemorySweep {
            project,
            days,
            dry_run,
        } => {
            rpc(
                &discovery,
                "memory.sweep",
                json!({
                    "project_id": project,
                    "older_than_days": days,
                    "dry_run": dry_run
                }),
            )
            .await?
        }
        Command::MemoryStats { project } => {
            rpc(&discovery, "memory.stats", json!({"project_id": project})).await?
        }
        Command::MemoryConflicts {
            project,
            status,
            limit,
        } => {
            rpc(
                &discovery,
                "memory.conflicts",
                json!({"project_id": project, "status": status, "limit": limit}),
            )
            .await?
        }
        Command::MemoryConflictScan { project } => {
            rpc(
                &discovery,
                "memory.conflict.scan",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::MemoryConflictResolve { id, action, note } => {
            let action = action.replace('-', "_");
            rpc(
                &discovery,
                "memory.conflict.resolve",
                json!({"id": id, "action": action, "note": note}),
            )
            .await?
        }
        Command::MemoryHealth { project } => {
            rpc(&discovery, "memory.health", json!({"project_id": project})).await?
        }
        Command::MemoryPrune {
            project,
            days,
            keep_latest,
            dry_run,
        } => {
            rpc(
                &discovery,
                "memory.prune",
                json!({
                    "project_id": project,
                    "older_than_days": days,
                    "keep_latest": keep_latest,
                    "dry_run": dry_run
                }),
            )
            .await?
        }
        Command::MemoryPolicy { project } => {
            rpc(
                &discovery,
                "memory.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::MemoryPolicySet {
            project,
            enable,
            disable,
            max_items,
            max_age_days,
            clear,
            embedding_enable,
            embedding_disable,
            embedding_endpoint,
            embedding_model,
            embedding_dimensions,
            embedding_batch_size,
            embedding_timeout_secs,
            embedding_max_candidates,
            embedding_min_similarity,
            embedding_api_key_env,
            embedding_index,
            ivf_nlist,
            ivf_nprobe,
            ivf_min_vectors,
            ivf_rebuild_vectors,
            embedding_clear,
        } => {
            if embedding_enable && embedding_disable {
                bail!("--embedding-enable and --embedding-disable cannot be combined");
            }
            if embedding_clear
                && (embedding_enable
                    || embedding_disable
                    || embedding_endpoint.is_some()
                    || embedding_model.is_some()
                    || embedding_dimensions.is_some()
                    || embedding_batch_size.is_some()
                    || embedding_timeout_secs.is_some()
                    || embedding_max_candidates.is_some()
                    || embedding_min_similarity.is_some()
                    || embedding_api_key_env.is_some()
                    || embedding_index.is_some()
                    || ivf_nlist.is_some()
                    || ivf_nprobe.is_some()
                    || ivf_min_vectors.is_some()
                    || ivf_rebuild_vectors.is_some())
            {
                bail!("--embedding-clear cannot be combined with other embedding flags");
            }
            // Preserve v2 lifecycle settings that this command does not edit.
            let current = rpc(
                &discovery,
                "memory.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let policy = current
                .get("policy")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("memory policy get returned no policy"))?;
            let (enabled, max_items, max_age_days) =
                resolve_memory_retention(&policy, enable, disable, clear, max_items, max_age_days)?;
            let current_embedding = policy.get("embedding").cloned().unwrap_or(Value::Null);
            let embedding_flags = embedding_enable
                || embedding_disable
                || embedding_endpoint.is_some()
                || embedding_model.is_some()
                || embedding_dimensions.is_some()
                || embedding_batch_size.is_some()
                || embedding_timeout_secs.is_some()
                || embedding_max_candidates.is_some()
                || embedding_min_similarity.is_some()
                || embedding_api_key_env.is_some()
                || embedding_index.is_some()
                || ivf_nlist.is_some()
                || ivf_nprobe.is_some()
                || ivf_min_vectors.is_some()
                || ivf_rebuild_vectors.is_some();
            let embedding = if embedding_clear {
                Value::Null
            } else if !embedding_flags {
                current_embedding
            } else {
                let mut object = if current_embedding.is_object() {
                    current_embedding
                } else {
                    json!({})
                };
                if object.get("enabled").is_none() {
                    object["enabled"] = json!(false);
                }
                if object.get("dimensions").is_none() {
                    object["dimensions"] = json!(1536);
                }
                if object.get("batch_size").is_none() {
                    object["batch_size"] = json!(16);
                }
                if object.get("timeout_secs").is_none() {
                    object["timeout_secs"] = json!(30);
                }
                if object.get("max_candidates").is_none() {
                    object["max_candidates"] = json!(64);
                }
                if object.get("min_similarity").is_none() {
                    object["min_similarity"] = json!(0.20);
                }
                if object.get("index").is_none() {
                    object["index"] = json!("scan");
                }
                if object.get("ivf_nlist").is_none() {
                    object["ivf_nlist"] = json!(64);
                }
                if object.get("ivf_nprobe").is_none() {
                    object["ivf_nprobe"] = json!(8);
                }
                if object.get("ivf_min_vectors").is_none() {
                    object["ivf_min_vectors"] = json!(256);
                }
                if object.get("ivf_rebuild_vectors").is_none() {
                    object["ivf_rebuild_vectors"] = json!(4096);
                }
                if embedding_enable {
                    object["enabled"] = json!(true);
                }
                if embedding_disable {
                    object["enabled"] = json!(false);
                }
                if let Some(endpoint) = embedding_endpoint {
                    object["endpoint"] = json!(endpoint);
                }
                if let Some(model) = embedding_model {
                    object["model"] = json!(model);
                }
                if let Some(dimensions) = embedding_dimensions {
                    object["dimensions"] = json!(dimensions);
                }
                if let Some(batch_size) = embedding_batch_size {
                    object["batch_size"] = json!(batch_size);
                }
                if let Some(timeout_secs) = embedding_timeout_secs {
                    object["timeout_secs"] = json!(timeout_secs);
                }
                if let Some(max_candidates) = embedding_max_candidates {
                    object["max_candidates"] = json!(max_candidates);
                }
                if let Some(min_similarity) = embedding_min_similarity {
                    object["min_similarity"] = json!(min_similarity);
                }
                if let Some(env_name) = embedding_api_key_env {
                    object["api_key_env"] = json!(env_name);
                }
                if let Some(index) = embedding_index {
                    object["index"] = json!(index);
                }
                if let Some(nlist) = ivf_nlist {
                    object["ivf_nlist"] = json!(nlist);
                }
                if let Some(nprobe) = ivf_nprobe {
                    object["ivf_nprobe"] = json!(nprobe);
                }
                if let Some(min_vectors) = ivf_min_vectors {
                    object["ivf_min_vectors"] = json!(min_vectors);
                }
                if let Some(rebuild_vectors) = ivf_rebuild_vectors {
                    object["ivf_rebuild_vectors"] = json!(rebuild_vectors);
                }
                object
            };
            rpc(
                &discovery,
                "memory.policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "enabled": enabled,
                        "max_items": max_items,
                        "max_age_days": max_age_days,
                        "task_result_verify_days": policy.get("task_result_verify_days").cloned().unwrap_or(json!(90)),
                        "default_verify_days": policy.get("default_verify_days").cloned().unwrap_or(json!(365)),
                        "auto_supersede": policy.get("auto_supersede").cloned().unwrap_or(json!(true)),
                        "brief_max_chars": policy.get("brief_max_chars").cloned().unwrap_or(json!(4000)),
                        "sweep_stale": policy.get("sweep_stale").cloned().unwrap_or(json!(false)),
                        "embedding": embedding
                    }
                }),
            )
            .await?
        }
        Command::MemoryEmbeddings { project } => {
            rpc(
                &discovery,
                "memory.embeddings.status",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ToolOutputPolicy { project } => {
            rpc(
                &discovery,
                "tool_output.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::MemoryEmbeddingsRebuild { project } => {
            rpc(
                &discovery,
                "memory.embeddings.rebuild",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::MemoryEmbeddingsTest { project, text } => {
            rpc(
                &discovery,
                "memory.embeddings.test",
                json!({"project_id": project, "text": text}),
            )
            .await?
        }
        Command::MemoryEmbeddingsIndex { project } => {
            rpc(
                &discovery,
                "memory.embeddings.index.status",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::MemoryEmbeddingsIndexRebuild { project } => {
            rpc(
                &discovery,
                "memory.embeddings.index.rebuild",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ToolOutputPolicySet {
            project,
            disable,
            enable,
            spill_threshold,
            preview_bytes,
            max_turn_bytes,
            retention_secs,
            max_entries,
            tool_thresholds,
        } => {
            if disable && enable {
                bail!("--disable and --enable are mutually exclusive");
            }
            let current = rpc(
                &discovery,
                "tool_output.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_default();
            if !policy.is_object() {
                policy = json!({});
            }
            if let Some(object) = policy.as_object_mut() {
                if disable {
                    object.insert("enabled".into(), json!(false));
                }
                if enable {
                    object.insert("enabled".into(), json!(true));
                }
                if let Some(value) = spill_threshold {
                    object.insert("spill_threshold_bytes".into(), json!(value));
                }
                if let Some(value) = preview_bytes {
                    object.insert("preview_bytes".into(), json!(value));
                }
                if let Some(value) = max_turn_bytes {
                    object.insert("max_total_bytes_per_turn".into(), json!(value));
                }
                if let Some(value) = retention_secs {
                    object.insert("retention_secs".into(), json!(value));
                }
                if let Some(value) = max_entries {
                    object.insert("max_entries".into(), json!(value));
                }
                if !tool_thresholds.is_empty() {
                    let mut thresholds = object
                        .get("tool_thresholds")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    for entry in parse_tool_thresholds(&tool_thresholds)? {
                        thresholds.insert(entry.0, json!(entry.1));
                    }
                    object.insert("tool_thresholds".into(), Value::Object(thresholds));
                }
            }
            rpc(
                &discovery,
                "tool_output.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::ToolOutputs {
            project,
            session,
            tool,
            before,
            include_expired,
            limit,
        } => {
            rpc(
                &discovery,
                "tool_output.list",
                json!({
                    "project_id": project,
                    "session_id": session,
                    "tool_name": tool,
                    "before_created_at": before,
                    "include_expired": include_expired,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::ToolOutputStatus { project, session } => {
            rpc(
                &discovery,
                "tool_output.status",
                json!({"project_id": project, "session_id": session}),
            )
            .await?
        }
        Command::ToolOutputPrune {
            project,
            session,
            older_than_secs,
            max_entries,
            dry_run,
        } => {
            rpc(
                &discovery,
                "tool_output.prune",
                json!({
                    "project_id": project,
                    "session_id": session,
                    "older_than_secs": older_than_secs,
                    "max_entries": max_entries,
                    "dry_run": dry_run,
                }),
            )
            .await?
        }
        Command::ToolOutputAudit {
            project,
            verify_hashes,
            clean_orphans,
        } => {
            rpc(
                &discovery,
                "tool_output.audit",
                json!({
                    "project_id": project,
                    "verify_hashes": verify_hashes,
                    "clean_orphans": clean_orphans,
                }),
            )
            .await?
        }
        _ => unreachable!("command routed to the wrong dispatch family"),
    };
    Ok(Some(value))
}
