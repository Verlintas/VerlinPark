//! Custos-owned baseline verification and explicit repair.
//!
//! This is the first self-heal foundation: a content-addressed manifest and
//! copies of Custos-owned files. It is deliberately explicit-only in this
//! slice (`--vault-init` / `--vault-repair --yes`); automatic repair and signed
//! releases remain later work. It never touches VerlinPark source, worktrees,
//! project policy, or database rows.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::charter;
use crate::discovery;
use crate::launchd::{self, LaunchdPaths};

pub const VAULT_SCHEMA: &str = "verlinpark.custos_vault/v1";
const MANIFEST: &str = "manifest.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VaultManifest {
    pub schema: String,
    pub build_hash: String,
    pub charter_hash: String,
    pub generated_at: String,
    pub status: String,
    pub files: BTreeMap<String, VaultFile>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VaultFile {
    pub kind: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VaultState {
    Ok,
    NotInitialized,
    Drift,
    VaultDrift,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VaultFileReport {
    pub name: String,
    pub kind: String,
    pub target: String,
    pub target_state: String,
    pub vault_state: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VaultReport {
    pub schema: String,
    pub state: VaultState,
    pub manifest_path: String,
    pub build_hash: Option<String>,
    pub charter_hash: Option<String>,
    pub files: Vec<VaultFileReport>,
    pub errors: Vec<String>,
}

pub fn vault_root(data_dir: &Path) -> PathBuf {
    data_dir.join("custos").join("vault")
}

pub fn manifest_path(data_dir: &Path) -> PathBuf {
    vault_root(data_dir).join(MANIFEST)
}

pub fn init(data_dir: &Path, paths: &LaunchdPaths, binary: &Path) -> Result<VaultReport> {
    if !binary.is_file() {
        bail!("custosd binary is missing: {}", binary.display());
    }
    let root = vault_root(data_dir);
    fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
    set_mode(&root, 0o700)?;
    let mut files = BTreeMap::new();
    copy_into_vault(&root, "custosd", binary, "binary", &mut files)?;
    for role in launchd::ALL_ROLES {
        let plist = paths.plist_path(role);
        if plist.is_file() {
            let name = plist
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or_else(|| anyhow::anyhow!("invalid plist filename"))?;
            copy_into_vault(&root, name, &plist, "plist", &mut files)?;
        }
    }
    let manifest = VaultManifest {
        schema: VAULT_SCHEMA.to_string(),
        build_hash: discovery::build_hash(),
        charter_hash: charter::hash(),
        generated_at: vp_protocol::now_rfc3339(),
        status: "unsigned_dev".to_string(),
        files,
    };
    let path = manifest_path(data_dir);
    let tmp = root.join(".manifest.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(&manifest)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    set_mode(&tmp, 0o600)?;
    fs::rename(&tmp, &path).with_context(|| format!("renaming {}", path.display()))?;
    set_mode(&path, 0o400)?;
    verify(data_dir, paths, binary)
}

pub fn verify(data_dir: &Path, paths: &LaunchdPaths, binary: &Path) -> Result<VaultReport> {
    let path = manifest_path(data_dir);
    let mut report = VaultReport {
        schema: "verlinpark.custos_vault_report/v1".to_string(),
        state: VaultState::NotInitialized,
        manifest_path: path.to_string_lossy().to_string(),
        build_hash: None,
        charter_hash: None,
        files: Vec::new(),
        errors: Vec::new(),
    };
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            report.errors.push("vault manifest missing".to_string());
            return Ok(report);
        }
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let manifest: VaultManifest = match serde_json::from_slice(&bytes) {
        Ok(manifest) => manifest,
        Err(error) => {
            report
                .errors
                .push(format!("vault manifest invalid: {error}"));
            report.state = VaultState::VaultDrift;
            return Ok(report);
        }
    };
    report.build_hash = Some(manifest.build_hash.clone());
    report.charter_hash = Some(manifest.charter_hash.clone());
    if manifest.schema != VAULT_SCHEMA {
        report.errors.push("vault schema mismatch".to_string());
        report.state = VaultState::VaultDrift;
        return Ok(report);
    }
    if manifest.charter_hash != charter::hash() {
        report
            .errors
            .push("charter hash changed since vault init".to_string());
        report.state = VaultState::VaultDrift;
    }
    let root = vault_root(data_dir);
    let mut target_drift = false;
    let mut vault_drift = false;
    for (name, entry) in &manifest.files {
        let vault_file = root.join(name);
        let target = target_path(paths, binary, entry.kind.as_str(), name);
        let vault_hash = hash_file(&vault_file).ok();
        let target_hash = target.as_deref().and_then(|path| hash_file(path).ok());
        let vault_state = if vault_hash.as_deref() == Some(entry.sha256.as_str()) {
            "ok"
        } else if vault_hash.is_none() {
            "missing"
        } else {
            "drift"
        };
        let target_state = match target.as_deref() {
            Some(_) if target_hash.as_deref() == Some(entry.sha256.as_str()) => "ok",
            Some(path) if path.is_file() => "drift",
            Some(_) => "missing",
            None => "unresolved",
        };
        if vault_state != "ok" {
            vault_drift = true;
        }
        if target_state != "ok" {
            target_drift = true;
        }
        report.files.push(VaultFileReport {
            name: name.clone(),
            kind: entry.kind.clone(),
            target: target
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_else(|| "(unresolved)".to_string()),
            target_state: target_state.to_string(),
            vault_state: vault_state.to_string(),
            bytes: entry.bytes,
        });
    }
    report.state = if vault_drift {
        VaultState::VaultDrift
    } else if target_drift {
        VaultState::Drift
    } else {
        VaultState::Ok
    };
    Ok(report)
}

/// Explicit repair of drifted/missing targets from the vault. The tampered
/// target is quarantined first; nothing is deleted. The process must be
/// restarted by launchd/operator after a binary repair.
pub fn repair(
    data_dir: &Path,
    paths: &LaunchdPaths,
    binary: &Path,
    confirmed: bool,
) -> Result<VaultReport> {
    if !confirmed {
        bail!("vault repair requires explicit confirmation");
    }
    let before = verify(data_dir, paths, binary)?;
    if before.state == VaultState::Ok {
        return Ok(before);
    }
    if before.state == VaultState::VaultDrift {
        bail!("vault baseline is missing or drifted; refusing to repair from it");
    }
    let manifest: VaultManifest = serde_json::from_slice(&fs::read(manifest_path(data_dir))?)?;
    let root = vault_root(data_dir);
    let quarantine = data_dir
        .join("custos")
        .join("quarantine")
        .join(compact_timestamp());
    for file in &before.files {
        if file.target_state == "ok" || file.vault_state != "ok" {
            continue;
        }
        let Some(entry) = manifest.files.get(&file.name) else {
            continue;
        };
        let Some(target) = target_path(paths, binary, entry.kind.as_str(), &file.name) else {
            continue;
        };
        if target.exists() {
            fs::create_dir_all(&quarantine)?;
            set_mode(&quarantine, 0o700)?;
            let quarantined = quarantine.join(&file.name);
            fs::rename(&target, &quarantined)
                .with_context(|| format!("quarantining {}", target.display()))?;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(root.join(&file.name), &target)
            .with_context(|| format!("restoring {}", target.display()))?;
        set_mode(&target, if entry.kind == "binary" { 0o755 } else { 0o644 })?;
    }
    verify(data_dir, paths, binary)
}

fn copy_into_vault(
    root: &Path,
    name: &str,
    source: &Path,
    kind: &str,
    files: &mut BTreeMap<String, VaultFile>,
) -> Result<()> {
    let target = root.join(name);
    fs::copy(source, &target).with_context(|| format!("copying {}", source.display()))?;
    set_mode(&target, if kind == "binary" { 0o500 } else { 0o400 })?;
    files.insert(
        name.to_string(),
        VaultFile {
            kind: kind.to_string(),
            sha256: hash_file(&target)?,
            bytes: fs::metadata(&target)?.len(),
        },
    );
    Ok(())
}

fn target_path(paths: &LaunchdPaths, binary: &Path, kind: &str, name: &str) -> Option<PathBuf> {
    match kind {
        "binary" => Some(binary.to_path_buf()),
        "plist" => {
            let role = launchd::ALL_ROLES.into_iter().find(|role| {
                paths
                    .plist_path(*role)
                    .file_name()
                    .and_then(|value| value.to_str())
                    == Some(name)
            })?;
            Some(paths.plist_path(role))
        }
        _ => None,
    }
}

fn hash_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path)?;
    let digest = Sha256::digest(&bytes);
    Ok(format!("sha256:{}", hex(&digest)))
}

fn compact_timestamp() -> String {
    let value = vp_protocol::now_rfc3339();
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(20)
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, TempDir, LaunchdPaths, PathBuf) {
        let root = TempDir::new().unwrap();
        let data = root.path().join("data");
        let home = root.path().join("home");
        let binary = root.path().join("custosd");
        fs::create_dir_all(data.join("custos")).unwrap();
        fs::create_dir_all(home.join("Library/LaunchAgents")).unwrap();
        fs::write(&binary, b"binary-v1").unwrap();
        let paths = LaunchdPaths::new(home, &data, &binary);
        fs::write(
            paths.plist_path(crate::protocol::CustosRole::Core),
            b"plist-v1",
        )
        .unwrap();
        (root, TempDir::new().unwrap(), paths, binary)
    }

    #[test]
    fn init_verify_and_repair_round_trip() {
        let (_root, _unused, paths, binary) = fixture();
        let data = paths.data_dir.clone();
        let report = init(&data, &paths, &binary).unwrap();
        assert_eq!(report.state, VaultState::Ok);
        assert!(manifest_path(&data).is_file());

        fs::write(&binary, b"binary-v2").unwrap();
        let drift = verify(&data, &paths, &binary).unwrap();
        assert_eq!(drift.state, VaultState::Drift);
        assert_eq!(
            drift
                .files
                .iter()
                .find(|file| file.kind == "binary")
                .unwrap()
                .target_state,
            "drift"
        );

        let repaired = repair(&data, &paths, &binary, true).unwrap();
        assert_eq!(repaired.state, VaultState::Ok);
        assert_eq!(fs::read(&binary).unwrap(), b"binary-v1");
        assert!(data.join("custos").join("quarantine").is_dir());
    }

    #[test]
    fn repair_requires_confirmation() {
        let (_root, _unused, paths, binary) = fixture();
        let data = paths.data_dir.clone();
        init(&data, &paths, &binary).unwrap();
        assert!(repair(&data, &paths, &binary, false).is_err());
    }

    #[test]
    fn verify_without_manifest_is_not_initialized() {
        let (_root, _unused, paths, binary) = fixture();
        let report = verify(&paths.data_dir, &paths, &binary).unwrap();
        assert_eq!(report.state, VaultState::NotInitialized);
    }
}
