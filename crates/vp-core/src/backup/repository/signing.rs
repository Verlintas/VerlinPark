//! Ed25519 signatures over repository metadata and snapshot manifests.
//!
//! # Signing scope
//!
//! Two objects are signed with domain-separated Ed25519 signatures:
//!
//! 1. **Repository metadata** (`repository.json`), after its optional
//!    ChaCha20-Poly1305 metadata MAC is computed. The signature covers the
//!    repository id, format version, encryption parameters, create/update
//!    timestamps, the complete prune-tombstone table, the active key id, and
//!    (when present) the signed head fields. This is the object that binds
//!    *tombstones*.
//! 2. **Snapshot manifests**, over a canonical statement that includes the
//!    repository id, snapshot id, generation (`sequence`), parent, creation
//!    time, signing key id, and the SHA-256 of the canonical manifest body
//!    (which covers the complete entry table and totals).
//!
//! Together they cover every field required by the hardening plan: repository
//! id, generation, parent, entry table, tombstones, created_at, and key_id.
//! The metadata signature is what makes tombstone additions/removals
//! tamper-evident; per-snapshot signatures make the immutable chain
//! tamper-evident and allow old snapshots to remain verifiable after rotation.
//!
//! # Compatibility
//!
//! v2 repositories have no `signing` metadata and no manifest signatures. They
//! remain readable and are reported as `unsigned`; this module never promotes a
//! v2 repository to `verified`. v1 ZIP archives remain entirely unauthenticated.
//!
//! # Secret handling
//!
//! Verification needs only public keys from the keyring. Signing needs a
//! [`SigningCredential`] loaded by the keyring module. Signatures contain no
//! key material and are safe to store next to encrypted or plaintext data.

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use vp_protocol::now_rfc3339;

use super::format::{RepositoryMetadata, RepositorySigning, SnapshotManifest};
use crate::backup::error::BackupSecurityError;
use crate::backup::signing::{
    Keyring, METADATA_SIGNATURE_DOMAIN, SIGNATURE_BYTES, SIGNING_ALGORITHM,
    SNAPSHOT_SIGNATURE_DOMAIN, SigningCredential, verify_payload,
};

const SNAPSHOT_SIGNATURE_KIND: &str = "verlinpark-backup-snapshot-signature";
const SNAPSHOT_SIGNATURE_VERSION: u32 = 1;

/// Per-snapshot detached signature stored inside the authenticated manifest.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManifestSignature {
    pub algorithm: String,
    pub key_id: String,
    pub signature_b64: String,
    pub signed_at: String,
}

impl ManifestSignature {
    pub(crate) fn validate_shape(&self) -> Result<()> {
        if self.algorithm != SIGNING_ALGORITHM {
            bail!(
                "backup snapshot signature uses unsupported algorithm {:?}",
                self.algorithm
            );
        }
        crate::backup::signing::validate_key_id(&self.key_id)?;
        let signature = BASE64
            .decode(self.signature_b64.as_bytes())
            .context("backup snapshot signature is not valid base64")?;
        if signature.len() != SIGNATURE_BYTES {
            bail!(
                "backup snapshot signature is {} bytes; expected {SIGNATURE_BYTES}",
                signature.len()
            );
        }
        if BASE64.encode(&signature) != self.signature_b64 {
            bail!("backup snapshot signature is not canonical base64");
        }
        if self.signed_at.is_empty() || self.signed_at.len() > 64 {
            bail!("backup snapshot signature timestamp is malformed");
        }
        Ok(())
    }

    fn signature_bytes(&self) -> Result<Vec<u8>> {
        BASE64
            .decode(self.signature_b64.as_bytes())
            .context("backup snapshot signature is not valid base64")
    }
}

/// Verification state of one signed object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureStatus {
    /// A signature was present and verified against a keyring public key.
    Verified,
    /// The object predates signing or belongs to an unsigned repository.
    Unsigned,
    /// A signature exists but no keyring was supplied and it was not required.
    Unverified,
}

impl SignatureStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Unsigned => "unsigned",
            Self::Unverified => "unverified",
        }
    }
}

/// Report row for one signed object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureCheck {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl SignatureCheck {
    pub fn verified(key_id: &str) -> Self {
        Self {
            status: "verified".to_string(),
            key_id: Some(key_id.to_string()),
            detail: None,
        }
    }

    pub fn unsigned(detail: impl Into<String>) -> Self {
        Self {
            status: "unsigned".to_string(),
            key_id: None,
            detail: Some(detail.into()),
        }
    }

    pub fn unverified(key_id: &str, detail: impl Into<String>) -> Self {
        Self {
            status: "unverified".to_string(),
            key_id: Some(key_id.to_string()),
            detail: Some(detail.into()),
        }
    }

    pub fn is_verified(&self) -> bool {
        self.status == "verified"
    }
}

/// Canonical bytes signed for repository metadata.
///
/// The `metadata_signature` field itself is excluded. The metadata MAC is
/// included when present, so the signature also binds the MAC value that
/// encrypted repositories rely on.
pub(crate) fn metadata_signature_payload(metadata: &RepositoryMetadata) -> Result<Vec<u8>> {
    let mut copy = metadata.clone();
    match &mut copy.signing {
        Some(signing) => signing.metadata_signature = None,
        None => bail!("repository has no signing metadata to sign"),
    }
    serde_json::to_vec(&copy).context("serializing signed repository metadata")
}

#[derive(Serialize)]
struct SnapshotStatement<'a> {
    kind: &'static str,
    version: u32,
    repository_id: &'a str,
    generation: u64,
    snapshot_id: &'a str,
    parent: Option<&'a str>,
    created_at: &'a str,
    key_id: &'a str,
    manifest_sha256: String,
}

fn snapshot_signature_payload(
    repository_id: &str,
    key_id: &str,
    manifest: &SnapshotManifest,
) -> Result<Vec<u8>> {
    let mut copy = manifest.clone();
    copy.signature = None;
    let body = serde_json::to_vec(&copy).context("serializing signed backup snapshot manifest")?;
    let statement = SnapshotStatement {
        kind: SNAPSHOT_SIGNATURE_KIND,
        version: SNAPSHOT_SIGNATURE_VERSION,
        repository_id,
        generation: manifest.sequence,
        snapshot_id: &manifest.snapshot_id,
        parent: manifest.parent.as_deref(),
        created_at: &manifest.created_at,
        key_id,
        manifest_sha256: crate::backup::crypto::sha256_hex(&body),
    };
    serde_json::to_vec(&statement).context("serializing backup snapshot signature statement")
}

/// Sign a snapshot manifest in place.
pub(crate) fn sign_snapshot_manifest(
    repository_id: &str,
    credential: &SigningCredential,
    manifest: &mut SnapshotManifest,
) -> Result<()> {
    let payload = snapshot_signature_payload(repository_id, credential.key_id(), manifest)?;
    let signature = credential.sign(SNAPSHOT_SIGNATURE_DOMAIN, &payload)?;
    let entry = ManifestSignature {
        algorithm: SIGNING_ALGORITHM.to_string(),
        key_id: credential.key_id().to_string(),
        signature_b64: BASE64.encode(signature),
        signed_at: now_rfc3339(),
    };
    entry.validate_shape()?;
    manifest.signature = Some(entry);
    Ok(())
}

/// Verify one snapshot manifest against the keyring.
///
/// A signed repository (`required = true`) rejects an unsigned snapshot. A
/// mixed repository (an upgraded v2 chain plus newly signed snapshots) reports
/// historical snapshots as `unsigned` instead of pretending they are verified.
pub(crate) fn verify_snapshot_signature(
    metadata: &RepositoryMetadata,
    keyring: Option<&Keyring>,
    manifest: &SnapshotManifest,
) -> Result<SignatureCheck> {
    if metadata.format_version < 3 {
        // A v2 repository is never promoted to verified, even if a manifest
        // happens to carry a signature after an interrupted upgrade.
        return Ok(SignatureCheck::unsigned(
            "v2 repository manifests are unsigned and not trusted as authentic",
        ));
    }
    let required = metadata
        .signing
        .as_ref()
        .map(|signing| signing.required)
        .unwrap_or(false);
    let Some(signature) = &manifest.signature else {
        if required {
            return Err(BackupSecurityError::SignatureMissing {
                object: format!("snapshot {}", manifest.snapshot_id),
            }
            .into());
        }
        return Ok(SignatureCheck::unsigned(
            "snapshot predates signing or belongs to an unsigned repository",
        ));
    };
    signature.validate_shape()?;
    let Some(keyring) = keyring else {
        if required {
            return Err(BackupSecurityError::KeyringRequired.into());
        }
        return Ok(SignatureCheck::unverified(
            &signature.key_id,
            "no keyring was supplied; signature not checked",
        ));
    };
    let public = keyring.public_key(&signature.key_id)?;
    let payload = snapshot_signature_payload(&metadata.repository_id, &signature.key_id, manifest)?;
    verify_payload(
        &public,
        SNAPSHOT_SIGNATURE_DOMAIN,
        &payload,
        &signature.signature_bytes()?,
    )
    .map_err(|error| {
        if error
            .downcast_ref::<BackupSecurityError>()
            .is_some_and(|error| matches!(error, BackupSecurityError::SignatureInvalid { .. }))
        {
            BackupSecurityError::SignatureInvalid {
                object: format!("snapshot {}", manifest.snapshot_id),
                key_id: signature.key_id.clone(),
            }
            .into()
        } else {
            error
        }
    })?;
    Ok(SignatureCheck::verified(&signature.key_id))
}

/// Verify repository metadata signing configuration and signature.
///
/// v2 metadata must not claim a signing block. v3 metadata must carry one and
/// its signature must verify against the keyring. The metadata MAC (when
/// encrypted) is verified separately by the format layer before this call.
pub(crate) fn verify_metadata_signature(
    metadata: &RepositoryMetadata,
    keyring: Option<&Keyring>,
) -> Result<SignatureCheck> {
    if metadata.format_version < 3 {
        if metadata.signing.is_some() {
            return Err(BackupSecurityError::RepositoryFormat {
                format_version: metadata.format_version,
                minimum: 2,
                maximum: 2,
            }
            .into());
        }
        return Ok(SignatureCheck::unsigned(
            "v2 repositories are encrypted and MACed but not signed",
        ));
    }
    let Some(signing) = &metadata.signing else {
        return Err(BackupSecurityError::SignatureMissing {
            object: "repository metadata".to_string(),
        }
        .into());
    };
    let Some(signature_b64) = &signing.metadata_signature else {
        return Err(BackupSecurityError::SignatureMissing {
            object: "repository metadata".to_string(),
        }
        .into());
    };
    let Some(keyring) = keyring else {
        return Err(BackupSecurityError::KeyringRequired.into());
    };
    let public = keyring.public_key(&signing.active_key_id)?;
    let payload = metadata_signature_payload(metadata)?;
    let signature = BASE64
        .decode(signature_b64.as_bytes())
        .context("backup repository metadata signature is not valid base64")?;
    verify_payload(&public, METADATA_SIGNATURE_DOMAIN, &payload, &signature).map_err(|error| {
        if error
            .downcast_ref::<BackupSecurityError>()
            .is_some_and(|error| matches!(error, BackupSecurityError::SignatureInvalid { .. }))
        {
            BackupSecurityError::SignatureInvalid {
                object: "repository metadata".to_string(),
                key_id: signing.active_key_id.clone(),
            }
            .into()
        } else {
            error
        }
    })?;
    Ok(SignatureCheck::verified(&signing.active_key_id))
}

/// Sign repository metadata in place after its MAC has been computed.
pub(crate) fn sign_repository_metadata(
    metadata: &mut RepositoryMetadata,
    credential: &SigningCredential,
) -> Result<()> {
    let Some(signing) = metadata.signing.as_mut() else {
        return Err(BackupSecurityError::SigningRequired.into());
    };
    signing.metadata_signature = None;
    let payload = metadata_signature_payload(metadata)?;
    let signature = credential.sign(METADATA_SIGNATURE_DOMAIN, &payload)?;
    let entry = metadata
        .signing
        .as_mut()
        .ok_or_else(|| anyhow!("repository signing block disappeared during signing"))?;
    entry.metadata_signature = Some(BASE64.encode(signature));
    Ok(())
}

/// Current signing summary for reports.
pub(crate) fn signing_summary(
    metadata: &RepositoryMetadata,
) -> (bool, Option<String>, Option<String>) {
    let signed = metadata.format_version >= 3 && metadata.signing.is_some();
    let key_id = metadata
        .signing
        .as_ref()
        .map(|signing| signing.active_key_id.clone());
    let algorithm = signed.then(|| SIGNING_ALGORITHM.to_string());
    (signed, key_id, algorithm)
}

/// Configure metadata for signing without writing the signature yet.
pub(crate) fn configure_signing(
    metadata: &mut RepositoryMetadata,
    credential: &SigningCredential,
    required: bool,
) {
    metadata.format_version = 3;
    ensure_signing_block(metadata, required);
    if let Some(signing) = metadata.signing.as_mut() {
        signing.active_key_id = credential.key_id().to_string();
        signing.signed_at = now_rfc3339();
        signing.metadata_signature = None;
    }
}

/// Ensure a metadata signing block exists for a signed repository.
pub(crate) fn ensure_signing_block(metadata: &mut RepositoryMetadata, required: bool) {
    if metadata.signing.is_none() {
        metadata.signing = Some(RepositorySigning {
            active_key_id: String::new(),
            required,
            metadata_signature: None,
            signed_at: now_rfc3339(),
            head: None,
        });
    }
    metadata.format_version = 3;
}

/// Check the signed head against the actual retained chain.
///
/// The head is written on every signed backup, prune, or rotation. A crash
/// between publishing a manifest and updating the head can leave a stale head;
/// this is reported as a consistency issue, not silently accepted as verified.
pub(crate) fn check_signed_head(
    metadata: &RepositoryMetadata,
    snapshots: &[super::format::ValidatedSnapshot],
) -> Vec<String> {
    let Some(signing) = &metadata.signing else {
        return Vec::new();
    };
    let Some(head) = &signing.head else {
        if snapshots.is_empty() {
            return Vec::new();
        }
        return vec!["signed repository metadata has no head for a non-empty chain".to_string()];
    };
    let Some(latest) = snapshots.last() else {
        return vec!["signed repository head exists but the chain is empty".to_string()];
    };
    let mut issues = Vec::new();
    if head.generation != latest.sequence() {
        issues.push(format!(
            "signed head generation {} does not match latest snapshot generation {}",
            head.generation,
            latest.sequence()
        ));
    }
    if head.latest_snapshot_id != latest.id() {
        issues.push("signed head snapshot id does not match the latest snapshot".to_string());
    }
    if head.latest_manifest_sha256 != latest.manifest_sha256 {
        issues.push("signed head manifest hash does not match the latest snapshot".to_string());
    }
    if head.latest_parent.as_deref() != latest.parent() {
        issues.push("signed head parent does not match the latest snapshot".to_string());
    }
    match entries_sha256(&latest.manifest) {
        Ok(digest) if digest == head.latest_entries_sha256 => {}
        Ok(_) => {
            issues.push("signed head entry digest does not match the latest snapshot".to_string())
        }
        Err(error) => issues.push(format!(
            "signed head entry digest could not be verified: {error:#}"
        )),
    }
    match tombstones_sha256(metadata) {
        Ok(digest) if digest == head.tombstones_sha256 => {}
        Ok(_) => issues.push("signed head tombstone digest does not match metadata".to_string()),
        Err(error) => issues.push(format!(
            "signed head tombstone digest could not be verified: {error:#}"
        )),
    }
    issues
}

/// Build the signed head for the latest retained snapshot.
pub(crate) fn build_head(
    metadata: &RepositoryMetadata,
    latest: Option<&super::format::ValidatedSnapshot>,
) -> Result<Option<super::format::RepositoryHead>> {
    let Some(latest) = latest else {
        return Ok(None);
    };
    Ok(Some(super::format::RepositoryHead {
        generation: latest.sequence(),
        latest_snapshot_id: latest.id().to_string(),
        latest_manifest_sha256: latest.manifest_sha256.clone(),
        latest_parent: latest.parent().map(str::to_string),
        latest_entries_sha256: entries_sha256(&latest.manifest)?,
        tombstones_sha256: tombstones_sha256(metadata)?,
        updated_at: now_rfc3339(),
    }))
}

/// SHA-256 over the canonical entry table.
pub(crate) fn entries_sha256(manifest: &SnapshotManifest) -> Result<String> {
    let bytes =
        serde_json::to_vec(&manifest.entries).context("serializing backup snapshot entries")?;
    Ok(crate::backup::crypto::sha256_hex(&bytes))
}

/// SHA-256 over the canonical prune-tombstone table.
pub(crate) fn tombstones_sha256(metadata: &RepositoryMetadata) -> Result<String> {
    let bytes = serde_json::to_vec(&metadata.pruned_snapshots)
        .context("serializing backup prune tombstones")?;
    Ok(crate::backup::crypto::sha256_hex(&bytes))
}

/// Keyring public key availability for a signed repository.
#[cfg(test)]
pub(crate) fn missing_keyring_keys(
    metadata: &RepositoryMetadata,
    keyring: &Keyring,
    snapshots: &[SnapshotManifest],
) -> Vec<String> {
    let mut missing = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut ids = Vec::new();
    if let Some(signing) = &metadata.signing {
        ids.push(signing.active_key_id.clone());
    }
    for snapshot in snapshots {
        if let Some(signature) = &snapshot.signature {
            ids.push(signature.key_id.clone());
        }
    }
    for key_id in ids {
        if keyring.public_key(&key_id).is_ok() || !seen.insert(key_id.clone()) {
            continue;
        }
        missing.push(key_id);
    }
    missing
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::repository::format::{
        EncryptionMetadata, RepositoryHead, SnapshotEntry, SnapshotTotals, new_metadata,
    };
    use crate::backup::signing::{Keyring, SecretSource, validate_key_id};
    use tempfile::TempDir;
    use uuid::Uuid;

    fn signed_metadata(
        keyring: &Keyring,
        required: bool,
    ) -> (RepositoryMetadata, SigningCredential) {
        let credential = keyring.signing_credential(None).unwrap();
        let mut metadata = new_metadata(EncryptionMetadata::plaintext());
        metadata.format_version = 3;
        metadata.signing = Some(RepositorySigning {
            active_key_id: credential.key_id().to_string(),
            required,
            metadata_signature: None,
            signed_at: now_rfc3339(),
            head: None,
        });
        sign_repository_metadata(&mut metadata, &credential).unwrap();
        (metadata, credential)
    }

    fn signed_manifest(
        metadata: &RepositoryMetadata,
        credential: &SigningCredential,
        sequence: u64,
        parent: Option<String>,
    ) -> SnapshotManifest {
        let mut manifest = SnapshotManifest {
            kind: "verlinpark-backup-snapshot".to_string(),
            format_version: 1,
            snapshot_id: Uuid::now_v7().to_string(),
            sequence,
            parent,
            incremental: sequence > 1,
            created_at: now_rfc3339(),
            app_version: "test".to_string(),
            protocol_version: 1,
            encrypted: false,
            includes_artifacts: false,
            kdf_fingerprint: None,
            entries: vec![SnapshotEntry {
                path: crate::backup::DATABASE_FILENAME.to_string(),
                size: 4096,
                sha256: "a".repeat(64),
                compression: super::super::blob::Compression::None,
                stored_bytes: 4200,
            }],
            totals: SnapshotTotals {
                entries: 1,
                bytes: 4096,
            },
            signature: None,
        };
        sign_snapshot_manifest(&metadata.repository_id, credential, &mut manifest).unwrap();
        manifest
    }

    fn keyring(dir: &TempDir, key_id: &str) -> Keyring {
        Keyring::generate_key(
            dir.path(),
            key_id,
            true,
            SecretSource::File {
                name: format!("{key_id}.key"),
            },
        )
        .unwrap();
        Keyring::load(dir.path()).unwrap()
    }

    #[test]
    fn metadata_and_snapshot_signatures_round_trip() {
        let dir = TempDir::new().unwrap();
        let keyring = keyring(&dir, "primary");
        let (metadata, credential) = signed_metadata(&keyring, true);
        let check = verify_metadata_signature(&metadata, Some(&keyring)).unwrap();
        assert!(check.is_verified());
        assert_eq!(check.key_id.as_deref(), Some("primary"));

        let manifest = signed_manifest(&metadata, &credential, 1, None);
        let check = verify_snapshot_signature(&metadata, Some(&keyring), &manifest).unwrap();
        assert!(check.is_verified());
        assert_eq!(check.key_id.as_deref(), Some("primary"));
        assert!(manifest.signature.is_some());
        assert!(!format!("{:?}", manifest.signature).contains("seed"));
    }

    #[test]
    fn tampered_manifest_entry_generation_parent_and_creation_fail() {
        let dir = TempDir::new().unwrap();
        let keyring = keyring(&dir, "primary");
        let (metadata, credential) = signed_metadata(&keyring, true);
        let manifest = signed_manifest(&metadata, &credential, 1, None);

        let mut tampered = manifest.clone();
        tampered.entries[0].sha256 = "b".repeat(64);
        assert!(verify_snapshot_signature(&metadata, Some(&keyring), &tampered).is_err());

        let mut tampered = manifest.clone();
        tampered.sequence = 2;
        assert!(verify_snapshot_signature(&metadata, Some(&keyring), &tampered).is_err());

        let mut tampered = manifest.clone();
        tampered.parent = Some(Uuid::now_v7().to_string());
        assert!(verify_snapshot_signature(&metadata, Some(&keyring), &tampered).is_err());

        let mut tampered = manifest.clone();
        tampered.created_at = "1999-01-01T00:00:00Z".to_string();
        assert!(verify_snapshot_signature(&metadata, Some(&keyring), &tampered).is_err());

        let mut tampered = manifest.clone();
        tampered.totals.bytes += 1;
        assert!(verify_snapshot_signature(&metadata, Some(&keyring), &tampered).is_err());
    }

    #[test]
    fn tampered_metadata_tombstone_or_generation_fails() {
        let dir = TempDir::new().unwrap();
        let keyring = keyring(&dir, "primary");
        let (mut metadata, _) = signed_metadata(&keyring, true);
        metadata
            .pruned_snapshots
            .push(super::super::format::PrunedSnapshot {
                id: Uuid::now_v7().to_string(),
                sequence: 1,
                created_at: now_rfc3339(),
                pruned_at: now_rfc3339(),
                manifest_sha256: "c".repeat(64),
            });
        let error = verify_metadata_signature(&metadata, Some(&keyring)).unwrap_err();
        assert!(format!("{error:#}").contains("BACKUP_SIGNATURE_INVALID"));

        let mut fresh = metadata.clone();
        fresh.pruned_snapshots.clear();
        sign_repository_metadata(&mut fresh, &keyring.signing_credential(None).unwrap()).unwrap();
        fresh.format_version = 4;
        assert!(verify_metadata_signature(&fresh, Some(&keyring)).is_err());
    }

    #[test]
    fn wrong_or_missing_key_is_typed() {
        let first_dir = TempDir::new().unwrap();
        let first = keyring(&first_dir, "first");
        let (metadata, credential) = signed_metadata(&first, true);
        let manifest = signed_manifest(&metadata, &credential, 1, None);

        let second_dir = TempDir::new().unwrap();
        let second = keyring(&second_dir, "second");
        let error = verify_snapshot_signature(&metadata, Some(&second), &manifest).unwrap_err();
        assert!(format!("{error:#}").contains("BACKUP_KEY_NOT_FOUND"));

        // Same key id with different key material must not verify.
        let third_dir = TempDir::new().unwrap();
        let third = keyring(&third_dir, "first");
        let error = verify_snapshot_signature(&metadata, Some(&third), &manifest).unwrap_err();
        assert!(format!("{error:#}").contains("BACKUP_SIGNATURE_INVALID"));

        let error = verify_snapshot_signature(&metadata, None, &manifest).unwrap_err();
        assert!(format!("{error:#}").contains("BACKUP_KEYRING_REQUIRED"));

        let error = verify_metadata_signature(&metadata, None).unwrap_err();
        assert!(format!("{error:#}").contains("BACKUP_KEYRING_REQUIRED"));
    }

    #[test]
    fn unsigned_snapshots_are_reported_not_verified() {
        let dir = TempDir::new().unwrap();
        let keyring = keyring(&dir, "primary");
        let (metadata, credential) = signed_metadata(&keyring, true);
        let mut manifest = signed_manifest(&metadata, &credential, 1, None);
        manifest.signature = None;
        let error = verify_snapshot_signature(&metadata, Some(&keyring), &manifest).unwrap_err();
        assert!(format!("{error:#}").contains("BACKUP_SIGNATURE_MISSING"));

        let (mut mixed, _) = signed_metadata(&keyring, false);
        mixed.format_version = 3;
        let check = verify_snapshot_signature(&mixed, Some(&keyring), &manifest).unwrap();
        assert_eq!(check.status, "unsigned");
        assert!(!check.is_verified());
    }

    #[test]
    fn v2_metadata_is_explicitly_unsigned() {
        let metadata = new_metadata(EncryptionMetadata::plaintext());
        assert_eq!(metadata.format_version, 2);
        let check = verify_metadata_signature(&metadata, None).unwrap();
        assert_eq!(check.status, "unsigned");
        let mut bad = metadata;
        bad.signing = Some(RepositorySigning {
            active_key_id: "primary".to_string(),
            required: false,
            metadata_signature: None,
            signed_at: now_rfc3339(),
            head: None,
        });
        assert!(verify_metadata_signature(&bad, None).is_err());
    }

    #[test]
    fn rotation_verifies_old_and_new_snapshots() {
        let dir = TempDir::new().unwrap();
        let keyring = keyring(&dir, "old");
        let (metadata, old_credential) = signed_metadata(&keyring, true);
        let old_manifest = signed_manifest(&metadata, &old_credential, 1, None);

        Keyring::generate_key(
            dir.path(),
            "new",
            true,
            SecretSource::File {
                name: "new.key".to_string(),
            },
        )
        .unwrap();
        let mut rotated = Keyring::load(dir.path()).unwrap();
        rotated.rotate_to("new").unwrap();
        let new_credential = rotated.signing_credential(None).unwrap();
        let mut upgraded = metadata.clone();
        upgraded.signing.as_mut().unwrap().active_key_id = "new".to_string();
        sign_repository_metadata(&mut upgraded, &new_credential).unwrap();
        let new_manifest = signed_manifest(
            &upgraded,
            &new_credential,
            2,
            Some(old_manifest.snapshot_id.clone()),
        );

        // Both historical and new snapshots verify with the rotated keyring.
        verify_snapshot_signature(&upgraded, Some(&rotated), &old_manifest).unwrap();
        verify_snapshot_signature(&upgraded, Some(&rotated), &new_manifest).unwrap();
        verify_metadata_signature(&upgraded, Some(&rotated)).unwrap();
        assert!(
            missing_keyring_keys(&upgraded, &rotated, &[old_manifest, new_manifest]).is_empty()
        );
    }

    #[test]
    fn signature_shape_and_base64_are_validated() {
        let dir = TempDir::new().unwrap();
        let keyring = keyring(&dir, "primary");
        let (metadata, credential) = signed_metadata(&keyring, true);
        let mut manifest = signed_manifest(&metadata, &credential, 1, None);

        manifest.signature.as_mut().unwrap().algorithm = "rsa".to_string();
        assert!(verify_snapshot_signature(&metadata, Some(&keyring), &manifest).is_err());

        let mut manifest = signed_manifest(&metadata, &credential, 1, None);
        manifest.signature.as_mut().unwrap().signature_b64 = "not-base64".to_string();
        assert!(verify_snapshot_signature(&metadata, Some(&keyring), &manifest).is_err());

        let mut metadata = metadata;
        metadata.signing.as_mut().unwrap().metadata_signature = Some("00".to_string());
        assert!(verify_metadata_signature(&metadata, Some(&keyring)).is_err());
    }

    #[test]
    fn entry_and_tombstone_digests_are_deterministic() {
        let dir = TempDir::new().unwrap();
        let keyring = keyring(&dir, "primary");
        let (metadata, credential) = signed_metadata(&keyring, true);
        let manifest = signed_manifest(&metadata, &credential, 1, None);
        assert_eq!(
            entries_sha256(&manifest).unwrap(),
            entries_sha256(&manifest).unwrap()
        );
        assert_eq!(
            tombstones_sha256(&metadata).unwrap(),
            tombstones_sha256(&metadata).unwrap()
        );
        assert_eq!(tombstones_sha256(&metadata).unwrap().len(), 64);
        assert!(validate_key_id("primary").is_ok());
        let _ = RepositoryHead::default();
    }
}
