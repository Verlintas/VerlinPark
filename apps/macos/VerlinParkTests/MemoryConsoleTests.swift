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

    func testMemoryConflictDecodesScopeAndExcerpts() throws {
        let conflict = try JSONDecoder().decode(
            MemoryConflict.self,
            from: Data(
                """
                {
                  "id": "conflict-1234567890",
                  "project_id": "project-1",
                  "session_id": "session-1",
                  "memory_a_id": "memory-a",
                  "memory_b_id": "memory-b",
                  "memory_a_title": "Parser uses tabs",
                  "memory_b_title": "Parser uses spaces",
                  "memory_a_kind": "convention",
                  "memory_b_kind": "decision",
                  "memory_a_excerpt": "Indent parser files with tabs.",
                  "memory_b_excerpt": "Indent parser files with four spaces.",
                  "scope_kind": "path",
                  "scope_value": "src/parser.rs",
                  "reason": "Both active memories cite path src/parser.rs and have different bodies.",
                  "status": "open",
                  "detected_at": "2026-11-11T03:00:00Z",
                  "resolved_at": null,
                  "resolution": null,
                  "note": null
                }
                """.utf8
            )
        )
        XCTAssertEqual(conflict.scopeLabel, "path src/parser.rs")
        XCTAssertEqual(conflict.initial, "conflict")
        XCTAssertTrue(conflict.isOpen)
        XCTAssertEqual(conflict.memoryAKind, "convention")
        XCTAssertEqual(conflict.memoryBExcerpt, "Indent parser files with four spaces.")
        XCTAssertEqual(conflict.reason.contains("different bodies"), true)
    }

    func testMemoryHealthAndConflictScanDecode() throws {
        let health = try JSONDecoder().decode(
            MemoryHealthResult.self,
            from: Data(
                """
                {
                  "project_id": "project-1",
                  "total": 9,
                  "active": 5,
                  "stale": 2,
                  "superseded": 1,
                  "archived": 1,
                  "pinned": 2,
                  "evidence_covered": 4,
                  "verified": 3,
                  "verification_due": 2,
                  "open_conflicts": 1,
                  "by_kind": [{"key": "fact", "count": 4}]
                }
                """.utf8
            )
        )
        XCTAssertEqual(health.active, 5)
        XCTAssertEqual(health.evidenceCovered, 4)
        XCTAssertEqual(health.verificationDue, 2)
        XCTAssertEqual(health.openConflicts, 1)
        XCTAssertEqual(health.byKind.first?.count, 4)

        let scan = try JSONDecoder().decode(
            MemoryConflictScanResult.self,
            from: Data(
                """
                {
                  "project_id": "project-1",
                  "detected": 2,
                  "open": 3,
                  "conflicts": []
                }
                """.utf8
            )
        )
        XCTAssertEqual(scan.detected, 2)
        XCTAssertEqual(scan.open, 3)
        XCTAssertTrue(scan.conflicts.isEmpty)
    }

    func testMemoryConflictResolveResultDecodesUpdatedMemories() throws {
        let result = try JSONDecoder().decode(
            MemoryConflictResolveResult.self,
            from: Data(
                """
                {
                  "conflict": {
                    "id": "conflict-1",
                    "project_id": "project-1",
                    "session_id": "session-1",
                    "memory_a_id": "memory-a",
                    "memory_b_id": "memory-b",
                    "memory_a_title": "Keep A",
                    "memory_b_title": "Lose B",
                    "memory_a_kind": "fact",
                    "memory_b_kind": "fact",
                    "memory_a_excerpt": "A body",
                    "memory_b_excerpt": "B body",
                    "scope_kind": "task",
                    "scope_value": "T3",
                    "reason": "Both active memories cite task T3 and have different bodies.",
                    "status": "resolved",
                    "detected_at": "2026-11-11T03:00:00Z",
                    "resolved_at": "2026-11-11T04:00:00Z",
                    "resolution": "keep_a",
                    "note": "A matches the merged commit."
                  },
                  "memories": [
                    {
                      "id": "memory-a",
                      "session_id": "session-1",
                      "project_id": "project-1",
                      "agent_id": "agent-1",
                      "agent_name": "Leader",
                      "kind": "fact",
                      "title": "Keep A",
                      "body": "A body",
                      "tags": [],
                      "created_at": "2026-11-11T01:00:00Z",
                      "status": "active"
                    },
                    {
                      "id": "memory-b",
                      "session_id": "session-1",
                      "project_id": "project-1",
                      "agent_id": "agent-1",
                      "agent_name": "Leader",
                      "kind": "fact",
                      "title": "Lose B",
                      "body": "B body",
                      "tags": [],
                      "created_at": "2026-11-11T01:00:00Z",
                      "status": "superseded",
                      "superseded_by": "memory-a"
                    }
                  ]
                }
                """.utf8
            )
        )
        XCTAssertEqual(result.conflict.status, "resolved")
        XCTAssertEqual(result.conflict.resolution, "keep_a")
        XCTAssertFalse(result.conflict.isOpen)
        XCTAssertEqual(result.memories.count, 2)
        XCTAssertEqual(result.memories[1].displayStatus, "superseded")
        XCTAssertEqual(result.memories[1].supersededBy, "memory-a")
    }

    func testMemoryConflictResolutionMapsToRPCStringsAndHelpers() {
        XCTAssertEqual(MemoryConflictResolution.keepA.rawValue, "keep_a")
        XCTAssertEqual(MemoryConflictResolution.keepB.rawValue, "keep_b")
        XCTAssertEqual(MemoryConflictResolution.keepBoth.rawValue, "keep_both")
        XCTAssertEqual(MemoryConflictResolution.archiveBoth.rawValue, "archive_both")
        XCTAssertEqual(MemoryConflictResolution.keepA.title, "Keep A")
        XCTAssertTrue(MemoryConflictResolution.keepA.detail.contains("Supersede B"))
        XCTAssertTrue(MemoryConflictResolution.keepBoth.detail.contains("without changing"))
        XCTAssertFalse(MemoryConflictResolution.keepBoth.isDestructive)
        XCTAssertTrue(MemoryConflictResolution.archiveBoth.isDestructive)
        XCTAssertTrue(MemoryConflictResolution.archiveBoth.detail.contains("Archive both"))
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
