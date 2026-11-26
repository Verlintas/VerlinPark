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
