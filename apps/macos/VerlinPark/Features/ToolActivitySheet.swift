import SwiftUI

/// Full bounded inspector for the selected session's tool activity. The sheet
/// is read-only: filters and search are local projections of the RPC window, and
/// reloading goes back to the same `session.tool_activity` endpoint.
struct ToolActivitySheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    @State private var filter: ToolActivityFilter = .all
    @State private var search = ""
    @State private var selectedTool: String?
    @State private var expandedID: String?

    private var activity: ToolActivityResult? {
        model.toolActivity
    }

    private var entries: [ToolActivityEntry] {
        ToolActivityMetrics.filtered(
            activity,
            filter: filter,
            search: search,
            tool: selectedTool
        )
    }

    private var filteredSummary: ToolActivitySummary {
        ToolActivityMetrics.summary(for: entries)
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            VPHairline()
            HStack(spacing: 0) {
                sidebar
                VPHairline(vertical: true)
                detailList
            }
        }
        .frame(minWidth: 760, minHeight: 520)
        .background(VPTheme.canvas)
        .task {
            if model.toolActivity == nil {
                await model.loadToolActivity()
            }
        }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 16) {
            VStack(alignment: .leading, spacing: 3) {
                Text("Tool Activity")
                    .font(VPTheme.font(20, weight: .semibold, rounded: true))
                Text(
                    "Bounded newest-first projection of tool.finished. No provider content is shown."
                )
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
            .disabled(model.toolActivityBusy)
            Button("Done") { dismiss() }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 16)
        .background(VPTheme.surface)
    }

    private var sidebar: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("FILTER")
                .font(VPTheme.mono(8, weight: .semibold))
                .tracking(0.8)
                .foregroundStyle(VPTheme.textTertiary)
            Picker("Filter", selection: $filter) {
                ForEach(ToolActivityFilter.allCases) { option in
                    Text(option.label).tag(option)
                }
            }
            .pickerStyle(.segmented)
            .labelsHidden()

            TextField("Tool, code, or hint", text: $search)
                .textFieldStyle(.roundedBorder)
                .font(VPTheme.mono(9))

            VPHairline()
            Text("TOOLS")
                .font(VPTheme.mono(8, weight: .semibold))
                .tracking(0.8)
                .foregroundStyle(VPTheme.textTertiary)
            Button {
                selectedTool = nil
            } label: {
                toolRow("All tools", count: activity?.entries.count ?? 0, selected: selectedTool == nil)
            }
            .buttonStyle(.plain)
            ForEach(ToolActivityMetrics.toolNames(activity), id: \.self) { name in
                Button {
                    selectedTool = selectedTool == name ? nil : name
                } label: {
                    toolRow(
                        name,
                        count: activity?.entries.count(where: { $0.name == name }) ?? 0,
                        selected: selectedTool == name
                    )
                }
                .buttonStyle(.plain)
            }
            Spacer(minLength: 0)
        }
        .padding(16)
        .frame(width: 190, alignment: .topLeading)
        .background(VPTheme.surface.opacity(0.6))
    }

    private func toolRow(_ name: String, count: Int, selected: Bool) -> some View {
        HStack(spacing: 6) {
            Text(name)
                .font(VPTheme.mono(8.5, weight: selected ? .semibold : .regular))
                .foregroundStyle(selected ? VPTheme.accent : VPTheme.textSecondary)
                .lineLimit(1)
            Spacer(minLength: 0)
            Text("\(count)")
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
        }
        .padding(.horizontal, 6)
        .padding(.vertical, 4)
        .background(
            selected ? VPTheme.accent.opacity(0.10) : Color.clear,
            in: RoundedRectangle(cornerRadius: 6)
        )
        .contentShape(Rectangle())
    }

    private var detailList: some View {
        VStack(alignment: .leading, spacing: 10) {
            summaryStrip
            if entries.isEmpty {
                VStack(spacing: 8) {
                    Image(systemName: "wrench.and.screwdriver")
                        .font(.system(size: 22, weight: .light))
                        .foregroundStyle(VPTheme.textTertiary)
                    Text(activity == nil ? "Loading tool activity…" : "No matching tool calls.")
                        .font(VPTheme.mono(9))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVStack(spacing: 0) {
                        ForEach(entries) { entry in
                            entryRow(entry)
                            VPHairline()
                        }
                    }
                }
            }
            if let status = model.toolActivityStatus {
                Text(status)
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private var summaryStrip: some View {
        HStack(spacing: 8) {
            metric("Visible", "\(filteredSummary.calls)", "of \(activity?.total ?? 0) total")
            metric("Errors", "\(filteredSummary.errors)", "\(filteredSummary.retryableErrors) retryable")
            metric(
                "Avg",
                ToolActivityMetrics.durationLabel(filteredSummary.averageDurationMs),
                "filtered view"
            )
            metric("Max", ToolActivityMetrics.durationLabel(filteredSummary.maxDurationMs), "slowest call")
            metric("Output", ToolActivityMetrics.byteLabel(filteredSummary.outputBytes), "full result bytes")
            metric(
                "Approval",
                filteredSummary.approvalWaits == 0
                    ? "—"
                    : ToolActivityMetrics.durationLabel(filteredSummary.maxApprovalWaitMs),
                "max wait · \(filteredSummary.approvalWaits) request(s)"
            )
        }
    }

    private func metric(_ title: String, _ value: String, _ detail: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title.uppercased())
                .font(VPTheme.mono(7, weight: .semibold))
                .foregroundStyle(VPTheme.textTertiary)
            Text(value)
                .font(VPTheme.mono(11, weight: .semibold))
                .monospacedDigit()
            Text(detail)
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textTertiary)
                .lineLimit(1)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(7)
        .background(VPTheme.surfaceRaised.opacity(0.5), in: RoundedRectangle(cornerRadius: 8))
    }

    private func entryRow(_ entry: ToolActivityEntry) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 7) {
                Image(systemName: entry.isError ? "xmark.octagon.fill" : "checkmark.circle")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(entry.isError ? VPTheme.danger : VPTheme.success)
                    .frame(width: 12)
                Text(entry.name)
                    .font(VPTheme.mono(9.5, weight: .semibold))
                Text(ToolActivityMetrics.statusLabel(entry))
                    .font(VPTheme.mono(7.5, weight: .semibold))
                    .foregroundStyle(entry.isError ? VPTheme.danger : VPTheme.textTertiary)
                if entry.retryable {
                    Text("RETRYABLE")
                        .font(VPTheme.mono(6.5, weight: .semibold))
                        .foregroundStyle(VPTheme.warning)
                }
                Spacer()
                Text(entry.createdAt)
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
                Text(ToolActivityMetrics.durationLabel(entry.durationMs))
                    .font(VPTheme.mono(8))
                    .monospacedDigit()
                Text(ToolActivityMetrics.byteLabel(entry.outputBytes))
                    .font(VPTheme.mono(8))
                    .monospacedDigit()
                Button {
                    withAnimation(VPMotion.snap) {
                        expandedID = expandedID == entry.id ? nil : entry.id
                    }
                } label: {
                    Image(systemName: "chevron.right")
                        .font(.system(size: 7, weight: .bold))
                        .rotationEffect(.degrees(expandedID == entry.id ? 90 : 0))
                }
                .buttonStyle(.plain)
                .frame(width: 14)
            }
            if expandedID == entry.id {
                HStack(spacing: 14) {
                    meta("seq", "\(entry.seq)")
                    meta("risk", entry.approvalRisk ?? "—")
                    meta("mode", entry.approvalMode ?? "—")
                    meta(
                        "approval wait",
                        entry.approvalWaitMs.map(ToolActivityMetrics.durationLabel) ?? "—"
                    )
                    meta("truncated", entry.truncated ? "yes" : "no")
                }
                .padding(.leading, 19)
                if let hint = entry.hint, !hint.isEmpty {
                    Text(hint)
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.danger.opacity(0.85))
                        .padding(.leading, 19)
                        .textSelection(.enabled)
                }
            }
        }
        .padding(.vertical, 6)
        .contentShape(Rectangle())
    }

    private func meta(_ label: String, _ value: String) -> some View {
        HStack(spacing: 3) {
            Text(label.uppercased())
                .font(VPTheme.mono(6.5, weight: .semibold))
                .foregroundStyle(VPTheme.textTertiary)
            Text(value)
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textSecondary)
        }
    }
}
