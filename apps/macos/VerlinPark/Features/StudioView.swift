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
        case .collab: "Collab"
        case .preview: "Preview"
        }
    }
}

struct StudioView: View {
    @Environment(AppModel.self) private var model
    @Environment(BrandStore.self) private var brand
    @Binding var tab: StudioTab
    @State private var showingOrgEditor = false

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider().opacity(0.35)

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
            .frame(maxWidth: .infinity, maxHeight: .infinity)

            if model.mode == .project, tab == .collab {
                Divider().opacity(0.35)
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
                    .buttonStyle(.plain)
                    .foregroundStyle(.secondary)
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 9)
            }
        }
        .background(.ultraThinMaterial.opacity(0.55))
        .sheet(isPresented: $showingOrgEditor) {
            OrgPresetSheet()
                .environment(model)
        }
    }

    private var header: some View {
        HStack(spacing: 8) {
            Picker("", selection: $tab) {
                ForEach(StudioTab.allCases) { tab in
                    Text(tab.title).tag(tab)
                }
            }
            .pickerStyle(.segmented)
            .labelsHidden()

            if model.mode == .project {
                Text(model.selectedSession == nil ? "no session" : "live")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
    }
}

struct CollabGraphView: View {
    @Environment(AppModel.self) private var model
    @Environment(BrandStore.self) private var brand
    @State private var showComputerQueue = false

    var body: some View {
        ScrollView {
            VStack(spacing: 0) {
                OrgNodeCard(
                    role: "LEADER",
                    name: "Leader",
                    status: model.selectedSession?.status ?? "idle",
                    task: model.selectedSession?.goal ?? "Waiting for a session",
                    model: model.agents.first { $0.role == "leader" }?.model ?? "manual",
                    accent: .green,
                    isPlaceholder: false
                )

                if let plan = model.teamPlan {
                    OrgConnector()
                    TeamPlanCard(plan: plan)
                }

                if !model.todos.isEmpty {
                    OrgConnector()
                    LeaderPlanCard(todos: model.todos)
                }

                OrgConnector()

                AgentTierCard(
                    title: "Managers",
                    range: "1–5",
                    action: "delegate",
                    detail: "split the objective · assign Workers · review",
                    icon: "person.2",
                    agents: model.agents.filter { $0.role == "manager" }
                )

                OrgConnector()

                AgentTierCard(
                    title: "Workers",
                    range: "1–10",
                    action: "execute",
                    detail: "implement · test · report to Manager",
                    icon: "person.3",
                    agents: model.agents.filter { $0.role == "worker" }
                )

                OrgConnector()

                AgentTierCard(
                    title: "Verifiers",
                    range: "1–100",
                    action: "gate",
                    detail: "inspect diff · run tests · approve or reject",
                    icon: "checkmark.shield",
                    agents: model.agents.filter { $0.role == "verifier" }
                )

                OrgConnector()

                AgentTierCard(
                    title: "Labour",
                    range: "1–40",
                    action: "assist",
                    detail: "research · small edits · report to Worker",
                    icon: "hammer",
                    agents: model.agents.filter { $0.role == "labour" }
                )

                Divider()
                    .opacity(0.3)
                    .padding(.vertical, 14)

                computerLeaseCard
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 12)
        }
    }

    private var computerLeaseCard: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Label("Computer lease", systemImage: "display")
                    .font(.system(size: 10, weight: .semibold, design: .monospaced))
                Spacer()
                Text("planned")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
            Text(
                "GUI/browser serialization is design-only; shell and worktree writes already run in parallel."
            )
            .font(.system(size: 10))
            .foregroundStyle(.secondary)
            HStack(spacing: 6) {
                Circle().fill(.green).frame(width: 5, height: 5)
                Text("not wired · 0 queued")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
                Spacer()
                Button(showComputerQueue ? "Hide" : "Details") {
                    showComputerQueue.toggle()
                }
                .buttonStyle(.plain)
                .font(.system(size: 9))
                .foregroundStyle(.secondary)
            }
        }
        .padding(10)
        .background(.black.opacity(0.12), in: RoundedRectangle(cornerRadius: 11, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 11, style: .continuous)
                .strokeBorder(.white.opacity(0.05), lineWidth: 1)
        }
    }
}

private func planBudgetLabel(tokens: Int?, cost: Double?) -> String? {
    var parts: [String] = []
    if let tokens {
        parts.append("\(tokens) tok")
    }
    if let cost {
        parts.append(String(format: "$%.2f", cost))
    }
    return parts.isEmpty ? nil : parts.joined(separator: " / ")
}

private struct PlanTaskRow: View {
    let task: PlanTask
    let status: String

    var body: some View {
        HStack(alignment: .top, spacing: 7) {
            Image(systemName: icon)
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(color)
                .frame(width: 12)
                .padding(.top, 1)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 5) {
                    Text(task.id)
                        .font(.system(size: 8, weight: .medium, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    Text(task.title)
                        .font(.system(size: 10))
                        .lineLimit(2)
                }
                HStack(spacing: 5) {
                    Text(task.department)
                    if let profile = task.profile {
                        Text("· \(profile)")
                    }
                    if !task.deps.isEmpty {
                        Text("· deps \(task.deps.joined(separator: ","))")
                    }
                    if let budget = planBudgetLabel(
                        tokens: task.tokenBudget,
                        cost: task.costBudgetUsd
                    ) {
                        Text("· \(budget)")
                    }
                }
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
            }
            Spacer(minLength: 0)
            Text(status)
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(color)
        }
        .padding(7)
        .background(.white.opacity(0.025), in: RoundedRectangle(cornerRadius: 8))
    }

    private var icon: String {
        switch status {
        case "running": "circle.lefthalf.filled"
        case "completed": "checkmark.circle.fill"
        case "failed", "blocked", "cancelled": "xmark.octagon"
        default: "circle"
        }
    }

    private var color: Color {
        switch status {
        case "running": .orange
        case "completed": .green
        case "failed", "blocked", "cancelled": .red
        default: .secondary
        }
    }
}

struct TeamPlanCard: View {
    @Environment(AppModel.self) private var model
    let plan: TeamPlan

    private var completed: Int {
        plan.tasks.count { model.planTaskStatuses[$0.id] == "completed" }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("TEAM PLAN")
                    .font(.system(size: 9, weight: .semibold, design: .monospaced))
                    .foregroundStyle(.secondary)
                Spacer()
                Text("\(completed)/\(plan.tasks.count) tasks · \(plan.departments.count) dept")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
            Text(plan.goal)
                .font(.system(size: 10, weight: .medium))
                .lineLimit(2)
            ForEach(plan.departments) { department in
                HStack(alignment: .top, spacing: 6) {
                    Circle()
                        .fill(.cyan.opacity(0.7))
                        .frame(width: 5, height: 5)
                        .padding(.top, 4)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(department.managerTitle)
                            .font(.system(size: 10, weight: .medium))
                        Text("\(department.name) · \(department.profile)")
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.tertiary)
                    }
                    Spacer(minLength: 0)
                }
            }
        }
        .padding(10)
        .background(.cyan.opacity(0.05), in: RoundedRectangle(cornerRadius: 10))
        .overlay {
            RoundedRectangle(cornerRadius: 10)
                .strokeBorder(.cyan.opacity(0.15), lineWidth: 1)
        }
    }
}

struct LeaderPlanCard: View {
    let todos: [TodoItem]

    private var completed: Int {
        todos.count { $0.status == "completed" }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("LEADER PLAN")
                    .font(.system(size: 9, weight: .semibold, design: .monospaced))
                    .foregroundStyle(.secondary)
                Spacer()
                Text("\(completed)/\(todos.count)")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
            ForEach(todos) { todo in
                HStack(alignment: .top, spacing: 7) {
                    Image(systemName: icon(for: todo.status))
                        .font(.system(size: 9, weight: .semibold))
                        .foregroundStyle(color(for: todo.status))
                        .frame(width: 12)
                    Text(todo.title)
                        .font(.system(size: 10))
                        .foregroundStyle(todo.status == "completed" ? .tertiary : .secondary)
                        .strikethrough(todo.status == "completed")
                        .fixedSize(horizontal: false, vertical: true)
                    Spacer(minLength: 0)
                }
            }
        }
        .padding(10)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 10))
        .overlay {
            RoundedRectangle(cornerRadius: 10)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
    }

    private func icon(for status: String) -> String {
        switch status {
        case "completed": "checkmark.circle.fill"
        case "in_progress": "circle.lefthalf.filled"
        default: "circle"
        }
    }

    private func color(for status: String) -> Color {
        switch status {
        case "completed": .green
        case "in_progress": .orange
        default: .secondary
        }
    }
}

struct OrgNodeCard: View {
    let role: String
    let name: String
    let status: String
    let task: String
    let model: String
    var accent: Color = .green
    var isPlaceholder = false

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            ZStack {
                Circle()
                    .fill(accent.opacity(isPlaceholder ? 0.08 : 0.16))
                    .frame(width: 28, height: 28)
                Image(systemName: icon)
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(isPlaceholder ? .secondary : accent)
            }

            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(role)
                        .font(.system(size: 8, weight: .semibold, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    Text(name)
                        .font(.system(size: 12, weight: .semibold))
                    Spacer()
                    if !isPlaceholder {
                        StatusPill(status: status)
                    }
                }
                Text(task)
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                Text(model)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }

            Button {} label: {
                Image(systemName: "xmark.circle.fill")
                    .font(.system(size: 10))
                    .foregroundStyle(.tertiary)
            }
            .buttonStyle(.plain)
            .disabled(isPlaceholder)
            .opacity(isPlaceholder ? 0.25 : 1)
            .help("Pause or retire this agent")
        }
        .padding(10)
        .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(accent.opacity(isPlaceholder ? 0.08 : 0.16), lineWidth: 1)
        }
    }

    private var icon: String {
        switch role {
        case "LEADER": "crown"
        case "MANAGER": "person.2"
        case "WORKER": "person.3"
        default: "hammer"
        }
    }
}

struct AgentTierCard: View {
    let title: String
    let range: String
    let action: String
    let detail: String
    let icon: String
    let agents: [AgentRecord]

    var body: some View {
        if agents.isEmpty {
            TierPlaceholder(
                title: title,
                range: range,
                action: action,
                detail: detail,
                icon: icon
            )
        } else {
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 6) {
                    Image(systemName: icon)
                        .font(.system(size: 11, weight: .medium))
                        .foregroundStyle(.secondary)
                    Text(title.uppercased())
                        .font(.system(size: 9, weight: .semibold, design: .monospaced))
                        .foregroundStyle(.secondary)
                    Spacer()
                    Text("\(agents.count)")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
                ForEach(agents) { agent in
                    HStack(alignment: .top, spacing: 7) {
                        Circle()
                            .fill(color(for: agent.status))
                            .frame(width: 6, height: 6)
                            .padding(.top, 3)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(agent.name)
                                .font(.system(size: 10.5, weight: .medium))
                            if let title = agent.title, title != agent.name {
                                Text(title)
                                    .font(.system(size: 8, design: .monospaced))
                                    .foregroundStyle(.secondary)
                            }
                            HStack(spacing: 4) {
                                Text(agent.status)
                                if let profile = agent.profile {
                                    Text("· \(profile)")
                                }
                            }
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.tertiary)
                            if let branch = agent.branch {
                                Text(branch)
                                    .font(.system(size: 8, design: .monospaced))
                                    .foregroundStyle(.tertiary)
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                            }
                        }
                        Spacer(minLength: 0)
                    }
                }
            }
            .padding(10)
            .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
            .overlay {
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .strokeBorder(.white.opacity(0.07), lineWidth: 1)
            }
        }
    }

    private func color(for status: String) -> Color {
        switch status {
        case "running": .orange
        case "changes_requested": .orange
        case "idle": .green
        case "failed", "cancelled": .red
        default: .secondary
        }
    }
}

struct TierPlaceholder: View {
    let title: String
    let range: String
    let action: String
    let detail: String
    let icon: String

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: icon)
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(.secondary)
                .frame(width: 28, height: 28)
                .background(.white.opacity(0.05), in: Circle())

            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(title)
                        .font(.system(size: 12, weight: .semibold))
                    Text("× \(range)")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    Spacer()
                    Text(action)
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.green)
                }
                Text(detail)
                    .font(.system(size: 9.5))
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }

            Image(systemName: "plus")
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(.tertiary)
                .padding(.top, 4)
        }
        .padding(10)
        .background(.white.opacity(0.025), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(
                    .white.opacity(0.08),
                    style: StrokeStyle(lineWidth: 1, dash: [4, 4])
                )
        }
    }
}

struct OrgConnector: View {
    var body: some View {
        VStack(spacing: 0) {
            Rectangle()
                .fill(.white.opacity(0.12))
                .frame(width: 1, height: 10)
            Image(systemName: "chevron.down")
                .font(.system(size: 7, weight: .bold))
                .foregroundStyle(.tertiary)
            Rectangle()
                .fill(.white.opacity(0.12))
                .frame(width: 1, height: 10)
        }
        .frame(maxWidth: .infinity)
    }
}

struct ExternalResidentPanel: View {
    private let bridge = UnavailableResidentAgent()

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                HStack(alignment: .top, spacing: 12) {
                    ZStack {
                        Circle()
                            .fill(
                                LinearGradient(
                                    colors: [.green.opacity(0.55), .teal.opacity(0.35)],
                                    startPoint: .topLeading,
                                    endPoint: .bottomTrailing
                                )
                            )
                            .frame(width: 42, height: 42)
                        Image(systemName: "sparkles")
                            .font(.system(size: 16, weight: .bold))
                            .foregroundStyle(.white.opacity(0.9))
                    }

                    VStack(alignment: .leading, spacing: 3) {
                        HStack(spacing: 6) {
                            Text(bridge.displayName)
                                .font(.system(size: 16, weight: .semibold))
                            Text("EXTERNAL")
                                .font(.system(size: 8, weight: .semibold, design: .monospaced))
                                .padding(.horizontal, 5)
                                .padding(.vertical, 2)
                                .background(.white.opacity(0.06), in: Capsule())
                                .foregroundStyle(.secondary)
                        }
                        Text(
                            "Resident surface reserved for the NovaBAIC Aviiya bridge. Not implemented in this build."
                        )
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary)
                    }
                }

                Divider().opacity(0.3)

                SectionLabel(title: "Future bridge")
                VStack(alignment: .leading, spacing: 7) {
                    bridgeRow("person.crop.circle", "Identity", "Aviiya persona from NovaBAIC")
                    bridgeRow("brain", "Memory", "Shared resident memory, not local")
                    bridgeRow("wrench.and.screwdriver", "Tools", "Shared tool catalog")
                    bridgeRow(
                        "bubble.left.and.bubble.right",
                        "Sessions",
                        "Cross-device conversation continuity"
                    )
                }

                Divider().opacity(0.3)

                Text(
                    "This is a reserved seam, not a second assistant implementation. VerlinPark's project org remains independent."
                )
                .font(.system(size: 10))
                .foregroundStyle(.secondary)

                Link(destination: URL(string: "https://github.com/Verlintas/NovaBAIC")!) {
                    Label("NovaBAIC", systemImage: "arrow.up.right.square")
                        .font(.system(size: 10))
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
            }
            .padding(14)
        }
    }

    private func bridgeRow(_ icon: String, _ title: String, _ detail: String) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: icon)
                .font(.system(size: 10))
                .foregroundStyle(.secondary)
                .frame(width: 16)
            VStack(alignment: .leading, spacing: 1) {
                Text(title)
                    .font(.system(size: 10, weight: .medium))
                Text(detail)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
            Spacer()
        }
    }
}

struct PreviewPanelView: View {
    @Environment(AppModel.self) private var model
    @State private var mode: PreviewMode = .changes
    @State private var showingCommit = false
    @State private var showingDiscard = false
    @State private var showingLand = false
    @State private var showingPR = false
    @State private var commitMessage = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let session = model.selectedSession {
                header(session)
                Divider().opacity(0.3)
                switch mode {
                case .changes:
                    changes
                case .plan:
                    plan
                case .queue:
                    queue
                case .mailbox:
                    mailbox
                case .cost:
                    cost
                case .trace:
                    trace
                }
            } else {
                StudioEmptyView(
                    icon: "rectangle.stack",
                    title: "No session selected",
                    detail: "Choose a session in the center column."
                )
            }
        }
        .task(id: model.selectedSessionID) {
            if model.selectedSessionID != nil {
                await model.loadDiff()
                await model.loadMailbox()
                await model.loadUsage()
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
            "Open a pull request?",
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
                    Button("Open PR") {
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
                    .tint(.green)
                    .disabled(model.landingBusy)
                }
                Button {
                    Task {
                        switch mode {
                        case .queue:
                            await model.loadMergeQueue()
                        case .mailbox:
                            await model.loadMailbox()
                        case .cost:
                            await model.loadUsage()
                        default:
                            await model.loadDiff()
                        }
                    }
                } label: {
                    Image(systemName: "arrow.clockwise")
                        .font(.system(size: 10, weight: .semibold))
                }
                .buttonStyle(.plain)
                .help(
                    mode == .queue ? "Refresh queue" : mode == .mailbox ? "Refresh mailbox" :
                        mode == .cost ? "Refresh usage" : "Refresh diff"
                )
                .disabled(model.diffLoading)
            }
            Picker("", selection: $mode) {
                ForEach(PreviewMode.allCases) { mode in
                    Text(mode.title).tag(mode)
                }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .controlSize(.small)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
    }

    @ViewBuilder
    private var changes: some View {
        if model.diffLoading, model.sessionDiff == nil {
            ProgressView()
                .controlSize(.small)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
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

                    if !diff.patch.isEmpty {
                        DiffLines(text: diff.patch)
                    }
                }
                .padding(12)
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
                            buckets: ledger.totals.byDepartment
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
            ProgressView()
                .controlSize(.small)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
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
                            "Run an internal resolver agent without per-tool approvals (shell sandbox still applies), verify, then merge"
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
    }
}

private struct ArtifactRow: View {
    let artifact: Artifact

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
                Spacer()
                Text(artifact.agentName)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
            Text("\(artifact.sizeBytes) bytes · \(artifact.mimeType)")
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
            if artifact.storage == "file" {
                HStack(spacing: 8) {
                    Text(artifact.path ?? "path unavailable")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    if let path = artifact.path {
                        Button("Open") {
                            NSWorkspace.shared.open(URL(fileURLWithPath: path))
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
        }
        .padding(9)
        .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 9))
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
            Text(value)
                .font(.system(size: 15, weight: .semibold, design: .rounded))
                .monospacedDigit()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(9)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 9))
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
    case changes
    case plan
    case queue
    case mailbox
    case cost
    case trace

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .changes: "Changes"
        case .plan: "Plan"
        case .queue: "Queue"
        case .mailbox: "Mailbox"
        case .cost: "Cost"
        case .trace: "Trace"
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
            return .secondary
        }
        if line.hasPrefix("+") {
            return .green
        }
        if line.hasPrefix("-") {
            return .red
        }
        if line.hasPrefix("@@") || line.hasPrefix("diff --git") {
            return .cyan
        }
        return .secondary
    }

    private func background(for line: String) -> Color {
        if line.hasPrefix("+"), !line.hasPrefix("+++") {
            return .green.opacity(0.07)
        }
        if line.hasPrefix("-"), !line.hasPrefix("---") {
            return .red.opacity(0.06)
        }
        if line.hasPrefix("@@") {
            return .blue.opacity(0.06)
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
            Image(systemName: icon)
                .font(.system(size: 26, weight: .light))
                .foregroundStyle(.secondary)
            Text(title)
                .font(.system(size: 13, weight: .semibold))
            Text(detail)
                .font(.system(size: 10))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 240)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(24)
    }
}

struct OrgPresetSheet: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(AppModel.self) private var model
    @AppStorage("verlinpark.org.managers") private var managers = 4
    @AppStorage("verlinpark.org.workers") private var workers = 6
    @AppStorage("verlinpark.org.labour") private var labour = 12
    @AppStorage("verlinpark.org.autoscale") private var autoScale = true
    @AppStorage("verlinpark.org.budgetGate") private var budgetGate = 80

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            VStack(alignment: .leading, spacing: 3) {
                Text("Org Preset")
                    .font(.title2.weight(.semibold))
                Text(
                    "Small default team. Managers are created lazily when a plan task needs their department; Workers control batch concurrency. The session budget is the primary cost limit, though concurrent batches can overshoot by one turn."
                )
                .font(.callout)
                .foregroundStyle(.secondary)
            }

            Grid(alignment: .leading, horizontalSpacing: 18, verticalSpacing: 10) {
                GridRow {
                    Text("Managers").font(.system(size: 12))
                    Stepper("\(managers) · max 5", value: $managers, in: 1 ... 5)
                }
                GridRow {
                    Text("Workers").font(.system(size: 12))
                    Stepper("\(workers) · max 10", value: $workers, in: 1 ... 10)
                }
                GridRow {
                    Text("Labour").font(.system(size: 12))
                    Stepper("\(labour) · max 40", value: $labour, in: 1 ... 40)
                }
            }
            .font(.system(size: 11))

            Toggle("Auto-scale within budget (planned)", isOn: $autoScale)
                .disabled(true)
            HStack {
                Text("Freeze expansion at")
                Slider(value: Binding(
                    get: { Double(budgetGate) },
                    set: { budgetGate = Int($0) }
                ), in: 50 ... 95, step: 5)
                Text("\(budgetGate)%")
                    .font(.system(.caption, design: .monospaced))
                    .frame(width: 38, alignment: .trailing)
            }
            .font(.system(size: 11))
            .disabled(true)

            Text(
                "Plan dispatch runs ready tasks in dependency batches, each Worker in its own task worktree; Managers are created lazily. Merge queue and deterministic Integrator are implemented; cross-entry retry, auto-expansion, and per-role budgets are not."
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
