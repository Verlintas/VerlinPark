//! Bounded Office Open XML and OpenDocument text extraction.
//!
//! Office files are ZIP containers. This module never extracts archive members
//! to disk: it validates every entry name/size, then reads only the XML parts
//! needed for text extraction into bounded memory buffers. XML is treated as
//! untrusted input: DTDs are rejected, entity references are resolved only for
//! XML's built-in/predefined entities and numeric character references, and
//! every text/table/sheet/slide collection has a hard cap.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::{Component, Path};

use anyhow::Result;
use async_trait::async_trait;
use quick_xml::events::{BytesEnd, BytesStart, Event};
use quick_xml::reader::Reader;
use serde_json::{Map, Value, json};
use zip::ZipArchive;

use crate::tooling::fs::{sha256_file, utf8_line_count};
use crate::tools::{AgentTool, ToolContext, ToolOutput, safe_existing_path, truncate};

/// A larger container is rejected before the ZIP central directory is parsed.
const MAX_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
/// Declared uncompressed size of one archive member.
const MAX_ENTRY_UNCOMPRESSED_BYTES: u64 = 64 * 1024 * 1024;
/// Declared uncompressed size of the whole archive. This is intentionally
/// tighter than the ZIP format allows so a small compressed bomb cannot force
/// the daemon to inflate gigabytes even when only a few parts are read.
const MAX_TOTAL_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 10_000;
const MAX_XML_DEPTH: usize = 512;

const MAX_EXTRACTED_CHARS: usize = 1_000_000;
const MAX_OUTPUT_CHARS: usize = 200_000;
const DEFAULT_OUTPUT_CHARS: usize = 20_000;
const MAX_BLOCK_BYTES: usize = 200_000;
const MAX_CELL_BYTES: usize = 20_000;
const MAX_BLOCKS: usize = 20_000;
const MAX_TABLE_ROWS: usize = 2_000;
const MAX_TABLE_COLS: usize = 100;
const MAX_ROW_REPEAT_EXPANSION: usize = 8;
const MAX_SLIDES: usize = 1_000;
const MAX_SLIDE_PARAGRAPHS: usize = 2_000;
const MAX_SHEETS: usize = 200;
const MAX_SHARED_STRINGS: usize = 200_000;
const MAX_SHARED_STRING_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOTAL_SHEET_CELLS: usize = 10_000;
const MAX_WARNINGS: usize = 40;

#[derive(Debug, Clone)]
struct DocumentError {
    code: &'static str,
    message: String,
}

impl DocumentError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn malformed_xml(part: &str, error: impl std::fmt::Display) -> Self {
        Self::new("MALFORMED_XML", format!("{part} is not valid XML: {error}"))
    }
}

impl std::fmt::Display for DocumentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for DocumentError {}

type DocResult<T> = std::result::Result<T, DocumentError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DocumentFormat {
    Docx,
    Pptx,
    Xlsx,
    Odt,
    Ods,
    Odp,
}

impl DocumentFormat {
    fn from_path(path: &Path) -> Option<Self> {
        match path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("docx") => Some(Self::Docx),
            Some("pptx") => Some(Self::Pptx),
            Some("xlsx") => Some(Self::Xlsx),
            Some("odt") => Some(Self::Odt),
            Some("ods") => Some(Self::Ods),
            Some("odp") => Some(Self::Odp),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Docx => "docx",
            Self::Pptx => "pptx",
            Self::Xlsx => "xlsx",
            Self::Odt => "odt",
            Self::Ods => "ods",
            Self::Odp => "odp",
        }
    }

    fn is_presentation(self) -> bool {
        matches!(self, Self::Pptx | Self::Odp)
    }

    fn is_spreadsheet(self) -> bool {
        matches!(self, Self::Xlsx | Self::Ods)
    }
}

#[derive(Clone, Debug)]
struct ExtractionOptions {
    max_chars: usize,
    pages: Option<Vec<usize>>,
    sheets: Option<Vec<String>>,
    include_tables: bool,
    max_rows: usize,
    max_cols: usize,
}

#[derive(Clone, Copy, Debug)]
struct ZipLimits {
    max_entries: usize,
    max_entry_bytes: u64,
    max_total_bytes: u64,
}

impl Default for ZipLimits {
    fn default() -> Self {
        Self {
            max_entries: MAX_ARCHIVE_ENTRIES,
            max_entry_bytes: MAX_ENTRY_UNCOMPRESSED_BYTES,
            max_total_bytes: MAX_TOTAL_UNCOMPRESSED_BYTES,
        }
    }
}

#[derive(Debug)]
struct ArchiveIndex {
    by_name: BTreeMap<String, usize>,
    entry_count: usize,
}

impl ArchiveIndex {
    fn contains(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    fn get(&self, name: &str) -> Option<usize> {
        self.by_name.get(name).copied()
    }

    fn names(&self) -> impl Iterator<Item = &str> {
        self.by_name.keys().map(String::as_str)
    }
}

#[derive(Clone, Debug)]
enum DocumentBlock {
    Heading {
        level: u8,
        text: String,
    },
    Paragraph {
        text: String,
    },
    Table {
        rows: Vec<Vec<String>>,
        truncated: bool,
    },
}

#[derive(Clone, Debug)]
struct Slide {
    number: usize,
    name: Option<String>,
    part: String,
    paragraphs: Vec<String>,
    truncated: bool,
}

impl Slide {
    fn title(&self) -> Option<String> {
        self.paragraphs
            .first()
            .map(|paragraph| paragraph.chars().take(200).collect::<String>())
    }

    fn text(&self) -> String {
        self.paragraphs.join("\n")
    }
}

#[derive(Clone, Debug)]
struct Sheet {
    name: String,
    rows: Vec<Vec<String>>,
    truncated: bool,
}

#[derive(Clone, Debug)]
struct SlideSet {
    total: usize,
    slides: Vec<Slide>,
    truncated: bool,
}

#[derive(Clone, Debug)]
struct SheetSet {
    all_names: Vec<String>,
    sheets: Vec<Sheet>,
    truncated: bool,
}

#[derive(Clone, Debug)]
enum ExtractedKind {
    Blocks {
        blocks: Vec<DocumentBlock>,
        truncated: bool,
    },
    Slides(SlideSet),
    Sheets(SheetSet),
}

#[derive(Clone, Debug)]
struct ExtractedDocument {
    kind: ExtractedKind,
    archive_entries: usize,
    extracted_chars: usize,
    truncated: bool,
    warnings: Vec<String>,
}

struct ExtractionOutcome {
    document: ExtractedDocument,
    sha256: String,
}

pub struct ReadDocument;

#[async_trait]
impl AgentTool for ReadDocument {
    fn name(&self) -> &'static str {
        "read_document"
    }

    fn description(&self) -> &'static str {
        "Extract bounded text, tables, slides, and sheet rows from a DOCX, PPTX, XLSX, ODT, ODS, or ODP file in the worktree. Archive members are never written to disk."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Document path relative to the worktree root"},
                "max_chars": {"type": "integer", "minimum": 100, "maximum": 200000, "description": "Returned document text cap; default 20000"},
                "pages": {"type": "string", "description": "Optional 1-based slide range for PPTX/ODP, e.g. 1-3,5; ignored for other formats"},
                "sheets": {"type": "string", "description": "Optional comma-separated sheet names for XLSX/ODS; default all sheets"},
                "include_tables": {"type": "boolean", "description": "Include DOCX/ODT table rows; default true"},
                "max_rows": {"type": "integer", "minimum": 1, "maximum": 2000, "description": "Per-sheet row cap for XLSX/ODS; default 200"},
                "max_cols": {"type": "integer", "minimum": 1, "maximum": 200, "description": "Per-sheet column cap for XLSX/ODS; default 40"}
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let relative = input
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("path is required"))?;
        let path = safe_existing_path(&context.root, relative)?;
        if !path.is_file() {
            return Ok(ToolOutput::error_structured(
                "read_document path must be a file",
                json!({"code": "INVALID_ARGUMENT", "path": relative}),
            ));
        }
        let Some(format) = DocumentFormat::from_path(&path) else {
            return Ok(ToolOutput::error_structured(
                "unsupported document extension; read_document supports .docx, .pptx, .xlsx, .odt, .ods, and .odp",
                json!({"code": "UNSUPPORTED_FORMAT", "path": relative}),
            ));
        };
        let metadata = std::fs::metadata(&path)?;
        if metadata.len() > MAX_ARCHIVE_BYTES {
            return Ok(ToolOutput::error_structured(
                format!(
                    "document exceeds the {} MiB limit",
                    MAX_ARCHIVE_BYTES / 1024 / 1024
                ),
                json!({
                    "code": "FILE_TOO_LARGE",
                    "path": relative,
                    "size_bytes": metadata.len(),
                    "limit_bytes": MAX_ARCHIVE_BYTES,
                }),
            ));
        }
        let max_chars = input
            .get("max_chars")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_OUTPUT_CHARS as u64)
            .clamp(100, MAX_OUTPUT_CHARS as u64) as usize;
        let pages = match input.get("pages").and_then(Value::as_str) {
            Some(value) => match parse_page_selection(value) {
                Ok(pages) => Some(pages),
                Err(error) => {
                    return Ok(ToolOutput::error_structured(
                        error,
                        json!({"code": "INVALID_ARGUMENT", "pages": value}),
                    ));
                }
            },
            None => None,
        };
        let sheets = match string_list(&input, "sheets") {
            Ok(sheets) => sheets,
            Err(error) => {
                return Ok(ToolOutput::error_structured(
                    error,
                    json!({"code": "INVALID_ARGUMENT", "sheets": input.get("sheets").cloned()}),
                ));
            }
        };
        let options = ExtractionOptions {
            max_chars,
            pages,
            sheets,
            include_tables: input
                .get("include_tables")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            max_rows: input
                .get("max_rows")
                .and_then(Value::as_u64)
                .unwrap_or(200)
                .clamp(1, MAX_TABLE_ROWS as u64) as usize,
            max_cols: input
                .get("max_cols")
                .and_then(Value::as_u64)
                .unwrap_or(40)
                .clamp(1, 200) as usize,
        };
        let extraction_path = path.clone();
        let extraction_options = options.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            extract_document(&extraction_path, format, &extraction_options)
        })
        .await
        .map_err(|error| anyhow::anyhow!("document extraction task failed: {error}"))?;
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                return Ok(ToolOutput::error_structured(
                    error.message,
                    json!({
                        "code": error.code,
                        "path": relative,
                        "format": format.as_str(),
                    }),
                ));
            }
        };
        Ok(render_result(
            relative,
            format,
            metadata.len(),
            &outcome,
            &options,
        ))
    }
}

fn render_result(
    relative: &str,
    format: DocumentFormat,
    size_bytes: u64,
    outcome: &ExtractionOutcome,
    options: &ExtractionOptions,
) -> ToolOutput {
    let document = &outcome.document;
    let body_text = render_document_body(document, options);
    let mut warnings = document.warnings.clone();
    if options.pages.is_some() && !format.is_presentation() {
        warnings.push(format!(
            "pages was ignored because {} has no slide numbering",
            format.as_str()
        ));
    }
    if options.sheets.is_some() && !format.is_spreadsheet() {
        warnings.push(format!(
            "sheets was ignored because {} has no sheet numbering",
            format.as_str()
        ));
    }
    let warning_block = if warnings.is_empty() {
        String::new()
    } else {
        let mut rendered = String::from("warnings:\n");
        for warning in &warnings {
            let _ = writeln!(rendered, "- {warning}");
        }
        rendered
    };
    let available_body_chars = options
        .max_chars
        .saturating_sub(warning_block.chars().count());
    let (mut body, body_truncated) = truncate_chars(&body_text, available_body_chars);
    if body_truncated {
        body.push_str("\n… document text truncated");
    }
    let extracted_chars = document.extracted_chars;
    let chars = body.chars().count();
    let truncated = document.truncated || body_truncated;
    let entries = match &document.kind {
        ExtractedKind::Blocks { blocks, .. } => blocks.len(),
        ExtractedKind::Slides(slides) => slides.slides.len(),
        ExtractedKind::Sheets(sheets) => sheets.sheets.len(),
    };
    let summary = match &document.kind {
        ExtractedKind::Blocks { blocks, .. } => {
            let tables = blocks
                .iter()
                .filter(|block| matches!(block, DocumentBlock::Table { .. }))
                .count();
            let paragraphs = blocks.len().saturating_sub(tables);
            format!(
                "{relative} · {} · {paragraphs} paragraph(s), {tables} table(s) · {extracted_chars} chars{}{}",
                format.as_str(),
                if document.truncated {
                    " · extraction capped"
                } else {
                    ""
                },
                if body_truncated {
                    " · output truncated"
                } else {
                    ""
                }
            )
        }
        ExtractedKind::Slides(slides) => format!(
            "{relative} · {} · {}/{} slide(s) · {extracted_chars} chars{}{}",
            format.as_str(),
            slides.slides.len(),
            slides.total,
            if document.truncated {
                " · extraction capped"
            } else {
                ""
            },
            if body_truncated {
                " · output truncated"
            } else {
                ""
            }
        ),
        ExtractedKind::Sheets(sheets) => format!(
            "{relative} · {} · {}/{} sheet(s) · {extracted_chars} chars{}{}",
            format.as_str(),
            sheets.sheets.len(),
            sheets.all_names.len(),
            if document.truncated {
                " · extraction capped"
            } else {
                ""
            },
            if body_truncated {
                " · output truncated"
            } else {
                ""
            }
        ),
    };
    let content = truncate(format!("{summary}\n{warning_block}{body}"));
    let truncated = truncated || content.ends_with("output truncated");
    let mut structured = Map::new();
    structured.insert("path".to_string(), json!(relative));
    structured.insert("format".to_string(), json!(format.as_str()));
    structured.insert("size_bytes".to_string(), json!(size_bytes));
    structured.insert("sha256".to_string(), json!(outcome.sha256.clone()));
    structured.insert("entries".to_string(), json!(entries));
    structured.insert(
        "archive_entries".to_string(),
        json!(document.archive_entries),
    );
    structured.insert("chars".to_string(), json!(chars));
    structured.insert("extracted_chars".to_string(), json!(extracted_chars));
    structured.insert("lines".to_string(), json!(utf8_line_count(&body)));
    structured.insert("truncated".to_string(), json!(truncated));
    structured.insert("warnings".to_string(), json!(&warnings));
    structured.insert("max_chars".to_string(), json!(options.max_chars));
    match &document.kind {
        ExtractedKind::Blocks { blocks, .. } => {
            let tables = blocks
                .iter()
                .filter_map(|block| match block {
                    DocumentBlock::Table { rows, truncated } => Some(if options.include_tables {
                        json!({
                            "rows": rows,
                            "row_count": rows.len(),
                            "included": true,
                            "truncated": truncated,
                        })
                    } else {
                        json!({
                            "row_count": rows.len(),
                            "included": false,
                            "truncated": truncated,
                        })
                    }),
                    _ => None,
                })
                .collect::<Vec<_>>();
            structured.insert(
                "paragraphs".to_string(),
                json!(
                    blocks
                        .iter()
                        .filter(|block| !matches!(block, DocumentBlock::Table { .. }))
                        .count()
                ),
            );
            structured.insert("tables".to_string(), Value::Array(tables));
            structured.insert("include_tables".to_string(), json!(options.include_tables));
        }
        ExtractedKind::Slides(slides) => {
            structured.insert("slide_count".to_string(), json!(slides.total));
            structured.insert("page_count".to_string(), json!(slides.total));
            structured.insert("pages_requested".to_string(), json!(options.pages));
            structured.insert(
                "slides".to_string(),
                json!(
                    slides
                        .slides
                        .iter()
                        .map(|slide| json!({
                            "number": slide.number,
                            "name": slide.name,
                            "part": slide.part,
                            "title": slide.title(),
                            "text": slide.text(),
                            "chars": slide.text().chars().count(),
                            "truncated": slide.truncated,
                        }))
                        .collect::<Vec<_>>()
                ),
            );
        }
        ExtractedKind::Sheets(sheets) => {
            structured.insert("sheet_names".to_string(), json!(sheets.all_names));
            structured.insert("sheets_requested".to_string(), json!(options.sheets));
            structured.insert(
                "sheets".to_string(),
                json!(
                    sheets
                        .sheets
                        .iter()
                        .map(|sheet| json!({
                            "name": sheet.name,
                            "rows": sheet.rows,
                            "row_count": sheet.rows.len(),
                            "column_count": sheet.rows.iter().map(Vec::len).max().unwrap_or(0),
                            "truncated": sheet.truncated,
                        }))
                        .collect::<Vec<_>>()
                ),
            );
        }
    }
    ToolOutput::ok_structured(content, Value::Object(structured))
}

fn render_document_body(document: &ExtractedDocument, options: &ExtractionOptions) -> String {
    let mut output = String::new();
    match &document.kind {
        ExtractedKind::Blocks { blocks, .. } => {
            if blocks.is_empty() {
                output.push_str("(no extractable text)\n");
            }
            let mut table_index = 0usize;
            for block in blocks {
                match block {
                    DocumentBlock::Heading { level, text } => {
                        let _ = writeln!(
                            output,
                            "{} {}",
                            "#".repeat((*level).clamp(1, 6) as usize),
                            text
                        );
                    }
                    DocumentBlock::Paragraph { text } => {
                        let _ = writeln!(output, "{text}");
                    }
                    DocumentBlock::Table { rows, truncated } => {
                        table_index += 1;
                        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
                        if !options.include_tables {
                            let _ = writeln!(
                                output,
                                "[table {table_index}: {} rows x {columns} cols omitted]",
                                rows.len()
                            );
                            continue;
                        }
                        let _ = writeln!(
                            output,
                            "[table {table_index}: {} rows x {columns} cols{}]",
                            rows.len(),
                            if *truncated { " · truncated" } else { "" }
                        );
                        for row in rows {
                            let cells = row
                                .iter()
                                .map(|cell| cell.replace('\n', " / "))
                                .collect::<Vec<_>>()
                                .join(" | ");
                            let _ = writeln!(output, "| {cells} |");
                        }
                    }
                }
            }
        }
        ExtractedKind::Slides(slides) => {
            if slides.slides.is_empty() {
                output.push_str("(no extractable slide text)\n");
            }
            for slide in &slides.slides {
                let name = slide
                    .name
                    .as_ref()
                    .map(|name| format!(" · {name}"))
                    .unwrap_or_default();
                let _ = writeln!(output, "[slide {}{name}]", slide.number);
                let text = slide.text();
                if text.is_empty() {
                    output.push_str("(no text)\n");
                } else {
                    let _ = writeln!(output, "{text}");
                }
                if slide.truncated {
                    output.push_str("(slide text truncated)\n");
                }
            }
        }
        ExtractedKind::Sheets(sheets) => {
            if sheets.sheets.is_empty() {
                output.push_str("(no extractable sheet data)\n");
            }
            for (index, sheet) in sheets.sheets.iter().enumerate() {
                let columns = sheet.rows.iter().map(Vec::len).max().unwrap_or(0);
                let _ = writeln!(
                    output,
                    "[sheet {}: {} · {} rows x {columns} cols{}]",
                    index + 1,
                    sheet.name,
                    sheet.rows.len(),
                    if sheet.truncated { " · truncated" } else { "" }
                );
                for row in &sheet.rows {
                    let _ = writeln!(output, "{}", row.join(" | "));
                }
            }
        }
    }
    output
}

fn extract_document(
    path: &Path,
    format: DocumentFormat,
    options: &ExtractionOptions,
) -> DocResult<ExtractionOutcome> {
    let file = File::open(path).map_err(|error| {
        DocumentError::new(
            "DOCUMENT_READ_FAILED",
            format!("opening {} failed: {error}", path.display()),
        )
    })?;
    let mut archive = ZipArchive::new(file).map_err(|error| {
        DocumentError::new(
            "MALFORMED_ARCHIVE",
            format!("document is not a readable ZIP archive: {error}"),
        )
    })?;
    let limits = ZipLimits::default();
    let index = validate_archive(&mut archive, limits)?;
    let mut warnings = Vec::new();
    let kind = match format {
        DocumentFormat::Docx => {
            let (blocks, truncated) =
                extract_docx(&mut archive, &index, limits, options, &mut warnings)?;
            ExtractedKind::Blocks { blocks, truncated }
        }
        DocumentFormat::Odt => {
            let (blocks, truncated) =
                extract_odt(&mut archive, &index, limits, options, &mut warnings)?;
            ExtractedKind::Blocks { blocks, truncated }
        }
        DocumentFormat::Pptx => ExtractedKind::Slides(extract_pptx(
            &mut archive,
            &index,
            limits,
            options,
            &mut warnings,
        )?),
        DocumentFormat::Odp => ExtractedKind::Slides(extract_odp(
            &mut archive,
            &index,
            limits,
            options,
            &mut warnings,
        )?),
        DocumentFormat::Xlsx => ExtractedKind::Sheets(extract_xlsx(
            &mut archive,
            &index,
            limits,
            options,
            &mut warnings,
        )?),
        DocumentFormat::Ods => ExtractedKind::Sheets(extract_ods(
            &mut archive,
            &index,
            limits,
            options,
            &mut warnings,
        )?),
    };
    let truncated = match &kind {
        ExtractedKind::Blocks { truncated, .. } => *truncated,
        ExtractedKind::Slides(slides) => {
            slides.truncated || slides.slides.iter().any(|slide| slide.truncated)
        }
        ExtractedKind::Sheets(sheets) => {
            sheets.truncated || sheets.sheets.iter().any(|sheet| sheet.truncated)
        }
    };
    let extracted_chars = match &kind {
        ExtractedKind::Blocks { blocks, .. } => blocks
            .iter()
            .map(|block| match block {
                DocumentBlock::Heading { text, .. } | DocumentBlock::Paragraph { text } => {
                    text.chars().count()
                }
                DocumentBlock::Table { rows, .. } => rows
                    .iter()
                    .flat_map(|row| row.iter())
                    .map(|cell| cell.chars().count())
                    .sum::<usize>(),
            })
            .sum(),
        ExtractedKind::Slides(slides) => slides
            .slides
            .iter()
            .map(|slide| slide.text().chars().count())
            .sum(),
        ExtractedKind::Sheets(sheets) => sheets
            .sheets
            .iter()
            .flat_map(|sheet| sheet.rows.iter())
            .flat_map(|row| row.iter())
            .map(|cell| cell.chars().count())
            .sum(),
    };
    let sha256 = sha256_file(path).map_err(|error| {
        DocumentError::new(
            "DOCUMENT_READ_FAILED",
            format!("hashing {} failed: {error}", path.display()),
        )
    })?;
    Ok(ExtractionOutcome {
        document: ExtractedDocument {
            kind,
            archive_entries: index.entry_count,
            extracted_chars,
            truncated,
            warnings,
        },
        sha256,
    })
}

fn validate_archive<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    limits: ZipLimits,
) -> DocResult<ArchiveIndex> {
    let entry_count = archive.len();
    if entry_count > limits.max_entries {
        return Err(DocumentError::new(
            "ZIP_ENTRY_LIMIT",
            format!(
                "archive has {entry_count} entries; the limit is {}",
                limits.max_entries
            ),
        ));
    }
    let mut by_name = BTreeMap::new();
    let mut total_uncompressed = 0u64;
    for index in 0..entry_count {
        let entry = archive.by_index(index).map_err(|error| {
            DocumentError::new(
                "MALFORMED_ARCHIVE",
                format!("reading archive entry {index} failed: {error}"),
            )
        })?;
        if entry.encrypted() {
            return Err(DocumentError::new(
                "ENCRYPTED_ARCHIVE",
                "encrypted office documents are not supported",
            ));
        }
        let name = entry.name().to_string();
        if name.is_empty() || name.contains('\0') || name.contains('\\') {
            return Err(DocumentError::new(
                "ZIP_UNSAFE_ENTRY",
                format!("archive contains an unsafe path: {name:?}"),
            ));
        }
        if entry.enclosed_name().is_none() {
            return Err(DocumentError::new(
                "ZIP_PATH_TRAVERSAL",
                format!("archive member escapes the archive root: {name}"),
            ));
        }
        if Path::new(&name).components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(DocumentError::new(
                "ZIP_PATH_TRAVERSAL",
                format!("archive member escapes the archive root: {name}"),
            ));
        }
        if entry.is_dir() {
            continue;
        }
        let size = entry.size();
        if size > limits.max_entry_bytes {
            return Err(DocumentError::new(
                "ZIP_ENTRY_TOO_LARGE",
                format!(
                    "archive member {name} declares {size} uncompressed bytes; the per-entry limit is {}",
                    limits.max_entry_bytes
                ),
            ));
        }
        total_uncompressed = total_uncompressed.saturating_add(size);
        if total_uncompressed > limits.max_total_bytes {
            return Err(DocumentError::new(
                "ZIP_TOTAL_TOO_LARGE",
                format!(
                    "archive declares more than {} uncompressed bytes",
                    limits.max_total_bytes
                ),
            ));
        }
        if by_name.insert(name.clone(), index).is_some() {
            return Err(DocumentError::new(
                "DUPLICATE_ZIP_ENTRY",
                format!("archive contains duplicate member name {name}"),
            ));
        }
    }
    Ok(ArchiveIndex {
        by_name,
        entry_count,
    })
}

fn read_entry<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    index: usize,
    name: &str,
    limits: ZipLimits,
) -> DocResult<Vec<u8>> {
    let entry = archive.by_index(index).map_err(|error| {
        DocumentError::new(
            "MALFORMED_ARCHIVE",
            format!("opening archive member {name} failed: {error}"),
        )
    })?;
    if entry.encrypted() {
        return Err(DocumentError::new(
            "ENCRYPTED_ARCHIVE",
            format!("archive member {name} is encrypted"),
        ));
    }
    if entry.size() > limits.max_entry_bytes {
        return Err(DocumentError::new(
            "ZIP_ENTRY_TOO_LARGE",
            format!("archive member {name} exceeds the per-entry size limit"),
        ));
    }
    let mut buffer = Vec::with_capacity(entry.size().min(4 * 1024 * 1024) as usize);
    let mut limited = entry.take(limits.max_entry_bytes.saturating_add(1));
    limited.read_to_end(&mut buffer).map_err(|error| {
        DocumentError::new(
            "MALFORMED_ARCHIVE",
            format!("reading archive member {name} failed: {error}"),
        )
    })?;
    if buffer.len() as u64 > limits.max_entry_bytes {
        return Err(DocumentError::new(
            "ZIP_ENTRY_TOO_LARGE",
            format!("archive member {name} inflated beyond the per-entry size limit"),
        ));
    }
    Ok(buffer)
}

fn read_xml_part<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    index: &ArchiveIndex,
    limits: ZipLimits,
    name: &str,
) -> DocResult<String> {
    let Some(entry_index) = index.get(name) else {
        return Err(DocumentError::new(
            "MISSING_DOCUMENT_PART",
            format!("required document part {name} is missing"),
        ));
    };
    let bytes = read_entry(archive, entry_index, name, limits)?;
    decode_xml_bytes(name, &bytes)
}

fn decode_xml_bytes(name: &str, bytes: &[u8]) -> DocResult<String> {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return decode_utf16(name, rest, true);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return decode_utf16(name, rest, false);
    }
    String::from_utf8(bytes.to_vec()).map_err(|_| {
        DocumentError::new(
            "UNSUPPORTED_ENCODING",
            format!("{name} is not UTF-8 or BOM-marked UTF-16 XML"),
        )
    })
}

fn decode_utf16(name: &str, bytes: &[u8], little_endian: bool) -> DocResult<String> {
    if !bytes.len().is_multiple_of(2) {
        return Err(DocumentError::new(
            "UNSUPPORTED_ENCODING",
            format!("{name} has an odd number of UTF-16 bytes"),
        ));
    }
    let units = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            if little_endian {
                u16::from_le_bytes(*pair)
            } else {
                u16::from_be_bytes(*pair)
            }
        })
        .collect::<Vec<_>>();
    String::from_utf16(&units).map_err(|_| {
        DocumentError::new(
            "UNSUPPORTED_ENCODING",
            format!("{name} contains invalid UTF-16"),
        )
    })
}

fn check_xml_eof(depth: usize, part: &str) -> DocResult<()> {
    if depth != 0 {
        return Err(DocumentError::new(
            "MALFORMED_XML",
            format!("{part} ended with {depth} unclosed element(s)"),
        ));
    }
    Ok(())
}

fn xml_reader(xml: &str) -> Reader<&[u8]> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    reader
}

fn check_depth(depth: &mut usize, part: &str) -> DocResult<()> {
    *depth += 1;
    if *depth > MAX_XML_DEPTH {
        return Err(DocumentError::new(
            "MALFORMED_XML",
            format!("{part} nests deeper than {MAX_XML_DEPTH} elements"),
        ));
    }
    Ok(())
}

fn decoded_event_text(event: &Event<'_>, warnings: &mut Vec<String>) -> Option<String> {
    match event {
        Event::Text(text) => Some(text.clone().into_inner().into_owned()),
        Event::CData(text) => Some(text.clone().into_inner().into_owned()),
        Event::GeneralRef(reference) => {
            if let Ok(Some(character)) = reference.resolve_char_ref() {
                return Some(character.to_string());
            }
            let name = reference.clone().into_inner();
            let resolved = match name.as_ref() {
                "amp" => "&",
                "lt" => "<",
                "gt" => ">",
                "quot" => "\"",
                "apos" => "'",
                other => {
                    warn(
                        warnings,
                        format!("unresolved XML entity &{other}; rendered literally"),
                    );
                    return Some(format!("&{other};"));
                }
            };
            Some(resolved.to_string())
        }
        _ => None,
    }
}

fn local_name<'a>(element: &'a BytesStart<'_>) -> &'a [u8] {
    element.local_name().into_inner().as_bytes()
}

fn end_name<'a>(element: &'a BytesEnd<'_>) -> &'a [u8] {
    element.local_name().into_inner().as_bytes()
}

fn attribute(element: &BytesStart<'_>, local: &[u8]) -> Option<String> {
    for attribute in element.attributes().with_checks(false).flatten() {
        if attribute.key.local_name().as_ref().as_bytes() == local
            && let Ok(value) = attribute.normalized_value(quick_xml::XmlVersion::Implicit1_0)
        {
            return Some(value.into_owned());
        }
    }
    None
}

fn qualified_attribute(element: &BytesStart<'_>, name: &[u8]) -> Option<String> {
    for attribute in element.attributes().with_checks(false).flatten() {
        if attribute.key.as_ref().as_bytes() == name
            && let Ok(value) = attribute.normalized_value(quick_xml::XmlVersion::Implicit1_0)
        {
            return Some(value.into_owned());
        }
    }
    None
}

fn relationship_id(element: &BytesStart<'_>) -> Option<String> {
    if let Some(value) = qualified_attribute(element, b"r:id") {
        return Some(value);
    }
    for attribute in element.attributes().with_checks(false).flatten() {
        if attribute.key.local_name().as_ref().as_bytes() == b"id"
            && attribute.key.prefix().is_some()
            && let Ok(value) = attribute.normalized_value(quick_xml::XmlVersion::Implicit1_0)
        {
            return Some(value.into_owned());
        }
    }
    None
}

fn warn(warnings: &mut Vec<String>, message: impl Into<String>) {
    if warnings.len() >= MAX_WARNINGS {
        return;
    }
    let message = message.into();
    if !warnings.contains(&message) {
        warnings.push(message);
    }
}

fn push_capped(target: &mut String, text: &str, limit: usize) -> bool {
    if text.is_empty() {
        return true;
    }
    if target.len() >= limit {
        return false;
    }
    let remaining = limit - target.len();
    if text.len() <= remaining {
        target.push_str(text);
        return true;
    }
    let mut end = 0usize;
    for (index, character) in text.char_indices() {
        if index + character.len_utf8() > remaining {
            break;
        }
        end = index + character.len_utf8();
    }
    target.push_str(&text[..end]);
    false
}

fn normalize_inline(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_lines(text: &str) -> String {
    text.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

struct TextBudget {
    remaining: usize,
    truncated: bool,
}

impl TextBudget {
    fn new(limit: usize) -> Self {
        Self {
            remaining: limit,
            truncated: false,
        }
    }

    fn take(&mut self, value: &str, field_limit: usize) -> String {
        let limit = field_limit.min(self.remaining);
        let mut output = String::new();
        for character in value.chars() {
            if output.len() + character.len_utf8() > limit {
                self.truncated = true;
                break;
            }
            output.push(character);
        }
        self.remaining = self.remaining.saturating_sub(output.len());
        output
    }

    fn exhausted(&self) -> bool {
        self.remaining == 0
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> (String, bool) {
    let mut output = String::new();
    for (index, character) in text.chars().enumerate() {
        if index >= max_chars {
            return (output, true);
        }
        output.push(character);
    }
    (output, false)
}

fn parse_page_selection(value: &str) -> std::result::Result<Vec<usize>, String> {
    let mut pages = Vec::new();
    for part in value.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((start, end)) = part.split_once('-') {
            let start = start
                .trim()
                .parse::<usize>()
                .map_err(|_| format!("invalid page range: {part}"))?;
            let end = end
                .trim()
                .parse::<usize>()
                .map_err(|_| format!("invalid page range: {part}"))?;
            if start == 0 || end < start {
                return Err(format!("invalid page range: {part}"));
            }
            if end - start > 500 {
                return Err(format!("page range is limited to 500 pages: {part}"));
            }
            for page in start..=end {
                pages.push(page);
                if pages.len() > 1_000 {
                    return Err("page selection is limited to 1000 pages".to_string());
                }
            }
        } else {
            let page = part
                .parse::<usize>()
                .map_err(|_| format!("invalid page number: {part}"))?;
            if page == 0 {
                return Err("pages are 1-based and must be positive".to_string());
            }
            pages.push(page);
        }
    }
    pages.sort_unstable();
    pages.dedup();
    if pages.is_empty() {
        return Err("pages must contain at least one page or range".to_string());
    }
    Ok(pages)
}

fn string_list(input: &Value, key: &str) -> std::result::Result<Option<Vec<String>>, String> {
    let Some(value) = input.get(key) else {
        return Ok(None);
    };
    let mut values = Vec::new();
    match value {
        Value::String(text) => {
            for part in text.split(',') {
                let part = part.trim();
                if !part.is_empty() {
                    values.push(part.to_string());
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                let Some(item) = item.as_str() else {
                    return Err(format!("{key} array entries must be strings"));
                };
                let item = item.trim();
                if !item.is_empty() {
                    values.push(item.to_string());
                }
            }
        }
        _ => {
            return Err(format!(
                "{key} must be a comma-separated string or string array"
            ));
        }
    }
    values.sort();
    values.dedup();
    if values.is_empty() {
        return Err(format!("{key} must contain at least one name"));
    }
    if values.len() > MAX_SHEETS {
        return Err(format!("{key} is limited to {MAX_SHEETS} names"));
    }
    Ok(Some(values))
}

/* ------------------------------- DOCX ---------------------------------- */

fn extract_docx<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    index: &ArchiveIndex,
    limits: ZipLimits,
    options: &ExtractionOptions,
    warnings: &mut Vec<String>,
) -> DocResult<(Vec<DocumentBlock>, bool)> {
    let xml = read_xml_part(archive, index, limits, "word/document.xml")?;
    parse_docx(&xml, options, warnings)
}

fn parse_docx(
    xml: &str,
    _options: &ExtractionOptions,
    warnings: &mut Vec<String>,
) -> DocResult<(Vec<DocumentBlock>, bool)> {
    let mut reader = xml_reader(xml);
    let mut budget = TextBudget::new(MAX_EXTRACTED_CHARS);
    let mut blocks = Vec::new();
    let mut depth = 0usize;
    let mut in_paragraph = false;
    let mut paragraph_text = String::new();
    let mut heading_level = None;
    let mut in_text = false;
    let mut table_depth = 0usize;
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    let mut table_truncated = false;
    let mut truncated = false;
    let mut current_row: Vec<String> = Vec::new();
    let mut current_cell = String::new();
    let mut in_cell = false;

    'xml: loop {
        let event = reader
            .read_event()
            .map_err(|error| DocumentError::malformed_xml("word/document.xml", error))?;
        if let Some(text) = decoded_event_text(&event, warnings) {
            if in_paragraph && in_text && !push_capped(&mut paragraph_text, &text, MAX_BLOCK_BYTES)
            {
                truncated = true;
            }
            continue;
        }
        match event {
            Event::Start(element) => {
                check_depth(&mut depth, "word/document.xml")?;
                match local_name(&element) {
                    b"p" => {
                        in_paragraph = true;
                        paragraph_text.clear();
                        heading_level = None;
                    }
                    b"pStyle" if in_paragraph => {
                        if let Some(style) = attribute(&element, b"val") {
                            heading_level = heading_level_from_style(&style);
                        }
                    }
                    b"outlineLvl" if in_paragraph => {
                        if let Some(level) =
                            attribute(&element, b"val").and_then(|value| value.parse::<u8>().ok())
                        {
                            heading_level = Some(level.saturating_add(1).clamp(1, 6));
                        }
                    }
                    b"t" if in_paragraph => in_text = true,
                    b"tbl" => {
                        table_depth += 1;
                        if table_depth == 1 {
                            table_rows.clear();
                            table_truncated = false;
                        }
                    }
                    b"tr" if table_depth == 1 => current_row.clear(),
                    b"tc" if table_depth == 1 => {
                        in_cell = true;
                        current_cell.clear();
                    }
                    _ => {}
                }
            }
            Event::Empty(element) => match local_name(&element) {
                b"tab" if in_paragraph => {
                    if !push_capped(&mut paragraph_text, "\t", MAX_BLOCK_BYTES) {
                        truncated = true;
                    }
                }
                b"br" | b"cr" if in_paragraph => {
                    if !push_capped(&mut paragraph_text, "\n", MAX_BLOCK_BYTES) {
                        truncated = true;
                    }
                }
                b"pStyle" if in_paragraph => {
                    if let Some(style) = attribute(&element, b"val") {
                        heading_level = heading_level_from_style(&style);
                    }
                }
                b"outlineLvl" if in_paragraph => {
                    if let Some(level) =
                        attribute(&element, b"val").and_then(|value| value.parse::<u8>().ok())
                    {
                        heading_level = Some(level.saturating_add(1).clamp(1, 6));
                    }
                }
                b"tr" if table_depth == 1 => {
                    if table_rows.len() < MAX_TABLE_ROWS {
                        table_rows.push(Vec::new());
                    } else {
                        table_truncated = true;
                    }
                }
                b"tc" if table_depth == 1 => {
                    if current_row.len() < MAX_TABLE_COLS {
                        current_row.push(String::new());
                    } else {
                        table_truncated = true;
                    }
                }
                _ => {}
            },
            Event::End(element) => {
                depth = depth.saturating_sub(1);
                match end_name(&element) {
                    b"t" => in_text = false,
                    b"p" => {
                        in_paragraph = false;
                        let text = normalize_inline(&paragraph_text);
                        if !text.is_empty() {
                            if table_depth > 0 {
                                if in_cell {
                                    let taken = budget.take(&text, MAX_CELL_BYTES);
                                    if !current_cell.is_empty() {
                                        push_capped(&mut current_cell, "\n", MAX_CELL_BYTES);
                                    }
                                    push_capped(&mut current_cell, &taken, MAX_CELL_BYTES);
                                }
                            } else if blocks.len() < MAX_BLOCKS {
                                let taken = budget.take(&text, MAX_BLOCK_BYTES);
                                if !taken.is_empty() {
                                    blocks.push(match heading_level {
                                        Some(level) => {
                                            DocumentBlock::Heading { level, text: taken }
                                        }
                                        None => DocumentBlock::Paragraph { text: taken },
                                    });
                                }
                            } else {
                                truncated = true;
                                break 'xml;
                            }
                        }
                        paragraph_text.clear();
                        if budget.exhausted() {
                            truncated = true;
                            break 'xml;
                        }
                    }
                    b"tc" if table_depth == 1 => {
                        if in_cell {
                            in_cell = false;
                            let cell = normalize_lines(&current_cell);
                            if current_row.len() < MAX_TABLE_COLS {
                                current_row.push(cell);
                            } else {
                                table_truncated = true;
                            }
                            current_cell.clear();
                        }
                    }
                    b"tr" if table_depth == 1 => {
                        if table_rows.len() < MAX_TABLE_ROWS {
                            table_rows.push(std::mem::take(&mut current_row));
                        } else {
                            table_truncated = true;
                        }
                    }
                    b"tbl" if table_depth > 0 => {
                        table_depth -= 1;
                        if table_depth == 0 {
                            let rows = std::mem::take(&mut table_rows);
                            if !rows.is_empty() && blocks.len() < MAX_BLOCKS {
                                blocks.push(DocumentBlock::Table {
                                    rows,
                                    truncated: table_truncated,
                                });
                            } else if !rows.is_empty() {
                                truncated = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::DocType(_) => {
                return Err(DocumentError::new(
                    "UNSUPPORTED_XML",
                    "DTD declarations are not allowed in office documents",
                ));
            }
            Event::Eof => {
                check_xml_eof(depth, "word/document.xml")?;
                break;
            }
            _ => {}
        }
    }
    let truncated = truncated
        || budget.truncated
        || blocks.iter().any(|block| match block {
            DocumentBlock::Table { truncated, .. } => *truncated,
            _ => false,
        });
    Ok((blocks, truncated))
}

fn heading_level_from_style(style: &str) -> Option<u8> {
    let normalized = style.to_ascii_lowercase();
    if normalized == "title" {
        return Some(1);
    }
    if normalized.starts_with("heading") {
        let digits = normalized
            .chars()
            .filter(char::is_ascii_digit)
            .collect::<String>();
        if let Ok(level) = digits.parse::<u8>() {
            return Some(level.clamp(1, 6));
        }
        return Some(1);
    }
    None
}

/* ------------------------------- PPTX ---------------------------------- */

fn extract_pptx<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    index: &ArchiveIndex,
    limits: ZipLimits,
    options: &ExtractionOptions,
    warnings: &mut Vec<String>,
) -> DocResult<SlideSet> {
    let mut ordered = Vec::new();
    let presentation = index
        .get("ppt/presentation.xml")
        .map(|_| read_xml_part(archive, index, limits, "ppt/presentation.xml"))
        .transpose()
        .ok()
        .flatten();
    let relationships = index
        .get("ppt/_rels/presentation.xml.rels")
        .map(|_| read_xml_part(archive, index, limits, "ppt/_rels/presentation.xml.rels"))
        .transpose()
        .ok()
        .flatten();
    if let (Some(presentation), Some(relationships)) = (&presentation, &relationships) {
        let rels = parse_relationships(relationships, "ppt", warnings)?;
        let slide_ids = parse_presentation_slide_ids(presentation, warnings)?;
        let mut unresolved = 0usize;
        for id in slide_ids {
            match rels.get(&id) {
                Some(target) if target.starts_with("ppt/slides/") && index.contains(target) => {
                    ordered.push(target.clone());
                }
                _ => unresolved += 1,
            }
        }
        if unresolved > 0 {
            warn(
                warnings,
                format!("{unresolved} presentation slide relationship(s) could not be resolved"),
            );
        }
    }
    if ordered.is_empty() {
        let mut fallback = sorted_parts(index, "ppt/slides/", "slide");
        if !fallback.is_empty() {
            warn(
                warnings,
                "presentation order was unavailable; slides are ordered by filename",
            );
        }
        ordered.append(&mut fallback);
    }
    let total = ordered.len().min(MAX_SLIDES);
    let presentation_truncated = ordered.len() > MAX_SLIDES;
    if presentation_truncated {
        warn(
            warnings,
            format!("presentation has more than {MAX_SLIDES} slides; later slides were skipped"),
        );
    }
    let selected = selected_indices(total, options.pages.as_ref(), "slide", warnings);
    if options.pages.is_some() && selected.is_empty() {
        return Err(DocumentError::new(
            "PAGE_NOT_FOUND",
            format!("none of the requested pages exist; presentation has {total} slide(s)"),
        ));
    }
    let mut budget = TextBudget::new(MAX_EXTRACTED_CHARS);
    let mut slides = Vec::new();
    for slide_index in selected {
        let part = ordered[slide_index].clone();
        let number = slide_index + 1;
        let xml = match read_xml_part(archive, index, limits, &part) {
            Ok(xml) => xml,
            Err(error) => {
                warn(warnings, format!("slide {number}: {error}"));
                slides.push(Slide {
                    number,
                    name: None,
                    part,
                    paragraphs: Vec::new(),
                    truncated: false,
                });
                continue;
            }
        };
        match parse_slide_text(&xml, &mut budget, warnings, &part) {
            Ok((paragraphs, truncated)) => slides.push(Slide {
                number,
                name: None,
                part,
                paragraphs,
                truncated,
            }),
            Err(error) => {
                warn(warnings, format!("slide {number}: {error}"));
                slides.push(Slide {
                    number,
                    name: None,
                    part,
                    paragraphs: Vec::new(),
                    truncated: false,
                });
            }
        }
    }
    Ok(SlideSet {
        total,
        slides,
        truncated: presentation_truncated || budget.truncated,
    })
}

fn parse_presentation_slide_ids(xml: &str, warnings: &mut Vec<String>) -> DocResult<Vec<String>> {
    let mut reader = xml_reader(xml);
    let mut depth = 0usize;
    let mut ids = Vec::new();
    loop {
        let event = reader
            .read_event()
            .map_err(|error| DocumentError::malformed_xml("ppt/presentation.xml", error))?;
        match event {
            Event::Start(element) => {
                check_depth(&mut depth, "ppt/presentation.xml")?;
                if local_name(&element) == b"sldId" {
                    if let Some(id) = relationship_id(&element) {
                        ids.push(id);
                    } else {
                        warn(
                            warnings,
                            "a presentation slide entry has no relationship id",
                        );
                    }
                }
            }
            Event::Empty(element) => {
                if local_name(&element) == b"sldId"
                    && let Some(id) = relationship_id(&element)
                {
                    ids.push(id);
                }
            }
            Event::End(_) => depth = depth.saturating_sub(1),
            Event::DocType(_) => {
                return Err(DocumentError::new(
                    "UNSUPPORTED_XML",
                    "DTD declarations are not allowed in office documents",
                ));
            }
            Event::Eof => {
                check_xml_eof(depth, "ppt/presentation.xml")?;
                break;
            }
            _ => {}
        }
    }
    Ok(ids)
}

fn parse_slide_text(
    xml: &str,
    budget: &mut TextBudget,
    warnings: &mut Vec<String>,
    part: &str,
) -> DocResult<(Vec<String>, bool)> {
    let mut reader = xml_reader(xml);
    let mut depth = 0usize;
    let mut in_paragraph = false;
    let mut in_text = false;
    let mut paragraph_text = String::new();
    let mut paragraphs = Vec::new();
    let mut truncated = false;
    loop {
        let event = reader
            .read_event()
            .map_err(|error| DocumentError::malformed_xml(part, error))?;
        if let Some(text) = decoded_event_text(&event, warnings) {
            if in_paragraph && in_text && !push_capped(&mut paragraph_text, &text, MAX_BLOCK_BYTES)
            {
                truncated = true;
            }
            continue;
        }
        match event {
            Event::Start(element) => {
                check_depth(&mut depth, part)?;
                match local_name(&element) {
                    b"p" => {
                        in_paragraph = true;
                        paragraph_text.clear();
                    }
                    b"t" if in_paragraph => in_text = true,
                    _ => {}
                }
            }
            Event::Empty(element) => match local_name(&element) {
                b"br" if in_paragraph => {
                    truncated |= !push_capped(&mut paragraph_text, "\n", MAX_BLOCK_BYTES);
                }
                _ => {}
            },
            Event::End(element) => {
                depth = depth.saturating_sub(1);
                match end_name(&element) {
                    b"t" => in_text = false,
                    b"p" => {
                        in_paragraph = false;
                        let text = normalize_inline(&paragraph_text);
                        if !text.is_empty() {
                            if paragraphs.len() < MAX_SLIDE_PARAGRAPHS {
                                let taken = budget.take(&text, MAX_BLOCK_BYTES);
                                if !taken.is_empty() {
                                    paragraphs.push(taken);
                                }
                            } else {
                                truncated = true;
                                budget.truncated = true;
                            }
                        }
                        paragraph_text.clear();
                        if budget.exhausted() {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            Event::DocType(_) => {
                return Err(DocumentError::new(
                    "UNSUPPORTED_XML",
                    "DTD declarations are not allowed in office documents",
                ));
            }
            Event::Eof => {
                check_xml_eof(depth, part)?;
                break;
            }
            _ => {}
        }
    }
    Ok((paragraphs, truncated || budget.truncated))
}

fn sorted_parts(index: &ArchiveIndex, prefix: &str, stem: &str) -> Vec<String> {
    let mut parts = index
        .names()
        .filter(|name| {
            name.starts_with(prefix)
                && name.ends_with(".xml")
                && !name.contains("/_rels/")
                && name[prefix.len()..]
                    .strip_suffix(".xml")
                    .is_some_and(|name| name.starts_with(stem))
        })
        .map(str::to_string)
        .collect::<Vec<_>>();
    parts.sort_by(|left, right| {
        numeric_suffix(left)
            .cmp(&numeric_suffix(right))
            .then_with(|| left.cmp(right))
    });
    parts
}

fn numeric_suffix(name: &str) -> u64 {
    name.rsplit('/')
        .next()
        .unwrap_or(name)
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or(u64::MAX)
}

fn selected_indices(
    total: usize,
    pages: Option<&Vec<usize>>,
    label: &str,
    warnings: &mut Vec<String>,
) -> Vec<usize> {
    match pages {
        Some(pages) => {
            let mut selected = Vec::new();
            for page in pages {
                if *page == 0 || *page > total {
                    warn(
                        warnings,
                        format!("{label} {page} does not exist; document has {total} {label}(s)"),
                    );
                    continue;
                }
                selected.push(page - 1);
            }
            selected.sort_unstable();
            selected.dedup();
            selected
        }
        None => (0..total).collect(),
    }
}

/* ------------------------------- XLSX ---------------------------------- */

#[derive(Clone, Debug)]
struct WorkbookSheet {
    name: String,
    target: Option<String>,
}

fn extract_xlsx<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    index: &ArchiveIndex,
    limits: ZipLimits,
    options: &ExtractionOptions,
    warnings: &mut Vec<String>,
) -> DocResult<SheetSet> {
    let relationships = index
        .get("xl/_rels/workbook.xml.rels")
        .map(|_| read_xml_part(archive, index, limits, "xl/_rels/workbook.xml.rels"))
        .transpose()?;
    let rels = match relationships {
        Some(xml) => parse_relationships(&xml, "xl", warnings)?,
        None => {
            warn(
                warnings,
                "xl/_rels/workbook.xml.rels is missing; sheet paths were inferred",
            );
            BTreeMap::new()
        }
    };
    let mut workbook_sheets = Vec::new();
    if index.contains("xl/workbook.xml") {
        match read_xml_part(archive, index, limits, "xl/workbook.xml")
            .and_then(|xml| parse_xlsx_workbook(&xml, &rels, warnings))
        {
            Ok(sheets) => workbook_sheets = sheets,
            Err(error) => warn(
                warnings,
                format!("workbook metadata could not be read ({error}); sheet paths were inferred"),
            ),
        }
    } else {
        warn(
            warnings,
            "xl/workbook.xml is missing; sheet paths and names were inferred",
        );
    }
    if workbook_sheets.is_empty() {
        workbook_sheets = inferred_workbook_sheets(index);
    }
    if workbook_sheets.is_empty() {
        return Err(DocumentError::new(
            "MISSING_DOCUMENT_PART",
            "workbook contains no worksheet parts",
        ));
    }
    let workbook_truncated = workbook_sheets.len() > MAX_SHEETS;
    if workbook_truncated {
        warn(
            warnings,
            format!(
                "workbook has {} sheets; only the first {MAX_SHEETS} were considered",
                workbook_sheets.len()
            ),
        );
        workbook_sheets.truncate(MAX_SHEETS);
    }
    let all_names = workbook_sheets
        .iter()
        .map(|sheet| sheet.name.clone())
        .collect::<Vec<_>>();
    let selected = sheet_selection(&all_names, options.sheets.as_ref(), warnings)?;
    let shared_strings = if index.contains("xl/sharedStrings.xml") {
        let xml = read_xml_part(archive, index, limits, "xl/sharedStrings.xml")?;
        parse_shared_strings(&xml, warnings)?
    } else {
        Vec::new()
    };
    let mut budget = TextBudget::new(MAX_EXTRACTED_CHARS);
    let mut sheets = Vec::new();
    let mut total_cells = 0usize;
    let mut total_cells_truncated = false;
    for (sheet_index, workbook_sheet) in workbook_sheets.iter().enumerate() {
        if !selected[sheet_index] {
            continue;
        }
        let target = workbook_sheet
            .target
            .clone()
            .filter(|target| index.contains(target))
            .or_else(|| {
                let fallback = format!("xl/worksheets/sheet{}.xml", sheet_index + 1);
                index.contains(&fallback).then_some(fallback)
            });
        let Some(target) = target else {
            warn(
                warnings,
                format!(
                    "sheet {} ({}) has no readable worksheet part",
                    sheet_index + 1,
                    workbook_sheet.name
                ),
            );
            continue;
        };
        let xml = match read_xml_part(archive, index, limits, &target) {
            Ok(xml) => xml,
            Err(error) => {
                warn(
                    warnings,
                    format!(
                        "sheet {} ({}): {error}",
                        sheet_index + 1,
                        workbook_sheet.name
                    ),
                );
                continue;
            }
        };
        let remaining_cells = MAX_TOTAL_SHEET_CELLS.saturating_sub(total_cells);
        let mut sheet = match parse_xlsx_sheet(
            &xml,
            &XlsxSheetRequest {
                shared_strings: &shared_strings,
                options,
                sheet_name: &workbook_sheet.name,
                part: &target,
                remaining_cells,
            },
            &mut budget,
            warnings,
        ) {
            Ok(sheet) => sheet,
            Err(error) => {
                warn(
                    warnings,
                    format!(
                        "sheet {} ({}): {error}",
                        sheet_index + 1,
                        workbook_sheet.name
                    ),
                );
                continue;
            }
        };
        let cells = sheet
            .rows
            .iter()
            .flat_map(|row| row.iter())
            .filter(|cell| !cell.is_empty())
            .count();
        total_cells += cells;
        if total_cells > MAX_TOTAL_SHEET_CELLS {
            total_cells_truncated = true;
            sheet.truncated = true;
        }
        sheets.push(sheet);
    }
    if sheets.is_empty() {
        return Err(DocumentError::new(
            "SHEET_NOT_FOUND",
            "no selected worksheet could be read",
        ));
    }
    if total_cells_truncated || budget.truncated {
        for sheet in &mut sheets {
            sheet.truncated = true;
        }
    }
    Ok(SheetSet {
        all_names,
        sheets,
        truncated: workbook_truncated || total_cells_truncated || budget.truncated,
    })
}

fn parse_xlsx_workbook(
    xml: &str,
    rels: &BTreeMap<String, String>,
    warnings: &mut Vec<String>,
) -> DocResult<Vec<WorkbookSheet>> {
    let mut reader = xml_reader(xml);
    let mut depth = 0usize;
    let mut sheets = Vec::new();
    loop {
        let event = reader
            .read_event()
            .map_err(|error| DocumentError::malformed_xml("xl/workbook.xml", error))?;
        match event {
            Event::Start(element) => {
                check_depth(&mut depth, "xl/workbook.xml")?;
                parse_workbook_sheet(&element, rels, warnings, &mut sheets);
            }
            Event::Empty(element) => {
                parse_workbook_sheet(&element, rels, warnings, &mut sheets);
            }
            Event::End(_) => depth = depth.saturating_sub(1),
            Event::DocType(_) => {
                return Err(DocumentError::new(
                    "UNSUPPORTED_XML",
                    "DTD declarations are not allowed in office documents",
                ));
            }
            Event::Eof => {
                check_xml_eof(depth, "xl/workbook.xml")?;
                break;
            }
            _ => {}
        }
    }
    Ok(sheets)
}

fn parse_workbook_sheet(
    element: &BytesStart<'_>,
    rels: &BTreeMap<String, String>,
    warnings: &mut Vec<String>,
    sheets: &mut Vec<WorkbookSheet>,
) {
    if local_name(element) != b"sheet" {
        return;
    }
    let name = attribute(element, b"name").unwrap_or_else(|| format!("Sheet{}", sheets.len() + 1));
    let target = relationship_id(element)
        .and_then(|id| rels.get(&id).cloned())
        .filter(|target| target.starts_with("xl/worksheets/"));
    if target.is_none() {
        warn(
            warnings,
            format!("sheet {name} has no worksheet relationship; using a path fallback"),
        );
    }
    sheets.push(WorkbookSheet { name, target });
}

fn inferred_workbook_sheets(index: &ArchiveIndex) -> Vec<WorkbookSheet> {
    sorted_parts(index, "xl/worksheets/", "sheet")
        .into_iter()
        .enumerate()
        .map(|(sheet_index, target)| WorkbookSheet {
            name: format!("Sheet{}", sheet_index + 1),
            target: Some(target),
        })
        .collect()
}

fn parse_relationships(
    xml: &str,
    base_dir: &str,
    warnings: &mut Vec<String>,
) -> DocResult<BTreeMap<String, String>> {
    let mut reader = xml_reader(xml);
    let mut depth = 0usize;
    let mut relationships = BTreeMap::new();
    loop {
        let event = reader
            .read_event()
            .map_err(|error| DocumentError::malformed_xml("relationships XML", error))?;
        match event {
            Event::Start(element) => {
                check_depth(&mut depth, "relationships XML")?;
                parse_relationship(&element, base_dir, warnings, &mut relationships);
            }
            Event::Empty(element) => {
                parse_relationship(&element, base_dir, warnings, &mut relationships);
            }
            Event::End(_) => depth = depth.saturating_sub(1),
            Event::DocType(_) => {
                return Err(DocumentError::new(
                    "UNSUPPORTED_XML",
                    "DTD declarations are not allowed in office documents",
                ));
            }
            Event::Eof => {
                check_xml_eof(depth, "relationships XML")?;
                break;
            }
            _ => {}
        }
    }
    Ok(relationships)
}

fn parse_relationship(
    element: &BytesStart<'_>,
    base_dir: &str,
    warnings: &mut Vec<String>,
    relationships: &mut BTreeMap<String, String>,
) {
    if local_name(element) != b"Relationship" {
        return;
    }
    let Some(id) = attribute(element, b"Id") else {
        return;
    };
    if attribute(element, b"TargetMode").as_deref() == Some("External") {
        return;
    }
    let Some(target) = attribute(element, b"Target") else {
        return;
    };
    match resolve_part(base_dir, &target) {
        Some(target) => {
            relationships.insert(id, target);
        }
        None => warn(
            warnings,
            format!("relationship {id} has an unsafe target: {target}"),
        ),
    }
}

fn resolve_part(base_dir: &str, target: &str) -> Option<String> {
    if target.is_empty() || target.contains('\0') || target.contains('\\') {
        return None;
    }
    let mut parts = if target.starts_with('/') {
        Vec::new()
    } else {
        base_dir
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect::<Vec<_>>()
    };
    for component in target.trim_start_matches('/').split('/') {
        match component {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

fn sheet_selection(
    all_names: &[String],
    requested: Option<&Vec<String>>,
    warnings: &mut Vec<String>,
) -> DocResult<Vec<bool>> {
    let Some(requested) = requested else {
        return Ok(vec![true; all_names.len()]);
    };
    let mut selected = vec![false; all_names.len()];
    for name in requested {
        let matches = all_names
            .iter()
            .enumerate()
            .filter(|(_, candidate)| candidate.eq_ignore_ascii_case(name))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if matches.is_empty() {
            warn(
                warnings,
                format!(
                    "sheet {name} was not found; available sheets: {}",
                    all_names.join(", ")
                ),
            );
        }
        for index in matches {
            selected[index] = true;
        }
    }
    if !selected.iter().any(|value| *value) {
        return Err(DocumentError::new(
            "SHEET_NOT_FOUND",
            format!(
                "none of the requested sheets exist; available sheets: {}",
                all_names.join(", ")
            ),
        ));
    }
    Ok(selected)
}

fn parse_shared_strings(xml: &str, warnings: &mut Vec<String>) -> DocResult<Vec<String>> {
    let mut reader = xml_reader(xml);
    let mut depth = 0usize;
    let mut strings = Vec::new();
    let mut in_si = false;
    let mut in_text = false;
    let mut phonetic_depth = 0usize;
    let mut text = String::new();
    let mut total_bytes = 0usize;
    loop {
        let event = reader
            .read_event()
            .map_err(|error| DocumentError::malformed_xml("xl/sharedStrings.xml", error))?;
        if let Some(piece) = decoded_event_text(&event, warnings) {
            if in_si && in_text && phonetic_depth == 0 {
                push_capped(&mut text, &piece, MAX_CELL_BYTES);
            }
            continue;
        }
        match event {
            Event::Start(element) => {
                check_depth(&mut depth, "xl/sharedStrings.xml")?;
                match local_name(&element) {
                    b"si" => {
                        in_si = true;
                        text.clear();
                    }
                    b"rPh" if in_si => phonetic_depth += 1,
                    b"t" if in_si && phonetic_depth == 0 => in_text = true,
                    _ => {}
                }
            }
            Event::Empty(element) => {
                if local_name(&element) == b"t" && in_si {
                    in_text = false;
                }
            }
            Event::End(element) => {
                depth = depth.saturating_sub(1);
                match end_name(&element) {
                    b"t" => in_text = false,
                    b"rPh" if in_si => phonetic_depth = phonetic_depth.saturating_sub(1),
                    b"si" if in_si => {
                        in_si = false;
                        if strings.len() >= MAX_SHARED_STRINGS
                            || total_bytes >= MAX_SHARED_STRING_BYTES
                        {
                            warn(
                                warnings,
                                format!(
                                    "shared string table exceeds the {MAX_SHARED_STRINGS}-entry or {MAX_SHARED_STRING_BYTES}-byte cap; later strings are unavailable"
                                ),
                            );
                            break;
                        }
                        total_bytes = total_bytes.saturating_add(text.len());
                        strings.push(std::mem::take(&mut text));
                    }
                    _ => {}
                }
            }
            Event::DocType(_) => {
                return Err(DocumentError::new(
                    "UNSUPPORTED_XML",
                    "DTD declarations are not allowed in office documents",
                ));
            }
            Event::Eof => {
                check_xml_eof(depth, "xl/sharedStrings.xml")?;
                break;
            }
            _ => {}
        }
    }
    Ok(strings)
}

struct XlsxSheetRequest<'a> {
    shared_strings: &'a [String],
    options: &'a ExtractionOptions,
    sheet_name: &'a str,
    part: &'a str,
    remaining_cells: usize,
}

fn parse_xlsx_sheet(
    xml: &str,
    request: &XlsxSheetRequest<'_>,
    budget: &mut TextBudget,
    warnings: &mut Vec<String>,
) -> DocResult<Sheet> {
    let XlsxSheetRequest {
        shared_strings,
        options,
        sheet_name,
        part,
        remaining_cells,
    } = *request;
    let mut reader = xml_reader(xml);
    let mut depth = 0usize;
    let mut rows = Vec::new();
    let mut truncated = false;
    let mut skip_row = false;
    let mut in_row = false;
    let mut row_cells: BTreeMap<usize, String> = BTreeMap::new();
    let mut next_column = 0usize;
    let mut cell_active = false;
    let mut cell_column = 0usize;
    let mut cell_type = String::new();
    let mut in_value = false;
    let mut in_formula = false;
    let mut in_inline = false;
    let mut in_inline_text = false;
    let mut cell_value = String::new();
    let mut cell_inline = String::new();
    let mut cell_formula_seen = false;
    let mut missing_shared_string = false;
    let mut formula_without_value = false;
    let mut cell_count = 0usize;

    loop {
        let event = reader
            .read_event()
            .map_err(|error| DocumentError::malformed_xml(part, error))?;
        if let Some(text) = decoded_event_text(&event, warnings) {
            if cell_active && !skip_row {
                if in_inline && in_inline_text {
                    push_capped(&mut cell_inline, &text, MAX_CELL_BYTES);
                } else if in_value {
                    push_capped(&mut cell_value, &text, MAX_CELL_BYTES);
                } else if in_formula {
                    // Formulas are not values. They are captured only so a
                    // formula without a cached value can be reported honestly.
                    cell_formula_seen = true;
                }
            }
            continue;
        }
        match event {
            Event::Start(element) => {
                check_depth(&mut depth, part)?;
                match local_name(&element) {
                    b"row" => {
                        in_row = true;
                        row_cells.clear();
                        next_column = 0;
                        skip_row = rows.len() >= options.max_rows || cell_count >= remaining_cells;
                        if skip_row {
                            truncated = true;
                        }
                    }
                    b"c" if in_row && !skip_row => {
                        cell_active = true;
                        cell_value.clear();
                        cell_inline.clear();
                        cell_formula_seen = false;
                        cell_type = attribute(&element, b"t").unwrap_or_else(|| "n".to_string());
                        cell_column = attribute(&element, b"r")
                            .and_then(|reference| parse_cell_column(&reference))
                            .unwrap_or(next_column);
                    }
                    b"v" if cell_active && !skip_row => in_value = true,
                    b"f" if cell_active && !skip_row => {
                        in_formula = true;
                        cell_formula_seen = true;
                    }
                    b"is" if cell_active && !skip_row => in_inline = true,
                    b"t" if in_inline && !skip_row => in_inline_text = true,
                    _ => {}
                }
            }
            Event::Empty(element) => match local_name(&element) {
                b"row" if in_row => {
                    // handled at row end
                }
                b"f" if cell_active && !skip_row => cell_formula_seen = true,
                b"c" if in_row && !skip_row => {
                    next_column = next_column.saturating_add(1).min(options.max_cols);
                }
                _ => {}
            },
            Event::End(element) => {
                depth = depth.saturating_sub(1);
                match end_name(&element) {
                    b"v" => in_value = false,
                    b"f" => in_formula = false,
                    b"is" => in_inline = false,
                    b"t" => in_inline_text = false,
                    b"c" if cell_active && !skip_row => {
                        cell_active = false;
                        let value = xlsx_cell_value(
                            &cell_type,
                            &cell_value,
                            &cell_inline,
                            shared_strings,
                            &mut missing_shared_string,
                        );
                        if cell_formula_seen && value.is_empty() {
                            formula_without_value = true;
                        }
                        if cell_column < options.max_cols {
                            let value = normalize_lines(&value);
                            if !value.is_empty() {
                                let value = budget.take(&value, MAX_CELL_BYTES);
                                if !value.is_empty() {
                                    if cell_count < remaining_cells {
                                        row_cells.insert(cell_column, value);
                                        cell_count += 1;
                                    } else {
                                        truncated = true;
                                    }
                                }
                            }
                            next_column = cell_column.saturating_add(1);
                        } else {
                            truncated = true;
                            next_column = cell_column.saturating_add(1);
                        }
                    }
                    b"row" if in_row => {
                        in_row = false;
                        if !skip_row {
                            if rows.len() < options.max_rows {
                                let mut row = Vec::new();
                                if let Some(last) = row_cells.keys().next_back() {
                                    let length = (last + 1).min(options.max_cols);
                                    row.resize(length, String::new());
                                    for (column, value) in &row_cells {
                                        if *column < length {
                                            row[*column] = value.clone();
                                        }
                                    }
                                }
                                rows.push(row);
                            } else {
                                truncated = true;
                            }
                        }
                        row_cells.clear();
                    }
                    _ => {}
                }
            }
            Event::DocType(_) => {
                return Err(DocumentError::new(
                    "UNSUPPORTED_XML",
                    "DTD declarations are not allowed in office documents",
                ));
            }
            Event::Eof => {
                check_xml_eof(depth, part)?;
                break;
            }
            _ => {}
        }
    }
    if cell_count >= remaining_cells {
        truncated = true;
    }
    if missing_shared_string {
        warn(
            warnings,
            format!("sheet {sheet_name} references a missing or out-of-range shared string"),
        );
    }
    if formula_without_value {
        warn(
            warnings,
            format!(
                "sheet {sheet_name} contains formula cells without cached values; those cells are empty"
            ),
        );
    }
    Ok(Sheet {
        name: sheet_name.to_string(),
        rows,
        truncated,
    })
}

fn xlsx_cell_value(
    cell_type: &str,
    raw_value: &str,
    inline_value: &str,
    shared_strings: &[String],
    missing_shared_string: &mut bool,
) -> String {
    match cell_type {
        "s" => {
            let index = raw_value.trim().parse::<usize>();
            match index.ok().and_then(|index| shared_strings.get(index)) {
                Some(value) => value.clone(),
                None => {
                    *missing_shared_string = true;
                    String::new()
                }
            }
        }
        "inlineStr" => inline_value.to_string(),
        "b" => match raw_value.trim() {
            "1" | "true" | "TRUE" => "TRUE".to_string(),
            "0" | "false" | "FALSE" => "FALSE".to_string(),
            other => other.to_string(),
        },
        "e" => raw_value.trim().to_string(),
        _ => raw_value.to_string(),
    }
}

fn parse_cell_column(reference: &str) -> Option<usize> {
    let mut column = 0usize;
    let mut saw_letter = false;
    for character in reference.chars() {
        if !character.is_ascii_alphabetic() {
            break;
        }
        saw_letter = true;
        let value = (character.to_ascii_uppercase() as u8).checked_sub(b'A')? as usize + 1;
        column = column.checked_mul(26)?.checked_add(value)?;
        if column > 1_000_000 {
            return None;
        }
    }
    saw_letter.then_some(column.saturating_sub(1))
}

/* -------------------------------- ODF ---------------------------------- */

#[derive(Clone, Debug, Default)]
struct OdfCellAttributes {
    value_type: Option<String>,
    value: Option<String>,
    boolean_value: Option<String>,
    date_value: Option<String>,
    time_value: Option<String>,
    formula: Option<String>,
}

fn extract_odt<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    index: &ArchiveIndex,
    limits: ZipLimits,
    options: &ExtractionOptions,
    warnings: &mut Vec<String>,
) -> DocResult<(Vec<DocumentBlock>, bool)> {
    let xml = read_xml_part(archive, index, limits, "content.xml")?;
    parse_odt(&xml, options, warnings)
}

fn parse_odt(
    xml: &str,
    _options: &ExtractionOptions,
    warnings: &mut Vec<String>,
) -> DocResult<(Vec<DocumentBlock>, bool)> {
    let mut reader = xml_reader(xml);
    let mut budget = TextBudget::new(MAX_EXTRACTED_CHARS);
    let mut blocks = Vec::new();
    let mut depth = 0usize;
    let mut in_paragraph = false;
    let mut paragraph_text = String::new();
    let mut heading_level = None;
    let mut table_depth = 0usize;
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    let mut table_truncated = false;
    let mut truncated = false;
    let mut current_row: Vec<String> = Vec::new();
    let mut current_cell = String::new();
    let mut in_cell = false;
    let mut row_repeat = 1usize;
    let mut cell_repeat = 1usize;

    'xml: loop {
        let event = reader
            .read_event()
            .map_err(|error| DocumentError::malformed_xml("content.xml", error))?;
        if let Some(text) = decoded_event_text(&event, warnings) {
            if in_paragraph && !push_capped(&mut paragraph_text, &text, MAX_BLOCK_BYTES) {
                truncated = true;
            }
            continue;
        }
        match event {
            Event::Start(element) => {
                check_depth(&mut depth, "content.xml")?;
                match local_name(&element) {
                    b"p" | b"h" => {
                        in_paragraph = true;
                        paragraph_text.clear();
                        heading_level = if local_name(&element) == b"h" {
                            attribute(&element, b"outline-level")
                                .and_then(|value| value.parse::<u8>().ok())
                                .map(|level| level.clamp(1, 6))
                                .or(Some(1))
                        } else {
                            None
                        };
                    }
                    b"table" => {
                        table_depth += 1;
                        if table_depth == 1 {
                            table_rows.clear();
                            table_truncated = false;
                        }
                    }
                    b"table-row" if table_depth == 1 => {
                        current_row.clear();
                        row_repeat = repeat_count(&element);
                    }
                    b"table-cell" if table_depth == 1 => {
                        in_cell = true;
                        current_cell.clear();
                        cell_repeat = repeat_count(&element);
                    }
                    b"tab" if in_paragraph => {
                        push_capped(&mut paragraph_text, "\t", MAX_BLOCK_BYTES);
                    }
                    b"line-break" if in_paragraph => {
                        push_capped(&mut paragraph_text, "\n", MAX_BLOCK_BYTES);
                    }
                    b"s" if in_paragraph => {
                        let count = attribute(&element, b"c")
                            .and_then(|value| value.parse::<usize>().ok())
                            .unwrap_or(1)
                            .min(64);
                        for _ in 0..count {
                            if !push_capped(&mut paragraph_text, " ", MAX_BLOCK_BYTES) {
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::Empty(element) => match local_name(&element) {
                b"tab" if in_paragraph => {
                    push_capped(&mut paragraph_text, "\t", MAX_BLOCK_BYTES);
                }
                b"line-break" if in_paragraph => {
                    push_capped(&mut paragraph_text, "\n", MAX_BLOCK_BYTES);
                }
                b"s" if in_paragraph => {
                    let count = attribute(&element, b"c")
                        .and_then(|value| value.parse::<usize>().ok())
                        .unwrap_or(1)
                        .min(64);
                    for _ in 0..count {
                        if !push_capped(&mut paragraph_text, " ", MAX_BLOCK_BYTES) {
                            break;
                        }
                    }
                }
                b"table-row" if table_depth == 1 => {
                    if table_rows.len() < MAX_TABLE_ROWS {
                        table_rows.push(Vec::new());
                    } else {
                        table_truncated = true;
                    }
                }
                b"table-cell" if table_depth == 1 => {
                    if current_row.len() < MAX_TABLE_COLS {
                        current_row.push(String::new());
                    } else {
                        table_truncated = true;
                    }
                }
                _ => {}
            },
            Event::End(element) => {
                depth = depth.saturating_sub(1);
                match end_name(&element) {
                    b"p" | b"h" => {
                        in_paragraph = false;
                        let text = normalize_inline(&paragraph_text);
                        if !text.is_empty() {
                            if table_depth > 0 {
                                if in_cell {
                                    let taken = budget.take(&text, MAX_CELL_BYTES);
                                    if !current_cell.is_empty() {
                                        push_capped(&mut current_cell, "\n", MAX_CELL_BYTES);
                                    }
                                    push_capped(&mut current_cell, &taken, MAX_CELL_BYTES);
                                }
                            } else if blocks.len() < MAX_BLOCKS {
                                let taken = budget.take(&text, MAX_BLOCK_BYTES);
                                if !taken.is_empty() {
                                    blocks.push(match heading_level {
                                        Some(level) => {
                                            DocumentBlock::Heading { level, text: taken }
                                        }
                                        None => DocumentBlock::Paragraph { text: taken },
                                    });
                                }
                            } else {
                                truncated = true;
                                break 'xml;
                            }
                        }
                        paragraph_text.clear();
                        if budget.exhausted() {
                            truncated = true;
                            break 'xml;
                        }
                    }
                    b"table-cell" if table_depth == 1 => {
                        if in_cell {
                            in_cell = false;
                            let cell = normalize_lines(&current_cell);
                            let repeats = cell_repeat.min(MAX_TABLE_COLS);
                            if cell_repeat > repeats {
                                table_truncated = true;
                            }
                            push_repeated_cell(
                                &mut current_row,
                                &cell,
                                repeats,
                                MAX_TABLE_COLS,
                                &mut budget,
                                &mut table_truncated,
                            );
                            current_cell.clear();
                        }
                    }
                    b"table-row" if table_depth == 1 => {
                        let repeat = row_repeat.min(MAX_ROW_REPEAT_EXPANSION);
                        if row_repeat > repeat {
                            table_truncated = true;
                        }
                        if current_row.iter().any(|cell| !cell.is_empty()) {
                            for _ in 0..repeat {
                                if table_rows.len() >= MAX_TABLE_ROWS {
                                    table_truncated = true;
                                    break;
                                }
                                table_rows.push(current_row.clone());
                            }
                        } else if table_rows.len() < MAX_TABLE_ROWS {
                            table_rows.push(std::mem::take(&mut current_row));
                        }
                        current_row.clear();
                    }
                    b"table" if table_depth > 0 => {
                        table_depth -= 1;
                        if table_depth == 0 {
                            let rows = std::mem::take(&mut table_rows);
                            if !rows.is_empty() && blocks.len() < MAX_BLOCKS {
                                blocks.push(DocumentBlock::Table {
                                    rows,
                                    truncated: table_truncated,
                                });
                            } else if !rows.is_empty() {
                                truncated = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::DocType(_) => {
                return Err(DocumentError::new(
                    "UNSUPPORTED_XML",
                    "DTD declarations are not allowed in office documents",
                ));
            }
            Event::Eof => {
                check_xml_eof(depth, "content.xml")?;
                break;
            }
            _ => {}
        }
    }
    let truncated = truncated
        || budget.truncated
        || blocks.iter().any(|block| match block {
            DocumentBlock::Table { truncated, .. } => *truncated,
            _ => false,
        });
    Ok((blocks, truncated))
}

fn repeat_count(element: &BytesStart<'_>) -> usize {
    attribute(element, b"number-columns-repeated")
        .or_else(|| attribute(element, b"number-rows-repeated"))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1)
}

fn push_repeated_cell(
    row: &mut Vec<String>,
    value: &str,
    repeats: usize,
    max_cols: usize,
    budget: &mut TextBudget,
    truncated: &mut bool,
) {
    if value.is_empty() {
        for _ in 0..repeats {
            if row.len() >= max_cols {
                *truncated = true;
                break;
            }
            row.push(String::new());
        }
        return;
    }
    for index in 0..repeats {
        if row.len() >= max_cols {
            *truncated = true;
            break;
        }
        let copy = if index == 0 {
            value.to_string()
        } else {
            budget.take(value, MAX_CELL_BYTES)
        };
        if copy.is_empty() {
            *truncated = true;
            break;
        }
        row.push(copy);
    }
}

fn extract_odp<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    index: &ArchiveIndex,
    limits: ZipLimits,
    options: &ExtractionOptions,
    warnings: &mut Vec<String>,
) -> DocResult<SlideSet> {
    let xml = read_xml_part(archive, index, limits, "content.xml")?;
    parse_odp(&xml, options, warnings)
}

fn parse_odp(
    xml: &str,
    options: &ExtractionOptions,
    warnings: &mut Vec<String>,
) -> DocResult<SlideSet> {
    let mut reader = xml_reader(xml);
    let mut budget = TextBudget::new(MAX_EXTRACTED_CHARS);
    let mut depth = 0usize;
    let mut slides = Vec::new();
    let mut total = 0usize;
    let mut in_page = false;
    let mut page_selected = false;
    let mut page_name = None;
    let mut page_number = 0usize;
    let mut page_paragraphs = Vec::new();
    let mut page_truncated = false;
    let mut in_paragraph = false;
    let mut paragraph_text = String::new();
    let mut notes_depth = 0usize;

    loop {
        let event = reader
            .read_event()
            .map_err(|error| DocumentError::malformed_xml("content.xml", error))?;
        if let Some(text) = decoded_event_text(&event, warnings) {
            if in_page
                && page_selected
                && in_paragraph
                && notes_depth == 0
                && !push_capped(&mut paragraph_text, &text, MAX_BLOCK_BYTES)
            {
                page_truncated = true;
            }
            continue;
        }
        match event {
            Event::Start(element) => {
                check_depth(&mut depth, "content.xml")?;
                match local_name(&element) {
                    b"page" => {
                        total += 1;
                        in_page = true;
                        page_number = total;
                        page_selected = options
                            .pages
                            .as_ref()
                            .is_none_or(|pages| pages.contains(&page_number));
                        page_name = attribute(&element, b"name");
                        page_paragraphs.clear();
                        page_truncated = false;
                    }
                    b"notes" if in_page => notes_depth += 1,
                    b"p" | b"h" if in_page && page_selected && notes_depth == 0 => {
                        in_paragraph = true;
                        paragraph_text.clear();
                    }
                    b"tab" if in_paragraph => {
                        push_capped(&mut paragraph_text, "\t", MAX_BLOCK_BYTES);
                    }
                    b"line-break" if in_paragraph => {
                        push_capped(&mut paragraph_text, "\n", MAX_BLOCK_BYTES);
                    }
                    b"s" if in_paragraph => {
                        let count = attribute(&element, b"c")
                            .and_then(|value| value.parse::<usize>().ok())
                            .unwrap_or(1)
                            .min(64);
                        for _ in 0..count {
                            if !push_capped(&mut paragraph_text, " ", MAX_BLOCK_BYTES) {
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::Empty(element) => match local_name(&element) {
                b"tab" if in_paragraph => {
                    push_capped(&mut paragraph_text, "\t", MAX_BLOCK_BYTES);
                }
                b"line-break" if in_paragraph => {
                    push_capped(&mut paragraph_text, "\n", MAX_BLOCK_BYTES);
                }
                b"s" if in_paragraph => {
                    let count = attribute(&element, b"c")
                        .and_then(|value| value.parse::<usize>().ok())
                        .unwrap_or(1)
                        .min(64);
                    for _ in 0..count {
                        if !push_capped(&mut paragraph_text, " ", MAX_BLOCK_BYTES) {
                            break;
                        }
                    }
                }
                _ => {}
            },
            Event::End(element) => {
                depth = depth.saturating_sub(1);
                match end_name(&element) {
                    b"p" | b"h" => {
                        if in_paragraph {
                            in_paragraph = false;
                            let text = normalize_inline(&paragraph_text);
                            if !text.is_empty()
                                && page_selected
                                && notes_depth == 0
                                && page_paragraphs.len() < MAX_SLIDE_PARAGRAPHS
                            {
                                let taken = budget.take(&text, MAX_BLOCK_BYTES);
                                if !taken.is_empty() {
                                    page_paragraphs.push(taken);
                                }
                            } else if page_paragraphs.len() >= MAX_SLIDE_PARAGRAPHS {
                                page_truncated = true;
                                budget.truncated = true;
                            }
                            paragraph_text.clear();
                        }
                    }
                    b"notes" if in_page => notes_depth = notes_depth.saturating_sub(1),
                    b"page" if in_page => {
                        in_page = false;
                        if page_selected {
                            slides.push(Slide {
                                number: page_number,
                                name: page_name.take(),
                                part: "content.xml".to_string(),
                                paragraphs: std::mem::take(&mut page_paragraphs),
                                truncated: page_truncated || budget.truncated,
                            });
                            if slides.len() >= MAX_SLIDES {
                                if let Some(last) = slides.last_mut() {
                                    last.truncated = true;
                                }
                                warn(
                                    warnings,
                                    format!(
                                        "presentation has more than {MAX_SLIDES} slides; later slides were skipped"
                                    ),
                                );
                                break;
                            }
                        }
                        page_paragraphs.clear();
                    }
                    _ => {}
                }
            }
            Event::DocType(_) => {
                return Err(DocumentError::new(
                    "UNSUPPORTED_XML",
                    "DTD declarations are not allowed in office documents",
                ));
            }
            Event::Eof => {
                check_xml_eof(depth, "content.xml")?;
                break;
            }
            _ => {}
        }
    }
    if let Some(pages) = &options.pages {
        for page in pages {
            if *page > total {
                warn(
                    warnings,
                    format!("slide {page} does not exist; presentation has {total} slide(s)"),
                );
            }
        }
        if slides.is_empty() {
            return Err(DocumentError::new(
                "PAGE_NOT_FOUND",
                format!("none of the requested pages exist; presentation has {total} slide(s)"),
            ));
        }
    }
    Ok(SlideSet {
        total,
        truncated: slides.iter().any(|slide| slide.truncated) || budget.truncated,
        slides,
    })
}

fn extract_ods<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    index: &ArchiveIndex,
    limits: ZipLimits,
    options: &ExtractionOptions,
    warnings: &mut Vec<String>,
) -> DocResult<SheetSet> {
    let xml = read_xml_part(archive, index, limits, "content.xml")?;
    let mut all_names = scan_odf_table_names(&xml, warnings)?;
    if all_names.is_empty() {
        return Err(DocumentError::new(
            "MALFORMED_DOCUMENT",
            "spreadsheet contains no tables",
        ));
    }
    let names_truncated = all_names.len() > MAX_SHEETS;
    if names_truncated {
        all_names.truncate(MAX_SHEETS);
    }
    let selected = sheet_selection(&all_names, options.sheets.as_ref(), warnings)?;
    let mut budget = TextBudget::new(MAX_EXTRACTED_CHARS);
    let (mut sheets, total_cells_truncated) =
        parse_ods_sheets(&xml, &all_names, &selected, options, &mut budget, warnings)?;
    if total_cells_truncated || budget.truncated {
        for sheet in &mut sheets {
            sheet.truncated = true;
        }
    }
    if sheets.is_empty() {
        return Err(DocumentError::new(
            "SHEET_NOT_FOUND",
            "no selected sheet could be read",
        ));
    }
    Ok(SheetSet {
        all_names,
        sheets,
        truncated: names_truncated || total_cells_truncated || budget.truncated,
    })
}

fn scan_odf_table_names(xml: &str, warnings: &mut Vec<String>) -> DocResult<Vec<String>> {
    let mut reader = xml_reader(xml);
    let mut depth = 0usize;
    let mut names = Vec::new();
    loop {
        let event = reader
            .read_event()
            .map_err(|error| DocumentError::malformed_xml("content.xml", error))?;
        match event {
            Event::Start(element) => {
                check_depth(&mut depth, "content.xml")?;
                if !scan_odf_table_name(&element, warnings, &mut names) {
                    break;
                }
            }
            Event::Empty(element) => {
                if !scan_odf_table_name(&element, warnings, &mut names) {
                    break;
                }
            }
            Event::End(_) => depth = depth.saturating_sub(1),
            Event::DocType(_) => {
                return Err(DocumentError::new(
                    "UNSUPPORTED_XML",
                    "DTD declarations are not allowed in office documents",
                ));
            }
            Event::Eof => {
                check_xml_eof(depth, "content.xml")?;
                break;
            }
            _ => {}
        }
    }
    Ok(names)
}

fn scan_odf_table_name(
    element: &BytesStart<'_>,
    warnings: &mut Vec<String>,
    names: &mut Vec<String>,
) -> bool {
    if local_name(element) != b"table" {
        return true;
    }
    if names.len() >= MAX_SHEETS {
        warn(
            warnings,
            format!("spreadsheet has more than {MAX_SHEETS} tables; later sheets were skipped"),
        );
        return false;
    }
    names.push(attribute(element, b"name").unwrap_or_else(|| format!("Sheet{}", names.len() + 1)));
    true
}

fn parse_ods_sheets(
    xml: &str,
    all_names: &[String],
    selected: &[bool],
    options: &ExtractionOptions,
    budget: &mut TextBudget,
    warnings: &mut Vec<String>,
) -> DocResult<(Vec<Sheet>, bool)> {
    let mut reader = xml_reader(xml);
    let mut depth = 0usize;
    let mut sheets = Vec::new();
    let mut table_index = 0usize;
    let mut in_table = false;
    let mut skip_table_depth = 0usize;
    let mut sheet_name = String::new();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut truncated = false;
    let mut in_row = false;
    let mut row_repeat = 1usize;
    let mut skip_row = false;
    let mut row_cells: BTreeMap<usize, String> = BTreeMap::new();
    let mut next_column = 0usize;
    let mut in_cell = false;
    let mut cell_repeat = 1usize;
    let mut cell_text = String::new();
    let mut cell_attributes = OdfCellAttributes::default();
    let mut paragraph_active = false;
    let mut paragraph_text = String::new();
    let mut total_cells = 0usize;
    let mut total_cells_truncated = false;
    let mut formula_without_value = false;

    loop {
        let event = reader
            .read_event()
            .map_err(|error| DocumentError::malformed_xml("content.xml", error))?;
        if let Some(text) = decoded_event_text(&event, warnings) {
            if paragraph_active {
                push_capped(&mut paragraph_text, &text, MAX_CELL_BYTES);
            }
            continue;
        }
        match event {
            Event::Start(element) => {
                check_depth(&mut depth, "content.xml")?;
                match local_name(&element) {
                    b"table" => {
                        if skip_table_depth > 0 {
                            skip_table_depth += 1;
                            continue;
                        }
                        if in_table {
                            // Nested tables are formatting inside a cell; their
                            // text is captured through the enclosing cell.
                            skip_table_depth = 1;
                            continue;
                        }
                        if table_index >= all_names.len() {
                            skip_table_depth = 1;
                            table_index += 1;
                            continue;
                        }
                        let chosen = selected.get(table_index).copied().unwrap_or(false);
                        table_index += 1;
                        if !chosen {
                            skip_table_depth = 1;
                            continue;
                        }
                        in_table = true;
                        sheet_name = all_names
                            .get(table_index - 1)
                            .cloned()
                            .unwrap_or_else(|| format!("Sheet{table_index}"));
                        rows.clear();
                        truncated = false;
                    }
                    b"table-row" if in_table => {
                        in_row = true;
                        row_cells.clear();
                        next_column = 0;
                        row_repeat = repeat_count(&element);
                        if row_repeat > MAX_ROW_REPEAT_EXPANSION {
                            truncated = true;
                            row_repeat = MAX_ROW_REPEAT_EXPANSION;
                        }
                        skip_row =
                            rows.len() >= options.max_rows || total_cells >= MAX_TOTAL_SHEET_CELLS;
                        if skip_row {
                            truncated = true;
                        }
                    }
                    b"table-cell" if in_row && !skip_row => {
                        in_cell = true;
                        cell_text.clear();
                        paragraph_text.clear();
                        paragraph_active = false;
                        cell_repeat = repeat_count(&element);
                        cell_attributes = OdfCellAttributes {
                            value_type: attribute(&element, b"value-type"),
                            value: attribute(&element, b"value"),
                            boolean_value: attribute(&element, b"boolean-value"),
                            date_value: attribute(&element, b"date-value"),
                            time_value: attribute(&element, b"time-value"),
                            formula: attribute(&element, b"formula"),
                        };
                    }
                    b"p" | b"h" if in_cell => {
                        paragraph_active = true;
                        paragraph_text.clear();
                    }
                    b"tab" if paragraph_active => {
                        push_capped(&mut paragraph_text, "\t", MAX_CELL_BYTES);
                    }
                    b"line-break" if paragraph_active => {
                        push_capped(&mut paragraph_text, "\n", MAX_CELL_BYTES);
                    }
                    b"s" if paragraph_active => {
                        let count = attribute(&element, b"c")
                            .and_then(|value| value.parse::<usize>().ok())
                            .unwrap_or(1)
                            .min(64);
                        for _ in 0..count {
                            if !push_capped(&mut paragraph_text, " ", MAX_CELL_BYTES) {
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::Empty(element) => match local_name(&element) {
                b"table-row" if in_table => {
                    if rows.len() < options.max_rows {
                        rows.push(Vec::new());
                    } else {
                        truncated = true;
                    }
                }
                b"table-cell" if in_row && !skip_row => {
                    next_column = next_column.saturating_add(repeat_count(&element));
                    if next_column > options.max_cols {
                        truncated = true;
                    }
                }
                b"tab" if paragraph_active => {
                    push_capped(&mut paragraph_text, "\t", MAX_CELL_BYTES);
                }
                b"line-break" if paragraph_active => {
                    push_capped(&mut paragraph_text, "\n", MAX_CELL_BYTES);
                }
                b"s" if paragraph_active => {
                    let count = attribute(&element, b"c")
                        .and_then(|value| value.parse::<usize>().ok())
                        .unwrap_or(1)
                        .min(64);
                    for _ in 0..count {
                        if !push_capped(&mut paragraph_text, " ", MAX_CELL_BYTES) {
                            break;
                        }
                    }
                }
                _ => {}
            },
            Event::End(element) => {
                depth = depth.saturating_sub(1);
                match end_name(&element) {
                    b"p" | b"h" if paragraph_active => {
                        paragraph_active = false;
                        let text = normalize_inline(&paragraph_text);
                        if !text.is_empty() {
                            if !cell_text.is_empty() {
                                push_capped(&mut cell_text, "\n", MAX_CELL_BYTES);
                            }
                            push_capped(&mut cell_text, &text, MAX_CELL_BYTES);
                        }
                        paragraph_text.clear();
                    }
                    b"table-cell" if in_cell => {
                        in_cell = false;
                        let value = ods_cell_value(&cell_text, &cell_attributes);
                        if value.is_empty() && cell_attributes.formula.is_some() {
                            formula_without_value = true;
                        }
                        if !value.is_empty() {
                            let taken = budget.take(&value, MAX_CELL_BYTES);
                            if !taken.is_empty()
                                && next_column < options.max_cols
                                && total_cells < MAX_TOTAL_SHEET_CELLS
                            {
                                let repeats =
                                    cell_repeat.min(options.max_cols.saturating_sub(next_column));
                                for offset in 0..repeats {
                                    if total_cells >= MAX_TOTAL_SHEET_CELLS {
                                        total_cells_truncated = true;
                                        break;
                                    }
                                    row_cells.insert(next_column + offset, taken.clone());
                                    total_cells += 1;
                                }
                                next_column = next_column.saturating_add(repeats);
                            } else if !taken.is_empty() {
                                total_cells_truncated = true;
                            }
                        } else {
                            next_column = next_column.saturating_add(cell_repeat);
                        }
                        if next_column > options.max_cols {
                            truncated = true;
                            next_column = options.max_cols;
                        }
                        cell_text.clear();
                    }
                    b"table-row" if in_row => {
                        in_row = false;
                        if !skip_row && rows.len() < options.max_rows {
                            let row = row_vector(&row_cells, options.max_cols);
                            let has_content = row.iter().any(|value| !value.is_empty());
                            if has_content {
                                for _ in 0..row_repeat {
                                    if rows.len() >= options.max_rows {
                                        truncated = true;
                                        break;
                                    }
                                    rows.push(row.clone());
                                }
                            } else if rows.len() < options.max_rows {
                                rows.push(row);
                            }
                        }
                        row_cells.clear();
                    }
                    b"table" if in_table => {
                        in_table = false;
                        sheets.push(Sheet {
                            name: std::mem::take(&mut sheet_name),
                            rows: std::mem::take(&mut rows),
                            truncated: truncated || total_cells_truncated,
                        });
                    }
                    b"table" if skip_table_depth > 0 => {
                        skip_table_depth = skip_table_depth.saturating_sub(1);
                    }
                    _ => {}
                }
            }
            Event::DocType(_) => {
                return Err(DocumentError::new(
                    "UNSUPPORTED_XML",
                    "DTD declarations are not allowed in office documents",
                ));
            }
            Event::Eof => {
                check_xml_eof(depth, "content.xml")?;
                break;
            }
            _ => {}
        }
    }
    if formula_without_value {
        warn(
            warnings,
            "formula cells without a cached value are rendered as empty",
        );
    }
    if total_cells_truncated {
        for sheet in &mut sheets {
            sheet.truncated = true;
        }
    }
    Ok((sheets, total_cells_truncated))
}

fn row_vector(row_cells: &BTreeMap<usize, String>, max_cols: usize) -> Vec<String> {
    let Some(last) = row_cells.keys().next_back() else {
        return Vec::new();
    };
    let length = (last + 1).min(max_cols);
    let mut row = vec![String::new(); length];
    for (column, value) in row_cells {
        if *column < length {
            row[*column] = value.clone();
        }
    }
    row
}

fn ods_cell_value(text: &str, attributes: &OdfCellAttributes) -> String {
    let text = normalize_lines(text);
    if !text.is_empty() {
        return text;
    }
    match attributes.value_type.as_deref() {
        Some("boolean") => match attributes.boolean_value.as_deref() {
            Some("true") | Some("1") => "TRUE".to_string(),
            Some("false") | Some("0") => "FALSE".to_string(),
            _ => String::new(),
        },
        Some("date") => attributes.date_value.clone().unwrap_or_default(),
        Some("time") => attributes.time_value.clone().unwrap_or_default(),
        Some("float") | Some("currency") | Some("percentage") => {
            attributes.value.clone().unwrap_or_default()
        }
        Some("string") => attributes.value.clone().unwrap_or_default(),
        _ => attributes.value.clone().unwrap_or_default(),
    }
    .trim()
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::TempDir;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

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
        }
    }

    fn write_zip(path: &Path, entries: &[(&str, &str)]) {
        let file = File::create(path).unwrap();
        let mut writer = ZipWriter::new(file);
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for (name, contents) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(contents.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
    }

    fn write_docx(path: &Path, document: &str) {
        write_zip(
            path,
            &[
                (
                    "[Content_Types].xml",
                    r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>"#,
                ),
                ("word/document.xml", document),
            ],
        );
    }

    const DOCX_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Project Title</w:t></w:r></w:p>
    <w:p><w:r><w:t>Hello</w:t></w:r><w:r><w:t xml:space="preserve"> world &amp; friends</w:t></w:r></w:p>
    <w:tbl>
      <w:tr><w:tc><w:p><w:r><w:t>Name</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Value</w:t></w:r></w:p></w:tc></w:tr>
      <w:tr><w:tc><w:p><w:r><w:t>Alpha</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>42</w:t></w:r></w:p></w:tc></w:tr>
    </w:tbl>
    <w:p><w:r><w:t>After table</w:t></w:r></w:p>
  </w:body>
</w:document>"#;

    #[tokio::test]
    async fn extracts_docx_paragraphs_headings_and_tables_in_order() {
        let dir = TempDir::new().unwrap();
        write_docx(&dir.path().join("spec.docx"), DOCX_XML);
        let output = ReadDocument
            .execute(
                &context(dir.path()),
                json!({"path": "spec.docx", "include_tables": true}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        assert!(
            output.content.contains("# Project Title"),
            "{}",
            output.content
        );
        assert!(
            output.content.contains("Hello world & friends"),
            "{}",
            output.content
        );
        assert!(
            output.content.contains("| Name | Value |"),
            "{}",
            output.content
        );
        assert!(
            output.content.contains("| Alpha | 42 |"),
            "{}",
            output.content
        );
        let structured = output.structured.unwrap();
        assert_eq!(structured["format"], "docx");
        assert_eq!(structured["entries"], 4);
        assert_eq!(structured["tables"][0]["rows"][0][0], "Name");
        let heading = output
            .content
            .find("# Project Title")
            .expect("heading position");
        let paragraph = output
            .content
            .find("Hello world & friends")
            .expect("paragraph position");
        let table = output
            .content
            .find("| Name | Value |")
            .expect("table position");
        let after = output
            .content
            .find("After table")
            .expect("trailing paragraph position");
        assert!(heading < paragraph && paragraph < table && table < after);
    }

    #[tokio::test]
    async fn omits_docx_tables_when_requested() {
        let dir = TempDir::new().unwrap();
        write_docx(&dir.path().join("spec.docx"), DOCX_XML);
        let output = ReadDocument
            .execute(
                &context(dir.path()),
                json!({"path": "spec.docx", "include_tables": false}),
            )
            .await
            .unwrap();
        assert!(!output.is_error);
        assert!(
            !output.content.contains("| Name | Value |"),
            "{}",
            output.content
        );
        assert!(output.content.contains("table 1: 2 rows x 2 cols omitted"));
    }

    #[tokio::test]
    async fn truncates_docx_output_honestly() {
        let dir = TempDir::new().unwrap();
        let mut xml = String::from("<w:document xmlns:w=\"http://x\"><w:body>");
        for index in 0..50 {
            xml.push_str(&format!(
                "<w:p><w:r><w:t>paragraph {index} with some text</w:t></w:r></w:p>"
            ));
        }
        xml.push_str("</w:body></w:document>");
        write_docx(&dir.path().join("long.docx"), &xml);
        let output = ReadDocument
            .execute(
                &context(dir.path()),
                json!({"path": "long.docx", "max_chars": 100}),
            )
            .await
            .unwrap();
        assert!(!output.is_error);
        let structured = output.structured.unwrap();
        assert_eq!(structured["truncated"], true);
        assert!(output.content.contains("… document text truncated"));
    }

    #[tokio::test]
    async fn rejects_malformed_and_missing_docx_parts() {
        let dir = TempDir::new().unwrap();
        write_docx(
            &dir.path().join("broken.docx"),
            "<w:document><w:body><w:p><w:r><w:t>unclosed</w:t></w:r></w:p></w:body>",
        );
        let broken = ReadDocument
            .execute(&context(dir.path()), json!({"path": "broken.docx"}))
            .await
            .unwrap();
        assert!(broken.is_error);
        assert_eq!(broken.structured.unwrap()["code"], "MALFORMED_XML");

        write_zip(&dir.path().join("empty.docx"), &[("foo.xml", "<foo/>")]);
        let missing = ReadDocument
            .execute(&context(dir.path()), json!({"path": "empty.docx"}))
            .await
            .unwrap();
        assert!(missing.is_error);
        assert_eq!(missing.structured.unwrap()["code"], "MISSING_DOCUMENT_PART");
    }

    #[tokio::test]
    async fn rejects_path_escape_and_unsupported_extensions() {
        let dir = TempDir::new().unwrap();
        let outside = dir.path().parent().unwrap().join("outside.docx");
        std::fs::write(&outside, b"not a zip").unwrap();
        let escape = ReadDocument
            .execute(&context(dir.path()), json!({"path": "../outside.docx"}))
            .await;
        assert!(escape.is_err());
        std::fs::write(dir.path().join("legacy.doc"), b"binary").unwrap();
        let unsupported = ReadDocument
            .execute(&context(dir.path()), json!({"path": "legacy.doc"}))
            .await
            .unwrap();
        assert!(unsupported.is_error);
        assert_eq!(
            unsupported.structured.unwrap()["code"],
            "UNSUPPORTED_FORMAT"
        );
    }

    #[tokio::test]
    async fn enforces_zip_limits_and_path_traversal() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("limits.docx");
        write_zip(
            &path,
            &[
                ("word/document.xml", "<document>hello</document>"),
                ("other.xml", "<other/>"),
                ("third.xml", "<third/>"),
            ],
        );
        let file = File::open(&path).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        let too_few = validate_archive(
            &mut archive,
            ZipLimits {
                max_entries: 2,
                ..ZipLimits::default()
            },
        )
        .unwrap_err();
        assert_eq!(too_few.code, "ZIP_ENTRY_LIMIT");

        let file = File::open(&path).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        let too_large = validate_archive(
            &mut archive,
            ZipLimits {
                max_entry_bytes: 4,
                ..ZipLimits::default()
            },
        )
        .unwrap_err();
        assert_eq!(too_large.code, "ZIP_ENTRY_TOO_LARGE");

        let file = File::open(&path).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        let total = validate_archive(
            &mut archive,
            ZipLimits {
                max_total_bytes: 10,
                ..ZipLimits::default()
            },
        )
        .unwrap_err();
        assert_eq!(total.code, "ZIP_TOTAL_TOO_LARGE");

        let traversal = dir.path().join("traversal.docx");
        write_zip(
            &traversal,
            &[("../evil.xml", "<evil/>"), ("word/document.xml", "<d/>")],
        );
        let file = File::open(&traversal).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        let error = validate_archive(&mut archive, ZipLimits::default()).unwrap_err();
        assert_eq!(error.code, "ZIP_PATH_TRAVERSAL");
    }

    #[tokio::test]
    async fn follows_presentation_relationship_order_and_page_filter() {
        let dir = TempDir::new().unwrap();
        let presentation = r#"<?xml version="1.0"?>
<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <p:sldIdLst><p:sldId id="256" r:id="rId2"/><p:sldId id="257" r:id="rId1"/></p:sldIdLst>
</p:presentation>"#;
        let rels = r#"<?xml version="1.0"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/>
</Relationships>"#;
        let slide1 = r#"<p:sld xmlns:p="http://x" xmlns:a="http://y"><p:cSld><p:spTree>
          <p:sp><p:txBody><a:p><a:r><a:t>First-file text</a:t></a:r></a:p></p:txBody></p:sp>
        </p:spTree></p:cSld></p:sld>"#;
        let slide2 = r#"<p:sld xmlns:p="http://x" xmlns:a="http://y"><p:cSld><p:spTree>
          <p:sp><p:txBody><a:p><a:r><a:t>Second-file text</a:t></a:r></a:p><a:p><a:r><a:t>More detail</a:t></a:r></a:p></p:txBody></p:sp>
        </p:spTree></p:cSld></p:sld>"#;
        write_zip(
            &dir.path().join("deck.pptx"),
            &[
                ("ppt/presentation.xml", presentation),
                ("ppt/_rels/presentation.xml.rels", rels),
                ("ppt/slides/slide1.xml", slide1),
                ("ppt/slides/slide2.xml", slide2),
            ],
        );
        let output = ReadDocument
            .execute(&context(dir.path()), json!({"path": "deck.pptx"}))
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let first = output.content.find("[slide 1]").unwrap();
        let second = output.content.find("[slide 2]").unwrap();
        assert!(first < second);
        assert!(
            output.content.find("Second-file text").unwrap()
                < output.content.find("[slide 2]").unwrap()
        );
        assert!(output.content.find("First-file text").unwrap() > second);
        let structured = output.structured.unwrap();
        assert_eq!(structured["slide_count"], 2);
        assert_eq!(
            structured["slides"][0]["text"],
            "Second-file text\nMore detail"
        );
        assert_eq!(structured["slides"][1]["text"], "First-file text");

        let filtered = ReadDocument
            .execute(
                &context(dir.path()),
                json!({"path": "deck.pptx", "pages": "2"}),
            )
            .await
            .unwrap();
        assert!(!filtered.is_error);
        let structured = filtered.structured.unwrap();
        assert_eq!(structured["slides"].as_array().unwrap().len(), 1);
        assert_eq!(structured["slides"][0]["number"], 2);
    }

    #[tokio::test]
    async fn extracts_xlsx_shared_strings_types_rows_and_sheet_filter() {
        let dir = TempDir::new().unwrap();
        let workbook = r#"<?xml version="1.0"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets>
    <sheet name="Data" sheetId="1" r:id="rId1"/>
    <sheet name="Summary" sheetId="2" r:id="rId2"/>
  </sheets>
</workbook>"#;
        let rels = r#"<?xml version="1.0"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/>
</Relationships>"#;
        let shared = r#"<?xml version="1.0"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="3" uniqueCount="3">
  <si><t>Name</t></si>
  <si><t>Alpha</t></si>
  <si><r><t>Beta</t></r><r><t> Two</t></r></si>
</sst>"#;
        let sheet1 = r#"<?xml version="1.0"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>
  <row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row>
  <row r="2"><c r="A2" t="s"><v>2</v></c><c r="B2"><v>42</v></c><c r="C2" t="b"><v>1</v></c></row>
  <row r="3"><c r="A3" t="inlineStr"><is><t>Inline</t></is></c></row>
</sheetData></worksheet>"#;
        let sheet2 = r#"<?xml version="1.0"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>
  <row r="1"><c r="A1" t="inlineStr"><is><t>Total</t></is></c><c r="B1"><v>42</v></c></row>
</sheetData></worksheet>"#;
        write_zip(
            &dir.path().join("book.xlsx"),
            &[
                ("xl/workbook.xml", workbook),
                ("xl/_rels/workbook.xml.rels", rels),
                ("xl/sharedStrings.xml", shared),
                ("xl/worksheets/sheet1.xml", sheet1),
                ("xl/worksheets/sheet2.xml", sheet2),
            ],
        );
        let output = ReadDocument
            .execute(&context(dir.path()), json!({"path": "book.xlsx"}))
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let structured = output.structured.unwrap();
        assert_eq!(structured["sheet_names"][0], "Data");
        assert_eq!(structured["sheet_names"][1], "Summary");
        assert_eq!(structured["sheets"][0]["rows"][0][0], "Name");
        assert_eq!(structured["sheets"][0]["rows"][1][0], "Beta Two");
        assert_eq!(structured["sheets"][0]["rows"][1][1], "42");
        assert_eq!(structured["sheets"][0]["rows"][1][2], "TRUE");
        assert_eq!(structured["sheets"][0]["rows"][2][0], "Inline");
        assert!(output.content.contains("[sheet 1: Data"));

        let filtered = ReadDocument
            .execute(
                &context(dir.path()),
                json!({"path": "book.xlsx", "sheets": "summary"}),
            )
            .await
            .unwrap();
        let structured = filtered.structured.unwrap();
        assert_eq!(structured["sheets"].as_array().unwrap().len(), 1);
        assert_eq!(structured["sheets"][0]["name"], "Summary");
        assert_eq!(structured["sheets"][0]["rows"][0][1], "42");

        let capped = ReadDocument
            .execute(
                &context(dir.path()),
                json!({"path": "book.xlsx", "max_rows": 1, "max_cols": 2}),
            )
            .await
            .unwrap();
        let structured = capped.structured.unwrap();
        assert_eq!(structured["sheets"][0]["row_count"], 1);
        assert_eq!(structured["sheets"][0]["truncated"], true);

        let missing = ReadDocument
            .execute(
                &context(dir.path()),
                json!({"path": "book.xlsx", "sheets": "Nope"}),
            )
            .await
            .unwrap();
        assert!(missing.is_error);
        assert_eq!(missing.structured.unwrap()["code"], "SHEET_NOT_FOUND");
    }

    #[tokio::test]
    async fn extracts_odt_ods_and_odp_text() {
        let dir = TempDir::new().unwrap();
        let odt = r#"<?xml version="1.0"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0">
  <office:body><office:text>
    <text:h text:outline-level="2">ODT Heading</text:h>
    <text:p>ODT paragraph with <text:s text:c="2"/>spacing.</text:p>
    <table:table table:name="T1">
      <table:table-row><table:table-cell><text:p>A</text:p></table:table-cell><table:table-cell><text:p>B</text:p></table:table-cell></table:table-row>
      <table:table-row><table:table-cell><text:p>C</text:p></table:table-cell><table:table-cell><text:p>D</text:p></table:table-cell></table:table-row>
    </table:table>
  </office:text></office:body>
</office:document-content>"#;
        write_zip(&dir.path().join("note.odt"), &[("content.xml", odt)]);
        let output = ReadDocument
            .execute(&context(dir.path()), json!({"path": "note.odt"}))
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        assert!(
            output.content.contains("## ODT Heading"),
            "{}",
            output.content
        );
        assert!(output.content.contains("| A | B |"), "{}", output.content);
        let structured = output.structured.unwrap();
        assert_eq!(structured["format"], "odt");

        let ods = r#"<?xml version="1.0"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0">
  <office:body><office:spreadsheet>
    <table:table table:name="Numbers">
      <table:table-row><table:table-cell office:value-type="string"><text:p>Item</text:p></table:table-cell><table:table-cell office:value-type="string"><text:p>Count</text:p></table:table-cell></table:table-row>
      <table:table-row><table:table-cell office:value-type="string"><text:p>Apples</text:p></table:table-cell><table:table-cell office:value-type="float" office:value="3"><text:p>3</text:p></table:table-cell></table:table-row>
    </table:table>
    <table:table table:name="Empty"><table:table-row><table:table-cell/></table:table-row></table:table>
  </office:spreadsheet></office:body>
</office:document-content>"#;
        write_zip(&dir.path().join("book.ods"), &[("content.xml", ods)]);
        let output = ReadDocument
            .execute(
                &context(dir.path()),
                json!({"path": "book.ods", "sheets": "Numbers"}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let structured = output.structured.unwrap();
        assert_eq!(structured["sheet_names"].as_array().unwrap().len(), 2);
        assert_eq!(structured["sheets"].as_array().unwrap().len(), 1);
        assert_eq!(structured["sheets"][0]["rows"][1][0], "Apples");
        assert_eq!(structured["sheets"][0]["rows"][1][1], "3");

        let odp = r#"<?xml version="1.0"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0">
  <office:body><office:presentation>
    <draw:page draw:name="Intro"><draw:frame><draw:text-box><text:p>Slide one</text:p></draw:text-box></draw:frame></draw:page>
    <draw:page draw:name="Details"><draw:frame><draw:text-box><text:p>Slide two</text:p><text:p>Second paragraph</text:p></draw:text-box></draw:frame>
      <presentation:notes><text:p>Speaker note that should not be slide text</text:p></presentation:notes>
    </draw:page>
  </office:presentation></office:body>
</office:document-content>"#;
        write_zip(&dir.path().join("deck.odp"), &[("content.xml", odp)]);
        let output = ReadDocument
            .execute(&context(dir.path()), json!({"path": "deck.odp"}))
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let structured = output.structured.unwrap();
        assert_eq!(structured["slide_count"], 2);
        assert_eq!(structured["slides"][0]["name"], "Intro");
        assert_eq!(
            structured["slides"][1]["text"],
            "Slide two\nSecond paragraph"
        );
        assert!(!output.content.contains("Speaker note"));
    }
}
