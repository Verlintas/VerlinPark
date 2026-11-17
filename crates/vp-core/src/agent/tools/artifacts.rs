use super::*;

pub(in crate::agent) async fn run_publish_artifact_file(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    input: Value,
) -> Result<ToolOutput> {
    let relative = input
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("path is required"))?;
    let absolute = crate::tools::safe_existing_path(&spec.worktree, relative)?;
    if !absolute.is_file() {
        return Ok(ToolOutput::error_structured(
            "artifact source must be a file",
            json!({"code": "INVALID_ARGUMENT", "path": relative}),
        ));
    }
    let name = input
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            absolute
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| "artifact".to_string());
    let kind = input.get("kind").and_then(Value::as_str).unwrap_or("file");
    let mime_type = input
        .get("mime_type")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| crate::tooling::files::mime_for_path(&absolute).to_string());
    let artifact = engine
        .publish_artifact_file(
            &session.id,
            &spec.id,
            &spec.name,
            spec.task_id.clone(),
            &name,
            kind,
            &mime_type,
            &absolute,
            &spec.worktree,
        )
        .await?;
    let structured = serde_json::to_value(&artifact).unwrap_or_else(|_| json!({}));
    Ok(ToolOutput::ok_structured(
        format!(
            "published {} ({}) as {} · {} bytes",
            name, mime_type, artifact.id, artifact.size_bytes
        ),
        structured,
    ))
}

/// Convert a typed archive/media engine failure into the shared tool failure
/// contract. Unknown errors go through the ordinary anyhow classifier.
pub(in crate::agent) fn archive_media_failure(tool: &str, error: &anyhow::Error) -> ToolOutput {
    if let Some(failure) = error.downcast_ref::<crate::archive_media::ArchiveMediaError>() {
        let mut failure = failure.failure.clone();
        if failure.tool.is_none() {
            failure = failure.with_tool(tool);
        }
        return ToolOutput::error_failure(failure);
    }
    ToolOutput::from_tool_error(tool, error)
}

pub(in crate::agent) async fn run_archive_tool(
    engine: &Engine,
    name: &str,
    input: Value,
) -> ToolOutput {
    let artifact_id = match input
        .get("artifact_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => value.to_string(),
        None => {
            return ToolOutput::error_coded(
                name,
                crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                "artifact_id is required",
            );
        }
    };
    match name {
        "archive_list" => {
            let max_entries = input
                .get("max_entries")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok());
            let params = vp_protocol::types::ArtifactArchiveListParams {
                artifact_id,
                max_entries,
            };
            match engine.artifact_archive_list(params).await {
                Ok(result) => {
                    let mut text = format!(
                        "{} · {} · {} entr{} · {} bytes uncompressed{}",
                        result.name,
                        result.format,
                        result.entry_count,
                        if result.entry_count == 1 { "y" } else { "ies" },
                        result.total_uncompressed_bytes,
                        if result.truncated {
                            " · listing truncated at the entry cap"
                        } else {
                            ""
                        }
                    );
                    for entry in result.entries.iter().take(60) {
                        text.push('\n');
                        text.push_str(&format!(
                            "{}  {}  {} bytes{}",
                            if entry.safe { "safe" } else { "UNSAFE" },
                            entry.path,
                            entry.size_bytes,
                            entry
                                .unsafe_reason
                                .as_ref()
                                .map(|reason| format!(" · {reason}"))
                                .unwrap_or_default()
                        ));
                    }
                    if result.entries.len() > 60 {
                        text.push_str(
                            "\n… more entries omitted; use the RPC for the full bounded list",
                        );
                    }
                    ToolOutput::ok_structured(
                        text,
                        serde_json::to_value(&result).unwrap_or_else(|_| json!({})),
                    )
                }
                Err(error) => archive_media_failure(name, &error),
            }
        }
        "archive_extract" => {
            let entries = input
                .get("entries")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let all = input.get("all").and_then(Value::as_bool).unwrap_or(false);
            let params = vp_protocol::types::ArtifactArchiveExtractParams {
                artifact_id,
                entries,
                all,
                max_entries: input
                    .get("max_entries")
                    .and_then(Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok()),
                include_previews: Some(
                    input
                        .get("include_previews")
                        .and_then(Value::as_bool)
                        .unwrap_or(true),
                ),
            };
            match engine.artifact_archive_extract(params).await {
                Ok(result) => {
                    let mut text = format!(
                        "extracted {} entr{} ({} bytes) from {} · {} published artifact(s); daemon scratch removed",
                        result.entries.len(),
                        if result.entries.len() == 1 {
                            "y"
                        } else {
                            "ies"
                        },
                        result.total_bytes,
                        result.format,
                        result.published_artifacts.len()
                    );
                    for entry in &result.entries {
                        text.push('\n');
                        text.push_str(&format!(
                            "{}  {} bytes{}",
                            entry.path,
                            entry.size_bytes,
                            entry
                                .published_artifact_id
                                .as_ref()
                                .map(|id| format!(" · artifact {id}"))
                                .or_else(|| {
                                    entry
                                        .skipped_reason
                                        .as_ref()
                                        .map(|reason| format!(" · skipped: {reason}"))
                                })
                                .unwrap_or_default()
                        ));
                    }
                    ToolOutput::ok_structured(
                        text,
                        serde_json::to_value(&result).unwrap_or_else(|_| json!({})),
                    )
                }
                Err(error) => archive_media_failure(name, &error),
            }
        }
        other => ToolOutput::error_coded(
            other,
            crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
            "unknown archive action",
        ),
    }
}

pub(in crate::agent) async fn run_media_tool(
    engine: &Engine,
    spec: &AgentSpec,
    name: &str,
    input: Value,
) -> ToolOutput {
    match name {
        "media_info" => {
            if let Some(artifact_id) = input
                .get("artifact_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                let params = vp_protocol::types::MediaInfoParams {
                    artifact_id: artifact_id.to_string(),
                };
                return match engine.media_info(params).await {
                    Ok(result) => {
                        let summary = media_info_summary(&result.name, &result.info, 0);
                        ToolOutput::ok_structured(
                            summary,
                            serde_json::to_value(&result).unwrap_or_else(|_| json!({})),
                        )
                    }
                    Err(error) => archive_media_failure(name, &error),
                };
            }
            let Some(relative) = input.get("path").and_then(Value::as_str) else {
                return ToolOutput::error_coded(
                    "media_info",
                    crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                    "media_info requires artifact_id or a worktree-relative path",
                );
            };
            let path = match crate::tools::safe_existing_path(&spec.worktree, relative) {
                Ok(path) => path,
                Err(error) => {
                    return ToolOutput::error_failure(error.with_tool("media_info"));
                }
            };
            let tools = crate::tooling::media::MediaTools::discover();
            let path_for_task = path.clone();
            let info = match tokio::task::spawn_blocking(move || {
                crate::tooling::media::probe_media(
                    &path_for_task,
                    &tools,
                    std::time::Duration::from_secs(
                        crate::tooling::media::DEFAULT_FFPROBE_TIMEOUT_SECS,
                    ),
                )
            })
            .await
            {
                Ok(Ok(info)) => info,
                Ok(Err(error)) => {
                    let typed =
                        anyhow::Error::new(crate::archive_media::ArchiveMediaError::from(error));
                    return archive_media_failure(name, &typed);
                }
                Err(error) => {
                    return ToolOutput::error_coded(
                        "media_info",
                        crate::tooling::errors::ToolErrorCode::INTERNAL_ERROR,
                        format!("media probe task failed: {error}"),
                    );
                }
            };
            let size_bytes = std::fs::metadata(&path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            let summary = media_info_summary(relative, &info, size_bytes);
            ToolOutput::ok_structured(
                summary,
                json!({
                    "path": relative,
                    "size_bytes": size_bytes,
                    "info": info,
                }),
            )
        }
        "media_transcode" => {
            let Some(artifact_id) = input
                .get("artifact_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                return ToolOutput::error_coded(
                    "media_transcode",
                    crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                    "artifact_id is required",
                );
            };
            let Some(format) = input.get("format").and_then(Value::as_str) else {
                return ToolOutput::error_coded(
                    "media_transcode",
                    crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
                    "format is required",
                );
            };
            let params = vp_protocol::types::MediaTranscodeParams {
                artifact_id: artifact_id.to_string(),
                format: format.to_string(),
                max_duration_secs: input.get("max_duration_secs").and_then(Value::as_u64),
                max_bytes: input.get("max_bytes").and_then(Value::as_u64),
                // The approval broker has already approved this specific call;
                // the RPC surface still requires an explicit operator marker.
                approved: true,
            };
            match engine.media_transcode(params).await {
                Ok(result) => ToolOutput::ok_structured(
                    format!(
                        "transcoded to {} · artifact {} · {} bytes · {}s cap{}",
                        result.format,
                        result.artifact.id,
                        result.size_bytes,
                        result.max_duration_secs,
                        if result.warnings.is_empty() {
                            String::new()
                        } else {
                            format!(" · {}", result.warnings.join("; "))
                        }
                    ),
                    serde_json::to_value(&result).unwrap_or_else(|_| json!({})),
                ),
                Err(error) => archive_media_failure(name, &error),
            }
        }
        other => ToolOutput::error_coded(
            other,
            crate::tooling::errors::ToolErrorCode::INVALID_ARGUMENT,
            "unknown media action",
        ),
    }
}

pub(in crate::agent) fn media_info_summary(
    label: &str,
    info: &vp_protocol::types::MediaInfo,
    size_bytes: u64,
) -> String {
    let mut text = format!(
        "{} · {} · source {}",
        label,
        info.container.as_deref().unwrap_or("unknown container"),
        info.source
    );
    if let Some(duration) = info.duration_secs {
        text.push_str(&format!(" · {duration:.2}s"));
    }
    if let (Some(width), Some(height)) = (info.width, info.height) {
        text.push_str(&format!(" · {width}x{height}"));
    }
    if size_bytes > 0 {
        text.push_str(&format!(" · {size_bytes} bytes"));
    }
    for stream in info.streams.iter().take(8) {
        text.push_str(&format!(
            "\nstream {}: {} {}{}{}",
            stream.index,
            stream.codec_type,
            stream.codec_name,
            stream
                .width
                .zip(stream.height)
                .map(|(width, height)| format!(" {width}x{height}"))
                .unwrap_or_default(),
            stream
                .duration_secs
                .map(|duration| format!(" {duration:.2}s"))
                .unwrap_or_default()
        ));
    }
    for warning in &info.warnings {
        text.push_str(&format!("\nwarning: {warning}"));
    }
    text
}
