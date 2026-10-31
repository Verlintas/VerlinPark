use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use ignore::WalkBuilder;
use vp_protocol::types::{WorkspaceEntry, WorkspaceFileResult, WorkspaceTreeResult};

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
        .filter_entry(|entry| entry.file_name() != ".git")
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
    file.by_ref()
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
    })
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
    if relative
        .components()
        .any(|component| component.as_os_str() == ".git")
    {
        bail!("workspace path must not target .git");
    }
    Ok(resolved)
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

        let binary = [0_u8, 159, 146, 150, 10];
        std::fs::write(dir.path().join("blob.bin"), binary).unwrap();
        let file = read(dir.path(), "blob.bin").unwrap();
        assert!(file.binary);
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
}
