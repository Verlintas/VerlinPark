import SwiftUI

private struct LongTaskSheetHeader: View {
    let eyebrow: String
    let title: String
    let detail: String

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(L10n.t(eyebrow).uppercased())
                .font(VPTheme.mono(8, weight: .semibold))
                .tracking(1.2)
                .foregroundStyle(VPTheme.accent)
            Text(L10n.t(title))
                .font(VPTheme.font(20, weight: .semibold, rounded: true))
                .foregroundStyle(VPTheme.textPrimary)
            Text(L10n.t(detail))
                .font(VPTheme.font(10.5))
                .foregroundStyle(VPTheme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}

struct SteerSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let session: Session

    @State private var body_ = ""
    @State private var agentID = ""
    @State private var taskID = ""
    @State private var busy = false
    @State private var error: String?

    private var eligibleAgents: [AgentRecord] {
        model.agents.filter { ["running", "waiting_input"].contains($0.status) }
    }

    private var planTasks: [PlanTask] {
        model.teamPlan?.tasks ?? []
    }

    private var canSend: Bool {
        if case .success = LongTaskRules.validateSteeringBody(body_) {
            return !busy
        }
        return false
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            LongTaskSheetHeader(
                eyebrow: "Steer",
                title: "Steer Run",
                detail: "Add a bounded operator instruction to one active agent. Delivery happens immediately before the target run's next provider request; it cannot interrupt an in-flight model call or tool execution."
            )
            Label(session.goal, systemImage: "bubble.left")
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
                .lineLimit(1)

            VStack(alignment: .leading, spacing: 6) {
                Text("TARGET")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                Picker("Target agent", selection: $agentID) {
                    Text("Automatic (session Leader)").tag("")
                    ForEach(eligibleAgents) { agent in
                        Text(
                            "\(agent.name) · \(agent.role)\(agent.title.map { " · \($0)" } ?? "") · \(agent.status)"
                        )
                        .tag(agent.id)
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                Text(
                    eligibleAgents.isEmpty
                        ? L10n
                        .t(
                            "No running or waiting agents right now. The daemon will reject steering until the Leader or target agent is active."
                        )
                        : L10n
                        .t(
                            "Only running or waiting agents accept steering. Automatic targets the session Leader."
                        )
                )
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("TASK SCOPE")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                Picker("Task scope", selection: $taskID) {
                    Text("No task scope").tag("")
                    ForEach(planTasks) { task in
                        Text("\(task.id) · \(task.title)").tag(task.id)
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                Text(
                    "Optional. The target agent must currently own this plan task or the daemon rejects the request."
                )
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
            }

            VStack(alignment: .leading, spacing: 4) {
                Text("INSTRUCTION")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                TextEditor(text: $body_)
                    .font(VPTheme.mono(10))
                    .scrollContentBackground(.hidden)
                    .padding(7)
                    .frame(minHeight: 112)
                    .background(
                        VPTheme.surface,
                        in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                    )
                    .overlay {
                        RoundedRectangle(cornerRadius: 8, style: .continuous)
                            .strokeBorder(VPTheme.hairline, lineWidth: 1)
                    }
                    .overlay(alignment: .topLeading) {
                        if body_.isEmpty {
                            Text("What should this run change or verify?")
                                .font(VPTheme.mono(9))
                                .foregroundStyle(VPTheme.textTertiary)
                                .padding(.horizontal, 12)
                                .padding(.vertical, 11)
                                .allowsHitTesting(false)
                        }
                    }
                HStack {
                    Text(L10n.format("%lld / %lld characters", body_.count, LongTaskRules.maxBodyChars))
                        .font(VPTheme.mono(8))
                        .foregroundStyle(
                            body_.count > LongTaskRules.maxBodyChars
                                ? VPTheme.danger
                                : VPTheme.textTertiary
                        )
                    Spacer()
                    if let validationMessage {
                        Text(validationMessage)
                            .font(VPTheme.mono(8))
                            .foregroundStyle(VPTheme.warning)
                    }
                }
            }

            if let error {
                Label(error, systemImage: "exclamationmark.triangle")
                    .font(VPTheme.mono(8.5))
                    .foregroundStyle(VPTheme.danger)
                    .fixedSize(horizontal: false, vertical: true)
            }

            HStack {
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .disabled(busy)
                Button(busy ? L10n.t("Sending…") : L10n.t("Send")) {
                    Task { await send() }
                }
                .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                .keyboardShortcut(.defaultAction)
                .disabled(!canSend)
            }
        }
        .padding(22)
        .frame(width: 560)
        .background { VPCrystalBackdrop() }
        .tint(VPTheme.accent)
        .interactiveDismissDisabled(busy)
        .onAppear {
            body_ = ""
            agentID = ""
            taskID = ""
            error = nil
        }
    }

    private var validationMessage: String? {
        guard !body_.isEmpty else { return nil }
        switch LongTaskRules.validateSteeringBody(body_) {
        case .success:
            return nil
        case let .failure(error):
            return error.message
        }
    }

    private func send() async {
        let selectedAgent: String? = agentID.isEmpty ? nil : agentID
        let selectedTask: String? = taskID.isEmpty ? nil : taskID
        busy = true
        error = nil
        error = await model.steerSession(
            body: body_,
            agentID: selectedAgent,
            taskID: selectedTask
        )
        busy = false
        if error == nil {
            dismiss()
        }
    }
}

private enum DeferDeadlineMode: String, CaseIterable, Identifiable {
    case after
    case at

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .after: L10n.t("After N minutes")
        case .at: L10n.t("At date")
        }
    }
}

struct DeferSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let session: Session

    @State private var body_ = ""
    @State private var watchTaskIDs: Set<String> = []
    @State private var watchMode = "all"
    @State private var deadlineEnabled = false
    @State private var deadlineMode: DeferDeadlineMode = .after
    @State private var afterMinutesText = "60"
    @State private var deadlineDate = Date().addingTimeInterval(60 * 60)
    @State private var busy = false
    @State private var error: String?

    private var planTasks: [PlanTask] {
        model.teamPlan?.tasks ?? []
    }

    private var selectedWatchIDs: [String] {
        planTasks.map(\.id).filter { watchTaskIDs.contains($0) }
    }

    private var afterMinutes: Int? {
        guard deadlineEnabled, deadlineMode == .after else { return nil }
        let trimmed = afterMinutesText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let value = Int(trimmed), value > 0 else { return nil }
        return value
    }

    private var deadline: Date? {
        deadlineEnabled && deadlineMode == .at ? deadlineDate : nil
    }

    private var validation: LongTaskValidationError? {
        if case let .failure(error) = LongTaskRules.validateSteeringBody(body_) {
            return error
        }
        if deadlineEnabled, deadlineMode == .after, afterMinutes == nil {
            return .invalidAfterMinutes
        }
        switch LongTaskRules.validateDefer(
            body: body_,
            watchTaskIDs: selectedWatchIDs,
            watchMode: watchMode,
            afterMinutes: afterMinutes,
            deadline: deadline
        ) {
        case .success:
            return nil
        case let .failure(error):
            return error
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            LongTaskSheetHeader(
                eyebrow: "Defer",
                title: "Defer Continuation",
                detail: "Create a persisted operator prompt that wakes after watched plan tasks settle or a deadline arrives. It does not start a run now and does not add a plan dependency."
            )
            Label(session.goal, systemImage: "bubble.left")
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
                .lineLimit(1)

            VStack(alignment: .leading, spacing: 5) {
                Text("PROMPT")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                TextEditor(text: $body_)
                    .font(VPTheme.mono(10))
                    .scrollContentBackground(.hidden)
                    .padding(7)
                    .frame(minHeight: 88)
                    .background(
                        VPTheme.surface,
                        in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                    )
                    .overlay {
                        RoundedRectangle(cornerRadius: 8, style: .continuous)
                            .strokeBorder(VPTheme.hairline, lineWidth: 1)
                    }
                    .overlay(alignment: .topLeading) {
                        if body_.isEmpty {
                            Text("What should the next run do?")
                                .font(VPTheme.mono(9))
                                .foregroundStyle(VPTheme.textTertiary)
                                .padding(.horizontal, 12)
                                .padding(.vertical, 11)
                                .allowsHitTesting(false)
                        }
                    }
                Text(L10n.format("%lld / %lld characters", body_.count, LongTaskRules.maxBodyChars))
                    .font(VPTheme.mono(8))
                    .foregroundStyle(
                        body_.count > LongTaskRules.maxBodyChars
                            ? VPTheme.danger
                            : VPTheme.textTertiary
                    )
            }

            VStack(alignment: .leading, spacing: 5) {
                HStack {
                    Text("WATCH TASKS")
                        .font(VPTheme.mono(8, weight: .semibold))
                        .foregroundStyle(VPTheme.textTertiary)
                    Spacer()
                    Text(L10n.format("%lld selected", selectedWatchIDs.count))
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                if planTasks.isEmpty {
                    Text("No stored TeamPlan. Add a deadline to schedule this continuation instead.")
                        .font(VPTheme.mono(9))
                        .foregroundStyle(VPTheme.textSecondary)
                } else {
                    ScrollView {
                        VStack(alignment: .leading, spacing: 4) {
                            ForEach(planTasks) { task in
                                let status = taskStatus(task.id)
                                let terminal = LongTaskRules.isTerminalTaskStatus(status)
                                Toggle(
                                    isOn: Binding(
                                        get: { watchTaskIDs.contains(task.id) },
                                        set: { selected in
                                            if selected {
                                                watchTaskIDs.insert(task.id)
                                            } else {
                                                watchTaskIDs.remove(task.id)
                                            }
                                        }
                                    )
                                ) {
                                    HStack(spacing: 7) {
                                        Text(task.id)
                                            .font(VPTheme.mono(8.5, weight: .semibold))
                                        Text(task.title)
                                            .font(VPTheme.font(9.5))
                                            .lineLimit(1)
                                        Spacer()
                                        Text(task.department)
                                            .font(VPTheme.mono(8))
                                            .foregroundStyle(VPTheme.textTertiary)
                                        Text(status)
                                            .font(VPTheme.mono(8))
                                            .foregroundStyle(
                                                terminal ? VPTheme.textTertiary : VPTheme.accent
                                            )
                                    }
                                }
                                .toggleStyle(.checkbox)
                                .disabled(terminal)
                                .help(
                                    terminal
                                        ? L10n.t("This task is already settled and cannot be watched.")
                                        : L10n.t("Wake the continuation when this task settles.")
                                )
                            }
                        }
                        .padding(8)
                    }
                    .frame(maxHeight: 148)
                    .background(
                        VPTheme.surfaceSunken,
                        in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                    )
                    .overlay {
                        RoundedRectangle(cornerRadius: 8, style: .continuous)
                            .strokeBorder(VPTheme.hairline, lineWidth: 1)
                    }
                }
            }

            VStack(alignment: .leading, spacing: 5) {
                Text("WAKE WHEN")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                Picker("Wake when", selection: $watchMode) {
                    Text("All watched tasks").tag("all")
                    Text("Any watched task").tag("any")
                }
                .labelsHidden()
                .pickerStyle(.segmented)
                Text(
                    "A watched task counts as settled when it completes, fails, is cancelled, skipped, or interrupted."
                )
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
            }

            VStack(alignment: .leading, spacing: 7) {
                Toggle("Add a deadline", isOn: $deadlineEnabled)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                if deadlineEnabled {
                    Picker("Deadline mode", selection: $deadlineMode) {
                        ForEach(DeferDeadlineMode.allCases) { mode in
                            Text(mode.title).tag(mode)
                        }
                    }
                    .labelsHidden()
                    .pickerStyle(.segmented)
                    if deadlineMode == .after {
                        HStack(spacing: 8) {
                            TextField("Minutes", text: $afterMinutesText)
                                .textFieldStyle(.plain)
                                .font(VPTheme.mono(10))
                                .frame(width: 90)
                                .padding(.horizontal, 9)
                                .padding(.vertical, 6)
                                .vpFieldSurface(radius: 8)
                            Text("minutes")
                                .font(VPTheme.mono(8.5))
                                .foregroundStyle(VPTheme.textSecondary)
                            Text("1–43,200 minutes (30 days)")
                                .font(VPTheme.mono(8))
                                .foregroundStyle(VPTheme.textTertiary)
                        }
                    } else {
                        DatePicker(
                            "Deadline",
                            selection: $deadlineDate,
                            in: Date() ... Date().addingTimeInterval(LongTaskRules.maxDeadlineSecs),
                            displayedComponents: [.date, .hourAndMinute]
                        )
                        .font(VPTheme.mono(9))
                    }
                }
                Text(
                    "The continuation starts only when the session is idle, failed, or cancelled. Running, queued, paused, and waiting sessions keep it ready."
                )
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
            }

            if let validation {
                Label(validation.message, systemImage: "info.circle")
                    .font(VPTheme.mono(8.5))
                    .foregroundStyle(VPTheme.warning)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let error {
                Label(error, systemImage: "exclamationmark.triangle")
                    .font(VPTheme.mono(8.5))
                    .foregroundStyle(VPTheme.danger)
                    .fixedSize(horizontal: false, vertical: true)
            }

            HStack {
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .disabled(busy)
                Button(busy ? L10n.t("Creating…") : L10n.t("Create Continuation")) {
                    Task { await create() }
                }
                .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                .keyboardShortcut(.defaultAction)
                .disabled(busy || validation != nil)
            }
        }
        .padding(22)
        .frame(width: 580)
        .background { VPCrystalBackdrop() }
        .tint(VPTheme.accent)
        .interactiveDismissDisabled(busy)
        .onAppear {
            body_ = ""
            watchTaskIDs = []
            watchMode = "all"
            deadlineEnabled = false
            deadlineMode = .after
            afterMinutesText = "60"
            deadlineDate = Date().addingTimeInterval(60 * 60)
            error = nil
        }
    }

    private func taskStatus(_ taskID: String) -> String {
        if let latest = model.scheduledSessionTasks
            .filter({ $0.taskID == taskID })
            .max(by: { $0.planSeq < $1.planSeq })
        {
            return latest.status
        }
        return model.planTaskStatuses[taskID] ?? "pending"
    }

    private func create() async {
        switch LongTaskRules.validateDefer(
            body: body_,
            watchTaskIDs: selectedWatchIDs,
            watchMode: watchMode,
            afterMinutes: afterMinutes,
            deadline: deadline
        ) {
        case let .failure(validationError):
            error = validationError.message
        case let .success(payload):
            busy = true
            error = nil
            error = await model.deferContinuation(
                body: payload.body,
                watchTaskIDs: payload.watchTaskIDs,
                watchMode: payload.watchMode,
                afterSecs: payload.afterSecs,
                deadlineAt: payload.deadlineAt
            )
            busy = false
            if error == nil {
                dismiss()
            }
        }
    }
}
