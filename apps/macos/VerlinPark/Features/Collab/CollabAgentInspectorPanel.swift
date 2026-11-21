import Foundation
import SwiftUI

/// The Collab Agent Inspector is the operator's per-Agent surface: bounded
/// daemon metadata plus a direct operator channel. Selection and RPC calls stay
/// in `CollabGraphView`/`AppModel`; this view only presents the projection and
/// submits explicit, non-optimistic actions.
struct CollabAgentInspectorPanel: View {
    let node: CollabGraphNode
    let task: OrchestrationTask?
    let blockers: [OrchestrationBlocker]
    let busy: Bool
    let onAction: (CollabTaskCommand) -> Void
    let onClose: () -> Void

    @Environment(AppModel.self) private var model

    @State private var tab: CollabAgentInspectorTab
    @State private var composerText = ""
    @State private var wakeSession = false
    @State private var messageAttemptAgentID: String?
    @FocusState private var composerFocused: Bool

    private let messageLimit = 4000

    init(
        node: CollabGraphNode,
        task: OrchestrationTask?,
        blockers: [OrchestrationBlocker],
        busy: Bool,
        onAction: @escaping (CollabTaskCommand) -> Void,
        onClose: @escaping () -> Void,
        initialTab: CollabAgentInspectorTab = .overview
    ) {
        self.node = node
        self.task = task
        self.blockers = blockers
        self.busy = busy
        self.onAction = onAction
        self.onClose = onClose
        _tab = State(initialValue: initialTab)
    }

    private var agentID: String {
        node.id
    }

    private var inspector: AgentInspectResult? {
        guard model.collabAgentInspectAgentID == agentID,
              let inspect = model.collabAgentInspect,
              inspect.agent.id == agentID
        else { return nil }
        return inspect
    }

    private var resolvedAgent: AgentRecord? {
        inspector?.agent ?? model.agents.first { $0.id == agentID }
    }

    private var displayName: String {
        resolvedAgent?.name ?? node.name
    }

    private var displayTitle: String? {
        let title = resolvedAgent?.title ?? node.title
        guard let title, !title.isEmpty, title != displayName else { return nil }
        return title
    }

    private var role: String {
        resolvedAgent?.role ?? node.kind.rawValue
    }

    private var status: String {
        resolvedAgent?.status ?? node.status
    }

    private var taskStatus: String? {
        inspector?.task?.status ?? node.taskStatus ?? task?.status
    }

    private var identityID: String? {
        let value = inspector?.agent.identityId ?? resolvedAgent?.identityId
        guard let value, !value.isEmpty else { return nil }
        return value
    }

    private var parentName: String? {
        guard let parentID = resolvedAgent?.parentId ?? node.parentID else { return nil }
        if let parent = model.agents.first(where: { $0.id == parentID }) {
            return parent.name
        }
        return String(parentID.prefix(8))
    }

    private var profile: String? {
        let value = inspector?.agent.profile ?? resolvedAgent?.profile ?? node.profile
        guard let value, !value.isEmpty else { return nil }
        return value
    }

    private var modelName: String {
        let value = inspector?.agent.model ?? resolvedAgent?.model ?? node.model
        return value.isEmpty ? (inspector?.context.model ?? "—") : value
    }

    private var department: String? {
        let value = resolvedAgent?.department ?? node.department
        guard let value, !value.isEmpty else { return nil }
        return value
    }

    /// Broad "not finished" state used only for the avatar/status treatment.
    /// Paused/queued ephemeral Agents are still not steerable.
    private var isActiveRun: Bool {
        let candidates = [
            status,
            node.status,
            resolvedAgent?.status ?? "",
        ]
        return candidates.contains { value in
            [
                "running", "queued", "paused", "pausing", "cancelling", "waiting",
                "waiting_input", "thinking", "verifying", "merging", "in_progress",
                "delivering",
            ].contains(value.lowercased())
        }
    }

    /// The daemon only accepts `session.steer` for a running or waiting_input
    /// Agent. Paused/queued Agents must route to the identity inbox or show the
    /// typed not-active state; the UI must not promise steering they cannot
    /// receive.
    private var canSteer: Bool {
        // The loaded Agent record is authoritative; the graph node is a
        // projection and can briefly lag after a status transition.
        let current = inspector?.agent.status ?? resolvedAgent?.status ?? status
        return CollabInspectorComposerMode.canSteer(status: current)
    }

    private var composerMode: CollabInspectorComposerMode {
        CollabInspectorComposerMode.resolve(
            isActiveRun: canSteer,
            identityID: identityID,
            hasAgent: resolvedAgent != nil || inspector != nil
        )
    }

    private var canCompose: Bool {
        switch composerMode {
        case .steering, .inbox: true
        case .inactive, .unavailable: false
        }
    }

    private var trimmedComposerText: String {
        composerText.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var canSend: Bool {
        canCompose
            && !trimmedComposerText.isEmpty
            && !model.collabAgentMessageBusy
            && !model.collabAgentInspectBusy
    }

    private var steeringRows: [SteeringMessage] {
        var seen = Set<String>()
        let local = (inspector?.steering ?? []) + model.steeringMessages
        return local
            .filter { $0.agentId == agentID && seen.insert($0.id).inserted }
            .sorted { $0.createdAt > $1.createdAt }
    }

    private var inboxCounts: AgentInboxCountsView {
        inspector?.inboxCounts ?? AgentInboxCountsView()
    }

    private var timeline: [AgentTimelineEntry] {
        inspector?.timeline ?? []
    }

    private var taskPlan: PlanTask? {
        guard let taskID = inspector?.task?.taskID ?? node.taskID ?? task?.taskId else {
            return nil
        }
        return model.teamPlan?.tasks.first { $0.id == taskID }
    }

    private var delivery: AgentMessageDelivery? {
        guard let delivery = model.collabAgentMessageDelivery else { return nil }
        if !delivery.agentID.isEmpty, delivery.agentID != agentID {
            return nil
        }
        return delivery
    }

    private var deliveryNotice: String? {
        guard let delivery else { return nil }
        return model.collabAgentMessageNotice ?? AgentInspectRules.deliveryNotice(delivery)
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            VPHairline()
            tabs
            VPHairline()
            content
        }
        .background(
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .fill(VPTheme.surfaceRaised.opacity(0.985))
        )
        .overlay {
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .strokeBorder(VPTheme.edgeGradient, lineWidth: 1)
        }
        .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
        .shadow(color: .black.opacity(0.34), radius: 22, y: 10)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .onChange(of: agentID) { _, _ in
            composerText = ""
            wakeSession = false
            messageAttemptAgentID = nil
            composerFocused = false
        }
        .onChange(of: composerText) { _, value in
            guard value.count > messageLimit else { return }
            composerText = String(value.prefix(messageLimit))
        }
        .onChange(of: canCompose) { _, allowed in
            if !allowed {
                composerFocused = false
            }
        }
    }

    // MARK: - Header

    private var header: some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack(alignment: .top, spacing: 8) {
                VPAgentAvatar(
                    seed: displayName,
                    role: role,
                    size: 30,
                    active: isActiveRun
                )

                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 5) {
                        Text(displayName)
                            .font(VPTheme.font(12, weight: .semibold))
                            .foregroundStyle(VPTheme.textPrimary)
                            .lineLimit(1)
                        if let displayTitle {
                            Text(displayTitle)
                                .font(VPTheme.mono(7))
                                .foregroundStyle(VPTheme.textTertiary)
                                .lineLimit(1)
                        }
                    }
                    Text(headerSubtitle)
                        .font(VPTheme.mono(6.8))
                        .foregroundStyle(VPTheme.textTertiary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }

                Spacer(minLength: 2)

                if model.collabAgentInspectBusy {
                    ProgressView()
                        .controlSize(.mini)
                        .help(L10n.t("Loading the Agent inspector"))
                }

                Button {
                    Task { await refresh() }
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .buttonStyle(VPGlassIconButtonStyle(size: 23, tint: VPTheme.textSecondary))
                .disabled(model.collabAgentInspectBusy)
                .help(L10n.t("Reload the Agent inspector and steering"))

                VPCloseButton(
                    size: 23,
                    help: L10n.t("Close Agent inspector")
                ) {
                    onClose()
                }
            }

            HStack(spacing: 5) {
                StatusPill(status: status)
                miniTag(role.uppercased(), tint: VPTheme.roleColor(role))
                if let profile {
                    miniTag(profile.uppercased(), tint: VPTheme.textTertiary)
                }
                miniTag(
                    identityID == nil ? "EPHEMERAL" : "IDENTITY",
                    tint: identityID == nil ? VPTheme.textTertiary : VPTheme.accent
                )
                Spacer(minLength: 2)
                if inboxCounts.unread > 0 {
                    HStack(spacing: 3) {
                        Image(systemName: "tray.full.fill")
                            .font(.system(size: 6, weight: .semibold))
                        Text("\(inboxCounts.unread) \(L10n.t("unread").lowercased())")
                            .font(VPTheme.mono(6.2, weight: .semibold))
                    }
                    .foregroundStyle(VPTheme.warning)
                    .padding(.horizontal, 5)
                    .padding(.vertical, 2)
                    .background(VPTheme.warning.opacity(0.12), in: Capsule())
                }
                if inspector?.truncated == true {
                    miniTag("TRUNCATED", tint: VPTheme.warning)
                }
            }

            if let parentName {
                Text(L10n.format("Reports to %@", parentName))
                    .font(VPTheme.mono(6.4))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(1)
            }
        }
        .padding(.horizontal, 12)
        .padding(.top, 10)
        .padding(.bottom, 9)
    }

    private var headerSubtitle: String {
        [modelName, department, taskLabel]
            .compactMap { value in
                guard let value, !value.isEmpty else { return nil }
                return value
            }
            .joined(separator: " · ")
    }

    private var taskLabel: String? {
        if let taskID = inspector?.task?.taskID ?? node.taskID ?? task?.taskId {
            return taskID
        }
        return nil
    }

    private func miniTag(_ text: String, tint: Color) -> some View {
        Text(text)
            .font(VPTheme.mono(5.9, weight: .semibold))
            .tracking(0.3)
            .foregroundStyle(tint)
            .padding(.horizontal, 4)
            .padding(.vertical, 1.5)
            .background(tint.opacity(0.09), in: Capsule())
            .overlay {
                Capsule().strokeBorder(tint.opacity(0.18), lineWidth: 0.7)
            }
    }

    // MARK: - Tabs

    private var tabs: some View {
        VPSegmentedControl(
            segments: CollabAgentInspectorTab.allCases.map {
                VPSegment($0, title: $0.title)
            },
            selection: $tab,
            scrollable: true,
            compact: true
        )
        .frame(height: 27)
        .padding(.horizontal, 10)
        .padding(.vertical, 7)
    }

    @ViewBuilder
    private var content: some View {
        switch tab {
        case .overview:
            scrollBody {
                overviewContent
            }
        case .context:
            gatedContent { inspect in
                scrollBody {
                    contextContent(inspect)
                }
            }
        case .conversation:
            gatedContent { inspect in
                scrollBody {
                    timelineContent(
                        entries: filteredTimeline(
                            inspect.timeline,
                            categories: ["message", "tool", "result"]
                        ),
                        emptyMessage: L10n.t("No visible conversation events for this Agent."),
                        emptyDetail: L10n.t(
                            "Only bounded message, tool, and result entries from this Agent are shown."
                        )
                    )
                }
            }
        case .messages:
            messagesContent
        case .activity:
            gatedContent { inspect in
                scrollBody {
                    activityContent(inspect)
                }
            }
        }
    }

    private func scrollBody(@ViewBuilder content: () -> some View) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 9) {
                content()
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .scrollIndicators(.automatic)
    }

    @ViewBuilder
    private func gatedContent(
        @ViewBuilder content: (AgentInspectResult) -> some View
    ) -> some View {
        if let inspector {
            content(inspector)
        } else if model.collabAgentInspectBusy {
            inspectorLoadingState
        } else if let error = model.collabAgentInspectError {
            inspectorErrorState(error)
        } else {
            inspectorEmptyState
        }
    }

    // MARK: - Overview

    private var overviewContent: some View {
        VStack(alignment: .leading, spacing: 9) {
            inspectorStateStrip

            if let task {
                CollabInspectorActions(task: task, busy: busy, onAction: onAction)
                VPHairline()
            }

            sectionHeader("Agent")
            CollabDetailRow(label: "Role", value: role.capitalized, tint: VPTheme.roleColor(role))
            if let displayTitle {
                CollabDetailRow(label: "Title", value: displayTitle, tint: VPTheme.textPrimary)
            }
            if let department {
                CollabDetailRow(label: "Department", value: department, tint: VPTheme.textSecondary)
            }
            CollabDetailRow(
                label: "Status",
                value: status,
                tint: VPTheme.statusColor(status)
            )
            if let taskStatus {
                CollabDetailRow(
                    label: "Task status",
                    value: taskStatus,
                    tint: VPTheme.statusColor(taskStatus)
                )
            }
            CollabDetailRow(label: "Model", value: modelName, tint: VPTheme.textSecondary)
            if let profile {
                CollabDetailRow(label: "Profile", value: profile, tint: VPTheme.textSecondary)
            }
            if let identityID {
                CollabDetailRow(
                    label: "Identity",
                    value: String(identityID.prefix(16)),
                    tint: VPTheme.accent
                )
            }
            if let parentName {
                CollabDetailRow(label: "Parent", value: parentName, tint: VPTheme.textSecondary)
            }
            if let branch = resolvedAgent?.branch, !branch.isEmpty {
                CollabDetailRow(label: "Branch", value: branch, tint: VPTheme.textTertiary)
            }
            if let worktree = resolvedAgent?.worktree, !worktree.isEmpty {
                CollabDetailRow(label: "Worktree", value: worktree, tint: VPTheme.textTertiary)
            }

            if let taskLine = inspector?.task.map({ detail(for: $0) }) ?? node.taskLine {
                VPHairline()
                sectionHeader("Task")
                CollabDetailRow(label: "Task", value: taskLine, tint: VPTheme.textSecondary)
            }
            if let inspectTask = inspector?.task {
                if !inspectTask.writeScope.isEmpty {
                    CollabDetailRow(
                        label: "Scope",
                        value: inspectTask.writeScope.joined(separator: ", "),
                        tint: VPTheme.textSecondary
                    )
                }
                if !inspectTask.acceptance.isEmpty {
                    CollabDetailRow(
                        label: "Acceptance",
                        value: inspectTask.acceptance.joined(separator: " · "),
                        tint: VPTheme.textSecondary
                    )
                }
                if !inspectTask.deps.isEmpty {
                    CollabDetailRow(
                        label: "Depends on",
                        value: inspectTask.deps.joined(separator: ", "),
                        tint: VPTheme.warning
                    )
                }
                if inspectTask.maxAttempts > 0 {
                    CollabDetailRow(
                        label: "Attempts",
                        value: "\(inspectTask.attempts)/\(inspectTask.maxAttempts)",
                        tint: inspectTask.attempts >= inspectTask.maxAttempts
                            ? VPTheme.danger
                            : VPTheme.textSecondary
                    )
                }
                if let verifier = inspectTask.verifierStatus {
                    CollabDetailRow(
                        label: "Verifier",
                        value: verifier,
                        tint: VPTheme.statusColor(verifier)
                    )
                }
            } else {
                if !node.writeScope.isEmpty {
                    CollabDetailRow(
                        label: "Scope",
                        value: node.writeScope.joined(separator: ", "),
                        tint: VPTheme.textSecondary
                    )
                }
                if !node.acceptance.isEmpty {
                    CollabDetailRow(
                        label: "Acceptance",
                        value: node.acceptance.joined(separator: " · "),
                        tint: VPTheme.textSecondary
                    )
                }
                if !node.deps.isEmpty {
                    CollabDetailRow(
                        label: "Depends on",
                        value: node.deps.joined(separator: ", "),
                        tint: VPTheme.warning
                    )
                }
            }
            if let task {
                if let next = task.nextAttemptAt {
                    CollabDetailRow(
                        label: "Next retry",
                        value: VPTheme.relativeTime(next),
                        tint: VPTheme.warning
                    )
                }
                if !task.blockedBy.isEmpty {
                    CollabDetailRow(
                        label: "Blocked by",
                        value: task.blockedBy.joined(separator: ", "),
                        tint: VPTheme.warning
                    )
                }
            }

            if let budgetRows {
                VPHairline()
                sectionHeader("Budget")
                ForEach(budgetRows, id: \.label) { row in
                    CollabDetailRow(label: row.label, value: row.value, tint: row.tint)
                }
            }

            ForEach(blockers.prefix(3)) { blocker in
                CollabDetailRow(
                    label: blocker.kind.replacingOccurrences(of: "_", with: " "),
                    value: blocker.message,
                    tint: VPTheme.danger
                )
            }

            if let activity = node.activity, !activity.isEmpty {
                CollabDetailRow(label: "Activity", value: activity, tint: VPTheme.textSecondary)
            }
            if let lastError = node.lastError, !lastError.isEmpty {
                CollabDetailRow(label: "Last error", value: lastError, tint: VPTheme.danger)
            }
        }
    }

    private func detail(for task: AgentTaskView) -> String {
        task.taskID.isEmpty ? task.title : "\(task.taskID) · \(task.title)"
    }

    private struct BudgetRow {
        let label: String
        let value: String
        let tint: Color
    }

    private var budgetRows: [BudgetRow]? {
        var rows: [BudgetRow] = []
        if let tokenBudget = taskPlan?.tokenBudget {
            rows.append(BudgetRow(
                label: "Task tokens",
                value: tokenText(tokenBudget),
                tint: VPTheme.textSecondary
            ))
        }
        if let costBudget = taskPlan?.costBudgetUsd {
            rows.append(BudgetRow(
                label: "Task cost",
                value: moneyText(costBudget),
                tint: VPTheme.textSecondary
            ))
        }
        if let session = model.selectedSession {
            let used = session.tokensIn + session.tokensOut
            if let budget = session.tokenBudget {
                rows.append(BudgetRow(
                    label: "Session tokens",
                    value: "\(tokenText(used)) / \(tokenText(budget))",
                    tint: used >= budget ? VPTheme.danger : VPTheme.textSecondary
                ))
            } else if session.unlimited {
                rows.append(BudgetRow(
                    label: "Session tokens",
                    value: "\(tokenText(used)) · unlimited",
                    tint: VPTheme.textTertiary
                ))
            }
            if let budget = session.costBudgetUsd {
                rows.append(BudgetRow(
                    label: "Session cost",
                    value: "\(moneyText(session.costSpentUsd)) / \(moneyText(budget))",
                    tint: session.costSpentUsd >= budget ? VPTheme.danger : VPTheme.textSecondary
                ))
            }
        }
        if let inspect = inspector, inspect.context.costUSD > 0 {
            rows.append(BudgetRow(
                label: "Agent cost",
                value: moneyText(inspect.context.costUSD),
                tint: VPTheme.textSecondary
            ))
        }
        return rows.isEmpty ? nil : rows
    }

    // MARK: - Context

    private func contextContent(_ inspect: AgentInspectResult) -> some View {
        let context = inspect.context
        return VStack(alignment: .leading, spacing: 9) {
            inspectorStateStrip

            sectionHeader("Objective")
            Text(context.objective ?? node.objective ?? L10n.t("No objective recorded."))
                .font(VPTheme.font(10))
                .foregroundStyle(VPTheme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
                .lineLimit(5)
                .textSelection(.enabled)

            if let inspectTask = inspect.task {
                if !inspectTask.acceptance.isEmpty {
                    sectionHeader("Acceptance")
                    Text(inspectTask.acceptance.joined(separator: " · "))
                        .font(VPTheme.mono(7.5))
                        .foregroundStyle(VPTheme.textSecondary)
                        .fixedSize(horizontal: false, vertical: true)
                        .lineLimit(5)
                }
                if !inspectTask.writeScope.isEmpty {
                    sectionHeader("Write scope")
                    Text(inspectTask.writeScope.joined(separator: ", "))
                        .font(VPTheme.mono(7.5))
                        .foregroundStyle(VPTheme.textSecondary)
                        .fixedSize(horizontal: false, vertical: true)
                        .lineLimit(5)
                }
            }

            VPHairline()
            sectionHeader("Context usage")
            CollabInspectorMeter(
                used: context.estimatedTokens,
                window: context.contextWindow,
                threshold: context.thresholdTokens,
                hardLimit: context.hardLimitTokens
            )
            HStack(spacing: 8) {
                contextMetric("Window", contextWindowText(context.contextWindow))
                contextMetric("Threshold", contextWindowText(context.thresholdTokens))
                contextMetric("Hard limit", contextWindowText(context.hardLimitTokens))
            }
            if context.contextWindow == 0, context.estimatedTokens == 0 {
                emptyInline(L10n.t("No context telemetry is available for this Agent yet."))
            }

            VPHairline()
            sectionHeader("Provider telemetry")
            CollabDetailRow(
                label: "Input",
                value: tokenText(context.inputTokens),
                tint: VPTheme.textSecondary
            )
            CollabDetailRow(
                label: "Output",
                value: tokenText(context.outputTokens),
                tint: VPTheme.textSecondary
            )
            CollabDetailRow(
                label: "Cache read",
                value: tokenText(context.cacheReadTokens),
                tint: VPTheme.info
            )
            CollabDetailRow(
                label: "Cache write",
                value: tokenText(context.cacheWriteTokens),
                tint: VPTheme.info
            )
            CollabDetailRow(
                label: "Cost",
                value: moneyText(context.costUSD),
                tint: VPTheme.textSecondary
            )

            VPHairline()
            sectionHeader("Compaction & memory")
            CollabDetailRow(
                label: "Compaction",
                value: context.lastCompaction ?? L10n.t("No compaction recorded."),
                tint: VPTheme.textSecondary
            )
            CollabDetailRow(
                label: "Memory brief",
                value: memoryBriefText(context),
                tint: VPTheme.textSecondary
            )
            if let hash = context.memoryBriefHash, !hash.isEmpty {
                CollabDetailRow(
                    label: "Memory hash",
                    value: String(hash.prefix(20)),
                    tint: VPTheme.textTertiary
                )
            }

            VPHairline()
            sectionHeader("Tools & cache")
            CollabDetailRow(
                label: "Tool bundle",
                value: toolBundleText(context),
                tint: VPTheme.textSecondary
            )
            if let prefix = context.prefixHash, !prefix.isEmpty {
                CollabDetailRow(
                    label: "Prefix hash",
                    value: String(prefix.prefix(24)),
                    tint: VPTheme.textTertiary
                )
            }
            if !context.generatedAt.isEmpty {
                CollabDetailRow(
                    label: "Generated",
                    value: VPTheme.relativeTime(context.generatedAt),
                    tint: VPTheme.textTertiary
                )
            }
        }
    }

    private func contextMetric(_ label: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(L10n.t(label).uppercased())
                .font(VPTheme.mono(5.9, weight: .medium))
                .tracking(0.3)
                .foregroundStyle(VPTheme.textTertiary)
            Text(value)
                .font(VPTheme.mono(8, weight: .semibold))
                .foregroundStyle(VPTheme.textSecondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func contextWindowText(_ value: Int) -> String {
        value > 0 ? tokenText(value) : "—"
    }

    private func memoryBriefText(_ context: AgentContextView) -> String {
        guard context.memoryBriefChars > 0 || context.memoryBriefHash != nil else {
            return L10n.t("No memory brief recorded.")
        }
        return L10n.format("%@ characters", tokenText(context.memoryBriefChars))
    }

    private func toolBundleText(_ context: AgentContextView) -> String {
        let bundle = context.toolBundle?.trimmingCharacters(in: .whitespacesAndNewlines)
        let label = (bundle?.isEmpty == false ? bundle : nil) ?? L10n.t("not reported")
        return "\(label) · \(context.toolCount) \(L10n.t("tools"))"
    }

    // MARK: - Conversation & Activity

    private func activityContent(_ inspect: AgentInspectResult) -> some View {
        VStack(alignment: .leading, spacing: 9) {
            inspectorStateStrip
            let categories = categoryCounts(inspect.timeline)
            if !categories.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 4) {
                        ForEach(categories) { item in
                            HStack(spacing: 3) {
                                Image(systemName: timelineIcon(item.category))
                                    .font(.system(size: 6, weight: .semibold))
                                Text("\(item.category.uppercased()) \(item.count)")
                                    .font(VPTheme.mono(5.9, weight: .medium))
                            }
                            .foregroundStyle(timelineTint(item.category))
                            .padding(.horizontal, 5)
                            .padding(.vertical, 2)
                            .background(timelineTint(item.category).opacity(0.08), in: Capsule())
                        }
                    }
                }
            }

            timelineContent(
                entries: filteredTimeline(inspect.timeline, categories: nil),
                emptyMessage: L10n.t("No activity events for this Agent."),
                emptyDetail: L10n.t(
                    "Approvals, tools, commits, merges, coordination, and recovery events appear here."
                )
            )
        }
    }

    private func timelineContent(
        entries: [AgentTimelineEntry],
        emptyMessage: String,
        emptyDetail: String
    ) -> some View {
        VStack(alignment: .leading, spacing: 7) {
            if inspector?.truncated == true {
                inlineNotice(
                    L10n.t("The daemon bounded this timeline. Older events are omitted."),
                    tint: VPTheme.warning,
                    icon: "scissors"
                )
            }

            if entries.isEmpty {
                emptyBlock(emptyMessage, detail: emptyDetail)
            } else {
                HStack(spacing: 4) {
                    Text(L10n.t("Latest first"))
                        .font(VPTheme.mono(6.2, weight: .medium))
                        .foregroundStyle(VPTheme.textTertiary)
                    Spacer()
                    Text("\(entries.count)")
                        .font(VPTheme.mono(6.2, weight: .semibold))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                ForEach(entries.prefix(80)) { entry in
                    timelineRow(entry)
                }
                if entries.count > 80 {
                    Text(L10n.format("Showing 80 of %@ entries.", "\(entries.count)"))
                        .font(VPTheme.mono(6.4))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
        }
    }

    private func timelineRow(_ entry: AgentTimelineEntry) -> some View {
        let tint = entry.isError ? VPTheme.danger : timelineTint(entry.category)
        return HStack(alignment: .top, spacing: 7) {
            Image(systemName: entry.isError ? "exclamationmark.triangle.fill" : timelineIcon(entry.category))
                .font(.system(size: 8, weight: .semibold))
                .foregroundStyle(tint)
                .frame(width: 11)
                .padding(.top, 2)

            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 4) {
                    Text(timelineLabel(entry))
                        .font(VPTheme.mono(7.2, weight: .semibold))
                        .foregroundStyle(tint)
                        .lineLimit(1)
                    if let status = entry.status, !status.isEmpty {
                        Text(status.uppercased())
                            .font(VPTheme.mono(5.6, weight: .semibold))
                            .foregroundStyle(VPTheme.statusColor(status))
                            .lineLimit(1)
                    }
                    Spacer(minLength: 2)
                    if entry.seq > 0 {
                        Text("#\(entry.seq)")
                            .font(VPTheme.mono(5.6))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                    if !entry.createdAt.isEmpty {
                        Text(VPTheme.relativeTime(entry.createdAt))
                            .font(VPTheme.mono(5.6))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                }
                if let text = entry.text, !text.isEmpty {
                    Text(text)
                        .font(VPTheme.font(9))
                        .foregroundStyle(entry.isError ? VPTheme.danger : VPTheme.textPrimary)
                        .lineLimit(6)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                }
            }
        }
        .padding(.vertical, 4)
        .padding(.horizontal, 6)
        .background(
            VPTheme.surfaceSunken.opacity(0.62),
            in: RoundedRectangle(cornerRadius: 8, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .strokeBorder(entry.isError ? VPTheme.danger.opacity(0.22) : VPTheme.hairline, lineWidth: 1)
        }
    }

    private func filteredTimeline(
        _ entries: [AgentTimelineEntry],
        categories: Set<String>?
    ) -> [AgentTimelineEntry] {
        entries.enumerated()
            .filter { _, entry in
                guard let categories else { return true }
                return categories.contains(entry.category.lowercased())
            }
            .sorted { lhs, rhs in
                if lhs.element.seq != rhs.element.seq {
                    return lhs.element.seq > rhs.element.seq
                }
                if lhs.element.createdAt != rhs.element.createdAt {
                    return lhs.element.createdAt > rhs.element.createdAt
                }
                return lhs.offset > rhs.offset
            }
            .map(\.element)
    }

    private struct TimelineCategoryCount: Identifiable {
        let category: String
        let count: Int

        var id: String {
            category
        }
    }

    private func categoryCounts(_ entries: [AgentTimelineEntry]) -> [TimelineCategoryCount] {
        var counts: [String: Int] = [:]
        for entry in entries {
            let category = entry.category.isEmpty ? "other" : entry.category.lowercased()
            counts[category, default: 0] += 1
        }
        return counts
            .map { TimelineCategoryCount(category: $0.key, count: $0.value) }
            .sorted { lhs, rhs in
                if lhs.count != rhs.count {
                    return lhs.count > rhs.count
                }
                return lhs.category < rhs.category
            }
    }

    private func timelineLabel(_ entry: AgentTimelineEntry) -> String {
        if let tool = entry.tool, !tool.isEmpty {
            return tool
        }
        if !entry.kind.isEmpty {
            return entry.kind.replacingOccurrences(of: "_", with: " ")
                .replacingOccurrences(of: ".", with: " ")
        }
        return entry.category.isEmpty ? "event" : entry.category
    }

    private func timelineIcon(_ category: String) -> String {
        switch category.lowercased() {
        case "message": "text.bubble"
        case "tool": "wrench.and.screwdriver"
        case "approval": "checkmark.shield"
        case "result": "checkmark.circle"
        case "context": "gauge.with.dots.needle.67percent"
        case "coordination": "arrow.triangle.branch"
        case "merge": "arrow.triangle.merge"
        case "question": "questionmark.bubble"
        default: "circle.dotted"
        }
    }

    private func timelineTint(_ category: String) -> Color {
        switch category.lowercased() {
        case "message": VPTheme.accent
        case "tool": VPTheme.info
        case "approval": VPTheme.warning
        case "result": VPTheme.success
        case "context": VPTheme.plan
        case "coordination": VPTheme.accentDeep
        case "merge": VPTheme.info
        case "question": VPTheme.warning
        default: VPTheme.textTertiary
        }
    }

    // MARK: - Messages

    private var messagesContent: some View {
        VStack(spacing: 0) {
            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    inspectorStateStrip

                    sectionHeader("Operator steering")
                    if steeringRows.isEmpty {
                        emptyInline(
                            L10n.t(
                                "No steering messages. Running Agents receive steering immediately before their next provider request."
                            )
                        )
                    } else {
                        ForEach(steeringRows.prefix(20)) { steering in
                            steeringRow(steering)
                        }
                    }

                    VPHairline()
                    HStack(spacing: 5) {
                        sectionHeader("Agent inbox")
                        Spacer()
                        inboxCountsBadge("unread", inboxCounts.unread, tint: VPTheme.warning)
                        inboxCountsBadge("read", inboxCounts.read, tint: VPTheme.info)
                        inboxCountsBadge("done", inboxCounts.done, tint: VPTheme.success)
                    }
                    if inspector?.inbox.isEmpty != false {
                        if model.collabAgentInspectBusy, inspector == nil {
                            VPSkeletonRows(rows: 2, height: 10)
                        } else {
                            emptyInline(
                                L10n
                                    .t(
                                        "No queued inbox messages. Delivery is pull-based and reading this panel does not mark messages read."
                                    )
                            )
                        }
                    } else if let inbox = inspector?.inbox {
                        ForEach(inbox.prefix(30)) { message in
                            inboxRow(message)
                        }
                    }

                    Text(
                        L10n.t(
                            "Steering is cooperative and cannot interrupt an in-flight model stream or tool call. Inbox messages are queued until the Agent reads them; they are not instant replies and cannot approve an approval or bypass a gate."
                        )
                    )
                    .font(VPTheme.mono(6.4))
                    .foregroundStyle(VPTheme.textTertiary)
                    .fixedSize(horizontal: false, vertical: true)
                }
                .padding(12)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .scrollIndicators(.automatic)

            VPHairline()
            composer
        }
    }

    private func steeringRow(_ steering: SteeringMessage) -> some View {
        VPInsetCard(padding: 8) {
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 5) {
                    Text("STEERING")
                        .font(VPTheme.mono(5.8, weight: .semibold))
                        .tracking(0.3)
                        .foregroundStyle(VPTheme.accent)
                    Text(steering.displayStatus.uppercased())
                        .font(VPTheme.mono(5.8, weight: .semibold))
                        .foregroundStyle(VPTheme.statusColor(steering.status))
                    if let taskID = steering.taskId, !taskID.isEmpty {
                        Text(taskID)
                            .font(VPTheme.mono(5.8))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                    Spacer(minLength: 2)
                    Text(VPTheme.relativeTime(steering.createdAt))
                        .font(VPTheme.mono(5.8))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                Text(steering.body)
                    .font(VPTheme.font(9.5))
                    .foregroundStyle(VPTheme.textPrimary)
                    .lineLimit(3)
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 4) {
                    Text(steeringDetail(steering))
                        .font(VPTheme.mono(6))
                        .foregroundStyle(VPTheme.textTertiary)
                    Spacer(minLength: 0)
                }
            }
        }
    }

    private func steeringDetail(_ steering: SteeringMessage) -> String {
        switch steering.status {
        case "pending":
            L10n.t("Queued · delivered at the next provider boundary.")
        case "delivered":
            if let deliveredAt = steering.deliveredAt {
                L10n.format("Delivered %@", VPTheme.relativeTime(deliveredAt))
            } else {
                L10n.t("Delivered.")
            }
        case "cancelled":
            L10n.t("Cancelled before delivery.")
        default:
            steering.status
        }
    }

    private func inboxRow(_ message: AgentInboxView) -> some View {
        let tint = inboxTint(message.status)
        return VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 5) {
                Text(message.fromLabel.isEmpty ? L10n.t("operator") : message.fromLabel)
                    .font(VPTheme.mono(6.6, weight: .semibold))
                    .foregroundStyle(VPTheme.textSecondary)
                    .lineLimit(1)
                if !message.kind.isEmpty {
                    Text(message.kind.uppercased())
                        .font(VPTheme.mono(5.6, weight: .semibold))
                        .foregroundStyle(VPTheme.accent)
                        .padding(.horizontal, 3)
                        .padding(.vertical, 1)
                        .background(VPTheme.accent.opacity(0.08), in: Capsule())
                }
                Text(message.status.uppercased())
                    .font(VPTheme.mono(5.6, weight: .semibold))
                    .foregroundStyle(tint)
                Spacer(minLength: 2)
                if !message.createdAt.isEmpty {
                    Text(VPTheme.relativeTime(message.createdAt))
                        .font(VPTheme.mono(5.6))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
            Text(message.body.isEmpty ? "…" : message.body)
                .font(VPTheme.font(9.5))
                .foregroundStyle(VPTheme.textPrimary)
                .lineLimit(4)
                .fixedSize(horizontal: false, vertical: true)
            if let readAt = message.readAt, !readAt.isEmpty {
                Text(L10n.format("Read %@", VPTheme.relativeTime(readAt)))
                    .font(VPTheme.mono(5.8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
        }
        .padding(8)
        .background(
            VPTheme.surfaceSunken.opacity(message.isUnread ? 0.82 : 0.55),
            in: RoundedRectangle(cornerRadius: 8, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .strokeBorder(
                    message.isUnread ? VPTheme.warning.opacity(0.24) : VPTheme.hairline,
                    lineWidth: 1
                )
        }
    }

    private func inboxTint(_ status: String) -> Color {
        switch status.lowercased() {
        case "unread": VPTheme.warning
        case "read": VPTheme.info
        case "done": VPTheme.success
        case "expired": VPTheme.textTertiary
        default: VPTheme.statusColor(status)
        }
    }

    private func inboxCountsBadge(_ label: String, _ count: Int, tint: Color) -> some View {
        HStack(spacing: 2) {
            Text("\(count)")
                .font(VPTheme.mono(6, weight: .semibold))
            Text(L10n.t(label).uppercased())
                .font(VPTheme.mono(5.2, weight: .medium))
        }
        .foregroundStyle(count > 0 ? tint : VPTheme.textTertiary)
        .padding(.horizontal, 4)
        .padding(.vertical, 1)
        .background((count > 0 ? tint : VPTheme.textTertiary).opacity(0.08), in: Capsule())
    }

    // MARK: - Composer

    private var composer: some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack(spacing: 5) {
                Text(L10n.t("Direct channel").uppercased())
                    .font(VPTheme.mono(6.4, weight: .semibold))
                    .tracking(0.4)
                    .foregroundStyle(VPTheme.textTertiary)
                Spacer()
                Text(verbatim: "\(composerText.count)/\(messageLimit)")
                    .font(VPTheme.mono(6))
                    .foregroundStyle(
                        composerText.count >= messageLimit ? VPTheme.warning : VPTheme.textTertiary
                    )
            }

            composerModeBanner

            ZStack(alignment: .topLeading) {
                TextEditor(text: $composerText)
                    .font(VPTheme.font(9.5))
                    .foregroundStyle(VPTheme.textPrimary)
                    .scrollContentBackground(.hidden)
                    .padding(5)
                    .frame(minHeight: 66, maxHeight: 108)
                    .background(
                        VPTheme.surfaceSunken.opacity(0.78),
                        in: RoundedRectangle(cornerRadius: 9, style: .continuous)
                    )
                    .overlay {
                        RoundedRectangle(cornerRadius: 9, style: .continuous)
                            .strokeBorder(
                                composerFocused
                                    ? VPTheme.accent.opacity(0.50)
                                    : VPTheme.hairline,
                                lineWidth: composerFocused ? 1.2 : 1
                            )
                    }
                    .focused($composerFocused)
                    .disabled(!canCompose)
                    .opacity(canCompose ? 1 : 0.55)

                if composerText.isEmpty {
                    Text(composerPlaceholder)
                        .font(VPTheme.font(9.5))
                        .foregroundStyle(VPTheme.textTertiary)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 10)
                        .allowsHitTesting(false)
                }
            }

            if composerMode == .inbox {
                Toggle(isOn: $wakeSession) {
                    VStack(alignment: .leading, spacing: 1) {
                        Text(L10n.t("Wake session after queueing"))
                            .font(VPTheme.mono(6.6, weight: .medium))
                            .foregroundStyle(VPTheme.textSecondary)
                        Text(
                            L10n.t(
                                "Creates a deferred continuation only if the session is idle, failed, or cancelled. It does not start a live run."
                            )
                        )
                        .font(VPTheme.mono(5.9))
                        .foregroundStyle(VPTheme.textTertiary)
                        .fixedSize(horizontal: false, vertical: true)
                    }
                }
                .toggleStyle(.switch)
                .controlSize(.mini)
                .tint(VPTheme.accentDeep)
            }

            if messageAttemptAgentID == agentID,
               let error = model.collabAgentMessageError,
               !error.isEmpty
            {
                inlineNotice(error, tint: VPTheme.danger, icon: "exclamationmark.triangle.fill")
            } else if let notice = deliveryNotice, !notice.isEmpty {
                inlineNotice(notice, tint: VPTheme.success, icon: "tray.and.arrow.down")
            }

            if let delivery {
                deliveryRow(delivery)
            }

            HStack(spacing: 7) {
                Text(composerFootnote)
                    .font(VPTheme.mono(5.9))
                    .foregroundStyle(VPTheme.textTertiary)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 4)
                Button {
                    Task { await sendMessage() }
                } label: {
                    HStack(spacing: 4) {
                        if model.collabAgentMessageBusy {
                            ProgressView().controlSize(.mini)
                        } else {
                            Image(systemName: "paperplane.fill")
                                .font(.system(size: 8, weight: .semibold))
                        }
                        Text(L10n.t("Send"))
                            .font(VPTheme.mono(7, weight: .semibold))
                    }
                }
                .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                .controlSize(.small)
                .disabled(!canSend)
                .keyboardShortcut(.return, modifiers: [.command])
                .help(L10n.t("Send the message to this Agent"))
            }
        }
        .padding(12)
        .background(VPTheme.surfaceSunken.opacity(0.42))
    }

    @ViewBuilder
    private var composerModeBanner: some View {
        switch composerMode {
        case .steering:
            inlineNotice(
                L10n.t(
                    "Active run · this is queued as steering and delivered before the Agent's next provider request."
                ),
                tint: VPTheme.accent,
                icon: "bolt.horizontal.circle"
            )
        case .inbox:
            inlineNotice(
                L10n.t(
                    "Idle identity · this is queued in the Agent inbox. It is not an instant reply."
                ),
                tint: VPTheme.info,
                icon: "tray.and.arrow.down"
            )
        case .inactive:
            VStack(alignment: .leading, spacing: 2) {
                Text("AGENT_NOT_ACTIVE")
                    .font(VPTheme.mono(6.8, weight: .semibold))
                    .foregroundStyle(VPTheme.danger)
                Text(
                    L10n.t(
                        "This ephemeral Agent has no live run or project identity. Start or retry its task before messaging; the message is not stored."
                    )
                )
                .font(VPTheme.mono(6.2))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
            }
            .padding(7)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                VPTheme.danger.opacity(0.07),
                in: RoundedRectangle(cornerRadius: 8, style: .continuous)
            )
            .overlay {
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .strokeBorder(VPTheme.danger.opacity(0.20), lineWidth: 1)
            }
        case let .unavailable(reason):
            inlineNotice(reason, tint: VPTheme.textTertiary, icon: "hourglass")
        }
    }

    private var composerPlaceholder: String {
        switch composerMode {
        case .steering:
            L10n.t("Steer the active run…")
        case .inbox:
            L10n.t("Queue a message for this Agent…")
        case .inactive:
            L10n.t("Messaging is unavailable for this Agent.")
        case .unavailable:
            L10n.t("Waiting for the Agent inspector…")
        }
    }

    private var composerFootnote: String {
        switch composerMode {
        case .steering:
            L10n.t("Delivery is cooperative and cannot interrupt an in-flight stream or tool call.")
        case .inbox:
            L10n.t("Pull-based delivery. No instant reply is implied.")
        case .inactive, .unavailable:
            L10n.t("A direct message cannot approve an approval or bypass a gate.")
        }
    }

    private func deliveryRow(_ delivery: AgentMessageDelivery) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 5) {
                Text(delivery.kind.isEmpty ? "delivery" : delivery.kind)
                    .font(VPTheme.mono(6.2, weight: .semibold))
                    .foregroundStyle(VPTheme.textSecondary)
                if !delivery.status.isEmpty {
                    Text(delivery.status.uppercased())
                        .font(VPTheme.mono(5.8, weight: .semibold))
                        .foregroundStyle(VPTheme.statusColor(delivery.status))
                }
                Spacer(minLength: 2)
                if let identityID = delivery.identityID, !identityID.isEmpty {
                    Text(String(identityID.prefix(12)))
                        .font(VPTheme.mono(5.6))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
            if let wake = delivery.wake {
                Text(wakeText(wake))
                    .font(VPTheme.mono(5.9))
                    .foregroundStyle(VPTheme.textTertiary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let steering = delivery.steering {
                Text(L10n.format("Steering status: %@", steering.displayStatus))
                    .font(VPTheme.mono(5.9))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            if let inbox = delivery.inbox {
                Text(L10n.format("Inbox status: %@", inbox.status))
                    .font(VPTheme.mono(5.9))
                    .foregroundStyle(VPTheme.textTertiary)
            }
        }
        .padding(7)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            VPTheme.surfaceSunken.opacity(0.62),
            in: RoundedRectangle(cornerRadius: 8, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
    }

    private func wakeText(_ wake: AgentCoordinationWake) -> String {
        if wake.created {
            return L10n.format(
                "Wake: continuation created (%@).",
                wake.reason.isEmpty ? "requested" : wake.reason
            )
        }
        if wake.requested {
            return L10n.format(
                "Wake requested but not created (%@).",
                wake.reason.isEmpty ? "not eligible" : wake.reason
            )
        }
        return L10n.t("Wake: not requested.")
    }

    private func sendMessage() async {
        let body = trimmedComposerText
        guard !body.isEmpty else { return }
        let targetTaskID = inspector?.task?.taskID ?? node.taskID ?? task?.taskId
        messageAttemptAgentID = agentID
        await model.sendCollabAgentMessage(
            agentID: agentID,
            body: body,
            taskID: targetTaskID,
            wakeSession: wakeSession && composerMode == .inbox
        )
        // AppModel refreshes inspect and steering after a successful send and
        // surfaces typed failures. The view never synthesizes a pending state.
        if model.collabAgentMessageError == nil,
           model.collabAgentMessageDelivery != nil
        {
            composerText = ""
            composerFocused = false
        }
    }

    private func refresh() async {
        await model.loadCollabAgentInspect(agentID: agentID)
        await model.loadSteering()
    }

    // MARK: - Shared pieces

    @ViewBuilder
    private var inspectorStateStrip: some View {
        if model.collabAgentInspectBusy, inspector == nil {
            HStack(spacing: 6) {
                ProgressView().controlSize(.mini)
                Text(L10n.t("Loading the Agent inspector…"))
                    .font(VPTheme.mono(6.8))
                    .foregroundStyle(VPTheme.textSecondary)
            }
        } else if let error = model.collabAgentInspectError, !error.isEmpty {
            inlineNotice(error, tint: VPTheme.danger, icon: "exclamationmark.triangle.fill")
        } else if inspector == nil {
            inlineNotice(
                L10n.t("No inspector projection is loaded."),
                tint: VPTheme.textTertiary,
                icon: "info.circle"
            )
        }
    }

    private var inspectorLoadingState: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 7) {
                    ProgressView().controlSize(.small)
                    Text(L10n.t("Loading the Agent inspector…"))
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textSecondary)
                }
                VPSkeletonRows(rows: 4, height: 10)
            }
            .padding(12)
        }
    }

    private func inspectorErrorState(_ error: String) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 9) {
                inlineNotice(error, tint: VPTheme.danger, icon: "exclamationmark.triangle.fill")
                Button {
                    Task { await refresh() }
                } label: {
                    Label(L10n.t("Retry"), systemImage: "arrow.clockwise")
                        .font(VPTheme.mono(7.5, weight: .medium))
                }
                .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                .controlSize(.small)
                Text(
                    L10n.t(
                        "The inspector could not be loaded. The graph remains usable; close the panel to return to the team tree."
                    )
                )
                .font(VPTheme.mono(6.6))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
            }
            .padding(12)
        }
    }

    private var inspectorEmptyState: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 9) {
                emptyBlock(
                    L10n.t("No inspector data yet."),
                    detail: L10n.t("Refresh to request the bounded Agent projection.")
                )
                Button {
                    Task { await refresh() }
                } label: {
                    Label(L10n.t("Refresh"), systemImage: "arrow.clockwise")
                        .font(VPTheme.mono(7.5, weight: .medium))
                }
                .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                .controlSize(.small)
            }
            .padding(12)
        }
    }

    private func sectionHeader(_ title: String) -> some View {
        Text(L10n.t(title).uppercased())
            .font(VPTheme.mono(6.3, weight: .semibold))
            .tracking(0.5)
            .foregroundStyle(VPTheme.textTertiary)
    }

    private func inlineNotice(_ text: String, tint: Color, icon: String) -> some View {
        HStack(alignment: .top, spacing: 5) {
            Image(systemName: icon)
                .font(.system(size: 7, weight: .semibold))
                .foregroundStyle(tint)
                .padding(.top, 1)
            Text(text)
                .font(VPTheme.mono(6.4))
                .foregroundStyle(VPTheme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
        }
        .padding(7)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(tint.opacity(0.07), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .strokeBorder(tint.opacity(0.20), lineWidth: 1)
        }
    }

    private func emptyInline(_ text: String) -> some View {
        Text(text)
            .font(VPTheme.mono(6.6))
            .foregroundStyle(VPTheme.textTertiary)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(8)
            .background(
                VPTheme.surfaceSunken.opacity(0.48),
                in: RoundedRectangle(cornerRadius: 8, style: .continuous)
            )
    }

    private func emptyBlock(_ title: String, detail: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title)
                .font(VPTheme.font(10, weight: .semibold))
                .foregroundStyle(VPTheme.textSecondary)
            Text(detail)
                .font(VPTheme.mono(6.6))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(9)
        .background(
            VPTheme.surfaceSunken.opacity(0.52),
            in: RoundedRectangle(cornerRadius: 9, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
    }

    private func tokenText(_ value: Int) -> String {
        if value >= 1_000_000 {
            return String(format: "%.1fM", Double(value) / 1_000_000)
        }
        if value >= 10000 {
            return String(format: "%.1fk", Double(value) / 1000)
        }
        return "\(value)"
    }

    private func moneyText(_ value: Double) -> String {
        if value > 0, value < 0.01 {
            return String(format: "$%.4f", value)
        }
        return String(format: "$%.2f", value)
    }
}

enum CollabInspectorComposerMode: Equatable {
    case steering
    case inbox
    case inactive
    case unavailable(String)

    /// The daemon accepts steering only while the run is active. Paused,
    /// queued, cancelling, and verification-only states must route through the
    /// identity inbox or the typed not-active state.
    static func canSteer(status: String) -> Bool {
        ["running", "waiting_input"].contains(status.lowercased())
    }

    /// Delivery routing shown to the operator. The daemon remains authoritative
    /// and can still reject a send with a typed error; this only prevents the UI
    /// from presenting an idle ephemeral Agent as messageable.
    static func resolve(
        isActiveRun: Bool,
        identityID: String?,
        hasAgent: Bool
    ) -> CollabInspectorComposerMode {
        guard hasAgent else {
            return .unavailable(L10n.t("No live Agent record is loaded."))
        }
        if isActiveRun {
            return .steering
        }
        if let identityID, !identityID.isEmpty {
            return .inbox
        }
        return .inactive
    }
}

enum CollabAgentInspectorTab: String, CaseIterable, Hashable, Identifiable {
    case overview
    case context
    case conversation
    case messages
    case activity

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .overview: L10n.t("Overview")
        case .context: L10n.t("Context")
        case .conversation: L10n.t("Conversation")
        case .messages: L10n.t("Messages")
        case .activity: L10n.t("Activity")
        }
    }
}

/// A bounded usage meter that honors Reduce Motion and never implies provider
/// token accounting beyond the daemon-reported estimate.
private struct CollabInspectorMeter: View {
    let used: Int
    let window: Int
    let threshold: Int
    let hardLimit: Int

    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var fraction: Double {
        guard window > 0 else { return 0 }
        return min(1, max(0, Double(used) / Double(window)))
    }

    private var tint: Color {
        if hardLimit > 0, used >= hardLimit {
            return VPTheme.danger
        }
        if threshold > 0, used >= threshold {
            return VPTheme.warning
        }
        return VPTheme.accent
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            GeometryReader { proxy in
                ZStack(alignment: .leading) {
                    Capsule().fill(VPTheme.hairline)
                    Capsule()
                        .fill(
                            LinearGradient(
                                colors: [tint.opacity(0.55), tint],
                                startPoint: .leading,
                                endPoint: .trailing
                            )
                        )
                        .frame(width: max(fraction > 0 ? 3 : 0, proxy.size.width * fraction))
                }
            }
            .frame(height: 5)

            HStack(spacing: 4) {
                Text(fractionText)
                    .font(VPTheme.mono(7, weight: .semibold))
                    .foregroundStyle(tint)
                Text(L10n.t("of window"))
                    .font(VPTheme.mono(5.9))
                    .foregroundStyle(VPTheme.textTertiary)
                Spacer()
                if threshold > 0 {
                    Text(L10n.format("compacts at %@", compactTokens(threshold)))
                        .font(VPTheme.mono(5.8))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
        }
        .animation(VPMotion.resolved(VPMotion.smooth, reduceMotion: reduceMotion), value: fraction)
    }

    private var fractionText: String {
        guard window > 0 else {
            return used > 0 ? compactTokens(used) : "—"
        }
        return "\(compactTokens(used)) · \(Int((fraction * 100).rounded()))%"
    }

    private func compactTokens(_ value: Int) -> String {
        if value >= 1_000_000 {
            return String(format: "%.1fM", Double(value) / 1_000_000)
        }
        if value >= 10000 {
            return String(format: "%.1fk", Double(value) / 1000)
        }
        return "\(value)"
    }
}
