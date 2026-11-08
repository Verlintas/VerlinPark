import SwiftUI

struct StageView: View {
    @Environment(AppModel.self) private var model
    @Environment(BrandStore.self) private var brand
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Binding var showingNewSession: Bool
    @Binding var showingSearch: Bool
    @Binding var inspectorVisible: Bool

    var body: some View {
        Group {
            switch model.mode {
            case .resident:
                ExternalResidentView()
            case .project:
                if let project = model.selectedProject {
                    ProjectStageView(
                        project: project,
                        showingNewSession: $showingNewSession,
                        showingSearch: $showingSearch,
                        inspectorVisible: $inspectorVisible
                    )
                } else {
                    WelcomeStageView()
                }
            }
        }
        .id(model.mode)
        .transition(.vpScaleFade)
        .animation(VPMotion.resolved(VPMotion.smooth, reduceMotion: reduceMotion), value: model.mode)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background { VPCrystalSurface(tone: VPTheme.canvas, tintOpacity: 0.56) }
    }
}

private struct ProjectStageView: View {
    @Environment(AppModel.self) private var model
    @Environment(BrandStore.self) private var brand
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let project: Project
    @Binding var showingNewSession: Bool
    @Binding var showingSearch: Bool
    @Binding var inspectorVisible: Bool
    @State private var draft = ""
    @State private var showingContext = false
    @State private var showingFork = false
    @FocusState private var composerFocused: Bool
    @State private var composerHovering = false

    var body: some View {
        VStack(spacing: 0) {
            stageHeader
            VPHairline()
            if let session = model.selectedSession {
                SessionChatView(session: session)
                    .id(session.id)
                    .transition(.vpStage)
            } else {
                emptyState
                    .transition(.vpStage)
            }
            composer
        }
        .animation(
            VPMotion.resolved(VPMotion.smooth, reduceMotion: reduceMotion),
            value: model.selectedSessionID
        )
        .sheet(isPresented: $showingFork) {
            if let session = model.selectedSession {
                ForkSessionSheet(session: session)
                    .environment(model)
            }
        }
        .background {
            Button("") { composerFocused = true }
                .keyboardShortcut("l", modifiers: .command)
                .opacity(0)
                .frame(width: 0, height: 0)
                .accessibilityHidden(true)
        }
    }

    private func contextLabel(_ usage: ContextUsage) -> String {
        "ctx \(contextTokenLabel(usage.estimatedTokens))/\(contextTokenLabel(usage.thresholdTokens))"
    }

    private func contextHelp(_ usage: ContextUsage) -> String {
        var help =
            "Estimated context \(usage.estimatedTokens) tokens across \(usage.messages) messages. Window \(usage.contextWindow); summary threshold \(usage.thresholdTokens); hard limit \(usage.hardLimitTokens)."
        if let compaction = model.contextStatus?.lastCompaction {
            help +=
                " Last \(compaction.mode): summarized \(compaction.summarizedMessages), dropped \(compaction.droppedMessages), kept \(compaction.keptMessages)."
        }
        if let measurement = model.contextStatus?.lastMeasurement {
            help +=
                " Last provider call: estimated \(measurement.estimatedTokens), actual \(measurement.actualInputTokens) input tokens, calibration ×\(String(format: "%.2f", measurement.calibration))."
        }
        return help
    }

    private var stageHeader: some View {
        HStack(alignment: .center, spacing: 16) {
            VStack(alignment: .leading, spacing: 7) {
                HStack(spacing: 8) {
                    Text(project.name.uppercased())
                        .font(VPTheme.mono(8.5, weight: .semibold))
                        .tracking(0.8)
                        .foregroundStyle(VPTheme.projectTint(seed: project.id))

                    Rectangle()
                        .fill(VPTheme.hairline)
                        .frame(width: 1, height: 9)

                    if let info = model.agentInfo {
                        Text(info.configured ? (info.model ?? "MODEL") : "NO MODEL")
                            .font(VPTheme.mono(8, weight: .medium))
                            .foregroundStyle(info.configured ? VPTheme.textTertiary : VPTheme.warning)
                    }

                    if let session = model.selectedSession {
                        StatusPill(status: session.status)
                    }
                }

                Text(model.selectedSession?.goal ?? "Pick a session or start a new run")
                    .font(VPTheme.font(22, weight: .semibold, rounded: true))
                    .foregroundStyle(VPTheme.textPrimary)
                    .lineLimit(2)
                    .lineSpacing(1)
                    .contentTransition(.opacity)
                    .animation(
                        VPMotion.resolved(VPMotion.snap, reduceMotion: reduceMotion),
                        value: model.selectedSessionID
                    )

                HStack(spacing: 10) {
                    if let session = model.selectedSession {
                        if let branch = session.branch {
                            Label(branch, systemImage: "arrow.triangle.branch")
                                .lineLimit(1)
                                .truncationMode(.middle)
                        }
                        Text("session \(session.id.prefix(8))")
                        Text(tokenUsageLabel(session))
                        if let usage = model.contextStatus?.usage {
                            Button {
                                showingContext.toggle()
                            } label: {
                                HStack(spacing: 4) {
                                    Image(systemName: "gauge.with.dots.needle.33percent")
                                        .font(.system(size: 8, weight: .semibold))
                                    Text(contextLabel(usage))
                                }
                                .foregroundStyle(
                                    usage.estimatedTokens >= usage.thresholdTokens
                                        ? VPTheme.warning
                                        : VPTheme.textTertiary
                                )
                            }
                            .buttonStyle(.plain)
                            .help(contextHelp(usage))
                            .popover(isPresented: $showingContext, arrowEdge: .bottom) {
                                ContextPopoverView()
                                    .environment(model)
                            }
                        }
                    } else {
                        Text(project.root)
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Text("·")
                        Text("no session selected")
                    }
                }
                .font(VPTheme.mono(8.5))
                .foregroundStyle(VPTheme.textTertiary)
                .contentTransition(.opacity)
                .animation(
                    VPMotion.resolved(VPMotion.snap, reduceMotion: reduceMotion),
                    value: model.selectedSessionID
                )
            }

            Spacer(minLength: 10)

            HStack(spacing: 5) {
                Button {
                    showingSearch = true
                } label: {
                    Image(systemName: "magnifyingglass")
                }
                .buttonStyle(VPGlassIconButtonStyle(size: 26))
                .keyboardShortcut("f", modifiers: .command)
                .help("Search messages and events · ⌘F")

                Menu {
                    ForEach(model.sessions) { session in
                        Button {
                            if model.selectedProjectID != session.projectId {
                                model.selectProject(session.projectId)
                            }
                            model.selectSession(session.id)
                        } label: {
                            if session.id == model.selectedSessionID {
                                Label(session.goal, systemImage: "checkmark")
                            } else {
                                Text(session.goal)
                            }
                        }
                    }
                    Divider()
                    Button("New Session…") {
                        showingNewSession = true
                    }
                } label: {
                    Image(systemName: "bubble.left.and.bubble.right")
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
                .buttonStyle(VPGlassIconButtonStyle(size: 26))
                .help("Switch session")

                Button {
                    showingNewSession = true
                } label: {
                    Image(systemName: "plus")
                }
                .buttonStyle(VPGlassIconButtonStyle(size: 26, tint: VPTheme.accent))
                .help("New session · ⌘N")

                Button {
                    withAnimation(VPMotion.smooth) {
                        inspectorVisible.toggle()
                    }
                } label: {
                    Image(systemName: "sidebar.right")
                }
                .buttonStyle(
                    VPGlassIconButtonStyle(
                        size: 26,
                        tint: inspectorVisible ? VPTheme.accent : VPTheme.textSecondary
                    )
                )
                .help(inspectorVisible ? "Hide inspector · ⌥⌘I" : "Show inspector · ⌥⌘I")

                if let session = model.selectedSession {
                    Menu {
                        if session.status == "running" {
                            Button("Pause") { Task { await model.pauseSession() } }
                        }
                        if session.status == "paused" {
                            Button("Resume") { Task { await model.resumeSession() } }
                        }
                        if ["running", "paused", "queued"].contains(session.status) {
                            Divider()
                            Button("Stop", role: .destructive) {
                                Task { await model.cancelSession() }
                            }
                        }
                        Divider()
                        Menu("Priority") {
                            Button("Low (-10)") { Task { await model.setSessionPriority(-10) } }
                            Button("Normal (0)") { Task { await model.setSessionPriority(0) } }
                            Button("High (10)") { Task { await model.setSessionPriority(10) } }
                            Button("Urgent (50)") { Task { await model.setSessionPriority(50) } }
                        }
                        Divider()
                        Button("Fork Session…") { showingFork = true }
                        Button("Export Transcript…") {
                            Task { await model.exportTranscript() }
                        }
                    } label: {
                        Image(systemName: "ellipsis")
                    }
                    .menuStyle(.borderlessButton)
                    .fixedSize()
                    .buttonStyle(VPGlassIconButtonStyle(size: 26))
                    .help("Session actions")
                }
            }
        }
        .padding(.horizontal, 24)
        .padding(.vertical, 16)
        .background {
            LinearGradient(
                colors: [VPTheme.accent.opacity(0.035), .clear],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
        }
    }

    private func tokenUsageLabel(_ session: Session) -> String {
        let used = tokenLabel(session.tokensUsed)
        var parts: [String] = []
        if let budget = session.tokenBudget, budget > 0 {
            parts.append("\(used) / \(tokenLabel(budget)) tokens")
        } else {
            parts.append("\(used) tokens")
        }
        if session.costSpentUsd > 0 || session.costBudgetUsd != nil {
            let spent = String(format: "$%.4f", session.costSpentUsd)
            if let budget = session.costBudgetUsd {
                parts.append("\(spent) / \(String(format: "$%.2f", budget))")
            } else {
                parts.append(spent)
            }
        }
        if let seconds = session.timeBudgetSecs, seconds > 0 {
            parts.append("≤ \(seconds)s")
        }
        return parts.joined(separator: " · ")
    }

    private func tokenLabel(_ value: Int) -> String {
        if value >= 1_000_000 {
            return String(format: "%.1fm", Double(value) / 1_000_000)
        }
        if value >= 1000 {
            return String(format: "%.1fk", Double(value) / 1000)
        }
        return "\(value)"
    }

    private var emptyState: some View {
        VStack(spacing: 16) {
            ZStack {
                RoundedRectangle(cornerRadius: 18, style: .continuous)
                    .fill(VPTheme.accentWash)
                    .frame(width: 64, height: 64)
                Image(systemName: "rectangle.stack.badge.plus")
                    .font(.system(size: 24, weight: .light))
                    .foregroundStyle(VPTheme.accent)
                    .symbolRenderingMode(.hierarchical)
            }
            .vpAppear(index: 0, animation: VPMotion.emphasizedSpring)

            VStack(spacing: 6) {
                Text("The stage is quiet.")
                    .font(VPTheme.editorial(24))
                    .foregroundStyle(VPTheme.textPrimary)
                Text("Give the Leader one concrete goal. The team appears in Collab as it is recruited.")
                    .font(VPTheme.font(12))
                    .foregroundStyle(VPTheme.textSecondary)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 380)
            }
            .vpAppear(index: 1, animation: VPMotion.smooth)

            Button {
                showingNewSession = true
            } label: {
                Label("Start a session", systemImage: "plus")
            }
            .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
            .controlSize(.large)
            .vpAppear(index: 2, animation: VPMotion.smooth)

            HStack(spacing: 14) {
                Label("isolated worktree", systemImage: "arrow.triangle.branch")
                Label("verifier gate", systemImage: "checkmark.shield")
                Label("deterministic merge", systemImage: "arrow.triangle.merge")
            }
            .font(VPTheme.mono(8))
            .foregroundStyle(VPTheme.textTertiary)
            .vpAppear(index: 3, animation: VPMotion.smooth)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(40)
    }

    private var composer: some View {
        VStack(spacing: 8) {
            HStack(alignment: .bottom, spacing: 10) {
                Button {
                    showingNewSession = true
                } label: {
                    Image(systemName: "plus")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(VPTheme.textSecondary)
                        .frame(width: 24, height: 24)
                        .background(VPTheme.surfaceSunken, in: Circle())
                        .overlay {
                            Circle().strokeBorder(VPTheme.hairline, lineWidth: 1)
                        }
                }
                .buttonStyle(VPPressableButtonStyle(scale: 0.9))
                .help("New session")

                TextField(
                    model.selectedSession == nil
                        ? "Describe a task to start a session…"
                        : "Message the Leader…",
                    text: $draft,
                    axis: .vertical
                )
                .textFieldStyle(.plain)
                .font(VPTheme.font(12.5))
                .foregroundStyle(VPTheme.textPrimary)
                .lineLimit(1 ... 5)
                .focused($composerFocused)
                .onSubmit(send)

                Button(action: send) {
                    ZStack {
                        Circle()
                            .fill(
                                sendDisabled
                                    ? VPTheme.surfaceHover
                                    : VPTheme.accentDeep
                            )
                            .frame(width: 28, height: 28)
                        Image(systemName: "arrow.up")
                            .font(.system(size: 11, weight: .bold))
                            .foregroundStyle(
                                sendDisabled ? VPTheme.textTertiary : .white
                            )
                            .contentTransition(.symbolEffect(.replace))
                    }
                    .shadow(
                        color: sendDisabled ? .clear : VPTheme.accentDeep.opacity(0.28),
                        radius: 8,
                        y: 2
                    )
                }
                .buttonStyle(VPPressableButtonStyle(scale: 0.9))
                .disabled(sendDisabled)
                .help("Send · ↵")
            }

            HStack(spacing: 8) {
                if let info = model.agentInfo, info.configured {
                    Label(info.model ?? "model", systemImage: "cpu")
                        .foregroundStyle(VPTheme.textTertiary)
                } else {
                    Label("no model configured", systemImage: "exclamationmark.triangle")
                        .foregroundStyle(VPTheme.warning)
                }

                Spacer()

                if let session = model.selectedSession,
                   ["running", "paused", "queued"].contains(session.status)
                {
                    HStack(spacing: 5) {
                        VPStatusDot(
                            color: VPTheme.statusColor(session.status),
                            pulsing: session.status == "running",
                            size: 4
                        )
                        Text(session.status)
                            .foregroundStyle(VPTheme.statusColor(session.status))
                    }
                } else {
                    Text("↵ send · ⇧↵ newline")
                        .foregroundStyle(VPTheme.textTertiary)
                }

                Text("⌘L focus")
                    .foregroundStyle(VPTheme.textTertiary.opacity(0.7))
            }
            .font(VPTheme.mono(7.5))
        }
        .padding(.horizontal, 13)
        .padding(.vertical, 10)
        .background(VPTheme.surfaceRaised, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .strokeBorder(
                    composerFocused
                        ? VPTheme.accent.opacity(0.55)
                        : (composerHovering ? VPTheme.hairlineStrong : VPTheme.hairline),
                    lineWidth: composerFocused ? 1.2 : 1
                )
        }
        .shadow(
            color: composerFocused ? VPTheme.accent.opacity(0.16) : .black.opacity(0.16),
            radius: composerFocused ? 20 : 10,
            y: composerFocused ? 6 : 3
        )
        .onHover { composerHovering = $0 }
        .animation(VPMotion.resolved(VPMotion.snap, reduceMotion: reduceMotion), value: composerFocused)
        .animation(VPMotion.resolved(VPMotion.hover, reduceMotion: reduceMotion), value: composerHovering)
        .frame(maxWidth: 900)
        .padding(.horizontal, 24)
        .padding(.bottom, 18)
        .padding(.top, 10)
    }

    private var sendDisabled: Bool {
        let textEmpty = draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        let status = model.selectedSession?.status
        let running = status == "running" || status == "paused" || status == "queued"
        return textEmpty || model.isBusy || model.compactBusy || running
    }

    private func send() {
        let goal = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !goal.isEmpty else { return }
        draft = ""
        if model.selectedSession != nil {
            Task { await model.sendMessage(goal) }
        } else {
            Task { await model.createSession(goal: goal) }
        }
    }
}

private func contextTokenLabel(_ value: Int) -> String {
    if value >= 1_000_000 {
        return String(format: "%.1fM", Double(value) / 1_000_000)
    }
    if value >= 1000 {
        return "\(value / 1000)k"
    }
    return "\(value)"
}

private struct ContextPopoverView: View {
    @Environment(AppModel.self) private var model

    private var sessionStatus: String {
        model.selectedSession?.status ?? "draft"
    }

    private var compactDisabled: Bool {
        model.compactBusy
            || ["running", "queued", "paused", "waiting_input", "imported"].contains(sessionStatus)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if let status = model.contextStatus, let usage = status.usage {
                HStack(alignment: .firstTextBaseline) {
                    Text("Context")
                        .font(.system(size: 13, weight: .semibold))
                    Spacer()
                    Text("\(contextTokenLabel(usage.estimatedTokens)) active")
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(
                            usage.estimatedTokens >= usage.thresholdTokens
                                ? Color.orange
                                : Color.secondary
                        )
                }

                VPGradientProgress(
                    value: contextProgress(usage),
                    tint: usage.estimatedTokens >= usage.hardLimitTokens
                        ? .red
                        : usage.estimatedTokens >= usage.thresholdTokens ? .orange : .accentColor,
                    height: 6
                )

                HStack(spacing: 18) {
                    metric("Window", contextTokenLabel(usage.contextWindow))
                    metric("Summary at", contextTokenLabel(usage.thresholdTokens))
                    metric("Hard limit", contextTokenLabel(usage.hardLimitTokens))
                }

                Text("\(usage.messages) active messages. Estimates are refreshed before each provider call.")
                    .font(.caption)
                    .foregroundStyle(.secondary)

                if let measurement = status.lastMeasurement {
                    Divider()
                    Text("LAST PROVIDER CALL")
                        .font(.system(size: 9, weight: .semibold, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    HStack(spacing: 18) {
                        metric("Estimated", contextTokenLabel(measurement.estimatedTokens))
                        metric("Actual input", contextTokenLabel(measurement.actualInputTokens))
                        metric("Calibration", String(format: "×%.2f", measurement.calibration))
                    }
                    Text(
                        "\(measurement.model) · \(measurement.samples) samples · ratio \(String(format: "%.2f", measurement.ratio))"
                    )
                    .font(.caption2)
                    .foregroundStyle(
                        measurement.ratio > 1.1
                            ? Color.orange
                            : Color.secondary
                    )
                    if measurement.ratio > 1.1 {
                        Text("The heuristic is undercounting this provider; compaction is scaled up safely.")
                            .font(.caption2)
                            .foregroundStyle(.orange)
                    }
                }

                Divider()

                if let compaction = status.lastCompaction {
                    Text("LAST COMPACTION")
                        .font(.system(size: 9, weight: .semibold, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    Text(
                        "\(compaction.mode) · \(compaction.summarizedMessages) summarized · \(compaction.droppedMessages) dropped · \(compaction.keptMessages) kept"
                    )
                    .font(.system(size: 11))
                    Text(
                        "\(contextTokenLabel(compaction.estimatedTokensBefore)) → \(contextTokenLabel(compaction.estimatedTokensAfter)) active tokens"
                    )
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.secondary)
                } else {
                    Text("No compaction yet.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                Divider()

                HStack(spacing: 10) {
                    Button(model.compactBusy ? "Compacting…" : "Compact now") {
                        Task { await model.compactSession() }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(compactDisabled)
                    if let notice = model.compactNotice {
                        Text(notice)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                }
                Text(
                    "Summarizes older messages and keeps the recent tail. Original messages remain searchable."
                )
                .font(.caption2)
                .foregroundStyle(.tertiary)
            } else {
                Text("Context usage has not been recorded for this session yet.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(16)
        .frame(width: 370)
        .background { VPCrystalBackdrop() }
        .tint(VPTheme.accent)
    }

    private func contextProgress(_ usage: ContextUsage) -> Double {
        guard usage.contextWindow > 0 else { return 0 }
        return min(1, Double(usage.estimatedTokens) / Double(usage.contextWindow))
    }

    private func metric(_ title: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title.uppercased())
                .font(.system(size: 8, weight: .semibold, design: .monospaced))
                .foregroundStyle(.tertiary)
            Text(value)
                .font(.system(size: 11, design: .monospaced))
        }
    }
}

private struct ForkSessionSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let session: Session
    @State private var goal = ""
    @State private var error: String?
    @State private var busy = false
    @State private var checkpointID = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 4) {
                Text("FORK")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .tracking(1.2)
                    .foregroundStyle(VPTheme.accent)
                Text("Fork Session")
                    .font(VPTheme.font(20, weight: .semibold, rounded: true))
                Text(
                    "Creates a new session from a checkpoint of the active conversation and worktree. The source session is not modified."
                )
                .font(VPTheme.font(10.5))
                .foregroundStyle(VPTheme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("START FROM")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                Picker("Start from", selection: $checkpointID) {
                    Text("Current worktree").tag("")
                    ForEach(model.checkpoints) { checkpoint in
                        Text(
                            "\(checkpoint.label) · \(String(checkpoint.commit.prefix(8))) · \(checkpoint.messageCount) msgs\(checkpoint.dirty == true ? " · dirty" : "")"
                        )
                        .tag(checkpoint.id)
                    }
                }
                .pickerStyle(.menu)
                .labelsHidden()
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("GOAL")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                TextField("Goal", text: $goal, axis: .vertical)
                    .textFieldStyle(.plain)
                    .font(VPTheme.font(12))
                    .lineLimit(2 ... 4)
                    .padding(9)
                    .vpFieldSurface(radius: 9)
            }

            if let error {
                Label(error, systemImage: "exclamationmark.triangle")
                    .font(VPTheme.mono(8.5))
                    .foregroundStyle(VPTheme.warning)
                    .fixedSize(horizontal: false, vertical: true)
            }

            HStack {
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button(busy ? "Forking…" : "Fork Session") {
                    Task { await fork() }
                }
                .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                .keyboardShortcut(.defaultAction)
                .disabled(busy || model.forkBusy || goal.trimmingCharacters(in: .whitespacesAndNewlines)
                    .isEmpty)
            }
        }
        .padding(22)
        .frame(width: 540)
        .background { VPCrystalBackdrop() }
        .tint(VPTheme.accent)
        .onAppear {
            goal = "\(session.goal) (fork)"
            checkpointID = ""
            error = nil
            Task { await model.loadCheckpoints() }
        }
    }

    private func fork() async {
        busy = true
        error = nil
        let success = await model.forkSession(
            goal: goal,
            checkpointID: checkpointID.isEmpty ? nil : checkpointID
        )
        busy = false
        if success {
            dismiss()
        } else {
            error = "Fork failed. The session must be idle and its worktree must be available."
        }
    }
}

struct SessionChatView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let session: Session

    private var entries: [ChatEntry] {
        var result: [ChatEntry] = []
        var runningTools: [String: (name: String, input: JSONValue, agent: String?)] = [:]
        var resolvedApprovals: Set<String> = []

        for event in model.events {
            switch event.type {
            case "session.forked":
                if let source = event.payload.objectValue?["source_session_id"]?.stringValue,
                   let commit = event.payload.objectValue?["commit"]?.stringValue
                {
                    result.append(
                        .notice(
                            id: event.id,
                            text: "Forked from \(String(source.prefix(8))) at \(String(commit.prefix(8)))"
                        )
                    )
                } else if let target = event.payload.objectValue?["fork_session_id"]?.stringValue {
                    result.append(
                        .notice(
                            id: event.id,
                            text: "Forked to session \(String(target.prefix(8)))"
                        )
                    )
                }
            case "message.created":
                guard event.payload.objectValue?["role"]?.stringValue == "user",
                      let content = event.payload.objectValue?["content"]?.stringValue
                else { continue }
                result.append(.user(id: event.id, text: content))
            case "agent.message":
                guard let text = event.payload.objectValue?["text"]?.stringValue,
                      !text.isEmpty
                else { continue }
                result.append(.assistant(id: event.id, text: text, agent: agentLabel(event)))
            case "tool.started":
                guard let id = event.payload.objectValue?["tool_call_id"]?.stringValue,
                      let name = event.payload.objectValue?["name"]?.stringValue
                else { continue }
                let input = event.payload.objectValue?["input"] ?? .null
                runningTools[id] = (name, input, agentLabel(event))
            case "tool.finished":
                guard let id = event.payload.objectValue?["tool_call_id"]?.stringValue,
                      let name = event.payload.objectValue?["name"]?.stringValue
                else { continue }
                let output = event.payload.objectValue?["output"]?.stringValue ?? ""
                let isError = event.payload.objectValue?["is_error"]?.boolValue ?? false
                let meta = runningTools[id]
                result.append(
                    .tool(
                        id: event.id,
                        name: name,
                        input: meta?.input ?? .null,
                        output: output,
                        isError: isError,
                        agent: meta?.agent ?? agentLabel(event)
                    )
                )
            case "question.requested":
                guard let question = try? event.payload.decode(Question.self) else { continue }
                result.append(.question(id: question.id))
            case "approval.requested":
                guard let approval = try? event.payload.decode(Approval.self) else { continue }
                result.append(.approval(approval))
            case "approval.resolved":
                if let id = event.payload.objectValue?["approval_id"]?.stringValue {
                    resolvedApprovals.insert(id)
                }
            case "agent.failed", "agent.unavailable":
                let message = event.payload.objectValue?["error"]?.stringValue
                    ?? event.payload.objectValue?["message"]?.stringValue
                    ?? "The agent stopped."
                result.append(.error(id: event.id, text: message))
            case "agent.budget_warning":
                let scope = event.payload.objectValue?["scope"]?.stringValue ?? "session"
                let reason = event.payload.objectValue?["reason"]?.stringValue ?? "tokens"
                let text: String
                if reason == "cost" {
                    let spent = event.payload.objectValue?["cost_spent_usd"]?.doubleValue ?? 0
                    let limit = event.payload.objectValue?["cost_limit_usd"]?.doubleValue ?? 0
                    text = "\(scope) USD budget is at 80% ($"
                        + String(format: "%.4f", spent) + " / $"
                        + String(format: "%.2f", limit) + ")."
                } else {
                    let spent = event.payload.objectValue?["spent"]?.intValue ?? 0
                    let limit = event.payload.objectValue?["limit"]?.intValue ?? 0
                    text = "\(scope) token budget is at 80% (\(spent) / \(limit))."
                }
                result.append(.notice(id: event.id, text: text))
            case "agent.budget_exceeded":
                let reason = event.payload.objectValue?["reason"]?.stringValue ?? "tokens"
                let text: String
                switch reason {
                case "cost":
                    let limit = event.payload.objectValue?["cost_limit_usd"]?.doubleValue ?? 0
                    let spent = event.payload.objectValue?["cost_spent_usd"]?.doubleValue ?? 0
                    text = "USD budget exhausted ($\(String(format: "%.4f", spent)) / $"
                        + String(format: "%.2f", limit) + ")."
                case "time":
                    let limit = event.payload.objectValue?["time_budget_secs"]?.intValue ?? 0
                    text = "Wall-clock budget exhausted (\(limit)s)."
                default:
                    let limit = event.payload.objectValue?["limit"]?.intValue ?? 0
                    let spent = event.payload.objectValue?["spent"]?.intValue ?? 0
                    text = "Token budget exhausted (\(spent) / \(limit))."
                }
                result.append(.notice(id: event.id, text: text))
            case "agent.cancelled":
                result.append(.notice(id: event.id, text: "Run cancelled."))
            default:
                continue
            }
        }

        return result.map { entry in
            if case let .approval(approval) = entry,
               resolvedApprovals.contains(approval.id)
            {
                return .approvalResolved(approval)
            }
            return entry
        }
    }

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 12) {
                    transcriptHeader

                    if entries.isEmpty, model.streamingText.isEmpty {
                        emptyState
                    }
                    ForEach(entries) { entry in
                        entryView(entry)
                            .id(entry.id)
                            .transition(.vpRise)
                    }
                    if !model.streamingText.isEmpty {
                        assistantBubble(model.streamingText, streaming: true)
                            .id("streaming")
                            .transition(.vpRise)
                    } else if session.status == "running" {
                        VPThinkingIndicator(label: "Leader is thinking…", tint: VPTheme.accent)
                            .padding(.vertical, 4)
                            .transition(.vpContent)
                    } else if session.status == "queued" {
                        HStack(spacing: 8) {
                            VPStatusDot(color: .yellow, pulsing: true, size: 6)
                            Text("Queued for a global session slot")
                                .font(.system(size: 10, design: .monospaced))
                                .foregroundStyle(.secondary)
                        }
                        .padding(.vertical, 4)
                        .transition(.vpContent)
                    } else if session.status == "paused" {
                        HStack(spacing: 8) {
                            VPStatusDot(color: .yellow, size: 6)
                            Text("Leader is paused")
                                .font(.system(size: 10, design: .monospaced))
                                .foregroundStyle(.secondary)
                        }
                        .padding(.vertical, 4)
                        .transition(.vpContent)
                    }
                }
                .padding(.horizontal, 22)
                .padding(.top, 14)
                .padding(.bottom, 20)
                .frame(maxWidth: 880)
                .frame(maxWidth: .infinity)
                .animation(
                    VPMotion.resolved(VPMotion.smooth, reduceMotion: reduceMotion),
                    value: entries.count
                )
                .animation(
                    VPMotion.resolved(VPMotion.hover, reduceMotion: reduceMotion),
                    value: model.streamingText.isEmpty
                )
            }
            .onChange(of: entries.count) { _, _ in
                if let last = entries.last {
                    withAnimation(VPMotion.resolved(VPMotion.smooth, reduceMotion: reduceMotion)) {
                        proxy.scrollTo(last.id, anchor: .bottom)
                    }
                }
            }
            .onChange(of: model.streamingText) { _, _ in
                withAnimation(VPMotion.resolved(VPMotion.hover, reduceMotion: reduceMotion)) {
                    proxy.scrollTo("streaming", anchor: .bottom)
                }
            }
        }
    }

    private var transcriptHeader: some View {
        HStack(spacing: 9) {
            Text("TRANSCRIPT")
                .font(VPTheme.mono(8, weight: .semibold))
                .tracking(1.0)
                .foregroundStyle(VPTheme.textTertiary)
            Rectangle()
                .fill(VPTheme.hairline)
                .frame(height: 1)
            Text("\(entries.count) entr\(entries.count == 1 ? "y" : "ies")")
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
                .contentTransition(.numericText())
            if session.status == "running" {
                VPStatusChip(status: "running", label: "live")
            }
        }
        .padding(.bottom, 2)
    }

    private func agentLabel(_ event: Event) -> String? {
        guard let role = event.payload.objectValue?["agent_role"]?.stringValue,
              role != "leader"
        else {
            return nil
        }
        let name = event.payload.objectValue?["agent_name"]?.stringValue
        let title = event.payload.objectValue?["agent_title"]?.stringValue
        let label = title ?? name
        return label.map { "\(role.capitalized) · \($0)" } ?? role.capitalized
    }

    private var emptyState: some View {
        VStack(spacing: 12) {
            VPAgentAvatar(seed: "leader", role: "leader", size: 44, active: true)
                .vpAppear(index: 0, animation: VPMotion.emphasizedSpring)
            Text("Talk to the Leader")
                .font(VPTheme.editorial(20))
                .foregroundStyle(VPTheme.textPrimary)
                .vpAppear(index: 1)
            Text(
                "The Leader can inspect files and run commands. Write and shell calls ask for your approval before they run."
            )
            .font(VPTheme.font(11.5))
            .foregroundStyle(VPTheme.textSecondary)
            .multilineTextAlignment(.center)
            .frame(maxWidth: 380)
            .vpAppear(index: 2)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 48)
    }

    @ViewBuilder
    private func entryView(_ entry: ChatEntry) -> some View {
        switch entry {
        case let .user(_, text):
            HStack(alignment: .bottom, spacing: 8) {
                Spacer(minLength: 80)
                Text(text)
                    .font(VPTheme.font(12.5))
                    .foregroundStyle(VPTheme.textPrimary)
                    .lineSpacing(2)
                    .textSelection(.enabled)
                    .padding(.horizontal, 13)
                    .padding(.vertical, 10)
                    .background(
                        VPTheme.accent.opacity(0.13),
                        in: RoundedRectangle(cornerRadius: 13, style: .continuous)
                    )
                    .overlay {
                        RoundedRectangle(cornerRadius: 13, style: .continuous)
                            .strokeBorder(VPTheme.accent.opacity(0.18), lineWidth: 1)
                    }
            }
        case let .assistant(_, text, agent):
            assistantBubble(text, agent: agent)
        case let .tool(_, name, input, output, isError, agent):
            ToolCallCard(
                name: name,
                input: input,
                output: output,
                isError: isError,
                agent: agent
            )
        case let .question(id):
            QuestionCard(questionID: id)
        case let .approval(approval):
            ApprovalCard(approval: approval, resolved: false)
        case let .approvalResolved(approval):
            ApprovalCard(approval: approval, resolved: true)
        case let .notice(_, text):
            HStack(spacing: 8) {
                Image(systemName: "info.circle")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                Text(text)
                    .font(VPTheme.mono(9))
                    .foregroundStyle(VPTheme.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 7)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 9, style: .continuous))
            .overlay {
                RoundedRectangle(cornerRadius: 9, style: .continuous)
                    .strokeBorder(VPTheme.hairline, lineWidth: 1)
            }
        case let .error(_, text):
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(VPTheme.warning)
                    .padding(.top, 1)
                Text(text)
                    .font(VPTheme.mono(9))
                    .foregroundStyle(VPTheme.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                VPTheme.warning.opacity(0.07),
                in: RoundedRectangle(cornerRadius: 9, style: .continuous)
            )
            .overlay {
                RoundedRectangle(cornerRadius: 9, style: .continuous)
                    .strokeBorder(VPTheme.warning.opacity(0.18), lineWidth: 1)
            }
        }
    }

    private func assistantBubble(
        _ text: String,
        agent: String? = nil,
        streaming: Bool = false
    ) -> some View {
        let role = role(for: agent)
        return HStack(alignment: .top, spacing: 10) {
            VPAgentAvatar(
                seed: agent ?? "leader",
                role: role,
                size: 26,
                active: streaming
            )

            VStack(alignment: .leading, spacing: 5) {
                HStack(spacing: 6) {
                    Text(agent ?? "Leader")
                        .font(VPTheme.mono(8, weight: .semibold))
                        .foregroundStyle(VPTheme.roleColor(role))
                    if streaming {
                        Text("WRITING")
                            .font(VPTheme.mono(7, weight: .semibold))
                            .tracking(0.8)
                            .foregroundStyle(VPTheme.accent.opacity(0.8))
                    }
                }

                HStack(alignment: .bottom, spacing: 3) {
                    Text(text)
                        .font(VPTheme.font(12.5))
                        .foregroundStyle(VPTheme.textPrimary)
                        .lineSpacing(3)
                        .textSelection(.enabled)
                    if streaming {
                        VPStreamingCaret(tint: VPTheme.accent)
                    }
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
                .background(
                    VPTheme.surface.opacity(streaming ? 0.9 : 0.55),
                    in: RoundedRectangle(cornerRadius: 12, style: .continuous)
                )
                .overlay {
                    RoundedRectangle(cornerRadius: 12, style: .continuous)
                        .strokeBorder(
                            streaming ? VPTheme.accent.opacity(0.24) : VPTheme.hairline,
                            lineWidth: 1
                        )
                }
                .animation(VPMotion.hover, value: streaming)
            }

            Spacer(minLength: 40)
        }
    }

    private func role(for agent: String?) -> String {
        let value = (agent ?? "").lowercased()
        if value.contains("manager") {
            return "manager"
        }
        if value.contains("worker") {
            return "worker"
        }
        if value.contains("verifier") {
            return "verifier"
        }
        if value.contains("labour") {
            return "labour"
        }
        return "leader"
    }
}

private enum ChatEntry: Identifiable {
    case user(id: String, text: String)
    case assistant(id: String, text: String, agent: String?)
    case tool(
        id: String,
        name: String,
        input: JSONValue,
        output: String,
        isError: Bool,
        agent: String?
    )
    case approval(Approval)
    case approvalResolved(Approval)
    case question(id: String)
    case notice(id: String, text: String)
    case error(id: String, text: String)

    var id: String {
        switch self {
        case let .user(id, _), let .assistant(id, _, _), let .tool(id, _, _, _, _, _),
             let .notice(id, _), let .error(id, _):
            id
        case let .approval(approval), let .approvalResolved(approval):
            "approval-\(approval.id)"
        case let .question(id):
            "question-\(id)"
        }
    }
}

private struct QuestionCard: View {
    @Environment(AppModel.self) private var model
    let questionID: String
    @State private var draft = ""
    @FocusState private var answerFocused: Bool

    private var question: Question? {
        model.questions.first { $0.id == questionID }
    }

    var body: some View {
        if let question {
            VStack(alignment: .leading, spacing: 10) {
                HStack(alignment: .top, spacing: 9) {
                    ZStack {
                        RoundedRectangle(cornerRadius: 7, style: .continuous)
                            .fill(VPTheme.info.opacity(0.13))
                            .frame(width: 26, height: 26)
                        Image(systemName: "questionmark.bubble")
                            .font(.system(size: 11, weight: .semibold))
                            .foregroundStyle(VPTheme.info)
                    }

                    VStack(alignment: .leading, spacing: 2) {
                        Text("The team needs an answer")
                            .font(VPTheme.font(12, weight: .semibold))
                            .foregroundStyle(VPTheme.textPrimary)
                        Text(question.agentName)
                            .font(VPTheme.mono(8))
                            .foregroundStyle(VPTheme.textTertiary)
                    }

                    Spacer()

                    VPStatusChip(status: question.status)
                }

                Text(question.question)
                    .font(VPTheme.font(11.5))
                    .foregroundStyle(VPTheme.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)

                if question.isPending {
                    if !question.options.isEmpty {
                        VPFlowLayout(spacing: 6) {
                            ForEach(question.options, id: \.self) { option in
                                Button {
                                    Task { await model.answerQuestion(question.id, answer: option) }
                                } label: {
                                    Text(option)
                                        .font(VPTheme.font(10, weight: .medium))
                                        .foregroundStyle(VPTheme.textPrimary)
                                        .padding(.horizontal, 9)
                                        .padding(.vertical, 5)
                                        .background(
                                            VPTheme.surfaceRaised,
                                            in: Capsule()
                                        )
                                        .overlay {
                                            Capsule().strokeBorder(VPTheme.hairline, lineWidth: 1)
                                        }
                                }
                                .buttonStyle(VPPressableButtonStyle(scale: 0.96))
                            }
                        }
                    }

                    HStack(spacing: 7) {
                        TextField("Type an answer", text: $draft)
                            .textFieldStyle(.plain)
                            .font(VPTheme.font(10.5))
                            .focused($answerFocused)
                            .onSubmit {
                                submit(question)
                            }
                            .padding(.horizontal, 9)
                            .padding(.vertical, 6)
                            .vpFieldSurface(radius: 8, focused: answerFocused)

                        Button("Send") {
                            submit(question)
                        }
                        .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                        .disabled(
                            draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                        )
                    }
                } else if let answer = question.answer {
                    HStack(spacing: 6) {
                        Image(systemName: "checkmark.circle.fill")
                            .font(.system(size: 9))
                            .foregroundStyle(VPTheme.success)
                        Text(answer)
                            .font(VPTheme.mono(9))
                            .foregroundStyle(VPTheme.textSecondary)
                            .textSelection(.enabled)
                    }
                    .padding(8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(VPTheme.success.opacity(0.06), in: RoundedRectangle(cornerRadius: 8))
                }
            }
            .padding(12)
            .background(
                VPTheme.info.opacity(question.isPending ? 0.055 : 0.025),
                in: RoundedRectangle(cornerRadius: 12, style: .continuous)
            )
            .overlay {
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .strokeBorder(
                        VPTheme.info.opacity(question.isPending ? 0.22 : 0.08),
                        lineWidth: 1
                    )
            }
            .vpGlowBorder(VPTheme.info, radius: 12, active: question.isPending)
            .vpHoverLift(scale: 1.003, lift: 1, glow: VPTheme.info)
        }
    }

    private func submit(_ question: Question) {
        let answer = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !answer.isEmpty else { return }
        draft = ""
        Task { await model.answerQuestion(question.id, answer: answer) }
    }
}

struct ToolCallCard: View {
    let name: String
    let input: JSONValue
    let output: String
    let isError: Bool
    let agent: String?

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var expanded = false

    private var inputText: String {
        input == .null ? "" : input.prettyString()
    }

    private var outputText: String {
        output.isEmpty ? "(no output)" : output
    }

    private var canExpand: Bool {
        output.count > 420 || outputText.split(separator: "\n").count > 6
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 9) {
                ZStack {
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .fill((isError ? VPTheme.danger : VPTheme.info).opacity(0.12))
                        .frame(width: 22, height: 22)
                    Image(systemName: isError ? "exclamationmark.wrench" : "wrench.and.screwdriver")
                        .font(.system(size: 9, weight: .semibold))
                        .foregroundStyle(isError ? VPTheme.danger : VPTheme.info)
                }

                VStack(alignment: .leading, spacing: 1) {
                    Text(name)
                        .font(VPTheme.mono(9.5, weight: .semibold))
                        .foregroundStyle(VPTheme.textPrimary)
                    if let agent {
                        Text(agent)
                            .font(VPTheme.mono(7.5))
                            .foregroundStyle(VPTheme.textTertiary)
                            .lineLimit(1)
                    }
                }

                Spacer(minLength: 6)

                VPStatusChip(status: isError ? "failed" : "done", label: isError ? "failed" : "done")

                if canExpand {
                    Button {
                        withAnimation(
                            VPMotion.resolved(VPMotion.snap, reduceMotion: reduceMotion)
                        ) {
                            expanded.toggle()
                        }
                    } label: {
                        Image(systemName: "chevron.down")
                            .font(.system(size: 8, weight: .bold))
                            .foregroundStyle(VPTheme.textTertiary)
                            .rotationEffect(.degrees(expanded ? 180 : 0))
                            .frame(width: 18, height: 18)
                    }
                    .buttonStyle(VPPressableButtonStyle(scale: 0.9))
                    .help(expanded ? "Collapse output" : "Expand output")
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)

            if !inputText.isEmpty {
                VPHairline()
                Text(inputText)
                    .font(VPTheme.mono(8.5))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(2)
                    .textSelection(.enabled)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 7)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(VPTheme.surfaceSunken.opacity(0.7))
            }

            VPHairline()
            Text(outputText)
                .font(VPTheme.mono(9))
                .foregroundStyle(VPTheme.textSecondary)
                .lineLimit(expanded ? nil : 6)
                .textSelection(.enabled)
                .padding(.horizontal, 10)
                .padding(.vertical, 8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .animation(VPMotion.resolved(VPMotion.snap, reduceMotion: reduceMotion), value: expanded)
        }
        .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 11, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 11, style: .continuous)
                .strokeBorder(
                    isError ? VPTheme.danger.opacity(0.22) : VPTheme.hairline,
                    lineWidth: 1
                )
        }
        .vpHoverLift(scale: 1.003, lift: 1, glow: isError ? VPTheme.danger : VPTheme.info)
    }
}

struct ApprovalCard: View {
    private func agentLabel(role: String?, name: String) -> String {
        guard let role, role != "leader" else { return name }
        return "\(role.capitalized) · \(name)"
    }

    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let approval: Approval
    let resolved: Bool
    @State private var pending = false

    private var isWaiting: Bool {
        !resolved && approval.status == "pending"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .top, spacing: 9) {
                ZStack {
                    RoundedRectangle(cornerRadius: 7, style: .continuous)
                        .fill(VPTheme.warning.opacity(0.13))
                        .frame(width: 26, height: 26)
                    Image(systemName: "lock.shield")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(VPTheme.warning)
                }

                VStack(alignment: .leading, spacing: 2) {
                    Text("Permission required")
                        .font(VPTheme.font(12, weight: .semibold))
                        .foregroundStyle(VPTheme.textPrimary)
                    HStack(spacing: 5) {
                        Text(approval.tool)
                            .foregroundStyle(VPTheme.textTertiary)
                        if let agentName = approval.agentName {
                            Text("·")
                            Text(
                                agentLabel(
                                    role: approval.agentRole,
                                    name: approval.agentTitle ?? agentName
                                )
                            )
                        }
                    }
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
                }

                Spacer(minLength: 6)

                VPStatusChip(
                    status: isWaiting ? "pending" : "approved",
                    label: isWaiting ? "waiting" : "resolved"
                )
            }

            Text(approval.input.prettyString())
                .font(VPTheme.mono(9))
                .foregroundStyle(VPTheme.textSecondary)
                .lineLimit(6)
                .textSelection(.enabled)
                .padding(9)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(VPTheme.surfaceSunken, in: RoundedRectangle(cornerRadius: 8))
                .overlay {
                    RoundedRectangle(cornerRadius: 8)
                        .strokeBorder(VPTheme.hairline, lineWidth: 1)
                }

            if approval.tool == "shell" {
                Text(
                    "Shell policy: network \(model.shellPolicy.network) · \(model.shellPolicy.readDeny.count) extra read-deny paths. Writes stay confined to the worktree, Git metadata, and runtime scratch; common credential/state paths are read-denied."
                )
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
            }

            if approval.agentRole == "verifier", approval.tool == "shell" {
                Label(
                    "The Verifier can still modify its own worktree through shell. Approve only read-only commands.",
                    systemImage: "exclamationmark.triangle"
                )
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.warning)
                .fixedSize(horizontal: false, vertical: true)
            }

            if isWaiting {
                HStack(spacing: 7) {
                    Button("Deny") {
                        pending = true
                        Task { await model.resolveApproval(approval.id, approved: false) }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)

                    Button("Approve") {
                        pending = true
                        Task { await model.resolveApproval(approval.id, approved: true) }
                    }
                    .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                    .controlSize(.small)

                    if pending {
                        ProgressView()
                            .controlSize(.small)
                            .transition(.vpScaleFade)
                    }

                    Spacer()

                    Text("blocking the team")
                        .font(VPTheme.mono(7.5))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                .disabled(pending)
            } else {
                HStack(spacing: 6) {
                    Image(systemName: "checkmark.circle.fill")
                        .font(.system(size: 9))
                        .foregroundStyle(VPTheme.success)
                    Text("Approval resolved")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
        }
        .padding(12)
        .background(
            VPTheme.warning.opacity(isWaiting ? 0.055 : 0.025),
            in: RoundedRectangle(cornerRadius: 12, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(
                    VPTheme.warning.opacity(isWaiting ? 0.22 : 0.08),
                    lineWidth: 1
                )
        }
        .vpGlowBorder(VPTheme.warning, radius: 12, active: isWaiting)
        .vpHoverLift(scale: 1.003, lift: 1, glow: VPTheme.warning)
        .animation(
            VPMotion.resolved(VPMotion.snap, reduceMotion: reduceMotion),
            value: pending
        )
    }
}

private struct WelcomeStageView: View {
    @Environment(AppModel.self) private var model
    @Environment(BrandStore.self) private var brand

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                HStack(spacing: 8) {
                    BrandMark(config: brand.config, fontSize: 19)
                    Spacer()
                    ConnectionBadge(state: model.connection)
                }
                .padding(.bottom, 44)

                Text("AGENT STUDIO")
                    .font(VPTheme.mono(9, weight: .semibold))
                    .tracking(1.6)
                    .foregroundStyle(VPTheme.accent)
                    .padding(.bottom, 12)
                    .vpAppear(index: 0)

                Text("Agent teams,\nnot chat tabs.")
                    .font(VPTheme.editorial(44))
                    .foregroundStyle(VPTheme.textPrimary)
                    .lineSpacing(1)
                    .padding(.bottom, 16)
                    .vpAppear(index: 1, animation: VPMotion.emphasizedSpring)

                Text(
                    "Open a repository to start. VerlinPark isolates every task in a worktree, puts a Verifier in front of every merge, and keeps the whole run inspectable."
                )
                .font(VPTheme.font(13))
                .foregroundStyle(VPTheme.textSecondary)
                .lineSpacing(3)
                .frame(maxWidth: 540, alignment: .leading)
                .padding(.bottom, 30)
                .vpAppear(index: 2)

                HStack(spacing: 10) {
                    welcomeFeature(
                        "Isolated worktrees",
                        "Every task gets its own branch and write scope.",
                        icon: "arrow.triangle.branch",
                        tint: VPTheme.resident
                    )
                    welcomeFeature(
                        "Verifier gate",
                        "Approval is required before a Worker branch merges.",
                        icon: "checkmark.shield",
                        tint: VPTheme.warning
                    )
                    welcomeFeature(
                        "Deterministic merge",
                        "A serial integrator checks and rolls back on failure.",
                        icon: "arrow.triangle.merge",
                        tint: VPTheme.info
                    )
                }
                .frame(maxWidth: 720)
                .padding(.bottom, 30)
                .vpAppear(index: 3)

                HStack(spacing: 12) {
                    Button {
                        Task { await model.addProject() }
                    } label: {
                        Label("Add Project", systemImage: "folder.badge.plus")
                    }
                    .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                    .controlSize(.large)
                    .disabled(model.connection != .connected)
                    .vpAppear(index: 4)

                    Text("⇧⌘O")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                }

                Spacer(minLength: 40)

                HStack(spacing: 7) {
                    VPStatusDot(color: VPTheme.accent, size: 4)
                    Text("Leader → Manager → Worker → Labour")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                    Text("·")
                        .foregroundStyle(VPTheme.textTertiary.opacity(0.5))
                    Text("local-first · user-configured models")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                .vpAppear(index: 5)
            }
            .padding(.horizontal, 48)
            .padding(.vertical, 40)
            .frame(maxWidth: 880, alignment: .leading)
            .frame(maxWidth: .infinity, minHeight: 680, alignment: .leading)
        }
    }

    private func welcomeFeature(
        _ title: String,
        _ detail: String,
        icon: String,
        tint: Color
    ) -> some View {
        VStack(alignment: .leading, spacing: 9) {
            Image(systemName: icon)
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(tint)
                .frame(width: 28, height: 28)
                .background(tint.opacity(0.10), in: RoundedRectangle(cornerRadius: 8))
            Text(title)
                .font(VPTheme.font(11.5, weight: .semibold))
                .foregroundStyle(VPTheme.textPrimary)
            Text(detail)
                .font(VPTheme.font(9.5))
                .foregroundStyle(VPTheme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(12)
        .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(tint.opacity(0.14), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.015, lift: 2, glow: tint)
    }
}
