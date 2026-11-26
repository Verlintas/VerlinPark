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

    // MARK: - Project RunPolicy

    func testProjectRunPolicyResultDecodesDefaultsAndExplicitPolicy() throws {
        let defaultsJSON = try XCTUnwrap("""
        {
          "project_id": "prj-1",
          "policy": {
            "default_profile": "quality",
            "completion_reserve_ratio": 0.25,
            "attention_cap": 40,
            "stop_policy": {
              "require_evidence": false,
              "no_progress_turns": 9,
              "marginal_value_ratio": 0.1,
              "partial_delivery": false
            }
          }
        }
        """.data(using: .utf8))
        let defaults = try JSONDecoder().decode(ProjectRunPolicyResultView.self, from: defaultsJSON)
        XCTAssertEqual(defaults.projectID, "prj-1")
        XCTAssertFalse(defaults.isExplicit)
        XCTAssertEqual(defaults.sourceLabel, "Built-in defaults")
        XCTAssertNil(defaults.updatedLabel)
        XCTAssertEqual(defaults.policy.defaultProfile, .quality)
        XCTAssertEqual(defaults.policy.profileTitle, "Quality")
        XCTAssertEqual(defaults.policy.reservePercentLabel, "25%")
        XCTAssertEqual(defaults.policy.attentionLabel, "40")
        XCTAssertEqual(defaults.policy.noProgressLabel, "9 turns")
        XCTAssertEqual(defaults.policy.stopPolicy.marginalValueRatio, 0.1)
        XCTAssertFalse(defaults.policy.stopPolicy.requireEvidence)
        XCTAssertFalse(defaults.policy.stopPolicy.partialDelivery)

        let explicitJSON = try XCTUnwrap("""
        {
          "project_id": "prj-1",
          "updated_at": "2026-11-24T01:02:03Z",
          "policy": {
            "default_profile": "fast",
            "completion_reserve_ratio": 0.15,
            "attention_cap": 20,
            "stop_policy": {}
          }
        }
        """.data(using: .utf8))
        let explicit = try JSONDecoder().decode(ProjectRunPolicyResultView.self, from: explicitJSON)
        XCTAssertTrue(explicit.isExplicit)
        XCTAssertEqual(explicit.sourceLabel, "Explicit project policy")
        XCTAssertEqual(explicit.updatedLabel, "saved 2026-11-24T01:02:03Z")
        XCTAssertEqual(explicit.policy.profileTitle, "Fast")
        XCTAssertEqual(explicit.policy.stopPolicy.noProgressTurns, 6)
    }

    func testProjectRunPolicyValidationRejectsOutOfBoundsValues() throws {
        try ProjectRunPolicyView().validate()
        for ratio in [-0.01, 0.51, .infinity, .nan] {
            XCTAssertThrowsError(
                try ProjectRunPolicyView(completionReserveRatio: ratio).validate()
            )
        }
        XCTAssertThrowsError(try ProjectRunPolicyView(attentionCap: 0).validate())
        XCTAssertThrowsError(try ProjectRunPolicyView(attentionCap: 1001).validate())
        XCTAssertThrowsError(
            try ProjectRunPolicyView(
                stopPolicy: RunStopPolicyView(
                    requireEvidence: true,
                    noProgressTurns: 1001,
                    marginalValueRatio: 0.2,
                    partialDelivery: true
                )
            ).validate()
        )
        XCTAssertThrowsError(
            try ProjectRunPolicyView(
                stopPolicy: RunStopPolicyView(
                    requireEvidence: true,
                    noProgressTurns: 6,
                    marginalValueRatio: 1.5,
                    partialDelivery: true
                )
            ).validate()
        )
    }

    func testProjectRunPolicyDraftParsesAndPreservesStopPolicy() throws {
        let loaded = ProjectRunPolicyView(
            defaultProfile: .cheap,
            completionReserveRatio: 0.125,
            attentionCap: 7,
            stopPolicy: RunStopPolicyView(
                requireEvidence: false,
                noProgressTurns: 9,
                marginalValueRatio: 0.35,
                partialDelivery: false
            )
        )
        var draft = ProjectRunPolicyDraft(policy: loaded)
        XCTAssertEqual(draft.completionReservePercentText, "12.5")
        XCTAssertEqual(draft.attentionCapText, "7")
        XCTAssertEqual(draft.noProgressTurnsText, "9")
        XCTAssertNil(draft.validationIssue)
        let saved = try draft.makePolicy()
        XCTAssertEqual(saved.defaultProfile, .cheap)
        XCTAssertEqual(saved.completionReserveRatio, 0.125)
        XCTAssertEqual(saved.attentionCap, 7)
        XCTAssertEqual(saved.stopPolicy.noProgressTurns, 9)
        XCTAssertEqual(saved.stopPolicy.requireEvidence, false)
        XCTAssertEqual(saved.stopPolicy.marginalValueRatio, 0.35)
        XCTAssertEqual(saved.stopPolicy.partialDelivery, false)

        let valid = draft
        draft.completionReservePercentText = ""
        XCTAssertNotNil(draft.validationIssue)
        draft = valid
        draft.completionReservePercentText = "51"
        XCTAssertNotNil(draft.validationIssue)
        draft = valid
        draft.attentionCapText = "0"
        XCTAssertNotNil(draft.validationIssue)
        draft = valid
        draft.attentionCapText = "1001"
        XCTAssertNotNil(draft.validationIssue)
        draft = valid
        draft.noProgressTurnsText = "1001"
        XCTAssertNotNil(draft.validationIssue)
        draft = valid
        draft.noProgressTurnsText = "not a number"
        XCTAssertNotNil(draft.validationIssue)
        draft = valid
        draft.profile = .unattended
        XCTAssertEqual(try draft.makePolicy().profileTitle, "Unattended")
    }

    func testProjectRunPolicyEncodesSnakeCaseWireFields() throws {
        let policy = ProjectRunPolicyView(
            defaultProfile: .unattended,
            completionReserveRatio: 0.2,
            attentionCap: 12,
            stopPolicy: .standard
        )
        let object = try XCTUnwrap(policy.jsonValue().objectValue)
        XCTAssertEqual(object["default_profile"]?.stringValue, "unattended")
        XCTAssertEqual(object["completion_reserve_ratio"]?.doubleValue, 0.2)
        XCTAssertEqual(object["attention_cap"]?.intValue, 12)
        let stop = try XCTUnwrap(object["stop_policy"]?.objectValue)
        XCTAssertEqual(stop["no_progress_turns"]?.intValue, 6)
        XCTAssertEqual(stop["require_evidence"]?.boolValue, true)
    }

    // MARK: - Resource pause events

    func testResourcePauseActiveUsesLatestEventAndRequiresDaemonEvents() throws {
        let pause = pauseEvent(seq: 1, reason: "attention_cap", bottleneck: "attention")
        let info = try XCTUnwrap(
            ResourcePauseInfo.activePause(
                in: [pause],
                sessionID: "ses-1",
                eventStreamAvailable: true
            )
        )
        XCTAssertEqual(info.sequence, 1)
        XCTAssertEqual(info.reason, "attention_cap")
        XCTAssertEqual(info.roleLabel, "leader")
        XCTAssertEqual(info.bottleneckLabel, "attention")
        XCTAssertEqual(info.taskShortLabel, "task-123")
        XCTAssertEqual(info.agentShortLabel, "leader-agent")
        XCTAssertNil(
            ResourcePauseInfo.activePause(
                in: [pause],
                sessionID: "ses-1",
                eventStreamAvailable: false
            )
        )
        XCTAssertNil(
            ResourcePauseInfo.activePause(
                in: [pause],
                sessionID: "ses-2",
                eventStreamAvailable: true
            )
        )

        let resumed = sessionUpdatedEvent(seq: 3, status: "running")
        XCTAssertNil(
            ResourcePauseInfo.activePause(
                in: [pause, sessionUpdatedEvent(seq: 2, status: "paused"), resumed],
                sessionID: "ses-1",
                eventStreamAvailable: true
            )
        )
        let secondPause = pauseEvent(seq: 4, reason: "completion_reserve_low")
        let latest = try XCTUnwrap(
            ResourcePauseInfo.activePause(
                in: [pause, resumed, secondPause],
                sessionID: "ses-1",
                eventStreamAvailable: true
            )
        )
        XCTAssertEqual(latest.sequence, 4)
        XCTAssertEqual(latest.reason, "completion_reserve_low")
    }

    func testResourcePauseClearingRulesDoNotInferFromBottleneckOrPausedStatus() {
        XCTAssertFalse(ResourcePauseInfo.clearsPause(sessionUpdatedEvent(seq: 1, status: "paused")))
        XCTAssertFalse(ResourcePauseInfo.clearsPause(agentEvent(seq: 2, type: "agent.paused")))
        XCTAssertTrue(ResourcePauseInfo.clearsPause(agentEvent(seq: 3, type: "agent.resumed")))
        XCTAssertTrue(ResourcePauseInfo.clearsPause(sessionUpdatedEvent(seq: 4, status: "running")))
        XCTAssertTrue(ResourcePauseInfo.clearsPause(sessionUpdatedEvent(seq: 5, status: "resumed")))
        XCTAssertTrue(ResourcePauseInfo.clearsPause(sessionUpdatedEvent(seq: 6, status: "failed")))
        XCTAssertTrue(ResourcePauseInfo.clearsPause(sessionUpdatedEvent(seq: 7, status: "idle")))
        XCTAssertFalse(ResourcePauseInfo.clearsPause(agentEvent(seq: 8, type: "session.resource.decision")))
    }

    func testResourcePauseRejectsWrongSchemaAndBoundsLabels() throws {
        let wrongSchema = pauseEvent(seq: 1, schema: "other/v1")
        XCTAssertNil(
            ResourcePauseInfo.activePause(
                in: [wrongSchema],
                sessionID: "ses-1",
                eventStreamAvailable: true
            )
        )
        let longReason = String(repeating: "r", count: 400)
        let info = try XCTUnwrap(
            ResourcePauseInfo(event: pauseEvent(seq: 2, reason: longReason))
        )
        XCTAssertEqual(info.reasonLabel.count, 240)
        let minimal = try XCTUnwrap(
            ResourcePauseInfo(
                event: pauseEvent(
                    seq: 3,
                    bottleneck: nil,
                    role: nil,
                    taskID: nil,
                    agentID: nil
                )
            )
        )
        XCTAssertEqual(minimal.roleLabel, "role not reported")
        XCTAssertEqual(minimal.bottleneckLabel, "bottleneck not reported")
        XCTAssertNil(minimal.taskShortLabel)
        XCTAssertNil(minimal.agentShortLabel)
        XCTAssertTrue(
            ResourcePauseInfo.resumeOutcomeNotice(resumed: true)
                .contains("daemon reported")
        )
        XCTAssertTrue(
            ResourcePauseInfo.resumeOutcomeNotice(resumed: false)
                .contains("did not resume")
        )
    }

    func testResourcePauseDecodesBoundedAttentionBatch() throws {
        let details: [String: JSONValue] = [
            "attention_batch": .object([
                "schema": .string(AttentionBatchView.schema),
                "pending": .number(3),
                "interrupts": .number(7),
                "cap": .number(20),
                "by_risk": .array([
                    .object([
                        "risk": .string("host_state"),
                        "count": .number(2),
                        "weight": .number(6),
                    ]),
                ]),
                "items": .array([
                    .object([
                        "approval_id": .string("ap-1"),
                        "tool": .string("shell"),
                        "risk": .string("host_state"),
                        "task_id": .string("task-123"),
                        "priority": .number(4),
                        "waiters": .number(2),
                        "reason": .string("host change"),
                    ]),
                ]),
                "truncated": .bool(true),
            ]),
        ]
        let info = try XCTUnwrap(
            ResourcePauseInfo(event: pauseEvent(seq: 10, details: details))
        )
        let batch = try XCTUnwrap(info.attentionBatch)
        XCTAssertEqual(batch.pending, 3)
        XCTAssertEqual(batch.interrupts, 7)
        XCTAssertEqual(batch.cap, 20)
        XCTAssertEqual(batch.items.count, 1)
        XCTAssertEqual(batch.items[0].approvalID, "ap-1")
        XCTAssertEqual(batch.items[0].tool, "shell")
        XCTAssertEqual(batch.items[0].riskLabel, "HOST_STATE")
        XCTAssertEqual(batch.items[0].taskShortLabel, "task-123")
        XCTAssertEqual(batch.items[0].reasonLabel, "host change")
        XCTAssertEqual(batch.remainingCount, 2)
        XCTAssertTrue(batch.truncated)
        XCTAssertEqual(batch.summaryLabel, L10n.format("%lld pending · %lld/%lld interrupts", 3, 7, 20))
        XCTAssertEqual(batch.riskSummary, "host_state 2")

        let wrongSchema = try XCTUnwrap(
            ResourcePauseInfo(
                event: pauseEvent(
                    seq: 11,
                    details: [
                        "attention_batch": .object([
                            "schema": .string("other/v1"),
                            "pending": .number(1),
                        ]),
                    ]
                )
            )
        )
        XCTAssertNil(wrongSchema.attentionBatch)

        let noBatch = try XCTUnwrap(ResourcePauseInfo(event: pauseEvent(seq: 12)))
        XCTAssertNil(noBatch.attentionBatch)
    }

    // MARK: - Explicit partial delivery

    func testSessionDeliveryDecodesNullAndPartialRecord() throws {
        let nullJSON = try XCTUnwrap("""
        {
          "session_id": "ses-1",
          "partial": null,
          "readiness": {"session_id":"ses-1","level":"none","shippable":false,
                        "mergeable":false,"changed_paths":[],"remaining_work":[],
                        "blockers":[],"generated_at":"2026-11-24T00:00:00Z"}
        }
        """.data(using: .utf8))
        let nullResult = try JSONDecoder().decode(SessionDeliveryResultView.self, from: nullJSON)
        XCTAssertEqual(nullResult.sessionID, "ses-1")
        XCTAssertNil(nullResult.partial)
        XCTAssertEqual(nullResult.readiness.level, "none")
        XCTAssertNil(nullResult.partialRecord(for: "ses-1"))

        let recordJSON = try XCTUnwrap("""
        {
          "session_id": "ses-1",
          "partial": {
            "schema": "verlinpark.partial_delivery/v1",
            "id": "pd-1",
            "session_id": "ses-1",
            "project_id": "prj-1",
            "recorded_at": "2026-11-24T01:02:03Z",
            "recorded_by": "operator",
            "summary": "Implemented the sheet; remaining tests are pending.",
            "remaining_work": ["T2: run focused tests", "T3: review diff"],
            "readiness_level": "partial",
            "latest_commit": "abcdef1234567890",
            "changed_paths": ["apps/macos/VerlinPark/Features/ResourceGovernorViews.swift"],
            "blockers": ["EFFECT_UNKNOWN on integration"],
            "uncommitted": true
          },
          "readiness": {"session_id":"ses-1","level":"partial","shippable":true,
                        "mergeable":false,"changed_paths":["a.swift"],
                        "remaining_work":["T2: run focused tests"],"blockers":[],
                        "generated_at":"2026-11-24T00:00:00Z"}
        }
        """.data(using: .utf8))
        let result = try JSONDecoder().decode(SessionDeliveryResultView.self, from: recordJSON)
        let partial = try XCTUnwrap(result.partial)
        XCTAssertEqual(partial.id, "pd-1")
        XCTAssertEqual(partial.sessionID, "ses-1")
        XCTAssertEqual(partial.projectID, "prj-1")
        XCTAssertEqual(partial.recordedBy, "operator")
        XCTAssertEqual(partial.remainingWork.count, 2)
        XCTAssertEqual(partial.readinessLabel, "PARTIAL")
        XCTAssertEqual(partial.remainingCountLabel, "2 remaining items")
        XCTAssertEqual(partial.shortCommit, "abcdef123456")
        XCTAssertTrue(partial.uncommitted)
        XCTAssertTrue(partial.isPartialDelivery)
        XCTAssertEqual(result.partialRecord(for: "ses-1")?.id, "pd-1")
        XCTAssertNil(result.partialRecord(for: "ses-2"))
    }

    func testPartialDeliveryRecordDefaultsMissingAdditiveFields() throws {
        let json = try XCTUnwrap("""
        {"schema":"verlinpark.partial_delivery/v1","id":"pd-2",
         "session_id":"ses-1","summary":"Partial only",
         "remaining_work":[]}
        """.data(using: .utf8))
        let record = try JSONDecoder().decode(PartialDeliveryRecordView.self, from: json)
        XCTAssertEqual(record.projectID, "")
        XCTAssertEqual(record.recordedAt, "")
        XCTAssertNil(record.recordedBy)
        XCTAssertEqual(record.readinessLabel, "NOT REPORTED")
        XCTAssertTrue(record.changedPaths.isEmpty)
        XCTAssertTrue(record.blockers.isEmpty)
        XCTAssertFalse(record.uncommitted)
        XCTAssertTrue(record.isPartialDelivery)

        let missingSchemaJSON = try XCTUnwrap("""
        {"id":"pd-2b","session_id":"ses-1","summary":"Partial only"}
        """.data(using: .utf8))
        let missingSchema = try JSONDecoder().decode(
            PartialDeliveryRecordView.self,
            from: missingSchemaJSON
        )
        XCTAssertEqual(missingSchema.schema, PartialDeliveryRecordView.schema)
        XCTAssertTrue(missingSchema.isPartialDelivery)

        let wrongSchemaJSON = try XCTUnwrap("""
        {"schema":"other/v1","id":"pd-3","session_id":"ses-1","summary":"Partial"}
        """.data(using: .utf8))
        let wrongSchema = try JSONDecoder().decode(
            PartialDeliveryRecordView.self,
            from: wrongSchemaJSON
        )
        XCTAssertFalse(wrongSchema.isPartialDelivery)
    }

    func testPartialDeliveryResultDecodesRecordAndReadiness() throws {
        let json = try XCTUnwrap("""
        {
          "session_id": "ses-1",
          "record": {
            "schema": "verlinpark.partial_delivery/v1",
            "id": "pd-4",
            "session_id": "ses-1",
            "summary": "Only the explicit record was added.",
            "remaining_work": ["T1: wire the card"],
            "readiness_level": "partial"
          },
          "readiness": {"session_id":"ses-1","level":"partial","shippable":false,
                        "mergeable":false,"changed_paths":[],"remaining_work":[],
                        "blockers":[],"generated_at":"2026-11-24T00:00:00Z"}
        }
        """.data(using: .utf8))
        let result = try JSONDecoder().decode(PartialDeliveryResultView.self, from: json)
        XCTAssertEqual(result.sessionID, "ses-1")
        XCTAssertEqual(result.record.id, "pd-4")
        XCTAssertEqual(result.readiness.level, "partial")
        XCTAssertEqual(result.delivery.sessionID, "ses-1")
        XCTAssertEqual(result.delivery.partial?.id, "pd-4")

        let withoutRecord = try XCTUnwrap("""
        {"session_id":"ses-1","record":null,
         "readiness":{"session_id":"ses-1","level":"partial"}}
        """.data(using: .utf8))
        XCTAssertThrowsError(
            try JSONDecoder().decode(PartialDeliveryResultView.self, from: withoutRecord)
        )
        let withoutReadiness = try XCTUnwrap("""
        {"session_id":"ses-1","record":{"schema":"verlinpark.partial_delivery/v1",
         "id":"pd-5","session_id":"ses-1","summary":"Partial"}}
        """.data(using: .utf8))
        XCTAssertThrowsError(
            try JSONDecoder().decode(PartialDeliveryResultView.self, from: withoutReadiness)
        )
        let sessionWithoutReadiness = try XCTUnwrap("""
        {"session_id":"ses-1","partial":null}
        """.data(using: .utf8))
        XCTAssertThrowsError(
            try JSONDecoder().decode(SessionDeliveryResultView.self, from: sessionWithoutReadiness)
        )
    }

    func testPartialDeliveryEncodesSnakeCaseWireFields() throws {
        let record = PartialDeliveryRecordView(
            id: "pd-9",
            sessionID: "ses-1",
            projectID: "prj-1",
            recordedAt: "2026-11-24T01:02:03Z",
            recordedBy: "operator",
            summary: "Partial only",
            remainingWork: ["T1: pending"],
            readinessLevel: "partial",
            latestCommit: "abcdef1234567890",
            changedPaths: ["a.swift"],
            blockers: ["blocker"],
            uncommitted: true
        )
        let object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: JSONEncoder().encode(record)) as? [String: Any]
        )
        XCTAssertEqual(object["schema"] as? String, PartialDeliveryRecordView.schema)
        XCTAssertEqual(object["session_id"] as? String, "ses-1")
        XCTAssertEqual(object["project_id"] as? String, "prj-1")
        XCTAssertEqual(object["recorded_at"] as? String, "2026-11-24T01:02:03Z")
        XCTAssertEqual(object["recorded_by"] as? String, "operator")
        XCTAssertEqual(object["remaining_work"] as? [String], ["T1: pending"])
        XCTAssertEqual(object["readiness_level"] as? String, "partial")
        XCTAssertEqual(object["latest_commit"] as? String, "abcdef1234567890")
        XCTAssertEqual(object["changed_paths"] as? [String], ["a.swift"])
        XCTAssertNil(object["sessionID"])

        let result = try SessionDeliveryResultView(
            sessionID: "ses-1",
            partial: record,
            readiness: readiness(level: "partial")
        )
        let resultObject = try XCTUnwrap(
            JSONSerialization.jsonObject(with: JSONEncoder().encode(result)) as? [String: Any]
        )
        XCTAssertEqual(resultObject["session_id"] as? String, "ses-1")
        XCTAssertNil(resultObject["sessionID"])
        let partialObject = try XCTUnwrap(resultObject["partial"] as? [String: Any])
        XCTAssertEqual(partialObject["recorded_at"] as? String, "2026-11-24T01:02:03Z")

        let partialResult = try PartialDeliveryResultView(
            sessionID: "ses-1",
            record: record,
            readiness: readiness(level: "partial")
        )
        let partialResultObject = try XCTUnwrap(
            JSONSerialization.jsonObject(with: JSONEncoder().encode(partialResult))
                as? [String: Any]
        )
        XCTAssertEqual(partialResultObject["session_id"] as? String, "ses-1")
        XCTAssertEqual(
            (partialResultObject["record"] as? [String: Any])?["session_id"] as? String,
            "ses-1"
        )
        XCTAssertNil(partialResultObject["sessionID"])
    }

    func testPartialDeliveryDraftParsesLinesBoundsAndRequiresContent() throws {
        var draft = PartialDeliveryDraft()
        XCTAssertNil(draft.request)
        XCTAssertNotNil(draft.validationIssue)
        XCTAssertFalse(draft.isClientBounded)

        draft.summary = "  "
        draft.remainingWorkText = "\n  T1 done \r\n\r\nT2: tests\n"
        let request = try XCTUnwrap(draft.request)
        XCTAssertNil(request.summary)
        XCTAssertEqual(request.remainingWork, ["T1 done", "T2: tests"])
        XCTAssertNil(request.recordedBy)
        XCTAssertFalse(draft.isClientBounded)
        XCTAssertNil(draft.validationIssue)

        draft.recordedBy = "  operator  "
        XCTAssertEqual(draft.request?.recordedBy, "operator")
        XCTAssertEqual(draft.remainingItemCount, 2)

        draft.summary = String(repeating: "s", count: 2500)
        XCTAssertTrue(draft.isOverSummaryLimit)
        XCTAssertTrue(draft.isClientBounded)
        XCTAssertEqual(
            draft.request?.summary?.count,
            PartialDeliveryRecordView.summaryCharacterLimit
        )

        draft = PartialDeliveryDraft()
        draft.remainingWorkText = (0 ..< 60).map { "item-\($0)" }.joined(separator: "\n")
        XCTAssertEqual(draft.remainingItemCount, 60)
        XCTAssertTrue(draft.isOverRemainingItemLimit)
        XCTAssertEqual(
            draft.request?.remainingWork.count,
            PartialDeliveryRecordView.remainingItemLimit
        )

        draft.remainingWorkText = String(repeating: "x", count: 600)
        XCTAssertTrue(draft.hasOverlongRemainingItem)
        XCTAssertEqual(
            draft.request?.remainingWork.first?.count,
            PartialDeliveryRecordView.remainingItemCharacterLimit
        )

        draft.recordedBy = String(repeating: "r", count: 200)
        XCTAssertTrue(draft.isOverRecordedByLimit)
        XCTAssertEqual(
            draft.request?.recordedBy?.count,
            PartialDeliveryRecordView.recordedByCharacterLimit
        )

        let splitItems = PartialDeliveryRequest.normalized(
            summary: nil,
            remainingWork: ["A\nB", "   ", "C"],
            recordedBy: " "
        )
        XCTAssertEqual(splitItems?.remainingWork, ["A", "B", "C"])
        XCTAssertNil(splitItems?.recordedBy)
        XCTAssertNil(
            PartialDeliveryRequest.normalized(
                summary: " ",
                remainingWork: [" ", ""],
                recordedBy: "operator"
            )
        )
    }

    func testPartialDeliveryLabelsNeverImplyMergeOrCompletion() {
        let record = PartialDeliveryRecordView(
            id: "pd-10",
            sessionID: "ses-1",
            recordedAt: "2026-11-24T01:02:03Z",
            recordedBy: "operator",
            summary: "Partial only",
            remainingWork: ["T1: pending"],
            readinessLevel: "partial",
            latestCommit: "abcdef1234567890"
        )
        XCTAssertEqual(record.statusLabel, "PARTIAL DELIVERY")
        XCTAssertEqual(record.readinessLabel, "PARTIAL")
        XCTAssertEqual(record.remainingCountLabel, "1 remaining item")
        XCTAssertEqual(record.recordedAtLabel, "2026-11-24T01:02:03Z")
        XCTAssertEqual(record.recordedByLabel, "operator")
        XCTAssertEqual(record.shortCommit, "abcdef123456")
        XCTAssertTrue(record.truthfulCaveat.contains("not a merge"))
        XCTAssertTrue(record.truthfulCaveat.contains("completion"))

        let labels = [
            record.statusLabel,
            record.readinessLabel,
            record.truthfulCaveat,
            record.remainingCountLabel,
        ].joined(separator: " ").uppercased()
        XCTAssertFalse(labels.contains("MERGED"))
        XCTAssertFalse(labels.contains("COMPLETED"))
        XCTAssertTrue(labels.contains("NOT A MERGE"))

        let longRecord = PartialDeliveryRecordView(
            id: "pd-11",
            sessionID: "ses-1",
            recordedAt: String(repeating: "t", count: 100),
            recordedBy: String(repeating: "r", count: 200),
            summary: String(repeating: "s", count: 1000),
            remainingWork: ["one", "two"],
            readinessLevel: "partial"
        )
        XCTAssertLessThanOrEqual(longRecord.summaryLabel.count, 280)
        XCTAssertLessThanOrEqual(longRecord.recordedAtLabel.count, 40)
        XCTAssertEqual(longRecord.recordedByLabel?.count, 80)
        XCTAssertEqual(longRecord.remainingCountLabel, "2 remaining items")
    }

    func testPartialDeliveryValidationPreservesTypedDaemonError() {
        let error = CoreError.rpc(
            code: -32602,
            message: "Session is running",
            data: .object([
                "code": .string("DELIVERY_SESSION_RUNNING"),
                "hint": .string("Wait until the session is idle."),
            ])
        )
        let issue = BudgetValidationIssue.from(error)
        XCTAssertEqual(issue.code, "DELIVERY_SESSION_RUNNING")
        XCTAssertTrue(issue.displayText.contains("Session is running"))
        XCTAssertTrue(issue.displayText.contains("DELIVERY_SESSION_RUNNING"))
        XCTAssertTrue(issue.displayText.contains("Wait until the session is idle."))

        var draft = PartialDeliveryDraft()
        XCTAssertThrowsError(try draft.makeRequest())
        draft.remainingWorkText = "T1: finish the check"
        XCTAssertNoThrow(try draft.makeRequest())
    }

    func testSessionDeliverySelectionGuardsRejectOtherSessionsAndSchemas() throws {
        let readiness = try readiness(level: "partial")
        let record = PartialDeliveryRecordView(
            id: "pd-20",
            sessionID: "ses-1",
            summary: "Partial only"
        )
        let result = SessionDeliveryResultView(
            sessionID: "ses-1",
            partial: record,
            readiness: readiness
        )
        XCTAssertNotNil(result.partialRecord(for: "ses-1"))
        XCTAssertNil(result.partialRecord(for: "ses-2"))
        XCTAssertNil(result.partialRecord(for: ""))

        let wrongSchema = PartialDeliveryRecordView(
            schema: "other/v1",
            id: "pd-21",
            sessionID: "ses-1",
            summary: "Unknown schema"
        )
        let wrongSchemaResult = SessionDeliveryResultView(
            sessionID: "ses-1",
            partial: wrongSchema,
            readiness: readiness
        )
        XCTAssertNil(wrongSchemaResult.partialRecord(for: "ses-1"))

        let mismatched = PartialDeliveryRecordView(
            id: "pd-22",
            sessionID: "ses-2",
            summary: "Wrong session"
        )
        let mismatchedResult = SessionDeliveryResultView(
            sessionID: "ses-1",
            partial: mismatched,
            readiness: readiness
        )
        XCTAssertNil(mismatchedResult.partialRecord(for: "ses-1"))

        let emptySession = SessionDeliveryResultView(
            sessionID: "",
            partial: record,
            readiness: readiness
        )
        XCTAssertNil(emptySession.partialRecord(for: "ses-1"))
    }

    // MARK: - Completion-reserve rescue

    func testRescueStatusDecodesActiveGrantAndReserveWithAdditiveFields() throws {
        let json = try XCTUnwrap("""
        {
          "session_id": "ses-1",
          "active": true,
          "grant": {
            "schema": "verlinpark.rescue_grant/v1",
            "id": "grant-1",
            "session_id": "ses-1",
            "project_id": "prj-1",
            "recorded_at": "2026-11-24T00:00:00Z",
            "expires_at": "2026-11-24T01:00:00Z",
            "recorded_by": "operator",
            "reason": "near_completion",
            "readiness_level": "committed",
            "latest_commit": "abcdef1234567890",
            "no_progress_turns": 0,
            "additive_future_field": {"ignored": true}
          },
          "used_at": null,
          "used_grant_id": null,
          "eligible": false,
          "reason": "active_grant",
          "readiness": {
            "session_id": "ses-1",
            "level": "committed",
            "latest_commit": "abcdef1234567890"
          },
          "no_progress_turns": 0,
          "near_completion": [
            {"kind":"task","id":"T1","status":"completed","detail":"clean commit",
             "commit":"abcdef1234567890"}
          ],
          "looping": [],
          "recommendations": ["Finish T1"],
          "reserve": {
            "completion_reserve_ratio": 0.2,
            "remaining_tokens": 12000,
            "remaining_cost_usd": 1.5,
            "remaining_secs": 600,
            "hard_token_cap": 100000,
            "hard_cost_cap_usd": 5.0,
            "hard_wall_clock_secs": 3600
          },
          "generated_at": "2026-11-24T00:00:00Z"
        }
        """.data(using: .utf8))
        let status = try JSONDecoder().decode(RescueStatusView.self, from: json)
        XCTAssertEqual(status.sessionID, "ses-1")
        XCTAssertTrue(status.active)
        XCTAssertFalse(status.eligible)
        XCTAssertEqual(status.statusLabel, "RESCUE ACTIVE")
        XCTAssertEqual(status.grant?.id, "grant-1")
        XCTAssertEqual(status.activeGrant?.shortID, "grant-1")
        XCTAssertEqual(status.grant?.shortCommit, "abcdef123456")
        XCTAssertEqual(status.grant?.recordedByLabel, "operator")
        XCTAssertEqual(status.grant?.expiresAtLabel, "2026-11-24T01:00:00Z")
        XCTAssertEqual(status.nearCompletion.count, 1)
        XCTAssertEqual(status.nearCompletionPreview.first?.displayLabel, "TASK T1 · COMPLETED")
        XCTAssertEqual(status.recommendationPreviews, ["Finish T1"])
        XCTAssertEqual(status.reserve.completionReservePercentLabel, "20%")
        XCTAssertEqual(status.reserve.remainingLabel, "12.0k tokens · $1.50 · 10m")
        XCTAssertEqual(status.reserve.hardCapLabel, "100.0k tokens · $5.00 · 1h")
    }

    func testRescueStatusDoesNotInferActiveFromGrantPresenceOrMissingActiveField() throws {
        let usedJSON = try XCTUnwrap("""
        {
          "session_id": "ses-1",
          "active": false,
          "grant": {
            "schema": "verlinpark.rescue_grant/v1",
            "id": "grant-2",
            "session_id": "ses-1",
            "expires_at": "2026-11-24T01:00:00Z"
          },
          "used_at": "2026-11-24T00:30:00Z",
          "used_grant_id": "grant-2",
          "eligible": false,
          "reason": "none",
          "readiness": {}
        }
        """.data(using: .utf8))
        let used = try JSONDecoder().decode(RescueStatusView.self, from: usedJSON)
        XCTAssertFalse(used.active)
        XCTAssertNil(used.activeGrant)
        XCTAssertEqual(used.statusLabel, "RESCUE USED")
        XCTAssertEqual(used.usedAtLabel, "2026-11-24T00:30:00Z")
        XCTAssertEqual(used.usedGrantShortLabel, "grant-2")

        let inactiveJSON = try XCTUnwrap("""
        {
          "session_id": "ses-1",
          "active": false,
          "grant": {"schema": "verlinpark.rescue_grant/v1", "id": "grant-3"},
          "eligible": false,
          "reason": "none",
          "readiness": {}
        }
        """.data(using: .utf8))
        let inactive = try JSONDecoder().decode(RescueStatusView.self, from: inactiveJSON)
        XCTAssertNotNil(inactive.grant)
        XCTAssertNil(inactive.activeGrant)
        XCTAssertEqual(inactive.statusLabel, "RESCUE INACTIVE")

        let expiredJSON = try XCTUnwrap("""
        {
          "session_id": "ses-1",
          "active": false,
          "grant": {
            "schema": "verlinpark.rescue_grant/v1",
            "id": "grant-3b",
            "session_id": "ses-1",
            "expires_at": "2026-11-23T00:00:00Z"
          },
          "eligible": false,
          "reason": "none",
          "readiness": {}
        }
        """.data(using: .utf8))
        let expired = try JSONDecoder().decode(RescueStatusView.self, from: expiredJSON)
        XCTAssertFalse(expired.active)
        XCTAssertNil(expired.activeGrant)
        XCTAssertEqual(expired.statusLabel, "RESCUE INACTIVE")
        XCTAssertEqual(expired.grant?.expiresAtLabel, "2026-11-23T00:00:00Z")

        let missingActiveJSON = try XCTUnwrap("""
        {
          "session_id": "ses-1",
          "grant": {"schema": "verlinpark.rescue_grant/v1", "id": "grant-4"},
          "eligible": true,
          "reason": "near_completion",
          "readiness": {}
        }
        """.data(using: .utf8))
        let missingActive = try JSONDecoder().decode(RescueStatusView.self, from: missingActiveJSON)
        XCTAssertFalse(missingActive.active)
        XCTAssertNil(missingActive.activeGrant)
        XCTAssertEqual(missingActive.statusLabel, "RESCUE INACTIVE")
    }

    func testRescueStatusDecodesNullGrantAndConservativeDefaults() throws {
        let json = try XCTUnwrap("""
        {
          "session_id": "ses-2",
          "active": false,
          "grant": null,
          "used_at": null,
          "eligible": false,
          "reason": "dirty",
          "readiness": {},
          "near_completion": null,
          "looping": null,
          "recommendations": null
        }
        """.data(using: .utf8))
        let status = try JSONDecoder().decode(RescueStatusView.self, from: json)
        XCTAssertNil(status.grant)
        XCTAssertNil(status.activeGrant)
        XCTAssertFalse(status.active)
        XCTAssertFalse(status.eligible)
        XCTAssertEqual(status.statusLabel, "RESCUE UNAVAILABLE")
        XCTAssertTrue(status.nearCompletion.isEmpty)
        XCTAssertTrue(status.looping.isEmpty)
        XCTAssertTrue(status.recommendationPreviews.isEmpty)
        XCTAssertEqual(status.reserve.remainingLabel, "not reported")
        XCTAssertEqual(status.reserve.hardCapLabel, "not reported")
        XCTAssertEqual(status.reserve.completionReservePercentLabel, "not reported")
    }

    func testRescueResultRequiresDaemonStateAndSameSession() throws {
        let json = try XCTUnwrap("""
        {
          "session_id": "ses-1",
          "grant": {
            "schema": "verlinpark.rescue_grant/v1",
            "id": "grant-5",
            "session_id": "ses-1",
            "reason": "near_completion"
          },
          "status": {
            "session_id": "ses-1",
            "active": true,
            "grant": {"schema": "verlinpark.rescue_grant/v1", "id": "grant-5",
                      "session_id": "ses-1"},
            "eligible": false,
            "reason": "active_grant",
            "readiness": {}
          }
        }
        """.data(using: .utf8))
        let result = try JSONDecoder().decode(RescueResultView.self, from: json)
        XCTAssertEqual(result.sessionID, "ses-1")
        XCTAssertEqual(result.grant.id, "grant-5")
        XCTAssertTrue(result.status.active)
        XCTAssertTrue(result.isConsistent(for: "ses-1"))
        XCTAssertFalse(result.isConsistent(for: "ses-2"))
        XCTAssertFalse(result.isConsistent(for: ""))
        XCTAssertTrue(result.operatorNotice.contains("next turn only"))

        let missingGrant = try XCTUnwrap("""
        {"session_id":"ses-1","status":{"session_id":"ses-1","readiness":{}}}
        """.data(using: .utf8))
        XCTAssertThrowsError(
            try JSONDecoder().decode(RescueResultView.self, from: missingGrant)
        )

        let mismatched = try RescueResultView(
            sessionID: "ses-2",
            grant: RescueGrantView(id: "grant-6", sessionID: "ses-2"),
            status: rescueStatus(sessionID: "ses-1", readiness: readiness(level: "committed"))
        )
        XCTAssertFalse(mismatched.isConsistent(for: "ses-1"))
    }

    func testRescueReasonLabelsAreTruthful() throws {
        let readiness = try readiness(level: "committed")
        let expectations: [(String, String)] = [
            ("eligible", "near-completion"),
            ("near_completion", "near-completion"),
            ("active_grant", "already active"),
            ("dirty", "uncommitted"),
            ("blocked", "blocked"),
            ("none", "No near-completion"),
            ("merged", "already merged"),
            ("looping", "loop"),
            ("auto_budget", "fixed budgets"),
            ("unlimited", "Unlimited"),
            ("no_headroom", "headroom"),
            ("no_commit", "commit"),
            ("session_active", "running"),
            ("imported", "read-only"),
        ]
        for (reason, fragment) in expectations {
            let status = rescueStatus(
                eligible: reason == "eligible" || reason == "near_completion",
                reason: reason,
                readiness: readiness
            )
            XCTAssertTrue(
                status.reasonLabel.localizedCaseInsensitiveContains(fragment),
                "\(reason): \(status.reasonLabel)"
            )
        }
        let unknown = rescueStatus(reason: "custom_reason", readiness: readiness)
        XCTAssertTrue(unknown.reasonLabel.contains("custom_reason"))
    }

    func testRescueActionBlockReasonRequiresDaemonEligibilityAndQuiescentSession() throws {
        let readiness = try readiness(level: "committed")
        let eligible = rescueStatus(eligible: true, reason: "near_completion", readiness: readiness)
        XCTAssertNil(
            RescueActionAvailability.blockReason(
                sessionID: "ses-1",
                sessionStatus: "paused",
                daemonConnected: true,
                busy: false,
                rescue: eligible,
                error: nil
            )
        )
        for status in ["idle", "failed", "cancelled"] {
            XCTAssertNil(
                RescueActionAvailability.blockReason(
                    sessionID: "ses-1",
                    sessionStatus: status,
                    daemonConnected: true,
                    busy: false,
                    rescue: eligible,
                    error: nil
                ),
                status
            )
        }
        XCTAssertNotNil(
            RescueActionAvailability.blockReason(
                sessionID: nil,
                sessionStatus: "paused",
                daemonConnected: true,
                busy: false,
                rescue: eligible,
                error: nil
            )
        )
        XCTAssertTrue(
            try XCTUnwrap(
                RescueActionAvailability.blockReason(
                    sessionID: "ses-1",
                    sessionStatus: "paused",
                    daemonConnected: false,
                    busy: false,
                    rescue: eligible,
                    error: nil
                )
            ).contains("not connected")
        )
        XCTAssertTrue(
            try XCTUnwrap(
                RescueActionAvailability.blockReason(
                    sessionID: "ses-1",
                    sessionStatus: "paused",
                    daemonConnected: true,
                    busy: true,
                    rescue: eligible,
                    error: nil
                )
            ).contains("in progress")
        )
        XCTAssertTrue(
            try XCTUnwrap(
                RescueActionAvailability.blockReason(
                    sessionID: "ses-1",
                    sessionStatus: "running",
                    daemonConnected: true,
                    busy: false,
                    rescue: eligible,
                    error: nil
                )
            ).contains("paused, idle, failed, or cancelled")
        )
        XCTAssertTrue(
            try XCTUnwrap(
                RescueActionAvailability.blockReason(
                    sessionID: "ses-1",
                    sessionStatus: "paused",
                    daemonConnected: true,
                    busy: false,
                    rescue: nil,
                    error: nil
                )
            ).contains("unavailable")
        )

        let dirty = rescueStatus(reason: "dirty", readiness: readiness)
        XCTAssertEqual(
            RescueActionAvailability.blockReason(
                sessionID: "ses-1",
                sessionStatus: "paused",
                daemonConnected: true,
                busy: false,
                rescue: dirty,
                error: nil
            ),
            dirty.reasonLabel
        )

        let active = rescueStatus(
            active: true,
            grant: RescueGrantView(id: "grant-7", sessionID: "ses-1"),
            reason: "active_grant",
            readiness: readiness
        )
        XCTAssertEqual(
            RescueActionAvailability.blockReason(
                sessionID: "ses-1",
                sessionStatus: "paused",
                daemonConnected: true,
                busy: false,
                rescue: active,
                error: nil
            ),
            "A rescue grant is already active."
        )

        let errorReason = try XCTUnwrap(
            RescueActionAvailability.blockReason(
                sessionID: "ses-1",
                sessionStatus: "paused",
                daemonConnected: true,
                busy: false,
                rescue: eligible,
                error: "RESCUE_STATUS_FAILED while refreshing"
            )
        )
        XCTAssertTrue(errorReason.contains("RESCUE_STATUS_FAILED"))
        XCTAssertNil(
            RescueActionAvailability.blockReason(
                sessionID: "ses-1",
                sessionStatus: "paused",
                daemonConnected: true,
                busy: false,
                rescue: eligible,
                error: " "
            )
        )
    }

    func testRescuePreviewsAndLabelsAreBounded() throws {
        let longDetail = String(repeating: "d", count: 500)
        let candidates = (0 ..< 9).map { _ in
            RescueCandidateView(
                kind: String(repeating: "k", count: 60),
                id: String(repeating: "i", count: 60),
                status: String(repeating: "s", count: 60),
                detail: longDetail,
                commit: "abcdef1234567890"
            )
        }
        let recommendations = (0 ..< 7).map { _ in String(repeating: "r", count: 500) }
        let grant = RescueGrantView(
            id: "grant-8",
            sessionID: "ses-1",
            recordedAt: String(repeating: "t", count: 100),
            expiresAt: String(repeating: "e", count: 100),
            recordedBy: String(repeating: "b", count: 300),
            reason: String(repeating: "r", count: 600)
        )
        let status = try rescueStatus(
            active: true,
            grant: grant,
            usedAt: nil,
            eligible: false,
            reason: "active_grant",
            readiness: readiness(level: "committed"),
            nearCompletion: candidates,
            looping: candidates,
            recommendations: recommendations
        )
        XCTAssertEqual(status.nearCompletionPreview.count, 4)
        XCTAssertEqual(status.nearCompletionOverflow, 5)
        XCTAssertEqual(status.loopingPreview.count, 4)
        XCTAssertEqual(status.loopingOverflow, 5)
        XCTAssertEqual(status.recommendationPreviews.count, 4)
        XCTAssertEqual(status.recommendationOverflow, 3)
        XCTAssertTrue(status.recommendationPreviews.allSatisfy { $0.count <= 200 })
        XCTAssertTrue(status.nearCompletionPreview.allSatisfy { ($0.detailLabel?.count ?? 0) <= 200 })
        XCTAssertTrue(status.nearCompletionPreview.allSatisfy { $0.kindLabel.count <= 24 })
        XCTAssertTrue(status.nearCompletionPreview.allSatisfy { $0.shortCommit == "abcdef123456" })
        XCTAssertLessThanOrEqual(status.grant?.reasonLabel?.count ?? 1000, 240)
        XCTAssertLessThanOrEqual(status.grant?.recordedByLabel?.count ?? 1000, 120)
        XCTAssertEqual(status.grant?.recordedAtLabel.count, 40)
        XCTAssertEqual(status.grant?.expiresAtLabel.count, 40)
    }

    func testRescueGrantInputNormalizationTrimsAndBounds() {
        XCTAssertNil(RescueGrantView.normalizedReason(nil))
        XCTAssertNil(RescueGrantView.normalizedReason("   "))
        XCTAssertNil(RescueGrantView.normalizedRecordedBy("\n"))
        XCTAssertEqual(RescueGrantView.normalizedReason("  finish T1  "), "finish T1")
        XCTAssertEqual(
            RescueGrantView.normalizedReason(String(repeating: "r", count: 400))?.count,
            RescueGrantView.reasonCharacterLimit
        )
        XCTAssertEqual(
            RescueGrantView.normalizedRecordedBy(String(repeating: "b", count: 400))?.count,
            RescueGrantView.recordedByCharacterLimit
        )
    }

    // MARK: - R3 allocation preview

    func testAllocationPreviewDecodesFeasibilityEnvelopeTasksAndCalibration() throws {
        let json = try XCTUnwrap("""
        {
          "session_id": "ses-1",
          "generated_at": "2026-11-25T00:00:00Z",
          "profile": "balanced",
          "readiness": {"session_id": "ses-1", "level": "committed"},
          "envelope": {
            "token_budget": 120000,
            "spent_tokens": 80000,
            "remaining_tokens": 40000,
            "cost_budget_usd": 2.5,
            "spent_cost_usd": 1.5,
            "remaining_cost_usd": 1.0,
            "time_budget_secs": 3600,
            "remaining_secs": 1800,
            "completion_reserve_ratio": 0.2
          },
          "feasibility": {
            "feasible": true,
            "reason": "p90 fits the remaining envelope",
            "p50_tokens": 10000,
            "p90_tokens": 18000,
            "p50_cost_usd": 0.1,
            "p90_cost_usd": 0.2,
            "sample_calls": 12,
            "source": "task_history",
            "reserve_tokens": 20000,
            "headroom_tokens": 22000,
            "critical_path_tasks": ["T1", "T2"],
            "estimated_tasks": 8,
            "total_tasks": 8,
            "pressure_dimensions": ["tokens", "time"],
            "critical_path_available": true,
            "critical_path_reason": null
          },
          "tasks": [
            {"task_id":"T1","title":"Write model","department":"core","status":"pending",
             "priority":5,"token_budget":30000,"cost_budget_usd":0.5,
             "effective_token_budget":36000,"effective_cost_budget_usd":0.6,
             "envelope_reallocated":true,"p50_tokens":9000,
             "p90_tokens":16000,"observed_calls":4,"source":"history","critical_path":true,
             "ready":true,"feasible":true,"reason":"fits"},
            {"task_id":"T2","title":"Tests","department":"core","status":"pending"},
            {"task_id":"T3","title":"Review","department":"core","status":"pending"},
            {"task_id":"T4","title":"Docs","department":"core","status":"pending"},
            {"task_id":"T5","title":"Audit","department":"core","status":"pending"},
            {"task_id":"T6","title":"Bench","department":"core","status":"pending"},
            {"task_id":"T7","title":"Extra","department":"core","status":"pending"},
            {"task_id":"T8","title":"Overflow","department":"core","status":"pending"}
          ],
          "recommendations": [
            {"action":"reallocate","reason":"move 4k tokens to T2","confidence":"high","task_id":"T2"},
            {"action":"split_task","reason":"T3 is large","confidence":"medium","task_id":"T3"},
            {"action":"scout","reason":"unknown area","confidence":"low","task_id":null},
            {"action":"noop","reason":"a","confidence":"unknown"},
            {"action":"noop","reason":"b","confidence":""},
            {"action":"noop","reason":"c","confidence":"high"},
            {"action":"noop","reason":"d","confidence":"high"},
            {"action":"noop","reason":"e","confidence":"high"}
          ],
          "metareasoning": {
            "diagnosis_status": "stalled",
            "diagnosis_reason": "T2 has no progress",
            "diagnosis_target": "T2",
            "no_progress_turns": 4,
            "stalled_tasks": ["T2", {"task_id":"T4","title":"Docs"}, "T6", "T7", "T8", "T9", "T10"],
            "failed_tasks": ["T5"],
            "retrying_tasks": [{"id":"T3"}],
            "recommendations": [
              {"action":"replan","reason":"split T2","confidence":"medium","task_id":"T2"}
            ]
          },
          "calibration": {
            "attention_recommendation": "batch routine approvals",
            "trust_recommendation": "keep Verifier strict",
            "reason": "2 rejections in the last 5 tasks",
            "confidence": "medium",
            "pending_approvals": 2,
            "attention_interrupts": 7,
            "attention_cap": 20,
            "verifier_rejections": 2,
            "failed_tasks": 1
          },
          "truncated": true
        }
        """.data(using: .utf8))
        let allocation = try JSONDecoder().decode(AllocationPreviewView.self, from: json)
        XCTAssertEqual(allocation.sessionID, "ses-1")
        XCTAssertEqual(allocation.profileTitle, "Balanced")
        XCTAssertEqual(allocation.readinessLabel, "COMMITTED")
        XCTAssertTrue(allocation.truncated)
        XCTAssertEqual(allocation.envelope?.remainingTokens, 40000)
        XCTAssertEqual(allocation.envelope?.remainingLabel, "40.0k tokens · $1.00 · 30m")
        XCTAssertEqual(allocation.envelope?.reservePercentLabel, "20%")
        XCTAssertEqual(allocation.feasibility?.statusLabel, "FEASIBLE")
        XCTAssertTrue(allocation.feasibility?.feasible == true)
        XCTAssertEqual(allocation.feasibility?.sampleCalls, 12)
        XCTAssertEqual(allocation.feasibility?.source, "task_history")
        XCTAssertTrue(allocation.feasibility?.forecastLabel.contains("p50") == true)
        XCTAssertTrue(allocation.feasibility?.forecastLabel.contains("p90") == true)
        XCTAssertTrue(allocation.feasibility?.forecastLabel.contains("12 samples") == true)
        XCTAssertEqual(allocation.feasibility?.reserveHeadroomLabel, "reserve 20.0k · headroom 22.0k")
        XCTAssertEqual(allocation.feasibility?.criticalPathPreviews, ["T1", "T2"])
        XCTAssertEqual(allocation.feasibility?.estimateCoverageLabel, "8/8 task(s) estimated")
        XCTAssertEqual(allocation.feasibility?.pressureLabel, "tokens, time")
        XCTAssertNil(allocation.feasibility?.criticalPathWarning)
        XCTAssertEqual(allocation.taskPreviews.count, 6)
        XCTAssertEqual(allocation.taskOverflow, 2)
        XCTAssertEqual(allocation.taskPreviews.first?.titleLabel, "Write model")
        XCTAssertTrue(allocation.taskPreviews.first?.envelopeReallocated == true)
        XCTAssertEqual(
            allocation.taskPreviews.first?.effectiveBudgetLabel,
            "runtime 36.0k tokens · $0.60"
        )
        XCTAssertFalse(allocation.taskPreviews[1].envelopeReallocated)
        XCTAssertNil(allocation.taskPreviews[1].effectiveBudgetLabel)
        XCTAssertEqual(allocation.taskPreviews.first?.feasibleLabel, "FEASIBLE")
        XCTAssertEqual(allocation.taskPreviews.first?.readyLabel, "READY")
        XCTAssertEqual(allocation.taskPreviews[1].budgetLabel, "no task envelope reported")
        XCTAssertEqual(allocation.recommendationPreviews.count, 6)
        XCTAssertEqual(allocation.recommendationOverflow, 2)
        XCTAssertEqual(allocation.recommendationPreviews.first?.confidenceLabel, "HIGH")
        XCTAssertEqual(allocation.recommendationPreviews[1].actionLabel, "split task")
        XCTAssertEqual(allocation.recommendationPreviews[3].confidenceLabel, "UNKNOWN")
        XCTAssertEqual(allocation.recommendationPreviews[4].confidenceLabel, "CONFIDENCE NOT REPORTED")
        XCTAssertEqual(allocation.metareasoning?.diagnosisStatusLabel, "STALLED")
        XCTAssertEqual(allocation.metareasoning?.diagnosisTargetLabel, "T2")
        XCTAssertEqual(allocation.metareasoning?.noProgressLabel, "4 no-progress turns")
        XCTAssertEqual(allocation.metareasoning?.stalledPreviews.count, 6)
        XCTAssertEqual(allocation.metareasoning?.stalledOverflow, 1)
        XCTAssertEqual(allocation.metareasoning?.stalledPreviews[1], "T4")
        XCTAssertEqual(allocation.metareasoning?.retryingPreviews, ["T3"])
        XCTAssertEqual(allocation.metareasoning?.recommendationPreviews.first?.actionLabel, "replan")
        XCTAssertEqual(allocation.calibration?.attentionLabel, "batch routine approvals")
        XCTAssertEqual(allocation.calibration?.trustLabel, "keep Verifier strict")
        XCTAssertEqual(allocation.calibration?.confidenceLabel, "MEDIUM")
        XCTAssertEqual(
            allocation.calibration?.countLabel,
            "7/20 interrupts · 2 pending approvals · 2 verifier rejections · 1 failed task"
        )
        XCTAssertTrue(allocation.isForSession("ses-1"))
        XCTAssertFalse(allocation.isForSession("ses-2"))
        XCTAssertFalse(allocation.isForSession(""))
    }

    func testAllocationPreviewDefaultsMissingNullAndUnknownFieldsWithoutInvention() throws {
        let json = try XCTUnwrap("""
        {
          "session_id": "ses-2",
          "profile": "custom",
          "readiness": null,
          "envelope": null,
          "feasibility": {"reason": "unknown", "feasible": null, "tasks": null,
                          "critical_path_available": false,
                          "critical_path_reason": "BUDGET_PATH_UNKNOWN_TASK"},
          "tasks": null,
          "recommendations": null,
          "metareasoning": null,
          "calibration": null
        }
        """.data(using: .utf8))
        let allocation = try JSONDecoder().decode(AllocationPreviewView.self, from: json)
        XCTAssertEqual(allocation.profileTitle, "CUSTOM")
        XCTAssertNil(allocation.readiness)
        XCTAssertEqual(allocation.readinessLabel, "NOT REPORTED")
        XCTAssertNil(allocation.envelope)
        XCTAssertEqual(allocation.feasibility?.statusLabel, "FEASIBILITY NOT REPORTED")
        XCTAssertNil(allocation.feasibility?.feasible)
        XCTAssertEqual(allocation.feasibility?.forecastLabel, "No forecast reported")
        XCTAssertEqual(allocation.feasibility?.reserveHeadroomLabel, "Reserve/headroom not reported")
        XCTAssertNil(allocation.feasibility?.estimateCoverageLabel)
        XCTAssertNil(allocation.feasibility?.pressureLabel)
        XCTAssertEqual(
            allocation.feasibility?.criticalPathWarning,
            "critical path unavailable (BUDGET_PATH_UNKNOWN_TASK); feasibility falls back to all remaining tasks"
        )
        XCTAssertTrue(allocation.tasks.isEmpty)
        XCTAssertTrue(allocation.recommendations.isEmpty)
        XCTAssertNil(allocation.metareasoning)
        XCTAssertNil(allocation.calibration)
        XCTAssertFalse(allocation.truncated)
        XCTAssertFalse(allocation.isForSession("ses-1"))
    }

    func testAllocationEmptyAndUnknownProfileHaveNoFeasibilityClaim() throws {
        let json = try XCTUnwrap("{}".data(using: .utf8))
        let allocation = try JSONDecoder().decode(AllocationPreviewView.self, from: json)
        XCTAssertEqual(allocation.sessionID, "")
        XCTAssertEqual(allocation.profileTitle, "Profile not reported")
        XCTAssertNil(allocation.feasibility)
        XCTAssertNil(allocation.envelope)
        XCTAssertFalse(allocation.isForSession("ses-1"))
        XCTAssertEqual(allocation.readinessLabel, "NOT REPORTED")
    }

    // MARK: - R4 trade-offs and quality

    func testTradeoffViewDecodesFiveProfilesQualityAndCalibratedConfidence() throws {
        let json = try XCTUnwrap("""
        {
          "session_id": "ses-1",
          "generated_at": "2026-11-25T00:00:00Z",
          "current_profile": "balanced",
          "effective_completion_reserve_ratio": 0.2,
          "reserve_source": "project_policy",
          "profiles": [
            {"profile":"quality","primary_goal":"highest confidence",
             "completion_reserve_ratio":0.3,"attention_posture":"high",
             "quality_posture":"strict","projected_turns":20,
             "projected_cost_usd":1.2,"recommended":false},
            {"profile":"balanced","primary_goal":"balanced delivery",
             "completion_reserve_ratio":0.2,"attention_posture":"medium",
             "quality_posture":"standard","projected_turns":14,
             "projected_cost_usd":0.8,"recommended":true},
            {"profile":"fast","primary_goal":"shortest path",
             "completion_reserve_ratio":0.1,"attention_posture":"low",
             "quality_posture":"light","projected_turns":8,
             "projected_cost_usd":0.5},
            {"profile":"cheap","primary_goal":"lowest cost",
             "completion_reserve_ratio":0.15,"attention_posture":"low",
             "quality_posture":"standard","projected_turns":10,
             "projected_cost_usd":0.3},
            {"profile":"unattended","primary_goal":"minimal interrupts",
             "completion_reserve_ratio":0.2,"attention_posture":"minimal",
             "quality_posture":"standard","projected_turns":null,
             "projected_cost_usd":null}
          ],
          "quality": {
            "readiness_level": "verified",
            "latest_commit": "abcdef1234567890",
            "latest_test": "cargo test: pass",
            "verifier": "approved",
            "merge": "pending",
            "completed_tasks": 3,
            "total_tasks": 4,
            "merged_entries": 0,
            "changed_paths": ["Sources/A.swift", "Tests/A.swift"],
            "blockers": [],
            "reviewability": "watch",
            "reasons": ["one task remains"]
          },
          "confidence": {
            "level": "medium",
            "score": 0.72,
            "sample_calls": 9,
            "reasons": ["small sample", "one provider"]
          },
          "recommendations": [
            "finish T4 before landing",
            {"action": "keep verifier", "reason": "high-risk paths", "confidence": "high"}
          ],
          "warnings": ["forecast is based on task history"]
        }
        """.data(using: .utf8))
        let tradeoffs = try JSONDecoder().decode(TradeoffView.self, from: json)
        XCTAssertEqual(tradeoffs.sessionID, "ses-1")
        XCTAssertEqual(tradeoffs.currentProfileTitle, "Balanced")
        XCTAssertEqual(tradeoffs.effectiveReserveLabel, "20%")
        XCTAssertEqual(tradeoffs.reserveSourceLabel, "project policy")
        XCTAssertEqual(
            tradeoffs.activeReserveSummary,
            "active reserve 20% · project policy · profile rows are documented posture"
        )
        XCTAssertEqual(tradeoffs.profiles.count, 5)
        XCTAssertEqual(tradeoffs.profilePreviews.count, 5)
        XCTAssertEqual(tradeoffs.profileOverflow, 0)
        XCTAssertEqual(tradeoffs.profilePreviews.map(\.profileTitle), [
            "Quality", "Balanced", "Fast", "Cheap", "Unattended",
        ])
        XCTAssertTrue(tradeoffs.profilePreviews[1].isCurrent(tradeoffs.currentProfile))
        XCTAssertTrue(tradeoffs.profilePreviews[1].isRecommended)
        XCTAssertFalse(
            tradeoffs.profilePreviews[1].showsRecommendedBadge(
                currentProfile: tradeoffs.currentProfile
            ),
            "the current profile must never also render a RECOMMENDED badge"
        )
        XCTAssertFalse(tradeoffs.profilePreviews[2].isRecommended)
        XCTAssertEqual(tradeoffs.profilePreviews[0].reservePercentLabel, "posture 30%")
        XCTAssertEqual(tradeoffs.profilePreviews[0].forecastLabel, "~20 turns · $1.20")
        XCTAssertEqual(tradeoffs.quality?.readinessLabel, "VERIFIED")
        XCTAssertEqual(tradeoffs.quality?.reviewabilityLabel, "WATCH")
        XCTAssertEqual(tradeoffs.quality?.taskProgressLabel, "3 / 4 tasks")
        XCTAssertEqual(tradeoffs.quality?.commitLabel, "abcdef1234567890")
        XCTAssertEqual(tradeoffs.quality?.changedPathPreviews.count, 2)
        XCTAssertEqual(tradeoffs.confidence?.levelLabel, "MEDIUM")
        XCTAssertEqual(tradeoffs.confidence?.scoreLabel, "score 0.72")
        XCTAssertEqual(tradeoffs.confidence?.sampleLabel, "9 sample calls")
        XCTAssertEqual(tradeoffs.recommendationPreviews.count, 2)
        XCTAssertEqual(
            tradeoffs.recommendationPreviews[1],
            "keep verifier · high-risk paths · HIGH"
        )
        XCTAssertEqual(tradeoffs.warningPreviews, ["forecast is based on task history"])
        XCTAssertTrue(tradeoffs.isForSession("ses-1"))
        XCTAssertFalse(tradeoffs.isForSession("ses-2"))
    }

    func testTradeoffViewDefaultsMissingFieldsAndKeepsUnknownConfidenceTruthful() throws {
        let json = try XCTUnwrap("""
        {"session_id":"ses-3","current_profile":"experimental","profiles":null,
         "quality":null,"confidence":{"level":"unspecified","score":null,"reasons":null},
         "recommendations":null,"warnings":null}
        """.data(using: .utf8))
        let tradeoffs = try JSONDecoder().decode(TradeoffView.self, from: json)
        XCTAssertEqual(tradeoffs.currentProfileTitle, "EXPERIMENTAL")
        XCTAssertTrue(tradeoffs.profiles.isEmpty)
        XCTAssertNil(tradeoffs.quality)
        XCTAssertEqual(tradeoffs.confidence?.levelLabel, "UNSPECIFIED")
        XCTAssertEqual(tradeoffs.confidence?.scoreLabel, "score not reported")
        XCTAssertNil(tradeoffs.confidence?.sampleLabel)
        XCTAssertTrue(tradeoffs.recommendationPreviews.isEmpty)
        XCTAssertTrue(tradeoffs.warningPreviews.isEmpty)
        XCTAssertEqual(tradeoffs.quality?.readinessLabel ?? "NOT REPORTED", "NOT REPORTED")
        XCTAssertFalse(tradeoffs.isForSession("ses-1"))
    }

    func testTradeoffAndQualityListsAreBoundedWithTruncationCounts() throws {
        let long = String(repeating: "x", count: 400)
        let paths = (0 ..< 12).map { "Sources/File\($0).swift" }
        let blockers = (0 ..< 9).map { "blocker-\($0)" }
        let reasons = (0 ..< 8).map { "reason-\($0)" }
        let warnings = (0 ..< 9).map { "warning-\($0)-\(long)" }
        let recommendations = (0 ..< 8).map { "recommendation-\($0)-\(long)" }
        let object: [String: Any] = [
            "session_id": "ses-1",
            "profiles": [],
            "quality": [
                "readiness_level": "blocked",
                "changed_paths": paths,
                "blockers": blockers,
                "reviewability": "risk",
                "reasons": reasons,
            ],
            "confidence": [
                "level": "low",
                "score": 0.2,
                "reasons": reasons,
            ],
            "recommendations": recommendations,
            "warnings": warnings,
        ]
        let data = try JSONSerialization.data(withJSONObject: object)
        let tradeoffs = try JSONDecoder().decode(TradeoffView.self, from: data)
        XCTAssertEqual(tradeoffs.recommendationPreviews.count, 6)
        XCTAssertEqual(tradeoffs.recommendationOverflow, 2)
        XCTAssertEqual(tradeoffs.warningPreviews.count, 6)
        XCTAssertEqual(tradeoffs.warningOverflow, 3)
        XCTAssertTrue(tradeoffs.recommendationPreviews.allSatisfy { $0.count <= 240 })
        XCTAssertTrue(tradeoffs.warningPreviews.allSatisfy { $0.count <= 240 })
        XCTAssertEqual(tradeoffs.quality?.changedPathPreviews.count, 8)
        XCTAssertEqual(tradeoffs.quality?.changedPathOverflow, 4)
        XCTAssertEqual(tradeoffs.quality?.blockerPreviews.count, 6)
        XCTAssertEqual(tradeoffs.quality?.blockerOverflow, 3)
        XCTAssertEqual(tradeoffs.quality?.reasonPreviews.count, 5)
        XCTAssertEqual(tradeoffs.quality?.reasonOverflow, 3)
        XCTAssertEqual(tradeoffs.confidence?.reasonPreviews.count, 5)
        XCTAssertEqual(tradeoffs.confidence?.reasonOverflow, 3)
    }

    func testAllocationBoundsLongDisplayStringsAndRecommendationLists() throws {
        let long = String(repeating: "r", count: 600)
        var tasks: [[String: Any]] = []
        for index in 0 ..< 9 {
            tasks.append([
                "task_id": "T\(index)",
                "title": "T\(index)-\(long)",
                "status": "pending",
                "feasible": index == 0,
            ])
        }
        let recommendations = (0 ..< 9).map { index in
            ["action": "a\(index)-\(long)", "reason": long, "confidence": "high"]
        }
        let object: [String: Any] = [
            "session_id": "ses-1",
            "feasibility": [
                "feasible": false,
                "reason": long,
                "critical_path_tasks": (0 ..< 12).map { "T\($0)" },
            ],
            "tasks": tasks,
            "recommendations": recommendations,
        ]
        let data = try JSONSerialization.data(withJSONObject: object)
        let allocation = try JSONDecoder().decode(AllocationPreviewView.self, from: data)
        XCTAssertEqual(allocation.feasibility?.statusLabel, "NOT FEASIBLE")
        XCTAssertEqual(allocation.feasibility?.reasonLabel.count, 240)
        XCTAssertEqual(allocation.feasibility?.criticalPathPreviews.count, 8)
        XCTAssertEqual(allocation.feasibility?.criticalPathOverflow, 4)
        XCTAssertEqual(allocation.taskPreviews.count, 6)
        XCTAssertEqual(allocation.taskOverflow, 3)
        XCTAssertTrue(allocation.taskPreviews.allSatisfy { $0.titleLabel.count <= 120 })
        XCTAssertEqual(allocation.recommendationPreviews.count, 6)
        XCTAssertEqual(allocation.recommendationOverflow, 3)
        XCTAssertTrue(allocation.recommendationPreviews.allSatisfy { $0.reasonLabel.count <= 240 })
        XCTAssertTrue(allocation.recommendationPreviews.allSatisfy { $0.actionLabel.count <= 120 })
    }

    // MARK: - Project RunPolicy auto rescue

    func testProjectRunPolicyAutoRescueDefaultsOffAndPreservesLoadedValue() throws {
        let legacy = try XCTUnwrap("""
        {"default_profile":"balanced","completion_reserve_ratio":0.2,
         "attention_cap":20,"stop_policy":{}}
        """.data(using: .utf8))
        let decoded = try JSONDecoder().decode(ProjectRunPolicyView.self, from: legacy)
        XCTAssertFalse(decoded.autoRescue)
        XCTAssertEqual(decoded.autoRescueLabel, "Off")
        XCTAssertFalse(decoded.autoReallocateEnvelopes)
        XCTAssertEqual(decoded.autoReallocateLabel, "Off")
        XCTAssertTrue(decoded.autoReallocateDetail.contains("recorded overlays still apply"))
        XCTAssertFalse(ProjectRunPolicyView().autoRescue)
        XCTAssertFalse(ProjectRunPolicyDraft().autoRescue)
        XCTAssertFalse(ProjectRunPolicyView().autoReallocateEnvelopes)
        XCTAssertFalse(ProjectRunPolicyDraft().autoReallocateEnvelopes)

        let enabled = try XCTUnwrap("""
        {"default_profile":"fast","completion_reserve_ratio":0.1,
         "attention_cap":5,"stop_policy":{},"auto_rescue":true,
         "auto_reallocate_envelopes":true}
        """.data(using: .utf8))
        let decodedEnabled = try JSONDecoder().decode(ProjectRunPolicyView.self, from: enabled)
        XCTAssertTrue(decodedEnabled.autoRescue)
        XCTAssertEqual(decodedEnabled.autoRescueLabel, "On")
        XCTAssertTrue(decodedEnabled.autoRescueDetail.contains("at most one"))
        XCTAssertTrue(decodedEnabled.autoReallocateEnvelopes)
        XCTAssertEqual(decodedEnabled.autoReallocateLabel, "On")
        XCTAssertTrue(decodedEnabled.autoReallocateDetail.contains("stored TeamPlan"))

        var draft = ProjectRunPolicyDraft(policy: decodedEnabled)
        XCTAssertTrue(draft.autoRescue)
        XCTAssertTrue(draft.autoReallocateEnvelopes)
        XCTAssertEqual(try draft.makePolicy().autoRescue, true)
        XCTAssertEqual(try draft.makePolicy().autoReallocateEnvelopes, true)
        draft.autoRescue = false
        draft.autoReallocateEnvelopes = false
        XCTAssertEqual(try draft.makePolicy().autoRescue, false)
        XCTAssertEqual(try draft.makePolicy().autoReallocateEnvelopes, false)

        let encoded = try XCTUnwrap(decodedEnabled.jsonValue().objectValue)
        XCTAssertEqual(encoded["auto_rescue"]?.boolValue, true)
        XCTAssertEqual(encoded["auto_reallocate_envelopes"]?.boolValue, true)
        let encodedOff = try XCTUnwrap(ProjectRunPolicyView().jsonValue().objectValue)
        XCTAssertEqual(encodedOff["auto_rescue"]?.boolValue, false)
        XCTAssertEqual(encodedOff["auto_reallocate_envelopes"]?.boolValue, false)
    }

    private func rescueStatus(
        sessionID: String = "ses-1",
        active: Bool = false,
        grant: RescueGrantView? = nil,
        usedAt: String? = nil,
        usedGrantID: String? = nil,
        eligible: Bool = false,
        reason: String = "none",
        readiness: ReadinessSnapshotView,
        noProgressTurns: UInt32 = 0,
        nearCompletion: [RescueCandidateView] = [],
        looping: [RescueCandidateView] = [],
        recommendations: [String] = [],
        reserve: RescueReserveView = RescueReserveView()
    ) -> RescueStatusView {
        RescueStatusView(
            sessionID: sessionID,
            active: active,
            grant: grant,
            usedAt: usedAt,
            usedGrantID: usedGrantID,
            eligible: eligible,
            reason: reason,
            readiness: readiness,
            noProgressTurns: noProgressTurns,
            nearCompletion: nearCompletion,
            looping: looping,
            recommendations: recommendations,
            reserve: reserve,
            generatedAt: "2026-11-24T00:00:00Z"
        )
    }

    private func pauseEvent(
        seq: UInt64,
        sessionID: String = "ses-1",
        schema: String = ResourcePauseInfo.schema,
        reason: String = "attention_cap",
        bottleneck: String? = "attention",
        role: String? = "leader",
        taskID: String? = "task-123",
        agentID: String? = "leader-agent",
        details: [String: JSONValue] = [:]
    ) -> Event {
        var payload: [String: JSONValue] = [
            "schema": .string(schema),
            "phase": .string("turn"),
            "action": .string("pause_for_operator"),
            "reason": .string(reason),
            "generated_at": .string("2026-11-24T00:00:00Z"),
        ]
        payload["bottleneck"] = bottleneck.map(JSONValue.string)
        payload["role"] = role.map(JSONValue.string)
        payload["task_id"] = taskID.map(JSONValue.string)
        payload["agent_id"] = agentID.map(JSONValue.string)
        payload["details"] = .object(details)
        return Event(
            sessionId: sessionID,
            seq: seq,
            type: ResourcePauseInfo.eventKind,
            payload: .object(payload),
            createdAt: "2026-11-24T00:00:00Z"
        )
    }

    private func sessionUpdatedEvent(
        seq: UInt64,
        status: String,
        sessionID: String = "ses-1"
    ) -> Event {
        Event(
            sessionId: sessionID,
            seq: seq,
            type: "session.updated",
            payload: .object(["status": .string(status)]),
            createdAt: "2026-11-24T00:00:00Z"
        )
    }

    private func agentEvent(
        seq: UInt64,
        type: String,
        sessionID: String = "ses-1"
    ) -> Event {
        Event(
            sessionId: sessionID,
            seq: seq,
            type: type,
            payload: .object([:]),
            createdAt: "2026-11-24T00:00:00Z"
        )
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
