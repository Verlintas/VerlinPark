@testable import VerlinPark
import XCTest

final class RemoteIntegrationTests: XCTestCase {
    func testPolicyDecodesWithSafeDefaults() throws {
        let policy = try JSONDecoder().decode(
            RemoteIntegrationPolicy.self,
            from: Data("{}".utf8)
        )
        XCTAssertFalse(policy.enabled)
        XCTAssertTrue(policy.requireVerifier)
        XCTAssertEqual(policy.maxBundleBytes, 256 * 1024 * 1024)
        XCTAssertTrue(policy.allowedHosts.isEmpty)
        XCTAssertFalse(policy.keepImportRefs)
    }

    func testStatusDecodesRemoteEntriesAndCounts() throws {
        let json = """
        {
          "project_id": "p1",
          "session_id": "s1",
          "policy": {
            "enabled": true,
            "max_bundle_bytes": 1048576,
            "require_verifier": true,
            "allowed_hosts": ["build.example.com"],
            "keep_import_refs": false
          },
          "policy_updated_at": "2026-11-11T00:00:00Z",
          "entries": [
            {
              "entry_id": "e1",
              "session_id": "s1",
              "status": "conflict",
              "commit": "abcdef1234567890",
              "branch": "verlinpark/s1--remote-x",
              "remote_host": "build.example.com",
              "remote_branch": "feature/x",
              "bundle_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
              "imported_ref": "refs/verlinpark/remote/s1/x",
              "verifier_approved": true,
              "verifier_summary": "looks correct",
              "environment_caveat": "local only",
              "attempt": 2,
              "detail": "merge conflict",
              "created_at": "2026-11-11T00:00:00Z",
              "updated_at": "2026-11-11T00:01:00Z"
            }
          ],
          "total": 1,
          "pending": 0,
          "merged": 0,
          "conflicts": 1,
          "failed": 0,
          "retained_refs": 1,
          "verifier_required": true,
          "environment_caveat": "local only"
        }
        """
        let status = try JSONDecoder().decode(
            RemoteIntegrationStatusResult.self,
            from: Data(json.utf8)
        )
        XCTAssertTrue(status.policy.enabled)
        XCTAssertEqual(status.entries.count, 1)
        XCTAssertEqual(status.entries[0].remoteBranch, "feature/x")
        XCTAssertEqual(status.entries[0].attempt, 2)
        XCTAssertEqual(status.entries[0].verifierApproved, true)
        XCTAssertEqual(status.entries[0].verifierSummary, "looks correct")
        XCTAssertEqual(status.retainedRefs, 1)
        XCTAssertEqual(status.conflicts, 1)
    }

    func testPolicyDecodesExplicitAllowlistAndRetention() throws {
        let json = """
        {
          "enabled": true,
          "max_bundle_bytes": 4096,
          "require_verifier": false,
          "allowed_hosts": ["agent@build.example.com"],
          "keep_import_refs": true
        }
        """
        let policy = try JSONDecoder().decode(
            RemoteIntegrationPolicy.self,
            from: Data(json.utf8)
        )
        XCTAssertTrue(policy.enabled)
        XCTAssertFalse(policy.requireVerifier)
        XCTAssertEqual(policy.maxBundleBytes, 4096)
        XCTAssertEqual(policy.allowedHosts, ["agent@build.example.com"])
        XCTAssertTrue(policy.keepImportRefs)
        let encoded = try JSONEncoder().encode(policy)
        let decoded = try JSONDecoder().decode(RemoteIntegrationPolicy.self, from: encoded)
        XCTAssertEqual(decoded, policy)
    }

    func testCleanupResultDecodesRemovedAndSkippedRefs() throws {
        let json = """
        {
          "project_id": "p1",
          "session_id": null,
          "removed_refs": ["refs/verlinpark/remote/s/i"],
          "removed_worktrees": [],
          "skipped": ["e2: retryable entry (conflict) requires force"],
          "entries_examined": 2
        }
        """
        let result = try JSONDecoder().decode(
            RemoteIntegrationCleanupResult.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(result.removedRefs.count, 1)
        XCTAssertEqual(result.entriesExamined, 2)
        XCTAssertEqual(result.skipped.count, 1)
    }

    func testLegacyMergeQueueEntryDecodesWithoutRemoteFields() throws {
        let json = """
        {
          "id": "e1",
          "session_id": "s1",
          "agent_id": "a1",
          "agent_name": "Worker",
          "task_id": "T1",
          "branch": "verlinpark/s1--T1",
          "commit": "abcdef1234567890",
          "base_commit": "1234567890abcdef",
          "status": "merged",
          "attempt": 1,
          "created_at": "2026-11-11T00:00:00Z",
          "updated_at": "2026-11-11T00:00:00Z"
        }
        """
        let entry = try JSONDecoder().decode(
            MergeQueueEntry.self,
            from: Data(json.utf8)
        )
        XCTAssertFalse(entry.isRemote)
        XCTAssertNil(entry.remoteHost)
        XCTAssertNil(entry.importedRef)
    }
}
