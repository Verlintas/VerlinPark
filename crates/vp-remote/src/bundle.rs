//! Bounded Git bundle export for remote integration.
//!
//! A bundle is generated into a helper-owned directory inside the canonical
//! remote root and read back through small base64 JSON-RPC chunks. The helper
//! never returns a whole bundle in one frame, and it stops generating when the
//! requested byte cap is exceeded. The daemon treats the bytes as untrusted:
//! it verifies the total SHA-256, the advertised head, and `git bundle verify`
//! before a temporary ref is created.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as Base64Engine;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::process::Command;

use crate::git;
use crate::jail::PathJail;
use crate::protocol::{
    BUNDLE_TTL_SECS, GitBundleCreateParams, GitBundleCreateResult, GitBundleDeleteParams,
    GitBundleDeleteResult, GitBundleReadParams, GitBundleReadResult, MAX_ACTIVE_BUNDLES,
    MAX_BUNDLE_ID_CHARS, MAX_GIT_BUNDLE_BYTES, MAX_GIT_BUNDLE_CHUNK_BYTES,
    MIN_GIT_BUNDLE_CHUNK_BYTES, RemoteError, RemoteErrorCode,
};

const BUNDLE_DIR_NAME: &str = "remote-bundles";
const BUNDLE_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_STDERR_BYTES: usize = 64 * 1024;
static BUNDLE_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct BundleRecord {
    pub id: String,
    pub path: PathBuf,
    pub branch: String,
    pub head: String,
    pub export_ref: String,
    pub bytes: u64,
    pub sha256: String,
    pub chunk_bytes: u64,
    pub expires_unix_ms: u64,
    pub created: Instant,
}

#[derive(Debug, Default)]
pub struct BundleRegistry {
    records: HashMap<String, BundleRecord>,
}

impl BundleRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn get(&self, bundle_id: &str) -> Option<&BundleRecord> {
        self.records.get(bundle_id)
    }

    pub fn insert(&mut self, record: BundleRecord) {
        self.records.insert(record.id.clone(), record);
    }

    pub fn remove(&mut self, bundle_id: &str) -> Option<BundleRecord> {
        self.records.remove(bundle_id)
    }

    /// Remove expired records and return their files/refs for best-effort
    /// cleanup. The caller must be able to run Git to delete export refs.
    pub fn take_expired(&mut self) -> Vec<BundleRecord> {
        let now = unix_millis();
        let expired = self
            .records
            .iter()
            .filter(|(_, record)| record.expires_unix_ms <= now)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        expired
            .into_iter()
            .filter_map(|id| self.records.remove(&id))
            .collect()
    }
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}

fn bundle_id() -> String {
    format!(
        "b{:x}{:x}{:x}",
        unix_millis(),
        std::process::id(),
        BUNDLE_COUNTER.fetch_add(1, Ordering::SeqCst)
    )
}

pub fn validate_bundle_id(value: &str) -> Result<(), RemoteError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > MAX_BUNDLE_ID_CHARS {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!("bundle_id must be 1..={MAX_BUNDLE_ID_CHARS} characters"),
        ));
    }
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            "bundle_id may contain only ASCII letters, digits, '-' and '_'",
        ));
    }
    Ok(())
}

/// Directory used for temporary bundle files. A normal checkout keeps them
/// inside `.git` so they never appear in `git status`; a linked worktree or a
/// non-standard root falls back to a hidden directory that is cleaned on
/// delete or expiry. Every path is canonicalized and checked against the root.
fn bundle_dir(root: &Path) -> Result<PathBuf, RemoteError> {
    let git_dir = root.join(".git");
    let candidate = if git_dir.is_dir() {
        git_dir.join("verlinpark").join(BUNDLE_DIR_NAME)
    } else {
        root.join(".verlinpark").join(BUNDLE_DIR_NAME)
    };
    std::fs::create_dir_all(&candidate).map_err(|error| {
        RemoteError::new(
            RemoteErrorCode::PermissionDenied,
            format!(
                "cannot create remote bundle scratch directory {}: {error}",
                candidate.display()
            ),
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o700));
    }
    let canonical = candidate.canonicalize().map_err(|error| {
        RemoteError::new(
            RemoteErrorCode::Internal,
            format!(
                "cannot canonicalize remote bundle scratch directory {}: {error}",
                candidate.display()
            ),
        )
    })?;
    if !canonical.starts_with(root) {
        return Err(RemoteError::new(
            RemoteErrorCode::PathEscape,
            format!(
                "remote bundle scratch directory escapes the configured root: {}",
                canonical.display()
            ),
        ));
    }
    Ok(canonical)
}

fn cleanup_record(jail: &PathJail, record: &BundleRecord) {
    let _ = std::fs::remove_file(&record.path);
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(jail.root())
        .args(["update-ref", "-d", &record.export_ref])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn cleanup_expired(jail: &PathJail, registry: &mut BundleRegistry) {
    for record in registry.take_expired() {
        cleanup_record(jail, &record);
    }
}

async fn run_ref_update(root: &Path, reference: &str, value: &str) -> Result<(), RemoteError> {
    let output = Command::new("git")
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-C")
        .arg(root)
        .args(["update-ref", reference, value])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .await
        .map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::GitUnavailable,
                format!("cannot run git update-ref on the remote host: {error}"),
            )
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(RemoteError::new(
            RemoteErrorCode::GitFailed,
            format!(
                "git update-ref {reference} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ))
    }
}

async fn delete_ref(root: &Path, reference: &str) {
    let _ = Command::new("git")
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-C")
        .arg(root)
        .args(["update-ref", "-d", reference])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
}

fn validate_create_params(
    params: &GitBundleCreateParams,
) -> Result<(String, Option<String>, u64), RemoteError> {
    let branch = git::validate_branch(&params.branch)?;
    let base = params
        .base
        .as_deref()
        .map(|base| git::validate_revision(base, "base"))
        .transpose()?;
    if params.max_bytes == 0 || params.max_bytes > MAX_GIT_BUNDLE_BYTES {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!("max_bytes must be 1..={MAX_GIT_BUNDLE_BYTES}"),
        ));
    }
    if !(MIN_GIT_BUNDLE_CHUNK_BYTES..=MAX_GIT_BUNDLE_CHUNK_BYTES).contains(&params.chunk_bytes) {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!(
                "chunk_bytes must be {MIN_GIT_BUNDLE_CHUNK_BYTES}..={MAX_GIT_BUNDLE_CHUNK_BYTES}"
            ),
        ));
    }
    Ok((branch, base, params.max_bytes))
}

/// Resolve the branch head and export exactly that commit through a temporary
/// ref. The export ref is deleted as soon as Git has written the bundle.
pub async fn create(
    jail: &PathJail,
    registry: &mut BundleRegistry,
    params: &GitBundleCreateParams,
) -> Result<GitBundleCreateResult, RemoteError> {
    let (branch, base, max_bytes) = validate_create_params(params)?;
    cleanup_expired(jail, registry);
    if registry.len() >= MAX_ACTIVE_BUNDLES {
        return Err(RemoteError::new(
            RemoteErrorCode::OutputLimit,
            format!(
                "the helper already holds {MAX_ACTIVE_BUNDLES} bundle exports; read or delete one first"
            ),
        )
        .with_hint("page and delete the previous bundle before starting another export"));
    }
    let resolved = git::rev_parse(
        jail,
        &crate::protocol::GitRevParseParams {
            rev: branch.clone(),
            commit: true,
        },
    )
    .await?;
    let head = resolved.value;
    let id = bundle_id();
    validate_bundle_id(&id)?;
    let export_ref = format!("refs/verlinpark/export/{id}");
    run_ref_update(jail.root(), &export_ref, &head).await?;
    let directory = match bundle_dir(jail.root()) {
        Ok(directory) => directory,
        Err(error) => {
            delete_ref(jail.root(), &export_ref).await;
            return Err(error);
        }
    };
    let path = directory.join(format!("{id}.bundle"));
    let result = stream_bundle(jail.root(), &path, &export_ref, base.as_deref(), max_bytes).await;
    delete_ref(jail.root(), &export_ref).await;
    let (bytes, sha256) = match result {
        Ok(result) => result,
        Err(error) => {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(error);
        }
    };
    let chunk_bytes = params.chunk_bytes;
    let chunks = bytes.div_ceil(chunk_bytes.max(1)).max(1) as u32;
    let expires_unix_ms = unix_millis().saturating_add(BUNDLE_TTL_SECS * 1_000);
    let record = BundleRecord {
        id: id.clone(),
        path,
        branch: branch.clone(),
        head: head.clone(),
        export_ref: export_ref.clone(),
        bytes,
        sha256: sha256.clone(),
        chunk_bytes,
        expires_unix_ms,
        created: Instant::now(),
    };
    registry.insert(record);
    Ok(GitBundleCreateResult {
        bundle_id: id,
        branch,
        head,
        export_ref,
        base,
        bytes,
        sha256,
        chunk_bytes,
        chunks,
        expires_at_unix_ms: expires_unix_ms,
        operation_id: params.operation_id.clone(),
        replayed: false,
    })
}

async fn stream_bundle(
    root: &Path,
    path: &Path,
    export_ref: &str,
    base: Option<&str>,
    max_bytes: u64,
) -> Result<(u64, String), RemoteError> {
    let mut command = Command::new("git");
    command
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-C")
        .arg(root)
        .args(["bundle", "create", "-", export_ref]);
    if let Some(base) = base {
        command.args(["--not", base]);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    git::git_environment(&mut command, root);
    let mut child = command.spawn().map_err(|error| {
        RemoteError::new(
            RemoteErrorCode::GitUnavailable,
            format!("cannot run git bundle on the remote host: {error}"),
        )
    })?;
    let mut stdout = child.stdout.take().ok_or_else(|| {
        RemoteError::new(
            RemoteErrorCode::Internal,
            "git bundle did not expose stdout",
        )
    })?;
    let stderr_tail = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    if let Some(mut stderr) = child.stderr.take() {
        let tail = stderr_tail.clone();
        tokio::spawn(async move {
            let mut buffer = Vec::new();
            let _ = stderr.read_to_end(&mut buffer).await;
            if let Ok(mut tail) = tail.lock() {
                let start = buffer.len().saturating_sub(MAX_STDERR_BYTES);
                *tail = String::from_utf8_lossy(&buffer[start..]).to_string();
            }
        });
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .await
        .map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::PermissionDenied,
                format!(
                    "cannot create remote bundle file {}: {error}",
                    path.display()
                ),
            )
        })?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = vec![0u8; 256 * 1024];
    let outcome = tokio::time::timeout(BUNDLE_TIMEOUT, async {
        loop {
            let count = match stdout.read(&mut buffer).await {
                Ok(0) => break Ok(()),
                Ok(count) => count,
                Err(error) => {
                    break Err(RemoteError::new(
                        RemoteErrorCode::GitFailed,
                        format!("reading git bundle output failed: {error}"),
                    ));
                }
            };
            total = total.saturating_add(count as u64);
            if total > max_bytes {
                let _ = child.start_kill();
                break Err(RemoteError::new(
                    RemoteErrorCode::FileTooLarge,
                    format!(
                        "remote bundle exceeded the configured {max_bytes}-byte cap; no bundle was returned"
                    ),
                )
                .with_hint("integrate a smaller branch or raise RemoteIntegrationPolicy.max_bundle_bytes"));
            }
            hasher.update(&buffer[..count]);
            if let Err(error) = file.write_all(&buffer[..count]).await {
                let _ = child.start_kill();
                break Err(RemoteError::new(
                    RemoteErrorCode::Internal,
                    format!("writing remote bundle failed: {error}"),
                ));
            }
        }
    })
    .await;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(_) => {
            let _ = child.start_kill();
            Err(RemoteError::new(
                RemoteErrorCode::Timeout,
                format!(
                    "git bundle exceeded the {} second export timeout",
                    BUNDLE_TIMEOUT.as_secs()
                ),
            ))
        }
    };
    if let Err(error) = outcome {
        let _ = child.wait().await;
        return Err(error);
    }
    let status = child.wait().await.map_err(|error| {
        RemoteError::new(
            RemoteErrorCode::GitFailed,
            format!("git bundle did not exit cleanly: {error}"),
        )
    })?;
    if !status.success() {
        let detail = stderr_tail
            .lock()
            .map(|value| value.trim().to_string())
            .unwrap_or_default();
        return Err(RemoteError::new(
            RemoteErrorCode::GitFailed,
            if detail.is_empty() {
                format!("git bundle create {export_ref} failed")
            } else {
                format!("git bundle create {export_ref} failed: {detail}")
            },
        ));
    }
    file.sync_all().await.map_err(|error| {
        RemoteError::new(
            RemoteErrorCode::Internal,
            format!("syncing remote bundle failed: {error}"),
        )
    })?;
    let metadata = file.metadata().await.map_err(|error| {
        RemoteError::new(
            RemoteErrorCode::Internal,
            format!("inspecting remote bundle failed: {error}"),
        )
    })?;
    if metadata.len() > max_bytes {
        return Err(RemoteError::new(
            RemoteErrorCode::FileTooLarge,
            format!(
                "remote bundle is {} bytes; the cap is {max_bytes}",
                metadata.len()
            ),
        ));
    }
    if metadata.len() != total {
        return Err(RemoteError::new(
            RemoteErrorCode::Internal,
            "remote bundle size changed while it was written",
        ));
    }
    Ok((total, format!("{:x}", hasher.finalize())))
}

pub async fn read(
    registry: &mut BundleRegistry,
    params: &GitBundleReadParams,
) -> Result<GitBundleReadResult, RemoteError> {
    validate_bundle_id(&params.bundle_id)?;
    if params.limit == 0 || params.limit > MAX_GIT_BUNDLE_CHUNK_BYTES {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!("bundle read limit must be 1..={MAX_GIT_BUNDLE_CHUNK_BYTES}"),
        ));
    }
    let record = registry.get(&params.bundle_id).cloned().ok_or_else(|| {
        RemoteError::new(
            RemoteErrorCode::PathNotFound,
            format!(
                "bundle export {} is not available; it may have expired",
                params.bundle_id
            ),
        )
        .with_hint("create a new bundle export and page it before the TTL elapses")
    })?;
    if unix_millis() >= record.expires_unix_ms {
        if let Some(record) = registry.remove(&params.bundle_id) {
            let _ = tokio::fs::remove_file(&record.path).await;
        }
        return Err(RemoteError::new(
            RemoteErrorCode::PathNotFound,
            format!("bundle export {} expired", params.bundle_id),
        ));
    }
    if params.offset > record.bytes {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!(
                "bundle read offset {} exceeds {} bytes",
                params.offset, record.bytes
            ),
        ));
    }
    let mut file = tokio::fs::File::open(&record.path).await.map_err(|error| {
        RemoteError::new(
            RemoteErrorCode::PathNotFound,
            format!(
                "bundle export {} file is missing: {error}",
                params.bundle_id
            ),
        )
    })?;
    file.seek(std::io::SeekFrom::Start(params.offset))
        .await
        .map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::Internal,
                format!("cannot seek remote bundle: {error}"),
            )
        })?;
    let remaining = record.bytes.saturating_sub(params.offset);
    let limit = params.limit.min(remaining);
    let mut bytes = Vec::new();
    file.take(limit)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::Internal,
                format!("cannot read remote bundle chunk: {error}"),
            )
        })?;
    let bytes_read = bytes.len() as u64;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(GitBundleReadResult {
        bundle_id: params.bundle_id.clone(),
        offset: params.offset,
        bytes_read,
        base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        chunk_sha256: format!("{:x}", hasher.finalize()),
        done: params.offset.saturating_add(bytes_read) >= record.bytes,
        total_bytes: record.bytes,
        total_sha256: record.sha256.clone(),
    })
}

pub async fn delete(
    jail: &PathJail,
    registry: &mut BundleRegistry,
    params: &GitBundleDeleteParams,
) -> Result<GitBundleDeleteResult, RemoteError> {
    validate_bundle_id(&params.bundle_id)?;
    let record = registry.remove(&params.bundle_id);
    if let Some(record) = record {
        let _ = tokio::fs::remove_file(&record.path).await;
        delete_ref(jail.root(), &record.export_ref).await;
        Ok(GitBundleDeleteResult {
            bundle_id: params.bundle_id.clone(),
            deleted: true,
            operation_id: params.operation_id.clone(),
            replayed: false,
        })
    } else {
        Ok(GitBundleDeleteResult {
            bundle_id: params.bundle_id.clone(),
            deleted: false,
            operation_id: params.operation_id.clone(),
            replayed: false,
        })
    }
}

/// Best-effort cleanup used when a helper shuts down.
pub async fn cleanup_all(jail: &PathJail, registry: &mut BundleRegistry) {
    let records = registry
        .records
        .drain()
        .map(|(_, record)| record)
        .collect::<Vec<_>>();
    for record in records {
        let _ = tokio::fs::remove_file(&record.path).await;
        delete_ref(jail.root(), &record.export_ref).await;
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command as StdCommand;

    use tempfile::TempDir;

    use super::*;

    fn git(root: &Path, args: &[&str]) {
        let status = StdCommand::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn repo() -> (TempDir, PathJail) {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init", "-q"]);
        fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        git(dir.path(), &["add", "a.txt"]);
        git(dir.path(), &["commit", "-q", "-m", "first"]);
        let jail = PathJail::new(dir.path()).unwrap();
        (dir, jail)
    }

    fn params(max_bytes: u64) -> GitBundleCreateParams {
        GitBundleCreateParams {
            branch: "main".into(),
            base: None,
            max_bytes,
            chunk_bytes: 1024,
            operation_id: Some("bundle-1".into()),
        }
    }

    #[tokio::test]
    async fn creates_reads_and_deletes_a_bounded_bundle() {
        let (_dir, jail) = repo();
        let mut registry = BundleRegistry::new();
        let created = create(&jail, &mut registry, &params(1024 * 1024))
            .await
            .unwrap();
        assert_eq!(created.head.len(), 40);
        assert!(created.bytes > 0);
        assert_eq!(created.sha256.len(), 64);
        assert_eq!(created.chunks as u64, created.bytes.div_ceil(1024));
        let mut collected = Vec::new();
        let mut offset = 0u64;
        loop {
            let chunk = read(
                &mut registry,
                &GitBundleReadParams {
                    bundle_id: created.bundle_id.clone(),
                    offset,
                    limit: 1024,
                },
            )
            .await
            .unwrap();
            assert!(chunk.bytes_read <= 1024);
            collected.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(&chunk.base64)
                    .unwrap(),
            );
            offset += chunk.bytes_read;
            if chunk.done {
                break;
            }
        }
        assert_eq!(collected.len() as u64, created.bytes);
        let mut hasher = Sha256::new();
        hasher.update(&collected);
        assert_eq!(format!("{:x}", hasher.finalize()), created.sha256);
        let deleted = delete(
            &jail,
            &mut registry,
            &GitBundleDeleteParams {
                bundle_id: created.bundle_id.clone(),
                operation_id: Some("delete-1".into()),
            },
        )
        .await
        .unwrap();
        assert!(deleted.deleted);
        assert!(registry.get(&created.bundle_id).is_none());
        let again = delete(
            &jail,
            &mut registry,
            &GitBundleDeleteParams {
                bundle_id: created.bundle_id,
                operation_id: Some("delete-2".into()),
            },
        )
        .await
        .unwrap();
        assert!(!again.deleted);
    }

    #[tokio::test]
    async fn exports_a_thin_bundle_with_a_prerequisite() {
        let (dir, jail) = repo();
        let root = dir.path().canonicalize().unwrap();
        fs::write(root.join("b.txt"), "two\n").unwrap();
        git(&root, &["add", "b.txt"]);
        git(&root, &["commit", "-q", "-m", "second"]);
        let base = String::from_utf8(
            StdCommand::new("git")
                .arg("-C")
                .arg(&root)
                .args(["rev-parse", "HEAD~1"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        let mut registry = BundleRegistry::new();
        let mut params = params(1024 * 1024);
        params.base = Some(base.clone());
        let created = create(&jail, &mut registry, &params).await.unwrap();
        assert_eq!(created.base.as_deref(), Some(base.as_str()));
        let record = registry.get(&created.bundle_id).unwrap();
        let output = StdCommand::new("git")
            .arg("-C")
            .arg(&root)
            .args(["bundle", "verify", record.path.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(created.head.len(), 40);

        let mut unsafe_base = params;
        unsafe_base.base = Some("--upload-pack=evil".into());
        assert_eq!(
            create(&jail, &mut registry, &unsafe_base)
                .await
                .unwrap_err()
                .code,
            RemoteErrorCode::InvalidParams
        );
    }

    #[tokio::test]
    async fn enforces_the_byte_cap_and_leaves_no_partial_bundle() {
        let (dir, jail) = repo();
        let mut registry = BundleRegistry::new();
        let error = create(&jail, &mut registry, &params(64)).await.unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::FileTooLarge);
        assert!(registry.is_empty());
        let directory = bundle_dir(&dir.path().canonicalize().unwrap()).unwrap();
        assert!(fs::read_dir(directory).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn rejects_unsafe_branches_and_bounds() {
        let (_dir, jail) = repo();
        let mut registry = BundleRegistry::new();
        let mut unsafe_branch = params(1024 * 1024);
        unsafe_branch.branch = "--upload-pack=evil".into();
        assert_eq!(
            create(&jail, &mut registry, &unsafe_branch)
                .await
                .unwrap_err()
                .code,
            RemoteErrorCode::InvalidParams
        );
        let mut tiny_chunk = params(1024 * 1024);
        tiny_chunk.chunk_bytes = 16;
        assert_eq!(
            create(&jail, &mut registry, &tiny_chunk)
                .await
                .unwrap_err()
                .code,
            RemoteErrorCode::InvalidParams
        );
        let mut zero_cap = params(0);
        zero_cap.branch = "main".into();
        assert_eq!(
            create(&jail, &mut registry, &zero_cap)
                .await
                .unwrap_err()
                .code,
            RemoteErrorCode::InvalidParams
        );
    }

    #[tokio::test]
    async fn unknown_or_expired_bundles_fail_closed() {
        let (_dir, jail) = repo();
        let mut registry = BundleRegistry::new();
        let error = read(
            &mut registry,
            &GitBundleReadParams {
                bundle_id: "missing".into(),
                offset: 0,
                limit: 1024,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::PathNotFound);
        let created = create(&jail, &mut registry, &params(1024 * 1024))
            .await
            .unwrap();
        if let Some(record) = registry.records.get_mut(&created.bundle_id) {
            record.expires_unix_ms = 0;
        }
        let error = read(
            &mut registry,
            &GitBundleReadParams {
                bundle_id: created.bundle_id,
                offset: 0,
                limit: 1024,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::PathNotFound);
    }

    #[tokio::test]
    async fn caps_active_exports_and_delete_removes_files_and_refs() {
        let (dir, jail) = repo();
        let mut registry = BundleRegistry::new();
        let mut created = Vec::new();
        for index in 0..MAX_ACTIVE_BUNDLES {
            let mut request = params(1024 * 1024);
            request.operation_id = Some(format!("bundle-{index}"));
            created.push(create(&jail, &mut registry, &request).await.unwrap());
        }
        let mut overflow = params(1024 * 1024);
        overflow.operation_id = Some("bundle-overflow".into());
        assert_eq!(
            create(&jail, &mut registry, &overflow)
                .await
                .unwrap_err()
                .code,
            RemoteErrorCode::OutputLimit
        );
        let first = &created[0];
        let path = registry.get(&first.bundle_id).unwrap().path.clone();
        let export_ref = registry.get(&first.bundle_id).unwrap().export_ref.clone();
        assert!(path.exists());
        let deleted = delete(
            &jail,
            &mut registry,
            &GitBundleDeleteParams {
                bundle_id: first.bundle_id.clone(),
                operation_id: Some("delete-first".into()),
            },
        )
        .await
        .unwrap();
        assert!(deleted.deleted);
        assert!(!path.exists());
        let status = StdCommand::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["rev-parse", "--verify", "--quiet", &export_ref])
            .status()
            .unwrap();
        assert!(!status.success());
        assert_eq!(registry.len(), MAX_ACTIVE_BUNDLES - 1);
    }

    #[tokio::test]
    async fn read_bounds_and_offsets_are_explicit() {
        let (_dir, jail) = repo();
        let mut registry = BundleRegistry::new();
        let zero = read(
            &mut registry,
            &GitBundleReadParams {
                bundle_id: "missing".into(),
                offset: 0,
                limit: 0,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(zero.code, RemoteErrorCode::InvalidParams);
        let too_large = read(
            &mut registry,
            &GitBundleReadParams {
                bundle_id: "missing".into(),
                offset: 0,
                limit: MAX_GIT_BUNDLE_CHUNK_BYTES + 1,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(too_large.code, RemoteErrorCode::InvalidParams);
        let created = create(&jail, &mut registry, &params(1024 * 1024))
            .await
            .unwrap();
        let past_end = read(
            &mut registry,
            &GitBundleReadParams {
                bundle_id: created.bundle_id.clone(),
                offset: created.bytes + 1,
                limit: 1024,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(past_end.code, RemoteErrorCode::InvalidParams);
        let at_end = read(
            &mut registry,
            &GitBundleReadParams {
                bundle_id: created.bundle_id,
                offset: created.bytes,
                limit: 1024,
            },
        )
        .await
        .unwrap();
        assert_eq!(at_end.bytes_read, 0);
        assert!(at_end.done);
    }

    #[tokio::test]
    async fn expired_records_are_removed_with_their_files() {
        let (dir, jail) = repo();
        let mut registry = BundleRegistry::new();
        let created = create(&jail, &mut registry, &params(1024 * 1024))
            .await
            .unwrap();
        let record = registry.get(&created.bundle_id).unwrap().clone();
        if let Some(record) = registry.records.get_mut(&created.bundle_id) {
            record.expires_unix_ms = 0;
        }
        cleanup_expired(&jail, &mut registry);
        assert!(registry.is_empty());
        // `cleanup_expired` removed the file and export ref.
        assert!(!record.path.exists());
        let status = StdCommand::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["rev-parse", "--verify", "--quiet", &record.export_ref])
            .status()
            .unwrap();
        assert!(!status.success());
    }

    #[test]
    fn bundle_ids_are_path_safe() {
        assert!(validate_bundle_id("b123-abc_DEF").is_ok());
        assert!(validate_bundle_id("../escape").is_err());
        assert!(validate_bundle_id("a/b").is_err());
        assert!(validate_bundle_id("").is_err());
    }
}
