//! Closed-action executor for the Advanced self-test.
//!
//! Only [`SELFTEST_01`] is accepted. The executor verifies the signed intent and
//! 2-of-3 quorum, reserves the intent id against replay, writes a 0600 success
//! receipt under the root-owned receipts directory, and appends a hash-chained
//! audit entry. It never opens the VerlinPark database, event log, RPC socket,
//! project worktree, or any path outside the supplied [`RootLayout`].
//!
//! Ordering is deliberate:
//!
//! 1. verify the intent and quorum (pure);
//! 2. create the receipt with `create_new` (an existing receipt is a replay);
//! 3. record the replay marker;
//! 4. append and fsync the audit entry.
//!
//! A crash after step 2 but before step 4 leaves a receipt without an audit
//! entry and fails the next attempt closed as a replay; the caller sees the
//! error and can inspect the root-owned evidence. This slice does not implement
//! a recovery/repair path, and the self-test is not retried automatically.

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::audit::{AuditEvent, AuditRecord, append_audit};
use crate::canonical::{canonical_json, sha256_hex};
use crate::intent::{
    IntentError, IntentPayload, IntentVerifier, ReplayCache, Role, SignedIntent, TrustStore,
};
use crate::layout::{FILE_PRIVATE_MODE, LayoutError, RootLayout};

/// The only action authorized by this slice.
pub const SELFTEST_01: &str = "SELFTEST-01";

/// Closed action catalog for the root executor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootAction {
    Selftest01,
}

impl RootAction {
    pub fn from_id(action_id: &str) -> Option<Self> {
        match action_id {
            SELFTEST_01 => Some(Self::Selftest01),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Selftest01 => SELFTEST_01,
        }
    }
}

/// Static executor configuration. The target hash is the 32-byte commitment the
/// self-test expects; pid/start-time/code-signature resolution is not part of
/// this slice.
#[derive(Clone, Debug)]
pub struct ExecutorConfig {
    pub expected_uid: u32,
    pub expected_epoch: u64,
    pub expected_target_hash: String,
}

/// Successful self-test evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuccessReceipt {
    pub schema: String,
    pub receipt_id: String,
    pub action_id: String,
    pub intent_id: String,
    pub epoch: u64,
    pub target_hash: String,
    pub signer_roles: Vec<Role>,
    pub executed_at_unix: i64,
    pub tier: String,
    pub effect: String,
}

/// Result of one successful execution.
#[derive(Clone, Debug)]
pub struct ExecutionOutcome {
    pub receipt: SuccessReceipt,
    pub receipt_path: PathBuf,
    pub receipt_sha256: String,
    pub audit: AuditRecord,
}

/// Root executor. It owns no global state beyond the supplied layout and trust
/// store; every call revalidates the layout.
pub struct RootExecutor {
    layout: RootLayout,
    verifier: IntentVerifier,
    config: ExecutorConfig,
    replay: ReplayCache,
}

impl RootExecutor {
    pub fn new(
        layout: RootLayout,
        trust: TrustStore,
        config: ExecutorConfig,
    ) -> Result<Self, IntentError> {
        let verifier = IntentVerifier::new(
            trust,
            config.expected_epoch,
            config.expected_target_hash.clone(),
        )?;
        let replay = ReplayCache::new(layout.replay_dir());
        Ok(Self {
            layout,
            verifier,
            config,
            replay,
        })
    }

    pub fn layout(&self) -> &RootLayout {
        &self.layout
    }

    /// Execute the closed self-test action.
    pub fn execute(
        &self,
        signed: &SignedIntent,
        now_unix: i64,
    ) -> Result<ExecutionOutcome, ExecuteError> {
        let action = RootAction::from_id(&signed.payload.action_id)
            .ok_or_else(|| ExecuteError::UnsupportedAction(signed.payload.action_id.clone()))?;
        if action != RootAction::Selftest01 {
            return Err(ExecuteError::UnsupportedAction(
                signed.payload.action_id.clone(),
            ));
        }
        self.layout.validate(self.config.expected_uid, true)?;
        let verified = self.verifier.verify(signed, now_unix)?;
        if self.replay.contains(&verified.payload.intent_id) {
            return Err(ExecuteError::Replay {
                intent_id: verified.payload.intent_id,
            });
        }

        let receipt = build_receipt(&verified.payload, &verified.signer_roles, now_unix);
        let receipt_bytes = canonical_json(&receipt)?;
        let receipt_path = self.layout.receipt_file(&receipt.intent_id);
        write_new_private(&receipt_path, &receipt_bytes)?;
        let receipt_sha256 = sha256_hex(&receipt_bytes);

        if !self
            .replay
            .record(&receipt.intent_id, verified.payload.expires_at_unix)?
        {
            return Err(ExecuteError::Replay {
                intent_id: receipt.intent_id,
            });
        }

        let audit = append_audit(
            &self.layout,
            self.config.expected_uid,
            AuditEvent {
                recorded_at_unix: now_unix,
                kind: "selftest.success".to_string(),
                action_id: receipt.action_id.clone(),
                intent_id: receipt.intent_id.clone(),
                epoch: receipt.epoch,
                target_hash: receipt.target_hash.clone(),
                signer_roles: receipt.signer_roles.clone(),
                receipt_file: receipt_path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| receipt.intent_id.clone()),
                receipt_sha256: receipt_sha256.clone(),
            },
        )?;

        Ok(ExecutionOutcome {
            receipt,
            receipt_path,
            receipt_sha256,
            audit,
        })
    }
}

fn build_receipt(payload: &IntentPayload, signer_roles: &[Role], now_unix: i64) -> SuccessReceipt {
    SuccessReceipt {
        schema: "vp-custos-root-receipt-v1".to_string(),
        receipt_id: payload.intent_id.clone(),
        action_id: SELFTEST_01.to_string(),
        intent_id: payload.intent_id.clone(),
        epoch: payload.epoch,
        target_hash: payload.target_hash.clone(),
        signer_roles: signer_roles.to_vec(),
        executed_at_unix: now_unix,
        tier: "advanced".to_string(),
        effect: "none; no VerlinPark state was touched".to_string(),
    }
}

fn write_new_private(path: &std::path::Path, bytes: &[u8]) -> Result<(), ExecuteError> {
    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_PRIVATE_MODE)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return Err(ExecuteError::Replay {
                intent_id: path
                    .file_stem()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "<unknown>".to_string()),
            });
        }
        Err(source) => {
            return Err(ExecuteError::ReceiptIo {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    file.set_permissions(fs::Permissions::from_mode(FILE_PRIVATE_MODE))
        .map_err(|source| ExecuteError::ReceiptIo {
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| ExecuteError::ReceiptIo {
            path: path.to_path_buf(),
            source,
        })
}

/// Typed execution failures. No variant carries a private key or file content.
#[derive(Debug, Error)]
pub enum ExecuteError {
    #[error("action {0:?} is not in the root executor catalog")]
    UnsupportedAction(String),
    #[error(transparent)]
    Layout(#[from] LayoutError),
    #[error(transparent)]
    Intent(#[from] IntentError),
    #[error(transparent)]
    Audit(#[from] crate::audit::AuditError),
    #[error("canonical receipt error: {0}")]
    Canonical(#[from] crate::canonical::CanonicalError),
    #[error("intent {intent_id} was already executed (replay)")]
    Replay { intent_id: String },
    #[error("write receipt {path}: {source}")]
    ReceiptIo {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("record replay marker: {0}")]
    ReplayIo(#[from] io::Error),
}
