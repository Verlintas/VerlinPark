import AppKit
import Foundation
import SwiftUI
@testable import VerlinPark
import XCTest

/// Render-only coverage for the Collab Agent Inspector surface. The panel
/// reads a seeded AppModel projection; no daemon RPC or network call occurs.
final class CollabAgentInspectorUITests: XCTestCase {
    @MainActor
    func testAgentInspectorPanelRendersSeededProjection() throws {
        let decoder = JSONDecoder()
        let agentJSON = try XCTUnwrap("""
        {
          "id": "agent-1",
          "session_id": "session-1",
          "role": "worker",
          "name": "Worker One",
          "title": "Layout Worker",
          "identity_id": "identity-1",
          "profile": "coding",
          "parent_id": null,
          "task_id": "T1",
          "department": "Platform",
          "worktree": "/tmp/verlinpark/worker",
          "branch": "verlinpark/worker",
          "status": "running",
          "model": "deepseek-flash",
          "created_at": "2026-11-22T10:00:00Z"
        }
        """.data(using: .utf8))
        let agent = try decoder.decode(AgentRecord.self, from: agentJSON)

        let inspectJSON = try XCTUnwrap("""
        {
          "agent": {
            "id": "agent-1",
            "session_id": "session-1",
            "role": "worker",
            "name": "Worker One",
            "title": "Layout Worker",
            "identity_id": "identity-1",
            "profile": "coding",
            "parent_id": null,
            "task_id": "T1",
            "department": "Platform",
            "worktree": "/tmp/verlinpark/worker",
            "branch": "verlinpark/worker",
            "status": "running",
            "model": "deepseek-flash",
            "created_at": "2026-11-22T10:00:00Z"
          },
          "task": {
            "task_id": "T1",
            "title": "Build the inspector",
            "department": "Platform",
            "kind": "coding",
            "status": "running",
            "acceptance": ["Panel renders", "Delivery stays truthful"],
            "write_scope": ["apps/macos/VerlinPark/Features/Collab/**"],
            "deps": [],
            "attempts": 1,
            "max_attempts": 3,
            "verifier_status": "pending"
          },
          "context": {
            "objective": "Ship a bounded Agent inspector",
            "model": "deepseek-flash",
            "profile": "coding",
            "estimated_tokens": 42000,
            "context_window": 128000,
            "threshold_tokens": 96000,
            "hard_limit_tokens": 128000,
            "input_tokens": 39000,
            "output_tokens": 1200,
            "cache_read_tokens": 35000,
            "cache_write_tokens": 2000,
            "cost_usd": 0.42,
            "tool_count": 49,
            "tool_bundle": "coding",
            "prefix_hash": "prefix-abcdef",
            "last_compaction": "trim · 2026-11-22T10:00:00Z",
            "memory_brief_chars": 512,
            "memory_brief_hash": "memory-hash",
            "generated_at": "2026-11-22T10:01:00Z"
          },
          "timeline": [
            {
              "seq": 7,
              "kind": "tool.finished",
              "category": "tool",
              "created_at": "2026-11-22T10:00:00Z",
              "text": "cargo test --workspace passed",
              "tool": "shell",
              "tool_call_id": "call-1",
              "status": "ok",
              "is_error": false
            },
            {
              "seq": 8,
              "kind": "agent.message",
              "category": "message",
              "created_at": "2026-11-22T10:00:02Z",
              "text": "Tests are green; requested verifier review.",
              "tool": null,
              "tool_call_id": null,
              "status": "ok",
              "is_error": false
            }
          ],
          "steering": [{
            "id": "steer-1",
            "session_id": "session-1",
            "agent_id": "agent-1",
            "task_id": "T1",
            "body": "Please check the next boundary.",
            "status": "pending",
            "created_at": "2026-11-22T10:00:03Z",
            "delivered_at": null
          }],
          "inbox": [{
            "id": "msg-1",
            "from_label": "Operator",
            "kind": "message",
            "status": "unread",
            "body": "Review the failing test.",
            "created_at": "2026-11-22T10:00:04Z",
            "read_at": null,
            "expires_at": null
          }],
          "inbox_counts": {"unread": 1, "read": 0, "done": 0, "expired": 0},
          "truncated": false
        }
        """.data(using: .utf8))
        let inspect = try decoder.decode(AgentInspectResult.self, from: inspectJSON)

        let model = AppModel()
        model.selectedSessionID = "session-1"
        model.sessions = [
            Session(
                id: "session-1",
                projectId: "project-1",
                goal: "Inspect an Agent",
                status: "running",
                priority: 0,
                worktree: "/tmp/verlinpark/session",
                branch: "verlinpark/session",
                tokenBudget: 200_000,
                costBudgetUsd: 5,
                timeBudgetSecs: nil,
                unlimited: false,
                costSpentUsd: 0.5,
                baseCommit: nil,
                tokensIn: 20000,
                tokensOut: 1000,
                promptLayout: 3,
                createdAt: "2026-11-22T10:00:00Z"
            ),
        ]
        model.agents = [agent]
        model.collabAgentInspect = inspect
        model.collabAgentInspectAgentID = "agent-1"
        model.steeringMessages = inspect.steering

        let layout = CollabGraphBuilder.build(
            agents: [agent],
            plan: nil,
            statuses: [:],
            scheduled: [],
            events: [],
            sessionStatus: "running",
            goal: "Inspect an Agent",
            mergeQueue: []
        )
        let node = try XCTUnwrap(layout.nodes.first { $0.id == "agent-1" })

        let panel = CollabAgentInspectorPanel(
            node: node,
            task: nil,
            blockers: [],
            busy: false,
            onAction: { _ in },
            onClose: {}
        )
        .environment(model)
        .frame(width: 380, height: 760)
        .background(VPTheme.canvas)

        guard let overview = renderPNG(panel) else {
            return XCTFail("could not render the Agent inspector overview")
        }
        let baseURL = URL(fileURLWithPath: NSTemporaryDirectory())
        try overview.write(to: baseURL.appendingPathComponent("verlinpark-agent-inspector.png"))
        XCTAssertGreaterThan(overview.count, 8000)

        for tab in CollabAgentInspectorTab.allCases {
            let tabPanel = CollabAgentInspectorPanel(
                node: node,
                task: nil,
                blockers: [],
                busy: false,
                onAction: { _ in },
                onClose: {},
                initialTab: tab
            )
            .environment(model)
            .frame(width: 400, height: 760)
            .background(VPTheme.canvas)
            guard let png = renderPNG(tabPanel) else {
                return XCTFail("could not render the Agent inspector \(tab.rawValue) tab")
            }
            try png.write(
                to: baseURL.appendingPathComponent("verlinpark-agent-inspector-\(tab.rawValue).png")
            )
            XCTAssertGreaterThan(png.count, 8000, "\(tab.rawValue) tab rendered empty")
        }
    }

    func testComposerRoutingStaysTruthful() {
        XCTAssertTrue(CollabInspectorComposerMode.canSteer(status: "running"))
        XCTAssertTrue(CollabInspectorComposerMode.canSteer(status: "waiting_input"))
        XCTAssertFalse(CollabInspectorComposerMode.canSteer(status: "paused"))
        XCTAssertFalse(CollabInspectorComposerMode.canSteer(status: "queued"))
        XCTAssertFalse(CollabInspectorComposerMode.canSteer(status: "verifying"))
        XCTAssertEqual(
            CollabInspectorComposerMode.resolve(
                isActiveRun: true,
                identityID: nil,
                hasAgent: true
            ),
            .steering
        )
        XCTAssertEqual(
            CollabInspectorComposerMode.resolve(
                isActiveRun: false,
                identityID: "identity-1",
                hasAgent: true
            ),
            .inbox
        )
        XCTAssertEqual(
            CollabInspectorComposerMode.resolve(
                isActiveRun: false,
                identityID: nil,
                hasAgent: true
            ),
            .inactive
        )
        guard case .unavailable = CollabInspectorComposerMode.resolve(
            isActiveRun: false,
            identityID: "identity-1",
            hasAgent: false
        ) else {
            return XCTFail("a missing Agent record must disable the composer")
        }
    }

    @MainActor
    private func renderPNG(_ panel: some View) -> Data? {
        let hosting = NSHostingView(rootView: panel)
        hosting.frame = NSRect(x: 0, y: 0, width: 400, height: 760)
        hosting.layoutSubtreeIfNeeded()
        guard let representation = hosting.bitmapImageRepForCachingDisplay(in: hosting.bounds) else {
            return nil
        }
        hosting.cacheDisplay(in: hosting.bounds, to: representation)
        return representation.representation(using: .png, properties: [:])
    }
}
