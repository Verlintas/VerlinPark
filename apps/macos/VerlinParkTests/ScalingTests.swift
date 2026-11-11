@testable import VerlinPark
import XCTest

final class ScalingTests: XCTestCase {
    func testLegacyPolicyDecodesDisabledWithBoundedDefaults() throws {
        let policy = try JSONDecoder().decode(
            ScalingPolicy.self,
            from: Data(#"{"enabled":true}"#.utf8)
        )
        XCTAssertTrue(policy.enabled)
        XCTAssertEqual(policy.minWorkers, 1)
        XCTAssertEqual(policy.maxWorkers, 4)
        XCTAssertEqual(policy.minLabour, 1)
        XCTAssertEqual(policy.maxLabour, 8)
        XCTAssertEqual(policy.expandReadyThreshold, 2)
        XCTAssertEqual(policy.expandCriticalDelaySecs, 120)
        XCTAssertEqual(policy.scaleDownIdleSecs, 600)
        XCTAssertEqual(policy.cooldownSecs, 120)
        XCTAssertEqual(policy.maxStep, 2)
        XCTAssertNil(policy.budgetHeadroomRatio)
        XCTAssertFalse(ScalingPolicy.disabled.enabled)
    }

    func testPolicyEncodesEveryControlAsSnakeCase() {
        let policy = ScalingPolicy(
            enabled: true,
            minWorkers: 2,
            maxWorkers: 6,
            minLabour: 3,
            maxLabour: 12,
            expandReadyThreshold: 4,
            expandCriticalDelaySecs: 30,
            scaleDownIdleSecs: 900,
            cooldownSecs: 45,
            maxStep: 3,
            budgetHeadroomRatio: 0.25
        )
        guard case let .object(object) = policy.jsonValue else {
            return XCTFail("scaling policy must encode as an object")
        }
        XCTAssertEqual(object["enabled"], .bool(true))
        XCTAssertEqual(object["min_workers"], .number(2))
        XCTAssertEqual(object["max_workers"], .number(6))
        XCTAssertEqual(object["min_labour"], .number(3))
        XCTAssertEqual(object["max_labour"], .number(12))
        XCTAssertEqual(object["expand_ready_threshold"], .number(4))
        XCTAssertEqual(object["expand_critical_delay_secs"], .number(30))
        XCTAssertEqual(object["scale_down_idle_secs"], .number(900))
        XCTAssertEqual(object["cooldown_secs"], .number(45))
        XCTAssertEqual(object["max_step"], .number(3))
        XCTAssertEqual(object["budget_headroom_ratio"], .number(0.25))
    }

    func testDecisionAndStatusDecodeWithAdditiveDefaults() throws {
        let previewJSON = """
        {
          "session_id": "ses-1",
          "project_id": "proj-1",
          "policy": {"enabled": true, "max_workers": 6},
          "decision": {
            "decision_id": "dec-1",
            "decision_key": "rev2:tick1",
            "session_id": "ses-1",
            "project_id": "proj-1",
            "plan_revision": 2,
            "action": "expand_workers",
            "current_workers": 2,
            "current_labour": 2,
            "target_workers": 4,
            "target_labour": 2,
            "active_workers": 2,
            "active_labour": 1,
            "ready_tasks": 5,
            "running_tasks": 2,
            "inputs_hash": "abcdef012345",
            "reasons": ["ready_tasks:5", "expand_workers:2->4"],
            "manager_recommended": true,
            "manager_evidence": "a new department would help",
            "applied": true,
            "created_at": "2026-11-11T00:00:00Z"
          }
        }
        """
        let preview = try JSONDecoder().decode(
            ScalingPreviewResult.self,
            from: Data(previewJSON.utf8)
        )
        XCTAssertEqual(preview.sessionID, "ses-1")
        XCTAssertEqual(preview.policy.maxWorkers, 6)
        XCTAssertEqual(preview.decision.action, "expand_workers")
        XCTAssertTrue(preview.decision.isExpansion)
        XCTAssertFalse(preview.decision.isScaleDown)
        XCTAssertTrue(preview.decision.applied)
        XCTAssertTrue(preview.decision.managerRecommended)
        XCTAssertEqual(preview.decision.reasons.count, 2)
        XCTAssertEqual(preview.decision.summary, "expand_workers · Workers 2→4 · Labour 2→2")

        let statusJSON = """
        {
          "global_worker_cap": 12,
          "global_labour_cap": 24,
          "global_worker_available": 9,
          "global_labour_available": 20,
          "projects": [{
            "project_id": "proj-1",
            "project_name": "demo",
            "policy": {"enabled": true},
            "sessions": [{
              "session_id": "ses-1",
              "project_id": "proj-1",
              "goal": "ship",
              "status": "idle",
              "plan_revision": 2,
              "plan_tasks": 6,
              "ready_tasks": 3,
              "running_tasks": 0,
              "completed_tasks": 2,
              "current_workers": 3,
              "current_labour": 5,
              "active_workers": 0,
              "active_labour": 0
            }]
          }]
        }
        """
        let status = try JSONDecoder().decode(
            ScalingStatusResult.self,
            from: Data(statusJSON.utf8)
        )
        XCTAssertEqual(status.globalWorkerAvailable, 9)
        XCTAssertEqual(status.projects.count, 1)
        XCTAssertEqual(status.projects[0].sessions.count, 1)
        let session = try XCTUnwrap(status.projects[0].sessions.first)
        XCTAssertEqual(session.currentWorkers, 3)
        XCTAssertEqual(session.currentLabour, 5)
        XCTAssertNil(session.latest)
        XCTAssertEqual(session.readyTasks, 3)
    }

    func testScaleDownDecisionAndLegacyStatusCollectionsDefault() throws {
        let decision = try JSONDecoder().decode(
            ScalingDecision.self,
            from: Data(
                """
                {
                  "action": "scale_down_labour",
                  "current_workers": 4,
                  "current_labour": 6,
                  "target_workers": 4,
                  "target_labour": 4
                }
                """.utf8
            )
        )
        XCTAssertTrue(decision.isScaleDown)
        XCTAssertFalse(decision.isExpansion)
        XCTAssertFalse(decision.applied)
        XCTAssertTrue(decision.reasons.isEmpty)
        XCTAssertEqual(decision.targetLabour, 4)

        let status = try JSONDecoder().decode(
            ScalingStatusResult.self,
            from: Data("{}".utf8)
        )
        XCTAssertEqual(status.globalWorkerCap, 0)
        XCTAssertTrue(status.projects.isEmpty)
    }
}

final class ScalingStatusCompatibilityTests: XCTestCase {
    func testProjectStatusDefaultsMissingPolicyAndSessions() throws {
        let json = """
        {"global_worker_cap": 12, "global_labour_cap": 24, "projects": [{"project_id": "p1", "project_name": "demo"}]}
        """
        let status = try JSONDecoder().decode(
            ScalingStatusResult.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(status.globalWorkerCap, 12)
        XCTAssertEqual(status.projects.count, 1)
        XCTAssertFalse(status.projects[0].policy.enabled)
        XCTAssertTrue(status.projects[0].sessions.isEmpty)
    }

    func testSessionStatusDecodesWithoutLatestDecision() throws {
        let json = """
        {
          "session_id": "s1",
          "project_id": "p1",
          "status": "running",
          "current_workers": 3,
          "current_labour": 4,
          "active_workers": 2,
          "active_labour": 1,
          "ready_tasks": 5
        }
        """
        let session = try JSONDecoder().decode(
            ScalingSessionStatus.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(session.sessionID, "s1")
        XCTAssertEqual(session.planTasks, 0)
        XCTAssertEqual(session.readyTasks, 5)
        XCTAssertEqual(session.activeWorkers, 2)
        XCTAssertNil(session.latest)
        XCTAssertNil(session.idleSinceWorkers)
        XCTAssertEqual(session.status, "running")
    }

    func testDecisionLegacyDefaultsKeepHoldSemantics() throws {
        let decision = try JSONDecoder().decode(
            ScalingDecision.self,
            from: Data("{}".utf8)
        )
        XCTAssertEqual(decision.action, "hold")
        XCTAssertFalse(decision.isExpansion)
        XCTAssertFalse(decision.isScaleDown)
        XCTAssertEqual(decision.targetWorkers, 0)
        XCTAssertTrue(decision.reasons.isEmpty)
    }
}
