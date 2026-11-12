@testable import VerlinPark
import XCTest

final class AgentIdentityTests: XCTestCase {
    private let decoder = JSONDecoder()

    func testProjectIdentityRosterDecodesReadOnlyFields() throws {
        let json = try XCTUnwrap("""
        {
          "identities": [
            {
              "id": "identity-1",
              "project_id": "prj-1",
              "role": "manager",
              "department": "platform",
              "component": null,
              "title": "Platform Lead",
              "profile": "generic",
              "model": null,
              "parent_identity_id": "leader-1",
              "status": "paused",
              "generation": 3,
              "created_at": "2026-11-12T00:00:00Z",
              "updated_at": "2026-11-12T00:05:00Z",
              "retired_at": null,
              "successor_id": null,
              "origin": "local"
            },
            {
              "id": "identity-2",
              "project_id": "prj-1",
              "role": "worker",
              "status": "retired",
              "generation": 1,
              "created_at": "2026-11-12T00:00:00Z",
              "updated_at": "2026-11-12T00:06:00Z",
              "retired_at": "2026-11-12T00:06:00Z",
              "origin": "imported"
            }
          ]
        }
        """.data(using: .utf8))

        let result = try decoder.decode(AgentIdentitiesResult.self, from: json)
        XCTAssertEqual(result.identities.count, 2)
        let manager = result.identities[0]
        XCTAssertEqual(manager.projectId, "prj-1")
        XCTAssertEqual(manager.displayName, "Platform Lead")
        XCTAssertEqual(manager.generation, 3)
        XCTAssertEqual(manager.status, "paused")
        XCTAssertFalse(manager.isImported)
        XCTAssertNil(manager.component)
        XCTAssertNil(manager.model)
        let worker = result.identities[1]
        XCTAssertEqual(worker.displayName, "Worker")
        XCTAssertTrue(worker.isImported)
        XCTAssertNotNil(worker.retiredAt)
        XCTAssertNil(worker.title)
    }

    func testLegacyAgentRecordWithoutIdentityStillDecodes() throws {
        let json = try XCTUnwrap("""
        {
          "id": "run-1",
          "session_id": "ses-1",
          "role": "leader",
          "name": "Leader",
          "status": "running",
          "model": "test-model",
          "created_at": "2026-11-12T00:00:00Z"
        }
        """.data(using: .utf8))
        let record = try decoder.decode(AgentRecord.self, from: json)
        XCTAssertNil(record.identityId)
        XCTAssertNil(record.parentId)
        XCTAssertNil(record.profile)
    }

    func testIdentityRunLinkDecodesWhenPresent() throws {
        let json = try XCTUnwrap("""
        {
          "id": "run-1",
          "session_id": "ses-1",
          "identity_id": "identity-1",
          "role": "worker",
          "name": "Worker",
          "status": "running",
          "model": "test-model",
          "created_at": "2026-11-12T00:00:00Z"
        }
        """.data(using: .utf8))
        let record = try decoder.decode(AgentRecord.self, from: json)
        XCTAssertEqual(record.identityId, "identity-1")
    }

    func testAgentMemoryRowsDecodeReadOnlyScopeAndEvidence() throws {
        let json = try XCTUnwrap("""
        {
          "memories": [
            {
              "id": "mem-1",
              "project_id": "prj-1",
              "identity_id": "identity-1",
              "department": "Platform",
              "component": "auth",
              "scope": "agent",
              "content": "The auth component owns JWT validation.",
              "content_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
              "evidence": [
                {"kind": "task", "reference": "T1", "note": null},
                {"kind": "commit", "reference": "abcdef123456", "note": "reviewed"}
              ],
              "source": "agent",
              "status": "active",
              "pinned": true,
              "created_at": "2026-11-13T00:00:00Z",
              "updated_at": "2026-11-13T00:05:00Z",
              "expires_at": null,
              "verified_at": "2026-11-13T00:06:00Z",
              "superseded_by": null,
              "stale_reason": null,
              "origin": "local",
              "expired": false
            },
            {
              "id": "mem-2",
              "project_id": "prj-1",
              "scope": "project",
              "content": "A project fact.",
              "source": "operator",
              "status": "stale",
              "stale_reason": "forgotten",
              "origin": "imported",
              "expired": true
            }
          ],
          "total": 2
        }
        """.data(using: .utf8))
        let result = try decoder.decode(AgentMemoryListResult.self, from: json)
        XCTAssertEqual(result.total, 2)
        XCTAssertEqual(result.memories.count, 2)
        let agent = result.memories[0]
        XCTAssertEqual(agent.scope, "agent")
        XCTAssertEqual(agent.identityId, "identity-1")
        XCTAssertEqual(agent.evidenceLabelParts, ["task T1", "commit abcdef12"])
        XCTAssertTrue(agent.isActive)
        XCTAssertFalse(agent.isImported)
        XCTAssertEqual(agent.statusDetail, "verified 2026-11-13T00:06:00Z")
        let imported = result.memories[1]
        XCTAssertEqual(imported.effectiveStatus, "expired")
        XCTAssertTrue(imported.isImported)
        XCTAssertFalse(imported.isActive)
        XCTAssertEqual(imported.statusDetail, "forgotten")
    }
}
