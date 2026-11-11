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
                tokenRow(snapshot)
                costRow(snapshot)
                contextRow(snapshot)
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
