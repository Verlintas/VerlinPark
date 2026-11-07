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

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Approvals")
                    Text("Pending across projects")
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.approvalCenterBusy {
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
                ForEach(model.pendingApprovals) { approval in
                    VStack(alignment: .leading, spacing: 5) {
                        HStack(spacing: 6) {
                            Text(approval.tool.uppercased())
                                .font(.system(size: 8, weight: .semibold, design: .monospaced))
                                .foregroundStyle(.orange)
                                .padding(.horizontal, 5)
                                .padding(.vertical, 2)
                                .background(.orange.opacity(0.1), in: Capsule())
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
                        HStack(spacing: 6) {
                            Button("Approve") {
                                Task {
                                    await model.resolveGlobalApproval(approval.id, approved: true)
                                }
                            }
                            .buttonStyle(.borderedProminent)
                            .controlSize(.small)
                            .tint(.green)
                            .disabled(model.approvalCenterBusy)
                            Button("Deny") {
                                Task {
                                    await model.resolveGlobalApproval(approval.id, approved: false)
                                }
                            }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                            .disabled(model.approvalCenterBusy)
                        }
                    }
                    .padding(9)
                    .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 9))
                }
            }
        }
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .task {
            await model.loadPendingApprovals()
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
                Button("Prune") {
                    confirmPrune = true
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.memoryBusy || model.memories.isEmpty)
                .help("Delete memories older than 30 days")
            }
            Text("Durable decisions, facts, conventions, and sources shared across sessions.")
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(.secondary)
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
            Text("Only memories created more than 30 days ago are removed. Recent memories are kept.")
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
                "Ready tasks are ordered by task priority plus session priority, with aging so low-priority work cannot starve. When auto-resume is enabled, the daemon can resume idle/failed plans after the cooldown. With one-task-per-tick enabled it claims and runs a single ready task, then releases the session so other sessions can make progress; the project cooldown still applies between dispatches, so use a small value such as 5s for continuous progress. Otherwise it resumes the whole stored plan. Write leases are stored by the daemon and enforce overlapping path scopes across sessions; the same session may share its worktree."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)

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
        .task(id: model.selectedProjectID) {
            await model.loadTaskQueue()
            await model.loadTaskSchedulerPolicy()
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
