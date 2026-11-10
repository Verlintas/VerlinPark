//! Bounded document/media readers.
//!
//! PDF text extraction runs in a blocking task and is capped before it reaches
//! the model. Page selection uses form-feed boundaries emitted by the extractor
//! when available; it never claims page-accurate output when the PDF does not
//! expose boundaries.

use std::path::Path;

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tooling::fs::{sha256_file, utf8_line_count};
use crate::tools::{AgentTool, ToolContext, ToolOutput, safe_existing_path, truncate};

const MAX_PDF_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PDF_CHARS: usize = 2_000_000;

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
        let text =
            match tokio::task::spawn_blocking(move || pdf_extract::extract_text(extraction_path))
                .await
            {
                Ok(Ok(text)) => text,
                Ok(Err(error)) => {
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
        let has_page_boundaries = pages.len() > 1;
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
        let page_count = if has_page_boundaries { pages.len() } else { 1 };
        let summary = format!(
            "{} · {} page boundary(ies) · {} bytes extracted · {} lines",
            relative,
            page_count,
            text.len(),
            utf8_line_count(&text)
        );
        Ok(ToolOutput::ok_structured(
            truncate(format!("{summary}\n{visible}")),
            json!({
                "path": relative,
                "size_bytes": metadata.len(),
                "sha256": byte_hash,
                "page_count": page_count,
                "page_boundaries_available": has_page_boundaries,
                "pages_requested": page_selection,
                "extracted_bytes": text.len(),
                "extracted_lines": utf8_line_count(&text),
                "text": visible,
                "truncated": visible.ends_with("… content truncated"),
                "warnings": if page_selection.is_some() && !has_page_boundaries {
                    vec!["page boundaries were not available; the full text was returned"]
                } else {
                    Vec::<&str>::new()
                },
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
        let mut ocr_text = None;
        let mut ocr_engine = None;
        if ocr {
            if which("tesseract").is_none() {
                warnings.push("tesseract is not installed; OCR skipped".to_string());
            } else {
                let command = format!(
                    "tesseract {} stdout --psm 6",
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
                        let mut chars = captured.stdout.chars();
                        let mut text = chars.by_ref().take(max_chars).collect::<String>();
                        if chars.next().is_some() {
                            text.push_str("\n… OCR text truncated");
                        }
                        ocr_text = Some(text);
                        ocr_engine = Some("tesseract");
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
        let hash = sha256_file(&path)?;
        let mime = crate::tooling::files::mime_for_path(&path);
        let summary = format!(
            "{} · {}x{} · {} · {} bytes{}",
            relative,
            width,
            height,
            format,
            metadata.len(),
            ocr_text
                .as_ref()
                .map(|text| format!(" · {} OCR chars", text.chars().count()))
                .unwrap_or_default()
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
                "ocr_engine": ocr_engine,
                "ocr_text": ocr_text,
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
        doc.trailer.set("Root", catalog_id);
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
    }
}
