@testable import VerlinPark
import XCTest

final class ApprovalOptimizerTests: XCTestCase {
    private let decoder = JSONDecoder()

    func testLegacyApprovalDecodesWithAttentionDefaults() throws {
        let data = Data(
            """
            {
              "id": "a1",
              "session_id": "s1",
              "tool_call_id": "call-1",
              "tool": "write_file",
              "input": {"path": "README.md"},
              "status": "pending",
              "created_at": "2026-11-12T00:00:00Z",
              "approval_risk": "workspace_write",
              "approval_reason": "workspace write"
            }
            """.utf8
        )
        let approval = try decoder.decode(Approval.self, from: data)
        XCTAssertEqual(approval.id, "a1")
        XCTAssertEqual(approval.waiterCount, 0)
        XCTAssertFalse(approval.notify)
        XCTAssertEqual(approval.priority, 0)
        XCTAssertNil(approval.agentId)
        XCTAssertNil(approval.dedupKey)
        XCTAssertTrue(approval.isPending)
    }

    func testApprovalDecodesCoalescingAndAttentionFields() throws {
        let data = Data(
            """
            {
              "id": "a1",
              "session_id": "s1",
              "tool_call_id": "call-1",
              "tool": "write_file",
              "input": {"path": "src/lib.rs"},
              "status": "pending",
              "created_at": "2026-11-12T00:00:00Z",
              "agent_name": "Worker",
              "agent_role": "worker",
              "agent_id": "worker-1",
              "task_id": "task-1",
              "approval_risk": "workspace_write",
              "approval_reason": "write",
              "waiter_count": 3,
              "template_id": "template-1",
              "priority": 42,
              "expires_at": "2026-11-13T00:00:00Z",
              "notify": false,
              "notify_key": "s1|write_file|workspace_write",
              "dedup_key": "abc"
            }
            """.utf8
        )
        let approval = try decoder.decode(Approval.self, from: data)
        XCTAssertEqual(approval.waiterCount, 3)
        XCTAssertEqual(approval.priority, 42)
        XCTAssertEqual(approval.templateId, "template-1")
        XCTAssertFalse(approval.notify)
        XCTAssertTrue(approval.attentionLabel.contains("3 waiters"))
        XCTAssertTrue(approval.attentionLabel.contains("criticality 42"))
    }

    func testTemplateDefaultsAndRemainingUses() throws {
        let data = Data(
            """
            {
              "id": "t1",
              "project_id": "p1",
              "name": "writes",
              "tool_pattern": "write_file",
              "created_by": "operator",
              "created_at": "2026-11-12T00:00:00Z"
            }
            """.utf8
        )
        let template = try decoder.decode(ApprovalTemplate.self, from: data)
        XCTAssertTrue(template.enabled)
        XCTAssertEqual(template.action, "allow")
        XCTAssertTrue(template.riskLevels.isEmpty)
        XCTAssertTrue(template.pathGlobs.isEmpty)
        XCTAssertNil(template.remainingUses)
        XCTAssertFalse(template.isExhausted)
    }

    func testAttentionPolicyDefaultsAndJsonObject() {
        let policy = AttentionPolicy()
        XCTAssertTrue(policy.enabled)
        XCTAssertEqual(policy.maxPendingPerSession, 64)
        XCTAssertEqual(policy.priorityBoostRisk, "host_state")
        let object = policy.jsonObject
        XCTAssertEqual(object["max_pending_per_session"]?.intValue, 64)
        XCTAssertEqual(object["priority_boost_risk"]?.stringValue, "host_state")
    }

    func testBatchResultDecodesPartialFailures() throws {
        let data = Data(
            """
            {
              "results": [
                {"approval_id": "a1", "resolved": true, "status": "approved", "waiter_count": 2},
                {"approval_id": "a2", "resolved": false, "status": "approved",
                 "code": "APPROVAL_NOT_PENDING", "message": "already resolved", "waiter_count": 1}
              ],
              "resolved_count": 1,
              "failed_count": 1
            }
            """.utf8
        )
        let result = try decoder.decode(ApprovalBatchResolveResult.self, from: data)
        XCTAssertEqual(result.resolvedCount, 1)
        XCTAssertEqual(result.failedCount, 1)
        XCTAssertTrue(result.results[0].resolved)
        XCTAssertEqual(result.results[1].code, "APPROVAL_NOT_PENDING")
    }

    func testQueueResultPreservesAttentionOrder() throws {
        let data = Data(
            """
            {
              "approvals": [
                {"id": "a1", "session_id": "s1", "tool_call_id": "c1", "tool": "computer",
                 "input": {}, "status": "pending", "created_at": "2026-11-12T00:00:00Z",
                 "approval_risk": "destructive", "waiter_count": 1},
                {"id": "a2", "session_id": "s1", "tool_call_id": "c2", "tool": "write_file",
                 "input": {"path": "a.txt"}, "status": "pending",
                 "created_at": "2026-11-12T00:00:01Z", "approval_risk": "workspace_write",
                 "waiter_count": 1}
              ],
              "policy": {},
              "pending_count": 2,
              "generated_at": "2026-11-12T00:00:02Z"
            }
            """.utf8
        )
        let queue = try decoder.decode(ApprovalQueueResult.self, from: data)
        XCTAssertEqual(queue.pendingCount, 2)
        XCTAssertEqual(queue.approvals[0].id, "a1")
        XCTAssertEqual(queue.policy.maxPendingPerSession, 64)
    }
}
