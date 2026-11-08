import AppKit
import SwiftUI

/// Pure presentation helpers for the navigation rail. Keeping grouping and
/// ordering here makes the sidebar behavior testable without a running daemon.
enum RailSessionList {
    static func sorted(
        _ sessions: [Session],
        projectID: String,
        sort: RailSessionSort = .recent
    ) -> [Session] {
        sort.sorted(sessions, projectID: projectID)
    }

    static func visible(
        _ sessions: [Session],
        projectID: String,
        expanded: Bool,
        sort: RailSessionSort = .recent,
        limit: Int = 6
    ) -> [Session] {
        let sorted = sorted(sessions, projectID: projectID, sort: sort)
        guard !expanded else { return sorted }
        return Array(sorted.prefix(max(0, limit)))
    }

    static func activeCount(_ sessions: [Session], projectID: String) -> Int {
        sessions.filter {
            $0.projectId == projectID
                && ["running", "queued", "paused", "verifying", "merging"].contains($0.status)
        }.count
    }
}

enum RailSessionSort: String, CaseIterable, Identifiable, Sendable {
    case recent
    case status
    case priority

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .recent: L10n.t("Recent")
        case .status: L10n.t("Status")
        case .priority: L10n.t("Priority")
        }
    }

    var icon: String {
        switch self {
        case .recent: "clock"
        case .status: "circle.dashed"
        case .priority: "flag"
        }
    }

    func sorted(_ sessions: [Session], projectID: String) -> [Session] {
        let filtered = sessions.filter { $0.projectId == projectID }
        switch self {
        case .recent:
            return filtered.sorted(by: Self.recentOrder)
        case .status:
            return filtered.sorted { lhs, rhs in
                let left = Self.statusRank(lhs.status)
                let right = Self.statusRank(rhs.status)
                if left != right {
                    return left < right
                }
                return Self.recentOrder(lhs, rhs)
            }
        case .priority:
            return filtered.sorted { lhs, rhs in
                let left = lhs.priority ?? 0
                let right = rhs.priority ?? 0
                if left != right {
                    return left > right
                }
                return Self.recentOrder(lhs, rhs)
            }
        }
    }

    private static func recentOrder(_ lhs: Session, _ rhs: Session) -> Bool {
        if lhs.createdAt == rhs.createdAt {
            return lhs.id > rhs.id
        }
        return lhs.createdAt > rhs.createdAt
    }

    private static func statusRank(_ status: String) -> Int {
        switch status.lowercased() {
        case "running", "verifying", "merging", "queued", "paused", "waiting_input":
            0
        case "idle", "draft", "waiting":
            1
        case "done", "completed", "landed", "merged":
            2
        case "conflict", "changes_requested":
            3
        case "failed", "cancelled", "interrupted", "blocked":
            4
        default:
            5
        }
    }
}

struct RailView: View {
    @Environment(AppModel.self) private var model
    @Environment(BrandStore.self) private var brand
    @Environment(MemoryMonitor.self) private var memory
    @Environment(PinnedItemsStore.self) private var pinnedStore
    @Environment(\.openSettings) private var openSettings
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    @Binding var showDashboard: Bool
    @Binding var showCommandPalette: Bool
    @Binding var showingNotifications: Bool
    @Binding var showingNewSession: Bool

    @State private var residentHovering = false
    @State private var hoveredProjectID: String?
    @State private var hoveredSessionID: String?
    @State private var hoveredAction: String?
    @State private var expandedProjects: Set<String> = []
    @State private var fullyExpandedProjects: Set<String> = []
    @AppStorage("verlinpark.rail.sort") private var sortRawValue = RailSessionSort.recent.rawValue
    @Namespace private var selectionNamespace

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            brandHeader
                .padding(.horizontal, 18)
                .padding(.top, 6)
                .padding(.bottom, 14)

            commandField
                .padding(.horizontal, 12)
                .padding(.bottom, 8)

            residentRow
                .padding(.horizontal, 10)

            VPHairline()
                .padding(.horizontal, 18)
                .padding(.vertical, 14)

            projectScroll

            VPHairline()
                .padding(.horizontal, 18)
                .padding(.top, 10)

            newSessionButton
                .padding(.horizontal, 12)
                .padding(.top, 12)

            footerActions
                .padding(.horizontal, 10)
                .padding(.top, 9)

            VPHairline()
                .padding(.horizontal, 18)
                .padding(.top, 10)

            HStack(spacing: 8) {
                MemoryPill()
                Spacer()
                Text("v0.1")
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary.opacity(0.7))
            }
            .padding(.horizontal, 18)
            .padding(.top, 9)
            .padding(.bottom, 12)
        }
        .background { VPCrystalSurface(tone: VPTheme.rail, solidTone: VPTheme.solidRail, tintOpacity: 0.70) }
        .onAppear {
            if let id = model.selectedProjectID {
                expandedProjects.insert(id)
            }
        }
        .onChange(of: model.selectedProjectID) { _, newValue in
            if let newValue {
                withAnimation(motion(VPMotion.smooth)) {
                    _ = expandedProjects.insert(newValue)
                }
            }
        }
        .onChange(of: model.projects.map(\.id)) { _, ids in
            guard expandedProjects.isEmpty, let first = ids.first else { return }
            withAnimation(motion(VPMotion.smooth)) {
                _ = expandedProjects.insert(first)
            }
        }
        .task {
            await model.loadAllSessions()
            guard model.connection == .connected, model.allSessionsComplete else { return }
            pinnedStore.prune(
                validSessionIDs: Set(model.allSessions.map(\.id)),
                validProjectIDs: Set(model.projects.map(\.id))
            )
        }
    }

    private var pinnedSessions: [Session] {
        let source = model.allSessions.isEmpty ? model.sessions : model.allSessions
        let byID = Dictionary(uniqueKeysWithValues: source.map { ($0.id, $0) })
        return pinnedStore.sessionIDs.compactMap { byID[$0] }
    }

    private var sessionSort: RailSessionSort {
        RailSessionSort(rawValue: sortRawValue) ?? .recent
    }

    private var orderedProjects: [Project] {
        model.projects.enumerated()
            .sorted { lhs, rhs in
                let leftPinned = pinnedStore.isPinnedProject(lhs.element.id)
                let rightPinned = pinnedStore.isPinnedProject(rhs.element.id)
                if leftPinned != rightPinned {
                    return leftPinned
                }
                return lhs.offset < rhs.offset
            }
            .map(\.element)
    }

    // MARK: Header

    private var brandHeader: some View {
        HStack(alignment: .center, spacing: 8) {
            Button {
                openSettings()
            } label: {
                VStack(alignment: .leading, spacing: 2) {
                    BrandMark(config: brand.config, fontSize: 23, maxWidth: 184)
                    Text("AGENT STUDIO")
                        .font(VPTheme.mono(7.5, weight: .medium))
                        .tracking(1.3)
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
            .buttonStyle(VPPressableButtonStyle(scale: 0.985))
            .help("Customize this name in Settings → Brand")

            Spacer(minLength: 0)

            Button {
                showCommandPalette = true
            } label: {
                Image(systemName: "command")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                    .frame(width: 24, height: 24)
                    .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 7))
                    .overlay {
                        RoundedRectangle(cornerRadius: 7)
                            .strokeBorder(VPTheme.hairline, lineWidth: 1)
                    }
            }
            .buttonStyle(VPPressableButtonStyle(scale: 0.92))
            .help("Command palette · ⌘K")
        }
        .vpAppear(index: 0, animation: VPMotion.emphasizedSpring)
    }

    private var commandField: some View {
        Button {
            showCommandPalette = true
        } label: {
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                Text("Search or jump")
                    .font(VPTheme.font(11))
                    .foregroundStyle(VPTheme.textSecondary)
                Spacer()
                VPKeyCap(text: "⌘K")
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 7)
            .background(VPTheme.surfaceSunken, in: RoundedRectangle(cornerRadius: 9, style: .continuous))
            .overlay {
                RoundedRectangle(cornerRadius: 9, style: .continuous)
                    .strokeBorder(VPTheme.hairline, lineWidth: 1)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(VPPressableButtonStyle(scale: 0.99))
        .help("Jump to a project, session, or action · ⌘K")
        .vpAppear(index: 1, animation: VPMotion.smooth)
    }

    // MARK: Modes

    private var residentRow: some View {
        let selected = model.mode == .resident
        return Button {
            withAnimation(motion(VPMotion.snap)) {
                model.selectResident()
            }
        } label: {
            HStack(spacing: 10) {
                ZStack {
                    RoundedRectangle(cornerRadius: 8, style: .continuous)
                        .fill(VPTheme.resident.opacity(0.14))
                        .frame(width: 28, height: 28)
                    Image(systemName: "cube.transparent")
                        .font(.system(size: 11, weight: .bold))
                        .foregroundStyle(VPTheme.resident)
                }
                VStack(alignment: .leading, spacing: 1) {
                    Text("Aviiya")
                        .font(VPTheme.font(12, weight: .semibold))
                        .foregroundStyle(VPTheme.textPrimary)
                    Text("NovaBAIC bridge")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                Spacer(minLength: 0)
                if selected {
                    Circle()
                        .fill(VPTheme.resident)
                        .frame(width: 5, height: 5)
                        .transition(.vpScaleFade)
                }
            }
            .padding(.horizontal, 9)
            .padding(.vertical, 8)
            .background {
                if selected {
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .fill(VPTheme.resident.opacity(0.09))
                        .overlay {
                            RoundedRectangle(cornerRadius: 10, style: .continuous)
                                .strokeBorder(VPTheme.resident.opacity(0.18), lineWidth: 1)
                        }
                        .matchedGeometryEffect(id: "rail-mode", in: selectionNamespace)
                } else if residentHovering {
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .fill(VPTheme.surface)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(VPPressableButtonStyle(scale: 0.988))
        .onHover { residentHovering = $0 }
        .animation(motion(VPMotion.hover), value: residentHovering)
        .animation(motion(VPMotion.snap), value: model.mode)
        .vpAppear(index: 2, animation: VPMotion.smooth)
    }

    // MARK: Projects

    private var projectScroll: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    VPKicker(text: "Projects", trailing: "\(model.projects.count)")
                    Menu {
                        ForEach(RailSessionSort.allCases) { sort in
                            Button {
                                withAnimation(motion(VPMotion.snap)) {
                                    sortRawValue = sort.rawValue
                                }
                            } label: {
                                Label(
                                    sort.title,
                                    systemImage: sessionSort == sort ? "checkmark" : sort.icon
                                )
                            }
                        }
                    } label: {
                        Image(systemName: "arrow.up.arrow.down")
                            .font(.system(size: 8, weight: .semibold))
                            .foregroundStyle(VPTheme.textTertiary)
                            .frame(width: 18, height: 18)
                    }
                    .menuStyle(.borderlessButton)
                    .menuIndicator(.hidden)
                    .fixedSize()
                    .help(L10n.t("Sort sessions"))
                    .buttonStyle(VPPressableButtonStyle(scale: 0.9))

                    Button {
                        Task { await model.addProject() }
                    } label: {
                        Image(systemName: "plus")
                            .font(.system(size: 9, weight: .semibold))
                            .foregroundStyle(VPTheme.textSecondary)
                            .frame(width: 18, height: 18)
                    }
                    .buttonStyle(VPPressableButtonStyle(scale: 0.9))
                    .help("Add a project · ⇧⌘O")
                }
                .padding(.horizontal, 10)
                .padding(.bottom, 5)

                if !pinnedSessions.isEmpty {
                    VPKicker(text: "Pinned", trailing: "\(pinnedSessions.count)")
                        .padding(.horizontal, 10)
                        .padding(.top, 2)
                        .padding(.bottom, 2)

                    ForEach(pinnedSessions) { session in
                        if let project = model.projects.first(where: { $0.id == session.projectId }) {
                            sessionRow(session, project: project)
                        }
                    }

                    VPHairline()
                        .padding(.horizontal, 8)
                        .padding(.vertical, 7)
                }

                ForEach(Array(orderedProjects.enumerated()), id: \.element.id) { index, project in
                    projectGroup(project, index: index)
                }

                if model.projects.isEmpty {
                    emptyProjects
                }
            }
            .padding(.horizontal, 8)
            .padding(.bottom, 8)
        }
        .scrollIndicators(.hidden)
    }

    private func projectGroup(_ project: Project, index: Int) -> some View {
        let selected = model.mode == .project && model.selectedProjectID == project.id
        let expanded = expandedProjects.contains(project.id) || selected
        let showAll = fullyExpandedProjects.contains(project.id)
        let sessions = RailSessionList.visible(
            model.sessions,
            projectID: project.id,
            expanded: showAll,
            sort: sessionSort
        )
        let total = RailSessionList.sorted(
            model.sessions,
            projectID: project.id,
            sort: sessionSort
        ).count
        let active = RailSessionList.activeCount(model.sessions, projectID: project.id)
        let tint = VPTheme.projectTint(seed: project.id)

        return VStack(alignment: .leading, spacing: 2) {
            Button {
                let alreadyHere = model.mode == .project && model.selectedProjectID == project.id
                if !alreadyHere {
                    model.selectProject(project.id)
                    withAnimation(motion(VPMotion.smooth)) {
                        _ = expandedProjects.insert(project.id)
                    }
                } else {
                    withAnimation(motion(VPMotion.smooth)) {
                        if expandedProjects.contains(project.id) {
                            expandedProjects.remove(project.id)
                        } else {
                            expandedProjects.insert(project.id)
                        }
                    }
                }
            } label: {
                HStack(spacing: 9) {
                    ZStack {
                        RoundedRectangle(cornerRadius: 6, style: .continuous)
                            .fill(tint.opacity(selected ? 0.20 : 0.10))
                            .frame(width: 24, height: 24)
                        Text(String(project.name.prefix(1)).uppercased())
                            .font(.system(size: 10, weight: .bold, design: .rounded))
                            .foregroundStyle(selected ? tint : VPTheme.textSecondary)
                    }

                    VStack(alignment: .leading, spacing: 1) {
                        Text(project.name)
                            .font(VPTheme.font(12, weight: selected ? .semibold : .medium))
                            .foregroundStyle(selected ? VPTheme.textPrimary : VPTheme.textSecondary)
                            .lineLimit(1)
                        HStack(spacing: 4) {
                            if active > 0 {
                                VPStatusDot(color: VPTheme.accent, pulsing: true, size: 4)
                                Text(L10n.format("%lld active", active))
                                    .foregroundStyle(VPTheme.accent)
                            } else {
                                Text(project.defaultBranch)
                                    .foregroundStyle(VPTheme.textTertiary)
                            }
                        }
                        .font(VPTheme.mono(7.5))
                        .lineLimit(1)
                    }

                    Spacer(minLength: 4)

                    if pinnedStore.isPinnedProject(project.id) {
                        Image(systemName: "pin.fill")
                            .font(.system(size: 7, weight: .semibold))
                            .foregroundStyle(tint)
                    }

                    Image(systemName: "chevron.right")
                        .font(.system(size: 7, weight: .bold))
                        .foregroundStyle(VPTheme.textTertiary)
                        .rotationEffect(.degrees(expanded ? 90 : 0))
                }
                .padding(.horizontal, 8)
                .padding(.vertical, 7)
                .background {
                    if selected {
                        RoundedRectangle(cornerRadius: 10, style: .continuous)
                            .fill(tint.opacity(0.08))
                            .overlay {
                                RoundedRectangle(cornerRadius: 10, style: .continuous)
                                    .strokeBorder(tint.opacity(0.16), lineWidth: 1)
                            }
                            .matchedGeometryEffect(id: "rail-selection", in: selectionNamespace)
                    } else if hoveredProjectID == project.id {
                        RoundedRectangle(cornerRadius: 10, style: .continuous)
                            .fill(VPTheme.surface)
                    }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(VPPressableButtonStyle(scale: 0.99))
            .onHover { hovering in
                if hovering {
                    hoveredProjectID = project.id
                } else if hoveredProjectID == project.id {
                    hoveredProjectID = nil
                }
            }
            .animation(motion(VPMotion.hover), value: hoveredProjectID)
            .animation(motion(VPMotion.snap), value: selected)
            .vpAppear(index: index + 3, animation: VPMotion.smooth)
            .contextMenu {
                Button(L10n.t("Open")) {
                    model.selectProject(project.id)
                    withAnimation(motion(VPMotion.smooth)) {
                        _ = expandedProjects.insert(project.id)
                    }
                }
                Button(L10n.t("New Session")) {
                    model.selectProject(project.id)
                    showingNewSession = true
                }
                Divider()
                Button(L10n.t("Copy Project Path")) {
                    copyToPasteboard(project.root)
                }
                Button(L10n.t("Reveal Project in Finder")) {
                    NSWorkspace.shared.activateFileViewerSelecting([
                        URL(fileURLWithPath: project.root),
                    ])
                }
                Divider()
                Button(
                    pinnedStore.isPinnedProject(project.id)
                        ? L10n.t("Unpin Project")
                        : L10n.t("Pin Project")
                ) {
                    pinnedStore.toggleProject(project.id)
                }
            }

            if expanded {
                VStack(spacing: 1) {
                    ForEach(sessions) { session in
                        sessionRow(session, project: project)
                    }

                    if total > sessions.count {
                        Button {
                            withAnimation(motion(VPMotion.smooth)) {
                                _ = fullyExpandedProjects.insert(project.id)
                            }
                        } label: {
                            HStack(spacing: 5) {
                                Text(L10n.format("Show %lld more", total - sessions.count))
                                Image(systemName: "chevron.down")
                                    .font(.system(size: 7, weight: .bold))
                            }
                            .font(VPTheme.mono(8, weight: .medium))
                            .foregroundStyle(VPTheme.textTertiary)
                            .padding(.leading, 38)
                            .padding(.vertical, 5)
                        }
                        .buttonStyle(VPPressableButtonStyle(scale: 0.99))
                    }
                }
                .padding(.leading, 8)
                .padding(.bottom, 4)
                .transition(.vpRise)
            }
        }
    }

    private func sessionRow(_ session: Session, project: Project) -> some View {
        let selected = model.selectedSessionID == session.id && model.mode == .project
        let running = ["running", "verifying", "merging"].contains(session.status)
        return Button {
            if model.selectedProjectID != project.id {
                model.selectProject(project.id)
            }
            model.selectSession(session.id)
        } label: {
            HStack(spacing: 7) {
                VPStatusDot(
                    color: VPTheme.statusColor(session.status),
                    pulsing: running,
                    size: 5
                )
                .frame(width: 9)

                Text(session.goal)
                    .font(VPTheme.font(10.5, weight: selected ? .medium : .regular))
                    .foregroundStyle(selected ? VPTheme.textPrimary : VPTheme.textSecondary)
                    .lineLimit(2)
                    .multilineTextAlignment(.leading)

                Spacer(minLength: 2)

                Text(VPRelativeTime.label(session.createdAt))
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            .padding(.leading, 8)
            .padding(.trailing, 7)
            .padding(.vertical, 5.5)
            .background {
                if selected {
                    RoundedRectangle(cornerRadius: 7, style: .continuous)
                        .fill(VPTheme.surfaceRaised)
                        .overlay {
                            RoundedRectangle(cornerRadius: 7, style: .continuous)
                                .strokeBorder(VPTheme.hairline, lineWidth: 1)
                        }
                        .overlay(alignment: .leading) {
                            Capsule()
                                .fill(VPTheme.accent)
                                .frame(width: 2, height: 14)
                                .offset(x: -1)
                        }
                } else if hoveredSessionID == session.id {
                    RoundedRectangle(cornerRadius: 7, style: .continuous)
                        .fill(VPTheme.surface)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(VPPressableButtonStyle(scale: 0.992))
        .onHover { hovering in
            if hovering {
                hoveredSessionID = session.id
            } else if hoveredSessionID == session.id {
                hoveredSessionID = nil
            }
        }
        .animation(motion(VPMotion.hover), value: hoveredSessionID)
        .animation(motion(VPMotion.snap), value: selected)
        .help("\(session.goal) · \(session.status)")
        .contextMenu {
            Button(L10n.t("Open")) {
                openSession(session)
            }
            if session.status == "running" {
                Button(L10n.t("Pause")) {
                    openSession(session)
                    Task { await model.pauseSession() }
                }
            }
            if session.status == "paused" {
                Button(L10n.t("Resume")) {
                    openSession(session)
                    Task { await model.resumeSession() }
                }
            }
            if ["running", "paused", "queued"].contains(session.status) {
                Button(L10n.t("Stop"), role: .destructive) {
                    openSession(session)
                    Task { await model.cancelSession() }
                }
            }
            Divider()
            Menu(L10n.t("Priority")) {
                Button(L10n.t("Low (-10)")) {
                    openSession(session)
                    Task { await model.setSessionPriority(-10) }
                }
                Button(L10n.t("Normal (0)")) {
                    openSession(session)
                    Task { await model.setSessionPriority(0) }
                }
                Button(L10n.t("High (10)")) {
                    openSession(session)
                    Task { await model.setSessionPriority(10) }
                }
                Button(L10n.t("Urgent (50)")) {
                    openSession(session)
                    Task { await model.setSessionPriority(50) }
                }
            }
            Divider()
            Button(L10n.t("Export Transcript…")) {
                openSession(session)
                Task { await model.exportTranscript() }
            }
            Button(L10n.t("Copy Session ID")) {
                copyToPasteboard(session.id)
            }
            Button(L10n.t("Copy Goal")) {
                copyToPasteboard(session.goal)
            }
            Button(
                pinnedStore.isPinnedSession(session.id)
                    ? L10n.t("Unpin Session")
                    : L10n.t("Pin Session")
            ) {
                pinnedStore.toggleSession(session.id)
            }
            if let branch = session.branch {
                Button(L10n.t("Copy Branch")) {
                    copyToPasteboard(branch)
                }
            }
            if let worktree = session.worktree {
                Button(L10n.t("Reveal Worktree")) {
                    NSWorkspace.shared.activateFileViewerSelecting([
                        URL(fileURLWithPath: worktree),
                    ])
                }
            }
        }
    }

    private func openSession(_ session: Session) {
        if model.selectedProjectID != session.projectId {
            model.selectProject(session.projectId)
        }
        model.selectSession(session.id)
    }

    private func copyToPasteboard(_ value: String) {
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(value, forType: .string)
    }

    private var emptyProjects: some View {
        VStack(alignment: .leading, spacing: 7) {
            Text("No projects yet.")
                .font(VPTheme.font(11, weight: .medium))
                .foregroundStyle(VPTheme.textSecondary)
            Text("Open a local repository to start a Leader session.")
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
            Button {
                Task { await model.addProject() }
            } label: {
                Label("Add project", systemImage: "folder.badge.plus")
                    .font(VPTheme.font(10, weight: .medium))
            }
            .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
        }
        .padding(10)
    }

    // MARK: Actions

    private var newSessionButton: some View {
        Button {
            showingNewSession = true
        } label: {
            HStack(spacing: 7) {
                Image(systemName: "plus")
                    .font(.system(size: 10, weight: .bold))
                Text("New Session")
                    .font(VPTheme.font(11.5, weight: .semibold))
                Spacer()
                Text("⌘N")
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(.white.opacity(0.62))
            }
            .foregroundStyle(.white)
            .padding(.horizontal, 11)
            .padding(.vertical, 8)
            .background(
                VPTheme.primaryGradient,
                in: RoundedRectangle(cornerRadius: 9, style: .continuous)
            )
            .overlay(alignment: .top) {
                RoundedRectangle(cornerRadius: 9, style: .continuous)
                    .strokeBorder(.white.opacity(0.20), lineWidth: 1)
            }
            .shadow(color: VPTheme.accent.opacity(0.18), radius: 10, y: 4)
        }
        .buttonStyle(VPPressableButtonStyle(scale: 0.985))
        .help("New session · ⌘N")
        .vpAppear(index: 9, animation: VPMotion.smooth)
    }

    private var footerActions: some View {
        HStack(spacing: 2) {
            footerAction(
                icon: "square.grid.2x2",
                title: "Dash",
                badge: nil
            ) {
                showDashboard = true
            }
            footerAction(
                icon: model.unreadNoticeCount > 0 ? "bell.badge" : "bell",
                title: "Inbox",
                badge: model.unreadNoticeCount
            ) {
                showingNotifications = true
            }
            footerAction(icon: "gearshape", title: "Settings", badge: nil) {
                openSettings()
            }
        }
    }

    private func footerAction(
        icon: String,
        title: String,
        badge: Int?,
        action: @escaping () -> Void
    ) -> some View {
        let hovering = hoveredAction == title
        return Button(action: action) {
            VStack(spacing: 4) {
                ZStack(alignment: .topTrailing) {
                    Image(systemName: icon)
                        .font(.system(size: 11, weight: .medium))
                        .symbolEffect(.bounce, value: badge ?? 0)
                    if let badge, badge > 0 {
                        Circle()
                            .fill(VPTheme.accent)
                            .frame(width: 4, height: 4)
                            .offset(x: 5, y: -2)
                    }
                }
                Text(L10n.t(title))
                    .font(VPTheme.mono(7.5, weight: .medium))
            }
            .foregroundStyle(hovering ? VPTheme.textPrimary : VPTheme.textSecondary)
            .frame(maxWidth: .infinity)
            .padding(.vertical, 7)
            .background(
                hovering ? VPTheme.surface : Color.clear,
                in: RoundedRectangle(cornerRadius: 8, style: .continuous)
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(VPPressableButtonStyle(scale: 0.97))
        .onHover { isHovering in
            if isHovering {
                hoveredAction = title
            } else if hoveredAction == title {
                hoveredAction = nil
            }
        }
        .animation(motion(VPMotion.hover), value: hovering)
        .help(title)
    }

    private func motion(_ animation: Animation) -> Animation? {
        VPMotion.resolved(animation, reduceMotion: reduceMotion)
    }
}
