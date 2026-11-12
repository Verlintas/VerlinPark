//! End-to-end rekey tests: ciphertext preservation, idempotence, resumption,
//! refusal paths, and secret non-leakage.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::json;
use tempfile::TempDir;
use vp_protocol::now_rfc3339;
use vp_protocol::types::{Project, Session};
use vp_store::Store;

use super::format::{
    SnapshotManifest, parse_metadata, read_metadata_bytes, snapshot_manifest_path,
};
use super::signing::sign_snapshot_manifest;
use super::*;
use crate::backup::crypto::KdfConfig;
use crate::backup::signing::{Keyring, SecretSource};

const PASSPHRASE: &str = "rekey test passphrase 42";

fn passphrase() -> Passphrase {
    Passphrase::new(PASSPHRASE.as_bytes().to_vec()).unwrap()
}

fn fast_kdf() -> KdfConfig {
    KdfConfig::fast_for_tests()
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

fn rotate_to(dir: &TempDir, key_id: &str) -> Keyring {
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
            key_id: None,
        }),
    }
}

fn seed_data_dir(dir: &Path) -> (Store, String) {
    let store = Store::open(dir).unwrap();
    let project = Project {
        id: "project-1".to_string(),
        name: "Rekey test".to_string(),
        root: "/tmp/rekey-test".to_string(),
        default_branch: "main".to_string(),
        created_at: now_rfc3339(),
    };
    store.insert_project_snapshot(&project).unwrap();
    let session = Session {
        id: "session-1".to_string(),
        project_id: project.id,
        goal: "rekey repository".to_string(),
        status: "idle".to_string(),
        priority: 0,
        worktree: None,
        branch: Some("session/rekey".to_string()),
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

fn b64_files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                stack.push(path);
            } else if entry.file_type().unwrap().is_file() {
                files.insert(path.clone(), fs::read(&path).unwrap());
            }
        }
    }
    files
}

fn blob_files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let blobs = root.join("blobs");
    let mut files = BTreeMap::new();
    if !blobs.is_dir() {
        return files;
    }
    for entry in fs::read_dir(blobs).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_file() {
            files.insert(path.clone(), fs::read(&path).unwrap());
        }
    }
    files
}

fn manifest_signature(repo: &Path, snapshot_id: &str, keyring: &Keyring) -> SnapshotManifest {
    let metadata = parse_metadata(&read_metadata_bytes(repo).unwrap()).unwrap();
    let passphrase = passphrase();
    let crypto = unlock_repository(
        &metadata,
        metadata.encryption.enabled.then_some(&passphrase),
    )
    .unwrap();
    super::format::load_snapshot(
        repo,
        &metadata,
        crypto.as_ref(),
        Some(&keyring.clone().with_revoked_keys_allowed(true)),
        snapshot_id,
    )
    .unwrap()
    .manifest
}

fn rewrite_manifest(repo: &Path, manifest: &SnapshotManifest) {
    let metadata = parse_metadata(&read_metadata_bytes(repo).unwrap()).unwrap();
    let passphrase = passphrase();
    let crypto = unlock_repository(
        &metadata,
        metadata.encryption.enabled.then_some(&passphrase),
    )
    .unwrap();
    let bytes = serde_json::to_vec(manifest).unwrap();
    let sealed = match crypto.as_ref() {
        Some(crypto) => crypto
            .seal_manifest(&metadata.repository_id, &manifest.snapshot_id, &bytes)
            .unwrap(),
        None => crate::backup::crypto::seal_manifest_plain(
            &metadata.repository_id,
            &manifest.snapshot_id,
            &bytes,
        )
        .unwrap(),
    };
    if crypto.is_some() {
        super::append_nonce(
            repo,
            "manifest",
            &manifest.snapshot_id,
            Some(&manifest.snapshot_id),
            &sealed.nonce,
            &now_rfc3339(),
        )
        .unwrap();
    }
    let path = snapshot_manifest_path(repo, &manifest.snapshot_id).unwrap();
    fs::write(path, sealed.bytes).unwrap();
}

#[test]
fn rekey_preserves_ciphertext_and_signatures_verify_with_the_new_key() {
    let data = TempDir::new().unwrap();
    let keys = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let old_keyring = make_keyring(&keys, "old");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    let first =
        create_repository_backup(data.path(), &repo, options(Some(&old_keyring), true, false))
            .unwrap();
    let second =
        create_repository_backup(data.path(), &repo, options(Some(&old_keyring), true, true))
            .unwrap();
    drop(store);

    let metadata_before = parse_metadata(&read_metadata_bytes(&repo).unwrap()).unwrap();
    let repository_id = metadata_before.repository_id.clone();
    let blobs_before = blob_files(&repo);
    assert!(!blobs_before.is_empty());
    let manifests_before: BTreeMap<String, SnapshotManifest> = [&first, &second]
        .iter()
        .map(|report| {
            (
                report.snapshot_id.clone(),
                manifest_signature(&repo, &report.snapshot_id, &old_keyring),
            )
        })
        .collect();
    let generations: Vec<u64> = manifests_before
        .values()
        .map(|manifest| manifest.sequence)
        .collect();
    let previous_manifests = b64_files(&repo.join("snapshots"));
    let metadata_bytes_before = read_metadata_bytes(&repo).unwrap();

    let rotated = rotate_to(&keys, "new");
    let mut rotated = rotated;
    rotated.revoke_key("old", false).unwrap();
    let rotated = Keyring::load(keys.path()).unwrap();

    // Before rekey, strict verification fails closed on the revoked signer.
    let error = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&rotated),
            ..RepositoryVerifyOptions::default()
        },
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("BACKUP_KEY_REVOKED"),
        "{error:#}"
    );

    let dry_run = rekey_repository(
        &repo,
        RepositoryRekeyOptions {
            passphrase: Some(&passphrase()),
            keyring: &rotated,
            dry_run: true,
            max_snapshots: None,
        },
    )
    .unwrap();
    assert!(dry_run.dry_run);
    assert!(!dry_run.applied);
    assert!(dry_run.changed);
    assert_eq!(dry_run.snapshots_resigned, 2);
    assert!(dry_run.blob_ciphertext_unchanged);
    assert_eq!(b64_files(&repo.join("snapshots")), previous_manifests);
    assert_eq!(read_metadata_bytes(&repo).unwrap(), metadata_bytes_before);

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
    assert!(report.changed);
    assert_eq!(report.repository_id, repository_id);
    assert_eq!(report.previous_key_id, "old");
    assert_eq!(report.key_id, "new");
    assert_eq!(report.snapshots_total, 2);
    assert_eq!(report.snapshots_resigned, 2);
    assert_eq!(report.snapshots_unchanged, 0);
    assert_eq!(report.snapshots_unsigned_preserved, 0);
    assert_eq!(report.generation, 2);
    assert!(report.parent_chain_preserved);
    assert!(report.entries_preserved);
    assert!(report.tombstones_preserved);
    assert!(report.blob_ciphertext_unchanged);
    assert_eq!(report.blob_ciphertext_sha256.len(), 64);
    assert!(report.revoked_source_key_ids.contains(&"old".to_string()));

    // Blob ciphertext and sizes are byte-identical.
    assert_eq!(blob_files(&repo), blobs_before);

    // Every manifest keeps its chain/entries but now names the new key.
    let metadata_after = parse_metadata(&read_metadata_bytes(&repo).unwrap()).unwrap();
    assert_eq!(metadata_after.repository_id, repository_id);
    assert_eq!(
        metadata_after.signing.as_ref().unwrap().active_key_id,
        "new"
    );
    assert_eq!(
        metadata_after
            .signing
            .as_ref()
            .unwrap()
            .head
            .as_ref()
            .unwrap()
            .generation,
        2
    );
    for report in [&first, &second] {
        let manifest = manifest_signature(&repo, &report.snapshot_id, &rotated);
        let before = &manifests_before[&report.snapshot_id];
        assert_eq!(manifest.sequence, before.sequence);
        assert_eq!(manifest.parent, before.parent);
        assert_eq!(manifest.entries, before.entries);
        assert_eq!(manifest.totals, before.totals);
        assert_eq!(manifest.created_at, before.created_at);
        assert_eq!(
            manifest.signature.as_ref().unwrap().key_id,
            "new",
            "snapshot {} was not re-signed",
            report.snapshot_id
        );
    }
    assert_eq!(
        manifests_before
            .values()
            .map(|manifest| manifest.sequence)
            .collect::<Vec<_>>(),
        generations
    );
    assert_eq!(
        rotated.public_key("old").unwrap().algorithm,
        crate::backup::signing::SIGNING_ALGORITHM
    );

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
    assert_eq!(verified.generation, 2);
    assert_eq!(verified.signed_snapshots, 2);
    assert_eq!(verified.metadata_signature.key_id.as_deref(), Some("new"));
    assert!(verified.metadata_signature.is_verified());
    assert!(verified.signed_head_consistent);

    // A second run is a byte-for-byte no-op.
    let manifests_after = b64_files(&repo.join("snapshots"));
    let metadata_after_bytes = read_metadata_bytes(&repo).unwrap();
    let idempotent = rekey_repository(
        &repo,
        RepositoryRekeyOptions {
            passphrase: Some(&passphrase()),
            keyring: &rotated,
            dry_run: false,
            max_snapshots: None,
        },
    )
    .unwrap();
    assert!(!idempotent.changed);
    assert!(!idempotent.applied);
    assert!(!idempotent.metadata_resigned);
    assert_eq!(idempotent.snapshots_resigned, 0);
    assert_eq!(idempotent.snapshots_unchanged, 2);
    assert_eq!(b64_files(&repo.join("snapshots")), manifests_after);
    assert_eq!(read_metadata_bytes(&repo).unwrap(), metadata_after_bytes);
}

#[test]
fn rekey_refuses_v2_missing_keys_and_a_revoked_active_key() {
    let data = TempDir::new().unwrap();
    let keys = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    let v2_repo = output.path().join("v2");
    create_repository_backup(data.path(), &v2_repo, options(None, true, false)).unwrap();
    let keyring = make_keyring(&keys, "old");
    let error = rekey_repository(
        &v2_repo,
        RepositoryRekeyOptions {
            passphrase: Some(&passphrase()),
            keyring: &keyring,
            dry_run: false,
            max_snapshots: None,
        },
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("BACKUP_UNSIGNED_REPOSITORY"),
        "{error:#}"
    );

    let signed_repo = output.path().join("signed");
    create_repository_backup(
        data.path(),
        &signed_repo,
        options(Some(&keyring), true, false),
    )
    .unwrap();

    let other_keys = TempDir::new().unwrap();
    let other = make_keyring(&other_keys, "different");
    let error = rekey_repository(
        &signed_repo,
        RepositoryRekeyOptions {
            passphrase: Some(&passphrase()),
            keyring: &other,
            dry_run: false,
            max_snapshots: None,
        },
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("BACKUP_KEY_NOT_FOUND"),
        "{error:#}"
    );

    let mut forced = Keyring::load(keys.path()).unwrap();
    forced.revoke_key("old", true).unwrap();
    assert!(forced.active_key_id().is_none());
    let error = rekey_repository(
        &signed_repo,
        RepositoryRekeyOptions {
            passphrase: Some(&passphrase()),
            keyring: &forced,
            dry_run: false,
            max_snapshots: None,
        },
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("BACKUP_SIGNING_REQUIRED"),
        "{error:#}"
    );
    let _ = store;
}

#[test]
fn rekey_refuses_missing_private_key_material() {
    let data = TempDir::new().unwrap();
    let keys = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let keyring = make_keyring(&keys, "primary");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    create_repository_backup(data.path(), &repo, options(Some(&keyring), true, false)).unwrap();
    drop(store);
    fs::remove_file(keys.path().join("primary.key")).unwrap();
    let error = rekey_repository(
        &repo,
        RepositoryRekeyOptions {
            passphrase: Some(&passphrase()),
            keyring: &keyring,
            dry_run: false,
            max_snapshots: None,
        },
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("BACKUP_KEY_MATERIAL_UNAVAILABLE"),
        "{error:#}"
    );
}

#[test]
fn rekey_resumes_after_a_partial_manifest_rewrite() {
    let data = TempDir::new().unwrap();
    let keys = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let old_keyring = make_keyring(&keys, "old");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    let first =
        create_repository_backup(data.path(), &repo, options(Some(&old_keyring), true, false))
            .unwrap();
    let second =
        create_repository_backup(data.path(), &repo, options(Some(&old_keyring), true, true))
            .unwrap();
    let third =
        create_repository_backup(data.path(), &repo, options(Some(&old_keyring), true, true))
            .unwrap();
    drop(store);

    let rotated = rotate_to(&keys, "new");
    let mut rotated = rotated;
    rotated.revoke_key("old", false).unwrap();
    let rotated = Keyring::load(keys.path()).unwrap();
    let new_credential = rotated.signing_credential(None).unwrap();

    // Simulate a crash after the first manifest was re-signed: the metadata
    // still names the old key and its signed head is stale.
    let mut manifest = manifest_signature(&repo, &first.snapshot_id, &rotated);
    let metadata = parse_metadata(&read_metadata_bytes(&repo).unwrap()).unwrap();
    sign_snapshot_manifest(&metadata.repository_id, &new_credential, &mut manifest).unwrap();
    rewrite_manifest(&repo, &manifest);
    assert_eq!(
        manifest_signature(&repo, &first.snapshot_id, &rotated)
            .signature
            .as_ref()
            .unwrap()
            .key_id,
        "new"
    );

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
    assert_eq!(report.snapshots_total, 3);
    assert_eq!(report.snapshots_resigned, 2);
    assert_eq!(report.snapshots_unchanged, 1);
    assert_eq!(report.generation, 3);
    for report in [&first, &second, &third] {
        assert_eq!(
            manifest_signature(&repo, &report.snapshot_id, &rotated)
                .signature
                .as_ref()
                .unwrap()
                .key_id,
            "new"
        );
    }
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
    assert!(verified.signed_head_consistent);
}

#[test]
fn rekey_preserves_unsigned_history_in_a_mixed_upgrade() {
    let data = TempDir::new().unwrap();
    let keys = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let old_keyring = make_keyring(&keys, "old");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    let v2 = create_repository_backup(data.path(), &repo, options(None, true, false)).unwrap();
    let signed =
        create_repository_backup(data.path(), &repo, options(Some(&old_keyring), true, true))
            .unwrap();
    drop(store);

    let rotated = rotate_to(&keys, "new");
    let mut rotated = rotated;
    rotated.revoke_key("old", false).unwrap();
    let rotated = Keyring::load(keys.path()).unwrap();
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
    assert_eq!(report.snapshots_total, 2);
    assert_eq!(report.snapshots_resigned, 1);
    assert_eq!(report.snapshots_unsigned_preserved, 1);
    assert_eq!(report.snapshots_unchanged, 0);
    assert_eq!(report.generation, 2);

    let metadata = parse_metadata(&read_metadata_bytes(&repo).unwrap()).unwrap();
    assert!(
        !metadata.signing.as_ref().unwrap().required,
        "an upgraded chain must keep its unsigned history explicit"
    );
    let v2_manifest = manifest_signature(&repo, &v2.snapshot_id, &rotated);
    assert!(v2_manifest.signature.is_none());
    let signed_manifest = manifest_signature(&repo, &signed.snapshot_id, &rotated);
    assert_eq!(signed_manifest.signature.as_ref().unwrap().key_id, "new");

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
    assert_eq!(verified.authenticity, "mixed");
    assert_eq!(verified.signed_snapshots, 1);
    assert_eq!(verified.unsigned_snapshots, 1);
}

#[test]
fn rekey_plaintext_repository_needs_no_passphrase() {
    let data = TempDir::new().unwrap();
    let keys = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let old_keyring = make_keyring(&keys, "old");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    create_repository_backup(
        data.path(),
        &repo,
        options(Some(&old_keyring), false, false),
    )
    .unwrap();
    create_repository_backup(data.path(), &repo, options(Some(&old_keyring), false, true)).unwrap();
    drop(store);
    let blobs_before = blob_files(&repo);

    let rotated = rotate_to(&keys, "new");
    let mut rotated = rotated;
    rotated.revoke_key("old", false).unwrap();
    let rotated = Keyring::load(keys.path()).unwrap();
    let report = rekey_repository(
        &repo,
        RepositoryRekeyOptions {
            passphrase: None,
            keyring: &rotated,
            dry_run: false,
            max_snapshots: None,
        },
    )
    .unwrap();
    assert_eq!(report.snapshots_resigned, 2);
    assert!(!report.encrypted);
    assert!(report.blob_ciphertext_unchanged);
    assert_eq!(blob_files(&repo), blobs_before);
    let verified = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: None,
            keyring: Some(&rotated),
            ..RepositoryVerifyOptions::default()
        },
    )
    .unwrap();
    assert!(verified.valid, "{:?}", verified.issues);
}

#[test]
fn rekey_reports_and_events_never_contain_seed_material() {
    let data = TempDir::new().unwrap();
    let keys = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let old_keyring = make_keyring(&keys, "old");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    create_repository_backup(data.path(), &repo, options(Some(&old_keyring), true, false)).unwrap();
    drop(store);
    let key_text = fs::read_to_string(keys.path().join("old.key")).unwrap();
    let key_json: serde_json::Value = serde_json::from_str(&key_text).unwrap();
    let seed_b64 = key_json["seed_b64"].as_str().unwrap();
    let seed = BASE64.decode(seed_b64).unwrap();

    let rotated = rotate_to(&keys, "new");
    let mut rotated = rotated;
    rotated.revoke_key("old", false).unwrap();
    let rotated = Keyring::load(keys.path()).unwrap();
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
    let rendered = serde_json::to_string(&report).unwrap();
    assert!(!rendered.contains(seed_b64));
    assert!(!rendered.contains("seed_b64"));
    assert!(!rendered.contains(PASSPHRASE));
    assert!(!format!("{report:?}").contains(seed_b64));

    let events = fs::read_to_string(repo.join("events.jsonl")).unwrap();
    assert!(events.contains("repository.rekeyed"), "{events}");
    assert!(!events.contains(seed_b64));
    assert!(!events.contains(String::from_utf8_lossy(&seed).as_ref()));

    for path in b64_files(&repo).keys() {
        let bytes = fs::read(path).unwrap();
        assert!(
            !bytes
                .windows(seed_b64.len())
                .any(|window| window == seed_b64.as_bytes()),
            "seed leaked into {}",
            path.display()
        );
    }
}

#[test]
fn rekey_preserves_prune_tombstones_and_a_stale_free_head() {
    let data = TempDir::new().unwrap();
    let keys = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let old_keyring = make_keyring(&keys, "old");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    for incremental in [false, true, true] {
        create_repository_backup(
            data.path(),
            &repo,
            options(Some(&old_keyring), true, incremental),
        )
        .unwrap();
    }
    drop(store);
    let old_credential = old_keyring.signing_credential(None).unwrap();
    let pruned = prune_repository_with_signing(
        &repo,
        1,
        false,
        Some(&passphrase()),
        Some(&old_keyring),
        Some(&old_credential),
    )
    .unwrap();
    assert_eq!(pruned.snapshots_pruned.len(), 2);
    let metadata_before = parse_metadata(&read_metadata_bytes(&repo).unwrap()).unwrap();
    assert_eq!(metadata_before.pruned_snapshots.len(), 2);
    let tombstones_before = metadata_before.pruned_snapshots.clone();

    let rotated = rotate_to(&keys, "new");
    let mut rotated = rotated;
    rotated.revoke_key("old", false).unwrap();
    let rotated = Keyring::load(keys.path()).unwrap();
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
    assert!(report.tombstones_preserved);
    assert_eq!(report.snapshots_total, 1);
    assert_eq!(report.snapshots_resigned, 1);
    assert_eq!(report.generation, 3);

    let metadata_after = parse_metadata(&read_metadata_bytes(&repo).unwrap()).unwrap();
    assert_eq!(metadata_after.repository_id, metadata_before.repository_id);
    assert_eq!(metadata_after.pruned_snapshots, tombstones_before);
    assert_eq!(
        metadata_after.signing.as_ref().unwrap().active_key_id,
        "new"
    );
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
    assert!(verified.chain_valid);
    assert!(verified.tombstones_valid);
    assert!(verified.signed_head_consistent);
    assert_eq!(verified.generation, 3);
    assert_eq!(verified.snapshot_count, 1);
    for pruned in &tombstones_before {
        assert!(!snapshot_manifest_path(&repo, &pruned.id).unwrap().exists());
    }
}

#[test]
fn rekey_to_a_merely_retired_key_is_allowed() {
    let data = TempDir::new().unwrap();
    let keys = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let old_keyring = make_keyring(&keys, "old");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    create_repository_backup(data.path(), &repo, options(Some(&old_keyring), true, false)).unwrap();
    drop(store);

    // Rotation alone revokes nothing. Rekey still moves all signatures to the
    // new active key and leaves the old public key usable for history.
    let rotated = rotate_to(&keys, "new");
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
    assert_eq!(report.snapshots_resigned, 1);
    assert!(report.revoked_source_key_ids.is_empty());
    assert_eq!(report.previous_key_ids, vec!["old".to_string()]);
    assert_eq!(report.key_id, "new");
    assert!(rotated.public_key("old").is_ok());
    assert!(!rotated.is_key_revoked("old").unwrap());
}

#[test]
fn rekey_can_be_batched_and_resumes_across_runs() {
    let data = TempDir::new().unwrap();
    let keys = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let old_keyring = make_keyring(&keys, "old");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    for incremental in [false, true, true] {
        create_repository_backup(
            data.path(),
            &repo,
            options(Some(&old_keyring), true, incremental),
        )
        .unwrap();
    }
    drop(store);
    let rotated = rotate_to(&keys, "new");
    let mut rotated = rotated;
    rotated.revoke_key("old", false).unwrap();
    let rotated = Keyring::load(keys.path()).unwrap();

    let invalid = rekey_repository(
        &repo,
        RepositoryRekeyOptions {
            passphrase: Some(&passphrase()),
            keyring: &rotated,
            dry_run: true,
            max_snapshots: Some(0),
        },
    )
    .unwrap_err();
    assert!(
        format!("{invalid:#}").contains("BACKUP_REKEY_REFUSED"),
        "{invalid:#}"
    );

    let first = rekey_repository(
        &repo,
        RepositoryRekeyOptions {
            passphrase: Some(&passphrase()),
            keyring: &rotated,
            dry_run: false,
            max_snapshots: Some(1),
        },
    )
    .unwrap();
    assert_eq!(first.snapshots_total, 3);
    assert_eq!(first.snapshots_pending, 3);
    assert_eq!(first.snapshots_resigned, 1);
    assert_eq!(first.snapshots_remaining, 2);
    assert!(!first.complete);
    assert_eq!(first.max_snapshots, 1);
    assert!(first.metadata_resigned);

    // A strict verify still fails closed while revoked-key signatures remain.
    let error = verify_repository(
        &repo,
        RepositoryVerifyOptions {
            passphrase: Some(&passphrase()),
            keyring: Some(&rotated),
            ..RepositoryVerifyOptions::default()
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("BACKUP_KEY_REVOKED"));

    let second = rekey_repository(
        &repo,
        RepositoryRekeyOptions {
            passphrase: Some(&passphrase()),
            keyring: &rotated,
            dry_run: false,
            max_snapshots: Some(1),
        },
    )
    .unwrap();
    assert_eq!(second.snapshots_pending, 2);
    assert_eq!(second.snapshots_resigned, 1);
    assert_eq!(second.snapshots_remaining, 1);
    assert!(!second.complete);

    let third = rekey_repository(
        &repo,
        RepositoryRekeyOptions {
            passphrase: Some(&passphrase()),
            keyring: &rotated,
            dry_run: false,
            max_snapshots: Some(1),
        },
    )
    .unwrap();
    assert_eq!(third.snapshots_pending, 1);
    assert_eq!(third.snapshots_resigned, 1);
    assert_eq!(third.snapshots_remaining, 0);
    assert!(third.complete);
    assert!(third.blob_ciphertext_unchanged);

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
    assert_eq!(verified.signed_snapshots, 3);
    assert_eq!(verified.generation, 3);

    let no_op = rekey_repository(
        &repo,
        RepositoryRekeyOptions {
            passphrase: Some(&passphrase()),
            keyring: &rotated,
            dry_run: false,
            max_snapshots: Some(1),
        },
    )
    .unwrap();
    assert!(no_op.complete);
    assert!(!no_op.changed);
    assert_eq!(no_op.snapshots_pending, 0);
    assert_eq!(no_op.snapshots_resigned, 0);
}

#[test]
fn rekey_appends_fresh_manifest_nonces_without_reusing_or_rewriting_blob_nonces() {
    let data = TempDir::new().unwrap();
    let keys = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let old_keyring = make_keyring(&keys, "old");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    create_repository_backup(data.path(), &repo, options(Some(&old_keyring), true, false)).unwrap();
    fs::write(
        data.path().join("projects.jsonl"),
        b"{\"id\":\"project-1\",\"name\":\"nonce\"}\n",
    )
    .unwrap();
    create_repository_backup(data.path(), &repo, options(Some(&old_keyring), true, true)).unwrap();
    drop(store);

    let nonce_text_before = fs::read_to_string(repo.join("nonces.jsonl")).unwrap();
    let lines_before: Vec<&str> = nonce_text_before.lines().collect();
    let mut nonces_before = BTreeSet::new();
    for line in &lines_before {
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        nonces_before.insert(value["nonce_b64"].as_str().unwrap().to_string());
    }
    assert_eq!(nonces_before.len(), lines_before.len());
    let blob_nonce_lines = lines_before
        .iter()
        .filter(|line| line.contains("\"kind\":\"blob\""))
        .count();

    let rotated = rotate_to(&keys, "new");
    let mut rotated = rotated;
    rotated.revoke_key("old", false).unwrap();
    let rotated = Keyring::load(keys.path()).unwrap();
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
    assert_eq!(report.snapshots_resigned, 2);

    let nonce_text_after = fs::read_to_string(repo.join("nonces.jsonl")).unwrap();
    assert!(
        nonce_text_after.starts_with(&nonce_text_before),
        "the nonce table must remain append-only"
    );
    let lines_after: Vec<&str> = nonce_text_after.lines().collect();
    assert_eq!(lines_after.len(), lines_before.len() + 2);
    let mut all_nonces = BTreeSet::new();
    for line in &lines_after {
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(
            all_nonces.insert(value["nonce_b64"].as_str().unwrap().to_string()),
            "rekey reused an AEAD nonce"
        );
    }
    let blob_nonce_lines_after = lines_after
        .iter()
        .filter(|line| line.contains("\"kind\":\"blob\""))
        .count();
    assert_eq!(blob_nonce_lines_after, blob_nonce_lines);
}

#[test]
fn rekey_dry_run_after_a_complete_rekey_reports_no_changes() {
    let data = TempDir::new().unwrap();
    let keys = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let old_keyring = make_keyring(&keys, "old");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    create_repository_backup(data.path(), &repo, options(Some(&old_keyring), true, false)).unwrap();
    drop(store);
    let rotated = rotate_to(&keys, "new");
    let mut rotated = rotated;
    rotated.revoke_key("old", false).unwrap();
    let rotated = Keyring::load(keys.path()).unwrap();
    rekey_repository(
        &repo,
        RepositoryRekeyOptions {
            passphrase: Some(&passphrase()),
            keyring: &rotated,
            dry_run: false,
            max_snapshots: None,
        },
    )
    .unwrap();

    let manifests = b64_files(&repo.join("snapshots"));
    let metadata = read_metadata_bytes(&repo).unwrap();
    let dry = rekey_repository(
        &repo,
        RepositoryRekeyOptions {
            passphrase: Some(&passphrase()),
            keyring: &rotated,
            dry_run: true,
            max_snapshots: Some(1),
        },
    )
    .unwrap();
    assert!(!dry.changed);
    assert!(!dry.applied);
    assert!(!dry.metadata_resigned);
    assert_eq!(dry.snapshots_pending, 0);
    assert_eq!(dry.snapshots_resigned, 0);
    assert!(dry.complete);
    assert!(dry.blob_ciphertext_unchanged);
    assert_eq!(b64_files(&repo.join("snapshots")), manifests);
    assert_eq!(read_metadata_bytes(&repo).unwrap(), metadata);
}

#[test]
fn rekey_refuses_a_tampered_source_signature_before_writing_anything() {
    let data = TempDir::new().unwrap();
    let keys = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let old_keyring = make_keyring(&keys, "old");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    let report =
        create_repository_backup(data.path(), &repo, options(Some(&old_keyring), true, false))
            .unwrap();
    drop(store);

    // Rewrite the manifest body and re-seal it, but keep the old signature.
    let mut manifest = manifest_signature(&repo, &report.snapshot_id, &old_keyring);
    manifest.entries[0].sha256 = "b".repeat(64);
    rewrite_manifest(&repo, &manifest);
    let metadata_before = read_metadata_bytes(&repo).unwrap();
    let manifests_before = b64_files(&repo.join("snapshots"));
    let blobs_before = blob_files(&repo);

    let rotated = rotate_to(&keys, "new");
    let mut rotated = rotated;
    rotated.revoke_key("old", false).unwrap();
    let rotated = Keyring::load(keys.path()).unwrap();
    let error = rekey_repository(
        &repo,
        RepositoryRekeyOptions {
            passphrase: Some(&passphrase()),
            keyring: &rotated,
            dry_run: false,
            max_snapshots: None,
        },
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("BACKUP_SIGNATURE_INVALID"),
        "{error:#}"
    );
    assert_eq!(read_metadata_bytes(&repo).unwrap(), metadata_before);
    assert_eq!(b64_files(&repo.join("snapshots")), manifests_before);
    assert_eq!(blob_files(&repo), blobs_before);
}

#[test]
fn rekey_repairs_a_stale_signed_head_even_without_signature_changes() {
    let data = TempDir::new().unwrap();
    let keys = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let old_keyring = make_keyring(&keys, "old");
    let (store, _) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    create_repository_backup(data.path(), &repo, options(Some(&old_keyring), true, false)).unwrap();
    drop(store);
    let rotated = rotate_to(&keys, "new");
    rekey_repository(
        &repo,
        RepositoryRekeyOptions {
            passphrase: Some(&passphrase()),
            keyring: &rotated,
            dry_run: false,
            max_snapshots: None,
        },
    )
    .unwrap();

    // Damage only the signed head and re-sign the metadata with the current
    // key: signatures are fine, the head is stale.
    let mut metadata = parse_metadata(&read_metadata_bytes(&repo).unwrap()).unwrap();
    metadata
        .signing
        .as_mut()
        .unwrap()
        .head
        .as_mut()
        .unwrap()
        .latest_manifest_sha256 = "b".repeat(64);
    let crypto = unlock_repository(&metadata, Some(&passphrase()))
        .unwrap()
        .unwrap();
    super::sign_metadata(&mut metadata, Some(&crypto)).unwrap();
    let credential = rotated.signing_credential(None).unwrap();
    super::signing::sign_repository_metadata(&mut metadata, &credential).unwrap();
    super::format::write_metadata_atomic(&repo, &metadata).unwrap();

    let dry = rekey_repository(
        &repo,
        RepositoryRekeyOptions {
            passphrase: Some(&passphrase()),
            keyring: &rotated,
            dry_run: true,
            max_snapshots: None,
        },
    )
    .unwrap();
    assert!(dry.metadata_resigned);
    assert!(dry.changed);
    assert_eq!(dry.snapshots_resigned, 0);
    assert_eq!(dry.snapshots_pending, 0);
    assert!(dry.complete);

    let repaired = rekey_repository(
        &repo,
        RepositoryRekeyOptions {
            passphrase: Some(&passphrase()),
            keyring: &rotated,
            dry_run: false,
            max_snapshots: None,
        },
    )
    .unwrap();
    assert!(repaired.metadata_resigned);
    assert_eq!(repaired.snapshots_resigned, 0);
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
    assert!(verified.signed_head_consistent);
}
