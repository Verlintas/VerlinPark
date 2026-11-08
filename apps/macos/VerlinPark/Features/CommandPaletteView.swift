import SwiftUI

/// A keyboard-first command surface for switching context and running the
/// common global actions. It is deliberately a lightweight overlay rather than
/// a separate window so it can animate over the current workspace.
struct CommandPaletteView: View {
    @Environment(AppModel.self) private var model
    @Environment(BrandStore.self) private var brand
    @Environment(\.openSettings) private var openSettings
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    @Binding var isPresented: Bool
    let openDashboard: () -> Void
    let openNewSession: () -> Void
    let openSearch: () -> Void
    let openShortcuts: () -> Void
    let openNotifications: () -> Void
    let toggleInspector: () -> Void
    let toggleFocusMode: () -> Void
    let resetLayout: () -> Void
    let applyLayoutPreset: (WorkspaceLayoutPreset) -> Void
    let toggleLanguage: () -> Void

    @State private var query = ""
    @State private var selectedIndex = 0
    @State private var recents = PaletteRecents.load()
    @State private var scope: PaletteScope = .all
    @State private var hoveringID: String?
    @FocusState private var queryFocused: Bool
    @Namespace private var selectionNamespace

    var body: some View {
        ZStack(alignment: .top) {
            Group {
                if reduceTransparency {
                    Rectangle()
                        .fill(VPTheme.solidCanvas)
                        .ignoresSafeArea()
                } else {
                    Rectangle()
                        .fill(VPTheme.canvas.opacity(0.62))
                        .background(.ultraThinMaterial)
                        .ignoresSafeArea()
                }
            }
            .onTapGesture { close() }
            .transition(.opacity)

            palette
                .padding(.top, 96)
                .padding(.horizontal, 24)
                .transition(.vpScaleFade)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .onAppear {
            query = ""
            selectedIndex = 0
            DispatchQueue.main.async {
                queryFocused = true
            }
        }
        .onChange(of: query) { _, _ in
            selectedIndex = 0
        }
        .onChange(of: scope) { _, _ in
            selectedIndex = 0
        }
        .task {
            await model.loadAllSessions()
        }
    }

    private var palette: some View {
        VStack(spacing: 0) {
            searchHeader
            scopeBar
            VPGradientRule()
                .frame(height: 1)
            results
            VPGradientRule()
                .frame(height: 1)
            footer
        }
        .frame(width: 660)
        .frame(maxHeight: 560)
        .background(
            VPTheme.surfaceRaised,
            in: RoundedRectangle(cornerRadius: 18, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 18, style: .continuous)
                .strokeBorder(VPTheme.hairlineStrong, lineWidth: 1)
        }
        .overlay(alignment: .top) {
            LinearGradient(
                colors: [VPTheme.accent.opacity(0.55), .clear],
                startPoint: .leading,
                endPoint: .trailing
            )
            .frame(height: 1)
            .padding(.horizontal, 40)
        }
        .shadow(color: .black.opacity(0.48), radius: 52, y: 24)
        .shadow(color: VPTheme.accent.opacity(0.10), radius: 36, y: 6)
        .clipShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
        .onKeyPress(.downArrow) {
            move(1)
            return .handled
        }
        .onKeyPress(.upArrow) {
            move(-1)
            return .handled
        }
        .onKeyPress(.escape) {
            close()
            return .handled
        }
    }

    private var searchHeader: some View {
        HStack(spacing: 12) {
            Image(systemName: "command")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(VPTheme.accent)
                .frame(width: 30, height: 30)
                .background(VPTheme.accent.opacity(0.11), in: RoundedRectangle(cornerRadius: 8))

            TextField("Jump to a project, session, or action…", text: $query)
                .textFieldStyle(.plain)
                .font(VPTheme.font(15, weight: .medium, rounded: true))
                .focused($queryFocused)
                .onSubmit { executeSelected() }

            if !query.isEmpty {
                Button {
                    query = ""
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .font(.system(size: 13))
                        .foregroundStyle(.tertiary)
                }
                .buttonStyle(VPPressableButtonStyle(scale: 0.9))
                .transition(.vpScaleFade)
            }

            PaletteKeyCap("esc")
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 13)
    }

    private var results: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 4) {
                    ForEach(Array(rankedItems.enumerated()), id: \.element.id) { index, item in
                        row(item, selected: index == selectedIndex)
                            .id(item.id)
                            .transition(.vpContent)
                    }
                    if rankedItems.isEmpty {
                        emptyState
                    }
                }
                .padding(10)
                .animation(VPMotion.smooth, value: rankedItems.count)
            }
            .onChange(of: selectedIndex) { _, newValue in
                guard rankedItems.indices.contains(newValue) else { return }
                withAnimation(VPMotion.resolved(VPMotion.hover, reduceMotion: reduceMotion)) {
                    proxy.scrollTo(rankedItems[newValue].id, anchor: .center)
                }
            }
        }
        .frame(maxHeight: 380)
    }

    private var scopeBar: some View {
        HStack(spacing: 4) {
            ForEach(PaletteScope.allCases) { candidate in
                let selected = scope == candidate
                Button {
                    withAnimation(VPMotion.snap) {
                        scope = candidate
                    }
                } label: {
                    HStack(spacing: 4) {
                        if let icon = candidate.icon {
                            Image(systemName: icon)
                                .font(.system(size: 7, weight: .semibold))
                        }
                        Text(candidate.title)
                            .font(VPTheme.mono(8, weight: .medium))
                        let count = candidate.count(in: items)
                        if count > 0 {
                            Text("\(count)")
                                .font(VPTheme.mono(7))
                                .foregroundStyle(selected ? VPTheme.accent : VPTheme.textTertiary)
                        }
                    }
                    .foregroundStyle(selected ? VPTheme.textPrimary : VPTheme.textSecondary)
                    .padding(.horizontal, 7)
                    .padding(.vertical, 4)
                    .background {
                        Capsule()
                            .fill(selected ? VPTheme.accent.opacity(0.10) : .clear)
                            .overlay {
                                Capsule().strokeBorder(
                                    selected ? VPTheme.accent.opacity(0.22) : .clear,
                                    lineWidth: 1
                                )
                            }
                    }
                }
                .buttonStyle(VPPressableButtonStyle(scale: 0.97))
            }
            Spacer()
        }
        .padding(.horizontal, 16)
        .padding(.bottom, 8)
    }

    private var emptyState: some View {
        VStack(spacing: 8) {
            VPFloatingSymbol(name: "magnifyingglass", size: 24, tint: .secondary)
            Text("No matching commands")
                .font(.system(size: 12, weight: .medium))
            Text("Try a project name, session goal, or an action such as “dashboard”.")
                .font(.system(size: 9, design: .monospaced))
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 36)
    }

    private var footer: some View {
        HStack(spacing: 12) {
            PaletteKeyCap("↑")
            PaletteKeyCap("↓")
            Text("navigate")
            PaletteKeyCap("↵")
            Text("open")
            Spacer()
            Text("\(rankedItems.count) result\(rankedItems.count == 1 ? "" : "s")")
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
                .contentTransition(.numericText())
        }
        .font(.system(size: 9, design: .monospaced))
        .foregroundStyle(.secondary)
        .padding(.horizontal, 16)
        .padding(.vertical, 9)
    }

    private func row(_ result: RankedPaletteItem, selected: Bool) -> some View {
        let item = result.item
        return Button {
            execute(item)
        } label: {
            HStack(spacing: 11) {
                Image(systemName: item.icon)
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(item.tint)
                    .frame(width: 28, height: 28)
                    .background(item.tint.opacity(0.10), in: RoundedRectangle(cornerRadius: 8))

                VStack(alignment: .leading, spacing: 2) {
                    VPHighlightedText(text: result.title, ranges: result.titleRanges)
                        .font(.system(size: 12, weight: .medium))
                        .lineLimit(1)
                    VPHighlightedText(text: result.subtitle, ranges: result.subtitleRanges)
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }

                Spacer(minLength: 8)
                if query.isEmpty, recents.contains(item.id) {
                    Image(systemName: "clock.arrow.circlepath")
                        .font(.system(size: 8, weight: .semibold))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                Text(item.kindLabel)
                    .font(.system(size: 8, weight: .medium, design: .monospaced))
                    .foregroundStyle(item.tint.opacity(0.85))
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(item.tint.opacity(0.10), in: Capsule())

                if selected {
                    Image(systemName: "return")
                        .font(.system(size: 9, weight: .semibold))
                        .foregroundStyle(.tertiary)
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .background {
                if selected {
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .fill(item.tint.opacity(0.10))
                        .matchedGeometryEffect(id: "palette-selection", in: selectionNamespace)
                } else if hoveringID == item.id {
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .fill(VPTheme.surface)
                }
            }
            .overlay {
                if selected {
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .strokeBorder(item.tint.opacity(0.20), lineWidth: 1)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(VPPressableButtonStyle(scale: 0.992))
        .onHover { hovering in
            if hovering {
                hoveringID = item.id
            } else if hoveringID == item.id {
                hoveringID = nil
            }
        }
        .animation(VPMotion.hover, value: hoveringID)
    }

    private struct RankedPaletteItem: Identifiable {
        let item: PaletteItem
        let title: String
        let subtitle: String
        let titleRanges: [Range<Int>]
        let subtitleRanges: [Range<Int>]

        var id: String {
            item.id
        }
    }

    private var rankedItems: [RankedPaletteItem] {
        let sourceItems = items.filter { scope.includes($0.kind) }
        let candidates = sourceItems.map {
            CommandPaletteCandidate(
                id: $0.id,
                title: L10n.t($0.title),
                subtitle: L10n.t($0.subtitle),
                keywords: "\($0.keywords) \($0.title) \($0.subtitle)"
            )
        }
        let byID = Dictionary(uniqueKeysWithValues: sourceItems.map { ($0.id, $0) })
        return CommandPaletteRanker.rank(candidates, query: query, recents: recents)
            .compactMap { match in
                guard let item = byID[match.id],
                      let candidate = candidates.first(where: { $0.id == match.id })
                else { return nil }
                return RankedPaletteItem(
                    item: item,
                    title: candidate.title,
                    subtitle: candidate.subtitle,
                    titleRanges: match.titleRanges,
                    subtitleRanges: match.subtitleRanges
                )
            }
    }

    private var items: [PaletteItem] {
        var items: [PaletteItem] = [
            PaletteItem(
                id: "action-new-session",
                title: "New Session",
                subtitle: "Start a Leader with a concrete goal",
                icon: "plus.bubble",
                tint: VPTheme.accent,
                kind: .action,
                keywords: "create start session task",
                action: {
                    close()
                    openNewSession()
                }
            ),
            PaletteItem(
                id: "action-dashboard",
                title: "Manage Dash",
                subtitle: "Agents, cost, queue, checkpoints, policies",
                icon: "rectangle.3.group",
                tint: VPTheme.resident,
                kind: .action,
                keywords: "dashboard manage agents usage queue",
                action: {
                    close()
                    openDashboard()
                }
            ),
            PaletteItem(
                id: "action-search",
                title: "Search Messages & Events",
                subtitle: "Full-text search across the current scope",
                icon: "magnifyingglass",
                tint: VPTheme.warning,
                kind: .action,
                keywords: "find query search transcript event",
                action: {
                    close()
                    openSearch()
                }
            ),
            PaletteItem(
                id: "action-settings",
                title: "Settings",
                subtitle: "Brand, providers, ACP, MCP, notifications",
                icon: "gearshape",
                tint: VPTheme.textSecondary,
                kind: .action,
                keywords: "preferences configure brand provider mcp acp",
                action: {
                    close()
                    openSettings()
                }
            ),
            PaletteItem(
                id: "action-notifications",
                title: "Notifications",
                subtitle: "Approvals, failed tasks, integrations, and stopped sessions",
                icon: "bell",
                tint: VPTheme.warning,
                kind: .action,
                keywords: "notifications inbox alerts approvals",
                action: {
                    close()
                    openNotifications()
                }
            ),
            PaletteItem(
                id: "action-shortcuts",
                title: "Keyboard Shortcuts",
                subtitle: "Global, navigation, session, and project keys",
                icon: "keyboard",
                tint: .indigo,
                kind: .action,
                keywords: "keyboard shortcuts keys help",
                action: {
                    close()
                    openShortcuts()
                }
            ),
            PaletteItem(
                id: "action-inspector",
                title: "Toggle Inspector",
                subtitle: "Show or hide the Collab/Preview side panel",
                icon: "sidebar.right",
                tint: VPTheme.info,
                kind: .action,
                keywords: "inspector studio collab preview sidebar panel",
                action: {
                    close()
                    toggleInspector()
                }
            ),
            PaletteItem(
                id: "action-focus",
                title: "Toggle Focus Mode",
                subtitle: "Hide the rail and studio for a wide stage",
                icon: "arrow.up.left.and.arrow.down.right",
                tint: VPTheme.plan,
                kind: .action,
                keywords: "focus zen hide sidebar studio",
                action: {
                    close()
                    toggleFocusMode()
                }
            ),
            PaletteItem(
                id: "action-reset-layout",
                title: "Reset Window Layout",
                subtitle: "Restore the default rail, inspector, and window size",
                icon: "rectangle.3.group",
                tint: VPTheme.info,
                kind: .action,
                keywords: "layout window size reset rail inspector",
                action: {
                    close()
                    resetLayout()
                }
            ),
            PaletteItem(
                id: "action-layout-balanced",
                title: "Balanced Layout",
                subtitle: "Rail, stage, and inspector in balance",
                icon: "rectangle.split.3x1",
                tint: VPTheme.accent,
                kind: .action,
                keywords: "layout balanced workspace",
                action: {
                    close()
                    applyLayoutPreset(.balanced)
                }
            ),
            PaletteItem(
                id: "action-layout-reading",
                title: "Reading Layout",
                subtitle: "Hide the inspector and widen the transcript",
                icon: "rectangle.leadinghalf.inset.filled",
                tint: VPTheme.info,
                kind: .action,
                keywords: "layout reading focus transcript inspector hide",
                action: {
                    close()
                    applyLayoutPreset(.reading)
                }
            ),
            PaletteItem(
                id: "action-layout-team",
                title: "Team Layout",
                subtitle: "Widen the inspector for Collab and Preview",
                icon: "rectangle.trailinghalf.inset.filled",
                tint: VPTheme.plan,
                kind: .action,
                keywords: "layout team collab preview inspector wide",
                action: {
                    close()
                    applyLayoutPreset(.team)
                }
            ),
            PaletteItem(
                id: "action-language",
                title: "Switch Language",
                subtitle: "Toggle English / 简体中文",
                icon: "globe",
                tint: VPTheme.plan,
                kind: .action,
                keywords: "language locale english chinese 简体 中文",
                action: {
                    close()
                    toggleLanguage()
                }
            ),
            PaletteItem(
                id: "action-new-project",
                title: "Add Project",
                subtitle: "Choose a local repository",
                icon: "folder.badge.plus",
                tint: .teal,
                kind: .action,
                keywords: "open repository add folder",
                action: {
                    close()
                    Task { await model.addProject() }
                }
            ),
        ]

        for project in model.projects {
            items.append(
                PaletteItem(
                    id: "project-\(project.id)",
                    title: project.name,
                    subtitle: "\(project.root) · \(project.defaultBranch)",
                    icon: "folder",
                    tint: VPTheme.resident,
                    kind: .project,
                    keywords: "project repository \(project.root)",
                    action: {
                        close()
                        model.selectProject(project.id)
                    }
                )
            )
        }

        let paletteSessions = model.allSessions.isEmpty ? model.sessions : model.allSessions
        let projectNames = Dictionary(
            uniqueKeysWithValues: model.projects.map { ($0.id, $0.name) }
        )
        for session in paletteSessions {
            let projectName = projectNames[session.projectId] ?? "project"
            items.append(
                PaletteItem(
                    id: "session-\(session.id)",
                    title: session.goal,
                    subtitle: "\(projectName) · \(session.status) · \(session.id)",
                    icon: "bubble.left.and.bubble.right",
                    tint: session.status == "running" ? VPTheme.accent : VPTheme.textSecondary,
                    kind: .session,
                    keywords: "session \(projectName) \(session.status) \(session.id)",
                    action: {
                        close()
                        model.selectProject(session.projectId)
                        model.selectSession(session.id)
                    }
                )
            )
        }

        return items
    }

    private func move(_ delta: Int) {
        guard !rankedItems.isEmpty else { return }
        selectedIndex = min(
            max(0, selectedIndex + delta),
            rankedItems.count - 1
        )
    }

    private func executeSelected() {
        guard rankedItems.indices.contains(selectedIndex) else { return }
        execute(rankedItems[selectedIndex].item)
    }

    private func execute(_ item: PaletteItem) {
        recents = PaletteRecents.record(item.id, in: recents)
        item.action()
    }

    private func close() {
        withAnimation(VPMotion.resolved(VPMotion.snap, reduceMotion: reduceMotion)) {
            isPresented = false
        }
    }
}

private enum PaletteScope: String, CaseIterable, Identifiable {
    case all
    case actions
    case projects
    case sessions

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .all: L10n.t("All")
        case .actions: L10n.t("Actions")
        case .projects: L10n.t("Projects")
        case .sessions: L10n.t("Sessions")
        }
    }

    var icon: String? {
        switch self {
        case .all: nil
        case .actions: "command"
        case .projects: "folder"
        case .sessions: "bubble.left.and.bubble.right"
        }
    }

    func includes(_ kind: PaletteItem.Kind) -> Bool {
        switch self {
        case .all: true
        case .actions: kind == .action
        case .projects: kind == .project
        case .sessions: kind == .session
        }
    }

    func count(in items: [PaletteItem]) -> Int {
        items.count(where: { includes($0.kind) })
    }
}

private struct PaletteItem: Identifiable {
    enum Kind {
        case action
        case project
        case session
    }

    let id: String
    let title: String
    let subtitle: String
    let icon: String
    let tint: Color
    let kind: Kind
    let keywords: String
    let action: () -> Void

    var kindLabel: String {
        switch kind {
        case .action: L10n.t("ACTION")
        case .project: L10n.t("PROJECT")
        case .session: L10n.t("SESSION")
        }
    }
}

private struct PaletteKeyCap: View {
    let label: String

    init(_ label: String) {
        self.label = label
    }

    var body: some View {
        Text(label)
            .font(.system(size: 8, weight: .medium, design: .monospaced))
            .foregroundStyle(.secondary)
            .padding(.horizontal, 5)
            .padding(.vertical, 2)
            .background(.white.opacity(0.06), in: RoundedRectangle(cornerRadius: 4))
            .overlay {
                RoundedRectangle(cornerRadius: 4)
                    .strokeBorder(.white.opacity(0.07), lineWidth: 1)
            }
    }
}
