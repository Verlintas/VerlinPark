import AppKit
import SwiftUI

enum StudioTab: String, CaseIterable, Identifiable {
    case collab
    case preview

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .collab: L10n.t("Collab")
        case .preview: L10n.t("Preview")
        }
    }
}

struct StudioView: View {
    @Environment(AppModel.self) private var model
    @Environment(BrandStore.self) private var brand
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Binding var tab: StudioTab
    @State private var showingOrgEditor = false

    var body: some View {
        VStack(spacing: 0) {
            header
            VPGradientRule()
                .frame(height: 1)

            Group {
                if model.mode == .resident {
                    if tab == .collab {
                        ExternalResidentPanel()
                    } else {
                        StudioEmptyView(
                            icon: "rectangle.on.rectangle",
                            title: "No project preview",
                            detail: "Open a project to inspect its trace and artifacts."
                        )
                    }
                } else {
                    switch tab {
                    case .collab:
                        CollabGraphView()
                    case .preview:
                        PreviewPanelView()
                    }
                }
            }
            .id(tab)
            .transition(.vpContent)
            .animation(VPMotion.resolved(VPMotion.smooth, reduceMotion: reduceMotion), value: tab)
            .frame(maxWidth: .infinity, maxHeight: .infinity)

            if model.mode == .project, tab == .collab {
                VPGradientRule()
                    .frame(height: 1)
                HStack {
                    Text("Org preset")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    Spacer()
                    Button {
                        showingOrgEditor = true
                    } label: {
                        Label("Edit", systemImage: "slider.horizontal.3")
                            .font(.system(size: 10))
                    }
                    .buttonStyle(VPPressableButtonStyle(scale: 0.95))
                    .foregroundStyle(.secondary)
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 9)
            }
        }
        .background { VPCrystalSurface(tone: VPTheme.rail, solidTone: VPTheme.solidRail, tintOpacity: 0.68) }
        .sheet(isPresented: $showingOrgEditor) {
            OrgPresetSheet()
                .environment(model)
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack(spacing: 7) {
                VPKicker(text: "Studio", trailing: nil)

                Spacer()

                if model.mode == .project {
                    let status = model.selectedSession?.status
                    let active = status == "running" || status == "queued"
                        || status == "paused" || status == "waiting_input"
                    HStack(spacing: 5) {
                        VPStatusDot(
                            color: active ? VPTheme.success : VPTheme.textTertiary,
                            pulsing: active,
                            size: 4
                        )
                        Text(
                            status == nil
                                ? "NO SESSION"
                                : (active ? "LIVE" : "IDLE")
                        )
                        .font(VPTheme.mono(7.5, weight: .semibold))
                        .tracking(0.6)
                        .foregroundStyle(active ? VPTheme.success : VPTheme.textTertiary)
                        .contentTransition(.opacity)
                    }
                }
            }

            VPSegmentedControl(
                segments: [
                    VPSegment(.collab, title: "Collab", icon: "person.3"),
                    VPSegment(.preview, title: "Preview", icon: "rectangle.on.rectangle"),
                ],
                selection: $tab,
                compact: false
            )
        }
        .padding(.horizontal, 12)
        .padding(.top, 12)
        .padding(.bottom, 10)
    }
}

struct ExternalResidentPanel: View {
    private let bridge = UnavailableResidentAgent()

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                VStack(alignment: .leading, spacing: 10) {
                    HStack(alignment: .top, spacing: 10) {
                        ZStack {
                            RoundedRectangle(cornerRadius: 9, style: .continuous)
                                .fill(VPTheme.resident.opacity(0.12))
                                .frame(width: 36, height: 36)
                            Image(systemName: "cube.transparent")
                                .font(.system(size: 14, weight: .semibold))
                                .foregroundStyle(VPTheme.resident)
                        }

                        VStack(alignment: .leading, spacing: 2) {
                            HStack(spacing: 6) {
                                Text(bridge.displayName)
                                    .font(VPTheme.font(16, weight: .semibold, rounded: true))
                                    .foregroundStyle(VPTheme.textPrimary)
                                Text("EXTERNAL")
                                    .font(VPTheme.mono(7, weight: .semibold))
                                    .tracking(0.6)
                                    .padding(.horizontal, 5)
                                    .padding(.vertical, 2)
                                    .background(VPTheme.surfaceRaised, in: Capsule())
                                    .foregroundStyle(VPTheme.textTertiary)
                            }
                            Text("NovaBAIC resident bridge")
                                .font(VPTheme.mono(8))
                                .foregroundStyle(VPTheme.textTertiary)
                        }

                        Spacer(minLength: 0)

                        StatusPill(status: bridge.status.rawValue)
                    }

                    Text(
                        "Resident surface reserved for the NovaBAIC Aviiya bridge. Not implemented in this build."
                    )
                    .font(VPTheme.font(10.5))
                    .foregroundStyle(VPTheme.textSecondary)
                    .lineSpacing(2)
                    .fixedSize(horizontal: false, vertical: true)
                }
                .padding(12)
                .background(
                    VPTheme.resident.opacity(0.045),
                    in: RoundedRectangle(cornerRadius: 12, style: .continuous)
                )
                .overlay {
                    RoundedRectangle(cornerRadius: 12, style: .continuous)
                        .strokeBorder(VPTheme.resident.opacity(0.14), lineWidth: 1)
                }

                VPKicker(text: "Bridge contract", trailing: "future")

                VStack(spacing: 0) {
                    bridgeRow(
                        "person.crop.circle",
                        "Identity",
                        "Aviiya persona from NovaBAIC"
                    )
                    VPHairline().padding(.leading, 25)
                    bridgeRow("brain", "Memory", "Shared resident memory, not local")
                    VPHairline().padding(.leading, 25)
                    bridgeRow("wrench.and.screwdriver", "Tools", "Shared tool catalog")
                    VPHairline().padding(.leading, 25)
                    bridgeRow(
                        "bubble.left.and.bubble.right",
                        "Sessions",
                        "Cross-device conversation continuity"
                    )
                }
                .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 11, style: .continuous))
                .overlay {
                    RoundedRectangle(cornerRadius: 11, style: .continuous)
                        .strokeBorder(VPTheme.hairline, lineWidth: 1)
                }

                Text(
                    "This is a reserved seam, not a second assistant implementation. VerlinPark's project org remains independent."
                )
                .font(VPTheme.font(9.5))
                .foregroundStyle(VPTheme.textTertiary)
                .lineSpacing(2)
                .fixedSize(horizontal: false, vertical: true)

                Link(destination: URL(string: "https://github.com/Verlintas/NovaBAIC")!) {
                    HStack(spacing: 5) {
                        Text("Open NovaBAIC")
                        Image(systemName: "arrow.up.right")
                            .font(.system(size: 7, weight: .bold))
                    }
                    .font(VPTheme.mono(8, weight: .medium))
                    .foregroundStyle(VPTheme.textSecondary)
                    .padding(.horizontal, 9)
                    .padding(.vertical, 5)
                    .background(VPTheme.surface, in: Capsule())
                    .overlay {
                        Capsule().strokeBorder(VPTheme.hairline, lineWidth: 1)
                    }
                }
                .buttonStyle(VPPressableButtonStyle(scale: 0.96))
            }
            .padding(14)
        }
    }

    private func bridgeRow(_ icon: String, _ title: String, _ detail: String) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: icon)
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(VPTheme.resident.opacity(0.8))
                .frame(width: 17)
                .padding(.top, 1)
            VStack(alignment: .leading, spacing: 1) {
                Text(L10n.t(title))
                    .font(VPTheme.font(10, weight: .medium))
                    .foregroundStyle(VPTheme.textPrimary)
                Text(L10n.t(detail))
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            Spacer()
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 7)
    }
}

struct PreviewPanelView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var mode: PreviewMode = .showcase
    @State private var showingCommit = false
    @State private var showingDiscard = false
    @State private var showingLand = false
    @State private var showingPR = false
    @State private var commitMessage = ""
    @State private var selectedDiffPath: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let session = model.selectedSession {
                header(session)
                Divider().opacity(0.3)
                Group {
                    switch mode {
                    case .showcase:
                        ShowcasePanel()
                    case .changes:
                        changes
                    case .plan:
                        plan
                    case .files:
                        FilesPanel()
                    case .history:
                        HistoryPanel()
                    case .queue:
                        queue
                    case .mailbox:
                        mailbox
                    case .cost:
                        cost
                    case .console:
                        ConsolePanel()
                    case .terminal:
                        TerminalPanel()
                    case .trace:
                        trace
                    }
                }
                .id(mode)
                .transition(.vpContent)
                .animation(
                    VPMotion.resolved(VPMotion.smooth, reduceMotion: reduceMotion),
                    value: mode
                )
            } else {
                StudioEmptyView(
                    icon: "rectangle.stack",
                    title: "No session selected",
                    detail: "Choose a session in the center column."
                )
            }
        }
        .task(id: model.selectedSessionID) {
            if mode == .files {
                await model.loadWorkspaceTree()
            }
            if mode == .history {
                await model.loadGitHistory()
            }
            if model.selectedSessionID != nil {
                await model.loadDiff()
                await model.loadMailbox()
                await model.loadUsage()
            }
        }
        .task(id: mode) {
            if mode == .files {
                await model.loadWorkspaceTree()
            }
            if mode == .history {
                await model.loadGitHistory()
            }
        }
        .alert("Commit worktree", isPresented: $showingCommit) {
            TextField("Message", text: $commitMessage)
            Button("Commit") {
                let message = commitMessage
                commitMessage = ""
                Task { await model.commitSession(message: message) }
            }
            Button("Cancel", role: .cancel) {
                commitMessage = ""
            }
        } message: {
            Text("Stages all changes in this session worktree and commits them on its branch.")
        }
        .confirmationDialog(
            "Discard uncommitted changes?",
            isPresented: $showingDiscard
        ) {
            Button("Discard changes", role: .destructive) {
                Task { await model.discardChanges() }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(
                "This resets tracked files and removes untracked files in the session worktree. Commits are kept."
            )
        }
        .confirmationDialog(
            "Open a PR / MR?",
            isPresented: $showingPR
        ) {
            Button("Push branch & open PR") {
                Task { await model.openPullRequest() }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(
                "Pushes the session branch to origin and runs gh pr create. gh must be installed and authenticated; no merge is performed locally."
            )
        }
        .confirmationDialog(
            "Land the session branch?",
            isPresented: $showingLand
        ) {
            Button("Land into \(model.selectedProject?.defaultBranch ?? "the default branch")") {
                Task { await model.landSession() }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(
                "The project checkout must be clean and checked out on its default branch. The session branch is merged with --no-ff; no push or PR is performed."
            )
        }
    }

    private var canCommit: Bool {
        model.sessionDiff?.hasUncommittedChanges ?? false
    }

    private func canLand(_ session: Session) -> Bool {
        guard session.status != "landed",
              let diff = model.sessionDiff
        else {
            return false
        }
        return !diff.hasUncommittedChanges && !diff.patch.isEmpty
    }

    private func header(_ session: Session) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Session review")
                    Text(session.goal)
                        .font(.system(size: 11, weight: .medium))
                        .lineLimit(1)
                }
                Spacer()
                if mode == .changes, canCommit {
                    Button {
                        showingDiscard = true
                    } label: {
                        Image(systemName: "trash")
                            .font(.system(size: 10, weight: .semibold))
                    }
                    .buttonStyle(.plain)
                    .help("Discard uncommitted changes")
                    .disabled(model.commitBusy)
                    Button("Commit") {
                        commitMessage = session.goal
                        showingCommit = true
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                    .disabled(model.commitBusy)
                } else if mode == .changes, canLand(session) {
                    Button("Open PR / MR") {
                        showingPR = true
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(model.pullRequestBusy)
                    Button("Land") {
                        showingLand = true
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                    .tint(VPTheme.success)
                    .disabled(model.landingBusy)
                }
                Button {
                    Task {
                        switch mode {
                        case .files:
                            await model.loadWorkspaceTree()
                            await model.reloadWorkspaceFile()
                        case .history:
                            await model.loadGitHistory()
                        case .queue:
                            await model.loadMergeQueue()
                        case .mailbox:
                            await model.loadMailbox()
                        case .cost:
                            await model.loadUsage()
                        case .console:
                            await model.pollConsole()
                        case .terminal:
                            await model.pollTerminal()
                        default:
                            await model.loadDiff()
                        }
                    }
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .buttonStyle(VPGlassIconButtonStyle(size: 24, tint: .secondary))
                .help(
                    mode == .files ? "Refresh files" :
                        mode == .history ? "Refresh history" :
                        mode == .queue ? "Refresh queue" : mode == .mailbox ? "Refresh mailbox" :
                        mode == .cost ? "Refresh usage" :
                        mode == .console ? "Refresh console" :
                        mode == .terminal ? "Refresh terminal" : "Refresh diff"
                )
                .disabled(model.diffLoading)
            }
            HStack(spacing: 6) {
                VPSegmentedControl(
                    segments: PreviewMode.primary.map {
                        VPSegment($0, title: $0.title, icon: $0.icon)
                    },
                    selection: $mode,
                    scrollable: true,
                    compact: true
                )
                Menu {
                    ForEach(PreviewMode.secondary) { item in
                        Button {
                            mode = item
                        } label: {
                            Label(item.title, systemImage: item.icon)
                        }
                    }
                } label: {
                    HStack(spacing: 3) {
                        Image(systemName: PreviewMode.secondary.contains(mode) ? mode
                            .icon : "ellipsis.circle")
                            .font(.system(size: 8, weight: .semibold))
                        Text(PreviewMode.secondary.contains(mode) ? mode.title : L10n.t("More"))
                            .font(VPTheme.mono(7.5, weight: .medium))
                    }
                    .foregroundStyle(PreviewMode.secondary.contains(mode) ? VPTheme.accent : VPTheme
                        .textTertiary)
                    .padding(.horizontal, 7)
                    .padding(.vertical, 4)
                    .background(VPTheme.surface, in: Capsule())
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .fixedSize()
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
    }

    @ViewBuilder
    private var changes: some View {
        if model.diffLoading, model.sessionDiff == nil {
            VPSkeletonRows(rows: 4, height: 12)
        } else if let error = model.diffError, model.sessionDiff == nil {
            StudioEmptyView(
                icon: "exclamationmark.triangle",
                title: "Diff unavailable",
                detail: error
            )
        } else if let diff = model.sessionDiff, !diff.isEmpty {
            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    HStack(spacing: 6) {
                        Image(systemName: "arrow.triangle.branch")
                        Text(diff.branch.isEmpty ? "worktree" : diff.branch)
                        Spacer()
                        Text("\(diff.patch.split(separator: "\n").count) lines")
                            .foregroundStyle(.tertiary)
                    }
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)

                    if let base = diff.baseCommit {
                        Text("since \(String(base.prefix(7))) · worktree is the session branch")
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.tertiary)
                    }
                    if !diff.hasUncommittedChanges, !diff.patch.isEmpty {
                        Text(
                            "All changes are committed on the session branch. Use Land to merge it into the project default branch."
                        )
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    }

                    if !diff.status.isEmpty {
                        Text(diff.status)
                            .font(.system(size: 9, design: .monospaced))
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                    }

                    if let commit = model.lastCommit {
                        Label(
                            "Committed \(commit.commit) · \(commit.message)",
                            systemImage: "checkmark.seal.fill"
                        )
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.green)
                    }
                    if let landing = model.lastLanding {
                        Label(
                            "Landed \(landing.commit) · \(landing.targetBranch)",
                            systemImage: "arrow.up.to.line"
                        )
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.green)
                    }
                    if let pr = model.lastPullRequest,
                       let urlString = pr.url,
                       let url = URL(string: urlString)
                    {
                        Link(destination: url) {
                            Label("Pull request opened", systemImage: "arrow.up.right.square")
                                .font(.system(size: 9, design: .monospaced))
                        }
                        .foregroundStyle(.green)
                    }

                    if !model.diffFiles.isEmpty {
                        HStack(spacing: 8) {
                            Text("FILES \(model.diffFiles.count)")
                                .font(.system(size: 9, weight: .medium, design: .monospaced))
                                .foregroundStyle(.tertiary)
                            Spacer()
                            Picker("", selection: $selectedDiffPath) {
                                Text("All changes").tag(String?.none)
                                ForEach(model.diffFiles) { file in
                                    Text("\(file.path)  +\(file.additions) -\(file.deletions)")
                                        .tag(Optional(file.path))
                                }
                            }
                            .pickerStyle(.menu)
                            .labelsHidden()
                            .controlSize(.small)
                        }
                    }

                    if let file = selectedDiffFile {
                        HStack {
                            Text(file.path)
                                .font(.system(size: 9, weight: .medium, design: .monospaced))
                                .lineLimit(1)
                                .truncationMode(.middle)
                            Spacer()
                            Text("+\(file.additions) -\(file.deletions)")
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(.tertiary)
                        }
                        DiffLines(text: file.patch)
                    } else if !diff.patch.isEmpty {
                        DiffLines(text: diff.patch)
                    }
                }
                .padding(12)
            }
            .onChange(of: model.diffFiles.map(\.path)) { _, paths in
                if let path = selectedDiffPath, !paths.contains(path) {
                    selectedDiffPath = nil
                }
            }
        } else if let commit = model.lastCommit {
            StudioEmptyView(
                icon: "checkmark.seal",
                title: "Committed \(commit.commit)",
                detail: "\(commit.message) · the worktree is clean."
            )
        } else {
            StudioEmptyView(
                icon: "checkmark.circle",
                title: "No changes yet",
                detail: "Edits from the Leader will appear in this worktree diff."
            )
        }
    }

    private var canResumePlan: Bool {
        guard let session = model.selectedSession,
              let plan = model.teamPlan,
              ["idle", "failed", "cancelled", "draft"].contains(session.status)
        else { return false }
        return plan.tasks.contains { model.planTaskStatuses[$0.id] != "completed" }
    }

    private var selectedDiffFile: SessionDiffFile? {
        guard let path = selectedDiffPath else { return nil }
        return model.diffFiles.first { $0.path == path }
    }

    @ViewBuilder
    private var plan: some View {
        if let plan = model.teamPlan {
            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    VStack(alignment: .leading, spacing: 3) {
                        Text(plan.goal)
                            .font(.system(size: 12, weight: .semibold))
                        Text(
                            "\(plan.departments.count) departments · \(plan.tasks.count) tasks"
                        )
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    }
                    if canResumePlan {
                        HStack(spacing: 8) {
                            Text("Retry unfinished tasks without another Leader call.")
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(.secondary)
                            Spacer()
                            if model.planResumeBusy {
                                ProgressView().controlSize(.small)
                            }
                            Button("Resume plan") {
                                Task { await model.resumePlan() }
                            }
                            .buttonStyle(.borderedProminent)
                            .controlSize(.small)
                            .disabled(model.planResumeBusy)
                        }
                        .padding(8)
                        .background(.orange.opacity(0.06), in: RoundedRectangle(cornerRadius: 8))
                    }
                    ForEach(plan.departments) { department in
                        HStack(spacing: 6) {
                            Image(systemName: "person.2")
                                .font(.system(size: 9))
                                .foregroundStyle(.secondary)
                            Text(department.managerTitle)
                                .font(.system(size: 10, weight: .medium))
                            Text("\(department.profile)")
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(.tertiary)
                            if let budget = planBudgetLabel(
                                tokens: department.tokenBudget,
                                cost: department.costBudgetUsd
                            ) {
                                Text(budget)
                                    .font(.system(size: 8, design: .monospaced))
                                    .foregroundStyle(.secondary)
                            }
                            Spacer()
                        }
                    }
                    Divider().opacity(0.25)
                    LazyVStack(alignment: .leading, spacing: 6) {
                        ForEach(plan.tasks) { task in
                            PlanTaskRow(
                                task: task,
                                status: model.planTaskStatuses[task.id] ?? "pending"
                            )
                        }
                    }
                }
                .padding(12)
            }
        } else {
            StudioEmptyView(
                icon: "list.bullet.clipboard",
                title: "No team plan",
                detail: "The Leader can submit a TeamPlan with departments, profiles, and a task DAG."
            )
        }
    }

    @ViewBuilder
    private var mailbox: some View {
        if model.mailbox.isEmpty, model.artifacts.isEmpty {
            StudioEmptyView(
                icon: "tray",
                title: "Mailbox is empty",
                detail: "Agents can send typed messages and publish artifacts with the mailbox tools."
            )
        } else {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 10) {
                    if !model.mailbox.isEmpty {
                        Text("MESSAGES")
                            .font(.system(size: 9, weight: .semibold, design: .monospaced))
                            .foregroundStyle(.tertiary)
                        ForEach(model.mailbox) { message in
                            MailboxMessageRow(message: message)
                        }
                    }
                    if !model.artifacts.isEmpty {
                        Text("ARTIFACTS")
                            .font(.system(size: 9, weight: .semibold, design: .monospaced))
                            .foregroundStyle(.tertiary)
                            .padding(.top, 4)
                        ForEach(model.artifacts) { artifact in
                            ArtifactRow(artifact: artifact)
                        }
                    }
                }
                .padding(12)
            }
        }
    }

    @ViewBuilder
    private var cost: some View {
        if let ledger = model.usageLedger {
            if ledger.entries.isEmpty {
                StudioEmptyView(
                    icon: "dollarsign.circle",
                    title: "No usage yet",
                    detail: "Provider calls record per-agent, per-task token and USD usage here."
                )
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 12) {
                        HStack {
                            Text("SESSION USAGE")
                                .font(.system(size: 9, weight: .semibold, design: .monospaced))
                                .foregroundStyle(.tertiary)
                            Spacer()
                            Button("Export CSV…") {
                                Task { await model.exportUsageCsv() }
                            }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                        }
                        HStack(spacing: 10) {
                            UsageMetric(
                                title: "Calls",
                                value: "\(ledger.totals.calls)"
                            )
                            UsageMetric(
                                title: "Tokens",
                                value: "\(ledger.totals.inputTokens + ledger.totals.outputTokens)"
                            )
                            UsageMetric(
                                title: "Cost",
                                value: String(format: "$%.4f", ledger.totals.costUsd)
                            )
                        }

                        UsageBucketSection(title: "BY ROLE", buckets: ledger.totals.byRole)
                        UsageBucketSection(
                            title: "BY DEPARTMENT",
                            buckets: ledger.totals.byDepartment ?? []
                        )
                        UsageBucketSection(title: "BY TASK", buckets: ledger.totals.byTask)
                        UsageBucketSection(title: "BY AGENT", buckets: ledger.totals.byAgent)
                        UsageBucketSection(title: "BY MODEL", buckets: ledger.totals.byModel)

                        Text("CALLS")
                            .font(.system(size: 9, weight: .semibold, design: .monospaced))
                            .foregroundStyle(.tertiary)
                            .padding(.top, 2)
                        ForEach(ledger.entries.reversed()) { entry in
                            UsageEntryRow(entry: entry)
                        }
                    }
                    .padding(12)
                }
            }
        } else if let status = model.usageStatus {
            StudioEmptyView(
                icon: "exclamationmark.triangle",
                title: "Usage unavailable",
                detail: status
            )
        } else {
            VPSkeletonRows(rows: 4, height: 14)
        }
    }

    @ViewBuilder
    private var queue: some View {
        if model.mergeQueue.isEmpty {
            StudioEmptyView(
                icon: "arrow.triangle.merge",
                title: "Merge queue is empty",
                detail: "Verifier-approved Workers enter the queue; the deterministic Integrator merges and checks them here."
            )
        } else {
            ScrollView {
                LazyVStack(spacing: 8) {
                    ForEach(model.mergeQueue) { entry in
                        MergeQueueRow(entry: entry)
                    }
                }
                .padding(12)
            }
        }
    }

    @ViewBuilder
    private var trace: some View {
        if model.events.isEmpty {
            StudioEmptyView(
                icon: "list.bullet.rectangle",
                title: "No trace yet",
                detail: "Events from the Leader and the project team will appear here."
            )
        } else {
            ScrollView {
                LazyVStack(spacing: 6) {
                    ForEach(model.events) { event in
                        PreviewEventRow(event: event)
                    }
                }
                .padding(12)
            }
        }
    }
}

private struct MergeQueueRow: View {
    @Environment(AppModel.self) private var model
    let entry: MergeQueueEntry
    @State private var retrying = false
    @State private var resolving = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 7) {
                Image(systemName: icon)
                    .foregroundStyle(color)
                    .contentTransition(.symbolEffect(.replace))
                    .animation(VPMotion.snap, value: entry.status)
                Text(entry.status.uppercased())
                    .font(.system(size: 9, weight: .semibold, design: .monospaced))
                    .foregroundStyle(color)
                Spacer()
                Text(entry.taskId ?? entry.agentName)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            HStack(spacing: 6) {
                Text(entry.agentName)
                    .font(.system(size: 10.5, weight: .medium))
                Spacer()
                Text(String(entry.commit.prefix(7)))
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
            Text(entry.branch)
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
                .lineLimit(1)
                .truncationMode(.middle)
            if let detail = entry.detail, !detail.isEmpty {
                Text(detail)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(4)
            }
            if entry.status == "conflict" || entry.status == "failed" || entry.status == "interrupted" {
                HStack(spacing: 6) {
                    if entry.status == "conflict" {
                        Button {
                            resolving = true
                            Task {
                                await model.resolveMergeEntry(entry.id)
                                resolving = false
                            }
                        } label: {
                            Label("Auto-resolve", systemImage: "wand.and.stars")
                                .font(.system(size: 9, weight: .medium))
                        }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.small)
                        .tint(.orange)
                        .disabled(resolving || retrying)
                        .help(
                            "Run an internal resolver agent without routine tool prompts (host-level operations stay blocked), verify, then merge"
                        )
                    }
                    Button {
                        retrying = true
                        Task {
                            await model.retryMergeEntry(entry.id)
                            retrying = false
                        }
                    } label: {
                        Label("Rebase & retry", systemImage: "arrow.clockwise")
                            .font(.system(size: 9, weight: .medium))
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(retrying || resolving)
                }
            }
        }
        .padding(10)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .strokeBorder(color.opacity(0.16), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.005, lift: 1, glow: color)
    }

    private var color: Color {
        switch entry.status {
        case "merged": .green
        case "conflict", "failed", "interrupted": .red
        case "merging", "checking": .orange
        default: .secondary
        }
    }

    private var icon: String {
        switch entry.status {
        case "merged": "checkmark.circle.fill"
        case "conflict": "exclamationmark.triangle.fill"
        case "failed", "interrupted": "xmark.circle.fill"
        case "merging", "checking": "arrow.triangle.2.circlepath"
        default: "clock"
        }
    }
}

private struct MailboxMessageRow: View {
    @Environment(AppModel.self) private var model
    let message: AgentMessage

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 6) {
                Text("\(message.fromName) → \(message.toName)")
                    .font(.system(size: 10, weight: .medium))
                Text(message.kind)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
                Spacer()
                Text(message.status.uppercased())
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(message.status == "unread" ? Color.orange : Color.secondary)
            }
            Text(message.body)
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
            if message.status == "unread" {
                Button("Mark read") {
                    Task { await model.markMessageRead(message.id) }
                }
                .buttonStyle(.plain)
                .font(.system(size: 9))
                .foregroundStyle(.secondary)
            }
        }
        .padding(9)
        .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 9))
        .vpHoverLift(scale: 1.005, lift: 1, glow: message.status == "unread" ? .orange : .white)
    }
}

private struct ShowcasePanel: View {
    @Environment(AppModel.self) private var model

    private var artifacts: [Artifact] {
        model.artifacts.sorted { $0.createdAt > $1.createdAt }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(L10n.t("Agent showcase"))
                        .font(VPTheme.font(13, weight: .semibold))
                    Text(L10n.t("Artifacts and results your agents published for you."))
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                Spacer()
                Text("\(artifacts.count)")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                Button {
                    Task { await model.loadMailbox() }
                } label: {
                    Image(systemName: "arrow.clockwise")
                        .font(.system(size: 9, weight: .semibold))
                }
                .buttonStyle(VPGlassIconButtonStyle(size: 22, tint: VPTheme.textSecondary))
            }
            .padding(14)

            VPHairline()

            if artifacts.isEmpty {
                VStack(spacing: 9) {
                    Image(systemName: "sparkles.rectangle.stack")
                        .font(.system(size: 26, weight: .light))
                        .foregroundStyle(VPTheme.textTertiary)
                    Text(L10n.t("Nothing on the whiteboard yet"))
                        .font(VPTheme.font(12, weight: .semibold))
                        .foregroundStyle(VPTheme.textSecondary)
                    Text(L10n
                        .t(
                            "Ask an agent to publish a screenshot, report, or preview artifact. Published artifacts appear here as a result board."
                        ))
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: 340)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .padding(24)
            } else {
                ScrollView {
                    LazyVGrid(
                        columns: [GridItem(.adaptive(minimum: 260), spacing: 10)],
                        alignment: .leading,
                        spacing: 10
                    ) {
                        ForEach(artifacts) { artifact in
                            ArtifactRow(artifact: artifact)
                        }
                    }
                    .padding(12)
                }
            }
        }
        .task {
            await model.loadMailbox()
        }
    }
}

private struct ArtifactRow: View {
    @Environment(AppModel.self) private var model
    let artifact: Artifact
    @State private var preview: ArtifactPreviewData?

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Text(artifact.name)
                    .font(.system(size: 10, weight: .medium))
                Text(artifact.kind)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
                Text(artifact.storage)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
                if let compression = artifact.compression, compression != "none" {
                    Text(compression)
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
                Spacer()
                Text(artifact.agentName)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
            Text("\(artifact.sizeBytes) bytes · \(artifact.mimeType)")
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
            if let image = preview?.image {
                Image(nsImage: image)
                    .resizable()
                    .scaledToFit()
                    .frame(maxHeight: 180)
                    .clipShape(RoundedRectangle(cornerRadius: 7))
            }
            if let text = preview?.text {
                ScrollView {
                    Text(text)
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(maxHeight: 180)
                .padding(6)
                .background(.black.opacity(0.18), in: RoundedRectangle(cornerRadius: 6))
            }
            if let hex = preview?.hex {
                ScrollView([.horizontal, .vertical]) {
                    Text(hex)
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                        .fixedSize(horizontal: true, vertical: false)
                }
                .frame(maxHeight: 160)
                .padding(6)
                .background(.black.opacity(0.18), in: RoundedRectangle(cornerRadius: 6))
                Text("Binary preview · first 1 KB")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
            if artifact.storage == "file" || preview != nil {
                HStack(spacing: 8) {
                    Text(preview?.path ?? artifact.path ?? "path unavailable")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    if preview != nil || artifact.path != nil {
                        Button("Open") {
                            Task { await model.openArtifact(artifact) }
                        }
                        .buttonStyle(.plain)
                        .font(.system(size: 9))
                        .foregroundStyle(.secondary)
                    }
                }
            } else {
                Text(artifact.content)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(8)
                    .textSelection(.enabled)
            }
            if preview?.truncated == true {
                Text("Preview truncated at 64 KB. Open the artifact for the full file.")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(9)
        .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 9))
        .vpHoverLift(scale: 1.005, lift: 1)
        .task(id: artifact.id) {
            preview = await model.artifactPreviewData(artifact)
        }
    }
}

private struct UsageMetric: View {
    let title: String
    let value: String

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title)
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
            VPAnimatedNumberText(value: value, size: 15)
                .monospacedDigit()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(9)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 9))
        .vpHoverLift(scale: 1.006, lift: 1)
    }
}

private struct UsageBucketSection: View {
    let title: String
    let buckets: [UsageBucket]

    var body: some View {
        if !buckets.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                Text(title)
                    .font(.system(size: 9, weight: .semibold, design: .monospaced))
                    .foregroundStyle(.tertiary)
                ForEach(buckets) { bucket in
                    HStack(spacing: 6) {
                        Text(bucket.key)
                            .font(.system(size: 9, design: .monospaced))
                            .lineLimit(1)
                        Spacer()
                        Text("\(bucket.calls) calls")
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.tertiary)
                        Text("\(bucket.inputTokens + bucket.outputTokens) tok")
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.secondary)
                        Text(String(format: "$%.4f", bucket.costUsd))
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.secondary)
                    }
                    .padding(.horizontal, 8)
                    .padding(.vertical, 5)
                    .background(.white.opacity(0.025), in: RoundedRectangle(cornerRadius: 7))
                }
            }
        }
    }
}

private struct UsageEntryRow: View {
    let entry: UsageEntry

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                Text(entry.role)
                    .font(.system(size: 9, weight: .medium, design: .monospaced))
                if let task = entry.taskId {
                    Text(task)
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
                Text(entry.agentName)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
                Spacer()
                if entry.estimated {
                    Text("est")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.orange)
                }
                Text(String(format: "$%.4f", entry.costUsd))
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
            HStack(spacing: 8) {
                Text(entry.model)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
                Spacer()
                Text("in \(entry.inputTokens) · out \(entry.outputTokens)")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
        }
        .padding(8)
        .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 8))
    }
}

private enum PreviewMode: String, CaseIterable, Identifiable {
    case showcase
    case changes
    case plan
    case files
    case history
    case queue
    case mailbox
    case cost
    case console
    case terminal
    case trace

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .showcase: L10n.t("Showcase")
        case .changes: L10n.t("Changes")
        case .plan: L10n.t("Plan")
        case .files: L10n.t("Files")
        case .history: L10n.t("History")
        case .queue: L10n.t("Queue")
        case .mailbox: L10n.t("Mailbox")
        case .cost: L10n.t("Cost")
        case .console: L10n.t("Console")
        case .terminal: L10n.t("Terminal")
        case .trace: L10n.t("Trace")
        }
    }

    var icon: String {
        switch self {
        case .showcase: "sparkles.rectangle.stack"
        case .changes: "plus.forwardslash.minus"
        case .plan: "list.bullet.clipboard"
        case .files: "folder"
        case .history: "clock.arrow.circlepath"
        case .queue: "arrow.triangle.merge"
        case .mailbox: "tray"
        case .cost: "dollarsign.circle"
        case .console: "terminal"
        case .terminal: "apple.terminal"
        case .trace: "list.bullet.rectangle"
        }
    }

    static let primary: [PreviewMode] = [.showcase, .changes, .files, .plan]
    static let secondary: [PreviewMode] = [
        .history, .queue, .mailbox, .cost, .console, .terminal, .trace,
    ]
}

private struct HistoryPanel: View {
    @Environment(AppModel.self) private var model
    @State private var hoveredCommit: String?
    @Namespace private var selectionNamespace

    var body: some View {
        GeometryReader { proxy in
            let compact = proxy.size.width < 560
            if compact {
                if model.gitSelectedCommit != nil {
                    detailPane(compact: true)
                } else {
                    listPane
                }
            } else {
                HStack(spacing: 0) {
                    listPane
                        .frame(width: min(320, max(240, proxy.size.width * 0.38)))
                    VPHairline(vertical: true)
                    detailPane(compact: false)
                }
            }
        }
    }

    private var listPane: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                Text(model.gitBranch ?? "session branch")
                    .font(VPTheme.mono(9))
                    .lineLimit(1)
                Spacer()
                if model.gitLogBusy {
                    ProgressView().controlSize(.small)
                }
                Text("\(model.gitCommits.count)")
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            .padding(10)

            if let base = model.gitBaseCommit, !base.isEmpty {
                Text("base \(String(base.prefix(12)))")
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
                    .padding(.horizontal, 10)
                    .padding(.bottom, 7)
            }

            VPHairline()

            if let error = model.gitLogError {
                Text(error)
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.warning)
                    .padding(10)
            }

            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(model.gitCommits) { commit in
                        Button {
                            Task { await model.loadGitCommit(commit.hash) }
                        } label: {
                            VStack(alignment: .leading, spacing: 3) {
                                HStack(spacing: 6) {
                                    Text(commit.shortHash)
                                        .font(VPTheme.mono(9, weight: .medium))
                                        .foregroundStyle(VPTheme.accent)
                                    Text(commit.subject)
                                        .font(VPTheme.font(10, weight: .medium))
                                        .foregroundStyle(VPTheme.textPrimary)
                                        .lineLimit(1)
                                    Spacer(minLength: 0)
                                }
                                HStack(spacing: 7) {
                                    Text(commit.author)
                                    Text(commit.authorDate)
                                    if !commit.refs.isEmpty {
                                        Text(commit.refs)
                                            .foregroundStyle(VPTheme.accent)
                                            .lineLimit(1)
                                    }
                                }
                                .font(VPTheme.mono(7))
                                .foregroundStyle(VPTheme.textTertiary)
                            }
                            .padding(.horizontal, 10)
                            .padding(.vertical, 7)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background {
                                if model.gitSelectedCommit == commit.hash {
                                    RoundedRectangle(cornerRadius: 7, style: .continuous)
                                        .fill(VPTheme.accent.opacity(0.10))
                                        .overlay {
                                            RoundedRectangle(cornerRadius: 7, style: .continuous)
                                                .strokeBorder(VPTheme.accent.opacity(0.16), lineWidth: 1)
                                        }
                                        .matchedGeometryEffect(
                                            id: "history-selection",
                                            in: selectionNamespace
                                        )
                                } else if hoveredCommit == commit.hash {
                                    RoundedRectangle(cornerRadius: 7, style: .continuous)
                                        .fill(VPTheme.surface)
                                }
                            }
                        }
                        .buttonStyle(VPPressableButtonStyle(scale: 0.99))
                        .onHover { hovering in
                            if hovering {
                                hoveredCommit = commit.hash
                            } else if hoveredCommit == commit.hash {
                                hoveredCommit = nil
                            }
                        }
                        .animation(VPMotion.hover, value: hoveredCommit)
                        .animation(VPMotion.snap, value: model.gitSelectedCommit)
                    }
                }
                .padding(.horizontal, 6)
                .padding(.vertical, 4)
            }

            if model.gitCommits.isEmpty, !model.gitLogBusy {
                Text("No commits on this session branch yet.")
                    .font(VPTheme.mono(9))
                    .foregroundStyle(VPTheme.textSecondary)
                    .padding(12)
            }
        }
    }

    private func detailPane(compact: Bool) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                if compact {
                    Button {
                        model.gitSelectedCommit = nil
                        model.gitShow = nil
                    } label: {
                        Image(systemName: "chevron.left")
                            .font(.system(size: 9, weight: .semibold))
                    }
                    .buttonStyle(VPGlassIconButtonStyle(size: 22, tint: VPTheme.textSecondary))
                    .help("Back to commits")
                } else {
                    Image(systemName: "clock.arrow.circlepath")
                        .font(.system(size: 10))
                        .foregroundStyle(VPTheme.textTertiary)
                }

                if let show = model.gitShow {
                    Text(String(show.commit.prefix(12)))
                        .font(VPTheme.mono(9))
                        .foregroundStyle(VPTheme.textSecondary)
                    if show.truncated {
                        Text("truncated")
                            .font(VPTheme.mono(7, weight: .medium))
                            .foregroundStyle(VPTheme.warning)
                            .padding(.horizontal, 4)
                            .padding(.vertical, 1)
                            .background(VPTheme.warning.opacity(0.10), in: Capsule())
                    }
                } else if let selected = model.gitSelectedCommit {
                    Text(String(selected.prefix(12)))
                        .font(VPTheme.mono(9))
                        .foregroundStyle(VPTheme.textSecondary)
                } else {
                    Text("Commit")
                        .font(VPTheme.mono(9))
                        .foregroundStyle(VPTheme.textSecondary)
                }

                Spacer()

                if model.gitShowBusy {
                    ProgressView().controlSize(.small)
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)

            VPHairline()

            if let error = model.gitShowError {
                Text(error)
                    .font(VPTheme.mono(9))
                    .foregroundStyle(VPTheme.warning)
                    .padding(10)
            }

            if let show = model.gitShow {
                ScrollView([.vertical, .horizontal]) {
                    Text(show.output)
                        .font(VPTheme.mono(9))
                        .foregroundStyle(VPTheme.textSecondary)
                        .textSelection(.enabled)
                        .fixedSize(horizontal: true, vertical: false)
                        .padding(10)
                }
            } else {
                VStack(spacing: 8) {
                    Image(systemName: "clock.arrow.circlepath")
                        .font(.system(size: 24))
                        .foregroundStyle(VPTheme.textTertiary)
                    Text(
                        model.gitLogBusy
                            ? "Loading commits…"
                            : "Select a commit to inspect its diff."
                    )
                    .font(VPTheme.mono(9))
                    .foregroundStyle(VPTheme.textSecondary)
                    if model.gitShowBusy {
                        ProgressView().controlSize(.small)
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
    }
}

private struct WorkspaceFileTab: Identifiable {
    let path: String
    var file: WorkspaceFileResult?
    var draft: String
    var error: String?
    var status: String?
    var saving = false

    var id: String {
        path
    }

    var dirty: Bool {
        guard let file, file.editable else { return false }
        return draft != file.content
    }
}

private final class WorkspaceTreeNode: Identifiable {
    let name: String
    let path: String
    let isDirectory: Bool
    let size: UInt64
    var children: [WorkspaceTreeNode]

    var id: String {
        path
    }

    init(name: String, path: String, isDirectory: Bool, size: UInt64, children: [WorkspaceTreeNode]) {
        self.name = name
        self.path = path
        self.isDirectory = isDirectory
        self.size = size
        self.children = children
    }
}

private struct FilesPanel: View {
    @Environment(AppModel.self) private var model
    @State private var hoveredPath: String?
    @State private var tabs: [WorkspaceFileTab] = []
    @State private var activePath: String?
    @State private var pendingClosePath: String?
    @State private var expandedPaths: Set<String> = []

    private var activeTab: WorkspaceFileTab? {
        tabs.first { $0.path == activePath }
    }

    private var tree: [WorkspaceTreeNode] {
        Self.buildTree(model.workspaceEntries, filter: model.workspaceFilter)
    }

    var body: some View {
        @Bindable var model = model
        GeometryReader { proxy in
            let compact = proxy.size.width < 620
            if compact {
                if activePath != nil {
                    detailPane(compact: true)
                } else {
                    listPane(filter: $model.workspaceFilter)
                }
            } else {
                HStack(spacing: 0) {
                    listPane(filter: $model.workspaceFilter)
                        .frame(width: min(320, max(240, proxy.size.width * 0.36)))
                    VPHairline(vertical: true)
                    detailPane(compact: false)
                }
            }
        }
        .task(id: model.selectedSessionID) {
            tabs = []
            activePath = nil
            expandedPaths = []
            if model.workspaceEntries.isEmpty {
                await model.loadWorkspaceTree()
            }
        }
        .confirmationDialog(
            L10n.t("Discard unsaved changes?"),
            isPresented: Binding(
                get: { pendingClosePath != nil },
                set: {
                    if !$0 {
                        pendingClosePath = nil
                    }
                }
            )
        ) {
            Button(L10n.t("Discard"), role: .destructive) {
                if let path = pendingClosePath {
                    close(path)
                }
                pendingClosePath = nil
            }
            Button(L10n.t("Cancel"), role: .cancel) { pendingClosePath = nil }
        } message: {
            Text(L10n.t("This file has unsaved changes."))
        }
    }

    private func listPane(filter: Binding<String>) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 7) {
                Image(systemName: "magnifyingglass")
                    .font(.system(size: 9))
                    .foregroundStyle(VPTheme.textTertiary)
                TextField(L10n.t("Filter files"), text: filter)
                    .textFieldStyle(.plain)
                    .font(VPTheme.mono(9))
                if model.workspaceTreeBusy {
                    ProgressView().controlSize(.small)
                }
                if !tree.isEmpty {
                    Button {
                        expandedPaths = Set(allDirectoryPaths(in: tree))
                    } label: {
                        Image(systemName: "chevron.down.circle")
                            .font(.system(size: 9, weight: .medium))
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(VPTheme.textTertiary)
                    .help(L10n.t("Expand all folders"))
                    Button {
                        expandedPaths = []
                    } label: {
                        Image(systemName: "chevron.right.circle")
                            .font(.system(size: 9, weight: .medium))
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(VPTheme.textTertiary)
                    .help(L10n.t("Collapse all folders"))
                }
            }
            .padding(8)

            VPHairline()

            if let error = model.workspaceTreeError {
                Text(error)
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.warning)
                    .padding(8)
            }

            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(tree) { node in
                        WorkspaceTreeNodeView(
                            node: node,
                            depth: 0,
                            activePath: activePath,
                            hoveredPath: $hoveredPath,
                            expandedPaths: expandedPaths,
                            forceExpanded: !model.workspaceFilter
                                .trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
                            onToggle: toggleFolder,
                            onOpen: open
                        )
                    }
                }
                .padding(.horizontal, 6)
                .padding(.vertical, 4)
            }

            if tree.isEmpty {
                Text(model.workspaceTreeBusy ? L10n.t("Loading files…") : L10n
                    .t("No files match the filter."))
                    .font(VPTheme.mono(9))
                    .foregroundStyle(VPTheme.textSecondary)
                    .padding(12)
            }
        }
    }

    private func detailPane(compact: Bool) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            tabStrip(compact: compact)
            VPHairline()
            if let tab = activeTab {
                detailHeader(tab: tab, compact: compact)
                VPHairline()
                if let error = tab.error {
                    Text(error)
                        .font(VPTheme.mono(9))
                        .foregroundStyle(VPTheme.warning)
                        .padding(10)
                }
                if let status = tab.status {
                    Text(status)
                        .font(VPTheme.mono(8))
                        .foregroundStyle(status.hasPrefix("Saved") ? VPTheme.success : VPTheme.warning)
                        .padding(.horizontal, 10)
                        .padding(.top, 5)
                }
                TextEditor(
                    text: Binding(
                        get: { tab.draft },
                        set: { updateDraft($0, path: tab.path) }
                    )
                )
                .font(VPTheme.mono(10))
                .foregroundStyle(VPTheme.textSecondary)
                .scrollContentBackground(.hidden)
                .disabled(!(tab.file?.editable ?? false) || tab.saving)
                .padding(6)
            } else {
                VStack(spacing: 8) {
                    Image(systemName: "doc.text.magnifyingglass")
                        .font(.system(size: 24))
                        .foregroundStyle(VPTheme.textTertiary)
                    Text(L10n.t("Select a file to inspect the session worktree."))
                        .font(VPTheme.mono(9))
                        .foregroundStyle(VPTheme.textSecondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
    }

    private func tabStrip(compact: Bool) -> some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 4) {
                if compact {
                    Button {
                        activePath = nil
                    } label: {
                        Image(systemName: "chevron.left")
                            .font(.system(size: 9, weight: .semibold))
                    }
                    .buttonStyle(VPGlassIconButtonStyle(size: 22, tint: VPTheme.textSecondary))
                    .help(L10n.t("Back to files"))
                }
                ForEach(tabs) { tab in
                    HStack(spacing: 4) {
                        Button {
                            activePath = tab.path
                        } label: {
                            HStack(spacing: 4) {
                                Image(systemName: tab.dirty ? "circle.fill" : "doc.text")
                                    .font(.system(size: tab.dirty ? 5 : 8))
                                    .foregroundStyle(tab.dirty ? VPTheme.accent : VPTheme.textTertiary)
                                Text((tab.path as NSString).lastPathComponent)
                                    .font(VPTheme.mono(
                                        7.5,
                                        weight: activePath == tab.path ? .semibold : .regular
                                    ))
                                    .foregroundStyle(activePath == tab.path ? VPTheme.textPrimary : VPTheme
                                        .textSecondary)
                                    .lineLimit(1)
                            }
                        }
                        .buttonStyle(.plain)
                        Button {
                            requestClose(tab)
                        } label: {
                            Image(systemName: "xmark")
                                .font(.system(size: 6, weight: .bold))
                                .foregroundStyle(VPTheme.textTertiary)
                        }
                        .buttonStyle(.plain)
                    }
                    .padding(.horizontal, 7)
                    .padding(.vertical, 5)
                    .background(
                        activePath == tab.path ? VPTheme.accent.opacity(0.10) : VPTheme.surface,
                        in: RoundedRectangle(cornerRadius: 6, style: .continuous)
                    )
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 5)
        }
        .background(VPTheme.surface.opacity(0.35))
    }

    private func detailHeader(tab: WorkspaceFileTab, compact _: Bool) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 7) {
                Image(systemName: tab.file?.binary == true ? "doc.zipper" : "doc.text")
                    .font(.system(size: 10))
                    .foregroundStyle(VPTheme.textTertiary)
                Text(tab.path)
                    .font(VPTheme.mono(9))
                    .foregroundStyle(VPTheme.textSecondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: 4)
                if let file = tab.file {
                    Text(formatSize(file.size))
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
            HStack(spacing: 5) {
                if tab.file?.binary == true {
                    fileBadge("binary", tint: VPTheme.warning)
                }
                if tab.file?.truncated == true {
                    fileBadge("truncated", tint: VPTheme.warning)
                }
                if tab.dirty {
                    fileBadge("modified", tint: VPTheme.accent)
                }
                Spacer(minLength: 0)
                if tab.file?.editable == true {
                    Button(L10n.t("Reload")) {
                        Task { await reload(tab.path) }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(tab.saving)
                    Button(L10n.t("Save")) {
                        Task { await save(tab.path) }
                    }
                    .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                    .controlSize(.small)
                    .disabled(!tab.dirty || tab.saving)
                }
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
    }

    private func fileBadge(_ text: String, tint: Color) -> some View {
        Text(L10n.t(text))
            .font(VPTheme.mono(7, weight: .medium))
            .foregroundStyle(tint)
            .padding(.horizontal, 4)
            .padding(.vertical, 1)
            .background(tint.opacity(0.10), in: Capsule())
    }

    private func open(_ entry: WorkspaceEntry) {
        if tabs.contains(where: { $0.path == entry.path }) {
            activePath = entry.path
            return
        }
        var tab = WorkspaceFileTab(path: entry.path, file: nil, draft: "", error: nil, status: nil)
        tab.status = L10n.t("Loading…")
        tabs.append(tab)
        activePath = entry.path
        Task { await load(entry.path) }
    }

    private func load(_ path: String) async {
        guard let file = await model.fetchWorkspaceFile(path: path) else {
            updateTab(path) {
                $0.error = L10n.t("Could not load this file.")
                $0.status = nil
            }
            return
        }
        updateTab(path) {
            $0.file = file
            $0.draft = file.content
            $0.error = nil
            $0.status = nil
        }
    }

    private func reload(_ path: String) async {
        updateTab(path) { $0.status = L10n.t("Reloading…") }
        await load(path)
    }

    private func save(_ path: String) async {
        guard let tab = tabs.first(where: { $0.path == path }) else { return }
        updateTab(path) { $0.saving = true; $0.status = L10n.t("Saving…") }
        let result = await model.writeWorkspaceFile(
            path: path,
            content: tab.draft,
            expectedSha256: tab.file?.sha256
        )
        updateTab(path) {
            $0.saving = false
            if let result {
                $0.file = WorkspaceFileResult(
                    path: result.path,
                    size: result.size,
                    truncated: false,
                    binary: false,
                    content: tab.draft,
                    sha256: result.sha256,
                    editable: true
                )
                $0.status = L10n.t("Saved")
            } else {
                $0.status = L10n.t("Save failed")
            }
        }
    }

    private func updateDraft(_ value: String, path: String) {
        updateTab(path) { $0.draft = value }
    }

    private func updateTab(_ path: String, _ mutate: (inout WorkspaceFileTab) -> Void) {
        guard let index = tabs.firstIndex(where: { $0.path == path }) else { return }
        mutate(&tabs[index])
    }

    private func requestClose(_ tab: WorkspaceFileTab) {
        if tab.dirty {
            pendingClosePath = tab.path
        } else {
            close(tab.path)
        }
    }

    private func close(_ path: String) {
        guard let index = tabs.firstIndex(where: { $0.path == path }) else { return }
        tabs.remove(at: index)
        if activePath == path {
            activePath = tabs.indices.contains(index) ? tabs[index].path : tabs.last?.path
        }
    }

    private func toggleFolder(_ path: String) {
        if expandedPaths.contains(path) {
            expandedPaths.remove(path)
        } else {
            expandedPaths.insert(path)
        }
    }

    private func allDirectoryPaths(in nodes: [WorkspaceTreeNode]) -> [String] {
        nodes.flatMap { node -> [String] in
            guard node.isDirectory else { return [] }
            return [node.path] + allDirectoryPaths(in: node.children)
        }
    }

    private static func buildTree(_ entries: [WorkspaceEntry], filter: String) -> [WorkspaceTreeNode] {
        let query = filter.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let grouped = Dictionary(grouping: entries) { entry -> String in
            let components = entry.path.split(separator: "/").map(String.init)
            return components.dropLast().joined(separator: "/")
        }
        func nodes(parent: String) -> [WorkspaceTreeNode] {
            (grouped[parent] ?? [])
                .sorted { lhs, rhs in
                    if lhs.isDirectory != rhs.isDirectory {
                        return lhs.isDirectory
                    }
                    return lhs.name.localizedStandardCompare(rhs.name) == .orderedAscending
                }
                .compactMap { entry in
                    if entry.isDirectory {
                        let children = nodes(parent: entry.path)
                        if !query.isEmpty, children.isEmpty {
                            return nil
                        }
                        return WorkspaceTreeNode(
                            name: entry.name,
                            path: entry.path,
                            isDirectory: true,
                            size: entry.size,
                            children: children
                        )
                    }
                    if !query.isEmpty,
                       !entry.path.lowercased().contains(query),
                       !entry.name.lowercased().contains(query)
                    {
                        return nil
                    }
                    return WorkspaceTreeNode(
                        name: entry.name,
                        path: entry.path,
                        isDirectory: false,
                        size: entry.size,
                        children: []
                    )
                }
        }
        return nodes(parent: "")
    }
}

private struct WorkspaceTreeNodeView: View {
    let node: WorkspaceTreeNode
    let depth: Int
    let activePath: String?
    @Binding var hoveredPath: String?
    let expandedPaths: Set<String>
    let forceExpanded: Bool
    let onToggle: (String) -> Void
    let onOpen: (WorkspaceEntry) -> Void

    private var isExpanded: Bool {
        forceExpanded || expandedPaths.contains(node.path)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if node.isDirectory {
                Button {
                    onToggle(node.path)
                } label: {
                    row(
                        icon: isExpanded ? "folder.fill" : "folder",
                        tint: VPTheme.plan,
                        isActive: false,
                        expanded: isExpanded
                    )
                }
                .buttonStyle(.plain)
                if isExpanded {
                    ForEach(node.children) { child in
                        WorkspaceTreeNodeView(
                            node: child,
                            depth: depth + 1,
                            activePath: activePath,
                            hoveredPath: $hoveredPath,
                            expandedPaths: expandedPaths,
                            forceExpanded: forceExpanded,
                            onToggle: onToggle,
                            onOpen: onOpen
                        )
                    }
                }
            } else {
                Button {
                    onOpen(
                        WorkspaceEntry(
                            path: node.path,
                            name: node.name,
                            kind: "file",
                            size: node.size
                        )
                    )
                } label: {
                    row(
                        icon: "doc.text",
                        tint: VPTheme.textTertiary,
                        isActive: activePath == node.path,
                        expanded: false
                    )
                }
                .buttonStyle(VPPressableButtonStyle(scale: 0.99))
                .onHover { hovering in
                    hoveredPath = hovering ? node.path : nil
                }
            }
        }
    }

    private func row(icon: String, tint: Color, isActive: Bool, expanded: Bool) -> some View {
        HStack(spacing: 4) {
            Spacer()
                .frame(width: CGFloat(depth) * 10)
            Image(systemName: node.isDirectory
                ? (expanded ? "chevron.down" : "chevron.right")
                : "circle.fill")
                .font(.system(size: node.isDirectory ? 6 : 2.5, weight: .bold))
                .foregroundStyle(VPTheme.textTertiary.opacity(node.isDirectory ? 1 : 0.35))
                .frame(width: 7)
            Image(systemName: icon)
                .font(.system(size: 8.5))
                .foregroundStyle(isActive ? VPTheme.accent : tint)
                .frame(width: 11)
            Text(node.name)
                .font(VPTheme.font(
                    node.isDirectory ? 9.5 : 10,
                    weight: node.isDirectory ? .medium : .regular
                ))
                .foregroundStyle(isActive ? VPTheme.textPrimary : VPTheme.textSecondary)
                .lineLimit(1)
            if node.isDirectory {
                Text("\(node.children.count)")
                    .font(VPTheme.mono(6.2))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            Spacer(minLength: 3)
            if !node.isDirectory {
                Text(formatSize(node.size))
                    .font(VPTheme.mono(6.8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
        }
        .padding(.vertical, 4)
        .padding(.horizontal, 5)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background {
            if isActive {
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(VPTheme.accent.opacity(0.10))
            } else if hoveredPath == node.path, !node.isDirectory {
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(VPTheme.surface)
            }
        }
        .contentShape(Rectangle())
    }
}

private struct ConsolePanel: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                VPStatusDot(
                    color: statusColor,
                    pulsing: model.consoleStatus == "running",
                    size: 7
                )
                Text(model.consoleStatus)
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.secondary)
                if !model.consoleWorktree.isEmpty {
                    Text(model.consoleWorktree)
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.tertiary)
                        .lineLimit(1)
                }
                Spacer()
                if model.consoleBusy {
                    ProgressView().controlSize(.small)
                }
                if model.consoleID != nil {
                    Button("Clear") {
                        model.clearConsole()
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    Button("Interrupt") {
                        Task { await model.interruptConsole() }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(model.consoleStatus != "running")
                    Button("Close") {
                        Task { await model.closeConsole() }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                } else {
                    Button("Open console") {
                        Task { await model.openConsole() }
                    }
                    .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                    .disabled(model.selectedSessionID == nil || model.consoleBusy)
                }
            }
            .padding(10)
            VPGradientRule()
                .frame(height: 1)
            ScrollViewReader { proxy in
                ScrollView {
                    Text(
                        model.consoleOutput.isEmpty
                            ? "Line-oriented shell in the session worktree. Commands run under the Seatbelt write sandbox; full-screen TUI programs are not supported."
                            : model.consoleOutput
                    )
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(model.consoleOutput.isEmpty ? .secondary : .primary)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(10)
                    .id("console-bottom")
                }
                .onChange(of: model.consoleOutput) {
                    proxy.scrollTo("console-bottom", anchor: .bottom)
                }
            }
            VPGradientRule()
                .frame(height: 1)
            if let error = model.consoleError {
                Text(error)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.orange)
                    .padding(.horizontal, 10)
                    .padding(.top, 4)
            }
            HStack(spacing: 8) {
                Text("$")
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.green)
                TextField("command", text: $model.consoleInput)
                    .textFieldStyle(.plain)
                    .font(.system(size: 11, design: .monospaced))
                    .onSubmit {
                        Task { await model.sendConsole() }
                    }
                    .disabled(model.consoleID == nil || model.consoleStatus != "running")
                Button("Send") {
                    Task { await model.sendConsole() }
                }
                .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                .disabled(
                    model.consoleID == nil
                        || model.consoleStatus != "running"
                        || model.consoleInput.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                )
            }
            .padding(10)
        }
        .task(id: model.consoleID) {
            guard model.consoleID != nil else { return }
            while !Task.isCancelled, model.consoleStatus == "running" {
                await model.pollConsole()
                try? await Task.sleep(for: .milliseconds(200))
            }
        }
    }

    private var statusColor: Color {
        switch model.consoleStatus {
        case "running": .green
        case "failed": .red
        case "exited": .secondary
        default: .secondary
        }
    }
}

private struct DiffLines: View {
    let text: String

    var body: some View {
        LazyVStack(alignment: .leading, spacing: 1) {
            ForEach(
                Array(text.split(separator: "\n", omittingEmptySubsequences: false).enumerated()),
                id: \.offset
            ) { _, line in
                let value = String(line)
                Text(value.isEmpty ? " " : value)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(color(for: value))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 3)
                    .background(background(for: value))
            }
        }
        .clipShape(RoundedRectangle(cornerRadius: 6))
    }

    private func color(for line: String) -> Color {
        if line.hasPrefix("+++") || line.hasPrefix("---") {
            return VPTheme.textTertiary
        }
        if line.hasPrefix("+") {
            return VPTheme.success
        }
        if line.hasPrefix("-") {
            return VPTheme.danger
        }
        if line.hasPrefix("@@") || line.hasPrefix("diff --git") {
            return VPTheme.info
        }
        return VPTheme.textSecondary
    }

    private func background(for line: String) -> Color {
        if line.hasPrefix("+"), !line.hasPrefix("+++") {
            return VPTheme.success.opacity(0.10)
        }
        if line.hasPrefix("-"), !line.hasPrefix("---") {
            return VPTheme.danger.opacity(0.09)
        }
        if line.hasPrefix("@@") {
            return VPTheme.info.opacity(0.09)
        }
        return .clear
    }
}

private struct PreviewEventRow: View {
    let event: Event

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(event.type)
                    .font(.system(size: 9, weight: .medium, design: .monospaced))
                Spacer()
                Text("#\(event.seq)")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
            Text(event.payload.prettyString())
                .font(.system(size: 9, design: .monospaced))
                .foregroundStyle(.secondary)
                .lineLimit(4)
        }
        .padding(9)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 9))
    }
}

struct StudioEmptyView: View {
    let icon: String
    let title: String
    let detail: String

    var body: some View {
        VStack(spacing: 9) {
            VPFloatingSymbol(name: icon, size: 26, tint: .secondary)
            Text(L10n.t(title))
                .font(.system(size: 13, weight: .semibold))
            Text(L10n.t(detail))
                .font(.system(size: 10))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 240)
        }
        .vpAppear(index: 1, animation: VPMotion.emphasizedSpring)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(24)
    }
}

struct OrgPresetSheet: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(AppModel.self) private var model
    var body: some View {
        @Bindable var model = model
        return VStack(alignment: .leading, spacing: 18) {
            VStack(alignment: .leading, spacing: 3) {
                Text("Org Preset")
                    .font(.title2.weight(.semibold))
                Text(
                    "Small default team. Managers are created lazily when a plan task needs their department; Workers control batch concurrency. The session budget reserves tokens before each provider call and settles from provider usage."
                )
                .font(.callout)
                .foregroundStyle(.secondary)
            }

            Grid(alignment: .leading, horizontalSpacing: 18, verticalSpacing: 10) {
                GridRow {
                    Text("Managers").font(.system(size: 12))
                    Stepper("\(model.orgManagers) · max 5", value: $model.orgManagers, in: 1 ... 5)
                }
                GridRow {
                    Text("Workers").font(.system(size: 12))
                    Stepper("\(model.orgWorkers) · max 10", value: $model.orgWorkers, in: 1 ... 10)
                }
                GridRow {
                    Text("Labour").font(.system(size: 12))
                    Stepper("\(model.orgLabour) · max 40", value: $model.orgLabour, in: 1 ... 40)
                }
            }
            .font(.system(size: 11))

            Text(
                "Plan dispatch runs ready tasks in dependency batches, each Worker in its own task worktree; Managers are created lazily. Merge queue, deterministic Integrator, cross-entry retry, per-role budgets, and opt-in plan Worker expansion are implemented; general Manager/Labour expansion and scale-down are not."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.tertiary)

            HStack {
                Spacer()
                Button("Apply & Done") {
                    Task { await model.applyOrgSettings() }
                    dismiss()
                }
                .keyboardShortcut(.defaultAction)
                .buttonStyle(.borderedProminent)
            }
        }
        .padding(24)
        .frame(width: 480)
    }
}

private func formatSize(_ bytes: UInt64) -> String {
    if bytes < 1024 {
        return "\(bytes) B"
    }
    let kb = Double(bytes) / 1024
    if kb < 1024 {
        return String(format: "%.1f KB", kb)
    }
    let mb = kb / 1024
    if mb < 1024 {
        return String(format: "%.1f MB", mb)
    }
    return String(format: "%.1f GB", mb / 1024)
}
