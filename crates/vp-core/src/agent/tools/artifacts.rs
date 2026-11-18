use super::*;

/// Resolve an artifact only when it belongs to the calling session. A foreign
/// artifact id is indistinguishable from a missing one, so a session cannot
/// probe another session's artifact namespace or publish derived versions into
/// it.
pub(in crate::agent) async fn session_artifact(
    engine: &Engine,
    session: &Session,
    artifact_id: &str,
) -> Result<Option<vp_protocol::types::Artifact>> {
    let Some(artifact) = engine.get_artifact(artifact_id).await? else {
        return Ok(None);
    };
    if artifact.session_id != session.id {
        return Ok(None);
    }
    Ok(Some(artifact))
}

pub(in crate::agent) fn artifact_not_found_output(tool: &str, artifact_id: &str) -> ToolOutput {
    ToolOutput::error_coded(
        tool,
        crate::tooling::errors::ToolErrorCode::NOT_FOUND,
        format!("artifact not found: {artifact_id}"),
    )
}

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
    session: &Session,
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
    match session_artifact(engine, session, &artifact_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return artifact_not_found_output(name, &artifact_id),
        Err(error) => return ToolOutput::from_tool_error(name, &error),
    }
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
    session: &Session,
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
                match session_artifact(engine, session, artifact_id).await {
                    Ok(Some(_)) => {}
                    Ok(None) => return artifact_not_found_output(name, artifact_id),
                    Err(error) => return ToolOutput::from_tool_error(name, &error),
                }
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
            match session_artifact(engine, session, artifact_id).await {
                Ok(Some(_)) => {}
                Ok(None) => return artifact_not_found_output(name, artifact_id),
                Err(error) => return ToolOutput::from_tool_error(name, &error),
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use vp_store::Store;

    struct Fixture {
        _data: TempDir,
        _repo: TempDir,
        engine: Engine,
        session_a: Session,
        session_b: Session,
    }

    fn fixture() -> Fixture {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("scope"))
            .unwrap();
        let session_a = store.create_session(&project.id, "session a").unwrap();
        let session_b = store.create_session(&project.id, "session b").unwrap();
        Fixture {
            _data: data,
            _repo: repo,
            engine: Engine::new(store),
            session_a,
            session_b,
        }
    }

    fn spec(name: &str) -> AgentSpec {
        AgentSpec {
            id: format!("agent-{name}"),
            role: AgentRole::Worker,
            name: name.to_string(),
            depth: 1,
            parent_name: None,
            title: None,
            task_id: None,
            department: None,
            identity_id: None,
            identity_generation: None,
            profile: AgentProfile::Coding,
            allow_delegation: false,
            task_kind: None,
            auto_approve: false,
            worktree: PathBuf::from("/tmp/scope-worktree"),
            branch: "scope".to_string(),
            session_worktree: PathBuf::from("/tmp/scope-worktree"),
            session_branch: "scope".to_string(),
            write_scope: Vec::new(),
            base_commit: "0".repeat(40),
            workspace_id: None,
            session_workspace_id: None,
            parent_workspace_id: None,
        }
    }

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buffer = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buffer);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, data) in entries {
                writer.start_file(*name, options).unwrap();
                writer.write_all(data).unwrap();
            }
            writer.finish().unwrap();
        }
        buffer.into_inner()
    }

    fn failure_code(output: &ToolOutput) -> &'static str {
        output
            .failure
            .as_ref()
            .expect("typed tool failure")
            .code
            .as_str()
    }

    async fn event_count(engine: &Engine, session_id: &str) -> usize {
        engine
            .list_events(session_id.to_string(), 0, 1_000)
            .await
            .unwrap()
            .len()
    }

    #[tokio::test]
    async fn archive_and_media_tools_reject_foreign_artifacts_before_publish() {
        let fixture = fixture();
        let spec_a = spec("Worker A");
        let archive = fixture
            .engine
            .publish_artifact_binary(
                &fixture.session_b.id,
                "worker-b",
                "Worker B",
                None,
                "bundle.zip",
                "archive",
                "application/zip",
                &zip_bytes(&[("a.txt", b"alpha")]),
            )
            .await
            .unwrap();
        let media = fixture
            .engine
            .publish_artifact_binary(
                &fixture.session_b.id,
                "worker-b",
                "Worker B",
                None,
                "clip.mp4",
                "video",
                "video/mp4",
                b"fake media bytes",
            )
            .await
            .unwrap();
        let before_artifacts = fixture
            .engine
            .list_artifacts(&fixture.session_b.id)
            .await
            .unwrap()
            .len();
        let before_events = event_count(&fixture.engine, &fixture.session_b.id).await;

        for (name, input) in [
            (
                "archive_list",
                serde_json::json!({"artifact_id": archive.id}),
            ),
            (
                "archive_extract",
                serde_json::json!({"artifact_id": archive.id, "all": true}),
            ),
        ] {
            let output = run_archive_tool(&fixture.engine, &fixture.session_a, name, input).await;
            assert!(output.is_error, "{name} must fail: {}", output.content);
            assert_eq!(failure_code(&output), "NOT_FOUND", "{name}");
        }

        for (name, input) in [
            ("media_info", serde_json::json!({"artifact_id": media.id})),
            (
                "media_transcode",
                serde_json::json!({"artifact_id": media.id, "format": "mp4"}),
            ),
        ] {
            let output =
                run_media_tool(&fixture.engine, &fixture.session_a, &spec_a, name, input).await;
            assert!(output.is_error, "{name} must fail: {}", output.content);
            assert_eq!(failure_code(&output), "NOT_FOUND", "{name}");
        }

        assert_eq!(
            fixture
                .engine
                .list_artifacts(&fixture.session_b.id)
                .await
                .unwrap()
                .len(),
            before_artifacts,
            "foreign archive/media calls must not publish derived artifacts"
        );
        assert_eq!(
            event_count(&fixture.engine, &fixture.session_b.id).await,
            before_events,
            "foreign archive/media calls must not append victim-session events"
        );

        // Positive control: the owner can list its archive without a worktree.
        let owned = run_archive_tool(
            &fixture.engine,
            &fixture.session_b,
            "archive_list",
            serde_json::json!({"artifact_id": archive.id}),
        )
        .await;
        assert!(!owned.is_error, "{}", owned.content);
        assert!(owned.content.contains("a.txt"));
    }
}
