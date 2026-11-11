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
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use vp_protocol::now_rfc3339;

use super::error::BackupSecurityError;

pub const ANCHOR_FILENAME: &str = "backup-anchor.json";
pub const ANCHOR_KIND: &str = "verlinpark-backup-anchor";
pub const ANCHOR_FORMAT_VERSION: u32 = 1;
pub const MAX_ANCHOR_BYTES: u64 = 256 * 1024;
pub const MAX_ANCHOR_REPOSITORIES: usize = 64;
pub const MAX_ANCHOR_PATH_BYTES: usize = 4096;

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
                "backup" | "verify" | "restore" => {}
                other => {
                    return Err(BackupSecurityError::AnchorInvalid {
                        reason: format!("anchor operation {other:?} is not recognised"),
                    }
                    .into());
                }
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
        Ok(AnchorSummary {
            path: self.path.display().to_string(),
            present,
            updated_at: present.then_some(anchor.updated_at),
            repositories,
        })
    }

    /// Remove the anchor file. Returns whether a file was removed.
    ///
    /// This is an explicit operator action: removing the anchor discards local
    /// rollback protection until the next backup/verify/restore records a new
    /// high-water mark.
    pub fn reset(&self) -> Result<bool> {
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
}
