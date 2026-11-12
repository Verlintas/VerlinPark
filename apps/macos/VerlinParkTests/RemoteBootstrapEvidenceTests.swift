import Foundation
@testable import VerlinPark
import XCTest

final class RemoteBootstrapEvidenceTests: XCTestCase {
    func testBootstrapStatusDecodesRecordAndRisk() throws {
        let json = """
        {
          "project_id": "prj-1",
          "configured": true,
          "host": "build.example.com",
          "install_path": "/home/agent/.verlinpark/bin/vp-remote-agent",
          "configured_helper_path": "~/.verlinpark/bin/vp-remote-agent",
          "path_matches_config": true,
          "record": {
            "project_id": "prj-1",
            "host": "build.example.com",
            "install_path": "/home/agent/.verlinpark/bin/vp-remote-agent",
            "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bytes": 4096,
            "helper_version": "0.1.0-dev",
            "protocol_version": 1,
            "target_os": "linux",
            "target_arch": "x86_64",
            "installed_at": "2026-11-12T00:00:00Z",
            "updated_at": "2026-11-12T00:00:00Z"
          },
          "risk": "host_state",
          "approval_required": true,
          "environment_caveat": "trusted SSH host; not sandboxed"
        }
        """
        let status = try JSONDecoder().decode(
            RemoteBootstrapStatusResult.self,
            from: Data(json.utf8)
        )
        XCTAssertTrue(status.configured)
        XCTAssertTrue(status.pathMatchesConfig)
        XCTAssertEqual(status.risk, "host_state")
        XCTAssertEqual(status.record?.helperVersion, "0.1.0-dev")
        XCTAssertEqual(status.record?.sha256.count, 64)
    }

    func testTestStatusDecodesPolicyEvidenceAndSupplementalFlags() throws {
        let json = """
        {
          "project_id": "prj-1",
          "session_id": "ses-1",
          "policy": {
            "enabled": true,
            "default_profile": "test",
            "profiles": [
              {"name": "test", "kind": "test", "command": "cargo test", "timeout_secs": 900}
            ],
            "timeout_secs": 900,
            "output_cap_bytes": 65536
          },
          "evidence": [
            {
              "id": "ev-1",
              "project_id": "prj-1",
              "session_id": "ses-1",
              "profile": "test",
              "kind": "test",
              "host": "build.example.com",
              "commit": "abcdef1234567890",
              "command": "cargo test",
              "status": "passed",
              "exit_code": 0,
              "duration_ms": 42,
              "output_ref": "remote-test-outputs/x.txt",
              "output_sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
              "output_bytes": 100,
              "output_truncated": false,
              "timed_out": false,
              "digest": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
              "environment_caveat": "supplemental",
              "created_at": "2026-11-12T00:00:00Z"
            }
          ],
          "total": 1,
          "passed": 1,
          "failed": 0,
          "timed_out": 0,
          "latest_digest": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
          "environment_caveat": "supplemental only",
          "local_verifier_required": true,
          "merge_gate_unchanged": true
        }
        """
        let status = try JSONDecoder().decode(
            RemoteTestStatusResult.self,
            from: Data(json.utf8)
        )
        XCTAssertTrue(status.policy.enabled)
        XCTAssertEqual(status.policy.defaultProfile, "test")
        XCTAssertEqual(status.policy.profiles.first?.command, "cargo test")
        XCTAssertEqual(status.evidence.first?.digest.count, 64)
        XCTAssertTrue(status.localVerifierRequired)
        XCTAssertTrue(status.mergeGateUnchanged)
    }

    func testRemoteQueueEntryDecodesSupplementalEvidenceFields() throws {
        let json = """
        {
          "entry_id": "entry-1",
          "session_id": "ses-1",
          "status": "queued",
          "commit": "abcdef1234567890",
          "branch": "verlinpark/remote",
          "remote_host": "build.example.com",
          "remote_branch": "feature/x",
          "attempt": 1,
          "remote_test_evidence_id": "ev-1",
          "remote_test_digest": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
          "remote_test_status": "passed",
          "created_at": "2026-11-12T00:00:00Z",
          "updated_at": "2026-11-12T00:00:01Z"
        }
        """
        let entry = try JSONDecoder().decode(
            RemoteIntegrationQueueEntry.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(entry.remoteTestEvidenceId, "ev-1")
        XCTAssertEqual(entry.remoteTestStatus, "passed")
        XCTAssertEqual(entry.remoteTestDigest?.count, 64)
    }

    func testLegacyStatusWithoutEvidenceStillDecodes() throws {
        let json = """
        {
          "project_id": "prj-1",
          "configured": false,
          "install_path": "~/.verlinpark/bin/vp-remote-agent",
          "configured_helper_path": "vp-remote-agent",
          "path_matches_config": false,
          "risk": "host_state",
          "approval_required": true,
          "environment_caveat": "trusted"
        }
        """
        let status = try JSONDecoder().decode(
            RemoteBootstrapStatusResult.self,
            from: Data(json.utf8)
        )
        XCTAssertFalse(status.configured)
        XCTAssertNil(status.record)
    }
}
