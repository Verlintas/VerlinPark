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
        .background {
            VPCrystalSurface(tone: VPTheme.canvas, solidTone: VPTheme.solidCanvas, tintOpacity: 0.56)
        }
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
    @State private var showingToolActivity = false
    @State private var showingGui = false
    @State private var showingProcesses = false
    @State private var showingFork = false
    @State private var showingSteer = false
    @State private var showingDefer = false
    @FocusState private var composerFocused: Bool
    @State private var composerHovering = false

    var body: some View {
        VStack(spacing: 0) {
            stageHeader
            VPHairline()
            if let session = model.selectedSession,
               let preemption = model.sessionPreemption,
               preemption.preempted,
               ["cancelled", "failed"].contains(session.status)
            {
                PreemptionBanner(result: preemption)
                    .padding(.horizontal, 18)
                    .padding(.vertical, 9)
                VPHairline()
            }
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
        .task(id: model.selectedProjectID) {
            if model.selectedProjectID != nil {
                await model.loadSkills(silent: true)
            }
        }
        .task(id: model.selectedSessionID) {
            await model.refreshProcessSummary()
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(5))
                await model.refreshProcessSummary()
            }
        }
        .sheet(isPresented: $showingToolActivity) {
            ToolActivitySheet()
                .environment(model)
        }
        .sheet(isPresented: $showingFork) {
            if let session = model.selectedSession {
                ForkSessionSheet(session: session)
                    .environment(model)
            }
        }
        .sheet(isPresented: $showingSteer) {
            if let session = model.selectedSession {
                SteerSheet(session: session)
                    .environment(model)
            }
        }
        .sheet(isPresented: $showingDefer) {
            if let session = model.selectedSession {
                DeferSheet(session: session)
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

    private var toolActivityLabel: String {
        guard let activity = model.toolActivity else { return "tools" }
        if activity.summary.errors > 0 {
            return "tools \(activity.summary.calls) · \(activity.summary.errors) err"
        }
        return "tools \(activity.summary.calls)"
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
                        Text(
                            info.configured
                                ? (model.daemonModelLabel ?? "MODEL")
                                : L10n.t("NO MODEL")
                        )
                        .font(VPTheme.mono(8, weight: .medium))
                        .foregroundStyle(info.configured ? VPTheme.textTertiary : VPTheme.warning)
                        .help(
                            model.daemonModelSource.map {
                                "Daemon-configured Leader model from \($0); local provider settings are not used for a live run."
                            } ?? "Daemon-configured model."
                        )
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
                        Button {
                            showingToolActivity = true
                        } label: {
                            HStack(spacing: 4) {
                                Image(systemName: "wrench.and.screwdriver")
                                    .font(.system(size: 8, weight: .semibold))
                                Text(toolActivityLabel)
                            }
                        }
                        .buttonStyle(.plain)
                        .help(
                            "Per-tool latency, stable error codes, retryability, output bytes, and truncation"
                        )
                        if model.activeProcessCount > 0 {
                            Button {
                                showingProcesses.toggle()
                            } label: {
                                HStack(spacing: 4) {
                                    Image(systemName: "gearshape.2")
                                        .font(.system(size: 8, weight: .semibold))
                                    Text("\(model.activeProcessCount) proc")
                                }
                                .foregroundStyle(VPTheme.info)
                            }
                            .buttonStyle(.plain)
                            .help("Durable supervised processes for this session")
                            .popover(isPresented: $showingProcesses, arrowEdge: .bottom) {
                                StageProcessPopover()
                                    .environment(model)
                            }
                        }
                        if model.hasLongTaskAttention {
                            longTaskAttentionPill
                        }
                        if let label = model.scalingStageLabel {
                            HStack(spacing: 4) {
                                Image(systemName: "arrow.up.right.circle")
                                    .font(.system(size: 8, weight: .semibold))
                                Text(label)
                            }
                            .foregroundStyle(VPTheme.textTertiary)
                            .help(
                                "Deterministic organization scaling: current Worker/Labour slots, ready tasks, and the latest bounded decision. Managers remain plan-owned."
                            )
                        }
                        if let label = model.guiStageLabel {
                            Button {
                                showingGui.toggle()
                            } label: {
                                guiActivityPill(label)
                            }
                            .buttonStyle(.plain)
                            .popover(isPresented: $showingGui, arrowEdge: .bottom) {
                                GuiStagePopover()
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

                if let session = model.selectedSession,
                   ["running", "paused"].contains(session.status)
                {
                    Button {
                        showingSteer = true
                    } label: {
                        Image(systemName: "arrow.up.message")
                    }
                    .buttonStyle(VPGlassIconButtonStyle(size: 26))
                    .help("Steer run…")
                    .accessibilityLabel(L10n.t("Steer run"))
                }

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
                            Button("Preempt Now") {
                                Task {
                                    await model.preemptActiveRun(
                                        reason: "operator preemption from the Stage"
                                    )
                                }
                            }
                            .help(
                                "Hard-preempt the run: abort an in-flight provider stream or tool call with bounded cleanup."
                            )
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
                        Button(
                            session.unlimited
                                ? L10n.t("Restore Stored Budget Limits")
                                : L10n.t("Enable Unlimited Budget")
                        ) {
                            Task { await model.setSessionUnlimited(!session.unlimited) }
                        }
                        .disabled(session.status == "imported")
                        Divider()
                        Button("Fork Session…") { showingFork = true }
                        Button("Export Transcript…") {
                            Task { await model.exportTranscript() }
                        }
                        Button("Defer Continuation…") { showingDefer = true }
                            .disabled(["imported", "landed"].contains(session.status))
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

    private func guiActivityPill(_ label: String) -> some View {
        HStack(spacing: 4) {
            Image(systemName: model.guiHeldBySelectedSession ? "macwindow.on.rectangle" : "macwindow")
                .font(.system(size: 7.5, weight: .semibold))
            Text(label)
                .lineLimit(1)
        }
        .font(VPTheme.mono(7.5, weight: .semibold))
        .foregroundStyle(model.guiHeldBySelectedSession ? VPTheme.accent : VPTheme.textTertiary)
        .padding(.horizontal, 6)
        .padding(.vertical, 2)
        .background(
            (model.guiHeldBySelectedSession ? VPTheme.accent : VPTheme.textTertiary).opacity(0.10),
            in: Capsule()
        )
        .overlay {
            Capsule()
                .strokeBorder(
                    (model.guiHeldBySelectedSession ? VPTheme.accent : VPTheme.textTertiary)
                        .opacity(0.24),
                    lineWidth: 1
                )
        }
        .help(
            "Unified browser/computer GUI lease. Generation, latency, and errors are in Manage Dash → Team."
        )
        .accessibilityLabel(L10n.t("GUI session activity"))
    }

    private var longTaskAttentionPill: some View {
        HStack(spacing: 4) {
            Image(systemName: "hourglass")
                .font(.system(size: 7.5, weight: .semibold))
            if model.pendingContinuationCount > 0 {
                Text(
                    L10n.format(
                        "%lld deferred",
                        model.pendingContinuationCount
                    )
                )
            }
            if model.pendingContinuationCount > 0, model.activeRecoveryCount > 0 {
                Text("·")
            }
            if model.activeRecoveryCount > 0 {
                Text(L10n.format("%lld recovery", model.activeRecoveryCount))
            }
        }
        .font(VPTheme.mono(7.5, weight: .semibold))
        .foregroundStyle(VPTheme.warning)
        .padding(.horizontal, 6)
        .padding(.vertical, 2)
        .background(VPTheme.warning.opacity(0.10), in: Capsule())
        .overlay {
            Capsule().strokeBorder(VPTheme.warning.opacity(0.24), lineWidth: 1)
        }
        .help(
            L10n.t(
                "Deferred continuations waiting to start plus active stall recoveries."
            )
        )
        .accessibilityLabel(L10n.t("Long-task operations pending"))
    }

    private func tokenUsageLabel(_ session: Session) -> String {
        let used = tokenLabel(session.tokensUsed)
        var parts: [String] = []
        if session.unlimited {
            parts.append("\(used) tokens · unlimited budget")
            if session.costSpentUsd > 0 {
                parts.append(String(format: "$%.4f", session.costSpentUsd))
            }
            return parts.joined(separator: " · ")
        }
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
                if !enabledSkills.isEmpty {
                    Menu {
                        ForEach(enabledSkills) { skill in
                            Button {
                                insertSkill(skill)
                            } label: {
                                Label(
                                    "\(skill.name) · \(skill.description)",
                                    systemImage: "wand.and.stars"
                                )
                            }
                        }
                    } label: {
                        Image(systemName: "wand.and.stars")
                            .font(.system(size: 10, weight: .semibold))
                            .foregroundStyle(VPTheme.accent)
                            .frame(width: 24, height: 24)
                            .background(VPTheme.surfaceSunken, in: Circle())
                            .overlay {
                                Circle().strokeBorder(VPTheme.hairline, lineWidth: 1)
                            }
                    }
                    .menuStyle(.borderlessButton)
                    .menuIndicator(.hidden)
                    .fixedSize()
                    .help(L10n.t("Insert a project skill prompt"))
                }
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
                    Label(model.daemonModelLabel ?? "model", systemImage: "cpu")
                        .foregroundStyle(VPTheme.textTertiary)
                        .help("Model resolved from daemon agent.info, not local provider defaults.")
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

    private var enabledSkills: [ProjectSkill] {
        model.skills.filter(\.enabled)
    }

    private func insertSkill(_ skill: ProjectSkill) {
        let instruction = "Use the \"\(skill.name)\" skill. \(skill.description)\n\n"
        if draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            draft = instruction
        } else {
            draft = instruction + draft
        }
        composerFocused = true
    }

    private func send() {
        let goal = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !goal.isEmpty else { return }
        if model.selectedSession != nil {
            draft = ""
            Task { await model.sendMessage(goal) }
        } else {
            Task {
                if await model.createSession(goal: goal) {
                    draft = ""
                }
            }
        }
    }
}

private struct StageProcessPopover: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("Supervised processes")
                    .font(.system(size: 11, weight: .semibold))
                Spacer()
                Text("\(model.processSummary.count) active")
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            if model.processSummary.isEmpty {
                Text("No durable processes are running for this session.")
                    .font(VPTheme.mono(8.5))
                    .foregroundStyle(VPTheme.textTertiary)
                    .frame(maxWidth: 260, alignment: .leading)
            } else {
                ForEach(model.processSummary) { process in
                    HStack(spacing: 6) {
                        Circle()
                            .fill(process.state == "running" ? VPTheme.success : VPTheme.warning)
                            .frame(width: 5, height: 5)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(process.name)
                                .font(VPTheme.mono(8.5, weight: .medium))
                                .lineLimit(1)
                            Text(
                                [
                                    process.state,
                                    process.pid.map { "pid \($0)" },
                                    "\(process.outputBytes) bytes",
                                ].compactMap(\.self).joined(separator: " · ")
                            )
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.textTertiary)
                        }
                        Spacer()
                        Button("Stop") {
                            Task { await model.stopSupervisedProcess(process.id) }
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.mini)
                        .disabled(model.processStopBusy)
                    }
                }
            }
        }
        .padding(12)
        .frame(minWidth: 280)
        .task {
            await model.refreshProcessSummary()
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
                        : usage.estimatedTokens >= usage.thresholdTokens ? VPTheme.warning : VPTheme.accent,
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

                if let snapshot = model.efficiencySnapshot,
                   snapshot.cacheHitRatio != nil || snapshot.promptPrefixHash != nil
                {
                    CacheTelemetryPanel(snapshot: snapshot)
                    HStack(spacing: 8) {
                        GovernorPill(status: model.governorStatus)
                        if let prefix = EfficiencyMetrics.shortHash(snapshot.promptPrefixHash) {
                            Text("prompt prefix \(prefix)…")
                                .font(.system(size: 9, design: .monospaced))
                                .foregroundStyle(.tertiary)
                                .textSelection(.enabled)
                        }
                        Spacer()
                    }
                }

                if let effects = model.sessionEffects, effects.counts.total > 0 {
                    EffectBadge(report: effects)
                }

                if let gates = model.sessionGates, gates.counts.total > 0 {
                    GateBadge(report: gates)
                }

                if let instructions = model.sessionInstructions {
                    InstructionSectionsView(
                        report: instructions,
                        contract: model.sessionContract?.contract
                    )
                }

                if let governor = model.governorStatus,
                   governor.enabled || governor.planPresent
                {
                    Divider()
                    HStack(spacing: 8) {
                        Text("CRITICAL PATH BUDGET")
                            .font(.system(size: 9, weight: .semibold, design: .monospaced))
                            .foregroundStyle(.tertiary)
                        GovernorPill(status: governor)
                        Text(GovernorMetrics.modeLabel(governor.reserve.mode).uppercased())
                            .font(.system(size: 9, weight: .semibold, design: .monospaced))
                            .foregroundStyle(
                                governor.reserve.mode == "multi"
                                    ? VPTheme.accent : VPTheme.textTertiary
                            )
                        Spacer()
                    }
                    if let path = governor.criticalPath {
                        Text(
                            "Path: \(GovernorMetrics.pathLabel(path.taskIDs)) · \(EfficiencyMetrics.tokenLabel(path.weightTokens)) tokens"
                        )
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    }
                    HStack(spacing: 18) {
                        metric(
                            "Protected",
                            EfficiencyMetrics.tokenLabel(governor.reserve.protectedTokens)
                        )
                        metric(
                            "Remaining",
                            EfficiencyMetrics.tokenLabel(governor.reserve.remainingTokens)
                        )
                        metric("Deferred", "\(governor.deferred.count)")
                        if governor.reserve.mode == "multi" {
                            metric(
                                "Paths",
                                "\(governor.reserve.pathCount)/\(governor.policy.maxPaths)"
                            )
                            metric(
                                "Allocated",
                                EfficiencyMetrics.tokenLabel(governor.reserve.allocatedTokens)
                            )
                        }
                        if governor.reserve.throttled {
                            Text("THROTTLED")
                                .font(.system(size: 9, weight: .semibold, design: .monospaced))
                                .foregroundStyle(VPTheme.warning)
                        }
                    }
                    if governor.reserve.mode == "multi", !governor.paths.isEmpty {
                        VStack(alignment: .leading, spacing: 2) {
                            ForEach(Array(governor.paths.prefix(2))) { path in
                                Text(
                                    "  #\(path.pathID) \(GovernorMetrics.pathRow(path)) · \(GovernorMetrics.departmentLabel(path))"
                                )
                                .font(.system(size: 9, design: .monospaced))
                                .foregroundStyle(
                                    path.deficitTokens > 0
                                        ? VPTheme.warning : VPTheme.textSecondary
                                )
                                .lineLimit(1)
                            }
                            if governor.paths.count > 2 {
                                Text("  +\(governor.paths.count - 2) more protected paths")
                                    .font(.system(size: 8, design: .monospaced))
                                    .foregroundStyle(.tertiary)
                            }
                        }
                    }
                    if governor.reserve.mode == "multi", !governor.departments.isEmpty {
                        Text(
                            "Floors: "
                                + governor.departments.prefix(3).map { department in
                                    "\(department.department) \(EfficiencyMetrics.tokenLabel(department.floorTokens))"
                                }.joined(separator: " · ")
                        )
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                        .lineLimit(1)
                    }
                    if let last = governor.lastDecisions.last {
                        Text("Last decision: \(last.action) \(last.reason) · \(last.detail)")
                            .font(.caption2)
                            .foregroundStyle(.tertiary)
                            .lineLimit(2)
                    }
                    if let error = governor.error {
                        Text("\(error.code) · \(error.message)")
                            .font(.caption2)
                            .foregroundStyle(VPTheme.warning)
                            .lineLimit(2)
                    }
                    Text("Deferred tasks stay queued; hard budgets and provider routing are unchanged.")
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                }

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
                    if let latency = compaction.latencyMs {
                        Text(
                            "\(latency) ms · \(compaction.backgroundReuse == true ? "prepared summary reused" : compaction.fallbackToSync == true ? "fallback to sync" : "synchronous")"
                        )
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(.secondary)
                    }
                } else {
                    Text("No compaction yet.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                Divider()

                ContextCheckpointSection(style: .popover)

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
    @State private var transcript: [TranscriptEntry] = []
    @State private var activeCitation: SourceCitation?

    private var entries: [TranscriptEntry] {
        transcript
    }

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 12) {
                    transcriptHeader

                    if entries.isEmpty,
                       model.streamingText.isEmpty,
                       session.status != "running"
                    {
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
                        VPThinkingIndicator(label: L10n.t("Leader is thinking…"), tint: VPTheme.accent)
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
            .onAppear {
                transcript = TranscriptProjection.build(events: model.events)
            }
            .onChange(of: model.events) { _, events in
                transcript = TranscriptProjection.build(events: events)
            }
        }
        .sheet(item: $activeCitation) { citation in
            SourceCitationSheet(citation: citation)
                .environment(model)
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

    private var emptyState: some View {
        VStack(spacing: 12) {
            VPAgentAvatar(seed: "leader", role: "leader", size: 44, active: true)
                .vpAppear(index: 0, animation: VPMotion.emphasizedSpring)
            Text("Talk to the Leader")
                .font(VPTheme.editorial(20))
                .foregroundStyle(VPTheme.textPrimary)
                .vpAppear(index: 1)
            Text(
                "The Leader can inspect and edit the worktree, run ordinary commands, and ask only before host-level changes."
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
    private func entryView(_ entry: TranscriptEntry) -> some View {
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
        case let .assistant(_, text, agent, citations):
            assistantBubble(text, agent: agent, citations: citations)
        case let .tool(entry):
            ToolTraceRow(entry: entry)
        case let .question(id):
            QuestionCard(questionID: id)
        case let .approval(entry):
            ApprovalNotice(entry: entry)
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
        streaming: Bool = false,
        citations: [SourceCitation] = []
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
                    Text(L10n.t(agent ?? "Leader"))
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
                    Text(citationAttributedText(text, citations: citations))
                        .font(VPTheme.font(12.5))
                        .foregroundStyle(VPTheme.textPrimary)
                        .lineSpacing(3)
                        .tint(VPTheme.accent)
                        .textSelection(.enabled)
                        .environment(\.openURL, OpenURLAction { url in
                            guard url.scheme == "verlinpark",
                                  url.host == "citation"
                            else { return .systemAction }
                            let key = url.lastPathComponent
                            if let citation = citations.first(where: { $0.key == key }) {
                                activeCitation = citation
                                return .handled
                            }
                            return .systemAction
                        })
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

                if !citations.isEmpty {
                    citationStrip(citations)
                }
            }

            Spacer(minLength: 40)
        }
    }

    private func citationAttributedText(
        _ text: String,
        citations: [SourceCitation]
    ) -> AttributedString {
        guard !citations.isEmpty else { return AttributedString(text) }
        let keys = Set(citations.map(\.key))
        let regex = try? NSRegularExpression(pattern: "\\[(c\\d+)\\]")
        guard let regex else { return AttributedString(text) }
        var result = AttributedString()
        var cursor = text.startIndex
        let fullRange = NSRange(text.startIndex..., in: text)
        for match in regex.matches(in: text, range: fullRange) {
            guard let range = Range(match.range, in: text),
                  let keyRange = Range(match.range(at: 1), in: text)
            else { continue }
            result += AttributedString(String(text[cursor ..< range.lowerBound]))
            let key = String(text[keyRange])
            if keys.contains(key) {
                var marker = AttributedString("[\(key)]")
                marker.link = URL(string: "verlinpark://citation/\(key)")
                marker.foregroundColor = VPTheme.accent
                marker.font = VPTheme.mono(9.5, weight: .semibold)
                result += marker
            } else {
                result += AttributedString(String(text[range]))
            }
            cursor = range.upperBound
        }
        if cursor < text.endIndex {
            result += AttributedString(String(text[cursor...]))
        }
        return result
    }

    private func citationStrip(_ citations: [SourceCitation]) -> some View {
        HStack(spacing: 5) {
            Text("SOURCES")
                .font(VPTheme.mono(6.5, weight: .semibold))
                .tracking(0.7)
                .foregroundStyle(VPTheme.textTertiary)
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 4) {
                    ForEach(citations) { citation in
                        Button {
                            activeCitation = citation
                        } label: {
                            HStack(spacing: 3) {
                                Text("[\(citation.key)]")
                                    .font(VPTheme.mono(7, weight: .semibold))
                                    .foregroundStyle(VPTheme.accent)
                                Text(citation.sourceTitle)
                                    .font(VPTheme.mono(7))
                                    .foregroundStyle(VPTheme.textSecondary)
                                    .lineLimit(1)
                                if let heading = citation.heading, !heading.isEmpty {
                                    Text("· \(heading)")
                                        .font(VPTheme.mono(7))
                                        .foregroundStyle(VPTheme.textTertiary)
                                        .lineLimit(1)
                                }
                            }
                            .padding(.horizontal, 6)
                            .padding(.vertical, 3)
                            .background(VPTheme.surface.opacity(0.55), in: Capsule())
                            .overlay {
                                Capsule().strokeBorder(VPTheme.hairline, lineWidth: 1)
                            }
                        }
                        .buttonStyle(.plain)
                        .help(citation.excerpt)
                    }
                }
            }
        }
        .padding(.leading, 2)
        .padding(.top, 1)
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

private struct SourceCitationSheet: View {
    let citation: SourceCitation
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Text("[\(citation.key)]")
                    .font(VPTheme.mono(11, weight: .bold))
                    .foregroundStyle(VPTheme.accent)
                VStack(alignment: .leading, spacing: 2) {
                    Text(citation.sourceTitle)
                        .font(VPTheme.font(14, weight: .semibold))
                        .foregroundStyle(VPTheme.textPrimary)
                    Text(
                        "\(citation.heading ?? L10n.t("Untitled section")) · chunk \(citation.chunkNo)"
                    )
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textSecondary)
                }
                Spacer()
                VPCloseButton(size: 24, help: L10n.t("Close")) { dismiss() }
            }
            Text(citation.excerpt)
                .font(VPTheme.mono(9))
                .foregroundStyle(VPTheme.textPrimary)
                .lineSpacing(2)
                .textSelection(.enabled)
                .padding(10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(VPTheme.surfaceSunken, in: RoundedRectangle(cornerRadius: 8))
                .overlay {
                    RoundedRectangle(cornerRadius: 8)
                        .strokeBorder(VPTheme.hairline, lineWidth: 1)
                }
            HStack(spacing: 8) {
                Text("origin: \(citation.origin) · score \(String(format: "%.2f", citation.score))")
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
                Spacer()
            }
        }
        .padding(20)
        .frame(width: 500)
        .background { VPCrystalBackdrop() }
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

/// Stage-level notice for a hard preemption. It is informational: it never
/// retries, resumes, or merges the interrupted work.
struct PreemptionBanner: View {
    let result: SessionPreemptionResult

    private var latest: PreemptionRecord? {
        result.records.last(where: { $0.terminal }) ?? result.records.last
    }

    var body: some View {
        HStack(alignment: .center, spacing: 10) {
            Image(systemName: "bolt.slash.fill")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(VPTheme.danger)
            VStack(alignment: .leading, spacing: 2) {
                Text("Run preempted")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(VPTheme.textPrimary)
                if let latest {
                    Text(bannerDetail(latest))
                        .font(VPTheme.mono(9.5))
                        .foregroundStyle(VPTheme.textSecondary)
                        .lineLimit(2)
                } else {
                    Text("Incomplete work was not merged. Inspect the worktree before resuming.")
                        .font(.system(size: 10))
                        .foregroundStyle(VPTheme.textSecondary)
                }
            }
            Spacer(minLength: 0)
            if latest?.checkpointId != nil {
                Label("checkpoint", systemImage: "camera.aperture")
                    .font(.system(size: 9, weight: .medium))
                    .foregroundStyle(VPTheme.textTertiary)
            }
        }
        .padding(.horizontal, 11)
        .padding(.vertical, 7)
        .background(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .fill(VPTheme.surface)
                .overlay {
                    RoundedRectangle(cornerRadius: 8, style: .continuous)
                        .strokeBorder(VPTheme.danger.opacity(0.35), lineWidth: 1)
                }
        )
        .accessibilityElement(children: .combine)
        .accessibilityLabel(
            latest.map { L10n.t("Run preempted: \($0.summary)") }
                ?? L10n.t("Run preempted")
        )
    }

    private func bannerDetail(_ record: PreemptionRecord) -> String {
        var detail = record.summary
        if record.sideEffectsUnknown {
            detail += " · verify side effects"
        }
        if record.partialInputTokens + record.partialOutputTokens > 0 {
            detail +=
                " · partial \(record.partialInputTokens) in / \(record.partialOutputTokens) out"
            if record.usageEstimated {
                detail += " (estimated)"
            }
        }
        return detail
    }
}

/// Ordered instruction sections from `session.instructions`. This is a
/// metadata-only cache view: keys, ordinals, character counts, and short
/// hashes. Prompt text is never requested or rendered.
private struct InstructionSectionsView: View {
    let report: SessionInstructionsReport
    let contract: SessionContractMetadata?

    private var leader: SessionInstructionStateReport? {
        report.leaderState
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text("INSTRUCTION SECTIONS")
                    .font(.system(size: 9, weight: .semibold, design: .monospaced))
                    .foregroundStyle(VPTheme.textTertiary)
                Text("v\(report.promptLayout)")
                    .font(.system(size: 9, weight: .semibold, design: .monospaced))
                    .foregroundStyle(VPTheme.accent)
                if let prefix = report.latestPrefixHash, !prefix.isEmpty {
                    Text("prefix \(String(prefix.prefix(12)))…")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(VPTheme.textTertiary)
                        .textSelection(.enabled)
                }
                if report.lastRebuilt > 0 {
                    Text("\(report.lastRebuilt) rebuilt")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(VPTheme.warning)
                }
                Spacer(minLength: 0)
            }
            if let contract {
                HStack(spacing: 8) {
                    Text("CONTRACT")
                        .font(.system(size: 9, weight: .semibold, design: .monospaced))
                        .foregroundStyle(VPTheme.accent)
                    Text(contract.shortHash + "…")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(VPTheme.textTertiary)
                        .textSelection(.enabled)
                    Text(contract.role)
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(VPTheme.textSecondary)
                    Text(contract.taskID ?? "session")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(VPTheme.textTertiary)
                    Text("\(contract.allowedPathsCount) paths · \(contract.acceptanceCount) acceptance")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(VPTheme.textTertiary)
                    Text(contract.egress)
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(VPTheme.textTertiary)
                    Text(contract.enforcementMode.uppercased())
                        .font(.system(size: 8, weight: .semibold, design: .monospaced))
                        .foregroundStyle(
                            contract.enforcementMode == "enforce"
                                ? VPTheme.accent : VPTheme.warning
                        )
                    Spacer(minLength: 0)
                }
            }
            if let leader, !leader.sections.isEmpty {
                ForEach(leader.sections) { section in
                    HStack(spacing: 8) {
                        Text(String(format: "%3d", section.ordinal))
                            .font(.system(size: 9, design: .monospaced))
                            .foregroundStyle(VPTheme.textTertiary)
                        Text(section.tail ? "TAIL" : "HEAD")
                            .font(.system(size: 8, weight: .semibold, design: .monospaced))
                            .foregroundStyle(section.tail ? VPTheme.warning : VPTheme.accent)
                            .frame(width: 30, alignment: .leading)
                        Text(section.key)
                            .font(.system(size: 9, design: .monospaced))
                            .foregroundStyle(VPTheme.textSecondary)
                        Text("\(section.chars) chars")
                            .font(.system(size: 9, design: .monospaced))
                            .foregroundStyle(VPTheme.textTertiary)
                        if section.available {
                            Text("\(section.shortHash)…")
                                .font(.system(size: 9, design: .monospaced))
                                .foregroundStyle(VPTheme.textTertiary)
                                .textSelection(.enabled)
                        } else {
                            Text("MISSING BLOB")
                                .font(.system(size: 8, weight: .semibold, design: .monospaced))
                                .foregroundStyle(VPTheme.danger)
                        }
                        Spacer(minLength: 0)
                    }
                }
                Text(
                    "Section text stays in the daemon instruction store; this view shows hashes only."
                )
                .font(.caption2)
                .foregroundStyle(.tertiary)
            } else {
                Text("No instruction state yet. It is materialized on the first run.")
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(8)
        .background(VPTheme.surfaceRaised.opacity(0.35), in: RoundedRectangle(cornerRadius: 9))
    }
}
