import SwiftUI

/// Collab is the live team graph: a deterministic tree of Leader, Managers,
/// Workers, Verifiers, and Labour, plus the tasks the scheduler is holding.
/// It is a monitor, not a second plan editor.
struct CollabGraphView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    @State private var zoom: CGFloat = 1
    @State private var autoFit = true
    @State private var showPlanned = true
    @State private var selectedID: String?
    @State private var showFullGraph = false
    @State private var surface: CollabSurface = .graph

    private var layout: CollabGraphLayout {
        CollabGraphBuilder.build(
            agents: model.agents,
            plan: model.teamPlan,
            statuses: model.planTaskStatuses,
            scheduled: model.scheduledSessionTasks,
            events: model.events,
            sessionStatus: model.selectedSession?.status ?? "idle",
            goal: model.selectedSession?.goal ?? L10n.t("Waiting for a session"),
            mergeQueue: model.mergeQueue,
            includePlanned: showPlanned,
            orchestration: model.orchestrationSnapshot
        )
    }

    private var selectedNode: CollabGraphNode? {
        guard let selectedID else { return nil }
        return layout.nodes.first { $0.id == selectedID }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            let layout = layout
            CollabGraphHeader(
                layout: layout,
                sessionStatus: model.selectedSession?.status ?? "idle",
                surface: $surface,
                showPlanned: $showPlanned,
                onExpand: { showFullGraph = true }
            )

            if surface == .ops {
                CollabOpsPanel()
            } else {
                GeometryReader { proxy in
                    Group {
                        if layout.isEmpty {
                            CollabGraphEmptyState(
                                goal: model.selectedSession?.goal ?? L10n.t("Waiting for a session")
                            )
                        } else {
                            ZStack(alignment: .topTrailing) {
                                graphScroll(layout: layout)
                                zoomControls(layout: layout, available: proxy.size)
                                    .padding(12)
                            }
                            .overlay(alignment: .bottom) {
                                if let selectedNode {
                                    CollabNodeInspector(
                                        node: selectedNode,
                                        task: task(for: selectedNode),
                                        blockers: blockers(for: selectedNode),
                                        busy: model.collabTaskActionBusyID != nil,
                                        onAction: { command in
                                            guard let task = task(for: selectedNode) else { return }
                                            Task { await model.collabTaskAction(task, action: command) }
                                        },
                                        onClose: { selectedID = nil }
                                    )
                                    .padding(.horizontal, 10)
                                    .padding(.bottom, 10)
                                    .transition(.move(edge: .bottom).combined(with: .opacity))
                                }
                            }
                        }
                    }
                    .onAppear {
                        fit(layout: layout, available: proxy.size, minimum: 0.55)
                    }
                    .onChange(of: model.selectedSessionID) { _, _ in
                        selectedID = nil
                        autoFit = true
                        fit(layout: layout, available: proxy.size, minimum: 0.55)
                    }
                    .onChange(of: layout.nodes.map(\.id)) { _, _ in
                        if let selectedID, !layout.nodes.contains(where: { $0.id == selectedID }) {
                            self.selectedID = nil
                        }
                        if autoFit {
                            fit(layout: layout, available: proxy.size, minimum: 0.55)
                        }
                    }
                }
            }
            if surface == .graph {
                VPHairline()
                CollabLiveStrip()
            }
        }
        .background(VPTheme.canvas)
        .sheet(isPresented: $showFullGraph) {
            CollabExpandedGraphView()
                .environment(model)
        }
    }

    private func task(for node: CollabGraphNode) -> OrchestrationTask? {
        guard let taskID = node.taskID else { return nil }
        return model.orchestrationSnapshot?.task(taskID)
    }

    private func blockers(for node: CollabGraphNode) -> [OrchestrationBlocker] {
        guard let taskID = node.taskID else { return [] }
        return model.orchestrationSnapshot?.blockers(for: taskID) ?? []
    }

    private func graphScroll(layout: CollabGraphLayout) -> some View {
        let scaled = CGSize(
            width: layout.size.width * zoom,
            height: layout.size.height * zoom
        )
        return ScrollView([.horizontal, .vertical]) {
            CollabGraphContent(
                layout: layout,
                selectedID: selectedID,
                reduceMotion: reduceMotion
            ) { id in
                withAnimation(VPMotion.resolved(VPMotion.snap, reduceMotion: reduceMotion)) {
                    selectedID = id
                }
            }
            .scaleEffect(zoom, anchor: .topLeading)
            .frame(width: scaled.width, height: scaled.height, alignment: .topLeading)
            .animation(
                VPMotion.resolved(VPMotion.smooth, reduceMotion: reduceMotion),
                value: structureKey(layout)
            )
        }
        .scrollIndicators(.hidden)
        .defaultScrollAnchor(.topLeading)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private func zoomControls(layout: CollabGraphLayout, available: CGSize) -> some View {
        HStack(spacing: 3) {
            Button {
                autoFit = false
                withAnimation(VPMotion.resolved(VPMotion.snap, reduceMotion: reduceMotion)) {
                    zoom = steppedZoom(zoom - 0.05)
                }
            } label: {
                Image(systemName: "minus")
            }
            .buttonStyle(VPGlassIconButtonStyle(size: 24, tint: VPTheme.textSecondary))
            .help(L10n.t("Zoom out"))

            Button {
                autoFit = false
                withAnimation(VPMotion.resolved(VPMotion.snap, reduceMotion: reduceMotion)) {
                    zoom = 1
                }
            } label: {
                Text("\(Int((zoom * 100).rounded()))%")
                    .font(VPTheme.mono(7.5, weight: .medium))
                    .foregroundStyle(VPTheme.textTertiary)
                    .frame(width: 34)
                    .contentTransition(.numericText())
            }
            .buttonStyle(.plain)
            .help(L10n.t("Reset to 100%"))

            Button {
                autoFit = false
                withAnimation(VPMotion.resolved(VPMotion.snap, reduceMotion: reduceMotion)) {
                    zoom = steppedZoom(zoom + 0.05)
                }
            } label: {
                Image(systemName: "plus")
            }
            .buttonStyle(VPGlassIconButtonStyle(size: 24, tint: VPTheme.textSecondary))
            .help(L10n.t("Zoom in"))

            Button {
                autoFit = true
                fit(layout: layout, available: available, minimum: 0.30)
            } label: {
                Image(systemName: "arrow.up.left.and.arrow.down.right")
            }
            .buttonStyle(VPGlassIconButtonStyle(size: 24, tint: VPTheme.accent))
            .help(L10n.t("Fit tree"))
        }
        .padding(4)
        .background(VPTheme.surface.opacity(0.92), in: Capsule())
        .overlay {
            Capsule().strokeBorder(VPTheme.edgeGradient, lineWidth: 1)
        }
        .shadow(color: .black.opacity(0.18), radius: 8, y: 3)
    }

    private func fit(
        layout: CollabGraphLayout,
        available: CGSize,
        minimum: CGFloat = 0.35
    ) {
        guard layout.size.width > 0, layout.size.height > 0 else { return }
        let usable = CGSize(
            width: max(160, available.width - 30),
            height: max(150, available.height - 30)
        )
        let scale = min(
            1.0,
            max(
                minimum,
                min(usable.width / layout.size.width, usable.height / layout.size.height)
            )
        )
        withAnimation(VPMotion.resolved(VPMotion.smooth, reduceMotion: reduceMotion)) {
            zoom = scale
        }
    }

    private func steppedZoom(_ value: CGFloat) -> CGFloat {
        (value * 20).rounded() / 20
    }

    private func structureKey(_ layout: CollabGraphLayout) -> String {
        layout.nodes
            .sorted { $0.id < $1.id }
            .map { "\($0.id):\($0.parentID ?? ""):\($0.kind.rawValue)" }
            .joined(separator: "|")
    }
}

/// The actual canvas, without scroll/zoom chrome. Kept separate so it can be
/// rendered deterministically in tests and reused by future surfaces.
struct CollabGraphContent: View {
    let layout: CollabGraphLayout
    let selectedID: String?
    let reduceMotion: Bool
    let onSelect: (String) -> Void

    var body: some View {
        ZStack(alignment: .topLeading) {
            CollabDotGrid(size: layout.size)
            CollabEdgeCanvas(layout: layout)
                .frame(width: layout.size.width, height: layout.size.height)
            ForEach(layout.nodes) { node in
                CollabNodeCard(
                    node: node,
                    selected: node.id == selectedID,
                    reduceMotion: reduceMotion
                )
                .frame(width: node.size.width, height: node.size.height)
                .position(node.position)
                .zIndex(node.id == selectedID ? 2 : 1)
                .transition(.scale(scale: 0.86).combined(with: .opacity))
                .onTapGesture {
                    onSelect(node.id)
                }
            }
        }
        .frame(width: layout.size.width, height: layout.size.height)
    }
}

/// A wide inspection surface for the same live graph. The Studio panel is
/// intentionally narrow; this sheet gives the tree room without turning the
/// panel into a second window manager.
struct CollabExpandedGraphView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    @State private var zoom: CGFloat = 1
    @State private var selectedID: String?
    @State private var showPlanned = true
    @State private var showOps = true

    private var layout: CollabGraphLayout {
        CollabGraphBuilder.build(
            agents: model.agents,
            plan: model.teamPlan,
            statuses: model.planTaskStatuses,
            scheduled: model.scheduledSessionTasks,
            events: model.events,
            sessionStatus: model.selectedSession?.status ?? "idle",
            goal: model.selectedSession?.goal ?? L10n.t("Waiting for a session"),
            mergeQueue: model.mergeQueue,
            includePlanned: showPlanned,
            orchestration: model.orchestrationSnapshot
        )
    }

    var body: some View {
        VStack(spacing: 0) {
            let layout = layout
            HStack(spacing: 8) {
                VPKicker(text: "Team graph", trailing: layout.metrics.headline)
                Spacer()
                Button {
                    withAnimation(VPMotion.snap) {
                        showPlanned.toggle()
                    }
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: showPlanned ? "rectangle.3.group.fill" : "rectangle.3.group")
                        Text(L10n.t("Plan"))
                    }
                    .font(VPTheme.mono(8, weight: .medium))
                    .foregroundStyle(showPlanned ? VPTheme.accent : VPTheme.textTertiary)
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                Button {
                    withAnimation(VPMotion.snap) { showOps.toggle() }
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: showOps ? "sidebar.right" : "sidebar.leading")
                        Text(L10n.t("Ops"))
                    }
                    .font(VPTheme.mono(8, weight: .medium))
                    .foregroundStyle(showOps ? VPTheme.accent : VPTheme.textTertiary)
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                Button(L10n.t("Done")) {
                    dismiss()
                }
                .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                .keyboardShortcut(.cancelAction)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 9)

            CollabRoleLegend()
                .padding(.horizontal, 14)
                .padding(.bottom, 7)

            VPHairline()

            HStack(spacing: 0) {
                GeometryReader { proxy in
                    ZStack(alignment: .topTrailing) {
                        ScrollView([.horizontal, .vertical]) {
                            CollabGraphContent(
                                layout: layout,
                                selectedID: selectedID,
                                reduceMotion: reduceMotion
                            ) { id in
                                withAnimation(VPMotion.resolved(VPMotion.snap, reduceMotion: reduceMotion)) {
                                    selectedID = id
                                }
                            }
                            .scaleEffect(zoom, anchor: .topLeading)
                            .frame(
                                width: layout.size.width * zoom,
                                height: layout.size.height * zoom,
                                alignment: .topLeading
                            )
                            .animation(
                                VPMotion.resolved(VPMotion.smooth, reduceMotion: reduceMotion),
                                value: layout.signature
                            )
                        }
                        .scrollIndicators(.hidden)
                        .defaultScrollAnchor(.topLeading)

                        HStack(spacing: 3) {
                            Button {
                                withAnimation(VPMotion.snap) {
                                    zoom = max(0.30, zoom - 0.1)
                                }
                            } label: {
                                Image(systemName: "minus")
                            }
                            .buttonStyle(VPGlassIconButtonStyle(size: 24, tint: VPTheme.textSecondary))
                            Text("\(Int((zoom * 100).rounded()))%")
                                .font(VPTheme.mono(7.5, weight: .medium))
                                .foregroundStyle(VPTheme.textTertiary)
                                .frame(width: 34)
                            Button {
                                withAnimation(VPMotion.snap) {
                                    zoom = min(1.8, zoom + 0.1)
                                }
                            } label: {
                                Image(systemName: "plus")
                            }
                            .buttonStyle(VPGlassIconButtonStyle(size: 24, tint: VPTheme.textSecondary))
                            Button {
                                fit(layout: layout, available: proxy.size)
                            } label: {
                                Image(systemName: "arrow.up.left.and.arrow.down.right")
                            }
                            .buttonStyle(VPGlassIconButtonStyle(size: 24, tint: VPTheme.accent))
                        }
                        .padding(4)
                        .background(VPTheme.surface.opacity(0.92), in: Capsule())
                        .overlay {
                            Capsule().strokeBorder(VPTheme.edgeGradient, lineWidth: 1)
                        }
                        .padding(12)
                    }
                    .overlay(alignment: .bottom) {
                        if let selectedID,
                           let node = layout.nodes.first(where: { $0.id == selectedID })
                        {
                            let task = node.taskID.flatMap { id in
                                model.orchestrationSnapshot?.task(id)
                            }
                            let blockers = node.taskID.flatMap { id in
                                model.orchestrationSnapshot?.blockers(for: id)
                            } ?? []
                            CollabNodeInspector(
                                node: node,
                                task: task,
                                blockers: blockers,
                                busy: model.collabTaskActionBusyID != nil,
                                onAction: { command in
                                    guard let task else { return }
                                    Task { await model.collabTaskAction(task, action: command) }
                                },
                                onClose: { self.selectedID = nil }
                            )
                            .padding(.horizontal, 12)
                            .padding(.bottom, 12)
                            .transition(.move(edge: .bottom).combined(with: .opacity))
                        }
                    }
                    .onAppear {
                        fit(layout: layout, available: proxy.size)
                    }
                    .onChange(of: layout.nodes.map(\.id)) { _, _ in
                        if let selectedID, !layout.nodes.contains(where: { $0.id == selectedID }) {
                            self.selectedID = nil
                        }
                    }
                }

                if showOps {
                    Divider()
                    CollabOpsPanel()
                        .frame(width: 360)
                        .transition(.move(edge: .trailing).combined(with: .opacity))
                }
            }
        }
        .frame(minWidth: 1060, minHeight: 620)
        .background(VPTheme.canvas)
    }

    private func steppedZoom(_ value: CGFloat) -> CGFloat {
        (value * 20).rounded() / 20
    }

    private func fit(layout: CollabGraphLayout, available: CGSize) {
        guard layout.size.width > 0, layout.size.height > 0 else { return }
        let scale = min(
            1.0,
            max(
                0.35,
                min(
                    (available.width - 40) / layout.size.width,
                    (available.height - 40) / layout.size.height
                )
            )
        )
        withAnimation(VPMotion.resolved(VPMotion.smooth, reduceMotion: reduceMotion)) {
            zoom = scale
        }
    }
}

// MARK: - Header

private struct CollabGraphHeader: View {
    let layout: CollabGraphLayout
    let sessionStatus: String
    @Binding var surface: CollabSurface
    @Binding var showPlanned: Bool
    var onExpand: (() -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack(spacing: 8) {
                VPKicker(
                    text: surface == .ops ? "Team operations" : "Team graph",
                    trailing: layout.metrics.planRevision > 1
                        ? "plan r\(layout.metrics.planRevision)"
                        : nil
                )
                Spacer(minLength: 4)
                StatusPill(status: sessionStatus)
                if let onExpand {
                    Button(action: onExpand) {
                        HStack(spacing: 3) {
                            Image(systemName: "arrow.up.right.square")
                                .font(.system(size: 8, weight: .semibold))
                            Text(L10n.t("Expand"))
                                .font(VPTheme.mono(7, weight: .medium))
                        }
                        .foregroundStyle(VPTheme.accent)
                        .padding(.horizontal, 7)
                        .padding(.vertical, 4)
                        .background(VPTheme.accent.opacity(0.10), in: Capsule())
                        .overlay {
                            Capsule().strokeBorder(VPTheme.accent.opacity(0.24), lineWidth: 1)
                        }
                    }
                    .buttonStyle(.plain)
                    .help(L10n.t("Open the full team graph"))
                }
            }

            VPSegmentedControl(
                segments: CollabSurface.allCases.map {
                    VPSegment($0, title: $0.title, icon: $0.icon)
                },
                selection: $surface
            )
            .frame(maxWidth: .infinity)
            .frame(height: 24)

            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 5) {
                    CollabMetricChip(
                        icon: "person.2",
                        label: L10n.t("Agents"),
                        value: "\(layout.metrics.runningAgents)/\(layout.metrics.totalAgents)",
                        tint: layout.metrics.runningAgents > 0 ? VPTheme.success : VPTheme.textTertiary
                    )
                    if layout.metrics.planPresent {
                        CollabMetricChip(
                            icon: "checklist",
                            label: L10n.t("Tasks"),
                            value: "\(layout.metrics.completedTasks)/\(layout.metrics.totalTasks)",
                            tint: layout.metrics.completedTasks == layout.metrics.totalTasks
                                ? VPTheme.success
                                : VPTheme.plan
                        )
                        if layout.metrics.readyTasks > 0 {
                            CollabMetricChip(
                                icon: "play.circle",
                                label: L10n.t("Ready"),
                                value: "\(layout.metrics.readyTasks)",
                                tint: VPTheme.accent
                            )
                        }
                        if layout.metrics.blockedTasks > 0 {
                            CollabMetricChip(
                                icon: "exclamationmark.triangle",
                                label: L10n.t("Blocked"),
                                value: "\(layout.metrics.blockedTasks)",
                                tint: VPTheme.warning
                            )
                        }
                    }
                    if layout.metrics.attentionCount > 0 {
                        CollabMetricChip(
                            icon: "waveform.path.ecg",
                            label: L10n.t("Attention"),
                            value: "\(layout.metrics.attentionCount)",
                            tint: VPTheme.danger
                        )
                    }
                    if layout.metrics.mergeQueueDepth > 0 {
                        CollabMetricChip(
                            icon: "arrow.triangle.merge",
                            label: L10n.t("Merge"),
                            value: "\(layout.metrics.mergeQueueDepth)",
                            tint: layout.metrics.mergeConflicts > 0
                                ? VPTheme.warning
                                : VPTheme.info
                        )
                    }
                }
            }
            .contentMargins(.horizontal, 0, for: .scrollContent)

            HStack(spacing: 7) {
                if layout.metrics.planPresent {
                    GeometryReader { proxy in
                        ZStack(alignment: .leading) {
                            Capsule().fill(VPTheme.hairline)
                            Capsule()
                                .fill(
                                    LinearGradient(
                                        colors: [VPTheme.plan.opacity(0.8), VPTheme.accent.opacity(0.9)],
                                        startPoint: .leading,
                                        endPoint: .trailing
                                    )
                                )
                                .frame(
                                    width: max(
                                        layout.metrics.planProgress > 0 ? 3 : 0,
                                        proxy.size.width * layout.metrics.planProgress
                                    )
                                )
                        }
                    }
                    .frame(height: 3)
                    .frame(maxWidth: .infinity)

                    Text("\(Int((layout.metrics.planProgress * 100).rounded()))%")
                        .font(VPTheme.mono(7.5, weight: .medium))
                        .foregroundStyle(VPTheme.textTertiary)
                        .frame(width: 30, alignment: .trailing)
                        .contentTransition(.numericText())
                } else {
                    Text(L10n.t("No team plan yet"))
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                    Spacer()
                }

                Button {
                    withAnimation(VPMotion.snap) {
                        showPlanned.toggle()
                    }
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: showPlanned ? "rectangle.3.group.fill" : "rectangle.3.group")
                            .font(.system(size: 8, weight: .semibold))
                        Text(L10n.t("Plan"))
                            .font(VPTheme.mono(7.5, weight: .medium))
                    }
                    .foregroundStyle(showPlanned ? VPTheme.accent : VPTheme.textTertiary)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 3)
                    .background(
                        showPlanned ? VPTheme.accent.opacity(0.12) : VPTheme.surface,
                        in: Capsule()
                    )
                    .overlay {
                        Capsule().strokeBorder(
                            showPlanned ? VPTheme.accent.opacity(0.28) : VPTheme.hairline,
                            lineWidth: 1
                        )
                    }
                }
                .buttonStyle(.plain)
                .help(L10n.t("Show planned managers and tasks that do not have an agent yet"))
            }

            CollabRoleLegend()
        }
        .padding(.horizontal, 12)
        .padding(.top, 4)
        .padding(.bottom, 8)
        .overlay(alignment: .bottom) {
            VPHairline()
        }
    }
}

private struct CollabMetricChip: View {
    let icon: String
    let label: String
    let value: String
    let tint: Color

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: icon)
                .font(.system(size: 7.5, weight: .semibold))
                .foregroundStyle(tint)
            Text(value)
                .font(VPTheme.mono(8, weight: .semibold))
                .foregroundStyle(VPTheme.textPrimary)
                .contentTransition(.numericText())
            Text(label.uppercased())
                .font(VPTheme.mono(6.5, weight: .medium))
                .tracking(0.4)
                .foregroundStyle(VPTheme.textTertiary)
        }
        .padding(.horizontal, 7)
        .padding(.vertical, 4)
        .background(VPTheme.surface, in: Capsule())
        .overlay {
            Capsule().strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
    }
}

private struct CollabRoleLegend: View {
    private struct LegendItem: Identifiable {
        let icon: String
        let label: String
        let tint: Color

        var id: String {
            label
        }
    }

    private let items: [LegendItem] = [
        LegendItem(icon: "crown", label: "Leader", tint: VPTheme.accent),
        LegendItem(icon: "person.crop.rectangle.stack", label: "Manager", tint: VPTheme.plan),
        LegendItem(icon: "person.3", label: "Worker", tint: VPTheme.info),
        LegendItem(icon: "checkmark.shield", label: "Verifier", tint: VPTheme.success),
        LegendItem(icon: "hammer", label: "Labour", tint: VPTheme.warning),
    ]

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 10) {
                ForEach(items) { item in
                    HStack(spacing: 3) {
                        Image(systemName: item.icon)
                            .font(.system(size: 6.5, weight: .semibold))
                            .foregroundStyle(item.tint)
                        Text(L10n.t(item.label).uppercased())
                            .font(VPTheme.mono(6.3, weight: .medium))
                            .tracking(0.5)
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                }
                HStack(spacing: 3) {
                    RoundedRectangle(cornerRadius: 1)
                        .strokeBorder(
                            VPTheme.textTertiary.opacity(0.55),
                            style: StrokeStyle(lineWidth: 1, dash: [2, 2])
                        )
                        .frame(width: 8, height: 6)
                    Text(L10n.t("PLANNED").uppercased())
                        .font(VPTheme.mono(6.3, weight: .medium))
                        .tracking(0.5)
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
        }
    }
}

// MARK: - Graph chrome

private struct CollabDotGrid: View {
    let size: CGSize

    var body: some View {
        Canvas { context, canvasSize in
            context.fill(
                Path(CGRect(origin: .zero, size: canvasSize)),
                with: .color(VPTheme.canvas)
            )
            context.fill(
                Path(CGRect(origin: .zero, size: canvasSize)),
                with: .radialGradient(
                    Gradient(colors: [VPTheme.accent.opacity(0.045), .clear]),
                    center: CGPoint(x: canvasSize.width / 2, y: 0),
                    startRadius: 0,
                    endRadius: max(280, canvasSize.width * 0.6)
                )
            )
            let step: CGFloat = 24
            let limit = 9000
            var count = 0
            var y: CGFloat = 12
            while y < canvasSize.height, count < limit {
                var x: CGFloat = 12
                while x < canvasSize.width, count < limit {
                    context.fill(
                        Path(ellipseIn: CGRect(x: x, y: y, width: 1.15, height: 1.15)),
                        with: .color(.white.opacity(0.035))
                    )
                    x += step
                    count += 1
                }
                y += step
            }
        }
        .frame(width: size.width, height: size.height)
        .allowsHitTesting(false)
    }
}

private struct CollabEdgeCanvas: View {
    let layout: CollabGraphLayout

    var body: some View {
        Canvas { context, _ in
            for edge in layout.edges {
                let midX = max(edge.from.x + 20, (edge.from.x + edge.to.x) / 2)
                var path = Path()
                path.move(to: edge.from)
                path.addCurve(
                    to: edge.to,
                    control1: CGPoint(x: midX, y: edge.from.y),
                    control2: CGPoint(x: midX, y: edge.to.y)
                )
                let planned = edge.childID.hasPrefix("__planned")
                let statusColor = VPTheme.statusColor(edge.status)
                if edge.active {
                    context.stroke(
                        path,
                        with: .color(VPTheme.accent.opacity(0.13)),
                        style: StrokeStyle(lineWidth: 5.5, lineCap: .round)
                    )
                }
                context.stroke(
                    path,
                    with: .color(
                        edge.active
                            ? VPTheme.accent.opacity(0.52)
                            : statusColor.opacity(planned ? 0.14 : 0.28)
                    ),
                    style: StrokeStyle(
                        lineWidth: edge.active ? 1.5 : 1,
                        lineCap: .round,
                        dash: planned ? [4, 4] : []
                    )
                )
                context.fill(
                    Path(ellipseIn: CGRect(
                        x: edge.to.x - 1.7,
                        y: edge.to.y - 1.7,
                        width: 3.4,
                        height: 3.4
                    )),
                    with: .color(
                        edge.active
                            ? VPTheme.accent.opacity(0.75)
                            : statusColor.opacity(planned ? 0.25 : 0.45)
                    )
                )
            }
        }
        .allowsHitTesting(false)
    }
}

// MARK: - Node card

private struct CollabNodeCard: View {
    let node: CollabGraphNode
    let selected: Bool
    let reduceMotion: Bool

    private var tint: Color {
        switch node.kind {
        case .leader: VPTheme.accent
        case .manager: VPTheme.plan
        case .worker: VPTheme.info
        case .verifier: VPTheme.success
        case .labour: VPTheme.warning
        case .group: VPTheme.textSecondary
        case .plannedTask: VPTheme.textTertiary
        }
    }

    private var icon: String {
        switch node.kind {
        case .leader: "crown"
        case .manager: "person.crop.rectangle.stack"
        case .worker: "person.3"
        case .verifier: "checkmark.shield"
        case .labour: "hammer"
        case .group: "rectangle.3.group"
        case .plannedTask: "square.dashed"
        }
    }

    private var statusColor: Color {
        node.isPlaceholder ? VPTheme.textTertiary : VPTheme.statusColor(node.statusColorKey)
    }

    private var isRunning: Bool {
        !node.isPlaceholder && node.isRunning
    }

    var body: some View {
        HStack(spacing: 0) {
            roleStripe
            cardContent
        }
        .background(cardBackground)
        .overlay(cardBorder)
        .overlay(alignment: .top) { glowOverlay }
        .shadow(
            color: isRunning ? tint.opacity(0.10) : .black.opacity(0.18),
            radius: isRunning ? 10 : 5,
            y: 3
        )
        .contentShape(RoundedRectangle(cornerRadius: 13, style: .continuous))
        .help(helpText)
    }

    private var roleStripe: some View {
        RoundedRectangle(cornerRadius: 2)
            .fill(
                LinearGradient(
                    colors: [tint.opacity(0.95), tint.opacity(0.35)],
                    startPoint: .top,
                    endPoint: .bottom
                )
            )
            .frame(width: 2.5)
            .padding(.vertical, 7)
    }

    private var cardContent: some View {
        VStack(alignment: .leading, spacing: 3) {
            headerRow
            bodyForKind
        }
        .padding(.leading, 7)
        .padding(.trailing, 8)
        .padding(.vertical, 6)
    }

    private var headerRow: some View {
        HStack(spacing: 4) {
            Image(systemName: icon)
                .font(.system(size: 8.5, weight: .semibold))
                .foregroundStyle(tint)
                .frame(width: 11)
            Text(node.name)
                .font(VPTheme.font(node.kind == .leader ? 11.5 : 10, weight: .semibold))
                .foregroundStyle(node.isPlaceholder ? VPTheme.textSecondary : VPTheme.textPrimary)
                .lineLimit(1)
            if node.isPlaceholder {
                plannedBadge
            }
            Spacer(minLength: 2)
            VPStatusDot(
                color: statusColor,
                pulsing: isRunning,
                size: node.kind == .leader ? 5 : 4
            )
        }
    }

    private var plannedBadge: some View {
        Text(L10n.t("PLANNED").uppercased())
            .font(VPTheme.mono(5.8, weight: .semibold))
            .tracking(0.4)
            .foregroundStyle(VPTheme.textTertiary)
            .padding(.horizontal, 3)
            .padding(.vertical, 1)
            .overlay {
                Capsule().strokeBorder(
                    VPTheme.textTertiary.opacity(0.4),
                    style: StrokeStyle(lineWidth: 0.8, dash: [2, 1.5])
                )
            }
    }

    @ViewBuilder
    private var bodyForKind: some View {
        switch node.kind {
        case .leader:
            leaderBody
        case .manager:
            managerBody
        case .worker:
            workerBody
        case .verifier, .labour:
            compactBody
        case .group:
            groupBody
        case .plannedTask:
            plannedTaskBody
        }
    }

    private var cardBackground: some View {
        RoundedRectangle(cornerRadius: node.kind == .leader ? 16 : 13, style: .continuous)
            .fill(
                LinearGradient(
                    colors: [
                        VPTheme.surfaceRaised.opacity(node.isPlaceholder ? 0.58 : 0.96),
                        VPTheme.surface.opacity(node.isPlaceholder ? 0.48 : 0.9),
                    ],
                    startPoint: .topLeading,
                    endPoint: .bottomTrailing
                )
            )
    }

    private var cardBorder: some View {
        RoundedRectangle(cornerRadius: node.kind == .leader ? 16 : 13, style: .continuous)
            .strokeBorder(
                borderStyle,
                style: StrokeStyle(
                    lineWidth: selected ? 1.4 : 1,
                    dash: node.isPlaceholder ? [4, 3] : []
                )
            )
    }

    private var borderStyle: AnyShapeStyle {
        if selected {
            AnyShapeStyle(VPTheme.accent.opacity(0.72))
        } else if isRunning {
            AnyShapeStyle(tint.opacity(0.30))
        } else {
            AnyShapeStyle(VPTheme.edgeGradient)
        }
    }

    @ViewBuilder
    private var glowOverlay: some View {
        if selected || isRunning {
            RoundedRectangle(cornerRadius: node.kind == .leader ? 16 : 13, style: .continuous)
                .strokeBorder(tint.opacity(selected ? 0.20 : 0.10), lineWidth: 4)
                .blur(radius: 4)
                .allowsHitTesting(false)
        }
    }

    private var leaderBody: some View {
        VStack(alignment: .leading, spacing: 3) {
            if let goal = node.objective, !goal.isEmpty {
                Text(goal)
                    .font(VPTheme.font(8.5))
                    .foregroundStyle(VPTheme.textSecondary)
                    .lineLimit(2)
            }
            if let progress = node.progress {
                HStack(spacing: 5) {
                    CollabMiniProgress(progress: progress, tint: VPTheme.accent)
                    Text("\(Int((progress * 100).rounded()))%")
                        .font(VPTheme.mono(6.5, weight: .medium))
                        .foregroundStyle(VPTheme.textTertiary)
                        .frame(width: 24, alignment: .trailing)
                }
            }
            HStack(spacing: 5) {
                Text(node.status.uppercased())
                    .foregroundStyle(statusColor)
                if node.totalDescendants > 0 {
                    Text("· \(node.activeDescendants) active / \(node.totalDescendants) reports")
                }
                if !node.model.isEmpty {
                    Text("· \(node.model)")
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }
            .font(VPTheme.mono(6.5))
            .foregroundStyle(VPTheme.textTertiary)
        }
    }

    private var managerBody: some View {
        VStack(alignment: .leading, spacing: 2.5) {
            if let objective = node.objective, !objective.isEmpty {
                Text(objective)
                    .font(VPTheme.font(7.8))
                    .foregroundStyle(VPTheme.textSecondary)
                    .lineLimit(1)
            }
            HStack(spacing: 5) {
                if let progress = node.progress {
                    CollabMiniProgress(progress: progress, tint: VPTheme.plan)
                        .frame(maxWidth: 72)
                    Text("\(Int((progress * 100).rounded()))%")
                        .font(VPTheme.mono(6.3))
                        .foregroundStyle(VPTheme.textTertiary)
                        .frame(width: 22, alignment: .trailing)
                }
                Text("\(node.childCount) reports")
                    .font(VPTheme.mono(6.3))
                    .foregroundStyle(VPTheme.textTertiary)
                if node.activeDescendants > 0 {
                    Text("· \(node.activeDescendants) active")
                        .font(VPTheme.mono(6.3))
                        .foregroundStyle(VPTheme.success)
                }
            }
            if let activity = node.activity, !activity.isEmpty {
                CollabActivityLine(text: activity, tint: statusColor)
            }
        }
    }

    private var workerBody: some View {
        VStack(alignment: .leading, spacing: 2.5) {
            if let taskLine = node.taskLine {
                Text(taskLine)
                    .font(VPTheme.font(8))
                    .foregroundStyle(VPTheme.textSecondary)
                    .lineLimit(1)
            }
            HStack(spacing: 4) {
                if let taskStatus = node.taskStatus {
                    CollabTaskStatusChip(status: taskStatus)
                }
                if let department = node.department, !department.isEmpty {
                    Text(department)
                        .font(VPTheme.mono(6.2))
                        .foregroundStyle(VPTheme.textTertiary)
                        .lineLimit(1)
                }
                Spacer(minLength: 0)
            }
            if node.ready || !node.blockedBy.isEmpty || node.nextAttemptAt != nil
                || node.verifierStatus != nil || node.attempts > 0
            {
                orchestrationBadges
            }
            if let activity = node.activity, !activity.isEmpty {
                CollabActivityLine(text: activity, tint: statusColor)
            }
        }
    }

    private var orchestrationBadges: some View {
        HStack(spacing: 3) {
            if node.ready {
                miniBadge("play.fill", tint: VPTheme.accent, text: L10n.t("ready"))
            }
            if !node.blockedBy.isEmpty {
                miniBadge("hand.raised.fill", tint: VPTheme.warning, text: L10n.t("blocked"))
            }
            if node.attempts > 0 {
                miniBadge("arrow.clockwise", tint: VPTheme.plan, text: "\(node.attempts)")
            }
            if node.nextAttemptAt != nil {
                miniBadge("clock", tint: VPTheme.warning, text: L10n.t("retry"))
            }
            if let verifier = node.verifierStatus {
                miniBadge(
                    verifier == "approved" ? "checkmark.shield.fill" : "shield.slash",
                    tint: VPTheme.statusColor(verifier),
                    text: verifier
                )
            }
            Spacer(minLength: 0)
        }
    }

    private func miniBadge(_ icon: String, tint: Color, text: String) -> some View {
        HStack(spacing: 2) {
            Image(systemName: icon)
                .font(.system(size: 5.5, weight: .semibold))
            Text(text.uppercased())
                .font(VPTheme.mono(5.2, weight: .semibold))
                .tracking(0.2)
        }
        .foregroundStyle(tint)
        .padding(.horizontal, 3)
        .padding(.vertical, 1.5)
        .background(tint.opacity(0.09), in: Capsule())
    }

    private var compactBody: some View {
        VStack(alignment: .leading, spacing: 2) {
            if let taskLine = node.taskLine {
                Text(taskLine)
                    .font(VPTheme.font(7.5))
                    .foregroundStyle(VPTheme.textSecondary)
                    .lineLimit(1)
            } else if let title = node.displayTitle {
                Text(title)
                    .font(VPTheme.font(7.5))
                    .foregroundStyle(VPTheme.textSecondary)
                    .lineLimit(1)
            }
            HStack(spacing: 4) {
                if let taskStatus = node.taskStatus {
                    CollabTaskStatusChip(status: taskStatus)
                } else {
                    Text(node.status.uppercased())
                        .font(VPTheme.mono(6.1, weight: .medium))
                        .foregroundStyle(statusColor)
                }
                if let activity = node.activity, !activity.isEmpty {
                    Text(activity)
                        .font(VPTheme.mono(6.1))
                        .foregroundStyle(VPTheme.textTertiary)
                        .lineLimit(1)
                }
            }
        }
    }

    private var groupBody: some View {
        HStack(spacing: 5) {
            Text(node.activity ?? node.status)
                .font(VPTheme.mono(6.8))
                .foregroundStyle(VPTheme.textTertiary)
                .lineLimit(1)
            Spacer(minLength: 0)
            Text("\(node.childCount)")
                .font(VPTheme.mono(7, weight: .semibold))
                .foregroundStyle(VPTheme.textSecondary)
        }
    }

    private var plannedTaskBody: some View {
        VStack(alignment: .leading, spacing: 2) {
            if let taskLine = node.taskLine {
                Text(taskLine)
                    .font(VPTheme.font(7.8))
                    .foregroundStyle(VPTheme.textSecondary)
                    .lineLimit(2)
            }
            HStack(spacing: 4) {
                CollabTaskStatusChip(status: node.taskStatus ?? node.status)
                if !node.deps.isEmpty {
                    Text("← \(node.deps.joined(separator: ", "))")
                        .font(VPTheme.mono(6.1))
                        .foregroundStyle(VPTheme.textTertiary)
                        .lineLimit(1)
                }
                Spacer(minLength: 0)
            }
            if node.ready || !node.blockedBy.isEmpty || node.nextAttemptAt != nil {
                orchestrationBadges
            }
        }
    }

    private var helpText: String {
        var parts = [node.kind.rawValue.capitalized, node.name]
        if let taskLine = node.taskLine {
            parts.append(taskLine)
        }
        if node.ready {
            parts.append(L10n.t("Ready to dispatch"))
        }
        if !node.blockedBy.isEmpty {
            parts.append("\(L10n.t("Blocked by")): \(node.blockedBy.joined(separator: ", "))")
        }
        if let verifier = node.verifierStatus {
            parts.append("\(L10n.t("Verifier")): \(verifier)")
        }
        if let activity = node.activity {
            parts.append(activity)
        }
        if let branch = node.branch {
            parts.append(branch)
        }
        return parts.joined(separator: " · ")
    }
}

private struct CollabMiniProgress: View {
    let progress: Double
    let tint: Color

    var body: some View {
        GeometryReader { proxy in
            ZStack(alignment: .leading) {
                Capsule().fill(VPTheme.hairline)
                Capsule()
                    .fill(tint.opacity(0.8))
                    .frame(width: max(2, proxy.size.width * min(1, max(0, progress))))
            }
        }
        .frame(height: 2.5)
    }
}

private struct CollabActivityLine: View {
    let text: String
    let tint: Color

    var body: some View {
        HStack(spacing: 3) {
            Circle()
                .fill(tint.opacity(0.85))
                .frame(width: 2.5, height: 2.5)
            Text(text)
                .font(VPTheme.mono(6.3))
                .foregroundStyle(VPTheme.textTertiary)
                .lineLimit(1)
                .truncationMode(.tail)
        }
    }
}

private struct CollabTaskStatusChip: View {
    let status: String

    private var tint: Color {
        VPTheme.statusColor(status)
    }

    var body: some View {
        Text(L10n.t(status).uppercased())
            .font(VPTheme.mono(5.9, weight: .semibold))
            .tracking(0.3)
            .foregroundStyle(tint)
            .padding(.horizontal, 4)
            .padding(.vertical, 1)
            .background(tint.opacity(0.10), in: Capsule())
            .overlay {
                Capsule().strokeBorder(tint.opacity(0.18), lineWidth: 0.7)
            }
    }
}

// MARK: - Inspector

private struct CollabNodeInspector: View {
    let node: CollabGraphNode
    let task: OrchestrationTask?
    let blockers: [OrchestrationBlocker]
    let busy: Bool
    let onAction: (CollabTaskCommand) -> Void
    let onClose: () -> Void

    private var tint: Color {
        switch node.kind {
        case .leader: VPTheme.accent
        case .manager: VPTheme.plan
        case .worker: VPTheme.info
        case .verifier: VPTheme.success
        case .labour: VPTheme.warning
        case .group, .plannedTask: VPTheme.textSecondary
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                Circle()
                    .fill(tint.opacity(0.9))
                    .frame(width: 5, height: 5)
                Text(node.name)
                    .font(VPTheme.font(11, weight: .semibold))
                    .foregroundStyle(VPTheme.textPrimary)
                    .lineLimit(1)
                if let title = node.displayTitle {
                    Text(title)
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                        .lineLimit(1)
                }
                Spacer()
                Text(node.kind.rawValue.uppercased())
                    .font(VPTheme.mono(6.5, weight: .semibold))
                    .tracking(0.5)
                    .foregroundStyle(tint)
                StatusPill(status: node.statusColorKey)
                VPCloseButton(
                    size: 24,
                    help: L10n.t("Close inspector")
                ) {
                    onClose()
                }
            }

            if let task {
                CollabInspectorActions(task: task, busy: busy, onAction: onAction)
            }

            ScrollView {
                VStack(alignment: .leading, spacing: 4) {
                    if let taskLine = node.taskLine {
                        CollabDetailRow(label: "Task", value: taskLine, tint: tint)
                    }
                    if let taskStatus = node.taskStatus {
                        CollabDetailRow(
                            label: "Task status",
                            value: taskStatus,
                            tint: VPTheme.statusColor(taskStatus)
                        )
                    }
                    if let task {
                        CollabDetailRow(
                            label: "Attempts",
                            value: "\(task.attempts)/\(task.maxAttempts)",
                            tint: task.attempts >= task.maxAttempts ? VPTheme.danger : VPTheme.textSecondary
                        )
                        if let verifier = task.verifierStatus {
                            CollabDetailRow(
                                label: "Verifier",
                                value: verifier,
                                tint: VPTheme.statusColor(verifier)
                            )
                        }
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
                    ForEach(blockers.prefix(2)) { blocker in
                        CollabDetailRow(
                            label: blocker.kind.replacingOccurrences(of: "_", with: " "),
                            value: blocker.message,
                            tint: VPTheme.danger
                        )
                    }
                    if let objective = node.objective, !objective.isEmpty {
                        CollabDetailRow(label: "Objective", value: objective, tint: VPTheme.textSecondary)
                    }
                    if let activity = node.activity, !activity.isEmpty {
                        CollabDetailRow(label: "Activity", value: activity, tint: VPTheme.textSecondary)
                    }
                    if !node.deps.isEmpty {
                        CollabDetailRow(
                            label: "Depends on",
                            value: node.deps.joined(separator: ", "),
                            tint: VPTheme.warning
                        )
                    }
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
                    if let branch = node.branch, !branch.isEmpty {
                        CollabDetailRow(label: "Branch", value: branch, tint: VPTheme.textTertiary)
                    }
                    if !node.model.isEmpty {
                        CollabDetailRow(label: "Model", value: node.model, tint: VPTheme.textTertiary)
                    }
                    if let error = node.lastError, !error.isEmpty {
                        CollabDetailRow(label: "Last error", value: error, tint: VPTheme.danger)
                    }
                }
            }
            .frame(maxHeight: 116)
        }
        .padding(9)
        .background(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .fill(VPTheme.surfaceRaised.opacity(0.97))
        )
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(VPTheme.edgeGradient, lineWidth: 1)
        }
        .shadow(color: .black.opacity(0.30), radius: 18, y: 8)
    }
}

private struct CollabInspectorActions: View {
    let task: OrchestrationTask
    let busy: Bool
    let onAction: (CollabTaskCommand) -> Void

    private var commands: [CollabTaskCommand] {
        var result: [CollabTaskCommand] = []
        if ["failed", "blocked", "interrupted", "cancelled"].contains(task.status) {
            result.append(.retry)
        }
        if ["running", "queued"].contains(task.status) {
            result.append(.pause)
        }
        if ["paused", "pausing"].contains(task.status) {
            result.append(.resume)
        }
        if ["pending", "failed", "blocked", "interrupted", "cancelled"].contains(task.status) {
            result.append(.skip)
        }
        if ["running", "queued", "paused", "pausing"].contains(task.status) {
            result.append(.cancel)
        }
        return result
    }

    var body: some View {
        HStack(spacing: 4) {
            ForEach(commands) { command in
                Button {
                    onAction(command)
                } label: {
                    HStack(spacing: 3) {
                        Image(systemName: command.icon)
                            .font(.system(size: 7, weight: .semibold))
                        Text(command.title)
                            .font(VPTheme.mono(6.4, weight: .medium))
                    }
                    .foregroundStyle(
                        command == .cancel || command == .skip
                            ? VPTheme.danger
                            : VPTheme.accent
                    )
                    .padding(.horizontal, 6)
                    .padding(.vertical, 3)
                    .background(
                        (command == .cancel || command == .skip
                            ? VPTheme.danger
                            : VPTheme.accent).opacity(0.09),
                        in: Capsule()
                    )
                }
                .buttonStyle(.plain)
                .disabled(busy)
            }
            Spacer()
        }
        .padding(.top, 2)
    }
}

private struct CollabDetailRow: View {
    let label: String
    let value: String
    let tint: Color

    var body: some View {
        HStack(alignment: .top, spacing: 6) {
            Text(L10n.t(label).uppercased())
                .font(VPTheme.mono(6.2, weight: .medium))
                .tracking(0.3)
                .foregroundStyle(VPTheme.textTertiary)
                .frame(width: 62, alignment: .leading)
            Text(value)
                .font(VPTheme.mono(6.8))
                .foregroundStyle(tint)
                .lineLimit(2)
                .truncationMode(.tail)
        }
    }
}

// MARK: - Empty state

private struct CollabGraphEmptyState: View {
    let goal: String

    var body: some View {
        VStack(spacing: 10) {
            ZStack {
                ForEach(0 ..< 3, id: \.self) { index in
                    RoundedRectangle(cornerRadius: 7, style: .continuous)
                        .strokeBorder(
                            VPTheme.textTertiary.opacity(0.22 - Double(index) * 0.04),
                            style: StrokeStyle(lineWidth: 1, dash: [4, 3])
                        )
                        .frame(width: 86 - CGFloat(index) * 8, height: 26)
                        .offset(y: CGFloat(index) * 38)
                }
                Rectangle()
                    .fill(VPTheme.hairline)
                    .frame(width: 1, height: 58)
                    .offset(y: 18)
            }
            .frame(height: 104)

            Text(L10n.t("The team tree is forming"))
                .font(VPTheme.font(12, weight: .semibold))
                .foregroundStyle(VPTheme.textSecondary)
            Text(goal)
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
                .lineLimit(2)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 280)
            Text(L10n
                .t("The Leader will populate this graph as Managers, Workers, and Verifiers are created."))
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 290)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(24)
    }
}

private struct CollabLiveStrip: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var running: [AgentRecord] {
        model.agents.filter {
            ["running", "verifying", "merging", "thinking"].contains($0.status)
        }
    }

    private var latest: (text: String, at: String)? {
        for event in model.events.reversed() {
            switch event.type {
            case "tool.started":
                let name = event.payload.objectValue?["name"]?.stringValue ?? "tool"
                let agent = event.payload.objectValue?["agent_name"]?.stringValue
                return ("\(agent ?? "Agent") · \(name)", event.createdAt)
            case "coordination.note":
                let message = event.payload.objectValue?["message"]?.stringValue ?? "coordination note"
                let agent = event.payload.objectValue?["agent_name"]?.stringValue ?? "Agent"
                return ("\(agent) · \(message)", event.createdAt)
            case "plan_task.finished":
                let task = event.payload.objectValue?["task_id"]?.stringValue ?? "task"
                let status = event.payload.objectValue?["task_status"]?.stringValue ?? "finished"
                return ("\(task) · \(status)", event.createdAt)
            case "merge_queue.enqueued", "merge_queue.updated":
                let status = event.payload.objectValue?["status"]?.stringValue ?? "updated"
                return ("merge queue · \(status)", event.createdAt)
            default:
                continue
            }
        }
        return nil
    }

    var body: some View {
        HStack(spacing: 8) {
            VPStatusDot(
                color: running.isEmpty ? VPTheme.textTertiary : VPTheme.success,
                pulsing: !running.isEmpty,
                size: 4
            )
            Text(running.isEmpty ? L10n.t("Team idle") : L10n.t("Live team"))
                .font(VPTheme.mono(7, weight: .semibold))
                .tracking(0.4)
                .foregroundStyle(running.isEmpty ? VPTheme.textTertiary : VPTheme.success)

            if running.isEmpty {
                Text("\(model.agents.count) agents on standby")
                    .font(VPTheme.mono(6.8))
                    .foregroundStyle(VPTheme.textTertiary)
            } else {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 5) {
                        ForEach(running.prefix(5)) { agent in
                            HStack(spacing: 3) {
                                Image(systemName: VPTheme.roleIcon(agent.role))
                                    .font(.system(size: 6, weight: .semibold))
                                    .foregroundStyle(VPTheme.roleColor(agent.role))
                                Text(agent.name)
                                    .font(VPTheme.mono(6.4))
                                    .foregroundStyle(VPTheme.textSecondary)
                                    .lineLimit(1)
                                if let task = agent.taskId {
                                    Text(task)
                                        .font(VPTheme.mono(5.8))
                                        .foregroundStyle(VPTheme.textTertiary)
                                }
                            }
                            .padding(.horizontal, 5)
                            .padding(.vertical, 2)
                            .background(VPTheme.surface, in: Capsule())
                        }
                    }
                }
            }

            Spacer(minLength: 4)

            if let latest {
                Text(latest.text)
                    .font(VPTheme.mono(6.6))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .frame(maxWidth: 190, alignment: .trailing)
                Text(VPTheme.relativeTime(latest.at))
                    .font(VPTheme.mono(5.8))
                    .foregroundStyle(VPTheme.textTertiary.opacity(0.8))
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .background(VPTheme.surface.opacity(0.35))
        .animation(
            VPMotion.resolved(VPMotion.smooth, reduceMotion: reduceMotion),
            value: running.map(\.id)
        )
    }
}
