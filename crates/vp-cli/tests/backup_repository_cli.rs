use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;
use vp_store::Store;

const PASSPHRASE_ENV: &str = "VERLINPARK_TEST_BACKUP_PASSPHRASE";
const PASSPHRASE: &str = "cli-correct horse battery staple";

fn vp() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vp"))
}

fn seed_data_dir(dir: &Path) {
    let store = Store::open(dir).unwrap();
    drop(store);
    fs::write(
        dir.join("projects.jsonl"),
        b"{\"id\":\"project-1\",\"name\":\"CLI backup\"}\n",
    )
    .unwrap();
}

fn run(args: &[&str]) -> Output {
    vp().args(args).output().unwrap()
}

fn run_with_env(args: &[&str], key: &str, value: &str) -> Output {
    vp().args(args).env(key, value).output().unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

fn stdout_json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout was not JSON ({error}):\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn repository_backup<'a>(data: &'a Path, repo: &'a Path, passphrase: bool) -> Vec<&'a str> {
    let mut args = vec![
        "--json",
        "--data-dir",
        path(data),
        "backup",
        "--repository",
        path(repo),
    ];
    if passphrase {
        args.extend(["--passphrase-env", PASSPHRASE_ENV]);
    } else {
        args.push("--plaintext");
    }
    args
}

#[test]
fn help_exposes_repository_commands_and_no_passphrase_argument() {
    let output = run(&["backup", "--help"]);
    assert_success(&output);
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("--repository"), "{help}");
    assert!(help.contains("--incremental"), "{help}");
    assert!(help.contains("--passphrase-env"), "{help}");
    assert!(!help.contains("--passphrase <"), "{help}");

    let output = run(&["restore", "--help"]);
    assert_success(&output);
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("--snapshot"), "{help}");
    assert!(help.contains("--repository"), "{help}");

    assert_success(&run(&["backup-list", "--help"]));
    assert_success(&run(&["backup-prune", "--help"]));
    assert_success(&run(&["backup-verify", "--help"]));
    assert_success(&run(&["backup-key-generate", "--help"]));
    assert_success(&run(&["backup-key-list", "--help"]));
    assert_success(&run(&["backup-key-rotate", "--help"]));
    assert_success(&run(&["backup-key-export-public", "--help"]));
    assert_success(&run(&["backup-keyring", "--help"]));
    assert_success(&run(&["backup-anchor", "--help"]));
    assert_success(&run(&["backup-key-revoke", "--help"]));
    assert_success(&run(&["backup-rekey", "--help"]));
    assert_success(&run(&["backup-anchor-export", "--help"]));
    assert_success(&run(&["backup-anchor-import", "--help"]));
    assert_success(&run(&["backup-anchor-verify", "--help"]));
    assert_success(&run(&["backup-key-events", "--help"]));
    let help = String::from_utf8_lossy(&run(&["backup-verify", "--help"]).stdout).to_string();
    assert!(help.contains("--allow-revoked"), "{help}");
}

#[test]
fn cli_plaintext_repository_round_trip_and_v1_compatibility() {
    let data = TempDir::new().unwrap();
    let output_dir = TempDir::new().unwrap();
    seed_data_dir(data.path());
    let repo = output_dir.path().join("repository");
    let backup = run(&repository_backup(data.path(), &repo, false));
    assert_success(&backup);
    let value = stdout_json(&backup);
    let snapshot = value["snapshot_id"].as_str().unwrap().to_string();
    assert_eq!(value["sequence"].as_u64(), Some(1));
    assert_eq!(value["encrypted"].as_bool(), Some(false));
    assert!(repo.join("repository.json").is_file());

    let listed = run(&[
        "--json",
        "backup-list",
        "--repository",
        path(&repo),
        "--verify",
    ]);
    assert_success(&listed);
    let value = stdout_json(&listed);
    assert_eq!(value["snapshot_count"].as_u64(), Some(1));
    assert_eq!(value["validation"]["valid"].as_bool(), Some(true));

    let target = output_dir.path().join("restored");
    let restored = run(&[
        "--json",
        "restore",
        "--repository",
        path(&repo),
        "--snapshot",
        &snapshot,
        "--data-dir",
        path(&target),
    ]);
    assert_success(&restored);
    assert!(target.join("verlinpark.db").is_file());

    // V1 ZIP commands keep working unchanged.
    let zip = output_dir.path().join("backup.zip");
    let v1 = run(&[
        "--json",
        "--data-dir",
        path(data.path()),
        "backup",
        "--output",
        path(&zip),
    ]);
    assert_success(&v1);
    assert_eq!(stdout_json(&v1)["format_version"].as_u64(), Some(1));
    let v1_target = output_dir.path().join("v1-restored");
    let v1_restore = run(&[
        "--json",
        "restore",
        "--input",
        path(&zip),
        "--data-dir",
        path(&v1_target),
    ]);
    assert_success(&v1_restore);
    assert!(v1_target.join("verlinpark.db").is_file());
}

#[test]
fn encrypted_cli_round_trip_uses_environment_and_file_passphrases() {
    let data = TempDir::new().unwrap();
    let output_dir = TempDir::new().unwrap();
    seed_data_dir(data.path());
    let repo = output_dir.path().join("repository");
    let backup = run_with_env(
        &repository_backup(data.path(), &repo, true),
        PASSPHRASE_ENV,
        PASSPHRASE,
    );
    assert_success(&backup);
    let snapshot = stdout_json(&backup)["snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();

    let listed = run_with_env(
        &[
            "--json",
            "backup-list",
            "--repository",
            path(&repo),
            "--passphrase-env",
            PASSPHRASE_ENV,
            "--verify",
        ],
        PASSPHRASE_ENV,
        PASSPHRASE,
    );
    assert_success(&listed);
    assert_eq!(stdout_json(&listed)["encrypted"].as_bool(), Some(true));

    // A wrong passphrase exits nonzero without echoing the supplied secret.
    let target = output_dir.path().join("wrong-target");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("keep.txt"), b"keep").unwrap();
    let wrong = run_with_env(
        &[
            "restore",
            "--repository",
            path(&repo),
            "--snapshot",
            &snapshot,
            "--data-dir",
            path(&target),
            "--force",
            "--passphrase-env",
            PASSPHRASE_ENV,
        ],
        PASSPHRASE_ENV,
        "definitely-not-the-passphrase",
    );
    assert_eq!(wrong.status.code(), Some(1));
    let error = stderr(&wrong);
    assert!(!error.contains("definitely-not-the-passphrase"), "{error}");
    assert_eq!(fs::read(target.join("keep.txt")).unwrap(), b"keep");

    // A 0600 passphrase file works; group-readable files are refused.
    let secret = output_dir.path().join("passphrase");
    fs::write(&secret, format!("{PASSPHRASE}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let repo_file = output_dir.path().join("repository-file");
    let file_backup = run(&[
        "--json",
        "--data-dir",
        path(data.path()),
        "backup",
        "--repository",
        path(&repo_file),
        "--passphrase-file",
        path(&secret),
    ]);
    assert_success(&file_backup);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o644)).unwrap();
        let refused = run(&[
            "backup",
            "--repository",
            path(&output_dir.path().join("insecure")),
            "--passphrase-file",
            path(&secret),
        ]);
        assert_eq!(refused.status.code(), Some(1));
        assert!(stderr(&refused).contains("group or others"));
    }
}

#[test]
fn cli_errors_and_exit_codes_are_explicit() {
    let data = TempDir::new().unwrap();
    let output_dir = TempDir::new().unwrap();
    seed_data_dir(data.path());

    // Missing mode is a runtime error (exit 1).
    let missing = run(&["backup", "--data-dir", path(data.path())]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("--output"));

    // Encrypted is the default; omitting both passphrase and --plaintext fails.
    let no_secret = run(&[
        "backup",
        "--data-dir",
        path(data.path()),
        "--repository",
        path(&output_dir.path().join("repo")),
    ]);
    assert_eq!(no_secret.status.code(), Some(1));
    assert!(stderr(&no_secret).contains("passphrase"));

    // Supplying the passphrase itself in argv is not a supported option and
    // is rejected by clap (exit 2).
    let argv_secret = run(&[
        "backup",
        "--repository",
        path(&output_dir.path().join("repo")),
        "--passphrase",
        "do-not-put-secrets-in-argv",
    ]);
    assert_eq!(argv_secret.status.code(), Some(2));
    assert!(stderr(&argv_secret).contains("unexpected argument"));

    // Mutually exclusive modes are rejected by clap.
    let conflict = run(&[
        "backup",
        "--repository",
        path(&output_dir.path().join("repo")),
        "--plaintext",
        "--passphrase-env",
        PASSPHRASE_ENV,
    ]);
    assert_eq!(conflict.status.code(), Some(2));

    // Incremental without a parent is a runtime error after repository init.
    let incremental = run(&[
        "backup",
        "--data-dir",
        path(data.path()),
        "--repository",
        path(&output_dir.path().join("empty-incremental")),
        "--plaintext",
        "--incremental",
    ]);
    assert_eq!(incremental.status.code(), Some(1));
    assert!(stderr(&incremental).contains("--incremental requires"));

    // Restore from a repository without --snapshot is explicit.
    let no_snapshot = run(&[
        "restore",
        "--repository",
        path(&output_dir.path().join("empty-incremental")),
        "--data-dir",
        path(&output_dir.path().join("target")),
    ]);
    assert_eq!(no_snapshot.status.code(), Some(1));
    assert!(stderr(&no_snapshot).contains("--snapshot"));
}

#[test]
fn cli_prune_dry_run_and_real_prune() {
    let data = TempDir::new().unwrap();
    let output_dir = TempDir::new().unwrap();
    seed_data_dir(data.path());
    let repo = output_dir.path().join("repository");
    assert_success(&run(&repository_backup(data.path(), &repo, false)));
    fs::write(
        data.path().join("projects.jsonl"),
        b"{\"id\":\"project-1\",\"name\":\"changed\"}\n",
    )
    .unwrap();
    let incr = run(&[
        "--json",
        "--data-dir",
        path(data.path()),
        "backup",
        "--repository",
        path(&repo),
        "--plaintext",
        "--incremental",
    ]);
    assert_success(&incr);

    let dry = run(&[
        "--json",
        "backup-prune",
        "--repository",
        path(&repo),
        "--keep",
        "1",
        "--dry-run",
    ]);
    assert_success(&dry);
    let value = stdout_json(&dry);
    assert_eq!(value["dry_run"].as_bool(), Some(true));
    assert_eq!(value["applied"].as_bool(), Some(false));
    assert_eq!(value["snapshots_pruned"].as_array().unwrap().len(), 1);
    assert_eq!(
        stdout_json(&run(&[
            "--json",
            "backup-list",
            "--repository",
            path(&repo)
        ]))["snapshot_count"]
            .as_u64(),
        Some(2)
    );

    let real = run(&[
        "--json",
        "backup-prune",
        "--repository",
        path(&repo),
        "--keep",
        "1",
    ]);
    assert_success(&real);
    assert_eq!(stdout_json(&real)["applied"].as_bool(), Some(true));
    let listed = stdout_json(&run(&[
        "--json",
        "backup-list",
        "--repository",
        path(&repo),
        "--verify",
    ]));
    assert_eq!(listed["snapshot_count"].as_u64(), Some(1));
    assert_eq!(listed["validation"]["valid"].as_bool(), Some(true));
}

#[test]
fn cli_missing_passphrase_environment_names_the_variable_without_a_value() {
    let data = TempDir::new().unwrap();
    let output_dir = TempDir::new().unwrap();
    seed_data_dir(data.path());
    let output = run(&repository_backup(
        data.path(),
        &output_dir.path().join("repo"),
        true,
    ));
    assert_eq!(output.status.code(), Some(1));
    let error = stderr(&output);
    assert!(error.contains(PASSPHRASE_ENV), "{error}");
    assert!(!error.contains(PASSPHRASE), "{error}");
}

fn generate_key(keys: &Path, key_id: &str) -> serde_json::Value {
    let output = run(&[
        "--json",
        "backup-key-generate",
        key_id,
        "--keyring-dir",
        path(keys),
    ]);
    assert_success(&output);
    stdout_json(&output)
}

fn signed_backup(data: &Path, repo: &Path, keys: &Path, key_id: &str, incremental: bool) -> Output {
    let mut args = vec![
        "--json".to_string(),
        "--data-dir".to_string(),
        path(data).to_string(),
        "backup".to_string(),
        "--repository".to_string(),
        path(repo).to_string(),
        "--passphrase-env".to_string(),
        PASSPHRASE_ENV.to_string(),
        "--signing-key".to_string(),
        key_id.to_string(),
        "--keyring-dir".to_string(),
        path(keys).to_string(),
    ];
    if incremental {
        args.push("--incremental".to_string());
    }
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    run_with_env(&borrowed, PASSPHRASE_ENV, PASSPHRASE)
}

#[test]
fn cli_signed_keyring_round_trip_and_unsigned_v2_reporting() {
    let data = TempDir::new().unwrap();
    let output_dir = TempDir::new().unwrap();
    let keys = output_dir.path().join("keys");
    seed_data_dir(data.path());

    let generated = generate_key(&keys, "primary");
    assert_eq!(generated["key_id"], "primary");
    assert_eq!(generated["active"], true);
    assert!(
        !String::from_utf8_lossy(&fs::read(keys.join("keyring.json")).unwrap())
            .contains("seed_b64")
    );

    let listed_keys = run(&["--json", "backup-key-list", "--keyring-dir", path(&keys)]);
    assert_success(&listed_keys);
    let value = stdout_json(&listed_keys);
    assert_eq!(value["key_count"].as_u64(), Some(1));
    assert_eq!(value["active_key_id"], "primary");

    let exported = run(&[
        "--json",
        "backup-key-export-public",
        "--keyring-dir",
        path(&keys),
    ]);
    assert_success(&exported);
    let exported = stdout_json(&exported);
    assert_eq!(exported["key_id"], "primary");
    assert!(exported["public_key_b64"].as_str().unwrap().len() > 20);

    let repo = output_dir.path().join("signed-repository");
    let backup = signed_backup(data.path(), &repo, &keys, "primary", false);
    assert_success(&backup);
    let snapshot = stdout_json(&backup);
    assert_eq!(snapshot["format_version"].as_u64(), Some(3));
    assert_eq!(snapshot["signed"].as_bool(), Some(true));
    assert_eq!(snapshot["signing_key_id"], "primary");
    assert_eq!(
        snapshot["anchor_update"]["highest_generation"].as_u64(),
        Some(1)
    );
    let snapshot_id = snapshot["snapshot_id"].as_str().unwrap().to_string();

    let verified = run_with_env(
        &[
            "--json",
            "backup-verify",
            "--repository",
            path(&repo),
            "--passphrase-env",
            PASSPHRASE_ENV,
            "--keyring-dir",
            path(&keys),
            "--data-dir",
            path(data.path()),
        ],
        PASSPHRASE_ENV,
        PASSPHRASE,
    );
    assert_success(&verified);
    let value = stdout_json(&verified);
    assert_eq!(value["valid"].as_bool(), Some(true));
    assert_eq!(value["authenticity"], "signed");
    assert_eq!(value["metadata_signature"]["status"], "verified");
    assert_eq!(value["generation"].as_u64(), Some(1));

    let listed = run_with_env(
        &[
            "--json",
            "backup-list",
            "--repository",
            path(&repo),
            "--passphrase-env",
            PASSPHRASE_ENV,
            "--keyring-dir",
            path(&keys),
            "--verify",
        ],
        PASSPHRASE_ENV,
        PASSPHRASE,
    );
    assert_success(&listed);
    let value = stdout_json(&listed);
    assert_eq!(value["signed"].as_bool(), Some(true));
    assert_eq!(value["validation"]["signed_snapshots"].as_u64(), Some(1));

    let target = output_dir.path().join("signed-restored");
    let restored = run_with_env(
        &[
            "--json",
            "restore",
            "--repository",
            path(&repo),
            "--snapshot",
            &snapshot_id,
            "--passphrase-env",
            PASSPHRASE_ENV,
            "--keyring-dir",
            path(&keys),
            "--data-dir",
            path(&target),
        ],
        PASSPHRASE_ENV,
        PASSPHRASE,
    );
    assert_success(&restored);
    let value = stdout_json(&restored);
    assert_eq!(value["signed"].as_bool(), Some(true));
    assert_eq!(value["signature_status"], "verified");
    assert!(target.join("verlinpark.db").is_file());

    // A v2 repository is explicitly unsigned, not silently treated as signed.
    let v2 = output_dir.path().join("v2-repository");
    let v2_backup = run(&repository_backup(data.path(), &v2, false));
    assert_success(&v2_backup);
    assert_eq!(stdout_json(&v2_backup)["format_version"].as_u64(), Some(2));
    assert_eq!(stdout_json(&v2_backup)["signed"].as_bool(), Some(false));
    let v2_verify = run(&[
        "--json",
        "backup-verify",
        "--repository",
        path(&v2),
        "--data-dir",
        path(data.path()),
    ]);
    assert_success(&v2_verify);
    let value = stdout_json(&v2_verify);
    assert_eq!(value["signed"].as_bool(), Some(false));
    assert_eq!(value["authenticity"], "unsigned");

    // Missing keyring for a signed repository is typed and actionable.
    let missing_keyring = run_with_env(
        &[
            "backup-verify",
            "--repository",
            path(&repo),
            "--passphrase-env",
            PASSPHRASE_ENV,
            "--keyring-dir",
            path(&output_dir.path().join("does-not-exist")),
        ],
        PASSPHRASE_ENV,
        PASSPHRASE,
    );
    assert_eq!(missing_keyring.status.code(), Some(1));
    assert!(stderr(&missing_keyring).contains("BACKUP_KEYRING_NOT_FOUND"));

    // A corrupt blob makes `backup-verify` exit nonzero with a typed code.
    let blob = fs::read_dir(v2.join("blobs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut bytes = fs::read(&blob).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    fs::write(&blob, bytes).unwrap();
    let corrupt = run(&[
        "--json",
        "backup-verify",
        "--repository",
        path(&v2),
        "--data-dir",
        path(data.path()),
    ]);
    assert_eq!(corrupt.status.code(), Some(1));
    assert!(
        stderr(&corrupt).contains("BACKUP_VERIFY_FAILED"),
        "{}",
        stderr(&corrupt)
    );
    let report = stdout_json(&corrupt);
    assert_eq!(report["valid"].as_bool(), Some(false));
    assert!(report["blobs_corrupt"].as_u64().unwrap() > 0);
}

#[test]
fn cli_rollback_rejection_key_rotation_and_anchor_reset() {
    let data = TempDir::new().unwrap();
    let output_dir = TempDir::new().unwrap();
    let keys = output_dir.path().join("keys");
    seed_data_dir(data.path());
    generate_key(&keys, "old");
    let repo = output_dir.path().join("repository");
    assert_success(&signed_backup(data.path(), &repo, &keys, "old", false));
    fs::write(
        data.path().join("projects.jsonl"),
        b"{\"id\":\"project-1\",\"name\":\"changed\"}\n",
    )
    .unwrap();
    assert_success(&signed_backup(data.path(), &repo, &keys, "old", true));
    let listed = run_with_env(
        &[
            "--json",
            "backup-list",
            "--repository",
            path(&repo),
            "--passphrase-env",
            PASSPHRASE_ENV,
            "--keyring-dir",
            path(&keys),
        ],
        PASSPHRASE_ENV,
        PASSPHRASE,
    );
    assert_success(&listed);
    let value = stdout_json(&listed);
    let first_snapshot = value["snapshots"][0]["snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Rotate to a newly generated key and take a third snapshot.
    let rotated = run(&[
        "--json",
        "backup-key-rotate",
        "new",
        "--keyring-dir",
        path(&keys),
    ]);
    assert_success(&rotated);
    assert_eq!(stdout_json(&rotated)["key_id"], "new");
    fs::write(
        data.path().join("projects.jsonl"),
        b"{\"id\":\"project-1\"}\n",
    )
    .unwrap();
    let third = signed_backup(data.path(), &repo, &keys, "new", true);
    assert_success(&third);
    assert_eq!(stdout_json(&third)["signing_key_id"], "new");

    let verified = run_with_env(
        &[
            "--json",
            "backup-verify",
            "--repository",
            path(&repo),
            "--passphrase-env",
            PASSPHRASE_ENV,
            "--keyring-dir",
            path(&keys),
            "--data-dir",
            path(data.path()),
        ],
        PASSPHRASE_ENV,
        PASSPHRASE,
    );
    assert_success(&verified);
    let value = stdout_json(&verified);
    assert_eq!(value["valid"].as_bool(), Some(true));
    assert_eq!(value["signed_snapshots"].as_u64(), Some(3));
    assert_eq!(value["generation"].as_u64(), Some(3));

    // Restoring the first generation is rejected by the local anchor.
    let rejected = run_with_env(
        &[
            "restore",
            "--repository",
            path(&repo),
            "--snapshot",
            &first_snapshot,
            "--passphrase-env",
            PASSPHRASE_ENV,
            "--keyring-dir",
            path(&keys),
            "--data-dir",
            path(data.path()),
            "--force",
        ],
        PASSPHRASE_ENV,
        PASSPHRASE,
    );
    assert_eq!(rejected.status.code(), Some(1));
    assert!(stderr(&rejected).contains("BACKUP_ROLLBACK_DETECTED"));

    let allowed = run_with_env(
        &[
            "--json",
            "restore",
            "--repository",
            path(&repo),
            "--snapshot",
            &first_snapshot,
            "--passphrase-env",
            PASSPHRASE_ENV,
            "--keyring-dir",
            path(&keys),
            "--data-dir",
            path(data.path()),
            "--allow-rollback",
            "--force",
        ],
        PASSPHRASE_ENV,
        PASSPHRASE,
    );
    assert_success(&allowed);
    let value = stdout_json(&allowed);
    assert_eq!(value["rollback"]["rollback"].as_bool(), Some(true));
    assert_eq!(
        value["anchor_update"]["highest_generation"].as_u64(),
        Some(3),
        "allow-rollback must not lower the high-water mark"
    );

    let anchor = run(&["--json", "backup-anchor", "--data-dir", path(data.path())]);
    assert_success(&anchor);
    let value = stdout_json(&anchor);
    assert_eq!(value["present"].as_bool(), Some(true));
    assert_eq!(
        value["repositories"][0]["last_verified_generation"].as_u64(),
        Some(1)
    );
    assert_eq!(
        value["repositories"][0]["highest_generation"].as_u64(),
        Some(3)
    );

    let reset = run(&[
        "--json",
        "backup-anchor",
        "--data-dir",
        path(data.path()),
        "--reset",
    ]);
    assert_success(&reset);
    assert_eq!(stdout_json(&reset)["reset"].as_bool(), Some(true));
    let anchor = run(&["--json", "backup-anchor", "--data-dir", path(data.path())]);
    assert_success(&anchor);
    assert_eq!(stdout_json(&anchor)["present"].as_bool(), Some(false));
}

#[test]
fn cli_revocation_rekey_and_portable_anchor_round_trip() {
    let data = TempDir::new().unwrap();
    let output_dir = TempDir::new().unwrap();
    let keys = output_dir.path().join("keys");
    seed_data_dir(data.path());
    generate_key(&keys, "old");
    let repo = output_dir.path().join("repository");
    assert_success(&signed_backup(data.path(), &repo, &keys, "old", false));
    fs::write(
        data.path().join("projects.jsonl"),
        b"{\"id\":\"project-1\",\"name\":\"rekey\"}\n",
    )
    .unwrap();
    assert_success(&signed_backup(data.path(), &repo, &keys, "old", true));

    let rotated = run(&[
        "--json",
        "backup-key-rotate",
        "new",
        "--keyring-dir",
        path(&keys),
    ]);
    assert_success(&rotated);
    let revoked = run(&[
        "--json",
        "backup-key-revoke",
        "old",
        "--keyring-dir",
        path(&keys),
    ]);
    assert_success(&revoked);
    let value = stdout_json(&revoked);
    assert_eq!(value["key_id"], "old");
    assert_eq!(value["changed"].as_bool(), Some(true));
    assert_eq!(value["was_active"].as_bool(), Some(false));

    let events = run(&[
        "--json",
        "backup-key-events",
        "--keyring-dir",
        path(&keys),
        "--limit",
        "10",
    ]);
    assert_success(&events);
    let value = stdout_json(&events);
    assert!(value["event_count"].as_u64().unwrap() >= 3);
    assert_eq!(value["events"][0]["event"].as_str(), Some("key.revoked"));
    let rendered = serde_json::to_string(&value).unwrap();
    assert!(!rendered.contains("seed_b64"));
    assert!(!rendered.contains("private"));

    let listed_keys = run(&["--json", "backup-key-list", "--keyring-dir", path(&keys)]);
    assert_success(&listed_keys);
    let value = stdout_json(&listed_keys);
    assert_eq!(value["revoked_key_count"].as_u64(), Some(1));
    let old = value["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|key| key["key_id"] == "old")
        .unwrap();
    assert!(old["revoked_at"].is_string());

    let refused = run_with_env(
        &[
            "backup-list",
            "--repository",
            path(&repo),
            "--passphrase-env",
            PASSPHRASE_ENV,
            "--keyring-dir",
            path(&keys),
        ],
        PASSPHRASE_ENV,
        PASSPHRASE,
    );
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        stderr(&refused).contains("BACKUP_KEY_REVOKED"),
        "{}",
        stderr(&refused)
    );

    let allowed = run_with_env(
        &[
            "--json",
            "backup-list",
            "--repository",
            path(&repo),
            "--passphrase-env",
            PASSPHRASE_ENV,
            "--keyring-dir",
            path(&keys),
            "--allow-revoked",
        ],
        PASSPHRASE_ENV,
        PASSPHRASE,
    );
    assert_success(&allowed);
    let value = stdout_json(&allowed);
    assert_eq!(value["validation"]["revoked_signatures"].as_u64(), Some(3));

    let dry_run = run_with_env(
        &[
            "--json",
            "backup-rekey",
            "--repository",
            path(&repo),
            "--passphrase-env",
            PASSPHRASE_ENV,
            "--keyring-dir",
            path(&keys),
            "--dry-run",
        ],
        PASSPHRASE_ENV,
        PASSPHRASE,
    );
    assert_success(&dry_run);
    let value = stdout_json(&dry_run);
    assert_eq!(value["dry_run"].as_bool(), Some(true));
    assert_eq!(value["applied"].as_bool(), Some(false));
    assert_eq!(value["snapshots_resigned"].as_u64(), Some(2));

    let rekey = run_with_env(
        &[
            "--json",
            "backup-rekey",
            "--repository",
            path(&repo),
            "--passphrase-env",
            PASSPHRASE_ENV,
            "--keyring-dir",
            path(&keys),
        ],
        PASSPHRASE_ENV,
        PASSPHRASE,
    );
    assert_success(&rekey);
    let value = stdout_json(&rekey);
    assert_eq!(value["applied"].as_bool(), Some(true));
    assert_eq!(value["previous_key_id"], "old");
    assert_eq!(value["key_id"], "new");
    assert_eq!(value["snapshots_resigned"].as_u64(), Some(2));
    assert_eq!(value["blob_ciphertext_unchanged"].as_bool(), Some(true));
    assert_eq!(value["parent_chain_preserved"].as_bool(), Some(true));
    assert_eq!(value["entries_preserved"].as_bool(), Some(true));

    let verified = run_with_env(
        &[
            "--json",
            "backup-verify",
            "--repository",
            path(&repo),
            "--passphrase-env",
            PASSPHRASE_ENV,
            "--keyring-dir",
            path(&keys),
            "--data-dir",
            path(data.path()),
        ],
        PASSPHRASE_ENV,
        PASSPHRASE,
    );
    assert_success(&verified);
    let value = stdout_json(&verified);
    assert_eq!(value["valid"].as_bool(), Some(true));
    assert_eq!(value["metadata_signature"]["key_id"], "new");
    assert_eq!(value["signed_snapshots"].as_u64(), Some(2));

    // Portable anchor export from the source host.
    let portable = output_dir.path().join("portable-anchor.json");
    let exported = run(&[
        "--json",
        "--data-dir",
        path(data.path()),
        "backup-anchor-export",
        "--keyring-dir",
        path(&keys),
        "--output",
        path(&portable),
    ]);
    assert_success(&exported);
    let value = stdout_json(&exported);
    assert_eq!(value["key_id"], "new");
    assert_eq!(value["repository_count"].as_u64(), Some(1));
    assert!(portable.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&portable).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    // A fresh host previews, then imports the signed document and raises its
    // anchor.
    let fresh = TempDir::new().unwrap();
    let inspected = run(&[
        "--json",
        "--data-dir",
        path(fresh.path()),
        "backup-anchor-verify",
        "--keyring-dir",
        path(&keys),
        "--input",
        path(&portable),
    ]);
    assert_success(&inspected);
    let value = stdout_json(&inspected);
    assert_eq!(value["signature"].as_str(), Some("verified"));
    assert_eq!(value["would_raise"].as_u64(), Some(1));
    assert_eq!(value["unchanged"].as_u64(), Some(0));
    let imported = run(&[
        "--json",
        "--data-dir",
        path(fresh.path()),
        "backup-anchor-import",
        "--keyring-dir",
        path(&keys),
        "--input",
        path(&portable),
    ]);
    assert_success(&imported);
    let value = stdout_json(&imported);
    assert_eq!(value["repositories_total"].as_u64(), Some(1));
    assert_eq!(value["repositories_raised"].as_u64(), Some(1));
    assert_eq!(value["raises"][0]["highest_generation"].as_u64(), Some(2));
    let anchor = run(&["--json", "backup-anchor", "--data-dir", path(fresh.path())]);
    assert_success(&anchor);
    let value = stdout_json(&anchor);
    assert_eq!(value["imported_repositories"].as_u64(), Some(1));
    assert_eq!(
        value["repositories"][0]["last_operation"].as_str(),
        Some("anchor_import")
    );
    assert_eq!(
        value["repositories"][0]["origin"].as_str(),
        Some("portable_import")
    );

    // Importing the same document again cannot lower or raise anything.
    let imported_again = run(&[
        "--json",
        "--data-dir",
        path(fresh.path()),
        "backup-anchor-import",
        "--keyring-dir",
        path(&keys),
        "--input",
        path(&portable),
    ]);
    assert_success(&imported_again);
    let value = stdout_json(&imported_again);
    assert_eq!(value["repositories_raised"].as_u64(), Some(0));
    assert_eq!(value["repositories_unchanged"].as_u64(), Some(1));
    let inspected_again = run(&[
        "--json",
        "--data-dir",
        path(fresh.path()),
        "backup-anchor-verify",
        "--keyring-dir",
        path(&keys),
        "--input",
        path(&portable),
    ]);
    assert_success(&inspected_again);
    let value = stdout_json(&inspected_again);
    assert_eq!(value["would_raise"].as_u64(), Some(0));
    assert_eq!(value["unchanged"].as_u64(), Some(1));

    // Tampering with a signed document is rejected with a typed code.
    let mut document: serde_json::Value =
        serde_json::from_slice(&fs::read(&portable).unwrap()).unwrap();
    document["repositories"][0]["highest_generation"] = serde_json::json!(99);
    let tampered = output_dir.path().join("tampered-anchor.json");
    fs::write(&tampered, serde_json::to_vec(&document).unwrap()).unwrap();
    let rejected = run(&[
        "--data-dir",
        path(fresh.path()),
        "backup-anchor-import",
        "--keyring-dir",
        path(&keys),
        "--input",
        path(&tampered),
    ]);
    assert_eq!(rejected.status.code(), Some(1));
    assert!(
        stderr(&rejected).contains("BACKUP_ANCHOR_SIGNATURE_INVALID"),
        "{}",
        stderr(&rejected)
    );
}

#[test]
fn cli_active_key_revocation_requires_force() {
    let output_dir = TempDir::new().unwrap();
    let keys = output_dir.path().join("keys");
    generate_key(&keys, "primary");

    let refused = run(&["backup-key-revoke", "primary", "--keyring-dir", path(&keys)]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        stderr(&refused).contains("BACKUP_KEY_REVOKE_ACTIVE"),
        "{}",
        stderr(&refused)
    );

    let forced = run(&[
        "--json",
        "backup-key-revoke",
        "primary",
        "--keyring-dir",
        path(&keys),
        "--force",
    ]);
    assert_success(&forced);
    let value = stdout_json(&forced);
    assert_eq!(value["was_active"].as_bool(), Some(true));
    assert!(value["active_key_id"].is_null());
    assert!(
        value["warning"]
            .as_str()
            .unwrap()
            .contains("no key can sign")
    );

    let listed = run(&["--json", "backup-key-list", "--keyring-dir", path(&keys)]);
    assert_success(&listed);
    assert!(stdout_json(&listed)["active_key_id"].is_null());

    let replacement = run(&[
        "--json",
        "backup-key-generate",
        "replacement",
        "--keyring-dir",
        path(&keys),
    ]);
    assert_success(&replacement);
    assert_eq!(stdout_json(&replacement)["active"].as_bool(), Some(true));
}
