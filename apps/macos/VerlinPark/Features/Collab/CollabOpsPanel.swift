import SwiftUI

/// Collab has two surfaces over the same deterministic team state: the graph
/// answers "who is working on what", and Ops answers "what needs a decision".
/// Ops is a monitor/controller surface; plan mutation still goes through the
/// revision RPC so it cannot drift from the stored plan.
enum CollabSurface: String, CaseIterable, Identifiable {
    case graph
    case ops

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .graph: L10n.t("Graph")
        case .ops: L10n.t("Ops")
        }
    }

    var icon: String {
        switch self {
        case .graph: "point.3.connected.trianglepath.dotted"
        case .ops: "slider.horizontal.3"
        }
    }
}

struct CollabOpsPanel: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    @State private var showCompose = false
    @State private var composeAction = "note"
    @State private var composeTaskID: String?
    @State private var composeAgentID: String?
    @State private var showRevision = false
    @State private var showPolicy = false
    @State private var selectedRevision: RevisionSelection?
    @State private var editingTask: OrchestrationTask?
    @State private var showPreview = false
    @State private var filter = CollabTaskFilter.all
    @State private var search = ""
    @State private var expandedDepartments = Set<String>()
    @State private var pendingCommand: CollabTaskCommand?
    @State private var pendingTask: OrchestrationTask?

    private var snapshot: OrchestrationSnapshot? {
        model.orchestrationSnapshot
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 10) {
                CollabOpsToolbar(
                    snapshot: snapshot,
                    busy: model.collabTaskActionBusyID != nil,
                    onRefresh: { Task { await model.loadOrchestrationSnapshot() } },
                    onPreview: {
                        showPreview = true
                        Task { await model.previewOrchestration() }
                    },
                    onAdvance: { Task { await model.advanceOrchestration() } },
                    onDispatch: { Task { await model.dispatchNextOrchestrationTask() } },
                    onCoordinate: {
                        composeAction = "note"
                        composeTaskID = nil
                        composeAgentID = nil
                        showCompose = true
                    },
                    onRevise: { showRevision = true },
                    onPolicy: { showPolicy = true }
                )

                if let snapshot {
                    CollabOpsMetricsStrip(metrics: snapshot.metrics)
                    if let status = model.collabTaskActionStatus, !status.isEmpty {
                        CollabOpsNotice(text: status, tint: VPTheme.info)
                    }
                    CollabOpsReadyQueue(
                        tasks: snapshot.tasks.filter(\.ready),
                        busyID: model.collabTaskActionBusyID,
                        onDispatch: { Task { await model.dispatchNextOrchestrationTask() } },
                        onAction: perform
                    )
                    CollabOpsBlockers(
                        blockers: snapshot.blockers,
                        notes: snapshot.notes,
                        tasks: snapshot.tasks,
                        busyID: model.collabTaskActionBusyID,
                        onResolve: { blocker in
                            Task { await model.resolveOrchestrationBlocker(blocker) }
                        },
                        onAction: perform,
                        onCompose: { blocker in
                            composeAction = blocker.kind == "coordination" ? "unblock" : "escalate"
                            composeTaskID = blocker.taskId
                            composeAgentID = blocker.agentId
                            showCompose = true
                        }
                    )
                    CollabOpsCoordinationInbox(
                        notes: snapshot.notes,
                        busy: model.collabTaskActionBusyID != nil,
                        onResolve: { note in
                            Task {
                                _ = await model.resolveOrchestrationBlocker(
                                    OrchestrationBlocker(
                                        kind: "coordination",
                                        noteId: note.noteId,
                                        taskId: note.taskId,
                                        agentId: note.agentId,
                                        agentName: note.agentName,
                                        message: note.message,
                                        createdAt: note.createdAt
                                    )
                                )
                            }
                        },
                        onReply: { note in
                            composeAction = "note"
                            composeTaskID = note.taskId
                            composeAgentID = note.agentId
                            showCompose = true
                        },
                        onBlock: { note in
                            composeAction = "blocker"
                            composeTaskID = note.taskId
                            composeAgentID = note.agentId
                            showCompose = true
                        }
                    )
                    CollabOpsTaskBoard(
                        tasks: snapshot.tasks,
                        filter: $filter,
                        search: $search,
                        expandedDepartments: $expandedDepartments,
                        busyID: model.collabTaskActionBusyID,
                        onAction: perform,
                        onEdit: { task in editingTask = task }
                    )
                    CollabOpsRevisionTimeline(
                        revisions: snapshot.revisions,
                        currentRevision: snapshot.planRevision,
                        onRevise: { showRevision = true },
                        onSelect: { revision in
                            selectedRevision = RevisionSelection(revision: revision)
                            Task { await model.loadPlanRevisionDetail(revision) }
                        }
                    )
                    CollabOpsDecisionFeed(
                        decisions: snapshot.recentDecisions,
                        notes: snapshot.recentNotes
                    )
                    CollabOpsPolicySummary(policy: snapshot.policy) {
                        showPolicy = true
                    }
                } else if let session = model.selectedSession {
                    CollabOpsEmptyState(session: session)
                } else {
                    CollabOpsEmptyState(session: nil)
                }
            }
            .padding(10)
        }
        .background(VPTheme.canvas)
        .sheet(isPresented: $showCompose) {
            CoordinationComposeSheet(
                initialAction: composeAction,
                initialTaskID: composeTaskID,
                initialAgentID: composeAgentID,
                tasks: snapshot?.tasks ?? [],
                agents: snapshot?.agents ?? []
            )
            .environment(model)
        }
        .sheet(isPresented: $showRevision) {
            PlanRevisionSheet()
                .environment(model)
        }
        .sheet(isPresented: $showPolicy) {
            OrchestrationPolicySheet()
                .environment(model)
        }
        .sheet(item: $selectedRevision) { selection in
            PlanRevisionDetailSheet(revision: selection.revision)
                .environment(model)
        }
        .sheet(item: $editingTask) { task in
            PlanTaskEditorSheet(task: task)
                .environment(model)
        }
        .sheet(isPresented: $showPreview) {
            OrchestrationPreviewSheet()
                .environment(model)
        }
        .confirmationDialog(
            confirmationTitle,
            isPresented: Binding(
                get: { pendingCommand != nil },
                set: {
                    if !$0 {
                        pendingCommand = nil; pendingTask = nil
                    }
                }
            ),
            presenting: pendingTask
        ) { task in
            if let command = pendingCommand {
                Button(command.title, role: command == .skip || command == .cancel ? .destructive : nil) {
                    perform(task, command)
                    pendingCommand = nil
                    pendingTask = nil
                }
            }
            Button(L10n.t("Keep task"), role: .cancel) {
                pendingCommand = nil
                pendingTask = nil
            }
        } message: { task in
            Text(
                L10n.t(
                    commandMessage(command: pendingCommand, task: task)
                )
            )
        }
    }

    private var confirmationTitle: String {
        guard let command = pendingCommand else { return "" }
        return L10n.t("\(command.title) task?")
    }

    private func commandMessage(command: CollabTaskCommand?, task: OrchestrationTask) -> String {
        switch command {
        case .skip:
            "Skip \(task.taskId)? Dependents will wait for a plan revision or retry."
        case .cancel:
            "Cancel \(task.taskId)? Its current work will be discarded at the next safe boundary."
        case .retry:
            "Retry \(task.taskId) from its last failed attempt."
        case .pause:
            "Pause \(task.taskId) at the next turn boundary."
        case .resume:
            "Resume \(task.taskId)."
        case nil:
            ""
        }
    }

    private func perform(_ task: OrchestrationTask, _ command: CollabTaskCommand) {
        switch command {
        case .skip, .cancel:
            pendingCommand = command
            pendingTask = task
        default:
            Task { await model.collabTaskAction(task, action: command) }
        }
    }
}

// MARK: - Toolbar

private struct CollabOpsToolbar: View {
    let snapshot: OrchestrationSnapshot?
    let busy: Bool
    let onRefresh: () -> Void
    let onPreview: () -> Void
    let onAdvance: () -> Void
    let onDispatch: () -> Void
    let onCoordinate: () -> Void
    let onRevise: () -> Void
    let onPolicy: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack(spacing: 6) {
                VPKicker(
                    text: "Team operations",
                    trailing: snapshot.map { "plan r\($0.planRevision)" }
                )
                Spacer()
                if let snapshot {
                    CollabHealthPill(metrics: snapshot.metrics)
                }
                if busy {
                    ProgressView()
                        .controlSize(.mini)
                        .scaleEffect(0.7)
                }
            }

            HStack(spacing: 5) {
                CollabOpsActionButton(
                    icon: "arrow.clockwise",
                    title: "Refresh",
                    tint: VPTheme.textSecondary,
                    action: onRefresh
                )
                CollabOpsActionButton(
                    icon: "eye",
                    title: "Preview",
                    tint: VPTheme.info,
                    action: onPreview
                )
                CollabOpsActionButton(
                    icon: "gearshape.2",
                    title: "Advance",
                    tint: VPTheme.accent,
                    action: onAdvance
                )
                CollabOpsActionButton(
                    icon: "play.circle",
                    title: "Run next",
                    tint: VPTheme.success,
                    action: onDispatch
                )
                CollabOpsActionButton(
                    icon: "bubble.left.and.bubble.right",
                    title: "Coordinate",
                    tint: VPTheme.info,
                    action: onCoordinate
                )
            }

            HStack(spacing: 5) {
                CollabOpsActionButton(
                    icon: "square.and.pencil",
                    title: "Revise plan",
                    tint: VPTheme.plan,
                    action: onRevise
                )
                CollabOpsActionButton(
                    icon: "dial.medium",
                    title: "Policy",
                    tint: VPTheme.warning,
                    action: onPolicy
                )
            }
        }
    }
}

private struct CollabOpsActionButton: View {
    let icon: String
    let title: String
    let tint: Color
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 4) {
                Image(systemName: icon)
                    .font(.system(size: 8, weight: .semibold))
                Text(L10n.t(title))
                    .font(VPTheme.mono(7.2, weight: .medium))
            }
            .foregroundStyle(tint)
            .padding(.horizontal, 7)
            .padding(.vertical, 5)
            .background(tint.opacity(0.09), in: Capsule())
            .overlay {
                Capsule().strokeBorder(tint.opacity(0.22), lineWidth: 1)
            }
        }
        .buttonStyle(.plain)
    }
}

private struct CollabHealthPill: View {
    let metrics: OrchestrationMetrics

    private var tint: Color {
        if metrics.failedTasks > 0 || metrics.blockedTasks > 0 || metrics.mergeConflicts > 0 {
            VPTheme.danger
        } else if metrics.runningAgents > 0 || metrics.activeTasks > 0 {
            VPTheme.success
        } else {
            VPTheme.textTertiary
        }
    }

    private var label: String {
        if metrics.failedTasks > 0 || metrics.blockedTasks > 0 || metrics.mergeConflicts > 0 {
            "ATTENTION"
        } else if metrics.runningAgents > 0 || metrics.activeTasks > 0 {
            "LIVE"
        } else {
            "IDLE"
        }
    }

    var body: some View {
        HStack(spacing: 4) {
            VPStatusDot(color: tint, pulsing: label == "LIVE", size: 4)
            Text(label)
                .font(VPTheme.mono(6.8, weight: .semibold))
                .tracking(0.55)
                .foregroundStyle(tint)
        }
        .padding(.horizontal, 6)
        .padding(.vertical, 3)
        .background(tint.opacity(0.09), in: Capsule())
        .overlay { Capsule().strokeBorder(tint.opacity(0.20), lineWidth: 1) }
    }
}

// MARK: - Metrics

private struct CollabOpsMetricsStrip: View {
    let metrics: OrchestrationMetrics

    private struct Metric: Identifiable {
        let id: String
        let icon: String
        let value: Int
        let label: String
        let tint: Color
    }

    private var items: [Metric] {
        [
            Metric(
                id: "agents",
                icon: "person.2",
                value: metrics.runningAgents,
                label: "Active",
                tint: VPTheme.success
            ),
            Metric(
                id: "ready",
                icon: "play.circle",
                value: metrics.readyTasks,
                label: "Ready",
                tint: VPTheme.accent
            ),
            Metric(
                id: "active",
                icon: "bolt",
                value: metrics.activeTasks,
                label: "Running",
                tint: VPTheme.info
            ),
            Metric(
                id: "blocked",
                icon: "hand.raised",
                value: metrics.blockedTasks,
                label: "Blocked",
                tint: VPTheme.warning
            ),
            Metric(
                id: "failed",
                icon: "xmark.octagon",
                value: metrics.failedTasks,
                label: "Failed",
                tint: VPTheme.danger
            ),
            Metric(
                id: "retry",
                icon: "arrow.clockwise",
                value: metrics.retryingTasks,
                label: "Retrying",
                tint: VPTheme.plan
            ),
        ]
    }

    var body: some View {
        LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 5), count: 3), spacing: 5) {
            ForEach(items) { item in
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 3) {
                        Image(systemName: item.icon)
                            .font(.system(size: 6.5, weight: .semibold))
                            .foregroundStyle(item.tint)
                        Text("\(item.value)")
                            .font(VPTheme.mono(10, weight: .semibold))
                            .foregroundStyle(item.value > 0 ? VPTheme.textPrimary : VPTheme.textTertiary)
                            .contentTransition(.numericText())
                    }
                    Text(L10n.t(item.label).uppercased())
                        .font(VPTheme.mono(5.8, weight: .medium))
                        .tracking(0.4)
                        .foregroundStyle(VPTheme.textTertiary)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 7)
                .padding(.vertical, 6)
                .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
                .overlay {
                    RoundedRectangle(cornerRadius: 8, style: .continuous)
                        .strokeBorder(
                            item.value > 0 ? item.tint.opacity(0.18) : VPTheme.hairline,
                            lineWidth: 1
                        )
                }
            }
        }
    }
}

private struct CollabOpsNotice: View {
    let text: String
    let tint: Color

    var body: some View {
        HStack(alignment: .top, spacing: 5) {
            Image(systemName: "info.circle")
                .font(.system(size: 7.5, weight: .semibold))
                .foregroundStyle(tint)
            Text(text)
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(tint.opacity(0.08), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .strokeBorder(tint.opacity(0.18), lineWidth: 1)
        }
    }
}

// MARK: - Ready queue

private struct CollabOpsReadyQueue: View {
    let tasks: [OrchestrationTask]
    let busyID: String?
    let onDispatch: () -> Void
    let onAction: (OrchestrationTask, CollabTaskCommand) -> Void

    var body: some View {
        CollabOpsSection(
            title: "Ready queue",
            count: tasks.count,
            tint: VPTheme.accent,
            trailing: tasks.isEmpty ? nil : AnyView(
                Button(action: onDispatch) {
                    HStack(spacing: 3) {
                        Image(systemName: "play.fill")
                            .font(.system(size: 7, weight: .semibold))
                        Text(L10n.t("Run next"))
                            .font(VPTheme.mono(6.8, weight: .medium))
                    }
                    .foregroundStyle(VPTheme.success)
                }
                .buttonStyle(.plain)
            )
        ) {
            if tasks.isEmpty {
                Text(L10n.t("No task is ready. Checks, retries, or a plan revision may be needed."))
                    .font(VPTheme.mono(7.2))
                    .foregroundStyle(VPTheme.textTertiary)
                    .padding(.vertical, 3)
            } else {
                ForEach(tasks.prefix(8)) { task in
                    HStack(spacing: 6) {
                        Image(systemName: "circle.dashed")
                            .font(.system(size: 7, weight: .semibold))
                            .foregroundStyle(VPTheme.accent)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(task.title)
                                .font(VPTheme.font(9.5, weight: .medium))
                                .foregroundStyle(VPTheme.textPrimary)
                                .lineLimit(1)
                            Text("\(task.taskId) · \(task.department) · p\(task.priority)")
                                .font(VPTheme.mono(6.5))
                                .foregroundStyle(VPTheme.textTertiary)
                                .lineLimit(1)
                        }
                        Spacer(minLength: 4)
                        CollabTaskStatusChip(status: task.status)
                    }
                    .padding(.horizontal, 7)
                    .padding(.vertical, 5)
                    .background(
                        VPTheme.surface.opacity(0.6),
                        in: RoundedRectangle(cornerRadius: 7, style: .continuous)
                    )
                }
            }
        }
    }
}

// MARK: - Blockers

private struct CollabOpsBlockers: View {
    let blockers: [OrchestrationBlocker]
    let notes: [OrchestrationNote]
    let tasks: [OrchestrationTask]
    let busyID: String?
    let onResolve: (OrchestrationBlocker) -> Void
    let onAction: (OrchestrationTask, CollabTaskCommand) -> Void
    let onCompose: (OrchestrationBlocker) -> Void

    var body: some View {
        CollabOpsSection(
            title: "Attention feed",
            count: blockers.count,
            tint: blockers.isEmpty ? VPTheme.textTertiary : VPTheme.danger
        ) {
            if blockers.isEmpty {
                Text(L10n.t("No blockers or failed agents. The controller is idle."))
                    .font(VPTheme.mono(7.2))
                    .foregroundStyle(VPTheme.textTertiary)
                    .padding(.vertical, 3)
            } else {
                ForEach(blockers.prefix(12)) { blocker in
                    VStack(alignment: .leading, spacing: 4) {
                        HStack(spacing: 4) {
                            Image(systemName: blockerIcon(blocker.kind))
                                .font(.system(size: 7, weight: .semibold))
                                .foregroundStyle(blockerTint(blocker.kind))
                            Text(blockerLabel(blocker.kind).uppercased())
                                .font(VPTheme.mono(5.9, weight: .semibold))
                                .tracking(0.45)
                                .foregroundStyle(blockerTint(blocker.kind))
                            if let taskID = blocker.taskId {
                                Text(taskID)
                                    .font(VPTheme.mono(6.2, weight: .medium))
                                    .foregroundStyle(VPTheme.textTertiary)
                            }
                            Spacer(minLength: 2)
                            Text(VPTheme.relativeTime(blocker.createdAt))
                                .font(VPTheme.mono(5.8))
                                .foregroundStyle(VPTheme.textTertiary)
                        }
                        Text(blocker.message)
                            .font(VPTheme.mono(7.2))
                            .foregroundStyle(VPTheme.textSecondary)
                            .fixedSize(horizontal: false, vertical: true)
                            .lineLimit(3)
                        HStack(spacing: 5) {
                            if blocker.kind == "coordination",
                               blocker.taskId != nil || blocker.noteId != nil
                            {
                                Button(L10n.t("Resolve")) {
                                    onResolve(blocker)
                                }
                                .font(VPTheme.mono(6.8, weight: .medium))
                                .buttonStyle(.borderless)
                                .foregroundStyle(VPTheme.success)
                            }
                            if blocker.kind == "task_failed" || blocker.kind == "task_blocked",
                               let taskID = blocker.taskId,
                               let task = tasks.first(where: { $0.taskId == taskID })
                            {
                                Button(L10n.t("Retry")) {
                                    onAction(task, .retry)
                                }
                                .font(VPTheme.mono(6.8, weight: .medium))
                                .buttonStyle(.borderless)
                                .foregroundStyle(VPTheme.accent)
                                .disabled(busyID != nil)
                            }
                            Button(L10n.t("Add note")) {
                                onCompose(blocker)
                            }
                            .font(VPTheme.mono(6.8, weight: .medium))
                            .buttonStyle(.borderless)
                            .foregroundStyle(VPTheme.info)
                        }
                    }
                    .padding(.horizontal, 7)
                    .padding(.vertical, 6)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(
                        VPTheme.surface.opacity(0.55),
                        in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                    )
                    .overlay {
                        RoundedRectangle(cornerRadius: 8, style: .continuous)
                            .strokeBorder(VPTheme.hairline, lineWidth: 1)
                    }
                }
            }
        }
    }

    private func blockerIcon(_ kind: String) -> String {
        switch kind {
        case "task_failed": "xmark.octagon"
        case "task_blocked": "hand.raised"
        case "merge_queue": "arrow.triangle.merge"
        case "user_question": "questionmark.bubble"
        case "coordination": "bubble.left.and.exclamationmark.bubble.right"
        case "agent_failed": "person.crop.circle.badge.exclamationmark"
        default: "exclamationmark.triangle"
        }
    }

    private func blockerTint(_ kind: String) -> Color {
        switch kind {
        case "task_failed", "agent_failed": VPTheme.danger
        case "merge_queue", "task_blocked": VPTheme.warning
        case "user_question": VPTheme.info
        case "coordination": VPTheme.plan
        default: VPTheme.textSecondary
        }
    }

    private func blockerLabel(_ kind: String) -> String {
        kind.replacingOccurrences(of: "_", with: " ")
    }
}

// MARK: - Task board

enum CollabTaskFilter: String, CaseIterable, Identifiable {
    case all
    case attention
    case active
    case done

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .all: L10n.t("All")
        case .attention: L10n.t("Attention")
        case .active: L10n.t("Active")
        case .done: L10n.t("Done")
        }
    }

    func matches(_ task: OrchestrationTask) -> Bool {
        switch self {
        case .all:
            true
        case .attention:
            ["failed", "blocked", "interrupted", "cancelled"].contains(task.status)
        case .active:
            ["running", "queued", "paused", "pausing", "cancelling"].contains(task.status)
        case .done:
            ["completed", "skipped"].contains(task.status)
        }
    }
}

private struct CollabOpsTaskBoard: View {
    let tasks: [OrchestrationTask]
    @Binding var filter: CollabTaskFilter
    @Binding var search: String
    @Binding var expandedDepartments: Set<String>
    let busyID: String?
    let onAction: (OrchestrationTask, CollabTaskCommand) -> Void
    let onEdit: (OrchestrationTask) -> Void

    private var filtered: [OrchestrationTask] {
        let query = search.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return tasks.filter { task in
            guard filter.matches(task) else { return false }
            guard !query.isEmpty else { return true }
            return task.title.lowercased().contains(query)
                || task.taskId.lowercased().contains(query)
                || task.department.lowercased().contains(query)
                || (task.ownerAgentName?.lowercased().contains(query) ?? false)
        }
    }

    private var grouped: [(String, [OrchestrationTask])] {
        Dictionary(grouping: filtered, by: \.department)
            .map { ($0.key, $0.value.sorted { lhs, rhs in
                if lhs.priority != rhs.priority {
                    return lhs.priority > rhs.priority
                }
                return lhs.taskId < rhs.taskId
            }) }
            .sorted { $0.0.localizedCaseInsensitiveCompare($1.0) == .orderedAscending }
    }

    var body: some View {
        CollabOpsSection(
            title: "Task board",
            count: filtered.count,
            tint: VPTheme.plan
        ) {
            VStack(alignment: .leading, spacing: 5) {
                HStack(spacing: 5) {
                    Image(systemName: "magnifyingglass")
                        .font(.system(size: 7, weight: .semibold))
                        .foregroundStyle(VPTheme.textTertiary)
                    TextField(L10n.t("Filter tasks"), text: $search)
                        .textFieldStyle(.plain)
                        .font(VPTheme.mono(7.2))
                    if !search.isEmpty {
                        Button {
                            search = ""
                        } label: {
                            Image(systemName: "xmark.circle.fill")
                                .font(.system(size: 7))
                                .foregroundStyle(VPTheme.textTertiary)
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.horizontal, 7)
                .padding(.vertical, 4)
                .background(VPTheme.surface, in: Capsule())
                .overlay { Capsule().strokeBorder(VPTheme.hairline, lineWidth: 1) }

                HStack(spacing: 3) {
                    ForEach(CollabTaskFilter.allCases) { item in
                        Button {
                            withAnimation(VPMotion.snap) { filter = item }
                        } label: {
                            Text(item.title)
                                .font(VPTheme.mono(6.4, weight: .medium))
                                .foregroundStyle(filter == item ? VPTheme.accent : VPTheme.textTertiary)
                                .padding(.horizontal, 6)
                                .padding(.vertical, 3)
                                .background(
                                    filter == item ? VPTheme.accent.opacity(0.10) : Color.clear,
                                    in: Capsule()
                                )
                        }
                        .buttonStyle(.plain)
                    }
                    Spacer()
                }

                if grouped.isEmpty {
                    Text(L10n.t("No tasks match this filter."))
                        .font(VPTheme.mono(7.2))
                        .foregroundStyle(VPTheme.textTertiary)
                        .padding(.vertical, 3)
                } else {
                    ForEach(grouped, id: \.0) { department, departmentTasks in
                        departmentSection(department, tasks: departmentTasks)
                    }
                }
            }
        }
    }

    private func departmentSection(
        _ department: String,
        tasks: [OrchestrationTask]
    ) -> some View {
        let expanded = expandedDepartments.contains(department)
        return VStack(alignment: .leading, spacing: 3) {
            Button {
                withAnimation(VPMotion.snap) {
                    if expanded {
                        expandedDepartments.remove(department)
                    } else {
                        expandedDepartments.insert(department)
                    }
                }
            } label: {
                HStack(spacing: 4) {
                    Image(systemName: expanded ? "chevron.down" : "chevron.right")
                        .font(.system(size: 6, weight: .bold))
                        .foregroundStyle(VPTheme.textTertiary)
                    Text(department.uppercased())
                        .font(VPTheme.mono(6.4, weight: .semibold))
                        .tracking(0.45)
                        .foregroundStyle(VPTheme.plan)
                    Text("\(tasks.count)")
                        .font(VPTheme.mono(6))
                        .foregroundStyle(VPTheme.textTertiary)
                    Spacer()
                }
                .padding(.vertical, 2)
            }
            .buttonStyle(.plain)

            if expanded {
                ForEach(tasks) { task in
                    taskRow(task)
                }
            }
        }
    }

    private func taskRow(_ task: OrchestrationTask) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 4) {
                Circle()
                    .fill(VPTheme.statusColor(task.status))
                    .frame(width: 4, height: 4)
                Text(task.title)
                    .font(VPTheme.font(8.8, weight: .medium))
                    .foregroundStyle(VPTheme.textPrimary)
                    .lineLimit(1)
                Spacer(minLength: 3)
                Text(task.taskId)
                    .font(VPTheme.mono(5.8))
                    .foregroundStyle(VPTheme.textTertiary)
                taskMenu(task)
            }
            HStack(spacing: 5) {
                CollabTaskStatusChip(status: task.status)
                Text("p\(task.priority)")
                    .font(VPTheme.mono(5.8))
                    .foregroundStyle(VPTheme.textTertiary)
                if task.attempts > 0 {
                    Text("\(task.attempts)/\(task.maxAttempts)")
                        .font(VPTheme.mono(5.8))
                        .foregroundStyle(task.attempts >= task.maxAttempts ? VPTheme.danger : VPTheme
                            .textTertiary)
                }
                if let owner = task.ownerAgentName {
                    Text(owner)
                        .font(VPTheme.mono(5.8))
                        .foregroundStyle(VPTheme.info)
                        .lineLimit(1)
                }
                if let verifier = task.verifierStatus {
                    Text("v:\(verifier)")
                        .font(VPTheme.mono(5.8))
                        .foregroundStyle(VPTheme.statusColor(verifier))
                }
                if let next = task.nextAttemptAt {
                    Text(VPTheme.relativeTime(next))
                        .font(VPTheme.mono(5.8))
                        .foregroundStyle(VPTheme.warning)
                }
                Spacer()
            }
            if !task.blockedBy.isEmpty {
                Text("Blocked by \(task.blockedBy.joined(separator: ", "))")
                    .font(VPTheme.mono(6))
                    .foregroundStyle(VPTheme.warning)
                    .lineLimit(1)
            } else if let error = task.lastError, !error.isEmpty {
                Text(error)
                    .font(VPTheme.mono(6))
                    .foregroundStyle(VPTheme.danger)
                    .lineLimit(1)
            }
        }
        .padding(.horizontal, 7)
        .padding(.vertical, 4)
        .background(VPTheme.surface.opacity(0.45), in: RoundedRectangle(cornerRadius: 6, style: .continuous))
    }

    private func taskMenu(_ task: OrchestrationTask) -> some View {
        Menu {
            Button {
                onEdit(task)
            } label: {
                Label(L10n.t("Edit task…"), systemImage: "square.and.pencil")
            }
            Divider()
            if ["failed", "blocked", "interrupted", "cancelled"].contains(task.status) {
                Button {
                    onAction(task, .retry)
                } label: {
                    Label(L10n.t("Retry"), systemImage: "arrow.clockwise")
                }
            }
            if ["running", "queued"].contains(task.status) {
                Button {
                    onAction(task, .pause)
                } label: {
                    Label(L10n.t("Pause"), systemImage: "pause.fill")
                }
            }
            if ["paused", "pausing"].contains(task.status) {
                Button {
                    onAction(task, .resume)
                } label: {
                    Label(L10n.t("Resume"), systemImage: "play.fill")
                }
            }
            if ["pending", "failed", "blocked", "interrupted", "cancelled"].contains(task.status) {
                Button {
                    onAction(task, .skip)
                } label: {
                    Label(L10n.t("Skip"), systemImage: "forward.end.fill")
                }
            }
            if ["running", "queued", "paused", "pausing"].contains(task.status) {
                Divider()
                Button(role: .destructive) {
                    onAction(task, .cancel)
                } label: {
                    Label(L10n.t("Cancel"), systemImage: "xmark.octagon")
                }
            }
        } label: {
            Image(systemName: "ellipsis.circle")
                .font(.system(size: 8.5, weight: .medium))
                .foregroundStyle(VPTheme.textTertiary)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .disabled(busyID != nil)
    }
}

private struct CollabTaskStatusChip: View {
    let status: String

    var body: some View {
        HStack(spacing: 2.5) {
            Circle()
                .fill(VPTheme.statusColor(status))
                .frame(width: 3.5, height: 3.5)
            Text(status.uppercased())
                .font(VPTheme.mono(5.6, weight: .semibold))
                .tracking(0.3)
                .foregroundStyle(VPTheme.statusColor(status))
        }
        .padding(.horizontal, 4)
        .padding(.vertical, 2)
        .background(VPTheme.statusColor(status).opacity(0.09), in: Capsule())
    }
}

// MARK: - Revisions

private struct CollabOpsRevisionTimeline: View {
    let revisions: [PlanRevisionInfo]
    let currentRevision: Int
    let onRevise: () -> Void
    let onSelect: (Int) -> Void

    var body: some View {
        CollabOpsSection(
            title: "Plan revisions",
            count: revisions.count,
            tint: VPTheme.plan,
            trailing: AnyView(
                Button(action: onRevise) {
                    HStack(spacing: 3) {
                        Image(systemName: "square.and.pencil")
                            .font(.system(size: 7, weight: .semibold))
                        Text(L10n.t("Revise"))
                            .font(VPTheme.mono(6.8, weight: .medium))
                    }
                    .foregroundStyle(VPTheme.plan)
                }
                .buttonStyle(.plain)
            )
        ) {
            if revisions.isEmpty {
                Text(L10n.t("No plan revision has been recorded."))
                    .font(VPTheme.mono(7.2))
                    .foregroundStyle(VPTheme.textTertiary)
            } else {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(revisions.suffix(6)) { revision in
                        Button {
                            onSelect(revision.revision)
                        } label: {
                            HStack(alignment: .top, spacing: 6) {
                                VStack(spacing: 0) {
                                    Circle()
                                        .fill(
                                            revision.revision == currentRevision
                                                ? VPTheme.plan
                                                : VPTheme.textTertiary.opacity(0.6)
                                        )
                                        .frame(width: 5, height: 5)
                                    Rectangle()
                                        .fill(VPTheme.hairline)
                                        .frame(width: 1, height: 22)
                                }
                                VStack(alignment: .leading, spacing: 1) {
                                    HStack(spacing: 4) {
                                        Text("r\(revision.revision)")
                                            .font(VPTheme.mono(6.4, weight: .semibold))
                                            .foregroundStyle(VPTheme.plan)
                                        Text(revision.kindLabel.uppercased())
                                            .font(VPTheme.mono(5.6, weight: .semibold))
                                            .tracking(0.35)
                                            .foregroundStyle(VPTheme.textTertiary)
                                        Text(revision.author)
                                            .font(VPTheme.mono(5.8))
                                            .foregroundStyle(VPTheme.textTertiary)
                                        Spacer()
                                        Text(VPTheme.relativeTime(revision.createdAt))
                                            .font(VPTheme.mono(5.6))
                                            .foregroundStyle(VPTheme.textTertiary)
                                    }
                                    Text(revision.reason ?? L10n.t("Initial team plan"))
                                        .font(VPTheme.mono(6.6))
                                        .foregroundStyle(VPTheme.textSecondary)
                                        .lineLimit(2)
                                    Text(revision.diffLabel)
                                        .font(VPTheme.mono(5.9))
                                        .foregroundStyle(VPTheme.textTertiary)
                                        .lineLimit(1)
                                }
                                Image(systemName: "chevron.right")
                                    .font(.system(size: 6, weight: .semibold))
                                    .foregroundStyle(VPTheme.textTertiary)
                                    .padding(.top, 2)
                            }
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                    }
                }
            }
        }
    }
}

// MARK: - Decisions and notes

private struct CollabOpsDecisionFeed: View {
    let decisions: [OrchestrationDecision]
    let notes: [OrchestrationNote]

    var body: some View {
        CollabOpsSection(
            title: "Controller activity",
            count: decisions.count,
            tint: VPTheme.info
        ) {
            if decisions.isEmpty, notes.isEmpty {
                Text(L10n.t("No orchestration decision yet. Finish a task or run Advance."))
                    .font(VPTheme.mono(7.2))
                    .foregroundStyle(VPTheme.textTertiary)
            } else {
                ForEach(decisions.prefix(8)) { decision in
                    HStack(alignment: .top, spacing: 5) {
                        Image(systemName: decision.icon)
                            .font(.system(size: 6.8, weight: .semibold))
                            .foregroundStyle(VPTheme.info)
                            .frame(width: 10)
                        VStack(alignment: .leading, spacing: 1) {
                            HStack(spacing: 4) {
                                Text(decision.action.uppercased())
                                    .font(VPTheme.mono(5.8, weight: .semibold))
                                    .tracking(0.35)
                                    .foregroundStyle(VPTheme.info)
                                if let taskID = decision.taskId {
                                    Text(taskID)
                                        .font(VPTheme.mono(5.8))
                                        .foregroundStyle(VPTheme.textTertiary)
                                }
                                Spacer()
                                Text(VPTheme.relativeTime(decision.createdAt))
                                    .font(VPTheme.mono(5.6))
                                    .foregroundStyle(VPTheme.textTertiary)
                            }
                            Text(decision.detail)
                                .font(VPTheme.mono(6.5))
                                .foregroundStyle(VPTheme.textSecondary)
                                .lineLimit(2)
                        }
                    }
                    .padding(.vertical, 2)
                }
                ForEach(notes.prefix(5)) { note in
                    HStack(alignment: .top, spacing: 5) {
                        Image(systemName: note.action == "blocker" ? "hand.raised" : "bubble.left")
                            .font(.system(size: 6.8, weight: .semibold))
                            .foregroundStyle(note.status == "open" ? VPTheme.warning : VPTheme.textTertiary)
                            .frame(width: 10)
                        VStack(alignment: .leading, spacing: 1) {
                            HStack(spacing: 4) {
                                Text(note.agentName)
                                    .font(VPTheme.mono(5.8, weight: .semibold))
                                    .foregroundStyle(VPTheme.textSecondary)
                                Text(note.action.uppercased())
                                    .font(VPTheme.mono(5.4, weight: .semibold))
                                    .tracking(0.3)
                                    .foregroundStyle(VPTheme.textTertiary)
                                if let taskID = note.taskId {
                                    Text(taskID)
                                        .font(VPTheme.mono(5.4))
                                        .foregroundStyle(VPTheme.textTertiary)
                                }
                                Spacer()
                                Text(VPTheme.relativeTime(note.createdAt))
                                    .font(VPTheme.mono(5.4))
                                    .foregroundStyle(VPTheme.textTertiary)
                            }
                            Text(note.message)
                                .font(VPTheme.mono(6.3))
                                .foregroundStyle(VPTheme.textSecondary)
                                .lineLimit(2)
                        }
                    }
                    .padding(.vertical, 2)
                }
            }
        }
    }
}

// MARK: - Policy

private struct CollabOpsPolicySummary: View {
    let policy: OrchestrationPolicy
    let onEdit: () -> Void

    var body: some View {
        CollabOpsSection(
            title: "Orchestration policy",
            count: nil,
            tint: VPTheme.warning,
            trailing: AnyView(
                Button(action: onEdit) {
                    Image(systemName: "slider.horizontal.3")
                        .font(.system(size: 7.5, weight: .semibold))
                        .foregroundStyle(VPTheme.warning)
                }
                .buttonStyle(.plain)
            )
        ) {
            VStack(alignment: .leading, spacing: 3) {
                PolicySummaryRow(
                    label: "Retries",
                    value: policy.autoRetryFailed
                        ? "\(policy.maxAttempts) attempts · \(policy.retryBackoffSecs)s backoff"
                        : "disabled"
                )
                PolicySummaryRow(label: "On failure", value: policy.dependencyFailure)
                PolicySummaryRow(
                    label: "Escalation",
                    value: policy.escalateAfterRetries ? "on" : "off"
                )
                PolicySummaryRow(
                    label: "Blockers",
                    value: policy.autoResolveBlockers ? "auto-resolve on completion" : "manual"
                )
            }
        }
    }
}

private struct PolicySummaryRow: View {
    let label: String
    let value: String

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            Text(L10n.t(label).uppercased())
                .font(VPTheme.mono(5.8, weight: .medium))
                .tracking(0.3)
                .foregroundStyle(VPTheme.textTertiary)
                .frame(width: 68, alignment: .leading)
            Text(value)
                .font(VPTheme.mono(6.6))
                .foregroundStyle(VPTheme.textSecondary)
                .lineLimit(1)
            Spacer()
        }
    }
}

// MARK: - Section chrome

private struct CollabOpsSection<Content: View>: View {
    let title: String
    let count: Int?
    let tint: Color
    let trailing: AnyView?
    @ViewBuilder let content: Content

    init(
        title: String,
        count: Int?,
        tint: Color,
        trailing: AnyView? = nil,
        @ViewBuilder content: () -> Content
    ) {
        self.title = title
        self.count = count
        self.tint = tint
        self.trailing = trailing
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 4) {
                Text(L10n.t(title).uppercased())
                    .font(VPTheme.mono(6.3, weight: .semibold))
                    .tracking(0.5)
                    .foregroundStyle(tint)
                if let count {
                    Text("\(count)")
                        .font(VPTheme.mono(6))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                Spacer()
                if let trailing {
                    trailing
                }
            }
            content
        }
        .padding(8)
        .background(VPTheme.surface.opacity(0.42), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
    }
}

private struct CollabOpsEmptyState: View {
    let session: Session?

    var body: some View {
        VStack(spacing: 8) {
            Image(systemName: "slider.horizontal.3")
                .font(.system(size: 20, weight: .light))
                .foregroundStyle(VPTheme.textTertiary)
            Text(L10n.t("No orchestration state"))
                .font(VPTheme.font(11, weight: .semibold))
                .foregroundStyle(VPTheme.textSecondary)
            Text(
                session == nil
                    ? L10n.t("Open a session to inspect its team operations.")
                    : L10n
                    .t(
                        "Run the Leader to create a TeamPlan, then retries, blockers, and revisions appear here."
                    )
            )
            .font(VPTheme.mono(7.4))
            .foregroundStyle(VPTheme.textTertiary)
            .multilineTextAlignment(.center)
            .frame(maxWidth: 260)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 30)
    }
}

// MARK: - Coordination composer

struct CoordinationComposeSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    let initialAction: String
    let tasks: [OrchestrationTask]
    let agents: [AgentRecord]

    @State private var action: String
    @State private var taskID: String
    @State private var targetAgentID: String
    @State private var severity = "info"
    @State private var message = ""
    @State private var busy = false
    @State private var error: String?

    private let actions = ["note", "blocker", "handoff", "decision", "escalate", "unblock"]

    init(
        initialAction: String,
        initialTaskID: String?,
        initialAgentID: String?,
        tasks: [OrchestrationTask],
        agents: [AgentRecord]
    ) {
        self.initialAction = initialAction
        self.tasks = tasks
        self.agents = agents
        _action = State(initialValue: actions.contains(initialAction) ? initialAction : "note")
        _taskID = State(initialValue: initialTaskID ?? "")
        _targetAgentID = State(initialValue: initialAgentID ?? "")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                VPKicker(text: "Coordinate", trailing: action.uppercased())
                Spacer()
                Button(L10n.t("Cancel")) { dismiss() }
                    .buttonStyle(.borderless)
                    .keyboardShortcut(.cancelAction)
            }

            Grid(alignment: .leading, horizontalSpacing: 10, verticalSpacing: 8) {
                GridRow {
                    Text(L10n.t("Action"))
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                    Picker("", selection: $action) {
                        ForEach(actions, id: \.self) { item in
                            Text(item.capitalized).tag(item)
                        }
                    }
                    .labelsHidden()
                    .frame(width: 180)
                }
                GridRow {
                    Text(L10n.t("Task"))
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                    Picker("", selection: $taskID) {
                        Text(L10n.t("None")).tag("")
                        ForEach(tasks) { task in
                            Text("\(task.taskId) · \(task.title)").tag(task.taskId)
                        }
                    }
                    .labelsHidden()
                    .frame(width: 280)
                }
                GridRow {
                    Text(L10n.t("Agent"))
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                    Picker("", selection: $targetAgentID) {
                        Text(L10n.t("Any")).tag("")
                        ForEach(agents) { agent in
                            Text("\(agent.name) · \(agent.role)").tag(agent.id)
                        }
                    }
                    .labelsHidden()
                    .frame(width: 280)
                }
                GridRow {
                    Text(L10n.t("Severity"))
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                    Picker("", selection: $severity) {
                        Text("Info").tag("info")
                        Text("Warning").tag("warning")
                        Text("Critical").tag("critical")
                    }
                    .labelsHidden()
                    .pickerStyle(.segmented)
                    .frame(width: 220)
                }
            }

            VStack(alignment: .leading, spacing: 4) {
                Text(L10n.t("Message").uppercased())
                    .font(VPTheme.mono(6.5, weight: .semibold))
                    .tracking(0.4)
                    .foregroundStyle(VPTheme.textTertiary)
                TextEditor(text: $message)
                    .font(VPTheme.mono(9))
                    .scrollContentBackground(.hidden)
                    .padding(6)
                    .frame(minHeight: 92)
                    .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
                    .overlay {
                        RoundedRectangle(cornerRadius: 8, style: .continuous)
                            .strokeBorder(VPTheme.hairline, lineWidth: 1)
                    }
            }

            if let error {
                Text(error)
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.danger)
            }

            HStack {
                Text("\(message.count)/2000")
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.textTertiary)
                Spacer()
                Button(L10n.t("Send note")) {
                    send()
                }
                .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                .disabled(busy || message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(16)
        .frame(width: 540)
        .background(VPTheme.canvas)
    }

    private func send() {
        busy = true
        error = nil
        Task {
            let ok = await model.addCoordinationNote(
                action: action,
                taskID: taskID.isEmpty ? nil : taskID,
                targetAgentID: targetAgentID.isEmpty ? nil : targetAgentID,
                message: message,
                severity: severity
            )
            busy = false
            if ok {
                dismiss()
            } else {
                error = model.collabTaskActionStatus
            }
        }
    }
}

// MARK: - Plan revision

struct PlanRevisionSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    @State private var draft = ""
    @State private var reason = ""
    @State private var expectedRevision = 0
    @State private var parsedPlan: TeamPlan?
    @State private var parseError: String?
    @State private var busy = false

    private var currentPlan: TeamPlan? {
        model.teamPlan
    }

    private var diff: PlanDraftDiff {
        PlanDraftDiff(old: currentPlan, new: parsedPlan)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                VPKicker(
                    text: "Revise team plan",
                    trailing: "r\(expectedRevision) → r\(expectedRevision + 1)"
                )
                Spacer()
                Button(L10n.t("Cancel")) { dismiss() }
                    .buttonStyle(.borderless)
                    .keyboardShortcut(.cancelAction)
            }

            Text(
                L10n
                    .t(
                        "Edit the stored plan JSON. Active, completed, and queued tasks cannot be removed or rewritten."
                    )
            )
            .font(VPTheme.mono(7.6))
            .foregroundStyle(VPTheme.textTertiary)

            HStack(alignment: .top, spacing: 10) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(L10n.t("Plan JSON").uppercased())
                        .font(VPTheme.mono(6.4, weight: .semibold))
                        .tracking(0.4)
                        .foregroundStyle(VPTheme.textTertiary)
                    TextEditor(text: $draft)
                        .font(VPTheme.mono(8))
                        .scrollContentBackground(.hidden)
                        .padding(6)
                        .frame(minWidth: 420, minHeight: 330)
                        .background(
                            VPTheme.surface,
                            in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                        )
                        .overlay {
                            RoundedRectangle(cornerRadius: 8, style: .continuous)
                                .strokeBorder(
                                    parseError == nil ? VPTheme.hairline : VPTheme.danger.opacity(0.5),
                                    lineWidth: 1
                                )
                        }
                }

                VStack(alignment: .leading, spacing: 7) {
                    Text(L10n.t("Change summary").uppercased())
                        .font(VPTheme.mono(6.4, weight: .semibold))
                        .tracking(0.4)
                        .foregroundStyle(VPTheme.textTertiary)
                    PlanDraftSummary(diff: diff, error: parseError)
                    if let parsedPlan {
                        Text("\(parsedPlan.departments.count) departments · \(parsedPlan.tasks.count) tasks")
                            .font(VPTheme.mono(6.8))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                    Spacer()
                }
                .frame(width: 220, alignment: .leading)
                .padding(8)
                .background(
                    VPTheme.surface.opacity(0.5),
                    in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                )
            }

            VStack(alignment: .leading, spacing: 4) {
                Text(L10n.t("Reason").uppercased())
                    .font(VPTheme.mono(6.4, weight: .semibold))
                    .tracking(0.4)
                    .foregroundStyle(VPTheme.textTertiary)
                TextField(L10n.t("Why is the team changing course?"), text: $reason)
                    .textFieldStyle(.roundedBorder)
                    .font(VPTheme.mono(8.5))
            }

            if let status = model.planRevisionStatus, !status.isEmpty {
                Text(status)
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(status.lowercased().contains("recorded") ? VPTheme.success : VPTheme
                        .danger)
            }

            HStack {
                Button(L10n.t("Reset")) {
                    load()
                }
                .buttonStyle(.borderless)
                Text("Revision is optimistic: another revision will be rejected if the plan changed.")
                    .font(VPTheme.mono(6.4))
                    .foregroundStyle(VPTheme.textTertiary)
                Spacer()
                Button(L10n.t("Record revision")) {
                    submit()
                }
                .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.plan))
                .disabled(busy || parsedPlan == nil || reason.trimmingCharacters(in: .whitespacesAndNewlines)
                    .isEmpty)
            }
        }
        .padding(16)
        .frame(width: 760)
        .background(VPTheme.canvas)
        .onAppear(perform: load)
        .onChange(of: draft) { _, _ in validate() }
    }

    private func load() {
        expectedRevision = model.orchestrationSnapshot?.planRevision ?? 0
        guard let plan = currentPlan else {
            draft = "{}"
            parsedPlan = nil
            parseError = "Session has no stored TeamPlan."
            return
        }
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        draft = (try? encoder.encode(plan)).flatMap { String(data: $0, encoding: .utf8) } ?? "{}"
        validate()
    }

    private func validate() {
        do {
            let plan = try JSONDecoder().decode(TeamPlan.self, from: Data(draft.utf8))
            guard !plan.goal.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
                throw PlanDraftError.emptyGoal
            }
            guard !plan.departments.isEmpty else {
                throw PlanDraftError.noDepartments
            }
            parsedPlan = plan
            parseError = nil
        } catch {
            parsedPlan = nil
            parseError = error.localizedDescription
        }
    }

    private func submit() {
        guard let parsedPlan else { return }
        busy = true
        Task {
            let ok = await model.reviseSelectedTeamPlan(
                parsedPlan,
                reason: reason,
                expectedRevision: expectedRevision
            )
            busy = false
            if ok {
                dismiss()
            }
        }
    }
}

enum PlanDraftError: LocalizedError {
    case emptyGoal
    case noDepartments

    var errorDescription: String? {
        switch self {
        case .emptyGoal: "Plan goal must not be empty."
        case .noDepartments: "Plan must contain at least one department."
        }
    }
}

struct PlanDraftDiff {
    let added: [String]
    let removed: [String]
    let changed: [String]

    init(old: TeamPlan?, new: TeamPlan?) {
        guard let new else {
            added = []
            removed = []
            changed = []
            return
        }
        let oldTasks = Dictionary(uniqueKeysWithValues: (old?.tasks ?? []).map { ($0.id, $0) })
        let newTasks = Dictionary(uniqueKeysWithValues: new.tasks.map { ($0.id, $0) })
        added = new.tasks.filter { oldTasks[$0.id] == nil }.map(\.id).sorted()
        removed = (old?.tasks ?? []).filter { newTasks[$0.id] == nil }.map(\.id).sorted()
        changed = new.tasks.filter { task in
            guard let previous = oldTasks[task.id] else { return false }
            return previous != task
        }.map(\.id).sorted()
    }

    var isEmpty: Bool {
        added.isEmpty && removed.isEmpty && changed.isEmpty
    }
}

private struct PlanDraftSummary: View {
    let diff: PlanDraftDiff
    let error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            if let error {
                Text(error)
                    .font(VPTheme.mono(6.8))
                    .foregroundStyle(VPTheme.danger)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if diff.isEmpty, error == nil {
                Text(L10n.t("No task changes"))
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            if !diff.added.isEmpty {
                diffRow("Added", ids: diff.added, tint: VPTheme.success)
            }
            if !diff.changed.isEmpty {
                diffRow("Changed", ids: diff.changed, tint: VPTheme.warning)
            }
            if !diff.removed.isEmpty {
                diffRow("Removed", ids: diff.removed, tint: VPTheme.danger)
            }
        }
    }

    private func diffRow(_ label: String, ids: [String], tint: Color) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(L10n.t(label).uppercased())
                .font(VPTheme.mono(5.8, weight: .semibold))
                .tracking(0.35)
                .foregroundStyle(tint)
            Text(ids.joined(separator: ", "))
                .font(VPTheme.mono(6.3))
                .foregroundStyle(VPTheme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}

// MARK: - Policy editor

struct OrchestrationPolicySheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    @State private var draft = OrchestrationPolicy.standard
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                VPKicker(text: "Orchestration policy")
                Spacer()
                Button(L10n.t("Cancel")) { dismiss() }
                    .buttonStyle(.borderless)
                    .keyboardShortcut(.cancelAction)
            }

            Text(
                L10n
                    .t(
                        "These controls are deterministic. They never route a model; they decide retries, dependency handling, and escalation."
                    )
            )
            .font(VPTheme.mono(7.6))
            .foregroundStyle(VPTheme.textTertiary)
            .fixedSize(horizontal: false, vertical: true)

            OrchestrationPolicyEditor(policy: $draft)

            if let error {
                Text(error)
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.danger)
            }
            if let status = model.orchestrationPolicyStatus, !status.isEmpty {
                Text(status)
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(status.lowercased().contains("saved") ? VPTheme.success : VPTheme.danger)
            }

            HStack {
                Spacer()
                Button(L10n.t("Save policy")) {
                    busy = true
                    error = nil
                    Task {
                        let ok = await model.saveOrchestrationPolicy(draft)
                        busy = false
                        if ok {
                            dismiss()
                        } else {
                            error = model.orchestrationPolicyStatus
                        }
                    }
                }
                .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.warning))
                .disabled(busy)
            }
        }
        .padding(16)
        .frame(width: 560)
        .background(VPTheme.canvas)
        .onAppear { draft = model.orchestrationPolicy }
    }
}

struct OrchestrationPolicyEditor: View {
    @Binding var policy: OrchestrationPolicy

    @State private var attemptsText = "2"
    @State private var backoffText = "30"
    @State private var boostText = "10"

    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            Toggle(isOn: $policy.autoRetryFailed) {
                VStack(alignment: .leading, spacing: 1) {
                    Text(L10n.t("Automatically retry failed tasks"))
                        .font(VPTheme.font(10, weight: .medium))
                    Text(L10n.t("Retries are scheduled by the controller with persisted backoff."))
                        .font(VPTheme.mono(6.8))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
            .toggleStyle(.switch)

            Grid(alignment: .leading, horizontalSpacing: 10, verticalSpacing: 7) {
                GridRow {
                    PolicyFieldLabel(title: "Max attempts")
                    TextField("", text: $attemptsText)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 80)
                        .onChange(of: attemptsText) { _, value in
                            if let parsed = Int(value) {
                                policy.maxAttempts = parsed
                            }
                        }
                    PolicyFieldLabel(title: "Backoff secs")
                    TextField("", text: $backoffText)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 80)
                        .onChange(of: backoffText) { _, value in
                            if let parsed = Int(value) {
                                policy.retryBackoffSecs = parsed
                            }
                        }
                }
                GridRow {
                    PolicyFieldLabel(title: "Priority boost")
                    TextField("", text: $boostText)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 80)
                        .onChange(of: boostText) { _, value in
                            if let parsed = Int(value) {
                                policy.retryPriorityBoost = parsed
                            }
                        }
                    PolicyFieldLabel(title: "Dependency failure")
                    Picker("", selection: $policy.dependencyFailure) {
                        Text(L10n.t("Block")).tag("block")
                        Text(L10n.t("Cancel")).tag("cancel")
                        Text(L10n.t("Retry")).tag("retry")
                    }
                    .labelsHidden()
                    .frame(width: 120)
                }
            }

            Toggle(isOn: $policy.escalateAfterRetries) {
                Text(L10n.t("Record a critical escalation after retries are exhausted"))
                    .font(VPTheme.font(10, weight: .medium))
            }
            .toggleStyle(.switch)

            Toggle(isOn: $policy.autoResolveBlockers) {
                Text(L10n.t("Resolve coordination blockers when their task completes"))
                    .font(VPTheme.font(10, weight: .medium))
            }
            .toggleStyle(.switch)

            Text("Retry budget applies per task attempt. Backoff is exponential and capped at 24h.")
                .font(VPTheme.mono(6.6))
                .foregroundStyle(VPTheme.textTertiary)
        }
        .onAppear {
            attemptsText = String(policy.maxAttempts)
            backoffText = String(policy.retryBackoffSecs)
            boostText = String(policy.retryPriorityBoost)
        }
    }
}

private struct PolicyFieldLabel: View {
    let title: String

    var body: some View {
        Text(L10n.t(title).uppercased())
            .font(VPTheme.mono(6.2, weight: .medium))
            .tracking(0.35)
            .foregroundStyle(VPTheme.textTertiary)
    }
}

// MARK: - Revision detail

struct RevisionSelection: Identifiable, Hashable {
    let revision: Int
    var id: Int {
        revision
    }
}

struct PlanRevisionDetailSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    let revision: Int
    @State private var restoreReason = ""
    @State private var showRestoreConfirm = false
    @State private var copied = false

    private var detail: PlanRevisionDetailResult? {
        guard model.planRevisionDetail?.revision.revision == revision else { return nil }
        return model.planRevisionDetail
    }

    private var planJSON: String {
        guard let plan = detail?.plan else { return "" }
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        return (try? encoder.encode(plan))
            .flatMap { String(data: $0, encoding: .utf8) } ?? ""
    }

    private var tasks: [PlanTask] {
        detail?.plan.tasks ?? []
    }

    private var added: Set<String> {
        Set(detail?.revision.added ?? [])
    }

    private var changed: Set<String> {
        Set(detail?.revision.changed ?? [])
    }

    private var removed: Set<String> {
        Set(detail?.revision.removed ?? [])
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                VPKicker(
                    text: "Plan revision",
                    trailing: detail.map { "r\($0.revision.revision)" }
                )
                if let detail {
                    Text(detail.revision.kindLabel.uppercased())
                        .font(VPTheme.mono(6.2, weight: .semibold))
                        .tracking(0.4)
                        .foregroundStyle(VPTheme.plan)
                        .padding(.horizontal, 5)
                        .padding(.vertical, 2)
                        .background(VPTheme.plan.opacity(0.09), in: Capsule())
                    Text(detail.revision.author)
                        .font(VPTheme.mono(6.4))
                        .foregroundStyle(VPTheme.textTertiary)
                    Text(VPTheme.relativeTime(detail.revision.createdAt))
                        .font(VPTheme.mono(6.4))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                Spacer()
                if model.planRevisionDetailBusy {
                    ProgressView().controlSize(.mini)
                }
                Button(L10n.t("Copy JSON")) {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(planJSON, forType: .string)
                    copied = true
                }
                .buttonStyle(.borderless)
                .font(VPTheme.mono(7.5))
                .disabled(detail == nil)
                Button(L10n.t("Close")) { dismiss() }
                    .buttonStyle(.borderless)
                    .keyboardShortcut(.cancelAction)
            }

            if let detail {
                Text(detail.revision.reason ?? L10n.t("Initial team plan"))
                    .font(VPTheme.font(11, weight: .medium))
                    .foregroundStyle(VPTheme.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)

                HStack(spacing: 5) {
                    revisionDiffChip("Added", detail.revision.added, VPTheme.success)
                    revisionDiffChip("Changed", detail.revision.changed, VPTheme.warning)
                    revisionDiffChip("Removed", detail.revision.removed, VPTheme.danger)
                    Spacer()
                    Text("\(detail.plan.departments.count) departments · \(detail.plan.tasks.count) tasks")
                        .font(VPTheme.mono(6.8))
                        .foregroundStyle(VPTheme.textTertiary)
                }

                HStack(alignment: .top, spacing: 10) {
                    VStack(alignment: .leading, spacing: 5) {
                        Text(L10n.t("Tasks").uppercased())
                            .font(VPTheme.mono(6.3, weight: .semibold))
                            .tracking(0.4)
                            .foregroundStyle(VPTheme.textTertiary)
                        ScrollView {
                            LazyVStack(alignment: .leading, spacing: 3) {
                                ForEach(tasks) { task in
                                    revisionTaskRow(task)
                                }
                            }
                        }
                        .frame(width: 330, height: 320)
                    }

                    VStack(alignment: .leading, spacing: 5) {
                        Text(L10n.t("Stored plan JSON").uppercased())
                            .font(VPTheme.mono(6.3, weight: .semibold))
                            .tracking(0.4)
                            .foregroundStyle(VPTheme.textTertiary)
                        ScrollView([.vertical, .horizontal]) {
                            Text(planJSON)
                                .font(VPTheme.mono(7.2))
                                .foregroundStyle(VPTheme.textSecondary)
                                .textSelection(.enabled)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(8)
                        }
                        .frame(minWidth: 360, minHeight: 320)
                        .background(
                            VPTheme.surface,
                            in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                        )
                        .overlay {
                            RoundedRectangle(cornerRadius: 8, style: .continuous)
                                .strokeBorder(VPTheme.hairline, lineWidth: 1)
                        }
                    }
                }

                Divider().opacity(0.4)

                VStack(alignment: .leading, spacing: 6) {
                    Text(L10n.t("Restore this revision").uppercased())
                        .font(VPTheme.mono(6.3, weight: .semibold))
                        .tracking(0.4)
                        .foregroundStyle(VPTheme.textTertiary)
                    Text(
                        L10n
                            .t(
                                "Restore creates a new revision from this snapshot. Active or completed task changes remain protected by the same revision validation."
                            )
                    )
                    .font(VPTheme.mono(6.8))
                    .foregroundStyle(VPTheme.textTertiary)
                    .fixedSize(horizontal: false, vertical: true)
                    HStack(spacing: 6) {
                        TextField(L10n.t("Why restore this revision?"), text: $restoreReason)
                            .textFieldStyle(.roundedBorder)
                            .font(VPTheme.mono(8))
                        Button(L10n.t("Restore")) {
                            showRestoreConfirm = true
                        }
                        .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.plan))
                        .disabled(
                            model.planRestoreBusy
                                || revision == model.orchestrationSnapshot?.planRevision
                                || restoreReason.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                        )
                    }
                    if let status = model.planRestoreStatus, !status.isEmpty {
                        Text(status)
                            .font(VPTheme.mono(7))
                            .foregroundStyle(status.lowercased().contains("restored") ? VPTheme
                                .success : VPTheme.danger)
                    }
                    if revision == model.orchestrationSnapshot?.planRevision {
                        Text(L10n.t("This is already the current plan revision."))
                            .font(VPTheme.mono(6.8))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                }
            } else if let status = model.planRevisionDetailStatus {
                Text(status)
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.danger)
            } else {
                ProgressView().controlSize(.small)
            }
        }
        .padding(16)
        .frame(width: 780)
        .background(VPTheme.canvas)
        .onAppear {
            if detail == nil {
                Task { await model.loadPlanRevisionDetail(revision) }
            }
        }
        .confirmationDialog(
            L10n.t("Restore plan revision?"),
            isPresented: $showRestoreConfirm
        ) {
            Button(L10n.t("Restore revision"), role: .destructive) {
                Task {
                    _ = await model.restorePlanRevision(
                        revision,
                        reason: restoreReason,
                        expectedRevision: model.orchestrationSnapshot?.planRevision ?? revision
                    )
                }
            }
            Button(L10n.t("Cancel"), role: .cancel) {}
        } message: {
            Text(L10n.t("This records a new plan revision with the stored tasks from this snapshot."))
        }
        .overlay(alignment: .bottomTrailing) {
            if copied {
                Text(L10n.t("Copied"))
                    .font(VPTheme.mono(6.5, weight: .semibold))
                    .foregroundStyle(VPTheme.success)
                    .padding(.horizontal, 7)
                    .padding(.vertical, 4)
                    .background(VPTheme.surface, in: Capsule())
                    .padding(14)
                    .transition(.opacity)
            }
        }
    }

    private func revisionTaskRow(_ task: PlanTask) -> some View {
        let tint: Color = added.contains(task.id)
            ? VPTheme.success
            : (changed.contains(task.id) ? VPTheme.warning : VPTheme.textTertiary)
        return HStack(spacing: 5) {
            Circle()
                .fill(added.contains(task.id) ? VPTheme.success : changed.contains(task.id) ? VPTheme
                    .warning : VPTheme.textTertiary.opacity(0.5))
                .frame(width: 4, height: 4)
            VStack(alignment: .leading, spacing: 1) {
                Text(task.title)
                    .font(VPTheme.font(8.5, weight: .medium))
                    .foregroundStyle(VPTheme.textPrimary)
                    .lineLimit(1)
                Text("\(task.id) · \(task.department) · p\(task.priority)")
                    .font(VPTheme.mono(5.9))
                    .foregroundStyle(tint)
                    .lineLimit(1)
            }
            Spacer()
        }
        .padding(.horizontal, 6)
        .padding(.vertical, 4)
        .background(VPTheme.surface.opacity(0.45), in: RoundedRectangle(cornerRadius: 6, style: .continuous))
    }

    private func revisionDiffChip(_ label: String, _ ids: [String], _ tint: Color) -> some View {
        HStack(spacing: 3) {
            Text(L10n.t(label).uppercased())
                .font(VPTheme.mono(5.7, weight: .semibold))
                .tracking(0.3)
            Text(ids.isEmpty ? "0" : ids.joined(separator: ","))
                .font(VPTheme.mono(6))
                .lineLimit(1)
        }
        .foregroundStyle(tint)
        .padding(.horizontal, 5)
        .padding(.vertical, 2)
        .background(tint.opacity(0.08), in: Capsule())
    }
}

// MARK: - Task editor

struct PlanTaskEditorSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    let task: OrchestrationTask

    @State private var title = ""
    @State private var department = ""
    @State private var kind = "coding"
    @State private var priority = "0"
    @State private var deps = ""
    @State private var writeScope = ""
    @State private var acceptance = ""
    @State private var tokenBudget = ""
    @State private var costBudget = ""
    @State private var reason = ""
    @State private var error: String?
    @State private var loaded = false

    private var planTask: PlanTask? {
        model.teamPlan?.tasks.first { $0.id == task.taskId }
    }

    private var departments: [PlanDepartment] {
        model.teamPlan?.departments ?? []
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                VPKicker(text: "Edit plan task", trailing: task.taskId)
                Spacer()
                Button(L10n.t("Cancel")) { dismiss() }
                    .buttonStyle(.borderless)
                    .keyboardShortcut(.cancelAction)
            }

            Text(
                L10n
                    .t(
                        "Saving creates a new plan revision. Active, running, or completed task changes are rejected by the same revision safety checks as a full plan edit."
                    )
            )
            .font(VPTheme.mono(7.2))
            .foregroundStyle(VPTheme.textTertiary)
            .fixedSize(horizontal: false, vertical: true)

            Grid(alignment: .leading, horizontalSpacing: 10, verticalSpacing: 8) {
                GridRow {
                    fieldLabel("Title")
                    TextField("", text: $title)
                        .textFieldStyle(.roundedBorder)
                        .font(VPTheme.mono(8.5))
                        .frame(width: 360)
                    fieldLabel("Department")
                    Picker("", selection: $department) {
                        ForEach(departments) { department in
                            Text(department.name).tag(department.name)
                        }
                    }
                    .labelsHidden()
                    .frame(width: 150)
                }
                GridRow {
                    fieldLabel("Kind")
                    TextField("", text: $kind)
                        .textFieldStyle(.roundedBorder)
                        .font(VPTheme.mono(8.5))
                        .frame(width: 160)
                    fieldLabel("Priority")
                    TextField("", text: $priority)
                        .textFieldStyle(.roundedBorder)
                        .font(VPTheme.mono(8.5))
                        .frame(width: 80)
                }
                GridRow {
                    fieldLabel("Dependencies")
                    TextField(L10n.t("comma-separated task ids"), text: $deps)
                        .textFieldStyle(.roundedBorder)
                        .font(VPTheme.mono(8.5))
                        .frame(width: 360)
                    fieldLabel("Write scope")
                    TextField(L10n.t("comma-separated paths"), text: $writeScope)
                        .textFieldStyle(.roundedBorder)
                        .font(VPTheme.mono(8.5))
                        .frame(width: 250)
                }
                GridRow {
                    fieldLabel("Acceptance")
                    TextField(L10n.t("comma-separated checks"), text: $acceptance)
                        .textFieldStyle(.roundedBorder)
                        .font(VPTheme.mono(8.5))
                        .frame(width: 360)
                    fieldLabel("Token cap")
                    TextField("", text: $tokenBudget)
                        .textFieldStyle(.roundedBorder)
                        .font(VPTheme.mono(8.5))
                        .frame(width: 90)
                    fieldLabel("USD cap")
                    TextField("", text: $costBudget)
                        .textFieldStyle(.roundedBorder)
                        .font(VPTheme.mono(8.5))
                        .frame(width: 90)
                }
            }

            VStack(alignment: .leading, spacing: 4) {
                fieldLabel("Reason")
                TextField(L10n.t("Why is this task changing?"), text: $reason)
                    .textFieldStyle(.roundedBorder)
                    .font(VPTheme.mono(8.5))
            }

            if let error {
                Text(error)
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.danger)
            }
            if let status = model.planRevisionStatus, !status.isEmpty {
                Text(status)
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(status.lowercased().contains("updated") ? VPTheme.success : VPTheme
                        .danger)
            }

            HStack {
                Text("Expected revision r\(model.orchestrationSnapshot?.planRevision ?? 0)")
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.textTertiary)
                Spacer()
                Button(L10n.t("Save task revision")) {
                    save()
                }
                .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.plan))
                .disabled(model.planRevisionBusy || reason.trimmingCharacters(in: .whitespacesAndNewlines)
                    .isEmpty)
            }
        }
        .padding(16)
        .frame(width: 720)
        .background(VPTheme.canvas)
        .onAppear(perform: load)
    }

    private func load() {
        guard !loaded else { return }
        loaded = true
        guard let planTask else {
            error = "Task is not part of the current TeamPlan."
            return
        }
        title = planTask.title
        department = planTask.department
        kind = planTask.kind
        priority = String(planTask.priority ?? 0)
        deps = planTask.deps.joined(separator: ", ")
        writeScope = planTask.writeScope.joined(separator: ", ")
        acceptance = planTask.acceptance.joined(separator: ", ")
        tokenBudget = planTask.tokenBudget.map { String($0) } ?? ""
        costBudget = planTask.costBudgetUsd.map { String($0) } ?? ""
    }

    private func save() {
        guard let planTask else {
            error = "Task is not part of the current TeamPlan."
            return
        }
        guard let priorityValue = Int(priority), (-100 ... 100).contains(priorityValue) else {
            error = "Priority must be between -100 and 100."
            return
        }
        guard departments.contains(where: { $0.name == department }) else {
            error = "Choose a department from the current plan."
            return
        }
        let tokens = tokenBudget.trimmingCharacters(in: .whitespaces)
        let cost = costBudget.trimmingCharacters(in: .whitespaces)
        let updated = PlanTask(
            id: planTask.id,
            department: department,
            title: title,
            kind: kind,
            priority: priorityValue,
            profile: planTask.profile,
            deps: splitList(deps),
            writeScope: splitList(writeScope),
            acceptance: splitList(acceptance),
            tokenBudget: tokens.isEmpty ? nil : Int(tokens),
            costBudgetUsd: cost.isEmpty ? nil : Double(cost)
        )
        if tokens.isEmpty == false, updated.tokenBudget == nil {
            error = "Token cap must be a whole number."
            return
        }
        if cost.isEmpty == false, updated.costBudgetUsd == nil {
            error = "USD cap must be a number."
            return
        }
        Task {
            let ok = await model.editPlanTask(updated, reason: reason)
            if ok {
                dismiss()
            } else {
                error = model.planRevisionStatus
            }
        }
    }

    private func splitList(_ text: String) -> [String] {
        text.split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
    }

    private func fieldLabel(_ title: String) -> some View {
        Text(L10n.t(title).uppercased())
            .font(VPTheme.mono(6.1, weight: .medium))
            .tracking(0.35)
            .foregroundStyle(VPTheme.textTertiary)
    }
}

// MARK: - Controller preview

struct OrchestrationPreviewSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    private var actions: [OrchestrationPreviewAction] {
        model.orchestrationPreview?.actions ?? []
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                VPKicker(
                    text: "Controller preview",
                    trailing: "\(actions.count) action(s)"
                )
                Spacer()
                if model.orchestrationPreviewBusy {
                    ProgressView().controlSize(.small)
                }
                Button(L10n.t("Refresh")) {
                    Task { await model.previewOrchestration() }
                }
                .buttonStyle(.borderless)
                .font(VPTheme.mono(7.5))
                Button(L10n.t("Close")) { dismiss() }
                    .buttonStyle(.borderless)
                    .keyboardShortcut(.cancelAction)
            }

            Text(
                L10n
                    .t(
                        "Preview is a pure projection. It shows exactly which retries, blocks, cancels, escalations, or automatic blocker resolutions the controller would apply, without changing any task."
                    )
            )
            .font(VPTheme.mono(7.4))
            .foregroundStyle(VPTheme.textTertiary)
            .fixedSize(horizontal: false, vertical: true)

            if actions.isEmpty {
                VStack(spacing: 6) {
                    Image(systemName: "checkmark.circle")
                        .font(.system(size: 18, weight: .light))
                        .foregroundStyle(VPTheme.success)
                    Text(L10n.t("Nothing to apply"))
                        .font(VPTheme.font(10.5, weight: .semibold))
                        .foregroundStyle(VPTheme.textSecondary)
                    Text(model.orchestrationPreviewStatus ?? L10n.t("The controller would make no changes."))
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 24)
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 5) {
                        ForEach(actions) { action in
                            HStack(alignment: .top, spacing: 7) {
                                Image(systemName: action.icon)
                                    .font(.system(size: 8, weight: .semibold))
                                    .foregroundStyle(tint(action.action))
                                    .frame(width: 12)
                                VStack(alignment: .leading, spacing: 2) {
                                    HStack(spacing: 4) {
                                        Text(action.action.uppercased())
                                            .font(VPTheme.mono(6.2, weight: .semibold))
                                            .tracking(0.35)
                                            .foregroundStyle(tint(action.action))
                                        Text(action.taskId)
                                            .font(VPTheme.mono(6.2, weight: .medium))
                                            .foregroundStyle(VPTheme.textSecondary)
                                        if action.attempt > 0 {
                                            Text("attempt \(action.attempt)")
                                                .font(VPTheme.mono(5.9))
                                                .foregroundStyle(VPTheme.textTertiary)
                                        }
                                        Spacer()
                                    }
                                    Text(action.detail)
                                        .font(VPTheme.mono(7.1))
                                        .foregroundStyle(VPTheme.textSecondary)
                                        .fixedSize(horizontal: false, vertical: true)
                                }
                            }
                            .padding(.horizontal, 8)
                            .padding(.vertical, 6)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(
                                tint(action.action).opacity(0.05),
                                in: RoundedRectangle(cornerRadius: 7, style: .continuous)
                            )
                            .overlay {
                                RoundedRectangle(cornerRadius: 7, style: .continuous)
                                    .strokeBorder(tint(action.action).opacity(0.12), lineWidth: 1)
                            }
                        }
                    }
                }
                .frame(maxHeight: 300)
            }

            if let status = model.orchestrationPreviewStatus, !actions.isEmpty {
                Text(status)
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
            }

            HStack {
                Text("Applying is atomic per decision and event-backed.")
                    .font(VPTheme.mono(6.4))
                    .foregroundStyle(VPTheme.textTertiary)
                Spacer()
                Button(L10n.t("Apply actions")) {
                    Task {
                        _ = await model.advanceOrchestration()
                        dismiss()
                    }
                }
                .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                .disabled(actions.isEmpty || model.orchestrationPreviewBusy)
            }
        }
        .padding(16)
        .frame(width: 560)
        .background(VPTheme.canvas)
        .onAppear {
            if model.orchestrationPreview == nil {
                Task { await model.previewOrchestration() }
            }
        }
    }

    private func tint(_ action: String) -> Color {
        switch action {
        case "retry": VPTheme.accent
        case "block": VPTheme.warning
        case "unblock": VPTheme.success
        case "cancel": VPTheme.danger
        case "escalate": VPTheme.danger
        case "resolve_blockers": VPTheme.info
        default: VPTheme.textSecondary
        }
    }
}

// MARK: - Coordination inbox

private struct CollabOpsCoordinationInbox: View {
    let notes: [OrchestrationNote]
    let busy: Bool
    let onResolve: (OrchestrationNote) -> Void
    let onReply: (OrchestrationNote) -> Void
    let onBlock: (OrchestrationNote) -> Void

    @State private var openOnly = true

    private var filtered: [OrchestrationNote] {
        let values = notes.sorted { $0.createdAt > $1.createdAt }
        return openOnly ? values.filter { $0.status == "open" } : values
    }

    var body: some View {
        CollabOpsSection(
            title: "Coordination inbox",
            count: filtered.count,
            tint: filtered.isEmpty ? VPTheme.textTertiary : VPTheme.plan,
            trailing: AnyView(
                Button {
                    withAnimation(VPMotion.snap) { openOnly.toggle() }
                } label: {
                    Text(openOnly ? L10n.t("Open") : L10n.t("All"))
                        .font(VPTheme.mono(6.2, weight: .medium))
                        .foregroundStyle(openOnly ? VPTheme.warning : VPTheme.textTertiary)
                }
                .buttonStyle(.plain)
            )
        ) {
            if filtered.isEmpty {
                Text(
                    openOnly
                        ? L10n.t("No open coordination note. Resolved notes remain in All.")
                        : L10n.t("No coordination note has been recorded.")
                )
                .font(VPTheme.mono(7.2))
                .foregroundStyle(VPTheme.textTertiary)
                .padding(.vertical, 3)
            } else {
                ForEach(filtered.prefix(10)) { note in
                    VStack(alignment: .leading, spacing: 3) {
                        HStack(spacing: 4) {
                            Image(systemName: icon(note.action))
                                .font(.system(size: 6.8, weight: .semibold))
                                .foregroundStyle(tint(note))
                            Text(note.action.uppercased())
                                .font(VPTheme.mono(5.8, weight: .semibold))
                                .tracking(0.35)
                                .foregroundStyle(tint(note))
                            Text(note.agentName)
                                .font(VPTheme.mono(6))
                                .foregroundStyle(VPTheme.textSecondary)
                            if let taskID = note.taskId {
                                Text("→ \(taskID)")
                                    .font(VPTheme.mono(5.8))
                                    .foregroundStyle(VPTheme.textTertiary)
                            }
                            if let target = note.targetAgentId {
                                Text("· @\(target.prefix(8))")
                                    .font(VPTheme.mono(5.6))
                                    .foregroundStyle(VPTheme.info)
                            }
                            Spacer()
                            Text(VPTheme.relativeTime(note.createdAt))
                                .font(VPTheme.mono(5.5))
                                .foregroundStyle(VPTheme.textTertiary)
                        }
                        Text(note.message)
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.textSecondary)
                            .lineLimit(3)
                            .fixedSize(horizontal: false, vertical: true)
                        HStack(spacing: 5) {
                            if note.status == "open", note.taskId != nil {
                                Button(L10n.t("Resolve")) {
                                    onResolve(note)
                                }
                                .font(VPTheme.mono(6.4, weight: .medium))
                                .buttonStyle(.borderless)
                                .foregroundStyle(VPTheme.success)
                            }
                            Button(L10n.t("Reply")) {
                                onReply(note)
                            }
                            .font(VPTheme.mono(6.4, weight: .medium))
                            .buttonStyle(.borderless)
                            .foregroundStyle(VPTheme.info)
                            if note.status != "open" {
                                Button(L10n.t("Block task")) {
                                    onBlock(note)
                                }
                                .font(VPTheme.mono(6.4, weight: .medium))
                                .buttonStyle(.borderless)
                                .foregroundStyle(VPTheme.warning)
                            }
                            Spacer()
                            if note.status == "resolved" {
                                Text(L10n.t("RESOLVED").uppercased())
                                    .font(VPTheme.mono(5.4, weight: .semibold))
                                    .tracking(0.3)
                                    .foregroundStyle(VPTheme.success)
                            }
                        }
                    }
                    .padding(.horizontal, 7)
                    .padding(.vertical, 5)
                    .background(
                        VPTheme.surface.opacity(0.5),
                        in: RoundedRectangle(cornerRadius: 7, style: .continuous)
                    )
                }
            }
        }
    }

    private func icon(_ action: String) -> String {
        switch action {
        case "blocker": "hand.raised"
        case "handoff": "arrow.right.circle"
        case "decision": "checkmark.seal"
        case "escalate": "exclamationmark.bubble"
        case "unblock": "lock.open"
        default: "bubble.left"
        }
    }

    private func tint(_ note: OrchestrationNote) -> Color {
        if note.status == "resolved" {
            return VPTheme.success
        }
        switch note.action {
        case "blocker": return VPTheme.warning
        case "escalate": return VPTheme.danger
        case "decision": return VPTheme.plan
        case "handoff": return VPTheme.info
        default: return VPTheme.textSecondary
        }
    }
}
