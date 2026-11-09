use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use ignore::WalkBuilder;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use vp_protocol::types::{
    WorkspaceEntry, WorkspaceFileResult, WorkspaceGrepMatch, WorkspaceGrepResult,
    WorkspaceListResult, WorkspaceSearchResult, WorkspaceTreeResult, WorkspaceWriteResult,
};

const MAX_ENTRIES: usize = 5_000;
const MAX_DEPTH: usize = 20;
const MAX_FILE_BYTES: u64 = 512 * 1024;
const BINARY_PREVIEW_BYTES: usize = 1_024;

pub fn tree(root: &Path) -> Result<WorkspaceTreeResult> {
    let root = root.canonicalize().context("resolving workspace root")?;
    let mut entries = Vec::new();
    let mut truncated = false;
    let walker = WalkBuilder::new(&root)
        .hidden(false)
        .max_depth(Some(MAX_DEPTH))
        .filter_entry(|entry| {
            !entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(".git")
        })
        .build();
    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let path = entry.path();
        if path == root {
            continue;
        }
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        let kind = if file_type.is_dir() {
            "dir"
        } else if file_type.is_file() {
            "file"
        } else {
            continue;
        };
        let relative = match path.strip_prefix(&root) {
            Ok(relative) => relative.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy().to_string();
        let size = if kind == "file" {
            entry.metadata().map(|metadata| metadata.len()).unwrap_or(0)
        } else {
            0
        };
        entries.push(WorkspaceEntry {
            path: relative,
            name,
            kind: kind.to_string(),
            size,
        });
        if entries.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }
    }
    entries.sort_by(|left, right| {
        (left.kind != "dir", left.path.to_lowercase())
            .cmp(&(right.kind != "dir", right.path.to_lowercase()))
    });
    Ok(WorkspaceTreeResult {
        root: root.to_string_lossy().to_string(),
        entries,
        truncated,
    })
}

pub fn read(root: &Path, relative: &str) -> Result<WorkspaceFileResult> {
    let root = root.canonicalize().context("resolving workspace root")?;
    let path = resolve(&root, relative)?;
    let metadata = std::fs::metadata(&path).context("reading file metadata")?;
    if metadata.is_dir() {
        bail!("path is a directory: {relative}");
    }
    let mut file = File::open(&path).context("opening workspace file")?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("reading workspace file")?;
    let mut truncated = bytes.len() as u64 > MAX_FILE_BYTES;
    if truncated {
        bytes.truncate(MAX_FILE_BYTES as usize);
    }
    let binary =
        bytes.iter().take(8 * 1024).any(|byte| *byte == 0) || std::str::from_utf8(&bytes).is_err();
    let content = if binary {
        let preview = &bytes[..bytes.len().min(BINARY_PREVIEW_BYTES)];
        truncated = truncated || bytes.len() > BINARY_PREVIEW_BYTES;
        hex_preview(preview)
    } else {
        String::from_utf8_lossy(&bytes).to_string()
    };
    Ok(WorkspaceFileResult {
        path: relative.to_string(),
        size: metadata.len(),
        truncated,
        binary,
        content,
        sha256: sha256_hex(&bytes),
        editable: !truncated && !binary,
    })
}

pub fn write(
    root: &Path,
    relative: &str,
    content: &str,
    expected_sha256: Option<&str>,
) -> Result<WorkspaceWriteResult> {
    if content.len() as u64 > MAX_FILE_BYTES {
        bail!("file content exceeds {MAX_FILE_BYTES} bytes");
    }
    let root = root.canonicalize().context("resolving workspace root")?;
    let path = resolve_for_write(&root, relative)?;
    if let Some(expected) = expected_sha256 {
        let current = std::fs::read(&path).context("reading file for conflict check")?;
        if current.len() as u64 > MAX_FILE_BYTES {
            bail!("file is too large to edit safely");
        }
        let actual = sha256_hex(&current);
        if actual != expected {
            bail!("file changed on disk; reload before saving");
        }
    }
    let bytes = content.as_bytes();
    let temp = path
        .parent()
        .context("file has no parent")?
        .join(format!(".verlinpark-{}.tmp", Uuid::now_v7()));
    let result = (|| -> Result<()> {
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .context("creating temporary file")?;
            file.write_all(bytes).context("writing temporary file")?;
            file.sync_all().context("syncing temporary file")?;
        }
        if let Ok(metadata) = std::fs::metadata(&path) {
            let _ = std::fs::set_permissions(&temp, metadata.permissions());
        }
        std::fs::rename(&temp, &path).context("replacing workspace file")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result?;
    Ok(WorkspaceWriteResult {
        path: relative.to_string(),
        size: bytes.len() as u64,
        sha256: sha256_hex(bytes),
    })
}

fn resolve_for_write(root: &Path, relative: &str) -> Result<PathBuf> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("workspace path must be relative and must not contain ..");
    }
    if relative_path.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(".git")
    }) {
        bail!("workspace path must not target .git");
    }
    let candidate = root.join(relative_path);
    if candidate.exists() {
        let resolved = candidate
            .canonicalize()
            .with_context(|| format!("resolving workspace path {relative}"))?;
        if !resolved.starts_with(root) {
            bail!("workspace path escapes the session worktree");
        }
        if resolved.is_dir() {
            bail!("path is a directory: {relative}");
        }
        let canonical_relative = resolved.strip_prefix(root).unwrap_or(&resolved);
        if canonical_relative.components().any(|component| {
            component
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(".git")
        }) {
            bail!("workspace path must not target .git");
        }
        return Ok(resolved);
    }
    let parent = candidate
        .parent()
        .context("workspace path has no parent")?
        .canonicalize()
        .with_context(|| format!("resolving parent for {relative}"))?;
    if !parent.starts_with(root) {
        bail!("workspace path escapes the session worktree");
    }
    let parent_relative = parent.strip_prefix(root).unwrap_or(&parent);
    if parent_relative.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(".git")
    }) {
        bail!("workspace path must not target .git");
    }
    let name = candidate
        .file_name()
        .context("workspace path has no file name")?;
    Ok(parent.join(name))
}

fn resolve(root: &Path, relative: &str) -> Result<PathBuf> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("workspace path must be relative and must not contain ..");
    }
    let candidate = root.join(relative_path);
    let resolved = candidate
        .canonicalize()
        .with_context(|| format!("resolving workspace path {relative}"))?;
    if !resolved.starts_with(root) {
        bail!("workspace path escapes the session worktree");
    }
    let relative = resolved.strip_prefix(root).unwrap_or(&resolved);
    if relative.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(".git")
    }) {
        bail!("workspace path must not target .git");
    }
    Ok(resolved)
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn hex_preview(bytes: &[u8]) -> String {
    let mut output = String::new();
    for (index, chunk) in bytes.chunks(16).enumerate() {
        let offset = index * 16;
        let hex = chunk
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        let ascii = chunk
            .iter()
            .map(|byte| {
                if byte.is_ascii_graphic() || *byte == b' ' {
                    *byte as char
                } else {
                    '.'
                }
            })
            .collect::<String>();
        output.push_str(&format!("{offset:08x}  {hex:<47}  {ascii}\n"));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn lists_files_and_hides_git() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join("README.md"), "hello").unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join(".git/config"), "secret").unwrap();
        let tree = tree(dir.path()).unwrap();
        assert!(tree.entries.iter().any(|entry| entry.path == "README.md"));
        assert!(
            tree.entries
                .iter()
                .any(|entry| entry.path == "src/main.rs" && entry.kind == "file")
        );
        assert!(!tree.entries.iter().any(|entry| entry.path.contains(".git")));
        assert_eq!(tree.entries[0].kind, "dir");
    }

    #[test]
    fn reads_text_and_binary_previews() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "hello workspace").unwrap();
        let file = read(dir.path(), "notes.txt").unwrap();
        assert_eq!(file.content, "hello workspace");
        assert!(!file.binary);
        assert!(!file.truncated);
        assert!(file.editable);
        assert_eq!(file.sha256.len(), 64);

        let binary = [0_u8, 159, 146, 150, 10];
        std::fs::write(dir.path().join("blob.bin"), binary).unwrap();
        let file = read(dir.path(), "blob.bin").unwrap();
        assert!(file.binary);
        assert!(!file.editable);
        assert!(file.content.starts_with("00000000"));
        assert!(file.content.contains("00 9f 92 96 0a"));
    }

    #[test]
    fn rejects_paths_outside_the_worktree() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        assert!(read(dir.path(), "../secret.txt").is_err());
        assert!(read(dir.path(), outside.path().to_str().unwrap()).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                outside.path().join("secret.txt"),
                dir.path().join("escape.txt"),
            )
            .unwrap();
            assert!(read(dir.path(), "escape.txt").is_err());
        }
    }

    #[test]
    fn writes_files_with_conflict_detection() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "old").unwrap();
        let file = read(dir.path(), "notes.txt").unwrap();
        let written = write(dir.path(), "notes.txt", "new content", Some(&file.sha256)).unwrap();
        assert_eq!(written.size, 11);
        assert_eq!(
            read(dir.path(), "notes.txt").unwrap().content,
            "new content"
        );
        assert!(write(dir.path(), "notes.txt", "stale", Some(&file.sha256)).is_err());

        let created = write(dir.path(), "new.txt", "created", None).unwrap();
        assert_eq!(created.path, "new.txt");
        assert_eq!(read(dir.path(), "new.txt").unwrap().content, "created");
    }

    #[test]
    fn rejects_writes_outside_the_worktree() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        assert!(write(dir.path(), "../escape.txt", "nope", None).is_err());
        assert!(write(dir.path(), outside.path().to_str().unwrap(), "nope", None).is_err());
        assert!(write(dir.path(), ".git/config", "nope", None).is_err());
        assert!(
            write(
                dir.path(),
                "too-large.txt",
                &"a".repeat(MAX_FILE_BYTES as usize + 1),
                None
            )
            .is_err()
        );
    }

    #[test]
    fn truncates_large_files() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("large.txt"),
            vec![b'a'; MAX_FILE_BYTES as usize + 1],
        )
        .unwrap();
        let file = read(dir.path(), "large.txt").unwrap();
        assert!(file.truncated);
        assert_eq!(file.size, MAX_FILE_BYTES + 1);
        assert_eq!(file.content.len(), MAX_FILE_BYTES as usize);
    }
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("src/nested")).unwrap();
        fs::create_dir_all(dir.path().join("docs")).unwrap();
        fs::create_dir_all(dir.path().join(".git/objects")).unwrap();
        fs::write(dir.path().join("README.md"), "hello").unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(dir.path().join("src/nested/deep.rs"), "deep").unwrap();
        fs::write(dir.path().join("docs/guide.md"), "guide").unwrap();
        fs::write(dir.path().join(".git/config"), "secret").unwrap();
        dir
    }

    #[test]
    fn lists_one_directory_with_dirs_first_and_pagination() {
        let dir = fixture();
        let root = list(dir.path(), "", Some(0), Some(2)).unwrap();
        assert_eq!(root.entries.len(), 2);
        assert_eq!(root.entries[0].kind, "dir");
        assert_eq!(root.entries[1].kind, "dir");
        assert_eq!(root.total, 3);
        assert!(root.has_more);
        assert!(!root.truncated);
        assert!(!root.entries.iter().any(|entry| entry.name == ".git"));

        let page = list(dir.path(), "", Some(2), Some(2)).unwrap();
        assert_eq!(page.entries.len(), 1);
        assert!(page.entries.iter().all(|entry| entry.kind == "file"));
        assert!(!page.has_more);

        let nested = list(dir.path(), "src/nested", None, None).unwrap();
        assert_eq!(nested.path, "src/nested");
        assert_eq!(nested.entries.len(), 1);
        assert_eq!(nested.entries[0].path, "src/nested/deep.rs");
    }

    #[test]
    fn list_rejects_escape_and_git_paths() {
        let dir = fixture();
        assert!(list(dir.path(), "../", None, None).is_err());
        assert!(list(dir.path(), "/tmp", None, None).is_err());
        assert!(list(dir.path(), ".git", None, None).is_err());
        assert!(list(dir.path(), "README.md", None, None).is_err());
    }

    #[test]
    fn searches_names_in_nested_directories() {
        let dir = fixture();
        let result = search(dir.path(), "deep", Some(10)).unwrap();
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].path, "src/nested/deep.rs");
        assert!(!result.truncated);

        let guide = search(dir.path(), "GUIDE", None).unwrap();
        assert_eq!(guide.entries[0].path, "docs/guide.md");

        assert!(search(dir.path(), "   ", None).is_err());
        let limited = search(dir.path(), ".rs", Some(1)).unwrap();
        assert_eq!(limited.entries.len(), 1);
        assert!(limited.truncated);
    }
}

const DEFAULT_LIST_LIMIT: usize = 500;
const MAX_LIST_LIMIT: usize = 2_000;
const MAX_LIST_ENTRIES: usize = 20_000;
const DEFAULT_SEARCH_LIMIT: usize = 300;
const MAX_SEARCH_LIMIT: usize = 1_000;

/// List one directory level. The file browser uses this instead of walking the
/// whole worktree on every open, so large repositories stay responsive.
pub fn list(
    root: &Path,
    relative: &str,
    offset: Option<u32>,
    limit: Option<u32>,
) -> Result<WorkspaceListResult> {
    let root = root.canonicalize().context("resolving workspace root")?;
    let directory = resolve_directory(&root, relative)?;
    let offset = offset.unwrap_or(0) as usize;
    let limit = limit
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, MAX_LIST_LIMIT);
    let mut entries = Vec::new();
    let mut truncated = false;
    for entry in std::fs::read_dir(&directory).context("reading workspace directory")? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy().to_string();
        if name.eq_ignore_ascii_case(".git") {
            continue;
        }
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            continue;
        }
        let kind = if file_type.is_dir() {
            "dir"
        } else if file_type.is_file() {
            "file"
        } else {
            continue;
        };
        let path = entry.path();
        let relative_path = path
            .strip_prefix(&root)
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_else(|_| name.clone());
        let size = if kind == "file" {
            entry.metadata().map(|metadata| metadata.len()).unwrap_or(0)
        } else {
            0
        };
        entries.push(WorkspaceEntry {
            path: relative_path,
            name,
            kind: kind.to_string(),
            size,
        });
        if entries.len() >= MAX_LIST_ENTRIES {
            truncated = true;
            break;
        }
    }
    entries.sort_by(|left, right| {
        (left.kind != "dir", left.name.to_lowercase())
            .cmp(&(right.kind != "dir", right.name.to_lowercase()))
    });
    let total = entries.len();
    let page = entries
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    Ok(WorkspaceListResult {
        root: root.to_string_lossy().to_string(),
        path: relative.trim_matches('/').to_string(),
        entries: page,
        total,
        has_more: offset.saturating_add(limit) < total,
        truncated,
    })
}

/// Name-only, bounded search used by the Files filter. It still walks the
/// worktree, but only on an explicit query and never returns more than the
/// configured page.
pub fn search(root: &Path, query: &str, limit: Option<u32>) -> Result<WorkspaceSearchResult> {
    let query = query.trim();
    if query.is_empty() || query.chars().count() > 200 {
        bail!("workspace search query must be 1-200 characters");
    }
    let limit = limit
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_SEARCH_LIMIT)
        .clamp(1, MAX_SEARCH_LIMIT);
    let root = root.canonicalize().context("resolving workspace root")?;
    let needle = query.to_lowercase();
    let mut entries = Vec::new();
    let mut truncated = false;
    let walker = WalkBuilder::new(&root)
        .hidden(false)
        .max_depth(Some(MAX_DEPTH))
        .filter_entry(|entry| {
            !entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(".git")
        })
        .build();
    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let path = entry.path();
        let relative = match path.strip_prefix(&root) {
            Ok(relative) => relative.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy().to_string();
        if !relative.to_lowercase().contains(&needle) && !name.to_lowercase().contains(&needle) {
            continue;
        }
        let size = entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        entries.push(WorkspaceEntry {
            path: relative,
            name,
            kind: "file".to_string(),
            size,
        });
        if entries.len() >= limit {
            truncated = true;
            break;
        }
    }
    entries.sort_by_key(|entry| entry.path.to_lowercase());
    Ok(WorkspaceSearchResult { entries, truncated })
}

const DEFAULT_GREP_RESULTS: usize = 200;
const MAX_GREP_RESULTS: usize = 1_000;
const MAX_GREP_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_GREP_PREVIEW_CHARS: usize = 280;

/// Bounded literal text search across the session worktree. The daemon never
/// turns the query into a regular expression, so a hostile or accidentally
/// catastrophic pattern cannot consume unbounded CPU. Huge and non-UTF-8 files
/// are skipped, and the result cap stops the walk early.
pub fn grep(
    root: &Path,
    query: &str,
    relative: Option<&str>,
    case_sensitive: bool,
    max_results: Option<u32>,
) -> Result<WorkspaceGrepResult> {
    let query = query.trim();
    if query.is_empty() || query.chars().count() > 200 {
        bail!("workspace text search query must be 1-200 characters");
    }
    if query.contains('\0') {
        bail!("workspace text search query must not contain NUL");
    }
    let root = root.canonicalize().context("resolving workspace root")?;
    let search_root = match relative.filter(|value| !value.trim_matches('/').is_empty()) {
        Some(relative) => resolve_directory(&root, relative)?,
        None => root.clone(),
    };
    let max_results = max_results
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_GREP_RESULTS)
        .clamp(1, MAX_GREP_RESULTS);
    let needle = if case_sensitive {
        query.to_string()
    } else {
        query.to_lowercase()
    };
    let mut matches = Vec::new();
    let mut files_scanned = 0usize;
    let mut truncated = false;
    let walker = WalkBuilder::new(&search_root)
        .hidden(false)
        .max_depth(Some(MAX_DEPTH))
        .filter_entry(|entry| {
            !entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(".git")
        })
        .build();
    'files: for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() || file_type.is_symlink() {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.len() > MAX_GREP_FILE_BYTES {
            continue;
        }
        let path = entry.path();
        let Ok(relative_path) = path.strip_prefix(&root) else {
            continue;
        };
        let relative_path = relative_path.to_string_lossy().to_string();
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        if bytes.iter().take(8_192).any(|byte| *byte == 0) {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        files_scanned = files_scanned.saturating_add(1);
        for (index, line) in text.lines().enumerate() {
            let haystack = if case_sensitive {
                line.to_string()
            } else {
                line.to_lowercase()
            };
            let Some(byte_index) = haystack.find(&needle) else {
                continue;
            };
            let column = haystack[..byte_index].chars().count().saturating_add(1) as u32;
            matches.push(WorkspaceGrepMatch {
                path: relative_path.clone(),
                line: (index + 1) as u32,
                column,
                preview: grep_preview(line, MAX_GREP_PREVIEW_CHARS),
            });
            if matches.len() >= max_results {
                truncated = true;
                break 'files;
            }
        }
    }
    Ok(WorkspaceGrepResult {
        query: query.to_string(),
        matches,
        files_scanned,
        truncated,
    })
}

fn grep_preview(line: &str, limit: usize) -> String {
    let trimmed = line.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_string();
    }
    let mut preview = trimmed
        .chars()
        .take(limit.saturating_sub(1))
        .collect::<String>();
    preview.push('…');
    preview
}

fn resolve_directory(root: &Path, relative: &str) -> Result<PathBuf> {
    let relative = relative.trim_matches('/');
    if relative.is_empty() {
        return Ok(root.to_path_buf());
    }
    let path = Path::new(relative);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("workspace path must be relative and must not contain ..");
    }
    if path.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(".git")
    }) {
        bail!("workspace path must not target .git");
    }
    let resolved = root
        .join(path)
        .canonicalize()
        .with_context(|| format!("resolving workspace directory {relative}"))?;
    if !resolved.starts_with(root) {
        bail!("workspace path escapes the session worktree");
    }
    if !resolved.is_dir() {
        bail!("workspace path is not a directory: {relative}");
    }
    Ok(resolved)
}

#[cfg(test)]
mod grep_tests {
    use super::*;
    use std::fs;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("src/nested")).unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(
            dir.path().join("src/main.rs"),
            "fn main() {\n    let token = find_token();\n}\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/nested/helper.rs"),
            "pub fn helper() {\n    // TOKEN lives here too\n}\n",
        )
        .unwrap();
        fs::write(dir.path().join("README.md"), "token in the docs\n").unwrap();
        fs::write(dir.path().join(".git/config"), "token secret\n").unwrap();
        fs::write(dir.path().join("binary.bin"), b"token\0binary").unwrap();
        dir
    }

    #[test]
    fn finds_literal_matches_with_lines_and_columns() {
        let dir = fixture();
        let result = grep(dir.path(), "token", None, false, None).unwrap();
        assert_eq!(result.matches.len(), 3);
        assert_eq!(result.query, "token");
        assert!(result.files_scanned >= 3);
        assert!(!result.truncated);
        let main = result
            .matches
            .iter()
            .find(|item| item.path == "src/main.rs")
            .unwrap();
        assert_eq!(main.line, 2);
        assert_eq!(main.column, 9);
        assert!(result.matches.iter().any(|item| item.path == "README.md"));
        assert!(!result.matches.iter().any(|item| item.path.contains(".git")));
        assert!(!result.matches.iter().any(|item| item.path == "binary.bin"));
    }

    #[test]
    fn respects_case_path_filters_and_result_caps() {
        let dir = fixture();
        let upper = grep(dir.path(), "TOKEN", None, true, None).unwrap();
        assert_eq!(upper.matches.len(), 1);
        assert_eq!(upper.matches[0].path, "src/nested/helper.rs");

        let scoped = grep(dir.path(), "token", Some("src"), false, None).unwrap();
        assert_eq!(scoped.matches.len(), 2);
        assert!(
            scoped
                .matches
                .iter()
                .all(|item| item.path.starts_with("src/"))
        );

        let capped = grep(dir.path(), "token", None, false, Some(1)).unwrap();
        assert_eq!(capped.matches.len(), 1);
        assert!(capped.truncated);

        let lower = grep(dir.path(), "Token", None, false, None).unwrap();
        assert_eq!(lower.matches.len(), 3);
    }

    #[test]
    fn rejects_bad_queries_and_outside_paths() {
        let dir = fixture();
        assert!(grep(dir.path(), "   ", None, false, None).is_err());
        assert!(grep(dir.path(), "token", Some("../"), false, None).is_err());
        assert!(grep(dir.path(), "token", Some(".git"), false, None).is_err());
        assert!(grep(dir.path(), "token\0", None, false, None).is_err());
    }
}
