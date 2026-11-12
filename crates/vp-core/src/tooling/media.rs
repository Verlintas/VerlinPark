//! Bounded document/media readers.
//!
//! PDF text extraction runs in a blocking task and is capped before it reaches
//! the model. Page selection uses form-feed boundaries emitted by the extractor
//! when available; it never claims page-accurate output when the PDF does not
//! expose boundaries.

use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use vp_protocol::types::{MediaInfo, MediaStreamInfo};

use crate::tooling::fs::{sha256_file, utf8_line_count};
use crate::tools::{AgentTool, ToolContext, ToolOutput, safe_existing_path, truncate};

const MAX_PDF_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PDF_CHARS: usize = 2_000_000;
pub const MAX_MEDIA_PROBE_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_FFPROBE_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_FFMPEG_OUTPUT_BYTES: usize = 64 * 1024;
pub const DEFAULT_FFPROBE_TIMEOUT_SECS: u64 = 20;
pub const DEFAULT_TRANSCODE_TIMEOUT_SECS: u64 = 600;
pub const TRANSCODE_MAX_DURATION_SECS: u64 = 600;
pub const TRANSCODE_DEFAULT_DURATION_SECS: u64 = 60;
pub const TRANSCODE_MAX_BYTES: u64 = 256 * 1024 * 1024;
pub const TRANSCODE_DEFAULT_BYTES: u64 = 64 * 1024 * 1024;
pub const TRANSCODE_MIN_BYTES: u64 = 1024 * 1024;
pub const MEDIA_PREVIEW_BYTES: usize = 4 * 1024;

#[derive(Debug, Default)]
struct PdfMetadata {
    page_count: Option<usize>,
    title: Option<String>,
    author: Option<String>,
    subject: Option<String>,
    creator: Option<String>,
    producer: Option<String>,
    created: Option<String>,
    modified: Option<String>,
}

fn pdf_metadata(path: &Path) -> Option<PdfMetadata> {
    let document = lopdf::Document::load(path).ok()?;
    let page_count = Some(document.get_pages().len());
    let info = document
        .trailer
        .get(b"Info")
        .ok()
        .and_then(|object| match object {
            lopdf::Object::Reference(id) => document.get_object(*id).ok(),
            other => Some(other),
        })
        .and_then(|object| object.as_dict().ok());
    let string =
        |key: &[u8]| info.and_then(|dictionary| pdf_info_string(&document, dictionary, key));
    Some(PdfMetadata {
        page_count,
        title: string(b"Title"),
        author: string(b"Author"),
        subject: string(b"Subject"),
        creator: string(b"Creator"),
        producer: string(b"Producer"),
        created: string(b"CreationDate"),
        modified: string(b"ModDate"),
    })
}

fn pdf_info_string(
    document: &lopdf::Document,
    dictionary: &lopdf::Dictionary,
    key: &[u8],
) -> Option<String> {
    let object = dictionary.get(key).ok()?;
    let object = match object {
        lopdf::Object::Reference(id) => document.get_object(*id).ok()?,
        other => other,
    };
    let bytes = object.as_str().ok()?;
    let decoded = if let Some(bytes) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_be_bytes(*pair))
            .collect::<Vec<_>>();
        String::from_utf16(&units).unwrap_or_default()
    } else if let Ok(text) = std::str::from_utf8(bytes) {
        text.to_string()
    } else {
        // PDFDocEncoding is close enough to Latin-1 for display metadata. It is
        // never used for security decisions or text extraction.
        bytes.iter().map(|byte| char::from(*byte)).collect()
    };
    let decoded = decoded.trim();
    (!decoded.is_empty()).then(|| decoded.chars().take(300).collect())
}

pub struct ReadPdf;

#[async_trait]
impl AgentTool for ReadPdf {
    fn name(&self) -> &'static str {
        "read_pdf"
    }

    fn description(&self) -> &'static str {
        "Extract bounded text from a PDF in the worktree. Supports page ranges when the document exposes page boundaries."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "PDF path relative to the worktree root"},
                "pages": {"type": "string", "description": "Optional 1-based page range such as 1-5,8; default all"},
                "max_chars": {"type": "integer", "minimum": 100, "maximum": 200000, "description": "Default 20000"},
                "include_page_markers": {"type": "boolean", "description": "Insert [page N] markers; default true"}
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let relative = input
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("path is required"))?;
        let path = safe_existing_path(&context.root, relative)?;
        if !path.is_file() {
            return Ok(ToolOutput::error_structured(
                "read_pdf path must be a file",
                json!({"code": "INVALID_ARGUMENT", "path": relative}),
            ));
        }
        let metadata = std::fs::metadata(&path)?;
        if metadata.len() > MAX_PDF_BYTES {
            return Ok(ToolOutput::error_structured(
                format!("PDF exceeds the {} MiB limit", MAX_PDF_BYTES / 1024 / 1024),
                json!({"code": "FILE_TOO_LARGE", "size_bytes": metadata.len()}),
            ));
        }
        let max_chars = input
            .get("max_chars")
            .and_then(Value::as_u64)
            .unwrap_or(20_000)
            .clamp(100, 200_000) as usize;
        let page_selection = match input.get("pages").and_then(Value::as_str) {
            Some(value) => match parse_page_selection(value) {
                Ok(pages) => Some(pages),
                Err(error) => {
                    return Ok(ToolOutput::error_structured(
                        error.to_string(),
                        json!({"code": "INVALID_ARGUMENT", "pages": value}),
                    ));
                }
            },
            None => None,
        };
        let include_markers = input
            .get("include_page_markers")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let extraction_path = path.clone();
        let extraction = tokio::task::spawn_blocking(move || {
            let text = pdf_extract::extract_text(&extraction_path);
            let metadata = pdf_metadata(&extraction_path);
            (text, metadata)
        })
        .await;
        let (text, pdf_metadata) = match extraction {
            Ok((Ok(text), metadata)) => (text, metadata),
            Ok((Err(error), _)) => {
                return Ok(ToolOutput::error_structured(
                    format!("PDF extraction failed: {error}"),
                    json!({"code": "PDF_EXTRACT_FAILED", "path": relative}),
                ));
            }
            Err(error) => {
                return Ok(ToolOutput::error_structured(
                    format!("PDF extraction task failed: {error}"),
                    json!({"code": "PDF_EXTRACT_FAILED", "path": relative}),
                ));
            }
        };
        if text.len() > MAX_PDF_CHARS {
            return Ok(ToolOutput::error_structured(
                format!("extracted text exceeds {MAX_PDF_CHARS} bytes; use a page range"),
                json!({"code": "FILE_TOO_LARGE", "extracted_bytes": text.len()}),
            ));
        }
        let pages = text.split('\u{0c}').map(str::to_string).collect::<Vec<_>>();
        let boundary_count = pages.len();
        let has_page_boundaries = boundary_count > 1;
        let metadata_page_count = pdf_metadata
            .as_ref()
            .and_then(|metadata| metadata.page_count);
        let page_count = metadata_page_count.unwrap_or(if has_page_boundaries {
            boundary_count
        } else {
            1
        });
        let page_count_source = if metadata_page_count.is_some() {
            "pdf-metadata"
        } else if has_page_boundaries {
            "text-boundaries"
        } else {
            "unknown"
        };
        let mut warnings = Vec::new();
        if pdf_metadata.is_none() {
            warnings.push("PDF metadata could not be read; page count is best effort".to_string());
        }
        if let Some(metadata_page_count) = metadata_page_count
            && has_page_boundaries
            && metadata_page_count != boundary_count
        {
            warnings.push(format!(
                "PDF metadata reports {metadata_page_count} page(s) but text extraction produced {boundary_count} boundary(ies)"
            ));
        }
        if page_selection.is_some() && !has_page_boundaries {
            warnings
                .push("page boundaries were not available; the full text was returned".to_string());
        }
        let selected = select_pages(&pages, page_selection.as_ref());
        let include_markers = include_markers && has_page_boundaries;
        let mut output = String::new();
        for (index, page) in selected.iter().enumerate() {
            if include_markers {
                let page_number = page_selection
                    .as_ref()
                    .and_then(|selection| selection.get(index))
                    .copied()
                    .unwrap_or(index + 1);
                output.push_str(&format!("\n[page {page_number}]\n"));
            }
            output.push_str(page.trim());
            output.push('\n');
            if output.chars().count() >= max_chars {
                break;
            }
        }
        let mut chars = output.chars();
        let mut visible = chars.by_ref().take(max_chars).collect::<String>();
        if chars.next().is_some() {
            visible.push_str("\n… content truncated");
        }
        let byte_hash = sha256_file(&path)?;
        let metadata_value = pdf_metadata
            .as_ref()
            .map(|metadata| {
                json!({
                    "title": metadata.title,
                    "author": metadata.author,
                    "subject": metadata.subject,
                    "creator": metadata.creator,
                    "producer": metadata.producer,
                    "created": metadata.created,
                    "modified": metadata.modified,
                })
            })
            .unwrap_or(Value::Null);
        let summary = format!(
            "{} · {} page(s) · {} bytes extracted · {} lines{}",
            relative,
            page_count,
            text.len(),
            utf8_line_count(&text),
            if has_page_boundaries {
                " · page boundaries available"
            } else {
                " · page boundaries unavailable"
            }
        );
        Ok(ToolOutput::ok_structured(
            truncate(format!("{summary}\n{visible}")),
            json!({
                "path": relative,
                "size_bytes": metadata.len(),
                "sha256": byte_hash,
                "page_count": page_count,
                "page_count_source": page_count_source,
                "metadata_page_count": metadata_page_count,
                "page_boundaries_available": has_page_boundaries,
                "boundary_count": boundary_count,
                "pages_requested": page_selection,
                "extracted_bytes": text.len(),
                "extracted_lines": utf8_line_count(&text),
                "text": visible,
                "truncated": visible.ends_with("… content truncated"),
                "warnings": warnings,
                "metadata": metadata_value,
                "confidence": "pdf-text-extractor",
            }),
        ))
    }
}

fn parse_page_selection(value: &str) -> Result<Vec<usize>> {
    let mut pages = Vec::new();
    for part in value.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((start, end)) = part.split_once('-') {
            let start = start.trim().parse::<usize>()?;
            let end = end.trim().parse::<usize>()?;
            if start == 0 || end < start {
                bail!("invalid page range: {part}");
            }
            if end - start > 500 {
                bail!("page range is limited to 500 pages: {part}");
            }
            for page in start..=end {
                pages.push(page);
                if pages.len() > 1_000 {
                    bail!("page selection is limited to 1000 pages");
                }
            }
        } else {
            let page = part.parse::<usize>()?;
            if page == 0 {
                bail!("pages are 1-based and must be positive");
            }
            pages.push(page);
        }
    }
    pages.sort_unstable();
    pages.dedup();
    if pages.is_empty() {
        bail!("pages must contain at least one page or range");
    }
    Ok(pages)
}

fn select_pages<'a>(pages: &'a [String], selection: Option<&Vec<usize>>) -> Vec<&'a str> {
    match selection {
        Some(selection) if pages.len() > 1 => selection
            .iter()
            .filter_map(|page| pages.get(page.saturating_sub(1)).map(String::as_str))
            .collect(),
        _ => pages.iter().map(String::as_str).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_page_selections() {
        assert_eq!(parse_page_selection("1-3,5").unwrap(), vec![1, 2, 3, 5]);
        assert!(parse_page_selection("0").is_err());
        assert!(parse_page_selection("5-3").is_err());
        assert!(parse_page_selection("").is_err());
    }

    #[test]
    fn selects_pages_with_form_feed_boundaries() {
        let pages = vec!["one".to_string(), "two".to_string(), "three".to_string()];
        let selected = select_pages(&pages, Some(&vec![2, 3]));
        assert_eq!(selected, vec!["two", "three"]);
        assert_eq!(select_pages(&pages, None).len(), 3);
    }
}

#[derive(Debug, Default)]
struct OcrResult {
    text: String,
    truncated: bool,
    words: usize,
    confidence: Option<f32>,
    confidence_min: Option<f32>,
    confidence_max: Option<f32>,
}

fn parse_tesseract_tsv(tsv: &str, max_chars: usize) -> OcrResult {
    let mut result = OcrResult::default();
    let mut last_line = None;
    let mut weighted_confidence = 0.0f32;
    let mut confidence_weight = 0usize;
    for line in tsv.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() < 12 {
            continue;
        }
        let confidence = match fields[10].trim().parse::<f32>() {
            Ok(confidence) if confidence >= 0.0 => confidence,
            _ => continue,
        };
        let word = fields[11..].join("\t").trim().to_string();
        if word.is_empty() {
            continue;
        }
        let line_key = (
            fields[2].parse::<u32>().unwrap_or(0),
            fields[3].parse::<u32>().unwrap_or(0),
            fields[4].parse::<u32>().unwrap_or(0),
        );
        if !result.text.is_empty() {
            result.text.push(if last_line == Some(line_key) {
                ' '
            } else {
                '\n'
            });
        }
        result.text.push_str(&word);
        last_line = Some(line_key);
        let weight = word.chars().count().max(1);
        weighted_confidence += confidence * weight as f32;
        confidence_weight += weight;
        result.words += 1;
        result.confidence_min = Some(
            result
                .confidence_min
                .map_or(confidence, |current| current.min(confidence)),
        );
        result.confidence_max = Some(
            result
                .confidence_max
                .map_or(confidence, |current| current.max(confidence)),
        );
    }
    if confidence_weight > 0 {
        result.confidence =
            Some((weighted_confidence / confidence_weight as f32 * 10.0).round() / 10.0);
    }
    let mut chars = result.text.chars();
    let mut text = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        text.push_str("\n… OCR text truncated");
        result.truncated = true;
    }
    result.text = text;
    result
}

pub struct ReadImage;

#[async_trait]
impl AgentTool for ReadImage {
    fn name(&self) -> &'static str {
        "read_image"
    }

    fn description(&self) -> &'static str {
        "Read image dimensions, format, hash, and optional OCR text. OCR uses the local tesseract binary when installed."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Image path relative to the worktree root"},
                "ocr": {"type": "boolean", "description": "Run tesseract OCR when available; default false"},
                "max_chars": {"type": "integer", "minimum": 100, "maximum": 200000, "description": "OCR text cap; default 20000"},
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let relative = input
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("path is required"))?;
        let path = safe_existing_path(&context.root, relative)?;
        if !path.is_file() {
            return Ok(ToolOutput::error_structured(
                "read_image path must be a file",
                json!({"code": "INVALID_ARGUMENT", "path": relative}),
            ));
        }
        let metadata = std::fs::metadata(&path)?;
        if metadata.len() > 64 * 1024 * 1024 {
            return Ok(ToolOutput::error_structured(
                "image exceeds the 64 MiB limit",
                json!({"code": "FILE_TOO_LARGE", "size_bytes": metadata.len()}),
            ));
        }
        let dimensions = image_dimensions(context, &path).await;
        let Some((width, height, format)) = dimensions else {
            return Ok(ToolOutput::error_structured(
                "the file is not a readable image on this platform",
                json!({"code": "UNSUPPORTED_CONTENT", "path": relative}),
            ));
        };
        let ocr = input.get("ocr").and_then(Value::as_bool).unwrap_or(false);
        let max_chars = input
            .get("max_chars")
            .and_then(Value::as_u64)
            .unwrap_or(20_000)
            .clamp(100, 200_000) as usize;
        let mut warnings = Vec::new();
        let mut ocr_result = None;
        if ocr {
            if which("tesseract").is_none() {
                warnings.push("tesseract is not installed; OCR skipped".to_string());
            } else {
                let command = format!(
                    "tesseract {} stdout --psm 6 tsv",
                    shell_quote(&path.to_string_lossy())
                );
                match crate::tools::run_sandboxed_shell_captured(
                    context,
                    &command,
                    std::time::Duration::from_secs(60),
                )
                .await
                {
                    Ok(captured) if captured.exit_code == 0 => {
                        let result = parse_tesseract_tsv(&captured.stdout, max_chars);
                        if result.words == 0 {
                            warnings
                                .push("tesseract completed but recognized no words".to_string());
                        }
                        if result.truncated {
                            warnings
                                .push(format!("OCR text was truncated at {max_chars} characters"));
                        }
                        ocr_result = Some(result);
                    }
                    Ok(captured) => {
                        warnings.push(format!(
                            "tesseract exited {}: {}",
                            captured.exit_code,
                            captured.stderr.chars().take(300).collect::<String>()
                        ));
                    }
                    Err(error) => warnings.push(format!("OCR failed: {error}")),
                }
            }
        }
        let ocr_text = ocr_result.as_ref().map(|result| result.text.clone());
        let hash = sha256_file(&path)?;
        let mime = crate::tooling::files::mime_for_path(&path);
        let ocr_summary = ocr_result
            .as_ref()
            .map(|result| {
                format!(
                    " · {} OCR chars{}",
                    result.text.chars().count(),
                    result
                        .confidence
                        .map(|confidence| format!(" · {confidence:.1}% OCR confidence"))
                        .unwrap_or_default()
                )
            })
            .unwrap_or_default();
        let summary = format!(
            "{} · {}x{} · {} · {} bytes{}",
            relative,
            width,
            height,
            format,
            metadata.len(),
            ocr_summary
        );
        Ok(ToolOutput::ok_structured(
            truncate(format!(
                "{summary}\n{}",
                ocr_text.as_deref().unwrap_or("(OCR not requested)")
            )),
            json!({
                "path": relative,
                "size_bytes": metadata.len(),
                "sha256": hash,
                "mime_type": mime,
                "width": width,
                "height": height,
                "format": format,
                "ocr_engine": ocr_result.as_ref().map(|_| "tesseract-tsv"),
                "ocr_text": ocr_text,
                "ocr_confidence": ocr_result.as_ref().and_then(|result| result.confidence),
                "ocr_confidence_min": ocr_result.as_ref().and_then(|result| result.confidence_min),
                "ocr_confidence_max": ocr_result.as_ref().and_then(|result| result.confidence_max),
                "ocr_words": ocr_result.as_ref().map(|result| result.words),
                "warnings": warnings,
            }),
        ))
    }
}

async fn image_dimensions(context: &ToolContext, path: &Path) -> Option<(u64, u64, String)> {
    let command = format!(
        "sips -g pixelWidth -g pixelHeight -g format {}",
        shell_quote(&path.to_string_lossy())
    );
    let captured = crate::tools::run_sandboxed_shell_captured(
        context,
        &command,
        std::time::Duration::from_secs(30),
    )
    .await
    .ok()?;
    if captured.exit_code != 0 {
        return None;
    }
    let mut width = None;
    let mut height = None;
    let mut format = "unknown".to_string();
    for line in captured.stdout.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "pixelWidth" => width = value.parse::<u64>().ok(),
            "pixelHeight" => height = value.parse::<u64>().ok(),
            "format" => format = value.to_string(),
            _ => {}
        }
    }
    width
        .zip(height)
        .map(|(width, height)| (width, height, format))
}

fn which(program: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | ':'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// Explicit media-tool availability. `None` means the daemon must not fake
/// metadata; callers receive a typed `MEDIA_TOOL_UNAVAILABLE` failure.
#[derive(Debug, Clone, Default)]
pub struct MediaTools {
    pub ffprobe: Option<PathBuf>,
    pub ffmpeg: Option<PathBuf>,
    pub sips: Option<PathBuf>,
}

impl MediaTools {
    /// Discover tools from explicit overrides first, then PATH. The overrides
    /// exist for operators with non-standard installs and for deterministic
    /// tests; they never change the approval or bounds model.
    pub fn discover() -> Self {
        let ffprobe = std::env::var_os("VERLINPARK_FFPROBE")
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .or_else(|| which("ffprobe"));
        let ffmpeg = std::env::var_os("VERLINPARK_FFMPEG")
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .or_else(|| which("ffmpeg"));
        let sips = if Path::new("/usr/bin/sips").is_file() {
            Some(PathBuf::from("/usr/bin/sips"))
        } else {
            which("sips")
        };
        Self {
            ffprobe,
            ffmpeg,
            sips,
        }
    }

    pub fn with_programs(
        ffprobe: Option<PathBuf>,
        ffmpeg: Option<PathBuf>,
        sips: Option<PathBuf>,
    ) -> Self {
        Self {
            ffprobe,
            ffmpeg,
            sips,
        }
    }

    pub fn summary(&self) -> Value {
        json!({
            "ffprobe": self.ffprobe.as_ref().map(|path| path.to_string_lossy().to_string()),
            "ffmpeg": self.ffmpeg.as_ref().map(|path| path.to_string_lossy().to_string()),
            "image_metadata": self.sips.as_ref().map(|path| path.to_string_lossy().to_string()),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaError {
    /// A required host tool is not installed. The name is a stable tool id, not
    /// a user path.
    ToolUnavailable(&'static str),
    /// The probe ran but did not produce parseable, bounded metadata.
    ProbeFailed(String),
    /// A host tool exceeded its bounded execution window and was killed.
    Timeout(&'static str),
    LimitExceeded(&'static str),
    FormatUnsupported(String),
    TranscodeFailed(String),
    ApprovalRequired,
    InvalidInput(String),
}

impl MediaError {
    pub fn code(&self) -> crate::tooling::errors::ToolErrorCode {
        use crate::tooling::errors::ToolErrorCode;
        match self {
            Self::ToolUnavailable(_) => ToolErrorCode::MEDIA_TOOL_UNAVAILABLE,
            Self::ProbeFailed(_) => ToolErrorCode::MEDIA_PROBE_FAILED,
            Self::Timeout(_) => ToolErrorCode::TIMEOUT,
            Self::LimitExceeded(_) => ToolErrorCode::MEDIA_LIMIT_EXCEEDED,
            Self::FormatUnsupported(_) => ToolErrorCode::MEDIA_FORMAT_UNSUPPORTED,
            Self::TranscodeFailed(_) => ToolErrorCode::MEDIA_TRANSCODE_FAILED,
            Self::ApprovalRequired => ToolErrorCode::APPROVAL_REQUIRED,
            Self::InvalidInput(_) => ToolErrorCode::INVALID_ARGUMENT,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::ToolUnavailable(tool) => format!(
                "media tool '{tool}' is not installed; metadata and transcoding are not fabricated"
            ),
            Self::ProbeFailed(detail) => format!("media probe failed: {detail}"),
            Self::Timeout(tool) => format!("media tool '{tool}' exceeded its bounded timeout"),
            Self::LimitExceeded(limit) => {
                format!("media request exceeds the hard {limit} limit")
            }
            Self::FormatUnsupported(format) => {
                format!("unsupported media output format: {format}")
            }
            Self::TranscodeFailed(detail) => format!("media transcode failed: {detail}"),
            Self::ApprovalRequired => {
                "media transcoding requires explicit confirmation (approved=true or --yes)"
                    .to_string()
            }
            Self::InvalidInput(detail) => format!("invalid media request: {detail}"),
        }
    }

    pub fn evidence(&self) -> Option<String> {
        match self {
            Self::ToolUnavailable(tool) => Some(format!("tool={tool}")),
            Self::Timeout(tool) => Some(format!("tool={tool}")),
            Self::LimitExceeded(limit) => Some(format!("limit={limit}")),
            Self::FormatUnsupported(format) => Some(format!("format={format}")),
            Self::ProbeFailed(detail)
            | Self::TranscodeFailed(detail)
            | Self::InvalidInput(detail) => {
                Some(format!("detail={}", sanitize_media_detail(detail)))
            }
            Self::ApprovalRequired => None,
        }
    }
}

impl std::fmt::Display for MediaError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl std::error::Error for MediaError {}

#[derive(Debug, Clone)]
struct CommandCapture {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_bytes: u64,
    stderr_bytes: u64,
    stdout_truncated: bool,
    stderr_truncated: bool,
    exit_code: i32,
    timed_out: bool,
    elapsed: Duration,
}

/// Run one local helper with no shell, a scrubbed environment, bounded output,
/// and a hard timeout. Pipes are drained on reader threads so a chatty helper
/// cannot deadlock the daemon before the timeout fires.
fn run_bounded(
    program: &Path,
    args: &[String],
    timeout: Duration,
    stdout_cap: usize,
    stderr_cap: usize,
) -> Result<CommandCapture, MediaError> {
    let started = Instant::now();
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    for key in ["TMPDIR", "LANG", "LC_ALL", "SYSTEMROOT", "TERM"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    let mut path_entries = vec![
        "/usr/bin".to_string(),
        "/bin".to_string(),
        "/usr/sbin".to_string(),
        "/sbin".to_string(),
    ];
    if let Some(parent) = program.parent() {
        path_entries.insert(0, parent.to_string_lossy().to_string());
    }
    if let Some(existing) = std::env::var_os("PATH") {
        path_entries.push(existing.to_string_lossy().to_string());
    }
    command.env("PATH", path_entries.join(":"));
    command.env("HOME", std::env::temp_dir());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = command
        .spawn()
        .map_err(|_error| MediaError::ToolUnavailable(program_tool_name(program)))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| MediaError::ProbeFailed("helper stdout pipe unavailable".to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| MediaError::ProbeFailed("helper stderr pipe unavailable".to_string()))?;
    let stdout_handle = std::thread::spawn(move || read_capped(stdout, stdout_cap));
    let stderr_handle = std::thread::spawn(move || read_capped(stderr, stderr_cap));

    let deadline = started + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                kill_child_group(&mut child);
                let _ = child.wait();
                return Err(MediaError::ProbeFailed(format!(
                    "waiting for helper: {}",
                    sanitize_media_detail(&error.to_string())
                )));
            }
        }
        if Instant::now() >= deadline {
            timed_out = true;
            kill_child_group(&mut child);
            match child.wait() {
                Ok(status) => break status,
                Err(_) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::process::ExitStatusExt;
                        break std::process::ExitStatus::from_raw(137);
                    }
                    #[cfg(not(unix))]
                    {
                        return Err(MediaError::Timeout(program_tool_name(program)));
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = stdout_handle
        .join()
        .unwrap_or_else(|_| (Vec::new(), 0, false));
    let stderr = stderr_handle
        .join()
        .unwrap_or_else(|_| (Vec::new(), 0, false));
    Ok(CommandCapture {
        stdout: stdout.0,
        stdout_bytes: stdout.1,
        stdout_truncated: stdout.2,
        stderr: stderr.0,
        stderr_bytes: stderr.1,
        stderr_truncated: stderr.2,
        exit_code: status.code().unwrap_or(-1),
        timed_out,
        elapsed: started.elapsed(),
    })
}

fn program_tool_name(program: &Path) -> &'static str {
    match program.file_name().and_then(|name| name.to_str()) {
        Some("ffprobe") => "ffprobe",
        Some("ffmpeg") => "ffmpeg",
        Some("sips") => "sips",
        _ => "media-helper",
    }
}

/// Kill the helper and any children it spawned. The child is placed in its own
/// process group before spawn, so a shell wrapper cannot leave a grandchild
/// holding the output pipes open after the timeout.
fn kill_child_group(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pid = child.id();
        if let Ok(pid) = i32::try_from(pid) {
            // Negative pid targets the process group created by
            // `process_group(0)` above.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
    let _ = child.kill();
}

fn read_capped(mut reader: impl std::io::Read, cap: usize) -> (Vec<u8>, u64, bool) {
    let mut retained = Vec::new();
    let mut total = 0u64;
    let mut truncated = false;
    let mut buffer = [0u8; 16 * 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                total = total.saturating_add(read as u64);
                let remaining = cap.saturating_sub(retained.len());
                if remaining > 0 {
                    retained.extend_from_slice(&buffer[..read.min(remaining)]);
                }
                if read > remaining {
                    truncated = true;
                }
            }
            Err(_) => break,
        }
    }
    (retained, total, truncated)
}

fn sanitize_media_detail(detail: &str) -> String {
    detail
        .chars()
        .filter(|character| !character.is_control())
        .take(400)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Probe a local media file. Never accepts a URL, never invokes a shell, and
/// never invents duration, dimensions, or codecs. If ffprobe is unavailable and
/// the file is a still image, native image metadata is used and explicitly
/// labeled `image-metadata`.
pub fn probe_media(
    path: &Path,
    tools: &MediaTools,
    timeout: Duration,
) -> Result<MediaInfo, MediaError> {
    let display = path.to_string_lossy();
    if display.contains("://") {
        return Err(MediaError::InvalidInput(
            "media probing accepts local files only".to_string(),
        ));
    }
    if !path.is_file() {
        return Err(MediaError::InvalidInput(
            "media path is not a file".to_string(),
        ));
    }
    let metadata = std::fs::metadata(path)
        .map_err(|error| MediaError::ProbeFailed(format!("metadata: {error}")))?;
    if metadata.len() > MAX_MEDIA_PROBE_BYTES {
        return Err(MediaError::LimitExceeded("media_input_bytes"));
    }
    let mut warnings = Vec::new();
    if let Some(ffprobe) = &tools.ffprobe {
        let args = vec![
            "-v".to_string(),
            "error".to_string(),
            "-print_format".to_string(),
            "json".to_string(),
            "-show_format".to_string(),
            "-show_streams".to_string(),
            "-protocol_whitelist".to_string(),
            "file".to_string(),
            "-i".to_string(),
            path.to_string_lossy().to_string(),
        ];
        match run_bounded(ffprobe, &args, timeout, MAX_FFPROBE_OUTPUT_BYTES, 16 * 1024) {
            Ok(capture) if capture.timed_out => {
                return Err(MediaError::Timeout("ffprobe"));
            }
            Ok(capture) if capture.exit_code == 0 => {
                if capture.stdout_truncated {
                    return Err(MediaError::LimitExceeded("ffprobe_output_bytes"));
                }
                let text = String::from_utf8(capture.stdout).map_err(|_| {
                    MediaError::ProbeFailed("ffprobe output was not UTF-8".to_string())
                })?;
                let mut info = parse_ffprobe_json(&text)?;
                info.source = "ffprobe".to_string();
                info.tool = Some("ffprobe".to_string());
                info.warnings.extend(warnings);
                return Ok(info);
            }
            Ok(capture) => {
                warnings.push(format!(
                    "ffprobe exited {} after {} stdout/{} stderr bytes{}: {}",
                    capture.exit_code,
                    capture.stdout_bytes,
                    capture.stderr_bytes,
                    if capture.stderr_truncated {
                        " (stderr truncated)"
                    } else {
                        ""
                    },
                    sanitize_media_detail(&String::from_utf8_lossy(&capture.stderr))
                ));
            }
            Err(error) => warnings.push(error.message()),
        }
    } else {
        warnings.push("ffprobe is not installed".to_string());
    }

    if is_still_image(path) {
        if let Some(sips) = &tools.sips {
            let args = vec![
                "-g".to_string(),
                "pixelWidth".to_string(),
                "-g".to_string(),
                "pixelHeight".to_string(),
                "-g".to_string(),
                "format".to_string(),
                path.to_string_lossy().to_string(),
            ];
            match run_bounded(sips, &args, timeout, 64 * 1024, 8 * 1024) {
                Ok(capture) if capture.timed_out => return Err(MediaError::Timeout("sips")),
                Ok(capture) if capture.exit_code == 0 => {
                    let text = String::from_utf8_lossy(&capture.stdout);
                    let mut width = None;
                    let mut height = None;
                    let mut format = None;
                    for line in text.lines() {
                        let Some((key, value)) = line.split_once(':') else {
                            continue;
                        };
                        let value = value.trim();
                        match key.trim() {
                            "pixelWidth" => width = value.parse::<u64>().ok(),
                            "pixelHeight" => height = value.parse::<u64>().ok(),
                            "format" => format = Some(value.to_ascii_lowercase()),
                            _ => {}
                        }
                    }
                    if width.is_some() || height.is_some() {
                        let mut info = MediaInfo {
                            container: format.clone(),
                            format_long_name: format,
                            width,
                            height,
                            source: "image-metadata".to_string(),
                            tool: Some("sips".to_string()),
                            warnings,
                            ..MediaInfo::default()
                        };
                        info.warnings.push(
                            "ffprobe was unavailable or failed; still-image metadata came from sips"
                                .to_string(),
                        );
                        return Ok(info);
                    }
                }
                Ok(capture) => warnings.push(format!(
                    "sips exited {}: {}",
                    capture.exit_code,
                    sanitize_media_detail(&String::from_utf8_lossy(&capture.stderr))
                )),
                Err(error) => warnings.push(error.message()),
            }
        } else {
            warnings.push("sips is not installed".to_string());
        }
    }

    if tools.ffprobe.is_none() {
        return Err(MediaError::ToolUnavailable("ffprobe"));
    }
    Err(MediaError::ProbeFailed(if warnings.is_empty() {
        "probe produced no metadata".to_string()
    } else {
        warnings.join("; ")
    }))
}

fn is_still_image(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some(
            "png"
                | "jpg"
                | "jpeg"
                | "gif"
                | "heic"
                | "heif"
                | "tif"
                | "tiff"
                | "bmp"
                | "webp"
                | "avif"
        )
    )
}

/// Parse the bounded JSON emitted by `ffprobe -print_format json`. The parser
/// keeps only known scalar fields and caps stream/tag counts, so a hostile file
/// cannot inject unbounded data into events or the UI.
pub fn parse_ffprobe_json(text: &str) -> Result<MediaInfo, MediaError> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| MediaError::ProbeFailed(format!("invalid ffprobe JSON: {error}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| MediaError::ProbeFailed("ffprobe JSON root is not an object".to_string()))?;
    let format = object.get("format").and_then(Value::as_object);
    let mut info = MediaInfo {
        container: format
            .and_then(|format| format.get("format_name"))
            .and_then(Value::as_str)
            .map(|value| value.chars().take(200).collect()),
        format_long_name: format
            .and_then(|format| format.get("format_long_name"))
            .and_then(Value::as_str)
            .map(|value| value.chars().take(300).collect()),
        duration_secs: format
            .and_then(|format| format.get("duration"))
            .and_then(parse_json_number)
            .filter(|duration| duration.is_finite() && *duration >= 0.0),
        bit_rate: format
            .and_then(|format| format.get("bit_rate"))
            .and_then(parse_json_u64),
        ..MediaInfo::default()
    };
    if let Some(tags) = format
        .and_then(|format| format.get("tags"))
        .and_then(Value::as_object)
    {
        for (key, value) in tags.iter().take(24) {
            if key.chars().count() > 80 {
                continue;
            }
            if let Some(text) = value.as_str() {
                info.tags.insert(
                    key.to_ascii_lowercase(),
                    text.chars().take(300).collect::<String>(),
                );
            }
        }
    }
    let streams = object
        .get("streams")
        .and_then(Value::as_array)
        .map(|streams| streams.iter().take(64).collect::<Vec<_>>())
        .unwrap_or_default();
    for (index, stream) in streams.into_iter().enumerate() {
        let Some(stream) = stream.as_object() else {
            continue;
        };
        let codec_type = stream
            .get("codec_type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .chars()
            .take(40)
            .collect::<String>();
        let stream_info = MediaStreamInfo {
            index: stream
                .get("index")
                .and_then(parse_json_u64)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(index as u32),
            codec_type,
            codec_name: stream
                .get("codec_name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .chars()
                .take(80)
                .collect(),
            codec_long_name: stream
                .get("codec_long_name")
                .and_then(Value::as_str)
                .map(|value| value.chars().take(200).collect()),
            width: stream.get("width").and_then(parse_json_u64),
            height: stream.get("height").and_then(parse_json_u64),
            duration_secs: stream
                .get("duration")
                .and_then(parse_json_number)
                .filter(|duration| duration.is_finite() && *duration >= 0.0),
            bit_rate: stream.get("bit_rate").and_then(parse_json_u64),
            sample_rate: stream.get("sample_rate").and_then(parse_json_u64),
            channels: stream.get("channels").and_then(parse_json_u64),
            language: stream
                .get("tags")
                .and_then(|tags| tags.get("language"))
                .and_then(Value::as_str)
                .map(|value| value.chars().take(40).collect()),
            frames: stream.get("nb_frames").and_then(parse_json_u64),
        };
        if info.width.is_none() && stream_info.codec_type == "video" {
            info.width = stream_info.width;
            info.height = stream_info.height;
        }
        if info.duration_secs.is_none() {
            info.duration_secs = stream_info.duration_secs;
        }
        info.streams.push(stream_info);
    }
    if info.streams.is_empty() && info.container.is_none() {
        return Err(MediaError::ProbeFailed(
            "ffprobe returned neither a format nor any stream".to_string(),
        ));
    }
    Ok(info)
}

fn parse_json_number(value: &Value) -> Option<f64> {
    value.as_f64().or_else(|| {
        value
            .as_str()
            .and_then(|text| text.trim().parse::<f64>().ok())
    })
}

fn parse_json_u64(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_str()
            .and_then(|text| text.trim().parse::<u64>().ok())
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaOutputFormat {
    Mp4,
    Webm,
    Mp3,
    Wav,
    Png,
    Jpeg,
}

impl MediaOutputFormat {
    pub fn parse(value: &str) -> Result<Self, MediaError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mp4" => Ok(Self::Mp4),
            "webm" => Ok(Self::Webm),
            "mp3" => Ok(Self::Mp3),
            "wav" => Ok(Self::Wav),
            "png" => Ok(Self::Png),
            "jpeg" | "jpg" => Ok(Self::Jpeg),
            other => Err(MediaError::FormatUnsupported(
                other.chars().take(40).collect(),
            )),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Webm => "webm",
            Self::Mp3 => "mp3",
            Self::Wav => "wav",
            Self::Png => "png",
            Self::Jpeg => "jpeg",
        }
    }

    pub fn extension(&self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Webm => "webm",
            Self::Mp3 => "mp3",
            Self::Wav => "wav",
            Self::Png => "png",
            Self::Jpeg => "jpg",
        }
    }

    pub fn mime_type(&self) -> &'static str {
        match self {
            Self::Mp4 => "video/mp4",
            Self::Webm => "video/webm",
            Self::Mp3 => "audio/mpeg",
            Self::Wav => "audio/wav",
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
        }
    }

    pub fn codec_args(&self) -> Vec<&'static str> {
        match self {
            Self::Mp4 => vec![
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-crf",
                "23",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-b:a",
                "128k",
                "-movflags",
                "+faststart",
            ],
            Self::Webm => vec![
                "-c:v",
                "libvpx-vp9",
                "-crf",
                "32",
                "-b:v",
                "0",
                "-c:a",
                "libopus",
                "-b:a",
                "96k",
            ],
            Self::Mp3 => vec!["-vn", "-c:a", "libmp3lame", "-b:a", "192k"],
            Self::Wav => vec!["-vn", "-c:a", "pcm_s16le"],
            Self::Png => vec!["-an", "-frames:v", "1", "-c:v", "png"],
            Self::Jpeg => vec!["-an", "-frames:v", "1", "-q:v", "3"],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscodePlan {
    pub format: MediaOutputFormat,
    pub max_duration_secs: u64,
    pub max_bytes: u64,
    pub timeout: Duration,
}

/// Validate an explicit transcode request. No defaults are silent: the caller
/// must set `approved`, and every bound is clamped down to the daemon hard cap.
pub fn plan_transcode(
    format: &str,
    approved: bool,
    max_duration_secs: Option<u64>,
    max_bytes: Option<u64>,
) -> Result<TranscodePlan, MediaError> {
    if !approved {
        return Err(MediaError::ApprovalRequired);
    }
    let format = MediaOutputFormat::parse(format)?;
    let max_duration_secs = max_duration_secs
        .unwrap_or(TRANSCODE_DEFAULT_DURATION_SECS)
        .clamp(1, TRANSCODE_MAX_DURATION_SECS);
    let max_bytes = max_bytes
        .unwrap_or(TRANSCODE_DEFAULT_BYTES)
        .clamp(TRANSCODE_MIN_BYTES, TRANSCODE_MAX_BYTES);
    let timeout_secs = max_duration_secs
        .saturating_add(60)
        .clamp(30, DEFAULT_TRANSCODE_TIMEOUT_SECS);
    Ok(TranscodePlan {
        format,
        max_duration_secs,
        max_bytes,
        timeout: Duration::from_secs(timeout_secs),
    })
}

#[derive(Debug, Clone)]
pub struct TranscodeOutcome {
    pub size_bytes: u64,
    pub duration_secs: Option<f64>,
    pub elapsed: Duration,
    pub command_summary: String,
    pub warnings: Vec<String>,
}

/// Run ffmpeg into `output` and enforce post-conditions. The output path is
/// supplied by the caller and must already be inside daemon-owned scratch; this
/// function does not accept a workspace destination.
pub fn transcode_media(
    input: &Path,
    output: &Path,
    plan: &TranscodePlan,
    tools: &MediaTools,
) -> Result<TranscodeOutcome, MediaError> {
    let Some(ffmpeg) = &tools.ffmpeg else {
        return Err(MediaError::ToolUnavailable("ffmpeg"));
    };
    if !input.is_file() {
        return Err(MediaError::InvalidInput(
            "transcode input is not a file".to_string(),
        ));
    }
    let mut args = vec![
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-v".to_string(),
        "error".to_string(),
        "-y".to_string(),
        "-protocol_whitelist".to_string(),
        "file".to_string(),
        "-i".to_string(),
        input.to_string_lossy().to_string(),
        "-t".to_string(),
        plan.max_duration_secs.to_string(),
        "-map_metadata".to_string(),
        "-1".to_string(),
    ];
    args.extend(plan.format.codec_args().into_iter().map(str::to_string));
    args.push(output.to_string_lossy().to_string());
    let capture = run_bounded(
        ffmpeg,
        &args,
        plan.timeout,
        MAX_FFMPEG_OUTPUT_BYTES,
        MAX_FFMPEG_OUTPUT_BYTES,
    )?;
    if capture.timed_out {
        let _ = std::fs::remove_file(output);
        return Err(MediaError::Timeout("ffmpeg"));
    }
    if capture.exit_code != 0 {
        let _ = std::fs::remove_file(output);
        let mut detail = sanitize_ffmpeg_stderr(&capture.stderr, input, output);
        if capture.stderr_truncated {
            detail.push_str(" (stderr truncated)");
        }
        return Err(MediaError::TranscodeFailed(detail));
    }
    let metadata = std::fs::metadata(output)
        .map_err(|error| MediaError::TranscodeFailed(format!("output metadata: {error}")))?;
    if metadata.len() > plan.max_bytes {
        let _ = std::fs::remove_file(output);
        return Err(MediaError::LimitExceeded("output_bytes"));
    }
    if metadata.len() == 0 {
        let _ = std::fs::remove_file(output);
        return Err(MediaError::TranscodeFailed(
            "ffmpeg produced an empty output".to_string(),
        ));
    }
    let mut warnings = Vec::new();
    let duration_secs = match probe_media(output, tools, Duration::from_secs(20)) {
        Ok(info) => {
            if let Some(duration) = info.duration_secs
                && duration > plan.max_duration_secs as f64 + 1.0
            {
                let _ = std::fs::remove_file(output);
                return Err(MediaError::LimitExceeded("duration"));
            }
            info.duration_secs
        }
        Err(error) => {
            warnings.push(format!(
                "output duration was not re-probed: {}",
                error.message()
            ));
            None
        }
    };
    Ok(TranscodeOutcome {
        size_bytes: metadata.len(),
        duration_secs,
        elapsed: capture.elapsed,
        command_summary: format!(
            "{} · {} · max {}s",
            ffmpeg
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("ffmpeg"),
            plan.format.as_str(),
            plan.max_duration_secs
        ),
        warnings,
    })
}

fn sanitize_ffmpeg_stderr(stderr: &[u8], input: &Path, output: &Path) -> String {
    let mut text = String::from_utf8_lossy(stderr).to_string();
    for path in [input, output] {
        let rendered = path.to_string_lossy();
        if !rendered.is_empty() {
            text = text.replace(rendered.as_ref(), "<media>");
        }
    }
    let line = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("ffmpeg failed");
    sanitize_media_detail(line)
}

/// A bounded, encoding-labeled preview of extracted archive or media bytes.
/// The caller decides whether the artifact itself is published.
pub fn bounded_preview(bytes: &[u8]) -> (String, &'static str, bool) {
    let retained = &bytes[..bytes.len().min(MEDIA_PREVIEW_BYTES)];
    let truncated = bytes.len() > retained.len();
    match std::str::from_utf8(retained) {
        Ok(text) => (
            text.chars().take(MEDIA_PREVIEW_BYTES).collect(),
            "utf8",
            truncated,
        ),
        Err(_) => (hex_preview(retained), "hex", truncated),
    }
}

fn hex_preview(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 3);
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            text.push(' ');
        }
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

#[cfg(test)]
mod media_tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    const FFPROBE_FIXTURE: &str = r#"{
        "streams": [
            {"index": 0, "codec_name": "h264", "codec_long_name": "H.264 / AVC", "codec_type": "video",
             "width": 1920, "height": 1080, "duration": "12.500000", "bit_rate": "4500000", "nb_frames": "300"},
            {"index": 1, "codec_name": "aac", "codec_type": "audio", "sample_rate": "48000", "channels": 2,
             "duration": "12.480000", "bit_rate": "128000", "tags": {"language": "eng"}}
        ],
        "format": {
            "format_name": "mov,mp4,m4a,3gp,3g2,mj2",
            "format_long_name": "QuickTime / MOV",
            "duration": "12.500000",
            "bit_rate": "4700000",
            "tags": {"encoder": "Lavf60.16.100", "major_brand": "isom"}
        }
    }"#;

    #[test]
    fn parses_ffprobe_fixture_with_bounded_fields() {
        let info = parse_ffprobe_json(FFPROBE_FIXTURE).unwrap();
        assert_eq!(info.container.as_deref(), Some("mov,mp4,m4a,3gp,3g2,mj2"));
        assert_eq!(info.format_long_name.as_deref(), Some("QuickTime / MOV"));
        assert_eq!(info.duration_secs, Some(12.5));
        assert_eq!(info.bit_rate, Some(4_700_000));
        assert_eq!(info.width, Some(1920));
        assert_eq!(info.height, Some(1080));
        assert_eq!(info.streams.len(), 2);
        assert_eq!(info.streams[0].codec_name, "h264");
        assert_eq!(info.streams[0].frames, Some(300));
        assert_eq!(info.streams[1].channels, Some(2));
        assert_eq!(info.streams[1].language.as_deref(), Some("eng"));
        assert_eq!(
            info.tags.get("encoder").map(String::as_str),
            Some("Lavf60.16.100")
        );
    }

    #[test]
    fn rejects_invalid_ffprobe_json() {
        assert!(matches!(
            parse_ffprobe_json("not json"),
            Err(MediaError::ProbeFailed(_))
        ));
        assert!(matches!(
            parse_ffprobe_json("{}"),
            Err(MediaError::ProbeFailed(_))
        ));
    }

    #[test]
    fn missing_ffprobe_is_a_typed_error_not_fake_metadata() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("clip.mp4");
        std::fs::write(&path, b"not really media").unwrap();
        let tools = MediaTools::with_programs(None, None, None);
        let error = probe_media(&path, &tools, Duration::from_secs(2)).unwrap_err();
        assert_eq!(error, MediaError::ToolUnavailable("ffprobe"));
        assert_eq!(
            error.code(),
            crate::tooling::errors::ToolErrorCode::MEDIA_TOOL_UNAVAILABLE
        );
    }

    #[test]
    fn timeout_kills_a_hung_probe_helper() {
        let dir = TempDir::new().unwrap();
        let script = dir.path().join("ffprobe");
        std::fs::write(&script, "#!/bin/sh\nsleep 5\nprintf '{}'\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let media = dir.path().join("clip.mp4");
        std::fs::write(&media, b"media").unwrap();
        let tools = MediaTools::with_programs(Some(script), None, None);
        let error = probe_media(&media, &tools, Duration::from_millis(120)).unwrap_err();
        assert_eq!(error, MediaError::Timeout("ffprobe"));
    }

    #[test]
    fn timeout_kills_a_helper_grandchild_process_group() {
        let dir = TempDir::new().unwrap();
        let script = dir.path().join("ffprobe");
        std::fs::write(&script, "#!/bin/sh\nsleep 5 &\nwait\nprintf '{}'\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let media = dir.path().join("clip.mp4");
        std::fs::write(&media, b"media").unwrap();
        let tools = MediaTools::with_programs(Some(script), None, None);
        let started = Instant::now();
        let error = probe_media(&media, &tools, Duration::from_millis(120)).unwrap_err();
        assert_eq!(error, MediaError::Timeout("ffprobe"));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the process group must be killed before the reader join"
        );
    }

    #[test]
    fn ffprobe_fixture_script_round_trips() {
        let dir = TempDir::new().unwrap();
        let script = dir.path().join("ffprobe");
        std::fs::write(
            &script,
            format!("#!/bin/sh\ncat <<'EOF'\n{FFPROBE_FIXTURE}\nEOF\n"),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let media = dir.path().join("clip.mp4");
        std::fs::write(&media, b"media").unwrap();
        let tools = MediaTools::with_programs(Some(script), None, None);
        let info = probe_media(&media, &tools, Duration::from_secs(5)).unwrap();
        assert_eq!(info.source, "ffprobe");
        assert_eq!(info.tool.as_deref(), Some("ffprobe"));
        assert_eq!(info.duration_secs, Some(12.5));
    }

    #[test]
    fn ffprobe_output_cap_is_enforced() {
        let dir = TempDir::new().unwrap();
        let script = dir.path().join("ffprobe");
        std::fs::write(
            &script,
            "#!/bin/sh\ni=0\nwhile [ $i -lt 200000 ]; do printf 'aaaaaaaaaaaaaaaa'; i=$((i+1)); done\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let media = dir.path().join("clip.mp4");
        std::fs::write(&media, b"media").unwrap();
        let tools = MediaTools::with_programs(Some(script), None, None);
        let error = probe_media(&media, &tools, Duration::from_secs(10)).unwrap_err();
        assert!(
            matches!(
                error,
                MediaError::LimitExceeded("ffprobe_output_bytes") | MediaError::ProbeFailed(_)
            ),
            "{error:?}"
        );
    }

    #[test]
    fn still_image_falls_back_to_image_metadata_without_ffprobe() {
        if !Path::new("/usr/bin/sips").is_file() {
            return;
        }
        use base64::Engine as _;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("dot.png");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            .unwrap();
        std::fs::write(&path, bytes).unwrap();
        let tools = MediaTools::with_programs(None, None, Some(PathBuf::from("/usr/bin/sips")));
        let info = probe_media(&path, &tools, Duration::from_secs(10)).unwrap();
        assert_eq!(info.source, "image-metadata");
        assert_eq!(info.width, Some(1));
        assert_eq!(info.height, Some(1));
        assert!(
            info.warnings
                .iter()
                .any(|warning| warning.contains("ffprobe"))
        );
    }

    #[test]
    fn validates_transcode_plan_bounds_and_approval() {
        let error = plan_transcode("mp4", false, None, None).unwrap_err();
        assert_eq!(error, MediaError::ApprovalRequired);

        let plan = plan_transcode("mp4", true, Some(999_999), Some(999_999_999)).unwrap();
        assert_eq!(plan.max_duration_secs, TRANSCODE_MAX_DURATION_SECS);
        assert_eq!(plan.max_bytes, TRANSCODE_MAX_BYTES);

        let plan = plan_transcode("webm", true, Some(0), Some(1)).unwrap();
        assert_eq!(plan.max_duration_secs, 1);
        assert_eq!(plan.max_bytes, TRANSCODE_MIN_BYTES);

        assert!(matches!(
            plan_transcode("avi", true, None, None),
            Err(MediaError::FormatUnsupported(_))
        ));
    }

    #[test]
    fn codec_arguments_are_deterministic_and_format_scoped() {
        assert!(MediaOutputFormat::Mp4.codec_args().contains(&"libx264"));
        assert!(MediaOutputFormat::Webm.codec_args().contains(&"libvpx-vp9"));
        assert!(MediaOutputFormat::Mp3.codec_args().contains(&"libmp3lame"));
        assert!(MediaOutputFormat::Wav.codec_args().contains(&"pcm_s16le"));
        assert!(MediaOutputFormat::Png.codec_args().contains(&"png"));
        assert!(MediaOutputFormat::Jpeg.codec_args().contains(&"3"));
        assert_eq!(
            MediaOutputFormat::parse("JPG").unwrap(),
            MediaOutputFormat::Jpeg
        );
    }

    #[test]
    fn missing_ffmpeg_never_claims_a_transcode() {
        let dir = TempDir::new().unwrap();
        let input = dir.path().join("in.mp4");
        let output = dir.path().join("out.mp4");
        std::fs::write(&input, b"media").unwrap();
        let plan = plan_transcode("mp4", true, None, None).unwrap();
        let tools = MediaTools::with_programs(None, None, None);
        let error = transcode_media(&input, &output, &plan, &tools).unwrap_err();
        assert_eq!(error, MediaError::ToolUnavailable("ffmpeg"));
        assert!(!output.exists());
    }

    #[test]
    fn transcode_enforces_output_bytes_and_removes_excess() {
        let dir = TempDir::new().unwrap();
        let script = dir.path().join("ffmpeg");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nout=\"\"\nwhile [ $# -gt 0 ]; do out=\"$1\"; shift; done\nhead -c {} /dev/zero > \"$out\"\n",
                3 * 1024 * 1024
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let input = dir.path().join("in.mp4");
        let output = dir.path().join("out.mp4");
        std::fs::write(&input, b"media").unwrap();
        let plan = plan_transcode("mp4", true, Some(5), Some(1024 * 1024)).unwrap();
        let tools = MediaTools::with_programs(None, Some(script), None);
        let error = transcode_media(&input, &output, &plan, &tools).unwrap_err();
        assert_eq!(error, MediaError::LimitExceeded("output_bytes"));
        assert!(!output.exists());
    }

    #[test]
    fn transcode_failure_redacts_paths() {
        let dir = TempDir::new().unwrap();
        let script = dir.path().join("ffmpeg");
        std::fs::write(
            &script,
            "#!/bin/sh\nprev=\"\"\nfor arg in \"$@\"; do\n  if [ \"$prev\" = \"-i\" ]; then\n    echo \"Invalid data found in $arg\" >&2\n    exit 1\n  fi\n  prev=\"$arg\"\ndone\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let input = dir.path().join("input.mp4");
        let output = dir.path().join("output.mp4");
        std::fs::write(&input, b"media").unwrap();
        let plan = plan_transcode("mp4", true, Some(5), Some(1024 * 1024)).unwrap();
        let tools = MediaTools::with_programs(None, Some(script), None);
        let error = transcode_media(&input, &output, &plan, &tools).unwrap_err();
        let message = error.message();
        assert!(!message.contains(input.to_string_lossy().as_ref()));
        assert!(!message.contains(output.to_string_lossy().as_ref()));
        assert!(message.contains("<media>"));
    }

    #[test]
    fn transcode_rejects_duration_over_the_postcondition_cap() {
        let dir = TempDir::new().unwrap();
        let ffmpeg = dir.path().join("ffmpeg");
        std::fs::write(
            &ffmpeg,
            "#!/bin/sh\nout=\"\"\nwhile [ $# -gt 0 ]; do out=\"$1\"; shift; done\nprintf 'payload' > \"$out\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();
        let ffprobe = dir.path().join("ffprobe");
        std::fs::write(
            &ffprobe,
            "#!/bin/sh\nprintf '{\"format\":{\"format_name\":\"mp4\",\"duration\":\"999\"},\"streams\":[{\"index\":0,\"codec_type\":\"video\",\"codec_name\":\"h264\"}]}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&ffprobe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let input = dir.path().join("in.mp4");
        let output = dir.path().join("out.mp4");
        std::fs::write(&input, b"media").unwrap();
        let plan = plan_transcode("mp4", true, Some(5), Some(1024 * 1024)).unwrap();
        let tools = MediaTools::with_programs(Some(ffprobe), Some(ffmpeg), None);
        let error = transcode_media(&input, &output, &plan, &tools).unwrap_err();
        assert_eq!(error, MediaError::LimitExceeded("duration"));
        assert!(!output.exists());
    }

    #[test]
    fn ffprobe_json_caps_streams_and_tags() {
        let streams = (0..100)
            .map(|index| {
                json!({
                    "index": index,
                    "codec_type": "video",
                    "codec_name": "h264",
                    "width": 16,
                    "height": 16
                })
            })
            .collect::<Vec<_>>();
        let mut tags = serde_json::Map::new();
        for index in 0..40 {
            tags.insert(format!("tag{index}"), json!(format!("value{index}")));
        }
        let value = json!({
            "streams": streams,
            "format": {
                "format_name": "mp4",
                "duration": "1.5",
                "tags": tags
            }
        });
        let info = parse_ffprobe_json(&value.to_string()).unwrap();
        assert_eq!(info.streams.len(), 64);
        assert_eq!(info.tags.len(), 24);
        assert_eq!(info.duration_secs, Some(1.5));
    }

    #[test]
    fn ffprobe_json_ignores_malformed_optional_values() {
        let value = json!({
            "streams": [
                null,
                {"index": "2", "codec_type": "video", "codec_name": null, "duration": "-4", "width": "bad"},
                {"index": 3, "codec_type": "audio", "codec_name": "aac", "sample_rate": "48000"}
            ],
            "format": {"format_name": "matroska", "duration": "not-a-number"}
        });
        let info = parse_ffprobe_json(&value.to_string()).unwrap();
        assert_eq!(info.streams.len(), 2);
        assert_eq!(info.streams[0].index, 2);
        assert_eq!(info.streams[0].codec_name, "");
        assert!(info.streams[0].duration_secs.is_none());
        assert!(info.streams[0].width.is_none());
        assert_eq!(info.streams[1].sample_rate, Some(48_000));
        assert!(info.duration_secs.is_none());
    }

    #[test]
    fn bounded_preview_labels_text_and_binary() {
        let (text, encoding, truncated) = bounded_preview(b"hello");
        assert_eq!(text, "hello");
        assert_eq!(encoding, "utf8");
        assert!(!truncated);

        let (hex, encoding, _) = bounded_preview(&[0xff, 0x00]);
        assert_eq!(encoding, "hex");
        assert_eq!(hex, "ff 00");
    }

    #[test]
    fn transcode_restricts_input_protocols_to_local_files() {
        let dir = TempDir::new().unwrap();
        let args_file = dir.path().join("args.txt");
        let script = dir.path().join("ffmpeg");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nout=\"\"\nfor arg in \"$@\"; do out=\"$arg\"; printf '%s\\n' \"$arg\" >> {}; done\nprintf 'media' > \"$out\"\n",
                args_file.to_string_lossy()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let input = dir.path().join("in.mp4");
        let output = dir.path().join("out.mp4");
        std::fs::write(&input, b"media").unwrap();
        let plan = plan_transcode("mp4", true, Some(5), Some(1024 * 1024)).unwrap();
        let tools = MediaTools::with_programs(None, Some(script), None);
        let outcome = transcode_media(&input, &output, &plan, &tools).unwrap();
        assert_eq!(outcome.size_bytes, 5);
        let args = std::fs::read_to_string(&args_file).unwrap();
        assert!(args.contains("-protocol_whitelist\nfile\n-i\n"));
        assert!(args.contains("-nostdin"));
        assert!(!args.contains("http"));
    }

    #[test]
    fn read_capped_drains_but_bounds_retained_bytes() {
        let (retained, total, truncated) =
            read_capped(std::io::Cursor::new(vec![b'x'; 4096]), 1024);
        assert_eq!(retained.len(), 1024);
        assert_eq!(total, 4096);
        assert!(truncated);
    }

    #[allow(dead_code)]
    fn write_script(path: &Path, body: &str) {
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[cfg(test)]
mod image_tests {
    use super::*;
    use base64::Engine;
    use serde_json::json;
    use tempfile::TempDir;

    fn context(root: &Path) -> ToolContext {
        ToolContext {
            root: root.to_path_buf(),
            project_root: root.to_path_buf(),
            sandbox_root: root.join(".sandbox"),
            secret_roots: Vec::new(),
            network_allowed: false,
            home: root.join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
            remote: None,
            session_id: None,
            process_supervisor: None,
            test_cache: None,
        }
    }

    #[tokio::test]
    async fn reads_png_dimensions() {
        let dir = TempDir::new().unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            .unwrap();
        std::fs::write(dir.path().join("dot.png"), bytes).unwrap();
        let output = ReadImage
            .execute(
                &context(dir.path()),
                json!({"path": "dot.png", "ocr": false}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let structured = output.structured.unwrap();
        assert_eq!(structured["width"], 1);
        assert_eq!(structured["height"], 1);
        assert!(structured["format"].as_str().unwrap().contains("png"));
    }
    #[tokio::test]
    async fn extracts_text_from_a_real_pdf() {
        use lopdf::content::{Content, Operation};
        use lopdf::{Document, Object, Stream, dictionary};
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("hello.pdf");
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica"
        });
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id }
        });
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 24.into()]),
                Operation::new("Td", vec![100.into(), 700.into()]),
                Operation::new("Tj", vec![Object::string_literal("Hello PDF")]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog", "Pages" => pages_id
        });
        let info_id = doc.add_object(dictionary! {
            "Title" => Object::string_literal("Metadata Title"),
            "Author" => Object::string_literal("Tester"),
        });
        doc.trailer.set("Root", catalog_id);
        doc.trailer.set("Info", info_id);
        doc.save(&path).unwrap();

        let output = ReadPdf
            .execute(
                &context(dir.path()),
                json!({"path": "hello.pdf", "include_page_markers": false}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        assert!(output.content.contains("Hello PDF"), "{}", output.content);
        let structured = output.structured.unwrap();
        assert_eq!(structured["page_count"], 1);
        assert_eq!(structured["page_count_source"], "pdf-metadata");
        assert_eq!(structured["metadata"]["title"], "Metadata Title");
        assert_eq!(structured["metadata"]["author"], "Tester");
    }

    #[test]
    fn parses_tesseract_tsv_confidence_and_line_breaks() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
5\t1\t1\t1\t1\t1\t0\t0\t10\t10\t96.0\tHello\n\
5\t1\t1\t1\t1\t2\t10\t0\t10\t10\t90.0\tworld\n\
5\t1\t1\t1\t2\t1\t0\t12\t10\t10\t80.0\tNext\n\
5\t1\t1\t1\t2\t2\t10\t12\t10\t10\t-1\t\n";
        let result = parse_tesseract_tsv(tsv, 20_000);
        assert_eq!(result.text, "Hello world\nNext");
        assert_eq!(result.words, 3);
        assert_eq!(result.confidence, Some(89.3));
        assert_eq!(result.confidence_min, Some(80.0));
        assert_eq!(result.confidence_max, Some(96.0));
        assert!(!result.truncated);

        let truncated = parse_tesseract_tsv(tsv, 5);
        assert_eq!(truncated.text, "Hello\n… OCR text truncated");
        assert!(truncated.truncated);
    }
}
