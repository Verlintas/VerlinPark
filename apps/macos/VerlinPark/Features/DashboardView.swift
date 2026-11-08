import SwiftUI

enum DashboardSection: String, CaseIterable, Identifiable {
    case overview
    case inbox
    case team
    case usage
    case git
    case checkpoints
    case storage
    case policies
    case diagnostics

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .overview: "Overview"
        case .inbox: "Inbox"
        case .team: "Team & Tasks"
        case .usage: "Usage & Budget"
        case .git: "Git & Forks"
        case .checkpoints: "Checkpoints"
        case .storage: "Storage & Memory"
        case .policies: "Policies"
        case .diagnostics: "Diagnostics"
        }
    }

    var detail: String {
        switch self {
        case .overview: "Workspace, capacity, and pending actions"
        case .inbox: "Approvals and blocking questions"
        case .team: "Agents, scheduler, and task queue"
        case .usage: "Project spend, budget, and per-session usage"
        case .git: "Fork lineage, project forest, and merge-back"
        case .checkpoints: "Snapshots, retention, and approximate storage"
        case .storage: "Artifacts, mailbox, memory, and worktree retention"
        case .policies: "Shell, integration, and safety policy"
        case .diagnostics: "Doctor report and runtime checks"
        }
    }

    var icon: String {
        switch self {
        case .overview: "square.grid.2x2"
        case .inbox: "tray.full"
        case .team: "person.3"
        case .usage: "chart.bar.xaxis"
        case .git: "arrow.triangle.branch"
        case .checkpoints: "camera.on.rectangle"
        case .storage: "externaldrive"
        case .policies: "shield.lefthalf.filled"
        case .diagnostics: "stethoscope"
        }
    }
}

struct DashboardView: View {
    @Environment(AppModel.self) private var model
    @Environment(BrandStore.self) private var brand
    @Environment(MemoryMonitor.self) private var memory
    @Environment(\.dismiss) private var dismiss
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var section: DashboardSection = .overview
    @State private var filter = ""
    @State private var hoveredSection: DashboardSection?
    @State private var refreshing = false
    @FocusState private var filterFocused: Bool
    @Namespace private var sectionNamespace
    @AppStorage("verlinpark.dashboard.section") private var storedSection =
        DashboardSection.overview.rawValue

    var body: some View {
        HStack(spacing: 0) {
            sidebar
            VPGradientRule(vertical: true)
                .frame(width: 1)
            VStack(alignment: .leading, spacing: 0) {
                contentHeader
                VPGradientRule()
                    .frame(height: 1)
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        sectionContent
                            .id(section)
                            .transition(.vpContent)
                    }
                    .padding(20)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .animation(
                        VPMotion.resolved(VPMotion.smooth, reduceMotion: reduceMotion),
                        value: section
                    )
                }
            }
        }
        .frame(width: 1080, height: 740)
        .background(.ultraThinMaterial)
        .onAppear {
            if let saved = DashboardSection(rawValue: storedSection) {
                section = saved
            }
            if section != .overview, section != .team {
                Task { await model.loadDashboardSnapshot() }
            }
            Task { await refreshSection(section) }
        }
        .onChange(of: section) { _, newValue in
            storedSection = newValue.rawValue
            Task { await refreshSection(newValue) }
        }
    }

    private var sidebar: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                BrandMark(config: brand.config, fontSize: 18)
                VStack(alignment: .leading, spacing: 1) {
                    Text("Manage Dash")
                        .font(.system(size: 16, weight: .semibold, design: .rounded))
                    Text(brand.config.effectiveName)
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
            }
            .padding(.bottom, 6)

            projectPicker
            sessionPicker

            Divider().opacity(0.2)
                .padding(.vertical, 4)

            HStack(spacing: 6) {
                Image(systemName: "magnifyingglass")
                    .font(.system(size: 9))
                    .foregroundStyle(.tertiary)
                TextField("Filter sections", text: $filter)
                    .textFieldStyle(.plain)
                    .font(.system(size: 9, design: .monospaced))
                    .focused($filterFocused)
            }
            .padding(.horizontal, 7)
            .padding(.vertical, 5)
            .background(.white.opacity(0.04), in: RoundedRectangle(cornerRadius: 7))
            .vpFocusRing(filterFocused, color: .accentColor, radius: 7)
            .padding(.bottom, 2)
            .help("Filter sections by title or description")

            if filteredSections.isEmpty {
                Text("No sections match.")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 6)
            }
            ForEach(Array(filteredSections.enumerated()), id: \.element.id) { index, item in
                Button {
                    section = item
                } label: {
                    HStack(spacing: 8) {
                        Image(systemName: item.icon)
                            .font(.system(size: 11, weight: .medium))
                            .frame(width: 16)
                        Text(item.title)
                            .font(.system(size: 11, weight: .medium))
                        Spacer(minLength: 4)
                        if let badge = badge(for: item), badge > 0 {
                            VPCountBadge(
                                count: badge,
                                tint: item == .inbox ? .orange : .accentColor,
                                compact: true
                            )
                        }
                    }
                    .foregroundStyle(section == item ? Color.primary : Color.secondary)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 6)
                    .background {
                        if section == item {
                            RoundedRectangle(cornerRadius: 8)
                                .fill(Color.accentColor.opacity(0.16))
                                .matchedGeometryEffect(
                                    id: "dashboard-section",
                                    in: sectionNamespace
                                )
                        } else if hoveredSection == item {
                            RoundedRectangle(cornerRadius: 8)
                                .fill(.white.opacity(0.045))
                        }
                    }
                    .overlay {
                        RoundedRectangle(cornerRadius: 8)
                            .strokeBorder(
                                section == item ? Color.accentColor.opacity(0.18) : .clear,
                                lineWidth: 1
                            )
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(VPPressableButtonStyle(scale: 0.985))
                .onHover { hovering in
                    if hovering {
                        hoveredSection = item
                    } else if hoveredSection == item {
                        hoveredSection = nil
                    }
                }
                .animation(VPMotion.hover, value: hoveredSection)
                .animation(VPMotion.snap, value: section)
                .keyboardShortcut(
                    KeyEquivalent(Character(String(index + 1))),
                    modifiers: .command
                )
                .help("\(item.detail) · ⌘\(index + 1)")
            }

            Spacer(minLength: 10)
            Text("⌘1–⌘9 switch sections")
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
                .padding(.bottom, 2)
            connectionFooter
            Button("Done") { dismiss() }
                .buttonStyle(VPPrimaryButtonStyle(tint: .accentColor))
                .keyboardShortcut(.cancelAction)
        }
        .padding(14)
        .frame(width: 214)
        .background(.black.opacity(0.10))
    }

    private var filteredSections: [DashboardSection] {
        let query = filter.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else { return DashboardSection.allCases }
        return DashboardSection.allCases.filter { item in
            item.title.localizedCaseInsensitiveContains(query)
                || item.detail.localizedCaseInsensitiveContains(query)
        }
    }

    private var projectPicker: some View {
        Picker(
            "Project",
            selection: Binding(
                get: { model.selectedProjectID ?? "" },
                set: { model.selectProject($0.isEmpty ? nil : $0) }
            )
        ) {
            Text("All projects").tag("")
            ForEach(model.projects) { project in
                Text(project.name).tag(project.id)
            }
        }
        .pickerStyle(.menu)
        .controlSize(.small)
        .help("Switch project without closing Manage Dash")
    }

    private var sessionPicker: some View {
        Picker(
            "Session",
            selection: Binding(
                get: { model.selectedSessionID ?? "" },
                set: { model.selectSession($0.isEmpty ? nil : $0) }
            )
        ) {
            Text("No session").tag("")
            ForEach(model.sessions) { session in
                Text(session.goal).tag(session.id)
            }
        }
        .pickerStyle(.menu)
        .controlSize(.small)
        .disabled(model.selectedProjectID == nil)
        .help("Switch session without closing Manage Dash")
    }

    private var connectionFooter: some View {
        HStack(spacing: 6) {
            VPStatusDot(
                color: model.connection == .connected ? .green : .orange,
                pulsing: model.connection == .connecting,
                size: 6
            )
            Text(model.connection == .connected ? "daemon connected" : "daemon offline")
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.secondary)
            Spacer()
            Text("v0.1")
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
        }
        .padding(.bottom, 4)
    }

    private var contentHeader: some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 8) {
                    Image(systemName: section.icon)
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(.secondary)
                        .contentTransition(.symbolEffect(.replace))
                    Text(section.title)
                        .font(.system(size: 20, weight: .semibold, design: .rounded))
                        .contentTransition(.opacity)
                }
                .animation(VPMotion.snap, value: section)
                Text(section.detail)
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.secondary)
                HStack(spacing: 8) {
                    if let project = model.selectedProject {
                        Text(project.name)
                            .lineLimit(1)
                        Text("·")
                        Text(project.root)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    } else {
                        Text("No project selected · project-scoped cards are disabled")
                    }
                    if let session = model.selectedSession {
                        Text("·")
                        Text(session.goal)
                            .lineLimit(1)
                    }
                }
                .font(.system(size: 9, design: .monospaced))
                .foregroundStyle(.tertiary)
            }
            Spacer()
            Button {
                Task {
                    refreshing = true
                    await refreshSection(section)
                    refreshing = false
                }
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
                    .font(.system(size: 10, weight: .medium))
                    .padding(.horizontal, 9)
                    .padding(.vertical, 4)
                    .background(.white.opacity(0.05), in: Capsule())
                    .overlay {
                        Capsule().strokeBorder(.white.opacity(0.07), lineWidth: 1)
                    }
                    .rotationEffect(.degrees(refreshing && !reduceMotion ? 360 : 0))
                    .animation(
                        refreshing && !reduceMotion ? VPMotion.spin : VPMotion.snap,
                        value: refreshing
                    )
            }
            .buttonStyle(VPPressableButtonStyle(scale: 0.96))
            .controlSize(.small)
            .keyboardShortcut("r", modifiers: .command)
            .disabled(refreshing)
            .help("Refresh the visible section · ⌘R")
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 16)
    }

    @ViewBuilder
    private var sectionContent: some View {
        switch section {
        case .overview:
            DashboardOverviewView(section: $section)
        case .inbox:
            ApprovalCenterCard()
            QuestionCenterCard()
        case .team:
            AgentsCard()
            TaskSchedulerCard()
        case .usage:
            if let project = model.selectedProject {
                ProjectUsageCard(project: project)
                ProjectBudgetCard(project: project)
            } else {
                DashboardNoProjectCard(detail: "Select a project to inspect usage and budgets.")
            }
        case .git:
            ForksCard()
            ProjectForkForestCard()
        case .checkpoints:
            CheckpointsCard()
        case .storage:
            if let project = model.selectedProject {
                MemoryCard(project: project)
                MemoryPolicyCard(project: project)
                ArtifactPolicyCard(project: project)
                MailboxPolicyCard(project: project)
                WorktreeCleanupCard(project: project)
            } else {
                DashboardNoProjectCard(detail: "Select a project to inspect storage and retention.")
            }
        case .policies:
            if let project = model.selectedProject {
                ShellPolicyCard(project: project)
                IntegrationPolicyCard(project: project)
            } else {
                DashboardNoProjectCard(detail: "Select a project to edit policies.")
            }
        case .diagnostics:
            DoctorCard()
        }
    }

    private func badge(for section: DashboardSection) -> Int? {
        switch section {
        case .inbox:
            if let summary = model.dashboardSnapshot?.summary {
                summary.pendingApprovals + summary.pendingQuestions
            } else {
                model.pendingApprovals.count + model.pendingQuestions.count
            }
        case .team:
            model.dashboardSnapshot?.summary.runningTasks
                ?? model.taskQueue?.tasks.count(where: { $0.status == "running" })
                ?? 0
        case .git:
            model.dashboardSnapshot?.summary.forkRoots
                ?? model.projectForkForest?.roots.count
                ?? 0
        case .checkpoints:
            model.dashboardSnapshot?.summary.checkpoints ?? model.checkpoints.count
        default:
            nil
        }
    }

    private func refreshSection(_ section: DashboardSection) async {
        switch section {
        case .overview:
            await model.refreshAgentInfo()
            await model.loadProjectUsage()
            await model.loadProjectForkForest()
            await model.loadDashboardSnapshot()
            await model.loadAgents()
            await model.loadPendingApprovals()
            await model.loadPendingQuestions()
        case .inbox:
            await model.loadPendingApprovals()
            await model.loadPendingQuestions()
        case .team:
            await model.loadAgents()
            await model.loadTaskQueue()
            await model.loadDashboardSnapshot()
            await model.refreshAgentInfo()
        case .usage:
            await model.loadProjectUsage()
            await model.loadProjectBudget()
        case .git:
            await model.loadProjectForkForest()
            await model.loadLineage()
            if model.forkTree != nil {
                await model.loadForkTree()
            }
            await model.loadCheckpoints()
        case .checkpoints:
            await model.loadCheckpoints()
            await model.loadCheckpointPolicy()
            await model.loadCheckpointStats()
        case .storage:
            await model.loadMemories()
            await model.loadMemoryPolicy()
            await model.loadArtifactPolicy()
            await model.loadMailboxPolicy()
            await model.loadWorktreePolicy()
        case .policies:
            await model.loadShellPolicy()
            await model.loadIntegrationPolicy()
        case .diagnostics:
            await model.runDoctor()
        }
    }
}
