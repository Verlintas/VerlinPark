//! Safe, bounded archive inspection and extraction.
//!
//! Only three formats are accepted: ZIP, POSIX tar, and gzip-compressed POSIX
//! tar. The daemon never invokes an external decompressor and never guesses an
//! archive format from a file extension. Detection is magic/header based and a
//! malformed container fails closed instead of falling through to another
//! decoder.
//!
//! Listing is metadata-only. Extraction re-validates every selected entry,
//! writes into a caller-provided daemon-owned scratch directory through a
//! canonical relative join, and never creates symlinks, hardlinks, devices, or
//! FIFOs. Paths, entry counts, per-entry bytes, total bytes, and compression
//! ratios are hard caps; no tool argument can raise them.
//!
//! The module returns structured errors, not provider text. `archive_media.rs`
//! maps them to the Q3 tool-error contract and to typed JSON-RPC failures.

use std::collections::{BTreeSet, HashSet};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use vp_protocol::types::{ArchiveEntry, ArchiveLimits, ArtifactArchiveExtractParams};

use crate::tooling::errors::ToolErrorCode;

const ZIP_MAGIC: &[u8] = b"PK\x03\x04";
const ZIP_EMPTY_MAGIC: &[u8] = b"PK\x05\x06";
const ZIP_SPANNED_MAGIC: &[u8] = b"PK\x07\x08";
const GZIP_MAGIC: &[u8] = b"\x1f\x8b";
const TAR_USTAR_OFFSET: usize = 257;
const TAR_USTAR_LEN: usize = 5;
const TAR_BLOCK: usize = 512;
const MAX_PATH_CHARS: usize = 1_024;
const MAX_COMPONENT_CHARS: usize = 255;

/// The accepted archive containers. `TarGz` is gzip containing tar, not a
/// single-file gzip stream; a bare gzip member is rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    Tar,
    TarGz,
}

impl ArchiveFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::Tar => "tar",
            Self::TarGz => "tar.gz",
        }
    }

    pub fn is_compressed(self) -> bool {
        matches!(self, Self::Zip | Self::TarGz)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveError {
    /// The container or any selected entry is password-protected/encrypted.
    Encrypted,
    /// The bytes do not match a supported magic/header.
    UnsupportedFormat,
    /// The container is truncated, has an invalid central directory, or has a
    /// header that cannot be parsed. The bounded detail never includes file
    /// content.
    Malformed(String),
    /// A hard cap was hit. The string is the limit name, not a user path.
    LimitExceeded(&'static str),
    /// The entry is traversal, absolute, a link/device, a duplicate, or over a
    /// per-entry/ratio cap. The string is a short stable reason.
    UnsafeEntry(String),
    /// A requested path is not present in the archive listing.
    EntryNotFound(String),
    /// No entry was selected and `all` was not requested.
    EmptySelection,
}

impl ArchiveError {
    pub fn code(&self) -> ToolErrorCode {
        match self {
            Self::Encrypted => ToolErrorCode::ARCHIVE_ENCRYPTED,
            Self::UnsupportedFormat => ToolErrorCode::ARCHIVE_FORMAT_UNSUPPORTED,
            Self::Malformed(_) => ToolErrorCode::ARCHIVE_CORRUPT,
            Self::LimitExceeded(_) => ToolErrorCode::ARCHIVE_LIMIT_EXCEEDED,
            Self::UnsafeEntry(_) => ToolErrorCode::ARCHIVE_UNSAFE_ENTRY,
            Self::EntryNotFound(_) => ToolErrorCode::ARCHIVE_ENTRY_NOT_FOUND,
            Self::EmptySelection => ToolErrorCode::ARCHIVE_EMPTY_SELECTION,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Encrypted => "archive is encrypted or password-protected".to_string(),
            Self::UnsupportedFormat => {
                "unsupported archive format; only zip, tar, and tar.gz are accepted".to_string()
            }
            Self::Malformed(detail) => format!("archive is malformed: {detail}"),
            Self::LimitExceeded(limit) => {
                format!("archive exceeds the hard {limit} limit")
            }
            Self::UnsafeEntry(reason) => {
                format!("archive entry is not safe to extract: {reason}")
            }
            Self::EntryNotFound(path) => format!("archive entry not found: {path}"),
            Self::EmptySelection => "no archive entries were selected".to_string(),
        }
    }

    pub fn evidence(&self) -> Option<String> {
        match self {
            Self::UnsafeEntry(reason) => Some(format!("reason={reason}")),
            Self::EntryNotFound(path) => Some(format!("path={}", safe_display_path(path))),
            Self::LimitExceeded(limit) => Some(format!("limit={limit}")),
            Self::Malformed(detail) => Some(format!("detail={}", sanitize_detail(detail))),
            Self::Encrypted | Self::UnsupportedFormat | Self::EmptySelection => None,
        }
    }
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl std::error::Error for ArchiveError {}

#[derive(Debug, Clone)]
pub struct ArchiveListing {
    pub format: ArchiveFormat,
    pub entries: Vec<ArchiveEntry>,
    pub entry_count: usize,
    pub truncated: bool,
    pub total_uncompressed_bytes: u64,
    pub total_compressed_bytes: u64,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ExtractedEntry {
    pub path: String,
    pub kind: String,
    pub size_bytes: u64,
    pub sha256: Option<String>,
    /// Absolute path inside the caller's scratch root. The caller must delete
    /// the scratch root; this path is never a workspace path.
    pub scratch_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ArchiveExtraction {
    pub format: ArchiveFormat,
    pub entries: Vec<ExtractedEntry>,
    pub total_bytes: u64,
    pub warnings: Vec<String>,
}

/// Detect the format from bytes. A file named `.zip` containing tar bytes is a
/// tar; a file named `.tar` containing a ZIP central directory is a ZIP.
pub fn detect_format(bytes: &[u8]) -> Result<ArchiveFormat, ArchiveError> {
    if bytes.starts_with(ZIP_MAGIC)
        || bytes.starts_with(ZIP_EMPTY_MAGIC)
        || bytes.starts_with(ZIP_SPANNED_MAGIC)
    {
        return Ok(ArchiveFormat::Zip);
    }
    if bytes.starts_with(GZIP_MAGIC) {
        return Ok(ArchiveFormat::TarGz);
    }
    if looks_like_tar(bytes) {
        return Ok(ArchiveFormat::Tar);
    }
    Err(ArchiveError::UnsupportedFormat)
}

fn looks_like_tar(bytes: &[u8]) -> bool {
    if bytes.len() < TAR_BLOCK {
        return false;
    }
    if bytes.get(TAR_USTAR_OFFSET..TAR_USTAR_OFFSET + TAR_USTAR_LEN) == Some(b"ustar") {
        return true;
    }
    tar_header_checksum_valid(&bytes[..TAR_BLOCK])
}

fn tar_header_checksum_valid(header: &[u8]) -> bool {
    if header.len() < TAR_BLOCK {
        return false;
    }
    if header.iter().all(|byte| *byte == 0) {
        return false;
    }
    let Some(expected) = parse_tar_octal(&header[148..156]) else {
        return false;
    };
    let mut sum: u64 = 0;
    for (index, byte) in header.iter().enumerate() {
        sum += if (148..156).contains(&index) {
            u64::from(b' ')
        } else {
            u64::from(*byte)
        };
    }
    sum == expected
}

fn parse_tar_octal(field: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(field).ok()?.trim_matches(['\0', ' ']);
    if text.is_empty() {
        return Some(0);
    }
    let text = text.trim_start_matches('0');
    if text.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(text, 8).ok()
}

/// List an archive without extracting bytes. The listing still reads each ZIP
/// entry header and iterates tar headers, so malformed containers fail closed.
pub fn list_archive(bytes: &[u8], limits: &ArchiveLimits) -> Result<ArchiveListing, ArchiveError> {
    if bytes.len() as u64 > limits.max_archive_bytes {
        return Err(ArchiveError::LimitExceeded("archive_bytes"));
    }
    let format = detect_format(bytes)?;
    let mut warning_set = BTreeSet::new();
    let mut entries = Vec::new();
    let mut entry_count = 0usize;
    let mut truncated = false;
    let mut total_uncompressed = 0u64;
    let mut total_compressed = 0u64;
    let mut seen = HashSet::<String>::new();

    match format {
        ArchiveFormat::Zip => list_zip(
            bytes,
            limits,
            &mut entries,
            &mut entry_count,
            &mut truncated,
            &mut total_uncompressed,
            &mut total_compressed,
            &mut seen,
            &mut warning_set,
        )?,
        ArchiveFormat::Tar | ArchiveFormat::TarGz => list_tar(
            bytes,
            format,
            limits,
            &mut entries,
            &mut entry_count,
            &mut truncated,
            &mut total_uncompressed,
            &mut seen,
            &mut warning_set,
        )?,
    }

    if total_uncompressed > limits.max_total_bytes {
        warning_set.insert("total_uncompressed_bytes_exceeded".to_string());
        for entry in &mut entries {
            if entry.kind == "file" && entry.safe {
                entry.safe = false;
                entry.unsafe_reason = Some("total_uncompressed_bytes_exceeded".to_string());
            }
        }
    }

    Ok(ArchiveListing {
        format,
        entries,
        entry_count,
        truncated,
        total_uncompressed_bytes: total_uncompressed,
        total_compressed_bytes: total_compressed,
        warnings: warning_set.into_iter().collect(),
    })
}

#[allow(clippy::too_many_arguments)]
fn list_zip(
    bytes: &[u8],
    limits: &ArchiveLimits,
    entries: &mut Vec<ArchiveEntry>,
    entry_count: &mut usize,
    truncated: &mut bool,
    total_uncompressed: &mut u64,
    total_compressed: &mut u64,
    seen: &mut HashSet<String>,
    warnings: &mut BTreeSet<String>,
) -> Result<(), ArchiveError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| zip_error("central directory", error))?;
    let len = archive.len();
    for index in 0..len {
        *entry_count += 1;
        if *entry_count > limits.max_entries {
            *truncated = true;
            warnings.insert("entry_count_truncated".to_string());
            break;
        }
        let file = archive
            .by_index(index)
            .map_err(|error| zip_error("entry header", error))?;
        if file.encrypted() {
            return Err(ArchiveError::Encrypted);
        }
        let raw_name = file.name().to_string();
        let kind = zip_entry_kind(file.is_dir(), file.unix_mode());
        let size = file.size();
        let compressed = file.compressed_size();
        *total_compressed = total_compressed.saturating_add(compressed);
        *total_uncompressed = total_uncompressed.saturating_add(size);
        let mut entry = classify_entry(
            index,
            &raw_name,
            kind,
            size,
            Some(compressed),
            file.unix_mode(),
        );
        apply_zip_safety(&mut entry, size, compressed, limits, seen, warnings);
        entries.push(entry);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn list_tar(
    bytes: &[u8],
    format: ArchiveFormat,
    limits: &ArchiveLimits,
    entries: &mut Vec<ArchiveEntry>,
    entry_count: &mut usize,
    truncated: &mut bool,
    total_uncompressed: &mut u64,
    seen: &mut HashSet<String>,
    warnings: &mut BTreeSet<String>,
) -> Result<(), ArchiveError> {
    match format {
        ArchiveFormat::Tar => {
            let archive = tar::Archive::new(Cursor::new(bytes));
            list_tar_entries(
                archive,
                limits,
                entries,
                entry_count,
                truncated,
                total_uncompressed,
                seen,
                warnings,
            )
        }
        ArchiveFormat::TarGz => {
            let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
            let archive = tar::Archive::new(decoder);
            list_tar_entries(
                archive,
                limits,
                entries,
                entry_count,
                truncated,
                total_uncompressed,
                seen,
                warnings,
            )
        }
        ArchiveFormat::Zip => unreachable!("zip is listed by list_zip"),
    }
}

#[allow(clippy::too_many_arguments)]
fn list_tar_entries<R: Read>(
    mut archive: tar::Archive<R>,
    limits: &ArchiveLimits,
    entries: &mut Vec<ArchiveEntry>,
    entry_count: &mut usize,
    truncated: &mut bool,
    total_uncompressed: &mut u64,
    seen: &mut HashSet<String>,
    warnings: &mut BTreeSet<String>,
) -> Result<(), ArchiveError> {
    let iterator = archive
        .entries()
        .map_err(|error| malformed_tar("entry iterator", error))?;
    for entry in iterator {
        *entry_count += 1;
        if *entry_count > limits.max_entries {
            *truncated = true;
            warnings.insert("entry_count_truncated".to_string());
            break;
        }
        let entry = entry.map_err(|error| malformed_tar("entry", error))?;
        let header = entry.header();
        let entry_type = header.entry_type();
        let kind = tar_entry_kind(entry_type);
        let size = header.size().unwrap_or(0);
        *total_uncompressed = total_uncompressed.saturating_add(size);
        let mode = header.mode().ok();
        let modified = header
            .mtime()
            .ok()
            .and_then(|seconds| time::OffsetDateTime::from_unix_timestamp(seconds as i64).ok())
            .and_then(|value| {
                value
                    .format(&time::format_description::well_known::Rfc3339)
                    .ok()
            });
        let raw_path = match entry.path() {
            Ok(path) => match path.to_str() {
                Some(text) => text.to_string(),
                None => {
                    let display = path.to_string_lossy().to_string();
                    let mut entry = classify_entry(
                        (*entry_count).saturating_sub(1),
                        &display,
                        kind,
                        size,
                        None,
                        mode,
                    );
                    entry.safe = false;
                    entry.unsafe_reason = Some("non_utf8_path".to_string());
                    entries.push(entry);
                    continue;
                }
            },
            Err(error) => {
                return Err(malformed_tar("path", error));
            }
        };
        let mut entry = classify_entry(
            (*entry_count).saturating_sub(1),
            &raw_path,
            kind,
            size,
            None,
            mode,
        );
        entry.modified_at = modified;
        apply_tar_safety(&mut entry, size, limits, seen, warnings);
        entries.push(entry);
    }
    Ok(())
}

fn zip_error(context: &str, error: zip::result::ZipError) -> ArchiveError {
    match error {
        zip::result::ZipError::UnsupportedArchive(detail)
            if detail == zip::result::ZipError::PASSWORD_REQUIRED =>
        {
            ArchiveError::Encrypted
        }
        other => ArchiveError::Malformed(format!(
            "zip {context}: {}",
            sanitize_detail(&other.to_string())
        )),
    }
}

fn malformed_tar(context: &str, error: impl std::fmt::Display) -> ArchiveError {
    ArchiveError::Malformed(format!(
        "tar {context}: {}",
        sanitize_detail(&error.to_string())
    ))
}

fn zip_entry_kind(is_dir: bool, mode: Option<u32>) -> &'static str {
    if is_dir {
        return "directory";
    }
    match mode.map(|mode| mode & 0o170_000) {
        Some(0o120_000) => "symlink",
        Some(0o100_000) => "file",
        Some(0o060_000) => "device",
        Some(0o020_000) => "device",
        Some(0o010_000) => "fifo",
        Some(0o140_000) => "other",
        _ => "file",
    }
}

fn tar_entry_kind(entry_type: tar::EntryType) -> &'static str {
    if entry_type.is_dir() {
        "directory"
    } else if entry_type.is_symlink() {
        "symlink"
    } else if entry_type.is_hard_link() {
        "hardlink"
    } else if entry_type.is_block_special() || entry_type.is_character_special() {
        "device"
    } else if entry_type.is_fifo() {
        "fifo"
    } else if entry_type.is_file() {
        "file"
    } else {
        "other"
    }
}

fn classify_entry(
    index: usize,
    raw_path: &str,
    kind: &'static str,
    size: u64,
    compressed_size: Option<u64>,
    mode: Option<u32>,
) -> ArchiveEntry {
    match normalize_entry_path(raw_path) {
        Ok(path) => ArchiveEntry {
            index,
            path,
            kind: kind.to_string(),
            size_bytes: size,
            compressed_size,
            mode,
            safe: true,
            unsafe_reason: None,
            duplicate: false,
            modified_at: None,
        },
        Err(reason) => ArchiveEntry {
            index,
            path: safe_display_path(raw_path),
            kind: kind.to_string(),
            size_bytes: size,
            compressed_size,
            mode,
            safe: false,
            unsafe_reason: Some(reason.to_string()),
            duplicate: false,
            modified_at: None,
        },
    }
}

fn apply_zip_safety(
    entry: &mut ArchiveEntry,
    size: u64,
    compressed: u64,
    limits: &ArchiveLimits,
    seen: &mut HashSet<String>,
    warnings: &mut BTreeSet<String>,
) {
    mark_common_safety(entry, size, limits, seen);
    if entry.kind != "file" && entry.safe {
        entry.safe = false;
        entry.unsafe_reason = Some(format!("unsupported_entry_kind:{}", entry.kind));
    }
    if entry.safe {
        if compressed == 0 && size > 0 {
            entry.safe = false;
            entry.unsafe_reason = Some("invalid_compression_metadata".to_string());
        } else if compressed > 0 {
            let ratio = size / compressed.max(1);
            if ratio > limits.max_compression_ratio {
                entry.safe = false;
                entry.unsafe_reason = Some("compression_ratio_exceeded".to_string());
                warnings.insert("compression_ratio_exceeded".to_string());
            }
        }
    }
}

fn apply_tar_safety(
    entry: &mut ArchiveEntry,
    size: u64,
    limits: &ArchiveLimits,
    seen: &mut HashSet<String>,
    _warnings: &mut BTreeSet<String>,
) {
    mark_common_safety(entry, size, limits, seen);
    if entry.kind != "file" && entry.kind != "directory" && entry.safe {
        entry.safe = false;
        entry.unsafe_reason = Some(format!("unsupported_entry_kind:{}", entry.kind));
    }
}

fn mark_common_safety(
    entry: &mut ArchiveEntry,
    size: u64,
    limits: &ArchiveLimits,
    seen: &mut HashSet<String>,
) {
    if !entry.safe {
        return;
    }
    if !seen.insert(entry.path.clone()) {
        entry.safe = false;
        entry.duplicate = true;
        entry.unsafe_reason = Some("duplicate_entry".to_string());
        return;
    }
    if size > limits.max_entry_bytes {
        entry.safe = false;
        entry.unsafe_reason = Some("entry_too_large".to_string());
    }
}

/// Normalize an archive entry path. Any traversal, absolute path, empty path,
/// NUL byte, drive prefix, or long component is rejected. Backslashes are
/// treated as separators before validation so a Windows-built archive cannot
/// smuggle `..\\` through the macOS extractor.
pub fn normalize_entry_path(raw: &str) -> Result<String, &'static str> {
    if raw.is_empty() {
        return Err("empty_path");
    }
    if raw.contains('\0') {
        return Err("nul_byte");
    }
    if raw.chars().count() > MAX_PATH_CHARS {
        return Err("path_too_long");
    }
    let unified = raw.replace('\\', "/");
    if unified.starts_with('/') {
        return Err("absolute_path");
    }
    let bytes = unified.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return Err("absolute_path");
    }
    let mut parts = Vec::new();
    for component in unified.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." {
            return Err("path_traversal");
        }
        if component.chars().count() > MAX_COMPONENT_CHARS {
            return Err("component_too_long");
        }
        if component.chars().any(|character| character.is_control()) {
            return Err("control_character");
        }
        parts.push(component);
    }
    if parts.is_empty() {
        return Err("empty_path");
    }
    Ok(parts.join("/"))
}

/// Sanitize a display-only raw path. It is never used for filesystem access.
pub fn safe_display_path(raw: &str) -> String {
    let mut text = raw
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_PATH_CHARS)
        .collect::<String>();
    if text.is_empty() {
        text = "(empty)".to_string();
    }
    text
}

fn sanitize_detail(detail: &str) -> String {
    detail
        .chars()
        .filter(|character| !character.is_control())
        .take(300)
        .collect()
}

/// Select the entries to extract. `all` and explicit paths are mutually
/// exclusive by validation; an empty explicit list means `EmptySelection`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveSelection {
    All,
    Paths(Vec<String>),
}

impl ArchiveSelection {
    pub fn from_params(params: &ArtifactArchiveExtractParams) -> Result<Self, ArchiveError> {
        if params.all && !params.entries.is_empty() {
            return Err(ArchiveError::UnsafeEntry(
                "all_and_entries_are_mutually_exclusive".to_string(),
            ));
        }
        if params.all {
            return Ok(Self::All);
        }
        if params.entries.is_empty() {
            return Err(ArchiveError::EmptySelection);
        }
        Ok(Self::Paths(params.entries.clone()))
    }
}

/// Extract selected entries into `scratch_root`. The caller owns the directory
/// lifecycle and must remove it after publishing derived artifacts. The
/// function re-lists the archive with the same hard caps, validates every
/// selected entry, and always writes through a canonical join under the root.
pub fn extract_archive_to_scratch(
    bytes: &[u8],
    selection: &ArchiveSelection,
    limits: &ArchiveLimits,
    scratch_root: &Path,
) -> Result<ArchiveExtraction, ArchiveError> {
    let listing = list_archive(bytes, limits)?;
    if listing.truncated {
        return Err(ArchiveError::LimitExceeded("entry_count"));
    }
    if listing.total_uncompressed_bytes > limits.max_total_bytes {
        return Err(ArchiveError::LimitExceeded("total_uncompressed_bytes"));
    }
    let selected = resolve_selection(&listing, selection, limits)?;
    std::fs::create_dir_all(scratch_root)
        .map_err(|error| ArchiveError::Malformed(format!("scratch root: {error}")))?;
    let canonical_root = scratch_root
        .canonicalize()
        .map_err(|error| ArchiveError::Malformed(format!("canonical scratch root: {error}")))?;

    match listing.format {
        ArchiveFormat::Zip => extract_zip(bytes, &selected, &listing, limits, &canonical_root),
        ArchiveFormat::Tar => {
            let archive = tar::Archive::new(Cursor::new(bytes));
            extract_tar(
                archive,
                listing.format,
                &selected,
                limits,
                &canonical_root,
                &listing,
            )
        }
        ArchiveFormat::TarGz => {
            let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
            let archive = tar::Archive::new(decoder);
            extract_tar(
                archive,
                listing.format,
                &selected,
                limits,
                &canonical_root,
                &listing,
            )
        }
    }
}

fn resolve_selection(
    listing: &ArchiveListing,
    selection: &ArchiveSelection,
    limits: &ArchiveLimits,
) -> Result<BTreeSet<String>, ArchiveError> {
    let by_path = listing
        .entries
        .iter()
        .map(|entry| (entry.path.clone(), entry))
        .collect::<std::collections::HashMap<_, _>>();
    let mut selected = BTreeSet::new();
    match selection {
        ArchiveSelection::All => {
            for entry in &listing.entries {
                if entry.kind == "file" && entry.safe {
                    selected.insert(entry.path.clone());
                }
            }
        }
        ArchiveSelection::Paths(paths) => {
            for raw in paths {
                let normalized = normalize_entry_path(raw).map_err(|reason| {
                    ArchiveError::UnsafeEntry(format!("{reason}:{}", safe_display_path(raw)))
                })?;
                let Some(entry) = by_path.get(&normalized) else {
                    return Err(ArchiveError::EntryNotFound(normalized));
                };
                if !entry.safe {
                    return Err(ArchiveError::UnsafeEntry(
                        entry
                            .unsafe_reason
                            .clone()
                            .unwrap_or_else(|| "unsafe_entry".to_string()),
                    ));
                }
                if entry.kind != "file" {
                    return Err(ArchiveError::UnsafeEntry(format!(
                        "unsupported_entry_kind:{}",
                        entry.kind
                    )));
                }
                selected.insert(normalized);
            }
        }
    }
    if selected.is_empty() {
        return Err(ArchiveError::EmptySelection);
    }
    if selected.len() > limits.max_entries {
        return Err(ArchiveError::LimitExceeded("entry_count"));
    }
    Ok(selected)
}

fn extract_zip(
    bytes: &[u8],
    selected: &BTreeSet<String>,
    listing: &ArchiveListing,
    limits: &ArchiveLimits,
    root: &Path,
) -> Result<ArchiveExtraction, ArchiveError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| zip_error("central directory", error))?;
    let mut extracted = Vec::new();
    let mut total = 0u64;
    let mut warnings = listing.warnings.clone();
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|error| zip_error("entry header", error))?;
        if file.encrypted() {
            return Err(ArchiveError::Encrypted);
        }
        let raw = file.name().to_string();
        let normalized = match normalize_entry_path(&raw) {
            Ok(path) => path,
            Err(_) => continue,
        };
        if !selected.contains(&normalized) {
            continue;
        }
        let kind = zip_entry_kind(file.is_dir(), file.unix_mode());
        if kind != "file" {
            return Err(ArchiveError::UnsafeEntry(format!(
                "unsupported_entry_kind:{kind}"
            )));
        }
        let entry = listing_entry(listing, &normalized)?;
        if !entry.safe {
            return Err(ArchiveError::UnsafeEntry(
                entry
                    .unsafe_reason
                    .clone()
                    .unwrap_or_else(|| "unsafe_entry".to_string()),
            ));
        }
        let destination = scratch_join(root, &normalized)?;
        let (size, sha256) = write_bounded(&mut file, &destination, limits)?;
        total = total.saturating_add(size);
        if total > limits.max_total_bytes {
            return Err(ArchiveError::LimitExceeded("total_uncompressed_bytes"));
        }
        if let Some(compressed) = entry.compressed_size
            && compressed > 0
        {
            let ratio = size / compressed.max(1);
            if ratio > limits.max_compression_ratio {
                warnings.push(format!("compression_ratio_exceeded:{normalized}"));
                return Err(ArchiveError::UnsafeEntry(
                    "compression_ratio_exceeded".to_string(),
                ));
            }
        }
        extracted.push(ExtractedEntry {
            path: normalized,
            kind: kind.to_string(),
            size_bytes: size,
            sha256: Some(sha256),
            scratch_path: Some(destination),
        });
    }
    ensure_all_selected(selected, &extracted)?;
    Ok(ArchiveExtraction {
        format: ArchiveFormat::Zip,
        entries: extracted,
        total_bytes: total,
        warnings,
    })
}

fn extract_tar<R: Read>(
    mut archive: tar::Archive<R>,
    format: ArchiveFormat,
    selected: &BTreeSet<String>,
    limits: &ArchiveLimits,
    root: &Path,
    listing: &ArchiveListing,
) -> Result<ArchiveExtraction, ArchiveError> {
    let iterator = archive
        .entries()
        .map_err(|error| malformed_tar("entry iterator", error))?;
    let mut extracted = Vec::new();
    let mut total = 0u64;
    for entry in iterator {
        let mut entry = entry.map_err(|error| malformed_tar("entry", error))?;
        let header = entry.header();
        let entry_type = header.entry_type();
        let kind = tar_entry_kind(entry_type);
        let raw_path = match entry.path() {
            Ok(path) => match path.to_str() {
                Some(text) => text.to_string(),
                None => continue,
            },
            Err(error) => return Err(malformed_tar("path", error)),
        };
        let normalized = match normalize_entry_path(&raw_path) {
            Ok(path) => path,
            Err(_) => continue,
        };
        if !selected.contains(&normalized) {
            continue;
        }
        if kind != "file" {
            return Err(ArchiveError::UnsafeEntry(format!(
                "unsupported_entry_kind:{kind}"
            )));
        }
        let listed = listing_entry(listing, &normalized)?;
        if !listed.safe {
            return Err(ArchiveError::UnsafeEntry(
                listed
                    .unsafe_reason
                    .clone()
                    .unwrap_or_else(|| "unsafe_entry".to_string()),
            ));
        }
        let declared = header.size().unwrap_or(0);
        if declared > limits.max_entry_bytes {
            return Err(ArchiveError::LimitExceeded("entry_bytes"));
        }
        let destination = scratch_join(root, &normalized)?;
        let (size, sha256) = write_bounded(&mut entry, &destination, limits)?;
        total = total.saturating_add(size);
        if total > limits.max_total_bytes {
            return Err(ArchiveError::LimitExceeded("total_uncompressed_bytes"));
        }
        extracted.push(ExtractedEntry {
            path: normalized,
            kind: kind.to_string(),
            size_bytes: size,
            sha256: Some(sha256),
            scratch_path: Some(destination),
        });
    }
    ensure_all_selected(selected, &extracted)?;
    Ok(ArchiveExtraction {
        format,
        entries: extracted,
        total_bytes: total,
        warnings: listing.warnings.clone(),
    })
}

fn listing_entry<'a>(
    listing: &'a ArchiveListing,
    path: &str,
) -> Result<&'a ArchiveEntry, ArchiveError> {
    listing
        .entries
        .iter()
        .find(|entry| entry.path == path)
        .ok_or_else(|| ArchiveError::EntryNotFound(path.to_string()))
}

fn ensure_all_selected(
    selected: &BTreeSet<String>,
    extracted: &[ExtractedEntry],
) -> Result<(), ArchiveError> {
    let found = extracted
        .iter()
        .map(|entry| entry.path.as_str())
        .collect::<HashSet<_>>();
    for path in selected {
        if !found.contains(path.as_str()) {
            return Err(ArchiveError::EntryNotFound(path.clone()));
        }
    }
    Ok(())
}

fn scratch_join(root: &Path, normalized: &str) -> Result<PathBuf, ArchiveError> {
    let mut current = root.to_path_buf();
    for component in normalized.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(ArchiveError::UnsafeEntry("path_traversal".to_string()));
        }
        if component.contains('\\') || component.contains('\0') {
            return Err(ArchiveError::UnsafeEntry(
                "invalid_path_component".to_string(),
            ));
        }
        current.push(component);
    }
    let parent = current
        .parent()
        .ok_or_else(|| ArchiveError::UnsafeEntry("path_traversal".to_string()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| ArchiveError::Malformed(format!("scratch directory: {error}")))?;
    let canonical_parent = parent
        .canonicalize()
        .map_err(|error| ArchiveError::Malformed(format!("canonical scratch path: {error}")))?;
    if !canonical_parent.starts_with(root) {
        return Err(ArchiveError::UnsafeEntry("scratch_path_escape".to_string()));
    }
    Ok(canonical_parent.join(
        current
            .file_name()
            .ok_or_else(|| ArchiveError::UnsafeEntry("empty_path".to_string()))?,
    ))
}

fn write_bounded(
    reader: &mut impl Read,
    destination: &Path,
    limits: &ArchiveLimits,
) -> Result<(u64, String), ArchiveError> {
    if destination.exists() {
        return Err(ArchiveError::UnsafeEntry("duplicate_entry".to_string()));
    }
    let file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)
        .map_err(|error| ArchiveError::Malformed(format!("create scratch file: {error}")))?;
    let mut writer = std::io::BufWriter::new(file);
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| ArchiveError::Malformed(format!("read entry bytes: {error}")))?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > limits.max_entry_bytes {
            return Err(ArchiveError::LimitExceeded("entry_bytes"));
        }
        hasher.update(&buffer[..read]);
        writer
            .write_all(&buffer[..read])
            .map_err(|error| ArchiveError::Malformed(format!("write scratch file: {error}")))?;
    }
    writer
        .flush()
        .map_err(|error| ArchiveError::Malformed(format!("flush scratch file: {error}")))?;
    Ok((total, format!("{:x}", hasher.finalize())))
}

/// Whether an entry kind is a regular file that can become an artifact.
pub fn is_extractable_file(kind: &str) -> bool {
    kind == "file"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn limits() -> ArchiveLimits {
        ArchiveLimits {
            max_entries: 100,
            max_entry_bytes: 1024 * 1024,
            max_total_bytes: 4 * 1024 * 1024,
            max_archive_bytes: 8 * 1024 * 1024,
            max_compression_ratio: 100,
        }
    }

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buffer = Cursor::new(Vec::new());
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

    fn tar_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, *name, *data).unwrap();
        }
        builder.into_inner().unwrap()
    }

    fn tar_single_with_mode(name: &str, data: &[u8], mode: u32) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        builder.append_data(&mut header, name, data).unwrap();
        builder.into_inner().unwrap()
    }

    fn gzip_tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let tar = tar_bytes(entries);
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&tar).unwrap();
        encoder.finish().unwrap()
    }

    fn tar_with_header(
        name: &str,
        entry_type: tar::EntryType,
        data: &[u8],
        link: Option<&str>,
    ) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(entry_type);
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        if let Some(link) = link {
            header.set_link_name(link).unwrap();
        }
        header.set_cksum();
        builder.append_data(&mut header, name, data).unwrap();
        builder.into_inner().unwrap()
    }

    /// Rewrite the first tar header name in place and recompute the checksum.
    /// The high-level tar builder deliberately rejects `..`, so traversal tests
    /// exercise the reader against a hand-built adversarial header.
    fn patch_tar_name(mut bytes: Vec<u8>, name: &str) -> Vec<u8> {
        assert!(name.len() <= 100);
        bytes[..100].fill(0);
        bytes[..name.len()].copy_from_slice(name.as_bytes());
        bytes[148..156].fill(b' ');
        let checksum: u64 = bytes[..512].iter().map(|byte| u64::from(*byte)).sum();
        let text = format!("{:06o}\0 ", checksum);
        bytes[148..156].copy_from_slice(text.as_bytes());
        bytes
    }

    #[test]
    fn path_component_and_length_caps_are_enforced() {
        let long_component = "a".repeat(300);
        assert_eq!(
            normalize_entry_path(&format!("dir/{long_component}")).unwrap_err(),
            "component_too_long"
        );
        let long_path = (0..12)
            .map(|index| format!("{}{index}", "x".repeat(90)))
            .collect::<Vec<_>>()
            .join("/");
        assert_eq!(
            normalize_entry_path(&long_path).unwrap_err(),
            "path_too_long"
        );
    }

    #[test]
    fn empty_zip_is_valid_but_empty_tar_is_not_supported() {
        let zip = zip_bytes(&[]);
        let listing = list_archive(&zip, &limits()).unwrap();
        assert_eq!(listing.entry_count, 0);
        assert!(listing.entries.is_empty());
        let scratch = TempDir::new().unwrap();
        let error =
            extract_archive_to_scratch(&zip, &ArchiveSelection::All, &limits(), scratch.path())
                .unwrap_err();
        assert_eq!(error, ArchiveError::EmptySelection);

        let empty_tar = vec![0u8; 1024];
        assert_eq!(
            detect_format(&empty_tar).unwrap_err(),
            ArchiveError::UnsupportedFormat
        );
    }

    #[test]
    fn tar_duplicate_entries_are_rejected_like_zip_duplicates() {
        let mut builder = tar::Builder::new(Vec::new());
        for data in [b"first".as_slice(), b"second".as_slice()] {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, "same.txt", data).unwrap();
        }
        let bytes = builder.into_inner().unwrap();
        let listing = list_archive(&bytes, &limits()).unwrap();
        assert_eq!(listing.entries.len(), 2);
        assert!(listing.entries[0].safe);
        assert!(!listing.entries[1].safe);
        assert!(listing.entries[1].duplicate);
        assert_eq!(
            listing.entries[1].unsafe_reason.as_deref(),
            Some("duplicate_entry")
        );
    }

    #[test]
    fn selection_normalizes_dot_prefix_and_backslashes() {
        let bytes = zip_bytes(&[("docs/a.txt", b"alpha")]);
        let scratch = TempDir::new().unwrap();
        let extraction = extract_archive_to_scratch(
            &bytes,
            &ArchiveSelection::Paths(vec!["./docs\\a.txt".to_string()]),
            &limits(),
            scratch.path(),
        )
        .unwrap();
        assert_eq!(extraction.entries[0].path, "docs/a.txt");
    }

    #[test]
    fn tar_gz_cannot_bypass_the_total_uncompressed_cap() {
        let payload = vec![b'x'; 2 * 1024 * 1024];
        let bytes = gzip_tar(&[("big.bin", &payload)]);
        let mut tight = limits();
        tight.max_total_bytes = 1024;
        let scratch = TempDir::new().unwrap();
        let error =
            extract_archive_to_scratch(&bytes, &ArchiveSelection::All, &tight, scratch.path())
                .unwrap_err();
        assert_eq!(error.code(), ToolErrorCode::ARCHIVE_LIMIT_EXCEEDED);
        let entries = std::fs::read_dir(scratch.path()).unwrap().count();
        assert_eq!(entries, 0);
    }

    #[test]
    fn tar_mode_and_mtime_are_metadata_only() {
        let bytes = tar_single_with_mode("script.sh", b"#!/bin/sh\n", 0o700);
        let listing = list_archive(&bytes, &limits()).unwrap();
        assert_eq!(listing.entries[0].mode, Some(0o700));
        assert!(listing.entries[0].modified_at.is_some());
        let scratch = TempDir::new().unwrap();
        let extraction =
            extract_archive_to_scratch(&bytes, &ArchiveSelection::All, &limits(), scratch.path())
                .unwrap();
        let mode = std::fs::metadata(extraction.entries[0].scratch_path.as_ref().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_ne!(
            mode, 0o700,
            "archive modes are not applied to scratch files"
        );
    }

    #[test]
    fn detects_zip_tar_and_tar_gz_from_bytes() {
        assert_eq!(
            detect_format(&zip_bytes(&[("a.txt", b"a")])).unwrap(),
            ArchiveFormat::Zip
        );
        assert_eq!(
            detect_format(&tar_bytes(&[("a.txt", b"a")])).unwrap(),
            ArchiveFormat::Tar
        );
        assert_eq!(
            detect_format(&gzip_tar(&[("a.txt", b"a")])).unwrap(),
            ArchiveFormat::TarGz
        );
        assert_eq!(
            detect_format(b"just text").unwrap_err(),
            ArchiveError::UnsupportedFormat
        );
    }

    #[test]
    fn lists_zip_entries_with_sizes_and_safe_flags() {
        let bytes = zip_bytes(&[("docs/a.txt", b"hello"), ("docs/b.bin", &[0, 1, 2])]);
        let listing = list_archive(&bytes, &limits()).unwrap();
        assert_eq!(listing.format, ArchiveFormat::Zip);
        assert_eq!(listing.entry_count, 2);
        assert_eq!(listing.entries[0].path, "docs/a.txt");
        assert_eq!(listing.entries[0].size_bytes, 5);
        assert!(listing.entries[0].safe);
        assert!(listing.entries[1].safe);
        assert_eq!(listing.total_uncompressed_bytes, 8);
    }

    #[test]
    fn lists_tar_and_tar_gz_entries() {
        for bytes in [
            tar_bytes(&[("a.txt", b"one"), ("dir/b.txt", b"two")]),
            gzip_tar(&[("a.txt", b"one"), ("dir/b.txt", b"two")]),
        ] {
            let listing = list_archive(&bytes, &limits()).unwrap();
            assert_eq!(listing.entry_count, 2);
            assert!(listing.entries.iter().all(|entry| entry.safe));
            assert_eq!(
                listing
                    .entries
                    .iter()
                    .map(|entry| entry.path.as_str())
                    .collect::<Vec<_>>(),
                vec!["a.txt", "dir/b.txt"]
            );
        }
    }

    #[test]
    fn rejects_zip_slip_and_absolute_paths() {
        let bytes = zip_bytes(&[("../escape.txt", b"bad"), ("/abs.txt", b"bad")]);
        let listing = list_archive(&bytes, &limits()).unwrap();
        assert!(!listing.entries[0].safe);
        assert_eq!(
            listing.entries[0].unsafe_reason.as_deref(),
            Some("path_traversal")
        );
        assert!(!listing.entries[1].safe);
        assert_eq!(
            listing.entries[1].unsafe_reason.as_deref(),
            Some("absolute_path")
        );
        let scratch = TempDir::new().unwrap();
        let error = extract_archive_to_scratch(
            &bytes,
            &ArchiveSelection::Paths(vec!["../escape.txt".to_string()]),
            &limits(),
            scratch.path(),
        )
        .unwrap_err();
        assert_eq!(error.code(), ToolErrorCode::ARCHIVE_UNSAFE_ENTRY);
        assert!(!scratch.path().join("escape.txt").exists());
    }

    #[test]
    fn rejects_backslash_traversal() {
        let bytes = zip_bytes(&[("..\\escape.txt", b"bad")]);
        let listing = list_archive(&bytes, &limits()).unwrap();
        assert!(!listing.entries[0].safe);
        assert_eq!(
            listing.entries[0].unsafe_reason.as_deref(),
            Some("path_traversal")
        );
    }

    #[test]
    fn rejects_tar_traversal() {
        let bytes = patch_tar_name(tar_bytes(&[("safe.txt", b"bad")]), "../../etc/passwd");
        let listing = list_archive(&bytes, &limits()).unwrap();
        assert!(!listing.entries[0].safe);
        assert_eq!(
            listing.entries[0].unsafe_reason.as_deref(),
            Some("path_traversal")
        );
    }

    #[test]
    fn rejects_tar_symlink_hardlink_and_device_entries() {
        for (entry_type, kind, link) in [
            (tar::EntryType::Symlink, "symlink", Some("target")),
            (tar::EntryType::Link, "hardlink", Some("target")),
            (tar::EntryType::Char, "device", None),
            (tar::EntryType::Block, "device", None),
            (tar::EntryType::Fifo, "fifo", None),
        ] {
            let bytes = tar_with_header("entry", entry_type, b"", link);
            let listing = list_archive(&bytes, &limits()).unwrap();
            assert_eq!(listing.entries[0].kind, kind);
            assert!(!listing.entries[0].safe, "{kind} must be unsafe");
            assert!(
                listing.entries[0]
                    .unsafe_reason
                    .as_deref()
                    .unwrap_or("")
                    .starts_with("unsupported_entry_kind")
            );
        }
    }

    #[test]
    fn rejects_zip_symlink_mode() {
        let bytes = patch_zip_symlink(zip_bytes(&[("link", b"target")]));
        let listing = list_archive(&bytes, &limits()).unwrap();
        assert_eq!(listing.entries[0].kind, "symlink");
        assert!(!listing.entries[0].safe);
    }

    fn patch_zip_symlink(mut bytes: Vec<u8>) -> Vec<u8> {
        let central = bytes
            .windows(4)
            .position(|window| window == b"PK\x01\x02")
            .expect("central directory header");
        // version made by = Unix (3) + 0x1e, then external attributes hold the
        // symlink mode in the high 16 bits.
        bytes[central + 4..central + 6].copy_from_slice(&0x031eu16.to_le_bytes());
        bytes[central + 38..central + 42].copy_from_slice(&(0o120777u32 << 16).to_le_bytes());
        bytes
    }

    #[test]
    fn rejects_duplicate_normalized_entries() {
        let mut buffer = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buffer);
            let options = zip::write::SimpleFileOptions::default();
            writer.start_file("./same.txt", options).unwrap();
            writer.write_all(b"first").unwrap();
            writer.start_file("same.txt", options).unwrap();
            writer.write_all(b"second").unwrap();
            writer.finish().unwrap();
        }
        let bytes = buffer.into_inner();
        let listing = list_archive(&bytes, &limits()).unwrap();
        assert_eq!(listing.entries.len(), 2);
        assert!(listing.entries[0].safe);
        assert!(!listing.entries[1].safe);
        assert!(listing.entries[1].duplicate);
        assert_eq!(
            listing.entries[1].unsafe_reason.as_deref(),
            Some("duplicate_entry")
        );
        let scratch = TempDir::new().unwrap();
        let error = extract_archive_to_scratch(
            &bytes,
            &ArchiveSelection::Paths(vec!["same.txt".to_string()]),
            &limits(),
            scratch.path(),
        )
        .unwrap_err();
        assert_eq!(error.code(), ToolErrorCode::ARCHIVE_UNSAFE_ENTRY);
    }

    #[test]
    fn enforces_entry_count_and_entry_size_caps() {
        let bytes = zip_bytes(&[("a.txt", b"a"), ("b.txt", b"b"), ("c.txt", b"c")]);
        let mut tight = limits();
        tight.max_entries = 2;
        let listing = list_archive(&bytes, &tight).unwrap();
        assert!(listing.truncated);
        assert_eq!(listing.entries.len(), 2);
        let scratch = TempDir::new().unwrap();
        let error =
            extract_archive_to_scratch(&bytes, &ArchiveSelection::All, &tight, scratch.path())
                .unwrap_err();
        assert_eq!(error.code(), ToolErrorCode::ARCHIVE_LIMIT_EXCEEDED);

        let big = vec![b'x'; 2_048];
        let bytes = zip_bytes(&[("big.bin", &big)]);
        let mut small = limits();
        small.max_entry_bytes = 1_024;
        let listing = list_archive(&bytes, &small).unwrap();
        assert!(!listing.entries[0].safe);
        assert_eq!(
            listing.entries[0].unsafe_reason.as_deref(),
            Some("entry_too_large")
        );
    }

    #[test]
    fn enforces_total_uncompressed_cap() {
        let bytes = zip_bytes(&[("a.bin", &vec![b'a'; 1024]), ("b.bin", &vec![b'b'; 1024])]);
        let mut tight = limits();
        tight.max_total_bytes = 1_500;
        let listing = list_archive(&bytes, &tight).unwrap();
        assert!(
            listing
                .warnings
                .iter()
                .any(|warning| warning == "total_uncompressed_bytes_exceeded")
        );
        assert!(listing.entries.iter().all(|entry| !entry.safe));
        let scratch = TempDir::new().unwrap();
        let error =
            extract_archive_to_scratch(&bytes, &ArchiveSelection::All, &tight, scratch.path())
                .unwrap_err();
        assert_eq!(error.code(), ToolErrorCode::ARCHIVE_LIMIT_EXCEEDED);
    }

    #[test]
    fn rejects_compression_bomb_ratio() {
        let payload = vec![b'A'; 64 * 1024];
        let bytes = zip_bytes(&[("bomb.txt", &payload)]);
        let mut tight = limits();
        tight.max_compression_ratio = 2;
        let listing = list_archive(&bytes, &tight).unwrap();
        assert!(!listing.entries[0].safe);
        assert_eq!(
            listing.entries[0].unsafe_reason.as_deref(),
            Some("compression_ratio_exceeded")
        );
    }

    #[test]
    fn rejects_encrypted_zip() {
        let bytes = encrypted_zip_bytes();
        let error = list_archive(&bytes, &limits()).unwrap_err();
        assert_eq!(error, ArchiveError::Encrypted);
        assert_eq!(error.code(), ToolErrorCode::ARCHIVE_ENCRYPTED);
    }

    #[test]
    fn rejects_malformed_containers() {
        let mut zip = zip_bytes(&[("a.txt", b"a")]);
        zip.truncate(zip.len() / 2);
        assert!(matches!(
            list_archive(&zip, &limits()),
            Err(ArchiveError::Malformed(_))
        ));

        let mut tar = tar_bytes(&[("a.txt", b"a")]);
        tar[0] = b'X';
        tar[1] = b'X';
        assert!(matches!(
            list_archive(&tar, &limits()),
            Err(ArchiveError::UnsupportedFormat | ArchiveError::Malformed(_))
        ));
    }

    #[test]
    fn extracts_selected_entries_into_scratch_only() {
        let bytes = zip_bytes(&[("a.txt", b"alpha"), ("dir/b.txt", b"beta")]);
        let scratch = TempDir::new().unwrap();
        let extraction = extract_archive_to_scratch(
            &bytes,
            &ArchiveSelection::Paths(vec!["dir/b.txt".to_string()]),
            &limits(),
            scratch.path(),
        )
        .unwrap();
        assert_eq!(extraction.entries.len(), 1);
        let entry = &extraction.entries[0];
        assert_eq!(entry.path, "dir/b.txt");
        assert_eq!(entry.size_bytes, 4);
        assert!(entry.sha256.is_some());
        let path = entry.scratch_path.as_ref().unwrap();
        assert!(path.starts_with(scratch.path().canonicalize().unwrap()));
        assert_eq!(std::fs::read(path).unwrap(), b"beta");
        assert!(!scratch.path().join("a.txt").exists());
    }

    #[test]
    fn extracts_all_safe_files_and_skips_unsafe_paths() {
        let bytes = zip_bytes(&[("ok.txt", b"ok"), ("../bad.txt", b"bad")]);
        let scratch = TempDir::new().unwrap();
        let extraction =
            extract_archive_to_scratch(&bytes, &ArchiveSelection::All, &limits(), scratch.path())
                .unwrap();
        assert_eq!(extraction.entries.len(), 1);
        assert_eq!(extraction.entries[0].path, "ok.txt");
        assert!(!scratch.path().join("bad.txt").exists());
    }

    #[test]
    fn extraction_requires_explicit_selection() {
        let bytes = zip_bytes(&[("a.txt", b"a")]);
        let scratch = TempDir::new().unwrap();
        let error = extract_archive_to_scratch(
            &bytes,
            &ArchiveSelection::Paths(Vec::new()),
            &limits(),
            scratch.path(),
        )
        .unwrap_err();
        assert_eq!(error, ArchiveError::EmptySelection);
    }

    #[test]
    fn extraction_rejects_missing_entry() {
        let bytes = zip_bytes(&[("a.txt", b"a")]);
        let scratch = TempDir::new().unwrap();
        let error = extract_archive_to_scratch(
            &bytes,
            &ArchiveSelection::Paths(vec!["missing.txt".to_string()]),
            &limits(),
            scratch.path(),
        )
        .unwrap_err();
        assert!(matches!(error, ArchiveError::EntryNotFound(_)));
    }

    #[test]
    fn extraction_rejects_all_and_entries_together() {
        let params = ArtifactArchiveExtractParams {
            artifact_id: "art".to_string(),
            entries: vec!["a".to_string()],
            all: true,
            max_entries: None,
            include_previews: None,
        };
        assert!(matches!(
            ArchiveSelection::from_params(&params),
            Err(ArchiveError::UnsafeEntry(_))
        ));
    }

    #[test]
    fn path_normalization_handles_dots_and_windows_separators() {
        assert_eq!(normalize_entry_path("./a/./b.txt").unwrap(), "a/b.txt");
        assert_eq!(normalize_entry_path("a\\b.txt").unwrap(), "a/b.txt");
        assert_eq!(
            normalize_entry_path("C:\\evil").unwrap_err(),
            "absolute_path"
        );
        assert_eq!(
            normalize_entry_path("a/../b").unwrap_err(),
            "path_traversal"
        );
        assert_eq!(normalize_entry_path("").unwrap_err(), "empty_path");
        assert_eq!(normalize_entry_path("a\0b").unwrap_err(), "nul_byte");
    }

    #[test]
    fn display_path_never_contains_control_characters() {
        assert_eq!(safe_display_path("a\nb\tc"), "abc");
        assert_eq!(safe_display_path(""), "(empty)");
    }

    /// Set the encryption bit in the local and central headers of a valid ZIP.
    /// The listing path must fail on the flag before attempting decompression,
    /// so a valid deflate stream is still present.
    fn encrypted_zip_bytes() -> Vec<u8> {
        let mut bytes = zip_bytes(&[("secret.txt", b"secret")]);
        let local_flags = 6;
        let flags = u16::from_le_bytes([bytes[local_flags], bytes[local_flags + 1]]) | 1;
        bytes[local_flags..local_flags + 2].copy_from_slice(&flags.to_le_bytes());
        let central = bytes
            .windows(4)
            .position(|window| window == b"PK\x01\x02")
            .expect("central directory header");
        let central_flags = central + 8;
        let flags = u16::from_le_bytes([bytes[central_flags], bytes[central_flags + 1]]) | 1;
        bytes[central_flags..central_flags + 2].copy_from_slice(&flags.to_le_bytes());
        bytes
    }
}
