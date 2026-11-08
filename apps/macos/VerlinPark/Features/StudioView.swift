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
                    HStack(spacing: 5) {
                        VPStatusDot(
                            color: model.selectedSession == nil ? VPTheme.textTertiary : VPTheme.success,
                            pulsing: model.selectedSession != nil,
                            size: 4
                        )
                        Text(model.selectedSession == nil ? "NO SESSION" : "LIVE")
                            .font(VPTheme.mono(7.5, weight: .semibold))
                            .tracking(0.6)
                            .foregroundStyle(
                                model.selectedSession == nil
                                    ? VPTheme.textTertiary
                                    : VPTheme.success
                            )
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
                    accent: VPTheme.accent,
                    isPlaceholder: false
                )
                .vpAppear(index: 0, animation: VPMotion.emphasizedSpring)

                if let plan = model.teamPlan {
                    OrgConnector()
                        .vpAppear(index: 1)
                    TeamPlanCard(plan: plan)
                        .vpAppear(index: 2)
                }

                if !model.roundtableRounds.isEmpty {
                    OrgConnector()
                        .vpAppear(index: 3)
                    RoundtableCard(rounds: model.roundtableRounds)
                        .vpAppear(index: 4)
                }

                if !model.todos.isEmpty {
                    OrgConnector()
                        .vpAppear(index: 5)
                    LeaderPlanCard(todos: model.todos)
                        .vpAppear(index: 6)
                }

                OrgConnector()
                    .vpAppear(index: 7)

                AgentTierCard(
                    title: "Managers",
                    range: "1–5",
                    action: "delegate",
                    detail: "split the objective · assign Workers · review",
                    icon: "person.2",
                    agents: model.agents.filter { $0.role == "manager" }
                )
                .vpAppear(index: 8)

                OrgConnector()
                    .vpAppear(index: 9)

                AgentTierCard(
                    title: "Workers",
                    range: "1–10",
                    action: "execute",
                    detail: "implement · test · report to Manager",
                    icon: "person.3",
                    agents: model.agents.filter { $0.role == "worker" }
                )
                .vpAppear(index: 10)

                OrgConnector()
                    .vpAppear(index: 11)

                AgentTierCard(
                    title: "Verifiers",
                    range: "1–100",
                    action: "gate",
                    detail: "inspect diff · run tests · approve or reject",
                    icon: "checkmark.shield",
                    agents: model.agents.filter { $0.role == "verifier" }
                )
                .vpAppear(index: 12)

                OrgConnector()
                    .vpAppear(index: 13)

                AgentTierCard(
                    title: "Labour",
                    range: "1–40",
                    action: "assist",
                    detail: "research · small edits · report to Worker",
                    icon: "hammer",
                    agents: model.agents.filter { $0.role == "labour" }
                )
                .vpAppear(index: 14)

                Divider()
                    .opacity(0.3)
                    .padding(.vertical, 14)

                computerLeaseCard
                    .vpAppear(index: 15)
            }
            .padding(.horizontal, 12)
            .padding(.top, 4)
            .padding(.bottom, 14)
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
        .background(VPTheme.surfaceSunken, in: RoundedRectangle(cornerRadius: 11, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 11, style: .continuous)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
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
                .contentTransition(.symbolEffect(.replace))
                .animation(VPMotion.snap, value: status)
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
                    if let priority = task.priority, priority != 0 {
                        Text("· P\(priority)")
                            .foregroundStyle(priority > 0 ? Color.orange : Color.secondary)
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
        .vpHoverHighlight(radius: 8)
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
        case "running": VPTheme.accent
        case "completed": VPTheme.success
        case "failed", "blocked", "cancelled": VPTheme.danger
        default: VPTheme.textTertiary
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
                    .font(VPTheme.mono(9, weight: .semibold))
                    .tracking(0.7)
                    .foregroundStyle(VPTheme.plan)
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
                        .fill(VPTheme.plan.opacity(0.85))
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
        .padding(11)
        .background(VPTheme.plan.opacity(0.045), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(VPTheme.plan.opacity(0.16), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.004, lift: 1, glow: VPTheme.plan)
    }
}

private struct RoundtableCard: View {
    let rounds: [RoundtableRound]
    @State private var expanded: Set<Int> = [1]

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Label("MANAGER ROUNDTABLE", systemImage: "bubble.left.and.bubble.right")
                    .font(.system(size: 9, weight: .semibold, design: .monospaced))
                    .foregroundStyle(.secondary)
                Spacer()
                Text("\(rounds.count) round\(rounds.count == 1 ? "" : "s")")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
            ForEach(rounds) { round in
                DisclosureGroup(isExpanded: binding(for: round.round)) {
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(round.positions) { position in
                            VStack(alignment: .leading, spacing: 2) {
                                HStack(spacing: 6) {
                                    Text(position.title)
                                        .font(.system(size: 9, weight: .medium))
                                    if let profile = position.profile {
                                        Text(profile)
                                            .font(.system(size: 8, design: .monospaced))
                                            .foregroundStyle(.tertiary)
                                    }
                                    Spacer(minLength: 0)
                                    if let agent = position.agentName, agent != position.title {
                                        Text(agent)
                                            .font(.system(size: 8, design: .monospaced))
                                            .foregroundStyle(.tertiary)
                                            .lineLimit(1)
                                    }
                                }
                                Text(position.position)
                                    .font(.system(size: 9))
                                    .foregroundStyle(.secondary)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                            .padding(.vertical, 2)
                        }
                    }
                    .padding(.top, 4)
                } label: {
                    Text("Round \(round.round)")
                        .font(.system(size: 9, weight: .medium, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
            }
        }
        .padding(11)
        .background(VPTheme.plan.opacity(0.035), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(VPTheme.plan.opacity(0.12), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.004, lift: 1, glow: VPTheme.plan)
    }

    private func binding(for round: Int) -> Binding<Bool> {
        Binding(
            get: { expanded.contains(round) },
            set: { isExpanded in
                if isExpanded {
                    expanded.insert(round)
                } else {
                    expanded.remove(round)
                }
            }
        )
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
                    .font(VPTheme.mono(9, weight: .semibold))
                    .tracking(0.7)
                    .foregroundStyle(VPTheme.textSecondary)
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
        .padding(11)
        .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
        .vpHoverLift(scale: 1.004, lift: 1)
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
        case "completed": VPTheme.success
        case "in_progress": VPTheme.accent
        default: VPTheme.textTertiary
        }
    }
}

struct OrgNodeCard: View {
    let role: String
    let name: String
    let status: String
    let task: String
    let model: String
    var accent: Color = VPTheme.accent
    var isPlaceholder = false

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            ZStack {
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .fill(accent.opacity(isPlaceholder ? 0.07 : 0.13))
                    .frame(width: 30, height: 30)
                Image(systemName: icon)
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(isPlaceholder ? VPTheme.textTertiary : accent)
            }

            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(L10n.t(role))
                        .font(VPTheme.mono(8, weight: .semibold))
                        .tracking(0.7)
                        .foregroundStyle(accent)
                    Text(name)
                        .font(VPTheme.font(12, weight: .semibold))
                        .foregroundStyle(VPTheme.textPrimary)
                    Spacer()
                    if !isPlaceholder {
                        StatusPill(status: status)
                    }
                }
                Text(task)
                    .font(VPTheme.font(10))
                    .foregroundStyle(VPTheme.textSecondary)
                    .lineLimit(2)
                Text(model)
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
            }

            Button {} label: {
                Image(systemName: "xmark")
                    .font(.system(size: 8, weight: .bold))
                    .foregroundStyle(VPTheme.textTertiary)
                    .frame(width: 18, height: 18)
                    .background(VPTheme.surfaceRaised, in: Circle())
            }
            .buttonStyle(VPPressableButtonStyle(scale: 0.9))
            .disabled(isPlaceholder)
            .opacity(isPlaceholder ? 0.25 : 1)
            .help("Pause or retire this agent")
        }
        .padding(11)
        .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(accent.opacity(isPlaceholder ? 0.08 : 0.18), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.004, lift: 1, glow: accent)
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

    private var tint: Color {
        switch title.lowercased() {
        case "managers": VPTheme.plan
        case "workers": VPTheme.info
        case "verifiers": VPTheme.success
        case "labour": VPTheme.warning
        default: VPTheme.textSecondary
        }
    }

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
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(tint)
                    Text(L10n.t(title).uppercased())
                        .font(VPTheme.mono(9, weight: .semibold))
                        .tracking(0.7)
                        .foregroundStyle(VPTheme.textSecondary)
                    Spacer()
                    Text("\(agents.count)")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                ForEach(agents) { agent in
                    HStack(alignment: .top, spacing: 7) {
                        VPStatusDot(
                            color: color(for: agent.status),
                            pulsing: agent.status == "running",
                            size: 5
                        )
                        .padding(.top, 4)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(agent.name)
                                .font(VPTheme.font(10.5, weight: .medium))
                                .foregroundStyle(VPTheme.textPrimary)
                            if let title = agent.title, title != agent.name {
                                Text(title)
                                    .font(VPTheme.mono(8))
                                    .foregroundStyle(VPTheme.textSecondary)
                            }
                            HStack(spacing: 4) {
                                Text(agent.status)
                                if let profile = agent.profile {
                                    Text("· \(profile)")
                                }
                            }
                            .font(VPTheme.mono(7.5))
                            .foregroundStyle(VPTheme.textTertiary)
                            if let branch = agent.branch {
                                Text(branch)
                                    .font(VPTheme.mono(7.5))
                                    .foregroundStyle(VPTheme.textTertiary)
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                            }
                        }
                        Spacer(minLength: 0)
                    }
                    .padding(.vertical, 2)
                }
            }
            .padding(11)
            .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
            .overlay {
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .strokeBorder(VPTheme.hairline, lineWidth: 1)
            }
            .vpHoverLift(scale: 1.004, lift: 1)
        }
    }

    private func color(for status: String) -> Color {
        VPTheme.statusColor(status)
    }
}

struct TierPlaceholder: View {
    let title: String
    let range: String
    let action: String
    let detail: String
    let icon: String

    private var tint: Color {
        switch title.lowercased() {
        case "managers": VPTheme.plan
        case "workers": VPTheme.info
        case "verifiers": VPTheme.success
        case "labour": VPTheme.warning
        default: VPTheme.textSecondary
        }
    }

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            ZStack {
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .fill(tint.opacity(0.08))
                    .frame(width: 28, height: 28)
                Image(systemName: icon)
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(tint.opacity(0.8))
            }

            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(L10n.t(title))
                        .font(VPTheme.font(12, weight: .semibold))
                        .foregroundStyle(VPTheme.textSecondary)
                    Text("× \(range)")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                    Spacer()
                    Text(action)
                        .font(VPTheme.mono(7.5, weight: .medium))
                        .foregroundStyle(tint)
                }
                Text(L10n.t(detail))
                    .font(VPTheme.font(9.5))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(2)
            }

            Image(systemName: "plus")
                .font(.system(size: 8, weight: .bold))
                .foregroundStyle(VPTheme.textTertiary)
                .padding(.top, 4)
        }
        .padding(11)
        .background(VPTheme.surface.opacity(0.45), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(
                    VPTheme.hairline,
                    style: StrokeStyle(lineWidth: 1, dash: [4, 4])
                )
        }
        .vpHoverLift(scale: 1.004, lift: 1)
    }
}

struct OrgConnector: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var drawn = false

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
        .scaleEffect(y: drawn || reduceMotion ? 1 : 0.01, anchor: .top)
        .opacity(drawn || reduceMotion ? 1 : 0)
        .animation(
            VPMotion.resolved(VPMotion.smooth, reduceMotion: reduceMotion),
            value: drawn
        )
        .onAppear { drawn = true }
        .frame(maxWidth: .infinity)
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
    @State private var mode: PreviewMode = .changes
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
            if model.consoleID != nil {
                await model.closeConsole()
            }
            if model.terminalID != nil {
                await model.closeTerminal()
            }
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
            VPSegmentedControl(
                segments: PreviewMode.allCases.map {
                    VPSegment($0, title: $0.title, icon: $0.icon)
                },
                selection: $mode,
                scrollable: true,
                compact: true
            )
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

private struct FilesPanel: View {
    @Environment(AppModel.self) private var model
    @State private var hoveredPath: String?
    @Namespace private var selectionNamespace
    @State private var draft = ""
    @State private var dirty = false
    @State private var pendingEntry: WorkspaceEntry?
    @State private var confirmDiscard = false
    @State private var confirmClose = false

    private var filteredEntries: [WorkspaceEntry] {
        let filter = model.workspaceFilter.trimmingCharacters(in: .whitespacesAndNewlines)
        return model.workspaceEntries.filter { entry in
            guard !entry.isDirectory else { return false }
            guard !filter.isEmpty else { return true }
            return entry.path.localizedCaseInsensitiveContains(filter)
                || entry.name.localizedCaseInsensitiveContains(filter)
        }
    }

    var body: some View {
        @Bindable var model = model
        GeometryReader { proxy in
            let compact = proxy.size.width < 560
            if compact {
                if model.workspaceFile != nil {
                    detailPane(compact: true)
                } else {
                    listPane(filter: $model.workspaceFilter)
                }
            } else {
                HStack(spacing: 0) {
                    listPane(filter: $model.workspaceFilter)
                        .frame(width: min(300, max(230, proxy.size.width * 0.40)))
                    VPHairline(vertical: true)
                    detailPane(compact: false)
                }
            }
        }
        .onAppear {
            syncDraft()
        }
        .onChange(of: fileKey) {
            syncDraft()
        }
        .confirmationDialog(
            "Discard unsaved changes?",
            isPresented: $confirmDiscard,
            titleVisibility: .visible
        ) {
            Button("Discard", role: .destructive) {
                if let entry = pendingEntry {
                    pendingEntry = nil
                    Task { await model.openWorkspaceFile(entry) }
                }
            }
            Button("Cancel", role: .cancel) {
                pendingEntry = nil
            }
        } message: {
            Text("The current draft has not been saved.")
        }
        .confirmationDialog(
            "Discard unsaved changes?",
            isPresented: $confirmClose,
            titleVisibility: .visible
        ) {
            Button("Discard", role: .destructive) {
                closeFile()
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("The current draft has not been saved.")
        }
    }

    private func listPane(filter: Binding<String>) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 6) {
                TextField("Filter files", text: filter)
                    .textFieldStyle(.plain)
                    .font(VPTheme.mono(9))
                    .padding(.horizontal, 7)
                    .padding(.vertical, 5)
                    .vpFieldSurface(radius: 7)
                if model.workspaceTreeBusy {
                    ProgressView().controlSize(.small)
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
                    ForEach(filteredEntries) { entry in
                        Button {
                            select(entry)
                        } label: {
                            HStack(spacing: 7) {
                                Image(systemName: "doc.text")
                                    .font(.system(size: 9))
                                    .foregroundStyle(VPTheme.textTertiary)
                                VStack(alignment: .leading, spacing: 1) {
                                    Text(entry.name)
                                        .font(VPTheme.font(10, weight: .medium))
                                        .foregroundStyle(VPTheme.textPrimary)
                                        .lineLimit(1)
                                    Text(entry.path)
                                        .font(VPTheme.mono(7))
                                        .foregroundStyle(VPTheme.textTertiary)
                                        .lineLimit(1)
                                        .truncationMode(.middle)
                                }
                                Spacer(minLength: 4)
                                Text(formatSize(entry.size))
                                    .font(VPTheme.mono(7))
                                    .foregroundStyle(VPTheme.textTertiary)
                            }
                            .padding(.horizontal, 9)
                            .padding(.vertical, 6)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background {
                                if model.workspaceSelectedPath == entry.path {
                                    RoundedRectangle(cornerRadius: 7, style: .continuous)
                                        .fill(VPTheme.accent.opacity(0.10))
                                        .overlay {
                                            RoundedRectangle(cornerRadius: 7, style: .continuous)
                                                .strokeBorder(VPTheme.accent.opacity(0.16), lineWidth: 1)
                                        }
                                        .matchedGeometryEffect(
                                            id: "file-selection",
                                            in: selectionNamespace
                                        )
                                } else if hoveredPath == entry.path {
                                    RoundedRectangle(cornerRadius: 7, style: .continuous)
                                        .fill(VPTheme.surface)
                                }
                            }
                        }
                        .buttonStyle(VPPressableButtonStyle(scale: 0.99))
                        .onHover { hovering in
                            if hovering {
                                hoveredPath = entry.path
                            } else if hoveredPath == entry.path {
                                hoveredPath = nil
                            }
                        }
                        .animation(VPMotion.hover, value: hoveredPath)
                        .animation(VPMotion.snap, value: model.workspaceSelectedPath)
                    }
                }
                .padding(.horizontal, 6)
                .padding(.vertical, 4)
            }

            if filteredEntries.isEmpty {
                Text(
                    model.workspaceTreeBusy
                        ? "Loading files…"
                        : "No files match the filter."
                )
                .font(VPTheme.mono(9))
                .foregroundStyle(VPTheme.textSecondary)
                .padding(12)
            }
        }
    }

    private func detailPane(compact: Bool) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            if let file = model.workspaceFile {
                detailHeader(file: file, compact: compact)
                VPHairline()
                if let error = model.workspaceFileError {
                    Text(error)
                        .font(VPTheme.mono(9))
                        .foregroundStyle(VPTheme.warning)
                        .padding(10)
                }
                if let status = model.workspaceSaveStatus {
                    Text(status)
                        .font(VPTheme.mono(8))
                        .foregroundStyle(status.hasPrefix("Saved") ? VPTheme.success : VPTheme.warning)
                        .padding(.horizontal, 10)
                        .padding(.top, 5)
                }
                TextEditor(text: $draft)
                    .font(VPTheme.mono(10))
                    .foregroundStyle(VPTheme.textSecondary)
                    .scrollContentBackground(.hidden)
                    .disabled(!file.editable || model.workspaceFileBusy || model.workspaceSaveBusy)
                    .onChange(of: draft) { _, value in
                        dirty = file.editable && value != file.content
                    }
                    .padding(6)
            } else {
                VStack(spacing: 8) {
                    Image(systemName: "doc.text.magnifyingglass")
                        .font(.system(size: 24))
                        .foregroundStyle(VPTheme.textTertiary)
                    Text(
                        model.workspaceTreeBusy
                            ? "Loading session files…"
                            : "Select a file to inspect the session worktree."
                    )
                    .font(VPTheme.mono(9))
                    .foregroundStyle(VPTheme.textSecondary)
                    if model.workspaceFileBusy {
                        ProgressView().controlSize(.small)
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
    }

    private func detailHeader(file: WorkspaceFileResult, compact: Bool) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 7) {
                if compact {
                    Button {
                        if dirty {
                            confirmClose = true
                        } else {
                            closeFile()
                        }
                    } label: {
                        Image(systemName: "chevron.left")
                            .font(.system(size: 9, weight: .semibold))
                    }
                    .buttonStyle(VPGlassIconButtonStyle(size: 22, tint: VPTheme.textSecondary))
                    .help("Back to files")
                }

                Image(systemName: file.binary ? "doc.zipper" : "doc.text")
                    .font(.system(size: 10))
                    .foregroundStyle(VPTheme.textTertiary)
                Text(file.path)
                    .font(VPTheme.mono(9))
                    .foregroundStyle(VPTheme.textSecondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: 4)
                Text(formatSize(file.size))
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
            }

            HStack(spacing: 5) {
                if file.binary {
                    fileBadge("binary", tint: VPTheme.warning)
                }
                if file.truncated {
                    fileBadge("truncated", tint: VPTheme.warning)
                }
                if dirty {
                    fileBadge("modified", tint: VPTheme.accent)
                }
                Spacer(minLength: 0)
                if file.editable {
                    Button("Reload") {
                        reload()
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(model.workspaceSaveBusy)
                    Button("Save") {
                        Task {
                            if await model.saveWorkspaceFile(content: draft) {
                                dirty = false
                            }
                        }
                    }
                    .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                    .controlSize(.small)
                    .disabled(!dirty || model.workspaceSaveBusy)
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

    private func closeFile() {
        model.workspaceSelectedPath = nil
        model.workspaceFile = nil
        draft = ""
        dirty = false
    }

    private var fileKey: String {
        guard let file = model.workspaceFile else { return "" }
        return "\(file.path):\(file.sha256)"
    }

    private func syncDraft() {
        guard let file = model.workspaceFile else {
            draft = ""
            dirty = false
            return
        }
        draft = file.content
        dirty = false
    }

    private func select(_ entry: WorkspaceEntry) {
        if dirty {
            pendingEntry = entry
            confirmDiscard = true
        } else {
            Task { await model.openWorkspaceFile(entry) }
        }
    }

    private func reload() {
        guard let path = model.workspaceSelectedPath,
              let entry = model.workspaceEntries.first(where: { $0.path == path })
        else { return }
        if dirty {
            pendingEntry = entry
            confirmDiscard = true
        } else {
            Task { await model.reloadWorkspaceFile() }
        }
    }

    private func formatSize(_ bytes: UInt64) -> String {
        if bytes < 1024 {
            return "\(bytes) B"
        }
        if bytes < 1024 * 1024 {
            return String(format: "%.1f KB", Double(bytes) / 1024)
        }
        return String(format: "%.1f MB", Double(bytes) / 1024 / 1024)
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
            Text(title)
                .font(.system(size: 13, weight: .semibold))
            Text(detail)
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
