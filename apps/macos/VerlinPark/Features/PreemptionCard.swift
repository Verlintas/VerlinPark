import SwiftUI

/// Manage Dash card for hard preemption policy, counts, and recent terminal
/// records. This is an operator surface; it never retries or merges work.
struct PreemptionCard: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model
        VPSurfaceCard(padding: 14) {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        SectionLabel(title: "Preemption & deadlines")
                        Text("Hard stops for wall-clock, explicit preempt, and tool deadlines")
                            .font(.system(size: 13, weight: .semibold, design: .rounded))
                    }
                    Spacer()
                    if model.preemptionBusy {
                        ProgressView().controlSize(.small)
                    }
                    Button("Refresh") {
                        Task { await model.loadPreemption() }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(model.preemptionBusy)
                }

                if let session = model.selectedSession {
                    HStack(spacing: 8) {
                        statusPill(session.status)
                        if let result = model.sessionPreemption {
                            statusPill(
                                result.policy.enabled
                                    ? "wall-clock on"
                                    : "wall-clock off"
                            )
                            statusPill(
                                "tool deadline "
                                    + (result.policy.toolDeadlineSecs.map(String.init) ?? "off")
                            )
                            if let overview = model.preemptionOverview {
                                statusPill("project total \(overview.total)")
                            }
                        }
                        Spacer()
                        Button("Preempt Now") {
                            Task {
                                await model.preemptActiveRun(
                                    reason: "operator preemption from Manage Dash"
                                )
                            }
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                        .disabled(
                            model.preemptionBusy
                                || !["running", "paused", "queued"].contains(session.status)
                        )
                    }

                    if let result = model.sessionPreemption {
                        HStack(spacing: 12) {
                            countLabel("by phase", counts: result.byPhase)
                            countLabel("by reason", counts: result.byReason)
                        }
                        DisclosureGroup("Policy") {
                            VStack(alignment: .leading, spacing: 8) {
                                Toggle(
                                    "Automatic wall-clock preemption",
                                    isOn: $model.preemptionPolicyDraft.enabled
                                )
                                Toggle(
                                    "Session checkpoint at preemption",
                                    isOn: $model.preemptionPolicyDraft.checkpointOnPreempt
                                )
                                Toggle(
                                    "Per-tool deadline",
                                    isOn: Binding(
                                        get: {
                                            model.preemptionPolicyDraft.toolDeadlineSecs != nil
                                        },
                                        set: { enabled in
                                            model.preemptionPolicyDraft.toolDeadlineSecs =
                                                enabled ? 30 : nil
                                        }
                                    )
                                )
                                HStack(spacing: 10) {
                                    policyField(
                                        "provider grace ms",
                                        value: $model.preemptionPolicyDraft.providerGraceMs
                                    )
                                    policyField(
                                        "tool grace ms",
                                        value: $model.preemptionPolicyDraft.toolGraceMs
                                    )
                                    policyField(
                                        "kill escalation ms",
                                        value: $model.preemptionPolicyDraft.killEscalationMs
                                    )
                                    policyField(
                                        "tool deadline secs",
                                        value: Binding(
                                            get: {
                                                model.preemptionPolicyDraft.toolDeadlineSecs
                                                    ?? 0
                                            },
                                            set: { value in
                                                model.preemptionPolicyDraft.toolDeadlineSecs =
                                                    value == 0 ? nil : value
                                            }
                                        )
                                    )
                                }
                                HStack {
                                    Text(
                                        "Prompt graces are clamped by the daemon; no value is infinite."
                                    )
                                    .font(VPTheme.mono(7.5))
                                    .foregroundStyle(VPTheme.textTertiary)
                                    Spacer()
                                    if model.preemptionPolicyBusy {
                                        ProgressView().controlSize(.small)
                                    }
                                    Button("Save Policy") {
                                        Task { await model.savePreemptionPolicy() }
                                    }
                                    .buttonStyle(.bordered)
                                    .controlSize(.small)
                                    .disabled(model.preemptionPolicyBusy)
                                }
                                if let status = model.preemptionPolicyStatus,
                                   !status.isEmpty
                                {
                                    Text(status)
                                        .font(VPTheme.mono(8))
                                        .foregroundStyle(VPTheme.textTertiary)
                                        .lineLimit(2)
                                }
                            }
                            .padding(.vertical, 4)
                        }
                        if result.records.isEmpty {
                            emptyState("No hard preemptions recorded for this session.")
                        } else {
                            VStack(alignment: .leading, spacing: 5) {
                                SectionLabel(title: "Recent records")
                                ForEach(result.records.suffix(6).reversed()) { record in
                                    recordRow(record)
                                }
                            }
                        }
                    } else {
                        emptyState("Select refresh to load preemption history.")
                    }

                    if let status = model.preemptionStatus, !status.isEmpty {
                        Text(status)
                            .font(VPTheme.mono(8.5))
                            .foregroundStyle(VPTheme.textTertiary)
                            .lineLimit(3)
                    }
                } else {
                    emptyState("Select a session to inspect preemption history.")
                }

                Text(
                    "Preemption never merges incomplete work and never claims exactly-once side effects. A tool deadline stops one tool; wall-clock and explicit preemption stop the run."
                )
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func countLabel(_ title: String, counts: [PreemptionCount]) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title.uppercased())
                .font(VPTheme.mono(7.5, weight: .semibold))
                .foregroundStyle(VPTheme.textTertiary)
            if counts.isEmpty {
                Text("none")
                    .font(VPTheme.mono(9))
                    .foregroundStyle(VPTheme.textSecondary)
            } else {
                Text(
                    counts
                        .map { "\($0.key) \($0.count)" }
                        .joined(separator: " · ")
                )
                .font(VPTheme.mono(9))
                .foregroundStyle(VPTheme.textSecondary)
            }
        }
    }

    private func recordRow(_ record: PreemptionRecord) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Image(
                systemName: record.sideEffectsUnknown
                    ? "exclamationmark.triangle.fill"
                    : "bolt.slash"
            )
            .font(.system(size: 9, weight: .semibold))
            .foregroundStyle(
                record.sideEffectsUnknown ? VPTheme.warning : VPTheme.textTertiary
            )
            VStack(alignment: .leading, spacing: 1) {
                Text(record.summary)
                    .font(VPTheme.mono(9))
                    .foregroundStyle(VPTheme.textSecondary)
                HStack(spacing: 6) {
                    Text(record.agentRole.isEmpty ? "agent" : record.agentRole)
                    if let task = record.taskId, !task.isEmpty {
                        Text("task \(task)")
                    }
                    if record.usageEstimated,
                       record.partialInputTokens + record.partialOutputTokens > 0
                    {
                        Text("usage estimated")
                    }
                    if record.checkpointId != nil {
                        Text("checkpoint saved")
                    }
                    if let commit = record.taskCheckpointCommit, !commit.isEmpty {
                        Text("task snapshot \(commit.prefix(8))")
                    }
                }
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
            }
            Spacer(minLength: 0)
        }
    }

    private func policyField(
        _ label: String,
        value: Binding<UInt64>
    ) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label)
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textTertiary)
            TextField("", value: value, format: .number)
                .textFieldStyle(.roundedBorder)
                .font(VPTheme.mono(9))
                .frame(width: 92)
        }
    }

    private func statusPill(_ text: String) -> some View {
        Text(text)
            .font(VPTheme.mono(8, weight: .medium))
            .padding(.horizontal, 7)
            .padding(.vertical, 3)
            .background(VPTheme.surfaceSunken, in: Capsule())
            .overlay {
                Capsule().strokeBorder(VPTheme.hairline, lineWidth: 1)
            }
            .foregroundStyle(VPTheme.textSecondary)
    }

    private func emptyState(_ text: String) -> some View {
        Text(text)
            .font(VPTheme.mono(8.5))
            .foregroundStyle(VPTheme.textTertiary)
    }
}
