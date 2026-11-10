import Foundation
@testable import VerlinPark
import XCTest

final class MemoryConsoleTests: XCTestCase {
    func testMemoryItemDecodesEvidenceAndLifecycle() throws {
        let json = """
        {
          "id": "memory-1",
          "session_id": "session-1",
          "project_id": "project-1",
          "agent_id": "orchestrator",
          "agent_name": "Orchestrator",
          "kind": "task_result",
          "title": "Task T1 result",
          "body": "Implemented the parser and tests.",
          "tags": ["T1", "task_result"],
          "created_at": "2026-11-11T01:00:00Z",
          "status": "active",
          "source": "task_result",
          "evidence": [
            {"kind": "task", "reference": "T1"},
            {"kind": "commit", "reference": "0123456789abcdef"},
            {"kind": "path", "reference": "src/parser.rs", "note": "changed"}
          ],
          "supersedes": ["memory-0"],
          "superseded_by": null,
          "pinned": true,
          "use_count": 3,
          "last_used_at": "2026-11-11T02:00:00Z",
          "last_verified_at": "2026-11-11T02:10:00Z",
          "verify_after": "2027-02-09T02:10:00Z",
          "stale_reason": null
        }
        """
        let memory = try JSONDecoder().decode(MemoryItem.self, from: Data(json.utf8))
        XCTAssertEqual(memory.displayStatus, "active")
        XCTAssertEqual(memory.displaySource, "task_result")
        XCTAssertTrue(memory.isPinned)
        XCTAssertEqual(memory.evidenceCount, 3)
        XCTAssertEqual(memory.useCount, 3)
        XCTAssertEqual(memory.lastVerifiedAt, "2026-11-11T02:10:00Z")
        XCTAssertEqual(memory.verifyAfter, "2027-02-09T02:10:00Z")
        XCTAssertTrue(memory.isActive)
        XCTAssertEqual(memory.evidenceLabelParts.count, 3)
        XCTAssertTrue(memory.evidenceLabelParts[1].contains("01234567"))
        XCTAssertEqual(memory.statusDetail?.contains("verify after"), true)
    }

    func testMemoryItemDecodesLegacyShapeWithDefaults() throws {
        let json = """
        {
          "id": "memory-old",
          "session_id": "session-1",
          "project_id": "project-1",
          "agent_id": "agent-1",
          "agent_name": "Leader",
          "kind": "decision",
          "title": "Use SQLite",
          "body": "The daemon stores projections in SQLite.",
          "tags": [],
          "created_at": "2026-01-01T00:00:00Z"
        }
        """
        let memory = try JSONDecoder().decode(MemoryItem.self, from: Data(json.utf8))
        XCTAssertEqual(memory.displayStatus, "active")
        XCTAssertEqual(memory.displaySource, "agent")
        XCTAssertFalse(memory.isPinned)
        XCTAssertEqual(memory.evidenceCount, 0)
        XCTAssertEqual(memory.useCount, nil)
        XCTAssertTrue(memory.isActive)
        XCTAssertTrue(memory.evidenceLabelParts.isEmpty)
    }

    func testStaleMemoryPrefersReasonOverVerificationDeadline() throws {
        let json = """
        {
          "id": "memory-stale",
          "session_id": "session-1",
          "project_id": "project-1",
          "agent_id": "agent-1",
          "agent_name": "Leader",
          "kind": "fact",
          "title": "Old fact",
          "body": "body",
          "tags": [],
          "created_at": "2026-01-01T00:00:00Z",
          "status": "stale",
          "source": "agent",
          "stale_reason": "verification expired",
          "verify_after": "2026-02-01T00:00:00Z"
        }
        """
        let memory = try JSONDecoder().decode(MemoryItem.self, from: Data(json.utf8))
        XCTAssertEqual(memory.displayStatus, "stale")
        XCTAssertEqual(memory.statusDetail, "verification expired")
        XCTAssertFalse(memory.isActive)
    }

    func testMemoryPolicyDecodesVerificationControls() throws {
        let json = """
        {
          "project_id": "project-1",
          "policy": {
            "enabled": false,
            "max_items": 1000,
            "max_age_days": 180,
            "task_result_verify_days": 45,
            "default_verify_days": 300,
            "auto_supersede": false,
            "brief_max_chars": 2500,
            "sweep_stale": true
          }
        }
        """
        let result = try JSONDecoder().decode(MemoryPolicyResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.policy.taskResultVerifyDays, 45)
        XCTAssertEqual(result.policy.defaultVerifyDays, 300)
        XCTAssertEqual(result.policy.autoSupersede, false)
        XCTAssertEqual(result.policy.briefMaxChars, 2500)
        XCTAssertEqual(result.policy.sweepStale, true)
    }

    func testMemoryStatsAndSweepDecode() throws {
        let stats = try JSONDecoder().decode(
            MemoryStatsResult.self,
            from: Data(
                """
                {
                  "project_id": "project-1",
                  "total": 7,
                  "by_status": [{"key": "active", "count": 5}, {"key": "stale", "count": 2}],
                  "by_source": [{"key": "task_result", "count": 4}],
                  "by_kind": [{"key": "fact", "count": 3}]
                }
                """.utf8
            )
        )
        XCTAssertEqual(stats.total, 7)
        XCTAssertEqual(stats.byStatus.first { $0.key == "stale" }?.count, 2)
        XCTAssertEqual(stats.bySource.first?.key, "task_result")

        let sweep = try JSONDecoder().decode(
            MemorySweepResult.self,
            from: Data(
                """
                {
                  "project_id": "project-1",
                  "candidates": 2,
                  "marked_stale": 2,
                  "memory_ids": ["a", "b"],
                  "dry_run": false
                }
                """.utf8
            )
        )
        XCTAssertEqual(sweep.markedStale, 2)
        XCTAssertEqual(sweep.memoryIds, ["a", "b"])
        XCTAssertFalse(sweep.dryRun)
    }

    func testVerificationCheckDecodesUncheckedEvidence() throws {
        let result = try JSONDecoder().decode(
            MemoryVerifyResult.self,
            from: Data(
                """
                {
                  "memory": {
                    "id": "memory-1",
                    "session_id": "session-1",
                    "project_id": "project-1",
                    "agent_id": "agent-1",
                    "agent_name": "Leader",
                    "kind": "fact",
                    "title": "Docs claim",
                    "body": "body",
                    "tags": [],
                    "created_at": "2026-01-01T00:00:00Z"
                  },
                  "checks": [
                    {
                      "evidence": {"kind": "url", "reference": "https://example.com"},
                      "status": "unchecked",
                      "detail": "URL evidence is not fetched"
                    }
                  ]
                }
                """.utf8
            )
        )
        XCTAssertEqual(result.checks.count, 1)
        XCTAssertEqual(result.checks[0].status, "unchecked")
        XCTAssertEqual(result.checks[0].evidence.kind, "url")
    }
}
