@testable import VerlinPark
import XCTest

final class ProgramTests: XCTestCase {
    func testProgramListDecodesLegacyRowsWithAdditiveDefaults() throws {
        let json = """
        {
          "programs": [
            {
              "program": {
                "id": "prog-1",
                "project_id": "project-1",
                "goal": "Ship it",
                "status": "active",
                "created_at": "2026-11-11T00:00:00Z",
                "updated_at": "2026-11-11T00:00:01Z"
              }
            }
          ]
        }
        """
        let result = try JSONDecoder().decode(
            ProgramListResult.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(result.programs.count, 1)
        let entry = try XCTUnwrap(result.programs.first)
        XCTAssertEqual(entry.program.goal, "Ship it")
        XCTAssertFalse(entry.program.policy.autoDispatch)
        XCTAssertEqual(entry.program.policy.maxAttempts, 2)
        XCTAssertEqual(entry.program.policy.maxParallelMilestones, 2)
        XCTAssertFalse(entry.program.archived)
        XCTAssertEqual(entry.counts.total, 0)
    }

    func testProgramStatusDecodesMilestonesUsageAndEvents() throws {
        let json = """
        {
          "program": {
            "id": "prog-2",
            "project_id": "project-1",
            "goal": "Multi-session program",
            "status": "active",
            "policy": {
              "auto_dispatch": true,
              "max_attempts": 3,
              "retry_backoff_secs": 30,
              "dependency_failure": "retry",
              "max_parallel_milestones": 4
            },
            "token_budget": 100000,
            "cost_budget_usd": 12.5,
            "archived": false,
            "created_at": "2026-11-11T00:00:00Z",
            "updated_at": "2026-11-11T00:00:01Z"
          },
          "milestones": [
            {
              "id": "ms-1",
              "program_id": "prog-2",
              "key": "1.1",
              "title": "Foundation",
              "objective": "Build the base",
              "acceptance": ["tests pass"],
              "deps": [],
              "priority": 5,
              "status": "running",
              "attempts": 1,
              "max_attempts": 3,
              "session_id": "ses-1",
              "plan_revision": 1,
              "created_at": "2026-11-11T00:00:00Z",
              "updated_at": "2026-11-11T00:00:01Z"
            }
          ],
          "counts": {
            "pending": 0,
            "ready": 0,
            "running": 1,
            "verifying": 0,
            "completed": 0,
            "failed": 0,
            "blocked": 0,
            "cancelled": 0
          },
          "usage": {"token_spent": 321, "cost_spent_usd": 0.42},
          "events": [
            {
              "program_id": "prog-2",
              "project_id": "project-1",
              "seq": 1,
              "type": "program.created",
              "created_at": "2026-11-11T00:00:00Z"
            }
          ]
        }
        """
        let detail = try JSONDecoder().decode(
            ProgramStatusResult.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(detail.program.status, "active")
        XCTAssertTrue(detail.program.policy.autoDispatch)
        XCTAssertEqual(detail.program.policy.dependencyFailure, "retry")
        XCTAssertEqual(detail.counts.total, 1)
        XCTAssertEqual(detail.counts.active, 1)
        let milestone = try XCTUnwrap(detail.milestones.first)
        XCTAssertEqual(milestone.key, "1.1")
        XCTAssertEqual(milestone.sessionID, "ses-1")
        XCTAssertTrue(milestone.isActive)
        XCTAssertFalse(milestone.canRetry)
        XCTAssertEqual(detail.usage.tokenSpent, 321)
        XCTAssertEqual(detail.events.first?.type, "program.created")
        XCTAssertEqual(detail.events.first?.id, "prog-2-1")
    }

    func testProgramActionAvailabilityAndPolicyEncoding() throws {
        let json = """
        {
          "program": {
            "id": "prog-3",
            "project_id": "project-1",
            "goal": "Paused program",
            "status": "paused",
            "created_at": "2026-11-11T00:00:00Z",
            "updated_at": "2026-11-11T00:00:01Z"
          }
        }
        """
        let program = try JSONDecoder().decode(
            ProgramResult.self,
            from: Data(json.utf8)
        ).program
        XCTAssertTrue(program.canResume)
        XCTAssertFalse(program.canPause)
        XCTAssertFalse(program.canDelete)

        let policy = ProgramPolicy(
            autoDispatch: true,
            maxAttempts: 4,
            retryBackoffSecs: 45,
            dependencyFailure: "cancel",
            tokenBudget: 9000,
            costBudgetUsd: 3.25,
            maxParallelMilestones: 6
        )
        guard case let .object(object) = policy.jsonValue else {
            return XCTFail("policy must encode as an object")
        }
        XCTAssertEqual(object["auto_dispatch"], .bool(true))
        XCTAssertEqual(object["max_attempts"], .number(4))
        XCTAssertEqual(object["retry_backoff_secs"], .number(45))
        XCTAssertEqual(object["dependency_failure"], .string("cancel"))
        XCTAssertEqual(object["max_parallel_milestones"], .number(6))
        XCTAssertEqual(object["token_budget"], .number(9000))
        XCTAssertEqual(object["cost_budget_usd"], .number(3.25))
    }

    func testBlockedMilestoneOffersRetryAndNotStart() throws {
        let json = """
        {
          "id": "ms-blocked",
          "program_id": "prog-4",
          "key": "2",
          "title": "Blocked",
          "objective": "Needs attention",
          "status": "blocked",
          "attempts": 2,
          "max_attempts": 2,
          "created_at": "2026-11-11T00:00:00Z",
          "updated_at": "2026-11-11T00:00:01Z"
        }
        """
        let milestone = try JSONDecoder().decode(
            ProgramMilestone.self,
            from: Data(json.utf8)
        )
        XCTAssertTrue(milestone.isTerminal)
        XCTAssertTrue(milestone.canRetry)
        XCTAssertFalse(milestone.canStart)
        XCTAssertFalse(milestone.canSkip)
    }
}

final class ProgramTickTests: XCTestCase {
    func testProgramTickDecodesCountsAndReadyMilestones() throws {
        let json = """
        {
          "programs": 2,
          "milestones_advanced": 5,
          "sessions_started": 1,
          "completed": 1,
          "failed": 0,
          "ready": [
            {
              "id": "ms-ready",
              "program_id": "prog-1",
              "key": "1",
              "title": "Ready",
              "objective": "dispatch",
              "status": "ready",
              "attempts": 0,
              "max_attempts": 2,
              "created_at": "2026-11-11T00:00:00Z",
              "updated_at": "2026-11-11T00:00:00Z"
            }
          ]
        }
        """
        let tick = try JSONDecoder().decode(ProgramTickResult.self, from: Data(json.utf8))
        XCTAssertEqual(tick.programs, 2)
        XCTAssertEqual(tick.milestonesAdvanced, 5)
        XCTAssertEqual(tick.sessionsStarted, 1)
        XCTAssertEqual(tick.ready.count, 1)
        XCTAssertTrue(try XCTUnwrap(tick.ready.first).canStart)
    }

    func testProgramStatusDefaultsMissingAdditiveCollections() throws {
        let json = """
        {
          "program": {
            "id": "prog-legacy",
            "project_id": "project-1",
            "goal": "Legacy",
            "status": "draft",
            "created_at": "2026-11-11T00:00:00Z",
            "updated_at": "2026-11-11T00:00:00Z"
          }
        }
        """
        let detail = try JSONDecoder().decode(ProgramStatusResult.self, from: Data(json.utf8))
        XCTAssertTrue(detail.milestones.isEmpty)
        XCTAssertEqual(detail.counts.total, 0)
        XCTAssertEqual(detail.usage.tokenSpent, 0)
        XCTAssertTrue(detail.events.isEmpty)
    }

    func testTerminalProgramCannotResumePauseOrCancelFromTheCard() throws {
        let json = """
        {
          "program": {
            "id": "prog-done",
            "project_id": "project-1",
            "goal": "Done",
            "status": "completed",
            "archived": false,
            "created_at": "2026-11-11T00:00:00Z",
            "updated_at": "2026-11-11T00:00:00Z"
          }
        }
        """
        let program = try JSONDecoder().decode(ProgramResult.self, from: Data(json.utf8)).program
        XCTAssertTrue(program.isTerminal)
        XCTAssertFalse(program.canPause)
        XCTAssertFalse(program.canResume)
        XCTAssertTrue(program.canDelete)
    }
}
