import Foundation
@testable import VerlinPark
import XCTest

final class AutoBudgetTests: XCTestCase {
    private func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
        try JSONDecoder().decode(type, from: Data(json.utf8))
    }

    private let autoStatusJSON = """
    {
      "session_id": "s1",
      "mode": "auto",
      "unlimited": false,
      "effective_token_budget": 100000,
      "effective_cost_budget_usd": 1.5,
      "hard_token_cap": 500000,
      "hard_cost_cap_usd": 5.0,
      "spent_tokens": 62000,
      "spent_cost_usd": 0.42,
      "elapsed_secs": 120,
      "remaining_tokens": 38000,
      "remaining_cost_usd": 1.08,
      "remaining_secs": 480,
      "policy": {
        "enabled": true,
        "hard_token_cap": 500000,
        "hard_cost_cap_usd": 5.0,
        "min_runway_turns": 4,
        "no_progress_turns": 6,
        "future_policy_field": {"keep": true}
      },
      "state": {
        "extensions": 2,
        "total_turns": 14,
        "turns_without_progress": 6,
        "paused_reason": "no_progress",
        "paused_at": "2026-11-21T12:00:00Z",
        "elapsed_secs": 120,
        "progress": {"threshold": 6}
      },
      "forecast": {
        "sample_calls": 8,
        "avg_input_tokens_per_call": 5000,
        "avg_output_tokens_per_call": 500,
        "avg_cost_usd_per_call": 0.04,
        "remaining_tokens": 38000,
        "remaining_cost_usd": 1.08,
        "remaining_secs": 480,
        "runway_turns": 9,
        "runway_secs": 600,
        "runway_cost_usd": 0.8,
        "critical_path_turns": 14,
        "source": "providerledger",
        "generated_at": "2026-11-21T12:01:00Z",
        "future_forecast_field": "keep"
      },
      "updated_at": "2026-11-21T12:01:00Z",
      "future_status_field": "keep"
    }
    """

    // MARK: Decode and round trip

    func testAutoBudgetPolicyDecodesRustDefaults() throws {
        let policy = try decode(AutoBudgetPolicy.self, "{}")
        XCTAssertFalse(policy.enabled)
        XCTAssertNil(policy.initialTokenBudget)
        XCTAssertNil(policy.initialCostBudgetUSD)
        XCTAssertNil(policy.initialWallClockSecs)
        XCTAssertNil(policy.hardTokenCap)
        XCTAssertNil(policy.hardCostCapUSD)
        XCTAssertNil(policy.hardWallClockSecs)
        XCTAssertEqual(policy.extendRatio, 0.25, accuracy: 0.0001)
        XCTAssertEqual(policy.maxMultiplier, 4.0, accuracy: 0.0001)
        XCTAssertEqual(policy.forecastCalls, 8)
        XCTAssertEqual(policy.minRunwayTurns, 4)
        XCTAssertEqual(policy.warnAtRatio, 0.20, accuracy: 0.0001)
        XCTAssertEqual(policy.pauseAtRatio, 0.05, accuracy: 0.0001)
        XCTAssertEqual(policy.maxTurnsPerRun, 0)
        XCTAssertNil(policy.maxTotalTurns)
        XCTAssertEqual(policy.noProgressTurns, 6)
        XCTAssertEqual(policy.noProgressAction, .pause)
        XCTAssertTrue(policy.autoToolBundle)
        XCTAssertFalse(policy.hasHardCap)
        XCTAssertTrue(policy.extraFields.isEmpty)
    }

    func testUnknownPolicyFieldsSurviveRoundTrip() throws {
        let original = try decode(
            AutoBudgetPolicy.self,
            """
            {
              "enabled": true,
              "hard_cost_cap_usd": 25,
              "future_knob": 42,
              "future_nested": {"a": [1, 2, 3]}
            }
            """
        )
        XCTAssertEqual(original.hardCostCapUSD, 25)
        XCTAssertEqual(original.extraFields["future_knob"]?.intValue, 42)

        let encoded = try JSONEncoder().encode(original)
        let object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: encoded) as? [String: Any]
        )
        XCTAssertEqual(object["future_knob"] as? Int, 42)
        XCTAssertNotNil(object["future_nested"])

        let decoded = try JSONDecoder().decode(AutoBudgetPolicy.self, from: encoded)
        XCTAssertEqual(decoded.extraFields["future_knob"]?.intValue, 42)
        XCTAssertEqual(decoded.extraFields["future_nested"], original.extraFields["future_nested"])
    }

    func testUnknownBudgetModeIsPreservedAndNeverUnlimited() throws {
        let mode = try decode(BudgetMode.self, "\"adaptive\"")
        XCTAssertEqual(mode.rawValue, "adaptive")
        XCTAssertFalse(mode.isUnlimited)
        XCTAssertFalse(mode.isKnown)

        let encoded = try JSONEncoder().encode(mode)
        XCTAssertEqual(String(data: encoded, encoding: .utf8), "\"adaptive\"")
    }

    func testStatusDecodesMissingAdditiveFieldsAndMapsRunway() throws {
        let status = try decode(SessionBudgetStatus.self, autoStatusJSON)
        XCTAssertEqual(status.sessionID, "s1")
        XCTAssertEqual(status.mode, .auto)
        XCTAssertFalse(status.unlimited)
        XCTAssertEqual(status.effectiveTokenBudget, 100_000)
        XCTAssertEqual(status.hardTokenCap, 500_000)
        XCTAssertEqual(status.spentTokens, 62000)
        XCTAssertEqual(status.state?.extensions, 2)
        XCTAssertEqual(status.state?.turnsWithoutProgress, 6)
        XCTAssertEqual(status.forecast.runwayTurns, 9)
        XCTAssertEqual(status.forecast.source, .providerLedger)
        XCTAssertTrue(status.forecast.source.isProviderLedger)
        XCTAssertEqual(
            BudgetFormatting.forecastSource(
                BudgetForecastSource(rawValue: "provider_ledger")
            ),
            "provider ledger"
        )
        XCTAssertEqual(
            status.policy?.extraFields["future_policy_field"]?.objectValue?["keep"],
            JSONValue.bool(true)
        )

        let summary = BudgetRunwaySummary(status: status)
        XCTAssertEqual(summary.mode, .auto)
        XCTAssertFalse(summary.unlimited)
        XCTAssertEqual(summary.extensionCount, 2)
        XCTAssertTrue(summary.isPaused)
        XCTAssertTrue(summary.needsAttention)
        XCTAssertEqual(summary.tone, .paused)
        XCTAssertEqual(summary.noProgressTurns, 6)
        XCTAssertEqual(summary.noProgressThreshold, 6)
        XCTAssertEqual(summary.noProgressLabel, "6 / 6 turns without progress")
        XCTAssertTrue(summary.pauseReasonLabel?.contains("No progress") == true)
        XCTAssertTrue(summary.runwayLabel.contains("~9 turns"))
        XCTAssertTrue(summary.headline.contains("Auto"))
        XCTAssertTrue(summary.headline.contains("% left"))
        XCTAssertEqual(summary.criticalPathTurns, 14)
        XCTAssertEqual(summary.forecastSource, .providerLedger)

        let encoded = try JSONEncoder().encode(status)
        let object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: encoded) as? [String: Any]
        )
        XCTAssertEqual(object["future_status_field"] as? String, "keep")
        let forecast = try XCTUnwrap(object["forecast"] as? [String: Any])
        XCTAssertEqual(forecast["future_forecast_field"] as? String, "keep")
    }

    func testLegacyStatusWithoutForecastOrPolicyUsesSafeDefaults() throws {
        let status = try decode(
            SessionBudgetStatus.self,
            """
            {
              "session_id": "s2",
              "mode": "fixed",
              "effective_token_budget": 200000,
              "spent_tokens": 1000
            }
            """
        )
        XCTAssertEqual(status.forecast.source, .unavailable)
        XCTAssertNil(status.policy)
        XCTAssertNil(status.state)
        XCTAssertFalse(status.unlimited)

        let summary = BudgetRunwaySummary(status: status)
        XCTAssertEqual(summary.tone, .normal)
        XCTAssertFalse(summary.isPaused)
        XCTAssertNil(summary.noProgressLabel)
        XCTAssertEqual(summary.runwayLabel, "No forecast samples")
        XCTAssertTrue(summary.hardCapLabel.contains("No hard cap"))
        XCTAssertTrue(summary.effectiveLabel.contains("200.0k tokens"))
    }

    func testUnlimitedStatusNeverLooksLikeAuto() throws {
        let status = try decode(
            SessionBudgetStatus.self,
            """
            {
              "session_id": "s3",
              "mode": "unlimited",
              "unlimited": true,
              "spent_tokens": 999999
            }
            """
        )
        let summary = BudgetRunwaySummary(status: status)
        XCTAssertTrue(summary.unlimited)
        XCTAssertEqual(summary.mode, .unlimited)
        XCTAssertEqual(summary.tone, .unlimited)
        XCTAssertFalse(summary.needsAttention)
        XCTAssertTrue(summary.headline.contains("Unlimited"))
        XCTAssertTrue(summary.headline.contains("gates bypassed"))
    }

    // MARK: Create preset validation

    func testNewSessionDefaultsToAutoAndRequiresOneHardCap() throws {
        var draft = SessionBudgetPresetDraft()
        XCTAssertEqual(draft.preset, .auto)
        XCTAssertEqual(draft.capKind, .tokens)
        XCTAssertEqual(
            draft.validation(basePolicy: AutoBudgetPolicy())?.code,
            "AUTO_BUDGET_CAP_REQUIRED"
        )
        draft.autoCapText = "500000"
        XCTAssertNil(draft.validation(basePolicy: AutoBudgetPolicy()))

        let arguments = try draft.arguments(basePolicy: AutoBudgetPolicy())
        XCTAssertEqual(arguments.mode, .auto)
        XCTAssertEqual(arguments.unlimited, false)
        XCTAssertNil(arguments.tokenBudget)
        XCTAssertNil(arguments.costBudgetUSD)
        XCTAssertEqual(arguments.autoPolicy?.hardTokenCap, 500_000)
        XCTAssertNil(arguments.autoPolicy?.hardCostCapUSD)
        XCTAssertEqual(arguments.autoPolicy?.enabled, true)
    }

    func testNewSessionAutoDefaultsToTokenCapAndUSDIsExplicit() throws {
        var draft = SessionBudgetPresetDraft()
        XCTAssertEqual(draft.capKind, .tokens)
        draft.autoCapText = "250000"
        let tokenArguments = try draft.arguments(basePolicy: AutoBudgetPolicy())
        XCTAssertEqual(tokenArguments.autoPolicy?.hardTokenCap, 250_000)
        XCTAssertNil(tokenArguments.autoPolicy?.hardCostCapUSD)

        draft.capKind = .usd
        draft.autoCapText = "25"
        let usdArguments = try draft.arguments(basePolicy: AutoBudgetPolicy())
        XCTAssertEqual(usdArguments.autoPolicy?.hardCostCapUSD, 25)
        XCTAssertNil(usdArguments.autoPolicy?.hardTokenCap)

        // An explicit project USD cap remains the preselected default.
        let usdProject = AutoBudgetPolicy(
            enabled: true,
            hardCostCapUSD: 12
        )
        let usdDefault = SessionBudgetPresetDraft(projectPolicy: usdProject)
        XCTAssertEqual(usdDefault.capKind, .usd)
        XCTAssertEqual(usdDefault.autoCapText, "12")

        // A token-only project cap selects tokens and stays enforceable.
        let tokenProject = AutoBudgetPolicy(
            enabled: true,
            hardTokenCap: 500_000
        )
        let tokenDefault = SessionBudgetPresetDraft(projectPolicy: tokenProject)
        XCTAssertEqual(tokenDefault.capKind, .tokens)
        XCTAssertEqual(tokenDefault.autoCapText, "500000")
    }

    func testNewSessionTokenCapAndPresetSelection() throws {
        var draft = SessionBudgetPresetDraft()
        draft.capKind = .tokens
        draft.autoCapText = "500000"
        draft.autoWallClockText = "3600"
        let arguments = try draft.arguments(basePolicy: AutoBudgetPolicy())
        XCTAssertEqual(arguments.mode, .auto)
        XCTAssertEqual(arguments.autoPolicy?.hardTokenCap, 500_000)
        XCTAssertEqual(arguments.autoPolicy?.hardWallClockSecs, 3600)
        XCTAssertNil(arguments.autoPolicy?.hardCostCapUSD)

        draft.preset = .fixed
        draft.fixedTokenText = "100000"
        draft.fixedCostText = "2.5"
        draft.fixedTimeText = "600"
        let fixed = try draft.arguments(basePolicy: AutoBudgetPolicy())
        XCTAssertEqual(fixed.mode, .fixed)
        XCTAssertEqual(fixed.tokenBudget, 100_000)
        XCTAssertEqual(fixed.costBudgetUSD, 2.5)
        XCTAssertEqual(fixed.timeBudgetSecs, 600)
        XCTAssertNil(fixed.autoPolicy)
        XCTAssertFalse(fixed.unlimited)

        draft.preset = .unlimited
        let unlimited = try draft.arguments(basePolicy: AutoBudgetPolicy())
        XCTAssertEqual(unlimited.mode, .unlimited)
        XCTAssertTrue(unlimited.unlimited)
        XCTAssertNil(unlimited.tokenBudget)
        XCTAssertNil(unlimited.autoPolicy)
    }

    func testNewSessionInvalidCapsAreRejected() {
        var draft = SessionBudgetPresetDraft()
        draft.capKind = .tokens
        draft.autoCapText = "1.5"
        XCTAssertEqual(
            draft.validation(basePolicy: AutoBudgetPolicy())?.code,
            "INVALID_BUDGET_CAP"
        )

        draft.capKind = .usd
        draft.autoCapText = "-4"
        XCTAssertEqual(
            draft.validation(basePolicy: AutoBudgetPolicy())?.code,
            "INVALID_BUDGET_CAP"
        )

        draft.preset = .fixed
        draft.fixedTokenText = ""
        XCTAssertEqual(
            draft.validation(basePolicy: AutoBudgetPolicy())?.code,
            "INVALID_BUDGET"
        )
        XCTAssertThrowsError(try draft.arguments(basePolicy: AutoBudgetPolicy()))
    }

    func testNewSessionInheritsProjectHardCapsWithoutWeakeningThem() throws {
        let project = AutoBudgetPolicy(
            enabled: true,
            hardTokenCap: 500_000,
            hardCostCapUSD: 25,
            noProgressTurns: 8
        )
        var draft = SessionBudgetPresetDraft(projectPolicy: project)
        XCTAssertEqual(draft.capKind, .usd)
        XCTAssertEqual(draft.autoCapText, "25")
        draft.autoCapText = "10"
        let arguments = try draft.arguments(basePolicy: project)
        XCTAssertEqual(arguments.autoPolicy?.hardCostCapUSD, 10)
        XCTAssertEqual(arguments.autoPolicy?.hardTokenCap, 500_000)
        XCTAssertEqual(arguments.autoPolicy?.noProgressTurns, 8)
    }

    // MARK: Project policy draft

    func testProjectPolicyDraftDefaultsAreSafe() throws {
        var draft = AutoBudgetPolicyDraft()
        XCTAssertFalse(draft.enabled)
        XCTAssertNil(draft.validation())
        let disabled = try draft.makePolicy()
        XCTAssertFalse(disabled.enabled)
        XCTAssertFalse(disabled.hasHardCap)

        draft.enabled = true
        XCTAssertEqual(draft.validation()?.code, "AUTO_BUDGET_CAP_REQUIRED")
        XCTAssertThrowsError(try draft.makePolicy())

        draft.hardCostCapText = "12.5"
        let enabled = try draft.makePolicy()
        XCTAssertTrue(enabled.enabled)
        XCTAssertEqual(enabled.hardCostCapUSD, 12.5)
        XCTAssertEqual(enabled.extendRatio, 0.25, accuracy: 0.0001)
        XCTAssertEqual(enabled.noProgressTurns, 6)
        XCTAssertEqual(enabled.noProgressAction, .pause)
        XCTAssertTrue(enabled.autoToolBundle)
    }

    func testProjectPolicyDraftValidatesRanges() {
        var draft = AutoBudgetPolicyDraft()
        draft.enabled = true
        draft.hardTokenCapText = "5000"
        draft.extendRatio = 1.5
        XCTAssertEqual(draft.validation()?.code, "INVALID_AUTO_BUDGET_POLICY")

        draft.extendRatio = 0.25
        draft.pauseAtRatio = 0.5
        draft.warnAtRatio = 0.2
        XCTAssertEqual(draft.validation()?.code, "INVALID_AUTO_BUDGET_POLICY")

        draft.pauseAtRatio = 0.05
        draft.maxTurnsPerRun = 0
        XCTAssertEqual(draft.validation()?.code, "INVALID_AUTO_BUDGET_POLICY")
    }

    // MARK: Session edit validation

    func testSessionEditRejectsBudgetBelowSpend() throws {
        let status = try decode(SessionBudgetStatus.self, autoStatusJSON)
        var draft = SessionBudgetEditDraft(status: status)
        draft.tokenBudgetText = "50000"
        XCTAssertEqual(
            draft.validation(status: status)?.code,
            "BUDGET_BELOW_SPENT"
        )
        XCTAssertThrowsError(try draft.params(sessionID: "s1", status: status))

        draft.tokenBudgetText = "62000"
        XCTAssertNil(draft.validation(status: status))
    }

    func testSessionEditAutoRequiresCapAndSendsTypedPolicy() throws {
        let status = try decode(SessionBudgetStatus.self, autoStatusJSON)
        var draft = SessionBudgetEditDraft(status: status)
        draft.hardTokenCapText = ""
        draft.hardCostCapText = ""
        draft.hardWallClockText = ""
        XCTAssertEqual(
            draft.validation(status: status)?.code,
            "AUTO_BUDGET_CAP_REQUIRED"
        )

        draft.hardTokenCapText = "900000"
        let params = try draft.params(sessionID: "s1", status: status)
        XCTAssertEqual(params.mode, .auto)
        XCTAssertEqual(params.hardTokenCap, 900_000)
        XCTAssertEqual(params.policy?.hardTokenCap, 900_000)
        XCTAssertTrue(params.clearHardCostCap, "blanking an existing cap clears it explicitly")
        XCTAssertNil(params.policy?.hardCostCapUSD)
    }

    func testSessionEditUnlimitedIsExplicitOnly() throws {
        let status = try decode(SessionBudgetStatus.self, autoStatusJSON)
        var draft = SessionBudgetEditDraft(status: status)
        draft.mode = .unlimited
        let params = try draft.params(sessionID: "s1", status: status)
        XCTAssertEqual(params.mode, .unlimited)
        XCTAssertEqual(params.unlimited, true)
        XCTAssertNil(params.tokenBudget)
        XCTAssertNil(params.policy)

        let object = try params.requestObject()
        XCTAssertEqual(object["mode"]?.stringValue, "unlimited")
        XCTAssertEqual(object["unlimited"]?.boolValue, true)
        XCTAssertNil(object["token_budget"])
    }

    func testSetSessionBudgetRequestOmitsNilFieldsAndPreservesPolicyExtras() throws {
        var params = SetSessionBudgetParams(sessionID: "s1")
        params.mode = .auto
        params.hardCostCapUSD = 10
        params.clearCostBudget = true
        params.policy = AutoBudgetPolicy(
            enabled: true,
            hardCostCapUSD: 10,
            extraFields: ["future_knob": .string("keep")]
        )
        let object = try params.requestObject()
        XCTAssertEqual(object["session_id"]?.stringValue, "s1")
        XCTAssertEqual(object["mode"]?.stringValue, "auto")
        XCTAssertEqual(object["hard_cost_cap_usd"]?.doubleValue, 10)
        XCTAssertEqual(object["clear_cost_budget"]?.boolValue, true)
        XCTAssertNil(object["token_budget"])
        XCTAssertNil(object["clear_token_budget"])
        let policy = try XCTUnwrap(object["policy"]?.objectValue)
        XCTAssertEqual(policy["future_knob"]?.stringValue, "keep")
        XCTAssertEqual(policy["hard_cost_cap_usd"]?.doubleValue, 10)
    }

    func testTypedValidationErrorKeepsCodeAndHint() {
        let issue = BudgetValidationIssue.from(
            CoreError.rpc(
                code: -32000,
                message: "auto budget requires a hard cap",
                data: .object([
                    "code": .string("AUTO_BUDGET_CAP_REQUIRED"),
                    "hint": .string("add a maximum USD or token amount"),
                ])
            )
        )
        XCTAssertEqual(issue.code, "AUTO_BUDGET_CAP_REQUIRED")
        XCTAssertEqual(issue.hint, "add a maximum USD or token amount")
        XCTAssertTrue(issue.displayText.contains("AUTO_BUDGET_CAP_REQUIRED"))
        XCTAssertTrue(issue.displayText.contains("add a maximum"))
    }

    // MARK: AppModel with a fake client

    @MainActor
    func testAppModelLoadsBudgetAndForecastThroughInjectedClient() async throws {
        let status = try decode(SessionBudgetStatus.self, autoStatusJSON)
        let forecast = BudgetForecast(
            sampleCalls: 4,
            runwayTurns: 15,
            runwaySecs: 900,
            runwayCostUSD: 1.25,
            source: .mixed,
            generatedAt: "2026-11-21T13:00:00Z"
        )
        let fake = FakeBudgetRPC(status: status, forecast: forecast)
        let model = AppModel(budgetRPC: fake)
        model.selectedSessionID = "s1"

        await model.loadSessionBudget()
        XCTAssertEqual(model.sessionBudgetStatus?.sessionID, "s1")
        XCTAssertEqual(model.budgetRunway?.extensionCount, 2)
        XCTAssertNil(model.sessionBudgetError)
        let sessionCalls = await fake.sessionBudgetCalls
        XCTAssertEqual(sessionCalls, ["s1"])

        await model.loadSessionBudgetForecast()
        XCTAssertEqual(model.sessionBudgetStatus?.forecast.runwayTurns, 15)
        XCTAssertEqual(model.sessionBudgetStatus?.forecast.source, .mixed)
        let forecastCalls = await fake.sessionBudgetForecastCalls
        XCTAssertEqual(forecastCalls, ["s1"])
    }

    @MainActor
    func testAppModelShowsTypedBudgetLoadErrorWithoutRetrying() async throws {
        let status = try decode(SessionBudgetStatus.self, autoStatusJSON)
        let fake = FakeBudgetRPC(
            status: status,
            statusError: .rpc(
                code: -32000,
                message: "auto budget requires a hard cap",
                data: .object([
                    "code": .string("AUTO_BUDGET_CAP_REQUIRED"),
                ])
            )
        )
        let model = AppModel(budgetRPC: fake)
        model.selectedSessionID = "s1"

        await model.loadSessionBudget()
        XCTAssertNil(model.sessionBudgetStatus)
        XCTAssertEqual(model.sessionBudgetError?.code, "AUTO_BUDGET_CAP_REQUIRED")
        XCTAssertTrue(
            model.sessionBudgetStatusMessage?.contains("AUTO_BUDGET_CAP_REQUIRED") == true
        )
        let calls = await fake.sessionBudgetCalls
        XCTAssertEqual(calls, ["s1"], "the app never auto-retries a budget request")
    }

    @MainActor
    func testAppModelRejectsAutoWithoutCapBeforeCallingDaemon() async throws {
        let status = try decode(SessionBudgetStatus.self, autoStatusJSON)
        let fake = FakeBudgetRPC(status: status)
        let model = AppModel(budgetRPC: fake)
        model.selectedSessionID = "s1"
        model.sessionBudgetStatus = status

        var draft = SessionBudgetEditDraft(status: status)
        draft.hardTokenCapText = ""
        draft.hardCostCapText = ""
        draft.hardWallClockText = ""

        let saved = await model.saveSessionBudget(draft)
        XCTAssertFalse(saved)
        XCTAssertEqual(model.sessionBudgetError?.code, "AUTO_BUDGET_CAP_REQUIRED")
        let calls = await fake.setSessionBudgetCalls
        XCTAssertTrue(calls.isEmpty, "invalid auto input must not reach the daemon")
    }

    @MainActor
    func testAppModelSavesSessionBudgetAndMergesSessionRow() async throws {
        var status = try decode(SessionBudgetStatus.self, autoStatusJSON)
        status.effectiveTokenBudget = 500_000
        status.hardTokenCap = 900_000
        let fake = FakeBudgetRPC(status: status)
        await fake.configure(setResult: SetSessionBudgetResult(sessionID: "s1", budget: status))

        let model = AppModel(budgetRPC: fake)
        model.selectedSessionID = "s1"
        model.sessionBudgetStatus = status
        model.sessions = [
            Session(
                id: "s1",
                projectId: "p1",
                goal: "auto work",
                status: "paused",
                priority: 0,
                worktree: nil,
                branch: nil,
                tokenBudget: 100_000,
                costBudgetUsd: 1.5,
                timeBudgetSecs: 600,
                unlimited: false,
                costSpentUsd: 0.42,
                baseCommit: nil,
                tokensIn: 62000,
                tokensOut: 0,
                createdAt: ""
            ),
        ]

        var draft = SessionBudgetEditDraft(status: status)
        draft.hardTokenCapText = "900000"
        let saved = await model.saveSessionBudget(draft)
        XCTAssertTrue(saved)
        XCTAssertNil(model.sessionBudgetError)
        XCTAssertEqual(model.sessions.first?.tokenBudget, 500_000)
        XCTAssertEqual(model.sessions.first?.unlimited, false)

        let sent = await fake.setSessionBudgetCalls
        XCTAssertEqual(sent.count, 1)
        XCTAssertEqual(sent.first?.mode, .auto)
        XCTAssertEqual(sent.first?.hardTokenCap, 900_000)
    }

    @MainActor
    func testAppModelLoadsAndSavesProjectAutoBudgetDefaults() async throws {
        let policy = AutoBudgetPolicy(
            enabled: true,
            hardTokenCap: 500_000,
            hardCostCapUSD: 25,
            noProgressTurns: 8
        )
        let fake = try FakeBudgetRPC(
            status: decode(SessionBudgetStatus.self, autoStatusJSON),
            projectResult: AutoBudgetPolicyResult(
                projectID: "p1",
                policy: policy,
                updatedAt: "2026-11-21T12:00:00Z"
            )
        )
        let model = AppModel(budgetRPC: fake)
        model.selectedProjectID = "p1"

        await model.loadProjectAutoBudgetPolicy(force: true)
        XCTAssertEqual(model.projectAutoBudgetPolicy.hardCostCapUSD, 25)
        XCTAssertEqual(model.projectAutoBudgetDraft.hardCostCapText, "25")
        XCTAssertEqual(model.projectAutoBudgetDraft.noProgressTurns, 8)
        XCTAssertNil(model.projectAutoBudgetError)

        model.projectAutoBudgetDraft.hardCostCapText = "12.5"
        await fake.configure(
            projectResult: AutoBudgetPolicyResult(
                projectID: "p1",
                policy: AutoBudgetPolicy(
                    enabled: true,
                    hardTokenCap: 500_000,
                    hardCostCapUSD: 12.5,
                    noProgressTurns: 8
                ),
                updatedAt: "2026-11-21T13:00:00Z"
            )
        )
        let saved = await model.saveProjectAutoBudgetPolicy()
        XCTAssertTrue(saved)
        XCTAssertEqual(model.projectAutoBudgetPolicy.hardCostCapUSD, 12.5)
        XCTAssertEqual(model.projectAutoBudgetDraft.hardCostCapText, "12.5")
        let sent = await fake.lastProjectPolicy
        XCTAssertEqual(sent?.hardCostCapUSD, 12.5)
        XCTAssertEqual(sent?.hardTokenCap, 500_000)
    }

    @MainActor
    func testAppModelExposesLastAutoExtensionReason() throws {
        let model = try AppModel(budgetRPC: FakeBudgetRPC(
            status: decode(SessionBudgetStatus.self, autoStatusJSON)
        ))
        model.selectedSessionID = "s1"
        model.events = [
            Event(
                sessionId: "s1",
                seq: 1,
                type: "session.budget.auto_extended",
                payload: .object(["reason": .string("forecast_runway")]),
                createdAt: ""
            ),
        ]
        XCTAssertEqual(model.lastAutoBudgetExtensionReason, "forecast_runway")
        XCTAssertEqual(
            BudgetFormatting.extensionReason("forecast_runway"),
            "forecast runway"
        )
    }

    @MainActor
    func testAppModelProjectPolicySaveRequiresCapWhenEnabled() async throws {
        let fake = try FakeBudgetRPC(
            status: decode(SessionBudgetStatus.self, autoStatusJSON)
        )
        let model = AppModel(budgetRPC: fake)
        model.selectedProjectID = "p1"
        model.projectAutoBudgetDraft.enabled = true
        model.projectAutoBudgetDraft.hardCostCapText = ""

        let saved = await model.saveProjectAutoBudgetPolicy()
        XCTAssertFalse(saved)
        XCTAssertEqual(model.projectAutoBudgetError?.code, "AUTO_BUDGET_CAP_REQUIRED")
        let sent = await fake.lastProjectPolicy
        XCTAssertNil(sent)
    }
}

// MARK: - Fake transport

private actor FakeBudgetRPC: BudgetRPC {
    var status: SessionBudgetStatus
    var forecast: BudgetForecast
    var statusError: CoreError?
    var projectResult: AutoBudgetPolicyResult?
    var setResult: SetSessionBudgetResult?
    var setProjectPolicyResult: AutoBudgetPolicyResult?
    private(set) var sessionBudgetCalls: [String] = []
    private(set) var sessionBudgetForecastCalls: [String] = []
    private(set) var setSessionBudgetCalls: [SetSessionBudgetParams] = []
    private(set) var lastProjectPolicy: AutoBudgetPolicy?

    init(
        status: SessionBudgetStatus,
        forecast: BudgetForecast = BudgetForecast(),
        statusError: CoreError? = nil,
        projectResult: AutoBudgetPolicyResult? = nil
    ) {
        self.status = status
        self.forecast = forecast
        self.statusError = statusError
        self.projectResult = projectResult
    }

    func configure(setResult: SetSessionBudgetResult?) {
        self.setResult = setResult
    }

    func configure(projectResult: AutoBudgetPolicyResult?) {
        self.projectResult = projectResult
    }

    func sessionBudget(sessionID: String) async throws -> SessionBudgetStatus {
        sessionBudgetCalls.append(sessionID)
        if let statusError {
            throw statusError
        }
        return status
    }

    func sessionBudgetForecast(sessionID: String) async throws -> SessionBudgetForecastResult {
        sessionBudgetForecastCalls.append(sessionID)
        if let statusError {
            throw statusError
        }
        return SessionBudgetForecastResult(sessionID: sessionID, forecast: forecast)
    }

    func setSessionBudget(_ params: SetSessionBudgetParams) async throws -> SetSessionBudgetResult {
        setSessionBudgetCalls.append(params)
        if let setResult {
            return setResult
        }
        return SetSessionBudgetResult(sessionID: params.sessionID, budget: status)
    }

    func projectAutoBudget(projectID: String) async throws -> AutoBudgetPolicyResult {
        if let statusError {
            throw statusError
        }
        return projectResult
            ?? AutoBudgetPolicyResult(projectID: projectID, policy: AutoBudgetPolicy())
    }

    func setProjectAutoBudget(
        projectID: String,
        policy: AutoBudgetPolicy
    ) async throws -> AutoBudgetPolicyResult {
        lastProjectPolicy = policy
        if let statusError {
            throw statusError
        }
        return setProjectPolicyResult
            ?? projectResult
            ?? AutoBudgetPolicyResult(projectID: projectID, policy: policy)
    }
}
