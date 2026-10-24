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
                    model: "manual",
                    accent: .green,
                    isPlaceholder: false
                )

                OrgConnector()

                TierPlaceholder(
                    title: "Managers",
                    range: "3–5",
                    action: "auto-recruit",
                    detail: "discuss · produce 2–3 outlines · assign work",
                    icon: "person.2"
                )

                OrgConnector()

                TierPlaceholder(
                    title: "Workers",
                    range: "2–10",
                    action: "auto-hire",
                    detail: "decompose tasks · run a worktree · report to Manager",
                    icon: "person.3"
                )

                OrgConnector()

                TierPlaceholder(
                    title: "Labour",
                    range: "3–40",
                    action: "auto-hire",
                    detail: "write code · run subtasks · report to Worker",
                    icon: "hammer"
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
                Text("idle")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
            Text("Only GUI / browser actions are serialized. Shell and worktree writes run in parallel.")
                .font(.system(size: 10))
                .foregroundStyle(.secondary)
            HStack(spacing: 6) {
                Circle().fill(.green).frame(width: 5, height: 5)
                Text("0 active · 0 queued")
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

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let session = model.selectedSession {
                header(session)
                Divider().opacity(0.3)
                switch mode {
                case .changes:
                    changes
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
            }
        }
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
                Button {
                    Task { await model.loadDiff() }
                } label: {
                    Image(systemName: "arrow.clockwise")
                        .font(.system(size: 10, weight: .semibold))
                }
                .buttonStyle(.plain)
                .help("Refresh diff")
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

                    if !diff.status.isEmpty {
                        Text(diff.status)
                            .font(.system(size: 9, design: .monospaced))
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                    }

                    if !diff.patch.isEmpty {
                        DiffLines(text: diff.patch)
                    }
                }
                .padding(12)
            }
        } else {
            StudioEmptyView(
                icon: "checkmark.circle",
                title: "No changes yet",
                detail: "Edits from the Leader will appear in this worktree diff."
            )
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

private enum PreviewMode: String, CaseIterable, Identifiable {
    case changes
    case trace

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .changes: "Changes"
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
                    "Four-tier project team. Auto-expansion is budget-gated; the Leader cannot exceed these caps."
                )
                .font(.callout)
                .foregroundStyle(.secondary)
            }

            Grid(alignment: .leading, horizontalSpacing: 18, verticalSpacing: 10) {
                GridRow {
                    Text("Managers").font(.system(size: 12))
                    Stepper("\(managers) · max 5", value: $managers, in: 3 ... 5)
                }
                GridRow {
                    Text("Workers").font(.system(size: 12))
                    Stepper("\(workers) · max 10", value: $workers, in: 2 ... 10)
                }
                GridRow {
                    Text("Labour").font(.system(size: 12))
                    Stepper("\(labour) · max 40", value: $labour, in: 3 ... 40)
                }
            }
            .font(.system(size: 11))

            Toggle("Auto-scale within budget", isOn: $autoScale)
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

            Text(
                "The runtime and model bindings land in M2. This preset is stored locally for the org graph and future scheduler."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.tertiary)

            HStack {
                Spacer()
                Button("Done") { dismiss() }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
            }
        }
        .padding(24)
        .frame(width: 480)
    }
}
