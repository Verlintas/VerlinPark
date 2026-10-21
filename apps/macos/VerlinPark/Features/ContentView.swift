import SwiftUI

struct ContentView: View {
    @Environment(AppModel.self) private var model
    @State private var showingNewSession = false

    var body: some View {
        NavigationSplitView {
            SidebarView()
                .navigationSplitViewColumnWidth(min: 230, ideal: 280, max: 360)
        } detail: {
            if let project = model.selectedProject {
                ProjectDetailView(project: project) {
                    showingNewSession = true
                }
            } else {
                WelcomeView()
            }
        }
        .navigationTitle("VerlinPark")
        .toolbar {
            ToolbarItem(placement: .automatic) {
                ConnectionBadge(state: model.connection)
            }
            ToolbarItemGroup(placement: .primaryAction) {
                Button {
                    Task { await model.addProject() }
                } label: {
                    Label("Add Project", systemImage: "folder.badge.plus")
                }
                .help("Add a local repository (⇧⌘O)")

                Button {
                    showingNewSession = true
                } label: {
                    Label("New Session", systemImage: "plus.bubble")
                }
                .disabled(model.selectedProject == nil)
                .help("Create a session in the selected project")

                Button {
                    Task { await model.refresh() }
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                .help("Refresh projects and sessions (⌘R)")
            }
        }
        .safeAreaInset(edge: .top) {
            if let error = model.lastError {
                ErrorBanner(message: error) {
                    model.clearError()
                }
            }
        }
        .sheet(isPresented: $showingNewSession) {
            NewSessionSheet()
                .environment(model)
        }
    }
}

private struct ErrorBanner: View {
    let message: String
    let dismiss: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange)
            Text(message)
                .font(.callout)
                .lineLimit(2)
            Spacer()
            Button("Dismiss", action: dismiss)
                .buttonStyle(.link)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
        .background(.orange.opacity(0.12))
    }
}

struct ConnectionBadge: View {
    let state: AppModel.ConnectionState

    var body: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(color)
                .frame(width: 7, height: 7)
            Text(label)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(.quaternary.opacity(0.5), in: Capsule())
    }

    private var color: Color {
        switch state {
        case .connected: .green
        case .connecting: .orange
        case .idle: .secondary
        case .failed: .red
        }
    }

    private var label: String {
        switch state {
        case .connected: "daemon connected"
        case .connecting: "connecting"
        case .idle: "idle"
        case .failed: "daemon offline"
        }
    }
}

struct SettingsView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Form {
            Section("Daemon") {
                LabeledContent("Status") {
                    ConnectionBadge(state: model.connection)
                }
                LabeledContent("Projects", value: "\(model.projects.count)")
                LabeledContent("Sessions", value: "\(model.sessions.count)")
            }

            Section("About") {
                LabeledContent("Version", value: "0.1.0-dev")
                LabeledContent("License", value: "GPL-3.0-only")
                Text(
                    "VerlinPark is a native macOS workbench for agent teams. The daemon owns sessions; the app is a client."
                )
                .font(.callout)
                .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
        .padding(20)
        .frame(width: 480)
    }
}
