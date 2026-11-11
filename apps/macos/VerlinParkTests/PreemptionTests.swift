import Foundation
@testable import VerlinPark
import XCTest

final class PreemptionTests: XCTestCase {
    func testPolicyDefaultsAndBackwardCompatibleDecode() throws {
        let policy = try JSONDecoder().decode(
            PreemptionPolicy.self,
            from: Data("{}".utf8)
        )
        XCTAssertTrue(policy.enabled)
        XCTAssertEqual(policy.providerGraceMs, 2000)
        XCTAssertEqual(policy.toolGraceMs, 5000)
        XCTAssertEqual(policy.killEscalationMs, 2000)
        XCTAssertNil(policy.toolDeadlineSecs)
        XCTAssertTrue(policy.checkpointOnPreempt)
    }

    func testSessionPreemptionResultDecodesRecordsAndCounts() throws {
        let json = """
        {
          "session_id": "ses-1",
          "policy": {
            "enabled": true,
            "provider_grace_ms": 750,
            "tool_grace_ms": 1500,
            "kill_escalation_ms": 500,
            "tool_deadline_secs": 30,
            "checkpoint_on_preempt": false
          },
          "active": false,
          "preempted": true,
          "records": [
            {
              "session_id": "ses-1",
              "project_id": "proj-1",
              "agent_id": "leader-1",
              "agent_name": "Leader",
              "agent_role": "leader",
              "reason": "wall_clock",
              "phase": "provider",
              "elapsed_ms": 1005,
              "side_effects_unknown": false,
              "partial_input_tokens": 40,
              "partial_output_tokens": 5,
              "partial_output_bytes": 14,
              "usage_estimated": false,
              "checkpoint_id": "cp-1",
              "task_checkpoint_commit": "abc123",
              "terminal": true,
              "created_at": "2026-11-12T00:00:00Z"
            }
          ],
          "by_phase": [{"key": "provider", "count": 1}],
          "by_reason": [{"key": "wall_clock", "count": 1}]
        }
        """
        let result = try JSONDecoder().decode(
            SessionPreemptionResult.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(result.sessionId, "ses-1")
        XCTAssertEqual(result.policy.providerGraceMs, 750)
        XCTAssertEqual(result.policy.toolDeadlineSecs, 30)
        XCTAssertFalse(result.policy.checkpointOnPreempt)
        XCTAssertFalse(result.active)
        XCTAssertTrue(result.preempted)
        XCTAssertEqual(result.records.count, 1)
        let record = try XCTUnwrap(result.records.first)
        XCTAssertEqual(record.reason, "wall_clock")
        XCTAssertEqual(record.phase, "provider")
        XCTAssertEqual(record.elapsedMs, 1005)
        XCTAssertEqual(record.partialInputTokens, 40)
        XCTAssertTrue(record.terminal)
        XCTAssertEqual(record.checkpointId, "cp-1")
        XCTAssertEqual(record.taskCheckpointCommit, "abc123")
        XCTAssertEqual(result.byPhase.first?.key, "provider")
        XCTAssertEqual(result.byPhase.first?.count, 1)
        XCTAssertEqual(result.byReason.first?.key, "wall_clock")
    }

    func testPreemptionRecordDefaultsForToolDeadlineSubEvent() throws {
        let json = """
        {
          "session_id": "ses-1",
          "reason": "tool_deadline",
          "phase": "tool",
          "tool": "shell",
          "side_effects_unknown": true,
          "terminal": false
        }
        """
        let record = try JSONDecoder().decode(
            PreemptionRecord.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(record.reason, "tool_deadline")
        XCTAssertEqual(record.phase, "tool")
        XCTAssertEqual(record.tool, "shell")
        XCTAssertTrue(record.sideEffectsUnknown)
        XCTAssertFalse(record.terminal)
        XCTAssertEqual(record.elapsedMs, 0)
        XCTAssertTrue(record.summary.contains("side effects unknown"))
    }

    func testSessionPreemptResultDecodesAndDefaults() throws {
        let result = try JSONDecoder().decode(
            SessionPreemptResult.self,
            from: Data(
                """
                {"session_id":"ses-1","preempted":true,"reason":"explicit","detail":"signalled"}
                """.utf8
            )
        )
        XCTAssertEqual(result.sessionId, "ses-1")
        XCTAssertTrue(result.preempted)
        XCTAssertEqual(result.reason, "explicit")
        XCTAssertEqual(result.detail, "signalled")

        let idle = try JSONDecoder().decode(
            SessionPreemptResult.self,
            from: Data("{}".utf8)
        )
        XCTAssertFalse(idle.preempted)
        XCTAssertEqual(idle.reason, "")
    }

    func testPreemptionOverviewDecodesProjectSummary() throws {
        let json = """
        {
          "project_id": "proj-1",
          "policy": {},
          "total": 3,
          "sessions": [
            {
              "session_id": "ses-1",
              "goal": "ship",
              "status": "cancelled",
              "terminal_count": 2,
              "last_reason": "wall_clock",
              "last_phase": "provider",
              "last_at": "2026-11-12T00:00:00Z"
            }
          ],
          "by_phase": [{"key": "provider", "count": 2}],
          "by_reason": [{"key": "wall_clock", "count": 2}, {"key": "explicit", "count": 1}]
        }
        """
        let overview = try JSONDecoder().decode(
            PreemptionOverviewResult.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(overview.projectId, "proj-1")
        XCTAssertEqual(overview.total, 3)
        XCTAssertTrue(overview.policy.enabled)
        XCTAssertEqual(overview.sessions.count, 1)
        let session = try XCTUnwrap(overview.sessions.first)
        XCTAssertEqual(session.sessionId, "ses-1")
        XCTAssertEqual(session.terminalCount, 2)
        XCTAssertEqual(session.lastReason, "wall_clock")
        XCTAssertEqual(overview.byReason.count, 2)
    }

    func testPreemptionPolicyRoundTripEncodingKeepsSnakeCaseKeys() throws {
        var policy = PreemptionPolicy()
        policy.enabled = false
        policy.providerGraceMs = 100
        policy.toolDeadlineSecs = 60
        let encoded = try JSONEncoder().encode(policy)
        let object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: encoded) as? [String: Any]
        )
        XCTAssertEqual(object["provider_grace_ms"] as? Int, 100)
        XCTAssertEqual(object["tool_deadline_secs"] as? Int, 60)
        XCTAssertEqual(object["checkpoint_on_preempt"] as? Bool, true)
        XCTAssertNil(object["providerGraceMs"])
    }
}
