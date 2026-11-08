import SwiftUI

/// A compact tool-call line. Tool calls are operational metadata, not content:
/// they stay one line until the reader asks for details.
struct ToolTraceRow: View {
    let entry: TranscriptToolEntry

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var expanded = false

    private var tint: Color {
        if entry.isError {
            return VPTheme.danger
        }
        return ToolTrace.isHostSensitive(name: entry.name, input: entry.input)
            ? VPTheme.warning
            : VPTheme.info
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Button {
                withAnimation(VPMotion.resolved(VPMotion.snap, reduceMotion: reduceMotion)) {
                    expanded.toggle()
                }
            } label: {
                HStack(alignment: .firstTextBaseline, spacing: 7) {
                    Image(systemName: ToolTrace.icon(name: entry.name, isError: entry.isError))
                        .font(.system(size: 9, weight: .semibold))
                        .foregroundStyle(tint)
                        .frame(width: 12)

                    Text(ToolTrace.displayName(entry.name))
                        .font(VPTheme.mono(9.5, weight: .semibold))
                        .foregroundStyle(VPTheme.textPrimary)
                        .lineLimit(1)

                    if !entry.summary.isEmpty {
                        Text(entry.summary)
                            .font(VPTheme.mono(9))
                            .foregroundStyle(VPTheme.textTertiary)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }

                    if let agent = entry.agent {
                        Text(agent)
                            .font(VPTheme.mono(7.5))
                            .foregroundStyle(VPTheme.textTertiary.opacity(0.8))
                            .lineLimit(1)
                    }

                    Spacer(minLength: 6)

                    if entry.isRunning {
                        ProgressView()
                            .controlSize(.mini)
                            .scaleEffect(0.7)
                    } else if let duration = entry.duration {
                        Text(durationLabel(duration))
                            .font(VPTheme.mono(7.5))
                            .foregroundStyle(VPTheme.textTertiary)
                            .contentTransition(.numericText())
                    }

                    Image(systemName: "chevron.right")
                        .font(.system(size: 7, weight: .bold))
                        .foregroundStyle(VPTheme.textTertiary)
                        .rotationEffect(.degrees(expanded ? 90 : 0))
                        .frame(width: 10)
                }
                .padding(.vertical, 3.5)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help(expanded ? "Hide tool details" : "Show tool details")

            if entry.isError, !expanded {
                Text(entry.output)
                    .font(VPTheme.mono(8.5))
                    .foregroundStyle(VPTheme.danger.opacity(0.82))
                    .lineLimit(2)
                    .textSelection(.enabled)
                    .padding(.leading, 19)
                    .padding(.bottom, 3)
            }

            if expanded {
                VStack(alignment: .leading, spacing: 6) {
                    if entry.input != .null {
                        detailLabel("INPUT")
                        Text(entry.input.prettyString())
                            .font(VPTheme.mono(8.5))
                            .foregroundStyle(VPTheme.textTertiary)
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    detailLabel("OUTPUT")
                    Text(entry.output.isEmpty ? "(no output)" : entry.output)
                        .font(VPTheme.mono(8.5))
                        .foregroundStyle(entry.isError ? VPTheme.danger : VPTheme.textSecondary)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .padding(.leading, 19)
                .padding(.top, 3)
                .padding(.bottom, 6)
                .transition(.vpContent)
            }
        }
        .animation(
            VPMotion.resolved(VPMotion.snap, reduceMotion: reduceMotion),
            value: expanded
        )
    }

    private func detailLabel(_ text: String) -> some View {
        Text(text)
            .font(VPTheme.mono(7, weight: .semibold))
            .tracking(0.8)
            .foregroundStyle(VPTheme.textTertiary)
    }

    private func durationLabel(_ duration: TimeInterval) -> String {
        if duration < 1 {
            return "\(Int((duration * 1000).rounded()))ms"
        }
        return String(format: "%.1fs", duration)
    }
}

/// A rare, high-signal interruption. This is intentionally flatter than a card:
/// the workspace should look calm even when it is asking for a decision.
struct ApprovalNotice: View {
    let entry: TranscriptApprovalEntry

    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var pending = false
    @State private var showingDetails = false
    @State private var errorMessage: String?

    private var title: String {
        if entry.isPending {
            return L10n.t("Permission required")
        }
        return entry.isDenied
            ? L10n.t("Permission denied")
            : L10n.t("Permission granted")
    }

    private var statusColor: Color {
        if entry.isPending {
            return VPTheme.warning
        }
        return entry.isDenied ? VPTheme.danger : VPTheme.success
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 7) {
                Image(systemName: entry.isPending ? "lock.shield" : statusIcon)
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(statusColor)

                Text(title)
                    .font(VPTheme.font(11, weight: .semibold))
                    .foregroundStyle(VPTheme.textPrimary)

                if let risk = entry.approval.approvalRisk, !risk.isEmpty {
                    Text(risk.uppercased())
                        .font(VPTheme.mono(7, weight: .semibold))
                        .tracking(0.6)
                        .foregroundStyle(statusColor)
                        .padding(.horizontal, 5)
                        .padding(.vertical, 2)
                        .background(statusColor.opacity(0.10), in: Capsule())
                }

                Spacer(minLength: 6)

                Text(entry.status.uppercased())
                    .font(VPTheme.mono(7, weight: .semibold))
                    .tracking(0.7)
                    .foregroundStyle(statusColor)
            }

            HStack(spacing: 5) {
                Text(entry.approval.tool)
                    .foregroundStyle(VPTheme.textSecondary)
                if let agent = entry.approval.agentTitle ?? entry.approval.agentName {
                    Text("·")
                    Text(agent)
                        .foregroundStyle(VPTheme.textTertiary)
                }
                if !entry.summary.isEmpty {
                    Text("·")
                    Text(entry.summary)
                        .foregroundStyle(VPTheme.textTertiary)
                        .lineLimit(2)
                        .truncationMode(.middle)
                }
            }
            .font(VPTheme.mono(8))

            if let reason = entry.approval.approvalReason, !reason.isEmpty {
                Text(reason)
                    .font(VPTheme.mono(8))
                    .foregroundStyle(statusColor.opacity(0.9))
                    .fixedSize(horizontal: false, vertical: true)
            }

            if let detail = entry.approval.input.objectValue, !detail.isEmpty {
                Button(showingDetails ? L10n.t("Hide details") : L10n.t("Details")) {
                    withAnimation(VPMotion.resolved(VPMotion.snap, reduceMotion: reduceMotion)) {
                        showingDetails.toggle()
                    }
                }
                .buttonStyle(.plain)
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)

                if showingDetails {
                    Text(entry.approval.input.prettyString())
                        .font(VPTheme.mono(8.5))
                        .foregroundStyle(VPTheme.textTertiary)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .transition(.vpContent)
                }
            }

            if entry.isPending {
                HStack(spacing: 7) {
                    Button(L10n.t("Deny")) {
                        pending = true
                        errorMessage = nil
                        Task {
                            if await !(model.resolveApproval(entry.approval.id, approved: false)) {
                                pending = false
                                errorMessage = L10n.t("Approval could not be resolved.")
                            }
                        }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)

                    Button(L10n.t("Approve")) {
                        pending = true
                        errorMessage = nil
                        Task {
                            if await !(model.resolveApproval(entry.approval.id, approved: true)) {
                                pending = false
                                errorMessage = L10n.t("Approval could not be resolved.")
                            }
                        }
                    }
                    .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                    .controlSize(.small)

                    if pending {
                        ProgressView()
                            .controlSize(.small)
                            .transition(.vpScaleFade)
                    }

                    Spacer()
                }
                .disabled(pending)
            }

            if let errorMessage {
                Text(errorMessage)
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.danger)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(.leading, 10)
        .padding(.vertical, 7)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            entry.isPending ? VPTheme.warning.opacity(0.045) : Color.clear
        )
        .overlay(alignment: .leading) {
            Rectangle()
                .fill(statusColor.opacity(entry.isPending ? 0.65 : 0.35))
                .frame(width: 2)
        }
        .animation(
            VPMotion.resolved(VPMotion.snap, reduceMotion: reduceMotion),
            value: entry.status
        )
        .onChange(of: entry.status) { _, _ in
            pending = false
            errorMessage = nil
        }
    }

    private var statusIcon: String {
        entry.isDenied ? "xmark.shield" : "checkmark.shield"
    }
}
