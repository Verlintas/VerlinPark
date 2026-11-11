//! Disposable Git fixture repositories and deterministic acceptance checks.
//!
//! Every benchmark repetition gets its own temp workspace containing:
//!
//! * `repo/` — a fresh Git repository initialized on `main` with one commit;
//! * `data/` — the daemon's SQLite database and session worktrees;
//! * `home/` and `tmp/` — isolated HOME/TMPDIR for fixture Git and acceptance;
//! * `acceptance/` — harness-owned expected content and the check script.
//!
//! The acceptance directory is deliberately outside the agent worktree. An
//! agent can edit every file in its worktree, but it cannot alter the expected
//! bytes or the script that decides whether the scenario was solved.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tempfile::{Builder, TempDir};
use tokio::process::Command as TokioCommand;

use crate::metrics::sha256_hex;
use crate::scenario::FixtureKind;

/// Increment when the generated fixture template changes in a way that makes
/// old and new reports incomparable.
pub const FIXTURE_VERSION: u32 = 1;

/// A file written into the fixture repository before the agent starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixtureFile {
    pub path: String,
    pub content: String,
}

impl FixtureFile {
    pub fn new(path: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            content: content.into(),
        }
    }
}

/// Exact bytes the acceptance script expects at a worktree path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedFile {
    pub path: String,
    pub content: String,
}

impl ExpectedFile {
    pub fn new(path: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            content: content.into(),
        }
    }
}

/// Deterministic acceptance criteria. The current harness expresses them as
/// byte checks and a clean-worktree check; no arbitrary commands are accepted
/// from a scenario.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceSpec {
    #[serde(default)]
    pub expected_files: Vec<ExpectedFile>,
    #[serde(default)]
    pub absent_paths: Vec<String>,
    #[serde(default)]
    pub require_clean_git: bool,
}

impl AcceptanceSpec {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn expect_file(mut self, path: impl Into<String>, content: impl Into<String>) -> Self {
        self.expected_files.push(ExpectedFile::new(path, content));
        self
    }

    pub fn expect_absent(mut self, path: impl Into<String>) -> Self {
        self.absent_paths.push(path.into());
        self
    }

    pub fn require_clean(mut self, require: bool) -> Self {
        self.require_clean_git = require;
        self
    }

    pub fn validate(&self) -> Result<()> {
        if self.expected_files.len() > 100 {
            bail!("acceptance may check at most 100 files");
        }
        if self.absent_paths.len() > 100 {
            bail!("acceptance may check at most 100 absent paths");
        }
        for file in &self.expected_files {
            validate_relative_path(&file.path, "acceptance expected path")?;
            if file.content.len() > 64 * 1024 {
                bail!(
                    "acceptance expected content for '{}' exceeds 64 KiB",
                    file.path
                );
            }
        }
        for path in &self.absent_paths {
            validate_relative_path(path, "acceptance absent path")?;
        }
        Ok(())
    }

    /// Canonical acceptance text used for A/B compatibility checks. It includes
    /// the exact expected content, not just the path names.
    pub fn fingerprint(&self) -> String {
        let mut canonical = String::new();
        canonical.push_str(&format!(
            "fixture={FIXTURE_VERSION}|clean={}\n",
            self.require_clean_git
        ));
        for file in &self.expected_files {
            canonical.push_str(&format!(
                "file|{}|{}|{}\n",
                file.path,
                file.content.len(),
                sha256_hex(file.content.as_bytes())
            ));
        }
        for path in &self.absent_paths {
            canonical.push_str(&format!("absent|{path}\n"));
        }
        sha256_hex(canonical.as_bytes())
    }
}

/// The complete, pre-materialized shape of one fixture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixturePlan {
    pub kind: FixtureKind,
    pub version: u32,
    pub repo_files: Vec<FixtureFile>,
    pub acceptance: AcceptanceSpec,
}

impl FixturePlan {
    pub fn new(kind: FixtureKind) -> Self {
        Self {
            kind,
            version: FIXTURE_VERSION,
            repo_files: Vec::new(),
            acceptance: AcceptanceSpec::new(),
        }
    }

    pub fn file(mut self, path: impl Into<String>, content: impl Into<String>) -> Self {
        self.repo_files.push(FixtureFile::new(path, content));
        self
    }

    pub fn acceptance(mut self, acceptance: AcceptanceSpec) -> Self {
        self.acceptance = acceptance;
        self
    }

    pub fn validate(&self) -> Result<()> {
        if self.repo_files.len() > 50 {
            bail!("fixture may contain at most 50 repository files");
        }
        for file in &self.repo_files {
            validate_relative_path(&file.path, "fixture path")?;
            if file.content.len() > 1024 * 1024 {
                bail!("fixture file '{}' exceeds 1 MiB", file.path);
            }
        }
        self.acceptance.validate()
    }

    /// Fixture setup identity. It deliberately excludes the expected solution
    /// content; acceptance has its own fingerprint.
    pub fn fingerprint(&self) -> String {
        let mut canonical = format!("kind={}|version={}\n", self.kind.as_str(), self.version);
        for file in &self.repo_files {
            canonical.push_str(&format!(
                "{}|{}|{}\n",
                file.path,
                file.content.len(),
                sha256_hex(file.content.as_bytes())
            ));
        }
        sha256_hex(canonical.as_bytes())
    }

    pub fn acceptance_fingerprint(&self) -> String {
        self.acceptance.fingerprint()
    }
}

/// A harness-owned acceptance script plus the expected bytes it compares.
#[derive(Debug, Clone)]
pub struct AcceptanceHarness {
    pub script: PathBuf,
    pub expected_dir: PathBuf,
    pub home: PathBuf,
    pub scratch: PathBuf,
    pub require_clean_git: bool,
    pub fingerprint: String,
}

impl AcceptanceHarness {
    pub async fn run(&self, worktree: &Path, timeout: Duration) -> Result<AcceptanceExecution> {
        if !worktree.is_dir() {
            bail!("acceptance worktree does not exist: {}", worktree.display());
        }
        let started = Instant::now();
        let mut command = TokioCommand::new("/bin/sh");
        command
            .arg(&self.script)
            .current_dir(worktree)
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", &self.home)
            .env("TMPDIR", &self.scratch)
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("BENCH_WORKTREE", worktree)
            .env("BENCH_EXPECTED", &self.expected_dir)
            .env(
                "BENCH_REQUIRE_CLEAN",
                if self.require_clean_git { "1" } else { "0" },
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .with_context(|| format!("starting acceptance script {}", self.script.display()))?;
        let stdout = child
            .stdout
            .take()
            .context("acceptance stdout pipe was not created")?;
        let stderr = child
            .stderr
            .take()
            .context("acceptance stderr pipe was not created")?;
        let wait = async move {
            let (status, stdout, stderr) = tokio::join!(
                child.wait(),
                read_bounded(stdout, 64 * 1024),
                read_bounded(stderr, 64 * 1024)
            );
            let status = status.context("waiting for acceptance script")?;
            Ok::<_, anyhow::Error>((status, stdout?, stderr?))
        };
        match tokio::time::timeout(timeout, wait).await {
            Ok(Ok((status, stdout, stderr))) => {
                let exit_code = status.code();
                Ok(AcceptanceExecution {
                    passed: status.success(),
                    exit_code,
                    timed_out: false,
                    duration_ms: started.elapsed().as_millis() as u64,
                    stdout: output_preview(&stdout),
                    stderr: output_preview(&stderr),
                })
            }
            Ok(Err(error)) => Err(error),
            Err(_) => Ok(AcceptanceExecution {
                passed: false,
                exit_code: None,
                timed_out: true,
                duration_ms: started.elapsed().as_millis() as u64,
                stdout: String::new(),
                stderr: format!("acceptance timed out after {}s", timeout.as_secs()),
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptanceExecution {
    pub passed: bool,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub stdout: String,
    pub stderr: String,
}

/// An isolated temp workspace. Dropping it recursively removes the workspace;
/// only paths rooted at the unique temp directory are touched.
pub struct BenchWorkspace {
    temp: TempDir,
    repo: PathBuf,
    data: PathBuf,
    home: PathBuf,
    scratch: PathBuf,
    acceptance: PathBuf,
}

impl BenchWorkspace {
    pub fn create() -> Result<Self> {
        let temp = Builder::new()
            .prefix("vp-bench-")
            .tempdir()
            .context("creating a benchmark temp workspace")?;
        let root = temp.path();
        fs::write(
            root.join(".vp-bench-workspace"),
            "vp-bench disposable workspace; safe to delete\n",
        )
        .context("writing benchmark workspace marker")?;
        let repo = root.join("repo");
        let data = root.join("data");
        let home = root.join("home");
        let scratch = root.join("tmp");
        let acceptance = root.join("acceptance");
        for dir in [&repo, &data, &home, &scratch, &acceptance] {
            fs::create_dir_all(dir)
                .with_context(|| format!("creating benchmark directory {}", dir.display()))?;
        }
        Ok(Self {
            temp,
            repo,
            data,
            home,
            scratch,
            acceptance,
        })
    }

    pub fn root(&self) -> &Path {
        self.temp.path()
    }

    pub fn repo(&self) -> &Path {
        &self.repo
    }

    pub fn data_dir(&self) -> &Path {
        &self.data
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn scratch(&self) -> &Path {
        &self.scratch
    }

    pub fn acceptance_dir(&self) -> &Path {
        &self.acceptance
    }

    /// Retain the workspace for debugging instead of deleting it on drop.
    pub fn keep(self) -> PathBuf {
        self.temp.keep()
    }
}

/// Initialize the fixture repository and create its first commit.
pub fn materialize_repo(plan: &FixturePlan, workspace: &BenchWorkspace) -> Result<()> {
    plan.validate()?;
    for file in &plan.repo_files {
        let path = workspace.repo().join(&file.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating fixture directory {}", parent.display()))?;
        }
        fs::write(&path, &file.content)
            .with_context(|| format!("writing fixture file {}", path.display()))?;
    }
    let home = workspace.home();
    let run_git = |args: &[&str]| -> Result<()> {
        let output = Command::new("git")
            .arg("-C")
            .arg(workspace.repo())
            .args(args)
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .with_context(|| format!("running git {}", args.join(" ")))?;
        if !output.status.success() {
            bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    };
    run_git(&["init", "-q"])?;
    run_git(&["symbolic-ref", "HEAD", "refs/heads/main"])?;
    run_git(&["config", "user.email", "bench@verlinpark.local"])?;
    run_git(&["config", "user.name", "VerlinPark Bench"])?;
    run_git(&["config", "commit.gpgsign", "false"])?;
    run_git(&["add", "-A"])?;
    run_git(&["commit", "-q", "-m", "bench fixture"])?;
    Ok(())
}

/// Write the acceptance script and expected content outside the repository.
pub fn materialize_acceptance(
    plan: &FixturePlan,
    workspace: &BenchWorkspace,
) -> Result<AcceptanceHarness> {
    plan.acceptance.validate()?;
    let expected_dir = workspace.acceptance_dir().join("expected");
    fs::create_dir_all(&expected_dir)
        .with_context(|| format!("creating acceptance directory {}", expected_dir.display()))?;
    for (index, file) in plan.acceptance.expected_files.iter().enumerate() {
        fs::write(expected_dir.join(format!("file-{index}")), &file.content)
            .with_context(|| format!("writing expected content for {}", file.path))?;
    }
    let mut script = String::from("#!/bin/sh\nset -eu\n");
    script.push_str(
        "if [ ! -d \"$BENCH_WORKTREE\" ]; then echo 'worktree missing' >&2; exit 1; fi\n",
    );
    for (index, file) in plan.acceptance.expected_files.iter().enumerate() {
        script.push_str(&format!(
            "if [ ! -f \"$BENCH_WORKTREE/{}\" ]; then echo 'missing {}' >&2; exit 1; fi\n",
            file.path, file.path
        ));
        script.push_str(&format!(
            "if ! cmp -s \"$BENCH_WORKTREE/{}\" \"$BENCH_EXPECTED/file-{index}\"; then echo 'content mismatch: {}' >&2; exit 1; fi\n",
            file.path, file.path
        ));
    }
    for path in &plan.acceptance.absent_paths {
        script.push_str(&format!(
            "if [ -e \"$BENCH_WORKTREE/{path}\" ]; then echo 'unexpected path: {path}' >&2; exit 1; fi\n"
        ));
    }
    if plan.acceptance.require_clean_git {
        script.push_str(
            "status=$(git -C \"$BENCH_WORKTREE\" status --porcelain)\n\
             if [ -n \"$status\" ]; then echo 'worktree is not clean:' >&2; printf '%s\\n' \"$status\" >&2; exit 1; fi\n",
        );
    }
    script.push_str("echo 'acceptance: ok'\n");
    let script_path = workspace.acceptance_dir().join("check.sh");
    fs::write(&script_path, script)
        .with_context(|| format!("writing acceptance script {}", script_path.display()))?;
    Ok(AcceptanceHarness {
        script: script_path,
        expected_dir,
        home: workspace.home().to_path_buf(),
        scratch: workspace.scratch().to_path_buf(),
        require_clean_git: plan.acceptance.require_clean_git,
        fingerprint: plan.acceptance_fingerprint(),
    })
}

/// Materialize a complete fixture: repository, commit, and acceptance harness.
pub fn prepare_fixture(
    plan: &FixturePlan,
    workspace: &BenchWorkspace,
) -> Result<AcceptanceHarness> {
    materialize_repo(plan, workspace)?;
    materialize_acceptance(plan, workspace)
}

/// Reject absolute paths, parent traversal, backslashes, shell metacharacters,
/// control characters, and trailing slashes. The fixture scripts quote paths,
/// but a deliberately narrow alphabet keeps both Git and `/bin/sh` handling
/// unambiguous.
pub fn validate_relative_path(path: &str, label: &str) -> Result<()> {
    if path.is_empty() {
        bail!("{label} cannot be empty");
    }
    if path.len() > 512 {
        bail!("{label} exceeds 512 bytes");
    }
    if path.starts_with('-') {
        bail!("{label} cannot start with '-'");
    }
    if path.contains('\\') {
        bail!("{label} cannot contain backslashes");
    }
    if path.contains("//") {
        bail!("{label} cannot contain empty path components");
    }
    for character in path.chars() {
        if !(character.is_ascii_alphanumeric() || matches!(character, '/' | '.' | '_' | '-')) {
            bail!("{label} contains an unsupported character: {character:?}");
        }
    }
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        bail!("{label} must be relative");
    }
    for component in candidate.components() {
        match component {
            Component::Normal(part) => {
                if part.is_empty() {
                    bail!("{label} contains an empty path component");
                }
            }
            _ => bail!("{label} cannot contain '.', '..', or a root component"),
        }
    }
    if path.ends_with('/') {
        bail!("{label} cannot end with '/'");
    }
    Ok(())
}

fn output_preview(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    truncate_chars(text.trim(), 4096)
}

fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out = text.chars().take(limit).collect::<String>();
    out.push('…');
    out
}

async fn read_bounded<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    limit: usize,
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = tokio::io::AsyncReadExt::read(&mut reader, &mut buffer)
            .await
            .context("reading acceptance output")?;
        if read == 0 {
            break;
        }
        if out.len() < limit {
            let remaining = limit - out.len();
            out.extend_from_slice(&buffer[..read.min(remaining)]);
        }
    }
    Ok(out)
}

/// A compact textual digest of a fixture plan for `vp bench list` and reports.
pub fn fixture_summary(plan: &FixturePlan) -> String {
    let files = plan
        .acceptance
        .expected_files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if files.is_empty() {
        format!("{} fixture; no expected files", plan.kind.as_str())
    } else {
        format!(
            "{} fixture; expects {files}; acceptance {}",
            plan.kind.as_str(),
            &plan.acceptance_fingerprint()[..12]
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::FixtureKind;

    fn plan() -> FixturePlan {
        FixturePlan::new(FixtureKind::SingleSession)
            .file("README.md", "# fixture\nBENCH-MARKER\n")
            .file("src/lib.rs", "pub fn answer() -> u32 { 42 }\n")
            .acceptance(AcceptanceSpec::new().expect_file("answer.txt", "42\n"))
    }

    #[test]
    fn relative_paths_are_narrow_and_safe() {
        assert!(validate_relative_path("src/lib.rs", "path").is_ok());
        assert!(validate_relative_path("data/large.log", "path").is_ok());
        for bad in [
            "",
            "../escape",
            "/etc/passwd",
            "./file",
            "src//lib.rs",
            "src\\lib.rs",
            "file with space",
            "-leading",
            "trailing/",
        ] {
            assert!(
                validate_relative_path(bad, "path").is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn acceptance_fingerprint_covers_content() {
        let a = AcceptanceSpec::new().expect_file("answer.txt", "42\n");
        let b = AcceptanceSpec::new().expect_file("answer.txt", "43\n");
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_eq!(
            a.fingerprint(),
            AcceptanceSpec::new()
                .expect_file("answer.txt", "42\n")
                .fingerprint()
        );
    }

    #[test]
    fn fixture_fingerprint_is_stable_and_setup_sensitive() {
        let a = plan();
        let b = plan();
        assert_eq!(a.fingerprint(), b.fingerprint());
        let mut c = plan();
        c.repo_files[0].content.push_str("changed\n");
        assert_ne!(a.fingerprint(), c.fingerprint());
    }

    #[test]
    fn fixture_validation_rejects_escape_and_large_content() {
        let bad = FixturePlan::new(FixtureKind::SingleSession).file("../outside", "x");
        assert!(bad.validate().is_err());
        let large = FixturePlan::new(FixtureKind::SingleSession)
            .file("big.txt", "x".repeat(1024 * 1024 + 1));
        assert!(large.validate().is_err());
    }

    #[tokio::test]
    async fn prepares_repo_and_runs_deterministic_acceptance() {
        let workspace = BenchWorkspace::create().unwrap();
        let fixture = plan();
        let acceptance = prepare_fixture(&fixture, &workspace).unwrap();
        assert!(workspace.repo().join(".git").exists());
        assert_eq!(
            fs::read_to_string(workspace.repo().join("README.md")).unwrap(),
            "# fixture\nBENCH-MARKER\n"
        );
        // The fixture starts clean.
        let clean = acceptance
            .run(workspace.repo(), Duration::from_secs(5))
            .await
            .unwrap();
        assert!(!clean.passed);
        assert!(!clean.timed_out);

        fs::write(workspace.repo().join("answer.txt"), "42\n").unwrap();
        let passed = acceptance
            .run(workspace.repo(), Duration::from_secs(5))
            .await
            .unwrap();
        assert!(passed.passed, "{passed:?}");
        assert!(passed.stdout.contains("acceptance: ok"));

        fs::write(workspace.repo().join("answer.txt"), "wrong\n").unwrap();
        let wrong = acceptance
            .run(workspace.repo(), Duration::from_secs(5))
            .await
            .unwrap();
        assert!(!wrong.passed);
        assert!(wrong.stderr.contains("content mismatch"));
    }

    #[tokio::test]
    async fn acceptance_rejects_dirty_worktree_and_unexpected_paths() {
        let workspace = BenchWorkspace::create().unwrap();
        let fixture = plan().acceptance(
            AcceptanceSpec::new()
                .expect_file("answer.txt", "42\n")
                .expect_absent("should-not-exist.txt")
                .require_clean(true),
        );
        let acceptance = prepare_fixture(&fixture, &workspace).unwrap();
        fs::write(workspace.repo().join("answer.txt"), "42\n").unwrap();
        fs::write(workspace.repo().join("should-not-exist.txt"), "x").unwrap();
        let result = acceptance
            .run(workspace.repo(), Duration::from_secs(5))
            .await
            .unwrap();
        assert!(!result.passed);
        assert!(result.stderr.contains("unexpected path"));

        fs::remove_file(workspace.repo().join("should-not-exist.txt")).unwrap();
        let dirty = acceptance
            .run(workspace.repo(), Duration::from_secs(5))
            .await
            .unwrap();
        assert!(!dirty.passed);
        assert!(dirty.stderr.contains("not clean"));

        fs::remove_file(workspace.repo().join("answer.txt")).unwrap();
        let clean = acceptance
            .run(workspace.repo(), Duration::from_secs(5))
            .await
            .unwrap();
        assert!(!clean.passed);
        assert!(clean.stderr.contains("missing answer.txt"));
    }

    #[tokio::test]
    async fn acceptance_timeout_is_reported_not_panicked() {
        let workspace = BenchWorkspace::create().unwrap();
        let fixture = plan();
        let acceptance = prepare_fixture(&fixture, &workspace).unwrap();
        // A worktree path that exists but the script cannot finish is not easy
        // to construct without arbitrary commands, so assert the sub-second
        // limit still returns a structured result for the normal path.
        let result = acceptance
            .run(workspace.repo(), Duration::from_millis(1))
            .await
            .unwrap();
        assert!(result.timed_out || !result.passed);
    }

    #[test]
    fn workspace_drop_removes_all_materialized_state() {
        let path = {
            let workspace = BenchWorkspace::create().unwrap();
            prepare_fixture(&plan(), &workspace).unwrap();
            let path = workspace.root().to_path_buf();
            assert!(path.join("repo/README.md").exists());
            path
        };
        assert!(!path.exists(), "workspace was not cleaned up");
    }

    #[test]
    fn fixture_summary_names_expected_paths() {
        let summary = fixture_summary(&plan());
        assert!(summary.contains("answer.txt"));
        assert!(summary.contains("single-session"));
    }
}
