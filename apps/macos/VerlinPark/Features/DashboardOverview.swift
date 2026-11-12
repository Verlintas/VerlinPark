import SwiftUI

struct DashboardOverviewView: View {
    @Environment(AppModel.self) private var model
    @Environment(MemoryMonitor.self) private var memory
    @Environment(BrandStore.self) private var brand
    @Binding var section: DashboardSection

    private var summary: DashboardSummary? {
        model.dashboardSnapshot?.summary
    }

    private var runningTasks: Int {
        summary?.runningTasks
            ?? model.taskQueue?.tasks.count(where: { $0.status == "running" })
            ?? 0
    }

    var body: some View {
        LazyVGrid(
            columns: [
                GridItem(.flexible(), spacing: 12),
                GridItem(.flexible(), spacing: 12),
                GridItem(.flexible(), spacing: 12),
                GridItem(.flexible(), spacing: 12),
            ],
            spacing: 12
        ) {
            metric(
                "Workspace",
                value: "\(summary?.projects ?? model.projects.count)",
                detail: workspaceDetail,
                index: 0
            )
            metric(
                "Inbox",
                value: "\(inboxCount)",
                detail: "\(approvalCount) approvals · \(questionCount) questions",
                index: 1
            )
            metric(
                "Scheduler",
                value: "\(taskCount)",
                detail: "\(runningTasks) running · \(leaseCount) leases",
                index: 2
            )
            metric(
                "Checkpoints",
                value: "\(checkpointCount)",
                detail: "\(dirtyCheckpointCount) dirty · \(model.checkpoints.count) current session",
                index: 3
            )
            metric(
                "Fork roots",
                value: "\(forkRootCount)",
                detail: "\(forkSessionCount) forked sessions",
                index: 4
            )
            metric(
                "Capacity",
                value: capacityLabel,
                detail: "global workers / labour availability",
                index: 5
            )
            metric(
                "Memory",
                value: memory.text,
                detail: "app + daemon resident",
                index: 6
            )
            metric(
                "Brand",
                value: brand.config.effectiveName,
                detail: "\(brand.config.style.title) mark",
                index: 7
            )
            metric(
                "Preemptions",
                value: "\(preemptionCount)",
                detail: preemptionDetail,
                index: 8
            )
        }

        DashboardPendingActions(section: $section)

        DashboardAttentionCard(section: $section)

        if model.selectedProject == nil {
            DashboardProjectBreakdownCard()
        }

        DashboardRuntimeCard()

        DashboardRecentActivityCard(section: $section)
    }

    private var preemptionCount: Int {
        if let overview = model.preemptionOverview {
            return Int(overview.total)
        }
        return model.sessionPreemption?.records.count(where: { $0.terminal }) ?? 0
    }

    private var preemptionDetail: String {
        if let overview = model.preemptionOverview, let reason = overview.byReason.first {
            return "\(overview.sessions.count) sessions · \(reason.key) \(reason.count)"
        }
        guard let preemption = model.sessionPreemption, let last = preemption.records.last else {
            return "no hard preemptions"
        }
        return "\(last.reason) · \(last.phase)"
    }

    private var approvalCount: Int {
        summary?.pendingApprovals ?? model.pendingApprovals.count
    }

    private var questionCount: Int {
        summary?.pendingQuestions ?? model.pendingQuestions.count
    }

    private var inboxCount: Int {
        approvalCount + questionCount
    }

    private var taskCount: Int {
        summary?.tasks ?? model.taskQueue?.tasks.count ?? 0
    }

    private var leaseCount: Int {
        summary?.writeLeases ?? model.taskQueue?.leases.count ?? 0
    }

    private var checkpointCount: Int {
        summary?.checkpoints ?? model.projectForkForest?.checkpoints ?? model.checkpoints.count
    }

    private var dirtyCheckpointCount: Int {
        summary?.dirtyCheckpoints ?? model.projectForkForest?.dirtyCheckpoints ?? 0
    }

    private var forkRootCount: Int {
        summary?.forkRoots ?? model.projectForkForest?.roots.count ?? 0
    }

    private var forkSessionCount: Int {
        summary?.forkSessions ?? model.projectForkForest?.total ?? 0
    }

    private var workspaceDetail: String {
        guard let summary else {
            return "\(model.sessions.count) sessions · \(model.events.count) events"
        }
        return "\(summary.sessions) sessions · \(summary.runningSessions) running · \(summary.queuedSessions) queued"
    }

    private var capacityLabel: String {
        guard let capacity = (summary?.agent ?? model.agentInfo)?.capacity else { return "—" }
        return "\(capacity.workerAvailable) / \(capacity.labourAvailable)"
    }

    private func metric(
        _ title: String,
        value: String,
        detail: String,
        index: Int
    ) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            SectionLabel(title: title)
            VPAnimatedNumberText(value: value, size: 22)
                .lineLimit(1)
            Text(detail)
                .font(.system(size: 9, design: .monospaced))
                .foregroundStyle(.secondary)
                .lineLimit(2)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(13)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 13, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 13, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.006, lift: 1)
        .vpAppear(index: index)
    }
}

private struct DashboardPendingActions: View {
    @Environment(AppModel.self) private var model
    @Binding var section: DashboardSection

    private var summary: DashboardSummary? {
        model.dashboardSnapshot?.summary
    }

    private var approvalCount: Int {
        summary?.pendingApprovals ?? model.pendingApprovals.count
    }

    private var questionCount: Int {
        summary?.pendingQuestions ?? model.pendingQuestions.count
    }

    private var runningTasks: Int {
        summary?.runningTasks
            ?? model.taskQueue?.tasks.count(where: { $0.status == "running" })
            ?? 0
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionLabel(title: "Pending actions")
            HStack(spacing: 10) {
                action(
                    title: "Approvals",
                    count: approvalCount,
                    icon: "checkmark.shield",
                    color: .orange,
                    destination: .inbox
                )
                action(
                    title: "Questions",
                    count: questionCount,
                    icon: "questionmark.bubble",
                    color: .cyan,
                    destination: .inbox
                )
                action(
                    title: "Running tasks",
                    count: runningTasks,
                    icon: "gearshape.2",
                    color: .green,
                    destination: .team
                )
                action(
                    title: "Fork issues",
                    count: model.forkAudit?.issues.count ?? 0,
                    icon: "arrow.triangle.branch",
                    color: .orange,
                    destination: .git
                )
            }
        }
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .vpAppear(index: 8)
    }

    private func action(
        title: String,
        count: Int,
        icon: String,
        color: Color,
        destination: DashboardSection
    ) -> some View {
        Button {
            section = destination
        } label: {
            VStack(alignment: .leading, spacing: 4) {
                HStack {
                    Image(systemName: icon)
                        .foregroundStyle(color)
                    Spacer()
                    VPAnimatedNumberText(value: "\(count)", size: 16)
                }
                Text(title)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
            .padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 9))
        }
        .buttonStyle(VPPressableButtonStyle(scale: 0.97))
    }
}

private struct DashboardAttentionCard: View {
    @Environment(AppModel.self) private var model
    @Binding var section: DashboardSection

    private var alerts: [DashboardAlert] {
        Array((model.dashboardSnapshot?.alerts ?? []).prefix(8))
    }

    private var totalAlerts: Int {
        model.dashboardSnapshot?.alerts.count ?? 0
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Needs attention")
                    Text(model.dashboardStatus ?? "Alerts are derived from live daemon state.")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
                Spacer()
                if model.dashboardBusy {
                    ProgressView()
                        .controlSize(.small)
                }
            }
            if alerts.isEmpty {
                Text("No failed sessions, blocked tasks, pending approvals, questions, or merge conflicts.")
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.tertiary)
            } else {
                VStack(spacing: 5) {
                    ForEach(alerts) { alert in
                        row(alert)
                    }
                }
            }
            if totalAlerts > alerts.count || model.dashboardSnapshot?.alertsTruncated == true {
                Text(
                    "Showing \(alerts.count) of \(totalAlerts) alerts\(model.dashboardSnapshot?.alertsTruncated == true ? " (response capped)" : "")."
                )
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
            }
        }
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .vpAppear(index: 9)
    }

    private func row(_ alert: DashboardAlert) -> some View {
        HStack(alignment: .top, spacing: 9) {
            Image(systemName: icon(for: alert.severity))
                .font(.system(size: 10))
                .foregroundStyle(color(for: alert.severity))
                .frame(width: 14)
            VStack(alignment: .leading, spacing: 2) {
                Text(alert.title)
                    .font(.system(size: 10, weight: .medium, design: .monospaced))
                    .lineLimit(1)
                Text(alert.detail)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
            Spacer(minLength: 8)
            Text(alert.kind.replacingOccurrences(of: "_", with: " "))
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
            if destination(for: alert.action) != nil {
                Button("Open") {
                    open(alert)
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
            }
        }
        .padding(8)
        .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 8))
        .vpHoverHighlight(radius: 8)
    }

    private func open(_ alert: DashboardAlert) {
        if let projectID = alert.projectID, projectID != model.selectedProjectID {
            model.selectProject(projectID)
        }
        if let sessionID = alert.sessionID {
            model.selectSession(sessionID)
        }
        if let destination = destination(for: alert.action) {
            section = destination
        }
    }

    private func destination(for action: String) -> DashboardSection? {
        switch action {
        case "inbox":
            .inbox
        case "team", "session":
            .team
        case "git":
            .git
        case "usage":
            .usage
        case "diagnostics":
            .diagnostics
        default:
            nil
        }
    }

    private func icon(for severity: String) -> String {
        switch severity {
        case "critical":
            "exclamationmark.octagon.fill"
        case "warning":
            "exclamationmark.triangle.fill"
        default:
            "info.circle.fill"
        }
    }

    private func color(for severity: String) -> Color {
        switch severity {
        case "critical":
            .red
        case "warning":
            .orange
        default:
            .blue
        }
    }
}

private struct DashboardProjectBreakdownCard: View {
    @Environment(AppModel.self) private var model

    private var summaries: [DashboardProjectSummary] {
        Array((model.dashboardSnapshot?.summary.projectSummaries ?? []).prefix(8))
    }

    private var total: Int {
        model.dashboardSnapshot?.summary.projectSummaries.count ?? 0
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Projects")
                    Text("Activity across every local project")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Text("\(total)")
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
            if summaries.isEmpty {
                Text("No projects registered.")
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.tertiary)
            } else {
                VStack(spacing: 5) {
                    ForEach(summaries) { summary in
                        row(summary)
                    }
                }
                if total > summaries.count {
                    Text("Showing \(summaries.count) of \(total) projects with the most activity.")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
            }
        }
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .vpAppear(index: 10)
    }

    private func row(_ summary: DashboardProjectSummary) -> some View {
        HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(summary.name)
                        .font(.system(size: 10, weight: .medium, design: .monospaced))
                        .lineLimit(1)
                    if summary.failedSessions > 0 {
                        Text("\(summary.failedSessions) failed")
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.red)
                    }
                }
                Text(
                    "\(summary.sessions) sessions · \(summary.runningSessions) running · \(summary.queuedSessions) queued"
                )
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.secondary)
            }
            Spacer(minLength: 6)
            stat(
                "checkmark.shield",
                summary.pendingApprovals + summary.pendingQuestions,
                color: .orange
            )
            stat("gearshape.2", summary.tasks, color: .secondary)
            stat("camera", summary.checkpoints, color: .cyan)
            stat("arrow.triangle.branch", summary.forkRoots, color: .purple)
            Button("Open") {
                model.selectProject(summary.projectID)
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
        }
        .padding(8)
        .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 8))
        .vpHoverHighlight(radius: 8)
    }

    private func stat(_ symbol: String, _ value: Int, color: Color) -> some View {
        HStack(spacing: 2) {
            Image(systemName: symbol)
                .font(.system(size: 8))
                .foregroundStyle(color)
            VPAnimatedNumberText(value: "\(value)", size: 8, weight: .medium, design: .monospaced)
                .foregroundStyle(.secondary)
        }
        .font(.system(size: 8, weight: .medium, design: .monospaced))
    }
}

private struct DashboardRuntimeCard: View {
    @Environment(AppModel.self) private var model
    @Environment(MemoryMonitor.self) private var memory

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Runtime")
                    HStack(spacing: 6) {
                        VPStatusDot(
                            color: model.connection == .connected ? .green : .orange,
                            pulsing: model.connection == .connecting,
                            size: 6
                        )
                        Text(model.connection == .connected ? "Daemon connected" : "Daemon offline")
                            .font(.system(size: 13, weight: .semibold, design: .rounded))
                            .contentTransition(.opacity)
                    }
                }
                Spacer()
                if let info = model.agentInfo {
                    Text(info.configured ? (model.daemonModelLabel ?? "model") : "no model")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(info.configured ? Color.secondary : Color.orange)
                        .help(
                            "Daemon-configured Leader model; local provider settings are not used for a live run."
                        )
                }
            }
            Divider().opacity(0.2)
            Grid(alignment: .leading, horizontalSpacing: 18, verticalSpacing: 6) {
                GridRow {
                    LabeledContent("Project", value: model.selectedProject?.name ?? "none")
                    LabeledContent("Session", value: model.selectedSession?.status ?? "none")
                }
                GridRow {
                    LabeledContent("Provider", value: model.agentInfo?.provider ?? "not configured")
                    LabeledContent("Memory", value: memory.text)
                }
                GridRow {
                    LabeledContent("Workers", value: capacity("worker"))
                    LabeledContent("Labour", value: capacity("labour"))
                }
            }
            .font(.system(size: 10, design: .monospaced))
        }
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .vpAppear(index: 11)
    }

    private func capacity(_ kind: String) -> String {
        guard let capacity = model.agentInfo?.capacity else { return "—" }
        if kind == "worker" {
            return "\(capacity.workerAvailable) free / \(capacity.workerLimit)"
        }
        return "\(capacity.labourAvailable) free / \(capacity.labourLimit)"
    }
}

struct DashboardNoProjectCard: View {
    let detail: String

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            SectionLabel(title: "Project required")
            Text(detail)
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .vpAppear(index: 12)
    }
}

private struct DashboardRecentActivityCard: View {
    @Environment(AppModel.self) private var model
    @Binding var section: DashboardSection

    private var events: [Event] {
        Array(model.events.suffix(10).reversed())
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack {
                SectionLabel(title: "Recent activity")
                Spacer()
                Button("Open Trace") { section = .diagnostics }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
            }
            if events.isEmpty {
                Text("No events recorded for this session.")
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.tertiary)
            } else {
                VStack(spacing: 4) {
                    ForEach(events) { event in
                        HStack(alignment: .top, spacing: 8) {
                            Text(String(event.createdAt.prefix(19)))
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(.tertiary)
                                .frame(width: 118, alignment: .leading)
                            Text(event.type)
                                .font(.system(size: 9, weight: .medium, design: .monospaced))
                                .foregroundStyle(color(for: event.type))
                                .frame(width: 130, alignment: .leading)
                            Text(summary(event))
                                .font(.system(size: 9, design: .monospaced))
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                            Spacer(minLength: 0)
                        }
                        .padding(.vertical, 2)
                        .vpHoverHighlight(radius: 6)
                    }
                }
            }
        }
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .vpAppear(index: 13)
    }

    private func summary(_ event: Event) -> String {
        let payload = event.payload.objectValue
        if let text = payload?["text"]?.stringValue {
            return text
        }
        if let goal = payload?["goal"]?.stringValue {
            return goal
        }
        if let detail = payload?["detail"]?.stringValue {
            return detail
        }
        if let prompt = payload?["prompt"]?.stringValue {
            return prompt
        }
        if let status = payload?["status"]?.stringValue {
            return status
        }
        return event.payload.shortDescription
    }

    private func color(for type: String) -> Color {
        if type.contains("failed") || type.contains("cancelled") {
            return .red
        }
        if type.contains("budget") || type.contains("conflict") {
            return .orange
        }
        if type.contains("finished") || type.contains("merged") || type.contains("completed") {
            return .green
        }
        return .secondary
    }
}
