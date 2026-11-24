@testable import VerlinPark
import XCTest

final class ResourceGovernorTests: XCTestCase {
    func testResourceSnapshotDecodesBoundedDimensionsAndReadiness() throws {
        let json = try XCTUnwrap("""
        {
          "session_id": "ses-1",
          "generated_at": "2026-11-24T00:00:00Z",
          "profile": "balanced",
          "dimensions": [
            {"key":"money","label":"Provider cost","unit":"usd","used":0.0,
             "remaining":null,"soft_target":null,"hard_cap":null,"forecast_p50":null,
             "forecast_p90":null,"state":"ok","source":"unconfigured",
             "note":"prices are not configured"},
            {"key":"attention","label":"Operator attention","unit":"interrupts",
             "used":5.0,"remaining":15.0,"soft_target":16.0,"hard_cap":20.0,
             "forecast_p50":null,"forecast_p90":null,"state":"ok","source":"approvals",
             "note":"5 weighted interrupts"}
          ],
          "bottleneck": "attention",
          "attention": {"pending":1,"approved":2,"denied":0,"expired":0,
                        "interrupts":5,"by_risk":[{"key":"host_state","count":1,"weight":0}]},
          "readiness": {"session_id":"ses-1","level":"committed","shippable":false,
                        "mergeable":false,"latest_commit":"abc123","latest_test":null,
                        "verifier":null,"merge":null,"changed_paths":[],
                        "remaining_work":["T1: pending"],"blockers":[],
                        "generated_at":"2026-11-24T00:00:00Z"},
          "forecast": null,
          "warnings": ["R1 is observe-only."],
          "sample_calls": 7,
          "pricing_source": "unconfigured"
        }
        """.data(using: .utf8))
        let snapshot = try JSONDecoder().decode(ResourceSnapshotView.self, from: json)
        XCTAssertEqual(snapshot.profile, "balanced")
        XCTAssertEqual(snapshot.sampleCalls, 7)
        XCTAssertEqual(snapshot.pricingSource, "unconfigured")
        XCTAssertEqual(snapshot.dimension("money")?.state, "ok")
        XCTAssertEqual(snapshot.dimension("attention")?.hardCap, 20)
        XCTAssertEqual(snapshot.attention.pending, 1)
        XCTAssertEqual(snapshot.attention.interrupts, 5)
        XCTAssertEqual(snapshot.readiness?.level, "committed")
        XCTAssertEqual(snapshot.readiness?.remainingWork.count, 1)
        XCTAssertFalse(snapshot.readiness?.shippable ?? true)
        XCTAssertEqual(snapshot.bottleneck, "attention")
        XCTAssertTrue(snapshot.needsAttention)
    }

    func testResourceSnapshotDefaultsMissingOptionalCollections() throws {
        let json = try XCTUnwrap("""
        {"session_id":"ses-2","generated_at":"2026-11-24T00:00:00Z","profile":"balanced",
         "dimensions":[],"attention":{"pending":0},"warnings":[],"sample_calls":0,
         "pricing_source":"unconfigured"}
        """.data(using: .utf8))
        let snapshot = try JSONDecoder().decode(ResourceSnapshotView.self, from: json)
        XCTAssertTrue(snapshot.dimensions.isEmpty)
        XCTAssertNil(snapshot.readiness)
        XCTAssertEqual(snapshot.attention.interrupts, 0)
        XCTAssertTrue(snapshot.warnings.isEmpty)
        XCTAssertFalse(snapshot.needsAttention)
    }

    func testRunProfileWireMappingIsConservative() {
        XCTAssertEqual(RunProfile.fromWire("quality"), .quality)
        XCTAssertEqual(RunProfile.fromWire("FAST"), .fast)
        XCTAssertEqual(RunProfile.fromWire("unattended"), .unattended)
        XCTAssertEqual(RunProfile.fromWire("not-a-profile"), .balanced)
        XCTAssertEqual(RunProfile.allCases.count, 5)
    }

    func testRunContractDecodesCapsAndStopPolicy() throws {
        let json = try XCTUnwrap("""
        {"session_id":"ses-1","project_id":"prj-1","profile":"quality",
         "source":"derived","goal":"ship it","acceptance":["tests pass"],
         "risk_ceiling":"approval-managed","autonomy":"operator-configured",
         "hard_caps":{"token":2000000,"cost_usd":2.5,"wall_clock_secs":1800},
         "soft_targets":{"cost_usd":2.0,"time_secs":1440,"attention":16},
         "completion_reserve_ratio":0.2,"attention_cap":20,
         "stop_policy":{"require_evidence":true,"no_progress_turns":6,
                        "marginal_value_ratio":0.2,"partial_delivery":true},
         "warnings":[],"generated_at":"2026-11-24T00:00:00Z"}
        """.data(using: .utf8))
        let contract = try JSONDecoder().decode(RunContractView.self, from: json)
        XCTAssertEqual(contract.profile, "quality")
        XCTAssertEqual(contract.hardCaps.token, 2_000_000)
        XCTAssertEqual(contract.hardCaps.costUSD, 2.5)
        XCTAssertEqual(contract.softTargets.attention, 16)
        XCTAssertEqual(contract.attentionCap, 20)
        XCTAssertEqual(contract.stopPolicy.noProgressTurns, 6)
        XCTAssertEqual(contract.acceptance, ["tests pass"])
    }

    // MARK: - Top-up result and request validation

    func testTopUpResultDecodesCappedResumedBudgetAndResources() throws {
        let json = try XCTUnwrap("""
        {
          "session_id": "ses-1",
          "budget": {
            "session_id": "ses-1",
            "mode": "fixed",
            "unlimited": false,
            "effective_token_budget": 1200,
            "hard_token_cap": 1200,
            "spent_tokens": 900,
            "remaining_tokens": 300,
            "spent_cost_usd": 0.0,
            "elapsed_secs": 12
          },
          "resumed": true,
          "capped": true,
          "resources": {
            "session_id": "ses-1",
            "generated_at": "2026-11-24T00:00:00Z",
            "profile": "quality",
            "dimensions": [],
            "warnings": [],
            "sample_calls": 0,
            "pricing_source": "unconfigured"
          }
        }
        """.data(using: .utf8))
        let result = try JSONDecoder().decode(TopUpSessionResourcesResult.self, from: json)
        XCTAssertEqual(result.sessionID, "ses-1")
        XCTAssertTrue(result.resumed)
        XCTAssertTrue(result.capped)
        XCTAssertEqual(result.budget.effectiveTokenBudget, 1200)
        XCTAssertEqual(result.budget.hardTokenCap, 1200)
        XCTAssertEqual(result.budget.remainingTokens, 300)
        XCTAssertEqual(result.resources.profileValue, .quality)
        XCTAssertEqual(result.resources.sessionID, "ses-1")
    }

    func testTopUpResultRequiresBudgetAndResources() throws {
        let withoutBudget = try XCTUnwrap("""
        {"session_id":"ses-1","resumed":false,"capped":false,
         "resources":{"session_id":"ses-1","dimensions":[]}}
        """.data(using: .utf8))
        XCTAssertThrowsError(
            try JSONDecoder().decode(TopUpSessionResourcesResult.self, from: withoutBudget)
        )
        let withoutResources = try XCTUnwrap("""
        {"session_id":"ses-1","resumed":false,"capped":false,
         "budget":{"session_id":"ses-1","mode":"fixed"}}
        """.data(using: .utf8))
        XCTAssertThrowsError(
            try JSONDecoder().decode(TopUpSessionResourcesResult.self, from: withoutResources)
        )
    }

    func testTopUpResultNoticeReportsCappedAndResumeTruthfully() {
        let base = TopUpSessionResourcesResult(
            sessionID: "ses-1",
            budget: SessionBudgetStatus(sessionID: "ses-1"),
            resumed: false,
            capped: false,
            resources: ResourceSnapshotView.emptyFixture
        )
        XCTAssertEqual(
            base.operatorNotice(resumeRequested: false),
            "Budget updated within existing caps"
        )
        let capped = TopUpSessionResourcesResult(
            sessionID: "ses-1",
            budget: SessionBudgetStatus(sessionID: "ses-1"),
            resumed: false,
            capped: true,
            resources: ResourceSnapshotView.emptyFixture
        )
        XCTAssertEqual(
            capped.operatorNotice(resumeRequested: true),
            "Budget clamped to the existing hard cap · session not resumed"
        )
        let resumed = TopUpSessionResourcesResult(
            sessionID: "ses-1",
            budget: SessionBudgetStatus(sessionID: "ses-1"),
            resumed: true,
            capped: false,
            resources: ResourceSnapshotView.emptyFixture
        )
        XCTAssertEqual(
            resumed.operatorNotice(resumeRequested: true),
            "Budget updated within existing caps · session resumed"
        )
        var unlimitedBudget = SessionBudgetStatus(sessionID: "ses-1")
        unlimitedBudget.unlimited = true
        let unlimited = TopUpSessionResourcesResult(
            sessionID: "ses-1",
            budget: unlimitedBudget,
            resumed: false,
            capped: false,
            resources: ResourceSnapshotView.emptyFixture
        )
        XCTAssertTrue(
            unlimited.operatorNotice(resumeRequested: false)
                .contains("no cap was added or removed")
        )
    }

    func testTopUpRequestValidationRequiresOnePositiveIncrement() {
        XCTAssertNotNil(TopUpSessionResourcesRequest().validationIssue)
        XCTAssertNotNil(TopUpSessionResourcesRequest(resume: true).validationIssue)
        XCTAssertNotNil(TopUpSessionResourcesRequest(addTokens: 0).validationIssue)
        XCTAssertNotNil(TopUpSessionResourcesRequest(addCostUSD: 0).validationIssue)
        XCTAssertNotNil(TopUpSessionResourcesRequest(addCostUSD: -1).validationIssue)
        XCTAssertNotNil(TopUpSessionResourcesRequest(addCostUSD: .infinity).validationIssue)
        XCTAssertNotNil(TopUpSessionResourcesRequest(addTimeSecs: 0).validationIssue)

        let valid = TopUpSessionResourcesRequest(
            addTokens: 1,
            addCostUSD: 0.01,
            addTimeSecs: 1,
            resume: true
        )
        XCTAssertNil(valid.validationIssue)
        XCTAssertTrue(valid.hasIncrement)
    }

    func testTopUpDraftParsesOptionalValuesAndRejectsBadText() {
        var draft = TopUpSessionResourcesDraft()
        XCTAssertNil(draft.request)
        XCTAssertFalse(draft.hasAnyInput)
        XCTAssertNotNil(draft.validationIssue)

        draft.tokensText = " 250000 "
        draft.costUSDText = "1.50"
        draft.timeSecsText = "1800"
        draft.resume = true
        let request = draft.request
        XCTAssertEqual(request?.addTokens, 250_000)
        XCTAssertEqual(request?.addCostUSD, 1.5)
        XCTAssertEqual(request?.addTimeSecs, 1800)
        XCTAssertEqual(request?.resume, true)
        XCTAssertNil(draft.validationIssue)

        draft.tokensText = "-5"
        XCTAssertNil(draft.request)
        XCTAssertNotNil(draft.validationIssue)
        draft.tokensText = "1.5"
        XCTAssertNil(draft.request)
        draft.tokensText = "0"
        XCTAssertNil(draft.request)
        draft.tokensText = "12"
        draft.costUSDText = "nan"
        XCTAssertNil(draft.request)
        draft.costUSDText = "free"
        XCTAssertNil(draft.request)
        draft.costUSDText = ""
        draft.timeSecsText = "0"
        XCTAssertNil(draft.request)
    }

    // MARK: - Partial-delivery readiness

    func testReadinessLabelsAndCaveatsNeverImplyCompletion() throws {
        XCTAssertEqual(try readiness(level: "partial").levelLabel, "PARTIAL")
        XCTAssertEqual(try readiness(level: "committed").levelLabel, "COMMITTED")
        XCTAssertEqual(try readiness(level: "dirty").levelLabel, "DIRTY")
        XCTAssertEqual(try readiness(level: "blocked").levelLabel, "BLOCKED")
        XCTAssertEqual(try readiness(level: "tests_passed").levelLabel, "TESTS PASSED")
        XCTAssertEqual(try readiness(level: "none").levelLabel, "NONE")

        for level in ["partial", "committed", "dirty", "blocked", "tests_passed"] {
            let snapshot = try readiness(
                level: level,
                remaining: ["T2: pending"],
                blockers: ["effect unknown"]
            )
            XCTAssertTrue(snapshot.showsDeliveryDetail, level)
            XCTAssertFalse(snapshot.deliveryCaveat?.isEmpty ?? true, level)
            XCTAssertEqual(snapshot.mergeabilityLabel, "not mergeable", level)
            XCTAssertEqual(snapshot.shippabilityLabel, "not shippable", level)
            XCTAssertTrue(snapshot.needsAttention, level)
        }
        XCTAssertNil(try readiness(level: "verified").deliveryCaveat)
        XCTAssertNil(try readiness(level: "merged").deliveryCaveat)
        XCTAssertEqual(
            try readiness(level: "verified", mergeable: true).mergeabilityLabel,
            "mergeable"
        )
    }

    func testReadinessReportIsBoundedAndShowsRequiredFields() throws {
        let snapshot = try readiness(
            level: "partial",
            commit: "abcdef1234567890",
            remaining: ["T1: wire the sheet", "T2: run the focused tests"],
            blockers: ["EFFECT_UNKNOWN on integration"]
        )
        XCTAssertEqual(snapshot.shortCommit, "abcdef123456")
        let report = snapshot.boundedReport()
        XCTAssertTrue(report.contains("Level: PARTIAL (partial)"))
        XCTAssertTrue(report.contains("Commit: abcdef1234567890"))
        XCTAssertTrue(report.contains("Remaining work (2):"))
        XCTAssertTrue(report.contains("- T1: wire the sheet"))
        XCTAssertTrue(report.contains("Blockers (1):"))
        XCTAssertTrue(report.contains("- EFFECT_UNKNOWN on integration"))
        XCTAssertTrue(report.contains("Partial delivery"))

        let manyRemaining = (0 ..< 20).map { "T\($0): pending" }
        let manyBlockers = (0 ..< 20).map { "blocker-\($0)" }
        let large = try readiness(
            level: "blocked",
            commit: "abcdef1234567890",
            remaining: manyRemaining,
            blockers: manyBlockers
        )
        let bounded = large.boundedReport()
        XCTAssertLessThanOrEqual(bounded.count, 2400)
        XCTAssertFalse(bounded.isEmpty)
        let smaller = large.boundedReport(maxCharacters: 400)
        XCTAssertLessThanOrEqual(smaller.count, 400)
        XCTAssertTrue(smaller.contains("… (truncated)"))
    }

    private func readiness(
        level: String,
        commit: String? = nil,
        mergeable: Bool = false,
        remaining: [String] = [],
        blockers: [String] = []
    ) throws -> ReadinessSnapshotView {
        var object: [String: Any] = [
            "session_id": "ses-1",
            "level": level,
            "shippable": false,
            "mergeable": mergeable,
            "changed_paths": [],
            "remaining_work": remaining,
            "blockers": blockers,
            "generated_at": "2026-11-24T00:00:00Z",
        ]
        if let commit {
            object["latest_commit"] = commit
        } else {
            object["latest_commit"] = NSNull()
        }
        let data = try JSONSerialization.data(withJSONObject: object)
        return try JSONDecoder().decode(ReadinessSnapshotView.self, from: data)
    }
}

private extension ResourceSnapshotView {
    static var emptyFixture: ResourceSnapshotView {
        // A syntactically valid minimal snapshot; tests must not invent budget
        // or resource values beyond the wire defaults.
        try! JSONDecoder().decode(
            ResourceSnapshotView.self,
            from: Data(
                #"{"session_id":"ses-1","profile":"balanced","dimensions":[]}"#
                    .utf8
            )
        )
    }
}
