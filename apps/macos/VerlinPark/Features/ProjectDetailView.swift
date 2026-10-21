import SwiftUI

struct ProjectDetailView: View {
    @Environment(AppModel.self) private var model
    let project: Project
    let onNewSession: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Divider()
            if let session = model.selectedSession {
                SessionDetailView(session: session)
            } else {
                emptyState
            }
        }
        .navigationTitle(project.name)
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 20) {
            VStack(alignment: .leading, spacing: 6) {
                Text(project.name)
                    .font(.system(size: 26, weight: .semibold))
                Text(project.root)
                    .font(.system(.caption, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
                HStack(spacing: 12) {
                    Label(project.defaultBranch, systemImage: "arrow.triangle.branch")
                    Label(
                        "\(model.sessions.count) session\(model.sessions.count == 1 ? "" : "s")",
                        systemImage: "rectangle.stack"
                    )
                }
                .font(.caption)
                .foregroundStyle(.secondary)
            }

            Spacer()

            Button(action: onNewSession) {
                Label("New Session", systemImage: "plus.bubble")
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
        }
        .padding(20)
    }

    private var emptyState: some View {
        ContentUnavailableView {
            Label("No session selected", systemImage: "rectangle.stack.badge.plus")
        } description: {
            Text(
                "Create a session, give the Lead a concrete goal, and VerlinPark will build the agent team around it."
            )
        } actions: {
            Button("New Session", action: onNewSession)
                .buttonStyle(.borderedProminent)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

struct SessionDetailView: View {
    @Environment(AppModel.self) private var model
    let session: Session

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .top, spacing: 16) {
                VStack(alignment: .leading, spacing: 6) {
                    Text(session.goal)
                        .font(.system(size: 22, weight: .semibold))
                    Text("session \(session.id)")
                        .font(.system(.caption2, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                }
                Spacer()
                StatusPill(status: session.status)
            }
            .padding(20)

            Divider()

            if model.events.isEmpty {
                ContentUnavailableView {
                    Label("No events yet", systemImage: "list.bullet.rectangle")
                } description: {
                    Text("The event log will appear here as soon as the session starts working.")
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(model.events) { event in
                            EventRow(event: event)
                            Divider()
                        }
                    }
                    .padding(.horizontal, 20)
                }
            }
        }
    }
}

struct EventRow: View {
    let event: Event

    var body: some View {
        HStack(alignment: .top, spacing: 14) {
            Text(String(format: "#%04llu", event.seq))
                .font(.system(.caption, design: .monospaced))
                .foregroundStyle(.tertiary)
                .frame(width: 56, alignment: .leading)

            VStack(alignment: .leading, spacing: 5) {
                HStack(spacing: 10) {
                    Text(event.type)
                        .font(.system(.callout, design: .monospaced).weight(.medium))
                    Spacer()
                    Text(event.createdAt)
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
                Text(event.payload.prettyString())
                    .font(.system(.caption, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }
        }
        .padding(.vertical, 11)
    }
}

struct StatusPill: View {
    let status: String

    var body: some View {
        Text(status.uppercased())
            .font(.system(size: 10, weight: .semibold, design: .monospaced))
            .padding(.horizontal, 9)
            .padding(.vertical, 5)
            .background(color.opacity(0.15), in: Capsule())
            .foregroundStyle(color)
    }

    private var color: Color {
        switch status.lowercased() {
        case "done", "completed": .green
        case "failed", "cancelled": .red
        case "running", "verifying", "merging": .orange
        default: .secondary
        }
    }
}
