import SwiftUI

/// Layout choice for `ProgressCard`. Compact keeps the assessment, counts, and
/// next step; expanded adds the latest-round lists, evidence, and earlier rounds.
enum ProgressCardStyle: String, CaseIterable, Hashable, Sendable {
    case compact
    case expanded

    var isExpanded: Bool {
        self == .expanded
    }
}

/// Read-only advisory progress ledger card. It takes immutable models and action
/// closures only; it never talks to the daemon, the scheduler, or AppModel.
struct ProgressCard: View {
    let ledger: ProgressLedger?
    var style: ProgressCardStyle = .expanded
    var title: String?
    var isEnabled: Bool = true
    var isLoading: Bool = false
    var errorMessage: String?
    var onRefresh: (() -> Void)?
    var onReport: (() -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: 11) {
            header
            if let message = errorMessage, !message.isEmpty {
                notice(
                    icon: "exclamationmark.triangle.fill",
                    text: ProgressText.bounded(message, limit: 240),
                    tint: VPTheme.warning
                )
            }
            if !isEnabled {
                stateBlock(
                    icon: "pause.circle",
                    title: ProgressCopy.disabledTitle,
                    detail: ProgressCopy.disabledDetail,
                    tint: VPTheme.textTertiary
                )
            } else if let ledger {
                statusStrip(ledger)
                cardContent(ledger)
            } else if isLoading {
                loadingBlock
            } else {
                stateBlock(
                    icon: "tray",
                    title: ProgressCopy.unavailableTitle,
                    detail: ProgressCopy.unavailableDetail,
                    tint: VPTheme.textTertiary
                )
            }
            if isEnabled {
                footer(ledger)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(14)
        .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .vpAppear(index: 0)
    }

    // MARK: Header

    private var header: some View {
        HStack(alignment: .top, spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                SectionLabel(title: "Progress ledger")
                Text(displayTitle)
                    .font(VPTheme.font(13, weight: .semibold, rounded: true))
                    .foregroundStyle(VPTheme.textPrimary)
                    .lineLimit(1)
                Text(displaySubtitle)
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(1)
            }
            Spacer(minLength: 8)
            if isLoading {
                ProgressView()
                    .controlSize(.small)
            }
            if let onReport {
                Button("Report") {
                    onReport()
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(isLoading || !isEnabled)
                .help("Record one bounded Manager progress round. The result is advisory evidence.")
            }
            if let onRefresh {
                Button {
                    onRefresh()
                } label: {
                    Image(systemName: "arrow.clockwise")
                        .font(.system(size: 10, weight: .semibold))
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(isLoading)
                .help("Refresh the advisory progress ledger")
                .accessibilityLabel("Refresh progress ledger")
            }
        }
    }

    private var displayTitle: String {
        if let title {
            let trimmed = title.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty {
                return ProgressText.bounded(trimmed, limit: 120)
            }
        }
        if let ledger, !ledger.sessionID.isEmpty {
            return "Session \(String(ledger.sessionID.prefix(14)))"
        }
        return "Session progress"
    }

    private var displaySubtitle: String {
        guard let ledger else {
            return "Advisory Manager evidence · read-only"
        }
        return "Advisory Manager evidence · \(ledger.planRevisionLabel) · \(ledger.roundCountLabel)"
    }

    // MARK: Status

    private func statusStrip(_ ledger: ProgressLedger) -> some View {
        HStack(spacing: 6) {
            ProgressStatusPill(status: ledger.assessment.status)
            ProgressAdvisoryBadge()
            if ledger.truncated {
                compactBadge(ProgressCopy.truncatedBadge, tint: VPTheme.warning)
            }
            Spacer(minLength: 6)
            Text(ledger.planRevisionLabel)
                .font(VPTheme.mono(7.5, weight: .semibold))
                .monospacedDigit()
                .foregroundStyle(VPTheme.textTertiary)
            Text("·")
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
            Text(ledger.roundCountLabel)
                .font(VPTheme.mono(7.5))
                .monospacedDigit()
                .foregroundStyle(VPTheme.textTertiary)
        }
    }

    private func compactBadge(_ text: String, tint: Color) -> some View {
        Text(text.uppercased())
            .font(VPTheme.mono(7, weight: .semibold))
            .tracking(0.3)
            .foregroundStyle(tint)
            .padding(.horizontal, 5)
            .padding(.vertical, 2)
            .background(tint.opacity(0.10), in: Capsule())
            .overlay {
                Capsule().strokeBorder(tint.opacity(0.22), lineWidth: 1)
            }
    }

    // MARK: Content

    @ViewBuilder
    private func cardContent(_ ledger: ProgressLedger) -> some View {
        if style.isExpanded, ledger.isAdvisoryOnly {
            advisoryBanner
        }
        assessmentBlock(ledger.assessment)
        metricsGrid(ledger.assessment)
        if ledger.assessment.hasNextStep {
            nextStepBlock(ledger.assessment)
        }
        if let latest = ledger.latestRound {
            latestRoundSection(latest)
        } else {
            stateBlock(
                icon: "tray",
                title: ProgressCopy.emptyRoundsTitle,
                detail: ledger.assessment.hasReason
                    ? ledger.assessment.reason
                    : ProgressCopy.emptyRoundsDetail,
                tint: VPTheme.textTertiary
            )
        }
        if style.isExpanded, ledger.rounds.count > 1 {
            earlierRoundsSection(ledger)
        }
        if ledger.truncated, style.isExpanded {
            Text("Older rounds were truncated by the daemon; only the retained window is shown.")
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.warning)
                .lineLimit(2)
        }
    }

    private var advisoryBanner: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "shield.lefthalf.filled")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(VPTheme.accent)
                .frame(width: 12)
            VStack(alignment: .leading, spacing: 2) {
                Text(ProgressCopy.advisoryTitle.uppercased())
                    .font(VPTheme.mono(7.5, weight: .semibold))
                    .tracking(0.5)
                    .foregroundStyle(VPTheme.accent)
                Text(ProgressCopy.advisoryDetail)
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
            ProgressAdvisoryBadge()
        }
        .padding(9)
        .background(
            VPTheme.accent.opacity(0.055),
            in: RoundedRectangle(cornerRadius: 9, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .strokeBorder(VPTheme.accent.opacity(0.18), lineWidth: 1)
        }
    }

    private func assessmentBlock(_ assessment: ProgressAssessment) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Text(assessment.reasonCodeLabel)
                    .font(VPTheme.mono(7, weight: .semibold))
                    .tracking(0.35)
                    .foregroundStyle(assessment.status.tint)
                    .padding(.horizontal, 5)
                    .padding(.vertical, 2)
                    .background(assessment.status.tint.opacity(0.10), in: Capsule())
                    .overlay {
                        Capsule().strokeBorder(
                            assessment.status.tint.opacity(0.22),
                            lineWidth: 1
                        )
                    }
                if assessment.advisoryOnly {
                    Text(ProgressCopy.advisoryBadgeText.uppercased())
                        .font(VPTheme.mono(7, weight: .semibold))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                Spacer(minLength: 0)
                Text(assessment.status.label)
                    .font(VPTheme.mono(7.5, weight: .semibold))
                    .foregroundStyle(assessment.status.tint)
            }
            Text(
                assessment.hasReason
                    ? assessment.reason
                    : "No assessment reason was supplied."
            )
            .font(VPTheme.font(11))
            .foregroundStyle(assessment.hasReason ? VPTheme.textSecondary : VPTheme.textTertiary)
            .lineLimit(style.isExpanded ? 4 : 2)
            .fixedSize(horizontal: false, vertical: true)
            Text(assessment.status.helpText)
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
                .lineLimit(2)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(9)
        .background(
            assessment.status.tint.opacity(0.045),
            in: RoundedRectangle(cornerRadius: 9, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .strokeBorder(assessment.status.tint.opacity(0.16), lineWidth: 1)
        }
    }

    private func metricsGrid(_ assessment: ProgressAssessment) -> some View {
        LazyVGrid(
            columns: [GridItem(.adaptive(minimum: 104), spacing: 7, alignment: .leading)],
            alignment: .leading,
            spacing: 7
        ) {
            metric(
                "In-loop streak",
                "\(assessment.inLoopStreak)",
                detail: "consecutive in-loop rounds",
                tint: assessment.inLoopStreak > 0 ? VPTheme.warning : VPTheme.textPrimary
            )
            metric(
                "Stalled rounds",
                "\(assessment.stalledRounds)",
                detail: "no-progress rounds",
                tint: assessment.stalledRounds > 0 ? VPTheme.warning : VPTheme.textPrimary
            )
            metric(
                "Satisfied",
                "\(assessment.satisfiedCount)",
                detail: "reported complete items",
                tint: assessment.satisfiedCount > 0 ? VPTheme.success : VPTheme.textPrimary
            )
            metric(
                "Evidence",
                "\(assessment.evidenceCount)",
                detail: "bounded references",
                tint: assessment.evidenceCount > 0 ? VPTheme.accent : VPTheme.textPrimary
            )
        }
    }

    private func metric(
        _ label: String,
        _ value: String,
        detail: String,
        tint: Color
    ) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label.uppercased())
                .font(VPTheme.mono(7, weight: .semibold))
                .tracking(0.3)
                .foregroundStyle(VPTheme.textTertiary)
                .lineLimit(1)
            Text(value)
                .font(VPTheme.mono(12, weight: .semibold))
                .monospacedDigit()
                .foregroundStyle(tint)
            Text(detail)
                .font(VPTheme.mono(6.5))
                .foregroundStyle(VPTheme.textTertiary)
                .lineLimit(1)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(7)
        .background(
            VPTheme.surfaceRaised.opacity(0.5),
            in: RoundedRectangle(cornerRadius: 8, style: .continuous)
        )
    }

    private func nextStepBlock(_ assessment: ProgressAssessment) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            if let nextTask = assessment.nextTask {
                ProgressGuidanceLine(
                    label: "Next task",
                    text: nextTask,
                    icon: "arrow.forward.circle.fill",
                    tint: VPTheme.accent,
                    lineLimit: style.isExpanded ? 3 : 2
                )
            }
            if let instruction = assessment.instruction {
                ProgressGuidanceLine(
                    label: "Instruction",
                    text: instruction,
                    icon: "text.bubble",
                    tint: VPTheme.info,
                    lineLimit: style.isExpanded ? 4 : 2
                )
            }
        }
        .padding(9)
        .background(
            VPTheme.surfaceRaised.opacity(0.5),
            in: RoundedRectangle(cornerRadius: 9, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
    }

    private func latestRoundSection(_ round: ProgressRound) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                SectionLabel(title: "Latest round")
                Spacer(minLength: 0)
                Text(round.agentDisplayName)
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(1)
            }
            ProgressRoundRow(round: round, showsLists: style.isExpanded)
        }
    }

    private func earlierRoundsSection(_ ledger: ProgressLedger) -> some View {
        let earlier = Array(ledger.rounds.dropLast().suffix(2).reversed())
        return VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                SectionLabel(title: "Earlier rounds")
                Spacer(minLength: 0)
                Text("newest first")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            ForEach(Array(earlier.enumerated()), id: \.offset) { _, round in
                ProgressRoundRow(round: round, showsLists: false)
            }
            if ledger.rounds.count > 3 {
                Text("+\(ledger.rounds.count - 3) earlier rounds retained")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
            }
        }
    }

    // MARK: States

    private var loadingBlock: some View {
        HStack(spacing: 8) {
            ProgressView()
                .controlSize(.small)
            VStack(alignment: .leading, spacing: 1) {
                Text(ProgressCopy.loadingTitle)
                    .font(VPTheme.font(11, weight: .semibold))
                    .foregroundStyle(VPTheme.textSecondary)
                Text("Waiting for the advisory assessment.")
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            Spacer(minLength: 0)
        }
        .padding(10)
        .background(
            VPTheme.surfaceSunken.opacity(0.55),
            in: RoundedRectangle(cornerRadius: 10, style: .continuous)
        )
    }

    private func stateBlock(
        icon: String,
        title: String,
        detail: String,
        tint: Color
    ) -> some View {
        HStack(alignment: .top, spacing: 9) {
            Image(systemName: icon)
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(tint)
                .frame(width: 15)
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(VPTheme.font(11.5, weight: .semibold))
                    .foregroundStyle(VPTheme.textSecondary)
                Text(ProgressText.bounded(detail, limit: 320))
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(3)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(10)
        .background(
            VPTheme.surfaceSunken.opacity(0.55),
            in: RoundedRectangle(cornerRadius: 10, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
    }

    private func notice(icon: String, text: String, tint: Color) -> some View {
        HStack(alignment: .top, spacing: 7) {
            Image(systemName: icon)
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(tint)
                .frame(width: 11)
            Text(text)
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textSecondary)
                .lineLimit(3)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .padding(8)
        .background(
            tint.opacity(0.07),
            in: RoundedRectangle(cornerRadius: 8, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .strokeBorder(tint.opacity(0.18), lineWidth: 1)
        }
    }

    private func footer(_ ledger: ProgressLedger?) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 5) {
                Image(systemName: "info.circle")
                    .font(.system(size: 7.5, weight: .semibold))
                Text("Advisory evidence only; no scheduler, merge, or approval action is implied.")
                    .font(VPTheme.mono(7.5))
            }
            .foregroundStyle(VPTheme.textTertiary)
            if let latest = ledger?.latestRound?.timestampLabel {
                Text("Latest round \(latest)")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
            }
        }
    }
}

// MARK: - Status pill

struct ProgressStatusPill: View {
    let status: ProgressStatus
    var compact = false

    var body: some View {
        HStack(spacing: 5) {
            Image(systemName: status.icon)
                .font(.system(size: compact ? 7.5 : 8, weight: .semibold))
            Text(status.label.uppercased())
                .font(VPTheme.mono(compact ? 7 : 7.5, weight: .semibold))
                .tracking(0.4)
                .lineLimit(1)
        }
        .foregroundStyle(status.tint)
        .padding(.horizontal, compact ? 6 : 7)
        .padding(.vertical, 3)
        .background(status.tint.opacity(0.11), in: Capsule())
        .overlay {
            Capsule().strokeBorder(status.tint.opacity(0.26), lineWidth: 1)
        }
        .help(status.helpText)
        .accessibilityLabel("Progress status: \(status.label)")
    }
}

struct ProgressAdvisoryBadge: View {
    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: "lock.shield")
                .font(.system(size: 7, weight: .semibold))
            Text(ProgressCopy.advisoryBadgeText.uppercased())
                .font(VPTheme.mono(7, weight: .semibold))
                .tracking(0.35)
        }
        .foregroundStyle(VPTheme.textTertiary)
        .padding(.horizontal, 6)
        .padding(.vertical, 3)
        .background(VPTheme.surfaceSunken, in: Capsule())
        .overlay {
            Capsule().strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
        .help(
            "The progress ledger is evidence only. It cannot authorize, retry, cancel, merge, or schedule work."
        )
        .accessibilityLabel("Advisory only. Evidence, not authorization.")
    }
}

// MARK: - Evidence chip

struct ProgressEvidenceChip: View {
    let evidence: ProgressEvidence

    private var reference: String {
        guard evidence.hasReference else { return "no reference" }
        return ProgressText.bounded(evidence.reference, limit: 48)
    }

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: evidence.kind.icon)
                .font(.system(size: 7, weight: .semibold))
            Text(evidence.kind.label.uppercased())
                .font(VPTheme.mono(6.5, weight: .semibold))
                .tracking(0.25)
            Text(verbatim: reference)
                .font(VPTheme.mono(7))
                .lineLimit(1)
                .truncationMode(.middle)
        }
        .foregroundStyle(evidence.kind.tint)
        .padding(.horizontal, 6)
        .padding(.vertical, 3)
        .background(evidence.kind.tint.opacity(0.09), in: Capsule())
        .overlay {
            Capsule().strokeBorder(evidence.kind.tint.opacity(0.22), lineWidth: 1)
        }
        .help(helpText)
        .accessibilityLabel(
            "\(evidence.kind.label) evidence: \(evidence.displayReference)"
        )
    }

    private var helpText: String {
        var parts = ["\(evidence.kind.label): \(evidence.displayReference)"]
        if let note = evidence.note, !note.isEmpty {
            parts.append(note)
        }
        parts.append("Evidence is a reference, not file content.")
        return parts.joined(separator: " · ")
    }
}

// MARK: - Round row

struct ProgressRoundRow: View {
    let round: ProgressRound
    var showsLists = true

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .top, spacing: 7) {
                VPAgentAvatar(seed: round.agentSeed, role: "manager", size: 20)
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(round.agentDisplayName)
                            .font(VPTheme.font(10.5, weight: .semibold))
                            .foregroundStyle(VPTheme.textPrimary)
                            .lineLimit(1)
                        if let taskID = round.taskID, !taskID.isEmpty {
                            Text("task \(taskID)")
                                .font(VPTheme.mono(7))
                                .foregroundStyle(VPTheme.textTertiary)
                                .lineLimit(1)
                                .truncationMode(.middle)
                        }
                        Spacer(minLength: 0)
                        if let timestamp = round.timestampLabel {
                            Text(timestamp)
                                .font(VPTheme.mono(7))
                                .foregroundStyle(VPTheme.textTertiary)
                                .lineLimit(1)
                        }
                    }
                    Text(round.countsSummary)
                        .font(VPTheme.mono(7.5))
                        .foregroundStyle(VPTheme.textTertiary)
                        .lineLimit(1)
                }
                Spacer(minLength: 0)
            }
            if showsLists {
                listBlocks
                evidenceFlow
                guidanceBlocks
            } else if !round.evidence.isEmpty {
                Text("\(round.evidence.count) evidence reference(s) retained")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
            }
        }
        .padding(8)
        .background(
            VPTheme.surfaceSunken.opacity(0.4),
            in: RoundedRectangle(cornerRadius: 9, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
    }

    @ViewBuilder
    private var listBlocks: some View {
        if round.listItemCount == 0 {
            Text("No list items in this round.")
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
        } else {
            VStack(alignment: .leading, spacing: 5) {
                ProgressItemList(
                    title: "Satisfied",
                    items: round.satisfied,
                    icon: "checkmark.circle.fill",
                    tint: VPTheme.success
                )
                ProgressItemList(
                    title: "In loop",
                    items: round.inLoop,
                    icon: "arrow.triangle.2.circlepath",
                    tint: VPTheme.warning
                )
                ProgressItemList(
                    title: "Progressing",
                    items: round.progressing,
                    icon: "arrow.up.right.circle.fill",
                    tint: VPTheme.accent
                )
                ProgressItemList(
                    title: "Blockers",
                    items: round.blockers,
                    icon: "hand.raised.fill",
                    tint: VPTheme.danger
                )
            }
        }
    }

    @ViewBuilder
    private var evidenceFlow: some View {
        if !round.evidence.isEmpty {
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text("EVIDENCE")
                        .font(VPTheme.mono(7, weight: .semibold))
                        .tracking(0.3)
                        .foregroundStyle(VPTheme.textTertiary)
                    Spacer(minLength: 0)
                    if round.evidence.count > 4 {
                        Text("+\(round.evidence.count - 4) more")
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                }
                VPFlowLayout(spacing: 5) {
                    ForEach(
                        Array(round.evidence.prefix(4).enumerated()),
                        id: \.offset
                    ) { _, evidence in
                        ProgressEvidenceChip(evidence: evidence)
                    }
                }
            }
        }
    }

    @ViewBuilder
    private var guidanceBlocks: some View {
        if round.nextTask != nil || round.instruction != nil {
            VStack(alignment: .leading, spacing: 4) {
                if let nextTask = round.nextTask {
                    ProgressGuidanceLine(
                        label: "Round next task",
                        text: nextTask,
                        icon: "arrow.forward.circle",
                        tint: VPTheme.accent,
                        lineLimit: 2
                    )
                }
                if let instruction = round.instruction {
                    ProgressGuidanceLine(
                        label: "Round instruction",
                        text: instruction,
                        icon: "text.bubble",
                        tint: VPTheme.info,
                        lineLimit: 2
                    )
                }
            }
        }
    }
}

// MARK: - Shared pieces

private struct ProgressItemList: View {
    let title: String
    let items: [String]
    let icon: String
    let tint: Color
    var limit = 4

    var body: some View {
        if !items.isEmpty {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 5) {
                    Image(systemName: icon)
                        .font(.system(size: 7, weight: .semibold))
                        .foregroundStyle(tint)
                    Text(title.uppercased())
                        .font(VPTheme.mono(6.5, weight: .semibold))
                        .tracking(0.3)
                        .foregroundStyle(VPTheme.textTertiary)
                    Text("\(items.count)")
                        .font(VPTheme.mono(6.5, weight: .semibold))
                        .monospacedDigit()
                        .foregroundStyle(tint.opacity(0.85))
                    Spacer(minLength: 0)
                }
                ForEach(Array(items.prefix(limit).enumerated()), id: \.offset) { _, item in
                    HStack(alignment: .top, spacing: 6) {
                        Circle()
                            .fill(tint.opacity(0.65))
                            .frame(width: 2.5, height: 2.5)
                            .padding(.top, 4.5)
                        Text(verbatim: item)
                            .font(VPTheme.mono(7.5))
                            .foregroundStyle(VPTheme.textSecondary)
                            .lineLimit(2)
                            .fixedSize(horizontal: false, vertical: true)
                        Spacer(minLength: 0)
                    }
                }
                if items.count > limit {
                    Text("+\(items.count - limit) more")
                        .font(VPTheme.mono(6.5))
                        .foregroundStyle(VPTheme.textTertiary)
                        .padding(.leading, 8)
                }
            }
        }
    }
}

private struct ProgressGuidanceLine: View {
    let label: String
    let text: String
    let icon: String
    let tint: Color
    var lineLimit = 3

    var body: some View {
        HStack(alignment: .top, spacing: 7) {
            Image(systemName: icon)
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(tint)
                .frame(width: 11)
            VStack(alignment: .leading, spacing: 1) {
                Text(label.uppercased())
                    .font(VPTheme.mono(6.5, weight: .semibold))
                    .tracking(0.35)
                    .foregroundStyle(VPTheme.textTertiary)
                Text(text)
                    .font(VPTheme.font(10.5))
                    .foregroundStyle(VPTheme.textSecondary)
                    .lineLimit(lineLimit)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
            }
            Spacer(minLength: 0)
        }
    }
}
