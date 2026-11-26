//! Explicit user LaunchAgent lifecycle for the four Custos ring roles.
//!
//! Custos never installs a launchd job automatically and never asks for root.
//! An operator command writes one user-level plist under
//! `~/Library/LaunchAgents` and loads it with
//! `launchctl bootstrap gui/<uid> <plist>`. Every launchctl call goes through
//! [`LaunchctlRunner`], so the lifecycle is testable without touching the real
//! `LaunchAgents` directory, and the only production runner passes argv
//! directly to `/bin/launchctl` (no sudo and no shell).
//!
//! This module is lifecycle only. It does not grant authority to a Custos
//! role, does not read provider credentials, and does not sandbox `custosd`.
//! Status parsing is intentionally conservative: file existence is not proof
//! that launchd loaded the job, and unrecognized launchctl output is reported
//! as [`RoleAgentState::Unknown`] rather than guessed.

use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::protocol::CustosRole;

pub const CORE_LABEL: &str = "com.verlintas.custos-core";
pub const WARDEN_LABEL: &str = "com.verlintas.custos-warden";
pub const ANCHOR_LABEL: &str = "com.verlintas.custos-anchor";
pub const SENTINEL_LABEL: &str = "com.verlintas.custos-sentinel";
pub const ALL_LABELS: [&str; 4] = [CORE_LABEL, WARDEN_LABEL, ANCHOR_LABEL, SENTINEL_LABEL];
pub const ALL_ROLES: [crate::protocol::CustosRole; 4] = [
    crate::protocol::CustosRole::Core,
    crate::protocol::CustosRole::Warden,
    crate::protocol::CustosRole::Anchor,
    crate::protocol::CustosRole::Sentinel,
];

const MAX_DETAIL_CHARS: usize = 600;

/// The stable launchd label for one Custos role.
pub fn label(role: CustosRole) -> &'static str {
    match role {
        CustosRole::Core => CORE_LABEL,
        CustosRole::Warden => WARDEN_LABEL,
        CustosRole::Anchor => ANCHOR_LABEL,
        CustosRole::Sentinel => SENTINEL_LABEL,
    }
}

/// The plist filename for one role. It deliberately matches the label.
pub fn plist_file_name(role: CustosRole) -> String {
    format!("{}.plist", label(role))
}

/// Filesystem locations for one user-level installation.
///
/// `agents_dir` is always `<home>/Library/LaunchAgents` and `log_dir` is
/// always `<data_dir>/custos/logs`. They are stored explicitly so tests and
/// callers can inspect exactly what will be written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchdPaths {
    pub home: PathBuf,
    pub data_dir: PathBuf,
    pub binary: PathBuf,
    pub agents_dir: PathBuf,
    pub log_dir: PathBuf,
}

impl LaunchdPaths {
    pub fn new(
        home: impl AsRef<Path>,
        data_dir: impl AsRef<Path>,
        binary: impl AsRef<Path>,
    ) -> Self {
        let home = home.as_ref().to_path_buf();
        let data_dir = data_dir.as_ref().to_path_buf();
        let binary = binary.as_ref().to_path_buf();
        let agents_dir = home.join("Library").join("LaunchAgents");
        let log_dir = data_dir.join("custos").join("logs");
        Self {
            home,
            data_dir,
            binary,
            agents_dir,
            log_dir,
        }
    }

    pub fn plist_path(&self, role: CustosRole) -> PathBuf {
        self.agents_dir.join(plist_file_name(role))
    }

    pub fn stdout_log(&self, role: CustosRole) -> PathBuf {
        self.log_dir.join(format!("{}.out.log", role.as_str()))
    }

    pub fn stderr_log(&self, role: CustosRole) -> PathBuf {
        self.log_dir.join(format!("{}.err.log", role.as_str()))
    }
}

/// Injectable launchctl surface. The real implementation shells out to the
/// fixed `/bin/launchctl` binary; tests use an in-memory fake.
pub trait LaunchctlRunner {
    /// Run one launchctl argv. Stdout is returned on success. A non-zero exit
    /// or an unavailable binary is an error whose text is safe to inspect for
    /// the small set of launchd not-found/disabled messages.
    fn run(&self, args: &[&str]) -> Result<String>;
}

/// The only production runner. It never uses sudo and never invokes a shell:
/// arguments are passed directly to `/bin/launchctl`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemLaunchctl;

impl LaunchctlRunner for SystemLaunchctl {
    fn run(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("/bin/launchctl")
            .args(args)
            .output()
            .map_err(|error| anyhow!("launchctl unavailable: {error}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if output.status.success() {
            return Ok(stdout);
        }
        let code = output.status.code().unwrap_or(-1);
        let mut detail = String::new();
        if !stdout.trim().is_empty() {
            detail.push_str(stdout.trim());
        }
        if !stderr.trim().is_empty() {
            if !detail.is_empty() {
                detail.push(' ');
            }
            detail.push_str(stderr.trim());
        }
        bail!(
            "launchctl {} failed ({}): {}",
            args.join(" "),
            code,
            bounded(&detail)
        )
    }
}

/// The current user's numeric uid, obtained only through the fixed
/// `/usr/bin/id -u` command. There is no shell and no `libc` dependency.
pub fn uid() -> Result<u32> {
    let output = Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .context("running /usr/bin/id -u")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("/usr/bin/id -u failed: {}", bounded(stderr.trim()));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let text = stdout.trim();
    text.parse::<u32>()
        .with_context(|| format!("parsing uid from {text:?}"))
}

/// One observable launchd state. `Installed` means the plist exists but
/// launchd does not report a loaded job; `Loaded` means loaded but not
/// currently running.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleAgentState {
    NotInstalled,
    Installed,
    Loaded,
    Running,
    Disabled,
    Unknown,
}

impl RoleAgentState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotInstalled => "not_installed",
            Self::Installed => "installed",
            Self::Loaded => "loaded",
            Self::Running => "running",
            Self::Disabled => "disabled",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for RoleAgentState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Read-only status for one role's LaunchAgent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleAgentStatus {
    pub role: String,
    pub label: String,
    pub plist_path: String,
    pub installed: bool,
    pub loaded: bool,
    pub running: bool,
    pub disabled: bool,
    pub pid: Option<u32>,
    pub state: RoleAgentState,
    pub launchctl_available: bool,
    pub detail: String,
}

/// Result of an explicit install.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallOutcome {
    pub role: String,
    pub label: String,
    pub plist_path: String,
    pub changed: bool,
    pub bootstrapped: bool,
    pub kickstarted: bool,
    pub detail: String,
    pub status: RoleAgentStatus,
}

/// Deterministic plist XML for one role.
///
/// There is no timestamp, hostname, environment value, or secret in the file.
/// Paths must be absolute because launchd cannot resolve a relative
/// `ProgramArguments` or log path.
pub fn plist_contents(role: CustosRole, paths: &LaunchdPaths) -> Result<String> {
    require_absolute(&paths.home, "home directory")?;
    require_absolute(&paths.binary, "custosd binary")?;
    require_absolute(&paths.data_dir, "data directory")?;
    require_absolute(&paths.agents_dir, "LaunchAgents directory")?;
    require_absolute(&paths.log_dir, "log directory")?;

    let mut plist = String::with_capacity(1024);
    plist.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    plist.push_str("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n");
    plist.push_str("<plist version=\"1.0\">\n<dict>\n");

    plist.push_str("  <key>Label</key>\n");
    let _ = writeln!(plist, "  <string>{}</string>", xml_escape(label(role)));

    plist.push_str("  <key>ProgramArguments</key>\n  <array>\n");
    let arguments = [
        paths.binary.to_string_lossy().into_owned(),
        "--data-dir".to_string(),
        paths.data_dir.to_string_lossy().into_owned(),
        "--role".to_string(),
        role.as_str().to_string(),
    ];
    for argument in &arguments {
        let _ = writeln!(
            plist,
            "    <string>{}</string>",
            xml_escape(argument.as_str())
        );
    }
    plist.push_str("  </array>\n");

    plist.push_str("  <key>RunAtLoad</key>\n  <true/>\n");
    plist.push_str("  <key>KeepAlive</key>\n  <true/>\n");
    plist.push_str("  <key>ThrottleInterval</key>\n  <integer>5</integer>\n");
    plist.push_str("  <key>LimitLoadToSessionType</key>\n  <string>Aqua</string>\n");
    plist.push_str("  <key>Umask</key>\n  <integer>63</integer>\n");

    plist.push_str("  <key>StandardOutPath</key>\n");
    let _ = writeln!(
        plist,
        "  <string>{}</string>",
        xml_escape(paths.stdout_log(role).to_string_lossy().as_ref())
    );
    plist.push_str("  <key>StandardErrorPath</key>\n");
    let _ = writeln!(
        plist,
        "  <string>{}</string>",
        xml_escape(paths.stderr_log(role).to_string_lossy().as_ref())
    );

    plist.push_str("</dict>\n</plist>\n");
    Ok(plist)
}

/// Explicitly install and bootstrap one role's LaunchAgent.
///
/// `confirmed` must be true. The plist is written atomically with mode 0644
/// before `launchctl bootstrap gui/<uid> <plist>`. A best-effort
/// `kickstart -k` follows. If bootstrap fails, the newly written plist is
/// removed and the error is returned.
pub fn install(
    role: CustosRole,
    paths: &LaunchdPaths,
    runner: &dyn LaunchctlRunner,
    confirmed: bool,
) -> Result<InstallOutcome> {
    if !confirmed {
        bail!("Custos LaunchAgent install requires explicit confirmation");
    }
    let plist = plist_contents(role, paths)?;
    let uid = uid()?;
    let target = service_target(uid, role);
    let domain = gui_domain(uid);

    let current = status_with_uid(role, paths, runner, uid)?;
    if current.loaded {
        bail!(
            "{} is already loaded; run uninstall before reinstalling",
            label(role)
        );
    }
    if current.disabled {
        bail!(
            "{} is disabled in launchd; enable it before installing",
            label(role)
        );
    }

    let plist_path = paths.plist_path(role);
    fs::create_dir_all(&paths.log_dir)
        .with_context(|| format!("creating {}", paths.log_dir.display()))?;
    set_private_dir(&paths.log_dir)?;
    write_plist_atomic(&plist_path, &plist)?;

    let plist_arg = plist_path.to_string_lossy().into_owned();
    let bootstrap = runner.run(&["bootstrap", domain.as_str(), plist_arg.as_str()]);
    if let Err(error) = bootstrap {
        let _ = fs::remove_file(&plist_path);
        return Err(error).with_context(|| {
            format!(
                "launchctl bootstrap failed for {}; the written plist was removed",
                label(role)
            )
        });
    }

    // Bootstrap already starts a RunAtLoad job. Kickstart makes an explicit
    // reinstall deterministic without requiring a separate manual load, but a
    // kickstart failure is not fatal because bootstrap succeeded.
    let kickstart = runner.run(&["kickstart", "-k", target.as_str()]);
    let kickstarted = kickstart.is_ok();

    let status = status_with_uid(role, paths, runner, uid)?;
    let detail = match &kickstart {
        Ok(_) => format!("installed and started {}", label(role)),
        Err(error) => format!(
            "installed {}; kickstart did not confirm a start: {}",
            label(role),
            bounded(&error_chain(error))
        ),
    };

    Ok(InstallOutcome {
        role: role.as_str().to_string(),
        label: label(role).to_string(),
        plist_path: plist_path.to_string_lossy().to_string(),
        changed: true,
        bootstrapped: true,
        kickstarted,
        detail,
        status,
    })
}

/// Remove one role's LaunchAgent.
///
/// The job is booted out before the plist is removed. A "not found"/"not
/// loaded" bootout is tolerated and still removes a leftover plist; any other
/// bootout failure aborts so a still-loaded job is not orphaned.
pub fn uninstall(
    role: CustosRole,
    paths: &LaunchdPaths,
    runner: &dyn LaunchctlRunner,
    confirmed: bool,
) -> Result<()> {
    if !confirmed {
        bail!("Custos LaunchAgent uninstall requires explicit confirmation");
    }
    let uid = uid()?;
    let target = service_target(uid, role);
    let plist_path = paths.plist_path(role);

    if let Err(error) = runner.run(&["bootout", target.as_str()]) {
        let text = error_chain(&error).to_ascii_lowercase();
        if !is_not_loaded(&text) {
            return Err(error).with_context(|| {
                format!(
                    "launchctl bootout failed for {}; the plist was left in place",
                    label(role)
                )
            });
        }
    }

    match fs::remove_file(&plist_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("removing {}", plist_path.display())),
    }
}

/// Read-only status for one role. This never writes a plist and never starts a
/// job.
pub fn status(
    role: CustosRole,
    paths: &LaunchdPaths,
    runner: &dyn LaunchctlRunner,
) -> Result<RoleAgentStatus> {
    let uid = uid()?;
    status_with_uid(role, paths, runner, uid)
}

fn status_with_uid(
    role: CustosRole,
    paths: &LaunchdPaths,
    runner: &dyn LaunchctlRunner,
    uid: u32,
) -> Result<RoleAgentStatus> {
    let plist_path = paths.plist_path(role);
    let installed = plist_path.is_file();
    let target = service_target(uid, role);
    let domain = gui_domain(uid);

    let print = runner.run(&["print", target.as_str()]);
    let disabled_query = runner.run(&["print-disabled", domain.as_str()]);

    let mut loaded = false;
    let mut running = false;
    let mut pid = None;
    let mut disabled = false;
    let mut launchctl_available;
    let mut unexpected = false;
    let mut detail = String::new();

    match &print {
        Ok(stdout) => {
            launchctl_available = true;
            loaded = true;
            let (parsed_pid, parsed_running) = parse_print(stdout);
            pid = parsed_pid;
            running = parsed_running;
        }
        Err(error) => {
            let text = error_chain(error);
            let lowered = text.to_ascii_lowercase();
            if lowered.contains("launchctl unavailable") {
                launchctl_available = false;
                unexpected = installed;
                detail = bounded(&text);
            } else {
                launchctl_available = true;
                if lowered.contains("disabled") {
                    disabled = true;
                } else if is_not_found(&lowered) {
                    loaded = false;
                } else {
                    unexpected = installed;
                    detail = bounded(&text);
                }
            }
        }
    }

    match &disabled_query {
        Ok(stdout) => {
            launchctl_available = true;
            if parse_disabled(stdout, label(role)) {
                disabled = true;
            }
        }
        Err(error) => {
            let text = error_chain(error).to_ascii_lowercase();
            if text.contains("launchctl unavailable") {
                if print.is_err() {
                    launchctl_available = false;
                }
            } else if text.contains(&label(role).to_ascii_lowercase()) && text.contains("disabled")
            {
                disabled = true;
            }
        }
    }

    let state = if disabled {
        RoleAgentState::Disabled
    } else if loaded && running {
        RoleAgentState::Running
    } else if loaded {
        RoleAgentState::Loaded
    } else if unexpected && installed {
        RoleAgentState::Unknown
    } else if installed {
        RoleAgentState::Installed
    } else {
        RoleAgentState::NotInstalled
    };

    if detail.is_empty() {
        detail = match state {
            RoleAgentState::Running => match pid {
                Some(pid) => format!("launchd reports pid {pid}"),
                None => "launchd reports the job running".to_string(),
            },
            RoleAgentState::Loaded => "launchd loaded the job but it is not running".to_string(),
            RoleAgentState::Disabled => "the job is disabled in launchd".to_string(),
            RoleAgentState::Unknown => "launchctl returned an unrecognized result".to_string(),
            RoleAgentState::Installed => {
                "the plist is installed but launchd does not report the job loaded".to_string()
            }
            RoleAgentState::NotInstalled => {
                "no plist and no loaded launchd job were found".to_string()
            }
        };
    }

    Ok(RoleAgentStatus {
        role: role.as_str().to_string(),
        label: label(role).to_string(),
        plist_path: plist_path.to_string_lossy().to_string(),
        installed,
        loaded,
        running,
        disabled,
        pid,
        state,
        launchctl_available,
        detail,
    })
}

fn service_target(uid: u32, role: CustosRole) -> String {
    format!("gui/{uid}/{}", label(role))
}

fn gui_domain(uid: u32) -> String {
    format!("gui/{uid}")
}

fn write_plist_atomic(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("custos.plist");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temp = parent.join(format!(".{file_name}.tmp-{}-{nonce}", std::process::id()));

    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .with_context(|| format!("creating {}", temp.display()))?;
        file.write_all(contents.as_bytes())
            .with_context(|| format!("writing {}", temp.display()))?;
        file.sync_all()
            .with_context(|| format!("syncing {}", temp.display()))?;
        set_plist_mode(&temp)?;
        fs::rename(&temp, path).with_context(|| format!("installing {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn require_absolute(path: &Path, what: &str) -> Result<()> {
    if !path.is_absolute() {
        bail!("{what} must be absolute: {}", path.display());
    }
    Ok(())
}

fn set_private_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("setting permissions on {}", path.display()))?;
    }
    let _ = path;
    Ok(())
}

fn set_plist_mode(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o644))
            .with_context(|| format!("setting permissions on {}", path.display()))?;
    }
    let _ = path;
    Ok(())
}

fn parse_print(stdout: &str) -> (Option<u32>, bool) {
    let mut pid = None;
    let mut running = false;
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("pid = ") {
            pid = value.trim().parse::<u32>().ok().filter(|pid| *pid > 0);
        }
        if let Some(value) = line.strip_prefix("state = ") {
            running = value.trim() == "running";
        }
    }
    if pid.is_some() && !running {
        // A printed pid is authoritative even if the state wording changed.
        running = true;
    }
    (pid, running)
}

fn parse_disabled(stdout: &str, target_label: &str) -> bool {
    let target = target_label.to_ascii_lowercase();
    stdout.lines().any(|line| {
        let line = line.trim().to_ascii_lowercase();
        line.contains(&target) && (line.contains("disabled") || line.ends_with("true"))
    })
}

fn is_not_found(text: &str) -> bool {
    [
        "could not find service",
        "service not found",
        "no such process",
        "not loaded",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn is_not_loaded(text: &str) -> bool {
    is_not_found(text) || text.contains("service is disabled")
}

fn error_chain(error: &anyhow::Error) -> String {
    format!("{error:#}")
}

fn bounded(value: &str) -> String {
    value.chars().take(MAX_DETAIL_CHARS).collect()
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
    use std::fs;
    use std::sync::Mutex;
    use tempfile::TempDir;

    type FakeResult = std::result::Result<String, String>;
    const NOT_FOUND: &str = "Could not find service in domain for user gui: 501";

    struct FakeLaunchctl {
        print: Mutex<FakeResult>,
        disabled: Mutex<FakeResult>,
        bootstrap: Mutex<FakeResult>,
        kickstart: Mutex<FakeResult>,
        bootout: Mutex<FakeResult>,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl Default for FakeLaunchctl {
        fn default() -> Self {
            Self {
                print: Mutex::new(Err(NOT_FOUND.to_string())),
                disabled: Mutex::new(Ok(String::new())),
                bootstrap: Mutex::new(Ok(String::new())),
                kickstart: Mutex::new(Ok(String::new())),
                bootout: Mutex::new(Ok(String::new())),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl FakeLaunchctl {
        fn running(pid: u32) -> Self {
            let fake = Self::default();
            *fake.print.lock().unwrap() = Ok(format!(
                "gui/501/{CORE_LABEL} = {{\n\tstate = running\n\tpid = {pid}\n}}"
            ));
            fake
        }

        fn loaded_not_running() -> Self {
            let fake = Self::default();
            *fake.print.lock().unwrap() = Ok(format!(
                "gui/501/{CORE_LABEL} = {{\n\tstate = not running\n}}"
            ));
            fake
        }

        fn disabled() -> Self {
            let fake = Self::default();
            *fake.disabled.lock().unwrap() = Ok(format!(
                "disabled services = {{\n\t\"{CORE_LABEL}\" => disabled\n}}"
            ));
            fake
        }

        fn unavailable() -> Self {
            let fake = Self::default();
            *fake.print.lock().unwrap() = Err("launchctl unavailable: no launchd".to_string());
            *fake.disabled.lock().unwrap() = Err("launchctl unavailable: no launchd".to_string());
            fake
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl LaunchctlRunner for FakeLaunchctl {
        fn run(&self, args: &[&str]) -> Result<String> {
            self.calls
                .lock()
                .unwrap()
                .push(args.iter().map(|arg| (*arg).to_string()).collect());
            let outcome = match args.first().copied() {
                Some("print") => self.print.lock().unwrap().clone(),
                Some("print-disabled") => self.disabled.lock().unwrap().clone(),
                Some("bootstrap") => self.bootstrap.lock().unwrap().clone(),
                Some("kickstart") => self.kickstart.lock().unwrap().clone(),
                Some("bootout") => self.bootout.lock().unwrap().clone(),
                _ => Err("unexpected launchctl command".to_string()),
            };
            outcome.map_err(anyhow::Error::msg)
        }
    }

    fn fixture() -> (TempDir, LaunchdPaths) {
        let root = TempDir::new().unwrap();
        let home = root.path().join("home");
        let data = root.path().join("data");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&data).unwrap();
        let binary = root.path().join("custosd");
        fs::write(&binary, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        }
        (root, LaunchdPaths::new(&home, &data, &binary))
    }

    fn write_installed_plist(paths: &LaunchdPaths, role: CustosRole) {
        fs::create_dir_all(&paths.agents_dir).unwrap();
        fs::write(paths.plist_path(role), b"plist").unwrap();
    }

    #[test]
    fn labels_are_stable_and_distinct() {
        assert_eq!(label(CustosRole::Core), CORE_LABEL);
        assert_eq!(label(CustosRole::Warden), WARDEN_LABEL);
        assert_eq!(label(CustosRole::Anchor), ANCHOR_LABEL);
        assert_eq!(label(CustosRole::Sentinel), SENTINEL_LABEL);
        assert_eq!(
            plist_file_name(CustosRole::Core),
            "com.verlintas.custos-core.plist"
        );
        let mut labels = ALL_LABELS.to_vec();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), 4);
    }

    #[test]
    fn plist_is_deterministic_for_all_four_roles() {
        let (_root, paths) = fixture();
        for role in [
            CustosRole::Core,
            CustosRole::Warden,
            CustosRole::Anchor,
            CustosRole::Sentinel,
        ] {
            let first = plist_contents(role, &paths).unwrap();
            let second = plist_contents(role, &paths).unwrap();
            assert_eq!(first, second);
            assert!(first.contains(&format!("<string>{}</string>", label(role))));
            assert!(first.contains("<key>ProgramArguments</key>\n  <array>\n"));
            assert!(first.contains("--data-dir"));
            assert!(first.contains("--role"));
            assert!(first.contains(&format!("<string>{}</string>", role.as_str())));
            assert!(first.contains("<key>RunAtLoad</key>\n  <true/>"));
            assert!(first.contains("<key>KeepAlive</key>\n  <true/>"));
            assert!(first.contains("<key>ThrottleInterval</key>\n  <integer>5</integer>"));
            assert!(first.contains("<key>LimitLoadToSessionType</key>\n  <string>Aqua</string>"));
            assert!(first.contains("<key>Umask</key>\n  <integer>63</integer>"));
            assert!(first.contains(&format!("custos/logs/{}.out.log", role.as_str())));
            assert!(first.contains(&format!("custos/logs/{}.err.log", role.as_str())));
            assert!(!first.contains("sudo"));
            assert!(!first.contains("UserName"));
            assert!(!first.contains("GroupName"));
        }
    }

    #[test]
    fn plist_escapes_xml_and_rejects_relative_paths() {
        let root = TempDir::new().unwrap();
        let home = root.path().join("home");
        let data = root.path().join("data & <custos>");
        let binary = root.path().join("custosd");
        let paths = LaunchdPaths::new(&home, &data, &binary);
        let plist = plist_contents(CustosRole::Core, &paths).unwrap();
        assert!(plist.contains("data &amp; &lt;custos&gt;"));
        assert!(!plist.contains("data & <custos>"));

        let relative = LaunchdPaths::new(".", "data", "custosd");
        let error = plist_contents(CustosRole::Core, &relative)
            .unwrap_err()
            .to_string();
        assert!(error.contains("must be absolute"), "{error}");
    }

    #[test]
    fn uid_uses_the_fixed_id_command() {
        uid().unwrap();
    }

    #[test]
    fn install_requires_confirmation() {
        let (_root, paths) = fixture();
        let runner = FakeLaunchctl::default();
        let error = install(CustosRole::Core, &paths, &runner, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("confirmation"), "{error}");
        assert!(!paths.plist_path(CustosRole::Core).exists());
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn install_writes_0644_plist_and_bootstraps() {
        let (_root, paths) = fixture();
        let runner = FakeLaunchctl::default();
        *runner.bootstrap.lock().unwrap() = Ok(String::new());
        *runner.kickstart.lock().unwrap() = Ok(String::new());

        let outcome = install(CustosRole::Core, &paths, &runner, true).unwrap();
        assert!(outcome.changed);
        assert!(outcome.bootstrapped);
        assert!(outcome.kickstarted);
        assert_eq!(outcome.role, "core");
        assert_eq!(outcome.label, CORE_LABEL);
        assert_eq!(outcome.status.state, RoleAgentState::Installed);
        assert!(paths.plist_path(CustosRole::Core).is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(paths.plist_path(CustosRole::Core))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o644);
        }

        let uid = uid().unwrap();
        let calls = runner.calls();
        assert!(calls.iter().any(|call| {
            call.first().map(String::as_str) == Some("bootstrap")
                && call.get(1) == Some(&format!("gui/{uid}"))
                && call.get(2)
                    == Some(
                        &paths
                            .plist_path(CustosRole::Core)
                            .to_string_lossy()
                            .into_owned(),
                    )
        }));
        assert!(calls.iter().any(|call| {
            call.first().map(String::as_str) == Some("kickstart")
                && call.get(1).map(String::as_str) == Some("-k")
                && call.get(2) == Some(&format!("gui/{uid}/{CORE_LABEL}"))
        }));
        assert!(calls.iter().all(|call| !matches!(
            call.first().map(String::as_str),
            Some("sudo" | "sh" | "bash" | "zsh")
        )));
    }

    #[test]
    fn install_refuses_an_already_loaded_job() {
        let (_root, paths) = fixture();
        let runner = FakeLaunchctl::running(4242);
        let error = install(CustosRole::Core, &paths, &runner, true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("already loaded"), "{error}");
        assert!(!paths.plist_path(CustosRole::Core).exists());
        assert!(
            runner
                .calls()
                .iter()
                .all(|call| call.first().map(String::as_str) != Some("bootstrap"))
        );
    }

    #[test]
    fn install_removes_the_written_plist_when_bootstrap_fails() {
        let (_root, paths) = fixture();
        let runner = FakeLaunchctl::default();
        *runner.bootstrap.lock().unwrap() =
            Err("Bootstrap failed: 5: Input/output error".to_string());
        let error = install(CustosRole::Core, &paths, &runner, true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("bootstrap failed"), "{error}");
        assert!(!paths.plist_path(CustosRole::Core).exists());
        assert!(
            runner
                .calls()
                .iter()
                .all(|call| call.first().map(String::as_str) != Some("kickstart"))
        );
    }

    #[test]
    fn uninstall_requires_confirmation_and_boots_out() {
        let (_root, paths) = fixture();
        write_installed_plist(&paths, CustosRole::Core);
        let runner = FakeLaunchctl::default();

        let error = uninstall(CustosRole::Core, &paths, &runner, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("confirmation"), "{error}");
        assert!(paths.plist_path(CustosRole::Core).exists());
        assert!(runner.calls().is_empty());

        uninstall(CustosRole::Core, &paths, &runner, true).unwrap();
        assert!(!paths.plist_path(CustosRole::Core).exists());
        let uid = uid().unwrap();
        assert!(runner.calls().iter().any(|call| {
            call.first().map(String::as_str) == Some("bootout")
                && call.get(1) == Some(&format!("gui/{uid}/{CORE_LABEL}"))
        }));
    }

    #[test]
    fn uninstall_tolerates_a_missing_service() {
        let (_root, paths) = fixture();
        write_installed_plist(&paths, CustosRole::Core);
        let runner = FakeLaunchctl::default();
        *runner.bootout.lock().unwrap() = Err(NOT_FOUND.to_string());
        uninstall(CustosRole::Core, &paths, &runner, true).unwrap();
        assert!(!paths.plist_path(CustosRole::Core).exists());
    }

    #[test]
    fn uninstall_keeps_the_plist_when_bootout_fails_unexpectedly() {
        let (_root, paths) = fixture();
        write_installed_plist(&paths, CustosRole::Core);
        let runner = FakeLaunchctl::default();
        *runner.bootout.lock().unwrap() = Err("Operation not permitted".to_string());
        let error = uninstall(CustosRole::Core, &paths, &runner, true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("bootout failed"), "{error}");
        assert!(paths.plist_path(CustosRole::Core).exists());
    }

    #[test]
    fn status_reports_a_running_job() {
        let (_root, paths) = fixture();
        write_installed_plist(&paths, CustosRole::Core);
        let runner = FakeLaunchctl::running(4242);
        let status = status(CustosRole::Core, &paths, &runner).unwrap();
        assert_eq!(status.state, RoleAgentState::Running);
        assert!(status.installed);
        assert!(status.loaded);
        assert!(status.running);
        assert!(!status.disabled);
        assert_eq!(status.pid, Some(4242));
        assert!(status.launchctl_available);
        assert_eq!(status.label, CORE_LABEL);
        assert_eq!(status.role, "core");
    }

    #[test]
    fn status_reports_loaded_without_running() {
        let (_root, paths) = fixture();
        write_installed_plist(&paths, CustosRole::Core);
        let runner = FakeLaunchctl::loaded_not_running();
        let status = status(CustosRole::Core, &paths, &runner).unwrap();
        assert_eq!(status.state, RoleAgentState::Loaded);
        assert!(status.loaded);
        assert!(!status.running);
        assert!(status.pid.is_none());
    }

    #[test]
    fn status_reports_disabled() {
        let (_root, paths) = fixture();
        write_installed_plist(&paths, CustosRole::Core);
        let runner = FakeLaunchctl::disabled();
        let status = status(CustosRole::Core, &paths, &runner).unwrap();
        assert_eq!(status.state, RoleAgentState::Disabled);
        assert!(status.disabled);
        assert!(!status.loaded);
    }

    #[test]
    fn status_distinguishes_installed_from_not_installed() {
        let (_root, paths) = fixture();
        let runner = FakeLaunchctl::default();
        let absent = status(CustosRole::Core, &paths, &runner).unwrap();
        assert_eq!(absent.state, RoleAgentState::NotInstalled);
        assert!(!absent.installed);
        assert!(!absent.loaded);
        assert!(absent.launchctl_available);

        write_installed_plist(&paths, CustosRole::Core);
        let installed = status(CustosRole::Core, &paths, &runner).unwrap();
        assert_eq!(installed.state, RoleAgentState::Installed);
        assert!(installed.installed);
        assert!(!installed.loaded);
    }

    #[test]
    fn status_reports_unknown_on_an_unexpected_launchctl_error() {
        let (_root, paths) = fixture();
        write_installed_plist(&paths, CustosRole::Core);
        let runner = FakeLaunchctl::default();
        *runner.print.lock().unwrap() = Err("permission denied".to_string());
        let status = status(CustosRole::Core, &paths, &runner).unwrap();
        assert_eq!(status.state, RoleAgentState::Unknown);
        assert!(status.installed);
        assert!(!status.loaded);
        assert!(status.launchctl_available);
        assert!(
            status.detail.contains("permission denied"),
            "{}",
            status.detail
        );
    }

    #[test]
    fn status_reports_unknown_when_launchctl_is_unavailable() {
        let (_root, paths) = fixture();
        write_installed_plist(&paths, CustosRole::Core);
        let status = status(CustosRole::Core, &paths, &FakeLaunchctl::unavailable()).unwrap();
        assert_eq!(status.state, RoleAgentState::Unknown);
        assert!(!status.launchctl_available);
        assert!(status.installed);
        assert!(!status.loaded);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn generated_plist_passes_plutil_lint() {
        let (root, paths) = fixture();
        let plist = plist_contents(CustosRole::Core, &paths).unwrap();
        let path = root.path().join("generated.plist");
        fs::write(&path, plist).unwrap();
        let status = std::process::Command::new("/usr/bin/plutil")
            .args(["-lint", path.to_str().unwrap()])
            .status()
            .unwrap();
        assert!(status.success());
    }
}
