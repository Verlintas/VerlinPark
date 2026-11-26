//! Custos-local discovery record. This is separate from the daemon's
//! `run/daemon.json`; the GUI/CLI can find the read-only socket without
//! touching the daemon token.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::protocol::{CUSTOS_PROTOCOL_VERSION, CUSTOS_VERSION, CustosRole};

pub const DISCOVERY_SCHEMA: &str = "verlinpark.custos_discovery/v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CustosDiscovery {
    pub schema: String,
    pub pid: u32,
    pub socket: String,
    pub protocol: u32,
    pub version: String,
    pub role: String,
    pub started_at: String,
    pub build_hash: String,
    pub observe_only: bool,
}

pub fn custos_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("custos")
}

pub fn default_socket_path(data_dir: &Path) -> PathBuf {
    custos_dir(data_dir).join("custos.sock")
}

pub fn discovery_path(data_dir: &Path) -> PathBuf {
    custos_dir(data_dir).join("discovery.json")
}

pub fn build_hash() -> String {
    option_env!("VERLINPARK_BUILD_HASH")
        .unwrap_or("sha256:dev")
        .to_string()
}

pub fn write_discovery(
    data_dir: &Path,
    socket: &Path,
    role: CustosRole,
    started_at: &str,
) -> Result<CustosDiscovery> {
    let discovery = CustosDiscovery {
        schema: DISCOVERY_SCHEMA.to_string(),
        pid: std::process::id(),
        socket: socket.to_string_lossy().to_string(),
        protocol: CUSTOS_PROTOCOL_VERSION,
        version: CUSTOS_VERSION.to_string(),
        role: role.as_str().to_string(),
        started_at: started_at.to_string(),
        build_hash: build_hash(),
        observe_only: true,
    };
    let dir = custos_dir(data_dir);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    set_dir_mode(&dir)?;
    let path = discovery_path(data_dir);
    let tmp = dir.join(".discovery.tmp");
    let bytes = serde_json::to_vec_pretty(&discovery)?;
    {
        let mut file =
            fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        set_file_mode(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, &path).with_context(|| format!("renaming {}", path.display()))?;
    Ok(discovery)
}

pub fn read_discovery(data_dir: &Path) -> Result<Option<CustosDiscovery>> {
    let path = discovery_path(data_dir);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let discovery =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(discovery))
}

pub fn remove_discovery(data_dir: &Path) {
    let _ = fs::remove_file(discovery_path(data_dir));
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
    fn discovery_round_trips() {
        let dir = tempfile::TempDir::new().unwrap();
        let socket = default_socket_path(dir.path());
        let discovery = write_discovery(dir.path(), &socket, CustosRole::Core, "now").unwrap();
        let read = read_discovery(dir.path()).unwrap().unwrap();
        assert_eq!(discovery.pid, read.pid);
        assert_eq!(read.role, "core");
        assert_eq!(read.socket, socket.to_string_lossy());
        assert!(read.observe_only);
    }

    #[test]
    fn missing_discovery_is_none() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(read_discovery(dir.path()).unwrap().is_none());
    }
}
