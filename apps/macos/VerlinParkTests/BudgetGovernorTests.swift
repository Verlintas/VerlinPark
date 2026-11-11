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
        XCTAssertEqual(policy.mode, "single")
        XCTAssertEqual(policy.maxPaths, 4)
        XCTAssertEqual(policy.departmentMinRatio, 0, accuracy: 0.0001)
        XCTAssertEqual(policy.observedWeightRatio, 0, accuracy: 0.0001)
        XCTAssertEqual(policy.starvationAfterSecs, 1800)
        XCTAssertFalse(policy.isMultiPath)

        let explicit = try JSONDecoder().decode(
            CriticalPathPolicy.self,
            from: Data(
                """
                {
                  "enabled": true,
                  "reserve_ratio": 0.4,
                  "throttle_at_ratio": 0.2,
                  "critical_boost": 10,
                  "min_task_budget": 2000,
                  "mode": "multi",
                  "max_paths": 6,
                  "department_min_ratio": 0.2,
                  "observed_weight_ratio": 0.5,
                  "starvation_after_secs": 600
                }
                """.utf8
            )
        )
        XCTAssertTrue(explicit.enabled)
        XCTAssertEqual(explicit.reserveRatio, 0.4, accuracy: 0.0001)
        XCTAssertEqual(explicit.minTaskBudget, 2000)
        XCTAssertEqual(explicit.mode, "multi")
        XCTAssertEqual(explicit.maxPaths, 6)
        XCTAssertEqual(explicit.departmentMinRatio, 0.2, accuracy: 0.0001)
        XCTAssertEqual(explicit.observedWeightRatio, 0.5, accuracy: 0.0001)
        XCTAssertEqual(explicit.starvationAfterSecs, 600)
        XCTAssertTrue(explicit.isMultiPath)
    }

    func testPolicyRoundTripsThroughJSONValue() throws {
        let policy = CriticalPathPolicy(
            enabled: true,
            reserveRatio: 0.5,
            throttleAtRatio: 0.1,
            criticalBoost: 5,
            minTaskBudget: 3000,
            mode: "multi",
            maxPaths: 8,
            departmentMinRatio: 0.15,
            observedWeightRatio: 0.25,
            starvationAfterSecs: 900
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

final class BudgetGovernorFairnessTests: XCTestCase {
    func testMultiStatusDecodesPathsAndReserveFairness() throws {
        let status = try JSONDecoder().decode(
            GovernorSessionStatus.self,
            from: Data(
                """
                {
                  "session_id": "s1",
                  "project_id": "p1",
                  "enabled": true,
                  "plan_present": true,
                  "policy": {"enabled": true, "mode": "multi", "max_paths": 6},
                  "reserve": {
                    "state": "active",
                    "mode": "multi",
                    "path_count": 2,
                    "protected_tokens": 10000,
                    "allocated_tokens": 9000,
                    "floor_tokens": 500,
                    "remaining_tokens": 10000
                  },
                  "paths": [
                    {
                      "path_id": 0,
                      "task_ids": ["heavy", "tail"],
                      "departments": ["impl"],
                      "weight_tokens": 10000,
                      "declared_weight_tokens": 9000,
                      "observed_tokens": 2000,
                      "floor_tokens": 400,
                      "allocation_tokens": 6000,
                      "deficit_tokens": 4000
                    },
                    {
                      "path_id": 1,
                      "task_ids": ["verify"],
                      "departments": ["qa"],
                      "weight_tokens": 4000,
                      "floor_tokens": 100,
                      "allocation_tokens": 3000,
                      "deficit_tokens": 1000
                    }
                  ],
                  "departments": [
                    {"department": "impl", "weight_tokens": 10000, "floor_tokens": 400, "path_ids": [0]},
                    {"department": "qa", "weight_tokens": 4000, "floor_tokens": 100, "path_ids": [1]}
                  ],
                  "deferred_order": ["side", "lint"],
                  "deferred": [{
                    "task_id": "side",
                    "reason": "reserve",
                    "code": "BUDGET_RESERVED",
                    "path_id": 1,
                    "path_deficit_tokens": 1000,
                    "starvation_credit": 5
                  }],
                  "generated_at": "2026-11-12T00:00:00Z"
                }
                """.utf8
            )
        )
        XCTAssertEqual(status.reserve.mode, "multi")
        XCTAssertEqual(status.reserve.pathCount, 2)
        XCTAssertEqual(status.reserve.allocatedTokens, 9000)
        XCTAssertEqual(status.reserve.floorTokens, 500)
        XCTAssertEqual(status.paths.count, 2)
        XCTAssertEqual(status.paths[0].taskIDs, ["heavy", "tail"])
        XCTAssertEqual(status.paths[0].declaredWeightTokens, 9000)
        XCTAssertEqual(status.paths[0].observedTokens, 2000)
        XCTAssertEqual(status.paths[0].deficitTokens, 4000)
        XCTAssertEqual(status.paths[1].departments, ["qa"])
        XCTAssertEqual(status.departments.count, 2)
        XCTAssertEqual(status.departments[0].department, "impl")
        XCTAssertEqual(status.departments[0].weightTokens, 10000)
        XCTAssertEqual(status.departments[0].floorTokens, 400)
        XCTAssertEqual(status.departments[0].pathIDs, [0])
        XCTAssertEqual(status.departments[1].department, "qa")
        XCTAssertEqual(status.deferredOrder, ["side", "lint"])
        XCTAssertEqual(status.deferred[0].pathID, 1)
        XCTAssertEqual(status.deferred[0].starvationCredit, 5)
    }

    func testFairnessMetricsDoNotInventValues() {
        var path = GovernorPath()
        XCTAssertNil(GovernorMetrics.allocationFraction(path))
        XCTAssertEqual(GovernorMetrics.departmentLabel(path), "—")
        XCTAssertEqual(GovernorMetrics.modeLabel(nil), "single path")
        XCTAssertEqual(GovernorMetrics.modeLabel("multi"), "multi-path")
        XCTAssertEqual(GovernorMetrics.starvationLabel(0), "overflow off")
        XCTAssertEqual(GovernorMetrics.starvationLabel(1800), "overflow 30m")
        XCTAssertEqual(GovernorMetrics.starvationLabel(7200), "overflow 2h")
        path.pathID = 2
        path.taskIDs = ["a", "b"]
        path.weightTokens = 1000
        path.allocationTokens = 250
        path.departments = ["impl", "qa", "ops", "sec"]
        XCTAssertEqual(GovernorMetrics.allocationFraction(path) ?? 0, 0.25, accuracy: 0.0001)
        XCTAssertTrue(GovernorMetrics.pathRow(path).hasPrefix("#2 25%"))
        XCTAssertEqual(GovernorMetrics.departmentLabel(path), "impl,qa,ops+1")
        XCTAssertEqual(GovernorMetrics.starvationLabel(45), "overflow 45s")
    }

    func testDeferredOrderFallsBackToDeferralListForOlderDaemons() throws {
        let status = try JSONDecoder().decode(
            GovernorSessionStatus.self,
            from: Data(
                """
                {
                  "session_id": "s1",
                  "deferred": [
                    {"task_id": "a", "reason": "reserve"},
                    {"task_id": "b", "reason": "throttle"}
                  ]
                }
                """.utf8
            )
        )
        XCTAssertTrue(status.deferredOrder.isEmpty)
        XCTAssertEqual(GovernorMetrics.deferredOrder(status), ["a", "b"])
    }

    func testMultiDecisionDecodesPathTable() throws {
        let decision = try JSONDecoder().decode(
            GovernorDecision.self,
            from: Data(
                """
                {
                  "decision_id": "gov-1",
                  "session_id": "s1",
                  "action": "admission",
                  "reason": "critical_admit",
                  "mode": "multi",
                  "allocated_tokens": 9000,
                  "deferred_order": ["side"],
                  "departments": [{
                    "department": "impl",
                    "weight_tokens": 10000,
                    "floor_tokens": 400,
                    "path_ids": [0]
                  }],
                  "paths": [{
                    "path_id": 0,
                    "task_ids": ["heavy"],
                    "weight_tokens": 10000,
                    "allocation_tokens": 6000,
                    "deficit_tokens": 4000
                  }]
                }
                """.utf8
            )
        )
        XCTAssertEqual(decision.mode, "multi")
        XCTAssertEqual(decision.allocatedTokens, 9000)
        XCTAssertEqual(decision.deferredOrder, ["side"])
        XCTAssertEqual(decision.paths.count, 1)
        XCTAssertEqual(decision.paths[0].taskIDs, ["heavy"])
        XCTAssertEqual(decision.paths[0].allocationTokens, 6000)
        XCTAssertEqual(decision.departments.count, 1)
        XCTAssertEqual(decision.departments[0].department, "impl")
        XCTAssertEqual(decision.departments[0].pathIDs, [0])
    }

    func testDecisionPathIDsDecodeForSharedPrefixTasks() throws {
        let decision = try JSONDecoder().decode(
            GovernorTaskDecision.self,
            from: Data(
                """
                {
                  "task_id": "root",
                  "critical": true,
                  "path_id": 0,
                  "path_ids": [0, 1],
                  "path_deficit_tokens": 25,
                  "starvation_credit": 5
                }
                """.utf8
            )
        )
        XCTAssertEqual(decision.pathID, 0)
        XCTAssertEqual(decision.pathIDs, [0, 1])
        XCTAssertEqual(decision.pathDeficitTokens, 25)
        XCTAssertEqual(decision.starvationCredit, 5)

        let deferral = try JSONDecoder().decode(
            GovernorDeferral.self,
            from: Data("{}".utf8)
        )
        XCTAssertNil(deferral.pathID)
        XCTAssertTrue(deferral.pathIDs.isEmpty)
        XCTAssertEqual(deferral.starvationCredit, 0)
    }

    func testFairnessPolicyRoundTripsEveryFieldThroughJSONValue() throws {
        let policy = CriticalPathPolicy(
            enabled: true,
            reserveRatio: 0.3,
            throttleAtRatio: 0.1,
            criticalBoost: 5,
            minTaskBudget: 2500,
            mode: "multi",
            maxPaths: 12,
            departmentMinRatio: 0.35,
            observedWeightRatio: 0.65,
            starvationAfterSecs: 1200
        )
        let value = policy.jsonValue()
        guard case let .object(object) = value else {
            return XCTFail("expected object json value")
        }
        XCTAssertEqual(object["mode"], .string("multi"))
        XCTAssertEqual(object["max_paths"], .number(12))
        XCTAssertEqual(object["department_min_ratio"], .number(0.35))
        XCTAssertEqual(object["observed_weight_ratio"], .number(0.65))
        XCTAssertEqual(object["starvation_after_secs"], .number(1200))
        let decoded = try JSONDecoder().decode(
            CriticalPathPolicy.self,
            from: JSONEncoder().encode(value)
        )
        XCTAssertEqual(decoded, policy)
        XCTAssertTrue(decoded.isMultiPath)
    }

    func testEfficiencySnapshotCarriesDepartmentsAndPathIDs() throws {
        let status = try JSONDecoder().decode(
            GovernorSessionStatus.self,
            from: Data(
                """
                {
                  "session_id": "s1",
                  "enabled": true,
                  "plan_present": true,
                  "reserve": {"state": "active", "mode": "multi", "path_count": 1},
                  "paths": [{
                    "path_id": 0,
                    "task_ids": ["root", "a"],
                    "departments": ["impl"],
                    "weight_tokens": 1000,
                    "allocation_tokens": 800,
                    "deficit_tokens": 200
                  }],
                  "departments": [{
                    "department": "impl",
                    "weight_tokens": 1000,
                    "floor_tokens": 100,
                    "path_ids": [0]
                  }],
                  "decisions": [{
                    "task_id": "root",
                    "critical": true,
                    "action": "admit",
                    "reason": "critical_admit",
                    "path_id": 0,
                    "path_ids": [0]
                  }]
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
        XCTAssertEqual(snapshot?.governor?.paths.first?.deficitTokens, 200)
        XCTAssertEqual(snapshot?.governor?.departments.first?.floorTokens, 100)
        XCTAssertEqual(snapshot?.governor?.decisions.first?.pathIDs, [0])
    }

    func testDepartmentAndPathMetricsHandleDegenerateValues() {
        var path = GovernorPath()
        path.weightTokens = 0
        path.allocationTokens = 100
        XCTAssertNil(GovernorMetrics.allocationFraction(path))
        XCTAssertEqual(GovernorMetrics.pathRow(path), "#0 — · 100/0 · complete")

        var department = GovernorDepartment()
        XCTAssertEqual(department.department, "")
        XCTAssertEqual(department.weightTokens, 0)
        XCTAssertEqual(department.floorTokens, 0)
        XCTAssertTrue(department.pathIDs.isEmpty)
        department.department = "impl"
        department.pathIDs = [0, 2]
        XCTAssertEqual(department.id, "impl")
        XCTAssertEqual(department.pathIDs.count, 2)
    }

    func testDepartmentTableIsOptionalForOlderPayloads() throws {
        let status = try JSONDecoder().decode(
            GovernorSessionStatus.self,
            from: Data(
                """
                {"session_id": "s1", "paths": [{"path_id": 0, "task_ids": ["a"]}]}
                """.utf8
            )
        )
        XCTAssertTrue(status.departments.isEmpty)
        XCTAssertEqual(status.paths.count, 1)
        XCTAssertTrue(status.deferredOrder.isEmpty)
    }

    func testSingleStatusKeepsAnEmptyPathTable() throws {
        let status = try JSONDecoder().decode(
            GovernorSessionStatus.self,
            from: Data("{}".utf8)
        )
        XCTAssertTrue(status.paths.isEmpty)
        XCTAssertTrue(status.deferredOrder.isEmpty)
        XCTAssertEqual(status.reserve.mode, "single")
        XCTAssertEqual(status.reserve.pathCount, 0)
    }
}
