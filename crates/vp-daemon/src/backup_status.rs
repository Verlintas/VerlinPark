use std::path::Path;

use anyhow::Result;

/// Read-only backup signing status. Never returns private key material.
pub(crate) fn backup_status(data_dir: &Path) -> Result<vp_protocol::types::BackupStatusResult> {
    backup_status_in(data_dir, &vp_core::backup::signing::default_key_dir())
}

/// Testable status reader with an explicit key directory.
pub(crate) fn backup_status_in(
    data_dir: &Path,
    key_dir: &Path,
) -> Result<vp_protocol::types::BackupStatusResult> {
    use vp_core::backup::anchor::AnchorStore;
    use vp_core::backup::signing::Keyring;
    use vp_protocol::types::{BackupAnchorStatus, BackupSigningKeyStatus, BackupStatusResult};

    let anchor = AnchorStore::new(data_dir).summary()?;
    let keyring_path = key_dir.join("keyring.json");
    let keyring = Keyring::load_optional(key_dir)?;
    let (keyring_present, active_key_id, keys) = match &keyring {
        Some(keyring) => {
            let summary = keyring.summary();
            (
                true,
                summary.active_key_id.clone(),
                summary
                    .keys
                    .into_iter()
                    .map(|key| BackupSigningKeyStatus {
                        key_id: key.key_id,
                        algorithm: key.algorithm,
                        public_key_b64: key.public_key_b64,
                        created_at: key.created_at,
                        retired_at: key.retired_at,
                        revoked_at: key.revoked_at,
                        active: key.active,
                    })
                    .collect(),
            )
        }
        None => (false, None, Vec::new()),
    };
    let imported_repositories = anchor.imported_repositories;
    let repositories = anchor
        .repositories
        .into_iter()
        .map(|entry| BackupAnchorStatus {
            repository_id: entry.repository_id,
            repository_path: entry.repository_path,
            highest_generation: entry.highest_generation,
            last_verified_generation: entry.last_verified_generation,
            last_snapshot_id: entry.last_snapshot_id,
            last_operation: entry.last_operation,
            first_seen_at: entry.first_seen_at,
            last_verified_at: entry.last_verified_at,
            origin: entry.origin,
        })
        .collect();
    Ok(BackupStatusResult {
        anchor_path: anchor.path,
        anchor_present: anchor.present,
        anchor_updated_at: anchor.updated_at,
        repositories,
        keyring_path: keyring_path.display().to_string(),
        keyring_present,
        active_key_id,
        key_count: keys.len(),
        revoked_key_count: keys.iter().filter(|key| key.revoked_at.is_some()).count(),
        keys,
        signed_repository_formats: vec![3],
        portable_anchor_format_version: vp_core::backup::anchor::PORTABLE_ANCHOR_FORMAT_VERSION,
        imported_repositories,
        note: "Backup signing, verification, rekey, and restore are CLI operations. Revoked keys verify only with --allow-revoked. The local anchor is not tamper-proof against this OS user or root and is not remote attestation, KMS, escrow, or a signed timestamp.".to_string(),
    })
}
