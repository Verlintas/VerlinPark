//! Re-sign a signed v3 repository with the keyring's active key.
//!
//! # What rekey does
//!
//! `vp backup-rekey` is the recovery path after a signing key is retired,
//! revoked, or lost. It rewrites only the *signatures*:
//!
//! - every signed snapshot manifest is opened with the repository passphrase,
//!   re-signed with the active key, re-sealed with a fresh AEAD nonce, and
//!   atomically replaced;
//! - the repository metadata MAC is recomputed and the metadata is re-signed,
//!   with a signed head that matches the newly sealed latest manifest;
//! - encrypted blob files are never opened for writing. Their ciphertext
//!   bytes and nonce records are untouched.
//!
//! The repository id, format version, generation sequence, parent chain,
//! entry table, prune tombstones, KDF parameters, and every blob address are
//! preserved. Historical snapshot generations are not renumbered.
//!
//! # Idempotence and resumability
//!
//! A manifest already signed by the active key is skipped, so a second run is
//! byte-for-byte a no-op once the metadata signature is current. Each manifest
//! is published by `rename` under the repository's exclusive lock, and the
//! metadata write is the final commit point. If the process is interrupted,
//! the repository contains a mix of signatures that are each valid under the
//! keyring's public keys; running `vp backup-rekey` again completes the job.
//!
//! # What rekey refuses
//!
//! v1 archives, v2 repositories, v3 metadata without a signing block, a
//! keyring without a private key for the active key, and a revoked active key
//! are all rejected with typed codes. Historical signatures made by a revoked
//! key are accepted as the *source* of a rekey because replacing exactly those
//! signatures is the point of the operation; every resulting signature uses
//! the active, non-revoked key.
//!
//! # Secret handling
//!
//! The passphrase is only used to derive the repository keys in memory.
//! Rekey reports contain key ids, public-key summaries, counts, and digests;
//! they never contain a seed, passphrase, derived key, or plaintext.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use vp_protocol::now_rfc3339;

use super::format::{
    TEMP_DIR, parse_metadata, read_metadata_bytes, sign_metadata, snapshot_manifest_path,
    verify_metadata_authenticity, write_metadata_atomic,
};
use super::signing::{
    build_head, check_signed_head, configure_signing, sign_repository_metadata,
    sign_snapshot_manifest, verify_metadata_signature,
};
use super::{
    Passphrase, RepositoryLock, ScratchPath, append_event, append_nonce,
    cleanup_stale_metadata_temps, ensure_layout, repository_root, scan_blob_hashes, scan_snapshots,
    unlock_repository, write_private,
};
use crate::backup::error::BackupSecurityError;
use crate::backup::signing::{Keyring, KeyringSummary, SigningCredential};

/// Maximum number of snapshots one rekey operation will rewrite.
///
/// The bound keeps a maintenance operation predictable. Repositories with
/// more snapshots must be rekeyed in explicit batches after pruning; the
/// operation is resumable, so a larger limit is never silently assumed.
pub const MAX_REKEY_SNAPSHOTS: usize = 10_000;
/// Maximum total size of manifest envelopes rekey will read in one pass.
pub const MAX_REKEY_MANIFEST_BYTES: u64 = 512 * 1024 * 1024;

/// Options for a repository rekey.
pub struct RepositoryRekeyOptions<'a> {
    pub passphrase: Option<&'a Passphrase>,
    pub keyring: &'a Keyring,
    /// Report what would change without writing anything.
    pub dry_run: bool,
    /// Optional batch bound for this run. `None` uses [`MAX_REKEY_SNAPSHOTS`].
    /// A smaller bound lets a large repository be rekeyed in resumable batches.
    pub max_snapshots: Option<usize>,
}

impl std::fmt::Debug for RepositoryRekeyOptions<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RepositoryRekeyOptions")
            .field("passphrase", &self.passphrase.map(|_| "<redacted>"))
            .field("keyring", &self.keyring.summary().path)
            .field("dry_run", &self.dry_run)
            .field("max_snapshots", &self.max_snapshots)
            .finish()
    }
}

/// Result of a rekey attempt.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepositoryRekeyReport {
    pub repository: String,
    pub repository_id: String,
    pub format_version: u32,
    pub encrypted: bool,
    pub dry_run: bool,
    pub applied: bool,
    /// True when signatures were (or, in dry-run mode, would be) rewritten.
    pub changed: bool,
    /// Metadata signing key before the operation.
    pub previous_key_id: String,
    /// Active key all resulting signatures use.
    pub key_id: String,
    /// Every key id that signed the repository before the operation.
    #[serde(default)]
    pub previous_key_ids: Vec<String>,
    /// Key ids that are revoked and whose historical signatures were replaced.
    #[serde(default)]
    pub revoked_source_key_ids: Vec<String>,
    pub metadata_resigned: bool,
    pub snapshots_total: usize,
    /// Snapshots that still carried a non-active signature when the run began.
    pub snapshots_pending: usize,
    pub snapshots_resigned: usize,
    pub snapshots_remaining: usize,
    /// True when no signed snapshot still points at a non-active key.
    pub complete: bool,
    pub max_snapshots: usize,
    pub snapshots_unchanged: usize,
    /// Unsigned historical snapshots from a v2-to-v3 upgrade. They are kept
    /// unsigned rather than retroactively certified.
    pub snapshots_unsigned_preserved: usize,
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_snapshot_id: Option<String>,
    pub parent_chain_preserved: bool,
    pub entries_preserved: bool,
    pub tombstones_preserved: bool,
    pub blobs_total: usize,
    pub blob_bytes: u64,
    /// SHA-256 over the sorted `(blob address, ciphertext file hash, size)`
    /// table. The same digest before and after proves blobs were not rewritten.
    pub blob_ciphertext_sha256: String,
    pub blob_ciphertext_unchanged: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyring: Option<KeyringSummary>,
    pub started_at: String,
    pub completed_at: String,
}

/// Re-sign a v3 repository with the active keyring key.
pub fn rekey_repository(
    repository: &Path,
    options: RepositoryRekeyOptions<'_>,
) -> Result<RepositoryRekeyReport> {
    let started_at = now_rfc3339();
    let root = repository_root(repository, false)?;
    let _lock = RepositoryLock::acquire(&root, super::LockMode::Exclusive)?;
    cleanup_stale_metadata_temps(&root)?;
    ensure_layout(&root)?;

    let metadata = parse_metadata(&read_metadata_bytes(&root)?)?;
    if metadata.format_version < 3 {
        return Err(BackupSecurityError::UnsignedRepository {
            format_version: metadata.format_version,
        }
        .into());
    }
    let Some(signing) = metadata.signing.as_ref() else {
        return Err(BackupSecurityError::RekeyRefused {
            reason: "v3 repository metadata has no signing block".to_string(),
        }
        .into());
    };
    if signing.metadata_signature.is_none() {
        return Err(BackupSecurityError::SignatureMissing {
            object: "repository metadata".to_string(),
        }
        .into());
    }
    let previous_key_id = signing.active_key_id.clone();
    let required = signing.required;

    // Target key selection is deliberately narrow: the active key only. A
    // repository cannot be rekeyed to an arbitrary retired key without an
    // explicit `backup-key-rotate` first, which makes the trust transition
    // visible in the keyring audit log.
    let batch_limit = options.max_snapshots.unwrap_or(MAX_REKEY_SNAPSHOTS);
    if batch_limit == 0 || batch_limit > MAX_REKEY_SNAPSHOTS {
        return Err(BackupSecurityError::RekeyRefused {
            reason: format!(
                "max_snapshots must be between 1 and {MAX_REKEY_SNAPSHOTS}; got {batch_limit}"
            ),
        }
        .into());
    }
    let key_id = options
        .keyring
        .active_key_id()
        .ok_or_else(|| anyhow::Error::from(BackupSecurityError::SigningRequired))?
        .to_string();
    options.keyring.ensure_not_revoked(&key_id)?;
    let credential: SigningCredential = options
        .keyring
        .signing_credential(Some(&key_id))
        .context("loading the active backup signing credential for rekey")?;
    let crypto = unlock_repository(&metadata, options.passphrase)?;
    verify_metadata_authenticity(&metadata, crypto.as_ref())?;

    // Source signatures may legitimately be signed by a revoked key. Accept
    // them for the *read* side, and prove each one still verifies before
    // replacing it.
    let source_keyring = options.keyring.clone().with_revoked_keys_allowed(true);
    let source_metadata_check = verify_metadata_signature(&metadata, Some(&source_keyring))?;
    if !source_metadata_check.is_verified() {
        return Err(BackupSecurityError::SignatureMissing {
            object: "repository metadata".to_string(),
        }
        .into());
    }
    let snapshots = scan_snapshots(&root, &metadata, crypto.as_ref(), Some(&source_keyring))?;
    if snapshots.len() > MAX_REKEY_SNAPSHOTS {
        return Err(BackupSecurityError::RekeyRefused {
            reason: format!(
                "repository has {} snapshots; rekey is bounded to {MAX_REKEY_SNAPSHOTS}",
                snapshots.len()
            ),
        }
        .into());
    }
    let manifest_bytes: u64 = snapshots
        .iter()
        .try_fold(0u64, |total, snapshot| {
            total.checked_add(snapshot.manifest_envelope_bytes)
        })
        .ok_or_else(|| anyhow!("backup manifest byte total overflowed"))?;
    if manifest_bytes > MAX_REKEY_MANIFEST_BYTES {
        return Err(BackupSecurityError::RekeyRefused {
            reason: format!(
                "repository manifests total {manifest_bytes} bytes; rekey is bounded to {MAX_REKEY_MANIFEST_BYTES}"
            ),
        }
        .into());
    }

    let (blobs_total, blob_bytes, blob_digest_before) = blob_ciphertext_fingerprint(&root)?;
    let mut previous_key_ids = BTreeSet::new();
    let mut revoked_source_key_ids = BTreeSet::new();
    previous_key_ids.insert(previous_key_id.clone());
    if source_metadata_check.is_revoked() {
        revoked_source_key_ids.insert(previous_key_id.clone());
    }
    for snapshot in &snapshots {
        if let Some(key) = snapshot.signing_key_id() {
            previous_key_ids.insert(key.to_string());
            if snapshot.signature_check.is_revoked() {
                revoked_source_key_ids.insert(key.to_string());
            }
        }
    }

    let pending_total = snapshots
        .iter()
        .filter(|snapshot| {
            snapshot
                .manifest
                .signature
                .as_ref()
                .is_some_and(|signature| signature.key_id != key_id)
        })
        .count();
    let mut snapshots_resigned = 0usize;
    let mut snapshots_unchanged = 0usize;
    let mut snapshots_unsigned_preserved = 0usize;
    let staging = root.join(TEMP_DIR);
    for snapshot in &snapshots {
        let Some(signature) = snapshot.manifest.signature.as_ref() else {
            // A mixed v2-upgrade chain keeps unsigned history unsigned. Signing
            // it now would retroactively certify bytes nobody authenticated.
            snapshots_unsigned_preserved += 1;
            continue;
        };
        if signature.key_id == key_id && snapshot.signature_check.is_verified() {
            snapshots_unchanged += 1;
            continue;
        }
        if snapshots_resigned >= batch_limit {
            continue;
        }
        let mut manifest = snapshot.manifest.clone();
        let entries_before = serde_json::to_vec(&manifest.entries)
            .context("serializing pre-rekey manifest entries")?;
        sign_snapshot_manifest(&metadata.repository_id, &credential, &mut manifest)?;
        let entries_after = serde_json::to_vec(&manifest.entries)
            .context("serializing post-rekey manifest entries")?;
        if entries_before != entries_after
            || manifest.snapshot_id != snapshot.manifest.snapshot_id
            || manifest.sequence != snapshot.manifest.sequence
            || manifest.parent != snapshot.manifest.parent
            || manifest.created_at != snapshot.manifest.created_at
            || manifest.totals != snapshot.manifest.totals
        {
            return Err(BackupSecurityError::RekeyFailed {
                reason: format!(
                    "re-signing snapshot {} changed immutable manifest fields",
                    snapshot.manifest.snapshot_id
                ),
            }
            .into());
        }
        snapshots_resigned += 1;
        if options.dry_run {
            continue;
        }
        let plaintext =
            serde_json::to_vec(&manifest).context("serializing rekeyed backup manifest")?;
        let sealed = match crypto.as_ref() {
            Some(crypto) => {
                crypto.seal_manifest(&metadata.repository_id, &manifest.snapshot_id, &plaintext)?
            }
            None => crate::backup::crypto::seal_manifest_plain(
                &metadata.repository_id,
                &manifest.snapshot_id,
                &plaintext,
            )?,
        };
        if sealed.bytes.is_empty() {
            return Err(BackupSecurityError::RekeyFailed {
                reason: format!(
                    "re-sealing snapshot {} produced an empty envelope",
                    manifest.snapshot_id
                ),
            }
            .into());
        }
        if crypto.is_some() {
            append_nonce(
                &root,
                "manifest",
                &manifest.snapshot_id,
                Some(&manifest.snapshot_id),
                &sealed.nonce,
                &now_rfc3339(),
            )?;
        }
        let final_path = snapshot_manifest_path(&root, &manifest.snapshot_id)?;
        let mut scratch = ScratchPath::file(&staging, "rekey-manifest")?;
        write_private(&scratch.path, &sealed.bytes)?;
        fs::rename(&scratch.path, &final_path).with_context(|| {
            format!(
                "publishing rekeyed snapshot manifest {}",
                final_path.display()
            )
        })?;
        scratch.disarm();
        File::open(&root)
            .and_then(|directory| directory.sync_all())
            .ok();
    }

    // Metadata is the final commit point. Rebuild the signed head from the
    // manifests that are actually on disk now; re-sealing changes each
    // manifest envelope hash even though the logical entries do not change.
    // A pre-existing stale head is repaired even when no signature changes.
    let initial_head_issues = check_signed_head(&metadata, &snapshots);
    let mut metadata_resigned = snapshots_resigned > 0
        || previous_key_id != key_id
        || !source_metadata_check.is_verified()
        || !initial_head_issues.is_empty();
    if !options.dry_run {
        let reloaded = scan_snapshots(&root, &metadata, crypto.as_ref(), Some(&source_keyring))?;
        let head_issues = check_signed_head(&metadata, &reloaded);
        let needs_write = metadata_resigned || !head_issues.is_empty();
        if needs_write {
            let mut next = metadata.clone();
            configure_signing(&mut next, &credential, required);
            next.signing
                .as_mut()
                .ok_or_else(|| anyhow!("new metadata signing block disappeared during rekey"))?
                .head = build_head(&next, reloaded.last())?;
            next.updated_at = now_rfc3339();
            sign_metadata(&mut next, crypto.as_ref())?;
            sign_repository_metadata(&mut next, &credential)?;
            if next.repository_id != metadata.repository_id
                || next.pruned_snapshots != metadata.pruned_snapshots
                || next.encryption != metadata.encryption
                || next.format_version != metadata.format_version
            {
                return Err(BackupSecurityError::RekeyFailed {
                    reason: "metadata preservation check failed during rekey".to_string(),
                }
                .into());
            }
            write_metadata_atomic(&root, &next)?;
        }
        metadata_resigned = needs_write;
    }

    let final_metadata = if options.dry_run {
        metadata.clone()
    } else {
        parse_metadata(&read_metadata_bytes(&root)?)?
    };
    let snapshots_remaining = pending_total.saturating_sub(snapshots_resigned);
    let complete = snapshots_remaining == 0;
    let final_snapshots = if options.dry_run {
        snapshots.clone()
    } else {
        // Metadata is always signed by the active key at this point, so it can
        // be checked strictly even in an incomplete batch.
        let final_check = verify_metadata_signature(&final_metadata, Some(options.keyring))?;
        if !final_check.is_verified() || final_check.is_revoked() {
            return Err(BackupSecurityError::RekeyFailed {
                reason: "metadata signature did not verify with the active key after rekey"
                    .to_string(),
            }
            .into());
        }
        let final_snapshots = scan_snapshots(
            &root,
            &final_metadata,
            crypto.as_ref(),
            Some(&source_keyring),
        )?;
        let head_issues = check_signed_head(&final_metadata, &final_snapshots);
        if !head_issues.is_empty() {
            return Err(BackupSecurityError::RekeyFailed {
                reason: format!(
                    "signed head is inconsistent after rekey: {}",
                    head_issues.join("; ")
                ),
            }
            .into());
        }
        if complete {
            // Only a complete run can prove that no signature still points at
            // a revoked key. An incomplete batch deliberately leaves later
            // snapshots for a resume run.
            scan_snapshots(
                &root,
                &final_metadata,
                crypto.as_ref(),
                Some(options.keyring),
            )?;
        }
        final_snapshots
    };

    let (_, blob_bytes_after, blob_digest_after) = blob_ciphertext_fingerprint(&root)?;
    let blob_ciphertext_unchanged =
        blob_digest_before == blob_digest_after && blob_bytes == blob_bytes_after;
    if !options.dry_run && !blob_ciphertext_unchanged {
        return Err(BackupSecurityError::RekeyFailed {
            reason: "encrypted blob files changed during rekey; repository is not trustworthy"
                .to_string(),
        }
        .into());
    }

    let mut parent_chain_preserved = snapshots.len() == final_snapshots.len();
    let mut entries_preserved = parent_chain_preserved;
    for (before, after) in snapshots.iter().zip(final_snapshots.iter()) {
        if before.id() != after.id()
            || before.sequence() != after.sequence()
            || before.parent() != after.parent()
        {
            parent_chain_preserved = false;
        }
        let entries_before = serde_json::to_vec(&before.manifest.entries)
            .context("serializing original manifest entries")?;
        let entries_after = serde_json::to_vec(&after.manifest.entries)
            .context("serializing rekeyed manifest entries")?;
        if entries_before != entries_after
            || before.manifest.totals != after.manifest.totals
            || before.manifest.created_at != after.manifest.created_at
        {
            entries_preserved = false;
        }
    }
    let tombstones_preserved = final_metadata.pruned_snapshots == metadata.pruned_snapshots;
    if !parent_chain_preserved || !entries_preserved || !tombstones_preserved {
        return Err(BackupSecurityError::RekeyFailed {
            reason: "rekey changed the repository id, chain, entries, or tombstones".to_string(),
        }
        .into());
    }
    let generation = final_snapshots
        .last()
        .map(super::format::ValidatedSnapshot::sequence)
        .unwrap_or(0);
    let latest_snapshot_id = final_snapshots
        .last()
        .map(|snapshot| snapshot.id().to_string());

    let changed = snapshots_resigned > 0 || metadata_resigned;
    if !options.dry_run
        && changed
        && let Err(error) = append_event(
            &root,
            "repository.rekeyed",
            serde_json::json!({
                "repository_id": metadata.repository_id,
                "previous_key_id": previous_key_id,
                "key_id": key_id,
                "previous_key_ids": previous_key_ids,
                "revoked_source_key_ids": revoked_source_key_ids,
                "snapshots_pending": pending_total,
                "snapshots_resigned": snapshots_resigned,
                "snapshots_remaining": snapshots_remaining,
                "complete": complete,
                "snapshots_unchanged": snapshots_unchanged,
                "snapshots_unsigned_preserved": snapshots_unsigned_preserved,
                "generation": generation,
                "blob_ciphertext_sha256": blob_digest_after,
            }),
        )
    {
        tracing::warn!(%error, "repository rekeyed but its audit event could not be appended");
    }

    Ok(RepositoryRekeyReport {
        repository: root.to_string_lossy().to_string(),
        repository_id: metadata.repository_id.clone(),
        format_version: metadata.format_version,
        encrypted: metadata.encryption.enabled,
        dry_run: options.dry_run,
        applied: !options.dry_run && changed,
        changed,
        previous_key_id,
        key_id,
        previous_key_ids: previous_key_ids.into_iter().collect(),
        revoked_source_key_ids: revoked_source_key_ids.into_iter().collect(),
        metadata_resigned,
        snapshots_total: snapshots.len(),
        snapshots_pending: pending_total,
        snapshots_resigned,
        snapshots_remaining,
        complete,
        max_snapshots: batch_limit,
        snapshots_unchanged,
        snapshots_unsigned_preserved,
        generation,
        latest_snapshot_id,
        parent_chain_preserved,
        entries_preserved,
        tombstones_preserved,
        blobs_total,
        blob_bytes,
        blob_ciphertext_sha256: blob_digest_after,
        blob_ciphertext_unchanged,
        keyring: Some(options.keyring.summary()),
        started_at,
        completed_at: now_rfc3339(),
    })
}

/// Hash every blob file and aggregate a stable ciphertext fingerprint.
fn blob_ciphertext_fingerprint(root: &Path) -> Result<(usize, u64, String)> {
    let blobs = scan_blob_hashes(root)?;
    let mut aggregate = Sha256::new();
    let mut total_bytes = 0u64;
    let mut count = 0usize;
    for (hash, size) in &blobs {
        let path = super::blob_path(root, hash)?;
        let file_hash = file_sha256(&path)
            .with_context(|| format!("hashing backup blob {}", path.display()))?;
        aggregate.update(hash.as_bytes());
        aggregate.update(b":");
        aggregate.update(file_hash.as_bytes());
        aggregate.update(b":");
        aggregate.update(size.to_le_bytes());
        aggregate.update(b"\n");
        total_bytes = total_bytes
            .checked_add(*size)
            .ok_or_else(|| anyhow!("backup blob byte total overflowed"))?;
        count += 1;
    }
    Ok((count, total_bytes, hex_lower(&aggregate.finalize())))
}

fn file_sha256(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("reading {}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex_lower(&digest.finalize()))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}
