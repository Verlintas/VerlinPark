//! Full repository verification without restoring.
//!
//! `vp backup-verify` answers one question: can this repository be trusted as
//! the generation it claims to be? It checks:
//!
//! - metadata MAC (encrypted repositories) and Ed25519 metadata signature;
//! - every snapshot manifest signature and its parent chain;
//! - the signed head against the actual latest snapshot;
//! - the prune-tombstone table and contiguous chain;
//! - every referenced blob, decrypting and hashing when a key is available;
//! - the local anti-rollback anchor, unless `--allow-rollback` is explicit.
//!
//! A successful verification records the observed generation in the anchor.
//! A verification that cannot write the anchor fails closed because otherwise
//! rollback protection would silently regress.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use vp_protocol::now_rfc3339;

use super::format::{self, ValidatedSnapshot};
use super::signing::{check_signed_head, signing_summary, verify_metadata_signature};
use super::{
    BLOB_VERIFY_CACHE_LIMIT, Compression, Passphrase, append_event, blob_path, open_for_read,
    push_issue, repository_root, scan_blob_hashes, scan_snapshots, verify_blob_file,
};
use crate::backup::anchor::{AnchorStore, AnchorUpdate, RollbackCheck};
use crate::backup::error::BackupSecurityError;
use crate::backup::repository::signing::SignatureCheck;
use crate::backup::signing::{Keyring, KeyringSummary};

/// Options for repository verification.
pub struct RepositoryVerifyOptions<'a> {
    pub passphrase: Option<&'a Passphrase>,
    pub keyring: Option<&'a Keyring>,
    pub anchor: Option<&'a AnchorStore>,
    pub allow_rollback: bool,
    /// Decrypt and hash every referenced blob. Presence is always checked.
    pub verify_blobs: bool,
}

impl Default for RepositoryVerifyOptions<'_> {
    fn default() -> Self {
        Self {
            passphrase: None,
            keyring: None,
            anchor: None,
            allow_rollback: false,
            verify_blobs: true,
        }
    }
}

/// Result of full repository verification.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepositoryVerifyReport {
    pub repository: String,
    pub format_version: u32,
    pub encrypted: bool,
    pub signed: bool,
    /// `signed`, `unsigned`, or `mixed` (an upgraded v2 chain).
    pub authenticity: String,
    pub signing_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyring: Option<KeyringSummary>,
    pub metadata_signature: SignatureCheck,
    pub generation: u64,
    pub latest_snapshot_id: String,
    pub snapshot_count: usize,
    pub signed_snapshots: usize,
    pub unsigned_snapshots: usize,
    pub chain_valid: bool,
    pub tombstones_valid: bool,
    pub entries_valid: bool,
    pub signed_head_consistent: bool,
    pub head_issues: Vec<String>,
    pub blob_verification: String,
    pub blobs_present: usize,
    pub blobs_missing: usize,
    pub blobs_verified: usize,
    pub blobs_corrupt: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback: Option<RollbackCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_update: Option<AnchorUpdate>,
    pub valid: bool,
    pub issues: Vec<String>,
    pub checked_at: String,
}

/// Verify a repository without restoring it.
pub fn verify_repository(
    repository: &Path,
    options: RepositoryVerifyOptions<'_>,
) -> Result<RepositoryVerifyReport> {
    let root = repository_root(repository, false)?;
    let session = open_for_read(&root, options.passphrase, options.keyring)?;
    let snapshots = scan_snapshots(
        &session.root,
        &session.metadata,
        session.crypto.as_ref(),
        session.keyring.as_ref(),
    )?;
    let metadata_signature =
        verify_metadata_signature(&session.metadata, session.keyring.as_ref())?;
    let (signed, _, _) = signing_summary(&session.metadata);
    let signed_snapshots = snapshots
        .iter()
        .filter(|snapshot| snapshot.signature_check.is_verified())
        .count();
    let unsigned_snapshots = snapshots.len() - signed_snapshots;
    let authenticity = match (signed, signed_snapshots, unsigned_snapshots) {
        (false, _, _) => "unsigned",
        (true, _, 0) => "signed",
        (true, 0, _) => "unsigned",
        (true, _, _) => "mixed",
    }
    .to_string();
    let head_issues = check_signed_head(&session.metadata, &snapshots);
    let generation = snapshots
        .last()
        .map(ValidatedSnapshot::sequence)
        .unwrap_or(0);
    let latest_snapshot_id = snapshots
        .last()
        .map(|snapshot| snapshot.id().to_string())
        .unwrap_or_default();
    let mut issues = Vec::new();
    for issue in &head_issues {
        push_issue(&mut issues, issue.clone());
    }

    let on_disk = scan_blob_hashes(&session.root)?;
    let mut referenced: BTreeMap<String, (u64, Compression)> = BTreeMap::new();
    let mut entries_valid = true;
    for snapshot in &snapshots {
        for entry in &snapshot.manifest.entries {
            match referenced.get(&entry.sha256) {
                Some((size, compression)) => {
                    if *size != entry.size || *compression != entry.compression {
                        entries_valid = false;
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
    }
    let mut blobs_present = 0usize;
    let mut blobs_missing = 0usize;
    let mut blobs_verified = 0usize;
    let mut blobs_corrupt = 0usize;
    let mut verified_cache = BTreeSet::new();
    for (hash, (size, compression)) in &referenced {
        match on_disk.get(hash) {
            Some(_) => {
                blobs_present += 1;
                if options.verify_blobs && verified_cache.len() < BLOB_VERIFY_CACHE_LIMIT {
                    verified_cache.insert(hash.clone());
                    let blob = blob_path(&session.root, hash)?;
                    match verify_blob_file(
                        &blob,
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
    if blobs_missing > 0 || blobs_corrupt > 0 {
        entries_valid = false;
    }

    let rollback = match options.anchor {
        // An empty repository has no generation to anchor yet.
        Some(_) if generation == 0 => None,
        Some(anchor) => match anchor.check(
            &session.metadata.repository_id,
            generation,
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
                        serde_json::json!({
                            "repository_id": session.metadata.repository_id,
                            "generation": generation,
                            "operation": "verify",
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

    let tombstones_valid = session.metadata.validate_structure().is_ok();
    let chain_valid = format::validate_chain(&session.metadata, &snapshots).is_ok();
    let signed_head_consistent = head_issues.is_empty();
    let valid = issues.is_empty()
        && chain_valid
        && tombstones_valid
        && entries_valid
        && blobs_missing == 0
        && blobs_corrupt == 0
        && signed_head_consistent;
    // Record the anchor only after a structurally valid verification. A
    // corrupt repository must not advance the high-water mark and block a
    // later restore of the last known-good generation.
    let mut anchor_update = None;
    if valid
        && generation > 0
        && let Some(anchor) = options.anchor
    {
        anchor_update = Some(
            anchor
                .record_verified(
                    &session.metadata.repository_id,
                    &session.root.to_string_lossy(),
                    snapshots.last().map(ValidatedSnapshot::id),
                    generation,
                    "verify",
                    options.allow_rollback,
                )
                .with_context(
                    || "verification succeeded but the anti-rollback anchor could not be updated",
                )?,
        );
    }
    if !signed && generation > 0 {
        push_issue(
            &mut issues,
            "repository format is unsigned; contents are encrypted/MACed but not authentic"
                .to_string(),
        );
    }
    if authenticity == "mixed" {
        push_issue(
            &mut issues,
            format!(
                "{unsigned_snapshots} historical snapshot(s) are unsigned after the v3 upgrade"
            ),
        );
    }
    Ok(RepositoryVerifyReport {
        repository: session.root.to_string_lossy().to_string(),
        format_version: session.metadata.format_version,
        encrypted: session.metadata.encryption.enabled,
        signed,
        authenticity,
        signing_status: if signed { "signed" } else { "unsigned" }.to_string(),
        keyring: session.keyring.as_ref().map(Keyring::summary),
        metadata_signature,
        generation,
        latest_snapshot_id,
        snapshot_count: snapshots.len(),
        signed_snapshots,
        unsigned_snapshots,
        chain_valid,
        tombstones_valid,
        entries_valid,
        signed_head_consistent,
        head_issues,
        blob_verification: if options.verify_blobs {
            "full".to_string()
        } else {
            "presence".to_string()
        },
        blobs_present,
        blobs_missing,
        blobs_verified,
        blobs_corrupt,
        rollback,
        anchor_update,
        valid,
        issues,
        checked_at: now_rfc3339(),
    })
}
