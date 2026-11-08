import Foundation
@testable import VerlinPark
import XCTest

final class TranscriptAndCollabTests: XCTestCase {
    func testTranscriptTracksRunningToolAndDuration() {
        let events = [
            event(
                1,
                "tool.started",
                [
                    "tool_call_id": .string("tool-1"),
                    "name": .string("shell"),
                    "input": .object(["command": .string("cargo   test --workspace")]),
                ],
                at: "2026-11-08T10:00:00.000Z"
            ),
            event(
                2,
                "tool.finished",
                [
                    "tool_call_id": .string("tool-1"),
                    "name": .string("shell"),
                    "output": .string("ok"),
                    "is_error": .bool(false),
                ],
                at: "2026-11-08T10:00:02.000Z"
            ),
        ]

        let running = TranscriptProjection.build(events: Array(events.prefix(1)))
        XCTAssertEqual(running.count, 1)
        guard case let .tool(runningTool) = running[0] else {
            return XCTFail("expected a tool entry")
        }
        XCTAssertTrue(runningTool.isRunning)
        XCTAssertEqual(runningTool.summary, "cargo test --workspace")

        let finished = TranscriptProjection.build(events: events)
        guard case let .tool(tool) = finished[0] else {
            return XCTFail("expected a tool entry")
        }
        XCTAssertFalse(tool.isRunning)
        XCTAssertEqual(tool.duration ?? 0, 2, accuracy: 0.01)
        XCTAssertEqual(tool.output, "ok")
        XCTAssertEqual(tool.id, "tool-tool-1")
    }

    func testTranscriptResolvesApprovalAndKeepsRiskReason() {
        let payload: [String: JSONValue] = [
            "id": .string("approval-1"),
            "session_id": .string("session-1"),
            "tool_call_id": .string("toolshell"),
            "tool": .string("shell"),
            "input": .object(["command": .string("shutdown -h now")]),
            "status": .string("pending"),
            "created_at": .string("2026-11-08T10:00:00Z"),
            "resolved_at": .null,
            "approval_risk": .string("destructive"),
            "approval_reason": .string("process or system power control"),
        ]
        let events = [
            event(1, "approval.requested", payload),
            event(
                2,
                "approval.resolved",
                [
                    "approval_id": .string("approval-1"),
                    "approved": .bool(false),
                    "resolved_at": .string("2026-11-08T10:00:05Z"),
                ]
            ),
        ]

        let entries = TranscriptProjection.build(events: events)
        XCTAssertEqual(entries.count, 1)
        guard case let .approval(approval) = entries[0] else {
            return XCTFail("expected an approval entry")
        }
        XCTAssertFalse(approval.isPending)
        XCTAssertTrue(approval.isDenied)
        XCTAssertEqual(approval.status, "denied")
        XCTAssertEqual(approval.approval.approvalRisk, "destructive")
        XCTAssertEqual(approval.approval.approvalReason, "process or system power control")
    }

    func testToolTraceSummariesCoverStructuredInputs() {
        XCTAssertEqual(
            ToolTrace.summary(
                name: "write_file",
                input: .object(["path": .string("src/lib.rs"), "content": .string("x")])
            ),
            "src/lib.rs"
        )
        XCTAssertEqual(
            ToolTrace.summary(
                name: "mcp",
                input: .object([
                    "action": .string("call"),
                    "server": .string("docs"),
                    "tool": .string("search"),
                ])
            ),
            "call · docs · search"
        )
        XCTAssertTrue(
            ToolTrace.isHostSensitive(
                name: "shell",
                input: .object(["command": .string("sudo rm -rf /")])
            )
        )
    }

    func testCollabSnapshotGroupsWorkersUnderManagersAndCountsPlan() {
        let agents = [
            agent(id: "leader", role: "leader", name: "Leader", status: "running"),
            agent(id: "manager-a", role: "manager", name: "Ada", title: "Platform", status: "running"),
            agent(id: "manager-b", role: "manager", name: "Bo", title: "Product", status: "idle"),
            agent(id: "worker-a1", role: "worker", name: "Worker 1", parent: "manager-a", status: "running"),
            agent(id: "worker-a2", role: "worker", name: "Worker 2", parent: "manager-a", status: "idle"),
            agent(id: "worker-free", role: "worker", name: "Worker 3", status: "idle"),
            agent(id: "verifier", role: "verifier", name: "Verifier 1", status: "idle"),
            agent(id: "labour", role: "labour", name: "Labour 1", status: "queued"),
        ]
        let plan = TeamPlan(
            goal: "ship",
            departments: [
                PlanDepartment(
                    name: "Platform",
                    managerTitle: "Platform",
                    profile: "coding",
                    objective: "build",
                    tokenBudget: nil,
                    costBudgetUsd: nil
                ),
                PlanDepartment(
                    name: "Product",
                    managerTitle: "Product",
                    profile: "research",
                    objective: "research",
                    tokenBudget: nil,
                    costBudgetUsd: nil
                ),
            ],
            tasks: [
                task("a"), task("b"), task("c"),
            ]
        )
        let snapshot = CollabSnapshot.build(
            agents: agents,
            plan: plan,
            statuses: ["a": "completed", "b": "running"],
            sessionStatus: "running",
            goal: "ship"
        )

        XCTAssertEqual(snapshot.managers.count, 2)
        XCTAssertEqual(snapshot.managers.first { $0.manager.id == "manager-a" }?.workers.count, 2)
        XCTAssertEqual(snapshot.managers.first { $0.manager.id == "manager-b" }?.workers.count, 0)
        XCTAssertEqual(snapshot.unassignedWorkers.map(\.id), ["worker-free"])
        XCTAssertEqual(snapshot.totalAgents, 8)
        XCTAssertEqual(snapshot.runningAgents, 3)
        XCTAssertEqual(snapshot.completedTasks, 1)
        XCTAssertEqual(snapshot.activeTasks, 1)
        XCTAssertEqual(snapshot.totalTasks, 3)
        XCTAssertTrue(snapshot.summary.contains("1/3 tasks"))
    }

    func testAgentInfoDecodesApprovalModeAndLegacyPayloads() throws {
        let json = """
        {
          "configured": true,
          "provider": "openai-compatible",
          "model": "local",
          "base_url": "http://127.0.0.1:1234/v1",
          "role_models": {"leader": {}, "manager": {}, "worker": {}, "verifier": {}, "labour": {}},
          "context_window": 32768,
          "approval_mode": "unattended"
        }
        """
        let info = try JSONDecoder().decode(AgentInfo.self, from: Data(json.utf8))
        XCTAssertEqual(info.approvalMode, "unattended")

        let legacy = """
        {
          "configured": false,
          "provider": null,
          "model": null,
          "base_url": null,
          "role_models": {"leader": {}, "manager": {}, "worker": {}, "verifier": {}, "labour": {}},
          "context_window": 128000
        }
        """
        let oldInfo = try JSONDecoder().decode(AgentInfo.self, from: Data(legacy.utf8))
        XCTAssertNil(oldInfo.approvalMode)
    }

    func testAgentApprovalModeRawValuesStayWireCompatible() {
        XCTAssertEqual(AgentApprovalMode.smart.rawValue, "smart")
        XCTAssertEqual(AgentApprovalMode.strict.rawValue, "strict")
        XCTAssertEqual(AgentApprovalMode.unattended.rawValue, "unattended")
    }

    private func event(
        _ seq: UInt64,
        _ type: String,
        _ payload: [String: JSONValue],
        at: String = "2026-11-08T10:00:00Z"
    ) -> Event {
        Event(
            sessionId: "session-1",
            seq: seq,
            type: type,
            payload: .object(payload),
            createdAt: at
        )
    }

    private func agent(
        id: String,
        role: String,
        name: String,
        title: String? = nil,
        parent: String? = nil,
        status: String
    ) -> AgentRecord {
        AgentRecord(
            id: id,
            sessionId: "session-1",
            role: role,
            name: name,
            title: title,
            profile: role == "manager" ? "coding" : nil,
            parentId: parent,
            worktree: "/tmp/\(id)",
            branch: "verlinpark/\(id)",
            status: status,
            model: "test",
            createdAt: "2026-11-08T10:00:00Z"
        )
    }

    private func task(_ id: String) -> PlanTask {
        PlanTask(
            id: id,
            department: "Platform",
            title: "Task \(id)",
            kind: "code",
            priority: 0,
            profile: "coding",
            deps: [],
            writeScope: [],
            acceptance: [],
            tokenBudget: nil,
            costBudgetUsd: nil
        )
    }
}
