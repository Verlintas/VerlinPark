import Foundation
@testable import VerlinPark
import XCTest

final class LongTaskConsoleTests: XCTestCase {
    func testRecoveryDecodesFullWireShapeAndActiveHelpers() throws {
        let json = """
        {
          "session_id": "session-1",
          "project_id": "project-1",
          "plan_seq": 7,
          "task_id": "task-1",
          "department": "api",
          "action": "retry",
          "status": "requested",
          "attempts": 1,
          "last_stall_count": 2,
          "requested_at": "2026-11-11T01:00:00Z",
          "updated_at": "2026-11-11T01:05:00Z",
          "last_error": "provider stalled",
          "next_action_at": "2026-11-11T01:10:00Z"
        }
        """
        let recovery = try JSONDecoder().decode(
            OrchestrationRecovery.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(recovery.taskId, "task-1")
        XCTAssertEqual(recovery.department, "api")
        XCTAssertEqual(recovery.action, "retry")
        XCTAssertEqual(recovery.attempts, 1)
        XCTAssertEqual(recovery.lastStallCount, 2)
        XCTAssertEqual(recovery.nextActionAt, "2026-11-11T01:10:00Z")
        XCTAssertTrue(recovery.isActiveRecovery)
        XCTAssertFalse(recovery.isTerminal)
        XCTAssertEqual(recovery.id, "session-1/7/task-1")
    }

    func testRecoveryTruthfullyReportsTerminalStates() throws {
        let json = """
        {
          "session_id": "session-1",
          "project_id": "project-1",
          "plan_seq": 3,
          "task_id": "task-2",
          "department": "web",
          "action": "escalate",
          "status": "unnecessary",
          "attempts": 0,
          "last_stall_count": 1,
          "requested_at": "2026-11-11T01:00:00Z",
          "updated_at": "2026-11-11T01:01:00Z"
        }
        """
        let recovery = try JSONDecoder().decode(
            OrchestrationRecovery.self,
            from: Data(json.utf8)
        )
        XCTAssertNil(recovery.lastError)
        XCTAssertNil(recovery.nextActionAt)
        XCTAssertFalse(recovery.isActiveRecovery)
        XCTAssertTrue(recovery.isTerminal)
    }

    func testRecoveriesResultDefaultsMissingRows() throws {
        let result = try JSONDecoder().decode(
            OrchestrationRecoveriesResult.self,
            from: Data(#"{"session_id":"session-1"}"#.utf8)
        )
        XCTAssertEqual(result.sessionId, "session-1")
        XCTAssertTrue(result.recoveries.isEmpty)
    }

    func testContinuationDecodesAllFields() throws {
        let json = """
        {
          "id": "continuation-1",
          "session_id": "session-1",
          "project_id": "project-1",
          "body": "Summarize the finished tasks.",
          "watch_task_ids": ["task-1", "task-2"],
          "watch_mode": "any",
          "deadline_at": "2026-11-12T02:00:00Z",
          "status": "ready",
          "attempts": 1,
          "max_attempts": 3,
          "wake_reason": "watch",
          "outcomes": [
            {"task_id": "task-1", "title": "API", "status": "completed"}
          ],
          "last_error": "session still running",
          "next_attempt_at": "2026-11-11T02:05:00Z",
          "created_at": "2026-11-11T01:00:00Z",
          "ready_at": "2026-11-11T02:00:00Z",
          "delivered_at": null,
          "updated_at": "2026-11-11T02:00:01Z"
        }
        """
        let continuation = try JSONDecoder().decode(
            SessionContinuation.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(continuation.watchTaskIds, ["task-1", "task-2"])
        XCTAssertEqual(continuation.watchMode, "any")
        XCTAssertEqual(continuation.deadlineAt, "2026-11-12T02:00:00Z")
        XCTAssertEqual(continuation.attempts, 1)
        XCTAssertEqual(continuation.maxAttempts, 3)
        XCTAssertEqual(continuation.wakeReason, "watch")
        XCTAssertEqual(continuation.outcomes.first?.title, "API")
        XCTAssertEqual(continuation.lastError, "session still running")
        XCTAssertEqual(continuation.readyAt, "2026-11-11T02:00:00Z")
        XCTAssertNil(continuation.deliveredAt)
        XCTAssertTrue(continuation.isPending)
        XCTAssertFalse(continuation.isTerminal)
        XCTAssertTrue(continuation.isCancelable)
    }

    func testContinuationDecodesWithoutOptionalAndDefaultedFields() throws {
        let json = """
        {
          "id": "continuation-2",
          "session_id": "session-1",
          "project_id": "project-1",
          "body": "Check the build later.",
          "status": "pending",
          "created_at": "2026-11-11T01:00:00Z"
        }
        """
        let continuation = try JSONDecoder().decode(
            SessionContinuation.self,
            from: Data(json.utf8)
        )
        XCTAssertTrue(continuation.watchTaskIds.isEmpty)
        XCTAssertEqual(continuation.watchMode, "all")
        XCTAssertEqual(continuation.attempts, 0)
        XCTAssertEqual(continuation.maxAttempts, 3)
        XCTAssertTrue(continuation.outcomes.isEmpty)
        XCTAssertNil(continuation.deadlineAt)
        XCTAssertNil(continuation.wakeReason)
        XCTAssertNil(continuation.lastError)
        XCTAssertNil(continuation.nextAttemptAt)
        XCTAssertNil(continuation.readyAt)
        XCTAssertNil(continuation.deliveredAt)
        XCTAssertEqual(continuation.updatedAt, continuation.createdAt)
    }

    func testContinuationTerminalAndDeliveryHelpers() {
        XCTAssertFalse(
            continuation(status: "pending").isTerminal
        )
        XCTAssertTrue(continuation(status: "delivering").isCancelable)
        XCTAssertTrue(continuation(status: "delivered").isTerminal)
        XCTAssertTrue(continuation(status: "cancelled").isTerminal)
        XCTAssertTrue(continuation(status: "failed").isTerminal)
        XCTAssertFalse(continuation(status: "failed").isCancelable)
        XCTAssertTrue(continuation(status: "pending").isStartable(sessionStatus: "idle"))
        XCTAssertFalse(continuation(status: "pending").isStartable(sessionStatus: "running"))
    }

    func testContinuationsResultDefaultsMissingRows() throws {
        let result = try JSONDecoder().decode(
            SessionContinuationsResult.self,
            from: Data(#"{"session_id":"session-1"}"#.utf8)
        )
        XCTAssertTrue(result.continuations.isEmpty)
    }

    func testSteeringDecodesTargetAndOptionalFields() throws {
        let json = """
        {
          "id": "steering-1",
          "session_id": "session-1",
          "agent_id": "agent-1",
          "body": "Run the focused test first.",
          "status": "pending",
          "created_at": "2026-11-11T01:00:00Z"
        }
        """
        let steering = try JSONDecoder().decode(
            SteeringMessage.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(steering.agentId, "agent-1")
        XCTAssertNil(steering.taskId)
        XCTAssertNil(steering.deliveredAt)
        XCTAssertTrue(steering.isPending)
        XCTAssertFalse(steering.isTerminal)

        let deliveredJSON = """
        {
          "id": "steering-2",
          "session_id": "session-1",
          "agent_id": "agent-2",
          "task_id": "task-1",
          "body": "Stop after this turn.",
          "status": "delivered",
          "created_at": "2026-11-11T01:00:00Z",
          "delivered_at": "2026-11-11T01:02:00Z"
        }
        """
        let delivered = try JSONDecoder().decode(
            SteeringMessage.self,
            from: Data(deliveredJSON.utf8)
        )
        XCTAssertEqual(delivered.taskId, "task-1")
        XCTAssertEqual(delivered.deliveredAt, "2026-11-11T01:02:00Z")
        XCTAssertFalse(delivered.isPending)
        XCTAssertTrue(delivered.isTerminal)
    }

    func testSessionStallsDecodeProgressAndObservations() throws {
        let json = """
        {
          "session_id": "session-1",
          "threshold_secs": 1800,
          "progress": [
            {
              "session_id": "session-1",
              "project_id": "project-1",
              "plan_seq": 4,
              "task_id": "task-1",
              "last_progress_at": "2026-11-11T01:20:00Z",
              "stall_count": 2,
              "last_turn": 9,
              "last_tool": "shell",
              "updated_at": "2026-11-11T01:20:00Z"
            }
          ],
          "stalls": [
            {
              "session_id": "session-1",
              "project_id": "project-1",
              "plan_seq": 4,
              "task_id": "task-1",
              "department": "api",
              "title": "Implement endpoint",
              "status": "running",
              "attempts": 1,
              "age_secs": 2400,
              "stall_count": 2,
              "last_progress_at": "2026-11-11T01:20:00Z",
              "last_tool": "shell",
              "observed_at": "2026-11-11T02:00:00Z"
            }
          ]
        }
        """
        let result = try JSONDecoder().decode(
            SessionStallsResult.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(result.thresholdSecs, 1800)
        XCTAssertEqual(result.progress.first?.lastTurn, 9)
        XCTAssertEqual(result.progress.first?.lastTool, "shell")
        XCTAssertEqual(result.stalls.first?.ageSecs, 2400)
        XCTAssertEqual(result.stalls.first?.attempts, 1)
        XCTAssertNil(result.stalls.first?.lastTurn)
        XCTAssertEqual(result.stalls.first?.lastTool, "shell")
        XCTAssertEqual(result.stalls.first?.id, "session-1/4/task-1/2026-11-11T02:00:00Z")
    }

    func testSessionStallsDefaultsMissingObservationArrays() throws {
        let result = try JSONDecoder().decode(
            SessionStallsResult.self,
            from: Data(#"{"session_id":"session-1"}"#.utf8)
        )
        XCTAssertEqual(result.thresholdSecs, 0)
        XCTAssertTrue(result.progress.isEmpty)
        XCTAssertTrue(result.stalls.isEmpty)
    }

    func testLongTaskSessionAndTaskStatusHelpers() {
        XCTAssertTrue(LongTaskRules.isSessionStartable("idle"))
        XCTAssertTrue(LongTaskRules.isSessionStartable("failed"))
        XCTAssertTrue(LongTaskRules.isSessionStartable("cancelled"))
        XCTAssertFalse(LongTaskRules.isSessionStartable("running"))
        XCTAssertFalse(LongTaskRules.isSessionStartable("paused"))
        XCTAssertFalse(LongTaskRules.isSessionStartable("queued"))

        XCTAssertTrue(LongTaskRules.isTerminalTaskStatus("completed"))
        XCTAssertTrue(LongTaskRules.isTerminalTaskStatus("interrupted"))
        XCTAssertFalse(LongTaskRules.isTerminalTaskStatus("blocked"))
        XCTAssertFalse(LongTaskRules.isTerminalTaskStatus("running"))
    }

    func testSteeringValidationTrimsAndBoundsBody() {
        switch LongTaskRules.validateSteeringBody("  focus on tests  ") {
        case let .success(body):
            XCTAssertEqual(body, "focus on tests")
        case let .failure(error):
            XCTFail("expected success, got \(error)")
        }

        XCTAssertEqual(
            LongTaskRules.validateSteeringBody(" \n "),
            .failure(.emptyBody)
        )
        XCTAssertEqual(
            LongTaskRules.validateSteeringBody(String(repeating: "x", count: 4001)),
            .failure(.bodyTooLong)
        )
    }

    func testDeferValidationBuildsWatchOnlyPayload() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let result = LongTaskRules.validateDefer(
            body: "  summarize the finished work  ",
            watchTaskIDs: [" task-b ", "task-a", "task-a", ""],
            watchMode: "ALL",
            afterMinutes: 60,
            deadline: nil,
            now: now
        )
        guard case let .success(payload) = result else {
            return XCTFail("expected a valid defer payload")
        }
        XCTAssertEqual(payload.body, "summarize the finished work")
        XCTAssertEqual(payload.watchTaskIDs, ["task-b", "task-a"])
        XCTAssertEqual(payload.watchMode, "all")
        XCTAssertEqual(payload.afterSecs, 3600)
        XCTAssertNil(payload.deadlineAt)
    }

    func testDeferValidationFormatsAbsoluteDeadline() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let deadline = now.addingTimeInterval(10 * 24 * 60 * 60)
        let result = LongTaskRules.validateDefer(
            body: "continue",
            watchTaskIDs: [],
            watchMode: "any",
            afterMinutes: nil,
            deadline: deadline,
            now: now
        )
        guard case let .success(payload) = result else {
            return XCTFail("expected a valid deadline payload")
        }
        XCTAssertNil(payload.afterSecs)
        XCTAssertEqual(payload.watchMode, "any")
        guard let deadlineAt = payload.deadlineAt else {
            return XCTFail("expected an RFC3339 deadline")
        }
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        XCTAssertEqual(formatter.date(from: deadlineAt), deadline)
    }

    func testDeferValidationRejectsUnsafeInputs() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        XCTAssertEqual(
            LongTaskRules.validateDefer(
                body: "",
                watchTaskIDs: ["task-1"],
                watchMode: "all",
                afterMinutes: nil,
                deadline: nil,
                now: now
            ),
            .failure(.emptyBody)
        )
        XCTAssertEqual(
            LongTaskRules.validateDefer(
                body: "continue",
                watchTaskIDs: [],
                watchMode: "all",
                afterMinutes: nil,
                deadline: nil,
                now: now
            ),
            .failure(.emptyWakeCondition)
        )
        XCTAssertEqual(
            LongTaskRules.validateDefer(
                body: "continue",
                watchTaskIDs: ["task-1"],
                watchMode: "sometimes",
                afterMinutes: nil,
                deadline: nil,
                now: now
            ),
            .failure(.invalidWatchMode)
        )
        XCTAssertEqual(
            LongTaskRules.validateDefer(
                body: "continue",
                watchTaskIDs: ["task-1"],
                watchMode: "all",
                afterMinutes: 0,
                deadline: nil,
                now: now
            ),
            .failure(.invalidAfterMinutes)
        )
        XCTAssertEqual(
            LongTaskRules.validateDefer(
                body: "continue",
                watchTaskIDs: ["task-1"],
                watchMode: "all",
                afterMinutes: 43201,
                deadline: nil,
                now: now
            ),
            .failure(.invalidAfterMinutes)
        )
        XCTAssertEqual(
            LongTaskRules.validateDefer(
                body: "continue",
                watchTaskIDs: ["task-1"],
                watchMode: "all",
                afterMinutes: nil,
                deadline: now.addingTimeInterval(-1),
                now: now
            ),
            .failure(.deadlineInPast)
        )
        XCTAssertEqual(
            LongTaskRules.validateDefer(
                body: "continue",
                watchTaskIDs: ["task-1"],
                watchMode: "all",
                afterMinutes: nil,
                deadline: now.addingTimeInterval(31 * 24 * 60 * 60),
                now: now
            ),
            .failure(.deadlineTooFar)
        )
    }

    private func continuation(status: String) -> SessionContinuation {
        let json = """
        {
          "id": "continuation-\(status)",
          "session_id": "session-1",
          "project_id": "project-1",
          "body": "continue",
          "status": "\(status)",
          "created_at": "2026-11-11T01:00:00Z"
        }
        """
        do {
            return try JSONDecoder().decode(
                SessionContinuation.self,
                from: Data(json.utf8)
            )
        } catch {
            fatalError("failed to decode fixture: \(error)")
        }
    }
}
