//! Filesystem helpers shared by agent tools: atomic writes, hashing, and
//! bounded text classification.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Write bytes through a same-directory temp file and atomic rename.
///
/// The existing file mode is copied where possible. The parent directory is
/// synced on Unix so a successful return means the rename is durable enough for
/// a local daemon restart.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("file has no parent directory: {}", path.display()))?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let existing_mode = std::fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());
    let temp = parent.join(format!(".vp-write-{}.tmp", Uuid::now_v7()));
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = options
            .open(&temp)
            .with_context(|| format!("creating temp file {}", temp.display()))?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        if let Some(permissions) = &existing_mode {
            let _ = file.set_permissions(permissions.clone());
        }
        drop(file);
        std::fs::rename(&temp, path).with_context(|| format!("replacing {}", path.display()))?;
        if let Ok(directory) = std::fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn sha256_file(path: &Path) -> Result<String> {
    let bytes =
        std::fs::read(path).with_context(|| format!("reading {} for hashing", path.display()))?;
    Ok(sha256_hex(&bytes))
}

pub(crate) fn utf8_line_count(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        text.lines().count()
    }
}

pub(crate) fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8_192).any(|byte| *byte == 0)
}

pub(crate) fn display_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}
