@testable import VerlinPark
import XCTest

final class TeamTemplateTests: XCTestCase {
    private let decoder = JSONDecoder()

    private func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
        try decoder.decode(type, from: Data(json.utf8))
    }

    func testLibraryEntryDecodesAppliedAndReadOnlyMetadata() throws {
        let result = try decode(
            TeamTemplateListResult.self,
            """
            {
              "templates": [
                {
                  "template_id": "tt_abc",
                  "name": "Platform Team",
                  "version": "1.2.3",
                  "schema_version": 1,
                  "content_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                  "created_at": "2026-11-13T00:00:00Z",
                  "source_project": "source",
                  "applied": true,
                  "applied_at": "2026-11-13T01:00:00Z",
                  "applied_by": "operator",
                  "read_only": false
                },
                {
                  "template_id": "tt_import",
                  "name": "Imported",
                  "version": "1.0.0",
                  "content_hash": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                  "applied": false,
                  "read_only": true
                }
              ],
              "truncated": false
            }
            """
        )
        XCTAssertEqual(result.templates.count, 2)
        XCTAssertEqual(result.templates[0].templateID, "tt_abc")
        XCTAssertTrue(result.templates[0].applied)
        XCTAssertFalse(result.templates[0].readOnly)
        XCTAssertFalse(result.templates[1].applied)
        XCTAssertTrue(result.templates[1].readOnly)
        XCTAssertEqual(result.templates[1].schemaVersion, 1)
    }

    func testPreviewDecodesOrgChartConflictsAndGaps() throws {
        let preview = try decode(
            TeamTemplatePreview.self,
            """
            {
              "project_id": "prj-1",
              "template_id": "tt_abc",
              "name": "Platform Team",
              "version": "1.0.0",
              "content_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
              "valid": true,
              "apply_allowed": false,
              "requires_approval": true,
              "leader_overlay": true,
              "leader_identity_id": "leader-1",
              "departments": [{
                "name": "Platform",
                "objective": "Own platform",
                "manager": {
                  "slot": "manager:Platform:",
                  "role": "manager",
                  "title": "Platform Manager",
                  "profile": "generic",
                  "action": "conflict",
                  "existing_identity_id": "manager-1",
                  "conflict_reason": "existing manager"
                },
                "workers": [{
                  "slot": "worker:Platform:auth:Auth Worker",
                  "role": "worker",
                  "component": "auth",
                  "title": "Auth Worker",
                  "profile": "coding",
                  "action": "blocked"
                }]
              }],
              "proposed_org": ["Leader — Leader (existing overlay; never overwritten)"],
              "new_hire_slots": [],
              "existing_slots": ["leader"],
              "skipped_slots": [],
              "blocked_slots": ["worker:Platform:auth:Auth Worker"],
              "conflicts": [{
                "slot": "manager:Platform:",
                "code": "TEMPLATE_CONFLICT",
                "existing_identity_id": "manager-1",
                "proposed_title": "Platform Manager",
                "message": "a live manager already owns this department"
              }],
              "budget_changes": ["template defaults: unset / unset; project: 1000 / 1.00"],
              "model_gaps": ["worker -> unconfigured"],
              "tool_gaps": [],
              "policy_conflicts": ["template token cap would widen the project cap"],
              "warnings": ["project identity policy is disabled"]
            }
            """
        )
        XCTAssertTrue(preview.valid)
        XCTAssertFalse(preview.applyAllowed)
        XCTAssertTrue(preview.leaderOverlay)
        XCTAssertEqual(preview.departments.first?.manager.action, "conflict")
        XCTAssertEqual(preview.departments.first?.workers.first?.actionLabel, "Blocked")
        XCTAssertEqual(preview.conflicts.first?.existingIdentityID, "manager-1")
        XCTAssertEqual(preview.modelGaps.count, 1)
        XCTAssertEqual(preview.policyConflicts.count, 1)
    }

    func testImportResultDecodesPendingHireAndEntry() throws {
        let result = try decode(
            TeamTemplateImportResult.self,
            """
            {
              "project_id": "prj-1",
              "preview": {
                "project_id": "prj-1",
                "template_id": "tt_abc",
                "name": "Platform Team",
                "version": "1.0.0",
                "content_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "valid": true,
                "apply_allowed": true,
                "requires_approval": true,
                "leader_overlay": true,
                "departments": [],
                "proposed_org": [],
                "new_hire_slots": ["manager:Platform:"],
                "existing_slots": ["leader"],
                "skipped_slots": [],
                "blocked_slots": [],
                "conflicts": [],
                "budget_changes": [],
                "model_gaps": [],
                "tool_gaps": [],
                "policy_conflicts": [],
                "warnings": []
              },
              "applied": true,
              "changed": true,
              "entry": {
                "template_id": "tt_abc",
                "name": "Platform Team",
                "version": "1.0.0",
                "schema_version": 1,
                "content_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "applied": true,
                "read_only": false
              },
              "pending_hire_request_ids": ["hire-1"],
              "warnings": ["applied 1 pending hire request(s); no identity was created"]
            }
            """
        )
        XCTAssertTrue(result.applied)
        XCTAssertTrue(result.changed)
        XCTAssertEqual(result.pendingHireRequestIDs, ["hire-1"])
        XCTAssertEqual(result.entry?.templateID, "tt_abc")
        XCTAssertFalse(result.entry?.readOnly ?? true)
    }

    func testDiffAndReplayDecodeAdditiveFields() throws {
        let diff = try decode(
            TeamTemplateDiffResult.self,
            """
            {
              "project_id": "prj-1",
              "template_id": "tt_abc",
              "content_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
              "rows": [
                {"kind": "leader_overlay", "slot": "leader", "detail": "kept"},
                {"kind": "not_applied", "slot": "template", "detail": "library only"}
              ],
              "truncated": false,
              "warnings": []
            }
            """
        )
        XCTAssertEqual(diff.rows.count, 2)
        XCTAssertEqual(diff.rows[1].kind, "not_applied")

        let replay = try decode(
            TeamTemplateReplayReport.self,
            """
            {
              "project_id": "prj-1",
              "events": 3,
              "templates": 1,
              "links": 1,
              "repaired_links": 0,
              "mismatches": [],
              "warnings": []
            }
            """
        )
        XCTAssertEqual(replay.events, 3)
        XCTAssertEqual(replay.repairedLinks, 0)
        XCTAssertTrue(replay.mismatches.isEmpty)
    }

    func testExportResultDecodesNestedTemplateIdentity() throws {
        let result = try decode(
            TeamTemplateExportResult.self,
            """
            {
              "template": {
                "template_id": "tt_export",
                "name": "Platform Team",
                "version": "1.0.0",
                "content_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
              },
              "json": "{}",
              "markdown": "# Platform Team",
              "warnings": []
            }
            """
        )
        XCTAssertEqual(result.templateID, "tt_export")
        XCTAssertEqual(result.name, "Platform Team")
        XCTAssertEqual(result.contentHash.count, 64)
        XCTAssertTrue(result.markdown.contains("Platform Team"))
    }

    func testResolutionRequestUsesSnakeCaseIdentityKey() throws {
        let request = TeamTemplateResolutionRequest(
            slot: "manager:Platform:",
            action: "merge",
            identityID: "manager-1"
        )
        let data = try JSONEncoder().encode(request)
        let object = try JSONSerialization.jsonObject(with: data) as? [String: String]
        XCTAssertEqual(object?["identity_id"], "manager-1")
        XCTAssertEqual(object?["action"], "merge")
    }
}
