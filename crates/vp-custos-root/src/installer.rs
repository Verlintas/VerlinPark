//! Installer plan and CLI contract. This module performs no installation.
//!
//! It models the root LaunchDaemons, independent role users and keys, state
//! directories, plist ownership, and the explicit `--dry-run` / `--yes`
//! contract that a future privileged installer must implement. There is no
//! `launchctl`, `dscl`, `sudo`, user creation, or filesystem write here, and
//! tests cannot accidentally execute one.
//!
//! The plan is intentionally data, not a script: execution needs separate
//! review, an authorization prompt, checksum verification, and rollback design.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::intent::Role;
use crate::layout::{
    DIR_BIN_MODE, DIR_PRIVATE_MODE, DIR_ROOT_MODE, FILE_MARKER_MODE, FILE_PRIVATE_MODE,
    FILE_PUBLIC_KEY_MODE, LayoutError, RootLayout,
};

pub const LAUNCH_DAEMONS_DIR: &str = "/Library/LaunchDaemons";
pub const ROLE_STATE_ROOT: &str = "/var/db/verlinpark-custos";
pub const LABEL_PREFIX: &str = "com.verlintas.custos";

/// One independent role account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleAccountPlan {
    pub role: Role,
    pub user_name: String,
    pub home_dir: PathBuf,
    pub private_key_path: PathBuf,
    pub public_key_path: PathBuf,
    pub launch_label: String,
}

/// Which OS user runs a LaunchDaemon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceUser {
    Root,
    Role(Role),
}

impl std::fmt::Display for ServiceUser {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Root => formatter.write_str("root"),
            Self::Role(role) => formatter.write_str(role.as_str()),
        }
    }
}

/// One root LaunchDaemon plist. Execution is explicitly out of scope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchDaemonPlan {
    pub label: String,
    pub run_as: ServiceUser,
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub plist_path: PathBuf,
    pub plist_owner: String,
    pub plist_mode: u32,
    pub keep_alive: bool,
    pub run_at_load: bool,
    pub throttle_interval_secs: u64,
}

/// One directory the installer would create/chown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryPlan {
    pub path: PathBuf,
    pub mode: u32,
    pub owner: String,
    pub purpose: String,
}

/// One file the installer would create/install.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilePlan {
    pub path: PathBuf,
    pub mode: u32,
    pub owner: String,
    pub purpose: String,
}

/// The complete plan. It is descriptive only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallerPlan {
    pub root: PathBuf,
    pub role_accounts: Vec<RoleAccountPlan>,
    pub daemons: Vec<LaunchDaemonPlan>,
    pub directories: Vec<DirectoryPlan>,
    pub files: Vec<FilePlan>,
    pub contract: InstallContract,
    pub steps: Vec<String>,
}

/// Explicit installer contract. There is no implicit apply path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallContract {
    pub requires_root: bool,
    pub dry_run_flag: String,
    pub apply_flag: String,
    pub apply_requires_explicit_confirmation: bool,
    pub default_without_flags: String,
}

impl Default for InstallContract {
    fn default() -> Self {
        Self {
            requires_root: true,
            dry_run_flag: "--dry-run".to_string(),
            apply_flag: "--yes".to_string(),
            apply_requires_explicit_confirmation: true,
            default_without_flags: "refuse".to_string(),
        }
    }
}

/// Flags parsed from the future installer CLI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InstallFlags {
    pub dry_run: bool,
    pub yes: bool,
}

/// What the flags authorize. Returning `Apply` does not execute anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallDecision {
    DryRun,
    Apply,
}

impl InstallFlags {
    /// Parse only `--dry-run` and `--yes`; unknown flags fail closed.
    pub fn from_args<I, S>(args: I) -> Result<Self, InstallContractError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut flags = Self::default();
        for argument in args {
            match argument.into().as_str() {
                "--dry-run" => flags.dry_run = true,
                "--yes" => flags.yes = true,
                other => return Err(InstallContractError::UnknownArgument(other.to_string())),
            }
        }
        Ok(flags)
    }

    /// Resolve the mutually exclusive contract.
    pub fn resolve(self) -> Result<InstallDecision, InstallContractError> {
        match (self.dry_run, self.yes) {
            (true, false) => Ok(InstallDecision::DryRun),
            (false, true) => Ok(InstallDecision::Apply),
            (true, true) => Err(InstallContractError::ConflictingMode),
            (false, false) => Err(InstallContractError::MissingMode),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum InstallContractError {
    #[error("choose exactly one of --dry-run or --yes")]
    MissingMode,
    #[error("--dry-run and --yes are mutually exclusive")]
    ConflictingMode,
    #[error("unsupported installer argument {0:?}")]
    UnknownArgument(String),
}

impl InstallerPlan {
    /// Build the Advanced plan for a layout root. No path is created or read.
    pub fn advanced(layout: &RootLayout) -> Result<Self, LayoutError> {
        let root = layout.root().to_path_buf();
        let program = layout.bin_dir().join("custos-rootd");
        let mut role_accounts = Vec::with_capacity(4);
        for role in Role::ALL {
            let state_dir = PathBuf::from(ROLE_STATE_ROOT).join(role.as_str());
            role_accounts.push(RoleAccountPlan {
                role,
                user_name: role_user_name(role).to_string(),
                home_dir: state_dir.clone(),
                private_key_path: state_dir.join("intent.seed"),
                public_key_path: layout.public_key_file(role),
                launch_label: format!("{LABEL_PREFIX}-{}", role.as_str()),
            });
        }

        let mut daemons = Vec::with_capacity(6);
        daemons.push(LaunchDaemonPlan {
            label: format!("{LABEL_PREFIX}-root-monitor"),
            run_as: ServiceUser::Root,
            program: program.clone(),
            arguments: vec![
                "monitor".to_string(),
                "--root".to_string(),
                root.display().to_string(),
            ],
            plist_path: PathBuf::from(LAUNCH_DAEMONS_DIR)
                .join(format!("{LABEL_PREFIX}-root-monitor.plist")),
            plist_owner: "root:wheel".to_string(),
            plist_mode: FILE_MARKER_MODE,
            keep_alive: true,
            run_at_load: true,
            throttle_interval_secs: 10,
        });
        daemons.push(LaunchDaemonPlan {
            label: format!("{LABEL_PREFIX}-root-executor"),
            run_as: ServiceUser::Root,
            program: program.clone(),
            arguments: vec![
                "executor".to_string(),
                "--root".to_string(),
                root.display().to_string(),
            ],
            plist_path: PathBuf::from(LAUNCH_DAEMONS_DIR)
                .join(format!("{LABEL_PREFIX}-root-executor.plist")),
            plist_owner: "root:wheel".to_string(),
            plist_mode: FILE_MARKER_MODE,
            keep_alive: true,
            run_at_load: true,
            throttle_interval_secs: 10,
        });
        for account in &role_accounts {
            daemons.push(LaunchDaemonPlan {
                label: account.launch_label.clone(),
                run_as: ServiceUser::Role(account.role),
                program: program.clone(),
                arguments: vec![
                    "role".to_string(),
                    account.role.as_str().to_string(),
                    "--root".to_string(),
                    root.display().to_string(),
                ],
                plist_path: PathBuf::from(LAUNCH_DAEMONS_DIR)
                    .join(format!("{}.plist", account.launch_label)),
                plist_owner: "root:wheel".to_string(),
                plist_mode: FILE_MARKER_MODE,
                keep_alive: true,
                run_at_load: true,
                throttle_interval_secs: 10,
            });
        }

        let directories = vec![
            directory(&root, DIR_ROOT_MODE, "root:wheel", "Advanced root state"),
            directory(
                &layout.bin_dir(),
                DIR_BIN_MODE,
                "root:wheel",
                "root-owned helper binaries",
            ),
            directory(
                &layout.keys_dir(),
                DIR_PRIVATE_MODE,
                "root:wheel",
                "trusted role public keys",
            ),
            directory(
                &layout.intents_dir(),
                DIR_PRIVATE_MODE,
                "root:wheel",
                "verified-intent staging",
            ),
            directory(
                &layout.receipts_dir(),
                DIR_PRIVATE_MODE,
                "root:wheel",
                "success receipts",
            ),
            directory(
                &layout.audit_dir(),
                DIR_PRIVATE_MODE,
                "root:wheel",
                "hash-chained audit",
            ),
            directory(
                &layout.state_dir(),
                DIR_PRIVATE_MODE,
                "root:wheel",
                "epoch and installation state",
            ),
            directory(
                &layout.replay_dir(),
                DIR_PRIVATE_MODE,
                "root:wheel",
                "replay markers",
            ),
        ];
        let mut all_directories = directories;
        all_directories.push(directory(
            Path::new(ROLE_STATE_ROOT),
            0o755,
            "root:wheel",
            "role account state root",
        ));
        for account in &role_accounts {
            all_directories.push(directory(
                &account.home_dir,
                DIR_PRIVATE_MODE,
                &account.user_name,
                "role-private signing state",
            ));
        }

        let mut files = vec![
            file(
                &layout.audit_log(),
                FILE_PRIVATE_MODE,
                "root:wheel",
                "append-only audit chain",
            ),
            file(
                &layout.epoch_file(),
                FILE_PRIVATE_MODE,
                "root:wheel",
                "monotonic generation epoch",
            ),
            file(
                &layout.advanced_marker(),
                FILE_MARKER_MODE,
                "root:wheel",
                "Advanced tier marker",
            ),
            file(
                &layout.install_record(),
                FILE_MARKER_MODE,
                "root:wheel",
                "installer record",
            ),
            file(
                &program,
                0o755,
                "root:wheel",
                "root monitor/executor and role helper binary",
            ),
        ];
        for account in &role_accounts {
            files.push(file(
                &account.public_key_path,
                FILE_PUBLIC_KEY_MODE,
                "root:wheel",
                "trusted role public key",
            ));
            files.push(file(
                &account.private_key_path,
                FILE_PRIVATE_MODE,
                &account.user_name,
                "role-private Ed25519 seed",
            ));
        }

        let steps = vec![
            "verify the installer bundle checksum and display this plan".to_string(),
            "require explicit --yes; --dry-run never mutates state".to_string(),
            "create the independent _custos_* role users with no login shell".to_string(),
            "create root-owned state directories and role-private key directories".to_string(),
            "generate one Ed25519 key per role in its private directory and publish only public keys".to_string(),
            "install root LaunchDaemon plists with root:wheel ownership and 0644 mode".to_string(),
            "bootstrap the root monitor, root executor, and four role daemons".to_string(),
            "run SELFTEST-01 and verify receipt and audit-chain evidence before reporting success".to_string(),
            "write the Advanced marker only after the self-test passes".to_string(),
        ];

        let plan = Self {
            root,
            role_accounts,
            daemons,
            directories: all_directories,
            files,
            contract: InstallContract::default(),
            steps,
        };
        plan.validate()?;
        Ok(plan)
    }

    /// Structural validation only. It never touches `/Library`.
    pub fn validate(&self) -> Result<(), LayoutError> {
        if !self.root.is_absolute() || self.root == Path::new("/") {
            return Err(LayoutError::PlanInvalid(
                "plan root must be an absolute non-root path".to_string(),
            ));
        }
        if !self.contract.requires_root
            || !self.contract.apply_requires_explicit_confirmation
            || self.contract.dry_run_flag != "--dry-run"
            || self.contract.apply_flag != "--yes"
            || self.contract.default_without_flags != "refuse"
        {
            return Err(LayoutError::PlanInvalid(
                "installer contract must require root, --dry-run, --yes, and refuse without flags"
                    .to_string(),
            ));
        }
        if self.role_accounts.len() != Role::ALL.len() {
            return Err(LayoutError::PlanInvalid(
                "expected exactly four role accounts".to_string(),
            ));
        }
        let mut labels = std::collections::HashSet::new();
        for daemon in &self.daemons {
            if !labels.insert(daemon.label.clone()) {
                return Err(LayoutError::PlanInvalid(format!(
                    "duplicate daemon label {}",
                    daemon.label
                )));
            }
            if daemon.plist_path.parent() != Some(Path::new(LAUNCH_DAEMONS_DIR)) {
                return Err(LayoutError::PlanInvalid(format!(
                    "plist outside {LAUNCH_DAEMONS_DIR}: {}",
                    daemon.plist_path.display()
                )));
            }
            if daemon.plist_mode != FILE_MARKER_MODE || daemon.plist_owner != "root:wheel" {
                return Err(LayoutError::PlanInvalid(format!(
                    "plist ownership/mode is not root:wheel 0644: {}",
                    daemon.plist_path.display()
                )));
            }
        }
        for role in Role::ALL {
            if !self
                .daemons
                .iter()
                .any(|daemon| daemon.run_as == ServiceUser::Role(role))
            {
                return Err(LayoutError::PlanInvalid(format!(
                    "missing launchd service for role {role}"
                )));
            }
        }
        if !self
            .daemons
            .iter()
            .any(|daemon| daemon.run_as == ServiceUser::Root)
        {
            return Err(LayoutError::PlanInvalid(
                "missing root monitor/executor service".to_string(),
            ));
        }
        for directory in &self.directories {
            if directory.mode & 0o022 != 0 {
                return Err(LayoutError::PlanInvalid(format!(
                    "directory plan is group/other writable: {}",
                    directory.path.display()
                )));
            }
        }
        for file in &self.files {
            if file.mode & 0o022 != 0 {
                return Err(LayoutError::PlanInvalid(format!(
                    "file plan is group/other writable: {}",
                    file.path.display()
                )));
            }
        }
        Ok(())
    }

    /// Stable human-readable plan. It says explicitly that nothing was changed.
    pub fn render_dry_run(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "CUSTOS Advanced installer plan (dry run)");
        let _ = writeln!(out, "root: {}", self.root.display());
        let _ = writeln!(out, "requires root: yes");
        let _ = writeln!(
            out,
            "apply contract: {} (no changes) or {} (explicit apply); no flags refuses",
            self.contract.dry_run_flag, self.contract.apply_flag
        );
        let _ = writeln!(out, "role users:");
        for account in &self.role_accounts {
            let _ = writeln!(
                out,
                "  {} -> {} (key {})",
                account.user_name,
                account.home_dir.display(),
                account.private_key_path.display()
            );
        }
        let _ = writeln!(out, "services:");
        for daemon in &self.daemons {
            let _ = writeln!(
                out,
                "  [{}] {} -> {}",
                daemon.run_as,
                daemon.label,
                daemon.plist_path.display()
            );
        }
        let _ = writeln!(out, "steps:");
        for (index, step) in self.steps.iter().enumerate() {
            let _ = writeln!(out, "  {}. {step}", index + 1);
        }
        let _ = writeln!(out, "No files, users, plists, or services were changed.");
        out
    }
}

fn role_user_name(role: Role) -> &'static str {
    match role {
        Role::Core => "_custos_core",
        Role::Warden => "_custos_warden",
        Role::Anchor => "_custos_anchor",
        Role::Sentinel => "_custos_sentinel",
    }
}

fn directory(path: &Path, mode: u32, owner: &str, purpose: &str) -> DirectoryPlan {
    DirectoryPlan {
        path: path.to_path_buf(),
        mode,
        owner: owner.to_string(),
        purpose: purpose.to_string(),
    }
}

fn file(path: &Path, mode: u32, owner: &str, purpose: &str) -> FilePlan {
    FilePlan {
        path: path.to_path_buf(),
        mode,
        owner: owner.to_string(),
        purpose: purpose.to_string(),
    }
}
