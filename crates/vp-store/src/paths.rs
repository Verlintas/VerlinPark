use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Root for VerlinPark's local state.
///
/// Override with `VERLINPARK_DATA_DIR` for development and tests. Otherwise use
/// the standard macOS Application Support location.
pub fn data_dir() -> PathBuf {
    if let Some(path) = env::var_os("VERLINPARK_DATA_DIR") {
        return PathBuf::from(path);
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("VerlinPark")
}

pub fn database_path() -> PathBuf {
    data_dir().join("verlinpark.db")
}

pub fn discovery_path() -> PathBuf {
    data_dir().join("run").join("daemon.json")
}

pub fn sessions_dir() -> PathBuf {
    data_dir().join("sessions")
}

pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}

pub fn ensure_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn ensure_private_file(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub fn ensure_base_dirs() -> io::Result<()> {
    ensure_private_dir(&data_dir())?;
    ensure_private_dir(&data_dir().join("run"))?;
    ensure_private_dir(&sessions_dir())?;
    ensure_private_dir(&logs_dir())?;
    Ok(())
}
