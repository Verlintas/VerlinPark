//! Explicit user LaunchAgent lifecycle for the local daemon (ADR-0186).
//!
//! VerlinPark never installs a LaunchAgent automatically and never asks for
//! root. `vp daemon-agent install` is an operator command that writes one
//! user-level plist under `~/Library/LaunchAgents`, then loads it with
//! `launchctl bootstrap gui/<uid> <plist>`. `status` is read-only and never
//! starts the daemon. `uninstall` boots the job out and removes the plist.
//!
//! The module is deliberately testable without launchd: every launchctl call
//! goes through [`LaunchctlRunner`], and path discovery accepts explicit roots
//! so a test can use a temporary home and data directory.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use vp_protocol::discovery::Discovery;
use vp_protocol::types::DaemonAgentStatusResult;

pub const DAEMON_AGENT_LABEL: &str = "com.verlinpark.daemon";
pub const DAEMON_AGENT_PLIST: &str = "com.verlinpark.daemon.plist";
const MAX_COMMAND_OUTPUT_CHARS: usize = 600;

/// Result of one launchctl subprocess. `status` is `None` when the binary
/// could not be executed at all (for example on a machine where launchd is not
/// available); that distinction is what keeps status honest instead of
/// reporting a false negative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchctlOutput {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl LaunchctlOutput {
    pub fn success(stdout: impl Into<String>) -> Self {
        Self {
            status: Some(0),
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    pub fn failure(status: i32, stderr: impl Into<String>) -> Self {
        Self {
            status: Some(status),
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            status: None,
            stdout: String::new(),
            stderr: message.into(),
        }
    }

    pub fn succeeded(&self) -> bool {
        self.status == Some(0)
    }

    fn combined(&self) -> String {
        let text = if self.stderr.trim().is_empty() {
            self.stdout.clone()
        } else {
            format!("{} {}", self.stdout, self.stderr)
        };
        text.chars().take(MAX_COMMAND_OUTPUT_CHARS).collect()
    }
}

/// Injectable launchctl surface. The real implementation shells out; tests use
/// an in-memory fake and assert that no daemon is spawned and no root command
/// is constructed.
pub trait LaunchctlRunner: Send + Sync {
    fn run(&self, args: &[String]) -> LaunchctlOutput;
}

/// The only production runner. It never uses sudo and never invokes a shell:
/// arguments are passed directly to `/bin/launchctl`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemLaunchctl;

impl LaunchctlRunner for SystemLaunchctl {
    fn run(&self, args: &[String]) -> LaunchctlOutput {
        match Command::new("/bin/launchctl").args(args).output() {
            Ok(output) => LaunchctlOutput {
                status: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            },
            Err(error) => LaunchctlOutput::unavailable(format!("launchctl unavailable: {error}")),
        }
    }
}

/// Filesystem locations for one user-level installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonAgentPaths {
    pub home: PathBuf,
    pub data_dir: PathBuf,
    pub binary: PathBuf,
    pub uid: u32,
}

impl DaemonAgentPaths {
    /// Resolve from the environment, with explicit overrides for tests and
    /// development. `binary` should point at `verlinpadd`, not the `vp` CLI.
    pub fn from_env(data_dir: Option<PathBuf>, binary: Option<PathBuf>) -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .or_else(dirs::home_dir)
            .ok_or_else(|| anyhow!("HOME is not set; cannot locate ~/Library/LaunchAgents"))?;
        let data_dir = data_dir.unwrap_or_else(vp_store::paths::data_dir);
        let binary = resolve_daemon_binary(binary)?;
        let uid = current_uid();
        Ok(Self {
            home,
            data_dir,
            binary,
            uid,
        })
    }

    pub fn plist_path(&self) -> PathBuf {
        self.home
            .join("Library")
            .join("LaunchAgents")
            .join(DAEMON_AGENT_PLIST)
    }

    pub fn log_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    pub fn stdout_log(&self) -> PathBuf {
        self.log_dir().join("daemon-agent.out.log")
    }

    pub fn stderr_log(&self) -> PathBuf {
        self.log_dir().join("daemon-agent.err.log")
    }

    fn discovery_path(&self) -> PathBuf {
        self.data_dir.join("run").join("daemon.json")
    }

    fn service_target(&self) -> String {
        format!("gui/{}/{}", self.uid, DAEMON_AGENT_LABEL)
    }

    fn gui_domain(&self) -> String {
        format!("gui/{}", self.uid)
    }
}

/// Find `verlinpadd` without guessing across unrelated directories. Priority:
/// explicit override, `VERLINPARK_DAEMON_BIN`, sibling of the current
/// executable, then each `PATH` entry. A missing binary is a hard error; the
/// installer never writes a plist that points at `vp`.
pub fn resolve_daemon_binary(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return absolute_existing_binary(path);
    }
    if let Some(path) = std::env::var_os("VERLINPARK_DAEMON_BIN") {
        return absolute_existing_binary(PathBuf::from(path));
    }
    let current = std::env::current_exe().context("locating the current executable")?;
    if let Some(parent) = current.parent() {
        let sibling = parent.join("verlinpadd");
        if is_executable_file(&sibling) {
            return Ok(sibling.canonicalize().unwrap_or(sibling));
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join("verlinpadd");
            if is_executable_file(&candidate) {
                return Ok(candidate.canonicalize().unwrap_or(candidate));
            }
        }
    }
    bail!(
        "verlinpadd was not found next to the vp binary or on PATH; pass --binary or set VERLINPARK_DAEMON_BIN"
    )
}

fn absolute_existing_binary(path: PathBuf) -> Result<PathBuf> {
    if !path.is_absolute() {
        bail!("daemon binary must be an absolute path: {}", path.display());
    }
    if !is_executable_file(&path) {
        bail!(
            "daemon binary is missing or not executable: {}",
            path.display()
        );
    }
    Ok(path.canonicalize().unwrap_or(path))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: `getuid` has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    501
}

/// One install/uninstall outcome. `commands` is the exact audited launchctl
/// argv sequence, never including a secret or a shell string.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DaemonAgentMutation {
    pub action: String,
    pub label: String,
    pub plist_path: String,
    pub changed: bool,
    pub dry_run: bool,
    pub plist: Option<String>,
    pub commands: Vec<String>,
    pub detail: String,
}

pub struct DaemonAgent<'a> {
    paths: DaemonAgentPaths,
    runner: &'a dyn LaunchctlRunner,
}

impl std::fmt::Debug for DaemonAgent<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DaemonAgent")
            .field("paths", &self.paths)
            .finish_non_exhaustive()
    }
}

impl<'a> DaemonAgent<'a> {
    pub fn new(paths: DaemonAgentPaths, runner: &'a dyn LaunchctlRunner) -> Self {
        Self { paths, runner }
    }

    pub fn paths(&self) -> &DaemonAgentPaths {
        &self.paths
    }

    /// Deterministic plist. Keys are emitted in a fixed order, values are
    /// XML-escaped, and no timestamp, hostname, or environment value is
    /// included. Launchd owns the log files; the daemon owns everything else.
    pub fn plist_contents(&self) -> String {
        let mut plist = String::new();
        plist.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        plist.push_str("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n");
        plist.push_str("<plist version=\"1.0\">\n<dict>\n");
        plist.push_str("  <key>Label</key>\n");
        let _ = writeln!(
            plist,
            "  <string>{}</string>",
            xml_escape(DAEMON_AGENT_LABEL)
        );
        plist.push_str("  <key>ProgramArguments</key>\n  <array>\n");
        for argument in [
            self.paths.binary.to_string_lossy().to_string(),
            "--data-dir".to_string(),
            self.paths.data_dir.to_string_lossy().to_string(),
        ] {
            let _ = writeln!(plist, "    <string>{}</string>", xml_escape(&argument));
        }
        plist.push_str("  </array>\n");
        plist.push_str("  <key>RunAtLoad</key>\n  <true/>\n");
        plist.push_str("  <key>KeepAlive</key>\n  <true/>\n");
        plist.push_str("  <key>ProcessType</key>\n  <string>Interactive</string>\n");
        plist.push_str("  <key>ThrottleInterval</key>\n  <integer>10</integer>\n");
        plist.push_str("  <key>Umask</key>\n  <integer>63</integer>\n");
        plist.push_str("  <key>StandardOutPath</key>\n");
        let _ = writeln!(
            plist,
            "  <string>{}</string>",
            xml_escape(&self.paths.stdout_log().to_string_lossy())
        );
        plist.push_str("  <key>StandardErrorPath</key>\n");
        let _ = writeln!(
            plist,
            "  <string>{}</string>",
            xml_escape(&self.paths.stderr_log().to_string_lossy())
        );
        plist.push_str("</dict>\n</plist>\n");
        plist
    }

    /// Read-only status. It never writes the plist or starts the daemon.
    pub fn status(&self) -> DaemonAgentStatusResult {
        let plist_path = self.paths.plist_path();
        let installed = plist_path.is_file();
        let print = self
            .runner
            .run(&["print".to_string(), self.paths.service_target()]);
        let disabled_query = self
            .runner
            .run(&["print-disabled".to_string(), self.paths.gui_domain()]);
        let disabled = parse_disabled(&disabled_query);
        let launchctl_available = print.status.is_some() || disabled_query.status.is_some();
        let discovery = read_live_discovery(&self.paths.discovery_path());
        let mut loaded = false;
        let mut running = false;
        let mut pid: Option<u32> = None;
        let mut detail = String::new();
        let mut unknown = false;
        if print.succeeded() {
            loaded = true;
            let (state_pid, state_running) = parse_print(&print.stdout);
            pid = state_pid;
            running = state_running;
        } else if print.status.is_none() {
            unknown = installed;
            detail = bounded(&print.stderr);
        } else {
            let combined = print.combined().to_ascii_lowercase();
            if combined.contains("could not find service")
                || combined.contains("service not found")
                || combined.contains("no such process")
                || combined.contains("not loaded")
                || combined.contains("disabled")
            {
                loaded = false;
            } else {
                unknown = installed;
                detail = bounded(&print.combined());
            }
        }
        let supervised = running && pid.is_some();
        if running && let Some(running_pid) = pid {
            detail = format!("launchd reports pid {running_pid}");
        } else if let Some(discovery) = discovery.as_ref() {
            detail = format!(
                "daemon pid {} is alive but is not running under {DAEMON_AGENT_LABEL}",
                discovery.pid
            );
        }
        let state = if disabled {
            "disabled"
        } else if loaded && running {
            "running"
        } else if loaded {
            "loaded"
        } else if unknown {
            "unknown"
        } else if installed {
            "installed"
        } else {
            "not_installed"
        };
        DaemonAgentStatusResult {
            label: DAEMON_AGENT_LABEL.to_string(),
            plist_path: plist_path.to_string_lossy().to_string(),
            installed,
            loaded,
            disabled,
            running,
            supervised,
            pid,
            state: state.to_string(),
            detail,
            launchctl_available,
        }
    }

    /// Install the user LaunchAgent. `confirmed` must be true; the CLI obtains
    /// explicit confirmation. A running daemon under another supervisor is a
    /// hard refusal even with `force`; force only permits replacing an
    /// existing, not-loaded plist.
    pub fn install(
        &self,
        confirmed: bool,
        dry_run: bool,
        force: bool,
    ) -> Result<DaemonAgentMutation> {
        if !confirmed {
            bail!("daemon-agent install requires explicit confirmation");
        }
        let status = self.status();
        if status.loaded {
            bail!(
                "{DAEMON_AGENT_LABEL} is already loaded; uninstall it before reinstalling (force never displaces a loaded job)"
            );
        }
        if read_live_discovery(&self.paths.discovery_path()).is_some() {
            bail!(
                "a daemon is already running outside {DAEMON_AGENT_LABEL}; stop it before installing"
            );
        }
        let plist_path = self.paths.plist_path();
        if !dry_run && plist_path.is_file() && !force {
            bail!(
                "{} already exists; pass --force to replace it or run daemon-agent uninstall first",
                plist_path.display()
            );
        }
        let plist = self.plist_contents();
        let mut commands = vec![
            format!(
                "launchctl bootstrap {} {}",
                self.paths.gui_domain(),
                self.paths.plist_path().display()
            ),
            format!("launchctl kickstart -k {}", self.paths.service_target()),
        ];
        if dry_run {
            return Ok(DaemonAgentMutation {
                action: "install".to_string(),
                label: DAEMON_AGENT_LABEL.to_string(),
                plist_path: self.paths.plist_path().to_string_lossy().to_string(),
                changed: false,
                dry_run: true,
                plist: Some(plist),
                commands: std::mem::take(&mut commands),
                detail: "dry run: launchctl was not invoked and no file was written".to_string(),
            });
        }
        let plist_path = self.paths.plist_path();
        let parent = plist_path
            .parent()
            .ok_or_else(|| anyhow!("LaunchAgents path has no parent"))?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
        std::fs::create_dir_all(self.paths.log_dir())
            .with_context(|| format!("creating {}", self.paths.log_dir().display()))?;
        vp_store::paths::ensure_private_dir(&self.paths.log_dir())?;
        let temp = parent.join(format!(".{DAEMON_AGENT_PLIST}.tmp-{}", std::process::id()));
        std::fs::write(&temp, plist.as_bytes())
            .with_context(|| format!("writing {}", temp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o644))?;
        }
        std::fs::rename(&temp, &plist_path)
            .with_context(|| format!("installing {}", plist_path.display()))?;
        let bootstrap = self.runner.run(&[
            "bootstrap".to_string(),
            self.paths.gui_domain(),
            plist_path.to_string_lossy().to_string(),
        ]);
        if !bootstrap.succeeded() {
            let _ = std::fs::remove_file(&plist_path);
            bail!(
                "launchctl bootstrap failed ({}): {}",
                bootstrap.status.unwrap_or(-1),
                bootstrap.combined()
            );
        }
        // Best effort: bootstrap already starts a RunAtLoad job. Kickstart
        // makes an idempotent reinstall deterministic without requiring a
        // separate manual load.
        let kickstart = self.runner.run(&[
            "kickstart".to_string(),
            "-k".to_string(),
            self.paths.service_target(),
        ]);
        let detail = if kickstart.succeeded() {
            "installed and started the user LaunchAgent".to_string()
        } else {
            format!(
                "installed; kickstart returned {} but launchd will start it on demand",
                kickstart.status.unwrap_or(-1)
            )
        };
        Ok(DaemonAgentMutation {
            action: "install".to_string(),
            label: DAEMON_AGENT_LABEL.to_string(),
            plist_path: plist_path.to_string_lossy().to_string(),
            changed: true,
            dry_run: false,
            plist: None,
            commands,
            detail,
        })
    }

    /// Remove the LaunchAgent. The job is booted out first; a failed bootout
    /// other than "not found" aborts before the plist is deleted so a running
    /// job cannot be orphaned.
    pub fn uninstall(&self, confirmed: bool, dry_run: bool) -> Result<DaemonAgentMutation> {
        if !confirmed {
            bail!("daemon-agent uninstall requires explicit confirmation");
        }
        let plist_path = self.paths.plist_path();
        let installed = plist_path.is_file();
        let service_target = self.paths.service_target();
        let commands = vec![format!("launchctl bootout {service_target}")];
        if dry_run {
            return Ok(DaemonAgentMutation {
                action: "uninstall".to_string(),
                label: DAEMON_AGENT_LABEL.to_string(),
                plist_path: plist_path.to_string_lossy().to_string(),
                changed: false,
                dry_run: true,
                plist: None,
                commands,
                detail: "dry run: launchctl was not invoked and no file was removed".to_string(),
            });
        }
        let bootout = self
            .runner
            .run(&["bootout".to_string(), service_target.clone()]);
        if !bootout.succeeded() && bootout.status.is_some() {
            let combined = bootout.combined().to_ascii_lowercase();
            let not_found = combined.contains("could not find service")
                || combined.contains("service not found")
                || combined.contains("no such process")
                || combined.contains("not loaded");
            if !not_found {
                bail!(
                    "launchctl bootout failed ({}): {}",
                    bootout.status.unwrap_or(-1),
                    bootout.combined()
                );
            }
        }
        let removed = if installed {
            std::fs::remove_file(&plist_path)
                .with_context(|| format!("removing {}", plist_path.display()))?;
            true
        } else {
            false
        };
        Ok(DaemonAgentMutation {
            action: "uninstall".to_string(),
            label: DAEMON_AGENT_LABEL.to_string(),
            plist_path: plist_path.to_string_lossy().to_string(),
            changed: removed,
            dry_run: false,
            plist: None,
            commands,
            detail: if removed {
                "booted the job out and removed the plist".to_string()
            } else {
                "the LaunchAgent plist was not installed".to_string()
            },
        })
    }
}

fn parse_print(stdout: &str) -> (Option<u32>, bool) {
    let mut pid = None;
    let mut running = false;
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("pid = ") {
            pid = value.trim().parse::<u32>().ok().filter(|pid| *pid > 0);
        }
        if line == "state = running" {
            running = true;
        }
        if line == "state = not running" {
            running = false;
        }
    }
    if pid.is_some() && !running {
        // A printed pid is authoritative even if the state line was absent or
        // wording changed. `supervised` still requires a real pid.
        running = true;
    }
    (pid, running)
}

fn parse_disabled(output: &LaunchctlOutput) -> bool {
    if !output.succeeded() {
        return false;
    }
    output.stdout.lines().any(|line| {
        let line = line.trim();
        line.contains(DAEMON_AGENT_LABEL)
            && (line.ends_with("disabled") || line.contains("=> disabled"))
    })
}

fn read_live_discovery(path: &Path) -> Option<Discovery> {
    let data = std::fs::read(path).ok()?;
    let discovery: Discovery = serde_json::from_slice(&data).ok()?;
    if process_alive(discovery.pid) {
        Some(discovery)
    } else {
        None
    }
}

fn process_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    #[cfg(unix)]
    // SAFETY: signal 0 performs an existence/permission check only.
    unsafe {
        libc::kill(pid as i32, 0) == 0
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn bounded(value: &str) -> String {
    value.chars().take(MAX_COMMAND_OUTPUT_CHARS).collect()
}

fn xml_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    #[derive(Default)]
    struct FakeLaunchctl {
        print: Mutex<Option<LaunchctlOutput>>,
        disabled: Mutex<Option<LaunchctlOutput>>,
        bootstrap: Mutex<Option<LaunchctlOutput>>,
        kickstart: Mutex<Option<LaunchctlOutput>>,
        bootout: Mutex<Option<LaunchctlOutput>>,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl FakeLaunchctl {
        fn running(pid: u32) -> Self {
            let fake = Self::default();
            *fake.print.lock().unwrap() = Some(LaunchctlOutput::success(format!(
                "gui/501/com.verlinpark.daemon = {{\n\tstate = running\n\tpid = {pid}\n}}"
            )));
            *fake.disabled.lock().unwrap() =
                Some(LaunchctlOutput::success("disabled services = {}"));
            fake
        }

        fn not_loaded() -> Self {
            let fake = Self::default();
            *fake.print.lock().unwrap() = Some(LaunchctlOutput::failure(
                113,
                "Could not find service \"com.verlinpark.daemon\" in domain for user gui: 501",
            ));
            *fake.disabled.lock().unwrap() =
                Some(LaunchctlOutput::success("disabled services = {}"));
            fake
        }

        fn disabled() -> Self {
            let fake = Self::not_loaded();
            *fake.disabled.lock().unwrap() = Some(LaunchctlOutput::success(
                "disabled services = {\n\t\"com.verlinpark.daemon\" => disabled\n}",
            ));
            fake
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl LaunchctlRunner for FakeLaunchctl {
        fn run(&self, args: &[String]) -> LaunchctlOutput {
            self.calls.lock().unwrap().push(args.to_vec());
            let slot = match args.first().map(String::as_str) {
                Some("print") => &self.print,
                Some("print-disabled") => &self.disabled,
                Some("bootstrap") => &self.bootstrap,
                Some("kickstart") => &self.kickstart,
                Some("bootout") => &self.bootout,
                _ => return LaunchctlOutput::failure(1, "unexpected command"),
            };
            slot.lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| LaunchctlOutput::success(""))
        }
    }

    fn paths(root: &TempDir) -> DaemonAgentPaths {
        let home = root.path().join("home");
        let data = root.path().join("data");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        let binary = root.path().join("verlinpadd");
        std::fs::write(&binary, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        DaemonAgentPaths {
            home,
            data_dir: data,
            binary,
            uid: 501,
        }
    }

    #[test]
    fn plist_is_deterministic_and_escapes_xml() {
        let root = TempDir::new().unwrap();
        let mut paths = paths(&root);
        paths.data_dir = root.path().join("weird & <data>");
        let runner = FakeLaunchctl::not_loaded();
        let agent = DaemonAgent::new(paths.clone(), &runner);
        let first = agent.plist_contents();
        let second = agent.plist_contents();
        assert_eq!(first, second);
        assert!(first.contains("<string>com.verlinpark.daemon</string>"));
        assert!(first.contains("<key>RunAtLoad</key>\n  <true/>"));
        assert!(first.contains("<key>KeepAlive</key>\n  <true/>"));
        assert!(first.contains("weird &amp; &lt;data&gt;"));
        assert!(first.contains("daemon-agent.out.log"));
        assert!(first.contains("--data-dir"));
        assert!(first.contains(&paths.binary.to_string_lossy().to_string()));
        assert!(!first.contains("sudo"));
    }

    #[test]
    fn status_parses_running_loaded_disabled_and_unknown() {
        let root = TempDir::new().unwrap();
        let paths = paths(&root);
        let running = FakeLaunchctl::running(std::process::id());
        let status = DaemonAgent::new(paths.clone(), &running).status();
        assert_eq!(status.state, "running");
        assert!(status.loaded && status.running && status.supervised);
        assert_eq!(status.pid, Some(std::process::id()));

        let not_loaded = FakeLaunchctl::not_loaded();
        let status = DaemonAgent::new(paths.clone(), &not_loaded).status();
        assert_eq!(status.state, "not_installed");
        assert!(!status.loaded);

        let disabled = FakeLaunchctl::disabled();
        let status = DaemonAgent::new(paths.clone(), &disabled).status();
        assert_eq!(status.state, "disabled");
        assert!(status.disabled);

        let unavailable = FakeLaunchctl {
            print: Mutex::new(Some(LaunchctlOutput::unavailable("no launchd"))),
            disabled: Mutex::new(Some(LaunchctlOutput::unavailable("no launchd"))),
            ..FakeLaunchctl::default()
        };
        let status = DaemonAgent::new(paths, &unavailable).status();
        assert_eq!(status.state, "not_installed");
        assert!(!status.launchctl_available);
    }

    #[test]
    fn install_requires_confirmation_and_dry_run_writes_nothing() {
        let root = TempDir::new().unwrap();
        let paths = paths(&root);
        let runner = FakeLaunchctl::not_loaded();
        let agent = DaemonAgent::new(paths.clone(), &runner);
        assert!(agent.install(false, false, false).is_err());
        let mutation = agent.install(true, true, false).unwrap();
        assert!(mutation.dry_run);
        assert!(!paths.plist_path().exists());
        assert!(!runner.calls().iter().any(|call| call[0] == "bootstrap"));
        assert!(
            mutation
                .commands
                .iter()
                .any(|command| command.contains("bootstrap"))
        );
        assert!(
            mutation
                .commands
                .iter()
                .all(|command| !command.contains("sudo"))
        );
    }

    #[test]
    fn install_refuses_to_overwrite_an_existing_plist_without_force() {
        let root = TempDir::new().unwrap();
        let paths = paths(&root);
        std::fs::create_dir_all(paths.plist_path().parent().unwrap()).unwrap();
        std::fs::write(paths.plist_path(), b"custom plist").unwrap();
        let runner = FakeLaunchctl::not_loaded();
        let agent = DaemonAgent::new(paths.clone(), &runner);
        let error = agent.install(true, false, false).unwrap_err().to_string();
        assert!(error.contains("already exists"), "{error}");
        assert_eq!(
            std::fs::read_to_string(paths.plist_path()).unwrap(),
            "custom plist"
        );
        let dry = agent.install(true, true, false).unwrap();
        assert!(dry.dry_run);
        assert_eq!(
            std::fs::read_to_string(paths.plist_path()).unwrap(),
            "custom plist"
        );
    }

    #[test]
    fn install_refuses_a_daemon_running_under_another_supervisor() {
        let root = TempDir::new().unwrap();
        let paths = paths(&root);
        std::fs::create_dir_all(paths.data_dir.join("run")).unwrap();
        let discovery = Discovery {
            pid: std::process::id(),
            port: 1234,
            token: "test".to_string(),
            protocol: 1,
            version: "test".to_string(),
            started_at: "2026-11-12T00:00:00Z".to_string(),
        };
        std::fs::write(
            paths.discovery_path(),
            serde_json::to_vec(&discovery).unwrap(),
        )
        .unwrap();
        let runner = FakeLaunchctl::not_loaded();
        let agent = DaemonAgent::new(paths, &runner);
        let error = agent.install(true, false, false).unwrap_err().to_string();
        assert!(error.contains("outside"), "{error}");
        assert!(!agent.paths().plist_path().exists());
    }

    #[test]
    fn install_writes_0644_plist_and_bootstraps() {
        let root = TempDir::new().unwrap();
        let paths = paths(&root);
        let runner = FakeLaunchctl::not_loaded();
        *runner.bootstrap.lock().unwrap() = Some(LaunchctlOutput::success(""));
        *runner.kickstart.lock().unwrap() = Some(LaunchctlOutput::success(""));
        let agent = DaemonAgent::new(paths.clone(), &runner);
        let mutation = agent.install(true, false, true).unwrap();
        assert!(mutation.changed);
        assert!(paths.plist_path().is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(paths.plist_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o644);
        }
        let calls = runner.calls();
        assert!(calls.iter().any(|call| call[0] == "bootstrap"));
        assert!(calls.iter().all(|call| call[0] != "sudo"));
        assert!(
            calls
                .iter()
                .all(|call| call.first().map(String::as_str) != Some("bash"))
        );
    }

    #[test]
    fn install_refuses_loaded_service_without_force() {
        let root = TempDir::new().unwrap();
        let paths = paths(&root);
        std::fs::create_dir_all(paths.plist_path().parent().unwrap()).unwrap();
        std::fs::write(paths.plist_path(), b"plist").unwrap();
        let runner = FakeLaunchctl::running(std::process::id());
        let agent = DaemonAgent::new(paths, &runner);
        let error = agent.install(true, false, false).unwrap_err().to_string();
        assert!(error.contains("already loaded"), "{error}");
    }

    #[test]
    fn uninstall_boots_out_and_removes_the_plist() {
        let root = TempDir::new().unwrap();
        let paths = paths(&root);
        std::fs::create_dir_all(paths.plist_path().parent().unwrap()).unwrap();
        std::fs::write(paths.plist_path(), b"plist").unwrap();
        let runner = FakeLaunchctl::not_loaded();
        *runner.bootout.lock().unwrap() = Some(LaunchctlOutput::success(""));
        let agent = DaemonAgent::new(paths.clone(), &runner);
        assert!(agent.uninstall(false, false).is_err());
        let dry = agent.uninstall(true, true).unwrap();
        assert!(dry.dry_run);
        assert!(paths.plist_path().exists());
        let mutation = agent.uninstall(true, false).unwrap();
        assert!(mutation.changed);
        assert!(!paths.plist_path().exists());
        assert!(runner.calls().iter().any(|call| call[0] == "bootout"));
    }

    #[test]
    fn uninstall_refuses_to_remove_plist_when_bootout_fails() {
        let root = TempDir::new().unwrap();
        let paths = paths(&root);
        std::fs::create_dir_all(paths.plist_path().parent().unwrap()).unwrap();
        std::fs::write(paths.plist_path(), b"plist").unwrap();
        let runner = FakeLaunchctl::not_loaded();
        *runner.bootout.lock().unwrap() = Some(LaunchctlOutput::failure(1, "permission denied"));
        let agent = DaemonAgent::new(paths.clone(), &runner);
        assert!(agent.uninstall(true, false).is_err());
        assert!(paths.plist_path().exists());
    }

    #[test]
    fn status_does_not_spawn_or_mutate() {
        let root = TempDir::new().unwrap();
        let paths = paths(&root);
        let runner = FakeLaunchctl::not_loaded();
        let agent = DaemonAgent::new(paths.clone(), &runner);
        for _ in 0..3 {
            let _ = agent.status();
        }
        assert!(!paths.plist_path().exists());
        let calls = runner.calls();
        assert!(
            calls
                .iter()
                .all(|call| call[0] == "print" || call[0] == "print-disabled")
        );
    }
    #[test]
    fn install_removes_a_written_plist_when_bootstrap_fails() {
        let root = TempDir::new().unwrap();
        let paths = paths(&root);
        let runner = FakeLaunchctl::not_loaded();
        *runner.bootstrap.lock().unwrap() = Some(LaunchctlOutput::failure(
            1,
            "Bootstrap failed: 5: Input/output error",
        ));
        let agent = DaemonAgent::new(paths.clone(), &runner);
        let error = agent.install(true, false, true).unwrap_err().to_string();
        assert!(error.contains("bootstrap failed"), "{error}");
        assert!(!paths.plist_path().exists());
        assert!(runner.calls().iter().all(|call| call[0] != "sudo"));
    }

    #[test]
    fn uninstall_missing_service_still_removes_the_plist() {
        let root = TempDir::new().unwrap();
        let paths = paths(&root);
        std::fs::create_dir_all(paths.plist_path().parent().unwrap()).unwrap();
        std::fs::write(paths.plist_path(), b"plist").unwrap();
        let runner = FakeLaunchctl::not_loaded();
        *runner.bootout.lock().unwrap() = Some(LaunchctlOutput::failure(
            113,
            "Could not find service \"com.verlinpark.daemon\"",
        ));
        let agent = DaemonAgent::new(paths.clone(), &runner);
        let mutation = agent.uninstall(true, false).unwrap();
        assert!(mutation.changed);
        assert!(!paths.plist_path().exists());
    }

    #[test]
    fn unexpected_launchctl_failure_is_unknown_when_plist_exists() {
        let root = TempDir::new().unwrap();
        let paths = paths(&root);
        std::fs::create_dir_all(paths.plist_path().parent().unwrap()).unwrap();
        std::fs::write(paths.plist_path(), b"plist").unwrap();
        let runner = FakeLaunchctl {
            print: Mutex::new(Some(LaunchctlOutput::failure(1, "permission denied"))),
            disabled: Mutex::new(Some(LaunchctlOutput::success("disabled services = {}"))),
            ..FakeLaunchctl::default()
        };
        let agent = DaemonAgent::new(paths, &runner);
        let status = agent.status();
        assert_eq!(status.state, "unknown");
        assert!(!status.loaded && !status.running);
        assert!(status.launchctl_available);
        assert!(status.detail.contains("permission denied"));
    }

    #[test]
    fn binary_resolution_rejects_relative_and_non_executable_paths() {
        let root = TempDir::new().unwrap();
        assert!(absolute_existing_binary(PathBuf::from("verlinpadd")).is_err());
        let file = root.path().join("verlinpadd");
        std::fs::write(&file, b"not executable").unwrap();
        assert!(absolute_existing_binary(file).is_err());
        let executable = root.path().join("verlinpadd-ok");
        std::fs::write(&executable, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(absolute_existing_binary(executable).is_ok());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn generated_plist_passes_plutil_lint() {
        let root = TempDir::new().unwrap();
        let paths = paths(&root);
        let runner = FakeLaunchctl::not_loaded();
        let agent = DaemonAgent::new(paths, &runner);
        let plist = root.path().join("generated.plist");
        std::fs::write(&plist, agent.plist_contents()).unwrap();
        let status = std::process::Command::new("/usr/bin/plutil")
            .args(["-lint", plist.to_str().unwrap()])
            .status()
            .unwrap();
        assert!(status.success());
    }
    #[test]
    fn parse_helpers_handle_not_running_disabled_layouts_and_absent_discovery() {
        let (pid, running) =
            parse_print("gui/501/com.verlinpark.daemon = {\n\tstate = not running\n\tpid = 0\n}");
        assert!(!running);
        assert!(pid.is_none());
        let (pid, running) =
            parse_print("gui/501/com.verlinpark.daemon = {\n\tstate = spawn scheduled\n}");
        assert!(!running);
        assert!(pid.is_none());
        let disabled = LaunchctlOutput::success(
            "disabled services = {\n\t\"com.verlinpark.daemon\" => disabled\n}",
        );
        assert!(parse_disabled(&disabled));
        let enabled = LaunchctlOutput::success(
            "disabled services = {\n\t\"com.verlinpark.daemon\" => enabled\n}",
        );
        assert!(!parse_disabled(&enabled));
        assert!(!parse_disabled(&LaunchctlOutput::failure(1, "disabled")));
        let root = TempDir::new().unwrap();
        let paths = paths(&root);
        assert!(read_live_discovery(&paths.discovery_path()).is_none());
        let discovery = Discovery {
            pid: 999_999,
            port: 1,
            token: "x".to_string(),
            protocol: 1,
            version: "x".to_string(),
            started_at: "2026-11-12T00:00:00Z".to_string(),
        };
        std::fs::create_dir_all(paths.data_dir.join("run")).unwrap();
        std::fs::write(
            paths.discovery_path(),
            serde_json::to_vec(&discovery).unwrap(),
        )
        .unwrap();
        assert!(read_live_discovery(&paths.discovery_path()).is_none());
    }

    #[test]
    fn dry_run_install_does_not_create_logs_or_launch_agents_directories() {
        let root = TempDir::new().unwrap();
        let paths = paths(&root);
        let runner = FakeLaunchctl::not_loaded();
        let agent = DaemonAgent::new(paths.clone(), &runner);
        let mutation = agent.install(true, true, false).unwrap();
        assert!(mutation.dry_run);
        assert!(!paths.plist_path().exists());
        assert!(!paths.log_dir().exists());
        assert!(!paths.home.join("Library").exists());
        assert!(runner.calls().iter().all(|call| call[0] != "bootstrap"));
    }

    #[test]
    fn plist_pins_user_paths_and_safe_launchd_keys() {
        let root = TempDir::new().unwrap();
        let paths = paths(&root);
        let runner = FakeLaunchctl::not_loaded();
        let plist = DaemonAgent::new(paths.clone(), &runner).plist_contents();
        assert!(plist.contains("<key>Umask</key>\n  <integer>63</integer>"));
        assert!(plist.contains("<key>ThrottleInterval</key>\n  <integer>10</integer>"));
        assert!(plist.contains("daemon-agent.err.log"));
        assert!(!plist.contains("UserName"));
        assert!(!plist.contains("GroupName"));
        assert!(!plist.contains("sudo"));
    }
    #[test]
    fn installed_but_not_loaded_reports_installed_without_running() {
        let root = TempDir::new().unwrap();
        let paths = paths(&root);
        std::fs::create_dir_all(paths.plist_path().parent().unwrap()).unwrap();
        std::fs::write(paths.plist_path(), b"plist").unwrap();
        let runner = FakeLaunchctl::not_loaded();
        let status = DaemonAgent::new(paths, &runner).status();
        assert_eq!(status.state, "installed");
        assert!(status.installed);
        assert!(!status.loaded && !status.running && !status.supervised);
        assert!(status.pid.is_none());
    }

    #[test]
    fn mutation_serializes_without_plist_body_or_secret() {
        let root = TempDir::new().unwrap();
        let paths = paths(&root);
        let runner = FakeLaunchctl::not_loaded();
        let mutation = DaemonAgent::new(paths, &runner)
            .install(true, true, false)
            .unwrap();
        let value = serde_json::to_value(&mutation).unwrap();
        assert_eq!(value["action"], serde_json::json!("install"));
        assert_eq!(value["dry_run"], serde_json::json!(true));
        assert!(value["plist"].is_string());
        assert!(!value.to_string().contains("sudo"));
    }
}

#[cfg(test)]
mod i5_lifecycle_tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    struct StaticRunner {
        print: Mutex<LaunchctlOutput>,
        disabled: Mutex<LaunchctlOutput>,
    }

    impl LaunchctlRunner for StaticRunner {
        fn run(&self, args: &[String]) -> LaunchctlOutput {
            match args.first().map(String::as_str) {
                Some("print") => self.print.lock().unwrap().clone(),
                Some("print-disabled") => self.disabled.lock().unwrap().clone(),
                _ => LaunchctlOutput::success(""),
            }
        }
    }

    #[test]
    fn disabled_state_wins_over_a_present_plist() {
        let root = TempDir::new().unwrap();
        let plist = root
            .path()
            .join("Library")
            .join("LaunchAgents")
            .join(DAEMON_AGENT_PLIST);
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        std::fs::write(&plist, b"plist").unwrap();
        let runner = StaticRunner {
            print: Mutex::new(LaunchctlOutput::failure(
                113,
                "Could not find service \"com.verlinpark.daemon\"",
            )),
            disabled: Mutex::new(LaunchctlOutput::success(
                "disabled services = {\n\t\"com.verlinpark.daemon\" => disabled\n}",
            )),
        };
        let paths = DaemonAgentPaths {
            home: root.path().to_path_buf(),
            data_dir: root.path().join("data"),
            binary: root.path().join("verlinpadd"),
            uid: 501,
        };
        let status = DaemonAgent::new(paths, &runner).status();
        assert!(status.installed);
        assert!(status.disabled);
        assert_eq!(status.state, "disabled");
    }
}
