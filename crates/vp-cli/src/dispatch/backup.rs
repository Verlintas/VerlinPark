//! Pre-discovery maintenance and backup repository commands.

use super::*;

pub(super) async fn dispatch(
    command: Command,
    _discovery: Discovery,
    _json: bool,
) -> Result<Option<Value>> {
    match command {
        Command::DbRebuild { .. } => unreachable!("db rebuild is handled before discovery"),
        Command::Backup { .. } => unreachable!("backup is handled before discovery"),
        Command::Restore { .. } => unreachable!("restore is handled before discovery"),
        Command::BackupList { .. } => unreachable!("backup-list is handled before discovery"),
        Command::BackupPrune { .. } => unreachable!("backup-prune is handled before discovery"),
        Command::BackupVerify { .. } => unreachable!("backup-verify is handled before discovery"),
        Command::BackupKeyGenerate { .. } => {
            unreachable!("backup-key-generate is handled before discovery")
        }
        Command::BackupKeyList { .. } => {
            unreachable!("backup-key-list is handled before discovery")
        }
        Command::BackupKeyEvents { .. } => {
            unreachable!("backup-key-events is handled before discovery")
        }
        Command::BackupKeyRotate { .. } => {
            unreachable!("backup-key-rotate is handled before discovery")
        }
        Command::BackupKeyRevoke { .. } => {
            unreachable!("backup-key-revoke is handled before discovery")
        }
        Command::BackupRekey { .. } => {
            unreachable!("backup-rekey is handled before discovery")
        }
        Command::BackupKeyExportPublic { .. } => {
            unreachable!("backup-key-export-public is handled before discovery")
        }
        Command::BackupKeyImport { .. } => {
            unreachable!("backup-key-import is handled before discovery")
        }
        Command::BackupKeyring { .. } => unreachable!("backup-keyring is handled before discovery"),
        Command::BackupAnchor { .. } => unreachable!("backup-anchor is handled before discovery"),
        Command::BackupAnchorExport { .. } => {
            unreachable!("backup-anchor-export is handled before discovery")
        }
        Command::BackupAnchorVerify { .. } => {
            unreachable!("backup-anchor-verify is handled before discovery")
        }
        Command::BackupAnchorImport { .. } => {
            unreachable!("backup-anchor-import is handled before discovery")
        }
        Command::Bench { .. } => unreachable!("bench is handled before discovery"),
        Command::CacheBench { .. } => {
            unreachable!("cache-bench is handled before discovery")
        }
        Command::DaemonAgent { .. } => {
            unreachable!("daemon-agent is handled before discovery")
        }
        Command::ToolCodes { .. } => unreachable!("tool-codes is handled before discovery"),
        _ => unreachable!("command routed to the wrong dispatch family"),
    }
}
