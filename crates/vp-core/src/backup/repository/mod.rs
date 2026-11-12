//! V2/V3 backup repository: encrypted, incremental, content-addressed snapshots.
//!
//! # What this adds
//!
//! The v1 ZIP backup (`vp backup --output`) remains byte-for-byte compatible.
//! This module adds an opt-in repository format that:
//!
//! - stores every allowlisted file as a content-addressed blob keyed by the
//!   SHA-256 of its plaintext;
//! - encrypts blobs and snapshot manifests with ChaCha20-Poly1305 under keys
//!   derived by Argon2id from a passphrase (or stores them unencrypted only
//!   when the caller explicitly asks);
//! - writes immutable snapshot manifests with a linear parent chain and a
//!   monotonic sequence number;
//! - optionally signs repository metadata and every snapshot manifest with
//!   Ed25519 (format v3), while v2 repositories remain readable and are
//!   reported as unsigned;
//! - reuses existing blobs for incremental snapshots and reports exact
//!   logical, stored, and deduplicated byte counts;
//! - validates the entire chain, signatures, and every blob before an atomic
//!   data-directory restore;
//! - records the verified generation in a local anti-rollback anchor and
//!   rejects older generations unless explicitly allowed;
//! - prunes old snapshots only after validating retained references, with a
//!   dry-run and tombstone-based parent handling.
//!
//! # Secret handling
//!
//! [`Passphrase`] is the only passphrase input. It is debug-redacted, bounded,
//! and zeroized. The CLI never accepts a passphrase argument: it reads an
//! environment variable selected by name or a permissions-checked file. No
//! passphrase, derived key, or plaintext key material is written to repository
//! metadata, manifests, logs, nonce records, or error messages. Ed25519 signing
//! keys are owned by [`crate::backup::signing`]; only public key ids and
//! signatures enter the repository.
//!
//! # Offline and daemon safety
//!
//! Backup is offline-safe and does not contact the daemon. It takes a SQLite
//! `VACUUM INTO` snapshot and copies only the v1 allowlisted paths. Restore
//! refuses when discovery or a held instance lock indicates a daemon, validates
//! every referenced manifest/blob into a sibling scratch directory, and only
//! then swaps the directory. A wrong passphrase, tampered file, missing parent,
//! bad KDF parameters, or partial chain leaves an existing data directory
//! untouched.
//!
//! Prune is destructive and deliberately narrow: it keeps the latest N
//! snapshots by sequence, writes authenticated tombstones before deleting any
//! file, and never deletes a blob referenced by a retained manifest. A crash
//! between the tombstone commit and file cleanup leaves tombstoned manifests
//! that are ignored by scans and can be cleaned by a later prune; it never
//! makes a retained snapshot depend on a deleted blob.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;
use vp_protocol::now_rfc3339;

use crate::backup::anchor::{AnchorStore, AnchorUpdate, RollbackCheck};
use crate::backup::signing::{Keyring, KeyringSummary, SigningCredential};
use crate::backup::{
    ArchiveKind, DATABASE_FILENAME, ScratchPath, classify_archive_path, collect_backup_sources,
    copy_private, daemon_appears_running, hash_file, non_empty_parent, unique_sibling,
    verify_sqlite_header, write_private,
};

pub use super::crypto::{KdfConfig, KdfSummary, Passphrase};
use super::crypto::{KdfParameters, RepositoryCrypto, sha256_hex};

mod blob;
mod format;
mod rekey;
mod signing;
mod verify;

pub use crate::backup::error::{BackupSecurityCode, BackupSecurityError};
pub use blob::Compression;
pub use rekey::{
    MAX_REKEY_SNAPSHOTS, RepositoryRekeyOptions, RepositoryRekeyReport, rekey_repository,
};
pub use signing::{ManifestSignature, SignatureCheck, SignatureStatus};
pub use verify::{RepositoryVerifyOptions, RepositoryVerifyReport, verify_repository};

use signing::{
    build_head, check_signed_head, configure_signing, entries_sha256, sign_repository_metadata,
    sign_snapshot_manifest, signing_summary, tombstones_sha256,
};

use blob::{
    BLOB_CHUNK_RECORD_BYTES, BLOB_HEADER_BYTES, inspect_blob, materialize_blob, seal_blob_file,
    verify_blob_file,
};
use format::{
    BLOBS_DIR, EVENTS_FILENAME, LOCK_FILENAME, MAX_KEEP, METADATA_FILENAME, NONCES_FILENAME,
    PrunedSnapshot, RepositoryMetadata, SNAPSHOT_FORMAT_VERSION, SNAPSHOTS_DIR, SnapshotEntry,
    SnapshotManifest, SnapshotTotals, TEMP_DIR, blob_hash_from_name, blob_path,
    canonical_metadata_bytes, find_snapshot, is_snapshot_id, load_snapshot, new_metadata,
    parse_metadata, read_metadata_bytes, scan_blob_hashes, scan_snapshots, sign_metadata,
    snapshot_manifest_id_from_name, snapshot_manifest_path, validate_chain,
    verify_metadata_authenticity, write_metadata_atomic,
};

const MAX_NONCE_TABLE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_REPORT_ISSUES: usize = 100;
const BLOB_VERIFY_CACHE_LIMIT: usize = 100_000;

/// Repository encryption selected for a new backup.
///
/// Plaintext is never an implicit fallback. The caller must pass
/// [`RepositoryEncryption::Plaintext`] explicitly, and an existing repository's
/// mode cannot be changed by a later backup.
pub enum RepositoryEncryption {
    Encrypted(Passphrase),
    Plaintext,
}

/// Options for one repository snapshot.
pub struct RepositoryBackupOptions {
    pub incremental: bool,
    pub include_artifacts: bool,
    pub compression: Compression,
    pub encryption: RepositoryEncryption,
    pub kdf: KdfConfig,
    /// Sign the repository with Ed25519. `None` keeps a v2 unsigned repository
    /// (or refuses to append to an existing v3 signed repository).
    pub signing: Option<RepositorySigningOptions>,
}

/// Keyring-backed signing selection for a new or existing repository.
#[derive(Clone)]
pub struct RepositorySigningOptions {
    pub keyring: Keyring,
    /// Explicit key id; `None` selects the keyring's active key.
    pub key_id: Option<String>,
}

impl std::fmt::Debug for RepositorySigningOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RepositorySigningOptions")
            .field("keyring", &self.keyring.summary().path)
            .field("key_id", &self.key_id)
            .finish()
    }
}

impl std::fmt::Debug for RepositoryBackupOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RepositoryBackupOptions")
            .field("incremental", &self.incremental)
            .field("include_artifacts", &self.include_artifacts)
            .field("compression", &self.compression)
            .field(
                "encryption",
                &match self.encryption {
                    RepositoryEncryption::Encrypted(_) => "encrypted",
                    RepositoryEncryption::Plaintext => "plaintext",
                },
            )
            .field("kdf", &self.kdf)
            .field("signing", &self.signing)
            .finish()
    }
}

/// Report for a newly written snapshot.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepositoryBackupReport {
    pub repository: String,
    pub format_version: u32,
    pub snapshot_format_version: u32,
    pub snapshot_id: String,
    pub sequence: u64,
    pub parent: Option<String>,
    pub created_at: String,
    pub incremental: bool,
    pub encrypted: bool,
    pub includes_artifacts: bool,
    pub compression: String,
    pub entries: usize,
    pub sessions: usize,
    pub artifacts: usize,
    pub bytes: u64,
    pub written_bytes: u64,
    /// Total logical bytes not written as new blobs (parent/store reuse plus
    /// duplicate paths).
    pub deduplicated_bytes: u64,
    pub deduplicated_from_parent_bytes: u64,
    pub deduplicated_from_store_bytes: u64,
    pub blobs_written: usize,
    pub blobs_reused: usize,
    pub stored_bytes: u64,
    pub manifest_bytes: u64,
    pub manifest_sha256: String,
    pub manifest_plaintext_sha256: String,
    pub kdf: Option<KdfSummary>,
    pub app_version: String,
    pub protocol_version: u32,
    #[serde(default)]
    pub signed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_algorithm: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_update: Option<AnchorUpdate>,
}

/// Summary of one snapshot for `backup-list`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepositorySnapshotSummary {
    pub snapshot_id: String,
    pub sequence: u64,
    pub parent: Option<String>,
    pub created_at: String,
    pub incremental: bool,
    pub encrypted: bool,
    pub includes_artifacts: bool,
    pub entries: usize,
    pub sessions: usize,
    pub artifacts: usize,
    pub bytes: u64,
    pub unique_blobs: usize,
    pub manifest_bytes: u64,
    pub manifest_sha256: String,
    #[serde(default)]
    pub signed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_key_id: Option<String>,
    #[serde(default)]
    pub signature_status: String,
}

/// Validation results exposed by list and prune reports.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepositoryValidationReport {
    pub valid: bool,
    pub manifests_valid: bool,
    pub chain_valid: bool,
    pub nonce_table_valid: bool,
    pub blob_verification: String,
    pub blobs_present: usize,
    pub blobs_missing: usize,
    pub blobs_verified: usize,
    pub blobs_corrupt: usize,
    pub issues: Vec<String>,
    #[serde(default)]
    pub signature_verification: String,
    #[serde(default)]
    pub signed_snapshots: usize,
    #[serde(default)]
    pub unsigned_snapshots: usize,
    #[serde(default)]
    pub metadata_signature: String,
    /// Number of signature checks accepted from revoked keys because the
    /// operator passed `--allow-revoked`.
    #[serde(default)]
    pub revoked_signatures: usize,
    #[serde(default)]
    pub revoked_key_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepositoryListReport {
    pub repository: String,
    pub format_version: u32,
    pub encrypted: bool,
    pub kdf: Option<KdfSummary>,
    pub created_at: String,
    pub snapshots: Vec<RepositorySnapshotSummary>,
    pub snapshot_count: usize,
    pub logical_bytes: u64,
    pub unique_blobs: usize,
    pub unique_plaintext_bytes: u64,
    pub stored_blob_bytes: u64,
    pub unreferenced_blobs: usize,
    pub unreferenced_blob_bytes: u64,
    pub nonce_records: usize,
    pub validation: RepositoryValidationReport,
    #[serde(default)]
    pub signed: bool,
    #[serde(default)]
    pub signing_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyring: Option<KeyringSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepositoryRestoreReport {
    pub repository: String,
    pub snapshot_id: String,
    pub sequence: u64,
    pub parent: Option<String>,
    pub created_at: String,
    pub output: String,
    pub data_dir: String,
    pub format_version: u32,
    pub encrypted: bool,
    pub includes_artifacts: bool,
    pub entries: usize,
    pub sessions: usize,
    pub artifacts: usize,
    pub bytes: u64,
    pub validated_blobs: usize,
    pub stored_blob_bytes: u64,
    pub replaced_existing: bool,
    pub kdf: Option<KdfSummary>,
    #[serde(default)]
    pub signed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_key_id: Option<String>,
    #[serde(default)]
    pub signature_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback: Option<RollbackCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_update: Option<AnchorUpdate>,
    #[serde(default)]
    pub head_issues: Vec<String>,
    /// Revoked-key signatures accepted because `allow_revoked` was explicit.
    #[serde(default)]
    pub revoked_signature_warnings: Vec<String>,
}

/// Options for a verified repository restore.
#[derive(Default)]
pub struct RepositoryRestoreOptions<'a> {
    pub passphrase: Option<&'a Passphrase>,
    pub keyring: Option<&'a Keyring>,
    pub anchor: Option<&'a AnchorStore>,
    pub allow_rollback: bool,
    /// Accept signatures made by revoked keys. Default false fails closed.
    pub allow_revoked: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PruneSnapshotReport {
    pub snapshot_id: String,
    pub sequence: u64,
    pub parent: Option<String>,
    pub created_at: String,
    pub bytes: u64,
    pub manifest_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepositoryPruneReport {
    pub repository: String,
    pub keep: usize,
    pub dry_run: bool,
    pub applied: bool,
    pub snapshots_total: usize,
    pub snapshots_kept: usize,
    pub snapshots_pruned: Vec<PruneSnapshotReport>,
    pub blobs_kept: usize,
    pub blobs_pruned: usize,
    pub protected_bytes: u64,
    pub reclaimed_bytes: u64,
    pub validation: RepositoryValidationReport,
    #[serde(default)]
    pub signed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_key_id: Option<String>,
}

/// Create one snapshot in a v2 repository.
///
/// The repository is created on the first call. An existing repository's
/// encryption mode must match `options.encryption`; its stored KDF parameters
/// are used regardless of `options.kdf`.
pub fn create_repository_backup(
    data_dir: &Path,
    repository: &Path,
    options: RepositoryBackupOptions,
) -> Result<RepositoryBackupReport> {
    if !data_dir.is_dir() {
        bail!("data directory not found at {}", data_dir.display());
    }
    let data_canonical = fs::canonicalize(data_dir)
        .with_context(|| format!("resolving data directory {}", data_dir.display()))?;
    validate_database_source(&data_canonical.join(DATABASE_FILENAME))?;
    let repository_candidate = resolve_future_path(repository)?;
    reject_repository_inside_data(&repository_candidate, &data_canonical)?;
    let root = repository_root(repository, true)?;
    reject_repository_inside_data(&root, &data_canonical)?;
    let credential = match &options.signing {
        Some(signing) => Some(
            signing
                .keyring
                .signing_credential(signing.key_id.as_deref())
                .context("loading the backup signing credential")?,
        ),
        None => None,
    };
    let keyring = options.signing.as_ref().map(|signing| &signing.keyring);
    let mut session = open_for_write(
        &root,
        &options.encryption,
        options.kdf,
        keyring,
        credential.as_ref(),
    )?;
    let snapshots = scan_snapshots(
        &session.root,
        &session.metadata,
        session.crypto.as_ref(),
        session.keyring.as_ref(),
    )?;
    let latest = snapshots.last();
    if options.incremental && latest.is_none() {
        bail!(
            "--incremental requires an existing snapshot in {}; \
             run a full repository backup first",
            session.root.display()
        );
    }
    if session.metadata.format_version >= 3 && credential.is_none() {
        return Err(BackupSecurityError::SigningRequired.into());
    }
    let parent = latest.map(|snapshot| snapshot.manifest.snapshot_id.clone());
    let parent_hashes: BTreeSet<String> = latest
        .map(|snapshot| snapshot.blob_hashes())
        .unwrap_or_default();
    let snapshot_id = Uuid::now_v7().to_string();
    let sequence = match latest {
        Some(snapshot) => snapshot
            .sequence()
            .checked_add(1)
            .ok_or_else(|| anyhow!("backup snapshot sequence overflowed"))?,
        None => 1,
    };

    let staging_root = session.root.join(TEMP_DIR);
    let stage = ScratchPath::directory(&staging_root, "snapshot")?;
    let database = data_canonical.join(DATABASE_FILENAME);
    validate_database_source(&database)?;
    vp_store::snapshot_database(&database, &stage.path.join(DATABASE_FILENAME))?;

    let mut relatives = BTreeSet::new();
    relatives.insert(DATABASE_FILENAME.to_string());
    for relative in collect_backup_sources(&data_canonical, options.include_artifacts)? {
        relatives.insert(relative);
    }
    for relative in &relatives {
        if relative == DATABASE_FILENAME {
            continue;
        }
        classify_archive_path(relative)?;
        copy_private(&data_canonical.join(relative), &stage.path.join(relative))?;
    }

    let mut entries = Vec::with_capacity(relatives.len());
    let mut dispositions: BTreeMap<String, BlobDisposition> = BTreeMap::new();
    let mut written_bytes = 0u64;
    let mut deduplicated_from_parent_bytes = 0u64;
    let mut deduplicated_from_store_bytes = 0u64;
    let mut blobs_written = 0usize;
    let mut blobs_reused = 0usize;
    let mut stored_bytes = 0u64;
    for relative in &relatives {
        let staged = stage.path.join(relative);
        let (size, sha256) = hash_file(&staged)?;
        let existing = dispositions.get(&sha256).cloned();
        let was_cached = existing.is_some();
        let disposition = match existing {
            Some(disposition) => {
                if disposition.plaintext_bytes != size {
                    bail!("content hash {sha256} appears with conflicting sizes in one snapshot");
                }
                disposition
            }
            None => {
                let blob_file = blob_path(&session.root, &sha256)?;
                let disposition = if blob_file.exists() {
                    let header = inspect_blob(&blob_file)?;
                    if header.encrypted != session.metadata.encryption.enabled {
                        bail!("backup blob {sha256} encryption flag does not match the repository");
                    }
                    let verified = verify_blob_file(
                        &blob_file,
                        &sha256,
                        size,
                        header.compression,
                        session.crypto.as_ref(),
                    )
                    .with_context(|| {
                        format!("validating reused backup blob {sha256} before reuse")
                    })?;
                    if verified.plaintext_bytes != size
                        || verified.sha256 != sha256
                        || verified.compression != header.compression
                        || verified.encrypted != session.metadata.encryption.enabled
                    {
                        bail!("backup blob {sha256} did not verify consistently before reuse");
                    }
                    blobs_reused += 1;
                    BlobDisposition {
                        compression: header.compression,
                        stored_bytes: verified.stored_bytes,
                        plaintext_bytes: size,
                        newly_written: false,
                    }
                } else {
                    if options.incremental && parent_hashes.contains(&sha256) {
                        bail!(
                            "parent snapshot references blob {sha256}, but the blob is missing; \
                             refusing to rewrite history"
                        );
                    }
                    let mut blob_scratch = ScratchPath::file(&staging_root, "blob")?;
                    let sealed = seal_blob_file(
                        &staged,
                        &blob_scratch.path,
                        &sha256,
                        options.compression,
                        session.crypto.as_ref(),
                    )?;
                    if sealed.plaintext_bytes != size {
                        bail!(
                            "sealed backup blob {sha256} has {} bytes; expected {}",
                            sealed.plaintext_bytes,
                            size
                        );
                    }
                    if sealed.encrypted != session.metadata.encryption.enabled
                        || sealed.sha256 != sha256
                    {
                        bail!("sealed backup blob did not match the repository or content hash");
                    }
                    if sealed.encrypted {
                        append_nonce(
                            &session.root,
                            "blob",
                            &sha256,
                            Some(&snapshot_id),
                            &sealed.nonce,
                            &now_rfc3339(),
                        )?;
                    }
                    publish_immutable(&blob_file, &mut blob_scratch)?;
                    blobs_written += 1;
                    stored_bytes = stored_bytes
                        .checked_add(sealed.stored_bytes)
                        .ok_or_else(|| anyhow!("backup stored byte total overflowed"))?;
                    BlobDisposition {
                        compression: sealed.compression,
                        stored_bytes: sealed.stored_bytes,
                        plaintext_bytes: size,
                        newly_written: true,
                    }
                };
                dispositions.insert(sha256.clone(), disposition.clone());
                disposition
            }
        };
        if disposition.newly_written && !was_cached {
            written_bytes = written_bytes
                .checked_add(size)
                .ok_or_else(|| anyhow!("backup written byte total overflowed"))?;
        } else if parent_hashes.contains(&sha256) {
            deduplicated_from_parent_bytes = deduplicated_from_parent_bytes
                .checked_add(size)
                .ok_or_else(|| anyhow!("backup dedup byte total overflowed"))?;
        } else {
            deduplicated_from_store_bytes = deduplicated_from_store_bytes
                .checked_add(size)
                .ok_or_else(|| anyhow!("backup dedup byte total overflowed"))?;
        }
        entries.push(SnapshotEntry {
            path: relative.clone(),
            size,
            sha256,
            compression: disposition.compression,
            stored_bytes: disposition.stored_bytes,
        });
    }

    let bytes = entries
        .iter()
        .try_fold(0u64, |total, entry| total.checked_add(entry.size))
        .ok_or_else(|| anyhow!("backup logical byte total overflowed"))?;
    if written_bytes > bytes {
        bail!("backup written bytes exceed logical bytes");
    }
    let deduplicated_bytes = bytes - written_bytes;
    let mut manifest = SnapshotManifest {
        kind: format::SNAPSHOT_KIND.to_string(),
        format_version: SNAPSHOT_FORMAT_VERSION,
        snapshot_id: snapshot_id.clone(),
        sequence,
        parent: parent.clone(),
        incremental: options.incremental,
        created_at: now_rfc3339(),
        app_version: vp_protocol::APP_VERSION.to_string(),
        protocol_version: vp_protocol::PROTOCOL_VERSION,
        encrypted: session.metadata.encryption.enabled,
        includes_artifacts: options.include_artifacts,
        kdf_fingerprint: session
            .metadata
            .encryption
            .kdf
            .as_ref()
            .map(KdfParameters::fingerprint),
        entries,
        totals: SnapshotTotals {
            entries: relatives.len() as u64,
            bytes,
        },
        signature: None,
    };
    format::validate_manifest(&manifest, &session.metadata)?;
    if let Some(credential) = credential.as_ref() {
        sign_snapshot_manifest(&session.metadata.repository_id, credential, &mut manifest)?;
    }

    let manifest_json =
        serde_json::to_vec(&manifest).context("serializing backup snapshot manifest")?;
    let sealed_manifest = match session.crypto.as_ref() {
        Some(crypto) => crypto.seal_manifest(
            &session.metadata.repository_id,
            &manifest.snapshot_id,
            &manifest_json,
        )?,
        None => super::crypto::seal_manifest_plain(
            &session.metadata.repository_id,
            &manifest.snapshot_id,
            &manifest_json,
        )?,
    };
    if sealed_manifest.bytes.is_empty() {
        bail!("backup snapshot manifest unexpectedly sealed to zero bytes");
    }
    if session.metadata.encryption.enabled {
        append_nonce(
            &session.root,
            "manifest",
            &manifest.snapshot_id,
            Some(&manifest.snapshot_id),
            &sealed_manifest.nonce,
            &now_rfc3339(),
        )?;
    }
    let manifest_path = snapshot_manifest_path(&session.root, &manifest.snapshot_id)?;
    let mut manifest_scratch = ScratchPath::file(&staging_root, "manifest")?;
    write_private(&manifest_scratch.path, &sealed_manifest.bytes)?;
    publish_immutable(&manifest_path, &mut manifest_scratch)?;
    let manifest_sha256 = sha256_hex(&sealed_manifest.bytes);

    if let Some(credential) = credential.as_ref() {
        let required = session
            .metadata
            .signing
            .as_ref()
            .map(|signing| signing.required)
            .unwrap_or(false);
        configure_signing(&mut session.metadata, credential, required);
        let head = format::RepositoryHead {
            generation: sequence,
            latest_snapshot_id: manifest.snapshot_id.clone(),
            latest_manifest_sha256: manifest_sha256.clone(),
            latest_parent: parent.clone(),
            latest_entries_sha256: entries_sha256(&manifest)?,
            tombstones_sha256: tombstones_sha256(&session.metadata)?,
            updated_at: now_rfc3339(),
        };
        if let Some(signing) = session.metadata.signing.as_mut() {
            signing.head = Some(head);
        }
        format::sign_metadata(&mut session.metadata, session.crypto.as_ref())?;
        sign_repository_metadata(&mut session.metadata, credential)?;
        format::write_metadata_atomic(&session.root, &session.metadata)?;
    }
    let (signed, signing_key_id, signature_algorithm) = signing_summary(&session.metadata);

    let sessions = manifest
        .entries
        .iter()
        .filter(|entry| matches!(classify_archive_path(&entry.path), Ok(ArchiveKind::Session)))
        .count();
    let artifacts = manifest
        .entries
        .iter()
        .filter(|entry| {
            matches!(
                classify_archive_path(&entry.path),
                Ok(ArchiveKind::Artifact)
            )
        })
        .count();
    let anchor_update = match AnchorStore::new(&data_canonical).record_verified(
        &session.metadata.repository_id,
        &session.root.to_string_lossy(),
        Some(&manifest.snapshot_id),
        sequence,
        "backup",
        // A newly written snapshot is an authored event, not a replay. It may
        // have a lower sequence after an explicit rollback restore; record it
        // without lowering the high-water mark.
        true,
    ) {
        Ok(update) => Some(update),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "backup snapshot {} was written but the anti-rollback anchor could not be updated",
                    manifest.snapshot_id
                )
            });
        }
    };
    let report = RepositoryBackupReport {
        repository: session.root.to_string_lossy().to_string(),
        format_version: session.metadata.format_version,
        snapshot_format_version: SNAPSHOT_FORMAT_VERSION,
        snapshot_id: manifest.snapshot_id.clone(),
        sequence,
        parent,
        created_at: manifest.created_at.clone(),
        incremental: options.incremental,
        encrypted: session.metadata.encryption.enabled,
        includes_artifacts: options.include_artifacts,
        compression: options.compression.as_str().to_string(),
        entries: manifest.entries.len(),
        sessions,
        artifacts,
        bytes,
        written_bytes,
        deduplicated_bytes,
        deduplicated_from_parent_bytes,
        deduplicated_from_store_bytes,
        blobs_written,
        blobs_reused,
        stored_bytes,
        manifest_bytes: sealed_manifest.bytes.len() as u64,
        manifest_sha256: manifest_sha256.clone(),
        manifest_plaintext_sha256: sealed_manifest.plaintext_sha256.clone(),
        kdf: session.metadata.encryption.summary(),
        app_version: manifest.app_version.clone(),
        protocol_version: manifest.protocol_version,
        signed,
        signing_key_id: signing_key_id.clone(),
        signature_algorithm,
        anchor_update,
    };
    if let Err(error) = append_event(
        &session.root,
        "snapshot.created",
        json!({
            "snapshot_id": manifest.snapshot_id,
            "sequence": sequence,
            "parent": report.parent,
            "incremental": options.incremental,
            "encrypted": report.encrypted,
            "entries": report.entries,
            "bytes": report.bytes,
            "written_bytes": report.written_bytes,
            "deduplicated_bytes": report.deduplicated_bytes,
            "blobs_written": report.blobs_written,
            "blobs_reused": report.blobs_reused,
            "manifest_sha256": manifest_sha256,
            "manifest_plaintext_sha256": report.manifest_plaintext_sha256,
            "app_version": report.app_version,
            "protocol_version": report.protocol_version,
            "signed": report.signed,
            "signing_key_id": report.signing_key_id,
        }),
    ) {
        tracing::warn!(%error, "backup snapshot written but its audit event could not be appended");
    }
    Ok(report)
}

/// List and validate snapshots. Encrypted repositories require a passphrase.
///
/// `verify_blobs` decrypts and hashes every unique referenced blob. Without it,
/// list still validates metadata authenticity, manifests, the parent chain,
/// nonce table, blob filename shape, and blob presence.
pub fn list_repository_snapshots(
    repository: &Path,
    passphrase: Option<&Passphrase>,
    verify_blobs: bool,
) -> Result<RepositoryListReport> {
    list_repository_snapshots_with_keyring(repository, passphrase, None, verify_blobs)
}

/// List and validate snapshots with an optional public-key keyring.
pub fn list_repository_snapshots_with_keyring(
    repository: &Path,
    passphrase: Option<&Passphrase>,
    keyring: Option<&Keyring>,
    verify_blobs: bool,
) -> Result<RepositoryListReport> {
    list_repository_snapshots_with_policy(repository, passphrase, keyring, verify_blobs, false)
}

/// List and validate with an explicit revoked-key policy.
pub fn list_repository_snapshots_with_policy(
    repository: &Path,
    passphrase: Option<&Passphrase>,
    keyring: Option<&Keyring>,
    verify_blobs: bool,
    allow_revoked: bool,
) -> Result<RepositoryListReport> {
    let root = repository_root(repository, false)?;
    let trusted = keyring.map(|keyring| keyring.clone().with_revoked_keys_allowed(allow_revoked));
    let session = open_for_read(&root, passphrase, trusted.as_ref())?;
    let snapshots = scan_snapshots(
        &session.root,
        &session.metadata,
        session.crypto.as_ref(),
        session.keyring.as_ref(),
    )?;
    let on_disk = scan_blob_hashes(&session.root)?;
    let mut issues = Vec::new();
    let mut summaries = Vec::with_capacity(snapshots.len());
    let mut referenced: BTreeMap<String, (u64, Compression)> = BTreeMap::new();
    let mut logical_bytes = 0u64;
    let mut signed_snapshots = 0usize;
    let mut unsigned_snapshots = 0usize;
    for snapshot in &snapshots {
        let blobs = snapshot.blob_hashes();
        if snapshot.signature_check.is_verified() {
            signed_snapshots += 1;
        } else {
            unsigned_snapshots += 1;
        }
        for entry in &snapshot.manifest.entries {
            logical_bytes = logical_bytes
                .checked_add(entry.size)
                .ok_or_else(|| anyhow!("backup list byte total overflowed"))?;
            match referenced.get(&entry.sha256) {
                Some((size, compression)) => {
                    if *size != entry.size || *compression != entry.compression {
                        push_issue(
                            &mut issues,
                            format!(
                                "snapshot {} entry {:?} disagrees with another snapshot for blob {}",
                                snapshot.id(),
                                entry.path,
                                entry.sha256
                            ),
                        );
                    }
                }
                None => {
                    referenced.insert(entry.sha256.clone(), (entry.size, entry.compression));
                }
            }
        }
        summaries.push(RepositorySnapshotSummary {
            snapshot_id: snapshot.id().to_string(),
            sequence: snapshot.sequence(),
            parent: snapshot.parent().map(str::to_string),
            created_at: snapshot.manifest.created_at.clone(),
            incremental: snapshot.manifest.incremental,
            encrypted: snapshot.encrypted,
            includes_artifacts: snapshot.manifest.includes_artifacts,
            entries: snapshot.manifest.entries.len(),
            sessions: snapshot.sessions(),
            artifacts: snapshot.artifacts(),
            bytes: snapshot.manifest.totals.bytes,
            unique_blobs: blobs.len(),
            manifest_bytes: snapshot.manifest_envelope_bytes,
            manifest_sha256: snapshot.manifest_sha256.clone(),
            signed: snapshot.signature_check.is_verified(),
            signing_key_id: snapshot.signing_key_id().map(str::to_string),
            signature_status: snapshot.signature_status().to_string(),
        });
    }

    let mut blobs_present = 0usize;
    let mut blobs_missing = 0usize;
    let mut blobs_verified = 0usize;
    let mut blobs_corrupt = 0usize;
    let mut verified_cache = BTreeSet::new();
    let mut stored_blob_bytes = 0u64;
    for (hash, (size, compression)) in &referenced {
        match on_disk.get(hash) {
            Some(stored) => {
                blobs_present += 1;
                stored_blob_bytes = stored_blob_bytes.saturating_add(*stored);
                if verify_blobs && verified_cache.len() < BLOB_VERIFY_CACHE_LIMIT {
                    verified_cache.insert(hash.clone());
                    let path = blob_path(&session.root, hash)?;
                    match verify_blob_file(
                        &path,
                        hash,
                        *size,
                        *compression,
                        session.crypto.as_ref(),
                    ) {
                        Ok(_) => blobs_verified += 1,
                        Err(error) => {
                            blobs_corrupt += 1;
                            push_issue(
                                &mut issues,
                                format!("blob {hash} failed verification: {error:#}"),
                            );
                        }
                    }
                }
            }
            None => {
                blobs_missing += 1;
                push_issue(&mut issues, format!("blob {hash} is missing"));
            }
        }
    }
    let unreferenced: Vec<(&String, &u64)> = on_disk
        .iter()
        .filter(|(hash, _)| !referenced.contains_key(*hash))
        .collect();
    let unreferenced_blob_bytes = unreferenced
        .iter()
        .fold(0u64, |total, (_, size)| total.saturating_add(**size));
    let unique_plaintext_bytes = referenced.values().map(|(size, _)| *size).sum();
    let nonce_table_valid = true;
    let metadata_check =
        signing::verify_metadata_signature(&session.metadata, session.keyring.as_ref())?;
    let mut revoked_key_ids = BTreeSet::new();
    if metadata_check.is_revoked()
        && let Some(key_id) = &metadata_check.key_id
    {
        revoked_key_ids.insert(key_id.clone());
    }
    let mut revoked_signatures = usize::from(metadata_check.is_revoked());
    for snapshot in &snapshots {
        if snapshot.signature_check.is_revoked() {
            revoked_signatures += 1;
            if let Some(key_id) = &snapshot.signature_check.key_id {
                revoked_key_ids.insert(key_id.clone());
            }
        }
    }
    let signed = session.metadata.format_version >= 3;
    let signing_status = if signed { "signed" } else { "unsigned" }.to_string();
    let valid = blobs_missing == 0 && blobs_corrupt == 0 && issues.is_empty();
    Ok(RepositoryListReport {
        repository: session.root.to_string_lossy().to_string(),
        format_version: session.metadata.format_version,
        encrypted: session.metadata.encryption.enabled,
        kdf: session.metadata.encryption.summary(),
        created_at: session.metadata.created_at.clone(),
        snapshot_count: summaries.len(),
        snapshots: summaries,
        logical_bytes,
        unique_blobs: referenced.len(),
        unique_plaintext_bytes,
        stored_blob_bytes,
        unreferenced_blobs: unreferenced.len(),
        unreferenced_blob_bytes,
        nonce_records: session.nonce_records,
        validation: RepositoryValidationReport {
            valid,
            manifests_valid: true,
            chain_valid: true,
            nonce_table_valid,
            blob_verification: if verify_blobs { "full" } else { "presence" }.to_string(),
            blobs_present,
            blobs_missing,
            blobs_verified,
            blobs_corrupt,
            issues,
            signature_verification: if signed {
                "manifest-signatures".to_string()
            } else {
                "unsigned".to_string()
            },
            signed_snapshots,
            unsigned_snapshots,
            metadata_signature: metadata_check.status,
            revoked_signatures,
            revoked_key_ids: revoked_key_ids.into_iter().collect(),
        },
        signed,
        signing_status,
        keyring: session.keyring.as_ref().map(Keyring::summary),
    })
}

/// Restore one repository snapshot into a stopped data directory.
pub fn restore_repository_snapshot(
    repository: &Path,
    snapshot_id: &str,
    data_dir: &Path,
    force: bool,
    passphrase: Option<&Passphrase>,
) -> Result<RepositoryRestoreReport> {
    restore_repository_snapshot_checked(
        repository,
        snapshot_id,
        data_dir,
        force,
        RepositoryRestoreOptions {
            passphrase,
            ..RepositoryRestoreOptions::default()
        },
    )
}

/// Restore with signature verification, rollback protection, and an audit anchor.
pub fn restore_repository_snapshot_checked(
    repository: &Path,
    snapshot_id: &str,
    data_dir: &Path,
    force: bool,
    options: RepositoryRestoreOptions<'_>,
) -> Result<RepositoryRestoreReport> {
    if !is_snapshot_id(snapshot_id) {
        bail!("invalid backup snapshot id {snapshot_id:?}");
    }
    let root = repository_root(repository, false)?;
    let trusted = options.keyring.map(|keyring| {
        keyring
            .clone()
            .with_revoked_keys_allowed(options.allow_revoked)
    });
    let session = open_for_read(&root, options.passphrase, trusted.as_ref())?;
    let snapshots = scan_snapshots(
        &session.root,
        &session.metadata,
        session.crypto.as_ref(),
        session.keyring.as_ref(),
    )?;
    let snapshot = find_snapshot(&snapshots, snapshot_id)?;
    let mut revoked_warning_set = BTreeSet::new();
    for snapshot in &snapshots {
        if snapshot.signature_check.is_revoked()
            && let Some(warning) = snapshot.signature_check.warning()
        {
            revoked_warning_set.insert(warning);
        }
    }
    if let Some(signing) = &session.metadata.signing
        && let Some(keyring) = session.keyring.as_ref()
        && let Some(warning) = keyring.revocation_warning(&signing.active_key_id)
    {
        revoked_warning_set.insert(warning);
    }
    let revoked_signature_warnings: Vec<String> = revoked_warning_set.into_iter().collect();
    let head_issues = check_signed_head(&session.metadata, &snapshots);
    let rollback = match options.anchor {
        Some(anchor) => match anchor.check(
            &session.metadata.repository_id,
            snapshot.sequence(),
            options.allow_rollback,
        ) {
            Ok(check) => Some(check),
            Err(error) => {
                if error
                    .downcast_ref::<BackupSecurityError>()
                    .is_some_and(|error| {
                        matches!(error, BackupSecurityError::RollbackDetected { .. })
                    })
                    && let Err(event_error) = append_event(
                        &session.root,
                        "snapshot.rollback_rejected",
                        json!({
                            "repository_id": session.metadata.repository_id,
                            "generation": snapshot.sequence(),
                            "operation": "restore",
                            "allow_rollback": options.allow_rollback,
                            "issues": format!("{error:#}"),
                        }),
                    )
                {
                    tracing::warn!(%event_error, "rollback rejection audit event could not be appended");
                }
                return Err(error);
            }
        },
        None => None,
    };
    let data_dir = absolute_path(data_dir)?;
    // The anchor normally lives inside the data directory. A restore swaps the
    // whole directory, so preserve the anchor bytes across the swap before
    // recording the restored generation; otherwise an explicit rollback would
    // accidentally reset the high-water mark.
    let preserved_anchor = match options.anchor {
        Some(anchor) => {
            let anchor_path = absolute_path(anchor.path())?;
            if anchor_path.starts_with(&data_dir) {
                Some(anchor.read()?)
            } else {
                None
            }
        }
        None => None,
    };
    let future_target = resolve_future_path(&data_dir)?;
    reject_repository_inside_data(&session.root, &future_target)?;
    let (existing, target_parent) = prepare_restore_target(&data_dir, force)?;
    let mut scratch = ScratchPath::directory(&target_parent, "restore")?;
    let mut validated_blobs = BTreeSet::new();
    let mut stored_blob_bytes = 0u64;
    for entry in &snapshot.manifest.entries {
        classify_archive_path(&entry.path)?;
        let source = blob_path(&session.root, &entry.sha256)?;
        let destination = scratch.path.join(Path::new(&entry.path));
        if let Some(parent) = destination.parent() {
            vp_store::paths::ensure_private_dir(parent)?;
        }
        let verified = materialize_blob(
            &source,
            &destination,
            &entry.sha256,
            entry.size,
            entry.compression,
            session.crypto.as_ref(),
        )
        .with_context(|| format!("restoring backup entry {:?}", entry.path))?;
        if verified.plaintext_bytes != entry.size
            || verified.sha256 != entry.sha256
            || verified.compression != entry.compression
            || verified.encrypted != snapshot.encrypted
            || verified.stored_bytes != entry.stored_bytes
        {
            bail!(
                "restored backup entry {:?} did not verify consistently",
                entry.path
            );
        }
        if validated_blobs.insert(entry.sha256.clone()) {
            stored_blob_bytes = stored_blob_bytes.saturating_add(verified.stored_bytes);
        }
    }
    verify_sqlite_header(&scratch.path.join(DATABASE_FILENAME))?;
    if let Some(reason) = daemon_appears_running(&data_dir)? {
        bail!(
            "refusing to replace {} because a daemon appears to be running: {reason}",
            data_dir.display()
        );
    }
    let replaced_existing = existing;
    let mut previous = None;
    if existing {
        let aside = unique_sibling(&target_parent, "restore-old");
        fs::rename(&data_dir, &aside).with_context(|| {
            format!(
                "moving existing target {} aside to {}",
                data_dir.display(),
                aside.display()
            )
        })?;
        previous = Some(aside);
    }
    if let Err(error) = fs::rename(&scratch.path, &data_dir) {
        if let Some(previous) = &previous
            && let Err(restore_error) = fs::rename(previous, &data_dir)
        {
            return Err(error).with_context(|| {
                format!(
                    "moving restored data to {}, and the previous target could not be restored from {}: {restore_error}",
                    data_dir.display(),
                    previous.display()
                )
            });
        }
        return Err(error)
            .with_context(|| format!("moving restored data into {}", data_dir.display()));
    }
    scratch.disarm();
    if let Some(previous) = previous
        && let Err(error) = fs::remove_dir_all(&previous)
    {
        tracing::warn!(
            %error,
            path = %previous.display(),
            "restored data but could not remove the previous target"
        );
    }
    let anchor_update = match options.anchor {
        Some(anchor) => {
            if let Some(preserved) = &preserved_anchor {
                anchor.write(preserved).with_context(
                    || "restoring the anti-rollback anchor after the data-directory swap",
                )?;
            }
            Some(anchor.record_verified(
                &session.metadata.repository_id,
                &session.root.to_string_lossy(),
                Some(snapshot.id()),
                snapshot.sequence(),
                "restore",
                options.allow_rollback,
            )?)
        }
        None => None,
    };
    let (signed, signing_key_id, _) = signing_summary(&session.metadata);
    let report = RepositoryRestoreReport {
        repository: session.root.to_string_lossy().to_string(),
        snapshot_id: snapshot.id().to_string(),
        sequence: snapshot.sequence(),
        parent: snapshot.parent().map(str::to_string),
        created_at: snapshot.manifest.created_at.clone(),
        output: data_dir.to_string_lossy().to_string(),
        data_dir: data_dir.to_string_lossy().to_string(),
        format_version: session.metadata.format_version,
        encrypted: snapshot.encrypted,
        includes_artifacts: snapshot.manifest.includes_artifacts,
        entries: snapshot.manifest.entries.len(),
        sessions: snapshot.sessions(),
        artifacts: snapshot.artifacts(),
        bytes: snapshot.manifest.totals.bytes,
        validated_blobs: validated_blobs.len(),
        stored_blob_bytes,
        replaced_existing,
        kdf: session.metadata.encryption.summary(),
        signed,
        signing_key_id,
        signature_status: snapshot.signature_status().to_string(),
        rollback,
        anchor_update,
        head_issues,
        revoked_signature_warnings: revoked_signature_warnings.clone(),
    };
    if let Err(error) = append_event(
        &session.root,
        "snapshot.restored",
        json!({
            "snapshot_id": report.snapshot_id,
            "sequence": report.sequence,
            "output": report.output,
            "replaced_existing": report.replaced_existing,
            "entries": report.entries,
            "bytes": report.bytes,
            "signed": report.signed,
            "signing_key_id": report.signing_key_id,
            "rollback_allowed": report.rollback.as_ref().map(|rollback| rollback.rollback),
        }),
    ) {
        tracing::warn!(%error, "restore completed but its audit event could not be appended");
    }
    if !revoked_signature_warnings.is_empty()
        && let Err(error) = append_event(
            &session.root,
            "snapshot.revoked_key_allowed",
            json!({
                "repository_id": session.metadata.repository_id,
                "snapshot_id": report.snapshot_id,
                "sequence": report.sequence,
                "operation": "restore",
                "allow_revoked": options.allow_revoked,
                "warnings": revoked_signature_warnings,
            }),
        )
    {
        tracing::warn!(%error, "revoked-key restore audit event could not be appended");
    }
    Ok(report)
}

/// Prune old snapshots, retaining the latest `keep` by sequence.
///
/// Dry-run performs the same validation and planning but writes nothing. A
/// real prune records authenticated tombstones before deleting any manifest or
/// blob. Blobs referenced by retained manifests are never removed.
pub fn prune_repository(
    repository: &Path,
    keep: usize,
    dry_run: bool,
    passphrase: Option<&Passphrase>,
) -> Result<RepositoryPruneReport> {
    prune_repository_with_signing(repository, keep, dry_run, passphrase, None, None)
}

/// Prune snapshots and re-sign the repository metadata when it is signed.
pub fn prune_repository_with_signing(
    repository: &Path,
    keep: usize,
    dry_run: bool,
    passphrase: Option<&Passphrase>,
    keyring: Option<&Keyring>,
    signing_key: Option<&SigningCredential>,
) -> Result<RepositoryPruneReport> {
    if keep == 0 {
        bail!("--keep must be at least 1; prune never removes the latest snapshot boundary");
    }
    if keep > MAX_KEEP {
        bail!("--keep must be at most {MAX_KEEP}");
    }
    let root = repository_root(repository, false)?;
    let session = if dry_run {
        open_for_read(&root, passphrase, keyring)?
    } else {
        open_for_write_existing(&root, passphrase, keyring)?
    };
    let snapshots = scan_snapshots(
        &session.root,
        &session.metadata,
        session.crypto.as_ref(),
        session.keyring.as_ref(),
    )?;
    if !dry_run && session.metadata.format_version >= 3 && signing_key.is_none() {
        return Err(BackupSecurityError::SigningRequired.into());
    }
    if snapshots.is_empty() {
        bail!("backup repository has no snapshots to prune");
    }
    let total = snapshots.len();
    let split = total.saturating_sub(keep);
    let to_prune = &snapshots[..split];
    let kept = &snapshots[split..];
    let on_disk = scan_blob_hashes(&session.root)?;
    let mut retained_hashes = BTreeSet::new();
    for snapshot in kept {
        for entry in &snapshot.manifest.entries {
            retained_hashes.insert(entry.sha256.clone());
        }
    }
    let mut protected_bytes = 0u64;
    for hash in &retained_hashes {
        let Some(size) = on_disk.get(hash) else {
            bail!(
                "retained backup snapshot references blob {hash}, but the blob is missing; \
                 refusing to prune"
            );
        };
        let path = blob_path(&session.root, hash)?;
        let header = inspect_blob(&path)
            .with_context(|| format!("validating retained blob {hash} before prune"))?;
        if hex_lower(&header.sha256) != *hash {
            bail!("retained backup blob {hash} has a mismatched header hash");
        }
        if header.encrypted != session.metadata.encryption.enabled {
            bail!("retained backup blob {hash} encryption does not match the repository");
        }
        protected_bytes = protected_bytes.saturating_add(*size);
    }
    let unreferenced: Vec<(String, u64)> = on_disk
        .iter()
        .filter(|(hash, _)| !retained_hashes.contains(*hash))
        .map(|(hash, size)| (hash.clone(), *size))
        .collect();
    let mut pruned = Vec::with_capacity(to_prune.len());
    for snapshot in to_prune {
        pruned.push(PruneSnapshotReport {
            snapshot_id: snapshot.id().to_string(),
            sequence: snapshot.sequence(),
            parent: snapshot.parent().map(str::to_string),
            created_at: snapshot.manifest.created_at.clone(),
            bytes: snapshot.manifest.totals.bytes,
            manifest_bytes: snapshot.manifest_envelope_bytes,
        });
    }
    let unreferenced_unique: BTreeMap<String, u64> = unreferenced.iter().cloned().collect();
    let unreferenced_bytes: u64 = unreferenced_unique.values().copied().sum();
    // Pruned manifest envelopes plus every on-disk blob not referenced by a
    // retained snapshot. Shared blobs are counted once and never removed.
    let reclaimed_bytes = to_prune
        .iter()
        .map(|snapshot| snapshot.manifest_envelope_bytes)
        .sum::<u64>()
        .saturating_add(unreferenced_bytes);
    let signed_snapshots = snapshots
        .iter()
        .filter(|snapshot| snapshot.signature_check.is_verified())
        .count();
    let unsigned_snapshots = snapshots.len() - signed_snapshots;
    let validation = RepositoryValidationReport {
        valid: true,
        manifests_valid: true,
        chain_valid: true,
        nonce_table_valid: true,
        blob_verification: "retained-presence".to_string(),
        blobs_present: retained_hashes.len(),
        blobs_missing: 0,
        blobs_verified: 0,
        blobs_corrupt: 0,
        issues: Vec::new(),
        signature_verification: if session.metadata.format_version >= 3 {
            "manifest-signatures".to_string()
        } else {
            "unsigned".to_string()
        },
        signed_snapshots,
        unsigned_snapshots,
        metadata_signature: if session.metadata.format_version >= 3 {
            "verified".to_string()
        } else {
            "unsigned".to_string()
        },
        revoked_signatures: 0,
        revoked_key_ids: Vec::new(),
    };
    let (signed, signing_key_id, _) = signing_summary(&session.metadata);
    let report = RepositoryPruneReport {
        repository: session.root.to_string_lossy().to_string(),
        keep,
        dry_run,
        applied: !dry_run,
        snapshots_total: total,
        snapshots_kept: kept.len(),
        snapshots_pruned: pruned.clone(),
        blobs_kept: retained_hashes.len(),
        blobs_pruned: unreferenced_unique.len(),
        protected_bytes,
        reclaimed_bytes,
        validation,
        signed,
        signing_key_id,
    };
    if dry_run {
        return Ok(report);
    }

    let pruned_at = now_rfc3339();
    let mut metadata = session.metadata.clone();
    if !to_prune.is_empty() {
        for snapshot in to_prune {
            metadata.pruned_snapshots.push(PrunedSnapshot {
                id: snapshot.id().to_string(),
                sequence: snapshot.sequence(),
                created_at: snapshot.manifest.created_at.clone(),
                pruned_at: pruned_at.clone(),
                manifest_sha256: snapshot.manifest_sha256.clone(),
            });
        }
        metadata
            .pruned_snapshots
            .sort_by_key(|tombstone| tombstone.sequence);
        if metadata.pruned_snapshots.len() > MAX_KEEP {
            bail!("backup repository prune history exceeds the supported bound");
        }
        metadata.updated_at = pruned_at.clone();
        format::validate_chain(&metadata, &snapshots[split..])?;
        if metadata.format_version >= 3 {
            let credential = signing_key
                .ok_or_else(|| anyhow::Error::from(BackupSecurityError::SigningRequired))?;
            let required = metadata
                .signing
                .as_ref()
                .map(|signing| signing.required)
                .unwrap_or(true);
            configure_signing(&mut metadata, credential, required);
            let head = build_head(&metadata, kept.last())?;
            if let Some(signing) = metadata.signing.as_mut() {
                signing.head = head;
            }
            sign_metadata(&mut metadata, session.crypto.as_ref())?;
            sign_repository_metadata(&mut metadata, credential)?;
        } else {
            sign_metadata(&mut metadata, session.crypto.as_ref())?;
        }
        write_metadata_atomic(&session.root, &metadata)?;
    }
    // Metadata is now the logical commit point. Files below are cleanup; a
    // crash after this point leaves tombstoned files that scans ignore and a
    // later prune removes.
    for snapshot in to_prune {
        let path = snapshot_manifest_path(&session.root, snapshot.id())?;
        if let Err(error) = fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(error)
                .with_context(|| format!("removing pruned snapshot {}", path.display()));
        }
        if let Err(error) = append_event(
            &session.root,
            "snapshot.pruned",
            json!({
                "snapshot_id": snapshot.id(),
                "sequence": snapshot.sequence(),
                "manifest_sha256": snapshot.manifest_sha256,
                "pruned_at": pruned_at,
            }),
        ) {
            tracing::warn!(%error, snapshot = %snapshot.id(), "pruned snapshot audit event could not be appended");
        }
    }
    // Clean up manifests left by an earlier interrupted prune.
    for name in read_snapshot_filenames(&session.root)? {
        let Some(id) = snapshot_manifest_id_from_name(&name) else {
            continue;
        };
        if metadata.is_pruned(id).is_some() {
            let path = snapshot_manifest_path(&session.root, id)?;
            let _ = fs::remove_file(path);
        }
    }
    for hash in unreferenced_unique.keys() {
        let path = blob_path(&session.root, hash)?;
        if let Err(error) = fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(error)
                .with_context(|| format!("removing unreferenced blob {}", path.display()));
        }
    }
    if let Err(error) = append_event(
        &session.root,
        "prune.completed",
        json!({
            "keep": keep,
            "snapshots_pruned": pruned.len(),
            "blobs_pruned": unreferenced_unique.len(),
            "reclaimed_bytes": report.reclaimed_bytes,
            "protected_bytes": report.protected_bytes,
        }),
    ) {
        tracing::warn!(%error, "prune completed but its audit event could not be appended");
    }
    Ok(report)
}

#[derive(Clone)]
struct BlobDisposition {
    compression: Compression,
    stored_bytes: u64,
    plaintext_bytes: u64,
    newly_written: bool,
}

struct RepositorySession {
    root: PathBuf,
    metadata: RepositoryMetadata,
    crypto: Option<RepositoryCrypto>,
    keyring: Option<Keyring>,
    nonce_records: usize,
    _lock: RepositoryLock,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LockMode {
    Shared,
    Exclusive,
}

struct RepositoryLock {
    file: File,
    path: PathBuf,
}

impl RepositoryLock {
    fn acquire(repository: &Path, mode: LockMode) -> Result<Self> {
        let path = repository.join(LOCK_FILENAME);
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            if metadata.file_type().is_symlink() {
                bail!(
                    "refusing symlinked backup repository lock {}",
                    path.display()
                );
            }
            if !metadata.is_file() {
                bail!(
                    "backup repository lock {} is not a regular file",
                    path.display()
                );
            }
        }
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = options
            .open(&path)
            .with_context(|| format!("opening backup repository lock {}", path.display()))?;
        vp_store::paths::ensure_private_file(&path)?;
        match mode {
            LockMode::Exclusive => {
                if let Err(error) = file.try_lock_exclusive() {
                    bail!(
                        "backup repository is locked by another backup, restore, or prune operation ({}): {error}",
                        path.display()
                    );
                }
            }
            LockMode::Shared => {
                if let Err(error) = file.try_lock_shared() {
                    bail!(
                        "backup repository is locked by another backup, restore, or prune operation ({}): {error}",
                        path.display()
                    );
                }
            }
        }
        Ok(Self { file, path })
    }
}

impl Drop for RepositoryLock {
    fn drop(&mut self) {
        if let Err(error) = FileExt::unlock(&self.file) {
            tracing::warn!(
                %error,
                path = %self.path.display(),
                "failed to release backup repository lock"
            );
        }
    }
}

fn repository_root(repository: &Path, create: bool) -> Result<PathBuf> {
    match fs::symlink_metadata(repository) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!(
                "refusing symlinked backup repository {}",
                repository.display()
            );
        }
        Ok(metadata) if !metadata.is_dir() => {
            bail!(
                "backup repository is not a directory: {}",
                repository.display()
            );
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !create => {
            bail!("backup repository not found at {}", repository.display());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            vp_store::paths::ensure_private_dir(repository)
                .with_context(|| format!("creating backup repository {}", repository.display()))?;
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("reading backup repository {}", repository.display()));
        }
    }
    fs::canonicalize(repository)
        .with_context(|| format!("resolving backup repository {}", repository.display()))
}

fn reject_repository_inside_data(repository: &Path, data_dir: &Path) -> Result<()> {
    if repository == data_dir
        || repository.starts_with(data_dir)
        || data_dir.starts_with(repository)
    {
        bail!(
            "backup repository {} must not overlap data directory {}",
            repository.display(),
            data_dir.display()
        );
    }
    Ok(())
}

fn open_for_write(
    root: &Path,
    encryption: &RepositoryEncryption,
    kdf: KdfConfig,
    keyring: Option<&Keyring>,
    credential: Option<&SigningCredential>,
) -> Result<RepositorySession> {
    let lock = RepositoryLock::acquire(root, LockMode::Exclusive)?;
    cleanup_stale_metadata_temps(root)?;
    let passphrase = match encryption {
        RepositoryEncryption::Encrypted(passphrase) => Some(passphrase),
        RepositoryEncryption::Plaintext => None,
    };
    let metadata = match fs::symlink_metadata(root.join(METADATA_FILENAME)) {
        Ok(_) => Some(parse_metadata(&read_metadata_bytes(root)?)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| {
                format!("reading backup repository metadata in {}", root.display())
            });
        }
    };
    let (metadata, crypto) = match metadata {
        Some(metadata) => {
            let crypto = unlock_repository(&metadata, passphrase)?;
            verify_metadata_authenticity(&metadata, crypto.as_ref())?;
            signing::verify_metadata_signature(&metadata, keyring)?;
            ensure_layout(root)?;
            (metadata, crypto)
        }
        None => {
            ensure_empty_for_initialization(root)?;
            let encryption_metadata = match encryption {
                RepositoryEncryption::Encrypted(_) => {
                    let kdf = KdfParameters::generate(kdf)?;
                    format::EncryptionMetadata::encrypted(kdf)
                }
                RepositoryEncryption::Plaintext => format::EncryptionMetadata::plaintext(),
            };
            let mut metadata = new_metadata(encryption_metadata);
            let crypto = unlock_repository(&metadata, passphrase)?;
            if let Some(credential) = credential {
                configure_signing(&mut metadata, credential, true);
                sign_metadata(&mut metadata, crypto.as_ref())?;
                sign_repository_metadata(&mut metadata, credential)?;
            } else {
                sign_metadata(&mut metadata, crypto.as_ref())?;
            }
            write_metadata_atomic(root, &metadata)?;
            ensure_layout(root)?;
            if let Err(error) = append_event(
                root,
                "repository.created",
                json!({
                    "repository_id": metadata.repository_id,
                    "encrypted": metadata.encryption.enabled,
                    "kdf": metadata.encryption.summary(),
                    "app_version": metadata.app_version,
                    "protocol_version": metadata.protocol_version,
                    "signed": metadata.format_version >= 3,
                    "signing_key_id": metadata.signing.as_ref().map(|signing| signing.active_key_id.clone()),
                }),
            ) {
                tracing::warn!(%error, "backup repository created but its audit event could not be appended");
            }
            (metadata, crypto)
        }
    };
    let nonce_records = read_nonce_table(root, metadata.encryption.enabled)?;
    Ok(RepositorySession {
        root: root.to_path_buf(),
        metadata,
        crypto,
        keyring: keyring.cloned(),
        nonce_records,
        _lock: lock,
    })
}

fn open_for_write_existing(
    root: &Path,
    passphrase: Option<&Passphrase>,
    keyring: Option<&Keyring>,
) -> Result<RepositorySession> {
    let lock = RepositoryLock::acquire(root, LockMode::Exclusive)?;
    cleanup_stale_metadata_temps(root)?;
    let bytes = read_metadata_bytes(root)?;
    let metadata = parse_metadata(&bytes)?;
    let crypto = unlock_repository(&metadata, passphrase)?;
    verify_metadata_authenticity(&metadata, crypto.as_ref())?;
    signing::verify_metadata_signature(&metadata, keyring)?;
    ensure_layout(root)?;
    let nonce_records = read_nonce_table(root, metadata.encryption.enabled)?;
    Ok(RepositorySession {
        root: root.to_path_buf(),
        metadata,
        crypto,
        keyring: keyring.cloned(),
        nonce_records,
        _lock: lock,
    })
}

fn open_for_read(
    root: &Path,
    passphrase: Option<&Passphrase>,
    keyring: Option<&Keyring>,
) -> Result<RepositorySession> {
    let lock = RepositoryLock::acquire(root, LockMode::Shared)?;
    let bytes = read_metadata_bytes(root)
        .with_context(|| format!("{} is not an initialized backup repository", root.display()))?;
    let metadata = parse_metadata(&bytes)?;
    let crypto = unlock_repository(&metadata, passphrase)?;
    verify_metadata_authenticity(&metadata, crypto.as_ref())?;
    signing::verify_metadata_signature(&metadata, keyring)?;
    let nonce_records = read_nonce_table(root, metadata.encryption.enabled)?;
    Ok(RepositorySession {
        root: root.to_path_buf(),
        metadata,
        crypto,
        keyring: keyring.cloned(),
        nonce_records,
        _lock: lock,
    })
}

fn unlock_repository(
    metadata: &RepositoryMetadata,
    passphrase: Option<&Passphrase>,
) -> Result<Option<RepositoryCrypto>> {
    if metadata.encryption.enabled {
        let Some(passphrase) = passphrase else {
            bail!(
                "backup repository is encrypted; provide --passphrase-env NAME or \
                 --passphrase-file PATH"
            );
        };
        let kdf = metadata
            .encryption
            .kdf
            .as_ref()
            .ok_or_else(|| anyhow!("encrypted backup repository is missing KDF parameters"))?;
        Ok(Some(RepositoryCrypto::derive(passphrase, kdf)?))
    } else {
        if passphrase.is_some() {
            bail!("backup repository is not encrypted; omit the passphrase option");
        }
        Ok(None)
    }
}

fn cleanup_stale_metadata_temps(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root).with_context(|| format!("reading {}", root.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(".verlinpark-metadata-") {
            continue;
        }
        let path = entry.path();
        if entry.file_type()?.is_file() {
            fs::remove_file(&path)
                .with_context(|| format!("removing stale metadata staging {}", path.display()))?;
        }
    }
    Ok(())
}

fn ensure_layout(root: &Path) -> Result<()> {
    for directory in [SNAPSHOTS_DIR, BLOBS_DIR, TEMP_DIR] {
        let path = root.join(directory);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!(
                    "refusing symlinked backup repository directory {}",
                    path.display()
                );
            }
            Ok(metadata) if !metadata.is_dir() => {
                bail!(
                    "backup repository directory {} is not a directory",
                    path.display()
                );
            }
            Ok(_) => vp_store::paths::ensure_private_dir(&path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                vp_store::paths::ensure_private_dir(&path)?;
            }
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", path.display()));
            }
        }
    }
    let temp = root.join(TEMP_DIR);
    for entry in fs::read_dir(&temp).with_context(|| format!("reading {}", temp.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(".verlinpark-") {
            continue;
        }
        let path = entry.path();
        let metadata = entry.metadata()?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            fs::remove_dir_all(&path)
                .with_context(|| format!("removing stale backup staging {}", path.display()))?;
        } else {
            fs::remove_file(&path)
                .with_context(|| format!("removing stale backup staging {}", path.display()))?;
        }
    }
    for file in [NONCES_FILENAME, EVENTS_FILENAME] {
        let path = root.join(file);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!(
                    "refusing symlinked backup repository log {}",
                    path.display()
                );
            }
            Ok(metadata) if !metadata.is_file() => {
                bail!(
                    "backup repository log {} is not a regular file",
                    path.display()
                );
            }
            Ok(_) => vp_store::paths::ensure_private_file(&path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                write_private(&path, b"")?;
            }
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", path.display()));
            }
        }
    }
    Ok(())
}

fn ensure_empty_for_initialization(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root).with_context(|| format!("reading {}", root.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        if name == LOCK_FILENAME {
            continue;
        }
        bail!(
            "refusing to initialize backup repository in non-empty directory {} (found {:?})",
            root.display(),
            name
        );
    }
    Ok(())
}

fn validate_database_source(database: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(database)
        .with_context(|| format!("reading {}", database.display()))?;
    if metadata.file_type().is_symlink() {
        bail!(
            "refusing to archive symlinked database {}",
            database.display()
        );
    }
    if !metadata.is_file() {
        bail!(
            "no database at {}; start the daemon once before backing up",
            database.display()
        );
    }
    Ok(())
}

fn publish_immutable(final_path: &Path, scratch: &mut ScratchPath) -> Result<()> {
    if let Some(parent) = final_path.parent() {
        vp_store::paths::ensure_private_dir(parent)?;
    }
    let scratch_path = scratch.path.clone();
    match fs::hard_link(&scratch_path, final_path) {
        Ok(()) => {
            let _ = fs::remove_file(&scratch_path);
            scratch.disarm();
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            bail!(
                "refusing to overwrite immutable backup object {}",
                final_path.display()
            );
        }
        Err(error) => Err(error).with_context(|| {
            format!(
                "publishing immutable backup object {} from {}",
                final_path.display(),
                scratch_path.display()
            )
        }),
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .context("resolving the current directory")?
            .join(path))
    }
}

fn resolve_future_path(path: &Path) -> Result<PathBuf> {
    let absolute = absolute_path(path)?;
    let mut ancestor = absolute.as_path();
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| anyhow!("cannot resolve future path {}", path.display()))?;
    }
    let canonical =
        fs::canonicalize(ancestor).with_context(|| format!("resolving {}", ancestor.display()))?;
    let suffix = absolute
        .strip_prefix(ancestor)
        .unwrap_or_else(|_| Path::new(""));
    Ok(canonical.join(suffix))
}

fn prepare_restore_target(data_dir: &Path, force: bool) -> Result<(bool, PathBuf)> {
    if data_dir.file_name().is_none() {
        bail!("restore target must be a directory path");
    }
    let existing = match fs::symlink_metadata(data_dir) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                bail!(
                    "refusing to restore over symlinked target {}",
                    data_dir.display()
                );
            }
            if !metadata.is_dir() {
                bail!("restore target {} is not a directory", data_dir.display());
            }
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", data_dir.display()));
        }
    };
    if let Some(reason) = daemon_appears_running(data_dir)? {
        bail!(
            "refusing to restore while a daemon appears to be running: {reason}. \
             Stop verlinpadd for {} and retry; restore never stops a daemon.",
            data_dir.display()
        );
    }
    let had_contents = existing
        && fs::read_dir(data_dir)
            .with_context(|| format!("reading {}", data_dir.display()))?
            .next()
            .is_some();
    if existing && !force {
        if data_dir.join(DATABASE_FILENAME).exists() {
            bail!(
                "target {} contains an existing database; pass --force to replace it",
                data_dir.display()
            );
        }
        if data_dir.join("run").join("daemon.lock").exists() {
            bail!(
                "target {} contains an instance lock; pass --force to replace it",
                data_dir.display()
            );
        }
        if had_contents {
            bail!(
                "target directory {} is not empty; pass --force to replace it",
                data_dir.display()
            );
        }
    }
    let parent = non_empty_parent(data_dir);
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let parent =
        fs::canonicalize(parent).with_context(|| format!("resolving {}", parent.display()))?;
    Ok((existing, parent))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NonceRecord {
    kind: String,
    object_id: String,
    snapshot_id: Option<String>,
    nonce_b64: String,
    created_at: String,
}

fn read_snapshot_filenames(root: &Path) -> Result<Vec<String>> {
    let directory = root.join(SNAPSHOTS_DIR);
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("refusing symlinked backup snapshots directory");
        }
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => bail!("backup snapshots path is not a directory"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("reading backup snapshots directory"),
    }
    let mut names = Vec::new();
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        if let Ok(name) = entry.file_name().into_string() {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

fn read_nonce_table(root: &Path, encrypted: bool) -> Result<usize> {
    let path = root.join(NONCES_FILENAME);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("refusing symlinked backup nonce table {}", path.display());
        }
        Ok(metadata) if !metadata.is_file() => {
            bail!(
                "backup nonce table {} is not a regular file",
                path.display()
            );
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", path.display()));
        }
    }
    let mut bytes = Vec::new();
    match File::open(&path) {
        Ok(file) => {
            file.take(MAX_NONCE_TABLE_BYTES + 1)
                .read_to_end(&mut bytes)
                .with_context(|| format!("reading {}", path.display()))?;
            if bytes.len() as u64 > MAX_NONCE_TABLE_BYTES {
                bail!("backup nonce table exceeds its size limit");
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if encrypted {
                return Ok(0);
            }
            return Ok(0);
        }
        Err(error) => {
            return Err(error).with_context(|| format!("opening {}", path.display()));
        }
    }
    let complete = if bytes.last() == Some(&b'\n') {
        bytes.as_slice()
    } else if let Some(last_newline) = bytes.iter().rposition(|byte| *byte == b'\n') {
        // A daemon-less CLI can be interrupted mid-append. Ignore only an
        // incomplete final line, never a malformed complete line.
        &bytes[..=last_newline]
    } else {
        &[]
    };
    let mut seen_nonces = BTreeSet::new();
    let mut count = 0usize;
    for (index, line) in complete.split(|byte| *byte == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        let record: NonceRecord = serde_json::from_slice(line)
            .with_context(|| format!("parsing backup nonce record #{}", index + 1))?;
        match record.kind.as_str() {
            "blob" => {
                if record.object_id.len() != 64
                    || !record
                        .object_id
                        .bytes()
                        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
                {
                    bail!("backup nonce table has an invalid blob id");
                }
            }
            "manifest" => {
                if !is_snapshot_id(&record.object_id) {
                    bail!("backup nonce table has an invalid manifest id");
                }
            }
            other => bail!("backup nonce table has an unknown object kind {other:?}"),
        }
        if let Some(snapshot_id) = &record.snapshot_id
            && !is_snapshot_id(snapshot_id)
        {
            bail!("backup nonce table has an invalid snapshot id");
        }
        let nonce = BASE64
            .decode(record.nonce_b64.as_bytes())
            .context("backup nonce table has an invalid base64 nonce")?;
        if nonce.len() != 12 {
            bail!("backup nonce table has a nonce with the wrong length");
        }
        if !seen_nonces.insert(nonce) {
            bail!("backup nonce table contains a duplicate nonce");
        }
        if record.created_at.is_empty() || record.created_at.len() > 64 {
            bail!("backup nonce table has a malformed timestamp");
        }
        count += 1;
    }
    let _ = encrypted;
    Ok(count)
}

fn append_nonce(
    root: &Path,
    kind: &str,
    object_id: &str,
    snapshot_id: Option<&str>,
    nonce: &[u8; 12],
    created_at: &str,
) -> Result<()> {
    if nonce == &[0u8; 12] {
        bail!("refusing to record an all-zero backup nonce for encrypted data");
    }
    let record = NonceRecord {
        kind: kind.to_string(),
        object_id: object_id.to_string(),
        snapshot_id: snapshot_id.map(str::to_string),
        nonce_b64: BASE64.encode(nonce),
        created_at: created_at.to_string(),
    };
    append_json_line(root, NONCES_FILENAME, &record)
}

fn append_event(root: &Path, event: &str, details: Value) -> Result<()> {
    let record = json!({
        "event": event,
        "created_at": now_rfc3339(),
        "details": details,
    });
    append_json_line(root, EVENTS_FILENAME, &record)
}

fn append_json_line<T: Serialize>(root: &Path, filename: &str, record: &T) -> Result<()> {
    let path = root.join(filename);
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        if metadata.file_type().is_symlink() {
            bail!(
                "refusing symlinked backup repository log {}",
                path.display()
            );
        }
        if !metadata.is_file() {
            bail!(
                "backup repository log {} is not a regular file",
                path.display()
            );
        }
    }
    let mut line = serde_json::to_vec(record).context("serializing backup repository log")?;
    if line.len() > 1024 * 1024 {
        bail!("backup repository log record is too large");
    }
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
        .with_context(|| format!("opening backup repository log {}", path.display()))?;
    vp_store::paths::ensure_private_file(&path)?;
    file.write_all(&line)
        .with_context(|| format!("appending to {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("syncing {}", path.display()))?;
    Ok(())
}

fn push_issue(issues: &mut Vec<String>, issue: String) {
    if issues.len() < MAX_REPORT_ISSUES {
        issues.push(issue);
    } else if issues.len() == MAX_REPORT_ISSUES {
        issues.push("additional validation issues omitted".to_string());
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

// Keep imports referenced only by test or future format validation visible to
// the compiler without weakening the public surface.
#[allow(dead_code)]
fn _format_symbols() {
    let _ = (
        METADATA_FILENAME,
        BLOBS_DIR,
        BLOB_HEADER_BYTES,
        BLOB_CHUNK_RECORD_BYTES,
        canonical_metadata_bytes,
        load_snapshot,
        validate_chain,
        write_metadata_atomic,
        scan_blob_hashes,
        blob_hash_from_name,
    );
}

#[cfg(test)]
mod rekey_tests;
#[cfg(test)]
mod security_tests;
#[cfg(test)]
mod tests;
