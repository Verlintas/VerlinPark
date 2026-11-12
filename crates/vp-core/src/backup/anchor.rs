//! Local anti-rollback anchor for verified backup repositories.
//!
//! The anchor is a small 0600 JSON file in the daemon data directory. After a
//! successful backup, verification, or restore it records the highest
//! repository generation this data directory has observed. A later verify or
//! restore that presents an older generation is rejected unless the operator
//! explicitly passes `--allow-rollback`.
//!
//! # What this is not
//!
//! The anchor is local state owned by the same OS user as the data directory.
//! It is not tamper-proof against that user, an administrator, or root, and it
//! is not remote attestation, key escrow, or a trusted timestamp. An attacker
//! who can rewrite both the repository and the anchor can defeat this check.
//! Its purpose is to catch accidental restores and simple replay of an older
//! signed repository, not a fully compromised host.
//!
//! An `--allow-rollback` operation records the lower generation as
//! `last_verified_generation` but deliberately does **not** lower
//! `highest_generation`, so a subsequent silent rollback is still rejected.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;
use vp_protocol::now_rfc3339;

use super::error::BackupSecurityError;
use super::signing::{
    Keyring, PORTABLE_ANCHOR_SIGNATURE_DOMAIN, SIGNATURE_BYTES, SIGNING_ALGORITHM,
    SigningCredential, validate_key_id, verify_payload,
};

pub const ANCHOR_FILENAME: &str = "backup-anchor.json";
pub const ANCHOR_EVENTS_FILENAME: &str = "backup-anchor-events.jsonl";
pub const ANCHOR_LOCK_FILENAME: &str = "backup-anchor.lock";
pub const ANCHOR_KIND: &str = "verlinpark-backup-anchor";
pub const ANCHOR_FORMAT_VERSION: u32 = 1;
pub const MAX_ANCHOR_BYTES: u64 = 256 * 1024;
pub const MAX_ANCHOR_REPOSITORIES: usize = 64;
pub const MAX_ANCHOR_PATH_BYTES: usize = 4096;

pub const PORTABLE_ANCHOR_KIND: &str = "verlinpark-portable-backup-anchor";
pub const PORTABLE_ANCHOR_FORMAT_VERSION: u32 = 1;
pub const PORTABLE_ANCHOR_FILENAME: &str = "backup-portable-anchor.json";
pub const MAX_PORTABLE_ANCHOR_BYTES: u64 = 1024 * 1024;
pub const MAX_PORTABLE_ANCHOR_REPOSITORIES: usize = MAX_ANCHOR_REPOSITORIES;

/// One repository's high-water mark.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RepositoryAnchor {
    pub repository_id: String,
    pub repository_path: String,
    pub highest_generation: u64,
    pub last_verified_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_snapshot_id: Option<String>,
    pub last_operation: String,
    pub first_seen_at: String,
    pub last_verified_at: String,
    /// `None` for observations made on this host; `portable_import` for
    /// high-water marks raised by a signed portable anchor document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

/// Persisted anchor table.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AnchorFile {
    pub kind: String,
    pub version: u32,
    pub updated_at: String,
    #[serde(default)]
    pub repositories: BTreeMap<String, RepositoryAnchor>,
}

impl Default for AnchorFile {
    fn default() -> Self {
        Self {
            kind: ANCHOR_KIND.to_string(),
            version: ANCHOR_FORMAT_VERSION,
            updated_at: now_rfc3339(),
            repositories: BTreeMap::new(),
        }
    }
}

impl AnchorFile {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn validate(&self) -> Result<()> {
        if self.kind != ANCHOR_KIND {
            return Err(BackupSecurityError::AnchorInvalid {
                reason: format!("unexpected anchor kind {:?}", self.kind),
            }
            .into());
        }
        if self.version != ANCHOR_FORMAT_VERSION {
            return Err(BackupSecurityError::AnchorInvalid {
                reason: format!("unsupported anchor version {}", self.version),
            }
            .into());
        }
        validate_timestamp(&self.updated_at, "anchor updated_at")?;
        if self.repositories.len() > MAX_ANCHOR_REPOSITORIES {
            return Err(BackupSecurityError::AnchorInvalid {
                reason: format!("anchor tracks more than {MAX_ANCHOR_REPOSITORIES} repositories"),
            }
            .into());
        }
        for (id, anchor) in &self.repositories {
            if id != &anchor.repository_id {
                return Err(BackupSecurityError::AnchorInvalid {
                    reason: "anchor repository map key does not match its entry".to_string(),
                }
                .into());
            }
            if Uuid::parse_str(id).is_err() || id.len() != 36 {
                return Err(BackupSecurityError::AnchorInvalid {
                    reason: "anchor repository id is not a canonical UUID".to_string(),
                }
                .into());
            }
            if anchor.repository_path.is_empty()
                || anchor.repository_path.len() > MAX_ANCHOR_PATH_BYTES
            {
                return Err(BackupSecurityError::AnchorInvalid {
                    reason: "anchor repository path is malformed".to_string(),
                }
                .into());
            }
            if anchor.highest_generation == 0 {
                return Err(BackupSecurityError::AnchorInvalid {
                    reason: "anchor highest generation must be positive".to_string(),
                }
                .into());
            }
            if anchor.last_verified_generation == 0
                || anchor.last_verified_generation > anchor.highest_generation
            {
                return Err(BackupSecurityError::AnchorInvalid {
                    reason: "anchor last verified generation is out of range".to_string(),
                }
                .into());
            }
            if let Some(snapshot_id) = &anchor.last_snapshot_id
                && (snapshot_id.len() != 36 || Uuid::parse_str(snapshot_id).is_err())
            {
                return Err(BackupSecurityError::AnchorInvalid {
                    reason: "anchor snapshot id is not a canonical UUID".to_string(),
                }
                .into());
            }
            match anchor.last_operation.as_str() {
                "backup" | "verify" | "restore" | "anchor_import" => {}
                other => {
                    return Err(BackupSecurityError::AnchorInvalid {
                        reason: format!("anchor operation {other:?} is not recognised"),
                    }
                    .into());
                }
            }
            if let Some(origin) = &anchor.origin
                && origin != "portable_import"
            {
                return Err(BackupSecurityError::AnchorInvalid {
                    reason: format!("anchor origin {origin:?} is not recognised"),
                }
                .into());
            }
            validate_timestamp(&anchor.first_seen_at, "anchor first_seen_at")?;
            validate_timestamp(&anchor.last_verified_at, "anchor last_verified_at")?;
        }
        Ok(())
    }

    pub fn anchor(&self, repository_id: &str) -> Option<&RepositoryAnchor> {
        self.repositories.get(repository_id)
    }
}

fn validate_timestamp(value: &str, label: &str) -> Result<()> {
    if value.is_empty() || value.len() > 64 {
        return Err(BackupSecurityError::AnchorInvalid {
            reason: format!("{label} is malformed"),
        }
        .into());
    }
    Ok(())
}

/// Result of a rollback precondition check.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RollbackCheck {
    pub repository_id: String,
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_generation: Option<u64>,
    pub accepted: bool,
    pub rollback: bool,
    pub allow_rollback: bool,
    pub anchor_updated: bool,
    pub anchor_path: String,
}

/// Result of recording an anchor observation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnchorUpdate {
    pub repository_id: String,
    pub generation: u64,
    pub previous_highest: Option<u64>,
    pub highest_generation: u64,
    pub rollback: bool,
    pub operation: String,
    pub anchor_path: String,
    pub updated_at: String,
}

/// Secret-free anchor summary for reports and the daemon status RPC.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnchorSummary {
    pub path: String,
    pub present: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    pub repositories: Vec<RepositoryAnchor>,
    /// Additive: entries whose high-water mark came from a portable import.
    #[serde(default)]
    pub imported_repositories: usize,
}

/// Anchor file accessor.
#[derive(Clone, Debug)]
pub struct AnchorStore {
    path: PathBuf,
}

impl AnchorStore {
    /// Anchor next to the data directory's database.
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(ANCHOR_FILENAME),
        }
    }

    /// Explicit anchor file path.
    pub fn from_file(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the anchor, treating a missing file as an empty anchor.
    ///
    /// Structural corruption is a typed error, never silently ignored: an
    /// unreadable anchor could otherwise disable rollback detection.
    pub fn read(&self) -> Result<AnchorFile> {
        match fs::symlink_metadata(&self.path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(BackupSecurityError::AnchorInvalid {
                    reason: format!("anchor {} is a symlink", self.path.display()),
                }
                .into());
            }
            Ok(metadata) if !metadata.is_file() => {
                return Err(BackupSecurityError::AnchorInvalid {
                    reason: format!("anchor {} is not a regular file", self.path.display()),
                }
                .into());
            }
            Ok(metadata) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = metadata.permissions().mode() & 0o777;
                    if mode & 0o077 != 0 {
                        return Err(BackupSecurityError::AnchorInvalid {
                            reason: format!(
                                "anchor {} has mode {mode:04o}; require 0600",
                                self.path.display()
                            ),
                        }
                        .into());
                    }
                }
                if metadata.len() > MAX_ANCHOR_BYTES {
                    return Err(BackupSecurityError::AnchorInvalid {
                        reason: "anchor exceeds its size limit".to_string(),
                    }
                    .into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AnchorFile::empty());
            }
            Err(error) => {
                return Err(BackupSecurityError::AnchorIo {
                    reason: format!("reading {}: {error}", self.path.display()),
                }
                .into());
            }
        }
        let mut bytes = Vec::new();
        File::open(&self.path)
            .with_context(|| format!("opening backup anchor {}", self.path.display()))?
            .take(MAX_ANCHOR_BYTES + 1)
            .read_to_end(&mut bytes)
            .with_context(|| format!("reading backup anchor {}", self.path.display()))?;
        if bytes.len() as u64 > MAX_ANCHOR_BYTES {
            return Err(BackupSecurityError::AnchorInvalid {
                reason: "anchor grew beyond its size limit".to_string(),
            }
            .into());
        }
        let anchor: AnchorFile =
            serde_json::from_slice(&bytes).map_err(|error| BackupSecurityError::AnchorInvalid {
                reason: format!("anchor JSON is invalid: {error}"),
            })?;
        anchor.validate()?;
        Ok(anchor)
    }

    /// Check whether `generation` is acceptable without writing anything.
    pub fn check(
        &self,
        repository_id: &str,
        generation: u64,
        allow_rollback: bool,
    ) -> Result<RollbackCheck> {
        let anchor = self.read()?;
        self.check_in(&anchor, repository_id, generation, allow_rollback)
    }

    fn check_in(
        &self,
        anchor: &AnchorFile,
        repository_id: &str,
        generation: u64,
        allow_rollback: bool,
    ) -> Result<RollbackCheck> {
        if generation == 0 {
            return Err(BackupSecurityError::AnchorInvalid {
                reason: "repository generation must be positive".to_string(),
            }
            .into());
        }
        let previous = anchor.anchor(repository_id);
        match previous {
            Some(previous) if generation < previous.highest_generation && !allow_rollback => {
                Err(BackupSecurityError::RollbackDetected {
                    repository_id: repository_id.to_string(),
                    generation,
                    anchor_generation: previous.highest_generation,
                }
                .into())
            }
            Some(previous) => Ok(RollbackCheck {
                repository_id: repository_id.to_string(),
                generation,
                anchor_generation: Some(previous.highest_generation),
                accepted: true,
                rollback: generation < previous.highest_generation,
                allow_rollback,
                anchor_updated: false,
                anchor_path: self.path.display().to_string(),
            }),
            None => Ok(RollbackCheck {
                repository_id: repository_id.to_string(),
                generation,
                anchor_generation: None,
                accepted: true,
                rollback: false,
                allow_rollback,
                anchor_updated: false,
                anchor_path: self.path.display().to_string(),
            }),
        }
    }

    /// Record a successful observation and persist atomically.
    ///
    /// Refuses to lower the high-water mark unless `allow_rollback` is set. The
    /// call fails closed if the anchor cannot be written: a restore that cannot
    /// record its generation must not silently lose rollback protection.
    pub fn record_verified(
        &self,
        repository_id: &str,
        repository_path: &str,
        snapshot_id: Option<&str>,
        generation: u64,
        operation: &str,
        allow_rollback: bool,
    ) -> Result<AnchorUpdate> {
        if generation == 0 {
            return Err(BackupSecurityError::AnchorInvalid {
                reason: "repository generation must be positive".to_string(),
            }
            .into());
        }
        match operation {
            "backup" | "verify" | "restore" => {}
            other => {
                return Err(BackupSecurityError::AnchorInvalid {
                    reason: format!("anchor operation {other:?} is not recognised"),
                }
                .into());
            }
        }
        if let Some(snapshot_id) = snapshot_id
            && (snapshot_id.len() != 36 || Uuid::parse_str(snapshot_id).is_err())
        {
            return Err(BackupSecurityError::AnchorInvalid {
                reason: "anchor snapshot id is not a canonical UUID".to_string(),
            }
            .into());
        }
        let _lock = self.lock()?;
        let mut anchor = self.read()?;
        let now = now_rfc3339();
        let previous = anchor.repositories.get(repository_id).cloned();
        if let Some(previous) = &previous
            && generation < previous.highest_generation
            && !allow_rollback
        {
            return Err(BackupSecurityError::RollbackDetected {
                repository_id: repository_id.to_string(),
                generation,
                anchor_generation: previous.highest_generation,
            }
            .into());
        }
        let previous_highest = previous.as_ref().map(|entry| entry.highest_generation);
        let highest = previous_highest.unwrap_or(generation).max(generation).max(
            previous
                .as_ref()
                .map(|entry| entry.last_verified_generation)
                .unwrap_or(0),
        );
        let rollback = previous_highest.is_some_and(|highest| generation < highest);
        let entry = RepositoryAnchor {
            repository_id: repository_id.to_string(),
            repository_path: repository_path.to_string(),
            highest_generation: highest,
            last_verified_generation: generation,
            last_snapshot_id: snapshot_id.map(str::to_string),
            last_operation: operation.to_string(),
            first_seen_at: previous
                .as_ref()
                .map(|entry| entry.first_seen_at.clone())
                .unwrap_or_else(|| now.clone()),
            last_verified_at: now.clone(),
            origin: None,
        };
        anchor.repositories.insert(repository_id.to_string(), entry);
        prune_anchor_entries(&mut anchor);
        anchor.updated_at = now.clone();
        anchor.validate()?;
        self.write(&anchor)?;
        Ok(AnchorUpdate {
            repository_id: repository_id.to_string(),
            generation,
            previous_highest,
            highest_generation: highest,
            rollback,
            operation: operation.to_string(),
            anchor_path: self.path.display().to_string(),
            updated_at: now,
        })
    }

    pub(crate) fn write(&self, anchor: &AnchorFile) -> Result<()> {
        anchor.validate()?;
        let parent = match self.path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        vp_store::paths::ensure_private_dir(parent)
            .with_context(|| format!("creating anchor directory {}", parent.display()))?;
        let mut bytes = serde_json::to_vec_pretty(anchor).context("serializing backup anchor")?;
        if bytes.len() as u64 > MAX_ANCHOR_BYTES {
            return Err(BackupSecurityError::AnchorInvalid {
                reason: "anchor exceeds its size limit".to_string(),
            }
            .into());
        }
        bytes.push(b'\n');
        let scratch = super::ScratchPath::file(parent, "anchor")?;
        super::write_private(&scratch.path, &bytes)?;
        fs::rename(&scratch.path, &self.path).map_err(|error| BackupSecurityError::AnchorIo {
            reason: format!("publishing {}: {error}", self.path.display()),
        })?;
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .ok();
        Ok(())
    }

    pub fn summary(&self) -> Result<AnchorSummary> {
        let anchor = self.read()?;
        let present = self.path.is_file();
        let mut repositories: Vec<RepositoryAnchor> = anchor.repositories.into_values().collect();
        repositories.sort_by(|left, right| {
            left.last_verified_at
                .cmp(&right.last_verified_at)
                .then_with(|| left.repository_id.cmp(&right.repository_id))
        });
        let imported_repositories = repositories
            .iter()
            .filter(|entry| entry.origin.as_deref() == Some("portable_import"))
            .count();
        Ok(AnchorSummary {
            path: self.path.display().to_string(),
            present,
            updated_at: present.then_some(anchor.updated_at),
            repositories,
            imported_repositories,
        })
    }

    /// Remove the anchor file. Returns whether a file was removed.
    ///
    /// This is an explicit operator action: removing the anchor discards local
    /// rollback protection until the next backup/verify/restore records a new
    /// high-water mark.
    pub fn reset(&self) -> Result<bool> {
        let _lock = self.lock()?;
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(BackupSecurityError::AnchorIo {
                reason: format!("removing {}: {error}", self.path.display()),
            }
            .into()),
        }
    }
}

fn prune_anchor_entries(anchor: &mut AnchorFile) {
    while anchor.repositories.len() > MAX_ANCHOR_REPOSITORIES {
        let oldest = anchor
            .repositories
            .iter()
            .min_by(|(left_id, left), (right_id, right)| {
                left.last_verified_at
                    .cmp(&right.last_verified_at)
                    .then_with(|| left_id.cmp(right_id))
            })
            .map(|(id, _)| id.clone());
        match oldest {
            Some(id) => {
                anchor.repositories.remove(&id);
            }
            None => break,
        }
    }
}

// ---------------------------------------------------------------------------
// Portable signed anchor documents
// ---------------------------------------------------------------------------

/// One repository high-water mark in a portable anchor document.
///
/// Paths are deliberately excluded: a portable document moves between hosts
/// where the repository may live at a different path. Only stable ids,
/// generations, and public observation metadata are exported.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PortableAnchorEntry {
    pub repository_id: String,
    pub highest_generation: u64,
    pub last_verified_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_operation: Option<String>,
    pub observed_at: String,
}

/// Detached Ed25519 signature over the canonical document body.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PortableAnchorSignature {
    pub algorithm: String,
    pub key_id: String,
    pub signature_b64: String,
    pub signed_at: String,
}

/// Signed, portable anti-rollback anchor document.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PortableAnchorDocument {
    pub kind: String,
    pub version: u32,
    pub exported_at: String,
    /// Must match `signature.key_id`; duplicated for readable reports.
    pub key_id: String,
    pub repositories: Vec<PortableAnchorEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<PortableAnchorSignature>,
}

impl PortableAnchorDocument {
    /// Canonical bytes signed by the exporting key.
    ///
    /// The signature object itself is excluded. Field order is the serde
    /// struct order, which is stable for a given format version.
    pub fn payload(&self) -> Result<Vec<u8>> {
        let mut copy = self.clone();
        copy.signature = None;
        serde_json::to_vec(&copy).context("serializing portable backup anchor payload")
    }

    pub fn document_sha256(&self) -> Result<String> {
        let bytes =
            serde_json::to_vec(self).context("serializing portable backup anchor document")?;
        Ok(super::crypto::sha256_hex(&bytes))
    }

    /// Structural validation independent of any keyring.
    pub fn validate(&self) -> Result<()> {
        if self.kind != PORTABLE_ANCHOR_KIND {
            return Err(BackupSecurityError::PortableAnchorInvalid {
                reason: format!("unexpected portable anchor kind {:?}", self.kind),
            }
            .into());
        }
        if self.version != PORTABLE_ANCHOR_FORMAT_VERSION {
            return Err(BackupSecurityError::PortableAnchorInvalid {
                reason: format!("unsupported portable anchor version {}", self.version),
            }
            .into());
        }
        if self.repositories.is_empty() {
            return Err(BackupSecurityError::PortableAnchorInvalid {
                reason: "portable anchor has no repositories".to_string(),
            }
            .into());
        }
        if self.repositories.len() > MAX_PORTABLE_ANCHOR_REPOSITORIES {
            return Err(BackupSecurityError::PortableAnchorInvalid {
                reason: format!(
                    "portable anchor has more than {MAX_PORTABLE_ANCHOR_REPOSITORIES} repositories"
                ),
            }
            .into());
        }
        validate_key_id(&self.key_id).map_err(|error| {
            BackupSecurityError::PortableAnchorInvalid {
                reason: format!("{error:#}"),
            }
        })?;
        require_rfc3339(&self.exported_at, "portable anchor exported_at")?;
        let mut ids = std::collections::BTreeSet::new();
        for entry in &self.repositories {
            if !ids.insert(entry.repository_id.clone()) {
                return Err(BackupSecurityError::PortableAnchorInvalid {
                    reason: "portable anchor contains a duplicate repository id".to_string(),
                }
                .into());
            }
            if entry.repository_id.len() != 36 || Uuid::parse_str(&entry.repository_id).is_err() {
                return Err(BackupSecurityError::PortableAnchorInvalid {
                    reason: "portable anchor repository id is not a canonical UUID".to_string(),
                }
                .into());
            }
            if entry.highest_generation == 0 {
                return Err(BackupSecurityError::PortableAnchorInvalid {
                    reason: "portable anchor highest generation must be positive".to_string(),
                }
                .into());
            }
            if entry.last_verified_generation == 0
                || entry.last_verified_generation > entry.highest_generation
            {
                return Err(BackupSecurityError::PortableAnchorInvalid {
                    reason: "portable anchor last verified generation is out of range".to_string(),
                }
                .into());
            }
            if let Some(snapshot_id) = &entry.last_snapshot_id
                && (snapshot_id.len() != 36 || Uuid::parse_str(snapshot_id).is_err())
            {
                return Err(BackupSecurityError::PortableAnchorInvalid {
                    reason: "portable anchor snapshot id is not a canonical UUID".to_string(),
                }
                .into());
            }
            if let Some(operation) = &entry.last_operation {
                match operation.as_str() {
                    "backup" | "verify" | "restore" | "anchor_import" => {}
                    other => {
                        return Err(BackupSecurityError::PortableAnchorInvalid {
                            reason: format!(
                                "portable anchor operation {other:?} is not recognised"
                            ),
                        }
                        .into());
                    }
                }
            }
            require_rfc3339(&entry.observed_at, "portable anchor observed_at")?;
        }
        if let Some(signature) = &self.signature {
            if signature.algorithm != SIGNING_ALGORITHM {
                return Err(BackupSecurityError::PortableAnchorInvalid {
                    reason: format!(
                        "portable anchor uses unsupported algorithm {:?}",
                        signature.algorithm
                    ),
                }
                .into());
            }
            if signature.key_id != self.key_id {
                return Err(BackupSecurityError::PortableAnchorInvalid {
                    reason: "portable anchor key id does not match its signature".to_string(),
                }
                .into());
            }
            let bytes = BASE64
                .decode(signature.signature_b64.as_bytes())
                .map_err(|_| BackupSecurityError::PortableAnchorInvalid {
                    reason: "portable anchor signature is not valid base64".to_string(),
                })?;
            if bytes.len() != SIGNATURE_BYTES {
                return Err(BackupSecurityError::PortableAnchorInvalid {
                    reason: "portable anchor signature has the wrong length".to_string(),
                }
                .into());
            }
            if BASE64.encode(&bytes) != signature.signature_b64 {
                return Err(BackupSecurityError::PortableAnchorInvalid {
                    reason: "portable anchor signature is not canonical base64".to_string(),
                }
                .into());
            }
            require_rfc3339(&signature.signed_at, "portable anchor signed_at")?;
        }
        Ok(())
    }

    fn signature_bytes(&self) -> Result<Vec<u8>> {
        let signature =
            self.signature
                .as_ref()
                .ok_or_else(|| BackupSecurityError::PortableAnchorInvalid {
                    reason: "portable anchor is not signed".to_string(),
                })?;
        BASE64
            .decode(signature.signature_b64.as_bytes())
            .map_err(|_| {
                BackupSecurityError::PortableAnchorInvalid {
                    reason: "portable anchor signature is not valid base64".to_string(),
                }
                .into()
            })
    }
}

fn require_rfc3339(value: &str, label: &str) -> Result<OffsetDateTime> {
    if value.is_empty() || value.len() > 64 {
        return Err(BackupSecurityError::PortableAnchorInvalid {
            reason: format!("{label} is malformed"),
        }
        .into());
    }
    OffsetDateTime::parse(value, &Rfc3339).map_err(|_| {
        BackupSecurityError::PortableAnchorInvalid {
            reason: format!("{label} is not an RFC 3339 timestamp"),
        }
        .into()
    })
}

/// Secret-free report returned by a portable anchor export.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortableAnchorExport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub key_id: String,
    pub exported_at: String,
    pub repository_count: usize,
    pub document_sha256: String,
    pub document: PortableAnchorDocument,
    pub note: String,
}

/// Options for importing a signed portable anchor.
pub struct PortableAnchorImportOptions<'a> {
    pub keyring: &'a Keyring,
    /// Accept a signer whose key is marked revoked. Default false fails closed.
    pub allow_revoked: bool,
    /// Reject a document older than the local anchor's last update even though
    /// merging is otherwise raise-only.
    pub reject_stale: bool,
}

impl std::fmt::Debug for PortableAnchorImportOptions<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PortableAnchorImportOptions")
            .field("keyring", &self.keyring.summary().path)
            .field("allow_revoked", &self.allow_revoked)
            .field("reject_stale", &self.reject_stale)
            .finish()
    }
}

/// One repository merge outcome.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PortableAnchorRaise {
    pub repository_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_highest: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_last_verified: Option<u64>,
    pub highest_generation: u64,
    pub last_verified_generation: u64,
    pub raised: bool,
}

/// Secret-free report returned by a portable anchor import.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortableAnchorImportReport {
    pub anchor_path: String,
    pub key_id: String,
    pub exported_at: String,
    pub document_sha256: String,
    pub stale_export: bool,
    /// True when the importing keyring accepted a revoked signer explicitly.
    #[serde(default)]
    pub signer_revoked: bool,
    pub repositories_total: usize,
    pub repositories_raised: usize,
    pub repositories_unchanged: usize,
    pub raises: Vec<PortableAnchorRaise>,
    pub updated_at: String,
    pub note: String,
}

/// Load and structurally validate a portable anchor file.
///
/// The document is public data, so 0644 is acceptable; symlinks and oversized
/// files are refused. Signature verification is a separate step because it
/// needs a keyring.
pub fn load_portable_anchor(path: &Path) -> Result<PortableAnchorDocument> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("reading portable backup anchor {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(BackupSecurityError::PortableAnchorInvalid {
            reason: format!("portable anchor {} is a symlink", path.display()),
        }
        .into());
    }
    if !metadata.is_file() {
        return Err(BackupSecurityError::PortableAnchorInvalid {
            reason: format!("portable anchor {} is not a regular file", path.display()),
        }
        .into());
    }
    if metadata.len() == 0 || metadata.len() > MAX_PORTABLE_ANCHOR_BYTES {
        return Err(BackupSecurityError::PortableAnchorInvalid {
            reason: "portable anchor size is outside the supported range".to_string(),
        }
        .into());
    }
    let mut bytes = Vec::new();
    File::open(path)
        .with_context(|| format!("opening portable backup anchor {}", path.display()))?
        .take(MAX_PORTABLE_ANCHOR_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading portable backup anchor {}", path.display()))?;
    if bytes.len() as u64 > MAX_PORTABLE_ANCHOR_BYTES {
        return Err(BackupSecurityError::PortableAnchorInvalid {
            reason: "portable anchor grew beyond its size limit".to_string(),
        }
        .into());
    }
    let document: PortableAnchorDocument = serde_json::from_slice(&bytes).map_err(|error| {
        BackupSecurityError::PortableAnchorInvalid {
            reason: format!("portable anchor JSON is invalid: {error}"),
        }
    })?;
    document.validate()?;
    Ok(document)
}

/// Verify the document signature against the keyring's public keys.
pub fn verify_portable_anchor(document: &PortableAnchorDocument, keyring: &Keyring) -> Result<()> {
    document.validate()?;
    let public = keyring.public_key_for_verification(&document.key_id)?;
    let payload = document.payload()?;
    verify_payload(
        &public,
        PORTABLE_ANCHOR_SIGNATURE_DOMAIN,
        &payload,
        &document.signature_bytes()?,
    )
    .map_err(|error| {
        if error
            .downcast_ref::<BackupSecurityError>()
            .is_some_and(|error| matches!(error, BackupSecurityError::SignatureInvalid { .. }))
        {
            BackupSecurityError::AnchorSignatureInvalid {
                key_id: document.key_id.clone(),
                reason: format!("{error:#}"),
            }
            .into()
        } else {
            error
        }
    })?;
    Ok(())
}

/// Read-only preview of a portable anchor against the local anchor.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortableAnchorInspectReport {
    pub anchor_path: String,
    pub key_id: String,
    pub exported_at: String,
    pub document_sha256: String,
    pub signature: String,
    /// True when the signer is revoked and the caller allowed it explicitly.
    #[serde(default)]
    pub signer_revoked: bool,
    pub stale_export: bool,
    pub local_updated_at: Option<String>,
    pub repositories_total: usize,
    pub would_raise: usize,
    pub unchanged: usize,
    pub raises: Vec<PortableAnchorRaise>,
    pub note: String,
}

/// Verify a portable anchor and preview its raise-only merge without writing.
pub fn inspect_portable_anchor(
    document: &PortableAnchorDocument,
    keyring: &Keyring,
    allow_revoked: bool,
    local: &AnchorStore,
) -> Result<PortableAnchorInspectReport> {
    let mut trusted = keyring.clone();
    if allow_revoked {
        trusted = trusted.with_revoked_keys_allowed(true);
    }
    verify_portable_anchor(document, &trusted)?;
    let anchor_present = local.path().is_file();
    let anchor = local.read()?;
    let stale_export = portable_stale_export(anchor_present, &anchor, document);
    let plan = plan_portable_merge(&anchor, document, &now_rfc3339());
    let signer_revoked = trusted.revocation_warning(&document.key_id).is_some();
    Ok(PortableAnchorInspectReport {
        anchor_path: local.path().display().to_string(),
        key_id: document.key_id.clone(),
        exported_at: document.exported_at.clone(),
        document_sha256: document.document_sha256()?,
        signature: "verified".to_string(),
        signer_revoked,
        stale_export,
        local_updated_at: anchor_present.then_some(anchor.updated_at),
        repositories_total: document.repositories.len(),
        would_raise: plan.raised_count,
        unchanged: document.repositories.len() - plan.raised_count,
        raises: plan.raises,
        note: "This is a read-only preview. Import verifies the signature again and only raises local high-water marks; it is not remote attestation, escrow, or a trusted timestamp.".to_string(),
    })
}

fn sign_portable_anchor(
    document: &mut PortableAnchorDocument,
    credential: &SigningCredential,
) -> Result<()> {
    document.key_id = credential.key_id().to_string();
    document.signature = None;
    let payload = document.payload()?;
    let signature = credential.sign(PORTABLE_ANCHOR_SIGNATURE_DOMAIN, &payload)?;
    document.signature = Some(PortableAnchorSignature {
        algorithm: SIGNING_ALGORITHM.to_string(),
        key_id: credential.key_id().to_string(),
        signature_b64: BASE64.encode(signature),
        signed_at: now_rfc3339(),
    });
    document.validate()
}

impl AnchorStore {
    /// Write a signed portable anchor document.
    ///
    /// The local anchor must contain at least one repository. `output: None`
    /// returns the document for stdout; an explicit path is written atomically
    /// with 0600 permissions and never overwrites an existing file.
    pub fn export_portable(
        &self,
        keyring: &Keyring,
        key_id: Option<&str>,
        output: Option<&Path>,
    ) -> Result<PortableAnchorExport> {
        let anchor = self.read()?;
        if anchor.repositories.is_empty() {
            return Err(BackupSecurityError::AnchorInvalid {
                reason: "local anchor has no verified repositories to export".to_string(),
            }
            .into());
        }
        let credential = keyring
            .signing_credential(key_id)
            .context("loading the backup signing credential for portable anchor export")?;
        let repositories: Vec<PortableAnchorEntry> = anchor
            .repositories
            .values()
            .map(|entry| PortableAnchorEntry {
                repository_id: entry.repository_id.clone(),
                highest_generation: entry.highest_generation,
                last_verified_generation: entry.last_verified_generation,
                last_snapshot_id: entry.last_snapshot_id.clone(),
                last_operation: Some(entry.last_operation.clone()),
                observed_at: entry.last_verified_at.clone(),
            })
            .collect();
        let mut document = PortableAnchorDocument {
            kind: PORTABLE_ANCHOR_KIND.to_string(),
            version: PORTABLE_ANCHOR_FORMAT_VERSION,
            exported_at: now_rfc3339(),
            key_id: credential.key_id().to_string(),
            repositories,
            signature: None,
        };
        sign_portable_anchor(&mut document, &credential)?;
        let mut bytes =
            serde_json::to_vec_pretty(&document).context("serializing portable backup anchor")?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_PORTABLE_ANCHOR_BYTES {
            return Err(BackupSecurityError::PortableAnchorInvalid {
                reason: "serialized portable anchor exceeds its size limit".to_string(),
            }
            .into());
        }
        if let Some(path) = output {
            write_portable_document(path, &bytes)?;
        }
        Ok(PortableAnchorExport {
            path: output.map(|path| path.display().to_string()),
            key_id: document.key_id.clone(),
            exported_at: document.exported_at.clone(),
            repository_count: document.repositories.len(),
            document_sha256: document.document_sha256()?,
            document,
            note: "Portable anchor documents contain public repository ids and generation high-water marks only. The signature proves which key exported the document; it is not remote attestation, escrow, a trusted timestamp, or proof of repository contents.".to_string(),
        })
    }

    /// Verify and merge a signed portable anchor.
    ///
    /// Merge is strictly raise-only: `highest_generation` and
    /// `last_verified_generation` can increase or stay equal, never decrease.
    /// An unsigned, tampered, wrong-key, or revoked-signer document is
    /// rejected before the local anchor is touched.
    pub fn import_portable(
        &self,
        document: &PortableAnchorDocument,
        options: PortableAnchorImportOptions<'_>,
    ) -> Result<PortableAnchorImportReport> {
        let mut keyring = options.keyring.clone();
        if options.allow_revoked {
            keyring = keyring.with_revoked_keys_allowed(true);
        }
        verify_portable_anchor(document, &keyring)?;
        let document_sha256 = document.document_sha256()?;
        let _lock = self.lock()?;
        let anchor_present = self.path.is_file();
        let mut anchor = self.read()?;
        let now = now_rfc3339();
        let stale_export = portable_stale_export(anchor_present, &anchor, document);
        if options.reject_stale && stale_export {
            return Err(BackupSecurityError::AnchorStale {
                exported_at: document.exported_at.clone(),
                local_updated_at: anchor.updated_at.clone(),
            }
            .into());
        }
        let signer_revoked = keyring.revocation_warning(&document.key_id).is_some();
        let plan = plan_portable_merge(&anchor, document, &now);
        let raises = plan.raises;
        let raised_count = plan.raised_count;
        for (repository_id, merged) in plan.merged {
            anchor.repositories.insert(repository_id, merged);
        }
        prune_anchor_entries(&mut anchor);
        anchor.updated_at = now.clone();
        anchor.validate()?;
        self.write(&anchor)?;
        let report = PortableAnchorImportReport {
            anchor_path: self.path.display().to_string(),
            key_id: document.key_id.clone(),
            exported_at: document.exported_at.clone(),
            document_sha256,
            stale_export,
            signer_revoked,
            repositories_total: raises.len(),
            repositories_raised: raised_count,
            repositories_unchanged: raises.len() - raised_count,
            raises,
            updated_at: now,
            note: "Imported high-water marks can only raise the local anchor. A fresh host can import a document to reject older repositories, but the anchor remains local state and does not provide remote attestation, escrow, or a trusted timestamp.".to_string(),
        };
        if let Err(error) = self.append_event(
            "anchor.imported",
            serde_json::json!({
                "key_id": report.key_id,
                "exported_at": report.exported_at,
                "document_sha256": report.document_sha256,
                "stale_export": report.stale_export,
                "repositories_total": report.repositories_total,
                "repositories_raised": report.repositories_raised,
            }),
        ) {
            tracing::warn!(%error, "portable anchor imported but its audit event could not be appended");
        }
        Ok(report)
    }

    /// Serialize anchor read-modify-write operations.
    ///
    /// The anchor is a single small file; without this lock, two concurrent
    /// backups of different repositories could lose one another's high-water
    /// marks. The lock is process-scoped through `fs2` and released on drop.
    fn lock(&self) -> Result<AnchorLock> {
        let parent = match self.path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        vp_store::paths::ensure_private_dir(parent)?;
        let path = parent.join(ANCHOR_LOCK_FILENAME);
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            if metadata.file_type().is_symlink() {
                return Err(BackupSecurityError::AnchorIo {
                    reason: format!("anchor lock {} is a symlink", path.display()),
                }
                .into());
            }
            if !metadata.is_file() {
                return Err(BackupSecurityError::AnchorIo {
                    reason: format!("anchor lock {} is not a regular file", path.display()),
                }
                .into());
            }
        }
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = options
            .open(&path)
            .map_err(|error| BackupSecurityError::AnchorIo {
                reason: format!("opening anchor lock {}: {error}", path.display()),
            })?;
        vp_store::paths::ensure_private_file(&path)?;
        file.lock_exclusive()
            .map_err(|error| BackupSecurityError::AnchorIo {
                reason: format!("locking anchor {}: {error}", path.display()),
            })?;
        Ok(AnchorLock { file, path })
    }

    fn append_event(&self, event: &str, details: serde_json::Value) -> Result<()> {
        let parent = match self.path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        vp_store::paths::ensure_private_dir(parent)?;
        let path = parent.join(ANCHOR_EVENTS_FILENAME);
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            if metadata.file_type().is_symlink() {
                bail!(
                    "refusing symlinked backup anchor audit log {}",
                    path.display()
                );
            }
            if !metadata.is_file() {
                bail!(
                    "backup anchor audit log {} is not a regular file",
                    path.display()
                );
            }
        }
        let record = serde_json::json!({
            "event": event,
            "created_at": now_rfc3339(),
            "details": details,
        });
        let mut line = serde_json::to_vec(&record).context("serializing backup anchor audit")?;
        line.push(b'\n');
        let mut options = OpenOptions::new();
        options.create(true).append(true).read(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options
            .open(&path)
            .with_context(|| format!("opening backup anchor audit log {}", path.display()))?;
        vp_store::paths::ensure_private_file(&path)?;
        file.write_all(&line)
            .with_context(|| format!("appending to {}", path.display()))?;
        file.sync_all()
            .with_context(|| format!("syncing {}", path.display()))?;
        Ok(())
    }
}

struct PortableMergePlan {
    raises: Vec<PortableAnchorRaise>,
    merged: Vec<(String, RepositoryAnchor)>,
    raised_count: usize,
}

fn plan_portable_merge(
    anchor: &AnchorFile,
    document: &PortableAnchorDocument,
    now: &str,
) -> PortableMergePlan {
    let mut raises = Vec::with_capacity(document.repositories.len());
    let mut merged = Vec::with_capacity(document.repositories.len());
    let mut raised_count = 0usize;
    for entry in &document.repositories {
        let previous = anchor.repositories.get(&entry.repository_id).cloned();
        let previous_highest = previous.as_ref().map(|entry| entry.highest_generation);
        let previous_last_verified = previous
            .as_ref()
            .map(|entry| entry.last_verified_generation);
        let highest = previous_highest.unwrap_or(0).max(entry.highest_generation);
        let last_verified = previous_last_verified
            .unwrap_or(0)
            .max(entry.last_verified_generation.min(highest));
        let portable_has_newer_evidence =
            entry.last_verified_generation > previous_last_verified.unwrap_or(0);
        let raised = previous_highest.is_none_or(|previous| {
            highest > previous || last_verified > previous_last_verified.unwrap_or(0)
        });
        if raised {
            raised_count += 1;
        }
        let merged_entry = RepositoryAnchor {
            repository_id: entry.repository_id.clone(),
            repository_path: previous
                .as_ref()
                .map(|entry| entry.repository_path.clone())
                .unwrap_or_else(|| "(portable import)".to_string()),
            highest_generation: highest,
            last_verified_generation: last_verified,
            last_snapshot_id: if portable_has_newer_evidence {
                entry.last_snapshot_id.clone().or_else(|| {
                    previous
                        .as_ref()
                        .and_then(|entry| entry.last_snapshot_id.clone())
                })
            } else {
                previous
                    .as_ref()
                    .and_then(|entry| entry.last_snapshot_id.clone())
                    .or_else(|| entry.last_snapshot_id.clone())
            },
            last_operation: if raised {
                "anchor_import".to_string()
            } else {
                previous
                    .as_ref()
                    .map(|entry| entry.last_operation.clone())
                    .unwrap_or_else(|| "anchor_import".to_string())
            },
            first_seen_at: previous
                .as_ref()
                .map(|entry| entry.first_seen_at.clone())
                .unwrap_or_else(|| entry.observed_at.clone()),
            last_verified_at: if raised {
                now.to_string()
            } else {
                previous
                    .as_ref()
                    .map(|entry| entry.last_verified_at.clone())
                    .unwrap_or_else(|| entry.observed_at.clone())
            },
            origin: if raised {
                Some("portable_import".to_string())
            } else {
                previous.as_ref().and_then(|entry| entry.origin.clone())
            },
        };
        raises.push(PortableAnchorRaise {
            repository_id: entry.repository_id.clone(),
            previous_highest,
            previous_last_verified,
            highest_generation: highest,
            last_verified_generation: last_verified,
            raised,
        });
        merged.push((entry.repository_id.clone(), merged_entry));
    }
    PortableMergePlan {
        raises,
        merged,
        raised_count,
    }
}

fn portable_stale_export(
    anchor_present: bool,
    anchor: &AnchorFile,
    document: &PortableAnchorDocument,
) -> bool {
    if !anchor_present {
        return false;
    }
    let local = parse_rfc3339(&anchor.updated_at);
    let exported = parse_rfc3339(&document.exported_at);
    match (exported, local) {
        (Some(exported), Some(local)) => exported < local,
        _ => false,
    }
}

struct AnchorLock {
    file: File,
    path: PathBuf,
}

impl Drop for AnchorLock {
    fn drop(&mut self) {
        if let Err(error) = self.file.unlock() {
            tracing::warn!(%error, path = %self.path.display(), "failed to release backup anchor lock");
        }
    }
}

fn parse_rfc3339(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339).ok()
}

fn write_portable_document(path: &Path, bytes: &[u8]) -> Result<()> {
    if fs::symlink_metadata(path).is_ok() {
        return Err(BackupSecurityError::PortableAnchorInvalid {
            reason: format!(
                "refusing to overwrite existing portable anchor {}",
                path.display()
            ),
        }
        .into());
    }
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    vp_store::paths::ensure_private_dir(parent)
        .with_context(|| format!("creating portable anchor directory {}", parent.display()))?;
    let scratch = super::ScratchPath::file(parent, "portable-anchor")?;
    super::write_private(&scratch.path, bytes)?;
    fs::rename(&scratch.path, path).map_err(|error| BackupSecurityError::AnchorIo {
        reason: format!("publishing portable anchor {}: {error}", path.display()),
    })?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn repo_id() -> String {
        Uuid::now_v7().to_string()
    }

    fn snapshot_id() -> String {
        Uuid::now_v7().to_string()
    }

    #[test]
    fn missing_anchor_allows_the_first_generation() {
        let dir = TempDir::new().unwrap();
        let store = AnchorStore::new(dir.path());
        let id = repo_id();
        let check = store.check(&id, 1, false).unwrap();
        assert!(check.accepted);
        assert!(!check.rollback);
        assert_eq!(check.anchor_generation, None);
        assert!(!dir.path().join(ANCHOR_FILENAME).exists());
    }

    #[test]
    fn record_then_accept_equal_and_newer_and_reject_older() {
        let dir = TempDir::new().unwrap();
        let store = AnchorStore::new(dir.path());
        let id = repo_id();
        let first = store
            .record_verified(
                &id,
                "/backups/repo",
                Some(&snapshot_id()),
                3,
                "backup",
                false,
            )
            .unwrap();
        assert_eq!(first.previous_highest, None);
        assert_eq!(first.highest_generation, 3);
        assert!(!first.rollback);
        assert!(dir.path().join(ANCHOR_FILENAME).is_file());

        let same = store.check(&id, 3, false).unwrap();
        assert!(same.accepted);
        assert_eq!(same.anchor_generation, Some(3));

        let newer = store
            .record_verified(
                &id,
                "/backups/repo",
                Some(&snapshot_id()),
                5,
                "restore",
                false,
            )
            .unwrap();
        assert_eq!(newer.previous_highest, Some(3));
        assert_eq!(newer.highest_generation, 5);

        let error = store.check(&id, 4, false).unwrap_err();
        assert!(format!("{error:#}").contains("BACKUP_ROLLBACK_DETECTED"));
        assert!(
            store
                .record_verified(&id, "/backups/repo", None, 4, "verify", false)
                .is_err()
        );
    }

    #[test]
    fn allow_rollback_does_not_lower_the_high_water_mark() {
        let dir = TempDir::new().unwrap();
        let store = AnchorStore::new(dir.path());
        let id = repo_id();
        store
            .record_verified(
                &id,
                "/backups/repo",
                Some(&snapshot_id()),
                7,
                "backup",
                false,
            )
            .unwrap();
        let allowed = store.check(&id, 4, true).unwrap();
        assert!(allowed.accepted);
        assert!(allowed.rollback);
        assert!(allowed.allow_rollback);
        assert_eq!(allowed.anchor_generation, Some(7));

        let update = store
            .record_verified(
                &id,
                "/backups/repo",
                Some(&snapshot_id()),
                4,
                "restore",
                true,
            )
            .unwrap();
        assert!(update.rollback);
        assert_eq!(update.highest_generation, 7);
        let anchor = store.read().unwrap();
        let entry = anchor.anchor(&id).unwrap();
        assert_eq!(entry.highest_generation, 7);
        assert_eq!(entry.last_verified_generation, 4);
        // A silent subsequent rollback to 5 is still rejected.
        assert!(store.check(&id, 5, false).is_err());
    }

    #[test]
    fn repositories_are_tracked_independently() {
        let dir = TempDir::new().unwrap();
        let store = AnchorStore::new(dir.path());
        let first = repo_id();
        let second = repo_id();
        store
            .record_verified(&first, "/a", None, 9, "backup", false)
            .unwrap();
        store
            .record_verified(&second, "/b", None, 1, "backup", false)
            .unwrap();
        assert!(store.check(&first, 9, false).is_ok());
        assert!(store.check(&second, 1, false).is_ok());
        assert!(store.check(&second, 0, true).is_err());
        let summary = store.summary().unwrap();
        assert!(summary.present);
        assert_eq!(summary.repositories.len(), 2);
    }

    #[test]
    fn anchor_is_private_and_written_atomically() {
        let dir = TempDir::new().unwrap();
        let store = AnchorStore::new(dir.path());
        let id = repo_id();
        store
            .record_verified(&id, "/backups/repo", None, 2, "verify", false)
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.path().join(ANCHOR_FILENAME))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        let bytes = fs::read(dir.path().join(ANCHOR_FILENAME)).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["kind"], ANCHOR_KIND);
        assert!(!dir.path().join("tmp").exists());
    }

    #[test]
    fn corrupt_insecure_or_symlinked_anchor_fails_closed() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(ANCHOR_FILENAME);
        fs::write(&path, b"not json").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let store = AnchorStore::from_file(&path);
        let error = store.read().unwrap_err();
        assert!(format!("{error:#}").contains("BACKUP_ANCHOR_INVALID"));

        fs::write(&path, serde_json::to_vec(&AnchorFile::empty()).unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        }
        let error = store.read().unwrap_err();
        assert!(format!("{error:#}").contains("0600"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            let real = dir.path().join("real-anchor.json");
            fs::rename(&path, &real).unwrap();
            symlink(&real, &path).unwrap();
            let error = store.read().unwrap_err();
            assert!(format!("{error:#}").contains("symlink"));
            let _ = PermissionsExt::mode(&fs::metadata(&real).unwrap().permissions());
        }
    }

    #[test]
    fn invalid_anchor_entries_are_rejected() {
        let mut anchor = AnchorFile::empty();
        let id = repo_id();
        anchor.repositories.insert(
            id.clone(),
            RepositoryAnchor {
                repository_id: id,
                repository_path: "/repo".to_string(),
                highest_generation: 1,
                last_verified_generation: 1,
                last_snapshot_id: None,
                last_operation: "verify".to_string(),
                first_seen_at: now_rfc3339(),
                last_verified_at: now_rfc3339(),
                origin: None,
            },
        );
        anchor.validate().unwrap();

        let mut bad = anchor.clone();
        bad.repositories
            .values_mut()
            .next()
            .unwrap()
            .last_verified_generation = 2;
        assert!(bad.validate().is_err());

        let mut bad = anchor.clone();
        bad.repositories.values_mut().next().unwrap().repository_id = repo_id();
        assert!(bad.validate().is_err());

        let mut bad = anchor.clone();
        bad.repositories.values_mut().next().unwrap().last_operation = "unknown".to_string();
        assert!(bad.validate().is_err());

        let mut bad = AnchorFile::empty();
        bad.version = 9;
        assert!(bad.validate().is_err());
    }

    #[test]
    fn anchor_reset_is_explicit_and_idempotent() {
        let dir = TempDir::new().unwrap();
        let store = AnchorStore::new(dir.path());
        assert!(!store.reset().unwrap());
        store
            .record_verified(&repo_id(), "/repo", None, 1, "backup", false)
            .unwrap();
        assert!(store.reset().unwrap());
        assert!(!store.reset().unwrap());
        assert!(!store.summary().unwrap().present);
    }

    #[test]
    fn anchor_retention_is_bounded() {
        let dir = TempDir::new().unwrap();
        let store = AnchorStore::new(dir.path());
        let mut ids = Vec::new();
        for _ in 0..(MAX_ANCHOR_REPOSITORIES + 3) {
            let id = repo_id();
            ids.push(id.clone());
            store
                .record_verified(&id, "/repo", None, 1, "backup", false)
                .unwrap();
        }
        let anchor = store.read().unwrap();
        assert_eq!(anchor.repositories.len(), MAX_ANCHOR_REPOSITORIES);
        // The most recently written entry is present.
        assert!(anchor.anchor(ids.last().unwrap()).is_some());
    }

    fn portable_keyring(dir: &TempDir, key_id: &str) -> Keyring {
        Keyring::generate_key(
            dir.path(),
            key_id,
            true,
            crate::backup::signing::SecretSource::File {
                name: format!("{key_id}.key"),
            },
        )
        .unwrap();
        Keyring::load(dir.path()).unwrap()
    }

    fn portable_source_store(dir: &TempDir, repository_id: &str, generation: u64) -> AnchorStore {
        let store = AnchorStore::new(dir.path());
        store
            .record_verified(
                repository_id,
                "/source/repository",
                None,
                generation,
                "verify",
                false,
            )
            .unwrap();
        store
    }

    #[test]
    fn portable_anchor_export_import_round_trip_raises_a_fresh_host() {
        let repo = repo_id();
        let source_dir = TempDir::new().unwrap();
        let target_dir = TempDir::new().unwrap();
        let keys = TempDir::new().unwrap();
        let keyring = portable_keyring(&keys, "primary");
        let source = portable_source_store(&source_dir, &repo, 7);

        let export = source.export_portable(&keyring, None, None).unwrap();
        assert_eq!(export.repository_count, 1);
        assert_eq!(export.key_id, "primary");
        assert_eq!(export.document.repositories[0].repository_id, repo);
        assert_eq!(export.document.repositories[0].highest_generation, 7);
        assert!(export.document.signature.is_some());
        verify_portable_anchor(&export.document, &keyring).unwrap();

        let target = AnchorStore::new(target_dir.path());
        let report = target
            .import_portable(
                &export.document,
                PortableAnchorImportOptions {
                    keyring: &keyring,
                    allow_revoked: false,
                    reject_stale: false,
                },
            )
            .unwrap();
        assert_eq!(report.repositories_total, 1);
        assert_eq!(report.repositories_raised, 1);
        assert_eq!(report.repositories_unchanged, 0);
        assert!(!report.stale_export);
        assert!(report.raises[0].raised);
        assert_eq!(report.raises[0].previous_highest, None);
        let anchor = target.read().unwrap();
        let entry = anchor.anchor(&repo).unwrap();
        assert_eq!(entry.highest_generation, 7);
        assert_eq!(entry.last_verified_generation, 7);
        assert_eq!(entry.last_operation, "anchor_import");
        assert_eq!(entry.origin.as_deref(), Some("portable_import"));
        assert_eq!(entry.repository_path, "(portable import)");
        assert_eq!(target.summary().unwrap().imported_repositories, 1);
        let audit = fs::read_to_string(target_dir.path().join(ANCHOR_EVENTS_FILENAME)).unwrap();
        assert!(audit.contains("anchor.imported"), "{audit}");
        assert!(audit.contains(&export.document_sha256), "{audit}");
    }

    #[test]
    fn portable_anchor_export_writes_a_private_file_and_refuses_overwrite() {
        let repo = repo_id();
        let source_dir = TempDir::new().unwrap();
        let keys = TempDir::new().unwrap();
        let keyring = portable_keyring(&keys, "primary");
        let source = portable_source_store(&source_dir, &repo, 2);
        let output = source_dir.path().join(PORTABLE_ANCHOR_FILENAME);
        let export = source
            .export_portable(&keyring, None, Some(&output))
            .unwrap();
        assert_eq!(export.path.as_deref(), Some(output.to_str().unwrap()));
        let loaded = load_portable_anchor(&output).unwrap();
        assert_eq!(loaded, export.document);
        verify_portable_anchor(&loaded, &keyring).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&output).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let error = source
            .export_portable(&keyring, None, Some(&output))
            .unwrap_err();
        assert!(format!("{error:#}").contains("refusing to overwrite"));
    }

    #[test]
    fn portable_anchor_import_only_raises_local_high_water_marks() {
        let repo = repo_id();
        let local_dir = TempDir::new().unwrap();
        let keys = TempDir::new().unwrap();
        let keyring = portable_keyring(&keys, "primary");
        let local = portable_source_store(&local_dir, &repo, 10);
        assert_eq!(
            local
                .read()
                .unwrap()
                .anchor(&repo)
                .unwrap()
                .highest_generation,
            10
        );

        let old_dir = TempDir::new().unwrap();
        let old = portable_source_store(&old_dir, &repo, 4);
        let old_doc = old.export_portable(&keyring, None, None).unwrap();
        let report = local
            .import_portable(
                &old_doc.document,
                PortableAnchorImportOptions {
                    keyring: &keyring,
                    allow_revoked: false,
                    reject_stale: false,
                },
            )
            .unwrap();
        assert_eq!(report.repositories_raised, 0);
        assert_eq!(report.repositories_unchanged, 1);
        assert!(!report.raises[0].raised);
        assert_eq!(report.raises[0].previous_highest, Some(10));
        assert_eq!(report.raises[0].highest_generation, 10);
        let entry = local.read().unwrap().anchor(&repo).unwrap().clone();
        assert_eq!(entry.highest_generation, 10);
        assert_eq!(entry.last_verified_generation, 10);
        assert_eq!(entry.last_operation, "verify");

        let newer_dir = TempDir::new().unwrap();
        let newer = portable_source_store(&newer_dir, &repo, 12);
        let newer_doc = newer.export_portable(&keyring, None, None).unwrap();
        let report = local
            .import_portable(
                &newer_doc.document,
                PortableAnchorImportOptions {
                    keyring: &keyring,
                    allow_revoked: false,
                    reject_stale: false,
                },
            )
            .unwrap();
        assert_eq!(report.repositories_raised, 1);
        let entry = local.read().unwrap().anchor(&repo).unwrap().clone();
        assert_eq!(entry.highest_generation, 12);
        assert_eq!(entry.last_verified_generation, 12);
        assert_eq!(entry.last_operation, "anchor_import");
    }

    #[test]
    fn portable_anchor_tamper_wrong_key_and_revoked_signer_are_typed() {
        let repo = repo_id();
        let source_dir = TempDir::new().unwrap();
        let keys = TempDir::new().unwrap();
        let keyring = portable_keyring(&keys, "primary");
        let source = portable_source_store(&source_dir, &repo, 6);
        let export = source.export_portable(&keyring, None, None).unwrap();
        let target_dir = TempDir::new().unwrap();
        let target = AnchorStore::new(target_dir.path());

        let mut tampered = export.document.clone();
        tampered.repositories[0].highest_generation += 1;
        let error = target
            .import_portable(
                &tampered,
                PortableAnchorImportOptions {
                    keyring: &keyring,
                    allow_revoked: false,
                    reject_stale: false,
                },
            )
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("BACKUP_ANCHOR_SIGNATURE_INVALID"),
            "{error:#}"
        );

        let other_keys = TempDir::new().unwrap();
        let other = portable_keyring(&other_keys, "other");
        let error = target
            .import_portable(
                &export.document,
                PortableAnchorImportOptions {
                    keyring: &other,
                    allow_revoked: false,
                    reject_stale: false,
                },
            )
            .unwrap_err();
        assert!(format!("{error:#}").contains("BACKUP_KEY_NOT_FOUND"));

        // Rotate past the exporting key and revoke it. The document signature is
        // still cryptographically valid, but it is untrusted by default.
        Keyring::generate_key(
            keys.path(),
            "replacement",
            false,
            crate::backup::signing::SecretSource::File {
                name: "replacement.key".to_string(),
            },
        )
        .unwrap();
        let mut rotated = Keyring::load(keys.path()).unwrap();
        rotated.rotate_to("replacement").unwrap();
        rotated.revoke_key("primary", false).unwrap();
        let revoked_keyring = Keyring::load(keys.path()).unwrap();
        let error = target
            .import_portable(
                &export.document,
                PortableAnchorImportOptions {
                    keyring: &revoked_keyring,
                    allow_revoked: false,
                    reject_stale: false,
                },
            )
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("BACKUP_KEY_REVOKED"),
            "{error:#}"
        );
        let report = target
            .import_portable(
                &export.document,
                PortableAnchorImportOptions {
                    keyring: &revoked_keyring,
                    allow_revoked: true,
                    reject_stale: false,
                },
            )
            .unwrap();
        assert_eq!(report.repositories_raised, 1);
        assert!(report.signer_revoked);
    }

    #[test]
    fn portable_anchor_stale_exports_are_raise_only_or_rejected_explicitly() {
        let repo = repo_id();
        let source_dir = TempDir::new().unwrap();
        let target_dir = TempDir::new().unwrap();
        let keys = TempDir::new().unwrap();
        let keyring = portable_keyring(&keys, "primary");
        let source = portable_source_store(&source_dir, &repo, 5);
        let export = source.export_portable(&keyring, None, None).unwrap();

        // Re-sign a deliberately old export with the same key.
        let credential = keyring.signing_credential(None).unwrap();
        let mut stale = export.document.clone();
        stale.exported_at = "2020-01-01T00:00:00Z".to_string();
        stale.signature = None;
        let payload = stale.payload().unwrap();
        let signature = credential
            .sign(
                super::super::signing::PORTABLE_ANCHOR_SIGNATURE_DOMAIN,
                &payload,
            )
            .unwrap();
        stale.signature = Some(PortableAnchorSignature {
            algorithm: "ed25519".to_string(),
            key_id: "primary".to_string(),
            signature_b64: BASE64.encode(signature),
            signed_at: now_rfc3339(),
        });
        stale.validate().unwrap();

        let target = AnchorStore::new(target_dir.path());
        // An existing local anchor is what makes an old export detectable.
        target
            .record_verified(&repo_id(), "/local", None, 1, "verify", false)
            .unwrap();
        let error = target
            .import_portable(
                &stale,
                PortableAnchorImportOptions {
                    keyring: &keyring,
                    allow_revoked: false,
                    reject_stale: true,
                },
            )
            .unwrap_err();
        assert!(format!("{error:#}").contains("BACKUP_ANCHOR_STALE"));
        let report = target
            .import_portable(
                &stale,
                PortableAnchorImportOptions {
                    keyring: &keyring,
                    allow_revoked: false,
                    reject_stale: false,
                },
            )
            .unwrap();
        assert!(report.stale_export);
        assert_eq!(report.repositories_raised, 1);
    }

    #[test]
    fn portable_anchor_validation_rejects_malformed_documents() {
        let repo = repo_id();
        let source_dir = TempDir::new().unwrap();
        let keys = TempDir::new().unwrap();
        let keyring = portable_keyring(&keys, "primary");
        let source = portable_source_store(&source_dir, &repo, 3);
        let export = source.export_portable(&keyring, None, None).unwrap();
        let valid = export.document;

        let mut duplicate = valid.clone();
        duplicate
            .repositories
            .push(duplicate.repositories[0].clone());
        assert!(duplicate.validate().is_err());

        let mut zero = valid.clone();
        zero.repositories[0].highest_generation = 0;
        assert!(zero.validate().is_err());

        let mut range = valid.clone();
        range.repositories[0].last_verified_generation = 9;
        assert!(range.validate().is_err());

        let mut mismatch = valid.clone();
        mismatch.key_id = "different".to_string();
        assert!(mismatch.validate().is_err());

        let mut unsigned = valid.clone();
        unsigned.signature = None;
        assert!(verify_portable_anchor(&unsigned, &keyring).is_err());

        let mut bad_base64 = valid.clone();
        bad_base64.signature.as_mut().unwrap().signature_b64 = "not-base64".to_string();
        assert!(bad_base64.validate().is_err());

        let mut bad_version = valid.clone();
        bad_version.version = 9;
        assert!(bad_version.validate().is_err());

        let mut no_repositories = valid.clone();
        no_repositories.repositories.clear();
        assert!(no_repositories.validate().is_err());

        let mut bad_timestamp = valid.clone();
        bad_timestamp.exported_at = "yesterday".to_string();
        assert!(bad_timestamp.validate().is_err());
    }

    #[test]
    fn portable_anchor_documents_never_contain_private_key_material() {
        let repo = repo_id();
        let source_dir = TempDir::new().unwrap();
        let keys = TempDir::new().unwrap();
        let keyring = portable_keyring(&keys, "primary");
        let source = portable_source_store(&source_dir, &repo, 1);
        let export = source.export_portable(&keyring, None, None).unwrap();
        let key_text = fs::read_to_string(keys.path().join("primary.key")).unwrap();
        let key_json: serde_json::Value = serde_json::from_str(&key_text).unwrap();
        let seed_b64 = key_json["seed_b64"].as_str().unwrap();
        let rendered = serde_json::to_string(&export).unwrap();
        assert!(!rendered.contains(seed_b64));
        assert!(!rendered.contains("seed_b64"));
        assert!(!rendered.contains("private"));
        let document = serde_json::to_string(&export.document).unwrap();
        assert!(!document.contains(seed_b64));
    }

    #[test]
    fn portable_anchor_handles_multiple_repositories_and_verified_only_raises() {
        let first = repo_id();
        let second = repo_id();
        let source_dir = TempDir::new().unwrap();
        let keys = TempDir::new().unwrap();
        let keyring = portable_keyring(&keys, "primary");
        let source = AnchorStore::new(source_dir.path());
        source
            .record_verified(&first, "/a", None, 3, "verify", false)
            .unwrap();
        source
            .record_verified(&second, "/b", None, 8, "verify", false)
            .unwrap();
        let export = source.export_portable(&keyring, None, None).unwrap();
        assert_eq!(export.document.repositories.len(), 2);
        let document_json = serde_json::to_string(&export.document).unwrap();
        assert!(!document_json.contains("repository_path"));

        let target_dir = TempDir::new().unwrap();
        let target = AnchorStore::new(target_dir.path());
        // Seed a local row whose highest is current but whose last verified
        // generation is older. An import may raise only the verified value.
        target
            .record_verified(&first, "/local/a", None, 3, "verify", false)
            .unwrap();
        target
            .record_verified(&first, "/local/a", None, 2, "restore", true)
            .unwrap();
        assert_eq!(
            target
                .read()
                .unwrap()
                .anchor(&first)
                .unwrap()
                .last_verified_generation,
            2
        );
        let report = target
            .import_portable(
                &export.document,
                PortableAnchorImportOptions {
                    keyring: &keyring,
                    allow_revoked: false,
                    reject_stale: false,
                },
            )
            .unwrap();
        assert_eq!(report.repositories_total, 2);
        assert_eq!(report.repositories_raised, 2);
        let anchor = target.read().unwrap();
        let raised = anchor.anchor(&first).unwrap();
        assert_eq!(raised.highest_generation, 3);
        assert_eq!(raised.last_verified_generation, 3);
        // The local path is preserved for an already-known repository.
        assert_eq!(raised.repository_path, "/local/a");
        let added = anchor.anchor(&second).unwrap();
        assert_eq!(added.highest_generation, 8);
        assert_eq!(added.repository_path, "(portable import)");
        assert_eq!(added.origin.as_deref(), Some("portable_import"));

        // A local verification supersedes portable provenance.
        target
            .record_verified(&second, "/local/b", None, 8, "verify", false)
            .unwrap();
        assert!(
            target
                .read()
                .unwrap()
                .anchor(&second)
                .unwrap()
                .origin
                .is_none()
        );
    }

    #[test]
    fn portable_anchor_rejects_documents_above_the_repository_bound() {
        let repo = repo_id();
        let source_dir = TempDir::new().unwrap();
        let keys = TempDir::new().unwrap();
        let keyring = portable_keyring(&keys, "primary");
        let source = portable_source_store(&source_dir, &repo, 1);
        let export = source.export_portable(&keyring, None, None).unwrap();
        let mut oversized = export.document.clone();
        for _ in 0..MAX_PORTABLE_ANCHOR_REPOSITORIES {
            let id = repo_id();
            oversized.repositories.push(PortableAnchorEntry {
                repository_id: id,
                highest_generation: 1,
                last_verified_generation: 1,
                last_snapshot_id: None,
                last_operation: Some("verify".to_string()),
                observed_at: now_rfc3339(),
            });
        }
        let error = oversized.validate().unwrap_err();
        assert!(format!("{error:#}").contains("more than"), "{error:#}");
    }

    #[test]
    fn concurrent_anchor_writers_do_not_lose_high_water_marks() {
        use std::sync::{Arc, Barrier};

        let dir = TempDir::new().unwrap();
        let store = AnchorStore::new(dir.path());
        let barrier = Arc::new(Barrier::new(8));
        let mut threads = Vec::new();
        for index in 0..8u64 {
            let store = store.clone();
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                let id = Uuid::now_v7().to_string();
                barrier.wait();
                store
                    .record_verified(&id, "/backups/repo", None, index + 1, "verify", false)
                    .unwrap();
                id
            }));
        }
        let mut ids = Vec::new();
        for thread in threads {
            ids.push(thread.join().unwrap());
        }
        let anchor = store.read().unwrap();
        assert_eq!(anchor.repositories.len(), 8);
        for id in ids {
            assert!(anchor.anchor(&id).is_some(), "lost anchor row {id}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.path().join(ANCHOR_LOCK_FILENAME))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn portable_anchor_inspect_previews_raise_only_merge_without_writing() {
        let repo = repo_id();
        let source_dir = TempDir::new().unwrap();
        let target_dir = TempDir::new().unwrap();
        let keys = TempDir::new().unwrap();
        let keyring = portable_keyring(&keys, "primary");
        let target = portable_source_store(&target_dir, &repo, 3);
        let source = portable_source_store(&source_dir, &repo, 5);
        let export = source.export_portable(&keyring, None, None).unwrap();

        let inspect = inspect_portable_anchor(&export.document, &keyring, false, &target).unwrap();
        assert_eq!(inspect.signature, "verified");
        assert!(!inspect.signer_revoked);
        assert!(!inspect.stale_export);
        assert_eq!(inspect.repositories_total, 1);
        assert_eq!(inspect.would_raise, 1);
        assert_eq!(inspect.unchanged, 0);
        assert_eq!(inspect.raises[0].previous_highest, Some(3));
        assert_eq!(inspect.raises[0].highest_generation, 5);
        assert!(inspect.raises[0].raised);
        // The preview must not have written the higher generation.
        assert_eq!(
            target
                .read()
                .unwrap()
                .anchor(&repo)
                .unwrap()
                .highest_generation,
            3
        );

        target
            .import_portable(
                &export.document,
                PortableAnchorImportOptions {
                    keyring: &keyring,
                    allow_revoked: false,
                    reject_stale: false,
                },
            )
            .unwrap();
        let after = inspect_portable_anchor(&export.document, &keyring, false, &target).unwrap();
        assert_eq!(after.would_raise, 0);
        assert_eq!(after.unchanged, 1);
        assert!(!after.raises[0].raised);

        let mut stale = export.document.clone();
        let credential = keyring.signing_credential(None).unwrap();
        stale.exported_at = "2020-01-01T00:00:00Z".to_string();
        stale.signature = None;
        let payload = stale.payload().unwrap();
        let signature = credential
            .sign(
                crate::backup::signing::PORTABLE_ANCHOR_SIGNATURE_DOMAIN,
                &payload,
            )
            .unwrap();
        stale.signature = Some(PortableAnchorSignature {
            algorithm: "ed25519".to_string(),
            key_id: "primary".to_string(),
            signature_b64: BASE64.encode(signature),
            signed_at: now_rfc3339(),
        });
        let stale_report = inspect_portable_anchor(&stale, &keyring, false, &target).unwrap();
        assert!(stale_report.stale_export);
    }

    #[test]
    fn portable_anchor_import_fails_closed_when_the_lock_is_obstructed() {
        let repo = repo_id();
        let source_dir = TempDir::new().unwrap();
        let target_dir = TempDir::new().unwrap();
        let keys = TempDir::new().unwrap();
        let keyring = portable_keyring(&keys, "primary");
        let source = portable_source_store(&source_dir, &repo, 9);
        let export = source.export_portable(&keyring, None, None).unwrap();
        let target = portable_source_store(&target_dir, &repo, 1);
        let anchor_path = target_dir.path().join(ANCHOR_FILENAME);
        let lock_path = target_dir.path().join(ANCHOR_LOCK_FILENAME);
        fs::remove_file(&lock_path).unwrap();
        fs::create_dir(&lock_path).unwrap();

        let result = target.import_portable(
            &export.document,
            PortableAnchorImportOptions {
                keyring: &keyring,
                allow_revoked: false,
                reject_stale: false,
            },
        );
        assert!(result.is_err());
        let anchor = target.read().unwrap();
        assert_eq!(anchor.anchor(&repo).unwrap().highest_generation, 1);
        assert!(anchor_path.is_file());
    }

    #[test]
    fn anchor_lock_rejects_a_symlink_and_legacy_rows_without_origin_load() {
        let dir = TempDir::new().unwrap();
        let store = AnchorStore::new(dir.path());
        let id = repo_id();
        store
            .record_verified(&id, "/repo", None, 1, "verify", false)
            .unwrap();

        // Remove the optional origin field to model an anchor written by an
        // older build; it must load and keep working.
        let path = dir.path().join(ANCHOR_FILENAME);
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["repositories"][&id]
            .as_object_mut()
            .unwrap()
            .remove("origin");
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let loaded = store.read().unwrap();
        assert!(loaded.anchor(&id).unwrap().origin.is_none());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            use std::os::unix::fs::symlink;
            let lock = dir.path().join(ANCHOR_LOCK_FILENAME);
            let _ = fs::remove_file(&lock);
            let target = dir.path().join("real.lock");
            fs::write(&target, b"").unwrap();
            fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
            symlink(&target, &lock).unwrap();
            let error = store
                .record_verified(&repo_id(), "/repo", None, 2, "verify", false)
                .unwrap_err();
            assert!(format!("{error:#}").contains("symlink"), "{error:#}");
        }
    }

    #[test]
    fn portable_anchor_export_refuses_an_empty_local_anchor() {
        let dir = TempDir::new().unwrap();
        let keys = TempDir::new().unwrap();
        let keyring = portable_keyring(&keys, "primary");
        let store = AnchorStore::new(dir.path());
        let error = store.export_portable(&keyring, None, None).unwrap_err();
        assert!(
            format!("{error:#}").contains("no verified repositories"),
            "{error:#}"
        );
    }

    #[test]
    fn portable_anchor_export_refuses_an_existing_or_symlinked_output() {
        let repo = repo_id();
        let source_dir = TempDir::new().unwrap();
        let keys = TempDir::new().unwrap();
        let keyring = portable_keyring(&keys, "primary");
        let source = portable_source_store(&source_dir, &repo, 1);
        let output = source_dir.path().join("existing.json");
        fs::write(&output, b"keep").unwrap();
        let error = source
            .export_portable(&keyring, None, Some(&output))
            .unwrap_err();
        assert!(format!("{error:#}").contains("refusing to overwrite"));
        assert_eq!(fs::read(&output).unwrap(), b"keep");

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let real = source_dir.path().join("real-output.json");
            fs::write(&real, b"real").unwrap();
            let link = source_dir.path().join("link-output.json");
            symlink(&real, &link).unwrap();
            let error = source
                .export_portable(&keyring, None, Some(&link))
                .unwrap_err();
            assert!(format!("{error:#}").contains("refusing to overwrite"));
            assert_eq!(fs::read(&real).unwrap(), b"real");
        }
    }
}
