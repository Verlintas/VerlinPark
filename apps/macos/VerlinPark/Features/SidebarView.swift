import SwiftUI

struct SidebarView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(spacing: 0) {
            List(selection: projectSelection) {
                Section("Projects") {
                    ForEach(model.projects) { project in
                        Label {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(project.name)
                                    .lineLimit(1)
                                Text(project.defaultBranch)
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                        } icon: {
                            Image(systemName: "folder")
                        }
                        .tag(project.id)
                    }
                }
            }
            .listStyle(.sidebar)
            .frame(minHeight: 180, idealHeight: 250)

            Divider()

            List(selection: sessionSelection) {
                Section("Sessions") {
                    if model.sessions.isEmpty {
                        Text("No sessions yet")
                            .font(.callout)
                            .foregroundStyle(.secondary)
                    }
                    ForEach(model.sessions) { session in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(session.goal)
                                .lineLimit(2)
                            Text(session.status.uppercased())
                                .font(.system(size: 9, weight: .semibold, design: .monospaced))
                                .foregroundStyle(.secondary)
                        }
                        .tag(session.id)
                    }
                }
            }
            .listStyle(.sidebar)

            Divider()

            HStack(spacing: 8) {
                Button {
                    Task { await model.addProject() }
                } label: {
                    Label("Add Project", systemImage: "plus")
                }
                .buttonStyle(.borderless)
                Spacer()
                if model.isBusy {
                    ProgressView()
                        .controlSize(.small)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
        }
    }

    private var projectSelection: Binding<String?> {
        Binding(
            get: { model.selectedProjectID },
            set: { model.selectProject($0) }
        )
    }

    private var sessionSelection: Binding<String?> {
        Binding(
            get: { model.selectedSessionID },
            set: { model.selectSession($0) }
        )
    }
}
