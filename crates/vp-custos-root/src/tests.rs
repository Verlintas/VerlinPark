//! Security-focused unit tests for the SELFTEST-01 vertical slice.
//!
//! Every test uses a `tempfile::TempDir`; no test calls `sudo`, `dscl`,
//! `launchctl`, writes `/Library`, or touches VerlinPark state.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use tempfile::TempDir;

use crate::audit::verify_audit_chain;
use crate::canonical::sha256_hex;
use crate::executor::{ExecuteError, ExecutorConfig, RootExecutor, SELFTEST_01};
use crate::installer::{InstallDecision, InstallFlags, InstallerPlan, LAUNCH_DAEMONS_DIR};
use crate::intent::{
    Ed25519Signer, IntentError, IntentPayload, Role, RoleSignature, SignedIntent, TrustStore,
    key_id_for_public_key,
};
use crate::layout::{DIR_PRIVATE_MODE, LayoutError, RootLayout, validate_existing_dir};
use crate::status::{AdvancedState, CustosTier, read_status};

const NOW: i64 = 1_800_000_000;
const EPOCH: u64 = 7;

struct Fixture {
    _temp: TempDir,
    layout: RootLayout,
    uid: u32,
    executor: RootExecutor,
    core: Ed25519Signer,
    warden: Ed25519Signer,
    anchor: Ed25519Signer,
    sentinel: Ed25519Signer,
    target_hash: String,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let layout = RootLayout::new(temp.path().join("VerlinParkCustos")).expect("layout");
        layout.ensure_directories().expect("directories");
        let uid = fs::metadata(layout.root()).expect("root metadata").uid();
        write_private(&layout.epoch_file(), format!("{EPOCH}\n").as_bytes());

        let core = Ed25519Signer::from_seed(Role::Core, [1u8; 32]).expect("core signer");
        let warden = Ed25519Signer::from_seed(Role::Warden, [2u8; 32]).expect("warden signer");
        let anchor = Ed25519Signer::from_seed(Role::Anchor, [3u8; 32]).expect("anchor signer");
        let sentinel =
            Ed25519Signer::from_seed(Role::Sentinel, [4u8; 32]).expect("sentinel signer");
        let trust = TrustStore::from_signers([&core, &warden, &anchor, &sentinel]).expect("trust");
        let target_hash = sha256_hex(b"target:selftest:fixture");
        let executor = RootExecutor::new(
            layout.clone(),
            trust,
            ExecutorConfig {
                expected_uid: uid,
                expected_epoch: EPOCH,
                expected_target_hash: target_hash.clone(),
            },
        )
        .expect("executor");
        Self {
            _temp: temp,
            layout,
            uid,
            executor,
            core,
            warden,
            anchor,
            sentinel,
            target_hash,
        }
    }

    fn payload(&self, seed: u64) -> IntentPayload {
        IntentPayload {
            version: 1,
            intent_id: intent_id(seed),
            action_id: SELFTEST_01.to_string(),
            epoch: EPOCH,
            issued_at_unix: NOW - 10,
            expires_at_unix: NOW + 60,
            target_hash: self.target_hash.clone(),
            nonce: intent_id(seed + 1000),
        }
    }

    fn sign(&self, payload: IntentPayload, signers: &[&Ed25519Signer]) -> SignedIntent {
        let signatures = signers
            .iter()
            .map(|signer| signer.sign_payload(&payload).expect("signature"))
            .collect();
        SignedIntent::new(payload, signatures)
    }

    fn execute(
        &self,
        signed: &SignedIntent,
    ) -> Result<crate::executor::ExecutionOutcome, ExecuteError> {
        self.executor.execute(signed, NOW)
    }
}

fn intent_id(seed: u64) -> String {
    format!("{seed:032x}")
}

fn write_private(path: &std::path::Path, bytes: &[u8]) {
    fs::write(path, bytes).expect("write private file");
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("private mode");
}

fn write_marker(path: &std::path::Path, bytes: &[u8]) {
    fs::write(path, bytes).expect("write marker");
    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).expect("marker mode");
}

fn marker_json(epoch: u64) -> Vec<u8> {
    format!(
        r#"{{"schema":"verlinpark.custos_advanced/v1","tier":"advanced","installed_at_unix":{},"epoch":{},"install_id":"{}","build_hash":"build","root_key_id":"root-key","created_at":"2026-11-29T00:00:00Z","charter_hash":"{}","signature":null}}"#,
        NOW,
        epoch,
        intent_id(77),
        sha256_hex(b"charter")
    )
    .into_bytes()
}

#[test]
fn valid_core_and_warden_selftest_writes_receipt_and_audit() {
    let fixture = Fixture::new();
    let payload = fixture.payload(1);
    let signed = fixture.sign(payload, &[&fixture.core, &fixture.warden]);

    let outcome = fixture.execute(&signed).expect("self-test succeeds");
    assert_eq!(outcome.receipt.action_id, SELFTEST_01);
    assert_eq!(outcome.receipt.signer_roles, vec![Role::Core, Role::Warden]);
    assert_eq!(
        outcome.receipt.effect,
        "none; no VerlinPark state was touched"
    );
    assert!(outcome.receipt_path.exists());
    let metadata = fs::metadata(&outcome.receipt_path).expect("receipt metadata");
    assert_eq!(metadata.uid(), fixture.uid);
    assert_eq!(metadata.mode() & 0o777, 0o600);
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(&outcome.receipt_path).expect("receipt bytes"))
            .expect("receipt JSON");
    assert_eq!(receipt["schema"], "vp-custos-root-receipt-v1");
    assert_eq!(receipt["intent_id"], intent_id(1));

    let chain = verify_audit_chain(&fixture.layout, fixture.uid).expect("valid audit chain");
    assert_eq!(chain.entries, 1);
    assert_eq!(chain.head_hash, outcome.audit.hash);
    assert_eq!(
        outcome.audit.entry.signer_roles,
        vec![Role::Core, Role::Warden]
    );
    assert_eq!(outcome.audit.entry.receipt_sha256, outcome.receipt_sha256);
}

#[test]
fn insufficient_quorum_is_rejected() {
    let fixture = Fixture::new();
    let signed = fixture.sign(fixture.payload(2), &[&fixture.core]);
    let error = fixture
        .execute(&signed)
        .expect_err("one signature must fail");
    assert!(
        matches!(
            error,
            ExecuteError::Intent(IntentError::InsufficientQuorum { got: 1, need: 2 })
        ),
        "{error}"
    );
    assert!(
        !fixture
            .layout
            .receipts_dir()
            .join(format!("{}.json", intent_id(2)))
            .exists()
    );
    assert_eq!(
        verify_audit_chain(&fixture.layout, fixture.uid)
            .expect("empty chain")
            .entries,
        0
    );
}

#[test]
fn wrong_role_is_rejected() {
    let fixture = Fixture::new();
    let payload = fixture.payload(3);
    let message = payload.signing_message().expect("signing message");
    let forged = RoleSignature {
        role: Role::Warden,
        key_id: fixture.core.key_id().to_string(),
        signature: fixture.core.sign_message(&message),
    };
    let signed = SignedIntent::new(payload, vec![forged]);
    let error = fixture
        .execute(&signed)
        .expect_err("role/key mismatch must fail");
    assert!(
        matches!(
            error,
            ExecuteError::Intent(IntentError::RoleKeyMismatch {
                key_role: Role::Core,
                claimed_role: Role::Warden
            })
        ),
        "{error}"
    );
}

#[test]
fn duplicate_role_is_rejected() {
    let fixture = Fixture::new();
    let payload = fixture.payload(4);
    let signature = fixture.core.sign_payload(&payload).expect("signature");
    let signed = SignedIntent::new(payload, vec![signature.clone(), signature]);
    let error = fixture
        .execute(&signed)
        .expect_err("duplicate role must fail");
    assert!(
        matches!(
            error,
            ExecuteError::Intent(IntentError::DuplicateRole(Role::Core))
        ),
        "{error}"
    );
}

#[test]
fn sentinel_cannot_vote() {
    let fixture = Fixture::new();
    let signed = fixture.sign(fixture.payload(5), &[&fixture.core, &fixture.sentinel]);
    let error = fixture
        .execute(&signed)
        .expect_err("sentinel vote must fail");
    assert!(
        matches!(
            error,
            ExecuteError::Intent(IntentError::IneligibleRole(Role::Sentinel))
        ),
        "{error}"
    );
}

#[test]
fn expired_intent_is_rejected() {
    let fixture = Fixture::new();
    let mut payload = fixture.payload(6);
    payload.issued_at_unix = NOW - 120;
    payload.expires_at_unix = NOW - 1;
    let signed = fixture.sign(payload, &[&fixture.core, &fixture.warden]);
    let error = fixture
        .execute(&signed)
        .expect_err("expired intent must fail");
    assert!(
        matches!(error, ExecuteError::Intent(IntentError::Expired)),
        "{error}"
    );
}

#[test]
fn replay_is_rejected() {
    let fixture = Fixture::new();
    let signed = fixture.sign(fixture.payload(7), &[&fixture.core, &fixture.anchor]);
    fixture.execute(&signed).expect("first execution");
    let error = fixture.execute(&signed).expect_err("replay must fail");
    assert!(matches!(error, ExecuteError::Replay { .. }), "{error}");
    let chain = verify_audit_chain(&fixture.layout, fixture.uid).expect("audit chain");
    assert_eq!(chain.entries, 1);
}

#[test]
fn tampered_payload_is_rejected() {
    let fixture = Fixture::new();
    let mut signed = fixture.sign(fixture.payload(8), &[&fixture.core, &fixture.warden]);
    signed.payload.nonce = intent_id(999);
    let error = fixture
        .execute(&signed)
        .expect_err("tampered payload must fail");
    assert!(
        matches!(
            error,
            ExecuteError::Intent(IntentError::InvalidSignature { role: Role::Core })
        ),
        "{error}"
    );
}

#[test]
fn wrong_target_is_rejected() {
    let fixture = Fixture::new();
    let mut payload = fixture.payload(9);
    payload.target_hash = sha256_hex(b"target:other");
    let signed = fixture.sign(payload, &[&fixture.core, &fixture.warden]);
    let error = fixture
        .execute(&signed)
        .expect_err("wrong target must fail");
    assert!(
        matches!(error, ExecuteError::Intent(IntentError::TargetMismatch)),
        "{error}"
    );
}

#[test]
fn stale_epoch_is_rejected() {
    let fixture = Fixture::new();
    let mut payload = fixture.payload(10);
    payload.epoch = EPOCH - 1;
    let signed = fixture.sign(payload, &[&fixture.core, &fixture.warden]);
    let error = fixture.execute(&signed).expect_err("stale epoch must fail");
    assert!(
        matches!(
            error,
            ExecuteError::Intent(IntentError::StaleEpoch {
                expected: EPOCH,
                actual: 6
            })
        ),
        "{error}"
    );
}

#[test]
fn unknown_action_is_rejected() {
    let fixture = Fixture::new();
    let mut payload = fixture.payload(11);
    payload.action_id = "SELFTEST-02".to_string();
    let signed = fixture.sign(payload, &[&fixture.core, &fixture.warden]);
    let error = fixture
        .execute(&signed)
        .expect_err("closed catalog must reject");
    assert!(
        matches!(error, ExecuteError::UnsupportedAction(ref action) if action == "SELFTEST-02"),
        "{error}"
    );
}

#[test]
fn audit_chain_verification_detects_tampering() {
    let fixture = Fixture::new();
    let first = fixture.sign(fixture.payload(12), &[&fixture.core, &fixture.warden]);
    fixture.execute(&first).expect("first");
    let second = fixture.sign(fixture.payload(13), &[&fixture.warden, &fixture.anchor]);
    fixture.execute(&second).expect("second");

    let chain = verify_audit_chain(&fixture.layout, fixture.uid).expect("two-entry chain");
    assert_eq!(chain.entries, 2);

    let path = fixture.layout.audit_log();
    let original = fs::read_to_string(&path).expect("audit text");
    let tampered = original.replacen("SELFTEST-01", "SELFTEST-02", 1);
    assert_ne!(original, tampered);
    fs::write(&path, tampered).expect("tamper audit");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("mode");
    let error = verify_audit_chain(&fixture.layout, fixture.uid).expect_err("tamper detected");
    assert!(
        matches!(
            error,
            crate::audit::AuditError::HashMismatch { .. }
                | crate::audit::AuditError::Sequence { .. }
                | crate::audit::AuditError::PreviousHash { .. }
        ),
        "{error}"
    );
}

#[test]
fn layout_rejects_symlink_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let real = temp.path().join("real");
    fs::create_dir(&real).expect("real dir");
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let layout = RootLayout::new(&link).expect("absolute layout");
    let uid = fs::metadata(&real).expect("metadata").uid();
    let error = layout.validate(uid, true).expect_err("symlink must fail");
    assert!(
        matches!(error, LayoutError::Symlink(ref path) if path == &link),
        "{error}"
    );
}

#[test]
fn layout_rejects_non_root_owner() {
    let temp = tempfile::tempdir().expect("tempdir");
    let actual = fs::metadata(temp.path()).expect("metadata").uid();
    let expected = if actual == 0 { 1 } else { 0 };
    let error =
        validate_existing_dir(temp.path(), expected, 0o755).expect_err("owner mismatch must fail");
    assert!(
        matches!(
            error,
            LayoutError::WrongOwner {
                expected: mismatch,
                actual: owner,
                ..
            } if mismatch == expected && owner == actual
        ),
        "{error}"
    );
}

#[test]
fn layout_rejects_broad_private_mode() {
    let fixture = Fixture::new();
    fs::set_permissions(fixture.layout.keys_dir(), fs::Permissions::from_mode(0o777))
        .expect("broad mode");
    let error = validate_existing_dir(&fixture.layout.keys_dir(), fixture.uid, DIR_PRIVATE_MODE)
        .expect_err("broad mode must fail");
    assert!(
        matches!(error, LayoutError::ModeTooBroad { mode: 0o777, .. }),
        "{error}"
    );
}

#[test]
fn layout_rejects_relative_and_parent_roots() {
    assert!(matches!(
        RootLayout::new("relative/root"),
        Err(LayoutError::RelativeRoot(_))
    ));
    assert!(matches!(
        RootLayout::new("/tmp/../etc"),
        Err(LayoutError::ParentComponent(_))
    ));
}

#[test]
fn installer_plan_is_complete_and_dry_run_only() {
    let fixture = Fixture::new();
    let plan = InstallerPlan::advanced(&fixture.layout).expect("plan");
    plan.validate().expect("valid plan");
    assert_eq!(plan.role_accounts.len(), 4);
    assert_eq!(plan.daemons.len(), 6);
    assert!(plan.contract.requires_root);
    assert_eq!(plan.contract.dry_run_flag, "--dry-run");
    assert_eq!(plan.contract.apply_flag, "--yes");
    assert_eq!(plan.contract.default_without_flags, "refuse");
    assert!(
        plan.daemons
            .iter()
            .any(|daemon| daemon.label == "com.verlintas.custos-root-monitor")
    );
    assert!(
        plan.daemons
            .iter()
            .any(|daemon| daemon.label == "com.verlintas.custos-root-executor")
    );
    assert!(
        plan.role_accounts
            .iter()
            .any(|account| account.user_name == "_custos_core")
    );
    assert!(
        plan.role_accounts
            .iter()
            .any(|account| account.user_name == "_custos_warden")
    );
    assert!(
        plan.role_accounts
            .iter()
            .any(|account| account.user_name == "_custos_anchor")
    );
    assert!(
        plan.role_accounts
            .iter()
            .any(|account| account.user_name == "_custos_sentinel")
    );
    for account in &plan.role_accounts {
        assert!(account.private_key_path.ends_with("intent.seed"));
        assert_eq!(
            account.public_key_path.extension(),
            Some(std::ffi::OsStr::new("pub"))
        );
    }
    assert!(
        plan.files
            .iter()
            .any(|file| file.path.ends_with("keys/core.pub"))
    );
    assert!(
        plan.files
            .iter()
            .any(|file| file.path.ends_with("bin/custos-rootd"))
    );
    for daemon in &plan.daemons {
        assert_eq!(
            daemon.plist_path.parent(),
            Some(std::path::Path::new(LAUNCH_DAEMONS_DIR))
        );
        assert_eq!(daemon.plist_owner, "root:wheel");
        assert_eq!(daemon.plist_mode, 0o644);
    }
    for directory in &plan.directories {
        assert_eq!(directory.mode & 0o022, 0, "no group/other write");
    }
    let rendered = plan.render_dry_run();
    assert!(rendered.contains("--dry-run"));
    assert!(rendered.contains("--yes"));
    assert!(rendered.contains("_custos_core"));
    assert!(rendered.contains("No files, users, plists, or services were changed."));
}

#[test]
fn install_contract_requires_explicit_mode() {
    assert_eq!(
        InstallFlags::default().resolve(),
        Err(crate::installer::InstallContractError::MissingMode)
    );
    assert_eq!(
        InstallFlags {
            dry_run: true,
            yes: true
        }
        .resolve(),
        Err(crate::installer::InstallContractError::ConflictingMode)
    );
    assert_eq!(
        InstallFlags {
            dry_run: true,
            yes: false
        }
        .resolve(),
        Ok(InstallDecision::DryRun)
    );
    assert_eq!(
        InstallFlags {
            dry_run: false,
            yes: true
        }
        .resolve(),
        Ok(InstallDecision::Apply)
    );
    assert_eq!(
        InstallFlags::from_args(["--dry-run"])
            .expect("flags")
            .resolve(),
        Ok(InstallDecision::DryRun)
    );
    assert_eq!(
        InstallFlags::from_args(["--bogus"]),
        Err(crate::installer::InstallContractError::UnknownArgument(
            "--bogus".to_string()
        ))
    );
}

#[test]
fn status_is_observer_without_marker() {
    let fixture = Fixture::new();
    let status = read_status(&fixture.layout, fixture.uid);
    assert_eq!(status.tier, CustosTier::Observer);
    assert!(!status.advanced_marker);
    assert_eq!(status.state, AdvancedState::NotInstalled);
    assert_eq!(status.epoch, None);
}

#[test]
fn status_reports_advanced_install_and_safe_mode() {
    let fixture = Fixture::new();
    write_marker(&fixture.layout.advanced_marker(), &marker_json(EPOCH));
    let status = read_status(&fixture.layout, fixture.uid);
    assert_eq!(status.tier, CustosTier::Advanced);
    assert!(status.advanced_marker);
    assert!(status.advanced_marker_valid);
    assert_eq!(status.state, AdvancedState::Installed);
    assert_eq!(status.epoch, Some(EPOCH));

    write_marker(&fixture.layout.safe_mode_marker(), b"safe\n");
    let status = read_status(&fixture.layout, fixture.uid);
    assert_eq!(status.state, AdvancedState::SafeMode);
}

#[test]
fn status_degrades_on_invalid_marker() {
    let fixture = Fixture::new();
    write_marker(&fixture.layout.advanced_marker(), b"{}");
    let status = read_status(&fixture.layout, fixture.uid);
    assert_eq!(status.tier, CustosTier::Advanced);
    assert!(status.advanced_marker);
    assert!(!status.advanced_marker_valid);
    assert_eq!(status.state, AdvancedState::Degraded);
    assert!(!status.issues.is_empty());
}

#[test]
fn status_degrades_on_tampered_audit_chain() {
    let fixture = Fixture::new();
    write_marker(&fixture.layout.advanced_marker(), &marker_json(EPOCH));
    let signed = fixture.sign(fixture.payload(14), &[&fixture.core, &fixture.anchor]);
    fixture.execute(&signed).expect("execution");

    let path = fixture.layout.audit_log();
    let original = fs::read_to_string(&path).expect("audit text");
    fs::write(
        &path,
        original.replacen("selftest.success", "selftest.failure", 1),
    )
    .expect("tamper");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("mode");

    let status = read_status(&fixture.layout, fixture.uid);
    assert_eq!(status.state, AdvancedState::Degraded);
    assert!(!status.issues.is_empty());
    assert_eq!(status.audit_entries, 0);
}

#[test]
fn key_ids_are_derived_from_public_keys() {
    let key = [9u8; 32];
    assert_eq!(
        key_id_for_public_key(&key),
        format!("ed25519:{}", &sha256_hex(&key)[..32])
    );
}
