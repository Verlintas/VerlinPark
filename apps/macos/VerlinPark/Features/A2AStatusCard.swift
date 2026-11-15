import SwiftUI

/// Read-only loopback A2A status card. It renders only protocol fields and
/// coarse action closures; it never fetches, starts, or stops the facade itself
/// and it never displays a token or token hash.
struct A2AStatusCard: View {
    let status: A2AStatus?
    var tasks: [A2ATaskSummary] = []
    var tasksTruncated: Bool = false
    var policy: A2APolicy?
    var isLoading: Bool = false
    var errorMessage: String?
    var onStart: (() -> Void)?
    var onStop: (() -> Void)?
    var onRefresh: (() -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: 11) {
            header
            if let message = errorMessage, !message.isEmpty {
                notice(
                    icon: "exclamationmark.triangle.fill",
                    title: "Status request failed",
                    text: A2AText.bounded(message, limit: 240),
                    tint: VPTheme.warning
                )
            }
            if let status {
                if !status.enabled {
                    notice(
                        icon: "pause.circle",
                        title: A2ACopy.disabledTitle,
                        text: A2ACopy.disabledDetail,
                        tint: VPTheme.textTertiary
                    )
                }
                statusBadges(status)
                endpointBlock(status)
                capacityBlock(status)
                inboundBlock(status)
                if let lastError = status.lastError {
                    notice(
                        icon: "exclamationmark.triangle.fill",
                        title: "Last A2A error",
                        text: lastError,
                        tint: VPTheme.danger
                    )
                }
                if let effectivePolicy = effectivePolicy(status) {
                    policyBlock(effectivePolicy)
                }
                tasksSection(status)
            } else if isLoading {
                loadingBlock
            } else {
                stateBlock(
                    icon: "questionmark.circle",
                    title: A2ACopy.unavailableTitle,
                    detail: A2ACopy.unavailableDetail,
                    tint: VPTheme.textTertiary
                )
            }
            footer
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
                SectionLabel(title: "A2A endpoint")
                Text("Loopback Agent2Agent")
                    .font(VPTheme.font(13, weight: .semibold, rounded: true))
                    .foregroundStyle(VPTheme.textPrimary)
                    .lineLimit(1)
                Text("Local-only facade · task lifecycle status")
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(1)
            }
            Spacer(minLength: 8)
            if isLoading {
                ProgressView()
                    .controlSize(.small)
            }
            if let onStart {
                Button("Start") {
                    onStart()
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(!canStart)
                .help("Start the loopback A2A facade under the project policy.")
            }
            if let onStop {
                Button("Stop") {
                    onStop()
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(!canStop)
                .help("Stop the loopback A2A facade.")
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
                .help("Refresh A2A status")
                .accessibilityLabel("Refresh A2A status")
            }
        }
    }

    private var canStart: Bool {
        guard !isLoading, let status, status.enabled, !status.running else {
            return false
        }
        if let effective = effectivePolicy(status), !effective.enabled {
            return false
        }
        return true
    }

    private var canStop: Bool {
        !isLoading && status?.running == true
    }

    private func effectivePolicy(_ status: A2AStatus) -> A2APolicy? {
        policy ?? status.policy
    }

    // MARK: Status

    private func statusBadges(_ status: A2AStatus) -> some View {
        VPFlowLayout(spacing: 5) {
            runtimePill(status)
            badge(
                A2ACopy.loopbackBadgeText,
                tint: VPTheme.textSecondary,
                icon: "lock.shield",
                help: A2ACopy.loopbackWarning
            )
            badge(
                status.tokenBadgeText,
                tint: status.tokenPresent ? VPTheme.success : VPTheme.textTertiary,
                icon: status.tokenPresent ? "lock.fill" : "lock.open",
                help: A2ACopy.noTokenNotice
            )
            badge(
                status.inboundStatusText,
                tint: status.inboundEnabled ? VPTheme.warning : VPTheme.textTertiary,
                icon: "arrow.down.left.circle",
                help: status.inboundEnabled
                    ? A2ACopy.inboundWarning
                    : A2ACopy.inboundDisabledNote
            )
            if status.isCapacityFull {
                badge(
                    "at capacity",
                    tint: VPTheme.warning,
                    icon: "exclamationmark.triangle.fill",
                    help: status.capacityDetail
                )
            }
        }
    }

    private func runtimePill(_ status: A2AStatus) -> some View {
        HStack(spacing: 5) {
            Image(systemName: status.stateIcon)
                .font(.system(size: 7.5, weight: .semibold))
            Text(status.stateLabel.uppercased())
                .font(VPTheme.mono(7.5, weight: .semibold))
                .tracking(0.35)
                .lineLimit(1)
        }
        .foregroundStyle(status.stateTint)
        .padding(.horizontal, 6)
        .padding(.vertical, 3)
        .background(status.stateTint.opacity(0.11), in: Capsule())
        .overlay {
            Capsule().strokeBorder(status.stateTint.opacity(0.26), lineWidth: 1)
        }
        .help(runtimeHelp(status))
        .accessibilityLabel("A2A status: \(status.stateLabel)")
    }

    private func runtimeHelp(_ status: A2AStatus) -> String {
        switch status.runtimeState {
        case .running:
            "The loopback A2A facade is accepting local task requests."
        case .stopped:
            "The facade is enabled by policy but is not currently listening."
        case .disabled:
            "The project policy disables the loopback facade."
        case .unavailable:
            "A2A status has not been loaded."
        }
    }

    private func badge(
        _ text: String,
        tint: Color,
        icon: String,
        help: String
    ) -> some View {
        HStack(spacing: 4) {
            Image(systemName: icon)
                .font(.system(size: 7, weight: .semibold))
            Text(text.uppercased())
                .font(VPTheme.mono(7, weight: .semibold))
                .tracking(0.25)
                .lineLimit(1)
        }
        .foregroundStyle(tint)
        .padding(.horizontal, 6)
        .padding(.vertical, 3)
        .background(tint.opacity(0.09), in: Capsule())
        .overlay {
            Capsule().strokeBorder(tint.opacity(0.22), lineWidth: 1)
        }
        .help(help)
    }

    // MARK: Endpoint and capacity

    private func endpointBlock(_ status: A2AStatus) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            endpointRow("Endpoint", value: status.displayURL)
            endpointRow("Agent card", value: status.displayCardURL)
        }
        .padding(8)
        .background(
            VPTheme.surfaceSunken.opacity(0.45),
            in: RoundedRectangle(cornerRadius: 9, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
    }

    private func endpointRow(_ label: String, value: String?) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(label.uppercased())
                .font(VPTheme.mono(6.5, weight: .semibold))
                .tracking(0.3)
                .foregroundStyle(VPTheme.textTertiary)
                .frame(width: 68, alignment: .leading)
            Text(verbatim: value ?? "not reported")
                .font(VPTheme.mono(8.5))
                .foregroundStyle(value == nil ? VPTheme.textTertiary : VPTheme.textSecondary)
                .lineLimit(2)
                .truncationMode(.middle)
                .textSelection(.enabled)
            Spacer(minLength: 0)
        }
    }

    private func capacityBlock(_ status: A2AStatus) -> some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 1) {
                Text("TASK CAPACITY")
                    .font(VPTheme.mono(6.5, weight: .semibold))
                    .tracking(0.3)
                    .foregroundStyle(VPTheme.textTertiary)
                Text(status.capacityLabel)
                    .font(VPTheme.mono(13, weight: .semibold))
                    .monospacedDigit()
                    .foregroundStyle(
                        status.isCapacityFull ? VPTheme.warning : VPTheme.textPrimary
                    )
            }
            if let fraction = status.capacityFraction {
                VPGradientProgress(
                    value: fraction,
                    tint: status.isCapacityFull ? VPTheme.warning : VPTheme.accent,
                    height: 4
                )
                .frame(width: 120)
            }
            Spacer(minLength: 0)
            Text(status.capacityDetail)
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
                .lineLimit(1)
        }
        .padding(8)
        .background(
            VPTheme.surfaceRaised.opacity(0.45),
            in: RoundedRectangle(cornerRadius: 9, style: .continuous)
        )
    }

    // MARK: Inbound, policy, errors

    @ViewBuilder
    private func inboundBlock(_ status: A2AStatus) -> some View {
        if status.inboundEnabled {
            notice(
                icon: "exclamationmark.shield.fill",
                title: "Inbound A2A enabled",
                text: A2ACopy.inboundWarning,
                tint: VPTheme.warning
            )
        } else {
            HStack(alignment: .top, spacing: 7) {
                Image(systemName: "shield.lefthalf.filled")
                    .font(.system(size: 8.5, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                    .frame(width: 10)
                Text(A2ACopy.inboundDisabledNote)
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(3)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 0)
            }
        }
    }

    private func policyBlock(_ policy: A2APolicy) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 8) {
                SectionLabel(title: "Policy")
                Spacer(minLength: 0)
                Text("project scoped")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            VPFlowLayout(spacing: 5) {
                badge(
                    policy.stateLabel,
                    tint: policy.stateTint,
                    icon: policy.enabled ? "checkmark.circle" : "pause.circle",
                    help: policy.enabled
                        ? "The project policy enables the A2A facade."
                        : "The project policy disables the A2A facade."
                )
                badge(
                    policy.inboundLabel,
                    tint: policy.allowInbound ? VPTheme.warning : VPTheme.textTertiary,
                    icon: "arrow.down.left.circle",
                    help: policy.allowInbound
                        ? A2ACopy.inboundWarning
                        : A2ACopy.inboundDisabledNote
                )
                badge(
                    "port \(policy.portLabel)",
                    tint: VPTheme.textSecondary,
                    icon: "number",
                    help: "The loopback listener port. Zero selects an ephemeral loopback port."
                )
                badge(
                    policy.tokenLabel,
                    tint: policy.tokenRequired ? VPTheme.accent : VPTheme.textTertiary,
                    icon: "lock",
                    help: A2ACopy.noTokenNotice
                )
                badge(
                    policy.capacityLabel,
                    tint: VPTheme.textSecondary,
                    icon: "list.number",
                    help: "Maximum concurrently retained A2A tasks."
                )
                badge(
                    "ttl \(policy.ttlLabel)",
                    tint: VPTheme.textSecondary,
                    icon: "clock",
                    help: policy.ttlDetail
                )
            }
            Text(policy.ttlDetail)
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textTertiary)
                .lineLimit(1)
            if policy.enabled {
                Text(policy.endpointHint)
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(1)
                    .textSelection(.enabled)
            }
        }
        .padding(8)
        .background(
            VPTheme.surfaceRaised.opacity(0.4),
            in: RoundedRectangle(cornerRadius: 9, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
    }

    // MARK: Tasks

    private func tasksSection(_ status: A2AStatus) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                SectionLabel(title: "Tasks")
                Text("\(tasks.count) loaded")
                    .font(VPTheme.mono(7))
                    .monospacedDigit()
                    .foregroundStyle(VPTheme.textTertiary)
                Spacer(minLength: 0)
                Text(status.taskCountLabel)
                    .font(VPTheme.mono(7))
                    .monospacedDigit()
                    .foregroundStyle(VPTheme.textTertiary)
            }
            if tasks.isEmpty {
                stateBlock(
                    icon: "tray",
                    title: status.taskCount > 0
                        ? "Task summaries not loaded"
                        : A2ACopy.emptyTasksTitle,
                    detail: status.taskCount > 0
                        ? "The status reports \(status.taskCountLabel); refresh the bounded task list."
                        : A2ACopy.emptyTasksDetail,
                    tint: VPTheme.textTertiary
                )
            } else {
                ForEach(Array(tasks.prefix(5).enumerated()), id: \.offset) { _, task in
                    A2ATaskRow(task: task)
                }
                if tasks.count > 5 {
                    Text("+\(tasks.count - 5) more task summaries")
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                if tasksTruncated {
                    Text("The daemon truncated the task list; older summaries are not shown.")
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.warning)
                        .lineLimit(2)
                }
                if status.taskCount > tasks.count {
                    Text(
                        "\(status.taskCount - tasks.count) task(s) are outside the bounded summary list."
                    )
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(2)
                }
            }
        }
    }

    // MARK: Shared blocks

    private var loadingBlock: some View {
        HStack(spacing: 8) {
            ProgressView()
                .controlSize(.small)
            VStack(alignment: .leading, spacing: 1) {
                Text(A2ACopy.loadingTitle)
                    .font(VPTheme.font(11, weight: .semibold))
                    .foregroundStyle(VPTheme.textSecondary)
                Text("Waiting for the loopback facade status.")
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
                Text(detail)
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

    private func notice(
        icon: String,
        title: String,
        text: String,
        tint: Color
    ) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: icon)
                .font(.system(size: 9.5, weight: .semibold))
                .foregroundStyle(tint)
                .frame(width: 12)
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(VPTheme.mono(7.5, weight: .semibold))
                    .tracking(0.25)
                    .foregroundStyle(tint)
                Text(text)
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textSecondary)
                    .lineLimit(3)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
        }
        .padding(8)
        .background(
            tint.opacity(0.06),
            in: RoundedRectangle(cornerRadius: 8, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .strokeBorder(tint.opacity(0.16), lineWidth: 1)
        }
    }

    private var footer: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(A2ACopy.loopbackWarning)
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
                .lineLimit(3)
                .fixedSize(horizontal: false, vertical: true)
            Text(A2ACopy.noTokenNotice)
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textTertiary)
        }
    }
}

// MARK: - Task state

struct A2ATaskStatePill: View {
    let state: A2ATaskState

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: state.icon)
                .font(.system(size: 7.5, weight: .semibold))
            Text(state.label.uppercased())
                .font(VPTheme.mono(7, weight: .semibold))
                .tracking(0.25)
                .lineLimit(1)
        }
        .foregroundStyle(state.tint)
        .padding(.horizontal, 6)
        .padding(.vertical, 3)
        .background(state.tint.opacity(0.10), in: Capsule())
        .overlay {
            Capsule().strokeBorder(state.tint.opacity(0.24), lineWidth: 1)
        }
        .help(state.helpText)
        .accessibilityLabel("A2A task state: \(state.label)")
    }
}

struct A2ATaskRow: View {
    let task: A2ATaskSummary

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            A2ATaskStatePill(state: task.state)
            VStack(alignment: .leading, spacing: 2) {
                Text(verbatim: task.displayID)
                    .font(VPTheme.mono(9, weight: .semibold))
                    .foregroundStyle(VPTheme.textPrimary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
                HStack(spacing: 8) {
                    if let context = task.displayContextID {
                        Text("context \(String(context.prefix(18)))")
                            .lineLimit(1)
                    }
                    Text(task.countsLabel)
                        .monospacedDigit()
                    if let timestamp = task.timestampLabel {
                        Text(timestamp)
                            .lineLimit(1)
                    }
                    Spacer(minLength: 0)
                }
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textTertiary)
            }
            Spacer(minLength: 0)
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
}
