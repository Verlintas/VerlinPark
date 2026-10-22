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
                SessionTimelineView(session: session)
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
                Text(project.root)
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
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
                "Describe a task to start a new session…",
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
            .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || model.isBusy)
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

    private func send() {
        let goal = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !goal.isEmpty else { return }
        draft = ""
        Task { await model.createSession(goal: goal) }
    }
}

struct SessionTimelineView: View {
    @Environment(AppModel.self) private var model
    let session: Session

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 3) {
                    Text(session.goal)
                        .font(.system(size: 21, weight: .semibold))
                    Text("session \(session.id)")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.tertiary)
                        .textSelection(.enabled)
                }
                Spacer()
                StatusPill(status: session.status)
            }
            .padding(.horizontal, 22)
            .padding(.top, 18)
            .padding(.bottom, 12)

            if model.events.isEmpty {
                VStack(spacing: 10) {
                    Image(systemName: "list.bullet.rectangle")
                        .font(.system(size: 30, weight: .light))
                        .foregroundStyle(.secondary)
                    Text("No events yet")
                        .font(.headline)
                    Text("The trace is empty. The Leader runtime lands in M1.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 8) {
                        ForEach(model.events) { event in
                            TimelineEventCard(event: event)
                        }
                    }
                    .padding(.horizontal, 18)
                    .padding(.bottom, 16)
                }
            }
        }
    }
}

struct TimelineEventCard: View {
    let event: Event

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: icon)
                .font(.system(size: 11, weight: .medium))
                .foregroundStyle(.green)
                .frame(width: 18, height: 18)
                .background(.green.opacity(0.12), in: Circle())

            VStack(alignment: .leading, spacing: 4) {
                HStack {
                    Text(event.type)
                        .font(.system(size: 11, weight: .medium, design: .monospaced))
                    Spacer()
                    Text(event.createdAt)
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
                Text(event.payload.prettyString())
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
                    .lineLimit(6)
            }
        }
        .padding(11)
        .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
    }

    private var icon: String {
        switch event.type {
        case "session.created": "sparkles"
        case "tool.started", "tool.finished": "wrench.and.screwdriver"
        case "message.created": "bubble.left"
        default: "circle.grid.2x2"
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
