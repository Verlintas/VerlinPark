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

    private var readiness: ReadinessSnapshotView? {
        resources.readiness
    }

    private var attention: ResourceAttentionView {
        resources.attention
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            header
            if let pause = model.resourcePause {
                resourcePauseBanner(pause)
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
