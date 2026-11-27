import Foundation

// MARK: - JSON keys

/// Dynamic coding key used to preserve unknown protocol fields across a
/// decode → edit → encode round trip. Known fields stay strongly typed; any
/// future additive field is retained verbatim instead of being dropped.
private struct BudgetDynamicCodingKey: CodingKey {
    let stringValue: String
    let intValue: Int?

    init?(stringValue: String) {
        guard !stringValue.isEmpty else { return nil }
        self.stringValue = stringValue
        intValue = nil
    }

    init?(intValue: Int) {
        stringValue = String(intValue)
        self.intValue = intValue
    }
}

private enum BudgetJSONCoding {
    static func extras<Known: CodingKey & CaseIterable>(
        from decoder: Decoder,
        known _: Known.Type
    ) throws -> [String: JSONValue] {
        let container = try decoder.container(keyedBy: BudgetDynamicCodingKey.self)
        let knownKeys = Set(Known.allCases.map(\.stringValue))
        var extras: [String: JSONValue] = [:]
        for key in container.allKeys where !knownKeys.contains(key.stringValue) {
            extras[key.stringValue] = try container.decode(JSONValue.self, forKey: key)
        }
        return extras
    }

    static func encodeExtras<Known: CodingKey & CaseIterable>(
        _ extras: [String: JSONValue],
        known _: Known.Type,
        to encoder: Encoder
    ) throws {
        guard !extras.isEmpty else { return }
        let knownKeys = Set(Known.allCases.map(\.stringValue))
        var container = encoder.container(keyedBy: BudgetDynamicCodingKey.self)
        for key in extras.keys.sorted() where !knownKeys.contains(key) {
            guard let codingKey = BudgetDynamicCodingKey(stringValue: key) else { continue }
            try container.encode(extras[key], forKey: codingKey)
        }
    }
}

// MARK: - Wire enums

/// Rust `BudgetMode` (`fixed | auto | unlimited`). This is a raw-value wrapper
/// rather than a closed Swift enum so an unknown future mode can be shown and
/// round-tripped without being coerced into `unlimited`.
struct BudgetMode: RawRepresentable, Codable, Hashable, Sendable, Identifiable {
    let rawValue: String

    init(rawValue: String) {
        self.rawValue = rawValue
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        rawValue = try container.decode(String.self)
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(rawValue)
    }

    static let fixed = BudgetMode(rawValue: "fixed")
    static let auto = BudgetMode(rawValue: "auto")
    static let unlimited = BudgetMode(rawValue: "unlimited")
    static let presets: [BudgetMode] = [.auto, .fixed, .unlimited]

    var id: String {
        rawValue
    }

    var isAuto: Bool {
        self == .auto
    }

    var isFixed: Bool {
        self == .fixed
    }

    var isUnlimited: Bool {
        self == .unlimited
    }

    var isKnown: Bool {
        Self.presets.contains(self)
    }
}

struct NoProgressAction: RawRepresentable, Codable, Hashable, Sendable, Identifiable {
    let rawValue: String

    init(rawValue: String) {
        self.rawValue = rawValue
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        rawValue = try container.decode(String.self)
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(rawValue)
    }

    static let pause = NoProgressAction(rawValue: "pause")
    static let escalate = NoProgressAction(rawValue: "escalate")
    static let all: [NoProgressAction] = [.pause, .escalate]

    var id: String {
        rawValue
    }
}

struct BudgetForecastSource: RawRepresentable, Codable, Hashable, Sendable, Identifiable {
    let rawValue: String

    init(rawValue: String) {
        self.rawValue = rawValue
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        rawValue = try container.decode(String.self)
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(rawValue)
    }

    static let unavailable = BudgetForecastSource(rawValue: "unavailable")
    /// Serde uses `#[serde(rename_all = "lowercase")]`, so the wire value is
    /// `providerledger` (not `provider_ledger`). The underscore form is also
    /// accepted because the CLI/docs use it and an older daemon may too.
    static let providerLedger = BudgetForecastSource(rawValue: "providerledger")
    static let providerLedgerUnderscored = BudgetForecastSource(rawValue: "provider_ledger")
    static let mixed = BudgetForecastSource(rawValue: "mixed")
    static let estimate = BudgetForecastSource(rawValue: "estimate")

    var id: String {
        rawValue
    }

    var isProviderLedger: Bool {
        self == .providerLedger || self == .providerLedgerUnderscored
    }
}

// MARK: - Policy

/// Project-scoped auto-budget defaults. Mirrors Rust `AutoBudgetPolicy` and
/// keeps the same safe defaults for every field the daemon omits.
struct AutoBudgetPolicy: Codable, Hashable, Sendable {
    var enabled: Bool
    var initialTokenBudget: UInt64?
    var initialCostBudgetUSD: Double?
    var initialWallClockSecs: UInt64?
    var hardTokenCap: UInt64?
    var hardCostCapUSD: Double?
    var hardWallClockSecs: UInt64?
    var extendRatio: Double
    var maxMultiplier: Double
    var forecastCalls: UInt32
    var minRunwayTurns: UInt32
    var warnAtRatio: Double
    var pauseAtRatio: Double
    var maxTurnsPerRun: UInt32
    var maxTotalTurns: UInt32?
    var noProgressTurns: UInt32
    var noProgressAction: NoProgressAction
    var autoToolBundle: Bool
    /// Additive fields this client does not understand yet. They are merged
    /// back on encode so an older app cannot erase a newer daemon policy field.
    var extraFields: [String: JSONValue]

    enum CodingKeys: String, CodingKey, CaseIterable {
        case enabled
        case initialTokenBudget = "initial_token_budget"
        case initialCostBudgetUSD = "initial_cost_budget_usd"
        case initialWallClockSecs = "initial_wall_clock_secs"
        case hardTokenCap = "hard_token_cap"
        case hardCostCapUSD = "hard_cost_cap_usd"
        case hardWallClockSecs = "hard_wall_clock_secs"
        case extendRatio = "extend_ratio"
        case maxMultiplier = "max_multiplier"
        case forecastCalls = "forecast_calls"
        case minRunwayTurns = "min_runway_turns"
        case warnAtRatio = "warn_at_ratio"
        case pauseAtRatio = "pause_at_ratio"
        case maxTurnsPerRun = "max_turns_per_run"
        case maxTotalTurns = "max_total_turns"
        case noProgressTurns = "no_progress_turns"
        case noProgressAction = "no_progress_action"
        case autoToolBundle = "auto_tool_bundle"
    }

    init(
        enabled: Bool = false,
        initialTokenBudget: UInt64? = nil,
        initialCostBudgetUSD: Double? = nil,
        initialWallClockSecs: UInt64? = nil,
        hardTokenCap: UInt64? = nil,
        hardCostCapUSD: Double? = nil,
        hardWallClockSecs: UInt64? = nil,
        extendRatio: Double = 0.25,
        maxMultiplier: Double = 4.0,
        forecastCalls: UInt32 = 8,
        minRunwayTurns: UInt32 = 4,
        warnAtRatio: Double = 0.20,
        pauseAtRatio: Double = 0.05,
        maxTurnsPerRun: UInt32 = 0,
        maxTotalTurns: UInt32? = nil,
        noProgressTurns: UInt32 = 6,
        noProgressAction: NoProgressAction = .pause,
        autoToolBundle: Bool = true,
        extraFields: [String: JSONValue] = [:]
    ) {
        self.enabled = enabled
        self.initialTokenBudget = initialTokenBudget
        self.initialCostBudgetUSD = initialCostBudgetUSD
        self.initialWallClockSecs = initialWallClockSecs
        self.hardTokenCap = hardTokenCap
        self.hardCostCapUSD = hardCostCapUSD
        self.hardWallClockSecs = hardWallClockSecs
        self.extendRatio = extendRatio
        self.maxMultiplier = maxMultiplier
        self.forecastCalls = forecastCalls
        self.minRunwayTurns = minRunwayTurns
        self.warnAtRatio = warnAtRatio
        self.pauseAtRatio = pauseAtRatio
        self.maxTurnsPerRun = maxTurnsPerRun
        self.maxTotalTurns = maxTotalTurns
        self.noProgressTurns = noProgressTurns
        self.noProgressAction = noProgressAction
        self.autoToolBundle = autoToolBundle
        self.extraFields = extraFields
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? false
        initialTokenBudget = try container.decodeIfPresent(UInt64.self, forKey: .initialTokenBudget)
        initialCostBudgetUSD =
            try container.decodeIfPresent(Double.self, forKey: .initialCostBudgetUSD)
        initialWallClockSecs =
            try container.decodeIfPresent(UInt64.self, forKey: .initialWallClockSecs)
        hardTokenCap = try container.decodeIfPresent(UInt64.self, forKey: .hardTokenCap)
        hardCostCapUSD = try container.decodeIfPresent(Double.self, forKey: .hardCostCapUSD)
        hardWallClockSecs =
            try container.decodeIfPresent(UInt64.self, forKey: .hardWallClockSecs)
        extendRatio = try container.decodeIfPresent(Double.self, forKey: .extendRatio) ?? 0.25
        maxMultiplier = try container.decodeIfPresent(Double.self, forKey: .maxMultiplier) ?? 4.0
        forecastCalls = try container.decodeIfPresent(UInt32.self, forKey: .forecastCalls) ?? 8
        minRunwayTurns = try container.decodeIfPresent(UInt32.self, forKey: .minRunwayTurns) ?? 4
        warnAtRatio = try container.decodeIfPresent(Double.self, forKey: .warnAtRatio) ?? 0.20
        pauseAtRatio = try container.decodeIfPresent(Double.self, forKey: .pauseAtRatio) ?? 0.05
        maxTurnsPerRun = try container.decodeIfPresent(UInt32.self, forKey: .maxTurnsPerRun) ?? 0
        maxTotalTurns = try container.decodeIfPresent(UInt32.self, forKey: .maxTotalTurns)
        noProgressTurns = try container.decodeIfPresent(UInt32.self, forKey: .noProgressTurns) ?? 6
        noProgressAction =
            try container.decodeIfPresent(NoProgressAction.self, forKey: .noProgressAction)
                ?? .pause
        autoToolBundle = try container.decodeIfPresent(Bool.self, forKey: .autoToolBundle) ?? true
        extraFields = try BudgetJSONCoding.extras(from: decoder, known: CodingKeys.self)
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(enabled, forKey: .enabled)
        try container.encodeIfPresent(initialTokenBudget, forKey: .initialTokenBudget)
        try container.encodeIfPresent(initialCostBudgetUSD, forKey: .initialCostBudgetUSD)
        try container.encodeIfPresent(initialWallClockSecs, forKey: .initialWallClockSecs)
        try container.encodeIfPresent(hardTokenCap, forKey: .hardTokenCap)
        try container.encodeIfPresent(hardCostCapUSD, forKey: .hardCostCapUSD)
        try container.encodeIfPresent(hardWallClockSecs, forKey: .hardWallClockSecs)
        try container.encode(extendRatio, forKey: .extendRatio)
        try container.encode(maxMultiplier, forKey: .maxMultiplier)
        try container.encode(forecastCalls, forKey: .forecastCalls)
        try container.encode(minRunwayTurns, forKey: .minRunwayTurns)
        try container.encode(warnAtRatio, forKey: .warnAtRatio)
        try container.encode(pauseAtRatio, forKey: .pauseAtRatio)
        try container.encode(maxTurnsPerRun, forKey: .maxTurnsPerRun)
        try container.encodeIfPresent(maxTotalTurns, forKey: .maxTotalTurns)
        try container.encode(noProgressTurns, forKey: .noProgressTurns)
        try container.encode(noProgressAction, forKey: .noProgressAction)
        try container.encode(autoToolBundle, forKey: .autoToolBundle)
        try BudgetJSONCoding.encodeExtras(extraFields, known: CodingKeys.self, to: encoder)
    }

    var hasHardCap: Bool {
        hardTokenCap != nil || hardCostCapUSD != nil || hardWallClockSecs != nil
    }

    func hardCaps() -> AutoBudgetCaps {
        AutoBudgetCaps(
            token: hardTokenCap,
            costUSD: hardCostCapUSD,
            wallClockSecs: hardWallClockSecs
        )
    }

    func requireHardCap() throws {
        guard hasHardCap else {
            throw BudgetValidationIssue(
                code: "AUTO_BUDGET_CAP_REQUIRED",
                message: "Auto budget requires at least one hard token, cost, or wall-clock cap."
            )
        }
    }

    func validate() throws {
        try hardCaps().validate()
        if !extendRatio.isFinite || extendRatio <= 0 || extendRatio > 1 {
            throw BudgetValidationIssue(
                code: "INVALID_AUTO_BUDGET_POLICY",
                message: "Extend ratio must be between 0 and 1."
            )
        }
        if !maxMultiplier.isFinite || maxMultiplier < 1 {
            throw BudgetValidationIssue(
                code: "INVALID_AUTO_BUDGET_POLICY",
                message: "Maximum multiplier must be at least 1."
            )
        }
        if !(1 ... 1000).contains(forecastCalls) {
            throw BudgetValidationIssue(
                code: "INVALID_AUTO_BUDGET_POLICY",
                message: "Forecast calls must be between 1 and 1000."
            )
        }
        if minRunwayTurns > 1000 {
            throw BudgetValidationIssue(
                code: "INVALID_AUTO_BUDGET_POLICY",
                message: "Minimum runway turns must be at most 1000."
            )
        }
        for (name, value) in [
            ("Warn ratio", warnAtRatio),
            ("Pause ratio", pauseAtRatio),
        ] {
            if !value.isFinite || value < 0 || value > 1 {
                throw BudgetValidationIssue(
                    code: "INVALID_AUTO_BUDGET_POLICY",
                    message: "\(name) must be between 0 and 1."
                )
            }
        }
        if pauseAtRatio > warnAtRatio {
            throw BudgetValidationIssue(
                code: "INVALID_AUTO_BUDGET_POLICY",
                message: "Pause ratio must not exceed the warn ratio."
            )
        }
        if maxTurnsPerRun > 200 {
            throw BudgetValidationIssue(
                code: "INVALID_AUTO_BUDGET_POLICY",
                message: "Turns per run must be 0 (no cap) or 1-200."
            )
        }
        if let maxTotalTurns, maxTotalTurns == 0
            || (maxTurnsPerRun > 0 && maxTotalTurns < maxTurnsPerRun)
        {
            throw BudgetValidationIssue(
                code: "INVALID_AUTO_BUDGET_POLICY",
                message: "Total turns must be at least the per-run turn limit."
            )
        }
        if noProgressTurns > 1000 {
            throw BudgetValidationIssue(
                code: "INVALID_AUTO_BUDGET_POLICY",
                message: "No-progress turns must be at most 1000."
            )
        }
    }

    func jsonValue() throws -> JSONValue {
        let data = try JSONEncoder().encode(self)
        return try JSONDecoder().decode(JSONValue.self, from: data)
    }
}

struct AutoBudgetCaps: Codable, Hashable, Sendable {
    var token: UInt64?
    var costUSD: Double?
    var wallClockSecs: UInt64?

    enum CodingKeys: String, CodingKey, CaseIterable {
        case token
        case costUSD = "cost_usd"
        case wallClockSecs = "wall_clock_secs"
    }

    init(token: UInt64? = nil, costUSD: Double? = nil, wallClockSecs: UInt64? = nil) {
        self.token = token
        self.costUSD = costUSD
        self.wallClockSecs = wallClockSecs
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        token = try container.decodeIfPresent(UInt64.self, forKey: .token)
        costUSD = try container.decodeIfPresent(Double.self, forKey: .costUSD)
        wallClockSecs = try container.decodeIfPresent(UInt64.self, forKey: .wallClockSecs)
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encodeIfPresent(token, forKey: .token)
        try container.encodeIfPresent(costUSD, forKey: .costUSD)
        try container.encodeIfPresent(wallClockSecs, forKey: .wallClockSecs)
    }

    var isEmpty: Bool {
        token == nil && costUSD == nil && wallClockSecs == nil
    }

    func validate() throws {
        if token == 0 {
            throw BudgetValidationIssue(
                code: "INVALID_BUDGET_CAP",
                message: "Hard token cap must be greater than zero."
            )
        }
        if let costUSD, !costUSD.isFinite || costUSD <= 0 {
            throw BudgetValidationIssue(
                code: "INVALID_BUDGET_CAP",
                message: "Hard cost cap must be a positive USD value."
            )
        }
        if wallClockSecs == 0 {
            throw BudgetValidationIssue(
                code: "INVALID_BUDGET_CAP",
                message: "Hard wall-clock cap must be greater than zero."
            )
        }
    }
}

// MARK: - State and forecast

struct SessionAutoBudgetState: Codable, Hashable, Sendable {
    var extensions: UInt32
    var lastExtensionAt: String?
    var totalTurns: UInt32
    var turnsWithoutProgress: UInt32
    var lastProgressSeq: UInt64
    var lastForecast: BudgetForecast?
    var pausedReason: String?
    var pausedAt: String?
    var elapsedSecs: UInt64
    var progress: JSONValue?
    var extraFields: [String: JSONValue]

    enum CodingKeys: String, CodingKey, CaseIterable {
        case extensions
        case lastExtensionAt = "last_extension_at"
        case totalTurns = "total_turns"
        case turnsWithoutProgress = "turns_without_progress"
        case lastProgressSeq = "last_progress_seq"
        case lastForecast = "last_forecast"
        case pausedReason = "paused_reason"
        case pausedAt = "paused_at"
        case elapsedSecs = "elapsed_secs"
        case progress
    }

    init(
        extensions: UInt32 = 0,
        lastExtensionAt: String? = nil,
        totalTurns: UInt32 = 0,
        turnsWithoutProgress: UInt32 = 0,
        lastProgressSeq: UInt64 = 0,
        lastForecast: BudgetForecast? = nil,
        pausedReason: String? = nil,
        pausedAt: String? = nil,
        elapsedSecs: UInt64 = 0,
        progress: JSONValue? = nil,
        extraFields: [String: JSONValue] = [:]
    ) {
        self.extensions = extensions
        self.lastExtensionAt = lastExtensionAt
        self.totalTurns = totalTurns
        self.turnsWithoutProgress = turnsWithoutProgress
        self.lastProgressSeq = lastProgressSeq
        self.lastForecast = lastForecast
        self.pausedReason = pausedReason
        self.pausedAt = pausedAt
        self.elapsedSecs = elapsedSecs
        self.progress = progress
        self.extraFields = extraFields
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        extensions = try container.decodeIfPresent(UInt32.self, forKey: .extensions) ?? 0
        lastExtensionAt = try container.decodeIfPresent(String.self, forKey: .lastExtensionAt)
        totalTurns = try container.decodeIfPresent(UInt32.self, forKey: .totalTurns) ?? 0
        turnsWithoutProgress =
            try container.decodeIfPresent(UInt32.self, forKey: .turnsWithoutProgress) ?? 0
        lastProgressSeq =
            try container.decodeIfPresent(UInt64.self, forKey: .lastProgressSeq) ?? 0
        lastForecast = try container.decodeIfPresent(BudgetForecast.self, forKey: .lastForecast)
        pausedReason = try container.decodeIfPresent(String.self, forKey: .pausedReason)
        pausedAt = try container.decodeIfPresent(String.self, forKey: .pausedAt)
        elapsedSecs = try container.decodeIfPresent(UInt64.self, forKey: .elapsedSecs) ?? 0
        progress = try container.decodeIfPresent(JSONValue.self, forKey: .progress)
        extraFields = try BudgetJSONCoding.extras(from: decoder, known: CodingKeys.self)
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(extensions, forKey: .extensions)
        try container.encodeIfPresent(lastExtensionAt, forKey: .lastExtensionAt)
        try container.encode(totalTurns, forKey: .totalTurns)
        try container.encode(turnsWithoutProgress, forKey: .turnsWithoutProgress)
        try container.encode(lastProgressSeq, forKey: .lastProgressSeq)
        try container.encodeIfPresent(lastForecast, forKey: .lastForecast)
        try container.encodeIfPresent(pausedReason, forKey: .pausedReason)
        try container.encodeIfPresent(pausedAt, forKey: .pausedAt)
        try container.encode(elapsedSecs, forKey: .elapsedSecs)
        try container.encodeIfPresent(progress, forKey: .progress)
        try BudgetJSONCoding.encodeExtras(extraFields, known: CodingKeys.self, to: encoder)
    }
}

struct BudgetForecast: Codable, Hashable, Sendable {
    var sampleCalls: UInt32
    var avgInputTokensPerCall: UInt64
    var avgOutputTokensPerCall: UInt64
    var avgCostUSDPerCall: Double
    var remainingTokens: UInt64?
    var remainingCostUSD: Double?
    var remainingSecs: UInt64?
    var runwayTurns: UInt32?
    var runwaySecs: UInt64?
    var runwayCostUSD: Double?
    var criticalPathTurns: UInt32?
    var source: BudgetForecastSource
    var generatedAt: String
    var extraFields: [String: JSONValue]

    enum CodingKeys: String, CodingKey, CaseIterable {
        case sampleCalls = "sample_calls"
        case avgInputTokensPerCall = "avg_input_tokens_per_call"
        case avgOutputTokensPerCall = "avg_output_tokens_per_call"
        case avgCostUSDPerCall = "avg_cost_usd_per_call"
        case remainingTokens = "remaining_tokens"
        case remainingCostUSD = "remaining_cost_usd"
        case remainingSecs = "remaining_secs"
        case runwayTurns = "runway_turns"
        case runwaySecs = "runway_secs"
        case runwayCostUSD = "runway_cost_usd"
        case criticalPathTurns = "critical_path_turns"
        case source
        case generatedAt = "generated_at"
    }

    init(
        sampleCalls: UInt32 = 0,
        avgInputTokensPerCall: UInt64 = 0,
        avgOutputTokensPerCall: UInt64 = 0,
        avgCostUSDPerCall: Double = 0,
        remainingTokens: UInt64? = nil,
        remainingCostUSD: Double? = nil,
        remainingSecs: UInt64? = nil,
        runwayTurns: UInt32? = nil,
        runwaySecs: UInt64? = nil,
        runwayCostUSD: Double? = nil,
        criticalPathTurns: UInt32? = nil,
        source: BudgetForecastSource = .unavailable,
        generatedAt: String = "",
        extraFields: [String: JSONValue] = [:]
    ) {
        self.sampleCalls = sampleCalls
        self.avgInputTokensPerCall = avgInputTokensPerCall
        self.avgOutputTokensPerCall = avgOutputTokensPerCall
        self.avgCostUSDPerCall = avgCostUSDPerCall
        self.remainingTokens = remainingTokens
        self.remainingCostUSD = remainingCostUSD
        self.remainingSecs = remainingSecs
        self.runwayTurns = runwayTurns
        self.runwaySecs = runwaySecs
        self.runwayCostUSD = runwayCostUSD
        self.criticalPathTurns = criticalPathTurns
        self.source = source
        self.generatedAt = generatedAt
        self.extraFields = extraFields
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sampleCalls = try container.decodeIfPresent(UInt32.self, forKey: .sampleCalls) ?? 0
        avgInputTokensPerCall =
            try container.decodeIfPresent(UInt64.self, forKey: .avgInputTokensPerCall) ?? 0
        avgOutputTokensPerCall =
            try container.decodeIfPresent(UInt64.self, forKey: .avgOutputTokensPerCall) ?? 0
        avgCostUSDPerCall =
            try container.decodeIfPresent(Double.self, forKey: .avgCostUSDPerCall) ?? 0
        remainingTokens = try container.decodeIfPresent(UInt64.self, forKey: .remainingTokens)
        remainingCostUSD =
            try container.decodeIfPresent(Double.self, forKey: .remainingCostUSD)
        remainingSecs = try container.decodeIfPresent(UInt64.self, forKey: .remainingSecs)
        runwayTurns = try container.decodeIfPresent(UInt32.self, forKey: .runwayTurns)
        runwaySecs = try container.decodeIfPresent(UInt64.self, forKey: .runwaySecs)
        runwayCostUSD = try container.decodeIfPresent(Double.self, forKey: .runwayCostUSD)
        criticalPathTurns =
            try container.decodeIfPresent(UInt32.self, forKey: .criticalPathTurns)
        source = try container.decodeIfPresent(BudgetForecastSource.self, forKey: .source)
            ?? .unavailable
        generatedAt = try container.decodeIfPresent(String.self, forKey: .generatedAt) ?? ""
        extraFields = try BudgetJSONCoding.extras(from: decoder, known: CodingKeys.self)
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(sampleCalls, forKey: .sampleCalls)
        try container.encode(avgInputTokensPerCall, forKey: .avgInputTokensPerCall)
        try container.encode(avgOutputTokensPerCall, forKey: .avgOutputTokensPerCall)
        try container.encode(avgCostUSDPerCall, forKey: .avgCostUSDPerCall)
        try container.encodeIfPresent(remainingTokens, forKey: .remainingTokens)
        try container.encodeIfPresent(remainingCostUSD, forKey: .remainingCostUSD)
        try container.encodeIfPresent(remainingSecs, forKey: .remainingSecs)
        try container.encodeIfPresent(runwayTurns, forKey: .runwayTurns)
        try container.encodeIfPresent(runwaySecs, forKey: .runwaySecs)
        try container.encodeIfPresent(runwayCostUSD, forKey: .runwayCostUSD)
        try container.encodeIfPresent(criticalPathTurns, forKey: .criticalPathTurns)
        try container.encode(source, forKey: .source)
        try container.encode(generatedAt, forKey: .generatedAt)
        try BudgetJSONCoding.encodeExtras(extraFields, known: CodingKeys.self, to: encoder)
    }
}

struct SessionBudgetStatus: Codable, Hashable, Sendable {
    var sessionID: String
    var mode: BudgetMode
    var unlimited: Bool
    var effectiveTokenBudget: UInt64?
    var effectiveCostBudgetUSD: Double?
    var effectiveTimeBudgetSecs: UInt64?
    var hardTokenCap: UInt64?
    var hardCostCapUSD: Double?
    var hardWallClockSecs: UInt64?
    var spentTokens: UInt64
    var spentCostUSD: Double
    var elapsedSecs: UInt64
    var remainingTokens: UInt64?
    var remainingCostUSD: Double?
    var remainingSecs: UInt64?
    var policy: AutoBudgetPolicy?
    var state: SessionAutoBudgetState?
    var forecast: BudgetForecast
    var updatedAt: String?
    var extraFields: [String: JSONValue]

    enum CodingKeys: String, CodingKey, CaseIterable {
        case sessionID = "session_id"
        case mode
        case unlimited
        case effectiveTokenBudget = "effective_token_budget"
        case effectiveCostBudgetUSD = "effective_cost_budget_usd"
        case effectiveTimeBudgetSecs = "effective_time_budget_secs"
        case hardTokenCap = "hard_token_cap"
        case hardCostCapUSD = "hard_cost_cap_usd"
        case hardWallClockSecs = "hard_wall_clock_secs"
        case spentTokens = "spent_tokens"
        case spentCostUSD = "spent_cost_usd"
        case elapsedSecs = "elapsed_secs"
        case remainingTokens = "remaining_tokens"
        case remainingCostUSD = "remaining_cost_usd"
        case remainingSecs = "remaining_secs"
        case policy
        case state
        case forecast
        case updatedAt = "updated_at"
    }

    init(
        sessionID: String = "",
        mode: BudgetMode = .fixed,
        unlimited: Bool = false,
        effectiveTokenBudget: UInt64? = nil,
        effectiveCostBudgetUSD: Double? = nil,
        effectiveTimeBudgetSecs: UInt64? = nil,
        hardTokenCap: UInt64? = nil,
        hardCostCapUSD: Double? = nil,
        hardWallClockSecs: UInt64? = nil,
        spentTokens: UInt64 = 0,
        spentCostUSD: Double = 0,
        elapsedSecs: UInt64 = 0,
        remainingTokens: UInt64? = nil,
        remainingCostUSD: Double? = nil,
        remainingSecs: UInt64? = nil,
        policy: AutoBudgetPolicy? = nil,
        state: SessionAutoBudgetState? = nil,
        forecast: BudgetForecast = BudgetForecast(),
        updatedAt: String? = nil,
        extraFields: [String: JSONValue] = [:]
    ) {
        self.sessionID = sessionID
        self.mode = mode
        self.unlimited = unlimited
        self.effectiveTokenBudget = effectiveTokenBudget
        self.effectiveCostBudgetUSD = effectiveCostBudgetUSD
        self.effectiveTimeBudgetSecs = effectiveTimeBudgetSecs
        self.hardTokenCap = hardTokenCap
        self.hardCostCapUSD = hardCostCapUSD
        self.hardWallClockSecs = hardWallClockSecs
        self.spentTokens = spentTokens
        self.spentCostUSD = spentCostUSD
        self.elapsedSecs = elapsedSecs
        self.remainingTokens = remainingTokens
        self.remainingCostUSD = remainingCostUSD
        self.remainingSecs = remainingSecs
        self.policy = policy
        self.state = state
        self.forecast = forecast
        self.updatedAt = updatedAt
        self.extraFields = extraFields
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        mode = try container.decodeIfPresent(BudgetMode.self, forKey: .mode) ?? .fixed
        unlimited = try container.decodeIfPresent(Bool.self, forKey: .unlimited) ?? false
        effectiveTokenBudget =
            try container.decodeIfPresent(UInt64.self, forKey: .effectiveTokenBudget)
        effectiveCostBudgetUSD =
            try container.decodeIfPresent(Double.self, forKey: .effectiveCostBudgetUSD)
        effectiveTimeBudgetSecs =
            try container.decodeIfPresent(UInt64.self, forKey: .effectiveTimeBudgetSecs)
        hardTokenCap = try container.decodeIfPresent(UInt64.self, forKey: .hardTokenCap)
        hardCostCapUSD = try container.decodeIfPresent(Double.self, forKey: .hardCostCapUSD)
        hardWallClockSecs =
            try container.decodeIfPresent(UInt64.self, forKey: .hardWallClockSecs)
        spentTokens = try container.decodeIfPresent(UInt64.self, forKey: .spentTokens) ?? 0
        spentCostUSD = try container.decodeIfPresent(Double.self, forKey: .spentCostUSD) ?? 0
        elapsedSecs = try container.decodeIfPresent(UInt64.self, forKey: .elapsedSecs) ?? 0
        remainingTokens = try container.decodeIfPresent(UInt64.self, forKey: .remainingTokens)
        remainingCostUSD = try container.decodeIfPresent(Double.self, forKey: .remainingCostUSD)
        remainingSecs = try container.decodeIfPresent(UInt64.self, forKey: .remainingSecs)
        policy = try container.decodeIfPresent(AutoBudgetPolicy.self, forKey: .policy)
        state = try container.decodeIfPresent(SessionAutoBudgetState.self, forKey: .state)
        forecast = try container.decodeIfPresent(BudgetForecast.self, forKey: .forecast)
            ?? BudgetForecast()
        updatedAt = try container.decodeIfPresent(String.self, forKey: .updatedAt)
        extraFields = try BudgetJSONCoding.extras(from: decoder, known: CodingKeys.self)
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(sessionID, forKey: .sessionID)
        try container.encode(mode, forKey: .mode)
        try container.encode(unlimited, forKey: .unlimited)
        try container.encodeIfPresent(effectiveTokenBudget, forKey: .effectiveTokenBudget)
        try container.encodeIfPresent(effectiveCostBudgetUSD, forKey: .effectiveCostBudgetUSD)
        try container.encodeIfPresent(effectiveTimeBudgetSecs, forKey: .effectiveTimeBudgetSecs)
        try container.encodeIfPresent(hardTokenCap, forKey: .hardTokenCap)
        try container.encodeIfPresent(hardCostCapUSD, forKey: .hardCostCapUSD)
        try container.encodeIfPresent(hardWallClockSecs, forKey: .hardWallClockSecs)
        try container.encode(spentTokens, forKey: .spentTokens)
        try container.encode(spentCostUSD, forKey: .spentCostUSD)
        try container.encode(elapsedSecs, forKey: .elapsedSecs)
        try container.encodeIfPresent(remainingTokens, forKey: .remainingTokens)
        try container.encodeIfPresent(remainingCostUSD, forKey: .remainingCostUSD)
        try container.encodeIfPresent(remainingSecs, forKey: .remainingSecs)
        try container.encodeIfPresent(policy, forKey: .policy)
        try container.encodeIfPresent(state, forKey: .state)
        try container.encode(forecast, forKey: .forecast)
        try container.encodeIfPresent(updatedAt, forKey: .updatedAt)
        try BudgetJSONCoding.encodeExtras(extraFields, known: CodingKeys.self, to: encoder)
    }
}

struct SessionBudgetForecastResult: Codable, Hashable, Sendable {
    var sessionID: String
    var forecast: BudgetForecast

    enum CodingKeys: String, CodingKey, CaseIterable {
        case sessionID = "session_id"
        case forecast
    }

    init(sessionID: String = "", forecast: BudgetForecast = BudgetForecast()) {
        self.sessionID = sessionID
        self.forecast = forecast
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        forecast = try container.decodeIfPresent(BudgetForecast.self, forKey: .forecast)
            ?? BudgetForecast()
    }
}

struct SetSessionBudgetResult: Codable, Hashable, Sendable {
    var sessionID: String
    var budget: SessionBudgetStatus

    enum CodingKeys: String, CodingKey, CaseIterable {
        case sessionID = "session_id"
        case budget
    }

    init(sessionID: String = "", budget: SessionBudgetStatus = SessionBudgetStatus()) {
        self.sessionID = sessionID
        self.budget = budget
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID) ?? ""
        budget = try container.decodeIfPresent(SessionBudgetStatus.self, forKey: .budget)
            ?? SessionBudgetStatus()
    }
}

struct AutoBudgetPolicyResult: Codable, Hashable, Sendable {
    var projectID: String
    var policy: AutoBudgetPolicy
    var updatedAt: String?

    enum CodingKeys: String, CodingKey, CaseIterable {
        case projectID = "project_id"
        case policy
        case updatedAt = "updated_at"
    }

    init(
        projectID: String = "",
        policy: AutoBudgetPolicy = AutoBudgetPolicy(),
        updatedAt: String? = nil
    ) {
        self.projectID = projectID
        self.policy = policy
        self.updatedAt = updatedAt
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        projectID = try container.decodeIfPresent(String.self, forKey: .projectID) ?? ""
        policy = try container.decodeIfPresent(AutoBudgetPolicy.self, forKey: .policy)
            ?? AutoBudgetPolicy()
        updatedAt = try container.decodeIfPresent(String.self, forKey: .updatedAt)
    }
}

// MARK: - Mutations

struct SetSessionBudgetParams: Hashable, Sendable {
    var sessionID: String
    var mode: BudgetMode?
    var unlimited: Bool?
    var tokenBudget: UInt64?
    var costBudgetUSD: Double?
    var timeBudgetSecs: UInt64?
    var clearTokenBudget = false
    var clearCostBudget = false
    var clearTimeBudget = false
    var hardTokenCap: UInt64?
    var hardCostCapUSD: Double?
    var hardWallClockSecs: UInt64?
    var clearHardTokenCap = false
    var clearHardCostCap = false
    var clearHardWallClockCap = false
    var policy: AutoBudgetPolicy?

    init(sessionID: String) {
        self.sessionID = sessionID
    }

    /// Only meaningful fields are sent. Nil values mean "keep the current
    /// value"; the explicit clear flags remove an optional budget or cap.
    func requestObject() throws -> [String: JSONValue] {
        var object: [String: JSONValue] = ["session_id": .string(sessionID)]
        if let mode {
            object["mode"] = .string(mode.rawValue)
        }
        if let unlimited {
            object["unlimited"] = .bool(unlimited)
        }
        if let tokenBudget {
            object["token_budget"] = .number(Double(tokenBudget))
        }
        if let costBudgetUSD {
            object["cost_budget_usd"] = .number(costBudgetUSD)
        }
        if let timeBudgetSecs {
            object["time_budget_secs"] = .number(Double(timeBudgetSecs))
        }
        if clearTokenBudget {
            object["clear_token_budget"] = .bool(true)
        }
        if clearCostBudget {
            object["clear_cost_budget"] = .bool(true)
        }
        if clearTimeBudget {
            object["clear_time_budget"] = .bool(true)
        }
        if let hardTokenCap {
            object["hard_token_cap"] = .number(Double(hardTokenCap))
        }
        if let hardCostCapUSD {
            object["hard_cost_cap_usd"] = .number(hardCostCapUSD)
        }
        if let hardWallClockSecs {
            object["hard_wall_clock_secs"] = .number(Double(hardWallClockSecs))
        }
        if clearHardTokenCap {
            object["clear_hard_token_cap"] = .bool(true)
        }
        if clearHardCostCap {
            object["clear_hard_cost_cap"] = .bool(true)
        }
        if clearHardWallClockCap {
            object["clear_hard_wall_clock_cap"] = .bool(true)
        }
        if let policy {
            object["policy"] = try policy.jsonValue()
        }
        return object
    }
}

// MARK: - Validation surface

struct BudgetValidationIssue: Error, Equatable, Sendable {
    let code: String
    let message: String
    var hint: String?

    init(code: String, message: String, hint: String? = nil) {
        self.code = code
        self.message = message
        self.hint = hint
    }

    var displayText: String {
        let base = "\(code) · \(message)"
        guard let hint, !hint.isEmpty else { return base }
        return "\(base) — \(hint)"
    }

    static func from(_ error: Error) -> BudgetValidationIssue {
        if let issue = error as? BudgetValidationIssue {
            return issue
        }
        if let core = error as? CoreError {
            switch core {
            case let .rpc(_, message, data):
                return BudgetValidationIssue(
                    code: data?.objectValue?["code"]?.stringValue ?? "BUDGET_REQUEST_FAILED",
                    message: message,
                    hint: data?.objectValue?["hint"]?.stringValue
                )
            case .notConnected, .disconnected:
                return BudgetValidationIssue(
                    code: "DAEMON_UNAVAILABLE",
                    message: core.localizedDescription
                )
            case .timeout:
                return BudgetValidationIssue(
                    code: "BUDGET_REQUEST_TIMEOUT",
                    message: core.localizedDescription
                )
            default:
                break
            }
        }
        return BudgetValidationIssue(
            code: "BUDGET_REQUEST_FAILED",
            message: error.localizedDescription
        )
    }
}

// MARK: - Numeric parsing

enum BudgetFieldValue<Value: Equatable>: Equatable {
    case empty
    case value(Value)
    case invalid
}

enum BudgetFieldParsing {
    static func uint64(_ text: String) -> BudgetFieldValue<UInt64> {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return .empty }
        guard let value = UInt64(trimmed), value > 0 else { return .invalid }
        return .value(value)
    }

    static func usd(_ text: String) -> BudgetFieldValue<Double> {
        var trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.hasPrefix("$") {
            trimmed.removeFirst()
            trimmed = trimmed.trimmingCharacters(in: .whitespaces)
        }
        guard !trimmed.isEmpty else { return .empty }
        guard let value = Double(trimmed), value.isFinite, value > 0 else { return .invalid }
        return .value(value)
    }

    static func optionalUInt64(
        _ text: String,
        code: String,
        name: String
    ) throws -> UInt64? {
        switch uint64(text) {
        case .empty:
            return nil
        case let .value(value):
            return value
        case .invalid:
            throw BudgetValidationIssue(
                code: code,
                message: "\(name) must be a whole number greater than zero."
            )
        }
    }

    static func optionalUSD(
        _ text: String,
        code: String,
        name: String
    ) throws -> Double? {
        switch usd(text) {
        case .empty:
            return nil
        case let .value(value):
            return value
        case .invalid:
            throw BudgetValidationIssue(
                code: code,
                message: "\(name) must be a positive USD amount."
            )
        }
    }
}

// MARK: - Create draft

enum BudgetAutoCapKind: String, CaseIterable, Identifiable, Sendable {
    case usd
    case tokens

    var id: String {
        rawValue
    }
}

struct SessionCreateBudgetArguments: Equatable, Sendable {
    var mode: BudgetMode
    var tokenBudget: UInt64?
    var costBudgetUSD: Double?
    var timeBudgetSecs: UInt64?
    var unlimited: Bool
    var autoPolicy: AutoBudgetPolicy?
}

/// The New Session budget preset form. Auto is the default and requires one
/// explicit USD or token hard cap; Unlimited is only ever selected by the
/// operator, never inferred from a missing value.
struct SessionBudgetPresetDraft: Equatable {
    var preset: BudgetMode = .auto
    /// Tokens are always enforceable; a USD cap only works when the project
    /// has role prices configured, so tokens are the blank/first-run default.
    /// An explicit project hard USD cap still selects USD below.
    var capKind: BudgetAutoCapKind = .tokens
    var autoCapText = ""
    var autoWallClockText = ""
    var fixedTokenText = "200000"
    var fixedCostText = ""
    var fixedTimeText = ""

    init(
        projectPolicy: AutoBudgetPolicy? = nil,
        defaultTokenBudget: Int = 200_000
    ) {
        fixedTokenText = "\(max(1, defaultTokenBudget))"
        if let hardCost = projectPolicy?.hardCostCapUSD {
            capKind = .usd
            autoCapText = BudgetFormatting.plainNumber(hardCost)
        } else if let hardTokens = projectPolicy?.hardTokenCap {
            capKind = .tokens
            autoCapText = "\(hardTokens)"
        }
        if let wall = projectPolicy?.hardWallClockSecs {
            autoWallClockText = "\(wall)"
        }
    }

    /// Applies project defaults only while the operator has not typed a value.
    mutating func applyProjectDefaults(_ policy: AutoBudgetPolicy) {
        if autoCapText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            if let hardCost = policy.hardCostCapUSD {
                capKind = .usd
                autoCapText = BudgetFormatting.plainNumber(hardCost)
            } else if let hardTokens = policy.hardTokenCap {
                capKind = .tokens
                autoCapText = "\(hardTokens)"
            }
        }
        if autoWallClockText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
           let wall = policy.hardWallClockSecs
        {
            autoWallClockText = "\(wall)"
        }
    }

    func validation(basePolicy: AutoBudgetPolicy) -> BudgetValidationIssue? {
        do {
            _ = try arguments(basePolicy: basePolicy)
            return nil
        } catch {
            return BudgetValidationIssue.from(error)
        }
    }

    func arguments(basePolicy: AutoBudgetPolicy) throws -> SessionCreateBudgetArguments {
        switch preset {
        case .auto:
            var policy = basePolicy
            policy.enabled = true
            switch capKind {
            case .usd:
                let cap = try BudgetFieldParsing.optionalUSD(
                    autoCapText,
                    code: "INVALID_BUDGET_CAP",
                    name: "Maximum USD"
                )
                guard cap != nil else {
                    throw BudgetValidationIssue(
                        code: "AUTO_BUDGET_CAP_REQUIRED",
                        message: "Auto budget requires a maximum USD or token cap."
                    )
                }
                policy.hardCostCapUSD = cap
            case .tokens:
                let cap = try BudgetFieldParsing.optionalUInt64(
                    autoCapText,
                    code: "INVALID_BUDGET_CAP",
                    name: "Maximum tokens"
                )
                guard cap != nil else {
                    throw BudgetValidationIssue(
                        code: "AUTO_BUDGET_CAP_REQUIRED",
                        message: "Auto budget requires a maximum USD or token cap."
                    )
                }
                policy.hardTokenCap = cap
            }
            if let wall = try BudgetFieldParsing.optionalUInt64(
                autoWallClockText,
                code: "INVALID_BUDGET_CAP",
                name: "Maximum wall clock"
            ) {
                policy.hardWallClockSecs = wall
            }
            try policy.requireHardCap()
            try policy.validate()
            return SessionCreateBudgetArguments(
                mode: .auto,
                tokenBudget: nil,
                costBudgetUSD: nil,
                timeBudgetSecs: nil,
                unlimited: false,
                autoPolicy: policy
            )
        case .fixed:
            let token = try BudgetFieldParsing.optionalUInt64(
                fixedTokenText,
                code: "INVALID_BUDGET",
                name: "Token budget"
            )
            guard let token else {
                throw BudgetValidationIssue(
                    code: "INVALID_BUDGET",
                    message: "A fixed budget requires a token budget."
                )
            }
            let cost = try BudgetFieldParsing.optionalUSD(
                fixedCostText,
                code: "INVALID_BUDGET",
                name: "USD budget"
            )
            let time = try BudgetFieldParsing.optionalUInt64(
                fixedTimeText,
                code: "INVALID_BUDGET",
                name: "Wall-clock budget"
            )
            return SessionCreateBudgetArguments(
                mode: .fixed,
                tokenBudget: token,
                costBudgetUSD: cost,
                timeBudgetSecs: time,
                unlimited: false,
                autoPolicy: nil
            )
        case .unlimited:
            return SessionCreateBudgetArguments(
                mode: .unlimited,
                tokenBudget: nil,
                costBudgetUSD: nil,
                timeBudgetSecs: nil,
                unlimited: true,
                autoPolicy: nil
            )
        default:
            throw BudgetValidationIssue(
                code: "INVALID_BUDGET_MODE",
                message: "Unknown budget mode \(preset.rawValue)."
            )
        }
    }
}

// MARK: - Session edit draft

struct SessionBudgetEditDraft: Equatable {
    var mode: BudgetMode
    var tokenBudgetText: String
    var costBudgetText: String
    var timeBudgetText: String
    var hardTokenCapText: String
    var hardCostCapText: String
    var hardWallClockText: String
    var policy: AutoBudgetPolicy?
    private let initialTokenBudget: UInt64?
    private let initialCostBudgetUSD: Double?
    private let initialTimeBudgetSecs: UInt64?
    private let initialHardTokenCap: UInt64?
    private let initialHardCostCapUSD: Double?
    private let initialHardWallClockSecs: UInt64?

    init(status: SessionBudgetStatus) {
        mode = status.unlimited ? .unlimited : status.mode
        tokenBudgetText = status.effectiveTokenBudget.map { "\($0)" } ?? ""
        costBudgetText = status.effectiveCostBudgetUSD.map {
            BudgetFormatting.plainNumber($0)
        } ?? ""
        timeBudgetText = status.effectiveTimeBudgetSecs.map { "\($0)" } ?? ""
        hardTokenCapText = status.hardTokenCap.map { "\($0)" } ?? ""
        hardCostCapText = status.hardCostCapUSD.map {
            BudgetFormatting.plainNumber($0)
        } ?? ""
        hardWallClockText = status.hardWallClockSecs.map { "\($0)" } ?? ""
        policy = status.policy
        initialTokenBudget = status.effectiveTokenBudget
        initialCostBudgetUSD = status.effectiveCostBudgetUSD
        initialTimeBudgetSecs = status.effectiveTimeBudgetSecs
        initialHardTokenCap = status.hardTokenCap
        initialHardCostCapUSD = status.hardCostCapUSD
        initialHardWallClockSecs = status.hardWallClockSecs
    }

    func validation(status: SessionBudgetStatus) -> BudgetValidationIssue? {
        do {
            _ = try params(sessionID: status.sessionID, status: status)
            return nil
        } catch {
            return BudgetValidationIssue.from(error)
        }
    }

    func params(
        sessionID: String,
        status: SessionBudgetStatus
    ) throws -> SetSessionBudgetParams {
        var params = SetSessionBudgetParams(sessionID: sessionID)
        params.mode = mode
        switch mode {
        case .unlimited:
            params.unlimited = true
            return params
        case .fixed:
            params.unlimited = false
            let token = try BudgetFieldParsing.optionalUInt64(
                tokenBudgetText,
                code: "INVALID_BUDGET",
                name: "Token budget"
            )
            let cost = try BudgetFieldParsing.optionalUSD(
                costBudgetText,
                code: "INVALID_BUDGET",
                name: "USD budget"
            )
            let time = try BudgetFieldParsing.optionalUInt64(
                timeBudgetText,
                code: "INVALID_BUDGET",
                name: "Wall-clock budget"
            )
            guard token != nil || cost != nil || time != nil else {
                throw BudgetValidationIssue(
                    code: "INVALID_BUDGET",
                    message: "A fixed budget requires at least one positive budget."
                )
            }
            try validateAtOrAboveSpend(
                token: token,
                cost: cost,
                status: status
            )
            params.tokenBudget = token
            params.costBudgetUSD = cost
            params.timeBudgetSecs = time
            params.clearTokenBudget = token == nil && initialTokenBudget != nil
            params.clearCostBudget = cost == nil && initialCostBudgetUSD != nil
            params.clearTimeBudget = time == nil && initialTimeBudgetSecs != nil
            return params
        case .auto:
            params.unlimited = false
            let token = try BudgetFieldParsing.optionalUInt64(
                tokenBudgetText,
                code: "INVALID_BUDGET",
                name: "Starting token budget"
            )
            let cost = try BudgetFieldParsing.optionalUSD(
                costBudgetText,
                code: "INVALID_BUDGET",
                name: "Starting USD budget"
            )
            let time = try BudgetFieldParsing.optionalUInt64(
                timeBudgetText,
                code: "INVALID_BUDGET",
                name: "Starting wall clock"
            )
            let hardToken = try BudgetFieldParsing.optionalUInt64(
                hardTokenCapText,
                code: "INVALID_BUDGET_CAP",
                name: "Hard token cap"
            )
            let hardCost = try BudgetFieldParsing.optionalUSD(
                hardCostCapText,
                code: "INVALID_BUDGET_CAP",
                name: "Hard USD cap"
            )
            let hardWall = try BudgetFieldParsing.optionalUInt64(
                hardWallClockText,
                code: "INVALID_BUDGET_CAP",
                name: "Hard wall-clock cap"
            )
            var policy = policy ?? AutoBudgetPolicy()
            policy.enabled = true
            policy.hardTokenCap = hardToken
            policy.hardCostCapUSD = hardCost
            policy.hardWallClockSecs = hardWall
            policy.initialTokenBudget = token
            policy.initialCostBudgetUSD = cost
            policy.initialWallClockSecs = time
            try policy.requireHardCap()
            try policy.validate()
            try validateAtOrAboveSpend(token: token, cost: cost, status: status)
            try validateCapsAtOrAboveSpend(
                token: hardToken,
                cost: hardCost,
                status: status
            )
            params.policy = policy
            params.tokenBudget = token
            params.costBudgetUSD = cost
            params.timeBudgetSecs = time
            params.hardTokenCap = hardToken
            params.hardCostCapUSD = hardCost
            params.hardWallClockSecs = hardWall
            params.clearTokenBudget = token == nil && initialTokenBudget != nil
            params.clearCostBudget = cost == nil && initialCostBudgetUSD != nil
            params.clearTimeBudget = time == nil && initialTimeBudgetSecs != nil
            params.clearHardTokenCap = hardToken == nil && initialHardTokenCap != nil
            params.clearHardCostCap = hardCost == nil && initialHardCostCapUSD != nil
            params.clearHardWallClockCap =
                hardWall == nil && initialHardWallClockSecs != nil
            return params
        default:
            throw BudgetValidationIssue(
                code: "INVALID_BUDGET_MODE",
                message: "Unknown budget mode \(mode.rawValue)."
            )
        }
    }

    private func validateAtOrAboveSpend(
        token: UInt64?,
        cost: Double?,
        status: SessionBudgetStatus
    ) throws {
        if let token, token < status.spentTokens {
            throw BudgetValidationIssue(
                code: "BUDGET_BELOW_SPENT",
                message: "Token budget is below tokens already spent (\(status.spentTokens))."
            )
        }
        if let cost, cost + 0.000_000_001 < status.spentCostUSD {
            throw BudgetValidationIssue(
                code: "BUDGET_BELOW_SPENT",
                message: "USD budget is below cost already spent (\(BudgetFormatting.money(status.spentCostUSD)))."
            )
        }
    }

    private func validateCapsAtOrAboveSpend(
        token: UInt64?,
        cost: Double?,
        status: SessionBudgetStatus
    ) throws {
        if let token, token < status.spentTokens {
            throw BudgetValidationIssue(
                code: "BUDGET_BELOW_SPENT",
                message: "Hard token cap is below tokens already spent (\(status.spentTokens))."
            )
        }
        if let cost, cost + 0.000_000_001 < status.spentCostUSD {
            throw BudgetValidationIssue(
                code: "BUDGET_BELOW_SPENT",
                message: "Hard USD cap is below cost already spent (\(BudgetFormatting.money(status.spentCostUSD)))."
            )
        }
    }
}

// MARK: - Project policy draft

struct AutoBudgetPolicyDraft: Equatable {
    var enabled = false
    var hardTokenCapText = ""
    var hardCostCapText = ""
    var hardWallClockText = ""
    var initialTokenBudgetText = ""
    var initialCostBudgetText = ""
    var initialWallClockText = ""
    var extendRatio = 0.25
    var maxMultiplier = 4.0
    var forecastCalls = 8
    var minRunwayTurns = 4
    var warnAtRatio = 0.20
    var pauseAtRatio = 0.05
    var maxTurnsPerRun = 0
    var maxTotalTurnsText = ""
    var noProgressTurns = 6
    var noProgressAction = NoProgressAction.pause
    var autoToolBundle = true
    private var extraFields: [String: JSONValue] = [:]

    init(policy: AutoBudgetPolicy = AutoBudgetPolicy()) {
        enabled = policy.enabled
        hardTokenCapText = policy.hardTokenCap.map { "\($0)" } ?? ""
        hardCostCapText = policy.hardCostCapUSD.map {
            BudgetFormatting.plainNumber($0)
        } ?? ""
        hardWallClockText = policy.hardWallClockSecs.map { "\($0)" } ?? ""
        initialTokenBudgetText = policy.initialTokenBudget.map { "\($0)" } ?? ""
        initialCostBudgetText = policy.initialCostBudgetUSD.map {
            BudgetFormatting.plainNumber($0)
        } ?? ""
        initialWallClockText = policy.initialWallClockSecs.map { "\($0)" } ?? ""
        extendRatio = policy.extendRatio
        maxMultiplier = policy.maxMultiplier
        forecastCalls = Int(policy.forecastCalls)
        minRunwayTurns = Int(policy.minRunwayTurns)
        warnAtRatio = policy.warnAtRatio
        pauseAtRatio = policy.pauseAtRatio
        maxTurnsPerRun = Int(policy.maxTurnsPerRun)
        maxTotalTurnsText = policy.maxTotalTurns.map { "\($0)" } ?? ""
        noProgressTurns = Int(policy.noProgressTurns)
        noProgressAction = policy.noProgressAction
        autoToolBundle = policy.autoToolBundle
        extraFields = policy.extraFields
    }

    func validation() -> BudgetValidationIssue? {
        do {
            _ = try makePolicy()
            return nil
        } catch {
            return BudgetValidationIssue.from(error)
        }
    }

    func makePolicy() throws -> AutoBudgetPolicy {
        let hardToken = try BudgetFieldParsing.optionalUInt64(
            hardTokenCapText,
            code: "INVALID_BUDGET_CAP",
            name: "Hard token cap"
        )
        let hardCost = try BudgetFieldParsing.optionalUSD(
            hardCostCapText,
            code: "INVALID_BUDGET_CAP",
            name: "Hard USD cap"
        )
        let hardWall = try BudgetFieldParsing.optionalUInt64(
            hardWallClockText,
            code: "INVALID_BUDGET_CAP",
            name: "Hard wall-clock cap"
        )
        let initialToken = try BudgetFieldParsing.optionalUInt64(
            initialTokenBudgetText,
            code: "INVALID_BUDGET",
            name: "Initial token budget"
        )
        let initialCost = try BudgetFieldParsing.optionalUSD(
            initialCostBudgetText,
            code: "INVALID_BUDGET",
            name: "Initial USD budget"
        )
        let initialWall = try BudgetFieldParsing.optionalUInt64(
            initialWallClockText,
            code: "INVALID_BUDGET",
            name: "Initial wall-clock budget"
        )
        let maxTotal = try BudgetFieldParsing.optionalUInt64(
            maxTotalTurnsText,
            code: "INVALID_AUTO_BUDGET_POLICY",
            name: "Total turns"
        )
        guard let forecastCalls = UInt32(exactly: forecastCalls) else {
            throw BudgetValidationIssue(
                code: "INVALID_AUTO_BUDGET_POLICY",
                message: "Forecast calls must be between 1 and 1000."
            )
        }
        guard let minRunwayTurns = UInt32(exactly: minRunwayTurns) else {
            throw BudgetValidationIssue(
                code: "INVALID_AUTO_BUDGET_POLICY",
                message: "Minimum runway turns must be at most 1000."
            )
        }
        guard let maxTurnsPerRun = UInt32(exactly: maxTurnsPerRun),
              maxTurnsPerRun <= 200
        else {
            throw BudgetValidationIssue(
                code: "INVALID_AUTO_BUDGET_POLICY",
                message: "Turns per run must be 0 (no cap) or 1-200."
            )
        }
        guard let noProgressTurns = UInt32(exactly: noProgressTurns) else {
            throw BudgetValidationIssue(
                code: "INVALID_AUTO_BUDGET_POLICY",
                message: "No-progress turns must be at most 1000."
            )
        }
        let maxTotalTurns: UInt32?
        if let maxTotal {
            guard let value = UInt32(exactly: maxTotal) else {
                throw BudgetValidationIssue(
                    code: "INVALID_AUTO_BUDGET_POLICY",
                    message: "Total turns is too large."
                )
            }
            maxTotalTurns = value
        } else {
            maxTotalTurns = nil
        }
        let policy = AutoBudgetPolicy(
            enabled: enabled,
            initialTokenBudget: initialToken,
            initialCostBudgetUSD: initialCost,
            initialWallClockSecs: initialWall,
            hardTokenCap: hardToken,
            hardCostCapUSD: hardCost,
            hardWallClockSecs: hardWall,
            extendRatio: extendRatio,
            maxMultiplier: maxMultiplier,
            forecastCalls: forecastCalls,
            minRunwayTurns: minRunwayTurns,
            warnAtRatio: warnAtRatio,
            pauseAtRatio: pauseAtRatio,
            maxTurnsPerRun: maxTurnsPerRun,
            maxTotalTurns: maxTotalTurns,
            noProgressTurns: noProgressTurns,
            noProgressAction: noProgressAction,
            autoToolBundle: autoToolBundle,
            extraFields: extraFields
        )
        try policy.validate()
        if enabled {
            try policy.requireHardCap()
        }
        return policy
    }
}

// MARK: - Runway summary

enum BudgetRunwayTone: Equatable, Sendable {
    case normal
    case warning
    case critical
    case paused
    case unlimited
}

struct BudgetRunwaySummary: Equatable, Sendable {
    let mode: BudgetMode
    let unlimited: Bool
    let effectiveTokenBudget: UInt64?
    let effectiveCostBudgetUSD: Double?
    let effectiveTimeBudgetSecs: UInt64?
    let hardTokenCap: UInt64?
    let hardCostCapUSD: Double?
    let hardWallClockSecs: UInt64?
    let spentTokens: UInt64
    let spentCostUSD: Double
    let elapsedSecs: UInt64
    let remainingTokens: UInt64?
    let remainingCostUSD: Double?
    let remainingSecs: UInt64?
    let usedRatio: Double
    let headline: String
    let effectiveLabel: String
    let hardCapLabel: String
    let spentLabel: String
    let remainingLabel: String
    let runwayLabel: String
    let runwayTurns: UInt32?
    let runwaySeconds: UInt64?
    let runwayCostUSD: Double?
    let extensionCount: UInt32
    let pauseReason: String?
    let pauseReasonLabel: String?
    let noProgressTurns: UInt32
    let noProgressThreshold: UInt32
    let noProgressLabel: String?
    let forecastSource: BudgetForecastSource
    let forecastSourceLabel: String
    let criticalPathTurns: UInt32?
    let tone: BudgetRunwayTone
    let updatedAt: String?

    init(status: SessionBudgetStatus) {
        let effectiveMode = status.unlimited ? BudgetMode.unlimited : status.mode
        mode = effectiveMode
        unlimited = status.unlimited || effectiveMode == .unlimited
        effectiveTokenBudget = status.effectiveTokenBudget
        effectiveCostBudgetUSD = status.effectiveCostBudgetUSD
        effectiveTimeBudgetSecs = status.effectiveTimeBudgetSecs
        hardTokenCap = status.hardTokenCap
        hardCostCapUSD = status.hardCostCapUSD
        hardWallClockSecs = status.hardWallClockSecs
        spentTokens = status.spentTokens
        spentCostUSD = status.spentCostUSD
        elapsedSecs = status.elapsedSecs
        remainingTokens = status.remainingTokens
        remainingCostUSD = status.remainingCostUSD
        remainingSecs = status.remainingSecs

        var ratios: [Double] = []
        if let budget = status.effectiveTokenBudget, budget > 0 {
            ratios.append(Double(status.spentTokens) / Double(budget))
        }
        if let budget = status.effectiveCostBudgetUSD, budget > 0 {
            ratios.append(status.spentCostUSD / budget)
        }
        if let budget = status.effectiveTimeBudgetSecs, budget > 0 {
            ratios.append(Double(status.elapsedSecs) / Double(budget))
        }
        let rawRatio = ratios.max() ?? 0
        usedRatio = rawRatio.isFinite ? min(1, max(0, rawRatio)) : 0

        extensionCount = status.state?.extensions ?? 0
        let pausedReason = status.state?.pausedReason
        pauseReason = pausedReason
        let progressTurns = status.state?.turnsWithoutProgress ?? 0
        let progressThreshold = status.policy?.noProgressTurns ?? 0
        noProgressTurns = progressTurns
        noProgressThreshold = progressThreshold
        forecastSource = status.forecast.source
        criticalPathTurns = status.forecast.criticalPathTurns
        runwayTurns = status.forecast.runwayTurns
        runwaySeconds = status.forecast.runwaySecs ?? status.remainingSecs
        runwayCostUSD = status.forecast.runwayCostUSD ?? status.remainingCostUSD
        updatedAt = status.updatedAt

        effectiveLabel = BudgetFormatting.combined(
            tokens: status.effectiveTokenBudget,
            cost: status.effectiveCostBudgetUSD,
            seconds: status.effectiveTimeBudgetSecs,
            empty: "Not budgeted"
        )
        hardCapLabel = BudgetFormatting.combined(
            tokens: status.hardTokenCap,
            cost: status.hardCostCapUSD,
            seconds: status.hardWallClockSecs,
            empty: "No hard cap"
        )
        spentLabel = BudgetFormatting.combined(
            tokens: status.spentTokens,
            cost: status.spentCostUSD,
            seconds: status.elapsedSecs,
            empty: "Nothing spent"
        )
        remainingLabel = BudgetFormatting.combined(
            tokens: status.remainingTokens,
            cost: status.remainingCostUSD,
            seconds: status.remainingSecs,
            empty: "Unbounded"
        )
        runwayLabel = BudgetFormatting.runway(
            turns: status.forecast.runwayTurns,
            seconds: status.forecast.runwaySecs,
            cost: status.forecast.runwayCostUSD
        )
        forecastSourceLabel = BudgetFormatting.forecastSource(status.forecast.source)
        pauseReasonLabel = pausedReason.map {
            BudgetFormatting.pauseReason(
                $0,
                turnsWithoutProgress: progressTurns,
                threshold: progressThreshold
            )
        }
        noProgressLabel = effectiveMode.isAuto && progressThreshold > 0
            ? "\(progressTurns) / \(progressThreshold) turns without progress"
            : nil

        var headlineParts: [String] = []
        if unlimited {
            headlineParts.append("Unlimited")
            headlineParts.append("economic gates bypassed")
        } else {
            headlineParts.append(BudgetFormatting.mode(effectiveMode))
            let percentLeft = Int(((1 - usedRatio) * 100).rounded())
            headlineParts.append("\(max(0, min(100, percentLeft)))% left")
            if let turns = status.forecast.runwayTurns {
                headlineParts.append("~\(turns) turns")
            }
        }
        headline = headlineParts.joined(separator: " · ")

        let minimumRunway = status.policy?.minRunwayTurns
        if status.state?.pausedReason != nil {
            tone = .paused
        } else if unlimited {
            tone = .unlimited
        } else if usedRatio >= 0.99 || status.remainingTokens == 0
            || status.remainingCostUSD == 0 || status.remainingSecs == 0
        {
            tone = .critical
        } else if usedRatio >= 0.80 {
            tone = .warning
        } else if let runway = status.forecast.runwayTurns,
                  let minimumRunway,
                  minimumRunway > 0,
                  runway <= minimumRunway
        {
            tone = .warning
        } else {
            tone = .normal
        }
    }

    var needsAttention: Bool {
        tone == .warning || tone == .critical || tone == .paused
    }

    var isPaused: Bool {
        pauseReason != nil
    }

    var resumeTitle: String {
        mode.isAuto ? "Resume Auto Budget Run" : "Resume Run"
    }
}

extension BudgetMode {
    var label: String {
        switch rawValue {
        case "auto": "Auto"
        case "fixed": "Fixed"
        case "unlimited": "Unlimited"
        default: rawValue
        }
    }
}

extension NoProgressAction {
    var label: String {
        switch rawValue {
        case "pause": "Pause"
        case "escalate": "Escalate"
        default: rawValue
        }
    }
}

// MARK: - Formatting

enum BudgetFormatting {
    static func mode(_ mode: BudgetMode) -> String {
        mode.label
    }

    static func tokens(_ value: UInt64?) -> String {
        guard let value else { return "—" }
        if value >= 1_000_000 {
            return String(format: "%.1fM", Double(value) / 1_000_000)
        }
        if value >= 1000 {
            return String(format: "%.1fk", Double(value) / 1000)
        }
        return "\(value)"
    }

    static func money(_ value: Double?) -> String {
        guard let value, value.isFinite else { return "—" }
        if value >= 1000 {
            return String(format: "$%.1f", value)
        }
        if value > 0, value < 0.01 {
            return String(format: "$%.4f", value)
        }
        return String(format: "$%.2f", value)
    }

    static func seconds(_ value: UInt64?) -> String {
        guard let value else { return "—" }
        let hours = value / 3600
        let minutes = (value % 3600) / 60
        let seconds = value % 60
        if hours > 0 {
            return minutes > 0 ? "\(hours)h \(minutes)m" : "\(hours)h"
        }
        if minutes > 0 {
            return seconds > 0 ? "\(minutes)m \(seconds)s" : "\(minutes)m"
        }
        return "\(seconds)s"
    }

    static func combined(
        tokens: UInt64?,
        cost: Double?,
        seconds: UInt64?,
        empty: String
    ) -> String {
        var parts: [String] = []
        if let tokens {
            parts.append("\(BudgetFormatting.tokens(tokens)) tokens")
        }
        if let cost {
            parts.append(money(cost))
        }
        if let seconds {
            parts.append(BudgetFormatting.seconds(seconds))
        }
        return parts.isEmpty ? empty : parts.joined(separator: " · ")
    }

    static func runway(
        turns: UInt32?,
        seconds: UInt64?,
        cost: Double?
    ) -> String {
        var parts: [String] = []
        if let turns {
            parts.append("~\(turns) turns")
        }
        if let seconds {
            parts.append(BudgetFormatting.seconds(seconds))
        }
        if let cost {
            parts.append(money(cost))
        }
        return parts.isEmpty ? "No forecast samples" : parts.joined(separator: " · ")
    }

    static func forecastSource(_ source: BudgetForecastSource) -> String {
        switch source.rawValue.lowercased() {
        case "providerledger", "provider_ledger": "provider ledger"
        case "mixed": "mixed ledger"
        case "estimate": "estimate"
        case "unavailable": "unavailable"
        default: source.rawValue
        }
    }

    static func plainNumber(_ value: Double) -> String {
        if value == value.rounded() {
            return String(format: "%.0f", value)
        }
        var text = String(format: "%.4f", value)
        while text.hasSuffix("0") {
            text.removeLast()
        }
        if text.hasSuffix(".") {
            text.removeLast()
        }
        return text
    }

    static func pauseReason(
        _ rawReason: String,
        turnsWithoutProgress: UInt32,
        threshold: UInt32
    ) -> String {
        switch rawReason.lowercased() {
        case "no_progress":
            if threshold > 0 {
                return "No progress for \(turnsWithoutProgress) of \(threshold) turns; the guard paused this run."
            }
            return "No progress was detected; the guard paused this run."
        case "budget", "budget_exhausted", "auto_budget_exhausted":
            return "The hard budget cap was reached; the session paused instead of overspending."
        case "operator":
            return "Paused by the operator."
        default:
            let human = rawReason
                .replacingOccurrences(of: "_", with: " ")
                .trimmingCharacters(in: .whitespacesAndNewlines)
            return human.isEmpty ? "Session auto-paused." : "Session auto-paused: \(human)."
        }
    }

    static func extensionReason(_ rawReason: String) -> String {
        switch rawReason.lowercased() {
        case "auto_budget_extension":
            "minimum runway"
        case "forecast_runway", "runway":
            "forecast runway"
        case "operator":
            "operator request"
        default:
            rawReason
                .replacingOccurrences(of: "_", with: " ")
                .trimmingCharacters(in: .whitespacesAndNewlines)
        }
    }
}

// MARK: - Client protocol

/// The five auto-budget RPCs. `CoreClient` is the production implementation;
/// tests inject a fake without starting a daemon. Every method is one explicit
/// request — the app never auto-retries a budget mutation.
protocol BudgetRPC: Sendable {
    func sessionBudget(sessionID: String) async throws -> SessionBudgetStatus
    func sessionBudgetForecast(sessionID: String) async throws -> SessionBudgetForecastResult
    func setSessionBudget(_ params: SetSessionBudgetParams) async throws -> SetSessionBudgetResult
    func projectAutoBudget(projectID: String) async throws -> AutoBudgetPolicyResult
    func setProjectAutoBudget(
        projectID: String,
        policy: AutoBudgetPolicy
    ) async throws -> AutoBudgetPolicyResult
}

extension CoreClient: BudgetRPC {}
