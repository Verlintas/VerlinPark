//! Artifact, archive, media, and artifact-policy commands.

use super::*;

pub(super) async fn dispatch(
    command: Command,
    discovery: Discovery,
    _json: bool,
) -> Result<Option<Value>> {
    let value = match command {
        Command::Artifacts { session } => {
            rpc(&discovery, "artifact.list", json!({"session_id": session})).await?
        }
        Command::ArtifactVersions { artifact } => {
            rpc(
                &discovery,
                "artifact.versions",
                json!({"artifact_id": artifact}),
            )
            .await?
        }
        Command::ArtifactDiff {
            artifact,
            from_version,
            to_version,
            context,
        } => {
            rpc(
                &discovery,
                "artifact.diff",
                json!({
                    "artifact_id": artifact,
                    "from_version": from_version,
                    "to_version": to_version,
                    "context": context
                }),
            )
            .await?
        }
        Command::ArtifactRestore { artifact, version } => {
            rpc(
                &discovery,
                "artifact.restore",
                json!({"artifact_id": artifact, "version": version}),
            )
            .await?
        }
        Command::Artifact { artifact } => {
            let value = rpc(&discovery, "artifact.get", json!({"artifact_id": artifact})).await?;
            if value["artifact"]["storage"].as_str() == Some("file") {
                rpc(
                    &discovery,
                    "artifact.materialize",
                    json!({"artifact_id": artifact}),
                )
                .await?
            } else {
                value
            }
        }
        Command::ArtifactArchiveList {
            artifact,
            max_entries,
        } => {
            rpc(
                &discovery,
                "artifact.archive.list",
                json!({"artifact_id": artifact, "max_entries": max_entries}),
            )
            .await?
        }
        Command::ArtifactArchiveExtract {
            artifact,
            entries,
            all,
            max_entries,
            no_previews,
        } => {
            if !all && entries.is_empty() {
                bail!("artifact-archive-extract requires at least one --entry or --all");
            }
            rpc(
                &discovery,
                "artifact.archive.extract",
                json!({
                    "artifact_id": artifact,
                    "entries": entries,
                    "all": all,
                    "max_entries": max_entries,
                    "include_previews": !no_previews,
                }),
            )
            .await?
        }
        Command::MediaInfo { artifact } => {
            rpc(&discovery, "media.info", json!({"artifact_id": artifact})).await?
        }
        Command::MediaTranscode {
            artifact,
            format,
            max_duration_secs,
            max_bytes,
            yes,
        } => {
            if !yes {
                bail!(
                    "media-transcode is an explicit host operation; pass --yes after reviewing the source and bounds"
                );
            }
            rpc(
                &discovery,
                "media.transcode",
                json!({
                    "artifact_id": artifact,
                    "format": format,
                    "max_duration_secs": max_duration_secs,
                    "max_bytes": max_bytes,
                    "approved": true,
                }),
            )
            .await?
        }
        Command::BackfillArtifacts { session } => {
            rpc(
                &discovery,
                "artifact.backfill",
                json!({"session_id": session}),
            )
            .await?
        }
        Command::ArtifactPolicy { project } => {
            rpc(
                &discovery,
                "artifact.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::ArtifactPolicySet {
            project,
            auto_gc,
            max_bytes,
            max_age_secs,
            max_count,
        } => {
            rpc(
                &discovery,
                "artifact.policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "auto_gc": auto_gc,
                        "max_bytes": max_bytes,
                        "max_age_secs": max_age_secs,
                        "max_count": max_count,
                    }
                }),
            )
            .await?
        }
        Command::Gc { project } => {
            rpc(&discovery, "artifact.gc", json!({"project_id": project})).await?
        }
        _ => unreachable!("command routed to the wrong dispatch family"),
    };
    Ok(Some(value))
}
