import SwiftUI

struct ProjectUsageCard: View {
    @Environment(AppModel.self) private var model
    let project: Project

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Usage by session")
                    Text(project.name)
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.projectUsageBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Refresh") {
                    Task { await model.loadProjectUsage() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.projectUsageBusy)
            }
            if let usage = model.projectUsage {
                HStack(spacing: 14) {
                    usageMetric("Calls", "\(usage.totals.calls)")
                    usageMetric(
                        "Tokens",
                        "\(usage.totals.inputTokens + usage.totals.outputTokens)"
                    )
                    usageMetric("Cost", String(format: "$%.4f", usage.totals.costUsd))
                    Spacer()
                    Text("\(usage.sessions.count) sessions")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
                if usage.sessions.isEmpty {
                    Text("No sessions yet.")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(Array(usage.sessions.prefix(8))) { session in
                        HStack(alignment: .top, spacing: 8) {
                            Text(session.status.uppercased())
                                .font(.system(size: 8, weight: .medium, design: .monospaced))
                                .foregroundStyle(color(for: session.status))
                                .frame(width: 62, alignment: .leading)
                            VStack(alignment: .leading, spacing: 1) {
                                Text(session.goal)
                                    .font(.system(size: 9, weight: .medium))
                                    .lineLimit(1)
                                Text(
                                    "calls \(session.calls) · tokens \(session.totalTokens) · $"
                                        + String(format: "%.4f", session.costUsd)
                                )
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(.tertiary)
                            }
                            Spacer(minLength: 0)
                        }
                    }
                    if usage.sessions.count > 8 {
                        Text(
                            "… and \(usage.sessions.count - 8) more sessions. Use `vp project-usage` for the full list."
                        )
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    }
                }
            } else if let status = model.projectUsageStatus {
                Text(status)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            } else {
                ProgressView().controlSize(.small)
            }
        }
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .vpAppear(index: 0)
    }

    private func usageMetric(_ title: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(title)
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
            Text(value)
                .font(.system(size: 12, weight: .semibold, design: .rounded))
                .monospacedDigit()
        }
    }

    private func color(for status: String) -> Color {
        switch status {
        case "idle", "completed": .green
        case "running", "queued", "paused": .orange
        case "failed", "cancelled": .red
        default: .secondary
        }
    }
}

struct ApprovalCenterCard: View {
    @Environment(AppModel.self) private var model
    @State private var templateApproval: Approval?

    private var selectedCount: Int {
        model.pendingApprovals.count { model.approvalSelection.contains($0.id) }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Approvals")
                    Text("Pending across projects")
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.approvalCenterBusy || model.approvalBatchBusy {
                    ProgressView().controlSize(.small)
                }
                Text("\(model.pendingApprovals.count) pending")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
                Button("Refresh") {
                    Task { await model.loadPendingApprovals() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.approvalCenterBusy)
            }
            if model.pendingApprovals.isEmpty {
                Text(model.approvalCenterStatus ?? "No pending approvals.")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            } else {
                HStack(spacing: 6) {
                    Button("Select all") {
                        model.selectAllPendingApprovals()
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    Button("Clear") {
                        model.clearApprovalSelection()
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(model.approvalSelection.isEmpty)
                    Spacer()
                    Text("\(selectedCount) selected")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    Button("Approve selected") {
                        Task { await model.resolveSelectedApprovals(approved: true) }
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                    .tint(.green)
                    .disabled(model.approvalSelection.isEmpty || model.approvalBatchBusy)
                    Button("Deny selected") {
                        Task { await model.resolveSelectedApprovals(approved: false) }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(model.approvalSelection.isEmpty || model.approvalBatchBusy)
                }
                if let status = model.approvalBatchStatus, !status.isEmpty {
                    Text(status)
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                ForEach(model.pendingApprovals) { approval in
                    approvalRow(approval)
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
        .vpAppear(index: 0)
        .task {
            await model.loadPendingApprovals()
        }
        .sheet(item: $templateApproval) { approval in
            ApprovalTemplateSheet(approval: approval)
        }
    }

    private func approvalRow(_ approval: Approval) -> some View {
        let selected = model.approvalSelection.contains(approval.id)
        return VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 6) {
                Button {
                    model.toggleApprovalSelection(approval.id)
                } label: {
                    Image(systemName: selected ? "checkmark.square.fill" : "square")
                        .font(.system(size: 11))
                        .foregroundStyle(selected ? VPTheme.accent : .secondary)
                }
                .buttonStyle(.plain)
                .help(selected ? "Deselect approval" : "Select approval")
                Text(approval.tool.uppercased())
                    .font(.system(size: 8, weight: .semibold, design: .monospaced))
                    .foregroundStyle(.orange)
                    .padding(.horizontal, 5)
                    .padding(.vertical, 2)
                    .background(.orange.opacity(0.1), in: Capsule())
                if let risk = approval.approvalRisk, !risk.isEmpty {
                    Text(risk.uppercased())
                        .font(.system(size: 7, weight: .semibold, design: .monospaced))
                        .foregroundStyle(riskColor(risk))
                        .padding(.horizontal, 4)
                        .padding(.vertical, 2)
                        .background(riskColor(risk).opacity(0.09), in: Capsule())
                }
                Text(approval.agentName ?? approval.agentRole ?? "agent")
                    .font(.system(size: 9, weight: .medium))
                if let title = approval.agentTitle {
                    Text("· \(title)")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
                Spacer()
                Text(
                    model.approvalSessionGoals[approval.sessionId]
                        ?? String(approval.sessionId.prefix(8))
                )
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
                .lineLimit(1)
            }
            Text(inputSummary(approval))
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.secondary)
                .lineLimit(3)
                .textSelection(.enabled)
            if let reason = approval.approvalReason, !reason.isEmpty {
                Text(reason)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(riskColor(approval.approvalRisk ?? ""))
                    .fixedSize(horizontal: false, vertical: true)
            }
            if !approval.attentionLabel.isEmpty {
                Text(approval.attentionLabel)
                    .font(.system(size: 7.5, design: .monospaced))
                    .foregroundStyle(VPTheme.accentDeep)
            }
            HStack(spacing: 6) {
                Button("Approve") {
                    Task {
                        await model.resolveGlobalApproval(approval.id, approved: true)
                    }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .tint(.green)
                .disabled(model.approvalCenterBusy || model.approvalBatchBusy)
                Button("Deny") {
                    Task {
                        await model.resolveGlobalApproval(approval.id, approved: false)
                    }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.approvalCenterBusy || model.approvalBatchBusy)
                if canCreateTemplate(approval) {
                    Button("Template") {
                        templateApproval = approval
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .help("Create a bounded template from this approval")
                }
            }
        }
        .padding(9)
        .background(
            selected ? VPTheme.accent.opacity(0.06) : .white.opacity(0.03),
            in: RoundedRectangle(cornerRadius: 9)
        )
    }

    private func canCreateTemplate(_ approval: Approval) -> Bool {
        guard let risk = approval.approvalRisk else { return false }
        let object = approval.input.objectValue
        let hasPath =
            object?["path"]?.stringValue != nil
                || object?["paths"]?.arrayValue?.isEmpty == false
        return ["workspace_write", "external"].contains(risk)
            && hasPath
            && !approval.tool.hasPrefix("remote_")
            && ![
                "mcp", "computer", "shell", "process", "run_project_task",
                "local_http", "code_diagnostics", "task_control",
            ].contains(approval.tool)
    }

    private func riskColor(_ risk: String) -> Color {
        switch risk {
        case "destructive": .red
        case "host_state": .orange
        case "external": .blue
        default: .secondary
        }
    }

    private func inputSummary(_ approval: Approval) -> String {
        let object = approval.input.objectValue
        if let command = object?["command"]?.stringValue {
            return command
        }
        if let path = object?["path"]?.stringValue {
            return path
        }
        if let url = object?["url"]?.stringValue {
            return url
        }
        return approval.tool
    }
}

struct MemoryCard: View {
    @Environment(AppModel.self) private var model
    let project: Project
    @State private var confirmPrune = false

    private func statsSummary(_ stats: MemoryStatsResult) -> String {
        func count(_ key: String) -> Int {
            stats.byStatus.first { $0.key == key }?.count ?? 0
        }
        return "\(stats.total) total · \(count("active")) active · \(count("stale")) stale · \(count("superseded")) superseded"
    }

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Project memory")
                    Text(project.name)
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.memoryBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Refresh") {
                    Task { await model.loadMemories() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.memoryBusy)
                Button("Sweep stale") {
                    Task { await model.sweepStaleMemories() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.memoryBusy || model.memorySweepBusy)
                .help("Mark memories past their verification window stale")
                Button("Prune") {
                    confirmPrune = true
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.memoryBusy || model.memories.isEmpty)
                .help("Delete unpinned memories older than 30 days")
            }
            Text("Evidence-backed decisions, facts, conventions, and task results shared across sessions.")
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(.secondary)
            HStack(spacing: 10) {
                Toggle("Include stale", isOn: $model.memoryIncludeStale)
                    .toggleStyle(.checkbox)
                    .controlSize(.small)
                    .onChange(of: model.memoryIncludeStale) { _, _ in
                        Task { await model.loadMemories() }
                    }
                if let stats = model.memoryStats {
                    Text(
                        statsSummary(stats)
                            + (model.memoryHealth.map { " · \($0.openConflicts) conflicts" } ?? "")
                    )
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
                } else if let status = model.memoryStatsStatus {
                    Text(status)
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
            }
            HStack(spacing: 8) {
                TextField("Search memories", text: $model.memoryQuery)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 9, design: .monospaced))
                    .onSubmit {
                        Task { await model.loadMemories() }
                    }
                Button("Search") {
                    Task { await model.loadMemories() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
            }
            if let session = model.selectedSession {
                VStack(alignment: .leading, spacing: 5) {
                    HStack(spacing: 6) {
                        TextField("kind (decision, fact, source)", text: $model.memoryNewKind)
                            .textFieldStyle(.roundedBorder)
                            .font(.system(size: 9, design: .monospaced))
                            .frame(width: 150)
                        TextField("title", text: $model.memoryNewTitle)
                            .textFieldStyle(.roundedBorder)
                            .font(.system(size: 9, design: .monospaced))
                    }
                    TextField("body", text: $model.memoryNewBody, axis: .vertical)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 9, design: .monospaced))
                        .lineLimit(2 ... 4)
                    HStack {
                        Text(session.goal)
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.tertiary)
                            .lineLimit(1)
                        Spacer()
                        Button("Add memory") {
                            Task { await model.createMemory() }
                        }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.small)
                        .disabled(model.memoryBusy)
                    }
                }
            } else {
                Text("Select a session to add a memory.")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
            if model.memories.isEmpty {
                Text(model.memoryStatus ?? "No project memories yet.")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            } else {
                ForEach(Array(model.memories.prefix(10))) { memory in
                    MemoryRow(memory: memory)
                }
                if model.memories.count > 10 {
                    Text("… and \(model.memories.count - 10) more memories.")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
            }
            if let status = model.memoryStatus, !model.memories.isEmpty {
                Text(status)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
        }
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .vpAppear(index: 0)
        .confirmationDialog(
            "Prune project memory?",
            isPresented: $confirmPrune,
            titleVisibility: .visible
        ) {
            Button("Delete memories older than 30 days", role: .destructive) {
                Task { await model.pruneMemories(olderThanDays: 30) }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(
                "Only unpinned memories created more than 30 days ago are removed. Recent and pinned memories are kept."
            )
        }
    }
}

private struct MemoryEmbeddingHealthStrip: View {
    let status: MemoryEmbeddingsStatusResult

    private var indexedLabel: String {
        "\(status.indexed)/\(status.indexable) indexed · \(status.pending) pending"
    }

    private var usageLabel: String {
        "\(status.usage.calls) embedding calls · \(status.usage.inputTokens) tokens"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 10) {
                Text(verbatim: "semantic \(status.healthLabel)")
                    .foregroundStyle(status.failed > 0 ? VPTheme.warning : VPTheme.accent)
                Text(verbatim: "coverage \(status.coverageLabel)")
                Text(verbatim: indexedLabel)
                if status.failed > 0 {
                    Text(verbatim: "\(status.failed) failed")
                        .foregroundStyle(VPTheme.warning)
                }
                if status.invalid > 0 {
                    Text(verbatim: "\(status.invalid) invalid")
                        .foregroundStyle(VPTheme.warning)
                }
                if status.usage.calls > 0 {
                    Text(verbatim: usageLabel)
                }
            }
            .font(.system(size: 8, design: .monospaced))
            .foregroundStyle(.secondary)
            if let error = status.lastError {
                Text(verbatim: error)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(VPTheme.warning)
                    .lineLimit(2)
            }
        }
    }
}

struct MemoryHealthCard: View {
    @Environment(AppModel.self) private var model
    let project: Project
    @State private var pendingConflict: MemoryConflict?
    @State private var pendingAction: MemoryConflictResolution = .keepA

    private var openConflicts: [MemoryConflict] {
        model.memoryConflicts.filter(\.isOpen)
    }

    private func metric(
        _ title: LocalizedStringKey,
        _ value: Int,
        color: Color = .secondary
    ) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title)
                .font(.system(size: 7.5, design: .monospaced))
                .foregroundStyle(.tertiary)
            Text("\(value)")
                .font(.system(size: 15, weight: .semibold, design: .rounded))
                .foregroundStyle(color)
        }
        .frame(minWidth: 64, alignment: .leading)
    }

    private func requestResolution(
        _ conflict: MemoryConflict,
        action: MemoryConflictResolution
    ) {
        if action.isDestructive {
            pendingConflict = conflict
            pendingAction = action
        } else {
            Task { await model.resolveMemoryConflict(conflict, action: action) }
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Memory health")
                    Text(project.name)
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.memoryConflictBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Refresh") {
                    Task {
                        await model.loadMemoryHealth()
                        await model.loadMemoryConflicts()
                    }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.memoryConflictBusy)
                Button("Scan") {
                    Task { await model.scanMemoryConflicts() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.memoryConflictBusy)
                .help("Detect same-path or same-task active claims with different bodies")
            }
            Text(
                "Potential conflicts are deterministic review signals, not semantic fact-checking. Detection never changes memory; only an explicit resolution does."
            )
            .font(.system(size: 9, design: .monospaced))
            .foregroundStyle(.secondary)

            if let health = model.memoryHealth {
                HStack(alignment: .top, spacing: 14) {
                    metric("active", health.active, color: VPTheme.success)
                    metric("stale", health.stale, color: VPTheme.warning)
                    metric("pinned", health.pinned, color: VPTheme.accent)
                    metric("evidence", health.evidenceCovered)
                    metric("due", health.verificationDue, color: VPTheme.warning)
                    metric(
                        "conflicts",
                        health.openConflicts,
                        color: health.openConflicts > 0 ? VPTheme.warning : .secondary
                    )
                    Spacer()
                }
                HStack(spacing: 10) {
                    Text(verbatim: "\(health.total) total · \(health.verified) verified")
                    if health.superseded > 0 {
                        Text(verbatim: "\(health.superseded) superseded")
                    }
                    if health.archived > 0 {
                        Text(verbatim: "\(health.archived) archived")
                    }
                    if !health.byKind.isEmpty {
                        Text(
                            verbatim: health.byKind
                                .map { "\($0.key) \($0.count)" }
                                .joined(separator: " · ")
                        )
                        .lineLimit(1)
                    }
                }
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
                if let embedding = model.memoryEmbeddingsStatus {
                    MemoryEmbeddingHealthStrip(status: embedding)
                }
            } else if let status = model.memoryConflictStatus {
                Text(status)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            }

            if model.memoryConflicts.isEmpty {
                Text(
                    model.memoryConflictStatus
                        ?? "No memory conflicts. Scan to detect same-scope active claims with different bodies."
                )
                .font(.system(size: 9, design: .monospaced))
                .foregroundStyle(.secondary)
            } else {
                ForEach(model.memoryConflicts) { conflict in
                    VStack(alignment: .leading, spacing: 5) {
                        HStack(spacing: 6) {
                            Text(conflict.scopeKind.uppercased())
                                .font(.system(size: 8, weight: .medium, design: .monospaced))
                                .foregroundStyle(.cyan)
                                .padding(.horizontal, 5)
                                .padding(.vertical, 1)
                                .background(.cyan.opacity(0.1), in: Capsule())
                            Text(conflict.scopeValue)
                                .font(.system(size: 9, weight: .medium, design: .monospaced))
                            Spacer()
                            Text(conflict.status.uppercased())
                                .font(.system(size: 7.5, weight: .semibold, design: .monospaced))
                                .foregroundStyle(conflict.isOpen ? VPTheme.warning : VPTheme.success)
                            Text(conflict.initial)
                                .font(.system(size: 7.5, design: .monospaced))
                                .foregroundStyle(.tertiary)
                        }
                        HStack(alignment: .top, spacing: 10) {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(
                                    verbatim: "\(conflict.memoryAKind) · \(conflict.memoryATitle)"
                                )
                                .font(.system(size: 8.5, weight: .medium))
                                Text(conflict.memoryAExcerpt)
                                    .font(.system(size: 8, design: .monospaced))
                                    .foregroundStyle(.secondary)
                                    .lineLimit(3)
                            }
                            VStack(alignment: .leading, spacing: 2) {
                                Text(
                                    verbatim: "\(conflict.memoryBKind) · \(conflict.memoryBTitle)"
                                )
                                .font(.system(size: 8.5, weight: .medium))
                                Text(conflict.memoryBExcerpt)
                                    .font(.system(size: 8, design: .monospaced))
                                    .foregroundStyle(.secondary)
                                    .lineLimit(3)
                            }
                        }
                        Text(conflict.reason)
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(VPTheme.accentDeep)
                        if conflict.isOpen {
                            HStack(spacing: 6) {
                                ForEach(MemoryConflictResolution.allCases) { action in
                                    Button(LocalizedStringKey(action.title)) {
                                        requestResolution(conflict, action: action)
                                    }
                                    .buttonStyle(.bordered)
                                    .controlSize(.mini)
                                    .disabled(model.memoryConflictBusy)
                                    .help(action.detail)
                                }
                            }
                        } else if let resolution = conflict.resolution {
                            Text(
                                verbatim: "resolved with \(resolution)"
                                    + (conflict.note.map { " · \($0)" } ?? "")
                            )
                            .font(.system(size: 7.5, design: .monospaced))
                            .foregroundStyle(.tertiary)
                        }
                    }
                    .padding(8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 8))
                }
                if openConflicts.isEmpty, model.memoryConflicts.count > 0 {
                    Text("No open conflicts remain. Resolved rows stay visible as history.")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
            }
            if let status = model.memoryConflictStatus, !model.memoryConflicts.isEmpty {
                Text(status)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
        }
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .vpAppear(index: 1)
        .confirmationDialog(
            LocalizedStringKey(pendingAction.title),
            isPresented: Binding(
                get: { pendingConflict != nil },
                set: {
                    if !$0 {
                        pendingConflict = nil
                    }
                }
            ),
            titleVisibility: .visible
        ) {
            if let conflict = pendingConflict {
                Button(LocalizedStringKey(pendingAction.title), role: .destructive) {
                    Task {
                        await model.resolveMemoryConflict(conflict, action: pendingAction)
                        pendingConflict = nil
                    }
                }
            }
            Button("Cancel", role: .cancel) {
                pendingConflict = nil
            }
        } message: {
            Text(pendingAction.detail)
        }
    }
}

struct QuestionCenterCard: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Questions")
                    Text("Waiting for user input")
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.questionCenterBusy {
                    ProgressView().controlSize(.small)
                }
                Text("\(model.pendingQuestions.count) pending")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
                Button("Refresh") {
                    Task { await model.loadPendingQuestions() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.questionCenterBusy)
            }
            if model.pendingQuestions.isEmpty {
                Text(model.questionCenterStatus ?? "No pending questions.")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            } else {
                ForEach(model.pendingQuestions) { question in
                    PendingQuestionRow(question: question)
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
        .vpAppear(index: 0)
        .task {
            await model.loadPendingQuestions()
        }
    }
}

struct PendingQuestionRow: View {
    @Environment(AppModel.self) private var model
    let question: Question
    @State private var draft = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 6) {
                Text(question.agentName)
                    .font(.system(size: 9, weight: .medium))
                Text(
                    model.sessionDirectory[question.sessionId]?.goal
                        ?? String(question.sessionId.prefix(8))
                )
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
                .lineLimit(1)
                Spacer()
            }
            Text(question.question)
                .font(.system(size: 10))
                .fixedSize(horizontal: false, vertical: true)
            if !question.options.isEmpty {
                HStack(spacing: 6) {
                    ForEach(question.options, id: \.self) { option in
                        Button(option) {
                            Task { await model.answerGlobalQuestion(question.id, answer: option) }
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                    }
                }
            }
            HStack(spacing: 6) {
                TextField("Answer", text: $draft)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 9, design: .monospaced))
                    .onSubmit {
                        submit()
                    }
                Button("Send") {
                    submit()
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(9)
        .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 9))
    }

    private func submit() {
        let answer = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !answer.isEmpty else { return }
        draft = ""
        Task { await model.answerGlobalQuestion(question.id, answer: answer) }
    }
}

struct OrchestrationOverviewCard: View {
    @Environment(AppModel.self) private var model

    private var overview: OrchestrationOverviewResult? {
        model.orchestrationOverview
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Mission control")
                    Text("Cross-session team operations")
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.orchestrationOverviewBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Refresh") {
                    Task { await model.loadOrchestrationOverview() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.orchestrationOverviewBusy)
            }

            if let overview {
                HStack(spacing: 14) {
                    overviewMetric("sessions", overview.totals.sessions, .cyan)
                    overviewMetric("running", overview.totals.running, .green)
                    overviewMetric("attention", overview.totals.attention, .orange)
                    overviewMetric("blocked", overview.totals.blockedTasks, .red)
                    overviewMetric("retrying", overview.totals.retryingTasks, .purple)
                    overviewMetric("conflicts", overview.totals.mergeConflicts, .yellow)
                    Spacer()
                }

                if overview.sessions.isEmpty {
                    Text("No session has a stored TeamPlan yet.")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.tertiary)
                } else {
                    ForEach(overview.sessions.prefix(8)) { session in
                        Button {
                            model.selectProject(session.projectId)
                            model.selectSession(session.sessionId)
                        } label: {
                            HStack(alignment: .top, spacing: 7) {
                                Circle()
                                    .fill(statusColor(session))
                                    .frame(width: 6, height: 6)
                                    .padding(.top, 4)
                                VStack(alignment: .leading, spacing: 2) {
                                    HStack(spacing: 5) {
                                        Text(session.projectName)
                                            .font(.system(size: 8, weight: .semibold, design: .monospaced))
                                            .foregroundStyle(.secondary)
                                        Text(session.goal)
                                            .font(.system(size: 9.5, weight: .medium))
                                            .lineLimit(1)
                                        Spacer()
                                        Text("r\(session.planRevision)")
                                            .font(.system(size: 7.5, design: .monospaced))
                                            .foregroundStyle(.tertiary)
                                    }
                                    HStack(spacing: 8) {
                                        Text(session.status)
                                            .font(.system(size: 7.5, design: .monospaced))
                                            .foregroundStyle(statusColor(session))
                                        Text(
                                            "tasks \(session.metrics.completedTasks)/\(session.metrics.totalTasks)"
                                        )
                                        .font(.system(size: 7.5, design: .monospaced))
                                        .foregroundStyle(.tertiary)
                                        if session.metrics.readyTasks > 0 {
                                            Text("ready \(session.metrics.readyTasks)")
                                                .font(.system(size: 7.5, design: .monospaced))
                                                .foregroundStyle(.green)
                                        }
                                        if session.metrics.blockedTasks > 0 {
                                            Text("blocked \(session.metrics.blockedTasks)")
                                                .font(.system(size: 7.5, design: .monospaced))
                                                .foregroundStyle(.orange)
                                        }
                                        if session.metrics.failedTasks > 0 {
                                            Text("failed \(session.metrics.failedTasks)")
                                                .font(.system(size: 7.5, design: .monospaced))
                                                .foregroundStyle(.red)
                                        }
                                        if session.metrics.retryingTasks > 0 {
                                            Text("retrying \(session.metrics.retryingTasks)")
                                                .font(.system(size: 7.5, design: .monospaced))
                                                .foregroundStyle(.purple)
                                        }
                                    }
                                    if let blocker = session.blockers.first {
                                        Text(blocker)
                                            .font(.system(size: 7.5, design: .monospaced))
                                            .foregroundStyle(.orange)
                                            .lineLimit(1)
                                    } else if let decision = session.lastDecision {
                                        Text(decision)
                                            .font(.system(size: 7.5, design: .monospaced))
                                            .foregroundStyle(.secondary)
                                            .lineLimit(1)
                                    }
                                }
                                Image(systemName: "chevron.right")
                                    .font(.system(size: 7, weight: .semibold))
                                    .foregroundStyle(.tertiary)
                                    .padding(.top, 4)
                            }
                            .padding(.horizontal, 7)
                            .padding(.vertical, 5)
                            .background(.white.opacity(0.025), in: RoundedRectangle(cornerRadius: 7))
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                    }
                }
            } else if let status = model.orchestrationOverviewStatus {
                Text(status)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
            } else {
                ProgressView().controlSize(.small)
            }

            Text(
                "Mission control aggregates only stored TeamPlans and their live task projections. It is a monitor: task actions and plan revisions stay in Collab Ops, the task queue, or the CLI."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)
        }
        .padding(12)
        .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 12))
        .onAppear {
            Task { await model.loadOrchestrationOverview() }
        }
    }

    private func statusColor(_ session: OrchestrationSessionSummary) -> Color {
        if session.metrics.attentionCount > 0 {
            return .orange
        }
        switch session.status {
        case "running", "queued":
            return .green
        case "failed", "cancelled":
            return .red
        default:
            return .secondary
        }
    }

    private func overviewMetric(_ label: String, _ value: Int, _ tint: Color) -> some View {
        HStack(spacing: 4) {
            Circle().fill(tint).frame(width: 5, height: 5)
            Text("\(value) \(label)")
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.secondary)
        }
    }
}

struct RuntimeLeaseCard: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Runtime leases")
                    Text("Cross-process session and integration ownership")
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.runtimeLeasesBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Refresh") {
                    Task { await model.loadRuntimeLeases() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.runtimeLeasesBusy)
                Button("Sweep expired") {
                    Task { await model.sweepRuntimeLeases() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.runtimeLeasesBusy)
            }

            HStack(spacing: 14) {
                leaseCount("sessions", model.runtimeLeases.filter { $0.kind == "session" }.count, tint: .cyan)
                leaseCount(
                    "integration",
                    model.runtimeLeases.filter { $0.kind != "session" }.count,
                    tint: .orange
                )
                if let owner = model.runtimeLeaseOwnerID {
                    Text("daemon \(owner.prefix(8))")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
                Spacer()
            }

            if model.runtimeLeases.isEmpty {
                Text("No runtime lease is held. Start a session run to see its heartbeat.")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
            } else {
                ForEach(model.runtimeLeases.prefix(8)) { lease in
                    HStack(spacing: 7) {
                        Image(systemName: lease.icon)
                            .font(.system(size: 8))
                            .foregroundStyle(lease.kind == "session" ? .cyan : .orange)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(lease.leaseKey)
                                .font(.system(size: 9, weight: .medium, design: .monospaced))
                                .lineLimit(1)
                            Text(
                                "\(lease.kind) · owner \(lease.ownerId.prefix(8)) · heartbeat \(VPTheme.relativeTime(lease.heartbeatAt))"
                            )
                            .font(.system(size: 7.5, design: .monospaced))
                            .foregroundStyle(.tertiary)
                        }
                        Spacer()
                        Text("expires \(VPTheme.relativeTime(lease.expiresAt))")
                            .font(.system(size: 7.5, design: .monospaced))
                            .foregroundStyle(.secondary)
                    }
                    .padding(.horizontal, 7)
                    .padding(.vertical, 4)
                    .background(.white.opacity(0.025), in: RoundedRectangle(cornerRadius: 6))
                }
            }

            if let status = model.runtimeLeasesStatus {
                Text(status)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
            }

            Text(
                "The daemon holds a SQLite lease with a heartbeat for each session run. A second process pointed at the same data directory cannot take an unexpired lease, while a crashed process is recoverable after the TTL. Expired leases are swept by the daemon tick and can be removed here."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
        }
        .padding(12)
        .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 12))
        .onAppear {
            Task { await model.loadRuntimeLeases() }
        }
    }

    private func leaseCount(_ label: String, _ count: Int, tint: Color) -> some View {
        HStack(spacing: 4) {
            Circle().fill(tint).frame(width: 5, height: 5)
            Text("\(count) \(label)")
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.secondary)
        }
    }
}

struct OrchestrationPolicyCard: View {
    @Environment(AppModel.self) private var model
    @State private var draft = OrchestrationPolicy.standard
    @State private var loaded = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Orchestration")
                    Text("Deterministic retries, dependencies, and escalations")
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.orchestrationPolicyBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Reload") {
                    Task {
                        await model.loadOrchestrationPolicy()
                        draft = model.orchestrationPolicy
                    }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
            }

            OrchestrationPolicyEditor(policy: $draft)

            HStack {
                if let status = model.orchestrationPolicyStatus {
                    Text(status)
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button("Save orchestration policy") {
                    Task { _ = await model.saveOrchestrationPolicy(draft) }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.orchestrationPolicyBusy)
            }

            Text(
                "The controller retries only tasks that failed, with exponential backoff stored in the task queue. When the retry budget is exhausted it can escalate a critical note and apply the dependency policy to downstream tasks. These are scheduling rules; model selection remains explicit and project-scoped."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)
        }
        .padding(12)
        .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 12))
        .onAppear {
            draft = model.orchestrationPolicy
            loaded = true
        }
        .onChange(of: model.orchestrationPolicy) { _, policy in
            if loaded {
                draft = policy
            }
        }
    }
}

struct IntakeTasksCard: View {
    @Environment(AppModel.self) private var model
    @State private var showComposer = false
    @State private var pendingDelete: IntakeTask?

    private var activeCount: Int {
        model.intakeTasks.count(where: \.isActive)
    }

    private var queuedCount: Int {
        model.intakeTasks.count(where: { $0.status == "queued" })
    }

    private var completedCount: Int {
        model.intakeTasks.count(where: { $0.status == "completed" })
    }

    private var failedCount: Int {
        model.intakeTasks.count(where: {
            ["failed", "interrupted", "blocked"].contains($0.status)
        })
    }

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Task inbox")
                    Text("Standalone work without a stored TeamPlan")
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.intakeBusy {
                    ProgressView().controlSize(.small)
                }
                Button(showComposer ? "Hide composer" : "New task") {
                    withAnimation(VPMotion.smooth) {
                        showComposer.toggle()
                    }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                Button("Refresh") {
                    Task { await model.loadIntakeTasks() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.intakeBusy)
            }

            HStack(spacing: 14) {
                intakeMetric("queued", queuedCount)
                intakeMetric("active", activeCount)
                intakeMetric(
                    "parallel cap",
                    Int(model.taskSchedulerMaxIntakeParallelText) ?? 0
                )
                intakeMetric("completed", completedCount)
                intakeMetric("failed/blocked", failedCount)
                Spacer()
                Text(
                    model.taskSchedulerPolicy.enabled
                        ? (model.taskSchedulerIncludeIntake
                            ? "auto-dispatch on"
                            : "auto-dispatch off")
                        : "scheduler off"
                )
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.secondary)
                Button(model.taskSchedulerIncludeIntake ? "Pause intake" : "Enable intake") {
                    let enabling = !model.taskSchedulerIncludeIntake
                    model.taskSchedulerIncludeIntake = enabling
                    if enabling {
                        model.taskSchedulerEnabled = true
                    }
                    Task { await model.saveTaskSchedulerPolicy() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.taskSchedulerPolicyBusy)
            }

            Text(
                "Each intake item becomes a deterministic one-task TeamPlan in a fresh session worktree. It uses the selected profile, the normal Worker → Verifier → Integrator path, and the project's global write leases. Auto-dispatch is default-on and bounded by the project intake parallelism; Run now always overrides the toggle."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)

            if let status = model.intakeStatus {
                Text(status)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
            }

            if showComposer {
                composer
                    .transition(.vpContent)
            }

            if model.intakeTasks.isEmpty {
                Text("No standalone tasks yet. New task starts from a title and a clear instruction.")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
            } else {
                ForEach(model.intakeTasks.prefix(12)) { task in
                    taskRow(task)
                }
                if model.intakeTasks.count > 12 {
                    Text("+\(model.intakeTasks.count - 12) more intake tasks")
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
        .vpAppear(index: 0)
        .confirmationDialog(
            "Delete this intake task?",
            isPresented: Binding(
                get: { pendingDelete != nil },
                set: {
                    if !$0 {
                        pendingDelete = nil
                    }
                }
            )
        ) {
            Button("Delete", role: .destructive) {
                if let task = pendingDelete {
                    Task { await model.deleteIntakeTask(task) }
                }
                pendingDelete = nil
            }
            Button("Cancel", role: .cancel) { pendingDelete = nil }
        } message: {
            Text(pendingDelete?.title ?? "")
        }
        .task(id: model.selectedProjectID) {
            await model.loadTaskSchedulerPolicy()
            var silent = false
            while !Task.isCancelled {
                await model.loadIntakeTasks(silent: silent)
                await model.loadSchedulerStatus(silent: silent)
                silent = true
                try? await Task.sleep(for: .seconds(6))
            }
        }
    }

    private var composer: some View {
        @Bindable var model = model
        return VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                Text(model.intakeEditingID == nil ? "Queue a standalone task" : "Edit intake task")
                    .font(.system(size: 9, weight: .semibold, design: .monospaced))
                    .foregroundStyle(.secondary)
                Spacer()
                if model.intakeEditingID != nil {
                    Button("Cancel edit") {
                        model.cancelIntakeEdit()
                    }
                    .buttonStyle(.plain)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
                }
            }
            HStack(spacing: 8) {
                TextField("Task title", text: $model.intakeDraftTitle)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 11, weight: .medium))
                Picker("Profile", selection: $model.intakeDraftProfile) {
                    ForEach(["coding", "research", "verification", "generic"], id: \.self) {
                        Text($0).tag($0)
                    }
                }
                .pickerStyle(.menu)
                .controlSize(.small)
                .frame(width: 110)
                TextField("kind (optional)", text: $model.intakeDraftKind)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 9, design: .monospaced))
                    .frame(width: 130)
                TextField("p", text: $model.intakeDraftPriorityText)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 9, design: .monospaced))
                    .frame(width: 44)
                    .help("Priority -100…100")
            }
            TextEditor(text: $model.intakeDraftInstructions)
                .font(.system(size: 10, design: .monospaced))
                .scrollContentBackground(.hidden)
                .frame(minHeight: 58, maxHeight: 90)
                .padding(4)
                .background(.black.opacity(0.16), in: RoundedRectangle(cornerRadius: 6))
                .overlay {
                    RoundedRectangle(cornerRadius: 6)
                        .strokeBorder(.white.opacity(0.08), lineWidth: 1)
                }
                .overlay(alignment: .topLeading) {
                    if model.intakeDraftInstructions.isEmpty {
                        Text("What should the team do? Include constraints and verification.")
                            .font(.system(size: 9, design: .monospaced))
                            .foregroundStyle(.tertiary)
                            .padding(.horizontal, 8)
                            .padding(.vertical, 8)
                            .allowsHitTesting(false)
                    }
                }
            HStack(spacing: 8) {
                TextField("write scope: src/a, docs", text: $model.intakeDraftScope)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 9, design: .monospaced))
                TextField("acceptance: tests pass, diff reviewed", text: $model.intakeDraftAcceptance)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 9, design: .monospaced))
            }
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 6) {
                    Text("Dependencies")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.secondary)
                    Text("must complete first")
                        .font(.system(size: 7, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    if !model.intakeDraftDeps.isEmpty {
                        Text("\(model.intakeDraftDeps.count) selected")
                            .font(.system(size: 7, design: .monospaced))
                            .foregroundStyle(.cyan)
                    }
                }
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 4) {
                        let candidates = model.intakeTasks
                            .filter { $0.id != model.intakeEditingID }
                            .prefix(24)
                        if candidates.isEmpty {
                            Text("No other intake tasks to depend on.")
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(.tertiary)
                        }
                        ForEach(Array(candidates)) { candidate in
                            let selected = model.intakeDraftDeps.contains(candidate.id)
                            Button {
                                model.toggleIntakeDependency(candidate.id)
                            } label: {
                                HStack(spacing: 3) {
                                    Image(systemName: selected
                                        ? "checkmark.circle.fill"
                                        : "circle")
                                        .font(.system(size: 8))
                                    Text(candidate.title)
                                        .font(.system(size: 7.5, design: .monospaced))
                                        .lineLimit(1)
                                    Text(candidate.status)
                                        .font(.system(size: 6.5, design: .monospaced))
                                        .foregroundStyle(.tertiary)
                                }
                                .foregroundStyle(selected ? .cyan : .secondary)
                                .padding(.horizontal, 6)
                                .padding(.vertical, 3)
                                .background(
                                    selected ? Color.cyan.opacity(0.10) : Color.white.opacity(0.03),
                                    in: Capsule()
                                )
                            }
                            .buttonStyle(.plain)
                        }
                    }
                }
            }
            HStack(spacing: 8) {
                TextField("labels", text: $model.intakeDraftLabels)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 9, design: .monospaced))
                    .frame(width: 150)
                TextField("attempts", text: $model.intakeDraftMaxAttemptsText)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 9, design: .monospaced))
                    .frame(width: 64)
                TextField("token budget", text: $model.intakeDraftTokenBudget)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 9, design: .monospaced))
                    .frame(width: 100)
                TextField("USD", text: $model.intakeDraftCostBudget)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 9, design: .monospaced))
                    .frame(width: 70)
                TextField("secs", text: $model.intakeDraftTimeBudget)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 9, design: .monospaced))
                    .frame(width: 70)
                Spacer()
                Button(model.intakeEditingID == nil ? "Queue task" : "Save changes") {
                    Task { await model.createIntakeTask() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.intakeBusy)
            }
        }
        .padding(10)
        .background(.white.opacity(0.025), in: RoundedRectangle(cornerRadius: 10))
    }

    private func taskRow(_ task: IntakeTask) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Circle()
                .fill(intakeStatusColor(task.status))
                .frame(width: 6, height: 6)
                .padding(.top, 4)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(task.title)
                        .font(.system(size: 10, weight: .medium))
                        .lineLimit(1)
                    Text(task.profile)
                        .font(.system(size: 7, design: .monospaced))
                        .foregroundStyle(.secondary)
                    Text("p\(task.priority)")
                        .font(.system(size: 7, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    Text("\(task.attempts)/\(task.maxAttempts)")
                        .font(.system(size: 7, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
                Text(task.instructions)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                if let error = task.lastError {
                    Text(error)
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.orange)
                        .lineLimit(2)
                } else if let summary = task.resultSummary {
                    Text(summary)
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                        .lineLimit(2)
                }
                if !task.writeScope.isEmpty {
                    Text("scope: \(task.writeScope.joined(separator: ", "))")
                        .font(.system(size: 7, design: .monospaced))
                        .foregroundStyle(.tertiary)
                        .lineLimit(1)
                }
                if !task.deps.isEmpty {
                    Text("waiting on: \(dependencySummary(task))")
                        .font(.system(size: 7, design: .monospaced))
                        .foregroundStyle(.tertiary)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 0)
            Text(task.status.uppercased())
                .font(.system(size: 7, weight: .semibold, design: .monospaced))
                .foregroundStyle(intakeStatusColor(task.status))
            if let sessionID = task.sessionID {
                Button {
                    model.selectSession(sessionID)
                } label: {
                    Image(systemName: "arrow.up.forward.app")
                        .font(.system(size: 9))
                }
                .buttonStyle(.plain)
                .foregroundStyle(.cyan)
                .help("Open the task session")
            }
            if model.intakeActionBusyID == task.id {
                ProgressView().controlSize(.mini).frame(width: 18)
            } else {
                if canRun(task) {
                    actionButton("play.circle.fill", tint: .green, help: "Run now") {
                        Task { await model.dispatchIntakeTask(task) }
                    }
                }
                if canCancel(task) {
                    actionButton("stop.circle.fill", tint: .red, help: "Cancel") {
                        Task { await model.cancelIntakeTask(task) }
                    }
                }
                if canRetry(task) {
                    actionButton("arrow.clockwise", tint: .cyan, help: "Retry") {
                        Task { await model.retryIntakeTask(task) }
                    }
                }
                if canEdit(task) {
                    actionButton("pencil", tint: .yellow, help: "Edit and re-queue") {
                        model.beginIntakeEdit(task)
                        withAnimation(VPMotion.smooth) {
                            showComposer = true
                        }
                    }
                }
                if task.isTerminal {
                    actionButton("trash", tint: .secondary, help: "Delete") {
                        pendingDelete = task
                    }
                }
            }
        }
        .padding(.vertical, 4)
        .padding(.horizontal, 6)
        .background(.white.opacity(0.02), in: RoundedRectangle(cornerRadius: 7))
    }

    private func actionButton(
        _ icon: String,
        tint: Color,
        help: String,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Image(systemName: icon)
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(tint)
                .frame(width: 22, height: 22)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(help)
    }

    private func intakeMetric(_ title: String, _ value: Int) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text("\(value)")
                .font(.system(size: 12, weight: .semibold, design: .rounded))
            Text(title)
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.secondary)
        }
    }

    private func canEdit(_ task: IntakeTask) -> Bool {
        ["queued", "failed", "interrupted", "cancelled"].contains(task.status)
    }

    private func dependencySummary(_ task: IntakeTask) -> String {
        task.deps
            .map { dep in
                if let match = model.intakeTasks.first(where: { $0.id == dep }) {
                    return "\(match.title) (\(match.status))"
                }
                return String(dep.prefix(8))
            }
            .joined(separator: ", ")
    }

    private func canRun(_ task: IntakeTask) -> Bool {
        ["queued", "failed", "interrupted", "cancelled"].contains(task.status)
    }

    private func canCancel(_ task: IntakeTask) -> Bool {
        ["queued", "failed", "interrupted", "running", "dispatching"].contains(task.status)
    }

    private func canRetry(_ task: IntakeTask) -> Bool {
        ["failed", "interrupted", "cancelled"].contains(task.status)
    }

    private func intakeStatusColor(_ status: String) -> Color {
        switch status {
        case "running", "dispatching": .green
        case "completed": .cyan
        case "failed", "interrupted", "blocked": .orange
        case "cancelled": .secondary
        default: .yellow
        }
    }
}

struct TaskSchedulerCard: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Task scheduler")
                    Text("Persistent global task-DAG queue")
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.taskQueueBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Refresh") {
                    Task { await model.loadTaskQueue() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.taskQueueBusy)
            }

            HStack(spacing: 12) {
                Toggle("Auto-resume pending plans", isOn: $model.taskSchedulerEnabled)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                Toggle("One task per tick", isOn: $model.taskSchedulerDispatchTasks)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                    .help(
                        "Dispatch one ready task per daemon tick instead of resuming the entire stored plan"
                    )
                Toggle("Include intake", isOn: $model.taskSchedulerIncludeIntake)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                    .help("Let the daemon dispatch standalone task-inbox items")
                TextField("intake max", text: $model.taskSchedulerMaxIntakeParallelText)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 9, design: .monospaced))
                    .frame(width: 58)
                TextField("cooldown secs", text: $model.taskSchedulerCooldownText)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 9, design: .monospaced))
                    .frame(width: 90)
                Text("1–86400s")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
                if model.taskSchedulerPolicyBusy {
                    ProgressView().controlSize(.small)
                }
                Spacer()
                Button("Dispatch one task") {
                    Task { await model.dispatchScheduledTaskNow() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.taskQueueBusy || model.taskActionBusyID != nil)
                Button("Save policy") {
                    Task { await model.saveTaskSchedulerPolicy() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.taskSchedulerPolicyBusy)
            }

            if let status = model.taskSchedulerPolicyStatus {
                Text(status)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
            }

            Text(
                "The autonomous dispatcher is default-on for new projects. Ready plan tasks are ordered by task priority plus session priority with aging; intake items use the same ordering, project round-robin, dependency gates, and global write leases. Auto-resume can continue idle/failed plans after the cooldown. One-task-per-tick claims a single ready task, then releases the session; otherwise the whole stored plan resumes. The scheduler never bypasses configured budgets or model choices."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)

            if let status = model.schedulerStatus {
                HStack(spacing: 14) {
                    schedulerMetric("active", status.activeSessions)
                    schedulerMetric("queued", status.queuedSessions)
                    schedulerMetric("paused", status.pausedSessions)
                    schedulerMetric(
                        "session cap",
                        status.globalSessionCap
                    )
                    schedulerMetric("workers", status.globalWorkerCap)
                    schedulerMetric("labour", status.globalLabourCap)
                    schedulerMetric("plan pending", status.pendingPlanTasks)
                    schedulerMetric("intake active", status.activeIntake)
                    Spacer()
                    Button("Refresh status") {
                        Task { await model.loadSchedulerStatus() }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(model.schedulerStatusBusy)
                }
                if let project = status.projects.first {
                    Text(
                        "Project: enabled=\(project.enabled ? "yes" : "no") · intake=\(project.includeIntake ? "yes" : "no") · parallel cap \(project.maxIntakeParallel) · cooldown \(project.cooldownSecs)s · pending sessions \(project.pendingSessions)"
                    )
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
                }
            } else if let error = model.schedulerStatusError {
                Text(error)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.orange)
            } else {
                ProgressView().controlSize(.small)
            }

            if let queue = model.taskQueue {
                HStack(spacing: 14) {
                    schedulerCount("queued", queue.tasks.count(where: { $0.status == "pending" }))
                    schedulerCount("running", queue.tasks.count(where: { $0.status == "running" }))
                    schedulerCount("completed", queue.tasks.count(where: { $0.status == "completed" }))
                    schedulerCount("blocked", queue.tasks.count(where: { $0.status == "blocked" }))
                    schedulerCount(
                        "paused",
                        queue.tasks.count(where: { $0.status == "pausing" || $0.status == "paused" })
                    )
                    schedulerCount("skipped", queue.tasks.count(where: { $0.status == "skipped" }))
                    schedulerCount("leases", queue.leases.count)
                    Spacer()
                }
                if let status = model.taskActionStatus {
                    Text(status)
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
                if queue.tasks.isEmpty {
                    Text("No plan tasks are queued for the selected project.")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.tertiary)
                } else {
                    ForEach(queue.tasks.prefix(8)) { task in
                        HStack(spacing: 7) {
                            Circle()
                                .fill(statusColor(task.status))
                                .frame(width: 6, height: 6)
                            Text(task.taskID)
                                .font(.system(size: 9, weight: .medium, design: .monospaced))
                                .frame(width: 70, alignment: .leading)
                            Text(task.title)
                                .font(.system(size: 9))
                                .lineLimit(1)
                            Spacer()
                            Text(task.department)
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(.secondary)
                            Text("p\(task.priority)")
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(.tertiary)
                            Text(task.status)
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(statusColor(task.status))
                            if model.taskActionBusyID == task.id {
                                ProgressView()
                                    .controlSize(.mini)
                                    .frame(width: 16)
                            } else {
                                if canPause(task.status) {
                                    Button {
                                        Task { await model.pauseRunningTask(task) }
                                    } label: {
                                        Image(systemName: "pause.circle")
                                            .font(.system(size: 8))
                                    }
                                    .buttonStyle(.plain)
                                    .foregroundStyle(.yellow)
                                    .help("Pause this task at its next turn/tool boundary")
                                }
                                if canResume(task.status) {
                                    Button {
                                        Task { await model.resumePausedTask(task) }
                                    } label: {
                                        Image(systemName: "play.circle")
                                            .font(.system(size: 8))
                                    }
                                    .buttonStyle(.plain)
                                    .foregroundStyle(.green)
                                    .help("Resume this paused task")
                                }
                                if canCancel(task.status) {
                                    Button {
                                        Task { await model.cancelRunningTask(task) }
                                    } label: {
                                        Image(systemName: "stop.circle")
                                            .font(.system(size: 8))
                                    }
                                    .buttonStyle(.plain)
                                    .foregroundStyle(.red)
                                    .help("Request cancellation of this running task")
                                }
                                if canRetry(task.status) {
                                    Button {
                                        Task { await model.retryScheduledTask(task) }
                                    } label: {
                                        Image(systemName: "arrow.clockwise")
                                            .font(.system(size: 8))
                                    }
                                    .buttonStyle(.plain)
                                    .foregroundStyle(.cyan)
                                    .help("Retry this task")
                                }
                                if canSkip(task.status) {
                                    Button {
                                        Task { await model.skipScheduledTask(task) }
                                    } label: {
                                        Image(systemName: "forward.end")
                                            .font(.system(size: 8))
                                    }
                                    .buttonStyle(.plain)
                                    .foregroundStyle(.orange)
                                    .help("Skip this queued task")
                                }
                            }
                        }
                        .padding(.vertical, 2)
                    }
                    if queue.tasks.count > 8 {
                        Text("+\(queue.tasks.count - 8) more tasks")
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.tertiary)
                    }
                }
                if !queue.leases.isEmpty {
                    Divider().opacity(0.25)
                    Text("Write leases")
                        .font(.system(size: 8, weight: .medium, design: .monospaced))
                        .foregroundStyle(.secondary)
                    ForEach(queue.leases.prefix(5)) { lease in
                        HStack(spacing: 7) {
                            Image(systemName: "lock")
                                .font(.system(size: 8))
                                .foregroundStyle(.cyan)
                            Text(lease.agentID)
                                .font(.system(size: 8, design: .monospaced))
                                .lineLimit(1)
                            Text(lease.scopes.isEmpty ? "unrestricted" : lease.scopes.joined(separator: ", "))
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                            Spacer()
                        }
                    }
                }
            } else {
                Text(model.taskQueueStatus ?? "Loading scheduler state…")
                    .font(.system(size: 9, design: .monospaced))
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
        .vpAppear(index: 0)
        .task(id: model.selectedProjectID) {
            await model.loadTaskQueue()
            await model.loadTaskSchedulerPolicy()
            await model.loadSchedulerStatus()
        }
    }

    private func schedulerMetric(_ title: String, _ value: Int) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text("\(value)")
                .font(.system(size: 11, weight: .semibold, design: .rounded))
                .monospacedDigit()
            Text(title)
                .font(.system(size: 7, design: .monospaced))
                .foregroundStyle(.secondary)
        }
    }

    private func schedulerCount(_ title: String, _ value: Int) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text("\(value)")
                .font(.system(size: 12, weight: .semibold, design: .rounded))
            Text(title)
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.secondary)
        }
    }

    private func canPause(_ status: String) -> Bool {
        status == "running"
    }

    private func canResume(_ status: String) -> Bool {
        ["pausing", "paused"].contains(status)
    }

    private func canCancel(_ status: String) -> Bool {
        ["running", "pausing", "paused"].contains(status)
    }

    private func canRetry(_ status: String) -> Bool {
        ["failed", "blocked", "interrupted", "cancelled", "skipped"].contains(status)
    }

    private func canSkip(_ status: String) -> Bool {
        ["pending", "failed", "blocked", "interrupted", "cancelled"].contains(status)
    }

    private func statusColor(_ status: String) -> Color {
        switch status {
        case "running": .green
        case "completed": .cyan
        case "failed", "blocked": .orange
        case "interrupted", "cancelling", "pausing", "paused": .yellow
        case "cancelled", "skipped": .secondary
        default: .secondary
        }
    }
}

struct ScalingCard: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Deterministic scaling")
                    Text("Bounded Worker/Labour slots · Managers stay plan-owned")
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.scalingPolicyBusy || model.scalingStatusBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Refresh") {
                    Task {
                        await model.loadScalingPolicy()
                        await model.loadScalingStatus()
                    }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                Toggle("Enabled", isOn: $model.scalingEnabled)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                Button("Save policy") {
                    Task { await model.saveScalingPolicy() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.scalingPolicyBusy)
            }

            HStack(spacing: 10) {
                scalingField("min W", text: $model.scalingMinWorkersText, width: 46)
                scalingField("max W", text: $model.scalingMaxWorkersText, width: 46)
                scalingField("min L", text: $model.scalingMinLabourText, width: 46)
                scalingField("max L", text: $model.scalingMaxLabourText, width: 46)
                scalingField("threshold", text: $model.scalingReadyThresholdText, width: 62)
                scalingField("critical s", text: $model.scalingCriticalDelayText, width: 72)
                scalingField("idle s", text: $model.scalingIdleSecsText, width: 64)
                scalingField("cooldown s", text: $model.scalingCooldownText, width: 76)
                scalingField("step", text: $model.scalingMaxStepText, width: 46)
                scalingField("budget ≥", text: $model.scalingBudgetRatioText, width: 58)
            }

            if let status = model.scalingPolicyStatus {
                Text(status)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            }

            Text(
                "Scaling is opt-in and evaluated by a deterministic daemon tick. Expansion needs ready work, saturated slots, the ready threshold, cooldown, plan demand, global capacity, and the optional budget headroom. Scale-down waits for the idle window, reduces Labour before Workers, and never goes below active agents, plan demand, or the minimums. A Manager recommendation is evidence only: the controller never creates Managers or rewrites the plan."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)

            HStack(spacing: 12) {
                Button("Preview selected session") {
                    Task { await model.previewScalingNow() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.scalingPreviewBusy || model.selectedSessionID == nil)
                if model.scalingPreviewBusy {
                    ProgressView().controlSize(.small)
                }
                Spacer()
                if let status = model.scalingStatus {
                    scalingMetric("worker cap", status.globalWorkerCap)
                    scalingMetric("worker free", status.globalWorkerAvailable)
                    scalingMetric("labour cap", status.globalLabourCap)
                    scalingMetric("labour free", status.globalLabourAvailable)
                }
            }

            if let preview = model.scalingPreview {
                VStack(alignment: .leading, spacing: 3) {
                    Text("Preview · \(preview.decision.summary)")
                        .font(.system(size: 9, weight: .medium, design: .monospaced))
                    Text(scalingPreviewDetails(preview.decision))
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    if !preview.decision.reasons.isEmpty {
                        Text(preview.decision.reasons.joined(separator: " | "))
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.secondary)
                            .lineLimit(3)
                    }
                    if let evidence = preview.decision.managerEvidence {
                        Text(evidence)
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.orange)
                    }
                }
                .padding(8)
                .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 8))
            }

            if let status = model.scalingStatus {
                if status.projects.allSatisfy({ !$0.policy.enabled }) {
                    Text("Scaling is disabled for the selected project.")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
                ForEach(status.projects) { project in
                    VStack(alignment: .leading, spacing: 4) {
                        Text("\(project.projectName) · \(project.policy.summary)")
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.secondary)
                        ForEach(project.sessions.prefix(8)) { session in
                            HStack(spacing: 7) {
                                Circle()
                                    .fill(scalingActionColor(session.latest))
                                    .frame(width: 6, height: 6)
                                Text(session.goal.isEmpty ? session.sessionID : session.goal)
                                    .font(.system(size: 9))
                                    .lineLimit(1)
                                Spacer()
                                Text("\(session.currentWorkers)W/\(session.currentLabour)L")
                                    .font(.system(size: 8, design: .monospaced))
                                Text("\(session.readyTasks) ready")
                                    .font(.system(size: 8, design: .monospaced))
                                    .foregroundStyle(.secondary)
                                Text(session.latest?.action ?? "none")
                                    .font(.system(size: 8, design: .monospaced))
                                    .foregroundStyle(scalingActionColor(session.latest))
                            }
                        }
                    }
                }
            } else if let error = model.scalingStatusError {
                Text(error)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.orange)
            }
        }
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .vpAppear(index: 0)
        .task(id: model.selectedProjectID) {
            await model.loadScalingPolicy()
            await model.loadScalingStatus()
        }
    }

    private func scalingField(
        _ title: String,
        text: Binding<String>,
        width: CGFloat
    ) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(title)
                .font(.system(size: 7, design: .monospaced))
                .foregroundStyle(.tertiary)
            TextField(title, text: text)
                .textFieldStyle(.roundedBorder)
                .font(.system(size: 9, design: .monospaced))
                .frame(width: width)
        }
    }

    private func scalingPreviewDetails(_ decision: ScalingDecision) -> String {
        let hash = String(decision.inputsHash.prefix(12))
        let ready = decision.readyTasks
        let running = decision.runningTasks
        let revision = decision.planRevision
        return "ready \(ready) · running \(running) · revision \(revision) · hash \(hash)"
    }

    private func scalingActionColor(_ decision: ScalingDecision?) -> Color {
        if decision?.isExpansion == true {
            return .cyan
        }
        if decision?.isScaleDown == true {
            return .orange
        }
        return .secondary
    }

    private func scalingMetric(_ title: String, _ value: Int) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text("\(value)")
                .font(.system(size: 11, weight: .semibold, design: .rounded))
                .monospacedDigit()
            Text(title)
                .font(.system(size: 7, design: .monospaced))
                .foregroundStyle(.secondary)
        }
    }
}

struct AgentsCard: View {
    @Environment(AppModel.self) private var model
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Agents")
                    Text(model.selectedSession?.goal ?? "No session selected")
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                        .lineLimit(1)
                }
                Spacer()
                if !model.agents.isEmpty {
                    Text("\(model.agents.count)")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
                Button("Refresh") {
                    Task { await model.loadAgents() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.selectedSessionID == nil)
            }
            if model.selectedSessionID == nil {
                Text("Select a session to inspect its agent roster.")
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.tertiary)
            } else if model.agents.isEmpty {
                Text("No agents recorded yet.")
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.tertiary)
            } else {
                VStack(spacing: 6) {
                    ForEach(model.agents) { agent in
                        HStack(spacing: 8) {
                            Image(systemName: icon(for: agent.role))
                                .font(.system(size: 10))
                                .foregroundStyle(color(for: agent.status))
                                .frame(width: 14)
                            VStack(alignment: .leading, spacing: 1) {
                                Text(agent.title ?? agent.name)
                                    .font(.system(size: 10, weight: .medium, design: .monospaced))
                                    .lineLimit(1)
                                Text(
                                    "\(agent.role) · \(agent.profile ?? "generic") · \(agent.model)"
                                )
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                            }
                            Spacer()
                            Text(agent.status)
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(color(for: agent.status))
                        }
                        .padding(8)
                        .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 8))
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
        .vpAppear(index: 0)
    }

    private func icon(for role: String) -> String {
        switch role {
        case "leader": "crown"
        case "manager": "person.badge.clock"
        case "worker": "hammer"
        case "verifier": "checkmark.shield"
        case "labour": "hand.raised"
        default: "person"
        }
    }

    private func color(for status: String) -> Color {
        switch status {
        case "running": .green
        case "waiting_input": .orange
        case "failed": .red
        case "cancelled": .secondary
        default: .secondary
        }
    }
}

struct ProgramsCard: View {
    @Environment(AppModel.self) private var model
    @State private var showComposer = false
    @State private var showDetail = false
    @State private var pendingDelete: ProjectProgram?

    private var activeCount: Int {
        model.programEntries.count(where: { $0.program.status == "active" })
    }

    private var activeMilestones: Int {
        model.programEntries.reduce(0) { $0 + $1.counts.active }
    }

    private var completedMilestones: Int {
        model.programEntries.reduce(0) { $0 + $1.counts.completed }
    }

    private var blockedMilestones: Int {
        model.programEntries.reduce(0) { $0 + $1.counts.blocked }
    }

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Programs")
                    Text("Project-scoped milestones across normal sessions")
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.programBusy || model.programDetailBusy {
                    ProgressView().controlSize(.small)
                }
                Button(showComposer ? "Hide composer" : "New program") {
                    withAnimation(VPMotion.smooth) {
                        showComposer.toggle()
                    }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                Button("Refresh") {
                    Task {
                        await model.loadPrograms()
                        await model.loadProgramDetail()
                    }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.programBusy)
            }

            HStack(spacing: 16) {
                programMetric("active", activeCount)
                programMetric("milestones running", activeMilestones)
                programMetric("completed", completedMilestones)
                programMetric("blocked", blockedMilestones)
                Spacer()
                Text("deterministic controller · no hidden routing")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }

            Text(
                "A milestone starts a normal session with a one-task TeamPlan. Admission, budgets, capacity, approvals, sandboxing, the Verifier, the Integrator, and the merge queue are unchanged. Completion needs an idle session, a completed plan task, integration evidence, and recorded acceptance."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)

            if let status = model.programStatus {
                Text(status)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
            }

            if showComposer {
                composer
                    .transition(.vpContent)
            }

            if model.programEntries.isEmpty {
                Text("No programs yet. A draft program is a safe way to stage milestones before activation.")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
            } else {
                ForEach(model.programEntries.prefix(10)) { entry in
                    programRow(entry)
                }
                if model.programEntries.count > 10 {
                    Text("+\(model.programEntries.count - 10) more programs")
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
        .vpAppear(index: 0)
        .sheet(isPresented: $showDetail) {
            ProgramDetailSheet()
                .environment(model)
        }
        .confirmationDialog(
            "Delete this program?",
            isPresented: Binding(
                get: { pendingDelete != nil },
                set: {
                    if !$0 {
                        pendingDelete = nil
                    }
                }
            )
        ) {
            Button("Delete", role: .destructive) {
                if let program = pendingDelete {
                    Task { await model.deleteProgram(program) }
                }
                pendingDelete = nil
            }
            Button("Cancel", role: .cancel) { pendingDelete = nil }
        } message: {
            Text(pendingDelete?.goal ?? "")
        }
        .task(id: model.selectedProjectID) {
            var silent = false
            while !Task.isCancelled {
                await model.loadPrograms(silent: silent)
                if model.selectedProgramID != nil, showDetail {
                    await model.loadProgramDetail(silent: true)
                }
                silent = true
                try? await Task.sleep(for: .seconds(6))
            }
        }
    }

    private var composer: some View {
        @Bindable var model = model
        return VStack(alignment: .leading, spacing: 8) {
            Text("Stage a program")
                .font(.system(size: 9, weight: .semibold, design: .monospaced))
                .foregroundStyle(.secondary)
            TextField("Program goal", text: $model.programDraftGoal)
                .textFieldStyle(.roundedBorder)
                .font(.system(size: 11, weight: .medium))
            HStack(spacing: 8) {
                TextField("Token budget", text: $model.programDraftTokenBudget)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 120)
                TextField("USD budget", text: $model.programDraftCostBudget)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 100)
                TextField("Attempts", text: $model.programDraftMaxAttemptsText)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 70)
                TextField("Backoff s", text: $model.programDraftBackoffText)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 85)
                TextField("Parallel", text: $model.programDraftParallelText)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 70)
                Picker("Dependency failure", selection: $model.programDraftDependencyFailure) {
                    Text("block").tag("block")
                    Text("cancel").tag("cancel")
                    Text("retry").tag("retry")
                }
                .pickerStyle(.menu)
                .frame(width: 150)
            }
            HStack(spacing: 12) {
                Toggle("Activate", isOn: $model.programDraftActivate)
                Toggle("Auto-dispatch", isOn: $model.programDraftAutoDispatch)
                Spacer()
                Button("Create program") {
                    Task { await model.createProgram() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.programBusy)
            }
            .font(.system(size: 10, design: .monospaced))
        }
        .padding(10)
        .background(.white.opacity(0.025), in: RoundedRectangle(cornerRadius: 10))
    }

    private func programRow(_ entry: ProgramListEntry) -> some View {
        let program = entry.program
        return VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 8) {
                Image(systemName: program.archived ? "archivebox" : "point.3.connected.trianglepath.dotted")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(programColor(program.status))
                Text(program.goal)
                    .font(.system(size: 11, weight: .semibold, design: .rounded))
                    .lineLimit(1)
                Text(program.status)
                    .font(.system(size: 8, weight: .semibold, design: .monospaced))
                    .foregroundStyle(programColor(program.status))
                if program.archived {
                    Text("archive")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
                Spacer()
                Text("\(entry.counts.completed)/\(entry.counts.total) done")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
                if entry.counts.active > 0 {
                    Text("\(entry.counts.active) active")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.green)
                }
                if entry.counts.blocked > 0 {
                    Text("\(entry.counts.blocked) blocked")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.orange)
                }
                if program.canPause {
                    programAction("pause.fill", tint: .yellow, help: "Pause new dispatch") {
                        Task { await model.pauseProgram(program) }
                    }
                }
                if program.canResume {
                    programAction("play.fill", tint: .green, help: "Resume program") {
                        Task { await model.resumeProgram(program) }
                    }
                }
                if !program.isTerminal, !program.archived {
                    programAction("xmark.circle", tint: .orange, help: "Cancel program") {
                        Task { await model.cancelProgram(program) }
                    }
                }
                programAction("info.circle", tint: .secondary, help: "Program detail") {
                    model.selectedProgramID = program.id
                    showDetail = true
                    Task { await model.loadProgramDetail() }
                }
                if program.canDelete {
                    programAction("trash", tint: .orange, help: "Delete program") {
                        pendingDelete = program
                    }
                }
            }
            HStack(spacing: 12) {
                if let tokens = program.tokenBudget {
                    Text("\(tokens.formatted()) tokens")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
                if let cost = program.costBudgetUsd {
                    Text(String(format: "$%.2f", cost))
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
                Text(
                    "priority \(entry.counts.ready) ready · \(entry.counts.failed) failed · \(entry.counts.cancelled) cancelled"
                )
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
                Spacer()
                Text(
                    "auto-dispatch \(program.policy.autoDispatch ? "on" : "off") · parallel \(program.policy.maxParallelMilestones)"
                )
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
            }
            if let reason = program.reason {
                Text(reason)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.orange.opacity(0.9))
                    .lineLimit(2)
            }
        }
        .padding(8)
        .background(.white.opacity(0.02), in: RoundedRectangle(cornerRadius: 9))
        .contentShape(Rectangle())
        .onTapGesture {
            model.selectedProgramID = program.id
            showDetail = true
            Task { await model.loadProgramDetail() }
        }
    }

    private func programMetric(_ title: String, _ value: Int) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text("\(value)")
                .font(.system(size: 12, weight: .semibold, design: .rounded))
            Text(title)
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.secondary)
        }
    }

    private func programAction(
        _ icon: String,
        tint: Color,
        help: String,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Image(systemName: icon)
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(tint)
                .frame(width: 22, height: 22)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(help)
    }

    private func programColor(_ status: String) -> Color {
        switch status {
        case "active": .green
        case "paused": .yellow
        case "completed": .cyan
        case "failed": .orange
        default: .secondary
        }
    }
}

struct ProgramDetailSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var showMilestoneComposer = false

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Program detail")
                    Text(model.programDetail?.program.goal ?? "Select a program")
                        .font(.system(size: 15, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.programDetailBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Close") { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
            .padding(18)
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    if let detail = model.programDetail {
                        header(detail)
                        milestoneComposer(detail)
                        ForEach(detail.milestones) { milestone in
                            milestoneRow(milestone, all: detail.milestones)
                        }
                        if detail.events.isEmpty {
                            Text("No program events yet.")
                                .font(.system(size: 9, design: .monospaced))
                                .foregroundStyle(.tertiary)
                        } else {
                            Text("Recent program events")
                                .font(.system(size: 9, weight: .semibold, design: .monospaced))
                                .foregroundStyle(.secondary)
                            ForEach(detail.events.suffix(8)) { event in
                                Text("\(event.seq)  \(event.type)  \(event.createdAt)")
                                    .font(.system(size: 8, design: .monospaced))
                                    .foregroundStyle(.tertiary)
                            }
                        }
                    } else {
                        Text("No program selected.")
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundStyle(.secondary)
                    }
                }
                .padding(18)
            }
        }
        .frame(minWidth: 760, minHeight: 600)
        .task {
            await model.loadProgramDetail()
        }
    }

    private func header(_ detail: ProgramStatusResult) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 10) {
                Text(detail.program.status)
                    .font(.system(size: 9, weight: .semibold, design: .monospaced))
                    .foregroundStyle(statusColor(detail.program.status))
                Text("\(detail.counts.completed)/\(detail.counts.total) milestones complete")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
                Spacer()
                if detail.program.canPause {
                    Button("Pause") {
                        Task { await model.pauseProgram(detail.program) }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                }
                if detail.program.canResume {
                    Button("Resume") {
                        Task { await model.resumeProgram(detail.program) }
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                }
                if !detail.program.isTerminal, !detail.program.archived {
                    Button("Cancel") {
                        Task { await model.cancelProgram(detail.program) }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                }
            }
            HStack(spacing: 16) {
                Text("\(detail.counts.running) running")
                Text("\(detail.counts.verifying) verifying")
                Text("\(detail.counts.completed) completed")
                Text("\(detail.counts.blocked) blocked")
                Text("\(detail.counts.cancelled) cancelled")
                Spacer()
                Text(
                    "usage \(detail.usage.tokenSpent.formatted()) tokens · \(String(format: "$%.4f", detail.usage.costSpentUsd))"
                )
            }
            .font(.system(size: 8, design: .monospaced))
            .foregroundStyle(.secondary)
            if let reason = detail.program.reason {
                Text(reason)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.orange)
            }
            Text(
                "policy: auto-dispatch \(detail.program.policy.autoDispatch ? "on" : "off") · attempts 1–\(detail.program.policy.maxAttempts) · backoff \(detail.program.policy.retryBackoffSecs)s · dependency \(detail.program.policy.dependencyFailure) · parallel \(detail.program.policy.maxParallelMilestones)"
            )
            .font(.system(size: 8, design: .monospaced))
            .foregroundStyle(.tertiary)
        }
        .padding(10)
        .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 10))
    }

    private func milestoneComposer(_ detail: ProgramStatusResult) -> some View {
        @Bindable var model = model
        return VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("Milestones")
                    .font(.system(size: 9, weight: .semibold, design: .monospaced))
                    .foregroundStyle(.secondary)
                Spacer()
                Button(showMilestoneComposer ? "Hide add form" : "Add milestone") {
                    withAnimation(VPMotion.smooth) {
                        showMilestoneComposer.toggle()
                    }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
            }
            if showMilestoneComposer {
                TextField("Milestone title", text: $model.programMilestoneDraftTitle)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 11, weight: .medium))
                TextField("Objective", text: $model.programMilestoneDraftObjective, axis: .vertical)
                    .textFieldStyle(.roundedBorder)
                    .lineLimit(2 ... 4)
                    .font(.system(size: 11))
                HStack(spacing: 8) {
                    TextField("Acceptance (comma separated)", text: $model.programMilestoneDraftAcceptance)
                        .textFieldStyle(.roundedBorder)
                    TextField("Scope", text: $model.programMilestoneDraftScope)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 120)
                    TextField("Priority", text: $model.programMilestoneDraftPriorityText)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 70)
                    TextField("Attempts", text: $model.programMilestoneDraftMaxAttemptsText)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 70)
                    Picker("Profile", selection: $model.programMilestoneDraftProfile) {
                        ForEach(["coding", "research", "verification", "generic"], id: \.self) {
                            Text($0).tag($0)
                        }
                    }
                    .pickerStyle(.menu)
                    .frame(width: 120)
                }
                HStack(spacing: 8) {
                    TextField("Token budget", text: $model.programMilestoneDraftTokenBudget)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 120)
                    TextField("USD budget", text: $model.programMilestoneDraftCostBudget)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 100)
                    Text("deps")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.secondary)
                    ForEach(detail.milestones) { dependency in
                        Toggle(dependency.key, isOn: Binding(
                            get: { model.programMilestoneDraftDeps.contains(dependency.id) },
                            set: { _ in model.toggleProgramMilestoneDependency(dependency.id) }
                        ))
                        .toggleStyle(.button)
                        .controlSize(.small)
                    }
                    Spacer()
                    Button("Add") {
                        Task { await model.createProgramMilestone() }
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                }
                .font(.system(size: 9, design: .monospaced))
            }
        }
    }

    private func milestoneRow(_ milestone: ProgramMilestone, all: [ProgramMilestone]) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 8) {
                Text(milestone.key)
                    .font(.system(size: 9, weight: .semibold, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .frame(width: 42, alignment: .leading)
                Text(milestone.title)
                    .font(.system(size: 11, weight: .semibold, design: .rounded))
                Text(milestone.status)
                    .font(.system(size: 8, weight: .semibold, design: .monospaced))
                    .foregroundStyle(statusColor(milestone.status))
                Spacer()
                Text("\(milestone.attempts)/\(milestone.maxAttempts)")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
                if milestone.canStart {
                    Button("Start") {
                        Task { await model.startProgramMilestone(milestone) }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                }
                if milestone.canRetry {
                    Button("Retry") {
                        Task { await model.retryProgramMilestone(milestone) }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                }
                if milestone.canSkip {
                    Button("Skip") {
                        Task { await model.skipProgramMilestone(milestone) }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                }
                if let sessionID = milestone.sessionID {
                    Button("Session") {
                        model.selectSession(sessionID)
                        dismiss()
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                }
            }
            if !milestone.deps.isEmpty {
                Text("deps: " + milestone.deps.map { dependency in
                    all.first(where: { $0.id == dependency })?.key ?? String(dependency.prefix(8))
                }.joined(separator: ", "))
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
            Text(milestone.objective)
                .font(.system(size: 9, design: .monospaced))
                .foregroundStyle(.secondary)
                .lineLimit(2)
            if let error = milestone.lastError {
                Text(error)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.orange)
                    .lineLimit(2)
            }
            if let result = milestone.result {
                Text(result)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.cyan.opacity(0.85))
                    .lineLimit(2)
            }
        }
        .padding(8)
        .background(.white.opacity(0.02), in: RoundedRectangle(cornerRadius: 8))
    }

    private func statusColor(_ status: String) -> Color {
        switch status {
        case "active", "running": .green
        case "ready", "verifying": .cyan
        case "paused", "failed": .yellow
        case "blocked": .orange
        case "completed": .cyan
        case "cancelled": .secondary
        default: .secondary
        }
    }
}
