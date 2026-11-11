import SwiftUI

/// Read-only cache/cost/context summary for the selected project or session.
/// Every number comes from `project.usage`, `session.usage`, `session.context`,
/// or the optional `session.cache` RPC.
struct EfficiencyCard: View {
    @Environment(AppModel.self) private var model
    let project: Project

    private var snapshot: EfficiencySnapshot? {
        model.efficiencySnapshot
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            header
            if let snapshot {
                ratioRow(snapshot)
                cachePlanRow(snapshot)
                recentCacheRows(snapshot)
                tokenRow(snapshot)
                costRow(snapshot)
                governorRow(snapshot)
                contextRow(snapshot)
                if model.selectedSession != nil {
                    Divider()
                    ContextCheckpointSection(style: .dashboard)
                }
                footer(snapshot)
            } else if let status = model.efficiencyStatus {
                Text(status)
                    .font(VPTheme.mono(9))
                    .foregroundStyle(VPTheme.textSecondary)
            } else {
                ProgressView()
                    .controlSize(.small)
            }
        }
        .padding(14)
        .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .vpAppear(index: 0)
    }

    private var header: some View {
        HStack(alignment: .top) {
            VStack(alignment: .leading, spacing: 2) {
                SectionLabel(title: "Efficiency")
                Text(model.selectedSession?.goal ?? project.name)
                    .font(VPTheme.font(13, weight: .semibold, rounded: true))
                    .lineLimit(1)
                Text(
                    model.selectedSession == nil
                        ? "Project totals · cache, cost, and context"
                        : "Selected session · cache, cost, and context"
                )
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
            }
            Spacer()
            if model.efficiencyBusy {
                ProgressView().controlSize(.small)
            }
            Button("Refresh") {
                Task { await model.loadEfficiency() }
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(model.efficiencyBusy)
        }
    }

    private func ratioRow(_ snapshot: EfficiencySnapshot) -> some View {
        HStack(alignment: .bottom, spacing: 18) {
            VStack(alignment: .leading, spacing: 3) {
                Text("CACHE HIT RATIO")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                Text(EfficiencyMetrics.percent(snapshot.cacheHitRatio))
                    .font(VPTheme.font(34, weight: .semibold, rounded: true))
                    .monospacedDigit()
                    .foregroundStyle(ratioColor(snapshot.cacheHitRatio))
                Text(
                    snapshot.cacheHitRatio == nil
                        ? "no cache telemetry yet"
                        : snapshot.ratioIsFromDaemon
                        ? "reported by the daemon"
                        : "derived from cached and uncached input"
                )
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
            }
            Spacer(minLength: 0)
            VStack(alignment: .trailing, spacing: 3) {
                Text("CACHE READ")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                Text(EfficiencyMetrics.tokenLabel(snapshot.cacheReadTokens))
                    .font(VPTheme.mono(16, weight: .semibold))
                    .monospacedDigit()
                if let cacheWrite = snapshot.cacheWriteTokens, cacheWrite > 0 {
                    Text("write \(EfficiencyMetrics.tokenLabel(cacheWrite))")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
        }
    }

    @ViewBuilder
    private func cachePlanRow(_ snapshot: EfficiencySnapshot) -> some View {
        if snapshot.estimatedSavingsUSD != nil
            || snapshot.lastCacheTTL != nil
            || snapshot.lastBreakpoints != nil
            || snapshot.lastMissReason != nil
        {
            HStack(spacing: 10) {
                if let savings = snapshot.estimatedSavingsUSD {
                    metric(
                        "Estimated savings",
                        EfficiencyMetrics.usd(savings),
                        detail: savings < 0
                            ? "cache writes cost more than reads saved"
                            : "vs uncached input at configured prices"
                    )
                }
                if let ttl = snapshot.lastCacheTTL {
                    metric(
                        "Cache TTL",
                        ttl.uppercased(),
                        detail: ttl == "1h" ? "extended pool, higher write cost" : "default ephemeral pool"
                    )
                }
                if let breakpoints = snapshot.lastBreakpoints {
                    metric(
                        "Breakpoints",
                        "\(breakpoints)",
                        detail: breakpoints > 0 ? "explicit + moving slots" : "provider-managed prefix"
                    )
                }
                if let miss = snapshot.lastMissReason {
                    metric(
                        "Last miss",
                        EfficiencyMetrics.missReasonLabel(miss),
                        detail: miss == "ttl_or_provider_miss"
                            ? "prefix unchanged; no provider TTL fact inferred"
                            : "derived from component hashes"
                    )
                }
            }
        }
    }

    @ViewBuilder
    private func recentCacheRows(_ snapshot: EfficiencySnapshot) -> some View {
        if !snapshot.recentCacheCalls.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                Text("RECENT CACHE CALLS")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                ForEach(Array(snapshot.recentCacheCalls.prefix(3).enumerated()), id: \.offset) { _, call in
                    HStack(spacing: 8) {
                        Text(call.role?.uppercased() ?? "AGENT")
                            .font(VPTheme.mono(8, weight: .semibold))
                            .foregroundStyle(VPTheme.textSecondary)
                            .frame(width: 62, alignment: .leading)
                        Text("\(EfficiencyMetrics.percent(call.cacheHitRatio)) hit")
                            .font(VPTheme.mono(8))
                            .monospacedDigit()
                            .foregroundStyle(
                                EfficiencyMetrics.isLowCacheHitRatio(call.cacheHitRatio)
                                    ? VPTheme.warning
                                    : VPTheme.success
                            )
                        Text("read \(EfficiencyMetrics.tokenLabel(call.cacheReadTokens))")
                            .font(VPTheme.mono(8))
                            .foregroundStyle(VPTheme.textTertiary)
                        if call.warm {
                            Text("warm")
                                .font(VPTheme.mono(8, weight: .semibold))
                                .foregroundStyle(VPTheme.accent)
                        } else if let miss = call.missReason {
                            Text("miss \(EfficiencyMetrics.missReasonLabel(miss))")
                                .font(VPTheme.mono(8))
                                .foregroundStyle(VPTheme.textTertiary)
                        }
                        Spacer(minLength: 0)
                        if let firstToken = call.firstTokenMS, firstToken > 0 {
                            Text("\(firstToken) ms")
                                .font(VPTheme.mono(8))
                                .monospacedDigit()
                                .foregroundStyle(VPTheme.textTertiary)
                        }
                    }
                }
                if snapshot.recentCacheCalls.count > 3 {
                    Text("+\(snapshot.recentCacheCalls.count - 3) older calls")
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
            .padding(8)
            .background(VPTheme.surfaceRaised.opacity(0.45), in: RoundedRectangle(cornerRadius: 9))
        }
    }

    private func tokenRow(_ snapshot: EfficiencySnapshot) -> some View {
        HStack(spacing: 10) {
            metric(
                "Uncached input",
                EfficiencyMetrics.tokenLabel(snapshot.uncachedInputTokens),
                detail: "billed at full input price"
            )
            metric(
                "Cached input",
                EfficiencyMetrics.tokenLabel(snapshot.cacheReadTokens),
                detail: "billed at the cache-read price"
            )
            metric(
                "Output",
                EfficiencyMetrics.tokenLabel(snapshot.outputTokens),
                detail: "generated tokens"
            )
        }
    }

    private func costRow(_ snapshot: EfficiencySnapshot) -> some View {
        HStack(spacing: 10) {
            metric(
                "Total cost",
                EfficiencyMetrics.usd(snapshot.totalCostUsd),
                detail: snapshot.scope == .session ? "selected session" : "project"
            )
            metric(
                "Cost / session",
                EfficiencyMetrics.usd(snapshot.costPerSession),
                detail: snapshot.sessionCount.map { "\($0) session\($0 == 1 ? "" : "s")" } ?? "—"
            )
            if let taskCount = snapshot.taskCount, taskCount > 0 {
                metric(
                    "Cost / task",
                    EfficiencyMetrics.usd(snapshot.costPerTask),
                    detail: "\(taskCount) ledger task bucket\(taskCount == 1 ? "" : "s")"
                )
            }
        }
    }

    @ViewBuilder
    private func contextRow(_ snapshot: EfficiencySnapshot) -> some View {
        if let estimated = snapshot.contextEstimatedTokens,
           let window = snapshot.contextWindow,
           let threshold = snapshot.contextThresholdTokens
        {
            VStack(alignment: .leading, spacing: 5) {
                HStack(spacing: 10) {
                    metric(
                        "Context",
                        "\(EfficiencyMetrics.tokenLabel(estimated)) / \(EfficiencyMetrics.tokenLabel(window))",
                        detail: "summary at \(EfficiencyMetrics.tokenLabel(threshold))"
                    )
                    if let compactionCount = snapshot.compactionCount {
                        metric(
                            "Compactions",
                            "\(compactionCount)",
                            detail: "observed in this session"
                        )
                    }
                    if let checkpointCount = snapshot.checkpointCount {
                        metric(
                            "Checkpoints",
                            "\(checkpointCount)",
                            detail: snapshot.latestCheckpointID.map { "latest \($0.prefix(8))" }
                                ?? "durable context boundaries"
                        )
                    }
                    if let preamble = snapshot.preambleTokensEstimate {
                        metric(
                            "Preamble",
                            EfficiencyMetrics.tokenLabel(preamble),
                            detail: "estimated stable prefix"
                        )
                    }
                    Spacer(minLength: 0)
                }
                if let fraction = snapshot.contextUsageFraction {
                    VPGradientProgress(
                        value: fraction,
                        tint: estimated >= threshold ? VPTheme.warning : VPTheme.accent,
                        height: 4
                    )
                }
                if let messages = snapshot.contextMessages {
                    Text("\(messages) active messages. Context estimates refresh before each provider call.")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                HStack(spacing: 10) {
                    metric(
                        "Tool spills",
                        "\(snapshot.toolOutputEntries ?? 0)",
                        detail: snapshot.toolOutputBytes.map {
                            "\(EfficiencyMetrics.tokenLabel($0)) stored · \(snapshot.toolOutputActiveEntries ?? 0) active"
                        } ?? "no store telemetry"
                    )
                    if let latency = snapshot.compactionLatencyMs {
                        metric(
                            "Last compaction",
                            "\(latency) ms",
                            detail: {
                                let before = EfficiencyMetrics.tokenLabel(snapshot.compactionTokensBefore)
                                let after = EfficiencyMetrics.tokenLabel(snapshot.compactionTokensAfter)
                                return "\(before) → \(after) tokens"
                            }()
                        )
                    }
                    if snapshot.compactionBackgroundReuse == true {
                        metric("Prepared reuse", "yes", detail: "summary prepared below threshold")
                    } else if snapshot.compactionFallbackToSync == true {
                        metric("Fallback", "sync", detail: "prepared summary was stale")
                    }
                    Spacer(minLength: 0)
                }
                if let enabled = snapshot.toolOutputEnabled,
                   let spill = snapshot.toolOutputSpillThresholdBytes,
                   let preview = snapshot.toolOutputPreviewBytes,
                   let retention = snapshot.toolOutputRetentionSecs,
                   let maxEntries = snapshot.toolOutputMaxEntries
                {
                    Text(
                        "Tool output policy: \(enabled ? "spill on" : "spill off") · >\(EfficiencyMetrics.tokenLabel(spill)) · preview \(EfficiencyMetrics.tokenLabel(preview)) · retention \(retention / 86400)d · cap \(maxEntries)"
                    )
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
                }
            }
        } else {
            Text("Context usage has not been recorded for this session yet.")
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
        }
    }

    /// Critical-path reserve, admitted/deferred work, and the last persisted
    /// governor decision. Everything is read-only daemon state.
    @ViewBuilder
    private func governorRow(_ snapshot: EfficiencySnapshot) -> some View {
        if let governor = snapshot.governor {
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 8) {
                    Text("CRITICAL PATH BUDGET")
                        .font(VPTheme.mono(8, weight: .semibold))
                        .foregroundStyle(VPTheme.textTertiary)
                    Text(governor.enabled ? "GOVERNOR ON" : "GOVERNOR OFF")
                        .font(VPTheme.mono(8, weight: .semibold))
                        .foregroundStyle(governor.enabled ? VPTheme.accent : VPTheme.textTertiary)
                    if governor.unlimited {
                        Text("UNLIMITED")
                            .font(VPTheme.mono(8, weight: .semibold))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                    if let error = governor.error {
                        Text(error.code)
                            .font(VPTheme.mono(8, weight: .semibold))
                            .foregroundStyle(VPTheme.warning)
                    }
                    Spacer(minLength: 0)
                    if let path = governor.criticalPath {
                        Text(
                            "path \(EfficiencyMetrics.tokenLabel(path.weightTokens)) · r\(path.planRevision)"
                        )
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                    }
                }
                HStack(spacing: 10) {
                    metric(
                        "Path",
                        GovernorMetrics.pathLabel(governor.criticalPath?.taskIDs ?? []),
                        detail: governor.criticalPath.map {
                            "\($0.taskCount) tasks · \($0.edgeCount) edges"
                        } ?? "no plan"
                    )
                    metric(
                        "Protected",
                        EfficiencyMetrics.tokenLabel(governor.reserve.protectedTokens),
                        detail: GovernorMetrics.percent(governor.reserve.reserveRatio) + " reserve"
                    )
                    metric(
                        "Remaining",
                        EfficiencyMetrics.tokenLabel(governor.reserve.remainingTokens),
                        detail: governor.reserve.state
                    )
                    metric(
                        "Deferred",
                        "\(governor.deferred.count)",
                        detail: governor.reserve.throttled ? "throttle zone" : "reserve pool"
                    )
                }
                if !governor.deferred.isEmpty {
                    ForEach(Array(governor.deferred.prefix(3).enumerated()), id: \.offset) { _, deferral in
                        HStack(spacing: 8) {
                            Text(deferral.taskID)
                                .font(VPTheme.mono(8, weight: .semibold))
                                .foregroundStyle(VPTheme.textSecondary)
                                .frame(width: 90, alignment: .leading)
                            Text(deferral.reason)
                                .font(VPTheme.mono(8))
                                .foregroundStyle(VPTheme.warning)
                            Text(deferral.code)
                                .font(VPTheme.mono(7))
                                .foregroundStyle(VPTheme.textTertiary)
                            Spacer(minLength: 0)
                            Text("weight \(EfficiencyMetrics.tokenLabel(deferral.weightTokens))")
                                .font(VPTheme.mono(7))
                                .foregroundStyle(VPTheme.textTertiary)
                        }
                    }
                    if governor.deferred.count > 3 {
                        Text("+\(governor.deferred.count - 3) more deferred tasks")
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                }
                if let decision = governor.lastDecisions.last {
                    Text("Last decision \(decision.action) \(decision.reason) · \(decision.detail)")
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                        .lineLimit(2)
                }
                if let error = governor.error {
                    Text("\(error.code) · \(error.message)")
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.warning)
                        .lineLimit(2)
                }
            }
            .padding(8)
            .background(VPTheme.surfaceRaised.opacity(0.45), in: RoundedRectangle(cornerRadius: 9))
        } else if let message = model.governorStatusMessage {
            Text(message)
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
        }
    }

    private func footer(_ snapshot: EfficiencySnapshot) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text("Cache read is billed, not free; this is a hit ratio, not a discount.")
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textSecondary)
            if let prefix = EfficiencyMetrics.shortHash(snapshot.promptPrefixHash) {
                Text("Prompt prefix \(prefix)…")
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
                    .textSelection(.enabled)
            }
            if let status = model.efficiencyStatus {
                Text(status)
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
        }
    }

    private func metric(_ title: String, _ value: String, detail: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title.uppercased())
                .font(VPTheme.mono(8, weight: .semibold))
                .foregroundStyle(VPTheme.textTertiary)
            Text(value)
                .font(VPTheme.mono(12, weight: .semibold))
                .monospacedDigit()
                .foregroundStyle(VPTheme.textPrimary)
            Text(detail)
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textTertiary)
                .lineLimit(1)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(8)
        .background(VPTheme.surfaceRaised.opacity(0.55), in: RoundedRectangle(cornerRadius: 9))
    }

    private func ratioColor(_ ratio: Double?) -> Color {
        guard let ratio else { return VPTheme.textTertiary }
        return EfficiencyMetrics.isLowCacheHitRatio(ratio) ? VPTheme.warning : VPTheme.success
    }
}

/// Compact cache-hit badge for the Stage context popover.
struct CacheHitPill: View {
    let ratio: Double?

    private var tint: Color {
        guard let ratio else { return VPTheme.textTertiary }
        return EfficiencyMetrics.isLowCacheHitRatio(ratio) ? VPTheme.warning : VPTheme.success
    }

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: "arrow.triangle.2.circlepath")
                .font(.system(size: 8, weight: .semibold))
            Text("cache \(EfficiencyMetrics.percent(ratio))")
                .font(VPTheme.mono(8, weight: .semibold))
        }
        .padding(.horizontal, 6)
        .padding(.vertical, 3)
        .foregroundStyle(tint)
        .background(tint.opacity(0.13), in: Capsule())
        .overlay {
            Capsule().strokeBorder(tint.opacity(0.24), lineWidth: 1)
        }
        .help("Share of provider input served from cache. Cache reads are still billed.")
    }
}

/// Compact governor badge for the Stage budget popover.
struct GovernorPill: View {
    let status: GovernorSessionStatus?

    var body: some View {
        if let status {
            let active = status.enabled && status.criticalPath != nil
            let pressure = status.reserve.throttled
            HStack(spacing: 4) {
                Image(systemName: active ? "point.topleft.down.curvedto.point.bottomright.up" :
                    "pause.circle")
                    .font(.system(size: 8, weight: .semibold))
                Text(
                    active
                        ? "governor \(status.deferred.count) deferred"
                        : "governor off"
                )
                .font(VPTheme.mono(8, weight: .semibold))
            }
            .padding(.horizontal, 6)
            .padding(.vertical, 3)
            .foregroundStyle(pressure ? VPTheme.warning : (active ? VPTheme.accent : VPTheme.textTertiary))
            .background(
                (pressure ? VPTheme.warning : (active ? VPTheme.accent : VPTheme.textTertiary))
                    .opacity(0.13),
                in: Capsule()
            )
            .overlay {
                Capsule().strokeBorder(
                    (pressure ? VPTheme.warning : (active ? VPTheme.accent : VPTheme.textTertiary))
                        .opacity(0.24),
                    lineWidth: 1
                )
            }
            .help("Critical-path budget reserve. Deferred tasks stay queued; hard budgets are unchanged.")
        }
    }
}
