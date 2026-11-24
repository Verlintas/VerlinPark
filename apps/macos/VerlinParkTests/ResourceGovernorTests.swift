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
        XCTAssertEqual(batch.summaryLabel, "3 pending · 7/20 interrupts")
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
