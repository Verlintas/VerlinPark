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
                cacheContinuityRow(snapshot)
                cachePlanRow(snapshot)
                cacheBreakdownRows(snapshot)
                recentCacheRows(snapshot)
                if let effects = model.sessionEffects, effects.counts.total > 0 {
                    EffectReceiptsSection(report: effects)
                }
                CacheCallTable(
                    calls: model.cacheCallRows,
                    truncated: model.sessionCacheCallsTruncated
                )
                CacheEpochTimelineView(epochs: snapshot.cacheEpochTimeline)
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

    /// Miss tokens, theoretical maximum, append-only continuity, epoch, and
    /// latency percentiles. Every value comes from `session.cache`; absent
    /// fields are shown as unavailable rather than replaced with a guess.
    @ViewBuilder
    private func cacheContinuityRow(_ snapshot: EfficiencySnapshot) -> some View {
        if snapshot.resolvedCacheMissTokens != nil
            || snapshot.theoreticalMaxHitRatio != nil
            || snapshot.cacheEpoch != nil
            || snapshot.appendOnlyCalls != nil
        {
            HStack(spacing: 10) {
                metric(
                    "Cache miss",
                    EfficiencyMetrics.tokenLabel(snapshot.resolvedCacheMissTokens),
                    detail: "uncached prompt tokens"
                )
                metric(
                    "Theoretical max",
                    EfficiencyMetrics.percent(snapshot.theoreticalMaxHitRatio),
                    detail: "previous prompt / current input"
                )
                metric(
                    "Append-only",
                    appendOnlyLabel(snapshot),
                    detail: appendOnlyDetail(snapshot)
                )
                metric(
                    "Cache epoch",
                    EfficiencyMetrics.shortHash(snapshot.cacheEpoch) ?? "—",
                    detail: snapshot.cacheEpochSource ?? "no epoch telemetry"
                )
            }
            if let p50 = snapshot.hitRatioP50, let p95 = snapshot.hitRatioP95 {
                HStack(spacing: 10) {
                    metric(
                        "Per-call p50 / p95",
                        "\(EfficiencyMetrics.percent(p50)) / \(EfficiencyMetrics.percent(p95))",
                        detail: "nearest-rank over observed provider calls"
                    )
                    metric(
                        "Theoretical p50",
                        EfficiencyMetrics.percent(snapshot.theoreticalMaxP50),
                        detail: "append-only headroom, not a provider promise"
                    )
                    if let gap = snapshot.cacheHeadroomGap {
                        metric(
                            "Headroom gap",
                            String(format: "%.1f pts", gap * 100),
                            detail: gap > 0.05 ? "inspect tail/provider eviction" : "within the expected tail"
                        )
                    }
                    Spacer(minLength: 0)
                }
            }
        }
    }

    /// Top miss reasons, per-role accounting, cross-session reuse, and the
    /// compaction-invalidated cache savings.
    @ViewBuilder
    private func cacheBreakdownRows(_ snapshot: EfficiencySnapshot) -> some View {
        if !snapshot.topMissReasons.isEmpty
            || !snapshot.perRole.isEmpty
            || snapshot.crossSessionReuseSessions != nil
            || snapshot.compactionLostCacheUSD != nil
        {
            VStack(alignment: .leading, spacing: 6) {
                if !snapshot.topMissReasons.isEmpty {
                    HStack(spacing: 8) {
                        Text("TOP MISS REASONS")
                            .font(VPTheme.mono(8, weight: .semibold))
                            .foregroundStyle(VPTheme.textTertiary)
                        ForEach(snapshot.topMissReasons.prefix(5)) { reason in
                            Text("\(EfficiencyMetrics.missReasonLabel(reason.key)) ×\(reason.calls)")
                                .font(VPTheme.mono(8))
                                .foregroundStyle(reason.key == "provider_cache_miss"
                                    ? VPTheme.warning : VPTheme.textSecondary)
                        }
                        Spacer(minLength: 0)
                    }
                }
                if !snapshot.perRole.isEmpty {
                    ForEach(Array(snapshot.perRole.prefix(4))) { role in
                        HStack(spacing: 8) {
                            Text(role.role.uppercased())
                                .font(VPTheme.mono(8, weight: .semibold))
                                .foregroundStyle(VPTheme.textSecondary)
                                .frame(width: 78, alignment: .leading)
                            Text("\(EfficiencyMetrics.percent(role.hitRatio)) hit")
                                .font(VPTheme.mono(8))
                                .monospacedDigit()
                            Text("miss \(EfficiencyMetrics.tokenLabel(role.cacheMissTokens))")
                                .font(VPTheme.mono(8))
                                .foregroundStyle(VPTheme.textTertiary)
                            Text("\(role.calls) calls")
                                .font(VPTheme.mono(8))
                                .foregroundStyle(VPTheme.textTertiary)
                            Spacer(minLength: 0)
                            Text(EfficiencyMetrics.usd(role.costUSD))
                                .font(VPTheme.mono(8))
                                .monospacedDigit()
                                .foregroundStyle(VPTheme.textTertiary)
                        }
                    }
                }
                HStack(spacing: 10) {
                    metric(
                        "Cross-session",
                        "\(snapshot.crossSessionReuseSessions ?? 0)",
                        detail: "\(EfficiencyMetrics.tokenLabel(snapshot.crossSessionHitTokens)) hit tokens observed"
                    )
                    metric(
                        "Lost to compaction",
                        EfficiencyMetrics.usd(snapshot.compactionLostCacheUSD),
                        detail: "estimated invalidated cache-read savings"
                    )
                    if !snapshot.cacheEpochTimeline.isEmpty {
                        metric(
                            "Epoch timeline",
                            "\(snapshot.cacheEpochTimeline.count)",
                            detail: "bounded, newest last"
                        )
                    }
                    Spacer(minLength: 0)
                }
            }
            .padding(8)
            .background(VPTheme.surfaceRaised.opacity(0.45), in: RoundedRectangle(cornerRadius: 9))
        }
    }

    private func appendOnlyLabel(_ snapshot: EfficiencySnapshot) -> String {
        guard let last = snapshot.lastAppendOnly else {
            guard let known = snapshot.appendOnlyKnownCalls, known > 0 else { return "—" }
            return "\(snapshot.appendOnlyCalls ?? 0)/\(known)"
        }
        return last ? "yes" : "no"
    }

    private func appendOnlyDetail(_ snapshot: EfficiencySnapshot) -> String {
        let calls = snapshot.appendOnlyCalls ?? 0
        let breaks = snapshot.appendOnlyBreaks ?? 0
        if calls == 0, breaks == 0 {
            return "no continuity telemetry"
        }
        return "\(calls) append-only · \(breaks) break\(breaks == 1 ? "" : "s")"
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
                        Text(
                            "read \(EfficiencyMetrics.tokenLabel(call.cacheReadTokens)) · miss \(EfficiencyMetrics.tokenLabel(call.cacheMissTokens))"
                        )
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                        Text("max \(EfficiencyMetrics.percent(call.theoreticalMaxHitRatio))")
                            .font(VPTheme.mono(8))
                            .foregroundStyle(VPTheme.textTertiary)
                        if let appendOnly = call.appendOnly {
                            Text(appendOnly ? "append" : "break")
                                .font(VPTheme.mono(8, weight: .semibold))
                                .foregroundStyle(appendOnly ? VPTheme.success : VPTheme.warning)
                        }
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
                    Text(GovernorMetrics.modeLabel(governor.reserve.mode).uppercased())
                        .font(VPTheme.mono(8, weight: .semibold))
                        .foregroundStyle(
                            governor.reserve.mode == "multi"
                                ? VPTheme.accent : VPTheme.textTertiary
                        )
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
                if governor.reserve.mode == "multi" || !governor.paths.isEmpty {
                    HStack(spacing: 10) {
                        metric(
                            "Paths",
                            "\(governor.reserve.pathCount)",
                            detail: "max \(governor.policy.maxPaths)"
                        )
                        metric(
                            "Allocated",
                            EfficiencyMetrics.tokenLabel(governor.reserve.allocatedTokens),
                            detail: GovernorMetrics.percent(
                                governor.reserve.protectedTokens > 0
                                    ? Double(governor.reserve.allocatedTokens)
                                    / Double(governor.reserve.protectedTokens)
                                    : nil
                            ) + " of protected"
                        )
                        metric(
                            "Floors",
                            EfficiencyMetrics.tokenLabel(governor.reserve.floorTokens),
                            detail: "department min \(GovernorMetrics.percent(governor.policy.departmentMinRatio))"
                        )
                        metric(
                            "Fairness",
                            GovernorMetrics.starvationLabel(governor.policy.starvationAfterSecs),
                            detail: "observed \(GovernorMetrics.percent(governor.policy.observedWeightRatio)) blend"
                        )
                    }
                    if !governor.paths.isEmpty {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("PROTECTED PATHS")
                                .font(VPTheme.mono(8, weight: .semibold))
                                .foregroundStyle(VPTheme.textTertiary)
                            ForEach(Array(governor.paths.prefix(4))) { path in
                                HStack(spacing: 8) {
                                    Text("#\(path.pathID)")
                                        .font(VPTheme.mono(8, weight: .semibold))
                                        .foregroundStyle(VPTheme.textSecondary)
                                        .frame(width: 22, alignment: .leading)
                                    Text(GovernorMetrics.pathRow(path))
                                        .font(VPTheme.mono(8))
                                        .foregroundStyle(
                                            path.deficitTokens > 0
                                                ? VPTheme.warning : VPTheme.textSecondary
                                        )
                                        .lineLimit(1)
                                    Spacer(minLength: 0)
                                    Text(GovernorMetrics.departmentLabel(path))
                                        .font(VPTheme.mono(7))
                                        .foregroundStyle(VPTheme.textTertiary)
                                }
                                if let fraction = GovernorMetrics.allocationFraction(path) {
                                    VPGradientProgress(
                                        value: fraction,
                                        tint: path.deficitTokens > 0 ? VPTheme.warning : VPTheme.accent,
                                        height: 2
                                    )
                                }
                            }
                            if governor.paths.count > 4 {
                                Text("+\(governor.paths.count - 4) more protected paths")
                                    .font(VPTheme.mono(7))
                                    .foregroundStyle(VPTheme.textTertiary)
                            }
                        }
                    }
                    if !governor.departments.isEmpty {
                        HStack(spacing: 10) {
                            ForEach(Array(governor.departments.prefix(3))) { department in
                                metric(
                                    department.department,
                                    EfficiencyMetrics.tokenLabel(department.floorTokens),
                                    detail: "floor · \(department.pathIDs.count) path\(department.pathIDs.count == 1 ? "" : "s")"
                                )
                            }
                            Spacer(minLength: 0)
                        }
                        if governor.departments.count > 3 {
                            Text("+\(governor.departments.count - 3) more departments")
                                .font(VPTheme.mono(7))
                                .foregroundStyle(VPTheme.textTertiary)
                        }
                    }
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
                    let order = GovernorMetrics.deferredOrder(governor)
                    if !order.isEmpty {
                        Text("Deferred order: \(order.prefix(6).joined(separator: ", "))")
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.textTertiary)
                            .lineLimit(1)
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

/// Bounded newest-first per-call cache table. It uses the dedicated
/// `session.cache.calls` RPC when available and falls back to the `session.cache`
/// recent-call list on older daemons. No prompt text is present.
struct CacheCallTable: View {
    let calls: [SessionCacheCall]
    let truncated: Bool

    var body: some View {
        if !calls.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 8) {
                    Text("PER-CALL CACHE TABLE")
                        .font(VPTheme.mono(8, weight: .semibold))
                        .foregroundStyle(VPTheme.textTertiary)
                    Text("\(calls.count) newest")
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                    if truncated {
                        Text("older rows truncated")
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.warning)
                    }
                    Spacer(minLength: 0)
                }
                Grid(alignment: .leading, horizontalSpacing: 10, verticalSpacing: 3) {
                    GridRow {
                        header("ROLE")
                        header("INPUT")
                        header("READ")
                        header("MISS")
                        header("HIT")
                        header("MAX")
                        header("APPEND")
                        header("REASON")
                        header("TTL")
                        header("FTL")
                    }
                    ForEach(calls.prefix(10)) { call in
                        GridRow {
                            cell(call.role?.uppercased() ?? "AGENT", weight: .semibold)
                            cell(EfficiencyMetrics.tokenLabel(call.inputTokens))
                            cell(EfficiencyMetrics.tokenLabel(call.cacheReadTokens))
                            cell(EfficiencyMetrics.tokenLabel(call.cacheMissTokens))
                            cell(
                                EfficiencyMetrics.percent(call.cacheHitRatio),
                                tint: EfficiencyMetrics.isLowCacheHitRatio(call.cacheHitRatio)
                                    ? VPTheme.warning : VPTheme.textPrimary
                            )
                            cell(EfficiencyMetrics.percent(call.theoreticalMaxHitRatio))
                            cell(
                                call.appendOnly == nil
                                    ? "n/a" : (call.appendOnly == true ? "yes" : "break"),
                                tint: call.appendOnly == false ? VPTheme.warning : VPTheme.textSecondary
                            )
                            cell(EfficiencyMetrics.missReasonLabel(call.missReason ?? "—"))
                            cell(call.cacheTTL ?? "—")
                            cell(call.firstTokenMS.map { "\($0) ms" } ?? "—")
                        }
                    }
                }
                if calls.count > 10 {
                    Text("+\(calls.count - 10) more rows")
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
            .padding(8)
            .background(VPTheme.surfaceRaised.opacity(0.35), in: RoundedRectangle(cornerRadius: 9))
        }
    }

    private func header(_ title: String) -> some View {
        Text(title)
            .font(VPTheme.mono(7, weight: .semibold))
            .foregroundStyle(VPTheme.textTertiary)
    }

    private func cell(_ value: String, tint: Color = VPTheme.textSecondary,
                      weight: Font.Weight = .regular) -> some View
    {
        Text(value)
            .font(VPTheme.mono(7, weight: weight))
            .monospacedDigit()
            .foregroundStyle(tint)
            .lineLimit(1)
    }
}

/// Bounded cache-epoch timeline. The daemon emits newest-last, so the oldest
/// entry is first and a new epoch is visually an appended row. Repeated
/// epochs are possible when a provider evicts a prefix; the row is evidence,
/// not a diagnosis.
struct CacheEpochTimelineView: View {
    let epochs: [SessionCacheEpochBucket]

    var body: some View {
        if !epochs.isEmpty {
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 8) {
                    Text("CACHE EPOCH TIMELINE")
                        .font(VPTheme.mono(8, weight: .semibold))
                        .foregroundStyle(VPTheme.textTertiary)
                    Text("\(epochs.count) observed · newest last")
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                    Spacer(minLength: 0)
                }
                ForEach(epochs) { epoch in
                    HStack(spacing: 8) {
                        Text(EfficiencyMetrics.shortHash(epoch.epoch, length: 8) ?? "—")
                            .font(VPTheme.mono(8, weight: .semibold))
                            .foregroundStyle(VPTheme.textSecondary)
                            .frame(width: 70, alignment: .leading)
                        Text(epoch.source ?? "unknown source")
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.textTertiary)
                            .frame(width: 110, alignment: .leading)
                        Text("\(epoch.calls) calls")
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.textTertiary)
                        Text(EfficiencyMetrics.percent(epoch.hitRatio))
                            .font(VPTheme.mono(7, weight: .semibold))
                            .monospacedDigit()
                            .foregroundStyle(EfficiencyMetrics.isLowCacheHitRatio(epoch.hitRatio)
                                ? VPTheme.warning : VPTheme.textSecondary)
                        Text("miss \(EfficiencyMetrics.tokenLabel(epoch.cacheMissTokens))")
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.textTertiary)
                        if epoch.appendOnlyBreaks > 0 {
                            Text("\(epoch.appendOnlyBreaks) break\(epoch.appendOnlyBreaks == 1 ? "" : "s")")
                                .font(VPTheme.mono(7, weight: .semibold))
                                .foregroundStyle(VPTheme.warning)
                        }
                        Spacer(minLength: 0)
                        Text(epoch.lastAt)
                            .font(VPTheme.mono(6))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                }
            }
            .padding(8)
            .background(VPTheme.surfaceRaised.opacity(0.35), in: RoundedRectangle(cornerRadius: 9))
        }
    }
}

/// Full cache panel for the Stage context popover. It is intentionally
/// read-only and derives every displayed value from the daemon snapshot.
struct CacheTelemetryPanel: View {
    let snapshot: EfficiencySnapshot

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack(spacing: 8) {
                CacheHitPill(ratio: snapshot.cacheHitRatio)
                if let theoretical = snapshot.theoreticalMaxHitRatio {
                    tag(
                        "max \(EfficiencyMetrics.percent(theoretical))",
                        tint: VPTheme.textSecondary,
                        help: "Theoretical maximum from the previous prompt size. Not a provider guarantee."
                    )
                }
                if let appendOnly = snapshot.lastAppendOnly {
                    tag(
                        appendOnly ? "append-only" : "prefix break",
                        tint: appendOnly ? VPTheme.success : VPTheme.warning,
                        help: appendOnly
                            ? "The previous canonical message list is an exact prefix of this call."
                            : "The previous canonical message list was not an exact prefix. Inspect the audit."
                    )
                }
                if let epoch = EfficiencyMetrics.shortHash(snapshot.cacheEpoch) {
                    tag(
                        "epoch \(epoch)",
                        tint: VPTheme.textTertiary,
                        help: snapshot.cacheEpochSource ?? "cache epoch"
                    )
                }
                Spacer(minLength: 0)
                if let ttl = snapshot.lastCacheTTL {
                    tag("ttl \(ttl)", tint: VPTheme.textTertiary, help: "Configured provider cache TTL.")
                }
                if let breakpoints = snapshot.lastBreakpoints {
                    tag(
                        "\(breakpoints) bp",
                        tint: VPTheme.textTertiary,
                        help: "Explicit provider breakpoints."
                    )
                }
                if let savings = snapshot.estimatedSavingsUSD {
                    tag(
                        "saved \(EfficiencyMetrics.usd(savings))",
                        tint: savings < 0 ? VPTheme.warning : VPTheme.textSecondary,
                        help: "Estimated from configured prices; cache reads are still billed."
                    )
                }
            }
            HStack(spacing: 14) {
                stat("read", EfficiencyMetrics.tokenLabel(snapshot.cacheReadTokens))
                stat("miss", EfficiencyMetrics.tokenLabel(snapshot.resolvedCacheMissTokens))
                stat("p50", EfficiencyMetrics.percent(snapshot.hitRatioP50))
                stat("p95", EfficiencyMetrics.percent(snapshot.hitRatioP95))
                stat("cost", EfficiencyMetrics.usd(snapshot.costUSD, digits: 3))
                stat(
                    "reuse",
                    "\(snapshot.crossSessionReuseSessions ?? 0) sess",
                    detail: "\(EfficiencyMetrics.tokenLabel(snapshot.crossSessionHitTokens)) hit"
                )
                stat(
                    "lost",
                    EfficiencyMetrics.usd(snapshot.compactionLostCacheUSD),
                    detail: "compaction"
                )
                Spacer(minLength: 0)
            }
            if !snapshot.topMissReasons.isEmpty {
                HStack(spacing: 6) {
                    Text("miss reasons")
                        .font(VPTheme.mono(7, weight: .semibold))
                        .foregroundStyle(VPTheme.textTertiary)
                    ForEach(snapshot.topMissReasons.prefix(5)) { reason in
                        Text("\(EfficiencyMetrics.missReasonLabel(reason.key)) ×\(reason.calls)")
                            .font(VPTheme.mono(7))
                            .foregroundStyle(reason.key == "provider_cache_miss"
                                ? VPTheme.warning : VPTheme.textTertiary)
                    }
                    Spacer(minLength: 0)
                }
            }
            if !snapshot.perRole.isEmpty {
                HStack(spacing: 10) {
                    ForEach(snapshot.perRole.prefix(4)) { role in
                        Text(
                            "\(role.role) \(EfficiencyMetrics.percent(role.hitRatio)) · miss \(EfficiencyMetrics.tokenLabel(role.cacheMissTokens)) · \(EfficiencyMetrics.usd(role.costUSD, digits: 3))"
                        )
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                    }
                    Spacer(minLength: 0)
                }
            }
            if !snapshot.recentCacheCalls.isEmpty {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(Array(snapshot.recentCacheCalls.prefix(3))) { call in
                        HStack(spacing: 6) {
                            Text(call.role?.uppercased() ?? "AGENT")
                                .font(VPTheme.mono(7, weight: .semibold))
                                .foregroundStyle(VPTheme.textSecondary)
                                .frame(width: 58, alignment: .leading)
                            Text(
                                "\(EfficiencyMetrics.percent(call.cacheHitRatio)) hit · read \(EfficiencyMetrics.tokenLabel(call.cacheReadTokens)) · miss \(EfficiencyMetrics.tokenLabel(call.cacheMissTokens))"
                            )
                            .font(VPTheme.mono(7))
                            .monospacedDigit()
                            .foregroundStyle(VPTheme.textTertiary)
                            if let appendOnly = call.appendOnly {
                                Text(appendOnly ? "append" : "break")
                                    .font(VPTheme.mono(7, weight: .semibold))
                                    .foregroundStyle(appendOnly ? VPTheme.success : VPTheme.warning)
                            }
                            if let miss = call.missReason {
                                Text(EfficiencyMetrics.missReasonLabel(miss))
                                    .font(VPTheme.mono(7))
                                    .foregroundStyle(VPTheme.textTertiary)
                            }
                            Spacer(minLength: 0)
                            if let epoch = EfficiencyMetrics.shortHash(call.cacheEpoch) {
                                Text(epoch)
                                    .font(VPTheme.mono(7))
                                    .foregroundStyle(VPTheme.textTertiary)
                            }
                        }
                    }
                }
            }
            if !snapshot.cacheEpochTimeline.isEmpty {
                CacheEpochTimelineView(epochs: snapshot.cacheEpochTimeline)
            }
            Text(
                "Cache reads are billed, not free; this is a hit ratio, not a discount. Cross-session reuse is observed, never guaranteed."
            )
            .font(VPTheme.mono(7))
            .foregroundStyle(VPTheme.textTertiary)
        }
        .padding(8)
        .background(VPTheme.surfaceRaised.opacity(0.35), in: RoundedRectangle(cornerRadius: 8))
    }

    private func tag(_ text: String, tint: Color, help: String) -> some View {
        Text(text)
            .font(VPTheme.mono(8, weight: .semibold))
            .padding(.horizontal, 5)
            .padding(.vertical, 2)
            .foregroundStyle(tint)
            .background(tint.opacity(0.10), in: Capsule())
            .help(help)
    }

    private func stat(_ title: String, _ value: String, detail: String? = nil) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(title.uppercased())
                .font(VPTheme.mono(7, weight: .semibold))
                .foregroundStyle(VPTheme.textTertiary)
            Text(value)
                .font(VPTheme.mono(9, weight: .semibold))
                .monospacedDigit()
                .foregroundStyle(VPTheme.textSecondary)
            if let detail {
                Text(detail)
                    .font(VPTheme.mono(6))
                    .foregroundStyle(VPTheme.textTertiary)
            }
        }
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
