//! End-to-end signing, key rotation, anti-rollback, and tamper tests.

use std::fs;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::json;
use tempfile::TempDir;
use vp_protocol::now_rfc3339;
use vp_protocol::types::{Project, Session};
use vp_store::Store;

use super::format::{
    RepositoryHead, RepositoryMetadata, SnapshotManifest, parse_metadata, read_metadata_bytes,
    write_metadata_atomic,
};
use super::*;
use crate::backup::DATABASE_FILENAME;
use crate::backup::anchor::{ANCHOR_FILENAME, AnchorStore};
use crate::backup::crypto::KdfConfig;
use crate::backup::error::BackupSecurityError;
use crate::backup::signing::{Keyring, SecretSource};

const PASSPHRASE: &str = "signed backups test passphrase 17";

fn passphrase() -> Passphrase {
    Passphrase::new(PASSPHRASE.as_bytes().to_vec()).unwrap()
}

fn fast_kdf() -> KdfConfig {
    KdfConfig::fast_for_tests()
}

fn keyring_dir() -> TempDir {
    TempDir::new().unwrap()
}

fn make_keyring(dir: &TempDir, key_id: &str) -> Keyring {
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

fn options(
    keyring: Option<&Keyring>,
    key_id: Option<&str>,
    encrypted: bool,
    incremental: bool,
) -> RepositoryBackupOptions {
    RepositoryBackupOptions {
        incremental,
        include_artifacts: false,
        compression: Compression::Zstd,
        encryption: if encrypted {
            RepositoryEncryption::Encrypted(passphrase())
        } else {
            RepositoryEncryption::Plaintext
        },
        kdf: fast_kdf(),
        signing: keyring.map(|keyring| RepositorySigningOptions {
            keyring: keyring.clone(),
            key_id: key_id.map(str::to_string),
        }),
    }
}

fn create_signed(
    data: &Path,
    repo: &Path,
    keyring: &Keyring,
    key_id: Option<&str>,
    encrypted: bool,
    incremental: bool,
) -> RepositoryBackupReport {
    create_repository_backup(
        data,
        repo,
        options(Some(keyring), key_id, encrypted, incremental),
    )
    .unwrap()
}

fn seed_data_dir(dir: &Path) -> (Store, String) {
    let store = Store::open(dir).unwrap();
    let project = Project {
        id: "project-1".to_string(),
        name: "Signed backup test".to_string(),
        root: "/tmp/signed-backup-test".to_string(),
        default_branch: "main".to_string(),
        created_at: now_rfc3339(),
    };
    store.insert_project_snapshot(&project).unwrap();
    let session = Session {
        id: "session-1".to_string(),
        project_id: project.id,
        goal: "verify signed backup".to_string(),
        status: "idle".to_string(),
        priority: 0,
        worktree: None,
        branch: Some("session/signed-backup".to_string()),
        token_budget: Some(1000),
        cost_budget_usd: None,
        time_budget_secs: None,
        unlimited: false,
        cost_spent_usd: 0.0,
        base_commit: None,
        tokens_in: 0,
        tokens_out: 0,
        prompt_layout: 2,
        created_at: now_rfc3339(),
    };
    store.insert_session_snapshot(&session).unwrap();
    store
        .append_event(
            &session.id,
            "session.created",
            json!({"goal": session.goal, "project_id": session.project_id}),
        )
        .unwrap();
    (store, session.id)
}

fn manifest_for(repo: &Path, snapshot_id: &str, keyring: Option<&Keyring>) -> SnapshotManifest {
    let metadata = parse_metadata(&read_metadata_bytes(repo).unwrap()).unwrap();
    let passphrase = passphrase();
    let crypto = unlock_repository(
        &metadata,
        metadata.encryption.enabled.then_some(&passphrase),
    )
    .unwrap();
    super::format::load_snapshot(repo, &metadata, crypto.as_ref(), keyring, snapshot_id)
        .unwrap()
        .manifest
}

fn repo_metadata(repo: &Path) -> RepositoryMetadata {
    parse_metadata(&read_metadata_bytes(repo).unwrap()).unwrap()
}

fn rewrite_manifest(repo: &Path, manifest: &SnapshotManifest) {
    let metadata = repo_metadata(repo);
    let crypto = unlock_repository(&metadata, Some(&passphrase()))
        .unwrap()
        .unwrap();
    let bytes = serde_json::to_vec(manifest).unwrap();
    let sealed = crypto
        .seal_manifest(&metadata.repository_id, &manifest.snapshot_id, &bytes)
        .unwrap();
    let path = super::format::snapshot_manifest_path(repo, &manifest.snapshot_id).unwrap();
    fs::write(&path, sealed.bytes).unwrap();
}

fn all_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                stack.push(path);
            } else if entry.file_type().unwrap().is_file() {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn signed_encrypted_repository_round_trip_and_verify() {
    let data = TempDir::new().unwrap();
    let keys = keyring_dir();
    let output = TempDir::new().unwrap();
    let keyring = make_keyring(&keys, "primary");
    let (store, session_id) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    let report = create_signed(data.path(), &repo, &keyring, Some("primary"), true, false);
    drop(store);

    assert_eq!(report.format_version, 3);
    assert!(report.signed);
    assert_eq!(report.signing_key_id.as_deref(), Some("primary"));
    assert_eq!(report.signature_algorithm.as_deref(), Some("ed25519"));
    assert!(report.anchor_update.is_some());
    assert!(data.path().join(ANCHOR_FILENAME).is_file());

    let listed =
        list_repository_snapshots_with_keyring(&repo, Some(&passphrase()), Some(&keyring), true)
            .unwrap();
    assert!(listed.signed);
    assert_eq!(listed.signing_status, "signed");
    assert_eq!(listed.validation.signed_snapshots, 1);
    assert_eq!(listed.validation.unsigned_snapshots, 0);
    assert_eq!(listed.validation.metadata_signature, "verified");
    assert!(listed.validation.valid);
    assert!(listed.snapshots[0].signed);
    assert_eq!(
        listed.snapshots[0].signing_key_id.as_deref(),
        Some("primary")
    );

    let anchor = AnchorStore::new(data.path());
    let verified = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&keyring),
            anchor: Some(&anchor),
            allow_rollback: false,
            allow_revoked: false,
            verify_blobs: true,
        },
    )
    .unwrap();
    assert!(verified.valid, "{:?}", verified.issues);
    assert!(verified.signed);
    assert_eq!(verified.authenticity, "signed");
    assert_eq!(verified.generation, 1);
    assert_eq!(verified.signed_snapshots, 1);
    assert_eq!(verified.unsigned_snapshots, 0);
    assert!(verified.metadata_signature.is_verified());
    assert!(verified.signed_head_consistent);
    assert_eq!(verified.blobs_missing, 0);
    assert_eq!(verified.blobs_corrupt, 0);
    assert!(verified.rollback.as_ref().unwrap().accepted);
    assert!(verified.anchor_update.is_some());

    let target = output.path().join("restored");
    let restored = restore_repository_snapshot_checked(
        &repo,
        &report.snapshot_id,
        &target,
        false,
        RepositoryRestoreOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&keyring),
            anchor: Some(&anchor),
            allow_rollback: false,
            allow_revoked: false,
        },
    )
    .unwrap();
    assert!(restored.signed);
    assert_eq!(restored.signature_status, "verified");
    assert!(restored.anchor_update.is_some());
    let store = Store::open(&target).unwrap();
    assert_eq!(
        store.get_session(&session_id).unwrap().unwrap().goal,
        "verify signed backup"
    );
}

#[test]
fn tampered_signed_manifest_entry_and_totals_fail_verification() {
    let data = TempDir::new().unwrap();
    let keys = keyring_dir();
    let output = TempDir::new().unwrap();
    let keyring = make_keyring(&keys, "primary");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    let report = create_signed(data.path(), &repo, &keyring, None, true, false);
    drop(store);

    let mut manifest = manifest_for(&repo, &report.snapshot_id, Some(&keyring));
    // An attacker with the passphrase can rewrite the encrypted manifest and
    // its AEAD tag, but cannot produce a new Ed25519 signature.
    manifest.entries[0].size += 1;
    manifest.totals.bytes += 1;
    rewrite_manifest(&repo, &manifest);

    let anchor = AnchorStore::new(data.path());
    let error = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&keyring),
            anchor: Some(&anchor),
            allow_rollback: false,
            allow_revoked: false,
            verify_blobs: true,
        },
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("BACKUP_SIGNATURE_INVALID"),
        "{error:#}"
    );

    let target = output.path().join("target");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("keep.txt"), b"keep me").unwrap();
    let error = restore_repository_snapshot_checked(
        &repo,
        &report.snapshot_id,
        &target,
        true,
        RepositoryRestoreOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&keyring),
            anchor: Some(&anchor),
            allow_rollback: false,
            allow_revoked: false,
        },
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("BACKUP_SIGNATURE_INVALID"),
        "{error:#}"
    );
    assert_eq!(fs::read(target.join("keep.txt")).unwrap(), b"keep me");
    assert!(!target.join(DATABASE_FILENAME).exists());
}

#[test]
fn tampered_metadata_tombstone_or_timestamp_fails_closed() {
    let data = TempDir::new().unwrap();
    let keys = keyring_dir();
    let output = TempDir::new().unwrap();
    let keyring = make_keyring(&keys, "primary");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    create_signed(data.path(), &repo, &keyring, None, true, false);
    drop(store);

    let mut metadata = repo_metadata(&repo);
    metadata.updated_at = "1999-01-01T00:00:00Z".to_string();
    let crypto = unlock_repository(&metadata, Some(&passphrase()))
        .unwrap()
        .unwrap();
    // Recompute the AEAD-backed MAC exactly as a passphrase-holding attacker
    // could; the Ed25519 signature over the metadata still fails.
    super::format::sign_metadata(&mut metadata, Some(&crypto)).unwrap();
    write_metadata_atomic(&repo, &metadata).unwrap();
    let error = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&keyring),
            ..RepositoryVerifyOptions::default()
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("BACKUP_SIGNATURE_INVALID"));

    let mut metadata = repo_metadata(&repo);
    metadata
        .pruned_snapshots
        .push(super::format::PrunedSnapshot {
            id: uuid::Uuid::now_v7().to_string(),
            sequence: 1,
            created_at: now_rfc3339(),
            pruned_at: now_rfc3339(),
            manifest_sha256: "a".repeat(64),
        });
    super::format::sign_metadata(&mut metadata, Some(&crypto)).unwrap();
    write_metadata_atomic(&repo, &metadata).unwrap();
    let error = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&keyring),
            ..RepositoryVerifyOptions::default()
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("BACKUP_SIGNATURE_INVALID"));
}

#[test]
fn wrong_and_missing_keys_are_typed_and_actionable() {
    let data = TempDir::new().unwrap();
    let keys = keyring_dir();
    let output = TempDir::new().unwrap();
    let keyring = make_keyring(&keys, "primary");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    let report = create_signed(data.path(), &repo, &keyring, None, true, false);
    drop(store);

    let other_keys = keyring_dir();
    let wrong = make_keyring(&other_keys, "primary");
    let error = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&wrong),
            ..RepositoryVerifyOptions::default()
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("BACKUP_SIGNATURE_INVALID"));

    let missing_keys = keyring_dir();
    let missing = make_keyring(&missing_keys, "different");
    let error = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&missing),
            ..RepositoryVerifyOptions::default()
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("BACKUP_KEY_NOT_FOUND"));

    let error = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: Some(&passphrase()),
            keyring: None,
            ..RepositoryVerifyOptions::default()
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("BACKUP_KEYRING_REQUIRED"));

    // Signing an existing v3 repository without a key is refused before a new
    // snapshot is written.
    let before = fs::read_dir(repo.join("snapshots")).unwrap().count();
    let error =
        create_repository_backup(data.path(), &repo, options(None, None, true, true)).unwrap_err();
    assert!(format!("{error:#}").contains("BACKUP_KEYRING_REQUIRED"));
    assert_eq!(
        fs::read_dir(repo.join("snapshots")).unwrap().count(),
        before
    );
    let _ = report;
}

#[test]
fn rotation_verifies_old_and_new_snapshots() {
    let data = TempDir::new().unwrap();
    let keys = keyring_dir();
    let output = TempDir::new().unwrap();
    let keyring = make_keyring(&keys, "old");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    let first = create_signed(data.path(), &repo, &keyring, Some("old"), true, false);
    drop(store);

    Keyring::generate_key(
        keys.path(),
        "new",
        true,
        SecretSource::File {
            name: "new.key".to_string(),
        },
    )
    .unwrap();
    let mut rotated = Keyring::load(keys.path()).unwrap();
    rotated.rotate_to("new").unwrap();
    let second = create_signed(data.path(), &repo, &rotated, Some("new"), true, true);
    assert_eq!(second.parent.as_deref(), Some(first.snapshot_id.as_str()));

    let anchor = AnchorStore::new(data.path());
    let verified = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&rotated),
            anchor: Some(&anchor),
            allow_rollback: false,
            allow_revoked: false,
            verify_blobs: true,
        },
    )
    .unwrap();
    assert!(verified.valid, "{:?}", verified.issues);
    assert_eq!(verified.generation, 2);
    assert_eq!(verified.signed_snapshots, 2);
    assert_eq!(verified.unsigned_snapshots, 0);
    assert_eq!(
        verified.keyring.as_ref().unwrap().active_key_id.as_deref(),
        Some("new")
    );
    assert_eq!(verified.metadata_signature.key_id.as_deref(), Some("new"));

    let listed =
        list_repository_snapshots_with_keyring(&repo, Some(&passphrase()), Some(&rotated), true)
            .unwrap();
    assert_eq!(listed.validation.signed_snapshots, 2);
    let keys_seen: Vec<&str> = listed
        .snapshots
        .iter()
        .filter_map(|snapshot| snapshot.signing_key_id.as_deref())
        .collect();
    assert!(keys_seen.contains(&"old"));
    assert!(keys_seen.contains(&"new"));

    // Old snapshots remain verifiable with the old public key after rotation.
    let old_public = rotated.public_key("old").unwrap();
    assert_eq!(old_public.algorithm, "ed25519");
}

#[test]
fn rollback_is_rejected_by_default_and_allowed_explicitly() {
    let data = TempDir::new().unwrap();
    let keys = keyring_dir();
    let output = TempDir::new().unwrap();
    let keyring = make_keyring(&keys, "primary");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    let first = create_signed(data.path(), &repo, &keyring, None, true, false);
    let second = create_signed(data.path(), &repo, &keyring, None, true, true);
    drop(store);
    assert_eq!(second.sequence, 2);

    let anchor = AnchorStore::new(data.path());
    let anchor_file = anchor.read().unwrap();
    let entry = anchor_file
        .anchor(&repo_metadata(&repo).repository_id)
        .unwrap();
    assert_eq!(entry.highest_generation, 2);

    let target = output.path().join("rollback-target");
    let error = restore_repository_snapshot_checked(
        &repo,
        &first.snapshot_id,
        &target,
        false,
        RepositoryRestoreOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&keyring),
            anchor: Some(&anchor),
            allow_rollback: false,
            allow_revoked: false,
        },
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("BACKUP_ROLLBACK_DETECTED"),
        "{error:#}"
    );
    assert!(!target.exists());
    let events = fs::read_to_string(repo.join("events.jsonl")).unwrap();
    assert!(events.contains("snapshot.rollback_rejected"), "{events}");
    assert!(events.contains("\"operation\":\"restore\""), "{events}");

    let restored = restore_repository_snapshot_checked(
        &repo,
        &first.snapshot_id,
        &target,
        false,
        RepositoryRestoreOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&keyring),
            anchor: Some(&anchor),
            allow_rollback: true,
            allow_revoked: false,
        },
    )
    .unwrap();
    assert_eq!(
        restored.rollback.as_ref().unwrap().anchor_generation,
        Some(2)
    );
    assert!(restored.rollback.as_ref().unwrap().rollback);
    let entry = anchor
        .read()
        .unwrap()
        .anchor(&repo_metadata(&repo).repository_id)
        .unwrap()
        .clone();
    assert_eq!(entry.highest_generation, 2);
    assert_eq!(entry.last_verified_generation, 1);
    assert_eq!(entry.last_operation, "restore");
}

#[test]
fn verification_rejects_an_anchor_generation_above_the_repository() {
    let data = TempDir::new().unwrap();
    let keys = keyring_dir();
    let output = TempDir::new().unwrap();
    let keyring = make_keyring(&keys, "primary");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    create_signed(data.path(), &repo, &keyring, None, true, false);
    drop(store);

    let repository_id = repo_metadata(&repo).repository_id;
    let anchor = AnchorStore::new(data.path());
    anchor
        .record_verified(
            &repository_id,
            &repo.to_string_lossy(),
            None,
            5,
            "verify",
            true,
        )
        .unwrap();
    let error = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&keyring),
            anchor: Some(&anchor),
            allow_rollback: false,
            allow_revoked: false,
            verify_blobs: true,
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("BACKUP_ROLLBACK_DETECTED"));
    let events = fs::read_to_string(repo.join("events.jsonl")).unwrap();
    assert!(events.contains("snapshot.rollback_rejected"), "{events}");
    assert!(events.contains("\"operation\":\"verify\""), "{events}");
    let verified = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&keyring),
            anchor: Some(&anchor),
            allow_rollback: true,
            allow_revoked: false,
            verify_blobs: false,
        },
    )
    .unwrap();
    assert!(verified.valid, "{:?}", verified.issues);
    assert!(verified.rollback.as_ref().unwrap().rollback);
}

#[test]
fn v2_repositories_are_readable_and_explicitly_unsigned() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    let report =
        create_repository_backup(data.path(), &repo, options(None, None, true, false)).unwrap();
    drop(store);
    assert_eq!(report.format_version, 2);
    assert!(!report.signed);
    assert!(report.signing_key_id.is_none());
    assert!(report.anchor_update.is_some());

    let listed = list_repository_snapshots(&repo, Some(&passphrase()), true).unwrap();
    assert_eq!(listed.format_version, 2);
    assert!(!listed.signed);
    assert_eq!(listed.signing_status, "unsigned");
    assert_eq!(listed.validation.signature_verification, "unsigned");
    assert_eq!(listed.validation.metadata_signature, "unsigned");
    assert_eq!(listed.validation.signed_snapshots, 0);

    let verified = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: Some(&passphrase()),
            keyring: None,
            anchor: None,
            allow_rollback: false,
            allow_revoked: false,
            verify_blobs: true,
        },
    )
    .unwrap();
    assert!(!verified.signed);
    assert_eq!(verified.authenticity, "unsigned");
    assert!(verified.valid);
    assert!(
        verified
            .issues
            .iter()
            .any(|issue| issue.contains("not authentic"))
    );

    // v1 ZIP archives remain available and unauthenticated by this module.
    let zip = output.path().join("backup.zip");
    let v1 = crate::backup::create_backup(data.path(), &zip, false).unwrap();
    assert_eq!(v1.format_version, 1);
    let v1_target = output.path().join("v1");
    let restored = crate::backup::restore_backup(&zip, &v1_target, false).unwrap();
    assert_eq!(restored.format_version, 1);
}

#[test]
fn upgrading_a_v2_repository_reports_mixed_signatures() {
    let data = TempDir::new().unwrap();
    let keys = keyring_dir();
    let output = TempDir::new().unwrap();
    let keyring = make_keyring(&keys, "primary");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    let first =
        create_repository_backup(data.path(), &repo, options(None, None, true, false)).unwrap();
    assert_eq!(first.format_version, 2);

    let second = create_signed(data.path(), &repo, &keyring, None, true, true);
    assert_eq!(second.format_version, 3);
    assert!(second.signed);
    drop(store);

    let metadata = repo_metadata(&repo);
    assert!(
        !metadata.signing.as_ref().unwrap().required,
        "upgraded v2 history must remain explicitly unsigned"
    );
    let verified = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&keyring),
            anchor: None,
            allow_rollback: false,
            allow_revoked: false,
            verify_blobs: true,
        },
    )
    .unwrap();
    assert_eq!(verified.authenticity, "mixed");
    assert_eq!(verified.signed_snapshots, 1);
    assert_eq!(verified.unsigned_snapshots, 1);
    assert!(verified.valid);
    assert!(
        verified
            .issues
            .iter()
            .any(|issue| issue.contains("unsigned after the v3 upgrade"))
    );
}

#[test]
fn prune_resigns_metadata_and_requires_the_key() {
    let data = TempDir::new().unwrap();
    let keys = keyring_dir();
    let output = TempDir::new().unwrap();
    let keyring = make_keyring(&keys, "primary");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    create_signed(data.path(), &repo, &keyring, None, true, false);
    create_signed(data.path(), &repo, &keyring, None, true, true);
    drop(store);

    let error = prune_repository(&repo, 1, false, Some(&passphrase())).unwrap_err();
    assert!(format!("{error:#}").contains("BACKUP_KEYRING_REQUIRED"));

    let credential = keyring.signing_credential(None).unwrap();
    let pruned = prune_repository_with_signing(
        &repo,
        1,
        false,
        Some(&passphrase()),
        Some(&keyring),
        Some(&credential),
    )
    .unwrap();
    assert!(pruned.signed);
    assert_eq!(pruned.snapshots_pruned.len(), 1);
    assert_eq!(pruned.snapshots_kept, 1);

    let verified = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&keyring),
            anchor: None,
            allow_rollback: false,
            allow_revoked: false,
            verify_blobs: true,
        },
    )
    .unwrap();
    assert!(verified.valid, "{:?}", verified.issues);
    assert_eq!(verified.generation, 2);
    assert_eq!(verified.snapshot_count, 1);
    assert!(verified.tombstones_valid);
    assert!(verified.chain_valid);
    assert!(verified.signed_head_consistent);
}

#[test]
fn stale_signed_head_is_reported_as_inconsistent() {
    let data = TempDir::new().unwrap();
    let keys = keyring_dir();
    let output = TempDir::new().unwrap();
    let keyring = make_keyring(&keys, "primary");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    create_signed(data.path(), &repo, &keyring, None, true, false);
    drop(store);

    let mut metadata = repo_metadata(&repo);
    let old_head = metadata.signing.as_ref().unwrap().head.clone().unwrap();
    let signing = metadata.signing.as_mut().unwrap();
    signing.head = Some(RepositoryHead {
        generation: 1,
        latest_snapshot_id: old_head.latest_snapshot_id,
        latest_manifest_sha256: "b".repeat(64),
        latest_parent: None,
        latest_entries_sha256: "c".repeat(64),
        tombstones_sha256: "d".repeat(64),
        updated_at: now_rfc3339(),
    });
    let credential = keyring.signing_credential(None).unwrap();
    let crypto = unlock_repository(&metadata, Some(&passphrase()))
        .unwrap()
        .unwrap();
    super::format::sign_metadata(&mut metadata, Some(&crypto)).unwrap();
    super::signing::sign_repository_metadata(&mut metadata, &credential).unwrap();
    write_metadata_atomic(&repo, &metadata).unwrap();

    let verified = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&keyring),
            anchor: None,
            allow_rollback: false,
            allow_revoked: false,
            verify_blobs: true,
        },
    )
    .unwrap();
    assert!(!verified.valid);
    assert!(!verified.signed_head_consistent);
    assert!(
        verified
            .head_issues
            .iter()
            .any(|issue| issue.contains("manifest hash"))
    );
}

#[test]
fn private_seed_never_appears_in_repository_events_or_reports() {
    let data = TempDir::new().unwrap();
    let keys = keyring_dir();
    let output = TempDir::new().unwrap();
    let keyring = make_keyring(&keys, "primary");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    let report = create_signed(data.path(), &repo, &keyring, None, true, false);
    drop(store);

    let key_text = fs::read_to_string(keys.path().join("primary.key")).unwrap();
    let key_json: serde_json::Value = serde_json::from_str(&key_text).unwrap();
    let seed_b64 = key_json["seed_b64"].as_str().unwrap();
    let seed = BASE64.decode(seed_b64).unwrap();

    for file in all_files(&repo) {
        let bytes = fs::read(&file).unwrap();
        assert!(
            !contains(&bytes, seed_b64.as_bytes()),
            "seed base64 leaked into {}",
            file.display()
        );
        assert!(
            !contains(&bytes, &seed),
            "seed bytes leaked into {}",
            file.display()
        );
        assert!(
            !contains(&bytes, b"seed_b64"),
            "seed field leaked into {}",
            file.display()
        );
    }

    let rendered = serde_json::to_string(&report).unwrap();
    assert!(!rendered.contains(seed_b64));
    assert!(!rendered.contains("seed_b64"));
    assert!(!rendered.contains("signing_credential"));

    let error = Keyring::load(&TempDir::new().unwrap().path().join("missing")).unwrap_err();
    assert!(!format!("{error:#}").contains(seed_b64));
}

#[test]
fn atomic_restore_failure_leaves_no_partial_swap_or_scratch() {
    let data = TempDir::new().unwrap();
    let keys = keyring_dir();
    let output = TempDir::new().unwrap();
    let keyring = make_keyring(&keys, "primary");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    let report = create_signed(data.path(), &repo, &keyring, None, true, false);
    drop(store);

    // Corrupt one blob without touching the signed manifest. Restore must
    // validate before swapping anything.
    let manifest = manifest_for(&repo, &report.snapshot_id, Some(&keyring));
    let blob = super::format::blob_path(&repo, &manifest.entries[0].sha256).unwrap();
    let mut bytes = fs::read(&blob).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    fs::write(&blob, bytes).unwrap();

    let target = output.path().join("existing");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("keep.txt"), b"keep").unwrap();
    let error = restore_repository_snapshot_checked(
        &repo,
        &report.snapshot_id,
        &target,
        true,
        RepositoryRestoreOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&keyring),
            anchor: Some(&AnchorStore::new(data.path())),
            allow_rollback: false,
            allow_revoked: false,
        },
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("failed verification")
            || format!("{error:#}").contains("authentication"),
        "{error:#}"
    );
    assert_eq!(fs::read(target.join("keep.txt")).unwrap(), b"keep");
    assert!(!target.join(DATABASE_FILENAME).exists());
    for entry in fs::read_dir(output.path()).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().to_string();
        assert!(
            !name.starts_with(".verlinpark-restore"),
            "scratch left behind: {name}"
        );
    }
}

#[test]
fn plaintext_signed_repository_verifies_without_a_passphrase() {
    let data = TempDir::new().unwrap();
    let keys = keyring_dir();
    let output = TempDir::new().unwrap();
    let keyring = make_keyring(&keys, "plain-key");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    let report = create_signed(data.path(), &repo, &keyring, None, false, false);
    drop(store);
    assert!(report.signed);
    assert!(!report.encrypted);

    let verified = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: None,
            keyring: Some(&keyring),
            anchor: None,
            allow_rollback: false,
            allow_revoked: false,
            verify_blobs: true,
        },
    )
    .unwrap();
    assert!(verified.valid, "{:?}", verified.issues);
    assert!(verified.signed);
    assert_eq!(verified.blobs_corrupt, 0);
}

#[test]
fn unsigned_plaintext_v2_has_no_signature_claims() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    let report =
        create_repository_backup(data.path(), &repo, options(None, None, false, false)).unwrap();
    drop(store);
    assert_eq!(report.format_version, 2);
    assert!(!report.signed);
    let metadata = repo_metadata(&repo);
    assert!(metadata.signing.is_none());
    let manifest = manifest_for(&repo, &report.snapshot_id, None);
    assert!(manifest.signature.is_none());
}

#[test]
fn typed_error_codes_are_stable() {
    let rollback = BackupSecurityError::RollbackDetected {
        repository_id: "repo".to_string(),
        generation: 1,
        anchor_generation: 2,
    };
    assert_eq!(rollback.code().as_str(), "BACKUP_ROLLBACK_DETECTED");
    assert!(rollback.to_string().contains("BACKUP_ROLLBACK_DETECTED"));
    let rollback_any = anyhow::Error::new(rollback);
    assert_eq!(
        crate::backup::error::backup_security_code(&rollback_any),
        Some("BACKUP_ROLLBACK_DETECTED")
    );
}

#[test]
fn duplicate_content_across_paths_is_valid_and_shared() {
    let data = TempDir::new().unwrap();
    let keys = keyring_dir();
    let output = TempDir::new().unwrap();
    let keyring = make_keyring(&keys, "primary");
    let (store, _) = seed_data_dir(data.path());
    let events = b"{\"event\":\"message.user\",\"text\":\"same\"}\n";
    fs::create_dir_all(data.path().join("sessions/session-1")).unwrap();
    fs::write(data.path().join("sessions/session-1/events.jsonl"), events).unwrap();
    fs::create_dir_all(data.path().join("sessions/session-2")).unwrap();
    fs::write(data.path().join("sessions/session-2/events.jsonl"), events).unwrap();
    let repo = output.path().join("repository");
    create_signed(data.path(), &repo, &keyring, None, true, false);
    drop(store);

    let verified = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&keyring),
            anchor: None,
            allow_rollback: false,
            allow_revoked: false,
            verify_blobs: true,
        },
    )
    .unwrap();
    assert!(verified.valid, "{:?}", verified.issues);
    assert!(verified.entries_valid);
    assert_eq!(verified.blobs_corrupt, 0);
}

#[test]
fn failed_verification_does_not_create_or_advance_the_anchor() {
    let data = TempDir::new().unwrap();
    let keys = keyring_dir();
    let output = TempDir::new().unwrap();
    let keyring = make_keyring(&keys, "primary");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    let report = create_signed(data.path(), &repo, &keyring, None, true, false);
    drop(store);
    let anchor = AnchorStore::new(data.path());
    assert!(anchor.reset().unwrap());
    assert!(!data.path().join(ANCHOR_FILENAME).exists());

    let manifest = manifest_for(&repo, &report.snapshot_id, Some(&keyring));
    let blob = super::format::blob_path(&repo, &manifest.entries[0].sha256).unwrap();
    let mut bytes = fs::read(&blob).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    fs::write(&blob, bytes).unwrap();

    let verified = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&keyring),
            anchor: Some(&anchor),
            allow_rollback: false,
            allow_revoked: false,
            verify_blobs: true,
        },
    )
    .unwrap();
    assert!(!verified.valid);
    assert!(verified.blobs_corrupt > 0);
    assert!(verified.anchor_update.is_none());
    assert!(
        !data.path().join(ANCHOR_FILENAME).exists(),
        "a corrupt repository must not create an anchor"
    );
}

#[test]
fn generated_keys_reject_an_unrecoverable_environment_source() {
    let dir = keyring_dir();
    let error = Keyring::generate_key(
        dir.path(),
        "primary",
        true,
        SecretSource::Environment {
            variable: "VERLINPARK_TEST_UNUSED_SIGNING_SEED".to_string(),
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("register_environment_key"));
}

#[test]
fn backup_after_an_explicit_rollback_never_lowers_the_anchor() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    create_repository_backup(data.path(), &repo, options(None, None, true, false)).unwrap();
    create_repository_backup(data.path(), &repo, options(None, None, true, true)).unwrap();
    drop(store);
    let repository_id = repo_metadata(&repo).repository_id;
    let anchor = AnchorStore::new(data.path());
    anchor
        .record_verified(
            &repository_id,
            &repo.to_string_lossy(),
            None,
            5,
            "verify",
            true,
        )
        .unwrap();
    let third =
        create_repository_backup(data.path(), &repo, options(None, None, true, true)).unwrap();
    assert_eq!(third.sequence, 3);
    let entry = anchor
        .read()
        .unwrap()
        .anchor(&repository_id)
        .unwrap()
        .clone();
    assert_eq!(entry.highest_generation, 5);
    assert_eq!(entry.last_verified_generation, 3);
}

#[test]
fn revoked_key_rejects_verify_list_and_restore_unless_explicitly_allowed() {
    let data = TempDir::new().unwrap();
    let keys = keyring_dir();
    let output = TempDir::new().unwrap();
    let keyring = make_keyring(&keys, "primary");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    let report = create_signed(data.path(), &repo, &keyring, None, true, false);
    drop(store);

    Keyring::generate_key(
        keys.path(),
        "new",
        true,
        SecretSource::File {
            name: "new.key".to_string(),
        },
    )
    .unwrap();
    let mut rotated = Keyring::load(keys.path()).unwrap();
    rotated.revoke_key("primary", false).unwrap();
    let rotated = Keyring::load(keys.path()).unwrap();
    assert_eq!(rotated.summary().revoked_key_count, 1);

    let anchor = AnchorStore::new(data.path());
    let error = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&rotated),
            anchor: Some(&anchor),
            allow_rollback: false,
            allow_revoked: false,
            verify_blobs: true,
        },
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("BACKUP_KEY_REVOKED"),
        "{error:#}"
    );

    let verified = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&rotated),
            anchor: Some(&anchor),
            allow_rollback: false,
            allow_revoked: true,
            verify_blobs: true,
        },
    )
    .unwrap();
    assert!(verified.valid, "{:?}", verified.issues);
    assert!(verified.metadata_signature.is_revoked());
    assert!(!verified.revoked_signature_warnings.is_empty());
    assert!(verified.revoked_signature_warnings[0].contains("revoked"));
    let events = fs::read_to_string(repo.join("events.jsonl")).unwrap();
    assert!(events.contains("snapshot.revoked_key_allowed"), "{events}");
    assert!(events.contains("\"operation\":\"verify\""), "{events}");

    let error =
        list_repository_snapshots_with_keyring(&repo, Some(&passphrase()), Some(&rotated), true)
            .unwrap_err();
    assert!(
        format!("{error:#}").contains("BACKUP_KEY_REVOKED"),
        "{error:#}"
    );
    let listed = list_repository_snapshots_with_policy(
        &repo,
        Some(&passphrase()),
        Some(&rotated),
        true,
        true,
    )
    .unwrap();
    assert!(listed.validation.valid);
    assert_eq!(listed.validation.revoked_signatures, 2);
    assert_eq!(
        listed.validation.revoked_key_ids,
        vec!["primary".to_string()]
    );

    let target = output.path().join("restore-target");
    let error = restore_repository_snapshot_checked(
        &repo,
        &report.snapshot_id,
        &target,
        false,
        RepositoryRestoreOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&rotated),
            anchor: Some(&anchor),
            allow_rollback: false,
            allow_revoked: false,
        },
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("BACKUP_KEY_REVOKED"),
        "{error:#}"
    );
    assert!(!target.exists());

    let restored = restore_repository_snapshot_checked(
        &repo,
        &report.snapshot_id,
        &target,
        false,
        RepositoryRestoreOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&rotated),
            anchor: Some(&anchor),
            allow_rollback: false,
            allow_revoked: true,
        },
    )
    .unwrap();
    assert!(restored.signed);
    assert!(!restored.revoked_signature_warnings.is_empty());
    let events = fs::read_to_string(repo.join("events.jsonl")).unwrap();
    assert!(events.contains("\"operation\":\"restore\""), "{events}");
    assert!(Store::open(&target).is_ok());
}

#[test]
fn revoked_key_blocks_new_backups_until_the_repository_is_rekeyed() {
    let data = TempDir::new().unwrap();
    let keys = keyring_dir();
    let output = TempDir::new().unwrap();
    let keyring = make_keyring(&keys, "primary");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    create_signed(data.path(), &repo, &keyring, None, true, false);
    drop(store);

    Keyring::generate_key(
        keys.path(),
        "replacement",
        true,
        SecretSource::File {
            name: "replacement.key".to_string(),
        },
    )
    .unwrap();
    let mut rotated = Keyring::load(keys.path()).unwrap();
    rotated.rotate_to("replacement").unwrap();
    rotated.revoke_key("primary", false).unwrap();
    let rotated = Keyring::load(keys.path()).unwrap();

    // Appending to a repository whose metadata is signed by a revoked key is
    // refused before a new snapshot is written.
    let snapshots_before = fs::read_dir(repo.join("snapshots")).unwrap().count();
    let error = create_repository_backup(
        data.path(),
        &repo,
        options(Some(&rotated), None, true, true),
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("BACKUP_KEY_REVOKED"),
        "{error:#}"
    );
    assert_eq!(
        fs::read_dir(repo.join("snapshots")).unwrap().count(),
        snapshots_before
    );

    // Rekey is the recovery path. After it completes, normal backup resumes
    // with the new active key.
    let report = rekey_repository(
        &repo,
        RepositoryRekeyOptions {
            passphrase: Some(&passphrase()),
            keyring: &rotated,
            dry_run: false,
            max_snapshots: None,
        },
    )
    .unwrap();
    assert!(report.applied);
    let third = create_repository_backup(
        data.path(),
        &repo,
        options(Some(&rotated), None, true, true),
    )
    .unwrap();
    assert_eq!(third.sequence, 2);
    assert_eq!(third.signing_key_id.as_deref(), Some("replacement"));

    let verified = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&rotated),
            anchor: None,
            allow_rollback: false,
            allow_revoked: false,
            verify_blobs: true,
        },
    )
    .unwrap();
    assert!(verified.valid, "{:?}", verified.issues);
    assert_eq!(verified.signed_snapshots, 2);
    assert_eq!(
        verified.metadata_signature.key_id.as_deref(),
        Some("replacement")
    );
}

#[test]
fn revoked_historical_snapshot_is_rejected_even_when_metadata_is_current() {
    let data = TempDir::new().unwrap();
    let keys = keyring_dir();
    let output = TempDir::new().unwrap();
    let old_keyring = make_keyring(&keys, "old");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    let first = create_signed(data.path(), &repo, &old_keyring, None, true, false);

    Keyring::generate_key(
        keys.path(),
        "new",
        true,
        SecretSource::File {
            name: "new.key".to_string(),
        },
    )
    .unwrap();
    let mut rotated = Keyring::load(keys.path()).unwrap();
    let second = create_signed(data.path(), &repo, &rotated, None, true, true);
    assert_eq!(second.signing_key_id.as_deref(), Some("new"));
    rotated.revoke_key("old", false).unwrap();
    let rotated = Keyring::load(keys.path()).unwrap();
    drop(store);

    // Metadata is signed by the still-trusted new key, but the first snapshot
    // is signed by the revoked old key.
    let metadata = repo_metadata(&repo);
    assert_eq!(metadata.signing.as_ref().unwrap().active_key_id, "new");
    let error = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&rotated),
            anchor: None,
            allow_rollback: false,
            allow_revoked: false,
            verify_blobs: true,
        },
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("BACKUP_KEY_REVOKED"),
        "{error:#}"
    );

    let verified = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&rotated),
            anchor: None,
            allow_rollback: false,
            allow_revoked: true,
            verify_blobs: true,
        },
    )
    .unwrap();
    assert!(verified.valid, "{:?}", verified.issues);
    assert!(!verified.metadata_signature.is_revoked());
    assert_eq!(verified.signed_snapshots, 2);
    assert!(
        verified
            .revoked_signature_warnings
            .iter()
            .any(|warning| warning.contains("old"))
    );

    let listed = list_repository_snapshots_with_policy(
        &repo,
        Some(&passphrase()),
        Some(&rotated),
        true,
        true,
    )
    .unwrap();
    assert_eq!(listed.validation.revoked_signatures, 1);
    assert_eq!(listed.validation.revoked_key_ids, vec!["old".to_string()]);
    let statuses: Vec<(&str, &str)> = listed
        .snapshots
        .iter()
        .map(|snapshot| {
            (
                snapshot.signing_key_id.as_deref().unwrap_or(""),
                snapshot.signature_status.as_str(),
            )
        })
        .collect();
    assert!(statuses.contains(&("old", "verified")));
    assert!(statuses.contains(&("new", "verified")));
    let _ = first;
}

#[test]
fn allow_revoked_verification_still_records_the_anchor_generation() {
    let data = TempDir::new().unwrap();
    let keys = keyring_dir();
    let output = TempDir::new().unwrap();
    let keyring = make_keyring(&keys, "primary");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    create_signed(data.path(), &repo, &keyring, None, true, false);
    drop(store);

    Keyring::generate_key(
        keys.path(),
        "new",
        true,
        SecretSource::File {
            name: "new.key".to_string(),
        },
    )
    .unwrap();
    let mut rotated = Keyring::load(keys.path()).unwrap();
    rotated.revoke_key("primary", false).unwrap();
    let rotated = Keyring::load(keys.path()).unwrap();
    let anchor = AnchorStore::new(output.path());
    let verified = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&rotated),
            anchor: Some(&anchor),
            allow_rollback: false,
            allow_revoked: true,
            verify_blobs: false,
        },
    )
    .unwrap();
    assert!(verified.valid, "{:?}", verified.issues);
    let update = verified.anchor_update.unwrap();
    assert_eq!(update.highest_generation, 1);
    let repository_id = repo_metadata(&repo).repository_id;
    assert_eq!(
        anchor
            .read()
            .unwrap()
            .anchor(&repository_id)
            .unwrap()
            .highest_generation,
        1
    );
}
