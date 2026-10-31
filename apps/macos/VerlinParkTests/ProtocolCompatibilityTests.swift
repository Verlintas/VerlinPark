import Foundation
@testable import VerlinPark
import XCTest

final class ProtocolCompatibilityTests: XCTestCase {
    func testAgentInfoWithoutCapacityStillDecodes() throws {
        let json = """
        {
          "configured": false,
          "provider": null,
          "model": null,
          "base_url": null,
          "role_models": {
            "leader": {},
            "manager": {},
            "worker": {},
            "verifier": {},
            "labour": {}
          }
        }
        """
        let info = try JSONDecoder().decode(AgentInfo.self, from: Data(json.utf8))
        XCTAssertFalse(info.configured)
        XCTAssertNil(info.capacity)
    }

    func testUsageTotalsWithoutDepartmentBucketStillDecodes() throws {
        let json = """
        {
          "session_id": "session-1",
          "entries": [],
          "totals": {
            "calls": 0,
            "input_tokens": 0,
            "output_tokens": 0,
            "cost_usd": 0,
            "by_role": [],
            "by_model": [],
            "by_agent": [],
            "by_task": []
          }
        }
        """
        let ledger = try JSONDecoder().decode(UsageLedgerResult.self, from: Data(json.utf8))
        XCTAssertEqual(ledger.totals.calls, 0)
        XCTAssertNil(ledger.totals.byDepartment)
    }

    func testSearchHitDecodesEventAndMessageShapes() throws {
        let json = """
        {
          "id": "session-1:7",
          "session_id": "session-1",
          "project_id": "project-1",
          "session_goal": "find it",
          "source": "event",
          "kind": "tool.finished",
          "seq": 7,
          "snippet": "needle",
          "created_at": "2026-10-31T00:00:00Z"
        }
        """
        let hit = try JSONDecoder().decode(SearchHit.self, from: Data(json.utf8))
        XCTAssertEqual(hit.source, "event")
        XCTAssertEqual(hit.detail, "tool.finished")
        XCTAssertEqual(hit.seq, 7)
        XCTAssertNil(hit.role)
    }

    func testUsageCsvResultDecodes() throws {
        let json = """
        {
          "session_id": "session-1",
          "csv": "created_at,agent_name\\n2026-10-31T00:00:00Z,Leader\\n"
        }
        """
        let result = try JSONDecoder().decode(UsageCsvResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.sessionId, "session-1")
        XCTAssertTrue(result.csv.hasPrefix("created_at,agent_name"))
    }

    func testPlanTaskWithoutBudgetsStillDecodes() throws {
        let json = """
        {
          "id": "T1",
          "department": "impl",
          "title": "Build",
          "kind": "coding",
          "deps": [],
          "write_scope": ["src"],
          "acceptance": ["done"]
        }
        """
        let task = try JSONDecoder().decode(PlanTask.self, from: Data(json.utf8))
        XCTAssertNil(task.tokenBudget)
        XCTAssertNil(task.costBudgetUsd)
    }
}
