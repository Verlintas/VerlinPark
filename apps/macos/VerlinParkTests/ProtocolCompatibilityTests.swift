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

    func testAppNoticeRoundTrip() throws {
        let notice = AppNotice(
            id: "approval-1",
            sessionId: "session-1",
            projectId: "project-1",
            kind: "approval",
            title: "Approval needed",
            detail: "shell · Worker",
            createdAt: "2026-10-31T00:00:00Z",
            isRead: false
        )
        let data = try JSONEncoder().encode(notice)
        let decoded = try JSONDecoder().decode(AppNotice.self, from: data)
        XCTAssertEqual(decoded, notice)
    }

    func testSessionPlanResumeResultDecodes() throws {
        let json = #"{"session_id":"session-1","started":true}"#
        let result = try JSONDecoder().decode(
            SessionPlanResumeResult.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(result.sessionId, "session-1")
        XCTAssertTrue(result.started)
    }

    func testProjectUsageResultDecodes() throws {
        let json = """
        {
          "project_id": "project-1",
          "sessions": [
            {
              "session_id": "session-1",
              "goal": "ship it",
              "status": "idle",
              "calls": 2,
              "input_tokens": 10,
              "output_tokens": 5,
              "cost_usd": 0.25
            }
          ],
          "totals": {
            "calls": 2,
            "input_tokens": 10,
            "output_tokens": 5,
            "cost_usd": 0.25,
            "by_role": [],
            "by_model": [],
            "by_agent": [],
            "by_task": []
          }
        }
        """
        let result = try JSONDecoder().decode(ProjectUsageResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.projectId, "project-1")
        XCTAssertEqual(result.sessions.first?.totalTokens, 15)
        XCTAssertEqual(result.totals.calls, 2)
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
