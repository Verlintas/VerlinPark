@testable import VerlinPark
import XCTest

final class ExternalGateTests: XCTestCase {
    private let decoder = JSONDecoder()

    func testGateListDecodesWithoutTokenOrVerifier() throws {
        let json = try XCTUnwrap("""
        {
          "session_id": "ses-1",
          "truncated": false,
          "counts": {"total": 2, "open": 1, "resolved": 1, "expired": 0, "cancelled": 0},
          "gates": [
            {
              "id": "gate-1",
              "session_id": "ses-1",
              "task_id": "T1",
              "kind": "ci",
              "label": "unit tests",
              "state": "open",
              "created_at": "2026-11-12T00:00:00Z",
              "expires_at": "2026-11-12T01:00:00Z",
              "payload": {"workflow": "ci"},
              "token_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            },
            {
              "id": "gate-2",
              "session_id": "ses-1",
              "kind": "human",
              "label": "release approval",
              "state": "resolved",
              "outcome": "resolved",
              "resolution_note": "approved by operator",
              "created_at": "2026-11-12T00:00:00Z"
            }
          ]
        }
        """.data(using: .utf8))

        let report = try decoder.decode(SessionGatesReport.self, from: json)
        XCTAssertEqual(report.sessionID, "ses-1")
        XCTAssertEqual(report.counts.total, 2)
        XCTAssertEqual(report.counts.open, 1)
        XCTAssertEqual(report.openGates.count, 1)
        XCTAssertEqual(report.gates[0].stateLabel, "OPEN")
        XCTAssertEqual(report.gates[0].kind, "ci")
        XCTAssertEqual(report.gates[1].outcome, "resolved")
        XCTAssertEqual(report.gates[1].resolutionNote, "approved by operator")
        XCTAssertFalse(String(describing: report).contains("token"))
        XCTAssertFalse(String(describing: report).contains("aaaaaaaa"))
    }

    func testLegacyGateListDefaultsToEmpty() throws {
        let json = try XCTUnwrap("""
        {"session_id": "ses-legacy"}
        """.data(using: .utf8))
        let report = try decoder.decode(SessionGatesReport.self, from: json)
        XCTAssertEqual(report.sessionID, "ses-legacy")
        XCTAssertTrue(report.gates.isEmpty)
        XCTAssertEqual(report.counts.total, 0)
        XCTAssertFalse(report.truncated)
        XCTAssertTrue(report.openGates.isEmpty)
    }

    func testGateViewDefaultsStaySafe() throws {
        let json = try XCTUnwrap("""
        {"id": "gate-1", "session_id": "ses-1"}
        """.data(using: .utf8))
        let gate = try decoder.decode(ExternalGateView.self, from: json)
        XCTAssertEqual(gate.kind, "custom")
        XCTAssertEqual(gate.state, "open")
        XCTAssertTrue(gate.isOpen)
        XCTAssertNil(gate.outcome)
        XCTAssertNil(gate.taskID)
        XCTAssertNil(gate.expiresAt)
    }

    func testCancelledAndExpiredGatesAreNotOpen() throws {
        let json = try XCTUnwrap("""
        {
          "session_id": "ses-1",
          "counts": {"total": 2, "open": 0, "resolved": 0, "expired": 1, "cancelled": 1},
          "gates": [
            {"id": "gate-1", "session_id": "ses-1", "kind": "ci", "label": "a", "state": "expired"},
            {"id": "gate-2", "session_id": "ses-1", "kind": "human", "label": "b", "state": "cancelled"}
          ]
        }
        """.data(using: .utf8))
        let report = try decoder.decode(SessionGatesReport.self, from: json)
        XCTAssertTrue(report.openGates.isEmpty)
        XCTAssertEqual(report.counts.expired, 1)
        XCTAssertEqual(report.counts.cancelled, 1)
        XCTAssertEqual(report.gates[0].stateLabel, "EXPIRED")
        XCTAssertEqual(report.gates[1].stateLabel, "CANCELLED")
    }
}
