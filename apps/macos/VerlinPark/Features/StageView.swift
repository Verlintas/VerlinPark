import SwiftUI

struct StageView: View {
    @Environment(AppModel.self) private var model
    @Environment(BrandStore.self) private var brand
    @Binding var showingNewSession: Bool

    var body: some View {
        Group {
            switch model.mode {
            case .resident:
                ExternalResidentView()
            case .project:
                if let project = model.selectedProject {
                    ProjectStageView(project: project, showingNewSession: $showingNewSession)
                } else {
                    WelcomeStageView()
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background {
            ZStack {
                Rectangle().fill(.regularMaterial)
                Color.black.opacity(0.22)
            }
        }
    }
}

private struct ProjectStageView: View {
    @Environment(AppModel.self) private var model
    @Environment(BrandStore.self) private var brand
    let project: Project
    @Binding var showingNewSession: Bool
    @State private var draft = ""
    @FocusState private var composerFocused: Bool

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider().opacity(0.35)
            if let session = model.selectedSession {
                SessionChatView(session: session)
            } else {
                emptyState
            }
            composer
        }
    }

    private var header: some View {
        HStack(alignment: .center, spacing: 14) {
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 8) {
                    Text(project.name)
                        .font(.system(size: 20, weight: .semibold))
                    StatusPill(status: model.connection == .connected ? "connected" : "offline")
                }
                HStack(spacing: 8) {
                    Text(project.root)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    if let info = model.agentInfo {
                        Text("·")
                        Text(info.configured ? (info.model ?? "model") : "no model")
                            .foregroundStyle(info.configured ? Color.gray : Color.orange)
                    }
                }
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(.secondary)
            }

            Spacer(minLength: 12)

            if !model.sessions.isEmpty {
                Menu {
                    ForEach(model.sessions) { session in
                        Button {
                            model.selectSession(session.id)
                        } label: {
                            if session.id == model.selectedSessionID {
                                Label(session.goal, systemImage: "checkmark")
                            } else {
                                Text(session.goal)
                            }
                        }
                    }
                    Divider()
                    Button("New Session…") {
                        showingNewSession = true
                    }
                } label: {
                    HStack(spacing: 6) {
                        Image(systemName: "bubble.left.and.bubble.right")
                        Text(model.selectedSession?.goal ?? "Sessions")
                            .lineLimit(1)
                        Image(systemName: "chevron.down")
                            .font(.system(size: 8, weight: .bold))
                    }
                    .font(.system(size: 11))
                    .padding(.horizontal, 10)
                    .padding(.vertical, 6)
                    .background(.ultraThinMaterial, in: Capsule())
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
            }

            Button {
                showingNewSession = true
            } label: {
                Image(systemName: "plus")
                    .font(.system(size: 11, weight: .semibold))
                    .frame(width: 26, height: 26)
                    .background(.ultraThinMaterial, in: Circle())
            }
            .buttonStyle(.plain)
            .help("New session")
        }
        .padding(.horizontal, 22)
        .padding(.vertical, 14)
    }

    private var emptyState: some View {
        VStack(spacing: 14) {
            Image(systemName: "rectangle.stack.badge.plus")
                .font(.system(size: 34, weight: .light))
                .foregroundStyle(.secondary)
            Text("Start a session")
                .font(.system(size: 20, weight: .semibold))
            Text("Give the Leader one concrete goal. The team will appear in Collab as it is recruited.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 380)
            Button("New Session", action: { showingNewSession = true })
                .buttonStyle(.borderedProminent)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(40)
    }

    private var composer: some View {
        HStack(alignment: .bottom, spacing: 10) {
            Button {
                showingNewSession = true
            } label: {
                Image(systemName: "plus")
                    .font(.system(size: 12, weight: .semibold))
                    .frame(width: 26, height: 26)
            }
            .buttonStyle(.plain)
            .help("New session")

            TextField(
                model.selectedSession == nil
                    ? "Describe a task to start a session…"
                    : "Message the Leader…",
                text: $draft,
                axis: .vertical
            )
            .textFieldStyle(.plain)
            .font(.system(size: 13))
            .lineLimit(1 ... 4)
            .focused($composerFocused)
            .onSubmit(send)

            Button(action: send) {
                Image(systemName: "arrow.up.circle.fill")
                    .font(.system(size: 22))
                    .foregroundStyle(draft.isEmpty ? Color.secondary : Color.green)
            }
            .buttonStyle(.plain)
            .disabled(sendDisabled)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .strokeBorder(.white.opacity(0.08), lineWidth: 1)
        }
        .padding(.horizontal, 18)
        .padding(.bottom, 16)
        .padding(.top, 8)
    }

    private var sendDisabled: Bool {
        let textEmpty = draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        let running = model.selectedSession?.status == "running"
        return textEmpty || model.isBusy || running
    }

    private func send() {
        let goal = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !goal.isEmpty else { return }
        draft = ""
        if model.selectedSession != nil {
            Task { await model.sendMessage(goal) }
        } else {
            Task { await model.createSession(goal: goal) }
        }
    }
}

struct SessionChatView: View {
    @Environment(AppModel.self) private var model
    let session: Session

    private var entries: [ChatEntry] {
        var result: [ChatEntry] = []
        var runningTools: [String: (name: String, input: JSONValue)] = [:]
        var resolvedApprovals: Set<String> = []

        for event in model.events {
            switch event.type {
            case "message.created":
                guard event.payload.objectValue?["role"]?.stringValue == "user",
                      let content = event.payload.objectValue?["content"]?.stringValue
                else { continue }
                result.append(.user(id: event.id, text: content))
            case "agent.message":
                guard let text = event.payload.objectValue?["text"]?.stringValue,
                      !text.isEmpty
                else { continue }
                result.append(.assistant(id: event.id, text: text))
            case "tool.started":
                guard let id = event.payload.objectValue?["tool_call_id"]?.stringValue,
                      let name = event.payload.objectValue?["name"]?.stringValue
                else { continue }
                let input = event.payload.objectValue?["input"] ?? .null
                runningTools[id] = (name, input)
            case "tool.finished":
                guard let id = event.payload.objectValue?["tool_call_id"]?.stringValue,
                      let name = event.payload.objectValue?["name"]?.stringValue
                else { continue }
                let output = event.payload.objectValue?["output"]?.stringValue ?? ""
                let isError = event.payload.objectValue?["is_error"]?.boolValue ?? false
                let meta = runningTools[id]
                result.append(
                    .tool(
                        id: event.id,
                        name: name,
                        input: meta?.input ?? .null,
                        output: output,
                        isError: isError
                    )
                )
            case "approval.requested":
                guard let approval = try? event.payload.decode(Approval.self) else { continue }
                result.append(.approval(approval))
            case "approval.resolved":
                if let id = event.payload.objectValue?["approval_id"]?.stringValue {
                    resolvedApprovals.insert(id)
                }
            case "agent.failed", "agent.unavailable":
                let message = event.payload.objectValue?["error"]?.stringValue
                    ?? event.payload.objectValue?["message"]?.stringValue
                    ?? "The agent stopped."
                result.append(.error(id: event.id, text: message))
            case "agent.budget_exceeded":
                let limit = event.payload.objectValue?["limit"]?.intValue ?? 0
                let spent = event.payload.objectValue?["spent"]?.intValue ?? 0
                result.append(
                    .notice(
                        id: event.id,
                        text: "Token budget exhausted (\(spent) / \(limit))."
                    )
                )
            case "agent.cancelled":
                result.append(.notice(id: event.id, text: "Run cancelled."))
            default:
                continue
            }
        }

        return result.map { entry in
            if case let .approval(approval) = entry,
               resolvedApprovals.contains(approval.id)
            {
                return .approvalResolved(approval)
            }
            return entry
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 3) {
                    Text(session.goal)
                        .font(.system(size: 21, weight: .semibold))
                    HStack(spacing: 8) {
                        Text("session \(session.id)")
                        if let branch = session.branch {
                            Text("·")
                            Image(systemName: "arrow.triangle.branch")
                            Text(branch)
                        }
                    }
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    .textSelection(.enabled)
                }
                Spacer()
                VStack(alignment: .trailing, spacing: 5) {
                    HStack(spacing: 8) {
                        if session.status == "running" {
                            Button {
                                Task { await model.cancelSession() }
                            } label: {
                                Label("Stop", systemImage: "stop.fill")
                                    .font(.system(size: 10, weight: .semibold))
                            }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                            .tint(.red)
                        }
                        StatusPill(status: session.status)
                    }
                    Text(tokenUsageLabel)
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
            }
            .padding(.horizontal, 22)
            .padding(.top, 18)
            .padding(.bottom, 12)

            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 10) {
                        if entries.isEmpty, model.streamingText.isEmpty {
                            emptyState
                        }
                        ForEach(entries) { entry in
                            entryView(entry)
                                .id(entry.id)
                        }
                        if !model.streamingText.isEmpty {
                            assistantBubble(model.streamingText + "▍")
                                .id("streaming")
                        } else if session.status == "running" {
                            HStack(spacing: 8) {
                                ProgressView().controlSize(.small)
                                Text("Leader is thinking…")
                                    .font(.system(size: 10, design: .monospaced))
                                    .foregroundStyle(.secondary)
                            }
                            .padding(.vertical, 4)
                        }
                    }
                    .padding(.horizontal, 18)
                    .padding(.bottom, 16)
                }
                .onChange(of: entries.count) { _, _ in
                    if let last = entries.last {
                        withAnimation(.easeOut(duration: 0.2)) {
                            proxy.scrollTo(last.id, anchor: .bottom)
                        }
                    }
                }
                .onChange(of: model.streamingText) { _, _ in
                    withAnimation(.easeOut(duration: 0.1)) {
                        proxy.scrollTo("streaming", anchor: .bottom)
                    }
                }
            }
        }
    }

    private var tokenUsageLabel: String {
        let used = tokenLabel(session.tokensUsed)
        guard let budget = session.tokenBudget, budget > 0 else {
            return "\(used) tokens"
        }
        return "\(used) / \(tokenLabel(budget)) tokens"
    }

    private func tokenLabel(_ value: Int) -> String {
        if value >= 1_000_000 {
            return String(format: "%.1fm", Double(value) / 1_000_000)
        }
        if value >= 1000 {
            return String(format: "%.1fk", Double(value) / 1000)
        }
        return "\(value)"
    }

    private var emptyState: some View {
        VStack(spacing: 10) {
            Image(systemName: "bubble.left.and.text.bubble.right")
                .font(.system(size: 30, weight: .light))
                .foregroundStyle(.secondary)
            Text("Talk to the Leader")
                .font(.headline)
            Text(
                "The Leader can inspect files and run commands. Write and shell calls ask for your approval."
            )
            .font(.callout)
            .foregroundStyle(.secondary)
            .multilineTextAlignment(.center)
            .frame(maxWidth: 380)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 40)
    }

    @ViewBuilder
    private func entryView(_ entry: ChatEntry) -> some View {
        switch entry {
        case let .user(_, text):
            HStack {
                Spacer(minLength: 60)
                Text(text)
                    .font(.system(size: 13))
                    .foregroundStyle(.white)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 9)
                    .background(
                        LinearGradient(
                            colors: [.green.opacity(0.8), .teal.opacity(0.6)],
                            startPoint: .topLeading,
                            endPoint: .bottomTrailing
                        ),
                        in: RoundedRectangle(cornerRadius: 13, style: .continuous)
                    )
            }
        case let .assistant(_, text):
            assistantBubble(text)
        case let .tool(_, name, input, output, isError):
            ToolCallCard(name: name, input: input, output: output, isError: isError)
        case let .approval(approval):
            ApprovalCard(approval: approval, resolved: false)
        case let .approvalResolved(approval):
            ApprovalCard(approval: approval, resolved: true)
        case let .notice(_, text):
            HStack(spacing: 8) {
                Image(systemName: "info.circle")
                    .foregroundStyle(.secondary)
                Text(text)
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
            .padding(10)
            .background(.white.opacity(0.04), in: RoundedRectangle(cornerRadius: 10))
        case let .error(_, text):
            HStack(spacing: 8) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(.orange)
                Text(text)
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
            .padding(10)
            .background(.orange.opacity(0.08), in: RoundedRectangle(cornerRadius: 10))
        }
    }

    private func assistantBubble(_ text: String) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Circle()
                .fill(.green.opacity(0.2))
                .frame(width: 20, height: 20)
                .overlay {
                    Image(systemName: "sparkles")
                        .font(.system(size: 8, weight: .bold))
                        .foregroundStyle(.green)
                }
            Text(text)
                .font(.system(size: 13))
                .textSelection(.enabled)
                .padding(.horizontal, 12)
                .padding(.vertical, 9)
                .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 13, style: .continuous))
                .overlay {
                    RoundedRectangle(cornerRadius: 13, style: .continuous)
                        .strokeBorder(.white.opacity(0.07), lineWidth: 1)
                }
            Spacer(minLength: 30)
        }
    }
}

private enum ChatEntry: Identifiable {
    case user(id: String, text: String)
    case assistant(id: String, text: String)
    case tool(id: String, name: String, input: JSONValue, output: String, isError: Bool)
    case approval(Approval)
    case approvalResolved(Approval)
    case notice(id: String, text: String)
    case error(id: String, text: String)

    var id: String {
        switch self {
        case let .user(id, _), let .assistant(id, _), let .tool(id, _, _, _, _),
             let .notice(id, _), let .error(id, _):
            id
        case let .approval(approval), let .approvalResolved(approval):
            "approval-\(approval.id)"
        }
    }
}

struct ToolCallCard: View {
    let name: String
    let input: JSONValue
    let output: String
    let isError: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 7) {
                Image(systemName: "wrench.and.screwdriver")
                    .font(.system(size: 10))
                    .foregroundStyle(isError ? .red : .secondary)
                Text(name)
                    .font(.system(size: 10, weight: .medium, design: .monospaced))
                Spacer()
                Text(isError ? "failed" : "done")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(isError ? Color.red : Color.gray)
            }
            if !input.prettyString().isEmpty, input != .null {
                Text(input.prettyString())
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    .lineLimit(3)
            }
            Text(output.isEmpty ? "(no output)" : output)
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
                .lineLimit(10)
        }
        .padding(10)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 10))
        .overlay {
            RoundedRectangle(cornerRadius: 10)
                .strokeBorder(isError ? .red.opacity(0.2) : .white.opacity(0.05), lineWidth: 1)
        }
    }
}

struct ApprovalCard: View {
    @Environment(AppModel.self) private var model
    let approval: Approval
    let resolved: Bool
    @State private var pending = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 7) {
                Image(systemName: "lock.shield")
                    .foregroundStyle(.orange)
                Text("Permission required")
                    .font(.system(size: 11, weight: .semibold))
                Spacer()
                Text(approval.tool)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
            Text(approval.input.prettyString())
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(.secondary)
                .lineLimit(6)
            if resolved || approval.status != "pending" {
                Text("Resolved")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
            } else {
                HStack {
                    Button("Deny") {
                        pending = true
                        Task { await model.resolveApproval(approval.id, approved: false) }
                    }
                    .buttonStyle(.bordered)
                    Button("Approve") {
                        pending = true
                        Task { await model.resolveApproval(approval.id, approved: true) }
                    }
                    .buttonStyle(.borderedProminent)
                }
                .disabled(pending)
                .controlSize(.small)
            }
        }
        .padding(11)
        .background(.orange.opacity(0.07), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(.orange.opacity(0.2), lineWidth: 1)
        }
    }
}

private struct WelcomeStageView: View {
    @Environment(AppModel.self) private var model
    @Environment(BrandStore.self) private var brand

    var body: some View {
        VStack(spacing: 20) {
            BrandMark(config: brand.config, fontSize: 34)
            Text("Agent teams, not chat tabs.")
                .font(.system(size: 22, weight: .semibold, design: .rounded))
            Text(
                "Open a repository to start. VerlinPark will isolate every task in a worktree and make the whole run inspectable."
            )
            .font(.callout)
            .foregroundStyle(.secondary)
            .multilineTextAlignment(.center)
            .frame(maxWidth: 420)
            Button {
                Task { await model.addProject() }
            } label: {
                Label("Add Project…", systemImage: "folder.badge.plus")
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .disabled(model.connection != .connected)
            ConnectionBadge(state: model.connection)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(40)
    }
}
