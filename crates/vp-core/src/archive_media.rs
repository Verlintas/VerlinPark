//! Safe artifact archive and explicit media operations.
//!
//! This module is the daemon-facing bridge between the artifact store and the
//! pure archive/media helpers in `tooling::archive` and `tooling::media`.
//!
//! Invariants:
//! - archive bytes come from a content-addressed artifact, never a caller path;
//! - extraction writes only under a daemon-owned scratch directory and the
//!   directory is removed before the RPC returns;
//! - derived artifacts carry `ArtifactProvenance` and are content-addressed;
//! - media metadata is either probed by ffprobe, read from native image
//!   metadata, or reported as a typed unavailable/failure result;
//! - transcoding is explicit (`approved=true`), approval-classified when
//!   invoked through the agent tool surface, and bounded in duration, bytes,
//!   format, and wall-clock time.
//!
//! No function here writes into a workspace or executes an archive member.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use vp_protocol::types::{
    ArchiveLimits, Artifact, ArtifactArchiveExtractParams, ArtifactArchiveExtractResult,
    ArtifactArchiveListParams, ArtifactArchiveListResult, ArtifactProvenance,
    ExtractedArchiveEntry, MediaInfoParams, MediaInfoResult, MediaTranscodeParams,
    MediaTranscodeResult,
};
use vp_store::Store;

use crate::Engine;
use crate::tooling::archive::{
    ArchiveError, ArchiveSelection, extract_archive_to_scratch, list_archive,
};
use crate::tooling::errors::{ToolErrorCode, ToolFailure};
use crate::tooling::media::{
    DEFAULT_FFPROBE_TIMEOUT_SECS, MediaError, MediaTools, bounded_preview, plan_transcode,
    probe_media, transcode_media,
};

/// Maximum archive bytes read from one artifact. It matches the default
/// `ArchiveLimits::max_archive_bytes`; the protocol value is repeated in the
/// response so clients can explain a rejected archive.
const MAX_ARCHIVE_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
/// The artifact store caps a single binary artifact at 5 MiB. Extracted and
/// transcoded outputs above this are bounded previews, not silently truncated
/// artifacts.
const ARTIFACT_BYTES_CAP: u64 = 5 * 1024 * 1024;
/// Max bytes read from an artifact blob before decompression. zstd can expand
/// a small blob, so the raw artifact size is not trusted as the only bound.
const MAX_COMPRESSED_ARTIFACT_BYTES: u64 = MAX_ARCHIVE_ARTIFACT_BYTES + 4 * 1024 * 1024;

/// A typed failure that crosses the engine boundary. The daemon maps it to a
/// JSON-RPC error with the same Q3 structured contract used by tools.
#[derive(Debug)]
pub struct ArchiveMediaError {
    pub failure: ToolFailure,
}

impl ArchiveMediaError {
    pub fn new(code: ToolErrorCode, message: impl Into<String>) -> Self {
        Self {
            failure: ToolFailure::new(code, message),
        }
    }

    pub fn with_evidence(mut self, evidence: impl Into<String>) -> Self {
        self.failure = self.failure.with_evidence(evidence);
        self
    }

    pub fn with_tool(mut self, tool: &str) -> Self {
        self.failure = self.failure.with_tool(tool);
        self
    }

    pub fn code(&self) -> ToolErrorCode {
        self.failure.code
    }
}

impl std::fmt::Display for ArchiveMediaError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.failure.message)
    }
}

impl std::error::Error for ArchiveMediaError {}

impl From<ArchiveError> for ArchiveMediaError {
    fn from(error: ArchiveError) -> Self {
        let mut failure = ToolFailure::new(error.code(), error.message()).with_tool("archive");
        if let Some(evidence) = error.evidence() {
            failure = failure.with_evidence(evidence);
        }
        Self { failure }
    }
}

impl From<MediaError> for ArchiveMediaError {
    fn from(error: MediaError) -> Self {
        let mut failure = ToolFailure::new(error.code(), error.message()).with_tool("media");
        if let Some(evidence) = error.evidence() {
            failure = failure.with_evidence(evidence);
        }
        Self { failure }
    }
}

/// Extract the structured failure for the daemon's RPC error mapping.
pub fn failure_data(error: &anyhow::Error) -> Option<Value> {
    error
        .downcast_ref::<ArchiveMediaError>()
        .map(|error| error.failure.to_structured())
}

fn artifact_not_found(artifact_id: &str) -> anyhow::Error {
    ArchiveMediaError::new(
        ToolErrorCode::NOT_FOUND,
        format!("artifact not found: {artifact_id}"),
    )
    .with_evidence(format!("artifact_id={artifact_id}"))
    .into()
}

fn artifact_size_exceeded(size: u64, limit: u64) -> ArchiveMediaError {
    ArchiveMediaError::new(
        ToolErrorCode::ARCHIVE_LIMIT_EXCEEDED,
        format!("artifact is {size} bytes, above the {limit} byte archive bound"),
    )
    .with_evidence(format!("size_bytes={size},limit_bytes={limit}"))
}

/// Read a bounded artifact body. File artifacts are content-addressed zstd
/// blobs; the stored path is validated under the daemon artifact directory and
/// the SHA-256 is verified after decompression. Inline artifacts are treated as
/// UTF-8 bytes, which is sufficient for small test archives and never exposes a
/// caller-controlled path.
#[allow(clippy::result_large_err)] // carries the bounded Q3 Failure object
fn read_artifact_bytes(
    store: &Store,
    artifact: &Artifact,
    max_bytes: u64,
) -> Result<Vec<u8>, ArchiveMediaError> {
    if artifact.storage == "inline" {
        if artifact.content.len() as u64 > max_bytes {
            return Err(artifact_size_exceeded(
                artifact.content.len() as u64,
                max_bytes,
            ));
        }
        return Ok(artifact.content.as_bytes().to_vec());
    }
    if artifact.storage != "file" {
        return Err(ArchiveMediaError::new(
            ToolErrorCode::UNSUPPORTED_CONTENT,
            format!(
                "artifact storage '{}' is not readable as bytes",
                artifact.storage
            ),
        ));
    }
    let source = artifact.path.as_deref().ok_or_else(|| {
        ArchiveMediaError::new(
            ToolErrorCode::ARCHIVE_CORRUPT,
            "file artifact has no stored blob path",
        )
    })?;
    let root = store
        .data_dir()
        .join("artifacts")
        .canonicalize()
        .map_err(|error| {
            ArchiveMediaError::new(
                ToolErrorCode::INTERNAL_ERROR,
                format!("artifact root is unavailable: {error}"),
            )
        })?;
    let source_path = PathBuf::from(source).canonicalize().map_err(|error| {
        ArchiveMediaError::new(
            ToolErrorCode::ARCHIVE_CORRUPT,
            format!("artifact blob is unavailable: {error}"),
        )
    })?;
    if !source_path.starts_with(&root) {
        return Err(ArchiveMediaError::new(
            ToolErrorCode::PATH_ESCAPE,
            "artifact blob path is outside the daemon artifact directory",
        ));
    }
    let metadata = std::fs::metadata(&source_path).map_err(|error| {
        ArchiveMediaError::new(
            ToolErrorCode::ARCHIVE_CORRUPT,
            format!("artifact blob metadata is unavailable: {error}"),
        )
    })?;
    if !metadata.is_file() {
        return Err(ArchiveMediaError::new(
            ToolErrorCode::ARCHIVE_CORRUPT,
            "artifact blob is not a regular file",
        ));
    }
    if metadata.len() > MAX_COMPRESSED_ARTIFACT_BYTES {
        return Err(artifact_size_exceeded(
            metadata.len(),
            MAX_COMPRESSED_ARTIFACT_BYTES,
        ));
    }
    let file = std::fs::File::open(&source_path).map_err(|error| {
        ArchiveMediaError::new(
            ToolErrorCode::READ_FAILED,
            format!("artifact blob cannot be opened: {error}"),
        )
    })?;
    let mut reader = std::io::BufReader::new(file);
    let data = if artifact.compression == "zstd" {
        let decoder = zstd::stream::read::Decoder::new(reader).map_err(|error| {
            ArchiveMediaError::new(
                ToolErrorCode::ARCHIVE_CORRUPT,
                format!("artifact blob is not valid zstd: {error}"),
            )
        })?;
        read_bounded_reader(decoder, max_bytes).map_err(|error| {
            if error.code() == ToolErrorCode::READ_FAILED {
                ArchiveMediaError::new(
                    ToolErrorCode::ARCHIVE_CORRUPT,
                    "artifact blob is not valid zstd",
                )
            } else {
                error
            }
        })?
    } else {
        read_bounded_reader(&mut reader, max_bytes)?
    };
    if data.is_empty() {
        return Err(ArchiveMediaError::new(
            ToolErrorCode::ARCHIVE_CORRUPT,
            "artifact blob is empty",
        ));
    }
    if !artifact.sha256.is_empty() {
        let digest = format!("{:x}", Sha256::digest(&data));
        if digest != artifact.sha256 {
            return Err(ArchiveMediaError::new(
                ToolErrorCode::HASH_MISMATCH,
                "artifact blob does not match its recorded SHA-256",
            )
            .with_evidence(format!("expected_sha256={}", artifact.sha256)));
        }
    }
    Ok(data)
}

#[allow(clippy::result_large_err)] // carries the bounded Q3 Failure object
fn read_bounded_reader(
    mut reader: impl std::io::Read,
    max_bytes: u64,
) -> Result<Vec<u8>, ArchiveMediaError> {
    let mut data = Vec::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer).map_err(|error| {
            ArchiveMediaError::new(
                ToolErrorCode::READ_FAILED,
                format!("reading artifact bytes: {error}"),
            )
        })?;
        if read == 0 {
            break;
        }
        data.extend_from_slice(&buffer[..read]);
        if data.len() as u64 > max_bytes {
            return Err(artifact_size_exceeded(data.len() as u64, max_bytes));
        }
    }
    Ok(data)
}

fn effective_limits(max_entries: Option<usize>) -> ArchiveLimits {
    let defaults = ArchiveLimits::default();
    ArchiveLimits {
        max_entries: max_entries
            .unwrap_or(defaults.max_entries)
            .clamp(1, defaults.max_entries),
        ..defaults
    }
}

/// A scratch directory that is always removed, on success or failure.
struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    #[allow(clippy::result_large_err)] // carries the bounded Q3 Failure object
    fn create(base: &Path, label: &str) -> Result<Self, ArchiveMediaError> {
        let path = base.join(format!("{label}-{}", Uuid::now_v7().simple()));
        vp_store::paths::ensure_private_dir(&path).map_err(|error| {
            ArchiveMediaError::new(
                ToolErrorCode::INTERNAL_ERROR,
                format!("creating daemon scratch directory: {error}"),
            )
        })?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, path = %self.path.display(), "failed to remove archive/media scratch directory");
        }
    }
}

fn artifact_provenance_base(artifact: &Artifact) -> ArtifactProvenance {
    ArtifactProvenance {
        source_artifact_id: Some(artifact.id.clone()),
        source_artifact_name: Some(artifact.name.clone()),
        source_sha256: (!artifact.sha256.is_empty()).then(|| artifact.sha256.clone()),
        source_mime_type: (!artifact.mime_type.is_empty()).then(|| artifact.mime_type.clone()),
        ..ArtifactProvenance::default()
    }
}

fn artifact_name_for_entry(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path).trim();
    let name = if name.is_empty() { "entry" } else { name };
    name.chars().take(200).collect()
}

fn mime_for_entry(path: &str) -> &'static str {
    crate::tooling::files::mime_for_path(Path::new(path))
}

async fn read_prefix(path: &Path, max_bytes: usize) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(path).await?;
    let mut buffer = vec![0u8; max_bytes];
    let read = file.read(&mut buffer).await?;
    buffer.truncate(read);
    Ok(buffer)
}

impl Engine {
    /// List a zip/tar/tar.gz artifact with hard caps. The response is metadata
    /// only; no entry bytes are written or published.
    pub async fn artifact_archive_list(
        &self,
        params: ArtifactArchiveListParams,
    ) -> Result<ArtifactArchiveListResult> {
        let artifact = self
            .get_artifact(&params.artifact_id)
            .await?
            .ok_or_else(|| artifact_not_found(&params.artifact_id))?;
        let bytes = read_artifact_bytes(&self.store, &artifact, MAX_ARCHIVE_ARTIFACT_BYTES)?;
        let limits = effective_limits(params.max_entries);
        let task_limits = limits.clone();
        let listing = tokio::task::spawn_blocking(move || list_archive(&bytes, &task_limits))
            .await
            .map_err(|error| {
                ArchiveMediaError::new(
                    ToolErrorCode::INTERNAL_ERROR,
                    format!("archive listing task failed: {error}"),
                )
            })?
            .map_err(ArchiveMediaError::from)?;
        Ok(ArtifactArchiveListResult {
            artifact_id: artifact.id.clone(),
            name: artifact.name.clone(),
            mime_type: artifact.mime_type.clone(),
            size_bytes: artifact.size_bytes,
            sha256: artifact.sha256.clone(),
            format: listing.format.as_str().to_string(),
            entries: listing.entries,
            entry_count: listing.entry_count,
            truncated: listing.truncated,
            total_uncompressed_bytes: listing.total_uncompressed_bytes,
            total_compressed_bytes: listing.total_compressed_bytes,
            limits: limits.clone(),
            warnings: listing.warnings,
        })
    }

    /// Extract named entries (or all safe files) into daemon scratch, publish
    /// each bounded entry as a content-addressed artifact, then remove scratch.
    pub async fn artifact_archive_extract(
        &self,
        params: ArtifactArchiveExtractParams,
    ) -> Result<ArtifactArchiveExtractResult> {
        let artifact = self
            .get_artifact(&params.artifact_id)
            .await?
            .ok_or_else(|| artifact_not_found(&params.artifact_id))?;
        let bytes = read_artifact_bytes(&self.store, &artifact, MAX_ARCHIVE_ARTIFACT_BYTES)?;
        let selection = ArchiveSelection::from_params(&params).map_err(ArchiveMediaError::from)?;
        let limits = effective_limits(params.max_entries);
        let scratch =
            ScratchDir::create(&self.store.data_dir().join("archive-extracts"), "extract")?;
        let scratch_path = scratch.path().to_path_buf();
        let extraction = {
            let limits = limits.clone();
            tokio::task::spawn_blocking(move || {
                extract_archive_to_scratch(&bytes, &selection, &limits, &scratch_path)
            })
            .await
            .map_err(|error| {
                ArchiveMediaError::new(
                    ToolErrorCode::INTERNAL_ERROR,
                    format!("archive extraction task failed: {error}"),
                )
            })?
            .map_err(ArchiveMediaError::from)?
        };
        let extraction_id = extraction
            .entries
            .first()
            .map(|_| Uuid::now_v7().to_string())
            .unwrap_or_else(|| Uuid::now_v7().to_string());
        let include_previews = params.include_previews.unwrap_or(true);
        let mut entries = Vec::with_capacity(extraction.entries.len());
        let mut published = Vec::new();
        let mut warnings = extraction.warnings.clone();
        for entry in &extraction.entries {
            let Some(path) = entry.scratch_path.as_deref() else {
                entries.push(ExtractedArchiveEntry {
                    path: entry.path.clone(),
                    kind: entry.kind.clone(),
                    size_bytes: entry.size_bytes,
                    sha256: entry.sha256.clone(),
                    published_artifact_id: None,
                    preview: None,
                    preview_encoding: "none".to_string(),
                    truncated: false,
                    skipped_reason: Some("no_scratch_path".to_string()),
                });
                continue;
            };
            let mut preview = None;
            let mut preview_encoding = "none".to_string();
            let mut preview_truncated = false;
            let mut read_bytes: Option<Vec<u8>> = None;
            if entry.size_bytes <= ARTIFACT_BYTES_CAP {
                match tokio::fs::read(path).await {
                    Ok(data) => read_bytes = Some(data),
                    Err(error) => {
                        warnings.push(format!("scratch read failed for {}: {error}", entry.path));
                    }
                }
            } else if include_previews {
                match read_prefix(path, crate::tooling::media::MEDIA_PREVIEW_BYTES).await {
                    Ok(data) => {
                        let (text, encoding, _truncated) = bounded_preview(&data);
                        preview = Some(text);
                        preview_encoding = encoding.to_string();
                        preview_truncated = true;
                    }
                    Err(error) => warnings.push(format!(
                        "scratch preview failed for {}: {error}",
                        entry.path
                    )),
                }
            }
            let mut published_artifact_id = None;
            let mut skipped_reason = None;
            if entry.size_bytes == 0 {
                skipped_reason = Some("empty_file".to_string());
            } else if entry.size_bytes > ARTIFACT_BYTES_CAP {
                skipped_reason = Some("exceeds_artifact_cap".to_string());
            } else if let Some(data) = read_bytes.as_ref() {
                if include_previews {
                    let (text, encoding, truncated) = bounded_preview(data);
                    preview = Some(text);
                    preview_encoding = encoding.to_string();
                    preview_truncated = truncated;
                }
                let provenance = ArtifactProvenance {
                    operation: "archive_extract".to_string(),
                    entry_path: Some(entry.path.clone()),
                    archive_format: Some(extraction.format.as_str().to_string()),
                    extraction_id: Some(extraction_id.clone()),
                    entry_sha256: entry.sha256.clone(),
                    ..artifact_provenance_base(&artifact)
                };
                match self
                    .publish_artifact_binary_with_provenance(
                        &artifact.session_id,
                        "daemon",
                        "Archive",
                        None,
                        &artifact_name_for_entry(&entry.path),
                        "archive-entry",
                        mime_for_entry(&entry.path),
                        data,
                        Some(provenance),
                    )
                    .await
                {
                    Ok(derived) => {
                        published_artifact_id = Some(derived.id.clone());
                        published.push(derived);
                    }
                    Err(error) => {
                        skipped_reason = Some(format!("publish_failed: {error}"));
                        warnings.push(format!(
                            "artifact publish failed for {}: {error}",
                            entry.path
                        ));
                    }
                }
            } else if skipped_reason.is_none() {
                skipped_reason = Some("unreadable_entry".to_string());
            }
            entries.push(ExtractedArchiveEntry {
                path: entry.path.clone(),
                kind: entry.kind.clone(),
                size_bytes: entry.size_bytes,
                sha256: entry.sha256.clone(),
                published_artifact_id,
                preview,
                preview_encoding,
                truncated: preview_truncated,
                skipped_reason,
            });
        }
        drop(scratch);
        Ok(ArtifactArchiveExtractResult {
            artifact_id: artifact.id,
            extraction_id,
            format: extraction.format.as_str().to_string(),
            entries,
            published_artifacts: published,
            total_bytes: extraction.total_bytes,
            scratch_retained: false,
            warnings,
        })
    }

    /// Probe a file-backed media artifact. Missing tools produce
    /// `MEDIA_TOOL_UNAVAILABLE`; a still image can fall back to native image
    /// metadata, explicitly labeled.
    pub async fn media_info(&self, params: MediaInfoParams) -> Result<MediaInfoResult> {
        let tools = MediaTools::discover();
        self.media_info_with_tools(&params.artifact_id, &tools)
            .await
    }

    pub(crate) async fn media_info_with_tools(
        &self,
        artifact_id: &str,
        tools: &MediaTools,
    ) -> Result<MediaInfoResult> {
        let artifact = self
            .get_artifact(artifact_id)
            .await?
            .ok_or_else(|| artifact_not_found(artifact_id))?;
        let path = self.materialize_for_media(artifact_id).await?;
        let tools = tools.clone();
        let info = tokio::task::spawn_blocking(move || {
            probe_media(
                &path,
                &tools,
                Duration::from_secs(DEFAULT_FFPROBE_TIMEOUT_SECS),
            )
        })
        .await
        .map_err(|error| {
            ArchiveMediaError::new(
                ToolErrorCode::INTERNAL_ERROR,
                format!("media probe task failed: {error}"),
            )
        })?
        .map_err(ArchiveMediaError::from)?;
        Ok(MediaInfoResult {
            artifact_id: artifact.id,
            name: artifact.name,
            mime_type: artifact.mime_type,
            size_bytes: artifact.size_bytes,
            sha256: artifact.sha256,
            info,
        })
    }

    /// Explicitly transcode a media artifact into a new content-addressed
    /// artifact. The output is bounded by duration, bytes, format, and the
    /// 5 MiB artifact cap; the source and output live under daemon-owned
    /// scratch or the artifact blob store.
    pub async fn media_transcode(
        &self,
        params: MediaTranscodeParams,
    ) -> Result<MediaTranscodeResult> {
        let tools = MediaTools::discover();
        self.media_transcode_with_tools(params, &tools).await
    }

    pub(crate) async fn media_transcode_with_tools(
        &self,
        params: MediaTranscodeParams,
        tools: &MediaTools,
    ) -> Result<MediaTranscodeResult> {
        let mut plan = plan_transcode(
            &params.format,
            params.approved,
            params.max_duration_secs,
            params.max_bytes,
        )
        .map_err(ArchiveMediaError::from)?;
        let requested_max_bytes = plan.max_bytes;
        plan.max_bytes = plan.max_bytes.min(ARTIFACT_BYTES_CAP);
        let artifact = self
            .get_artifact(&params.artifact_id)
            .await?
            .ok_or_else(|| artifact_not_found(&params.artifact_id))?;
        if artifact.storage != "file" {
            return Err(ArchiveMediaError::new(
                ToolErrorCode::UNSUPPORTED_CONTENT,
                "media transcoding requires a file-backed artifact",
            )
            .into());
        }
        let input = self.materialize_for_media(&params.artifact_id).await?;
        let scratch =
            ScratchDir::create(&self.store.data_dir().join("media-transcode"), "transcode")?;
        let output = scratch
            .path()
            .join(format!("output.{}", plan.format.extension()));
        let tools = tools.clone();
        let plan_for_task = plan.clone();
        let input_for_task = input.clone();
        let output_for_task = output.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            transcode_media(&input_for_task, &output_for_task, &plan_for_task, &tools)
        })
        .await
        .map_err(|error| {
            ArchiveMediaError::new(
                ToolErrorCode::INTERNAL_ERROR,
                format!("media transcode task failed: {error}"),
            )
        })?
        .map_err(ArchiveMediaError::from)?;
        let data = tokio::fs::read(&output).await.map_err(|error| {
            ArchiveMediaError::new(
                ToolErrorCode::MEDIA_TRANSCODE_FAILED,
                format!("transcoded output cannot be read: {error}"),
            )
        })?;
        if data.len() as u64 > ARTIFACT_BYTES_CAP {
            return Err(ArchiveMediaError::new(
                ToolErrorCode::MEDIA_LIMIT_EXCEEDED,
                format!(
                    "transcoded output is {} bytes, above the {ARTIFACT_BYTES_CAP} byte artifact cap",
                    data.len()
                ),
            )
            .with_evidence(format!(
                "size_bytes={},limit_bytes={ARTIFACT_BYTES_CAP}",
                data.len()
            ))
            .into());
        }
        let source_extension = Path::new(&artifact.name)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase());
        let provenance = ArtifactProvenance {
            operation: "media_transcode".to_string(),
            source_format: source_extension,
            output_format: Some(plan.format.as_str().to_string()),
            tool: Some("ffmpeg".to_string()),
            max_duration_secs: Some(plan.max_duration_secs),
            ..artifact_provenance_base(&artifact)
        };
        let stem = Path::new(&artifact.name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("media");
        let name = format!("{stem}-transcoded.{}", plan.format.extension());
        let name = name.chars().take(200).collect::<String>();
        let derived = self
            .publish_artifact_binary_with_provenance(
                &artifact.session_id,
                "daemon",
                "Media",
                None,
                &name,
                "media-transcode",
                plan.format.mime_type(),
                &data,
                Some(provenance),
            )
            .await?;
        let mut warnings = outcome.warnings;
        if requested_max_bytes > plan.max_bytes {
            warnings.push(format!(
                "output is capped at {ARTIFACT_BYTES_CAP} bytes by the content-addressed artifact store"
            ));
        }
        drop(scratch);
        Ok(MediaTranscodeResult {
            source_artifact_id: artifact.id,
            artifact: derived,
            format: plan.format.as_str().to_string(),
            duration_secs: outcome.duration_secs,
            size_bytes: outcome.size_bytes,
            elapsed_ms: outcome.elapsed.as_millis().min(u128::from(u64::MAX)) as u64,
            command_summary: outcome.command_summary,
            max_duration_secs: plan.max_duration_secs,
            max_bytes: plan.max_bytes,
            warnings,
        })
    }

    async fn materialize_for_media(&self, artifact_id: &str) -> Result<PathBuf> {
        let store = self.store.clone();
        let id = artifact_id.to_string();
        let path = tokio::task::spawn_blocking(move || store.materialize_artifact(&id))
            .await
            .map_err(|error| {
                ArchiveMediaError::new(
                    ToolErrorCode::INTERNAL_ERROR,
                    format!("artifact materialize task failed: {error}"),
                )
            })??;
        path.ok_or_else(|| {
            ArchiveMediaError::new(
                ToolErrorCode::MEDIA_PROBE_FAILED,
                "media inspection requires a file-backed artifact with a materialized blob",
            )
            .into()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tooling::media::{MediaOutputFormat, TRANSCODE_MAX_BYTES};
    use base64::Engine as _;
    use serde_json::json;
    use std::io::Write as _;
    use tempfile::TempDir;
    use vp_protocol::types::{
        ArtifactArchiveExtractParams, ArtifactArchiveListParams, MediaInfoParams,
        MediaTranscodeParams,
    };
    use vp_store::Store;

    async fn engine_fixture() -> (TempDir, Engine, Artifact) {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("archive-media"))
            .unwrap();
        let session = store
            .create_session(&project.id, "archive media test")
            .unwrap();
        let engine = Engine::new(store);
        let artifact = engine
            .publish_artifact_binary(
                &session.id,
                "tester",
                "Tester",
                None,
                "bundle.zip",
                "archive",
                "application/zip",
                &zip_bytes(&[("a.txt", b"alpha"), ("dir/b.txt", b"beta")]),
            )
            .await
            .unwrap();
        (data, engine, artifact)
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

    #[tokio::test]
    async fn lists_and_extracts_archive_entries_with_provenance() {
        let (_data, engine, artifact) = engine_fixture().await;
        let listing = engine
            .artifact_archive_list(ArtifactArchiveListParams {
                artifact_id: artifact.id.clone(),
                max_entries: None,
            })
            .await
            .unwrap();
        assert_eq!(listing.format, "zip");
        assert_eq!(listing.entries.len(), 2);
        assert_eq!(listing.entries[0].path, "a.txt");
        assert!(listing.entries[0].safe);
        assert_eq!(listing.total_uncompressed_bytes, 9);

        let result = engine
            .artifact_archive_extract(ArtifactArchiveExtractParams {
                artifact_id: artifact.id.clone(),
                entries: vec!["dir/b.txt".to_string()],
                all: false,
                max_entries: None,
                include_previews: Some(true),
            })
            .await
            .unwrap();
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.total_bytes, 4);
        assert!(!result.scratch_retained);
        assert_eq!(result.published_artifacts.len(), 1);
        let derived = &result.published_artifacts[0];
        assert_eq!(derived.kind, "archive-entry");
        let provenance = derived.provenance.as_ref().unwrap();
        assert_eq!(provenance.operation, "archive_extract");
        assert_eq!(
            provenance.source_artifact_id.as_deref(),
            Some(artifact.id.as_str())
        );
        assert_eq!(provenance.entry_path.as_deref(), Some("dir/b.txt"));
        assert_eq!(provenance.archive_format.as_deref(), Some("zip"));
        assert!(derived.sha256.len() == 64);
        assert_eq!(result.entries[0].preview.as_deref(), Some("beta"));
        assert_eq!(result.entries[0].preview_encoding, "utf8");
    }

    #[tokio::test]
    async fn extraction_rejects_unsafe_entries_before_publishing() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), None)
            .unwrap();
        let session = store.create_session(&project.id, "unsafe").unwrap();
        let engine = Engine::new(store);
        let artifact = engine
            .publish_artifact_binary(
                &session.id,
                "tester",
                "Tester",
                None,
                "slip.zip",
                "archive",
                "application/zip",
                &zip_bytes(&[("../escape.txt", b"bad")]),
            )
            .await
            .unwrap();
        let error = engine
            .artifact_archive_extract(ArtifactArchiveExtractParams {
                artifact_id: artifact.id,
                entries: vec!["../escape.txt".to_string()],
                all: false,
                max_entries: None,
                include_previews: None,
            })
            .await
            .unwrap_err();
        assert_eq!(
            failure_data(&error).unwrap()["code"],
            "ARCHIVE_UNSAFE_ENTRY"
        );
        let artifacts = engine.list_artifacts(&session.id).await.unwrap();
        assert_eq!(artifacts.len(), 1);
    }

    #[tokio::test]
    async fn archive_listing_rejects_truncated_corrupt_and_encrypted_buffers() {
        let (_data, engine, artifact) = engine_fixture().await;
        let listing = engine
            .artifact_archive_list(ArtifactArchiveListParams {
                artifact_id: artifact.id.clone(),
                max_entries: Some(1),
            })
            .await
            .unwrap();
        assert!(listing.truncated);
        assert_eq!(listing.entries.len(), 1);

        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), None)
            .unwrap();
        let session = store.create_session(&project.id, "corrupt").unwrap();
        let engine = Engine::new(store);
        let artifact = engine
            .publish_artifact_binary(
                &session.id,
                "tester",
                "Tester",
                None,
                "corrupt.zip",
                "archive",
                "application/zip",
                b"not an archive at all",
            )
            .await
            .unwrap();
        let error = engine
            .artifact_archive_list(ArtifactArchiveListParams {
                artifact_id: artifact.id,
                max_entries: None,
            })
            .await
            .unwrap_err();
        assert_eq!(
            failure_data(&error).unwrap()["code"],
            "ARCHIVE_FORMAT_UNSUPPORTED"
        );
    }

    #[tokio::test]
    async fn archive_hash_mismatch_fails_closed() {
        let (_data, engine, artifact) = engine_fixture().await;
        let blob = artifact.path.clone().unwrap();
        std::fs::write(&blob, b"corrupted blob").unwrap();
        let error = engine
            .artifact_archive_list(ArtifactArchiveListParams {
                artifact_id: artifact.id,
                max_entries: None,
            })
            .await
            .unwrap_err();
        let data = failure_data(&error).unwrap();
        assert!(
            matches!(
                data["code"].as_str(),
                Some("HASH_MISMATCH") | Some("ARCHIVE_CORRUPT")
            ),
            "{data}"
        );
    }

    fn fake_tools(dir: &Path) -> MediaTools {
        let ffprobe = dir.join("ffprobe");
        std::fs::write(
            &ffprobe,
            "#!/bin/sh\nprintf '{\"format\":{\"format_name\":\"mp4\",\"duration\":\"2.5\"},\"streams\":[{\"index\":0,\"codec_type\":\"video\",\"codec_name\":\"h264\",\"width\":640,\"height\":360}]}'\n",
        )
        .unwrap();
        std::fs::set_permissions(
            &ffprobe,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let ffmpeg = dir.join("ffmpeg");
        std::fs::write(
            &ffmpeg,
            "#!/bin/sh\nout=\"\"\nwhile [ $# -gt 0 ]; do out=\"$1\"; shift; done\nprintf 'media-bytes' > \"$out\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&ffmpeg, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        MediaTools::with_programs(Some(ffprobe), Some(ffmpeg), None)
    }

    #[tokio::test]
    async fn media_info_uses_injected_ffprobe_and_reports_missing_tools() {
        let (data, engine, artifact) = engine_fixture().await;
        let media = engine
            .publish_artifact_binary(
                &artifact.session_id,
                "tester",
                "Tester",
                None,
                "clip.mp4",
                "video",
                "video/mp4",
                b"fake media bytes",
            )
            .await
            .unwrap();
        let tools = fake_tools(data.path());
        let info = engine
            .media_info_with_tools(&media.id, &tools)
            .await
            .unwrap();
        assert_eq!(info.info.source, "ffprobe");
        assert_eq!(info.info.duration_secs, Some(2.5));
        assert_eq!(info.info.width, Some(640));
        assert_eq!(info.info.streams.len(), 1);

        let missing = MediaTools::with_programs(None, None, None);
        let error = engine
            .media_info_with_tools(&media.id, &missing)
            .await
            .unwrap_err();
        assert_eq!(
            failure_data(&error).unwrap()["code"],
            "MEDIA_TOOL_UNAVAILABLE"
        );
    }

    #[tokio::test]
    async fn media_transcode_requires_approval_and_publishes_provenance() {
        let (data, engine, artifact) = engine_fixture().await;
        let media = engine
            .publish_artifact_binary(
                &artifact.session_id,
                "tester",
                "Tester",
                None,
                "clip.mov",
                "video",
                "video/quicktime",
                b"fake media bytes",
            )
            .await
            .unwrap();
        let tools = fake_tools(data.path());
        let error = engine
            .media_transcode_with_tools(
                MediaTranscodeParams {
                    artifact_id: media.id.clone(),
                    format: "mp4".to_string(),
                    max_duration_secs: Some(5),
                    max_bytes: None,
                    approved: false,
                },
                &tools,
            )
            .await
            .unwrap_err();
        assert_eq!(failure_data(&error).unwrap()["code"], "APPROVAL_REQUIRED");

        let result = engine
            .media_transcode_with_tools(
                MediaTranscodeParams {
                    artifact_id: media.id.clone(),
                    format: "mp4".to_string(),
                    max_duration_secs: Some(5),
                    max_bytes: None,
                    approved: true,
                },
                &tools,
            )
            .await
            .unwrap();
        assert_eq!(result.format, "mp4");
        assert_eq!(result.max_duration_secs, 5);
        assert_eq!(result.max_bytes, ARTIFACT_BYTES_CAP);
        assert_eq!(result.size_bytes, 11);
        assert_eq!(result.artifact.kind, "media-transcode");
        let provenance = result.artifact.provenance.as_ref().unwrap();
        assert_eq!(provenance.operation, "media_transcode");
        assert_eq!(
            provenance.source_artifact_id.as_deref(),
            Some(media.id.as_str())
        );
        assert_eq!(provenance.output_format.as_deref(), Some("mp4"));
        assert_eq!(provenance.tool.as_deref(), Some("ffmpeg"));
    }

    #[tokio::test]
    async fn media_transcode_missing_ffmpeg_is_typed() {
        let (_data, engine, artifact) = engine_fixture().await;
        let tools = MediaTools::with_programs(None, None, None);
        let error = engine
            .media_transcode_with_tools(
                MediaTranscodeParams {
                    artifact_id: artifact.id,
                    format: "mp3".to_string(),
                    max_duration_secs: None,
                    max_bytes: None,
                    approved: true,
                },
                &tools,
            )
            .await
            .unwrap_err();
        assert_eq!(
            failure_data(&error).unwrap()["code"],
            "MEDIA_TOOL_UNAVAILABLE"
        );
    }

    #[tokio::test]
    async fn archive_and_media_operations_preserve_source_versioning() {
        let (_data, engine, artifact) = engine_fixture().await;
        let versions = engine.artifact_versions(&artifact.id).await.unwrap();
        assert_eq!(versions.versions.len(), 1);
        assert_eq!(versions.versions[0].version, 1);
        let result = engine
            .artifact_archive_extract(ArtifactArchiveExtractParams {
                artifact_id: artifact.id.clone(),
                entries: vec!["a.txt".to_string()],
                all: false,
                max_entries: None,
                include_previews: Some(false),
            })
            .await
            .unwrap();
        assert_eq!(result.published_artifacts.len(), 1);
        assert!(result.entries[0].preview.is_none());
        // The source artifact is unchanged and still at version 1.
        let current = engine.get_artifact(&artifact.id).await.unwrap().unwrap();
        assert_eq!(current.version, 1);
        assert_eq!(current.sha256, artifact.sha256);
    }

    #[tokio::test]
    async fn archive_listing_does_not_require_a_worktree() {
        let (data, engine, artifact) = engine_fixture().await;
        // The artifact fixture has no session worktree. The operation must use
        // only the content-addressed blob and daemon scratch.
        assert!(!data.path().join("archive-extracts").exists());
        let listing = engine
            .artifact_archive_list(ArtifactArchiveListParams {
                artifact_id: artifact.id,
                max_entries: None,
            })
            .await
            .unwrap();
        assert_eq!(listing.entry_count, 2);
    }

    #[test]
    fn protocol_json_shape_is_backward_compatible() {
        let value = json!({
            "artifact_id": "art",
            "entries": [],
            "all": false
        });
        let params: ArtifactArchiveExtractParams = serde_json::from_value(value).unwrap();
        assert!(params.include_previews.is_none());
        let value = json!({
            "artifact_id": "art",
            "format": "mp4",
            "approved": false
        });
        let params: MediaTranscodeParams = serde_json::from_value(value).unwrap();
        assert!(params.max_duration_secs.is_none());
        assert!(!params.approved);
        let _ = MediaInfoParams {
            artifact_id: "art".to_string(),
        };
    }

    #[test]
    fn media_output_mime_matches_format() {
        assert_eq!(MediaOutputFormat::Mp4.mime_type(), "video/mp4");
        assert_eq!(MediaOutputFormat::Mp3.mime_type(), "audio/mpeg");
        assert_eq!(MediaOutputFormat::Png.mime_type(), "image/png");
        assert_eq!(MediaOutputFormat::Jpeg.extension(), "jpg");
        const { assert!(TRANSCODE_MAX_BYTES >= ARTIFACT_BYTES_CAP) };
    }

    #[test]
    fn base64_fixture_is_valid_png_header() {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            .unwrap();
        assert!(bytes.starts_with(b"\x89PNG"));
    }
}
