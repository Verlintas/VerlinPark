//! Local half of remote worktree integration.
//!
//! A remote helper exports a branch as a Git bundle; this module validates the
//! downloaded bytes, checks the advertised head, imports the bundle into a
//! private temporary ref namespace, and creates a local worktree for retry and
//! conflict handling. It never touches the session branch. Verification and
//! integration are performed by `agent.rs` using the same Verifier and
//! Integrator paths as a local Worker commit.
//!
//! The remote host is trusted infrastructure (ADR-0156). This module does not
//! sandbox it; it makes the *transport* bounded and the *local import*
//! fail-closed.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use vp_protocol::remote::{
    MAX_REMOTE_BRANCH_CHARS, REMOTE_VERIFICATION_ENVIRONMENT_CAVEAT, RemoteIntegrationPolicy,
};
use vp_protocol::types::MERGE_SOURCE_REMOTE;

use crate::tooling::errors::{ToolErrorCode, ToolFailure};

/// Version of the local import layout. Temporary refs include the session and
/// an import id, never a mutable branch name.
pub const REMOTE_IMPORT_REF_PREFIX: &str = "refs/verlinpark/remote";
pub const MAX_IMPORT_ID_CHARS: usize = 80;
pub const MIN_BUNDLE_BYTES: u64 = 1;

/// Typed failure used by the daemon RPC. The same object serializes through the
/// Q3 tool-error contract, so CLI and Swift receive a stable code, retryability,
/// hint, and safe evidence instead of parsing a prose error.
#[derive(Debug, Clone)]
pub struct RemoteIntegrationFailure {
    /// Boxed so the `Err` side of fallible Git helpers stays small; clippy's
    /// large-error lint catches an inline `ToolFailure` on every signature.
    pub failure: Box<ToolFailure>,
}

impl RemoteIntegrationFailure {
    pub fn new(code: ToolErrorCode, message: impl Into<String>) -> Self {
        let failure = ToolFailure::new(code, message).with_tool("remote_integrate");
        Self {
            failure: Box::new(failure),
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        let failure = *self.failure;
        self.failure = Box::new(failure.with_hint(hint));
        self
    }

    pub fn with_evidence(mut self, evidence: impl Into<String>) -> Self {
        let failure = *self.failure;
        self.failure = Box::new(failure.with_evidence(evidence));
        self
    }

    pub fn with_retryable(mut self, retryable: bool) -> Self {
        let failure = *self.failure;
        self.failure = Box::new(failure.with_retryable(retryable));
        self
    }

    pub fn code(&self) -> &'static str {
        self.failure.code.as_str()
    }

    pub fn data(&self) -> Value {
        serde_json::to_value(self.failure.as_ref()).unwrap_or_else(|_| {
            json!({
                "code": self.code(),
                "message": self.failure.message,
                "retryable": self.failure.retryable,
                "hint": self.failure.hint,
            })
        })
    }
}

impl std::fmt::Display for RemoteIntegrationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "error[{}]: {}",
            self.code(),
            self.failure.message
        )
    }
}

impl std::error::Error for RemoteIntegrationFailure {}

fn failure<T>(
    code: ToolErrorCode,
    message: impl Into<String>,
) -> Result<T, RemoteIntegrationFailure> {
    Err(RemoteIntegrationFailure::new(code, message))
}

/// Convert an `anyhow` error that may wrap a typed remote-integration failure
/// into RPC error data. Other errors keep their ordinary message and no data.
pub fn failure_data(error: &anyhow::Error) -> Option<Value> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<RemoteIntegrationFailure>())
        .map(RemoteIntegrationFailure::data)
}

/// Validate a branch name accepted from an operator or a plan task before it
/// becomes an argv element. This mirrors the helper's conservative rules but is
/// deliberately independent: the daemon must not rely on the remote host to
/// reject a bad local input.
pub fn validate_remote_branch(branch: &str) -> Result<String, RemoteIntegrationFailure> {
    let branch = branch.trim();
    if branch.is_empty() || branch.chars().count() > MAX_REMOTE_BRANCH_CHARS {
        return Err(RemoteIntegrationFailure::new(
            ToolErrorCode::REMOTE_BUNDLE_INVALID,
            format!("remote branch must be 1..={MAX_REMOTE_BRANCH_CHARS} characters"),
        ));
    }
    if branch.starts_with("refs/heads/") {
        return Err(RemoteIntegrationFailure::new(
            ToolErrorCode::REMOTE_BUNDLE_INVALID,
            "remote branch must omit the refs/heads/ prefix",
        ));
    }
    if branch.starts_with('-')
        || branch.starts_with('/')
        || branch.ends_with('/')
        || branch.ends_with('.')
        || branch.ends_with(".lock")
        || branch.contains("..")
        || branch.contains("@{")
        || branch.contains("//")
        || branch.contains('\\')
        || branch.contains(':')
        || branch.contains('?')
        || branch.contains('*')
        || branch.contains('[')
        || branch.contains('~')
        || branch.contains('^')
        || branch.chars().any(char::is_control)
        || branch.chars().any(char::is_whitespace)
    {
        return Err(RemoteIntegrationFailure::new(
            ToolErrorCode::REMOTE_BUNDLE_INVALID,
            format!("unsafe remote branch name: {branch}"),
        ));
    }
    if branch.split('/').any(str::is_empty) {
        return Err(RemoteIntegrationFailure::new(
            ToolErrorCode::REMOTE_BUNDLE_INVALID,
            format!("unsafe remote branch name: {branch}"),
        ));
    }
    Ok(branch.to_string())
}

pub fn validate_import_id(value: &str) -> Result<(), RemoteIntegrationFailure> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > MAX_IMPORT_ID_CHARS {
        return Err(RemoteIntegrationFailure::new(
            ToolErrorCode::REMOTE_BUNDLE_IMPORT_FAILED,
            format!("import id must be 1..={MAX_IMPORT_ID_CHARS} characters"),
        ));
    }
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(RemoteIntegrationFailure::new(
            ToolErrorCode::REMOTE_BUNDLE_IMPORT_FAILED,
            "import id may contain only ASCII letters, digits, '-' and '_'",
        ));
    }
    Ok(())
}

/// Temporary ref for one import. It includes the session id and a bounded
/// import id so two sessions cannot collide and an operator can identify it.
pub fn import_ref(session_id: &str, import_id: &str) -> Result<String, RemoteIntegrationFailure> {
    validate_import_id(import_id)?;
    let session = session_id.trim();
    if session.is_empty()
        || session.chars().count() > 80
        || !session
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(RemoteIntegrationFailure::new(
            ToolErrorCode::REMOTE_BUNDLE_IMPORT_FAILED,
            "session id is not safe for a temporary Git ref",
        ));
    }
    Ok(format!("{REMOTE_IMPORT_REF_PREFIX}/{session}/{import_id}"))
}

pub fn validate_bundle_path(bundle_path: &Path) -> Result<(), RemoteIntegrationFailure> {
    let metadata = std::fs::symlink_metadata(bundle_path).map_err(|error| {
        RemoteIntegrationFailure::new(
            ToolErrorCode::REMOTE_BUNDLE_IMPORT_FAILED,
            format!(
                "cannot inspect downloaded bundle {}: {error}",
                bundle_path.display()
            ),
        )
    })?;
    if !metadata.is_file() {
        return failure(
            ToolErrorCode::REMOTE_BUNDLE_INVALID,
            format!(
                "downloaded bundle is not a regular file: {}",
                bundle_path.display()
            ),
        );
    }
    if metadata.len() < MIN_BUNDLE_BYTES {
        return failure(
            ToolErrorCode::REMOTE_BUNDLE_INVALID,
            "downloaded bundle is empty",
        );
    }
    Ok(())
}

pub fn sha256_file(path: &Path) -> Result<String, RemoteIntegrationFailure> {
    let mut file = std::fs::File::open(path).map_err(|error| {
        RemoteIntegrationFailure::new(
            ToolErrorCode::REMOTE_BUNDLE_IMPORT_FAILED,
            format!("cannot open downloaded bundle {}: {error}", path.display()),
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|error| {
            RemoteIntegrationFailure::new(
                ToolErrorCode::REMOTE_BUNDLE_IMPORT_FAILED,
                format!("cannot hash downloaded bundle: {error}"),
            )
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn run_git(root: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .output()
        .context("running local git")
}

fn git_stdout(root: &Path, args: &[&str]) -> Result<String> {
    let output = run_git(root, args)?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_command(root: &Path, args: &[&str], context: &str) -> Result<(), RemoteIntegrationFailure> {
    let output = run_git(root, args).map_err(|error| {
        RemoteIntegrationFailure::new(
            ToolErrorCode::REMOTE_BUNDLE_IMPORT_FAILED,
            format!("{context}: {error}"),
        )
        .with_hint("verify that the local project is a Git repository")
    })?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if stderr.is_empty() { stdout } else { stderr };
    Err(RemoteIntegrationFailure::new(
        ToolErrorCode::REMOTE_BUNDLE_IMPORT_FAILED,
        format!("{context}: {}", truncate_chars(&detail, 2_000)),
    ))
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut rendered = value.chars().take(limit).collect::<String>();
    rendered.push('…');
    rendered
}

fn validate_expected_head(head: &str) -> Result<String, RemoteIntegrationFailure> {
    let head = head.trim();
    if head.len() < 7
        || head.len() > 64
        || !head.chars().all(|character| character.is_ascii_hexdigit())
    {
        return Err(RemoteIntegrationFailure::new(
            ToolErrorCode::REMOTE_BUNDLE_HEAD_MISMATCH,
            format!("expected head is not a Git object id: {head}"),
        ));
    }
    Ok(head.to_ascii_lowercase())
}

#[derive(Debug, Clone)]
pub struct BundleHead {
    pub commit: String,
    pub reference: String,
}

/// Parse `git bundle list-heads` output. Lines are `<object> <ref>`.
pub fn parse_bundle_heads(output: &str) -> Vec<BundleHead> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let commit = parts.next()?.to_ascii_lowercase();
            let reference = parts.next()?.to_string();
            if commit.len() < 7
                || commit.len() > 64
                || !commit
                    .chars()
                    .all(|character| character.is_ascii_hexdigit())
                || reference.is_empty()
            {
                return None;
            }
            Some(BundleHead { commit, reference })
        })
        .collect()
}

/// Verify the bundle, match its advertised head, and fetch exactly that commit
/// into a private ref. The session branch is never referenced.
#[derive(Debug, Clone)]
pub struct ImportedBundle {
    pub imported_ref: String,
    pub commit: String,
    pub bundle_sha256: String,
    pub bytes: u64,
    pub bundle_head_ref: String,
}

#[allow(clippy::too_many_arguments)]
pub fn import_bundle(
    project_root: &Path,
    bundle_path: &Path,
    expected_head: &str,
    expected_sha256: Option<&str>,
    session_id: &str,
    import_id: &str,
    keep_import_refs: bool,
) -> Result<ImportedBundle, RemoteIntegrationFailure> {
    validate_bundle_path(bundle_path)?;
    let expected_head = validate_expected_head(expected_head)?;
    let bytes = std::fs::metadata(bundle_path)
        .map_err(|error| {
            RemoteIntegrationFailure::new(
                ToolErrorCode::REMOTE_BUNDLE_IMPORT_FAILED,
                format!("cannot inspect downloaded bundle: {error}"),
            )
        })?
        .len();
    let bundle_sha256 = sha256_file(bundle_path)?;
    if let Some(expected) = expected_sha256
        && !expected.trim().is_empty()
        && !expected.trim().eq_ignore_ascii_case(&bundle_sha256)
    {
        return failure(
            ToolErrorCode::REMOTE_BUNDLE_TAMPERED,
            format!(
                "downloaded bundle SHA-256 {} does not match the helper report {}",
                bundle_sha256,
                expected.trim()
            ),
        );
    }
    let root = project_root.canonicalize().map_err(|error| {
        RemoteIntegrationFailure::new(
            ToolErrorCode::REMOTE_BUNDLE_IMPORT_FAILED,
            format!(
                "cannot resolve project root {}: {error}",
                project_root.display()
            ),
        )
    })?;
    let verify = run_git(
        &root,
        &[
            "bundle",
            "verify",
            "--quiet",
            &bundle_path.to_string_lossy(),
        ],
    )
    .map_err(|error| {
        RemoteIntegrationFailure::new(
            ToolErrorCode::REMOTE_BUNDLE_INVALID,
            format!("cannot run git bundle verify: {error}"),
        )
    })?;
    if !verify.status.success() {
        return Err(RemoteIntegrationFailure::new(
            ToolErrorCode::REMOTE_BUNDLE_INVALID,
            format!(
                "git bundle verify rejected the download: {}",
                truncate_chars(String::from_utf8_lossy(&verify.stderr).trim(), 2_000)
            ),
        )
        .with_hint("regenerate the bundle on the remote helper and retry"));
    }
    let heads_output = git_stdout(
        &root,
        &["bundle", "list-heads", &bundle_path.to_string_lossy()],
    )
    .map_err(|error| {
        RemoteIntegrationFailure::new(
            ToolErrorCode::REMOTE_BUNDLE_INVALID,
            format!("cannot list bundle heads: {error}"),
        )
    })?;
    let heads = parse_bundle_heads(&heads_output);
    let Some(head) = heads
        .iter()
        .find(|head| head.commit.eq_ignore_ascii_case(&expected_head))
    else {
        let advertised = heads
            .iter()
            .map(|head| head.commit.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return failure(
            ToolErrorCode::REMOTE_BUNDLE_HEAD_MISMATCH,
            format!(
                "bundle advertises [{advertised}], but the recorded remote head is {expected_head}"
            ),
        );
    };
    let reference = import_ref(session_id, import_id)?;
    let force_ref = format!("+{}:{reference}", head.reference);
    git_command(
        &root,
        &[
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            "--no-recurse-submodules",
            &bundle_path.to_string_lossy(),
            &force_ref,
        ],
        "importing the remote bundle into a temporary ref",
    )?;
    let commit = match git_stdout(
        &root,
        &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
    ) {
        Ok(commit) => commit.to_ascii_lowercase(),
        Err(error) => {
            if !keep_import_refs {
                let _ = delete_import_ref(&root, &reference);
            }
            return failure(
                ToolErrorCode::REMOTE_BUNDLE_IMPORT_FAILED,
                format!("imported ref {reference} is not a commit: {error}"),
            );
        }
    };
    if commit != expected_head {
        let _ = delete_import_ref(&root, &reference);
        return failure(
            ToolErrorCode::REMOTE_BUNDLE_HEAD_MISMATCH,
            format!("imported commit {commit} does not match recorded head {expected_head}"),
        );
    }
    Ok(ImportedBundle {
        imported_ref: reference,
        commit,
        bundle_sha256,
        bytes,
        bundle_head_ref: head.reference.clone(),
    })
}

pub fn delete_import_ref(project_root: &Path, reference: &str) -> Result<()> {
    if !reference.starts_with(&format!("{REMOTE_IMPORT_REF_PREFIX}/")) {
        bail!("refusing to delete a ref outside {REMOTE_IMPORT_REF_PREFIX}: {reference}");
    }
    let root = project_root
        .canonicalize()
        .with_context(|| format!("resolving project root {}", project_root.display()))?;
    git_command(
        &root,
        &["update-ref", "-d", reference],
        "deleting temporary import ref",
    )
    .map_err(anyhow::Error::new)
}

pub fn update_import_ref(project_root: &Path, reference: &str, commit: &str) -> Result<()> {
    if !reference.starts_with(&format!("{REMOTE_IMPORT_REF_PREFIX}/")) {
        bail!("refusing to update a ref outside {REMOTE_IMPORT_REF_PREFIX}: {reference}");
    }
    let commit = validate_expected_head(commit).map_err(anyhow::Error::new)?;
    let root = project_root
        .canonicalize()
        .with_context(|| format!("resolving project root {}", project_root.display()))?;
    git_command(
        &root,
        &["update-ref", reference, &commit],
        "updating temporary import ref",
    )
    .map_err(anyhow::Error::new)
}

pub fn import_ref_exists(project_root: &Path, reference: &str) -> bool {
    let Ok(root) = project_root.canonicalize() else {
        return false;
    };
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", "--quiet", reference])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub fn prune_import_refs(
    project_root: &Path,
    references: &[String],
    keep: bool,
) -> Result<usize, RemoteIntegrationFailure> {
    if keep {
        return Ok(0);
    }
    let mut removed = 0usize;
    for reference in references {
        if reference.is_empty() {
            continue;
        }
        match delete_import_ref(project_root, reference) {
            Ok(()) => removed += 1,
            Err(error) => {
                return Err(RemoteIntegrationFailure::new(
                    ToolErrorCode::REMOTE_BUNDLE_IMPORT_FAILED,
                    format!("cannot delete temporary import ref {reference}: {error:#}"),
                ));
            }
        }
    }
    Ok(removed)
}

#[derive(Debug, Clone)]
pub struct RemoteWorktree {
    pub path: PathBuf,
    pub branch: String,
    pub base_commit: String,
}

/// Create or reattach the local worktree that represents an imported remote
/// branch. Conflict retry rebases this worktree and then updates the imported
/// ref to the rebased commit, so the Integrator always merges the exact ref.
pub fn create_remote_worktree(
    project_root: &Path,
    data_dir: &Path,
    project_id: &str,
    session_id: &str,
    import_id: &str,
    imported_ref: &str,
) -> Result<RemoteWorktree, RemoteIntegrationFailure> {
    validate_import_id(import_id)?;
    let root = project_root.canonicalize().map_err(|error| {
        RemoteIntegrationFailure::new(
            ToolErrorCode::REMOTE_BUNDLE_IMPORT_FAILED,
            format!("cannot resolve project root: {error}"),
        )
    })?;
    let branch = format!("verlinpark/{session_id}--remote-{import_id}");
    let path = data_dir
        .join("worktrees")
        .join(project_id)
        .join(session_id)
        .join(format!("remote-{import_id}"));
    let base_commit = git_stdout(
        &root,
        &[
            "rev-parse",
            "--verify",
            &format!("{imported_ref}^{{commit}}"),
        ],
    )
    .map_err(|error| {
        RemoteIntegrationFailure::new(
            ToolErrorCode::REMOTE_BUNDLE_IMPORT_FAILED,
            format!("imported ref {imported_ref} is missing: {error}"),
        )
    })?;
    if path.join(".git").is_file() {
        return Ok(RemoteWorktree {
            path,
            branch,
            base_commit,
        });
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            RemoteIntegrationFailure::new(
                ToolErrorCode::REMOTE_BUNDLE_IMPORT_FAILED,
                format!(
                    "cannot create remote worktree parent {}: {error}",
                    parent.display()
                ),
            )
        })?;
    }
    let path_string = path.to_string_lossy().to_string();
    git_command(
        &root,
        &["worktree", "add", "-B", &branch, &path_string, imported_ref],
        "creating a local worktree for the imported remote branch",
    )?;
    Ok(RemoteWorktree {
        path,
        branch,
        base_commit,
    })
}

/// Remove a remote integration worktree and its local branch. Used when the
/// Verifier rejects before any queue entry exists. The imported ref is removed
/// separately.
pub fn remove_remote_worktree(
    project_root: &Path,
    worktree: &Path,
    branch: &str,
) -> Result<(), RemoteIntegrationFailure> {
    let root = project_root.canonicalize().map_err(|error| {
        RemoteIntegrationFailure::new(
            ToolErrorCode::REMOTE_BUNDLE_IMPORT_FAILED,
            format!("cannot resolve project root: {error}"),
        )
    })?;
    let _ = run_git(
        &root,
        &["worktree", "remove", "--force", &worktree.to_string_lossy()],
    );
    let _ = run_git(&root, &["branch", "-D", branch]);
    Ok(())
}

pub fn environment_caveat() -> &'static str {
    REMOTE_VERIFICATION_ENVIRONMENT_CAVEAT
}

pub fn remote_source_label(host: &str, branch: &str) -> String {
    format!("{MERGE_SOURCE_REMOTE}:{host}:{branch}")
}

/// Validate that the project policy is enabled and the configured host is
/// explicitly allowed before any network or Git operation happens. Kept pure
/// so the daemon, CLI, and tests share exactly one decision.
pub fn authorize(
    policy: &RemoteIntegrationPolicy,
    config: &vp_protocol::remote::ExecutionHost,
    confirm: bool,
) -> Result<(), RemoteIntegrationFailure> {
    policy.validate().map_err(|error| {
        RemoteIntegrationFailure::new(
            ToolErrorCode::REMOTE_INTEGRATION_DISABLED,
            format!("remote integration policy is invalid: {error}"),
        )
    })?;
    if !policy.enabled {
        return failure(
            ToolErrorCode::REMOTE_INTEGRATION_DISABLED,
            "remote integration is disabled for this project",
        );
    }
    if !confirm {
        return failure(
            ToolErrorCode::REMOTE_INTEGRATION_APPROVAL_REQUIRED,
            "remote integration requires explicit operator confirmation",
        );
    }
    if !config.is_remote() {
        return failure(
            ToolErrorCode::REMOTE_INTEGRATION_HOST_NOT_ALLOWED,
            "remote integration requires an ssh execution host for this project",
        );
    }
    if !policy.allows_host(config) {
        return failure(
            ToolErrorCode::REMOTE_INTEGRATION_HOST_NOT_ALLOWED,
            format!(
                "host {} is not in the project remote integration allowlist",
                config.host
            ),
        );
    }
    Ok(())
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

    fn repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        git(dir.path(), &["add", "a.txt"]);
        git(dir.path(), &["commit", "-q", "-m", "first"]);
        dir
    }

    fn head(root: &Path) -> String {
        git_stdout(root, &["rev-parse", "HEAD"]).unwrap()
    }

    fn bundle_from(source: &Path, branch: &str) -> (TempDir, PathBuf, String) {
        let dir = TempDir::new().unwrap();
        let bundle = dir.path().join("branch.bundle");
        git(
            source,
            &["bundle", "create", bundle.to_str().unwrap(), branch],
        );
        let head = head(source);
        (dir, bundle, head)
    }

    #[test]
    fn imports_a_bundle_into_a_private_ref_and_cleans_up() {
        let origin = repo();
        let project = repo();
        // Make the project share the origin's object graph, then add a remote
        // branch commit to export.
        fs::write(origin.path().join("remote.txt"), "remote\n").unwrap();
        git(origin.path(), &["add", "remote.txt"]);
        git(origin.path(), &["commit", "-q", "-m", "remote change"]);
        git(
            project.path(),
            &["remote", "add", "origin", origin.path().to_str().unwrap()],
        );
        git(project.path(), &["fetch", "-q", "origin", "main"]);
        let (_dir, bundle, remote_head) = bundle_from(origin.path(), "main");
        let imported = import_bundle(
            project.path(),
            &bundle,
            &remote_head,
            None,
            "session-1",
            "import-1",
            false,
        )
        .unwrap();
        assert_eq!(imported.commit, remote_head);
        assert!(imported.imported_ref.starts_with(REMOTE_IMPORT_REF_PREFIX));
        assert!(import_ref_exists(project.path(), &imported.imported_ref));
        let local_head = head(project.path());
        assert_ne!(local_head, remote_head);
        let removed = prune_import_refs(
            project.path(),
            std::slice::from_ref(&imported.imported_ref),
            false,
        )
        .unwrap();
        assert_eq!(removed, 1);
        assert!(!import_ref_exists(project.path(), &imported.imported_ref));
    }

    #[test]
    fn rejects_malformed_and_empty_bundles() {
        let project = repo();
        let dir = TempDir::new().unwrap();
        let malformed = dir.path().join("bad.bundle");
        fs::write(&malformed, b"this is not a bundle\n").unwrap();
        let error = import_bundle(
            project.path(),
            &malformed,
            &"a".repeat(40),
            None,
            "session-1",
            "import-1",
            false,
        )
        .unwrap_err();
        assert_eq!(error.code(), "REMOTE_BUNDLE_INVALID");
        let empty = dir.path().join("empty.bundle");
        fs::write(&empty, b"").unwrap();
        let error = import_bundle(
            project.path(),
            &empty,
            &"a".repeat(40),
            None,
            "session-1",
            "import-1",
            false,
        )
        .unwrap_err();
        assert_eq!(error.code(), "REMOTE_BUNDLE_INVALID");
    }

    #[test]
    fn rejects_a_sha_mismatch_before_invoking_git() {
        let project = repo();
        let (_dir, bundle, remote_head) = bundle_from(project.path(), "main");
        let error = import_bundle(
            project.path(),
            &bundle,
            &remote_head,
            Some(&"0".repeat(64)),
            "session-1",
            "import-1",
            false,
        )
        .unwrap_err();
        assert_eq!(error.code(), "REMOTE_BUNDLE_TAMPERED");
        assert!(!import_ref_exists(
            project.path(),
            &import_ref("session-1", "import-1").unwrap()
        ));
    }

    #[test]
    fn rejects_a_head_mismatch_and_leaves_no_ref() {
        let project = repo();
        let (_dir, bundle, _remote_head) = bundle_from(project.path(), "main");
        let error = import_bundle(
            project.path(),
            &bundle,
            &"1".repeat(40),
            None,
            "session-1",
            "import-1",
            false,
        )
        .unwrap_err();
        assert_eq!(error.code(), "REMOTE_BUNDLE_HEAD_MISMATCH");
        assert!(!import_ref_exists(
            project.path(),
            &import_ref("session-1", "import-1").unwrap()
        ));
    }

    #[test]
    fn creates_a_local_worktree_for_conflict_retry() {
        let origin = repo();
        let project = repo();
        let data = TempDir::new().unwrap();
        fs::write(origin.path().join("remote.txt"), "remote\n").unwrap();
        git(origin.path(), &["add", "remote.txt"]);
        git(origin.path(), &["commit", "-q", "-m", "remote change"]);
        git(
            project.path(),
            &["remote", "add", "origin", origin.path().to_str().unwrap()],
        );
        git(project.path(), &["fetch", "-q", "origin", "main"]);
        let (_dir, bundle, remote_head) = bundle_from(origin.path(), "main");
        let imported = import_bundle(
            project.path(),
            &bundle,
            &remote_head,
            None,
            "session-1",
            "import-1",
            true,
        )
        .unwrap();
        let worktree = create_remote_worktree(
            project.path(),
            data.path(),
            "project-1",
            "session-1",
            "import-1",
            &imported.imported_ref,
        )
        .unwrap();
        assert_eq!(head(&worktree.path), remote_head);
        assert!(worktree.branch.contains("remote-import-1"));
        fs::write(worktree.path.join("local.txt"), "local\n").unwrap();
        git(&worktree.path, &["add", "local.txt"]);
        git(&worktree.path, &["commit", "-q", "-m", "retry commit"]);
        let retried = head(&worktree.path);
        update_import_ref(project.path(), &imported.imported_ref, &retried).unwrap();
        let stored = git_stdout(project.path(), &["rev-parse", &imported.imported_ref]).unwrap();
        assert_eq!(stored, retried);
        remove_remote_worktree(project.path(), &worktree.path, &worktree.branch).unwrap();
        assert!(!worktree.path.exists());
    }

    #[test]
    fn rejects_a_thin_bundle_whose_prerequisite_is_missing() {
        let origin = repo();
        fs::write(origin.path().join("thin.txt"), "thin\n").unwrap();
        git(origin.path(), &["add", "thin.txt"]);
        git(origin.path(), &["commit", "-q", "-m", "thin commit"]);
        let base = git_stdout(origin.path(), &["rev-parse", "HEAD~1"]).unwrap();
        let dir = TempDir::new().unwrap();
        let bundle_path = dir.path().join("thin.bundle");
        git(
            origin.path(),
            &[
                "bundle",
                "create",
                bundle_path.to_str().unwrap(),
                "main",
                "--not",
                &base,
            ],
        );
        let unrelated = TempDir::new().unwrap();
        git(unrelated.path(), &["init", "-q", "-b", "main"]);
        fs::write(unrelated.path().join("other.txt"), "other\n").unwrap();
        git(unrelated.path(), &["add", "other.txt"]);
        git(unrelated.path(), &["commit", "-q", "-m", "unrelated"]);
        let remote_head = head(origin.path());
        let error = import_bundle(
            unrelated.path(),
            &bundle_path,
            &remote_head,
            None,
            "session-1",
            "import-1",
            false,
        )
        .unwrap_err();
        assert_eq!(error.code(), "REMOTE_BUNDLE_INVALID");
        assert!(!import_ref_exists(
            unrelated.path(),
            &import_ref("session-1", "import-1").unwrap()
        ));
    }

    #[test]
    fn ref_cleanup_is_confined_to_the_remote_namespace() {
        let project = repo();
        let error = delete_import_ref(project.path(), "refs/heads/main").unwrap_err();
        assert!(format!("{error:#}").contains("refusing to delete"));
        assert!(
            update_import_ref(project.path(), "refs/heads/main", &head(project.path())).is_err()
        );
        let (_dir, bundle, _head) = bundle_from(project.path(), "main");
        let short = import_bundle(
            project.path(),
            &bundle,
            "not-a-commit",
            None,
            "session-1",
            "import-1",
            false,
        )
        .unwrap_err();
        assert_eq!(short.code(), "REMOTE_BUNDLE_HEAD_MISMATCH");
    }

    #[test]
    fn keep_import_refs_preserves_the_temporary_ref() {
        let project = repo();
        let (dir, bundle, remote_head) = bundle_from(project.path(), "main");
        let imported = import_bundle(
            project.path(),
            &bundle,
            &remote_head,
            None,
            "session-1",
            "keep-1",
            true,
        )
        .unwrap();
        let pruned = prune_import_refs(
            project.path(),
            std::slice::from_ref(&imported.imported_ref),
            true,
        )
        .unwrap();
        assert_eq!(pruned, 0);
        assert!(import_ref_exists(project.path(), &imported.imported_ref));
        let _ = dir;
    }

    #[test]
    fn branch_and_import_ids_are_conservative() {
        assert!(validate_remote_branch("feature/remote-1").is_ok());
        assert!(validate_remote_branch("main").is_ok());
        for branch in [
            "",
            "--upload-pack=x",
            "a..b",
            "a b",
            "refs/heads/main",
            "feature\\evil",
            "a:b",
            "a~1",
            "a^2",
        ] {
            assert!(validate_remote_branch(branch).is_err(), "{branch:?}");
        }
        assert!(import_ref("session-1", "import-1").is_ok());
        assert!(import_ref("../escape", "import-1").is_err());
        assert!(import_ref("session-1", "a/b").is_err());
    }

    #[test]
    fn policy_authorization_is_explicit_and_allowlisted() {
        use vp_protocol::remote::{ExecutionHost, ExecutionHostMode};
        let config = ExecutionHost {
            mode: ExecutionHostMode::Ssh,
            host: "build.example.com".into(),
            user: Some("agent".into()),
            root: "/srv/project".into(),
            token_env: Some("TOKEN".into()),
            ..ExecutionHost::default()
        };
        let policy = RemoteIntegrationPolicy::default();
        let error = authorize(&policy, &config, true).unwrap_err();
        assert_eq!(error.code(), "REMOTE_INTEGRATION_DISABLED");
        let policy = RemoteIntegrationPolicy {
            enabled: true,
            allowed_hosts: vec!["agent@build.example.com".into()],
            ..RemoteIntegrationPolicy::default()
        };
        let error = authorize(&policy, &config, false).unwrap_err();
        assert_eq!(error.code(), "REMOTE_INTEGRATION_APPROVAL_REQUIRED");
        let mut other = config.clone();
        other.host = "other.example.com".into();
        let error = authorize(&policy, &other, true).unwrap_err();
        assert_eq!(error.code(), "REMOTE_INTEGRATION_HOST_NOT_ALLOWED");
        assert!(authorize(&policy, &config, true).is_ok());
        assert!(authorize(&policy, &ExecutionHost::default(), true).is_err());
    }

    #[test]
    fn failures_use_the_q3_error_contract() {
        let failure = RemoteIntegrationFailure::new(
            ToolErrorCode::REMOTE_BUNDLE_TAMPERED,
            "chunk hash mismatch",
        )
        .with_hint("regenerate the bundle")
        .with_evidence("offset=1024");
        let data = failure.data();
        assert_eq!(data["code"], "REMOTE_BUNDLE_TAMPERED");
        assert_eq!(data["retryable"], false);
        assert_eq!(data["hint"], "regenerate the bundle");
        assert_eq!(data["evidence"], "offset=1024");
        assert_eq!(data["tool"], "remote_integrate");
        assert!(
            failure
                .to_string()
                .starts_with("error[REMOTE_BUNDLE_TAMPERED]")
        );
        let wrapped: anyhow::Error = failure.into();
        let rpc_data = failure_data(&wrapped).unwrap();
        assert_eq!(rpc_data["code"], "REMOTE_BUNDLE_TAMPERED");
    }

    #[test]
    fn bundle_heads_parse_and_ignore_malformed_lines() {
        let heads = parse_bundle_heads(
            "0123456789012345678901234567890123456789 refs/heads/main\n\
             not-a-hash refs/heads/x\n",
        );
        assert_eq!(heads.len(), 1);
        assert_eq!(heads[0].reference, "refs/heads/main");
    }
}
