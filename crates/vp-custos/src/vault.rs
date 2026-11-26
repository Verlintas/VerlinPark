//! Custos-owned baseline verification and explicit repair.
//!
//! The vault holds a content-addressed copy of Custos-owned files (the
//! `custosd` binary and installed LaunchAgent plists), plus an optional local
//! Ed25519 signature over the manifest body. It is deliberately explicit-only:
//! `--vault-init` / `--vault-repair --yes`. There is no automatic repair or
//! release signing in this slice, and it never touches VerlinPark source,
//! worktrees, project policy, or database rows.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rand::RngCore;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair as _, UnparsedPublicKey};
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
    #[serde(default)]
    pub signature: Option<VaultSignature>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VaultSignature {
    pub algorithm: String,
    pub key_id: String,
    pub public_key_b64: String,
    pub signature_b64: String,
    pub signed_at: String,
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
    pub signature: String,
    pub files: Vec<VaultFileReport>,
    pub errors: Vec<String>,
}

pub fn vault_root(data_dir: &Path) -> PathBuf {
    data_dir.join("custos").join("vault")
}

pub fn manifest_path(data_dir: &Path) -> PathBuf {
    vault_root(data_dir).join(MANIFEST)
}

/// Unsigned development baseline.
pub fn init(data_dir: &Path, paths: &LaunchdPaths, binary: &Path) -> Result<VaultReport> {
    init_inner(data_dir, paths, binary, None)
}

/// Baseline signed with a local 0600 Ed25519 seed. This is a local development
/// signature, not a signed release.
pub fn init_signed(
    data_dir: &Path,
    paths: &LaunchdPaths,
    binary: &Path,
    signing_key: &Path,
) -> Result<VaultReport> {
    let seed = read_signing_seed(signing_key)?;
    init_inner(data_dir, paths, binary, Some(&seed))
}

/// Export the base64 public key for a seed. This is public material; the seed
/// itself remains 0600 and is never printed.
pub fn export_public_key(seed_path: &Path, public_path: &Path, confirmed: bool) -> Result<String> {
    if !confirmed {
        bail!("public-key export requires explicit confirmation");
    }
    if public_path.exists() {
        bail!(
            "refusing to overwrite existing public key: {}",
            public_path.display()
        );
    }
    let seed = read_signing_seed(seed_path)?;
    let pair = Ed25519KeyPair::from_seed_unchecked(&seed)
        .map_err(|_| anyhow::anyhow!("signing seed is not a valid Ed25519 seed"))?;
    let text = format!("{}\n", BASE64.encode(pair.public_key().as_ref()));
    fs::write(public_path, text).with_context(|| format!("writing {}", public_path.display()))?;
    set_mode(public_path, 0o644)?;
    Ok(format!(
        "ed25519 public key written to {}",
        public_path.display()
    ))
}

/// Generate a 0600 32-byte Ed25519 seed for local vault signing.
pub fn keygen(path: &Path, confirmed: bool) -> Result<String> {
    if !confirmed {
        bail!("vault key generation requires explicit confirmation");
    }
    if path.exists() {
        bail!(
            "refusing to overwrite existing signing key: {}",
            path.display()
        );
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        set_mode(parent, 0o700)?;
    }
    let mut seed = [0u8; 32];
    rand::rng().fill_bytes(&mut seed);
    fs::write(path, seed).with_context(|| format!("writing {}", path.display()))?;
    set_mode(path, 0o600)?;
    let pair = Ed25519KeyPair::from_seed_unchecked(&seed)
        .map_err(|_| anyhow::anyhow!("generated seed is not a valid Ed25519 seed"))?;
    Ok(format!(
        "ed25519 key written to {} (public key {})",
        path.display(),
        BASE64.encode(pair.public_key().as_ref())
    ))
}

fn init_inner(
    data_dir: &Path,
    paths: &LaunchdPaths,
    binary: &Path,
    seed: Option<&[u8; 32]>,
) -> Result<VaultReport> {
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
    let mut manifest = VaultManifest {
        schema: VAULT_SCHEMA.to_string(),
        build_hash: discovery::build_hash(),
        charter_hash: charter::hash(),
        generated_at: vp_protocol::now_rfc3339(),
        status: "unsigned_dev".to_string(),
        files,
        signature: None,
    };
    if let Some(seed) = seed {
        manifest.status = "local_ed25519".to_string();
        manifest.signature = Some(sign_manifest(&manifest, seed)?);
    }
    write_manifest(data_dir, &manifest)?;
    verify(data_dir, paths, binary)
}

fn write_manifest(data_dir: &Path, manifest: &VaultManifest) -> Result<()> {
    let root = vault_root(data_dir);
    fs::create_dir_all(&root)?;
    let path = manifest_path(data_dir);
    let tmp = root.join(".manifest.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(manifest)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    set_mode(&tmp, 0o600)?;
    fs::rename(&tmp, &path).with_context(|| format!("renaming {}", path.display()))?;
    set_mode(&path, 0o400)?;
    Ok(())
}

fn sign_manifest(manifest: &VaultManifest, seed: &[u8; 32]) -> Result<VaultSignature> {
    let pair = Ed25519KeyPair::from_seed_unchecked(seed)
        .map_err(|_| anyhow::anyhow!("signing seed is not a valid Ed25519 seed"))?;
    let public = pair.public_key().as_ref().to_vec();
    let payload = canonical_payload(manifest)?;
    let signature = pair.sign(&payload);
    Ok(VaultSignature {
        algorithm: "ed25519".to_string(),
        key_id: key_id(&public),
        public_key_b64: BASE64.encode(&public),
        signature_b64: BASE64.encode(signature.as_ref()),
        signed_at: vp_protocol::now_rfc3339(),
    })
}

fn canonical_payload(manifest: &VaultManifest) -> Result<Vec<u8>> {
    let mut value = serde_json::to_value(manifest)?;
    if let Some(object) = value.as_object_mut() {
        object.remove("signature");
    }
    Ok(serde_json::to_vec(&value)?)
}

fn key_id(public: &[u8]) -> String {
    let digest = Sha256::digest(public);
    hex(&digest)[..16].to_string()
}

fn read_signing_seed(path: &Path) -> Result<[u8; 32]> {
    let metadata = fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!("signing key must be 0600: {}", path.display());
        }
    }
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let seed: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("signing key must contain exactly 32 bytes"))?;
    Ok(seed)
}

pub fn verify(data_dir: &Path, paths: &LaunchdPaths, binary: &Path) -> Result<VaultReport> {
    verify_inner(data_dir, paths, binary, None)
}

/// Verify against a caller-pinned Ed25519 public key. A missing/invalid
/// signature is a vault drift, not a local trust fallback.
pub fn verify_pinned(
    data_dir: &Path,
    paths: &LaunchdPaths,
    binary: &Path,
    pinned_public_key: &[u8],
) -> Result<VaultReport> {
    verify_inner(data_dir, paths, binary, Some(pinned_public_key))
}

fn verify_inner(
    data_dir: &Path,
    paths: &LaunchdPaths,
    binary: &Path,
    pinned_public_key: Option<&[u8]>,
) -> Result<VaultReport> {
    let path = manifest_path(data_dir);
    let mut report = VaultReport {
        schema: "verlinpark.custos_vault_report/v1".to_string(),
        state: VaultState::NotInitialized,
        manifest_path: path.to_string_lossy().to_string(),
        build_hash: None,
        charter_hash: None,
        signature: "none".to_string(),
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
    match manifest.signature.as_ref() {
        Some(signature) => match verify_signature(&manifest, signature, pinned_public_key) {
            Ok(state) => report.signature = state.to_string(),
            Err(error) => {
                report.signature = "invalid".to_string();
                report
                    .errors
                    .push(format!("vault signature invalid: {error:#}"));
                report.state = VaultState::VaultDrift;
            }
        },
        None if pinned_public_key.is_some() => {
            report.signature = "missing".to_string();
            report
                .errors
                .push("pinned release key requires a signed vault manifest".to_string());
            report.state = VaultState::VaultDrift;
        }
        None => report.signature = "unsigned_dev".to_string(),
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
    report.state = if report.state == VaultState::VaultDrift || vault_drift {
        VaultState::VaultDrift
    } else if target_drift {
        VaultState::Drift
    } else {
        VaultState::Ok
    };
    Ok(report)
}

fn verify_signature(
    manifest: &VaultManifest,
    signature: &VaultSignature,
    pinned_public_key: Option<&[u8]>,
) -> Result<&'static str> {
    if signature.algorithm != "ed25519" {
        bail!("unsupported signature algorithm");
    }
    let embedded = BASE64
        .decode(signature.public_key_b64.as_bytes())
        .context("public key is not base64")?;
    let sig = BASE64
        .decode(signature.signature_b64.as_bytes())
        .context("signature is not base64")?;
    if embedded.len() != 32 || sig.len() != 64 {
        bail!("public key or signature has the wrong length");
    }
    let public = match pinned_public_key {
        Some(pinned) => {
            if pinned.len() != 32 {
                bail!("pinned public key must be 32 bytes");
            }
            if pinned != embedded {
                bail!("signature public key does not match the pinned release key");
            }
            if key_id(pinned) != signature.key_id {
                bail!("signature key id does not match the pinned release key");
            }
            pinned
        }
        None => {
            if key_id(&embedded) != signature.key_id {
                bail!("signature key id does not match the public key");
            }
            embedded.as_slice()
        }
    };
    let payload = canonical_payload(manifest)?;
    UnparsedPublicKey::new(&ED25519, public)
        .verify(&payload, &sig)
        .map_err(|_| anyhow::anyhow!("Ed25519 verification failed"))?;
    Ok(if pinned_public_key.is_some() {
        "valid_pinned_ed25519"
    } else {
        "valid_local_ed25519"
    })
}

/// Read a base64 Ed25519 public key (32 raw bytes). A leading `ed25519:` is
/// accepted for convenience; no key material is logged.
pub fn read_public_key(path: &Path) -> Result<[u8; 32]> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let text = text.trim();
    let text = text.strip_prefix("ed25519:").unwrap_or(text);
    let bytes = BASE64
        .decode(text.as_bytes())
        .context("public key is not base64")?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("public key must contain exactly 32 bytes"))
}

/// Explicit repair of drifted/missing targets from the vault. The tampered
/// target is quarantined first; nothing is deleted. A binary repair requires a
/// launchd/operator restart afterward.
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

    fn fixture() -> (TempDir, LaunchdPaths, PathBuf) {
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
        (root, paths, binary)
    }

    #[test]
    fn init_verify_and_repair_round_trip() {
        let (_root, paths, binary) = fixture();
        let data = paths.data_dir.clone();
        let report = init(&data, &paths, &binary).unwrap();
        assert_eq!(report.state, VaultState::Ok);
        assert_eq!(report.signature, "unsigned_dev");
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
    fn signed_vault_verifies_and_detects_tamper() {
        let (root, paths, binary) = fixture();
        let data = paths.data_dir.clone();
        let key = root.path().join("vault.key");
        keygen(&key, true).unwrap();
        let report = init_signed(&data, &paths, &binary, &key).unwrap();
        assert_eq!(report.state, VaultState::Ok);
        assert_eq!(report.signature, "valid_local_ed25519");

        let path = manifest_path(&data);
        let mut manifest: VaultManifest =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        manifest.build_hash = "sha256:tampered".to_string();
        set_mode(&path, 0o600).unwrap();
        fs::write(&path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
        let tampered = verify(&data, &paths, &binary).unwrap();
        assert_eq!(tampered.state, VaultState::VaultDrift);
        assert_eq!(tampered.signature, "invalid");
    }

    #[test]
    fn pinned_release_key_verifies_only_the_matching_signature() {
        let (root, paths, binary) = fixture();
        let data = paths.data_dir.clone();
        let key = root.path().join("release.key");
        let public = root.path().join("release.pub");
        keygen(&key, true).unwrap();
        export_public_key(&key, &public, true).unwrap();
        init_signed(&data, &paths, &binary, &key).unwrap();
        let pinned = read_public_key(&public).unwrap();
        let report = verify_pinned(&data, &paths, &binary, &pinned).unwrap();
        assert_eq!(report.state, VaultState::Ok);
        assert_eq!(report.signature, "valid_pinned_ed25519");

        let other = root.path().join("other.key");
        keygen(&other, true).unwrap();
        let other_public = read_signing_seed(&other).unwrap();
        let wrong = {
            let pair = Ed25519KeyPair::from_seed_unchecked(&other_public).unwrap();
            let mut bytes = [0u8; 32];
            bytes.copy_from_slice(pair.public_key().as_ref());
            bytes
        };
        let rejected = verify_pinned(&data, &paths, &binary, &wrong).unwrap();
        assert_eq!(rejected.state, VaultState::VaultDrift);
        assert_eq!(rejected.signature, "invalid");
    }

    #[test]
    fn pinned_release_key_rejects_an_unsigned_vault() {
        let (_root, paths, binary) = fixture();
        let data = paths.data_dir.clone();
        init(&data, &paths, &binary).unwrap();
        let report = verify_pinned(&data, &paths, &binary, &[7u8; 32]).unwrap();
        assert_eq!(report.state, VaultState::VaultDrift);
        assert_eq!(report.signature, "missing");
    }

    #[test]
    fn repair_requires_confirmation() {
        let (_root, paths, binary) = fixture();
        let data = paths.data_dir.clone();
        init(&data, &paths, &binary).unwrap();
        assert!(repair(&data, &paths, &binary, false).is_err());
    }

    #[test]
    fn verify_without_manifest_is_not_initialized() {
        let (_root, paths, binary) = fixture();
        let report = verify(&paths.data_dir, &paths, &binary).unwrap();
        assert_eq!(report.state, VaultState::NotInitialized);
    }
}
