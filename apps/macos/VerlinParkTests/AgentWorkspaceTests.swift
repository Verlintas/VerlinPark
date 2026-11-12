@testable import VerlinPark
import XCTest

final class AgentWorkspaceTests: XCTestCase {
    private let decoder = JSONDecoder()

    func testWorkspaceListDecodesLeaseAndLegacyDefaults() throws {
        let json = try XCTUnwrap("""
        {
          "workspaces": [
            {
              "id": "wsp-1",
              "project_id": "prj-1",
              "identity_id": "identity-1",
              "component": "auth",
              "branch": "verlinpark/components/auth-1234--identity-1",
              "base_commit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
              "worktree_path": "/data/workspaces/prj-1/components/identity-1",
              "status": "active",
              "created_at": "2026-11-13T00:00:00Z",
              "updated_at": "2026-11-13T00:05:00Z",
              "last_session_id": "ses-1",
              "lease_active": true,
              "lease_expires_at": "2026-11-13T01:00:00Z",
              "origin": "local"
            },
            {
              "id": "wsp-2",
              "project_id": "prj-1",
              "identity_id": "identity-2",
              "component": "docs",
              "branch": "verlinpark/components/docs-5678--identity-2",
              "status": "archived"
            }
          ],
          "sessions": [
            {
              "id": "sww-1",
              "workspace_id": "wsp-1",
              "session_id": "ses-1",
              "identity_id": "identity-1",
              "worktree_path": "/data/workspaces/prj-1/sessions/ses-1/identity-1",
              "branch": "verlinpark/sessions/ses-1/worker-identity-1",
              "base_commit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
              "status": "active",
              "lease_active": true
            }
          ]
        }
        """.data(using: .utf8))

        let result = try decoder.decode(AgentWorkspaceListResult.self, from: json)
        XCTAssertEqual(result.workspaces.count, 2)
        XCTAssertEqual(result.sessions.count, 1)
        XCTAssertTrue(result.workspaces[0].leaseActive)
        XCTAssertEqual(result.workspaces[0].lastSessionId, "ses-1")
        XCTAssertFalse(result.workspaces[0].isImported)
        XCTAssertEqual(result.workspaces[1].status, "archived")
        XCTAssertEqual(result.workspaces[1].origin, "local")
        XCTAssertEqual(result.sessions[0].workspaceId, "wsp-1")
    }

    func testSessionWorkspaceAndImportedOriginDecode() throws {
        let json = try XCTUnwrap("""
        {
          "session_id": "ses-9",
          "bindings": [
            {
              "id": "sww-9",
              "workspace_id": "wsp-9",
              "session_id": "ses-9",
              "identity_id": "identity-9",
              "branch": "verlinpark/sessions/ses-9/worker-identity-9",
              "status": "released"
            }
          ]
        }
        """.data(using: .utf8))
        let result = try decoder.decode(SessionWorkspaceResult.self, from: json)
        XCTAssertEqual(result.sessionId, "ses-9")
        XCTAssertEqual(result.bindings.count, 1)
        XCTAssertEqual(result.bindings[0].status, "released")
        XCTAssertFalse(result.bindings[0].leaseActive)
        XCTAssertTrue(result.bindings[0].worktreePath.isEmpty)

        let imported: AgentWorkspace = try decoder.decode(AgentWorkspace.self, from: XCTUnwrap("""
        {
          "id": "wsp-imported",
          "project_id": "prj-1",
          "identity_id": "identity-1",
          "component": "auth",
          "branch": "verlinpark/components/auth-1234--identity-1",
          "status": "archived",
          "origin": "imported"
        }
        """.data(using: .utf8)))
        XCTAssertTrue(imported.isImported)
        XCTAssertFalse(imported.leaseActive)
    }
}
