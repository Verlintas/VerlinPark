@testable import VerlinPark
import XCTest

final class RuntimePinModelsTests: XCTestCase {
    private let decoder = JSONDecoder()

    func testCompatibilityReportDecodesDriftAndLegacyDefaults() throws {
        let data = Data(
            """
            {
              "status": "drift",
              "target": "plan",
              "session_id": "ses-1",
              "plan_revision": 3,
              "snapshot_age_secs": 120,
              "changed": [
                {"field": "policy_hash", "stored": "old", "current": "new", "severity": "drift"}
              ],
              "warnings": ["policy_changed"],
              "migration_required": false,
              "checked_at": "2026-11-12T00:00:00Z"
            }
            """.utf8
        )
        let report = try decoder.decode(CompatibilityReportView.self, from: data)
        XCTAssertTrue(report.isDrift)
        XCTAssertFalse(report.migrationRequired)
        XCTAssertEqual(report.planRevision, 3)
        XCTAssertEqual(report.snapshotAgeSecs, 120)
        XCTAssertEqual(report.changed.count, 1)
        XCTAssertEqual(report.changed[0].field, "policy_hash")
        XCTAssertNil(report.stored)
    }

    func testCompatibilityReportDecodesIncompatibleSnapshots() throws {
        let data = Data(
            """
            {
              "status": "incompatible",
              "target": "run",
              "session_id": "ses-1",
              "run_id": "run-1",
              "plan_revision": 4,
              "snapshot_age_secs": 9,
              "stored": {
                "schema_version": 1,
                "runtime_version": "0.1.0",
                "runtime_build": "build-a",
                "policy_hash": "policy-a",
                "tool_bundle_hash": "bundle-a",
                "prompt_layout_version": 3,
                "plan_revision": 4,
                "contract_hash": "contract-a",
                "created_at": "2026-11-12T00:00:00Z"
              },
              "current": {
                "runtime_version": "0.1.0",
                "runtime_build": "build-b",
                "policy_hash": "policy-b",
                "tool_bundle_hash": "bundle-b",
                "prompt_layout_version": 3,
                "plan_revision": 5,
                "contract_hash": "contract-b"
              },
              "changed": [],
              "warnings": ["plan_revision_changed"],
              "migration_required": true
            }
            """.utf8
        )
        let report = try decoder.decode(CompatibilityReportView.self, from: data)
        XCTAssertTrue(report.isIncompatible)
        XCTAssertTrue(report.migrationRequired)
        XCTAssertEqual(report.runID, "run-1")
        XCTAssertEqual(report.stored?.policyHash, "policy-a")
        XCTAssertEqual(report.current?.runtimeBuild, "build-b")
        XCTAssertEqual(report.current?.schemaVersion, 0)
    }

    func testControllerReplayDecodesAndKeepsControllerOnlyLabel() throws {
        let data = Data(
            """
            {
              "session_id": "ses-1",
              "limit": 2,
              "total_recorded": 9,
              "examined": 2,
              "matched": 1,
              "mismatched": 1,
              "skipped": 0,
              "truncated": true,
              "controller_equivalent_only": true,
              "entries": [
                {
                  "seq": 7,
                  "kind": "orchestration.decision",
                  "status": "mismatched",
                  "recorded_action": "retry:T1",
                  "recomputed_action": "block:T2",
                  "changes": [{"field": "action", "recorded": "retry:T1", "recomputed": "block:T2"}],
                  "reason": "different action set"
                }
              ]
            }
            """.utf8
        )
        let report = try decoder.decode(ControllerReplayReportView.self, from: data)
        XCTAssertTrue(report.controllerEquivalentOnly)
        XCTAssertTrue(report.truncated)
        XCTAssertEqual(report.entries.count, 1)
        XCTAssertEqual(report.entries[0].status, "mismatched")
        XCTAssertEqual(report.entries[0].changes[0].field, "action")
        XCTAssertEqual(report.generatedAt, "")
    }

    func testPlanMigrateResultDecodesWithoutOptionalSnapshot() throws {
        let data = Data(
            """
            {
              "session_id": "ses-1",
              "migrated": true,
              "plan_revision": 2,
              "warnings": ["open_gates: 1 external gate(s) remain open"],
              "event_seq": 42
            }
            """.utf8
        )
        let result = try decoder.decode(PlanMigrateResultView.self, from: data)
        XCTAssertTrue(result.migrated)
        XCTAssertEqual(result.planRevision, 2)
        XCTAssertEqual(result.eventSeq, 42)
        XCTAssertNil(result.snapshot)
        XCTAssertEqual(result.warnings.count, 1)
    }
}
