import AppKit
import SwiftUI

/// Visible failure state for `session.resources`. A missing snapshot must not
/// silently hide the whole resource strip; the operator sees the daemon/RPC
/// error and can retry explicitly. This view never invents dimensions.
struct StageResourceUnavailableCard: View {
    @Environment(AppModel.self) private var model
    let message: String

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(VPTheme.warning)
            VStack(alignment: .leading, spacing: 2) {
                Text("RESOURCES UNAVAILABLE")
                    .font(VPTheme.mono(6.5, weight: .semibold))
                    .tracking(0.6)
                    .foregroundStyle(VPTheme.warning)
                Text(String(message.prefix(280)))
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textSecondary)
                    .lineLimit(2)
            }
            Spacer(minLength: 6)
            Button {
                Task { await model.loadSessionResources() }
            } label: {
                Label("Retry", systemImage: "arrow.clockwise")
            }
            .buttonStyle(.plain)
            .font(VPTheme.mono(6.5, weight: .medium))
            .foregroundStyle(VPTheme.accent)
            .disabled(model.sessionResourcesBusy)
        }
        .padding(8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            VPTheme.warning.opacity(0.06),
            in: RoundedRectangle(cornerRadius: 8, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .strokeBorder(VPTheme.warning.opacity(0.18), lineWidth: 1)
        }
    }
}

/// Compact Stage view of `session.resources`. It shows the bottleneck,
/// readiness, and the primary resource dimensions; the full dimension list is
/// available on demand. Every displayed value is daemon-provided. The only
/// mutation is the explicit operator top-up sheet.
struct StageResourceCard: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let resources: ResourceSnapshotView

    @State private var expanded = false
    @State private var showingTopUp = false
    @State private var showingPartialDelivery = false
    @State private var showingRescueConfirmation = false

    private var readiness: ReadinessSnapshotView? {
        resources.readiness
    }

    private var attention: ResourceAttentionView {
        resources.attention
    }

    /// The rescue action is offered only when the newest daemon pause reason is
    /// a completion-reserve reason. Eligibility itself always comes from the
    /// daemon status, never from the reason or local resource math.
    private var showsRescueAvailability: Bool {
        guard let pause = model.resourcePause else { return false }
        let reason = pause.reason.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard reason.hasPrefix("completion_reserve_") else { return false }
        return model.selectedSessionRescue?.active != true
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            header
            if let rescue = model.selectedSessionRescue,
               rescue.active || rescue.usedAt != nil || rescue.grant != nil
            {
                rescueStateRow(rescue)
            }
            if let pause = model.resourcePause {
                resourcePauseBanner(pause)
            }
            if showsRescueAvailability {
                rescueAvailabilityRow
            }
            if expanded {
                Divider().overlay(VPTheme.hairline)
                dimensionRows
                if !resources.warnings.isEmpty {
                    warnings
                }
            } else {
                primaryStrip
                if let readiness, readiness.showsDeliveryDetail {
                    readinessCompactLine(readiness)
                }
            }
            if let partial = model.selectedPartialDelivery {
                partialDeliveryRow(partial)
            }
            partialDeliveryActionRow
            if let error = model.sessionResourcesError {
                Label(error, systemImage: "exclamationmark.triangle.fill")
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.warning)
                    .lineLimit(2)
            }
            if let error = model.sessionDeliveryError {
                Label(error, systemImage: "exclamationmark.triangle.fill")
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.warning)
                    .lineLimit(2)
            }
        }
        .padding(.horizontal, 11)
        .padding(.vertical, 7)
        .background(
            VPTheme.surface.opacity(0.82),
            in: RoundedRectangle(cornerRadius: 11, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 11, style: .continuous)
                .strokeBorder(tint.opacity(resources.needsAttention ? 0.28 : 0.12), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.001, lift: 1)
        .onAppear {
            if resources.needsAttention {
                expanded = true
            }
        }
        .animation(
            VPMotion.resolved(VPMotion.snap, reduceMotion: reduceMotion),
            value: expanded
        )
        .sheet(isPresented: $showingTopUp) {
            TopUpResourcesSheet(snapshot: resources)
                .environment(model)
        }
        .sheet(isPresented: $showingPartialDelivery) {
            PartialDeliverySheet(sessionID: model.selectedSessionID ?? resources.sessionID)
                .environment(model)
        }
        .sheet(isPresented: $showingRescueConfirmation) {
            RescueReserveSheet(sessionID: model.selectedSessionID ?? resources.sessionID)
                .environment(model)
        }
    }

    private var tint: Color {
        if readiness?.level == "blocked" || resources.dimensions.contains(where: {
            $0.state == "hard_limit"
        }) {
            return VPTheme.danger
        }
        if resources.needsAttention {
            return VPTheme.warning
        }
        return VPTheme.accent
    }

    private func resourcePauseBanner(_ pause: ResourcePauseInfo) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 6) {
                Image(systemName: "pause.circle.fill")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(VPTheme.warning)
                Text("PAUSED BY RESOURCE GOVERNOR")
                    .font(VPTheme.mono(6.5, weight: .semibold))
                    .tracking(0.6)
                    .foregroundStyle(VPTheme.warning)
                Text(pause.phaseActionLabel)
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(1)
                Spacer(minLength: 4)
                if model.resourcePauseBusy {
                    ProgressView().controlSize(.mini)
                }
                Button {
                    Task { await model.resumeResourcePausedSession() }
                } label: {
                    Label("Resume", systemImage: "play.fill")
                }
                .buttonStyle(.plain)
                .font(VPTheme.mono(6.5, weight: .semibold))
                .foregroundStyle(VPTheme.accent)
                .disabled(model.resourcePauseBusy || isImportedSession)
                .help("Ask the daemon to resume. It re-checks the same cap and may pause again.")
                Button {
                    model.clearSessionResourceTopUpFeedback()
                    showingTopUp = true
                } label: {
                    Label("Top Up", systemImage: "plus.circle")
                }
                .buttonStyle(.plain)
                .font(VPTheme.mono(6.5, weight: .medium))
                .foregroundStyle(VPTheme.textSecondary)
                .disabled(isImportedSession)
                .help("Open the existing explicit top-up sheet")
                Button {
                    model.openApprovalCenter()
                } label: {
                    Label("Approvals", systemImage: "tray.full")
                }
                .buttonStyle(.plain)
                .font(VPTheme.mono(6.5, weight: .medium))
                .foregroundStyle(VPTheme.textSecondary)
                .help("Open the Manage Dash approval and question inbox")
                Button {
                    model.openPolicyCenter()
                } label: {
                    Label("Policy", systemImage: "slider.horizontal.3")
                }
                .buttonStyle(.plain)
                .font(VPTheme.mono(6.5, weight: .medium))
                .foregroundStyle(VPTheme.textSecondary)
                .help("Open Manage Dash → Policies to adjust the project RunPolicy.")
            }
            Text(pause.reasonLabel)
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textSecondary)
                .lineLimit(3)
                .fixedSize(horizontal: false, vertical: true)
            if let batch = pause.attentionBatch, batch.pending > 0 {
                attentionBatchView(batch)
            }
            HStack(spacing: 10) {
                Text("role \(pause.roleLabel)")
                Text("bottleneck \(pause.bottleneckLabel)")
                if let task = pause.taskShortLabel {
                    Text("task \(task)")
                }
                if let agent = pause.agentShortLabel {
                    Text("agent \(agent)")
                }
                Spacer(minLength: 4)
                Text("event #\(pause.sequence)")
                    .foregroundStyle(VPTheme.textTertiary.opacity(0.8))
            }
            .font(VPTheme.mono(6.5))
            .foregroundStyle(VPTheme.textTertiary)
            .lineLimit(1)
            if let notice = model.resourcePauseNotice {
                Text(notice)
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.info)
                    .lineLimit(2)
            }
            if let error = model.resourcePauseError {
                Text(error)
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.warning)
                    .lineLimit(2)
            }
        }
        .padding(8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            VPTheme.warning.opacity(0.07),
            in: RoundedRectangle(cornerRadius: 8, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .strokeBorder(VPTheme.warning.opacity(0.20), lineWidth: 1)
        }
    }

    /// Compact daemon-authoritative rescue state. Presence of a grant record on
    /// an inactive status is never rendered as an active rescue.
    private func rescueStateRow(_ rescue: RescueStatusView) -> some View {
        let active = rescue.active
        let tint = active ? VPTheme.success : VPTheme.textTertiary
        return VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Image(systemName: active ? "shield.lefthalf.filled" : "shield.slash")
                    .font(.system(size: 8, weight: .semibold))
                    .foregroundStyle(tint)
                Text(rescue.statusLabel)
                    .font(VPTheme.mono(6.5, weight: .semibold))
                    .tracking(0.6)
                    .foregroundStyle(tint)
                if active {
                    if let grant = rescue.activeGrant {
                        Text("expires \(grant.expiresAtLabel)")
                            .font(VPTheme.mono(6.5))
                            .foregroundStyle(VPTheme.textTertiary)
                            .lineLimit(1)
                        if let by = grant.recordedByLabel {
                            Text("by \(by)")
                                .font(VPTheme.mono(6.5))
                                .foregroundStyle(VPTheme.textTertiary)
                                .lineLimit(1)
                        } else {
                            Text("by not reported")
                                .font(VPTheme.mono(6.5))
                                .foregroundStyle(VPTheme.textTertiary)
                                .lineLimit(1)
                        }
                    } else {
                        Text("grant details not reported")
                            .font(VPTheme.mono(6.5))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                } else if let used = rescue.usedAtLabel {
                    Text("used \(used)")
                        .font(VPTheme.mono(6.5))
                        .foregroundStyle(VPTheme.textTertiary)
                        .lineLimit(1)
                    if let grantID = rescue.usedGrantShortLabel {
                        Text("grant \(grantID)")
                            .font(VPTheme.mono(6.5))
                            .foregroundStyle(VPTheme.textTertiary)
                            .lineLimit(1)
                    }
                } else if let grant = rescue.grant {
                    Text("grant \(grant.shortID)")
                        .font(VPTheme.mono(6.5))
                        .foregroundStyle(VPTheme.textTertiary)
                        .lineLimit(1)
                    Text("expires \(grant.expiresAtLabel)")
                        .font(VPTheme.mono(6.5))
                        .foregroundStyle(VPTheme.textTertiary)
                        .lineLimit(1)
                }
                Spacer(minLength: 4)
                if model.rescueBusy {
                    ProgressView().controlSize(.mini)
                }
            }
            if active {
                Text(
                    "One-shot completion-reserve release for the next turn — not a top-up or hard-cap bypass. It does not add budget, remove hard caps, change model routing, or bypass the Verifier or Integrator."
                )
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
            } else if let used = rescue.usedAtLabel {
                Text(
                    "That one-shot grant was used at \(used). It did not add budget or remove any hard cap."
                )
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
            }
            if let notice = model.rescueNotice {
                Text(notice)
                    .font(VPTheme.mono(6.8))
                    .foregroundStyle(VPTheme.info)
                    .lineLimit(2)
            }
            if let error = model.rescueError {
                Text(error)
                    .font(VPTheme.mono(6.8))
                    .foregroundStyle(VPTheme.warning)
                    .lineLimit(2)
            }
        }
        .padding(7)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            tint.opacity(active ? 0.07 : 0.04),
            in: RoundedRectangle(cornerRadius: 7, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .strokeBorder(tint.opacity(active ? 0.20 : 0.12), lineWidth: 1)
        }
    }

    /// The only rescue action surface. It exists solely for a daemon-reported
    /// completion-reserve pause, and eligibility still comes from the status.
    @ViewBuilder
    private var rescueAvailabilityRow: some View {
        if model.selectedSessionRescue?.eligible == true,
           model.selectedSessionRescue?.active != true,
           model.rescueError == nil
        {
            HStack(spacing: 7) {
                Button {
                    showingRescueConfirmation = true
                } label: {
                    Label("Rescue reserve", systemImage: "shield.lefthalf.filled")
                }
                .buttonStyle(.plain)
                .font(VPTheme.mono(6.8, weight: .semibold))
                .foregroundStyle(VPTheme.accent)
                .disabled(!model.canGrantRescue)
                .help("Release this session's completion reserve for the next turn only.")
                if model.rescueBusy {
                    ProgressView().controlSize(.mini)
                }
                Text(
                    model.canGrantRescue
                        ? "Release the completion reserve for the next turn only."
                        : (model.rescueActionBlockReason ?? "Rescue is unavailable.")
                )
                .font(VPTheme.mono(6.5))
                .foregroundStyle(VPTheme.textTertiary)
                .lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 4)
                Text("EXPLICIT · ONE-SHOT")
                    .font(VPTheme.mono(6.2, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
            }
        } else {
            HStack(spacing: 6) {
                Image(systemName: "shield.slash")
                    .font(.system(size: 8, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                Text(
                    "Rescue unavailable · \(model.rescueActionBlockReason ?? "The daemon has not reported eligibility.")"
                )
                .font(VPTheme.mono(6.5))
                .foregroundStyle(VPTheme.textTertiary)
                .lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 4)
            }
        }
    }

    private func attentionBatchView(_ batch: AttentionBatchView) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 5) {
                Text(L10n.t("ATTENTION BATCH"))
                    .font(VPTheme.mono(6.2, weight: .semibold))
                    .tracking(0.5)
                    .foregroundStyle(VPTheme.warning)
                Text(batch.summaryLabel)
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textSecondary)
                Spacer(minLength: 4)
                if let risk = batch.riskSummary {
                    Text(risk)
                        .font(VPTheme.mono(6.5))
                        .foregroundStyle(VPTheme.textTertiary)
                        .lineLimit(1)
                }
            }
            ForEach(Array(batch.items.prefix(3))) { item in
                HStack(spacing: 6) {
                    Text(item.tool)
                        .font(VPTheme.mono(7, weight: .medium))
                        .foregroundStyle(VPTheme.textSecondary)
                        .lineLimit(1)
                    Text(item.riskLabel)
                        .font(VPTheme.mono(6.2, weight: .semibold))
                        .foregroundStyle(batchRiskColor(item.risk))
                    if let task = item.taskShortLabel {
                        Text("task \(task)")
                            .font(VPTheme.mono(6.2))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                    if let reason = item.reasonLabel {
                        Text(reason)
                            .font(VPTheme.mono(6.2))
                            .foregroundStyle(VPTheme.textTertiary)
                            .lineLimit(1)
                    }
                    Spacer(minLength: 4)
                    if item.waiters > 1 {
                        Text("×\(item.waiters)")
                            .font(VPTheme.mono(6.2))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                    Text("p\(item.priority)")
                        .font(VPTheme.mono(6.2))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
            if batch.truncated || batch.remainingCount > 0 {
                Button {
                    model.openApprovalCenter()
                } label: {
                    Text(
                        batch.remainingCount > 0
                            ? L10n.format(
                                "Review %lld more in Inbox",
                                batch.remainingCount
                            )
                            : L10n.t("Review in Inbox")
                    )
                    .font(VPTheme.mono(6.5, weight: .medium))
                    .foregroundStyle(VPTheme.accent)
                }
                .buttonStyle(.plain)
            }
        }
        .padding(6)
        .background(
            VPTheme.surfaceSunken.opacity(0.55),
            in: RoundedRectangle(cornerRadius: 6, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 6, style: .continuous)
                .strokeBorder(VPTheme.warning.opacity(0.14), lineWidth: 1)
        }
    }

    private func batchRiskColor(_ risk: String?) -> Color {
        switch risk {
        case "destructive": VPTheme.danger
        case "host_state", "mcp", "sensitive_gui": VPTheme.warning
        case "external": VPTheme.info
        default: VPTheme.textTertiary
        }
    }

    private var isImportedSession: Bool {
        model.selectedSession?.status == "imported"
    }

    private var header: some View {
        HStack(spacing: 8) {
            Image(systemName: "gauge.with.dots.needle.50percent")
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(tint)
                .frame(width: 12)
            Text("RESOURCES")
                .font(VPTheme.mono(7, weight: .semibold))
                .tracking(1.0)
                .foregroundStyle(VPTheme.textTertiary)
            Menu {
                ForEach(RunProfile.allCases) { profile in
                    Button {
                        Task { await model.setRunProfile(profile) }
                    } label: {
                        Text("\(profile.title) profile")
                    }
                }
            } label: {
                Text(resources.profileValue.title.uppercased())
                    .font(VPTheme.mono(7, weight: .semibold))
                    .tracking(0.5)
                    .foregroundStyle(tint)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(tint.opacity(0.11), in: Capsule())
                    .overlay {
                        Capsule().strokeBorder(tint.opacity(0.22), lineWidth: 1)
                    }
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
            .disabled(model.sessionResourcesBusy)
            .help("Change the session Run Contract profile")
            if let readiness {
                Text(readiness.levelLabel)
                    .font(VPTheme.mono(7, weight: .semibold))
                    .foregroundStyle(readiness.needsAttention ? VPTheme.warning : VPTheme.info)
            }
            if let bottleneck = resources.bottleneck {
                Text("bottleneck \(bottleneck)")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.warning)
                    .lineLimit(1)
            }
            Spacer(minLength: 6)
            if model.sessionResourcesBusy || model.sessionResourceTopUpBusy {
                ProgressView().controlSize(.mini)
            }
            Button {
                model.clearSessionResourceTopUpFeedback()
                showingTopUp = true
            } label: {
                Image(systemName: "plus.circle")
            }
            .buttonStyle(VPGlassIconButtonStyle(size: 20, tint: tint))
            .help("Top up session resources")
            .accessibilityLabel("Top up session resources")
            .disabled(
                model.sessionResourceTopUpBusy
                    || model.selectedSession?.status == "imported"
            )
            Button {
                expanded.toggle()
            } label: {
                Image(systemName: expanded ? "chevron.up" : "chevron.down")
            }
            .buttonStyle(VPGlassIconButtonStyle(size: 20, tint: VPTheme.textTertiary))
            .help(expanded ? "Collapse resources" : "Expand resources")
            Button {
                Task {
                    await model.loadSessionResources()
                    await model.loadSessionDelivery()
                    await model.loadSessionRescue()
                }
            } label: {
                Image(systemName: "arrow.clockwise")
            }
            .buttonStyle(VPGlassIconButtonStyle(size: 20, tint: VPTheme.textTertiary))
            .help("Refresh resource snapshot")
        }
    }

    private var primaryStrip: some View {
        HStack(spacing: 12) {
            ForEach(primaryDimensions) { dimension in
                compactMetric(dimension)
            }
            Spacer(minLength: 4)
            if attention.interrupts > 0 {
                Text("\(attention.interrupts) interrupts")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
            }
        }
    }

    private var primaryDimensions: [ResourceDimensionView] {
        ["money", "time", "attention", "risk", "context"].compactMap {
            resources.dimension($0)
        }
    }

    private func compactMetric(_ dimension: ResourceDimensionView) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(dimension.label.uppercased())
                .font(VPTheme.mono(6, weight: .semibold))
                .tracking(0.6)
                .foregroundStyle(VPTheme.textTertiary)
            Text(dimensionValue(dimension))
                .font(VPTheme.mono(8, weight: .medium))
                .foregroundStyle(dimension.isAttention ? VPTheme.warning : VPTheme.textSecondary)
                .lineLimit(1)
        }
        .help(dimension.note ?? dimension.source)
    }

    private var dimensionRows: some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(resources.dimensions) { dimension in
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(dimension.label)
                        .font(VPTheme.mono(7.5, weight: .medium))
                        .foregroundStyle(VPTheme.textSecondary)
                        .frame(width: 108, alignment: .leading)
                    Text(dimensionValue(dimension))
                        .font(VPTheme.mono(7.5))
                        .foregroundStyle(dimension.isAttention ? VPTheme.warning : VPTheme.textSecondary)
                    Spacer(minLength: 4)
                    Text(dimension.stateLabel)
                        .font(VPTheme.mono(6.5, weight: .semibold))
                        .foregroundStyle(dimension.isAttention ? VPTheme.warning : VPTheme.textTertiary)
                    Text(dimension.source)
                        .font(VPTheme.mono(6.5))
                        .foregroundStyle(VPTheme.textTertiary.opacity(0.8))
                        .lineLimit(1)
                }
                if let note = dimension.note, !note.isEmpty {
                    Text(note)
                        .font(VPTheme.mono(6.5))
                        .foregroundStyle(VPTheme.textTertiary)
                        .padding(.leading, 116)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            if let readiness {
                readinessLine(readiness)
            }
        }
    }

    private func readinessLine(_ readiness: ReadinessSnapshotView) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 7) {
                Text("READINESS")
                    .font(VPTheme.mono(6.5, weight: .semibold))
                    .tracking(0.6)
                    .foregroundStyle(VPTheme.textTertiary)
                Text(readiness.levelLabel)
                    .font(VPTheme.mono(7, weight: .semibold))
                    .foregroundStyle(readinessColor(readiness))
                Text(readiness.shippabilityLabel)
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.textTertiary)
                Text(readiness.mergeabilityLabel)
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(readiness.mergeable ? VPTheme.success : VPTheme.textTertiary)
                Spacer(minLength: 4)
                Button {
                    copyReadinessReport(readiness)
                } label: {
                    Label("Copy readiness report", systemImage: "doc.on.doc")
                }
                .buttonStyle(.plain)
                .font(VPTheme.mono(6.5))
                .foregroundStyle(VPTheme.textSecondary)
                .help("Copy a bounded plain-text readiness report")
                .accessibilityLabel("Copy readiness report")
            }
            if let caveat = readiness.deliveryCaveat {
                Text(caveat)
                    .font(VPTheme.mono(7))
                    .foregroundStyle(readinessColor(readiness))
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let commit = readiness.shortCommit {
                Text("commit \(commit)")
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(1)
                    .help(readiness.latestCommit ?? "")
            }
            if !readiness.remainingWork.isEmpty {
                detailList("REMAINING WORK", items: readiness.remainingWork, tint: VPTheme.textSecondary)
            }
            if !readiness.blockers.isEmpty {
                detailList("BLOCKERS", items: readiness.blockers, tint: VPTheme.danger)
            }
        }
    }

    private func readinessCompactLine(_ readiness: ReadinessSnapshotView) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 6) {
                Image(systemName: readinessIcon(readiness))
                    .font(.system(size: 8, weight: .semibold))
                    .foregroundStyle(readinessColor(readiness))
                Text(readiness.deliveryCaveat ?? readiness.levelLabel)
                    .font(VPTheme.mono(7))
                    .foregroundStyle(readinessColor(readiness))
                    .lineLimit(2)
                Spacer(minLength: 4)
                if !readiness.remainingWork.isEmpty {
                    Text("\(readiness.remainingWork.count) remaining")
                        .font(VPTheme.mono(6.5))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                Button {
                    copyReadinessReport(readiness)
                } label: {
                    Image(systemName: "doc.on.doc")
                }
                .buttonStyle(VPGlassIconButtonStyle(size: 18, tint: VPTheme.textTertiary))
                .help("Copy readiness report")
                .accessibilityLabel("Copy readiness report")
            }
            if let blocker = readiness.blockers.first {
                Text("blocker: \(blocker)")
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.danger)
                    .lineLimit(2)
            } else if let next = readiness.remainingWork.first {
                Text("next: \(next)")
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.textSecondary)
                    .lineLimit(2)
            }
        }
    }

    private func partialDeliveryRow(_ record: PartialDeliveryRecordView) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                Image(systemName: "shippingbox.fill")
                    .font(.system(size: 8, weight: .semibold))
                    .foregroundStyle(VPTheme.warning)
                Text(record.statusLabel)
                    .font(VPTheme.mono(6.5, weight: .semibold))
                    .tracking(0.6)
                    .foregroundStyle(VPTheme.warning)
                Text(record.recordedAtLabel)
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(1)
                Text(record.readinessLabel)
                    .font(VPTheme.mono(6.5, weight: .semibold))
                    .foregroundStyle(VPTheme.textSecondary)
                if let by = record.recordedByLabel {
                    Text("by \(by)")
                        .font(VPTheme.mono(6.5))
                        .foregroundStyle(VPTheme.textTertiary)
                        .lineLimit(1)
                }
                Spacer(minLength: 4)
                Text(record.remainingCountLabel)
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            Text(record.summaryLabel)
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textSecondary)
                .lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 10) {
                if let commit = record.shortCommit {
                    Text("commit \(commit)")
                        .help(record.latestCommit ?? "")
                }
                if record.uncommitted {
                    Text("uncommitted changes")
                        .foregroundStyle(VPTheme.warning)
                }
                if let preview = record.remainingPreview {
                    Text("next: \(preview)")
                        .lineLimit(1)
                }
                Spacer(minLength: 4)
                Text("RECORD · NOT A MERGE OR COMPLETION")
                    .font(VPTheme.mono(6.2, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            .font(VPTheme.mono(6.5))
            .foregroundStyle(VPTheme.textTertiary)
        }
        .padding(7)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            VPTheme.warning.opacity(0.06),
            in: RoundedRectangle(cornerRadius: 7, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .strokeBorder(VPTheme.warning.opacity(0.17), lineWidth: 1)
        }
        .help(record.truthfulCaveat)
    }

    private var partialDeliveryActionRow: some View {
        HStack(spacing: 7) {
            Button {
                model.clearPartialDeliveryFeedback()
                showingPartialDelivery = true
            } label: {
                Label("Record partial…", systemImage: "square.and.pencil")
            }
            .buttonStyle(.plain)
            .font(VPTheme.mono(6.8, weight: model.canRecordPartialDelivery ? .semibold : .regular))
            .foregroundStyle(
                model.canRecordPartialDelivery ? VPTheme.accent : VPTheme.textTertiary
            )
            .disabled(!model.canRecordPartialDelivery)
            .help(
                model.partialDeliveryActionBlockReason
                    ?? "Record an explicit partial delivery. This never merges or completes the session."
            )
            if model.partialDeliveryBusy || model.sessionDeliveryBusy {
                ProgressView().controlSize(.mini)
            }
            if let reason = model.partialDeliveryActionBlockReason {
                Text(reason)
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                Text("Explicit record · never an automatic merge or resume")
                    .font(VPTheme.mono(6.2))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(1)
            }
            Spacer(minLength: 4)
        }
    }

    private func detailList(_ title: String, items: [String], tint: Color) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(title)
                .font(VPTheme.mono(6, weight: .semibold))
                .tracking(0.5)
                .foregroundStyle(VPTheme.textTertiary)
            ForEach(Array(items.prefix(4).enumerated()), id: \.offset) { _, item in
                Text("- \(item)")
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(tint)
                    .lineLimit(2)
            }
            if items.count > 4 {
                Text("+ \(items.count - 4) more")
                    .font(VPTheme.mono(6))
                    .foregroundStyle(VPTheme.textTertiary)
            }
        }
        .padding(.leading, 2)
    }

    private func readinessIcon(_ readiness: ReadinessSnapshotView) -> String {
        readiness.level == "blocked" ? "xmark.octagon.fill" : "exclamationmark.circle.fill"
    }

    private func readinessColor(_ readiness: ReadinessSnapshotView) -> Color {
        if readiness.level == "blocked" || readiness.level == "dirty" {
            return VPTheme.danger
        }
        if readiness.showsDeliveryDetail {
            return VPTheme.warning
        }
        return readiness.needsAttention ? VPTheme.warning : VPTheme.textTertiary
    }

    private func copyReadinessReport(_ readiness: ReadinessSnapshotView) {
        let report = readiness.boundedReport()
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(report, forType: .string)
        model.showToast(
            "Readiness report copied",
            detail: "\(readiness.levelLabel) · bounded plain text",
            kind: .info
        )
    }

    private var warnings: some View {
        VStack(alignment: .leading, spacing: 2) {
            ForEach(resources.warnings.prefix(4), id: \.self) { warning in
                Text(warning)
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.textTertiary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private func dimensionValue(_ dimension: ResourceDimensionView) -> String {
        switch dimension.key {
        case "money":
            if resources.pricingSource == "unconfigured" {
                return "prices unset"
            }
            return String(format: "$%.2f", dimension.used)
        case "time":
            return formatSeconds(dimension.used)
        case "attention":
            return "\(Int(dimension.used)) / \(Int(dimension.hardCap ?? 0))"
        case "context":
            if dimension.state == "unavailable" {
                return "n/a"
            }
            return "\(compactNumber(dimension.used)) / \(compactNumber(dimension.hardCap ?? 0))"
        case "risk":
            return "\(Int(dimension.used)) units"
        default:
            return "\(compactNumber(dimension.used)) \(dimension.unit)"
        }
    }

    private func compactNumber(_ value: Double) -> String {
        if value >= 1_000_000 {
            return String(format: "%.1fm", value / 1_000_000)
        }
        if value >= 1000 {
            return String(format: "%.1fk", value / 1000)
        }
        return String(format: "%.0f", value)
    }

    private func formatSeconds(_ value: Double) -> String {
        let seconds = Int(value.rounded())
        if seconds >= 3600 {
            return "\(seconds / 3600)h \((seconds % 3600) / 60)m"
        }
        if seconds >= 60 {
            return "\(seconds / 60)m \(seconds % 60)s"
        }
        return "\(seconds)s"
    }
}

// MARK: - Operator top-up sheet

/// One explicit additive top-up. Validation is local so an empty/zero/negative
/// request cannot be submitted; the daemon remains authoritative for clamping
/// to hard caps and for the final budget/resources snapshot.
struct TopUpResourcesSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let snapshot: ResourceSnapshotView

    @State private var draft = TopUpSessionResourcesDraft()

    private var selectedBudget: SessionBudgetStatus? {
        guard let status = model.sessionBudgetStatus,
              status.sessionID.isEmpty || status.sessionID == snapshot.sessionID
        else { return nil }
        return status
    }

    private var imported: Bool {
        model.selectedSession?.status == "imported"
    }

    private var sessionMatches: Bool {
        snapshot.sessionID.isEmpty || model.selectedSessionID == snapshot.sessionID
    }

    private var validationIssue: BudgetValidationIssue? {
        draft.validationIssue
    }

    private var canSubmit: Bool {
        !model.sessionResourceTopUpBusy
            && !imported
            && sessionMatches
            && draft.request != nil
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            VPHairline()
            ScrollView {
                VStack(alignment: .leading, spacing: 15) {
                    currentBudget
                    fields
                    resumeToggle
                    validationLine
                    feedback
                    note
                }
                .padding(20)
            }
            VPHairline()
            footer
        }
        .frame(width: 540)
        .frame(maxHeight: 640)
        .background { VPCrystalBackdrop() }
        .tint(VPTheme.accent)
        .onChange(of: draft) { _, _ in
            model.clearSessionResourceTopUpFeedback()
        }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 12) {
            ZStack {
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .fill(VPTheme.accentWash)
                    .frame(width: 40, height: 40)
                Image(systemName: "plus.circle")
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(VPTheme.accent)
            }
            VStack(alignment: .leading, spacing: 3) {
                Text("TOP UP SESSION")
                    .font(VPTheme.mono(7.5, weight: .semibold))
                    .tracking(1.2)
                    .foregroundStyle(VPTheme.accent)
                Text("Add bounded resources without removing caps")
                    .font(VPTheme.font(18, weight: .semibold, rounded: true))
                    .foregroundStyle(VPTheme.textPrimary)
                    .lineLimit(2)
                Text("Session \(snapshot.sessionID.prefix(8)) · one explicit request")
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            Spacer()
            Button("Cancel") { dismiss() }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .keyboardShortcut(.cancelAction)
        }
        .padding(20)
        .background(VPTheme.rail)
    }

    private var currentBudget: some View {
        HStack(alignment: .top, spacing: 12) {
            summaryMetric(
                "Remaining",
                BudgetFormatting.combined(
                    tokens: selectedBudget?.remainingTokens,
                    cost: selectedBudget?.remainingCostUSD,
                    seconds: selectedBudget?.remainingSecs,
                    empty: "Unavailable"
                )
            )
            summaryMetric(
                "Mode",
                selectedBudget.map { $0.unlimited ? "Unlimited" : $0.mode.label }
                    ?? "Unavailable"
            )
            summaryMetric(
                "Hard cap",
                selectedBudget.map {
                    BudgetFormatting.combined(
                        tokens: $0.hardTokenCap,
                        cost: $0.hardCostCapUSD,
                        seconds: $0.hardWallClockSecs,
                        empty: "No hard cap"
                    )
                } ?? "Unavailable"
            )
        }
    }

    private var fields: some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text("INCREMENTS")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .tracking(0.7)
                    .foregroundStyle(VPTheme.textSecondary)
                Text("All optional; every entered value must be positive.")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            HStack(alignment: .top, spacing: 10) {
                numberField("Tokens", text: $draft.tokensText, placeholder: "e.g. 250000")
                numberField("Cost (USD)", text: $draft.costUSDText, placeholder: "e.g. 1.50")
                numberField("Time (seconds)", text: $draft.timeSecsText, placeholder: "e.g. 1800")
            }
            .disabled(model.sessionResourceTopUpBusy || imported)
        }
    }

    private var resumeToggle: some View {
        Toggle(isOn: $draft.resume) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Resume after top-up")
                    .font(VPTheme.mono(8.5, weight: .medium))
                    .foregroundStyle(VPTheme.textSecondary)
                Text(
                    "Only resumes a paused, failed, or cancelled session. Approvals, verification, and integration gates still apply."
                )
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
            }
        }
        .toggleStyle(.switch)
        .controlSize(.small)
        .disabled(model.sessionResourceTopUpBusy || imported)
    }

    @ViewBuilder
    private var validationLine: some View {
        if !sessionMatches {
            Label(
                "The selected session changed; close this sheet and reopen it from the active session.",
                systemImage: "arrow.triangle.2.circlepath"
            )
            .font(VPTheme.mono(7.5))
            .foregroundStyle(VPTheme.warning)
            .fixedSize(horizontal: false, vertical: true)
        } else if imported {
            Label(
                "Imported sessions are read-only; top-up is unavailable.",
                systemImage: "lock.fill"
            )
            .font(VPTheme.mono(7.5))
            .foregroundStyle(VPTheme.textTertiary)
        } else if !draft.hasAnyInput {
            Text("Enter at least one positive token, cost, or time increment. Resume alone is not a top-up.")
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
        } else if let validationIssue {
            Label(validationIssue.displayText, systemImage: "exclamationmark.triangle.fill")
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.warning)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    @ViewBuilder
    private var feedback: some View {
        if let result = model.sessionResourceTopUpResult,
           let notice = model.sessionResourceTopUpNotice
        {
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 6) {
                    badge(
                        result.capped ? "CAPPED AT HARD CAP" : "APPLIED",
                        tint: result.capped ? VPTheme.warning : VPTheme.success
                    )
                    if result.resumed {
                        badge("RESUMED", tint: VPTheme.success)
                    }
                    Spacer(minLength: 4)
                    Text(
                        "remaining \(BudgetFormatting.combined(tokens: result.budget.remainingTokens, cost: result.budget.remainingCostUSD, seconds: result.budget.remainingSecs, empty: "unavailable"))"
                    )
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(1)
                }
                Text(notice)
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(result.capped ? VPTheme.warning : VPTheme.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(9)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                (result.capped ? VPTheme.warning : VPTheme.success).opacity(0.08),
                in: RoundedRectangle(cornerRadius: 9)
            )
            .overlay {
                RoundedRectangle(cornerRadius: 9)
                    .strokeBorder(
                        (result.capped ? VPTheme.warning : VPTheme.success).opacity(0.22),
                        lineWidth: 1
                    )
            }
        }
        if let error = model.sessionResourceTopUpError {
            Label(error, systemImage: "exclamationmark.triangle.fill")
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.warning)
                .fixedSize(horizontal: false, vertical: true)
                .padding(9)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(VPTheme.warning.opacity(0.08), in: RoundedRectangle(cornerRadius: 9))
        }
    }

    private var note: some View {
        Text(
            selectedBudget?.unlimited == true
                ? "This session is Unlimited. A top-up does not change that mode, and no cap is added by this sheet."
                : "The daemon adds these amounts to the effective budget, clamps them to existing hard caps, and never removes a cap or selects Unlimited. No automatic top-up or polling."
        )
        .font(VPTheme.mono(7.5))
        .foregroundStyle(VPTheme.textTertiary)
        .fixedSize(horizontal: false, vertical: true)
    }

    private var footer: some View {
        HStack(spacing: 8) {
            if model.sessionResourceTopUpBusy {
                ProgressView().controlSize(.small)
            }
            Text(imported ? "Imported session · read-only" : "No Retry · one explicit request")
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textTertiary)
            Spacer()
            Button("Cancel") { dismiss() }
                .keyboardShortcut(.cancelAction)
            Button {
                submit()
            } label: {
                Label("Apply top-up", systemImage: "plus.circle")
            }
            .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
            .keyboardShortcut(.defaultAction)
            .disabled(!canSubmit)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 13)
        .background(VPTheme.rail)
    }

    private func submit() {
        guard let request = draft.request else { return }
        Task {
            await model.topUpSessionResources(
                sessionID: snapshot.sessionID.isEmpty ? nil : snapshot.sessionID,
                addTokens: request.addTokens,
                addCostUSD: request.addCostUSD,
                addTimeSecs: request.addTimeSecs,
                resume: request.resume,
                reason: "Stage top-up"
            )
        }
    }

    private func summaryMetric(_ title: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title.uppercased())
                .font(VPTheme.mono(6.5, weight: .semibold))
                .tracking(0.4)
                .foregroundStyle(VPTheme.textTertiary)
            Text(value)
                .font(VPTheme.mono(8.5))
                .foregroundStyle(VPTheme.textSecondary)
                .lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func numberField(
        _ title: String,
        text: Binding<String>,
        placeholder: String
    ) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title.uppercased())
                .font(VPTheme.mono(6.5, weight: .semibold))
                .tracking(0.4)
                .foregroundStyle(VPTheme.textTertiary)
            TextField(placeholder, text: text)
                .textFieldStyle(.plain)
                .font(VPTheme.mono(10))
                .vpFieldSurface(radius: 8)
                .accessibilityLabel(title)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func badge(_ title: String, tint: Color) -> some View {
        Text(title)
            .font(VPTheme.mono(6.5, weight: .semibold))
            .tracking(0.4)
            .foregroundStyle(tint)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(tint.opacity(0.11), in: Capsule())
            .overlay {
                Capsule().strokeBorder(tint.opacity(0.22), lineWidth: 1)
            }
    }
}

// MARK: - Explicit partial-delivery sheet

/// One explicit partial-delivery record. It never commits, merges, approves,
/// completes, or resumes the session; the daemon validates eligibility and is
/// authoritative for every rejection.
struct PartialDeliverySheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let sessionID: String

    @State private var draft = PartialDeliveryDraft()

    private var imported: Bool {
        model.selectedSession?.status == "imported"
    }

    private var runningOrQueued: Bool {
        guard let status = model.selectedSession?.status else { return false }
        return ["running", "queued"].contains(status)
    }

    private var sessionMatches: Bool {
        !sessionID.isEmpty && model.selectedSessionID == sessionID
    }

    private var canSubmit: Bool {
        model.canRecordPartialDelivery
            && sessionMatches
            && draft.request != nil
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            VPHairline()
            ScrollView {
                VStack(alignment: .leading, spacing: 15) {
                    warning
                    summaryField
                    remainingWorkField
                    recordedByField
                    boundsLine
                    validationLine
                    feedback
                }
                .padding(20)
            }
            VPHairline()
            footer
        }
        .frame(width: 560)
        .frame(maxHeight: 720)
        .background { VPCrystalBackdrop() }
        .tint(VPTheme.accent)
        .onChange(of: draft) { _, _ in
            model.clearPartialDeliveryFeedback()
        }
        .onAppear {
            model.clearPartialDeliveryFeedback()
        }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 12) {
            ZStack {
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .fill(VPTheme.warning.opacity(0.12))
                    .frame(width: 40, height: 40)
                Image(systemName: "shippingbox")
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(VPTheme.warning)
            }
            VStack(alignment: .leading, spacing: 3) {
                Text("RECORD PARTIAL DELIVERY")
                    .font(VPTheme.mono(7.5, weight: .semibold))
                    .tracking(1.2)
                    .foregroundStyle(VPTheme.warning)
                Text("Label remaining work without claiming completion")
                    .font(VPTheme.font(18, weight: .semibold, rounded: true))
                    .foregroundStyle(VPTheme.textPrimary)
                    .lineLimit(2)
                Text(
                    "Session \(sessionID.isEmpty ? "—" : String(sessionID.prefix(8))) · explicit operator action"
                )
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
            }
            Spacer()
            Button("Cancel") { dismiss() }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .keyboardShortcut(.cancelAction)
                .disabled(model.partialDeliveryBusy)
        }
        .padding(20)
        .background(VPTheme.rail)
    }

    private var warning: some View {
        Label(
            "This records a partial delivery only. It does not commit, merge, approve, complete, or resume the session, and it does not bypass the Verifier or Integrator. The daemon validates eligibility and rejects empty deliveries, running/queued or imported sessions, and already-complete merged sessions.",
            systemImage: "exclamationmark.triangle.fill"
        )
        .font(VPTheme.mono(7.5))
        .foregroundStyle(VPTheme.warning)
        .fixedSize(horizontal: false, vertical: true)
        .padding(9)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(VPTheme.warning.opacity(0.07), in: RoundedRectangle(cornerRadius: 9))
        .overlay {
            RoundedRectangle(cornerRadius: 9)
                .strokeBorder(VPTheme.warning.opacity(0.18), lineWidth: 1)
        }
    }

    private var summaryField: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text("SUMMARY")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .tracking(0.7)
                    .foregroundStyle(VPTheme.textSecondary)
                Spacer()
                Text(
                    "\(draft.summaryCharacterCount) / \(PartialDeliveryRecordView.summaryCharacterLimit) characters"
                )
                .font(VPTheme.mono(7))
                .foregroundStyle(
                    draft.isOverSummaryLimit ? VPTheme.warning : VPTheme.textTertiary
                )
            }
            TextEditor(text: $draft.summary)
                .font(VPTheme.mono(10))
                .scrollContentBackground(.hidden)
                .padding(7)
                .frame(minHeight: 78)
                .background(
                    VPTheme.surface,
                    in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                )
                .overlay {
                    RoundedRectangle(cornerRadius: 8, style: .continuous)
                        .strokeBorder(VPTheme.hairline, lineWidth: 1)
                }
                .overlay(alignment: .topLeading) {
                    if draft.summary.isEmpty {
                        Text("What is complete, and what remains uncertain?")
                            .font(VPTheme.mono(9))
                            .foregroundStyle(VPTheme.textTertiary)
                            .padding(.horizontal, 12)
                            .padding(.vertical, 11)
                            .allowsHitTesting(false)
                    }
                }
                .disabled(model.partialDeliveryBusy)
        }
    }

    private var remainingWorkField: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text("REMAINING WORK")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .tracking(0.7)
                    .foregroundStyle(VPTheme.textSecondary)
                Spacer()
                Text(
                    "\(draft.remainingItemCount) / \(PartialDeliveryRecordView.remainingItemLimit) items"
                )
                .font(VPTheme.mono(7))
                .foregroundStyle(
                    draft.isOverRemainingItemLimit ? VPTheme.warning : VPTheme.textTertiary
                )
            }
            TextEditor(text: $draft.remainingWorkText)
                .font(VPTheme.mono(10))
                .scrollContentBackground(.hidden)
                .padding(7)
                .frame(minHeight: 104)
                .background(
                    VPTheme.surface,
                    in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                )
                .overlay {
                    RoundedRectangle(cornerRadius: 8, style: .continuous)
                        .strokeBorder(VPTheme.hairline, lineWidth: 1)
                }
                .overlay(alignment: .topLeading) {
                    if draft.remainingWorkText.isEmpty {
                        Text("One remaining-work item per line")
                            .font(VPTheme.mono(9))
                            .foregroundStyle(VPTheme.textTertiary)
                            .padding(.horizontal, 12)
                            .padding(.vertical, 11)
                            .allowsHitTesting(false)
                    }
                }
                .disabled(model.partialDeliveryBusy)
            Text(
                "Blank lines are ignored. Each item is bounded to \(PartialDeliveryRecordView.remainingItemCharacterLimit) characters and at most \(PartialDeliveryRecordView.remainingItemLimit) items are sent."
            )
            .font(VPTheme.mono(7))
            .foregroundStyle(VPTheme.textTertiary)
            .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var recordedByField: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text("RECORDED BY (OPTIONAL)")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .tracking(0.7)
                    .foregroundStyle(VPTheme.textSecondary)
                Spacer()
                Text(
                    "\(draft.recordedByCharacterCount) / \(PartialDeliveryRecordView.recordedByCharacterLimit)"
                )
                .font(VPTheme.mono(7))
                .foregroundStyle(
                    draft.isOverRecordedByLimit ? VPTheme.warning : VPTheme.textTertiary
                )
            }
            TextField("e.g. operator", text: $draft.recordedBy)
                .textFieldStyle(.plain)
                .font(VPTheme.mono(10))
                .vpFieldSurface(radius: 8)
                .accessibilityLabel("Recorded by")
                .disabled(model.partialDeliveryBusy)
        }
    }

    @ViewBuilder
    private var boundsLine: some View {
        if draft.isClientBounded {
            Text(
                "Over-limit text is trimmed to the client bounds before sending: \(PartialDeliveryRecordView.summaryCharacterLimit) summary characters, \(PartialDeliveryRecordView.remainingItemLimit) remaining items, \(PartialDeliveryRecordView.remainingItemCharacterLimit) characters per item, and \(PartialDeliveryRecordView.recordedByCharacterLimit) recorded-by characters."
            )
            .font(VPTheme.mono(7))
            .foregroundStyle(VPTheme.warning)
            .fixedSize(horizontal: false, vertical: true)
        }
    }

    @ViewBuilder
    private var validationLine: some View {
        if !sessionMatches {
            Label(
                "The selected session changed; close this sheet and reopen it from the active session.",
                systemImage: "arrow.triangle.2.circlepath"
            )
            .font(VPTheme.mono(7.5))
            .foregroundStyle(VPTheme.warning)
            .fixedSize(horizontal: false, vertical: true)
        } else if imported {
            Label(
                "Imported sessions are read-only; partial delivery is unavailable.",
                systemImage: "lock.fill"
            )
            .font(VPTheme.mono(7.5))
            .foregroundStyle(VPTheme.textTertiary)
        } else if runningOrQueued {
            Label(
                "Running and queued sessions cannot record a partial delivery. Wait until the session is idle, paused, or failed.",
                systemImage: "clock.fill"
            )
            .font(VPTheme.mono(7.5))
            .foregroundStyle(VPTheme.warning)
            .fixedSize(horizontal: false, vertical: true)
        } else if let reason = model.partialDeliveryActionBlockReason {
            Text(reason)
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
        } else if draft.request == nil {
            Text("Add a summary or at least one remaining-work item to enable recording.")
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    @ViewBuilder
    private var feedback: some View {
        if let status = model.partialDeliveryStatus {
            VStack(alignment: .leading, spacing: 5) {
                HStack(spacing: 6) {
                    badge("RECORDED", tint: VPTheme.success)
                    if let readiness = model.selectedPartialDelivery?.readinessLabel {
                        badge(readiness, tint: VPTheme.textTertiary)
                    }
                    Spacer(minLength: 4)
                    Text("not merged or completed")
                        .font(VPTheme.mono(6.5, weight: .semibold))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                Text(status)
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(9)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(VPTheme.success.opacity(0.08), in: RoundedRectangle(cornerRadius: 9))
            .overlay {
                RoundedRectangle(cornerRadius: 9)
                    .strokeBorder(VPTheme.success.opacity(0.22), lineWidth: 1)
            }
        }
        if let error = model.partialDeliveryError {
            Label(error, systemImage: "exclamationmark.triangle.fill")
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.warning)
                .fixedSize(horizontal: false, vertical: true)
                .padding(9)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(VPTheme.warning.opacity(0.08), in: RoundedRectangle(cornerRadius: 9))
        }
    }

    private var footer: some View {
        HStack(spacing: 8) {
            if model.partialDeliveryBusy {
                ProgressView().controlSize(.small)
                Text("Recording…")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textSecondary)
            } else {
                Text(
                    model.partialDeliveryActionBlockReason
                        ?? (draft.request == nil
                            ? "Add a summary or at least one remaining item."
                            : "One explicit record · not a merge or completion")
                )
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textTertiary)
                .lineLimit(2)
            }
            Spacer()
            Button(L10n.t(model.partialDeliveryStatus == nil ? "Cancel" : "Close")) {
                dismiss()
            }
            .keyboardShortcut(.cancelAction)
            .disabled(model.partialDeliveryBusy)
            Button {
                submit()
            } label: {
                Label(L10n.t("Record partial delivery"), systemImage: "shippingbox")
            }
            .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
            .keyboardShortcut(.defaultAction)
            .disabled(!canSubmit)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 13)
        .background(VPTheme.rail)
    }

    private func submit() {
        guard let request = draft.request else { return }
        Task {
            await model.recordPartialDelivery(
                summary: request.summary ?? "",
                remainingWork: request.remainingWork,
                recordedBy: request.recordedBy
            )
        }
    }

    private func badge(_ title: String, tint: Color) -> some View {
        Text(title)
            .font(VPTheme.mono(6.5, weight: .semibold))
            .tracking(0.4)
            .foregroundStyle(tint)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(tint.opacity(0.11), in: Capsule())
            .overlay {
                Capsule().strokeBorder(tint.opacity(0.22), lineWidth: 1)
            }
    }
}

// MARK: - Completion-reserve rescue sheet

/// One explicit completion-reserve release. The daemon owns eligibility and
/// the refreshed status; this sheet only presents that evidence and an
/// optional audit note. It never adds budget or changes execution gates.
struct RescueReserveSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let sessionID: String

    @State private var reasonDraft = ""

    private var rescue: RescueStatusView? {
        model.selectedSessionRescue
    }

    private var sessionMatches: Bool {
        !sessionID.isEmpty && model.selectedSessionID == sessionID
    }

    private var imported: Bool {
        model.selectedSession?.status == "imported"
    }

    private var canSubmit: Bool {
        sessionMatches && model.canGrantRescue
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            VPHairline()
            ScrollView {
                VStack(alignment: .leading, spacing: 15) {
                    warning
                    summary
                    reserve
                    candidateSection(
                        "NEAR COMPLETION",
                        candidates: rescue?.nearCompletionPreview ?? [],
                        overflow: rescue?.nearCompletionOverflow ?? 0
                    )
                    candidateSection(
                        "LOOPING (NOT RESCUABLE)",
                        candidates: rescue?.loopingPreview ?? [],
                        overflow: rescue?.loopingOverflow ?? 0
                    )
                    recommendations
                    reasonField
                    validationLine
                    feedback
                }
                .padding(20)
            }
            VPHairline()
            footer
        }
        .frame(width: 580)
        .frame(maxHeight: 740)
        .background { VPCrystalBackdrop() }
        .tint(VPTheme.accent)
        .onChange(of: reasonDraft) { _, newValue in
            if newValue.count > RescueGrantView.reasonCharacterLimit {
                reasonDraft = String(newValue.prefix(RescueGrantView.reasonCharacterLimit))
            }
        }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 12) {
            ZStack {
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .fill(VPTheme.success.opacity(0.12))
                    .frame(width: 40, height: 40)
                Image(systemName: "shield.lefthalf.filled")
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(VPTheme.success)
            }
            VStack(alignment: .leading, spacing: 3) {
                Text("RELEASE COMPLETION RESERVE")
                    .font(VPTheme.mono(7.5, weight: .semibold))
                    .tracking(1.2)
                    .foregroundStyle(VPTheme.success)
                Text("One explicit rescue for the next turn")
                    .font(VPTheme.font(18, weight: .semibold, rounded: true))
                    .foregroundStyle(VPTheme.textPrimary)
                    .lineLimit(2)
                Text(
                    "Session \(sessionID.isEmpty ? "—" : String(sessionID.prefix(8))) · daemon-validated eligibility"
                )
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
            }
            Spacer()
            Button("Cancel") { dismiss() }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .keyboardShortcut(.cancelAction)
        }
        .padding(20)
        .background(VPTheme.rail)
    }

    private var warning: some View {
        Label(
            "This releases the session's completion reserve for the next turn only. It does not add budget, remove a hard cap, change model routing, or bypass the Verifier or Integrator. The daemon can still pause again if the cap is unchanged.",
            systemImage: "exclamationmark.shield.fill"
        )
        .font(VPTheme.mono(7.5))
        .foregroundStyle(VPTheme.warning)
        .fixedSize(horizontal: false, vertical: true)
        .padding(9)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(VPTheme.warning.opacity(0.07), in: RoundedRectangle(cornerRadius: 9))
        .overlay {
            RoundedRectangle(cornerRadius: 9)
                .strokeBorder(VPTheme.warning.opacity(0.18), lineWidth: 1)
        }
    }

    private var summary: some View {
        HStack(alignment: .top, spacing: 12) {
            metric("Readiness", rescue?.readiness.levelLabel ?? "Unavailable")
            metric("Latest commit", rescue?.readiness.shortCommit ?? "none reported")
            metric(
                "No-progress turns",
                rescue.map { "\($0.noProgressTurns)" } ?? "Unavailable"
            )
            metric("Daemon reason", rescue?.reasonLabel ?? "Unavailable")
        }
    }

    private var reserve: some View {
        VStack(alignment: .leading, spacing: 5) {
            Text("DAEMON-REPORTED RESERVE")
                .font(VPTheme.mono(8, weight: .semibold))
                .tracking(0.7)
                .foregroundStyle(VPTheme.textSecondary)
            HStack(alignment: .top, spacing: 12) {
                metric("Completion reserve", rescue?.reserve.completionReservePercentLabel ?? "Unavailable")
                metric("Remaining", rescue?.reserve.remainingLabel ?? "Unavailable")
                metric("Hard cap", rescue?.reserve.hardCapLabel ?? "Unavailable")
            }
        }
        .padding(9)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            VPTheme.surfaceSunken.opacity(0.55),
            in: RoundedRectangle(cornerRadius: 9, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
    }

    @ViewBuilder
    private func candidateSection(
        _ title: String,
        candidates: [RescueCandidateView],
        overflow: Int
    ) -> some View {
        if !candidates.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                Text(title)
                    .font(VPTheme.mono(7.5, weight: .semibold))
                    .tracking(0.6)
                    .foregroundStyle(VPTheme.textTertiary)
                ForEach(candidates) { candidate in
                    VStack(alignment: .leading, spacing: 1) {
                        HStack(spacing: 6) {
                            Text(candidate.displayLabel)
                                .font(VPTheme.mono(7.5, weight: .medium))
                                .foregroundStyle(VPTheme.textSecondary)
                                .lineLimit(1)
                            if let commit = candidate.shortCommit {
                                Text("commit \(commit)")
                                    .font(VPTheme.mono(6.5))
                                    .foregroundStyle(VPTheme.textTertiary)
                                    .lineLimit(1)
                            }
                            Spacer(minLength: 4)
                        }
                        if let detail = candidate.detailLabel {
                            Text(detail)
                                .font(VPTheme.mono(6.8))
                                .foregroundStyle(VPTheme.textTertiary)
                                .lineLimit(2)
                        }
                    }
                }
                if overflow > 0 {
                    Text("+ \(overflow) more")
                        .font(VPTheme.mono(6.5))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
        }
    }

    @ViewBuilder
    private var recommendations: some View {
        if let rescue, !rescue.recommendationPreviews.isEmpty {
            VStack(alignment: .leading, spacing: 3) {
                Text("DAEMON RECOMMENDATIONS")
                    .font(VPTheme.mono(7.5, weight: .semibold))
                    .tracking(0.6)
                    .foregroundStyle(VPTheme.textTertiary)
                ForEach(Array(rescue.recommendationPreviews.enumerated()), id: \.offset) { _, item in
                    Text("- \(item)")
                        .font(VPTheme.mono(6.8))
                        .foregroundStyle(VPTheme.textSecondary)
                        .lineLimit(2)
                }
                if rescue.recommendationOverflow > 0 {
                    Text("+ \(rescue.recommendationOverflow) more")
                        .font(VPTheme.mono(6.5))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
        }
    }

    private var reasonField: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text("OPERATOR AUDIT REASON (OPTIONAL)")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .tracking(0.7)
                    .foregroundStyle(VPTheme.textSecondary)
                Spacer()
                Text("\(reasonDraft.count) / \(RescueGrantView.reasonCharacterLimit)")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            TextField("e.g. finish the verified near-completion task", text: $reasonDraft)
                .textFieldStyle(.plain)
                .font(VPTheme.mono(10))
                .vpFieldSurface(radius: 8)
                .accessibilityLabel("Operator audit reason")
                .disabled(model.rescueBusy)
        }
    }

    @ViewBuilder
    private var validationLine: some View {
        if !sessionMatches {
            Label(
                "The selected session changed; close this sheet and reopen it from the active session.",
                systemImage: "arrow.triangle.2.circlepath"
            )
            .font(VPTheme.mono(7.5))
            .foregroundStyle(VPTheme.warning)
            .fixedSize(horizontal: false, vertical: true)
        } else if imported {
            Label(
                "Imported sessions are read-only; rescue is unavailable.",
                systemImage: "lock.fill"
            )
            .font(VPTheme.mono(7.5))
            .foregroundStyle(VPTheme.textTertiary)
        } else if let reason = model.rescueActionBlockReason {
            Text(reason)
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.warning)
                .fixedSize(horizontal: false, vertical: true)
        } else {
            Text(
                "The daemon reports this session eligible. Confirming sends one request with confirm=true; nothing is retried automatically."
            )
            .font(VPTheme.mono(7))
            .foregroundStyle(VPTheme.textTertiary)
            .fixedSize(horizontal: false, vertical: true)
        }
    }

    @ViewBuilder
    private var feedback: some View {
        if let error = model.rescueError {
            Label(error, systemImage: "exclamationmark.triangle.fill")
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.warning)
                .fixedSize(horizontal: false, vertical: true)
                .padding(9)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(VPTheme.warning.opacity(0.08), in: RoundedRectangle(cornerRadius: 9))
        }
        if let notice = model.rescueNotice {
            Text(notice)
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.info)
                .fixedSize(horizontal: false, vertical: true)
                .padding(9)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(VPTheme.info.opacity(0.07), in: RoundedRectangle(cornerRadius: 9))
        }
    }

    private var footer: some View {
        HStack(spacing: 8) {
            if model.rescueBusy {
                ProgressView().controlSize(.small)
                Text("Releasing…")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textSecondary)
            } else {
                Text(
                    model.rescueActionBlockReason
                        ?? "One explicit one-shot release · not a top-up or hard-cap bypass"
                )
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textTertiary)
                .lineLimit(2)
            }
            Spacer()
            Button("Cancel") { dismiss() }
                .keyboardShortcut(.cancelAction)
                .disabled(model.rescueBusy)
            Button {
                submit()
            } label: {
                Label("Release reserve", systemImage: "shield.lefthalf.filled")
            }
            .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
            .keyboardShortcut(.defaultAction)
            .disabled(!canSubmit || model.rescueBusy)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 13)
        .background(VPTheme.rail)
    }

    private func submit() {
        Task {
            let granted = await model.grantRescue(reason: reasonDraft)
            if granted {
                dismiss()
            }
        }
    }

    private func metric(_ title: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title.uppercased())
                .font(VPTheme.mono(6.5, weight: .semibold))
                .tracking(0.4)
                .foregroundStyle(VPTheme.textTertiary)
            Text(value)
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textSecondary)
                .lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

// MARK: - R3 allocation insight

/// Read-only Manage Dash card for `session.allocation`. Every feasibility,
/// forecast, envelope, recommendation, metareasoning, and calibration value is
/// daemon-provided. The card has one explicit Refresh action and never applies
/// a recommendation, changes a budget/task envelope/model, or resumes a run.
struct AllocationInsightCard: View {
    @Environment(AppModel.self) private var model

    private var allocation: AllocationPreviewView? {
        model.selectedSessionAllocation
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            header
            if let error = model.sessionAllocationError {
                errorRow(error)
            }
            if let allocation {
                content(allocation)
            } else if model.sessionAllocationBusy {
                HStack(spacing: 6) {
                    ProgressView().controlSize(.mini)
                    Text("Loading the daemon allocation preview…")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textSecondary)
                }
            } else {
                notReported(
                    "No allocation preview is loaded. Refresh to ask the daemon; no feasibility or forecast is inferred locally."
                )
            }
            footer
        }
        .padding(14)
        .background(
            .white.opacity(0.035),
            in: RoundedRectangle(cornerRadius: 14, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .task(id: model.selectedSessionID) {
            guard model.selectedSessionID != nil else { return }
            await model.loadSessionAllocation()
        }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 9) {
            Image(systemName: "chart.bar.doc.horizontal")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(VPTheme.accent)
                .frame(width: 18)
            VStack(alignment: .leading, spacing: 2) {
                SectionLabel(title: "Allocation insight")
                Text(model.selectedSession?.goal ?? "No session selected")
                    .font(.system(size: 13, weight: .semibold, design: .rounded))
                    .lineLimit(1)
            }
            Spacer(minLength: 4)
            Text("READ-ONLY · ADVISORY")
                .font(VPTheme.mono(6.5, weight: .semibold))
                .tracking(0.4)
                .foregroundStyle(VPTheme.textTertiary)
                .padding(.horizontal, 6)
                .padding(.vertical, 2)
                .background(VPTheme.textTertiary.opacity(0.10), in: Capsule())
            if model.sessionAllocationBusy {
                ProgressView().controlSize(.mini)
            }
            Button {
                Task { await model.refreshSessionAllocation() }
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(model.sessionAllocationBusy || model.selectedSessionID == nil)
        }
    }

    private func content(_ allocation: AllocationPreviewView) -> some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack(spacing: 8) {
                Text(allocation.profileTitle)
                    .font(VPTheme.mono(7.5, weight: .medium))
                    .foregroundStyle(VPTheme.textSecondary)
                Text("readiness \(allocation.readinessLabel)")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
                if allocation.truncated {
                    badge("DAEMON TRUNCATED", tint: VPTheme.warning)
                }
                Spacer(minLength: 4)
            }
            feasibilitySection(allocation.feasibility)
            envelopeSection(allocation.envelope)
            taskSection(allocation)
            recommendationSection(
                allocation.recommendationPreviews,
                overflow: allocation.recommendationOverflow
            )
            metareasoningSection(allocation.metareasoning)
            calibrationSection(allocation.calibration)
        }
    }

    @ViewBuilder
    private func feasibilitySection(_ feasibility: AllocationFeasibilityView?) -> some View {
        sectionTitle("FEASIBILITY")
        if let feasibility {
            HStack(spacing: 7) {
                badge(feasibility.statusLabel, tint: feasibilityTint(feasibility))
                Spacer(minLength: 4)
                Text(feasibility.forecastLabel)
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(2)
            }
            Text(feasibility.reasonLabel)
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
            if let coverage = feasibility.estimateCoverageLabel {
                Text(coverage)
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            if let pressure = feasibility.pressureLabel {
                Text("reserve pressure: \(pressure)")
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.warning)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let warning = feasibility.criticalPathWarning {
                Text(warning)
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.warning)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack(spacing: 10) {
                Text(feasibility.reserveHeadroomLabel)
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
                if !feasibility.criticalPathPreviews.isEmpty {
                    Text("critical path: \(criticalPathLabel(feasibility))")
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                        .lineLimit(1)
                }
            }
        } else {
            notReported("The daemon returned no feasibility result; no local estimate is invented.")
        }
    }

    private func criticalPathLabel(_ feasibility: AllocationFeasibilityView) -> String {
        var text = feasibility.criticalPathPreviews.joined(separator: ", ")
        if feasibility.criticalPathOverflow > 0 {
            text += " +\(feasibility.criticalPathOverflow)"
        }
        return text
    }

    @ViewBuilder
    private func envelopeSection(_ envelope: AllocationEnvelopeView?) -> some View {
        sectionTitle("ENVELOPE")
        if let envelope {
            HStack(alignment: .top, spacing: 14) {
                metric("Remaining", envelope.remainingLabel)
                metric("Budget", envelope.budgetLabel)
                metric("Spent", envelope.spentLabel)
                metric("Reserve", envelope.reservePercentLabel)
            }
        } else {
            notReported("The daemon returned no session envelope.")
        }
    }

    @ViewBuilder
    private func taskSection(_ allocation: AllocationPreviewView) -> some View {
        sectionTitle("TASK ENVELOPES (\(allocation.tasks.count))")
        if allocation.tasks.isEmpty {
            notReported("No task envelopes were returned.")
        } else {
            VStack(alignment: .leading, spacing: 7) {
                ForEach(Array(allocation.taskPreviews.enumerated()), id: \.offset) { _, task in
                    taskRow(task)
                }
                if allocation.taskOverflow > 0 {
                    Text("… \(allocation.taskOverflow) more task envelopes")
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
        }
    }

    private func taskRow(_ task: AllocationTaskEnvelopeView) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                Text(task.taskIDLabel)
                    .font(VPTheme.mono(6.5, weight: .semibold))
                    .foregroundStyle(VPTheme.accent)
                Text(task.titleLabel)
                    .font(VPTheme.mono(8, weight: .medium))
                    .foregroundStyle(VPTheme.textPrimary)
                    .lineLimit(1)
                Spacer(minLength: 4)
                if task.criticalPath == true {
                    badge("CRITICAL PATH", tint: VPTheme.warning)
                }
                if task.envelopeReallocated {
                    badge("RUNTIME ENVELOPE", tint: VPTheme.warning)
                }
                if let ready = task.readyLabel {
                    badge(ready, tint: task.ready == true ? VPTheme.success : VPTheme.textTertiary)
                }
                if let feasible = task.feasibleLabel {
                    badge(feasible, tint: task.feasible == true ? VPTheme.success : VPTheme.danger)
                }
            }
            HStack(spacing: 10) {
                Text(task.departmentLabel)
                Text(task.statusLabel)
                if let priority = task.priority {
                    Text("priority \(priority)")
                }
                Text(task.budgetLabel)
                if let effective = task.effectiveBudgetLabel {
                    Text(effective)
                        .foregroundStyle(VPTheme.warning)
                }
                if let source = task.sourceLabel {
                    Text(source)
                }
                Spacer(minLength: 4)
                Text(task.forecastLabel)
            }
            .font(VPTheme.mono(6.5))
            .foregroundStyle(VPTheme.textTertiary)
            .lineLimit(1)
            if let reason = task.reasonLabel {
                Text(reason)
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(8)
        .background(VPTheme.surfaceSunken.opacity(0.45), in: RoundedRectangle(cornerRadius: 8))
    }

    @ViewBuilder
    private func recommendationSection(
        _ recommendations: [AllocationRecommendationView],
        overflow: Int,
        title: String = "RECOMMENDATIONS"
    ) -> some View {
        sectionTitle(title)
        if recommendations.isEmpty {
            notReported("No advisory recommendations were returned.")
        } else {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(Array(recommendations.enumerated()), id: \.offset) { _, recommendation in
                    VStack(alignment: .leading, spacing: 2) {
                        HStack(spacing: 6) {
                            Text(recommendation.actionLabel)
                                .font(VPTheme.mono(7.5, weight: .medium))
                                .foregroundStyle(VPTheme.textPrimary)
                            badge(
                                recommendation.confidenceLabel,
                                tint: confidenceTint(recommendation.confidenceLabel)
                            )
                            if let taskID = recommendation.taskIDLabel {
                                Text(taskID)
                                    .font(VPTheme.mono(6.5))
                                    .foregroundStyle(VPTheme.accent)
                            }
                            Spacer(minLength: 4)
                        }
                        Text(recommendation.reasonLabel)
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.textSecondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                if overflow > 0 {
                    Text("… \(overflow) more recommendations")
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
        }
    }

    @ViewBuilder
    private func metareasoningSection(_ metareasoning: AllocationMetareasoningView?) -> some View {
        sectionTitle("METAREASONING")
        if let metareasoning, !metareasoning.isEmpty {
            HStack(spacing: 7) {
                badge(metareasoning.diagnosisStatusLabel, tint: VPTheme.textTertiary)
                if let target = metareasoning.diagnosisTargetLabel {
                    Text("target \(target)")
                        .font(VPTheme.mono(6.5))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                if let noProgress = metareasoning.noProgressLabel {
                    Text(noProgress)
                        .font(VPTheme.mono(6.5))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                Spacer(minLength: 4)
            }
            if let reason = metareasoning.diagnosisReasonLabel {
                Text(reason)
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            taskListRow("stalled", metareasoning.stalledPreviews, overflow: metareasoning.stalledOverflow)
            taskListRow("failed", metareasoning.failedPreviews, overflow: metareasoning.failedOverflow)
            taskListRow(
                "retrying",
                metareasoning.retryingPreviews,
                overflow: metareasoning.retryingOverflow
            )
            if !metareasoning.recommendationPreviews.isEmpty {
                recommendationSection(
                    metareasoning.recommendationPreviews,
                    overflow: metareasoning.recommendationOverflow,
                    title: "META RECOMMENDATIONS"
                )
            }
        } else if metareasoning != nil {
            notReported("The daemon returned an empty metareasoning summary.")
        } else {
            notReported("The daemon returned no metareasoning summary.")
        }
    }

    @ViewBuilder
    private func taskListRow(_ title: String, _ values: [String], overflow: Int) -> some View {
        if !values.isEmpty {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(title.uppercased())
                    .font(VPTheme.mono(6, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                    .frame(width: 52, alignment: .leading)
                Text(values.joined(separator: ", "))
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textSecondary)
                    .lineLimit(1)
                if overflow > 0 {
                    Text("+\(overflow)")
                        .font(VPTheme.mono(6.5))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
        }
    }

    @ViewBuilder
    private func calibrationSection(_ calibration: AllocationCalibrationView?) -> some View {
        sectionTitle("ATTENTION & TRUST CALIBRATION")
        if let calibration, !calibration.isEmpty {
            HStack(alignment: .top, spacing: 8) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("ATTENTION")
                        .font(VPTheme.mono(6, weight: .semibold))
                        .foregroundStyle(VPTheme.textTertiary)
                    Text(calibration.attentionLabel)
                        .font(VPTheme.mono(7.5))
                        .foregroundStyle(VPTheme.textSecondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                VStack(alignment: .leading, spacing: 2) {
                    Text("TRUST")
                        .font(VPTheme.mono(6, weight: .semibold))
                        .foregroundStyle(VPTheme.textTertiary)
                    Text(calibration.trustLabel)
                        .font(VPTheme.mono(7.5))
                        .foregroundStyle(VPTheme.textSecondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                badge(
                    calibration.confidenceLabel,
                    tint: confidenceTint(calibration.confidenceLabel)
                )
            }
            if let reason = calibration.reasonLabel {
                Text(reason)
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let counts = calibration.countLabel {
                Text(counts)
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.textTertiary)
            }
        } else if calibration != nil {
            notReported("The daemon returned an empty calibration summary.")
        } else {
            notReported("The daemon returned no attention/trust calibration.")
        }
    }

    private var footer: some View {
        Text(
            "Daemon estimates and advisory recommendations only. This card does not add budget, change a task envelope or model, alter routing, or bypass approvals, Verifier, or Integrator."
        )
        .font(VPTheme.mono(6.5))
        .foregroundStyle(VPTheme.textTertiary)
        .fixedSize(horizontal: false, vertical: true)
    }

    private func sectionTitle(_ title: String) -> some View {
        Text(title)
            .font(VPTheme.mono(6.5, weight: .semibold))
            .tracking(0.5)
            .foregroundStyle(VPTheme.textTertiary)
            .padding(.top, 2)
    }

    private func badge(_ title: String, tint: Color) -> some View {
        Text(title)
            .font(VPTheme.mono(6, weight: .semibold))
            .tracking(0.3)
            .foregroundStyle(tint)
            .lineLimit(1)
            .padding(.horizontal, 5)
            .padding(.vertical, 1)
            .background(tint.opacity(0.10), in: Capsule())
    }

    private func metric(_ title: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title.uppercased())
                .font(VPTheme.mono(6, weight: .semibold))
                .foregroundStyle(VPTheme.textTertiary)
            Text(value)
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textSecondary)
                .lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func notReported(_ text: String) -> some View {
        Text(text)
            .font(VPTheme.mono(7.5))
            .foregroundStyle(VPTheme.textTertiary)
            .fixedSize(horizontal: false, vertical: true)
    }

    private func errorRow(_ message: String) -> some View {
        Label(String(message.prefix(280)), systemImage: "exclamationmark.triangle.fill")
            .font(VPTheme.mono(7.5))
            .foregroundStyle(VPTheme.warning)
            .lineLimit(2)
    }

    private func feasibilityTint(_ feasibility: AllocationFeasibilityView) -> Color {
        guard let feasible = feasibility.feasible else { return VPTheme.textTertiary }
        guard feasible else { return VPTheme.danger }
        // `feasible` only means no known hard-budget violation. Reasons such as
        // `cost_unconfigured`, `token_unestimated`, or `unbounded_envelope` are
        // caveats, not a green guarantee.
        let reason = feasibility.reason.trimmingCharacters(in: .whitespacesAndNewlines)
        switch reason {
        case "", "feasible", "no_remaining_tasks":
            return VPTheme.success
        default:
            return VPTheme.warning
        }
    }

    private func confidenceTint(_ label: String) -> Color {
        switch label {
        case "HIGH": VPTheme.success
        case "MEDIUM": VPTheme.warning
        case "LOW": VPTheme.danger
        default: VPTheme.textTertiary
        }
    }
}

// MARK: - R4 trade-offs and quality

/// Read-only Manage Dash card for `session.tradeoffs`. It shows every profile
/// returned by the daemon, the delivery/quality review, and calibrated
/// confidence. It is advisory: no profile is selected, no routing changes, and
/// no completion/quality claim is derived locally.
struct TradeoffQualityCard: View {
    @Environment(AppModel.self) private var model

    private var tradeoffs: TradeoffView? {
        model.selectedSessionTradeoffs
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            header
            if let error = model.sessionTradeoffsError {
                errorRow(error)
            }
            if let tradeoffs {
                content(tradeoffs)
            } else if model.sessionTradeoffsBusy {
                HStack(spacing: 6) {
                    ProgressView().controlSize(.mini)
                    Text("Loading the daemon trade-off view…")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textSecondary)
                }
            } else {
                notReported(
                    "No trade-off view is loaded. Refresh to ask the daemon; no quality or confidence claim is inferred locally."
                )
            }
            footer
        }
        .padding(14)
        .background(
            .white.opacity(0.035),
            in: RoundedRectangle(cornerRadius: 14, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .task(id: model.selectedSessionID) {
            guard model.selectedSessionID != nil else { return }
            await model.loadSessionTradeoffs()
        }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 9) {
            Image(systemName: "arrow.left.arrow.right.circle")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(VPTheme.accent)
                .frame(width: 18)
            VStack(alignment: .leading, spacing: 2) {
                SectionLabel(title: "Trade-offs & quality")
                Text(model.selectedSession?.goal ?? "No session selected")
                    .font(.system(size: 13, weight: .semibold, design: .rounded))
                    .lineLimit(1)
            }
            Spacer(minLength: 4)
            Text("ADVISORY")
                .font(VPTheme.mono(6.5, weight: .semibold))
                .tracking(0.4)
                .foregroundStyle(VPTheme.textTertiary)
                .padding(.horizontal, 6)
                .padding(.vertical, 2)
                .background(VPTheme.textTertiary.opacity(0.10), in: Capsule())
            if model.sessionTradeoffsBusy {
                ProgressView().controlSize(.mini)
            }
            Button {
                Task { await model.refreshSessionTradeoffs() }
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(model.sessionTradeoffsBusy || model.selectedSessionID == nil)
        }
    }

    private func content(_ tradeoffs: TradeoffView) -> some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack(spacing: 8) {
                Text("current profile")
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.textTertiary)
                Text(tradeoffs.currentProfileTitle)
                    .font(VPTheme.mono(7.5, weight: .medium))
                    .foregroundStyle(VPTheme.textSecondary)
                Spacer(minLength: 4)
            }
            Text(tradeoffs.activeReserveSummary)
                .font(VPTheme.mono(6.5))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
            profileSection(tradeoffs)
            qualitySection(tradeoffs.quality)
            confidenceSection(tradeoffs.confidence)
            recommendationSection(
                "RECOMMENDATIONS",
                tradeoffs.recommendationPreviews,
                overflow: tradeoffs.recommendationOverflow,
                tint: VPTheme.accent
            )
            recommendationSection(
                "WARNINGS",
                tradeoffs.warningPreviews,
                overflow: tradeoffs.warningOverflow,
                tint: VPTheme.warning
            )
        }
    }

    @ViewBuilder
    private func profileSection(_ tradeoffs: TradeoffView) -> some View {
        sectionTitle("PROFILES (\(tradeoffs.profiles.count))")
        if tradeoffs.profiles.isEmpty {
            notReported("The daemon returned no profile frontier.")
        } else {
            VStack(alignment: .leading, spacing: 7) {
                ForEach(Array(tradeoffs.profilePreviews.enumerated()), id: \.offset) { _, profile in
                    profileRow(profile, currentProfile: tradeoffs.currentProfile)
                }
                if tradeoffs.profileOverflow > 0 {
                    Text("… \(tradeoffs.profileOverflow) more profiles")
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
        }
    }

    private func profileRow(_ profile: ProfileTradeoffView, currentProfile: String) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                Text(profile.profileTitle)
                    .font(VPTheme.mono(8, weight: .medium))
                    .foregroundStyle(VPTheme.textPrimary)
                if profile.isCurrent(currentProfile) {
                    badge("CURRENT", tint: VPTheme.accent)
                }
                if profile.showsRecommendedBadge(currentProfile: currentProfile) {
                    badge("RECOMMENDED", tint: VPTheme.success)
                }
                Spacer(minLength: 4)
                Text(profile.reservePercentLabel)
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            Text(profile.primaryGoalLabel)
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 10) {
                Text("attention \(profile.attentionLabel)")
                Text("quality \(profile.qualityLabel)")
                Spacer(minLength: 4)
                Text(profile.forecastLabel)
            }
            .font(VPTheme.mono(6.5))
            .foregroundStyle(VPTheme.textTertiary)
            .lineLimit(1)
        }
        .padding(8)
        .background(VPTheme.surfaceSunken.opacity(0.45), in: RoundedRectangle(cornerRadius: 8))
    }

    @ViewBuilder
    private func qualitySection(_ quality: QualityReviewView?) -> some View {
        sectionTitle("QUALITY REVIEW")
        if let quality, !quality.isEmpty {
            HStack(spacing: 6) {
                badge(quality.readinessLabel, tint: readinessTint(quality.readinessLabel))
                badge(quality.reviewabilityLabel, tint: reviewabilityTint(quality.reviewabilityLabel))
                Spacer(minLength: 4)
                Text(quality.taskProgressLabel)
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            HStack(spacing: 12) {
                metric("Commit", quality.commitLabel ?? "not reported")
                metric("Latest test", quality.testLabel ?? "not reported")
                metric("Verifier", quality.verifierLabel ?? "not reported")
                metric("Merge", quality.mergeLabel ?? "not reported")
            }
            if let merged = quality.mergedEntriesLabel {
                Text(merged)
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            qualityList("changed paths", quality.changedPathPreviews, overflow: quality.changedPathOverflow)
            qualityList("blockers", quality.blockerPreviews, overflow: quality.blockerOverflow)
            qualityList("reasons", quality.reasonPreviews, overflow: quality.reasonOverflow)
        } else if quality != nil {
            notReported("The daemon returned an empty quality review.")
        } else {
            notReported("The daemon returned no quality review.")
        }
    }

    @ViewBuilder
    private func qualityList(_ title: String, _ values: [String], overflow: Int) -> some View {
        if !values.isEmpty {
            VStack(alignment: .leading, spacing: 1) {
                Text(title.uppercased())
                    .font(VPTheme.mono(6, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                ForEach(Array(values.enumerated()), id: \.offset) { _, value in
                    Text("• \(value)")
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textSecondary)
                        .lineLimit(1)
                }
                if overflow > 0 {
                    Text("… \(overflow) more")
                        .font(VPTheme.mono(6.5))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
        }
    }

    @ViewBuilder
    private func confidenceSection(_ confidence: TradeoffConfidenceView?) -> some View {
        sectionTitle("CALIBRATED CONFIDENCE")
        if let confidence {
            HStack(spacing: 7) {
                badge(confidence.levelLabel, tint: confidenceTint(confidence.levelLabel))
                Text(confidence.scoreLabel)
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textSecondary)
                if let sample = confidence.sampleLabel {
                    Text(sample)
                        .font(VPTheme.mono(6.5))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                Spacer(minLength: 4)
            }
            qualityList("reasons", confidence.reasonPreviews, overflow: confidence.reasonOverflow)
        } else {
            notReported("The daemon returned no calibrated confidence.")
        }
    }

    @ViewBuilder
    private func recommendationSection(
        _ title: String,
        _ values: [String],
        overflow: Int,
        tint: Color
    ) -> some View {
        if !values.isEmpty {
            sectionTitle(title)
            VStack(alignment: .leading, spacing: 2) {
                ForEach(Array(values.enumerated()), id: \.offset) { _, value in
                    HStack(alignment: .firstTextBaseline, spacing: 5) {
                        Image(systemName: "circle.fill")
                            .font(.system(size: 4))
                            .foregroundStyle(tint)
                        Text(value)
                            .font(VPTheme.mono(7.5))
                            .foregroundStyle(VPTheme.textSecondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                if overflow > 0 {
                    Text("… \(overflow) more")
                        .font(VPTheme.mono(6.5))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
        }
    }

    private var footer: some View {
        Text(
            "Advisory only. Profiles are compared but none is selected; confidence and quality are daemon-reported and do not change routing, approvals, verification, or merge."
        )
        .font(VPTheme.mono(6.5))
        .foregroundStyle(VPTheme.textTertiary)
        .fixedSize(horizontal: false, vertical: true)
    }

    private func sectionTitle(_ title: String) -> some View {
        Text(title)
            .font(VPTheme.mono(6.5, weight: .semibold))
            .tracking(0.5)
            .foregroundStyle(VPTheme.textTertiary)
            .padding(.top, 2)
    }

    private func badge(_ title: String, tint: Color) -> some View {
        Text(title)
            .font(VPTheme.mono(6, weight: .semibold))
            .tracking(0.3)
            .foregroundStyle(tint)
            .lineLimit(1)
            .padding(.horizontal, 5)
            .padding(.vertical, 1)
            .background(tint.opacity(0.10), in: Capsule())
    }

    private func metric(_ title: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title.uppercased())
                .font(VPTheme.mono(6, weight: .semibold))
                .foregroundStyle(VPTheme.textTertiary)
            Text(value)
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textSecondary)
                .lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func notReported(_ text: String) -> some View {
        Text(text)
            .font(VPTheme.mono(7.5))
            .foregroundStyle(VPTheme.textTertiary)
            .fixedSize(horizontal: false, vertical: true)
    }

    private func errorRow(_ message: String) -> some View {
        Label(String(message.prefix(280)), systemImage: "exclamationmark.triangle.fill")
            .font(VPTheme.mono(7.5))
            .foregroundStyle(VPTheme.warning)
            .lineLimit(2)
    }

    private func readinessTint(_ label: String) -> Color {
        switch label {
        case "MERGED", "VERIFIED", "TESTS PASSED", "COMMITTED": VPTheme.success
        case "PARTIAL", "DIRTY": VPTheme.warning
        case "BLOCKED": VPTheme.danger
        default: VPTheme.textTertiary
        }
    }

    private func reviewabilityTint(_ label: String) -> Color {
        switch label {
        case "OK": VPTheme.success
        case "WATCH": VPTheme.warning
        case "RISK": VPTheme.danger
        default: VPTheme.textTertiary
        }
    }

    private func confidenceTint(_ label: String) -> Color {
        switch label {
        case "HIGH": VPTheme.success
        case "MEDIUM": VPTheme.warning
        case "LOW": VPTheme.danger
        default: VPTheme.textTertiary
        }
    }
}
