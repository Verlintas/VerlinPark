//! Custos role-ring heartbeats.
//!
//! The first slice runs one observe-only core. The file format and quorum
//! helpers are already here so the later core/warden/anchor/sentinel ring can
//! use the same layout.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::protocol::{CustosRole, RingRoleView, RingView};

pub const RING_SCHEMA: &str = "verlinpark.custos_heartbeat/v1";
pub const FRESH_SECS: u64 = 10;
pub const STALE_SECS: u64 = 60;
pub const AUTHORITY_ROLES: [CustosRole; 3] =
    [CustosRole::Core, CustosRole::Warden, CustosRole::Anchor];
pub const ALL_ROLES: [CustosRole; 4] = [
    CustosRole::Core,
    CustosRole::Warden,
    CustosRole::Anchor,
    CustosRole::Sentinel,
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Heartbeat {
    pub schema: String,
    pub role: String,
    pub pid: u32,
    pub started_at: String,
    pub updated_at: String,
    pub seq: u64,
    pub build_hash: String,
    pub charter_hash: String,
    pub observe_only: bool,
}

impl Heartbeat {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        role: CustosRole,
        pid: u32,
        started_at: String,
        updated_at: String,
        seq: u64,
        build_hash: String,
        charter_hash: String,
        observe_only: bool,
    ) -> Self {
        Self {
            schema: RING_SCHEMA.to_string(),
            role: role.as_str().to_string(),
            pid,
            started_at,
            updated_at,
            seq,
            build_hash,
            charter_hash,
            observe_only,
        }
    }
}

pub fn ring_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("custos").join("ring")
}

pub fn heartbeat_path(data_dir: &Path, role: CustosRole) -> PathBuf {
    ring_dir(data_dir).join(format!("{}.json", role.as_str()))
}

pub fn write_heartbeat(data_dir: &Path, heartbeat: &Heartbeat) -> Result<()> {
    let dir = ring_dir(data_dir);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    set_dir_mode(&dir)?;
    let path = dir.join(format!("{}.json", heartbeat.role));
    let tmp = dir.join(format!(".{}.tmp", heartbeat.role));
    let bytes = serde_json::to_vec_pretty(heartbeat)?;
    {
        let mut file =
            fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        set_file_mode(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, &path).with_context(|| format!("renaming {}", path.display()))?;
    Ok(())
}

pub fn read_heartbeat(data_dir: &Path, role: CustosRole) -> Result<Option<(Heartbeat, u64)>> {
    let path = heartbeat_path(data_dir, role);
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let heartbeat: Heartbeat =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;
    let age = metadata
        .modified()
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .unwrap_or(Duration::ZERO)
        .as_secs();
    Ok(Some((heartbeat, age)))
}

pub fn snapshot(data_dir: &Path) -> RingView {
    let mut roles = Vec::new();
    let mut fresh = 0u32;
    let mut stale = 0u32;
    let mut missing = 0u32;
    let mut authority_fresh = 0u32;
    for role in ALL_ROLES {
        match read_heartbeat(data_dir, role) {
            Ok(Some((_, age))) if age <= FRESH_SECS => {
                fresh += 1;
                if AUTHORITY_ROLES.contains(&role) {
                    authority_fresh += 1;
                }
                roles.push(RingRoleView {
                    role: role.as_str().to_string(),
                    state: "fresh".to_string(),
                    age_secs: Some(age),
                });
            }
            Ok(Some((_, age))) if age <= STALE_SECS => {
                stale += 1;
                roles.push(RingRoleView {
                    role: role.as_str().to_string(),
                    state: "stale".to_string(),
                    age_secs: Some(age),
                });
            }
            Ok(Some((_, age))) => {
                stale += 1;
                roles.push(RingRoleView {
                    role: role.as_str().to_string(),
                    state: "stale".to_string(),
                    age_secs: Some(age),
                });
            }
            Ok(None) => {
                missing += 1;
                roles.push(RingRoleView {
                    role: role.as_str().to_string(),
                    state: "missing".to_string(),
                    age_secs: None,
                });
            }
            Err(_) => {
                missing += 1;
                roles.push(RingRoleView {
                    role: role.as_str().to_string(),
                    state: "invalid".to_string(),
                    age_secs: None,
                });
            }
        }
    }
    RingView {
        mode: if fresh == 1 && missing == 3 {
            "single_role".to_string()
        } else {
            "ring".to_string()
        },
        fresh,
        stale,
        missing,
        quorum: authority_fresh >= 2,
        roles,
    }
}

fn set_dir_mode(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn set_file_mode(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Minimal support-role loop for warden/anchor/sentinel. The core uses the
/// admin server instead. This is heartbeat-only in the current slice; peer
/// kickstart arrives with the launchd installer.
pub async fn run_support_role(data_dir: PathBuf, role: CustosRole) -> Result<()> {
    let started_at = vp_protocol::now_rfc3339();
    let mut seq = 0u64;
    let mut write = || -> Result<()> {
        seq = seq.saturating_add(1);
        let heartbeat = Heartbeat::new(
            role,
            std::process::id(),
            started_at.clone(),
            vp_protocol::now_rfc3339(),
            seq,
            crate::discovery::build_hash(),
            crate::charter::hash(),
            true,
        );
        write_heartbeat(&data_dir, &heartbeat)
    };
    write()?;
    let mut last_attempt = std::collections::HashMap::new();
    let paths = launchd_paths(&data_dir);
    let runner = crate::launchd::SystemLaunchctl;
    let mut ticks = 0u64;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = tokio::time::sleep(Duration::from_secs(2)) => {
                if let Err(error) = write() {
                    tracing::warn!(%error, role = role.as_str(), "custos support heartbeat failed");
                }
                ticks = ticks.saturating_add(1);
                if ticks.is_multiple_of(5)
                    && let Some(paths) = paths.as_ref()
                {
                    for restart in restart_stale_peers(&data_dir, role, paths, &runner, &mut last_attempt) {
                        if restart.success {
                            tracing::info!(peer = restart.role.as_str(), "custos restarted stale peer");
                        } else {
                            tracing::warn!(peer = restart.role.as_str(), detail = %restart.detail, "custos peer restart failed");
                        }
                    }
                }
            }
        }
    }
    let _ = fs::remove_file(heartbeat_path(&data_dir, role));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_core_is_visible_but_has_no_quorum() {
        let dir = tempfile::TempDir::new().unwrap();
        let heartbeat = Heartbeat::new(
            CustosRole::Core,
            123,
            "2026-11-26T00:00:00Z".into(),
            "2026-11-26T00:00:00Z".into(),
            1,
            "sha256:dev".into(),
            "sha256:charter".into(),
            true,
        );
        write_heartbeat(dir.path(), &heartbeat).unwrap();
        let snapshot = snapshot(dir.path());
        assert_eq!(snapshot.mode, "single_role");
        assert_eq!(snapshot.fresh, 1);
        assert_eq!(snapshot.missing, 3);
        assert!(!snapshot.quorum);
    }

    #[test]
    fn two_authority_roles_form_a_quorum() {
        let dir = tempfile::TempDir::new().unwrap();
        for role in [CustosRole::Core, CustosRole::Warden] {
            let heartbeat = Heartbeat::new(
                role,
                123,
                "2026-11-26T00:00:00Z".into(),
                "2026-11-26T00:00:00Z".into(),
                1,
                "sha256:dev".into(),
                "sha256:charter".into(),
                true,
            );
            write_heartbeat(dir.path(), &heartbeat).unwrap();
        }
        let snapshot = snapshot(dir.path());
        assert!(snapshot.quorum);
        assert_eq!(snapshot.fresh, 2);
    }

    struct FakeRunner {
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl crate::launchd::LaunchctlRunner for FakeRunner {
        fn run(&self, args: &[&str]) -> Result<String> {
            self.calls.lock().unwrap().push(args.join(" "));
            Ok(String::new())
        }
    }

    fn paths(root: &Path) -> crate::launchd::LaunchdPaths {
        crate::launchd::LaunchdPaths::new(
            root.join("home"),
            root.join("data"),
            root.join("custosd"),
        )
    }

    #[test]
    fn watchdog_only_kickstarts_installed_stale_peers_once_per_cooldown() {
        let dir = tempfile::TempDir::new().unwrap();
        let paths = paths(dir.path());
        fs::create_dir_all(&paths.agents_dir).unwrap();
        fs::write(paths.plist_path(CustosRole::Warden), "plist").unwrap();
        let runner = FakeRunner {
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let mut last = std::collections::HashMap::new();
        let first = restart_stale_peers(dir.path(), CustosRole::Core, &paths, &runner, &mut last);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].role, CustosRole::Warden);
        assert!(first[0].success);
        let second = restart_stale_peers(dir.path(), CustosRole::Core, &paths, &runner, &mut last);
        assert!(
            second.is_empty(),
            "cooldown must suppress a second kickstart"
        );
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].contains("com.verlintas.custos-warden"));
    }

    #[test]
    fn watchdog_skips_peers_without_a_plist() {
        let dir = tempfile::TempDir::new().unwrap();
        let paths = paths(dir.path());
        fs::create_dir_all(&paths.agents_dir).unwrap();
        let runner = FakeRunner {
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let mut last = std::collections::HashMap::new();
        let restarts =
            restart_stale_peers(dir.path(), CustosRole::Core, &paths, &runner, &mut last);
        assert!(restarts.is_empty());
        assert!(runner.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn invalid_heartbeat_is_not_treated_as_fresh() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = heartbeat_path(dir.path(), CustosRole::Core);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"not json").unwrap();
        let snapshot = snapshot(dir.path());
        assert_eq!(snapshot.fresh, 0);
        assert_eq!(
            snapshot
                .roles
                .iter()
                .find(|role| role.role == "core")
                .unwrap()
                .state,
            "invalid"
        );
    }
}

/// One best-effort peer restart attempt. The watchdog never touches a role
/// whose plist is not installed, and never restarts itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerRestart {
    pub role: CustosRole,
    pub success: bool,
    pub detail: String,
}

/// Attempt to kickstart stale/missing peers through launchd. A peer without a
/// plist is intentionally skipped: Custos does not install agents implicitly.
pub fn restart_stale_peers(
    data_dir: &Path,
    self_role: CustosRole,
    paths: &crate::launchd::LaunchdPaths,
    runner: &dyn crate::launchd::LaunchctlRunner,
    last_attempt: &mut std::collections::HashMap<CustosRole, std::time::Instant>,
) -> Vec<PeerRestart> {
    let Ok(uid) = crate::launchd::uid() else {
        return Vec::new();
    };
    let mut restarts = Vec::new();
    for role in ALL_ROLES {
        if role == self_role {
            continue;
        }
        let stale = match read_heartbeat(data_dir, role) {
            Ok(None) => true,
            Ok(Some((_, age))) => age > STALE_SECS,
            Err(_) => true,
        };
        if !stale {
            continue;
        }
        if !paths.plist_path(role).is_file() {
            continue;
        }
        if let Some(last) = last_attempt.get(&role)
            && last.elapsed() < Duration::from_secs(30)
        {
            continue;
        }
        last_attempt.insert(role, std::time::Instant::now());
        let target = format!("gui/{uid}/{}", crate::launchd::label(role));
        let result = runner.run(&["kickstart", "-k", target.as_str()]);
        restarts.push(PeerRestart {
            role,
            success: result.is_ok(),
            detail: result
                .err()
                .map(|error| format!("{error:#}").chars().take(300).collect())
                .unwrap_or_else(|| format!("kickstarted {}", crate::launchd::label(role))),
        });
    }
    restarts
}

/// Resolve the current binary and home for a launchd watchdog. Returns None
/// when the process cannot determine a safe absolute path.
pub fn launchd_paths(data_dir: &Path) -> Option<crate::launchd::LaunchdPaths> {
    let home = dirs::home_dir()?;
    let binary = std::env::current_exe().ok()?;
    Some(crate::launchd::LaunchdPaths::new(home, data_dir, binary))
}
