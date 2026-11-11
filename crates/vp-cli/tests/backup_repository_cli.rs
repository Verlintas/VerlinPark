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
