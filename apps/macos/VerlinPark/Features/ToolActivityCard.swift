import SwiftUI

/// Bounded recent tool activity for the selected session. Every row comes from
/// `session.tool_activity` (a projection of `tool.finished` events); the view
/// never derives success from transcript text and never shows provider output.
struct ToolActivityCard: View {
    @Environment(AppModel.self) private var model

    private var activity: ToolActivityResult? {
        model.toolActivity
    }

    private var rows: [ToolActivityEntry] {
        ToolActivityMetrics.dashboardRows(activity)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            header
            if let activity {
                summary(activity)
                if !activity.summary.errorCodes.isEmpty {
                    codeBreakdown(activity.summary.errorCodes)
                }
                if rows.isEmpty {
                    Text("No tool calls recorded for this session yet.")
                        .font(VPTheme.mono(9))
                        .foregroundStyle(VPTheme.textTertiary)
                } else {
                    VStack(spacing: 0) {
                        ForEach(Array(rows.enumerated()), id: \.element.id) { index, entry in
                            row(entry)
                            if index < rows.count - 1 {
                                VPHairline()
                            }
                        }
                    }
                    if activity.truncated {
                        Text(
                            "Showing the newest \(rows.count) of \(activity.total) tool calls. Use `vp tool-activity` for the full bounded window."
                        )
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                    }
                }
            } else if let status = model.toolActivityStatus {
                Text(status)
                    .font(VPTheme.mono(9))
                    .foregroundStyle(VPTheme.textTertiary)
            } else if model.selectedSession != nil {
                ProgressView()
                    .controlSize(.small)
            } else {
                Text("Select a session to inspect per-tool latency and failures.")
                    .font(VPTheme.mono(9))
                    .foregroundStyle(VPTheme.textTertiary)
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
                SectionLabel(title: "Tool Activity")
                Text("Per-tool latency, result code, and output shape")
                    .font(VPTheme.font(13, weight: .semibold, rounded: true))
                Text("Projection of tool.finished events · no provider content")
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            Spacer()
            if model.toolActivityBusy {
                ProgressView().controlSize(.small)
            }
            Button("Refresh") {
                Task { await model.loadToolActivity() }
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(model.toolActivityBusy || model.selectedSessionID == nil)
        }
    }

    private func summary(_ activity: ToolActivityResult) -> some View {
        HStack(spacing: 10) {
            metric(
                "Calls",
                "\(activity.summary.calls)",
                detail: "returned window"
            )
            metric(
                "Errors",
                "\(activity.summary.errors)",
                detail: activity.summary.retryableErrors > 0
                    ? "\(activity.summary.retryableErrors) retryable"
                    : "none retryable"
            )
            metric(
                "Latency",
                ToolActivityMetrics.durationLabel(activity.summary.averageDurationMs),
                detail: "avg · max \(ToolActivityMetrics.durationLabel(activity.summary.maxDurationMs))"
            )
            metric(
                "Output",
                ToolActivityMetrics.byteLabel(activity.summary.outputBytes),
                detail: activity.summary.truncatedResults > 0
                    ? "\(activity.summary.truncatedResults) truncated"
                    : "no truncation"
            )
        }
    }

    private func codeBreakdown(_ codes: [ToolActivityCodeStat]) -> some View {
        HStack(spacing: 5) {
            Text("CODES")
                .font(VPTheme.mono(7, weight: .semibold))
                .tracking(0.6)
                .foregroundStyle(VPTheme.textTertiary)
            ForEach(codes.prefix(5)) { entry in
                Text("\(entry.code) ×\(entry.calls)")
                    .font(VPTheme.mono(7, weight: .semibold))
                    .foregroundStyle(VPTheme.danger.opacity(0.85))
                    .padding(.horizontal, 5)
                    .padding(.vertical, 2)
                    .background(VPTheme.danger.opacity(0.08), in: Capsule())
            }
            Spacer(minLength: 0)
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

    private func row(_ entry: ToolActivityEntry) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(alignment: .firstTextBaseline, spacing: 7) {
                Image(systemName: entry.isError ? "exclamationmark.triangle.fill" : "checkmark.circle")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(entry.isError ? VPTheme.danger : VPTheme.success)
                    .frame(width: 12)
                Text(entry.name)
                    .font(VPTheme.mono(9.5, weight: .semibold))
                    .foregroundStyle(VPTheme.textPrimary)
                    .lineLimit(1)
                Text(ToolActivityMetrics.statusLabel(entry))
                    .font(VPTheme.mono(8, weight: .semibold))
                    .foregroundStyle(entry.isError ? VPTheme.danger : VPTheme.textTertiary)
                if let risk = entry.approvalRisk, !risk.isEmpty {
                    Text(risk.uppercased())
                        .font(VPTheme.mono(7, weight: .semibold))
                        .foregroundStyle(VPTheme.warning)
                        .padding(.horizontal, 4)
                        .padding(.vertical, 1)
                        .background(VPTheme.warning.opacity(0.10), in: Capsule())
                }
                Spacer(minLength: 6)
                Text(ToolActivityMetrics.durationLabel(entry.durationMs))
                    .font(VPTheme.mono(8))
                    .monospacedDigit()
                    .foregroundStyle(VPTheme.textTertiary)
                Text(ToolActivityMetrics.byteLabel(entry.outputBytes))
                    .font(VPTheme.mono(8))
                    .monospacedDigit()
                    .foregroundStyle(VPTheme.textTertiary)
            }
            if entry.truncated {
                Text("output was truncated by the tool-output policy")
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
                    .padding(.leading, 19)
            }
            if let hint = entry.hint, entry.isError, !hint.isEmpty {
                Text(hint)
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.danger.opacity(0.82))
                    .lineLimit(2)
                    .padding(.leading, 19)
            }
        }
        .padding(.vertical, 5)
        .help(help(entry))
    }

    private func help(_ entry: ToolActivityEntry) -> String {
        var parts = [
            "\(entry.name) · \(ToolActivityMetrics.durationLabel(entry.durationMs))",
            "output \(ToolActivityMetrics.byteLabel(entry.outputBytes))",
            ToolActivityMetrics.statusLabel(entry),
        ]
        if let retry = ToolActivityMetrics.retryLabel(entry) {
            parts.append(retry)
        }
        if let mode = entry.approvalMode {
            parts.append("approval mode \(mode)")
        }
        if let wait = entry.approvalWaitMs {
            parts.append("approval wait \(ToolActivityMetrics.durationLabel(wait))")
        }
        return parts.joined(separator: " · ")
    }
}
