@testable import VerlinPark
import XCTest

final class AgentCoordinationTests: XCTestCase {
    private let decoder = JSONDecoder()

    func testAssignmentsDecodeLeaseMetadataWithoutToken() throws {
        let json = try XCTUnwrap("""
        {
          "assignments": [
            {
              "session_id": "ses-1",
              "project_id": "prj-1",
              "identity_id": "identity-1",
              "role": "worker",
              "status": "active",
              "assigned_at": "2026-11-13T00:00:00Z",
              "released_at": null,
              "heartbeat_at": "2026-11-13T00:05:00Z",
              "lease_expires_at": "2026-11-13T00:15:00Z",
              "lease_active": true
            },
            {
              "session_id": "ses-2",
              "project_id": "prj-1",
              "identity_id": "identity-2",
              "role": "manager",
              "status": "released",
              "assigned_at": "2026-11-12T00:00:00Z",
              "released_at": "2026-11-12T01:00:00Z",
              "heartbeat_at": "2026-11-12T00:30:00Z"
            }
          ]
        }
        """.data(using: .utf8))

        let result = try decoder.decode(SessionAssignmentsResult.self, from: json)
        XCTAssertEqual(result.assignments.count, 2)
        let active = result.assignments[0]
        XCTAssertEqual(active.id, "ses-1#identity-1")
        XCTAssertTrue(active.isLive)
        XCTAssertNil(active.releasedAt)
        let released = result.assignments[1]
        XCTAssertFalse(released.isLive)
        XCTAssertFalse(released.leaseActive)
        XCTAssertNotNil(released.releasedAt)
        XCTAssertNil(released.leaseExpiresAt)
    }

    func testInboxDecodesCountsArbitraryPayloadAndWake() throws {
        let json = try XCTUnwrap("""
        {
          "messages": [
            {
              "id": "msg-1",
              "project_id": "prj-1",
              "identity_id": "identity-1",
              "session_id": "ses-1",
              "from_identity_id": null,
              "from_label": "operator",
              "kind": "handoff",
              "payload": {"summary": "review T1", "priority": 3, "tags": ["a"]},
              "status": "unread",
              "created_at": "2026-11-13T00:00:00Z",
              "read_at": null,
              "expires_at": "2026-11-13T01:00:00Z"
            }
          ],
          "counts": {"unread": 1, "read": 0, "done": 2, "expired": 0}
        }
        """.data(using: .utf8))

        let result = try decoder.decode(AgentInboxListResult.self, from: json)
        XCTAssertEqual(result.messages.count, 1)
        XCTAssertEqual(result.counts.unread, 1)
        XCTAssertEqual(result.counts.done, 2)
        let message = result.messages[0]
        XCTAssertTrue(message.isUnread)
        XCTAssertEqual(message.payload.displaySummary, "review T1")
        XCTAssertEqual(message.fromLabel, "operator")
        XCTAssertNil(message.fromIdentityId)
    }

    func testSendAndReadResultsDecodeAndKeepWakeMetadata() throws {
        let json = try XCTUnwrap("""
        {
          "message": {
            "id": "msg-1",
            "project_id": "prj-1",
            "identity_id": "identity-1",
            "from_label": "operator",
            "kind": "message",
            "payload": {"body": "ping"},
            "status": "unread",
            "created_at": "2026-11-13T00:00:00Z"
          },
          "wake": {
            "requested": true,
            "created": false,
            "continuation_id": null,
            "reason": "session_running"
          }
        }
        """.data(using: .utf8))
        let sent = try decoder.decode(AgentInboxSendResult.self, from: json)
        XCTAssertEqual(sent.message.payload.displaySummary, "ping")
        XCTAssertTrue(sent.wake.requested)
        XCTAssertFalse(sent.wake.created)
        XCTAssertEqual(sent.wake.reason, "session_running")
        XCTAssertNil(sent.wake.continuationId)

        let readJSON = try XCTUnwrap("""
        {
          "message": {
            "id": "msg-1",
            "project_id": "prj-1",
            "identity_id": "identity-1",
            "from_label": "operator",
            "kind": "message",
            "payload": {},
            "status": "read",
            "created_at": "2026-11-13T00:00:00Z",
            "read_at": "2026-11-13T00:10:00Z"
          },
          "changed": true
        }
        """.data(using: .utf8))
        let read = try decoder.decode(AgentInboxReadResult.self, from: readJSON)
        XCTAssertTrue(read.changed)
        XCTAssertEqual(read.message.status, "read")
        XCTAssertEqual(read.message.payload.displaySummary, "…")
    }
}
