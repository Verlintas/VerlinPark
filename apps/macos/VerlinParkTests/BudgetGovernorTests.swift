import Foundation
@testable import VerlinPark
import XCTest

final class BudgetGovernorTests: XCTestCase {
    func testPolicyDecodesDefaultsAndClampsNothingClientSide() throws {
        let policy = try JSONDecoder().decode(
            CriticalPathPolicy.self,
            from: Data("{}".utf8)
        )
        XCTAssertFalse(policy.enabled)
        XCTAssertEqual(policy.reserveRatio, 0.25, accuracy: 0.0001)
        XCTAssertEqual(policy.throttleAtRatio, 0.35, accuracy: 0.0001)
        XCTAssertEqual(policy.criticalBoost, 25)
        XCTAssertEqual(policy.minTaskBudget, 1000)

        let explicit = try JSONDecoder().decode(
            CriticalPathPolicy.self,
            from: Data(
                """
                {
                  "enabled": true,
                  "reserve_ratio": 0.4,
                  "throttle_at_ratio": 0.2,
                  "critical_boost": 10,
                  "min_task_budget": 2000
                }
                """.utf8
            )
        )
        XCTAssertTrue(explicit.enabled)
        XCTAssertEqual(explicit.reserveRatio, 0.4, accuracy: 0.0001)
        XCTAssertEqual(explicit.minTaskBudget, 2000)
    }

    func testPolicyRoundTripsThroughJSONValue() throws {
        let policy = CriticalPathPolicy(
            enabled: true,
            reserveRatio: 0.5,
            throttleAtRatio: 0.1,
            criticalBoost: 5,
            minTaskBudget: 3000
        )
        let data = try JSONEncoder().encode(policy.jsonValue())
        let decoded = try JSONDecoder().decode(CriticalPathPolicy.self, from: data)
        XCTAssertEqual(decoded, policy)
    }

    func testGovernorStatusDecodesEmptyPayloadWithDefaults() throws {
        let status = try JSONDecoder().decode(
            GovernorSessionStatus.self,
            from: Data("{}".utf8)
        )
        XCTAssertEqual(status.sessionID, "")
        XCTAssertFalse(status.enabled)
        XCTAssertFalse(status.planPresent)
        XCTAssertNil(status.criticalPath)
        XCTAssertNil(status.error)
        XCTAssertEqual(status.reserve.state, "disabled")
        XCTAssertTrue(status.decisions.isEmpty)
        XCTAssertTrue(status.deferred.isEmpty)
        XCTAssertTrue(status.lastDecisions.isEmpty)
    }

    func testGovernorStatusDecodesPathReserveAndDecisions() throws {
        let status = try JSONDecoder().decode(
            GovernorSessionStatus.self,
            from: Data(
                """
                {
                  "session_id": "s1",
                  "project_id": "p1",
                  "session_status": "idle",
                  "plan_revision": 2,
                  "enabled": true,
                  "unlimited": false,
                  "plan_present": true,
                  "policy": {"enabled": true, "reserve_ratio": 0.25},
                  "critical_path": {
                    "plan_revision": 2,
                    "task_ids": ["a", "b"],
                    "weight_tokens": 5000,
                    "remaining_budget_tokens": 10000,
                    "complete": false,
                    "task_count": 2,
                    "edge_count": 1,
                    "inputs_hash": "hash"
                  },
                  "reserve": {
                    "state": "active",
                    "reserve_ratio": 0.25,
                    "throttle_at_ratio": 0.35,
                    "critical_boost": 25,
                    "min_task_budget": 1000,
                    "limit_tokens": 10000,
                    "spent_tokens": 0,
                    "reserved_tokens": 0,
                    "remaining_tokens": 10000,
                    "path_weight_tokens": 5000,
                    "ratio_reserve_tokens": 2500,
                    "protected_tokens": 7500,
                    "headroom_tokens": 2500,
                    "throttled": false
                  },
                  "decisions": [
                    {
                      "task_id": "a",
                      "department": "impl",
                      "critical": true,
                      "weight_tokens": 5000,
                      "order": 0,
                      "action": "admit",
                      "reason": "critical_admit",
                      "code": "",
                      "remaining_after_tokens": 5000
                    }
                  ],
                  "admitted": ["a"],
                  "deferred": [
                    {
                      "task_id": "side",
                      "department": "impl",
                      "critical": false,
                      "weight_tokens": 8000,
                      "reason": "reserve",
                      "code": "BUDGET_RESERVED",
                      "remaining_tokens": 5000,
                      "protected_tokens": 7500
                    }
                  ],
                  "last_decisions": [
                    {
                      "decision_id": "gov-1",
                      "session_id": "s1",
                      "action": "admission",
                      "reason": "reserve",
                      "detail": "path [a -> b]",
                      "created_at": "2026-11-11T00:00:00Z"
                    }
                  ],
                  "generated_at": "2026-11-11T00:00:00Z"
                }
                """.utf8
            )
        )
        XCTAssertEqual(status.sessionID, "s1")
        XCTAssertTrue(status.enabled)
        XCTAssertTrue(status.planPresent)
        XCTAssertEqual(status.criticalPath?.taskIDs, ["a", "b"])
        XCTAssertEqual(status.criticalPath?.weightTokens, 5000)
        XCTAssertEqual(status.reserve.protectedTokens, 7500)
        XCTAssertEqual(status.reserve.remainingTokens, 10000)
        XCTAssertEqual(status.decisions.count, 1)
        XCTAssertEqual(status.decisions[0].reason, "critical_admit")
        XCTAssertEqual(status.deferred[0].code, "BUDGET_RESERVED")
        XCTAssertEqual(status.lastDecisions.last?.action, "admission")
        XCTAssertEqual(status.lastDecisions.last?.detail, "path [a -> b]")
    }

    func testGovernorFailureDecodes() throws {
        let status = try JSONDecoder().decode(
            GovernorSessionStatus.self,
            from: Data(
                """
                {
                  "session_id": "s1",
                  "enabled": true,
                  "error": {
                    "code": "BUDGET_PATH_CYCLE",
                    "message": "cycle",
                    "retryable": false,
                    "hint": "fix the DAG"
                  }
                }
                """.utf8
            )
        )
        XCTAssertEqual(status.error?.code, "BUDGET_PATH_CYCLE")
        XCTAssertEqual(status.error?.retryable, false)
        XCTAssertEqual(status.error?.hint, "fix the DAG")
    }

    func testGovernorMetricsFormatTheReserve() {
        XCTAssertEqual(GovernorMetrics.percent(0.25), "25%")
        XCTAssertEqual(GovernorMetrics.percent(nil), "—")
        XCTAssertEqual(GovernorMetrics.percent(2.0), "100%")
        XCTAssertEqual(GovernorMetrics.pathLabel(["a", "b", "c"]), "a → b → c")
        XCTAssertEqual(GovernorMetrics.pathLabel([]), "complete")
        XCTAssertEqual(
            GovernorMetrics.pathLabel(["a", "b", "c", "d", "e", "f", "g"]),
            "a → b → c → d → e → f → +1"
        )

        let reserve = GovernorReserve()
        XCTAssertNil(GovernorMetrics.reserveFraction(reserve))
    }

    func testGovernorPressureAndReserveFraction() throws {
        let status = try JSONDecoder().decode(
            GovernorSessionStatus.self,
            from: Data(
                """
                {
                  "session_id": "s1",
                  "enabled": true,
                  "reserve": {
                    "state": "throttled",
                    "throttled": true,
                    "remaining_tokens": 4000,
                    "protected_tokens": 3000
                  }
                }
                """.utf8
            )
        )
        XCTAssertTrue(GovernorMetrics.isPressure(status.reserve))
        XCTAssertEqual(
            GovernorMetrics.reserveFraction(status.reserve) ?? 0,
            0.75,
            accuracy: 0.0001
        )
        XCTAssertEqual(GovernorMetrics.lastDecision(status), nil)
    }

    func testEfficiencySnapshotCarriesTheGovernor() throws {
        let status = try JSONDecoder().decode(
            GovernorSessionStatus.self,
            from: Data(
                """
                {
                  "session_id": "s1",
                  "enabled": true,
                  "plan_present": true,
                  "reserve": {"state": "throttled", "throttled": true, "protected_tokens": 10},
                  "critical_path": {"task_ids": ["a"], "weight_tokens": 100}
                }
                """.utf8
            )
        )
        let snapshot = EfficiencyMetrics.makeSnapshot(
            projectUsage: nil,
            usage: nil,
            context: nil,
            cache: nil,
            toolOutputs: nil,
            governor: status,
            eventTypes: [],
            latestPrefixHash: nil
        )
        XCTAssertNotNil(snapshot)
        XCTAssertEqual(snapshot?.governor?.sessionID, "s1")
        XCTAssertTrue(snapshot?.governor?.reserve.throttled ?? false)
    }
}
