//! Test cache and test-impact commands.

use super::*;

pub(super) async fn dispatch(
    command: Command,
    discovery: Discovery,
    _json: bool,
) -> Result<Option<Value>> {
    let value = match command {
        Command::TestCache { project } => {
            rpc(
                &discovery,
                "test_cache.stats",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::TestCacheEntries {
            project,
            session,
            kind,
            status,
            include_expired,
            limit,
        } => {
            rpc(
                &discovery,
                "test_cache.entries",
                json!({
                    "project_id": project,
                    "session_id": session,
                    "kind": kind,
                    "status": status,
                    "include_expired": include_expired,
                    "limit": limit,
                }),
            )
            .await?
        }
        Command::TestCachePolicySet {
            project,
            enabled,
            max_entries,
            retention_days,
            max_output_bytes,
            reuse_lint,
            include_integration,
            failed_retention_secs,
            extra_env,
            extra_path,
            replace_extra_scopes,
        } => {
            let current = rpc(
                &discovery,
                "test_cache.policy.get",
                json!({"project_id": project}),
            )
            .await?;
            let mut policy = current.get("policy").cloned().unwrap_or_default();
            if !policy.is_object() {
                policy = json!({});
            }
            if let Some(object) = policy.as_object_mut() {
                if let Some(value) = enabled {
                    object.insert("enabled".into(), json!(value));
                }
                if let Some(value) = max_entries {
                    object.insert("max_entries".into(), json!(value));
                }
                if let Some(value) = retention_days {
                    object.insert("retention_days".into(), json!(value));
                }
                if let Some(value) = max_output_bytes {
                    object.insert("max_output_bytes".into(), json!(value));
                }
                if let Some(value) = reuse_lint {
                    object.insert("reuse_lint".into(), json!(value));
                }
                if let Some(value) = include_integration {
                    object.insert("include_integration".into(), json!(value));
                }
                if let Some(value) = failed_retention_secs {
                    object.insert("failed_retention_secs".into(), json!(value));
                }
                if replace_extra_scopes {
                    object.insert("extra_env".into(), json!([]));
                    object.insert("extra_paths".into(), json!([]));
                }
                if !extra_env.is_empty() {
                    let mut values = object
                        .get("extra_env")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    values.extend(extra_env.iter().cloned().map(Value::String));
                    object.insert("extra_env".into(), json!(values));
                }
                if !extra_path.is_empty() {
                    let mut values = object
                        .get("extra_paths")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    values.extend(extra_path.iter().cloned().map(Value::String));
                    object.insert("extra_paths".into(), json!(values));
                }
            }
            rpc(
                &discovery,
                "test_cache.policy.set",
                json!({"project_id": project, "policy": policy}),
            )
            .await?
        }
        Command::TestCacheScopes { project } => {
            rpc(
                &discovery,
                "test_cache.scope.preview",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::TestCachePrune {
            project,
            older_than_secs,
            expired_only,
            kind,
            dry_run,
        } => {
            rpc(
                &discovery,
                "test_cache.prune",
                json!({
                    "project_id": project,
                    "older_than_secs": older_than_secs,
                    "expired_only": expired_only,
                    "kind": kind,
                    "dry_run": dry_run,
                }),
            )
            .await?
        }
        Command::TestCacheClear {
            project,
            session,
            kind,
            yes,
        } => {
            if !yes {
                bail!("refusing to clear test-cache metadata without --yes");
            }
            rpc(
                &discovery,
                "test_cache.clear",
                json!({
                    "project_id": project,
                    "session_id": session,
                    "kind": kind,
                    "confirm": yes,
                }),
            )
            .await?
        }
        Command::TestImpact {
            session,
            paths,
            limit,
            no_history,
        } => {
            rpc(
                &discovery,
                "test_cache.impact",
                json!({
                    "session_id": session,
                    "changed_paths": paths,
                    "limit": limit,
                    "include_history": !no_history,
                }),
            )
            .await?
        }
        _ => unreachable!("command routed to the wrong dispatch family"),
    };
    Ok(Some(value))
}
