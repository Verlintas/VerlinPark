import Foundation

/// A read-only snapshot assembled from the usage, context, and optional cache
/// RPCs. It contains only values the daemon reported (or values derived from
/// those numbers); the view layer never invents usage.
struct EfficiencySnapshot: Equatable, Sendable {
    enum Scope: Equatable, Sendable {
        case session
        case project
    }

    let scope: Scope
    let cacheHitRatio: Double?
    let cacheReadTokens: Int?
    let cacheWriteTokens: Int?
    let uncachedInputTokens: Int?
    let outputTokens: Int?
    let totalCostUsd: Double?
    let sessionCount: Int?
    let taskCount: Int?
    let contextEstimatedTokens: Int?
    let contextWindow: Int?
    let contextThresholdTokens: Int?
    let contextHardLimitTokens: Int?
    let contextMessages: Int?
    let compactionCount: Int?
    let compactionLatencyMs: Int?
    let compactionTokensBefore: Int?
    let compactionTokensAfter: Int?
    let compactionBackgroundReuse: Bool?
    let compactionFallbackToSync: Bool?
    let toolOutputEntries: Int?
    let toolOutputBytes: Int?
    let toolOutputActiveEntries: Int?
    let toolOutputEnabled: Bool?
    let toolOutputSpillThresholdBytes: Int?
    let toolOutputPreviewBytes: Int?
    let toolOutputRetentionSecs: Int?
    let toolOutputMaxEntries: Int?
    let preambleTokensEstimate: Int?
    let promptPrefixHash: String?
    let ratioIsFromDaemon: Bool

    var contextUsageFraction: Double? {
        guard let contextEstimatedTokens, let contextWindow, contextWindow > 0 else {
            return nil
        }
        return min(1, max(0, Double(contextEstimatedTokens) / Double(contextWindow)))
    }

    var costPerSession: Double? {
        EfficiencyMetrics.average(total: totalCostUsd, count: sessionCount)
    }

    var costPerTask: Double? {
        EfficiencyMetrics.average(total: totalCostUsd, count: taskCount)
    }
}

/// Pure efficiency math and formatting shared by the dashboard card, the Stage
/// context popover, and tests.
enum EfficiencyMetrics {
    /// Below this share the dashboard surfaces the ratio in the warning color.
    static let lowCacheHitRatio = 0.5

    // MARK: - Ratios

    static func clampRatio(_ value: Double) -> Double? {
        guard value.isFinite else { return nil }
        return min(1, max(0, value))
    }

    static func cacheHitRatio(_ totals: UsageTotals) -> Double? {
        if let reported = totals.cacheHitRatio {
            return clampRatio(reported)
        }
        return cacheHitRatio(
            cacheReadTokens: totals.cacheReadTokens,
            cacheWriteTokens: totals.cacheWriteTokens,
            uncachedInputTokens: uncachedInputTokens(totals)
        )
    }

    static func cacheHitRatio(_ status: SessionCacheStatus) -> Double? {
        if let reported = status.cacheHitRatio {
            return clampRatio(reported)
        }
        return cacheHitRatio(
            cacheReadTokens: status.cacheReadTokens,
            cacheWriteTokens: status.cacheWriteTokens,
            uncachedInputTokens: status.resolvedUncachedInputTokens
        )
    }

    /// `cache_read / (uncached_input + cache_read + cache_write)`.
    ///
    /// `UsageTotals.inputTokens` includes cache reads/writes (ADR-0138), so the
    /// caller must pass the normalized uncached remainder. A zero denominator
    /// means there is no ratio to report.
    static func cacheHitRatio(
        cacheReadTokens: Int?,
        cacheWriteTokens: Int?,
        uncachedInputTokens: Int?
    ) -> Double? {
        let read = max(0, cacheReadTokens ?? 0)
        let write = max(0, cacheWriteTokens ?? 0)
        let uncached = max(0, uncachedInputTokens ?? 0)
        let denominator = uncached + read + write
        guard denominator > 0 else { return nil }
        return clampRatio(Double(read) / Double(denominator))
    }

    static func uncachedInputTokens(_ totals: UsageTotals) -> Int {
        let cacheTotal = max(0, totals.cacheReadTokens ?? 0)
            + max(0, totals.cacheWriteTokens ?? 0)
        return max(0, totals.inputTokens - cacheTotal)
    }

    static func isLowCacheHitRatio(_ ratio: Double?) -> Bool {
        guard let ratio else { return false }
        return ratio < lowCacheHitRatio
    }

    // MARK: - Snapshot assembly

    static func makeSnapshot(
        projectUsage: ProjectUsageResult?,
        usage: UsageLedgerResult?,
        context: SessionContextStatus?,
        cache: SessionCacheStatus?,
        toolOutputs: ToolOutputStatus?,
        eventTypes: [String],
        latestPrefixHash: String?
    ) -> EfficiencySnapshot? {
        guard projectUsage != nil || usage != nil || context != nil || cache != nil
            || toolOutputs != nil
        else {
            return nil
        }

        let totals = usage?.totals ?? projectUsage?.totals
        let scope: EfficiencySnapshot.Scope = usage != nil ? .session : .project

        let reportedRatio = cache?.cacheHitRatio ?? totals?.cacheHitRatio
        let ratio: Double?
        let ratioIsFromDaemon: Bool
        if let reportedRatio, let clamped = clampRatio(reportedRatio) {
            ratio = clamped
            ratioIsFromDaemon = true
        } else if let cache,
                  cache.cacheReadTokens != nil || cache.cacheWriteTokens != nil,
                  cache.resolvedUncachedInputTokens != nil
        {
            ratio = cacheHitRatio(cache)
            ratioIsFromDaemon = false
        } else if let totals {
            ratio = cacheHitRatio(totals)
            ratioIsFromDaemon = false
        } else {
            ratio = nil
            ratioIsFromDaemon = false
        }

        let contextUsage = context?.usage
        let cacheReadTokens = cache?.cacheReadTokens ?? totals?.cacheReadTokens
        let cacheWriteTokens = cache?.cacheWriteTokens ?? totals?.cacheWriteTokens
        let uncachedInputTokens: Int? = if let cache, cache.resolvedUncachedInputTokens != nil {
            cache.resolvedUncachedInputTokens
        } else if let totals {
            EfficiencyMetrics.uncachedInputTokens(totals)
        } else {
            nil
        }

        return EfficiencySnapshot(
            scope: scope,
            cacheHitRatio: ratio,
            cacheReadTokens: cacheReadTokens,
            cacheWriteTokens: cacheWriteTokens,
            uncachedInputTokens: uncachedInputTokens,
            outputTokens: totals?.outputTokens,
            totalCostUsd: totals?.costUsd,
            sessionCount: usage != nil ? 1 : projectUsage?.sessions.count,
            taskCount: totals?.byTask.count,
            contextEstimatedTokens: contextUsage?.estimatedTokens,
            contextWindow: contextUsage?.contextWindow,
            contextThresholdTokens: contextUsage?.thresholdTokens,
            contextHardLimitTokens: contextUsage?.hardLimitTokens,
            contextMessages: contextUsage?.messages,
            compactionCount: context?.compactionCount ?? compactionCount(
                eventTypes: eventTypes,
                hasLastCompaction: context?.lastCompaction != nil
            ),
            compactionLatencyMs: context?.lastCompaction?.latencyMs
                ?? context?.lastCompactionLatencyMs,
            compactionTokensBefore: context?.lastCompaction?.estimatedTokensBefore
                ?? context?.lastCompactionTokensBefore,
            compactionTokensAfter: context?.lastCompaction?.estimatedTokensAfter
                ?? context?.lastCompactionTokensAfter,
            compactionBackgroundReuse: context?.lastCompaction?.backgroundReuse
                ?? context?.compactionBackgroundReuse,
            compactionFallbackToSync: context?.lastCompaction?.fallbackToSync
                ?? context?.compactionFallbackToSync,
            toolOutputEntries: toolOutputs?.entries,
            toolOutputBytes: toolOutputs?.totalBytes,
            toolOutputActiveEntries: toolOutputs?.activeEntries,
            toolOutputEnabled: toolOutputs?.policy.enabled,
            toolOutputSpillThresholdBytes: toolOutputs?.policy.spillThresholdBytes,
            toolOutputPreviewBytes: toolOutputs?.policy.previewBytes,
            toolOutputRetentionSecs: toolOutputs?.policy.retentionSecs,
            toolOutputMaxEntries: toolOutputs?.policy.maxEntries,
            preambleTokensEstimate: cache?.preambleTokensEstimate
                ?? contextUsage?.preambleTokensEstimate,
            promptPrefixHash: cache?.promptPrefixHash ?? latestPrefixHash,
            ratioIsFromDaemon: ratioIsFromDaemon
        )
    }

    // MARK: - Formatting

    static func percent(_ ratio: Double?, digits: Int = 1) -> String {
        guard let ratio = ratio.flatMap(clampRatio) else { return "—" }
        return String(format: "%.\(max(0, digits))f%%", ratio * 100)
    }

    static func tokenLabel(_ value: Int?) -> String {
        guard let value else { return "—" }
        let count = max(0, value)
        if count >= 1_000_000_000 {
            return String(format: "%.2fB", Double(count) / 1_000_000_000)
        }
        if count >= 1_000_000 {
            return String(format: "%.1fM", Double(count) / 1_000_000)
        }
        if count >= 1000 {
            return String(format: "%.1fK", Double(count) / 1000)
        }
        return "\(count)"
    }

    static func usd(_ value: Double?, digits: Int = 4) -> String {
        guard let value, value.isFinite else { return "—" }
        return String(format: "$%.\(max(0, digits))f", value)
    }

    static func average(total: Double?, count: Int?) -> Double? {
        guard let total, total.isFinite, let count, count > 0 else { return nil }
        return total / Double(count)
    }

    static func compactionCount(
        eventTypes: [String],
        hasLastCompaction: Bool
    ) -> Int? {
        let observed = eventTypes.count(where: { $0 == "context.compacted" })
        if observed > 0 {
            return observed
        }
        return hasLastCompaction ? 1 : nil
    }

    static func shortHash(_ hash: String?, length: Int = 10) -> String? {
        guard let hash, !hash.isEmpty else { return nil }
        return String(hash.prefix(max(1, length)))
    }
}
