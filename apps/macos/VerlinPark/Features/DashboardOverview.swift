import SwiftUI

struct DashboardOverviewView: View {
    @Environment(AppModel.self) private var model
    @Environment(MemoryMonitor.self) private var memory
    @Environment(BrandStore.self) private var brand
    @Binding var section: DashboardSection

    private var runningTasks: Int {
        model.taskQueue?.tasks.count(where: { $0.status == "running" }) ?? 0
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
                value: "\(model.projects.count)",
                detail: "\(model.sessions.count) sessions · \(model.events.count) events"
            )
            metric(
                "Inbox",
                value: "\(model.pendingApprovals.count + model.pendingQuestions.count)",
                detail: "\(model.pendingApprovals.count) approvals · \(model.pendingQuestions.count) questions"
            )
            metric(
                "Scheduler",
                value: "\(model.taskQueue?.tasks.count ?? 0)",
                detail: "\(runningTasks) running · \(model.taskQueue?.leases.count ?? 0) leases"
            )
            metric(
                "Checkpoints",
                value: "\(model.checkpoints.count)",
                detail: "\(model.projectForkForest?.checkpoints ?? 0) project checkpoints"
            )
            metric(
                "Fork roots",
                value: "\(model.projectForkForest?.roots.count ?? 0)",
                detail: "\(model.projectForkForest?.total ?? 0) sessions in forest"
            )
            metric(
                "Capacity",
                value: capacityLabel,
                detail: "global workers / labour availability"
            )
            metric(
                "Memory",
                value: memory.text,
                detail: "app + daemon resident"
            )
            metric(
                "Brand",
                value: brand.config.effectiveName,
                detail: "\(brand.config.style.title) mark"
            )
        }

        DashboardPendingActions(section: $section)

        DashboardRuntimeCard()

        DashboardRecentActivityCard(section: $section)
    }

    private var capacityLabel: String {
        guard let capacity = model.agentInfo?.capacity else { return "—" }
        return "\(capacity.workerAvailable) / \(capacity.labourAvailable)"
    }

    private func metric(_ title: String, value: String, detail: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            SectionLabel(title: title)
            Text(value)
                .font(.system(size: 22, weight: .semibold, design: .rounded))
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
    }
}

private struct DashboardPendingActions: View {
    @Environment(AppModel.self) private var model
    @Binding var section: DashboardSection

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionLabel(title: "Pending actions")
            HStack(spacing: 10) {
                action(
                    title: "Approvals",
                    count: model.pendingApprovals.count,
                    icon: "checkmark.shield",
                    color: .orange,
                    destination: .inbox
                )
                action(
                    title: "Questions",
                    count: model.pendingQuestions.count,
                    icon: "questionmark.bubble",
                    color: .cyan,
                    destination: .inbox
                )
                action(
                    title: "Running tasks",
                    count: model.taskQueue?.tasks.count(where: { $0.status == "running" }) ?? 0,
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
                    Text("\(count)")
                        .font(.system(size: 16, weight: .semibold, design: .rounded))
                }
                Text(title)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
            .padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 9))
        }
        .buttonStyle(.plain)
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
                    Text(model.connection == .connected ? "Daemon connected" : "Daemon offline")
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if let info = model.agentInfo {
                    Text(info.configured ? (info.model ?? "model") : "no model")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(info.configured ? Color.secondary : Color.orange)
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
