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
}
