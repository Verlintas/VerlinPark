use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde_json::json;
use tempfile::TempDir;
use vp_protocol::now_rfc3339;
use vp_protocol::types::{Project, Session};
use vp_store::Store;

use super::*;
use crate::backup::crypto::{KdfConfig, Passphrase, sha256_hex};

const TEST_PASSPHRASE: &str = "correct horse battery staple 42";
const WRONG_PASSPHRASE: &str = "this is not the backup passphrase";

fn fast_kdf() -> KdfConfig {
    KdfConfig::fast_for_tests()
}

fn passphrase() -> Passphrase {
    Passphrase::new(TEST_PASSPHRASE.as_bytes().to_vec()).unwrap()
}

fn wrong_passphrase() -> Passphrase {
    Passphrase::new(WRONG_PASSPHRASE.as_bytes().to_vec()).unwrap()
}

fn options(encrypted: bool, incremental: bool, include_artifacts: bool) -> RepositoryBackupOptions {
    RepositoryBackupOptions {
        incremental,
        include_artifacts,
        compression: Compression::Zstd,
        encryption: if encrypted {
            RepositoryEncryption::Encrypted(passphrase())
        } else {
            RepositoryEncryption::Plaintext
        },
        kdf: fast_kdf(),
        signing: None,
    }
}

fn plain_options(incremental: bool, include_artifacts: bool) -> RepositoryBackupOptions {
    RepositoryBackupOptions {
        incremental,
        include_artifacts,
        compression: Compression::None,
        encryption: RepositoryEncryption::Plaintext,
        kdf: fast_kdf(),
        signing: None,
    }
}

fn seed_data_dir(dir: &Path) -> (Store, String) {
    let store = Store::open(dir).unwrap();
    let project = Project {
        id: "project-1".to_string(),
        name: "Repository backup test".to_string(),
        root: "/tmp/repository-backup-test".to_string(),
        default_branch: "main".to_string(),
        created_at: now_rfc3339(),
    };
    store.insert_project_snapshot(&project).unwrap();
    let session = Session {
        id: "session-1".to_string(),
        project_id: project.id,
        goal: "verify repository backup".to_string(),
        status: "idle".to_string(),
        priority: 0,
        worktree: None,
        branch: Some("session/repository-backup".to_string()),
        token_budget: Some(1000),
        cost_budget_usd: None,
        time_budget_secs: None,
        unlimited: false,
        cost_spent_usd: 0.0,
        base_commit: None,
        tokens_in: 0,
        tokens_out: 0,
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
    store
        .append_event(
            &session.id,
            "message.user",
            json!({"text": "back up this session"}),
        )
        .unwrap();
    (store, session.id)
}

fn create(data: &Path, repo: &Path, encrypted: bool, incremental: bool) -> RepositoryBackupReport {
    create_repository_backup(data, repo, options(encrypted, incremental, false)).unwrap()
}

fn list(repo: &Path, encrypted: bool, verify: bool) -> RepositoryListReport {
    if encrypted {
        list_repository_snapshots(repo, Some(&passphrase()), verify).unwrap()
    } else {
        list_repository_snapshots(repo, None, verify).unwrap()
    }
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

fn sentinel_target(dir: &TempDir, name: &str) -> PathBuf {
    let target = dir.path().join(name);
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("keep.txt"), b"do not lose me").unwrap();
    target
}

fn manifest_for(repo: &Path, snapshot_id: &str, encrypted: bool) -> format::SnapshotManifest {
    let bytes = read_metadata_bytes(repo).unwrap();
    let metadata = parse_metadata(&bytes).unwrap();
    let crypto = if encrypted {
        Some(
            unlock_repository(&metadata, Some(&passphrase()))
                .unwrap()
                .unwrap(),
        )
    } else {
        None
    };
    load_snapshot(repo, &metadata, crypto.as_ref(), None, snapshot_id)
        .unwrap()
        .manifest
}

#[test]
fn plaintext_repository_round_trip_and_v1_compatibility() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, session_id) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    let report = create_repository_backup(data.path(), &repo, plain_options(false, false)).unwrap();
    drop(store);

    assert_eq!(report.format_version, 2);
    assert_eq!(report.snapshot_format_version, 1);
    assert!(!report.encrypted);
    assert!(report.parent.is_none());
    assert_eq!(report.sequence, 1);
    assert_eq!(report.entries, 2);
    assert_eq!(report.sessions, 1);
    assert_eq!(report.artifacts, 0);
    assert!(report.bytes > 0);
    assert_eq!(report.written_bytes, report.bytes);
    assert_eq!(report.deduplicated_bytes, 0);
    assert_eq!(report.manifest_sha256.len(), 64);
    assert_eq!(report.manifest_plaintext_sha256.len(), 64);
    assert!(!report.incremental);
    assert!(report.kdf.is_none());
    assert!(repo.join("repository.json").is_file());
    assert!(repo.join("snapshots").is_dir());
    assert!(repo.join("blobs").is_dir());
    assert!(repo.join("nonces.jsonl").is_file());
    assert!(repo.join("events.jsonl").is_file());

    let listed = list(&repo, false, true);
    assert_eq!(listed.snapshot_count, 1);
    assert!(listed.validation.valid);
    assert_eq!(listed.validation.blobs_missing, 0);
    assert_eq!(listed.validation.blobs_corrupt, 0);
    assert_eq!(listed.validation.blobs_verified, listed.unique_blobs);
    assert_eq!(listed.snapshots[0].snapshot_id, report.snapshot_id);

    let target = output.path().join("restored");
    let restored =
        restore_repository_snapshot(&repo, &report.snapshot_id, &target, false, None).unwrap();
    assert_eq!(restored.snapshot_id, report.snapshot_id);
    assert_eq!(restored.entries, 2);
    assert_eq!(restored.sessions, 1);
    assert!(!restored.replaced_existing);
    assert_eq!(restored.validated_blobs, listed.unique_blobs);

    let store = Store::open(&target).unwrap();
    let session = store.get_session(&session_id).unwrap().unwrap();
    assert_eq!(session.goal, "verify repository backup");
    assert_eq!(store.list_all_events(&session_id).unwrap().len(), 2);
    assert_eq!(
        store.read_event_mirror(&session_id).unwrap().unwrap().len(),
        2
    );

    // v1 ZIP remains available and unchanged in the same build.
    let zip = output.path().join("backup.zip");
    let v1 = crate::backup::create_backup(data.path(), &zip, false).unwrap();
    assert_eq!(v1.format_version, 1);
    let v1_target = output.path().join("v1-restored");
    let v1_restored = crate::backup::restore_backup(&zip, &v1_target, false).unwrap();
    assert_eq!(v1_restored.format_version, 1);
    assert!(v1_target.join(DATABASE_FILENAME).is_file());
}

#[test]
fn encrypted_repository_round_trip_and_no_secret_in_any_file() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, session_id) = seed_data_dir(data.path());
    let repo = output.path().join("repository");
    let report = create(data.path(), &repo, true, false);
    drop(store);

    assert!(report.encrypted);
    assert!(report.kdf.is_some());
    let kdf = report.kdf.as_ref().unwrap();
    assert_eq!(kdf.algorithm, "argon2id");
    assert_eq!(kdf.output_len, 32);
    assert_eq!(kdf.m_cost_kib, KdfConfig::fast_for_tests().m_cost_kib);
    assert!(kdf.fingerprint.len() == 64);

    let listed = list(&repo, true, true);
    assert!(listed.encrypted);
    assert_eq!(listed.snapshot_count, 1);
    assert!(listed.validation.valid);

    // No repository file, including metadata and append-only logs, contains
    // the passphrase or a plaintext copy of the database.
    for path in all_files(&repo) {
        let bytes = fs::read(&path).unwrap();
        assert!(
            !contains_subslice(&bytes, TEST_PASSPHRASE.as_bytes()),
            "passphrase leaked into {}",
            path.display()
        );
        assert!(
            !contains_subslice(&bytes, b"SQLite format 3\0"),
            "plaintext database leaked into {}",
            path.display()
        );
        assert!(
            !contains_subslice(&bytes, b"back up this session"),
            "plaintext event text leaked into {}",
            path.display()
        );
    }

    let target = output.path().join("restored");
    let restored = restore_repository_snapshot(
        &repo,
        &report.snapshot_id,
        &target,
        false,
        Some(&passphrase()),
    )
    .unwrap();
    assert!(restored.encrypted);
    let store = Store::open(&target).unwrap();
    assert_eq!(
        store.get_session(&session_id).unwrap().unwrap().goal,
        "verify repository backup"
    );
    assert_eq!(store.list_all_events(&session_id).unwrap().len(), 2);
}

#[test]
fn incremental_snapshots_reuse_blobs_and_report_exact_dedup_bytes() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let projects = data.path().join("projects.jsonl");
    fs::write(&projects, b"{\"id\":\"project-1\",\"note\":\"initial\"}\n").unwrap();
    let repo = output.path().join("repository");
    let first = create(data.path(), &repo, true, false);
    assert_eq!(first.parent, None);
    assert_eq!(first.written_bytes, first.bytes);

    let second = create(data.path(), &repo, true, true);
    assert_eq!(second.parent.as_deref(), Some(first.snapshot_id.as_str()));
    assert_eq!(second.sequence, 2);
    assert_eq!(second.written_bytes, 0);
    assert_eq!(second.blobs_written, 0);
    assert_eq!(second.blobs_reused, first.entries);
    assert_eq!(second.deduplicated_bytes, second.bytes);
    assert_eq!(second.deduplicated_from_parent_bytes, second.bytes);
    assert_eq!(second.deduplicated_from_store_bytes, 0);
    assert_eq!(second.bytes, first.bytes);

    // Change only projects.jsonl. The SQLite snapshot and event mirror are
    // byte-identical, so exactly one new blob is written.
    let changed = b"{\"id\":\"project-1\",\"note\":\"changed after the first snapshot\"}\n";
    fs::write(&projects, changed).unwrap();
    let third = create(data.path(), &repo, true, true);
    assert_eq!(third.parent.as_deref(), Some(second.snapshot_id.as_str()));
    assert_eq!(third.sequence, 3);
    assert_eq!(third.blobs_written, 1);
    assert_eq!(third.written_bytes, changed.len() as u64);
    assert_eq!(third.deduplicated_bytes, third.bytes - changed.len() as u64);
    assert!(third.deduplicated_from_parent_bytes > 0);
    assert_eq!(
        third.deduplicated_from_parent_bytes + third.deduplicated_from_store_bytes,
        third.deduplicated_bytes
    );

    // A non-incremental snapshot still uses content addressing but does not
    // require a parent for creation.
    let full_again = create(data.path(), &repo, true, false);
    assert!(!full_again.incremental);
    assert_eq!(full_again.blobs_written, 0);
    assert_eq!(full_again.written_bytes, 0);
    assert!(full_again.deduplicated_bytes > 0);

    let listed = list(&repo, true, true);
    assert_eq!(listed.snapshot_count, 4);
    assert!(listed.validation.valid);
    let sequences: Vec<u64> = listed
        .snapshots
        .iter()
        .map(|snapshot| snapshot.sequence)
        .collect();
    assert_eq!(sequences, vec![1, 2, 3, 4]);
}

#[test]
fn incremental_without_a_parent_fails_explicitly() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let repo = output.path().join("repository");
    let error =
        create_repository_backup(data.path(), &repo, options(true, true, false)).unwrap_err();
    assert!(
        format!("{error:#}").contains("--incremental requires an existing snapshot"),
        "unexpected error: {error:#}"
    );
    // The failed attempt initialized an empty repository but wrote no snapshot.
    assert!(repo.join("repository.json").is_file());
    let listed = list(&repo, true, false);
    assert_eq!(listed.snapshot_count, 0);
}

#[test]
fn parent_chain_restore_and_pruned_parent_tombstone() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, session_id) = seed_data_dir(data.path());
    drop(store);
    let repo = output.path().join("repository");
    let first = create(data.path(), &repo, true, false);

    let store = Store::open(data.path()).unwrap();
    store
        .append_event(
            &session_id,
            "message.user",
            json!({"text": "a second message"}),
        )
        .unwrap();
    drop(store);
    let second = create(data.path(), &repo, true, true);
    assert_eq!(second.parent.as_deref(), Some(first.snapshot_id.as_str()));

    let target_second = output.path().join("second");
    restore_repository_snapshot(
        &repo,
        &second.snapshot_id,
        &target_second,
        false,
        Some(&passphrase()),
    )
    .unwrap();
    let store = Store::open(&target_second).unwrap();
    assert_eq!(store.list_all_events(&session_id).unwrap().len(), 3);

    let target_first = output.path().join("first");
    restore_repository_snapshot(
        &repo,
        &first.snapshot_id,
        &target_first,
        false,
        Some(&passphrase()),
    )
    .unwrap();
    let store = Store::open(&target_first).unwrap();
    assert_eq!(store.list_all_events(&session_id).unwrap().len(), 2);

    // Prune everything except the latest snapshot. The retained manifest's
    // parent is now represented by an authenticated tombstone, not a missing
    // file, and the retained snapshot remains restorable.
    let prune = prune_repository(&repo, 1, false, Some(&passphrase())).unwrap();
    assert!(prune.applied);
    assert_eq!(prune.snapshots_pruned.len(), 1);
    let listed = list(&repo, true, true);
    assert_eq!(listed.snapshot_count, 1);
    assert_eq!(listed.snapshots[0].snapshot_id, second.snapshot_id);
    let target_after_prune = output.path().join("after-prune");
    restore_repository_snapshot(
        &repo,
        &second.snapshot_id,
        &target_after_prune,
        false,
        Some(&passphrase()),
    )
    .unwrap();
    let store = Store::open(&target_after_prune).unwrap();
    assert_eq!(store.list_all_events(&session_id).unwrap().len(), 3);

    // A new incremental snapshot after the prune continues the retained
    // prefix with the latest snapshot as its parent; the older tombstones do
    // not make the next sequence invalid.
    let after_prune = create(data.path(), &repo, true, true);
    assert_eq!(after_prune.sequence, 3);
    assert_eq!(
        after_prune.parent.as_deref(),
        Some(second.snapshot_id.as_str())
    );
    let listed = list(&repo, true, true);
    assert_eq!(listed.snapshot_count, 2);
    let target_after_incremental = output.path().join("after-incremental");
    restore_repository_snapshot(
        &repo,
        &after_prune.snapshot_id,
        &target_after_incremental,
        false,
        Some(&passphrase()),
    )
    .unwrap();
    assert!(target_after_incremental.join(DATABASE_FILENAME).is_file());
}

#[test]
fn wrong_passphrase_fails_before_the_target_or_repository_is_touched() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let repo = output.path().join("repository");
    let report = create(data.path(), &repo, true, false);
    let metadata_before = fs::read(repo.join("repository.json")).unwrap();
    let target = sentinel_target(&output, "target");

    let error = restore_repository_snapshot(
        &repo,
        &report.snapshot_id,
        &target,
        true,
        Some(&wrong_passphrase()),
    )
    .unwrap_err();
    let text = format!("{error:#}");
    assert!(
        text.contains("authentication failed") || text.contains("wrong"),
        "unexpected error: {text}"
    );
    assert!(
        !text.contains(WRONG_PASSPHRASE),
        "passphrase leaked in error: {text}"
    );
    assert!(
        !text.contains(TEST_PASSPHRASE),
        "passphrase leaked in error: {text}"
    );
    assert_eq!(
        fs::read(target.join("keep.txt")).unwrap(),
        b"do not lose me"
    );
    assert!(!target.join(DATABASE_FILENAME).exists());
    assert_eq!(
        fs::read(repo.join("repository.json")).unwrap(),
        metadata_before
    );

    let error = list_repository_snapshots(&repo, Some(&wrong_passphrase()), false).unwrap_err();
    assert!(!format!("{error:#}").contains(WRONG_PASSPHRASE));
    let error = prune_repository(&repo, 1, true, Some(&wrong_passphrase())).unwrap_err();
    assert!(!format!("{error:#}").contains(WRONG_PASSPHRASE));
}

#[test]
fn tampered_blob_manifest_and_truncated_blob_fail_atomically() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let repo = output.path().join("repository");
    let report = create(data.path(), &repo, true, false);
    let manifest = manifest_for(&repo, &report.snapshot_id, true);
    let database_entry = manifest
        .entries
        .iter()
        .find(|entry| entry.path == DATABASE_FILENAME)
        .unwrap();
    let blob = blob_path(&repo, &database_entry.sha256).unwrap();
    let original_blob = fs::read(&blob).unwrap();

    let target = sentinel_target(&output, "target");
    let mut tampered = original_blob.clone();
    let index = BLOB_HEADER_BYTES + 10;
    tampered[index] ^= 0xff;
    fs::write(&blob, &tampered).unwrap();
    let error = restore_repository_snapshot(
        &repo,
        &report.snapshot_id,
        &target,
        true,
        Some(&passphrase()),
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("restoring backup entry")
            || format!("{error:#}").contains("authentication"),
        "unexpected error: {error:#}"
    );
    assert_eq!(
        fs::read(target.join("keep.txt")).unwrap(),
        b"do not lose me"
    );
    assert!(!target.join(DATABASE_FILENAME).exists());
    fs::write(&blob, &original_blob).unwrap();

    // Truncate the final chunk. Restore must still leave the target untouched.
    fs::write(&blob, &original_blob[..original_blob.len() - 1]).unwrap();
    assert!(
        restore_repository_snapshot(
            &repo,
            &report.snapshot_id,
            &target,
            true,
            Some(&passphrase())
        )
        .is_err()
    );
    assert_eq!(
        fs::read(target.join("keep.txt")).unwrap(),
        b"do not lose me"
    );
    fs::write(&blob, &original_blob).unwrap();

    // Tamper with the authenticated snapshot manifest.
    let manifest_path = snapshot_manifest_path(&repo, &report.snapshot_id).unwrap();
    let original_manifest = fs::read(&manifest_path).unwrap();
    let mut tampered_manifest = original_manifest.clone();
    let last = tampered_manifest.len() - 1;
    tampered_manifest[last] ^= 0x01;
    fs::write(&manifest_path, tampered_manifest).unwrap();
    let error = restore_repository_snapshot(
        &repo,
        &report.snapshot_id,
        &target,
        true,
        Some(&passphrase()),
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("authenticating")
            || format!("{error:#}").contains("authentication"),
        "unexpected error: {error:#}"
    );
    assert_eq!(
        fs::read(target.join("keep.txt")).unwrap(),
        b"do not lose me"
    );
    fs::write(&manifest_path, original_manifest).unwrap();

    // The now-valid repository restores successfully.
    let restored = restore_repository_snapshot(
        &repo,
        &report.snapshot_id,
        &target,
        true,
        Some(&passphrase()),
    )
    .unwrap();
    assert!(restored.replaced_existing);
    assert!(!target.join("keep.txt").exists());
    assert!(target.join(DATABASE_FILENAME).is_file());
}

#[test]
fn missing_or_truncated_parent_chain_fails_closed() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let repo = output.path().join("repository");
    let first = create(data.path(), &repo, true, false);
    let second = create(data.path(), &repo, true, true);
    let first_path = snapshot_manifest_path(&repo, &first.snapshot_id).unwrap();
    let first_bytes = fs::read(&first_path).unwrap();

    fs::remove_file(&first_path).unwrap();
    let target = sentinel_target(&output, "target");
    let error = restore_repository_snapshot(
        &repo,
        &second.snapshot_id,
        &target,
        true,
        Some(&passphrase()),
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("chain")
            || format!("{error:#}").contains("sequence")
            || format!("{error:#}").contains("missing parent"),
        "unexpected error: {error:#}"
    );
    assert_eq!(
        fs::read(target.join("keep.txt")).unwrap(),
        b"do not lose me"
    );
    assert!(!target.join(DATABASE_FILENAME).exists());

    fs::write(&first_path, &first_bytes[..first_bytes.len() / 2]).unwrap();
    let error = restore_repository_snapshot(
        &repo,
        &second.snapshot_id,
        &target,
        true,
        Some(&passphrase()),
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("truncated")
            || format!("{error:#}").contains("authentication")
            || format!("{error:#}").contains("chain")
            || format!("{error:#}").contains("length mismatch"),
        "unexpected error: {error:#}"
    );
    assert_eq!(
        fs::read(target.join("keep.txt")).unwrap(),
        b"do not lose me"
    );
}

#[test]
fn bad_kdf_parameters_and_tampered_metadata_are_rejected() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let repo = output.path().join("repository");
    let report = create(data.path(), &repo, true, false);
    let metadata_path = repo.join("repository.json");
    let original = fs::read(&metadata_path).unwrap();

    let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
    value["encryption"]["kdf"]["m_cost_kib"] = json!(4096);
    fs::write(&metadata_path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    let error = list_repository_snapshots(&repo, Some(&passphrase()), false).unwrap_err();
    assert!(
        format!("{error:#}").contains("memory cost"),
        "unexpected error: {error:#}"
    );

    let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
    value["updated_at"] = json!("2099-01-01T00:00:00Z");
    fs::write(&metadata_path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    let error = list_repository_snapshots(&repo, Some(&passphrase()), false).unwrap_err();
    assert!(
        format!("{error:#}").contains("authentication failed"),
        "unexpected error: {error:#}"
    );

    fs::write(&metadata_path, &original).unwrap();
    let restored = list(&repo, true, true);
    assert_eq!(restored.snapshot_count, 1);
    assert_eq!(restored.snapshots[0].snapshot_id, report.snapshot_id);
}

#[test]
fn unauthenticated_tampered_manifest_path_is_still_rejected_by_the_allowlist() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let repo = output.path().join("repository");
    let report = create_repository_backup(data.path(), &repo, plain_options(false, false)).unwrap();
    let manifest = manifest_for(&repo, &report.snapshot_id, false);
    let mut value = serde_json::to_value(&manifest).unwrap();
    value["entries"][0]["path"] = json!("../escape.txt");
    let json = serde_json::to_vec(&value).unwrap();
    let sealed =
        super::super::crypto::seal_manifest_plain("repository-id", &report.snapshot_id, &json)
            .unwrap();
    // The repository id in the AAD is intentionally irrelevant to plaintext
    // sealing; this simulates an attacker who rewrites both manifest and body.
    fs::write(
        snapshot_manifest_path(&repo, &report.snapshot_id).unwrap(),
        &sealed.bytes,
    )
    .unwrap();
    let target = sentinel_target(&output, "target");
    let error =
        restore_repository_snapshot(&repo, &report.snapshot_id, &target, true, None).unwrap_err();
    assert!(
        format!("{error:#}").contains("outside the backup allowlist")
            || format!("{error:#}").contains("unsafe"),
        "unexpected error: {error:#}"
    );
    assert_eq!(
        fs::read(target.join("keep.txt")).unwrap(),
        b"do not lose me"
    );
    assert!(!target.join(DATABASE_FILENAME).exists());
}

#[test]
fn symlinked_blob_snapshot_repository_and_outside_paths_are_rejected() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let repo = output.path().join("repository");
    let report = create_repository_backup(data.path(), &repo, plain_options(false, false)).unwrap();
    let manifest = manifest_for(&repo, &report.snapshot_id, false);
    let entry = &manifest.entries[0];
    let blob = blob_path(&repo, &entry.sha256).unwrap();
    let original = fs::read(&blob).unwrap();
    let outside = output.path().join("outside.blob");
    fs::write(&outside, &original).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let backup = output.path().join("original.blob");
        fs::rename(&blob, &backup).unwrap();
        symlink(&outside, &blob).unwrap();
        let target = sentinel_target(&output, "target");
        assert!(
            restore_repository_snapshot(&repo, &report.snapshot_id, &target, true, None).is_err()
        );
        assert_eq!(
            fs::read(target.join("keep.txt")).unwrap(),
            b"do not lose me"
        );
        fs::remove_file(&blob).unwrap();
        fs::rename(&backup, &blob).unwrap();

        let manifest_path = snapshot_manifest_path(&repo, &report.snapshot_id).unwrap();
        let manifest_backup = output.path().join("original.manifest");
        fs::rename(&manifest_path, &manifest_backup).unwrap();
        symlink(&manifest_backup, &manifest_path).unwrap();
        assert!(list_repository_snapshots(&repo, None, false).is_err());
        fs::remove_file(&manifest_path).unwrap();
        fs::rename(&manifest_backup, &manifest_path).unwrap();

        let linked_repo = output.path().join("linked-repository");
        symlink(&repo, &linked_repo).unwrap();
        assert!(list_repository_snapshots(&linked_repo, None, false).is_err());
    }

    let nested_repo = data.path().join("nested-repository");
    let error = create_repository_backup(data.path(), &nested_repo, plain_options(false, false))
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("must not overlap data directory"),
        "unexpected error: {error:#}"
    );
}

#[test]
fn prune_dry_run_preserves_files_and_real_prune_protects_shared_blobs() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let projects = data.path().join("projects.jsonl");
    fs::write(&projects, b"{\"id\":\"project-1\",\"note\":\"stable\"}\n").unwrap();
    let repo = output.path().join("repository");
    let first = create(data.path(), &repo, true, false);
    let second = create(data.path(), &repo, true, true);
    fs::write(&projects, b"{\"id\":\"project-1\",\"note\":\"changed\"}\n").unwrap();
    let third = create(data.path(), &repo, true, true);
    assert_eq!(third.sequence, 3);
    assert_eq!(second.parent.as_deref(), Some(first.snapshot_id.as_str()));
    let first_manifest = manifest_for(&repo, &first.snapshot_id, true);
    let third_manifest = manifest_for(&repo, &third.snapshot_id, true);
    let shared_hash = first_manifest
        .entries
        .iter()
        .find(|entry| entry.path == DATABASE_FILENAME)
        .unwrap()
        .sha256
        .clone();
    assert!(
        third_manifest
            .entries
            .iter()
            .any(|entry| entry.sha256 == shared_hash),
        "the database blob should be shared across unchanged snapshots"
    );
    let metadata_before = fs::read(repo.join("repository.json")).unwrap();
    let files_before = all_files(&repo);

    let dry = prune_repository(&repo, 1, true, Some(&passphrase())).unwrap();
    assert!(dry.dry_run);
    assert!(!dry.applied);
    assert_eq!(dry.snapshots_total, 3);
    assert_eq!(dry.snapshots_kept, 1);
    assert_eq!(dry.snapshots_pruned.len(), 2);
    assert!(dry.reclaimed_bytes > 0);
    assert!(dry.protected_bytes > 0);
    assert_eq!(
        fs::read(repo.join("repository.json")).unwrap(),
        metadata_before
    );
    assert_eq!(all_files(&repo).len(), files_before.len());
    assert_eq!(list(&repo, true, false).snapshot_count, 3);

    let real = prune_repository(&repo, 1, false, Some(&passphrase())).unwrap();
    assert!(real.applied);
    assert_eq!(real.snapshots_pruned.len(), 2);
    assert_eq!(real.blobs_pruned, dry.blobs_pruned);
    assert_eq!(real.reclaimed_bytes, dry.reclaimed_bytes);
    assert_eq!(list(&repo, true, true).snapshot_count, 1);
    let listed = list_repository_snapshots(&repo, Some(&passphrase()), true).unwrap();
    assert_eq!(listed.snapshots[0].snapshot_id, third.snapshot_id);
    assert_eq!(listed.validation.blobs_missing, 0);

    // A blob shared by a pruned and a retained snapshot is protected by the
    // retained reference and remains on disk.
    assert!(blob_path(&repo, &shared_hash).unwrap().is_file());
    let latest_manifest = manifest_for(&repo, &third.snapshot_id, true);
    let projects_entry = latest_manifest
        .entries
        .iter()
        .find(|entry| entry.path == "projects.jsonl")
        .unwrap();
    assert!(blob_path(&repo, &projects_entry.sha256).unwrap().is_file());

    let target = output.path().join("restored-after-prune");
    restore_repository_snapshot(
        &repo,
        &third.snapshot_id,
        &target,
        false,
        Some(&passphrase()),
    )
    .unwrap();
    let store = Store::open(&target).unwrap();
    assert_eq!(
        store.get_session("session-1").unwrap().unwrap().goal,
        "verify repository backup"
    );
}

#[test]
fn prune_refuses_zero_keep_and_missing_retained_blobs() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let repo = output.path().join("repository");
    let first = create(data.path(), &repo, true, false);
    let second = create(data.path(), &repo, true, true);
    assert!(prune_repository(&repo, 0, true, Some(&passphrase())).is_err());

    let manifest = manifest_for(&repo, &second.snapshot_id, true);
    let entry = &manifest.entries[0];
    let blob = blob_path(&repo, &entry.sha256).unwrap();
    fs::remove_file(&blob).unwrap();
    let error = prune_repository(&repo, 1, false, Some(&passphrase())).unwrap_err();
    assert!(
        format!("{error:#}").contains("missing"),
        "unexpected error: {error:#}"
    );
    // Both manifests survive because prune failed before the metadata commit.
    assert!(
        snapshot_manifest_path(&repo, &first.snapshot_id)
            .unwrap()
            .is_file()
    );
    assert!(
        snapshot_manifest_path(&repo, &second.snapshot_id)
            .unwrap()
            .is_file()
    );
}

#[test]
fn nonce_table_records_are_unique_and_tampering_is_rejected() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let repo = output.path().join("repository");
    let report = create(data.path(), &repo, true, false);
    let nonces = fs::read_to_string(repo.join(NONCES_FILENAME)).unwrap();
    let line_count = nonces.lines().count();
    assert!(line_count > 0);
    let mut unique = std::collections::BTreeSet::new();
    for line in nonces.lines() {
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(value["nonce_b64"].as_str().unwrap().len() > 12);
        assert!(!line.contains(TEST_PASSPHRASE));
        unique.insert(value["nonce_b64"].as_str().unwrap().to_string());
    }
    assert_eq!(unique.len(), line_count);
    assert!(nonces.lines().any(|line| line.contains("\"manifest\"")));
    assert!(nonces.lines().any(|line| line.contains("\"blob\"")));

    // Duplicating an audit record is rejected by the next repository open.
    let first_line = nonces.lines().next().unwrap().to_string();
    let mut duplicate = nonces.clone();
    duplicate.push_str(&format!("{}\n", first_line));
    fs::write(repo.join(NONCES_FILENAME), duplicate).unwrap();
    let error = list_repository_snapshots(&repo, Some(&passphrase()), false).unwrap_err();
    assert!(
        format!("{error:#}").contains("duplicate nonce"),
        "unexpected error: {error:#}"
    );
    fs::write(repo.join(NONCES_FILENAME), nonces).unwrap();
    let listed = list(&repo, true, false);
    assert_eq!(listed.nonce_records, line_count);
    assert_eq!(listed.snapshots[0].snapshot_id, report.snapshot_id);
}

#[test]
fn repository_initialization_and_encryption_mode_are_strict() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let repo = output.path().join("repository");
    create(data.path(), &repo, true, false);

    let mut plain_again = options(true, false, false);
    plain_again.encryption = RepositoryEncryption::Plaintext;
    let error = create_repository_backup(data.path(), &repo, plain_again).unwrap_err();
    assert!(
        format!("{error:#}").contains("encrypted") || format!("{error:#}").contains("passphrase"),
        "unexpected error: {error:#}"
    );

    let plain_repo = output.path().join("plain-repository");
    create_repository_backup(data.path(), &plain_repo, plain_options(false, false)).unwrap();
    let error = create_repository_backup(data.path(), &plain_repo, options(true, false, false))
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("not encrypted"),
        "unexpected error: {error:#}"
    );
    assert!(list_repository_snapshots(&plain_repo, Some(&passphrase()), false).is_err());

    let non_empty = output.path().join("non-empty");
    fs::create_dir_all(&non_empty).unwrap();
    fs::write(non_empty.join("operator.txt"), b"do not delete").unwrap();
    let error =
        create_repository_backup(data.path(), &non_empty, plain_options(false, false)).unwrap_err();
    assert!(
        format!("{error:#}").contains("non-empty"),
        "unexpected error: {error:#}"
    );
    assert_eq!(
        fs::read(non_empty.join("operator.txt")).unwrap(),
        b"do not delete"
    );
}

#[test]
fn restore_target_preconditions_match_the_v1_contract() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let repo = output.path().join("repository");
    let report = create_repository_backup(data.path(), &repo, plain_options(false, false)).unwrap();

    let non_empty = output.path().join("non-empty-target");
    fs::create_dir_all(&non_empty).unwrap();
    fs::write(non_empty.join("keep.txt"), b"keep").unwrap();
    let error = restore_repository_snapshot(&repo, &report.snapshot_id, &non_empty, false, None)
        .unwrap_err();
    assert!(format!("{error:#}").contains("not empty"));
    assert_eq!(fs::read(non_empty.join("keep.txt")).unwrap(), b"keep");

    let daemon_target = output.path().join("daemon-target");
    fs::create_dir_all(daemon_target.join("run")).unwrap();
    fs::write(daemon_target.join("run/daemon.json"), b"{}").unwrap();
    let error = restore_repository_snapshot(&repo, &report.snapshot_id, &daemon_target, true, None)
        .unwrap_err();
    assert!(format!("{error:#}").contains("daemon"));
    assert!(daemon_target.join("run/daemon.json").exists());

    let lock_target = output.path().join("lock-target");
    fs::create_dir_all(lock_target.join("run")).unwrap();
    fs::write(lock_target.join("run/daemon.lock"), b"").unwrap();
    let error = restore_repository_snapshot(&repo, &report.snapshot_id, &lock_target, false, None)
        .unwrap_err();
    assert!(format!("{error:#}").contains("instance lock"));
}

#[test]
fn artifact_blobs_are_optional_in_the_repository_format() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let blob_name = format!("{}.zst", "b".repeat(64));
    fs::create_dir_all(data.path().join("artifacts/blobs")).unwrap();
    fs::write(
        data.path().join("artifacts/blobs").join(&blob_name),
        b"artifact bytes",
    )
    .unwrap();
    let repo = output.path().join("repository");
    let without =
        create_repository_backup(data.path(), &repo, plain_options(false, false)).unwrap();
    assert_eq!(without.artifacts, 0);
    assert!(!without.includes_artifacts);
    let listed = list_repository_snapshots(&repo, None, false).unwrap();
    assert_eq!(listed.snapshots[0].artifacts, 0);

    let repo_with = output.path().join("repository-with");
    let with =
        create_repository_backup(data.path(), &repo_with, plain_options(false, true)).unwrap();
    assert_eq!(with.artifacts, 1);
    assert!(with.includes_artifacts);
    let target = output.path().join("with-artifacts");
    restore_repository_snapshot(&repo_with, &with.snapshot_id, &target, false, None).unwrap();
    assert_eq!(
        fs::read(target.join("artifacts/blobs").join(&blob_name)).unwrap(),
        b"artifact bytes"
    );
}

#[test]
fn empty_files_and_chunk_boundaries_round_trip() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    fs::write(data.path().join("projects.jsonl"), b"").unwrap();
    for compression in [Compression::None, Compression::Zstd] {
        let repo = output
            .path()
            .join(format!("repository-{}", compression.as_str()));
        let mut opts = plain_options(false, false);
        opts.compression = compression;
        let report = create_repository_backup(data.path(), &repo, opts).unwrap();
        assert!(report.bytes > 0);
        let target = output
            .path()
            .join(format!("restored-{}", compression.as_str()));
        restore_repository_snapshot(&repo, &report.snapshot_id, &target, false, None).unwrap();
        assert_eq!(fs::read(target.join("projects.jsonl")).unwrap(), b"");
        assert!(target.join(DATABASE_FILENAME).is_file());
    }
}

#[test]
fn repository_lock_rejects_a_concurrent_mutation() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let repo = output.path().join("repository");
    create_repository_backup(data.path(), &repo, plain_options(false, false)).unwrap();
    let lock_path = repo.join(LOCK_FILENAME);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    file.try_lock_exclusive().unwrap();
    let error =
        create_repository_backup(data.path(), &repo, plain_options(true, false)).unwrap_err();
    assert!(
        format!("{error:#}").contains("locked"),
        "unexpected error: {error:#}"
    );
    file.unlock().unwrap();
    create_repository_backup(data.path(), &repo, plain_options(true, false)).unwrap();
}

#[test]
fn metadata_prune_tombstones_are_authenticated() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let repo = output.path().join("repository");
    create(data.path(), &repo, true, false);
    create(data.path(), &repo, true, true);
    prune_repository(&repo, 1, false, Some(&passphrase())).unwrap();
    let path = repo.join(METADATA_FILENAME);
    let original = fs::read(&path).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
    value["pruned_snapshots"] = json!([]);
    fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    let error = list_repository_snapshots(&repo, Some(&passphrase()), false).unwrap_err();
    assert!(
        format!("{error:#}").contains("authentication failed"),
        "unexpected error: {error:#}"
    );
    fs::write(&path, original).unwrap();
    assert_eq!(list(&repo, true, false).snapshot_count, 1);
}

#[test]
fn plaintext_repository_manifest_tampering_is_detected_by_self_hash() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let repo = output.path().join("repository");
    let report = create_repository_backup(data.path(), &repo, plain_options(false, false)).unwrap();
    let path = snapshot_manifest_path(&repo, &report.snapshot_id).unwrap();
    let mut bytes = fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    fs::write(&path, bytes).unwrap();
    assert!(list_repository_snapshots(&repo, None, false).is_err());
}

#[test]
fn restore_uses_a_sibling_scratch_and_leaves_no_scratch_on_success_or_failure() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let repo = output.path().join("repository");
    let report = create_repository_backup(data.path(), &repo, plain_options(false, false)).unwrap();
    let target_parent = output.path().join("targets");
    fs::create_dir_all(&target_parent).unwrap();
    let target = target_parent.join("restored");
    restore_repository_snapshot(&repo, &report.snapshot_id, &target, false, None).unwrap();
    let leftovers: Vec<String> = fs::read_dir(&target_parent)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with(".verlinpark-"))
        .collect();
    assert!(leftovers.is_empty(), "leftovers: {leftovers:?}");

    let failure_target = target_parent.join("failure");
    fs::create_dir_all(&failure_target).unwrap();
    fs::write(failure_target.join("keep.txt"), b"keep").unwrap();
    let manifest = manifest_for(&repo, &report.snapshot_id, false);
    let entry = &manifest.entries[0];
    let blob = blob_path(&repo, &entry.sha256).unwrap();
    let original = fs::read(&blob).unwrap();
    fs::write(&blob, &original[..BLOB_HEADER_BYTES]).unwrap();
    assert!(
        restore_repository_snapshot(&repo, &report.snapshot_id, &failure_target, true, None)
            .is_err()
    );
    let leftovers: Vec<String> = fs::read_dir(&target_parent)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with(".verlinpark-"))
        .collect();
    assert!(leftovers.is_empty(), "leftovers: {leftovers:?}");
    assert_eq!(fs::read(failure_target.join("keep.txt")).unwrap(), b"keep");
    fs::write(&blob, original).unwrap();
}

#[test]
fn list_reports_missing_and_unreferenced_blobs_without_lying_about_validity() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let repo = output.path().join("repository");
    let report = create_repository_backup(data.path(), &repo, plain_options(false, false)).unwrap();
    let manifest = manifest_for(&repo, &report.snapshot_id, false);
    let entry = &manifest.entries[0];
    let blob = blob_path(&repo, &entry.sha256).unwrap();
    fs::remove_file(&blob).unwrap();
    let listed = list_repository_snapshots(&repo, None, false).unwrap();
    assert!(!listed.validation.valid);
    assert_eq!(listed.validation.blobs_missing, 1);
    assert!(
        listed
            .validation
            .issues
            .iter()
            .any(|issue| issue.contains("missing"))
    );

    // An orphan blob whose name is valid is reported as unreferenced and does
    // not affect the validity of snapshots.
    let orphan_hash = "c".repeat(64);
    let orphan = blob_path(&repo, &orphan_hash).unwrap();
    fs::write(&orphan, vec![0u8; 100]).unwrap();
    let listed = list_repository_snapshots(&repo, None, false).unwrap();
    assert_eq!(listed.unreferenced_blobs, 1);
    assert_eq!(listed.unreferenced_blob_bytes, 100);
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn failed_editorial_hash_is_rejected_for_manifest_and_blob() {
    // Guard the common test-helper mistake of an uppercase or short digest.
    assert!(!format::is_sha256_hex(&"A".repeat(64)));
    assert!(!format::is_sha256_hex(&"a".repeat(63)));
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let repo = output.path().join("repository");
    let report = create_repository_backup(data.path(), &repo, plain_options(false, false)).unwrap();
    let manifest = manifest_for(&repo, &report.snapshot_id, false);
    assert!(
        manifest
            .entries
            .iter()
            .all(|entry| format::is_sha256_hex(&entry.sha256))
    );
    assert_eq!(sha256_hex(b"").len(), 64);
}

#[test]
fn compression_mode_is_recorded_per_entry_and_reused_blobs_keep_their_encoding() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let repo = output.path().join("repository");
    let first = create_repository_backup(
        data.path(),
        &repo,
        RepositoryBackupOptions {
            incremental: false,
            include_artifacts: false,
            compression: Compression::None,
            encryption: RepositoryEncryption::Plaintext,
            kdf: fast_kdf(),
            signing: None,
        },
    )
    .unwrap();
    let manifest = manifest_for(&repo, &first.snapshot_id, false);
    assert!(
        manifest
            .entries
            .iter()
            .all(|entry| entry.compression == Compression::None)
    );
    let second = create_repository_backup(
        data.path(),
        &repo,
        RepositoryBackupOptions {
            incremental: true,
            include_artifacts: false,
            compression: Compression::Zstd,
            encryption: RepositoryEncryption::Plaintext,
            kdf: fast_kdf(),
            signing: None,
        },
    )
    .unwrap();
    assert_eq!(second.blobs_written, 0);
    let manifest = manifest_for(&repo, &second.snapshot_id, false);
    assert!(
        manifest
            .entries
            .iter()
            .all(|entry| entry.compression == Compression::None)
    );
}

#[test]
fn repository_sizes_and_validation_cross_check() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let repo = output.path().join("repository");
    let report = create(data.path(), &repo, true, false);
    let listed = list(&repo, true, true);
    let manifest = manifest_for(&repo, &report.snapshot_id, true);
    let expected_logical: u64 = manifest.entries.iter().map(|entry| entry.size).sum();
    assert_eq!(listed.logical_bytes, expected_logical);
    assert_eq!(listed.unique_plaintext_bytes, expected_logical);
    assert_eq!(listed.unique_blobs, manifest.entries.len());
    assert!(listed.stored_blob_bytes > 0);
    let on_disk_manifest =
        fs::metadata(snapshot_manifest_path(&repo, &report.snapshot_id).unwrap())
            .unwrap()
            .len();
    assert_eq!(report.manifest_bytes, on_disk_manifest);
    assert_eq!(listed.snapshots[0].manifest_bytes, on_disk_manifest);
}

#[test]
fn interrupted_prune_tombstones_are_ignored_and_cleaned_up() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let repo = output.path().join("repository");
    let first = create(data.path(), &repo, true, false);
    let second = create(data.path(), &repo, true, true);
    let first_path = snapshot_manifest_path(&repo, &first.snapshot_id).unwrap();
    assert!(first_path.is_file());

    // Simulate a crash after the authenticated tombstone commit but before the
    // old manifest file was deleted.
    let bytes = read_metadata_bytes(&repo).unwrap();
    let mut metadata = parse_metadata(&bytes).unwrap();
    let crypto = unlock_repository(&metadata, Some(&passphrase()))
        .unwrap()
        .unwrap();
    metadata.pruned_snapshots.push(PrunedSnapshot {
        id: first.snapshot_id.clone(),
        sequence: first.sequence,
        created_at: first.created_at.clone(),
        pruned_at: now_rfc3339(),
        manifest_sha256: sha256_hex(&fs::read(&first_path).unwrap()),
    });
    metadata.updated_at = now_rfc3339();
    sign_metadata(&mut metadata, Some(&crypto)).unwrap();
    write_metadata_atomic(&repo, &metadata).unwrap();

    // The logical repository has one snapshot even though the tombstoned file
    // is still on disk.
    let listed = list(&repo, true, true);
    assert_eq!(listed.snapshot_count, 1);
    assert_eq!(listed.snapshots[0].snapshot_id, second.snapshot_id);
    assert!(first_path.is_file());

    let cleaned = prune_repository(&repo, 1, false, Some(&passphrase())).unwrap();
    assert!(cleaned.applied);
    assert!(cleaned.snapshots_pruned.is_empty());
    assert!(!first_path.exists());
    assert_eq!(list(&repo, true, true).snapshot_count, 1);
}

#[cfg(unix)]
#[test]
fn symlinked_repository_layout_logs_and_lock_are_rejected() {
    use std::os::unix::fs::symlink;
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);

    let repo = output.path().join("blobs-repository");
    create_repository_backup(data.path(), &repo, plain_options(false, false)).unwrap();
    let blobs = repo.join(BLOBS_DIR);
    let outside_blobs = output.path().join("outside-blobs");
    fs::rename(&blobs, &outside_blobs).unwrap();
    symlink(&outside_blobs, &blobs).unwrap();
    let before = all_files(&outside_blobs).len();
    let error =
        create_repository_backup(data.path(), &repo, plain_options(true, false)).unwrap_err();
    assert!(
        format!("{error:#}").contains("symlinked"),
        "unexpected error: {error:#}"
    );
    assert_eq!(all_files(&outside_blobs).len(), before);
    fs::remove_file(&blobs).unwrap();
    fs::rename(&outside_blobs, &blobs).unwrap();

    let repo = output.path().join("nonce-repository");
    create_repository_backup(data.path(), &repo, plain_options(false, false)).unwrap();
    let nonces = repo.join(NONCES_FILENAME);
    let outside_nonces = output.path().join("outside-nonces.jsonl");
    fs::write(&outside_nonces, b"").unwrap();
    fs::rename(&nonces, output.path().join("old-nonces.jsonl")).unwrap();
    symlink(&outside_nonces, &nonces).unwrap();
    let error = list_repository_snapshots(&repo, None, false).unwrap_err();
    assert!(
        format!("{error:#}").contains("symlinked"),
        "unexpected error: {error:#}"
    );
    assert_eq!(fs::read(&outside_nonces).unwrap(), b"");

    let repo = output.path().join("lock-repository");
    create_repository_backup(data.path(), &repo, plain_options(false, false)).unwrap();
    let lock = repo.join(LOCK_FILENAME);
    let outside_lock = output.path().join("outside.lock");
    fs::write(&outside_lock, b"").unwrap();
    fs::remove_file(&lock).unwrap();
    symlink(&outside_lock, &lock).unwrap();
    let error = list_repository_snapshots(&repo, None, false).unwrap_err();
    assert!(
        format!("{error:#}").contains("symlinked"),
        "unexpected error: {error:#}"
    );
}

#[test]
fn list_verify_reports_a_tampered_blob_without_claiming_validity() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let repo = output.path().join("repository");
    let report = create(data.path(), &repo, true, false);
    let manifest = manifest_for(&repo, &report.snapshot_id, true);
    let entry = &manifest.entries[0];
    let blob = blob_path(&repo, &entry.sha256).unwrap();
    let mut bytes = fs::read(&blob).unwrap();
    let index = BLOB_HEADER_BYTES + 10;
    bytes[index] ^= 0xff;
    fs::write(&blob, bytes).unwrap();
    let listed = list_repository_snapshots(&repo, Some(&passphrase()), true).unwrap();
    assert!(!listed.validation.valid);
    assert_eq!(listed.validation.blobs_corrupt, 1);
    assert_eq!(listed.validation.blobs_verified, listed.unique_blobs - 1);
    assert!(
        listed
            .validation
            .issues
            .iter()
            .any(|issue| issue.contains("failed verification"))
    );
}

#[test]
fn stale_staging_from_an_interrupted_backup_is_removed_before_the_next_write() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, _) = seed_data_dir(data.path());
    drop(store);
    let repo = output.path().join("repository");
    create(data.path(), &repo, true, false);
    let staging = repo.join(TEMP_DIR).join(".verlinpark-snapshot-crash.tmp");
    fs::create_dir_all(&staging).unwrap();
    fs::write(staging.join("plaintext.txt"), b"stale plaintext").unwrap();
    let report = create(data.path(), &repo, true, true);
    assert_eq!(report.sequence, 2);
    assert!(!staging.exists());
    for path in all_files(&repo) {
        assert!(
            !contains_subslice(&fs::read(&path).unwrap(), b"stale plaintext"),
            "stale plaintext survived in {}",
            path.display()
        );
    }
}

#[test]
fn repository_backups_preserve_the_v1_allowlist_and_exclusions() {
    let data = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let (store, session_id) = seed_data_dir(data.path());
    drop(store);
    fs::create_dir_all(data.path().join("run")).unwrap();
    fs::create_dir_all(data.path().join("logs")).unwrap();
    fs::create_dir_all(data.path().join("sessions").join(&session_id)).unwrap();
    fs::write(
        data.path().join("run/daemon.json"),
        br#"{"token":"secret-discovery-token"}"#,
    )
    .unwrap();
    fs::write(data.path().join("run/daemon.lock"), b"").unwrap();
    fs::write(data.path().join("logs/daemon.log"), b"log").unwrap();
    fs::write(data.path().join(".env"), b"OPENAI_API_KEY=secret").unwrap();
    fs::write(data.path().join("provider-keys.json"), b"secret").unwrap();
    fs::write(
        data.path()
            .join("sessions")
            .join(&session_id)
            .join("scratch.txt"),
        b"scratch",
    )
    .unwrap();

    let repo = output.path().join("repository");
    let report = create_repository_backup(data.path(), &repo, plain_options(false, false)).unwrap();
    let manifest = manifest_for(&repo, &report.snapshot_id, false);
    let paths: Vec<&str> = manifest
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(
        paths,
        vec!["sessions/session-1/events.jsonl", "verlinpark.db",]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    );
    for forbidden in [
        "run/daemon.json",
        "run/daemon.lock",
        "logs/daemon.log",
        ".env",
        "provider-keys.json",
        "sessions/session-1/scratch.txt",
    ] {
        assert!(!paths.contains(&forbidden), "{forbidden}");
    }
    for path in all_files(&repo) {
        let bytes = fs::read(&path).unwrap();
        assert!(!contains_subslice(&bytes, b"secret-discovery-token"));
        assert!(!contains_subslice(&bytes, b"OPENAI_API_KEY"));
    }
}
