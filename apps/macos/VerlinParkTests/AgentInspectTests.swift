import Foundation
@testable import VerlinPark
import XCTest

final class AgentInspectTests: XCTestCase {
    private let decoder = JSONDecoder()

    // MARK: - Decoding

    func testInspectResultDecodesFullPayload() throws {
        let result = try decodeInspect()

        XCTAssertEqual(result.agent.id, "agent-1")
        XCTAssertEqual(result.agent.taskId, "task-1")
        XCTAssertEqual(result.agent.identityId, "identity-1")
        XCTAssertEqual(result.agent.branch, "verlinpark/task-1")
        XCTAssertTrue(result.truncated)

        let task = try XCTUnwrap(result.task)
        XCTAssertEqual(task.taskID, "task-1")
        XCTAssertEqual(task.title, "Implement operator channel")
        XCTAssertEqual(task.department, "Platform")
        XCTAssertEqual(task.kind, "coding")
        XCTAssertEqual(task.status, "running")
        XCTAssertEqual(task.acceptance, ["Tests pass", "No cross-session target"])
        XCTAssertEqual(task.writeScope, ["apps/macos/**"])
        XCTAssertEqual(task.deps, ["task-0"])
        XCTAssertEqual(task.attempts, 1)
        XCTAssertEqual(task.maxAttempts, 3)
        XCTAssertEqual(task.verifierStatus, "pending")

        XCTAssertEqual(result.context.objective, "Ship the inspector")
        XCTAssertEqual(result.context.model, "deepseek-flash")
        XCTAssertEqual(result.context.profile, "coding")
        XCTAssertEqual(result.context.estimatedTokens, 42000)
        XCTAssertEqual(result.context.contextWindow, 128_000)
        XCTAssertEqual(result.context.thresholdTokens, 96000)
        XCTAssertEqual(result.context.hardLimitTokens, 128_000)
        XCTAssertEqual(result.context.inputTokens, 39000)
        XCTAssertEqual(result.context.outputTokens, 1200)
        XCTAssertEqual(result.context.cacheReadTokens, 35000)
        XCTAssertEqual(result.context.cacheWriteTokens, 2000)
        XCTAssertEqual(result.context.costUSD, 0.42, accuracy: 0.0001)
        XCTAssertEqual(result.context.toolCount, 49)
        XCTAssertEqual(result.context.toolBundle, "coding")
        XCTAssertEqual(result.context.prefixHash, "prefix-abc")
        XCTAssertEqual(result.context.lastCompaction, "trim · 2026-11-22T10:00:00Z")
        XCTAssertEqual(result.context.memoryBriefChars, 512)
        XCTAssertEqual(result.context.memoryBriefHash, "memory-hash")
        XCTAssertEqual(result.context.generatedAt, "2026-11-22T10:01:00Z")

        XCTAssertEqual(result.timeline.count, 2)
        XCTAssertEqual(result.timeline[0].seq, 7)
        XCTAssertEqual(result.timeline[0].kind, "tool.finished")
        XCTAssertEqual(result.timeline[0].category, "tool")
        XCTAssertEqual(result.timeline[0].tool, "shell")
        XCTAssertEqual(result.timeline[0].toolCallID, "call-1")
        XCTAssertEqual(result.timeline[0].status, "ok")
        XCTAssertFalse(result.timeline[0].isError)
        XCTAssertEqual(result.timeline[1].kind, "agent.message")
        XCTAssertTrue(result.timeline[1].isError)
        XCTAssertEqual(result.timeline[1].text, "A bounded failure")

        XCTAssertEqual(result.steering.count, 1)
        XCTAssertEqual(result.steering[0].id, "steer-1")
        XCTAssertEqual(result.steering[0].agentId, "agent-1")
        XCTAssertEqual(result.steering[0].taskId, "task-1")
        XCTAssertEqual(result.steering[0].status, "pending")

        XCTAssertEqual(result.inbox.count, 1)
        XCTAssertEqual(result.inbox[0].fromLabel, "Operator")
        XCTAssertEqual(result.inbox[0].kind, "message")
        XCTAssertTrue(result.inbox[0].isUnread)
        XCTAssertEqual(result.inbox[0].expiresAt, "2026-11-23T10:00:00Z")

        XCTAssertEqual(result.inboxCounts.unread, 1)
        XCTAssertEqual(result.inboxCounts.read, 2)
        XCTAssertEqual(result.inboxCounts.done, 3)
        XCTAssertEqual(result.inboxCounts.expired, 4)
        XCTAssertEqual(result.inboxCounts.total, 10, "total is derived when omitted")
    }

    func testInspectResultDecodesMinimalPayloadWithDefaults() throws {
        let json = try XCTUnwrap("""
        {
          "agent": {
            "id": "agent-1",
            "session_id": "ses-1",
            "role": "worker",
            "name": "Worker 1",
            "status": "idle",
            "model": "deepseek-flash",
            "created_at": "2026-11-22T10:00:00Z"
          },
          "context": {}
        }
        """.data(using: .utf8))

        let result = try decoder.decode(AgentInspectResult.self, from: json)
        XCTAssertEqual(result.agent.id, "agent-1")
        XCTAssertNil(result.task)
        XCTAssertTrue(result.timeline.isEmpty)
        XCTAssertTrue(result.steering.isEmpty)
        XCTAssertTrue(result.inbox.isEmpty)
        XCTAssertFalse(result.truncated)
        XCTAssertEqual(result.inboxCounts.unread, 0)
        XCTAssertEqual(result.inboxCounts.total, 0)
        XCTAssertEqual(result.context.model, "")
        XCTAssertEqual(result.context.estimatedTokens, 0)
        XCTAssertEqual(result.context.contextWindow, 0)
        XCTAssertNil(result.context.lastCompaction)
        XCTAssertNil(result.context.prefixHash)
    }

    func testInspectResultIgnoresUnknownFieldsAndEmptyArrays() throws {
        let json = try XCTUnwrap("""
        {
          "agent": {
            "id": "agent-2",
            "session_id": "ses-1",
            "role": "manager",
            "name": "Manager 1",
            "status": "idle",
            "model": "deepseek-flash",
            "created_at": "2026-11-22T10:00:00Z",
            "future_agent_field": {"nested": true}
          },
          "context": {
            "model": "deepseek-flash",
            "future_context_field": 99
          },
          "timeline": [],
          "steering": [],
          "inbox": [],
          "inbox_counts": {"unread": 0},
          "future_top_level_field": "ignored"
        }
        """.data(using: .utf8))

        let result = try decoder.decode(AgentInspectResult.self, from: json)
        XCTAssertEqual(result.agent.role, "manager")
        XCTAssertTrue(result.timeline.isEmpty)
        XCTAssertTrue(result.steering.isEmpty)
        XCTAssertTrue(result.inbox.isEmpty)
        XCTAssertEqual(result.inboxCounts.unread, 0)
        XCTAssertEqual(result.inboxCounts.total, 0)
        XCTAssertFalse(result.truncated)
    }

    func testContextAcceptsStructuredCompactionAndSnakeCaseKeys() throws {
        let json = try XCTUnwrap("""
        {
          "model": "deepseek-flash",
          "estimated_tokens": 100,
          "context_window": 1000,
          "threshold_tokens": 750,
          "hard_limit_tokens": 1000,
          "input_tokens": 90,
          "output_tokens": 10,
          "cache_read_tokens": 80,
          "cache_write_tokens": 5,
          "cost_usd": 0.01,
          "tool_count": 12,
          "tool_bundle": "research",
          "prefix_hash": "hash-1",
          "last_compaction": {
            "mode": "summary",
            "estimated_tokens_before": 500,
            "estimated_tokens_after": 200
          },
          "memory_brief_chars": 128,
          "memory_brief_hash": "hash-2",
          "generated_at": "2026-11-22T10:00:00Z"
        }
        """.data(using: .utf8))

        let context = try decoder.decode(AgentContextView.self, from: json)
        XCTAssertEqual(context.estimatedTokens, 100)
        XCTAssertEqual(context.thresholdTokens, 750)
        XCTAssertEqual(context.cacheReadTokens, 80)
        XCTAssertEqual(context.cacheWriteTokens, 5)
        XCTAssertEqual(context.memoryBriefChars, 128)
        XCTAssertEqual(context.lastCompaction, "summary · 500 → 200 tokens")
    }

    func testMessageSendResultDecodesSnakeCaseDelivery() throws {
        let json = try XCTUnwrap("""
        {
          "delivery": {
            "kind": "inbox",
            "status": "queued",
            "message": "Queued to the agent inbox.",
            "agent_id": "agent-1",
            "identity_id": "identity-1",
            "inbox": {
              "id": "msg-1",
              "from_label": "Operator",
              "kind": "message",
              "status": "unread",
              "body": "Please check the failing test.",
              "created_at": "2026-11-22T10:00:00Z",
              "read_at": null,
              "expires_at": null
            },
            "wake": {
              "requested": true,
              "created": true,
              "continuation_id": "cont-1",
              "reason": "idle session"
            }
          }
        }
        """.data(using: .utf8))

        let result = try decoder.decode(AgentMessageSendResult.self, from: json)
        XCTAssertEqual(result.delivery.kind, "inbox")
        XCTAssertEqual(result.delivery.status, "queued")
        XCTAssertEqual(result.delivery.agentID, "agent-1")
        XCTAssertEqual(result.delivery.identityID, "identity-1")
        XCTAssertEqual(result.delivery.inbox?.body, "Please check the failing test.")
        XCTAssertEqual(result.delivery.inbox?.fromLabel, "Operator")
        XCTAssertNil(result.delivery.steering)
        XCTAssertEqual(result.delivery.wake?.continuationId, "cont-1")
        XCTAssertEqual(result.delivery.wake?.created, true)
    }

    func testMessageSendResultDecodesMinimalDelivery() throws {
        let json = try XCTUnwrap("""
        {"delivery": {"kind": "steering", "status": "pending"}}
        """.data(using: .utf8))
        let result = try decoder.decode(AgentMessageSendResult.self, from: json)
        XCTAssertEqual(result.delivery.kind, "steering")
        XCTAssertEqual(result.delivery.status, "pending")
        XCTAssertEqual(result.delivery.agentID, "")
        XCTAssertNil(result.delivery.message)
        XCTAssertNil(result.delivery.steering)
        XCTAssertNil(result.delivery.wake)
    }

    // MARK: - Error and delivery copy

    func testAgentNotActivePreservesDaemonMessageCodeAndHint() {
        let error = CoreError.rpc(
            code: -32010,
            message: "agent agent-9 is idle and has no project identity",
            data: .object([
                "code": .string("AGENT_NOT_ACTIVE"),
                "hint": .string("Start or retry the task before messaging this agent."),
            ])
        )

        XCTAssertTrue(AgentInspectRules.isAgentNotActive(error))
        let text = AgentInspectRules.errorText(error)
        XCTAssertTrue(text.contains("agent agent-9 is idle"))
        XCTAssertTrue(text.contains("AGENT_NOT_ACTIVE"))
        XCTAssertTrue(text.contains("Start or retry the task"))
    }

    func testDeliveryNoticeUsesDaemonMessageBeforeFallback() throws {
        let delivery = try decodeSendResult(
            kind: "steering",
            status: "pending",
            message: "Queued until the next provider boundary."
        ).delivery
        XCTAssertEqual(AgentInspectRules.deliveryNotice(delivery), "Queued until the next provider boundary.")
        XCTAssertFalse(AgentInspectRules.deliveryNotice(delivery).contains("Delivered"))
    }

    func testDeliveryNoticeFallbackDescribesQueueingNotDelivery() throws {
        let steering = try decodeSendResult(kind: "steering", status: "pending", message: nil)
        XCTAssertTrue(AgentInspectRules.deliveryNotice(steering.delivery).contains("next provider boundary"))

        let inbox = try decodeSendResult(kind: "inbox", status: "queued", message: nil)
        XCTAssertTrue(AgentInspectRules.deliveryNotice(inbox.delivery).contains("agent inbox"))

        let rejected = try decodeSendResult(kind: "rejected", status: "rejected", message: nil)
        XCTAssertTrue(AgentInspectRules.deliveryNotice(rejected.delivery).contains("rejected"))
    }

    func testTimelineLimitIsBounded() {
        XCTAssertEqual(AgentInspectRules.normalizedTimelineLimit(nil), nil)
        XCTAssertEqual(AgentInspectRules.normalizedTimelineLimit(0), 1)
        XCTAssertEqual(AgentInspectRules.normalizedTimelineLimit(120), 120)
        XCTAssertEqual(AgentInspectRules.normalizedTimelineLimit(10000), 200)
    }

    // MARK: - AppModel behavior

    @MainActor
    func testAppModelLoadsInspectorForSelectedSession() async throws {
        let result = try decodeInspect()
        let rpc = FakeCollabAgentRPC(inspectResult: result)
        let model = AppModel(collabAgentRPC: rpc)
        model.selectedSessionID = "ses-1"

        await model.loadCollabAgentInspect(agentID: "agent-1")

        XCTAssertEqual(model.collabAgentInspect?.agent.id, "agent-1")
        XCTAssertEqual(model.collabAgentInspectAgentID, "agent-1")
        XCTAssertFalse(model.collabAgentInspectBusy)
        XCTAssertNil(model.collabAgentInspectError)

        let call = await rpc.lastInspectCall
        XCTAssertEqual(call?.sessionID, "ses-1")
        XCTAssertEqual(call?.agentID, "agent-1")
        XCTAssertEqual(call?.timelineLimit, AgentInspectRules.defaultTimelineLimit)
    }

    @MainActor
    func testAppModelRequiresSessionAndClearsOnSessionChange() async throws {
        let result = try decodeInspect()
        let rpc = FakeCollabAgentRPC(inspectResult: result)
        let model = AppModel(collabAgentRPC: rpc)

        await model.loadCollabAgentInspect(agentID: "agent-1")
        XCTAssertNil(model.collabAgentInspect)
        XCTAssertEqual(model.collabAgentInspectError, "Select a session first.")
        let noSessionCalls = await rpc.inspectCallCount
        XCTAssertEqual(noSessionCalls, 0)

        model.selectedSessionID = "ses-1"
        await model.loadCollabAgentInspect(agentID: "agent-1")
        XCTAssertNotNil(model.collabAgentInspect)

        model.selectSession("ses-2")
        XCTAssertNil(model.collabAgentInspect)
        XCTAssertNil(model.collabAgentInspectAgentID)
        XCTAssertNil(model.collabAgentInspectError)
        XCTAssertFalse(model.collabAgentInspectBusy, "session change must not leave a spinner behind")
    }

    @MainActor
    func testAppModelDropsStaleInspectorAfterSessionChange() async throws {
        let result = try decodeInspect()
        let rpc = FakeCollabAgentRPC(
            inspectResult: result,
            inspectDelay: .milliseconds(150)
        )
        let model = AppModel(collabAgentRPC: rpc)
        model.selectedSessionID = "ses-1"

        let load = Task {
            await model.loadCollabAgentInspect(agentID: "agent-1")
        }
        var started = false
        for _ in 0 ..< 100 {
            let calls = await rpc.inspectCallCount
            if calls > 0 {
                started = true
                break
            }
            try await Task.sleep(for: .milliseconds(5))
        }
        XCTAssertTrue(started, "the inspector request should be in flight before the session changes")
        model.selectSession("ses-2")
        await load.value

        XCTAssertNil(model.collabAgentInspect, "a response for the old session must be dropped")
        XCTAssertNil(model.collabAgentInspectAgentID)
        XCTAssertFalse(model.collabAgentInspectBusy)
    }

    @MainActor
    func testAppModelSendsMessageAndRefreshesSteeringAndInspect() async throws {
        let inspectResult = try decodeInspect()
        let sendResult = try decodeSendResult(
            kind: "steering",
            status: "pending",
            message: "Queued at the next boundary."
        )
        let rpc = FakeCollabAgentRPC(
            inspectResult: inspectResult,
            sendResult: sendResult
        )
        let model = AppModel(collabAgentRPC: rpc)
        model.selectedSessionID = "ses-1"

        await model.sendCollabAgentMessage(
            agentID: "agent-1",
            body: "  Please verify the failing test.  ",
            taskID: " task-1 ",
            wakeSession: false
        )

        XCTAssertNil(model.collabAgentMessageError)
        XCTAssertEqual(model.collabAgentMessageNotice, "Queued at the next boundary.")
        XCTAssertEqual(model.collabAgentMessageDelivery?.kind, "steering")
        XCTAssertEqual(model.collabAgentMessageDelivery?.status, "pending")
        XCTAssertEqual(model.steeringMessages.count, 1, "steering is reloaded after a send")
        XCTAssertEqual(model.collabAgentInspect?.agent.id, "agent-1", "inspect is refreshed after a send")
        XCTAssertFalse(model.collabAgentMessageBusy)

        let call = await rpc.lastSendCall
        XCTAssertEqual(call?.sessionID, "ses-1")
        XCTAssertEqual(call?.agentID, "agent-1")
        XCTAssertEqual(call?.body, "Please verify the failing test.")
        XCTAssertEqual(call?.taskID, "task-1")
        XCTAssertEqual(call?.wakeSession, false)
        XCTAssertNil(call?.operationID)
        let steeringCalls = await rpc.steeringCallCount
        let inspectCalls = await rpc.inspectCallCount
        XCTAssertEqual(steeringCalls, 1)
        XCTAssertEqual(inspectCalls, 1)
    }

    @MainActor
    func testAppModelSurfacesAgentNotActiveWithoutFakeSuccess() async throws {
        let error = CoreError.rpc(
            code: -32010,
            message: "agent agent-9 is idle and has no project identity",
            data: .object([
                "code": .string("AGENT_NOT_ACTIVE"),
                "hint": .string("Start or retry the task before messaging this agent."),
            ])
        )
        let rpc = try FakeCollabAgentRPC(inspectResult: decodeInspect(), sendError: error)
        let model = AppModel(collabAgentRPC: rpc)
        model.selectedSessionID = "ses-1"

        await model.sendCollabAgentMessage(
            agentID: "agent-9",
            body: "Are you there?",
            taskID: nil,
            wakeSession: true
        )

        let message = try XCTUnwrap(model.collabAgentMessageError)
        XCTAssertTrue(message.contains("agent agent-9 is idle"))
        XCTAssertTrue(message.contains("AGENT_NOT_ACTIVE"))
        XCTAssertTrue(message.contains("Start or retry the task"))
        XCTAssertNil(model.collabAgentMessageNotice)
        XCTAssertNil(model.collabAgentMessageDelivery)
        XCTAssertTrue(model.steeringMessages.isEmpty)
        XCTAssertFalse(model.collabAgentMessageBusy)
        let inspectCalls = await rpc.inspectCallCount
        XCTAssertEqual(inspectCalls, 0, "a rejected send must not fake an inspector refresh")
    }

    @MainActor
    func testAppModelRejectsBlankBodyBeforeRPC() async throws {
        let rpc = try FakeCollabAgentRPC(inspectResult: decodeInspect())
        let model = AppModel(collabAgentRPC: rpc)
        model.selectedSessionID = "ses-1"

        await model.sendCollabAgentMessage(
            agentID: "agent-1",
            body: "   \n",
            taskID: nil,
            wakeSession: false
        )

        XCTAssertNotNil(model.collabAgentMessageError)
        XCTAssertNil(model.collabAgentMessageNotice)
        let sendCalls = await rpc.sendCallCount
        XCTAssertEqual(sendCalls, 0)
    }

    @MainActor
    func testAppModelClearResetsInspectorAndChannelState() throws {
        let model = try AppModel(collabAgentRPC: FakeCollabAgentRPC(inspectResult: decodeInspect()))
        model.collabAgentInspect = try decodeInspect()
        model.collabAgentInspectAgentID = "agent-1"
        model.collabAgentInspectError = "boom"
        model.collabAgentMessageNotice = "queued"
        model.collabAgentMessageError = "error"

        model.clearCollabAgentInspect()

        XCTAssertNil(model.collabAgentInspect)
        XCTAssertNil(model.collabAgentInspectAgentID)
        XCTAssertNil(model.collabAgentInspectError)
        XCTAssertNil(model.collabAgentMessageNotice)
        XCTAssertNil(model.collabAgentMessageError)
        XCTAssertNil(model.collabAgentMessageDelivery)
        XCTAssertFalse(model.collabAgentInspectBusy)
        XCTAssertFalse(model.collabAgentMessageBusy)
    }

    // MARK: - Helpers

    private func decodeInspect() throws -> AgentInspectResult {
        let json = try XCTUnwrap(fullInspectJSON.data(using: .utf8))
        return try decoder.decode(AgentInspectResult.self, from: json)
    }

    private func decodeSendResult(
        kind: String,
        status: String,
        message: String?
    ) throws -> AgentMessageSendResult {
        let messageValue = message.map { "\"\($0)\"" } ?? "null"
        let json = try XCTUnwrap("""
        {
          "delivery": {
            "kind": "\(kind)",
            "status": "\(status)",
            "message": \(messageValue),
            "agent_id": "agent-1"
          }
        }
        """.data(using: .utf8))
        return try decoder.decode(AgentMessageSendResult.self, from: json)
    }

    private var fullInspectJSON: String {
        """
        {
          "agent": {
            "id": "agent-1",
            "session_id": "ses-1",
            "role": "worker",
            "name": "Worker 1",
            "title": "Operator Channel Worker",
            "identity_id": "identity-1",
            "workspace_id": "wsp-1",
            "parent_workspace_id": null,
            "profile": "coding",
            "parent_id": "agent-manager-1",
            "task_id": "task-1",
            "department": "Platform",
            "worktree": "/tmp/verlinpark/worktrees/task-1",
            "branch": "verlinpark/task-1",
            "status": "running",
            "model": "deepseek-flash",
            "created_at": "2026-11-22T10:00:00Z"
          },
          "task": {
            "task_id": "task-1",
            "title": "Implement operator channel",
            "department": "Platform",
            "kind": "coding",
            "status": "running",
            "acceptance": ["Tests pass", "No cross-session target"],
            "write_scope": ["apps/macos/**"],
            "deps": ["task-0"],
            "attempts": 1,
            "max_attempts": 3,
            "verifier_status": "pending"
          },
          "context": {
            "objective": "Ship the inspector",
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
            "prefix_hash": "prefix-abc",
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
              "created_at": "2026-11-22T10:00:02Z",
              "text": "cargo test -p vp-core",
              "tool": "shell",
              "tool_call_id": "call-1",
              "status": "ok",
              "is_error": false
            },
            {
              "seq": 8,
              "kind": "agent.message",
              "category": "conversation",
              "created_at": "2026-11-22T10:00:03Z",
              "text": "A bounded failure",
              "is_error": true
            }
          ],
          "steering": [
            {
              "id": "steer-1",
              "session_id": "ses-1",
              "agent_id": "agent-1",
              "task_id": "task-1",
              "body": "Please include the failing test.",
              "status": "pending",
              "created_at": "2026-11-22T10:00:04Z",
              "delivered_at": null
            }
          ],
          "inbox": [
            {
              "id": "msg-1",
              "from_label": "Operator",
              "kind": "message",
              "status": "unread",
              "body": "Please include the failing test.",
              "created_at": "2026-11-22T10:00:04Z",
              "read_at": null,
              "expires_at": "2026-11-23T10:00:00Z"
            }
          ],
          "inbox_counts": {
            "unread": 1,
            "read": 2,
            "done": 3,
            "expired": 4
          },
          "truncated": true
        }
        """
    }
}

// MARK: - Fake transport

private actor FakeCollabAgentRPC: CollabAgentRPC {
    struct InspectCall: Sendable, Equatable {
        let sessionID: String
        let agentID: String
        let timelineLimit: Int?
    }

    struct SendCall: Sendable, Equatable {
        let sessionID: String
        let agentID: String
        let body: String
        let taskID: String?
        let wakeSession: Bool
        let operationID: String?
    }

    private let inspectResult: AgentInspectResult
    private let sendResult: AgentMessageSendResult?
    private let sendError: CoreError?
    private let inspectDelay: Duration?

    private(set) var inspectCallCount = 0
    private(set) var sendCallCount = 0
    private(set) var steeringCallCount = 0
    private(set) var lastInspectCall: InspectCall?
    private(set) var lastSendCall: SendCall?

    init(
        inspectResult: AgentInspectResult,
        sendResult: AgentMessageSendResult? = nil,
        sendError: CoreError? = nil,
        inspectDelay: Duration? = nil
    ) {
        self.inspectResult = inspectResult
        self.sendResult = sendResult
        self.sendError = sendError
        self.inspectDelay = inspectDelay
    }

    func agentInspect(
        sessionID: String,
        agentID: String,
        timelineLimit: Int?
    ) async throws -> AgentInspectResult {
        inspectCallCount += 1
        lastInspectCall = InspectCall(
            sessionID: sessionID,
            agentID: agentID,
            timelineLimit: timelineLimit
        )
        if let inspectDelay {
            try await Task.sleep(for: inspectDelay)
        }
        return inspectResult
    }

    func sendAgentMessage(
        sessionID: String,
        agentID: String,
        body: String,
        taskID: String?,
        wakeSession: Bool,
        operationID: String?
    ) async throws -> AgentMessageSendResult {
        sendCallCount += 1
        lastSendCall = SendCall(
            sessionID: sessionID,
            agentID: agentID,
            body: body,
            taskID: taskID,
            wakeSession: wakeSession,
            operationID: operationID
        )
        if let sendError {
            throw sendError
        }
        guard let sendResult else {
            throw CoreError.rpc(code: -32603, message: "missing fake send result", data: nil)
        }
        return sendResult
    }

    func sessionSteering(
        sessionID: String,
        status _: String?,
        limit _: Int?
    ) async throws -> SessionSteeringResult {
        steeringCallCount += 1
        let json = Data("""
        {
          "session_id": "\(sessionID)",
          "steering": [
            {
              "id": "steer-1",
              "session_id": "\(sessionID)",
              "agent_id": "agent-1",
              "task_id": "task-1",
              "body": "Please include the failing test.",
              "status": "pending",
              "created_at": "2026-11-22T10:00:04Z",
              "delivered_at": null
            }
          ]
        }
        """.utf8)
        return try JSONDecoder().decode(SessionSteeringResult.self, from: json)
    }
}
