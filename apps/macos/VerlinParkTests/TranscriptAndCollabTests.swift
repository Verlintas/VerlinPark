import AppKit
import Foundation
import SwiftUI
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

    func testCollabGraphBuildsTreeTaskOwnersAndActivity() {
        let agents = [
            agent(id: "leader", role: "leader", name: "Leader", status: "running"),
            agent(
                id: "manager-a",
                role: "manager",
                name: "Ada",
                title: "Platform",
                parent: "leader",
                status: "running",
                department: "Platform"
            ),
            agent(
                id: "manager-b",
                role: "manager",
                name: "Bo",
                title: "Product",
                parent: "leader",
                status: "idle",
                department: "Product"
            ),
            agent(
                id: "worker-a1",
                role: "worker",
                name: "Worker 1",
                parent: "manager-a",
                status: "running",
                task: "a"
            ),
            agent(
                id: "worker-a2",
                role: "worker",
                name: "Worker 2",
                parent: "manager-a",
                status: "idle",
                task: "b"
            ),
            agent(id: "worker-free", role: "worker", name: "Worker 3", status: "idle"),
            agent(
                id: "verifier",
                role: "verifier",
                name: "Verifier 1",
                parent: "manager-a",
                status: "idle",
                task: "a"
            ),
            agent(
                id: "labour",
                role: "labour",
                name: "Labour 1",
                parent: "worker-a1",
                status: "queued",
                task: "a"
            ),
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
        let events = [
            event(
                1,
                "tool.started",
                [
                    "agent_id": .string("worker-a1"),
                    "name": .string("shell"),
                ]
            ),
        ]
        let layout = CollabGraphBuilder.build(
            agents: agents,
            plan: plan,
            statuses: ["a": "completed", "b": "running", "c": "pending"],
            scheduled: [],
            events: events,
            sessionStatus: "running",
            goal: "ship",
            mergeQueue: []
        )

        XCTAssertEqual(layout.rootID, "leader")
        XCTAssertEqual(layout.metrics.totalAgents, 8)
        XCTAssertEqual(layout.metrics.runningAgents, 3)
        XCTAssertEqual(layout.metrics.completedTasks, 1)
        XCTAssertEqual(layout.metrics.activeTasks, 1)
        XCTAssertEqual(layout.metrics.readyTasks, 1)
        XCTAssertEqual(layout.metrics.attentionCount, 0)
        XCTAssertTrue(layout.metrics.headline.contains("1/3 tasks"))

        let worker = layout.nodes.first { $0.id == "worker-a1" }
        XCTAssertEqual(worker?.taskID, "a")
        XCTAssertEqual(worker?.taskStatus, "completed")
        XCTAssertEqual(worker?.activeTool, "shell")
        XCTAssertEqual(worker?.activity, "using shell")

        let manager = layout.nodes.first { $0.id == "manager-a" }
        XCTAssertEqual(manager?.department, "Platform")
        XCTAssertEqual(manager?.objective, "build")
        XCTAssertEqual(manager?.progress ?? 0, 1.0 / 3.0, accuracy: 0.001)
        XCTAssertEqual(manager?.childCount, 4)

        XCTAssertTrue(layout.nodes.contains {
            $0.id == "__planned_task__c" && $0.isPlaceholder && $0.taskID == "c"
        })
        XCTAssertFalse(layout.nodes.contains { $0.id == "__planned_task__a" })
        let leader = layout.nodes.first { $0.id == "leader" }
        let workerNode = layout.nodes.first { $0.id == "worker-a1" }
        XCTAssertNotNil(leader)
        XCTAssertNotNil(workerNode)
        XCTAssertLessThan(leader?.position.x ?? 0, manager?.position.x ?? 0)
        XCTAssertLessThan(manager?.position.x ?? 0, workerNode?.position.x ?? 0)
        XCTAssertGreaterThan(layout.size.width, 0)
        XCTAssertGreaterThan(layout.size.height, 0)
        XCTAssertFalse(layout.edges.isEmpty)
        for node in layout.nodes {
            XCTAssertGreaterThanOrEqual(
                node.position.x - node.size.width / 2,
                0,
                "\(node.id) at \(node.position) size \(node.size)"
            )
            XCTAssertGreaterThanOrEqual(
                node.position.y - node.size.height / 2,
                0,
                "\(node.id) at \(node.position) size \(node.size)"
            )
        }
    }

    func testCollabGraphSurfacesCoordinationActivity() {
        let agents = [
            agent(id: "leader", role: "leader", name: "Leader", status: "running"),
            agent(
                id: "worker",
                role: "worker",
                name: "Worker",
                parent: "leader",
                status: "running",
                task: "T1"
            ),
        ]
        let plan = TeamPlan(
            goal: "coordinate",
            departments: [],
            tasks: [task("T1")]
        )
        let events = [
            event(
                1,
                "coordination.note",
                [
                    "agent_id": .string("worker"),
                    "action": .string("blocker"),
                    "task_id": .string("T1"),
                    "message": .string("waiting for the API schema"),
                    "severity": .string("warning"),
                ]
            ),
        ]
        let layout = CollabGraphBuilder.build(
            agents: agents,
            plan: plan,
            statuses: ["T1": "running"],
            scheduled: [],
            events: events,
            sessionStatus: "running",
            goal: "coordinate",
            mergeQueue: []
        )
        let worker = layout.nodes.first { $0.id == "worker" }
        XCTAssertEqual(worker?.activity, "blocker: waiting for the API schema")
    }

    func testCollabGraphMarksDependencyBlockedTasksAsNotReady() {
        let plan = TeamPlan(
            goal: "deps",
            departments: [
                PlanDepartment(
                    name: "Platform",
                    managerTitle: "Platform",
                    profile: "coding",
                    objective: "build",
                    tokenBudget: nil,
                    costBudgetUsd: nil
                ),
            ],
            tasks: [
                task("a"),
                PlanTask(
                    id: "b",
                    department: "Platform",
                    title: "Task b",
                    kind: "code",
                    priority: 0,
                    profile: "coding",
                    deps: ["a"],
                    writeScope: [],
                    acceptance: [],
                    tokenBudget: nil,
                    costBudgetUsd: nil
                ),
            ]
        )
        let layout = CollabGraphBuilder.build(
            agents: [agent(id: "leader", role: "leader", name: "Leader", status: "idle")],
            plan: plan,
            statuses: ["a": "failed", "b": "pending"],
            scheduled: [],
            events: [],
            sessionStatus: "idle",
            goal: "deps",
            mergeQueue: []
        )
        XCTAssertEqual(layout.metrics.readyTasks, 1)
        XCTAssertEqual(layout.metrics.attentionCount, 0)
        let b = layout.nodes.first { $0.taskID == "b" }
        XCTAssertEqual(b?.deps, ["a"])
    }

    func testCollabGraphChoosesTheHealthyLeaderAndHidesStaleLeaders() {
        let agents = [
            agent(id: "stale-leader", role: "leader", name: "Leader", status: "failed"),
            agent(id: "live-leader", role: "leader", name: "Leader", status: "running"),
            agent(
                id: "manager",
                role: "manager",
                name: "Platform",
                title: "Platform",
                parent: "live-leader",
                status: "running",
                department: "Platform"
            ),
        ]
        let layout = CollabGraphBuilder.build(
            agents: agents,
            plan: nil,
            statuses: [:],
            scheduled: [],
            events: [],
            sessionStatus: "running",
            goal: "leader selection",
            mergeQueue: []
        )
        XCTAssertEqual(layout.rootID, "live-leader")
        XCTAssertFalse(layout.nodes.contains { $0.id == "stale-leader" })
        XCTAssertTrue(layout.nodes.contains { $0.id == "manager" })
    }

    func testCollabGraphShowsPlannedManagersBeforeAgentsExist() {
        let plan = TeamPlan(
            goal: "planned",
            departments: [
                PlanDepartment(
                    name: "Platform",
                    managerTitle: "Platform",
                    profile: "coding",
                    objective: "build",
                    tokenBudget: nil,
                    costBudgetUsd: nil
                ),
            ],
            tasks: [task("a")]
        )
        let layout = CollabGraphBuilder.build(
            agents: [],
            plan: plan,
            statuses: [:],
            scheduled: [],
            events: [],
            sessionStatus: "draft",
            goal: "planned",
            mergeQueue: []
        )
        XCTAssertNotNil(layout.rootID)
        XCTAssertTrue(layout.nodes.contains { $0.id == "__planned_manager__Platform" })
        XCTAssertTrue(layout.nodes.contains { $0.id == "__planned_task__a" })
        XCTAssertEqual(layout.metrics.totalAgents, 0)
        XCTAssertEqual(layout.metrics.totalTasks, 1)
    }

    @MainActor
    func testCollabGraphRendersDemoSnapshot() throws {
        let model = AppModel()
        model.selectedSessionID = "demo-session"
        model.sessions = [
            Session(
                id: "demo-session",
                projectId: "demo-project",
                goal: "Ship the collaboration graph",
                status: "running",
                priority: 0,
                worktree: "/tmp/demo",
                branch: "verlinpark/demo",
                tokenBudget: 200_000,
                costBudgetUsd: nil,
                timeBudgetSecs: nil,
                costSpentUsd: 0.42,
                baseCommit: nil,
                tokensIn: 18400,
                tokensOut: 6200,
                createdAt: "2026-11-09T10:00:00Z"
            ),
        ]
        let departments = [
            PlanDepartment(
                name: "Platform",
                managerTitle: "Platform",
                profile: "coding",
                objective: "Build the runtime and integration surface",
                tokenBudget: nil,
                costBudgetUsd: nil
            ),
            PlanDepartment(
                name: "Product",
                managerTitle: "Product",
                profile: "research",
                objective: "Define the workflow and acceptance criteria",
                tokenBudget: nil,
                costBudgetUsd: nil
            ),
            PlanDepartment(
                name: "Quality",
                managerTitle: "Quality",
                profile: "verification",
                objective: "Verify changes and guard regressions",
                tokenBudget: nil,
                costBudgetUsd: nil
            ),
        ]
        let tasks = [
            demoTask("T1", "Extract the collaboration projection", deps: []),
            demoTask("T2", "Render the tree layout", deps: ["T1"]),
            demoTask("T3", "Add task status chips", deps: ["T2"]),
            demoTask("T4", "Connect the task queue", deps: ["T2"]),
            demoTask("T5", "Design the command surface", deps: []),
            demoTask("T6", "Write acceptance criteria", deps: ["T5"]),
            demoTask("T7", "Audit keyboard and focus", deps: ["T3", "T6"]),
            demoTask("T8", "Verify integration path", deps: ["T4", "T6"]),
            demoTask("T9", "Polish motion and zoom", deps: ["T3"]),
            demoTask("T10", "Document the orchestration model", deps: ["T7"]),
        ]
        model.teamPlan = TeamPlan(
            goal: "Ship the collaboration graph",
            departments: departments,
            tasks: tasks
        )
        model.planTaskStatuses = [
            "T1": "completed",
            "T2": "completed",
            "T3": "running",
            "T4": "running",
            "T5": "completed",
            "T6": "running",
            "T7": "pending",
            "T8": "pending",
            "T9": "blocked",
            "T10": "pending",
        ]
        model.agents = [
            agent(id: "leader", role: "leader", name: "Leader", status: "running"),
            agent(
                id: "manager-platform",
                role: "manager",
                name: "Platform",
                title: "Platform",
                parent: "leader",
                status: "running",
                department: "Platform"
            ),
            agent(
                id: "manager-product",
                role: "manager",
                name: "Product",
                title: "Product",
                parent: "leader",
                status: "running",
                department: "Product"
            ),
            agent(
                id: "manager-quality",
                role: "manager",
                name: "Quality",
                title: "Quality",
                parent: "leader",
                status: "idle",
                department: "Quality"
            ),
            agent(
                id: "worker-t3",
                role: "worker",
                name: "Tree layout",
                parent: "manager-platform",
                status: "running",
                task: "T3"
            ),
            agent(
                id: "worker-t4",
                role: "worker",
                name: "Queue bridge",
                parent: "manager-platform",
                status: "running",
                task: "T4"
            ),
            agent(
                id: "worker-t6",
                role: "worker",
                name: "Acceptance",
                parent: "manager-product",
                status: "running",
                task: "T6"
            ),
            agent(
                id: "worker-t9",
                role: "worker",
                name: "Motion polish",
                parent: "manager-platform",
                status: "failed",
                task: "T9"
            ),
            agent(id: "worker-free", role: "worker", name: "Unassigned", status: "idle"),
            agent(
                id: "verifier-t4",
                role: "verifier",
                name: "Verifier",
                parent: "manager-platform",
                status: "running",
                task: "T4"
            ),
            agent(
                id: "verifier-t6",
                role: "verifier",
                name: "Verifier",
                parent: "manager-product",
                status: "queued",
                task: "T6"
            ),
            agent(
                id: "labour-t3",
                role: "labour",
                name: "Layout research",
                parent: "worker-t3",
                status: "running",
                task: "T3"
            ),
            agent(
                id: "labour-t4",
                role: "labour",
                name: "Schema check",
                parent: "worker-t4",
                status: "queued",
                task: "T4"
            ),
        ]
        model.events = [
            event(1, "agent.spawned", ["agent_id": .string("worker-t3"), "department": .string("Platform")]),
            event(2, "tool.started", ["agent_id": .string("worker-t3"), "name": .string("shell")]),
            event(3, "tool.started", ["agent_id": .string("worker-t4"), "name": .string("write_file")]),
            event(4, "agent.message", ["agent_id": .string("worker-t6"), "text": .string("draft ready")]),
            event(5, "tool.started", ["agent_id": .string("labour-t3"), "name": .string("grep")]),
            event(
                6,
                "plan_task.finished",
                [
                    "task_id": .string("T1"),
                    "task_status": .string("completed"),
                    "summary": .string("projection landed"),
                ]
            ),
            event(
                7,
                "plan_task.finished",
                [
                    "task_id": .string("T2"),
                    "task_status": .string("completed"),
                    "summary": .string("tree layout landed"),
                ]
            ),
            event(
                8,
                "plan_task.finished",
                [
                    "task_id": .string("T9"),
                    "task_status": .string("failed"),
                    "summary": .string("animation test timed out"),
                ]
            ),
        ]
        model.mergeQueue = []

        let layout = CollabGraphBuilder.build(
            agents: model.agents,
            plan: model.teamPlan,
            statuses: model.planTaskStatuses,
            scheduled: model.scheduledSessionTasks,
            events: model.events,
            sessionStatus: "running",
            goal: model.selectedSession?.goal ?? "demo",
            mergeQueue: []
        )
        XCTAssertGreaterThan(layout.nodes.count, 10)
        let renderer = ImageRenderer(
            content: CollabGraphContent(
                layout: layout,
                selectedID: nil,
                reduceMotion: true,
                onSelect: { _ in }
            )
            .frame(width: layout.size.width, height: layout.size.height)
        )
        renderer.scale = 1
        guard let image = renderer.nsImage,
              let tiff = image.tiffRepresentation,
              let representation = NSBitmapImageRep(data: tiff),
              let png = representation.representation(using: .png, properties: [:])
        else {
            return XCTFail("could not render the Collab graph")
        }
        let url = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("verlinpark-collab-demo.png")
        try png.write(to: url)
        XCTAssertGreaterThan(png.count, 10000)
    }

    private func demoTask(_ id: String, _ title: String, deps: [String]) -> PlanTask {
        PlanTask(
            id: id,
            department: "Platform",
            title: title,
            kind: "coding",
            priority: 0,
            profile: "coding",
            deps: deps,
            writeScope: [],
            acceptance: ["tests pass"],
            tokenBudget: nil,
            costBudgetUsd: nil
        )
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
        status: String,
        task: String? = nil,
        department: String? = nil
    ) -> AgentRecord {
        AgentRecord(
            id: id,
            sessionId: "session-1",
            role: role,
            name: name,
            title: title,
            profile: role == "manager" ? "coding" : nil,
            parentId: parent,
            taskId: task,
            department: department,
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
