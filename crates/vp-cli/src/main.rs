use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::Parser;
use fs2::FileExt;
use serde_json::{Value, json};
use std::fs::OpenOptions;
use vp_core::backup::anchor::{
    AnchorStore, PortableAnchorImportOptions, inspect_portable_anchor, load_portable_anchor,
};
use vp_core::backup::repository::{
    Compression, KdfConfig, Passphrase, RepositoryBackupOptions, RepositoryEncryption,
    RepositoryRekeyOptions, RepositoryRestoreOptions, RepositorySigningOptions,
    RepositoryVerifyOptions, rekey_repository, verify_repository,
};
use vp_core::backup::signing::{Keyring, SecretSource, default_key_dir};
use vp_store::paths;

mod cli;
mod dispatch;
mod render;
mod rpc;
#[cfg(test)]
mod tests;

use cli::{Cli, Command, DaemonAgentAction};
use render::{
    filtered_tool_codes, print_backup_anchor, print_backup_key_events, print_backup_key_generation,
    print_backup_key_revocation, print_backup_key_rotation, print_backup_keyring, print_human,
    print_json, print_portable_anchor_export, print_portable_anchor_import,
    print_portable_anchor_verify, print_repository_backup, print_repository_list,
    print_repository_prune, print_repository_rekey, print_repository_restore,
    print_repository_verify, tool_activity_csv,
};
use rpc::{confirm_cli_action, read_discovery};

/// Accept both `vp cache warm <session>` and `vp cache-warm <session>`.
/// The status command keeps its historical `vp cache <session>` shape, so the
/// two-word warm spelling is normalized before clap parses positional args.
pub(crate) fn normalize_cache_warm_args(mut args: Vec<String>) -> Vec<String> {
    if args.get(1).map(String::as_str) == Some("cache")
        && args.get(2).map(String::as_str) == Some("warm")
    {
        args.remove(2);
        args[1] = "cache-warm".to_string();
    }
    args
}

/// The generated clap parser and the command dispatch future have both grown
/// with the CLI surface. Run them on a dedicated, generous stack instead of the
/// platform's default main-thread stack; the tokio workers get the same budget.
const CLI_STACK_BYTES: usize = 32 * 1024 * 1024;

fn main() -> Result<()> {
    let worker = std::thread::Builder::new()
        .name("vp-cli".into())
        .stack_size(CLI_STACK_BYTES)
        .spawn(|| -> Result<()> {
            let cli = Cli::parse_from(normalize_cache_warm_args(std::env::args().collect()));
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_stack_size(CLI_STACK_BYTES)
                .build()?
                .block_on(run(cli))
        })?;
    match worker.join() {
        Ok(result) => result,
        Err(_) => anyhow::bail!("vp CLI thread panicked"),
    }
}

async fn run(cli: Cli) -> Result<()> {
    if let Command::Bench { command } = &cli.command {
        match vp_bench::cli::execute(command.clone(), cli.json).await {
            Ok(outcome) => {
                if cli.json {
                    if let Some(value) = outcome.json {
                        print_json(&value)?;
                    }
                } else {
                    print!("{}", outcome.human);
                }
                std::process::exit(outcome.exit_code);
            }
            Err(error) => {
                eprintln!("vp bench: {error:#}");
                std::process::exit(2);
            }
        };
    }
    if let Command::CacheBench { command, run } = &cli.command {
        let command = command
            .clone()
            .unwrap_or(vp_bench::cache_bench::CacheBenchCommand::Run(run.clone()));
        match vp_bench::cache_bench::execute(command, cli.json).await {
            Ok(code) => std::process::exit(code),
            Err(error) => {
                eprintln!("vp cache-bench: {error:#}");
                std::process::exit(2);
            }
        }
    }
    if let Command::DbRebuild { dry_run, force } = &cli.command {
        let data_dir = cli.data_dir.clone().unwrap_or_else(paths::data_dir);
        if data_dir.join("run/daemon.json").exists() && !force {
            bail!(
                "daemon discovery file exists at {}; stop the daemon or pass --force",
                data_dir.join("run/daemon.json").display()
            );
        }
        std::fs::create_dir_all(data_dir.join("run"))?;
        let lock_path = data_dir.join("run/daemon.lock");
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        if lock_file.try_lock_exclusive().is_err() {
            bail!(
                "daemon lock is held at {}; stop the daemon before rebuilding",
                lock_path.display()
            );
        }
        let report = vp_core::rebuild_database(&data_dir, *dry_run).await?;
        let _ = lock_file.unlock();
        let value = serde_json::to_value(report)?;
        if cli.json {
            print_json(&value)?;
        } else {
            print_human(&value);
        }
        return Ok(());
    }
    if let Command::Backup {
        output,
        repository,
        include_artifacts,
        incremental,
        plaintext,
        passphrase_env,
        passphrase_file,
        no_compress,
        signing_key,
        sign,
        keyring_dir,
    } = &cli.command
    {
        let data_dir = cli.data_dir.clone().unwrap_or_else(paths::data_dir);
        match (output, repository) {
            (Some(output), None) => {
                if *incremental
                    || *plaintext
                    || passphrase_env.is_some()
                    || passphrase_file.is_some()
                    || *no_compress
                    || signing_key.is_some()
                    || *sign
                    || keyring_dir.is_some()
                {
                    bail!(
                        "--incremental, --plaintext, --passphrase-env, --passphrase-file, --no-compress, --signing-key, --sign, and --keyring-dir are repository-only options; use --repository"
                    );
                }
                let report = vp_core::backup::create_backup(&data_dir, output, *include_artifacts)?;
                let value = serde_json::to_value(report)?;
                if cli.json {
                    print_json(&value)?;
                } else {
                    print_human(&value);
                }
            }
            (None, Some(repository)) => {
                let passphrase =
                    resolve_passphrase(passphrase_env.as_deref(), passphrase_file.as_deref())?;
                let encryption = match (plaintext, passphrase) {
                    (true, Some(_)) => {
                        bail!("--plaintext conflicts with a repository passphrase")
                    }
                    (true, None) => RepositoryEncryption::Plaintext,
                    (false, Some(passphrase)) => RepositoryEncryption::Encrypted(passphrase),
                    (false, None) => bail!(
                        "repository backups are encrypted by default; pass --passphrase-env NAME or --passphrase-file PATH, or --plaintext to write an unencrypted repository"
                    ),
                };
                let options = RepositoryBackupOptions {
                    incremental: *incremental,
                    include_artifacts: *include_artifacts,
                    compression: if *no_compress {
                        Compression::None
                    } else {
                        Compression::Zstd
                    },
                    encryption,
                    kdf: KdfConfig::default(),
                    signing: resolve_signing_options(
                        keyring_dir.as_deref(),
                        signing_key.as_deref(),
                        *sign,
                    )?,
                };
                let report = vp_core::backup::repository::create_repository_backup(
                    &data_dir, repository, options,
                )?;
                let value = serde_json::to_value(report)?;
                if cli.json {
                    print_json(&value)?;
                } else {
                    print_repository_backup(&value);
                }
            }
            (Some(_), Some(_)) => {
                bail!("pass exactly one of --output or --repository")
            }
            (None, None) => {
                bail!(
                    "pass --output <zip> for a v1 backup or --repository <dir> for a v2 repository"
                )
            }
        }
        return Ok(());
    }
    if let Command::Restore {
        input,
        repository,
        snapshot,
        force,
        passphrase_env,
        passphrase_file,
        keyring_dir,
        allow_rollback,
        allow_revoked,
        anchor_file,
    } = &cli.command
    {
        let data_dir = cli.data_dir.clone().unwrap_or_else(paths::data_dir);
        match (input, repository) {
            (Some(input), None) => {
                if snapshot.is_some()
                    || passphrase_env.is_some()
                    || passphrase_file.is_some()
                    || keyring_dir.is_some()
                    || *allow_rollback
                    || *allow_revoked
                    || anchor_file.is_some()
                {
                    bail!(
                        "--snapshot, --passphrase-env, --passphrase-file, --keyring-dir, --allow-rollback, --allow-revoked, and --anchor-file are repository-only options; use --repository"
                    );
                }
                let report = vp_core::backup::restore_backup(input, &data_dir, *force)?;
                let value = serde_json::to_value(report)?;
                if cli.json {
                    print_json(&value)?;
                } else {
                    print_human(&value);
                }
            }
            (None, Some(repository)) => {
                let snapshot = snapshot.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("--repository restore requires --snapshot <id>")
                })?;
                let passphrase =
                    resolve_passphrase(passphrase_env.as_deref(), passphrase_file.as_deref())?;
                let keyring = load_keyring_optional(keyring_dir.as_deref())?;
                let anchor = anchor_store(&data_dir, anchor_file.as_deref());
                let report = vp_core::backup::repository::restore_repository_snapshot_checked(
                    repository,
                    snapshot,
                    &data_dir,
                    *force,
                    RepositoryRestoreOptions {
                        passphrase: passphrase.as_ref(),
                        keyring: keyring.as_ref(),
                        anchor: Some(&anchor),
                        allow_rollback: *allow_rollback,
                        allow_revoked: *allow_revoked,
                    },
                )?;
                let value = serde_json::to_value(report)?;
                if cli.json {
                    print_json(&value)?;
                } else {
                    print_repository_restore(&value);
                }
            }
            (Some(_), Some(_)) => bail!("pass exactly one of --input or --repository"),
            (None, None) => bail!(
                "pass --input <zip> for a v1 restore or --repository <dir> --snapshot <id> for a repository restore"
            ),
        }
        return Ok(());
    }
    if let Command::BackupList {
        repository,
        verify,
        passphrase_env,
        passphrase_file,
        keyring_dir,
        allow_revoked,
    } = &cli.command
    {
        let passphrase = resolve_passphrase(passphrase_env.as_deref(), passphrase_file.as_deref())?;
        let keyring = load_keyring_optional(keyring_dir.as_deref())?;
        let report = vp_core::backup::repository::list_repository_snapshots_with_policy(
            repository,
            passphrase.as_ref(),
            keyring.as_ref(),
            *verify,
            *allow_revoked,
        )?;
        let value = serde_json::to_value(report)?;
        if cli.json {
            print_json(&value)?;
        } else {
            print_repository_list(&value);
        }
        return Ok(());
    }
    if let Command::BackupPrune {
        repository,
        keep,
        dry_run,
        passphrase_env,
        passphrase_file,
        keyring_dir,
        signing_key,
        sign,
    } = &cli.command
    {
        let passphrase = resolve_passphrase(passphrase_env.as_deref(), passphrase_file.as_deref())?;
        let signing =
            resolve_signing_options(keyring_dir.as_deref(), signing_key.as_deref(), *sign)?;
        let fallback_keyring;
        let keyring = match signing.as_ref() {
            Some(signing) => Some(&signing.keyring),
            None => {
                fallback_keyring = load_keyring_optional(None)?;
                fallback_keyring.as_ref()
            }
        };
        let credential = match signing.as_ref() {
            Some(signing) => Some(
                signing
                    .keyring
                    .signing_credential(signing.key_id.as_deref())?,
            ),
            None => None,
        };
        let report = vp_core::backup::repository::prune_repository_with_signing(
            repository,
            *keep,
            *dry_run,
            passphrase.as_ref(),
            keyring,
            credential.as_ref(),
        )?;
        let value = serde_json::to_value(report)?;
        if cli.json {
            print_json(&value)?;
        } else {
            print_repository_prune(&value);
        }
        return Ok(());
    }
    if let Command::BackupVerify {
        repository,
        passphrase_env,
        passphrase_file,
        keyring_dir,
        allow_rollback,
        allow_revoked,
        anchor_file,
        presence_only,
    } = &cli.command
    {
        let data_dir = cli.data_dir.clone().unwrap_or_else(paths::data_dir);
        let passphrase = resolve_passphrase(passphrase_env.as_deref(), passphrase_file.as_deref())?;
        let keyring = load_keyring_optional(keyring_dir.as_deref())?;
        let anchor = anchor_store(&data_dir, anchor_file.as_deref());
        let report = verify_repository(
            repository,
            RepositoryVerifyOptions {
                passphrase: passphrase.as_ref(),
                keyring: keyring.as_ref(),
                anchor: Some(&anchor),
                allow_rollback: *allow_rollback,
                allow_revoked: *allow_revoked,
                verify_blobs: !*presence_only,
            },
        )?;
        let value = serde_json::to_value(report)?;
        let valid = value["valid"].as_bool().unwrap_or(false);
        if cli.json {
            print_json(&value)?;
        } else {
            print_repository_verify(&value);
        }
        if !valid {
            return Err(vp_core::backup::error::BackupSecurityError::VerificationFailed.into());
        }
        return Ok(());
    }
    if let Command::BackupKeyGenerate {
        key_id,
        keyring_dir,
        inactive,
        from_env,
    } = &cli.command
    {
        let dir = keyring_dir.clone().unwrap_or_else(default_key_dir);
        let report = match from_env {
            Some(variable) => {
                Keyring::register_environment_key(&dir, key_id, variable, !*inactive)?
            }
            None => Keyring::generate_key(
                &dir,
                key_id,
                !*inactive,
                SecretSource::File {
                    name: format!("{key_id}.key"),
                },
            )?,
        };
        let value = serde_json::to_value(report)?;
        if cli.json {
            print_json(&value)?;
        } else {
            print_backup_key_generation(&value);
        }
        return Ok(());
    }
    if let Command::BackupKeyList { keyring_dir } = &cli.command {
        let summary =
            load_keyring_optional(keyring_dir.as_deref())?.map(|keyring| keyring.summary());
        let value = match summary {
            Some(summary) => serde_json::to_value(summary)?,
            None => json!({
                "path": keyring_dir.clone().unwrap_or_else(default_key_dir).join("keyring.json").display().to_string(),
                "active_key_id": null,
                "key_count": 0,
                "keys": [],
                "present": false,
            }),
        };
        if cli.json {
            print_json(&value)?;
        } else {
            print_backup_keyring(&value);
        }
        return Ok(());
    }
    if let Command::BackupKeyEvents { keyring_dir, limit } = &cli.command {
        let dir = keyring_dir.clone().unwrap_or_else(default_key_dir);
        let report = match load_keyring_optional(Some(&dir))? {
            Some(keyring) => keyring.audit_events(*limit)?,
            None => vp_core::backup::signing::KeyringAuditReport {
                path: dir.join("keyring-events.jsonl").display().to_string(),
                present: false,
                event_count: 0,
                truncated: false,
                events: Vec::new(),
            },
        };
        let value = serde_json::to_value(report)?;
        if cli.json {
            print_json(&value)?;
        } else {
            print_backup_key_events(&value);
        }
        return Ok(());
    }
    if let Command::BackupKeyRotate {
        key_id,
        keyring_dir,
        use_existing,
        from_env,
    } = &cli.command
    {
        let dir = keyring_dir.clone().unwrap_or_else(default_key_dir);
        let report = if let Some(variable) = from_env {
            Keyring::register_environment_key(&dir, key_id, variable, false)?;
            let mut keyring = Keyring::load(&dir)?;
            keyring.rotate_to(key_id)?
        } else if *use_existing {
            let mut keyring = Keyring::load(&dir)?;
            keyring.rotate_to(key_id)?
        } else {
            Keyring::generate_key(
                &dir,
                key_id,
                false,
                SecretSource::File {
                    name: format!("{key_id}.key"),
                },
            )?;
            let mut keyring = Keyring::load(&dir)?;
            keyring.rotate_to(key_id)?
        };
        let value = serde_json::to_value(report)?;
        if cli.json {
            print_json(&value)?;
        } else {
            print_backup_key_rotation(&value);
        }
        return Ok(());
    }
    if let Command::BackupKeyRevoke {
        key_id,
        keyring_dir,
        force,
    } = &cli.command
    {
        let dir = keyring_dir.clone().unwrap_or_else(default_key_dir);
        let mut keyring = Keyring::load(&dir)?;
        let report = keyring.revoke_key(key_id, *force)?;
        let value = serde_json::to_value(report)?;
        if cli.json {
            print_json(&value)?;
        } else {
            print_backup_key_revocation(&value);
        }
        return Ok(());
    }
    if let Command::BackupRekey {
        repository,
        passphrase_env,
        passphrase_file,
        keyring_dir,
        dry_run,
        max_snapshots,
    } = &cli.command
    {
        let passphrase = resolve_passphrase(passphrase_env.as_deref(), passphrase_file.as_deref())?;
        let keyring = load_keyring_required(keyring_dir.as_deref())?;
        let report = rekey_repository(
            repository,
            RepositoryRekeyOptions {
                passphrase: passphrase.as_ref(),
                keyring: &keyring,
                dry_run: *dry_run,
                max_snapshots: *max_snapshots,
            },
        )?;
        let value = serde_json::to_value(report)?;
        if cli.json {
            print_json(&value)?;
        } else {
            print_repository_rekey(&value);
        }
        return Ok(());
    }
    if let Command::BackupKeyExportPublic {
        keyring_dir,
        key_id,
        output,
    } = &cli.command
    {
        let keyring = load_keyring_required(keyring_dir.as_deref())?;
        let export = keyring.export_public(key_id.as_deref())?;
        let value = serde_json::to_value(export)?;
        if let Some(output) = output {
            if output.exists() {
                bail!("public key output already exists at {}", output.display());
            }
            let bytes = serde_json::to_vec_pretty(&value)?;
            let parent = output.parent().unwrap_or_else(|| Path::new("."));
            std::fs::create_dir_all(parent)?;
            std::fs::write(output, bytes)?;
            if cli.json {
                print_json(&value)?;
            } else {
                println!(
                    "exported {} public key to {}",
                    value["key_id"].as_str().unwrap_or("?"),
                    output.display()
                );
            }
        } else if cli.json {
            print_json(&value)?;
        } else {
            println!(
                "{}  {}  fingerprint {}",
                value["key_id"].as_str().unwrap_or("?"),
                value["public_key_b64"].as_str().unwrap_or("?"),
                value["fingerprint"].as_str().unwrap_or("?")
            );
        }
        return Ok(());
    }
    if let Command::BackupKeyImport {
        key_id,
        keyring_dir,
        seed_file,
        from_env,
        inactive,
    } = &cli.command
    {
        let dir = keyring_dir.clone().unwrap_or_else(default_key_dir);
        let report = match (seed_file, from_env) {
            (Some(path), None) => Keyring::import_key_file(&dir, key_id, path, !*inactive)?,
            (None, Some(variable)) => {
                Keyring::register_environment_key(&dir, key_id, variable, !*inactive)?
            }
            _ => bail!("pass exactly one of --seed-file or --from-env"),
        };
        let value = serde_json::to_value(report)?;
        if cli.json {
            print_json(&value)?;
        } else {
            print_backup_key_generation(&value);
        }
        return Ok(());
    }
    if let Command::BackupKeyring { keyring_dir } = &cli.command {
        let summary =
            load_keyring_optional(keyring_dir.as_deref())?.map(|keyring| keyring.summary());
        let value = match summary {
            Some(summary) => serde_json::to_value(summary)?,
            None => json!({
                "path": keyring_dir.clone().unwrap_or_else(default_key_dir).join("keyring.json").display().to_string(),
                "active_key_id": null,
                "key_count": 0,
                "keys": [],
                "present": false,
            }),
        };
        if cli.json {
            print_json(&value)?;
        } else {
            print_backup_keyring(&value);
        }
        return Ok(());
    }
    if let Command::BackupAnchor { anchor_file, reset } = &cli.command {
        let data_dir = cli.data_dir.clone().unwrap_or_else(paths::data_dir);
        let anchor = anchor_store(&data_dir, anchor_file.as_deref());
        if *reset {
            let removed = anchor.reset()?;
            let value = json!({"reset": removed, "path": anchor.path().display().to_string()});
            if cli.json {
                print_json(&value)?;
            } else {
                println!(
                    "{} {}",
                    if removed { "removed" } else { "no anchor at" },
                    anchor.path().display()
                );
            }
            return Ok(());
        }
        let summary = anchor.summary()?;
        let value = serde_json::to_value(summary)?;
        if cli.json {
            print_json(&value)?;
        } else {
            print_backup_anchor(&value);
        }
        return Ok(());
    }
    if let Command::BackupAnchorExport {
        anchor_file,
        keyring_dir,
        key_id,
        output,
    } = &cli.command
    {
        let data_dir = cli.data_dir.clone().unwrap_or_else(paths::data_dir);
        let store = anchor_store(&data_dir, anchor_file.as_deref());
        let keyring = load_keyring_required(keyring_dir.as_deref())?;
        let export = store.export_portable(&keyring, key_id.as_deref(), output.as_deref())?;
        if cli.json || output.is_none() {
            if cli.json {
                print_json(&serde_json::to_value(&export)?)?;
            } else {
                println!("{}", serde_json::to_string_pretty(&export.document)?);
            }
        } else {
            print_portable_anchor_export(&serde_json::to_value(&export)?);
        }
        return Ok(());
    }
    if let Command::BackupAnchorVerify {
        anchor_file,
        keyring_dir,
        input,
        allow_revoked,
    } = &cli.command
    {
        let data_dir = cli.data_dir.clone().unwrap_or_else(paths::data_dir);
        let store = anchor_store(&data_dir, anchor_file.as_deref());
        let keyring = load_keyring_required(keyring_dir.as_deref())?;
        let document = load_portable_anchor(input)?;
        let report = inspect_portable_anchor(&document, &keyring, *allow_revoked, &store)?;
        let value = serde_json::to_value(report)?;
        if cli.json {
            print_json(&value)?;
        } else {
            print_portable_anchor_verify(&value);
        }
        return Ok(());
    }
    if let Command::BackupAnchorImport {
        anchor_file,
        keyring_dir,
        input,
        allow_revoked,
        require_newer,
    } = &cli.command
    {
        let data_dir = cli.data_dir.clone().unwrap_or_else(paths::data_dir);
        let store = anchor_store(&data_dir, anchor_file.as_deref());
        let keyring = load_keyring_required(keyring_dir.as_deref())?;
        let document = load_portable_anchor(input)?;
        let report = store.import_portable(
            &document,
            PortableAnchorImportOptions {
                keyring: &keyring,
                allow_revoked: *allow_revoked,
                reject_stale: *require_newer,
            },
        )?;
        let value = serde_json::to_value(report)?;
        if cli.json {
            print_json(&value)?;
        } else {
            print_portable_anchor_import(&value);
        }
        return Ok(());
    }
    if let Command::ToolCodes {
        category,
        retryable,
    } = &cli.command
    {
        let entries = filtered_tool_codes(category.as_deref(), *retryable);
        if cli.json {
            print_json(&serde_json::to_value(&entries)?)?;
        } else if entries.is_empty() {
            println!("No matching tool error codes.");
        } else {
            for entry in entries {
                println!(
                    "{:<28} {:<10} {:<9} {}",
                    entry.code.as_str(),
                    entry.category,
                    if entry.retryable {
                        "retryable"
                    } else {
                        "terminal"
                    },
                    entry.hint
                );
            }
        }
        return Ok(());
    }
    if let Command::DaemonAgent {
        action,
        yes,
        dry_run,
        data_dir,
        binary,
        force,
    } = &cli.command
    {
        let paths = vp_core::daemon_agent::DaemonAgentPaths::from_env(
            data_dir.clone().or_else(|| cli.data_dir.clone()),
            binary.clone(),
        )?;
        let runner = vp_core::daemon_agent::SystemLaunchctl;
        let agent = vp_core::daemon_agent::DaemonAgent::new(paths, &runner);
        match action {
            DaemonAgentAction::Status => {
                let status = agent.status();
                if cli.json {
                    print_json(&serde_json::to_value(status)?)?;
                } else {
                    println!(
                        "{}  state={}  installed={}  loaded={}  disabled={}  running={}  pid={}  supervised={}",
                        status.label,
                        status.state,
                        status.installed,
                        status.loaded,
                        status.disabled,
                        status.running,
                        status
                            .pid
                            .map(|pid| pid.to_string())
                            .unwrap_or_else(|| "-".into()),
                        status.supervised
                    );
                    if !status.detail.is_empty() {
                        println!("{}", status.detail);
                    }
                    println!("plist: {}", status.plist_path);
                }
            }
            DaemonAgentAction::Install => {
                let confirmed =
                    *yes || confirm_cli_action("Install the user LaunchAgent for verlinpadd?")?;
                let mutation = agent.install(confirmed, *dry_run, *force)?;
                if cli.json {
                    print_json(&serde_json::to_value(&mutation)?)?;
                } else {
                    println!("{} {}", mutation.action, mutation.label);
                    println!("plist: {}", mutation.plist_path);
                    println!("{}", mutation.detail);
                    for command in &mutation.commands {
                        println!("  {command}");
                    }
                    if let Some(plist) = &mutation.plist {
                        println!("\n{plist}");
                    }
                }
            }
            DaemonAgentAction::Uninstall => {
                let confirmed =
                    *yes || confirm_cli_action("Uninstall the user LaunchAgent for verlinpadd?")?;
                let mutation = agent.uninstall(confirmed, *dry_run)?;
                if cli.json {
                    print_json(&serde_json::to_value(&mutation)?)?;
                } else {
                    println!("{} {}", mutation.action, mutation.label);
                    println!("plist: {}", mutation.plist_path);
                    println!("{}", mutation.detail);
                    for command in &mutation.commands {
                        println!("  {command}");
                    }
                }
            }
        }
        return Ok(());
    }
    let discovery = read_discovery(cli.data_dir.as_deref())?;
    let want_tool_activity_csv = matches!(&cli.command, Command::ToolActivity { csv: true, .. });
    let value = dispatch::run_command(cli.command, discovery, cli.json).await?;
    if let Some(value) = value {
        if want_tool_activity_csv {
            print!("{}", tool_activity_csv(&value));
            return Ok(());
        }
        if cli.json {
            print_json(&value)?;
        } else {
            print_human(&value);
        }
    }
    Ok(())
}

/// Render an RPC failure for the terminal. The stable Q3 code and hint are
/// append-only additions to the human message; they are never inferred from
/// the message text.
pub(crate) fn parse_agent_identity_policy_json(value: Option<&str>) -> Result<Option<Value>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let parsed: Value = serde_json::from_str(value)
        .context("--policy-json must be a JSON object, for example '{}'")?;
    if !parsed.is_object() {
        bail!("--policy-json must be a JSON object, for example '{{}}'");
    }
    Ok(Some(parsed))
}

/// `kind:reference[:note]`. The reference is opaque and bounded by the daemon;
/// the CLI only splits on the first two colons.
pub(crate) fn parse_memory_evidence(value: &str) -> Result<Value> {
    let mut parts = value.splitn(3, ':');
    let kind = parts.next().unwrap_or_default().trim();
    let reference = parts.next().unwrap_or_default().trim();
    if kind.is_empty() || reference.is_empty() {
        bail!("--evidence must be kind:reference[:note]");
    }
    let mut evidence = json!({"kind": kind, "reference": reference});
    if let Some(note) = parts.next().map(str::trim).filter(|note| !note.is_empty()) {
        evidence["note"] = json!(note);
    }
    Ok(evidence)
}

pub(crate) fn insert_agent_identity_optional(
    params: &mut serde_json::Map<String, Value>,
    key: &str,
    value: Option<String>,
    clear: bool,
) {
    if clear {
        params.insert(key.to_string(), Value::Null);
    } else if let Some(value) = value {
        params.insert(key.to_string(), json!(value));
    }
}

fn load_keyring_optional(directory: Option<&Path>) -> Result<Option<Keyring>> {
    match directory {
        Some(directory) => Ok(Some(Keyring::load(directory)?)),
        None => Keyring::load_optional(&default_key_dir()),
    }
}

fn load_keyring_required(directory: Option<&Path>) -> Result<Keyring> {
    match directory {
        Some(directory) => Keyring::load(directory),
        None => Keyring::load(&default_key_dir()),
    }
}

/// Resolve signing options from CLI flags.
///
/// Signing is explicit: no flag means an unsigned v2 repository. Passing
/// `--signing-key`, `--sign`, or `--keyring-dir` loads the keyring and signs
/// with the requested or active key.
pub(crate) fn resolve_signing_options(
    keyring_dir: Option<&Path>,
    signing_key: Option<&str>,
    sign: bool,
) -> Result<Option<RepositorySigningOptions>> {
    if keyring_dir.is_none() && signing_key.is_none() && !sign {
        return Ok(None);
    }
    if sign && signing_key.is_some() {
        bail!("pass only one of --signing-key or --sign");
    }
    let keyring = load_keyring_required(keyring_dir)?;
    if let Some(key_id) = signing_key {
        keyring.public_key(key_id)?;
    } else {
        keyring.resolve_key_id(None)?;
    }
    Ok(Some(RepositorySigningOptions {
        keyring,
        key_id: signing_key.map(str::to_string),
    }))
}

pub(crate) fn anchor_store(data_dir: &Path, anchor_file: Option<&Path>) -> AnchorStore {
    match anchor_file {
        Some(path) => AnchorStore::from_file(path),
        None => AnchorStore::new(data_dir),
    }
}

fn resolve_passphrase(
    environment: Option<&str>,
    file: Option<&Path>,
) -> Result<Option<Passphrase>> {
    match (environment, file) {
        (None, None) => Ok(None),
        (Some(_), Some(_)) => {
            bail!("pass only one of --passphrase-env or --passphrase-file")
        }
        (Some(name), None) => {
            if name.is_empty() {
                bail!("--passphrase-env requires a non-empty environment variable name");
            }
            let value = std::env::var(name).with_context(|| {
                format!("backup passphrase environment variable {name} is not set or is not UTF-8")
            })?;
            Ok(Some(Passphrase::new(value.into_bytes())?))
        }
        (None, Some(path)) => {
            let metadata = std::fs::symlink_metadata(path)
                .with_context(|| format!("reading backup passphrase file {}", path.display()))?;
            if metadata.file_type().is_symlink() {
                bail!(
                    "refusing symlinked backup passphrase file {}",
                    path.display()
                );
            }
            if !metadata.is_file() {
                bail!(
                    "backup passphrase file {} is not a regular file",
                    path.display()
                );
            }
            if metadata.len() == 0 || metadata.len() > 4097 {
                bail!(
                    "backup passphrase file {} must contain 1..=4096 bytes",
                    path.display()
                );
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = metadata.permissions().mode() & 0o777;
                if mode & 0o077 != 0 {
                    bail!(
                        "backup passphrase file {} must not be accessible by group or others (mode {:04o})",
                        path.display(),
                        mode
                    );
                }
            }
            let mut bytes = std::fs::read(path)
                .with_context(|| format!("reading backup passphrase file {}", path.display()))?;
            if bytes.last() == Some(&b'\n') {
                bytes.pop();
                if bytes.last() == Some(&b'\r') {
                    bytes.pop();
                }
            }
            Ok(Some(Passphrase::new(bytes)?))
        }
    }
}

pub(crate) fn parse_tool_thresholds(values: &[String]) -> Result<Vec<(String, u64)>> {
    let mut thresholds = Vec::new();
    for value in values {
        let (tool, bytes) = value
            .split_once('=')
            .with_context(|| format!("--tool-threshold must be TOOL=BYTES: {value}"))?;
        let tool = tool.trim();
        if tool.is_empty() {
            bail!("--tool-threshold tool name must not be empty");
        }
        let bytes = bytes
            .trim()
            .parse::<u64>()
            .with_context(|| format!("--tool-threshold bytes must be an integer: {value}"))?;
        thresholds.push((tool.to_string(), bytes));
    }
    Ok(thresholds)
}

pub(crate) fn parse_env_pairs(
    values: &[String],
) -> Result<std::collections::BTreeMap<String, String>> {
    parse_pairs(values, "--env")
}

pub(crate) fn parse_pairs(
    values: &[String],
    flag: &str,
) -> Result<std::collections::BTreeMap<String, String>> {
    let mut pairs = std::collections::BTreeMap::new();
    for value in values {
        let (key, val) = value
            .split_once('=')
            .with_context(|| format!("{flag} must be KEY=VALUE: {value}"))?;
        if key.trim().is_empty() {
            bail!("{flag} key must not be empty");
        }
        pairs.insert(key.trim().to_string(), val.to_string());
    }
    Ok(pairs)
}

pub(crate) fn read_json_input(file: Option<&std::path::Path>) -> Result<String> {
    match file {
        Some(path) => {
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
        }
        None => {
            let mut buffer = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut buffer)?;
            Ok(buffer)
        }
    }
}

pub(crate) fn parse_dependency_failure(value: &str) -> Result<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "block" => Ok("block"),
        "cancel" => Ok("cancel"),
        "retry" => Ok("retry"),
        other => bail!("dependency failure policy must be block, cancel, or retry (got {other})"),
    }
}

/// Human rendering for `vp governor`, `vp critical-path`, and the project
/// policy commands. It reads only the real RPC payload; missing fields stay
/// absent instead of being invented.
/// Validate the optional CLI preview mode before the RPC. `None` means "use
/// the stored policy"; an unknown value is a local argument error, not a
/// silent fallback that would change admission semantics.
/// Merge CLI retrieval flags into a fetched/default policy object. This is a
/// pure function so the defaults, conflict checks, and field preservation are
/// testable without a daemon. The daemon remains the numeric clamp/validation
/// authority; the CLI never widens a value locally.
#[allow(clippy::too_many_arguments)]
pub(crate) fn merge_source_retrieval_policy(
    current: Value,
    enable_ann: bool,
    disable_ann: bool,
    ann_min_vectors: Option<u32>,
    ann_nprobe: Option<u32>,
    rerank: Option<String>,
    query_expansion: Option<String>,
    max_expanded_terms: Option<u32>,
    clear: bool,
) -> anyhow::Result<Value> {
    if enable_ann && disable_ann {
        anyhow::bail!("--enable-ann and --disable-ann cannot be combined");
    }
    if clear
        && (enable_ann
            || disable_ann
            || ann_min_vectors.is_some()
            || ann_nprobe.is_some()
            || rerank.is_some()
            || query_expansion.is_some()
            || max_expanded_terms.is_some())
    {
        anyhow::bail!("--clear cannot be combined with other retrieval policy flags");
    }
    let mut policy = if current.is_object() {
        current
    } else {
        json!({})
    };
    let defaults = [
        ("ann_enabled", json!(false)),
        ("ann_min_vectors", json!(256)),
        ("ann_nprobe", json!(8)),
        ("rerank", json!("deterministic")),
        ("query_expansion", json!("off")),
        ("max_expanded_terms", json!(8)),
    ];
    for (key, value) in defaults {
        if policy.get(key).is_none() {
            policy[key] = value;
        }
    }
    if enable_ann {
        policy["ann_enabled"] = json!(true);
    }
    if disable_ann {
        policy["ann_enabled"] = json!(false);
    }
    if let Some(value) = ann_min_vectors {
        policy["ann_min_vectors"] = json!(value);
    }
    if let Some(value) = ann_nprobe {
        policy["ann_nprobe"] = json!(value);
    }
    if let Some(value) = rerank {
        policy["rerank"] = json!(value);
    }
    if let Some(value) = query_expansion {
        policy["query_expansion"] = json!(value);
    }
    if let Some(value) = max_expanded_terms {
        policy["max_expanded_terms"] = json!(value);
    }
    Ok(policy)
}

pub(crate) fn parse_governor_mode(value: Option<&str>) -> anyhow::Result<Option<&'static str>> {
    match value {
        None => Ok(None),
        Some(value) => vp_protocol::types::CriticalPathMode::parse(value)
            .map(|mode| Some(mode.as_str()))
            .ok_or_else(|| anyhow::anyhow!("invalid --mode {value:?}; use `single` or `multi`")),
    }
}
