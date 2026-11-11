//! On-disk types and validation for the v2 backup repository.
//!
//! Everything in this module is deterministic and offline. The repository has
//! no daemon RPC surface: a snapshot is written by a CLI process, and restore
//! requires the daemon to be stopped exactly as the v1 ZIP path does.
//!
//! # Layout
//!
//! ```text
//! repository/
//!   repository.json          repository metadata (format, encryption, KDF,
//!                            prune tombstones, optional metadata MAC)
//!   snapshots/<uuid>.manifest authenticated snapshot manifest envelope
//!   blobs/<sha256>.blob       content-addressed sealed file envelopes
//!   nonces.jsonl              append-only audit table of random nonces
//!   events.jsonl              append-only created/pruned operations log
//!   repository.lock           advisory mutation/read lock
//!   tmp/                      private staging for atomic publication
//! ```
//!
//! Snapshot manifests are immutable and include the complete file list, so a
//! snapshot does not need its parent to restore. The parent chain records
//! incremental provenance and is validated strictly; a parent may only be
//! absent after a recorded prune tombstone. This is what lets prune remove old
//! snapshots while keeping the retained snapshots restorable and auditable.
//!
//! All paths listed in a manifest are revalidated through the v1 data-directory
//! allowlist (`verlinpark.db`, `projects.jsonl`,
//! `sessions/<id>/events.jsonl`, and optional `artifacts/blobs/<name>`). Blob
//! names must be lowercase SHA-256 hex plus `.blob`; snapshot names must be
//! canonical lowercase UUIDs plus `.manifest`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use vp_protocol::{APP_VERSION, PROTOCOL_VERSION, now_rfc3339};

use super::blob::{
    BLOB_CHUNK_RECORD_BYTES, BLOB_HEADER_BYTES, BLOB_MAX_PLAINTEXT_BYTES, Compression,
};
use crate::backup::crypto::{
    KdfParameters, KdfSummary, MAX_MANIFEST_PLAINTEXT_BYTES, RepositoryCrypto, sha256_hex,
};
use crate::backup::{
    ArchiveKind, DATABASE_FILENAME, ScratchPath, classify_archive_path, write_private,
};

pub(crate) const REPOSITORY_FORMAT_VERSION: u32 = 2;
pub(crate) const SNAPSHOT_FORMAT_VERSION: u32 = 1;
pub(crate) const REPOSITORY_KIND: &str = "verlinpark-backup-repository";
pub(crate) const SNAPSHOT_KIND: &str = "verlinpark-backup-snapshot";
pub(crate) const METADATA_FILENAME: &str = "repository.json";
pub(crate) const SNAPSHOTS_DIR: &str = "snapshots";
pub(crate) const BLOBS_DIR: &str = "blobs";
pub(crate) const NONCES_FILENAME: &str = "nonces.jsonl";
pub(crate) const EVENTS_FILENAME: &str = "events.jsonl";
pub(crate) const LOCK_FILENAME: &str = "repository.lock";
pub(crate) const TEMP_DIR: &str = "tmp";
pub(crate) const MANIFEST_SUFFIX: &str = ".manifest";
pub(crate) const BLOB_SUFFIX: &str = ".blob";
pub(crate) const MAX_METADATA_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const MAX_SNAPSHOT_ENVELOPE_BYTES: u64 = MAX_MANIFEST_PLAINTEXT_BYTES as u64 + 1024;
pub(crate) const MAX_SNAPSHOT_ENTRIES: usize = 100_000;
pub(crate) const MAX_CHAIN_LENGTH: usize = 100_000;
pub(crate) const MAX_BLOB_STORED_BYTES: u64 = 1 << 42;
pub(crate) const MAX_KEEP: usize = 100_000;

/// Repository metadata, persisted as `repository.json`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RepositoryMetadata {
    pub kind: String,
    pub format_version: u32,
    pub repository_id: String,
    pub created_at: String,
    pub updated_at: String,
    pub app_version: String,
    pub protocol_version: u32,
    pub encryption: EncryptionMetadata,
    #[serde(default)]
    pub pruned_snapshots: Vec<PrunedSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_mac: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct EncryptionMetadata {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kdf: Option<KdfParameters>,
}

impl EncryptionMetadata {
    pub(crate) fn encrypted(kdf: KdfParameters) -> Self {
        Self {
            enabled: true,
            kdf: Some(kdf),
        }
    }

    pub(crate) fn plaintext() -> Self {
        Self {
            enabled: false,
            kdf: None,
        }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        match (self.enabled, &self.kdf) {
            (true, Some(kdf)) => kdf.validate(),
            (true, None) => bail!("encrypted backup repository is missing KDF parameters"),
            (false, None) => Ok(()),
            (false, Some(_)) => {
                bail!("unencrypted backup repository must not persist KDF parameters")
            }
        }
    }

    pub(crate) fn summary(&self) -> Option<KdfSummary> {
        self.kdf.as_ref().map(KdfParameters::summary)
    }
}

/// A snapshot removed by prune. Tombstones are authenticated by the repository
/// metadata MAC in encrypted repositories and are recorded before files are
/// deleted, so a missing parent is only accepted when it was intentionally
/// pruned.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PrunedSnapshot {
    pub id: String,
    pub sequence: u64,
    pub created_at: String,
    pub pruned_at: String,
    pub manifest_sha256: String,
}

/// A snapshot manifest before its JSON body is sealed into an envelope.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnapshotManifest {
    pub kind: String,
    pub format_version: u32,
    pub snapshot_id: String,
    pub sequence: u64,
    pub parent: Option<String>,
    pub incremental: bool,
    pub created_at: String,
    pub app_version: String,
    pub protocol_version: u32,
    pub encrypted: bool,
    pub includes_artifacts: bool,
    pub kdf_fingerprint: Option<String>,
    pub entries: Vec<SnapshotEntry>,
    pub totals: SnapshotTotals,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnapshotEntry {
    /// POSIX-style path relative to the data directory.
    pub path: String,
    /// Original plaintext size.
    pub size: u64,
    /// Lowercase SHA-256 of original plaintext. Also the blob address.
    pub sha256: String,
    /// Compression of the stored blob envelope.
    pub compression: Compression,
    /// Size of the sealed blob file on disk.
    pub stored_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnapshotTotals {
    pub entries: u64,
    pub bytes: u64,
}

/// A manifest loaded and validated from disk.
#[derive(Clone, Debug)]
pub(crate) struct ValidatedSnapshot {
    pub manifest: SnapshotManifest,
    pub manifest_sha256: String,
    pub manifest_envelope_bytes: u64,
    pub encrypted: bool,
}

impl ValidatedSnapshot {
    pub(crate) fn id(&self) -> &str {
        &self.manifest.snapshot_id
    }

    pub(crate) fn sequence(&self) -> u64 {
        self.manifest.sequence
    }

    pub(crate) fn parent(&self) -> Option<&str> {
        self.manifest.parent.as_deref()
    }

    pub(crate) fn sessions(&self) -> usize {
        self.manifest
            .entries
            .iter()
            .filter(|entry| matches!(classify_archive_path(&entry.path), Ok(ArchiveKind::Session)))
            .count()
    }

    pub(crate) fn artifacts(&self) -> usize {
        self.manifest
            .entries
            .iter()
            .filter(|entry| {
                matches!(
                    classify_archive_path(&entry.path),
                    Ok(ArchiveKind::Artifact)
                )
            })
            .count()
    }

    pub(crate) fn blob_hashes(&self) -> BTreeSet<String> {
        self.manifest
            .entries
            .iter()
            .map(|entry| entry.sha256.clone())
            .collect()
    }
}

pub(crate) fn new_metadata(encryption: EncryptionMetadata) -> RepositoryMetadata {
    let now = now_rfc3339();
    RepositoryMetadata {
        kind: REPOSITORY_KIND.to_string(),
        format_version: REPOSITORY_FORMAT_VERSION,
        repository_id: Uuid::now_v7().to_string(),
        created_at: now.clone(),
        updated_at: now,
        app_version: APP_VERSION.to_string(),
        protocol_version: PROTOCOL_VERSION,
        encryption,
        pruned_snapshots: Vec::new(),
        metadata_mac: None,
    }
}

impl RepositoryMetadata {
    pub(crate) fn validate_structure(&self) -> Result<()> {
        if self.kind != REPOSITORY_KIND {
            bail!("not a VerlinPark backup repository (kind {:?})", self.kind);
        }
        if self.format_version != REPOSITORY_FORMAT_VERSION {
            bail!(
                "unsupported backup repository format version {}; this build supports {}",
                self.format_version,
                REPOSITORY_FORMAT_VERSION
            );
        }
        if !is_snapshot_id(&self.repository_id) {
            bail!("backup repository id is not a canonical UUID");
        }
        if self.created_at.is_empty() || self.created_at.len() > 64 {
            bail!("backup repository created_at is malformed");
        }
        if self.updated_at.is_empty() || self.updated_at.len() > 64 {
            bail!("backup repository updated_at is malformed");
        }
        if self.protocol_version == 0 {
            bail!("backup repository protocol version must be positive");
        }
        self.encryption.validate()?;
        if let Some(mac) = &self.metadata_mac
            && !is_sha256_hex(mac)
        {
            bail!("backup repository metadata MAC is not a SHA-256 hex digest");
        }
        if self.pruned_snapshots.len() > MAX_CHAIN_LENGTH {
            bail!("backup repository has too many prune tombstones");
        }
        let mut ids = BTreeSet::new();
        let mut sequences = BTreeSet::new();
        for tombstone in &self.pruned_snapshots {
            if !is_snapshot_id(&tombstone.id) {
                bail!("backup repository has an invalid pruned snapshot id");
            }
            if !ids.insert(tombstone.id.clone()) {
                bail!("backup repository has duplicate prune tombstones");
            }
            if !sequences.insert(tombstone.sequence) {
                bail!("backup repository has duplicate prune sequences");
            }
            if tombstone.sequence == 0 {
                bail!("backup repository has a zero prune sequence");
            }
            if tombstone.created_at.is_empty()
                || tombstone.created_at.len() > 64
                || tombstone.pruned_at.is_empty()
                || tombstone.pruned_at.len() > 64
            {
                bail!("backup repository has malformed prune timestamps");
            }
            if !is_sha256_hex(&tombstone.manifest_sha256) {
                bail!("backup repository prune tombstone has an invalid manifest hash");
            }
        }
        if !self.encryption.enabled && self.metadata_mac.is_some() {
            bail!("unencrypted backup repository must not have a metadata MAC");
        }
        Ok(())
    }

    pub(crate) fn is_pruned(&self, id: &str) -> Option<&PrunedSnapshot> {
        self.pruned_snapshots
            .iter()
            .find(|tombstone| tombstone.id == id)
    }
}

/// Canonical bytes authenticated by the metadata MAC.
///
/// The MAC covers the logical JSON representation with the MAC field absent,
/// not the on-disk whitespace. This keeps pretty-printed metadata readable
/// without weakening the check.
pub(crate) fn canonical_metadata_bytes(metadata: &RepositoryMetadata) -> Result<Vec<u8>> {
    let mut copy = metadata.clone();
    copy.metadata_mac = None;
    serde_json::to_vec(&copy).context("serializing backup repository metadata")
}

pub(crate) fn sign_metadata(
    metadata: &mut RepositoryMetadata,
    crypto: Option<&RepositoryCrypto>,
) -> Result<()> {
    match (metadata.encryption.enabled, crypto) {
        (true, Some(crypto)) => {
            let canonical = canonical_metadata_bytes(metadata)?;
            metadata.metadata_mac = Some(crypto.metadata_mac(&canonical));
        }
        (false, None) => metadata.metadata_mac = None,
        _ => bail!("backup repository encryption mode and key are inconsistent"),
    }
    Ok(())
}

pub(crate) fn verify_metadata_authenticity(
    metadata: &RepositoryMetadata,
    crypto: Option<&RepositoryCrypto>,
) -> Result<()> {
    if metadata.encryption.enabled {
        let Some(crypto) = crypto else {
            bail!("encrypted backup repository is missing its key");
        };
        let Some(mac) = &metadata.metadata_mac else {
            bail!("encrypted backup repository metadata is missing its authentication MAC");
        };
        let canonical = canonical_metadata_bytes(metadata)?;
        crypto.verify_metadata_mac(&canonical, mac)?;
    } else if metadata.metadata_mac.is_some() {
        bail!("unencrypted backup repository metadata unexpectedly has a MAC");
    }
    Ok(())
}

pub(crate) fn read_metadata_bytes(repository: &Path) -> Result<Vec<u8>> {
    let path = repository.join(METADATA_FILENAME);
    let metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("reading backup repository metadata {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!(
            "refusing symlinked backup repository metadata {}",
            path.display()
        );
    }
    if !metadata.is_file() {
        bail!(
            "backup repository metadata {} is not a regular file",
            path.display()
        );
    }
    if metadata.len() > MAX_METADATA_BYTES {
        bail!(
            "backup repository metadata is {} bytes; the limit is {}",
            metadata.len(),
            MAX_METADATA_BYTES
        );
    }
    let file = File::open(&path).with_context(|| format!("opening {}", path.display()))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        bail!("backup repository metadata grew beyond its limit");
    }
    Ok(bytes)
}

pub(crate) fn parse_metadata(bytes: &[u8]) -> Result<RepositoryMetadata> {
    let metadata: RepositoryMetadata =
        serde_json::from_slice(bytes).context("parsing backup repository metadata")?;
    metadata.validate_structure()?;
    Ok(metadata)
}

pub(crate) fn write_metadata_atomic(
    repository: &Path,
    metadata: &RepositoryMetadata,
) -> Result<()> {
    metadata.validate_structure()?;
    let mut bytes =
        serde_json::to_vec_pretty(metadata).context("serializing backup repository metadata")?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        bail!("backup repository metadata exceeds its size limit");
    }
    bytes.push(b'\n');
    let mut scratch = ScratchPath::file(repository, "metadata")?;
    write_private(&scratch.path, &bytes)?;
    let final_path = repository.join(METADATA_FILENAME);
    fs::rename(&scratch.path, &final_path).with_context(|| {
        format!(
            "publishing backup repository metadata {}",
            final_path.display()
        )
    })?;
    scratch.disarm();
    fs::File::open(repository)
        .and_then(|directory| directory.sync_all())
        .ok();
    Ok(())
}

pub(crate) fn snapshot_manifest_path(repository: &Path, snapshot_id: &str) -> Result<PathBuf> {
    if !is_snapshot_id(snapshot_id) {
        bail!("invalid backup snapshot id {snapshot_id:?}");
    }
    Ok(repository
        .join(SNAPSHOTS_DIR)
        .join(format!("{snapshot_id}{MANIFEST_SUFFIX}")))
}

pub(crate) fn blob_path(repository: &Path, sha256: &str) -> Result<PathBuf> {
    if !is_sha256_hex(sha256) {
        bail!("invalid backup blob hash {sha256:?}");
    }
    Ok(repository
        .join(BLOBS_DIR)
        .join(format!("{sha256}{BLOB_SUFFIX}")))
}

pub(crate) fn is_snapshot_id(value: &str) -> bool {
    if value.len() != 36 {
        return false;
    }
    match Uuid::parse_str(value) {
        Ok(uuid) => uuid.hyphenated().to_string() == value,
        Err(_) => false,
    }
}

pub(crate) fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

pub(crate) fn snapshot_manifest_id_from_name(name: &str) -> Option<&str> {
    let id = name.strip_suffix(MANIFEST_SUFFIX)?;
    is_snapshot_id(id).then_some(id)
}

pub(crate) fn blob_hash_from_name(name: &str) -> Option<&str> {
    let hash = name.strip_suffix(BLOB_SUFFIX)?;
    is_sha256_hex(hash).then_some(hash)
}

pub(crate) fn load_snapshot(
    repository: &Path,
    metadata: &RepositoryMetadata,
    crypto: Option<&RepositoryCrypto>,
    snapshot_id: &str,
) -> Result<ValidatedSnapshot> {
    let path = snapshot_manifest_path(repository, snapshot_id)?;
    let file_metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("reading backup snapshot {}", path.display()))?;
    if file_metadata.file_type().is_symlink() {
        bail!(
            "refusing symlinked backup snapshot manifest {}",
            path.display()
        );
    }
    if !file_metadata.is_file() {
        bail!(
            "backup snapshot manifest {} is not a regular file",
            path.display()
        );
    }
    if file_metadata.len() > MAX_SNAPSHOT_ENVELOPE_BYTES {
        bail!(
            "backup snapshot manifest is {} bytes; the limit is {}",
            file_metadata.len(),
            MAX_SNAPSHOT_ENVELOPE_BYTES
        );
    }
    let file = File::open(&path).with_context(|| format!("opening {}", path.display()))?;
    let mut envelope = Vec::with_capacity(file_metadata.len() as usize);
    file.take(MAX_SNAPSHOT_ENVELOPE_BYTES + 1)
        .read_to_end(&mut envelope)
        .with_context(|| format!("reading {}", path.display()))?;
    if envelope.len() as u64 > MAX_SNAPSHOT_ENVELOPE_BYTES {
        bail!("backup snapshot manifest grew beyond its limit");
    }
    let opened = match crypto {
        Some(crypto) => crypto
            .open_manifest(&metadata.repository_id, snapshot_id, &envelope)
            .with_context(|| format!("authenticating backup snapshot {snapshot_id}"))?,
        None => crate::backup::crypto::open_manifest_plain(
            &metadata.repository_id,
            snapshot_id,
            &envelope,
        )
        .with_context(|| format!("validating backup snapshot {snapshot_id}"))?,
    };
    if opened.encrypted != metadata.encryption.enabled {
        bail!("backup snapshot encryption does not match the repository");
    }
    let manifest: SnapshotManifest = serde_json::from_slice(&opened.plaintext)
        .with_context(|| format!("parsing backup snapshot {snapshot_id}"))?;
    if manifest.snapshot_id != snapshot_id {
        bail!(
            "backup snapshot manifest id {:?} does not match its filename {snapshot_id:?}",
            manifest.snapshot_id
        );
    }
    validate_manifest(&manifest, metadata)?;
    Ok(ValidatedSnapshot {
        manifest,
        manifest_sha256: sha256_hex(&envelope),
        manifest_envelope_bytes: envelope.len() as u64,
        encrypted: opened.encrypted,
    })
}

pub(crate) fn validate_manifest(
    manifest: &SnapshotManifest,
    metadata: &RepositoryMetadata,
) -> Result<()> {
    if manifest.kind != SNAPSHOT_KIND {
        bail!("backup snapshot has an unexpected kind {:?}", manifest.kind);
    }
    if manifest.format_version != SNAPSHOT_FORMAT_VERSION {
        bail!(
            "unsupported backup snapshot format version {}; this build supports {}",
            manifest.format_version,
            SNAPSHOT_FORMAT_VERSION
        );
    }
    if !is_snapshot_id(&manifest.snapshot_id) {
        bail!("backup snapshot id is not a canonical UUID");
    }
    if manifest.sequence == 0 {
        bail!("backup snapshot sequence must be positive");
    }
    if manifest.parent.as_deref() == Some(&manifest.snapshot_id) {
        bail!("backup snapshot cannot be its own parent");
    }
    if let Some(parent) = &manifest.parent
        && !is_snapshot_id(parent)
    {
        bail!("backup snapshot parent is not a canonical UUID");
    }
    if manifest.incremental && manifest.parent.is_none() {
        bail!("incremental backup snapshot must have a parent");
    }
    if manifest.created_at.is_empty() || manifest.created_at.len() > 64 {
        bail!("backup snapshot created_at is malformed");
    }
    if manifest.app_version.is_empty() || manifest.app_version.len() > 64 {
        bail!("backup snapshot app_version is malformed");
    }
    if manifest.protocol_version == 0 {
        bail!("backup snapshot protocol version must be positive");
    }
    if manifest.encrypted != metadata.encryption.enabled {
        bail!("backup snapshot encryption flag does not match the repository");
    }
    match (
        metadata.encryption.kdf.as_ref(),
        manifest.kdf_fingerprint.as_deref(),
    ) {
        (Some(kdf), Some(fingerprint)) => {
            if fingerprint != kdf.fingerprint() {
                bail!("backup snapshot KDF fingerprint does not match the repository");
            }
        }
        (Some(_), None) => bail!("encrypted backup snapshot is missing its KDF fingerprint"),
        (None, Some(_)) => bail!("unencrypted backup snapshot must not have a KDF fingerprint"),
        (None, None) => {}
    }
    if manifest.entries.is_empty() {
        bail!("backup snapshot has no entries");
    }
    if manifest.entries.len() > MAX_SNAPSHOT_ENTRIES {
        bail!("backup snapshot has too many entries");
    }
    let mut seen_paths = BTreeSet::new();
    let mut database_entries = 0usize;
    let mut artifact_entries = 0usize;
    let mut total_bytes = 0u64;
    let mut previous_path: Option<&str> = None;
    for entry in &manifest.entries {
        let kind = classify_archive_path(&entry.path)
            .with_context(|| format!("backup snapshot path {:?}", entry.path))?;
        if let Some(previous) = previous_path
            && previous >= entry.path.as_str()
        {
            bail!(
                "backup snapshot entries are not strictly sorted by path ({previous:?}, {:?})",
                entry.path
            );
        }
        previous_path = Some(&entry.path);
        if !seen_paths.insert(entry.path.clone()) {
            bail!("backup snapshot contains duplicate path {:?}", entry.path);
        }
        if !is_sha256_hex(&entry.sha256) {
            bail!(
                "backup snapshot has an invalid SHA-256 for {:?}",
                entry.path
            );
        }
        if entry.size > BLOB_MAX_PLAINTEXT_BYTES {
            bail!(
                "backup snapshot entry {:?} is {} bytes; the per-file limit is {}",
                entry.path,
                entry.size,
                BLOB_MAX_PLAINTEXT_BYTES
            );
        }
        let minimum_stored = (BLOB_HEADER_BYTES + BLOB_CHUNK_RECORD_BYTES) as u64;
        if entry.stored_bytes < minimum_stored || entry.stored_bytes > MAX_BLOB_STORED_BYTES {
            bail!(
                "backup snapshot entry {:?} has an invalid stored blob size {}",
                entry.path,
                entry.stored_bytes
            );
        }
        total_bytes = total_bytes
            .checked_add(entry.size)
            .ok_or_else(|| anyhow!("backup snapshot byte total overflowed"))?;
        match kind {
            ArchiveKind::Database => database_entries += 1,
            ArchiveKind::Artifact if !manifest.includes_artifacts => bail!(
                "backup snapshot contains artifact {:?} but artifacts are excluded",
                entry.path
            ),
            ArchiveKind::Artifact => artifact_entries += 1,
            ArchiveKind::Manifest => {
                bail!("backup snapshot must not list backup.json as a data entry")
            }
            ArchiveKind::Projects | ArchiveKind::Session => {}
        }
    }
    if database_entries != 1 {
        bail!(
            "backup snapshot must list exactly one {DATABASE_FILENAME}; found {database_entries}"
        );
    }
    if artifact_entries > 0 && !manifest.includes_artifacts {
        bail!("backup snapshot artifact flag is inconsistent");
    }
    if manifest.totals.entries != manifest.entries.len() as u64 {
        bail!(
            "backup snapshot totals list {} entries; the manifest has {}",
            manifest.totals.entries,
            manifest.entries.len()
        );
    }
    if manifest.totals.bytes != total_bytes {
        bail!(
            "backup snapshot totals list {} bytes; the manifest sums to {}",
            manifest.totals.bytes,
            total_bytes
        );
    }
    Ok(())
}

pub(crate) fn scan_snapshots(
    repository: &Path,
    metadata: &RepositoryMetadata,
    crypto: Option<&RepositoryCrypto>,
) -> Result<Vec<ValidatedSnapshot>> {
    let directory = repository.join(SNAPSHOTS_DIR);
    let mut ids = Vec::new();
    match fs::symlink_metadata(&directory) {
        Ok(meta) if meta.file_type().is_symlink() => {
            bail!("refusing symlinked backup snapshots directory")
        }
        Ok(meta) if meta.is_dir() => {
            for entry in fs::read_dir(&directory)
                .with_context(|| format!("reading {}", directory.display()))?
            {
                let entry = entry?;
                let file_type = entry.file_type()?;
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow!("backup snapshot filename is not UTF-8"))?;
                if file_type.is_symlink() {
                    bail!("refusing symlinked backup snapshot {name:?}");
                }
                if !file_type.is_file() {
                    bail!("unexpected non-file in backup snapshots directory: {name:?}");
                }
                let Some(id) = snapshot_manifest_id_from_name(&name) else {
                    bail!("unexpected file in backup snapshots directory: {name:?}");
                };
                ids.push(id.to_string());
            }
        }
        Ok(_) => bail!(
            "backup snapshots path is not a directory: {}",
            directory.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", directory.display()));
        }
    }
    ids.sort();
    let mut snapshots = Vec::with_capacity(ids.len());
    for id in ids {
        // A tombstoned manifest left by an interrupted prune is logically gone.
        // Scans ignore it; a later prune removes the file.
        if metadata.is_pruned(&id).is_some() {
            continue;
        }
        snapshots.push(load_snapshot(repository, metadata, crypto, &id)?);
    }
    snapshots.sort_by_key(|snapshot| snapshot.sequence());
    validate_chain(metadata, &snapshots)?;
    Ok(snapshots)
}

pub(crate) fn validate_chain(
    metadata: &RepositoryMetadata,
    snapshots: &[ValidatedSnapshot],
) -> Result<()> {
    if snapshots.len() + metadata.pruned_snapshots.len() > MAX_CHAIN_LENGTH {
        bail!("backup repository chain exceeds the supported length");
    }
    if snapshots.is_empty() {
        if !metadata.pruned_snapshots.is_empty() {
            bail!("backup repository has prune tombstones but no snapshots");
        }
        return Ok(());
    }

    let mut tombstones: Vec<&PrunedSnapshot> = metadata.pruned_snapshots.iter().collect();
    tombstones.sort_by_key(|tombstone| tombstone.sequence);
    for (index, tombstone) in tombstones.iter().enumerate() {
        if tombstone.sequence != (index + 1) as u64 {
            bail!(
                "backup repository prune history is not a contiguous prefix at sequence {}",
                tombstone.sequence
            );
        }
    }

    let first = &snapshots[0];
    if tombstones.is_empty() {
        if first.sequence() != 1 {
            bail!(
                "backup repository's first snapshot has sequence {}; expected 1",
                first.sequence()
            );
        }
        if first.parent().is_some() {
            bail!("backup repository's first snapshot must not have a parent");
        }
    } else {
        let expected_sequence = tombstones.len() as u64 + 1;
        if first.sequence() != expected_sequence {
            bail!(
                "backup repository's first retained snapshot has sequence {}; expected {}",
                first.sequence(),
                expected_sequence
            );
        }
        let Some(parent) = first.parent() else {
            bail!("backup repository's first retained snapshot is missing its pruned parent");
        };
        let Some(tombstone) = metadata.is_pruned(parent) else {
            bail!("backup repository is missing parent snapshot {parent}");
        };
        if tombstone.sequence + 1 != first.sequence() {
            bail!("backup repository prune tombstone sequence is inconsistent");
        }
    }

    for (index, snapshot) in snapshots.iter().enumerate() {
        let expected_sequence = first.sequence() + index as u64;
        if snapshot.sequence() != expected_sequence {
            bail!(
                "backup repository chain has a sequence gap at {}; expected {}",
                snapshot.sequence(),
                expected_sequence
            );
        }
        if index > 0 {
            let previous = &snapshots[index - 1];
            match snapshot.parent() {
                Some(parent) if parent == previous.id() => {}
                Some(parent) => bail!(
                    "backup snapshot {} references parent {parent}; expected {}",
                    snapshot.id(),
                    previous.id()
                ),
                None => bail!(
                    "backup snapshot {} is missing its parent {}",
                    snapshot.id(),
                    previous.id()
                ),
            }
        }
        if let Some(parent) = snapshot.parent()
            && parent == snapshot.id()
        {
            bail!("backup snapshot cannot be its own parent");
        }
    }
    Ok(())
}

pub(crate) fn find_snapshot<'a>(
    snapshots: &'a [ValidatedSnapshot],
    snapshot_id: &str,
) -> Result<&'a ValidatedSnapshot> {
    snapshots
        .iter()
        .find(|snapshot| snapshot.id() == snapshot_id)
        .ok_or_else(|| anyhow!("backup snapshot not found in repository: {snapshot_id}"))
}

/// Scan the blob directory once for validation/prune reports.
pub(crate) fn scan_blob_hashes(repository: &Path) -> Result<BTreeMap<String, u64>> {
    let directory = repository.join(BLOBS_DIR);
    let mut blobs = BTreeMap::new();
    match fs::symlink_metadata(&directory) {
        Ok(meta) if meta.file_type().is_symlink() => {
            bail!("refusing symlinked backup blobs directory")
        }
        Ok(meta) if meta.is_dir() => {
            for entry in fs::read_dir(&directory)
                .with_context(|| format!("reading {}", directory.display()))?
            {
                let entry = entry?;
                let file_type = entry.file_type()?;
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow!("backup blob filename is not UTF-8"))?;
                if file_type.is_symlink() {
                    bail!("refusing symlinked backup blob {name:?}");
                }
                if !file_type.is_file() {
                    bail!("unexpected non-file in backup blobs directory: {name:?}");
                }
                let Some(hash) = blob_hash_from_name(&name) else {
                    bail!("unexpected file in backup blobs directory: {name:?}");
                };
                let size = entry.metadata()?.len();
                if size > MAX_BLOB_STORED_BYTES {
                    bail!("backup blob {hash} exceeds the supported size");
                }
                blobs.insert(hash.to_string(), size);
            }
        }
        Ok(_) => bail!(
            "backup blobs path is not a directory: {}",
            directory.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", directory.display()));
        }
    }
    Ok(blobs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain_metadata() -> RepositoryMetadata {
        new_metadata(EncryptionMetadata::plaintext())
    }

    fn sample_manifest(sequence: u64, parent: Option<String>) -> SnapshotManifest {
        let created_at = now_rfc3339();
        let entries = vec![SnapshotEntry {
            path: DATABASE_FILENAME.to_string(),
            size: 4096,
            sha256: "a".repeat(64),
            compression: Compression::None,
            stored_bytes: 4200,
        }];
        SnapshotManifest {
            kind: SNAPSHOT_KIND.to_string(),
            format_version: SNAPSHOT_FORMAT_VERSION,
            snapshot_id: Uuid::now_v7().to_string(),
            sequence,
            parent,
            incremental: sequence > 1,
            created_at,
            app_version: APP_VERSION.to_string(),
            protocol_version: PROTOCOL_VERSION,
            encrypted: false,
            includes_artifacts: false,
            kdf_fingerprint: None,
            totals: SnapshotTotals {
                entries: 1,
                bytes: 4096,
            },
            entries,
        }
    }

    fn validated(manifest: SnapshotManifest) -> ValidatedSnapshot {
        ValidatedSnapshot {
            manifest,
            manifest_sha256: "b".repeat(64),
            manifest_envelope_bytes: 100,
            encrypted: false,
        }
    }

    #[test]
    fn metadata_structure_and_canonical_signing() {
        let metadata = plain_metadata();
        metadata.validate_structure().unwrap();
        assert_eq!(metadata.format_version, REPOSITORY_FORMAT_VERSION);
        assert_eq!(metadata.kind, REPOSITORY_KIND);
        assert!(is_snapshot_id(&metadata.repository_id));
        assert!(metadata.metadata_mac.is_none());
        assert!(!metadata.encryption.enabled);

        let bytes = canonical_metadata_bytes(&metadata).unwrap();
        let parsed = parse_metadata(&bytes).unwrap();
        assert_eq!(parsed, metadata);
    }

    #[test]
    fn metadata_rejects_unknown_fields_and_bad_ids() {
        let metadata = plain_metadata();
        let mut value = serde_json::to_value(&metadata).unwrap();
        value["unknown"] = serde_json::json!(true);
        let error = parse_metadata(&serde_json::to_vec(&value).unwrap()).unwrap_err();
        assert!(format!("{error:#}").contains("unknown field"), "{error:#}");

        let mut value = serde_json::to_value(&metadata).unwrap();
        value["repository_id"] = serde_json::json!("not-a-uuid");
        assert!(parse_metadata(&serde_json::to_vec(&value).unwrap()).is_err());

        let mut metadata = metadata;
        metadata.format_version = 99;
        assert!(metadata.validate_structure().is_err());
        metadata.format_version = REPOSITORY_FORMAT_VERSION;
        metadata.kind = "something-else".to_string();
        assert!(metadata.validate_structure().is_err());
    }

    #[test]
    fn metadata_rejects_inconsistent_encryption_and_mac() {
        let mut metadata = plain_metadata();
        metadata.encryption = EncryptionMetadata {
            enabled: true,
            kdf: None,
        };
        assert!(metadata.validate_structure().is_err());

        let mut metadata = plain_metadata();
        metadata.metadata_mac = Some("a".repeat(64));
        assert!(metadata.validate_structure().is_err());

        let mut metadata = plain_metadata();
        metadata.encryption = EncryptionMetadata {
            enabled: false,
            kdf: None,
        };
        metadata.metadata_mac = None;
        metadata.pruned_snapshots.push(PrunedSnapshot {
            id: Uuid::now_v7().to_string(),
            sequence: 0,
            created_at: now_rfc3339(),
            pruned_at: now_rfc3339(),
            manifest_sha256: "c".repeat(64),
        });
        assert!(metadata.validate_structure().is_err());
        metadata.pruned_snapshots[0].sequence = 1;
        metadata
            .pruned_snapshots
            .push(metadata.pruned_snapshots[0].clone());
        assert!(metadata.validate_structure().is_err());
    }

    #[test]
    fn manifest_validation_accepts_the_allowlist_and_rejects_traversal() {
        let metadata = plain_metadata();
        let manifest = sample_manifest(1, None);
        validate_manifest(&manifest, &metadata).unwrap();

        let mut traversing = manifest.clone();
        traversing.entries[0].path = "../escape".to_string();
        assert!(validate_manifest(&traversing, &metadata).is_err());

        let mut absolute = manifest.clone();
        absolute.entries[0].path = "/tmp/escape".to_string();
        assert!(validate_manifest(&absolute, &metadata).is_err());

        let mut duplicate = manifest.clone();
        duplicate.entries.push(duplicate.entries[0].clone());
        assert!(validate_manifest(&duplicate, &metadata).is_err());

        let mut no_database = manifest.clone();
        no_database.entries.clear();
        assert!(validate_manifest(&no_database, &metadata).is_err());

        let mut bad_hash = manifest.clone();
        bad_hash.entries[0].sha256 = "Z".repeat(64);
        assert!(validate_manifest(&bad_hash, &metadata).is_err());

        let mut manifest_entry = manifest.clone();
        manifest_entry.entries[0].path = "backup.json".to_string();
        assert!(validate_manifest(&manifest_entry, &metadata).is_err());

        let mut two_entries = manifest.clone();
        two_entries.entries.push(SnapshotEntry {
            path: "projects.jsonl".to_string(),
            size: 1,
            sha256: "b".repeat(64),
            compression: Compression::None,
            stored_bytes: 100,
        });
        two_entries.totals.entries = 2;
        two_entries.totals.bytes += 1;
        two_entries
            .entries
            .sort_by(|left, right| left.path.cmp(&right.path));
        validate_manifest(&two_entries, &metadata).unwrap();
        two_entries.entries.swap(0, 1);
        assert!(validate_manifest(&two_entries, &metadata).is_err());
    }

    #[test]
    fn artifact_flag_and_totals_are_validated() {
        let metadata = plain_metadata();
        let mut manifest = sample_manifest(1, None);
        manifest.entries.push(SnapshotEntry {
            path: format!("artifacts/blobs/{}.zst", "d".repeat(64)),
            size: 10,
            sha256: "e".repeat(64),
            compression: Compression::Zstd,
            stored_bytes: 200,
        });
        manifest
            .entries
            .sort_by(|left, right| left.path.cmp(&right.path));
        manifest.totals.entries = 2;
        manifest.totals.bytes += 10;
        assert!(validate_manifest(&manifest, &metadata).is_err());
        manifest.includes_artifacts = true;
        validate_manifest(&manifest, &metadata).unwrap();

        manifest.totals.bytes += 1;
        assert!(validate_manifest(&manifest, &metadata).is_err());
        manifest.totals.bytes -= 1;
        manifest.totals.entries = 1;
        assert!(validate_manifest(&manifest, &metadata).is_err());
    }

    #[test]
    fn incremental_requires_a_parent_and_encryption_matches_metadata() {
        let metadata = plain_metadata();
        let mut manifest = sample_manifest(2, None);
        assert!(validate_manifest(&manifest, &metadata).is_err());
        manifest.parent = Some(Uuid::now_v7().to_string());
        validate_manifest(&manifest, &metadata).unwrap();
        manifest.encrypted = true;
        assert!(validate_manifest(&manifest, &metadata).is_err());
        manifest.encrypted = false;
        manifest.kdf_fingerprint = Some("a".repeat(64));
        assert!(validate_manifest(&manifest, &metadata).is_err());
    }

    #[test]
    fn chain_validation_is_strict_and_linear() {
        let mut metadata = plain_metadata();
        let first = sample_manifest(1, None);
        let first_id = first.snapshot_id.clone();
        let second = sample_manifest(2, Some(first_id.clone()));
        validate_chain(
            &metadata,
            &[validated(first.clone()), validated(second.clone())],
        )
        .unwrap();

        // A missing middle parent is rejected.
        let third = sample_manifest(3, Some(first_id.clone()));
        assert!(
            validate_chain(
                &metadata,
                &[validated(first.clone()), validated(third.clone())]
            )
            .is_err()
        );

        // Duplicate sequence is rejected.
        let mut duplicate = second.clone();
        duplicate.snapshot_id = Uuid::now_v7().to_string();
        assert!(
            validate_chain(
                &metadata,
                &[validated(second.clone()), validated(duplicate)]
            )
            .is_err()
        );

        // A missing parent without a tombstone is rejected.
        assert!(validate_chain(&metadata, &[validated(second.clone())]).is_err());
        // A recorded prune tombstone makes the retained prefix valid.
        metadata.pruned_snapshots.push(PrunedSnapshot {
            id: first_id,
            sequence: 1,
            created_at: first.created_at.clone(),
            pruned_at: now_rfc3339(),
            manifest_sha256: "f".repeat(64),
        });
        validate_chain(&metadata, &[validated(second.clone())]).unwrap();
        let mut second_without_parent = second.clone();
        second_without_parent.parent = None;
        assert!(validate_chain(&metadata, &[validated(second_without_parent)]).is_err());
    }

    #[test]
    fn filenames_and_hashes_are_canonical() {
        let id = Uuid::now_v7().to_string();
        assert!(is_snapshot_id(&id));
        assert!(!is_snapshot_id(&id.to_uppercase()));
        assert!(!is_snapshot_id("../../etc/passwd"));
        assert_eq!(
            snapshot_manifest_id_from_name(&format!("{id}.manifest")),
            Some(id.as_str())
        );
        assert!(snapshot_manifest_id_from_name("../x.manifest").is_none());
        assert!(blob_hash_from_name(&format!("{}.blob", "a".repeat(64))).is_some());
        assert!(blob_hash_from_name(&format!("{}.blob", "A".repeat(64))).is_none());
        assert!(blob_hash_from_name("../escape.blob").is_none());
        assert!(is_sha256_hex(&"f".repeat(64)));
        assert!(!is_sha256_hex(&"f".repeat(63)));
    }

    #[test]
    fn metadata_write_is_atomic_and_private() {
        let dir = tempfile::TempDir::new().unwrap();
        let metadata = plain_metadata();
        write_metadata_atomic(dir.path(), &metadata).unwrap();
        let again = parse_metadata(&read_metadata_bytes(dir.path()).unwrap()).unwrap();
        assert_eq!(again, metadata);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.path().join(METADATA_FILENAME))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }
}
