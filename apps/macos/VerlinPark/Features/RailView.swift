import SwiftUI

struct RailView: View {
    @Environment(AppModel.self) private var model
    @Environment(BrandStore.self) private var brand
    @Environment(MemoryMonitor.self) private var memory
    @Environment(\.openSettings) private var openSettings

    @Binding var showDashboard: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            BrandMark(config: brand.config, fontSize: 22)
                .padding(.horizontal, 16)
                .padding(.top, 4)
                .padding(.bottom, 18)
                .help("Customize this name in Settings → Brand")

            Button {
                model.selectResident()
            } label: {
                HStack(spacing: 10) {
                    ZStack {
                        Circle()
                            .fill(
                                LinearGradient(
                                    colors: [.green.opacity(0.9), .teal.opacity(0.65)],
                                    startPoint: .topLeading,
                                    endPoint: .bottomTrailing
                                )
                            )
                            .frame(width: 26, height: 26)
                        Image(systemName: "sparkles")
                            .font(.system(size: 11, weight: .bold))
                            .foregroundStyle(.white)
                    }
                    VStack(alignment: .leading, spacing: 1) {
                        Text("Chat with Aviiya")
                            .font(.system(size: 13, weight: .semibold))
                        Text("resident · shared tools")
                            .font(.system(size: 9, design: .monospaced))
                            .foregroundStyle(.secondary)
                    }
                    Spacer(minLength: 0)
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 9)
                .background(
                    model.mode == .resident ? Color.white.opacity(0.12) : Color.clear,
                    in: RoundedRectangle(cornerRadius: 11, style: .continuous)
                )
                .overlay {
                    RoundedRectangle(cornerRadius: 11, style: .continuous)
                        .strokeBorder(
                            model.mode == .resident ? .white.opacity(0.14) : .clear,
                            lineWidth: 1
                        )
                }
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 10)

            Divider()
                .opacity(0.45)
                .padding(.vertical, 14)
                .padding(.horizontal, 16)

            HStack {
                SectionLabel(title: "Projects")
                Spacer()
                Button {
                    Task { await model.addProject() }
                } label: {
                    Image(systemName: "plus")
                        .font(.system(size: 10, weight: .semibold))
                        .frame(width: 20, height: 20)
                }
                .buttonStyle(.plain)
                .help("Add a project (⇧⌘O)")
            }
            .padding(.horizontal, 18)
            .padding(.bottom, 8)

            ScrollView {
                LazyVStack(spacing: 3) {
                    ForEach(model.projects) { project in
                        projectRow(project)
                    }

                    Button {
                        Task { await model.addProject() }
                    } label: {
                        HStack(spacing: 8) {
                            Image(systemName: "plus")
                                .font(.system(size: 10, weight: .semibold))
                            Text("New Project")
                                .font(.system(size: 12))
                            Spacer()
                        }
                        .foregroundStyle(.secondary)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 8)
                    }
                    .buttonStyle(.plain)
                    .padding(.top, 4)
                }
                .padding(.horizontal, 8)
            }

            Spacer(minLength: 12)

            VStack(spacing: 3) {
                railAction(
                    icon: "rectangle.3.group",
                    title: "Manage Dash",
                    subtitle: "agents · cost · queue"
                ) {
                    showDashboard = true
                }

                railAction(
                    icon: "gearshape",
                    title: "Settings",
                    subtitle: "brand · models · presets"
                ) {
                    openSettings()
                }
            }
            .padding(.horizontal, 8)

            Divider()
                .opacity(0.45)
                .padding(.horizontal, 16)
                .padding(.vertical, 10)

            HStack {
                MemoryPill()
                Spacer()
                Text("v0.1")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.quaternary)
            }
            .padding(.horizontal, 18)
            .padding(.bottom, 10)
        }
        .background(.ultraThinMaterial.opacity(0.55))
    }

    private func projectRow(_ project: Project) -> some View {
        let selected = model.mode == .project && model.selectedProjectID == project.id
        return Button {
            model.selectProject(project.id)
        } label: {
            HStack(spacing: 9) {
                Circle()
                    .fill(projectStatusColor)
                    .frame(width: 7, height: 7)
                VStack(alignment: .leading, spacing: 1) {
                    Text(project.name)
                        .font(.system(size: 12.5, weight: selected ? .semibold : .regular))
                        .lineLimit(1)
                    Text(project.defaultBranch)
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .background(
                selected ? Color.white.opacity(0.11) : Color.clear,
                in: RoundedRectangle(cornerRadius: 9, style: .continuous)
            )
        }
        .buttonStyle(.plain)
    }

    private var projectStatusColor: Color {
        switch model.connection {
        case .connected: .green
        case .connecting: .orange
        case .idle: .secondary
        case .failed: .red
        }
    }

    private func railAction(
        icon: String,
        title: String,
        subtitle: String,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(spacing: 10) {
                Image(systemName: icon)
                    .font(.system(size: 12, weight: .medium))
                    .frame(width: 18)
                    .foregroundStyle(.secondary)
                VStack(alignment: .leading, spacing: 1) {
                    Text(title)
                        .font(.system(size: 12, weight: .medium))
                    Text(subtitle)
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
                Spacer()
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 7)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}
