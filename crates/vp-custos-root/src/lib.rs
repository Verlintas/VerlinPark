//! Root-owned foundation for CUSTOS Advanced (ADR-0247).
//!
//! This crate is a deliberately small, testable slice:
//!
//! - the root state layout under
//!   `/Library/Application Support/VerlinParkCustos` (parameterized for tests);
//! - no-follow path, ownership, and mode validation using safe `std::fs`
//!   metadata (no `unsafe`, no syscall wrapper yet);
//! - Ed25519 role intents with canonical JSON, domain separation, distinct
//!   voting roles (`core`, `warden`, `anchor`; `sentinel` has no vote), a
//!   2-of-3 quorum, expiry, epoch, target-hash, and replay checks;
//! - one closed action, [`SELFTEST-01`](executor::SELFTEST_01), which writes a
//!   root-owned-style success receipt and appends a hash-chained audit entry;
//! - an installer **plan** with an explicit `--dry-run`/`--yes` contract, and a
//!   read-only status model.
//!
//! It does not call `launchctl`, `dscl`, or `sudo`, does not create users, does
//! not write `/Library`, does not use a network or a language model, and never
//! mutates VerlinPark state. Installer execution, role daemons, the root
//! monitor/executor process, and L2–L4 actions are intentionally out of scope.
//!
//! The SELFTEST-01 receipt is evidence that signing, quorum, replay, and audit
//! plumbing worked. It is not merge evidence, an approval, or a VerlinPark
//! session event.

#![forbid(unsafe_code)]

pub mod audit;
pub mod canonical;
pub mod executor;
pub mod installer;
pub mod intent;
pub mod layout;
pub mod status;

#[cfg(test)]
mod tests;

pub use audit::{
    AuditChain, AuditEntry, AuditEvent, AuditRecord, append_audit, verify_audit_chain,
};
pub use executor::{
    ExecutionOutcome, ExecutorConfig, RootAction, RootExecutor, SELFTEST_01, SuccessReceipt,
};
pub use installer::{
    InstallContract, InstallContractError, InstallDecision, InstallFlags, InstallerPlan,
    LaunchDaemonPlan, RoleAccountPlan,
};
pub use intent::{
    Ed25519Signer, IntentPayload, IntentVerifier, Role, RoleSignature, SignedIntent, TrustStore,
    TrustedKey, VerifiedIntent,
};
pub use layout::{
    ADVANCED_ROOT, DIR_PRIVATE_MODE, DIR_ROOT_MODE, FILE_MARKER_MODE, FILE_PRIVATE_MODE,
    FILE_PUBLIC_KEY_MODE, ROOT_UID, RootLayout, validate_existing_dir, validate_existing_file,
};
pub use status::{AdvancedMarker, AdvancedState, CustosTier, RootStatus, read_status};
