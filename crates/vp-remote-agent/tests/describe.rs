//! Contract test for the helper metadata command used by bootstrap.
//!
//! `--describe` must work without a token, root, or ledger because the
//! installer runs it on the temporary binary before the helper is installed.

use std::process::Command;

#[test]
fn describe_reports_version_protocol_target_and_capabilities_without_token() {
    let output = Command::new(env!("CARGO_BIN_EXE_vp-remote-agent"))
        .arg("remote-agent")
        .arg("--describe")
        .env_clear()
        .output()
        .expect("run vp-remote-agent --describe");
    assert!(
        output.status.success(),
        "describe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON describe");
    assert_eq!(
        value["protocol_version"].as_u64(),
        Some(u64::from(vp_remote::REMOTE_PROTOCOL_VERSION))
    );
    assert!(
        value["helper_version"]
            .as_str()
            .is_some_and(|version| !version.is_empty())
    );
    assert_eq!(value["target_os"].as_str(), Some(std::env::consts::OS));
    assert_eq!(value["target_arch"].as_str(), Some(std::env::consts::ARCH));
    let capabilities = value["capabilities"].as_array().expect("capabilities");
    for capability in [
        "fs.read",
        "fs.write",
        "exec",
        "git",
        "git.worktree",
        "git.bundle",
        "sha256",
    ] {
        assert!(
            capabilities.iter().any(|value| value == capability),
            "missing capability {capability}"
        );
    }
}

#[test]
fn describe_does_not_require_a_root_or_auth_token() {
    let output = Command::new(env!("CARGO_BIN_EXE_vp-remote-agent"))
        .arg("--describe")
        .env_clear()
        .output()
        .expect("run vp-remote-agent --describe");
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
}
