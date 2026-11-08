import SwiftUI

struct RailView: View {
    @Environment(AppModel.self) private var model
    @Environment(BrandStore.self) private var brand
    @Environment(MemoryMonitor.self) private var memory
    @Environment(\.openSettings) private var openSettings

    @Binding var showDashboard: Bool
    @Binding var showCommandPalette: Bool
    @Binding var showingNotifications: Bool
    @State private var residentHovering = false
    @State private var hoveredProjectID: String?
    @State private var hoveredAction: String?
    @Namespace private var selectionNamespace

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            BrandMark(config: brand.config, fontSize: 28)
                .vpAppear(index: 0, animation: VPMotion.emphasizedSpring)
                .padding(.horizontal, 16)
                .padding(.top, 4)
                .padding(.bottom, 12)
                .help("Customize this name in Settings → Brand")

            Button {
                showCommandPalette = true
            } label: {
                HStack(spacing: 7) {
                    Image(systemName: "command")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(.secondary)
                    Text("Jump to…")
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(.secondary)
                    Spacer()
                    Text("⌘K")
                        .font(.system(size: 8, weight: .medium, design: .monospaced))
                        .foregroundStyle(.tertiary)
                        .padding(.horizontal, 5)
                        .padding(.vertical, 1)
                        .background(.white.opacity(0.05), in: RoundedRectangle(cornerRadius: 4))
                }
                .padding(.horizontal, 9)
                .padding(.vertical, 6)
                .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
                .overlay {
                    RoundedRectangle(cornerRadius: 8, style: .continuous)
                        .strokeBorder(.white.opacity(0.06), lineWidth: 1)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(VPPressableButtonStyle(scale: 0.99))
            .padding(.horizontal, 10)
            .padding(.bottom, 10)
            .help("Jump to a project, session, or action · ⌘K")

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
                .background {
                    if model.mode == .resident {
                        RoundedRectangle(cornerRadius: 11, style: .continuous)
                            .fill(.white.opacity(0.12))
                            .matchedGeometryEffect(id: "rail-selection", in: selectionNamespace)
                    } else if residentHovering {
                        RoundedRectangle(cornerRadius: 11, style: .continuous)
                            .fill(.white.opacity(0.045))
                    }
                }
                .overlay {
                    RoundedRectangle(cornerRadius: 11, style: .continuous)
                        .strokeBorder(
                            model.mode == .resident ? .white.opacity(0.14) : .clear,
                            lineWidth: 1
                        )
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(VPPressableButtonStyle(scale: 0.985))
            .onHover { residentHovering = $0 }
            .animation(VPMotion.hover, value: residentHovering)
            .animation(VPMotion.snap, value: model.mode)
            .padding(.horizontal, 10)

            VPGradientRule()
                .frame(height: 1)
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
                    icon: model.unreadNoticeCount > 0 ? "bell.badge" : "bell",
                    title: "Notifications",
                    subtitle: "approvals & failures",
                    badge: model.unreadNoticeCount,
                    bounceValue: model.unreadNoticeCount
                ) {
                    showingNotifications = true
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

            VPGradientRule()
                .frame(height: 1)
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
            .background {
                if selected {
                    RoundedRectangle(cornerRadius: 9, style: .continuous)
                        .fill(.white.opacity(0.11))
                        .matchedGeometryEffect(id: "rail-selection", in: selectionNamespace)
                } else if hoveredProjectID == project.id {
                    RoundedRectangle(cornerRadius: 9, style: .continuous)
                        .fill(.white.opacity(0.045))
                }
            }
            .overlay {
                RoundedRectangle(cornerRadius: 9, style: .continuous)
                    .strokeBorder(selected ? .white.opacity(0.12) : .clear, lineWidth: 1)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(VPPressableButtonStyle(scale: 0.99))
        .onHover { hovering in
            if hovering {
                hoveredProjectID = project.id
            } else if hoveredProjectID == project.id {
                hoveredProjectID = nil
            }
        }
        .animation(VPMotion.hover, value: hoveredProjectID)
        .animation(VPMotion.snap, value: selected)
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
        badge: Int? = nil,
        bounceValue: Int = 0,
        action: @escaping () -> Void
    ) -> some View {
        let hovering = hoveredAction == title
        return Button(action: action) {
            HStack(spacing: 10) {
                Image(systemName: icon)
                    .font(.system(size: 12, weight: .medium))
                    .frame(width: 18)
                    .foregroundStyle(hovering ? Color.primary : Color.secondary)
                    .symbolEffect(.bounce, value: bounceValue)
                VStack(alignment: .leading, spacing: 1) {
                    Text(title)
                        .font(.system(size: 12, weight: .medium))
                    Text(subtitle)
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
                Spacer()
                if let badge, badge > 0 {
                    VPCountBadge(count: badge, tint: .orange, compact: true)
                        .transition(.vpScaleFade)
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 7)
            .offset(x: hovering ? 1 : 0)
            .background(
                hovering ? Color.white.opacity(0.05) : Color.clear,
                in: RoundedRectangle(cornerRadius: 9, style: .continuous)
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(VPPressableButtonStyle(scale: 0.99))
        .onHover { isHovering in
            if isHovering {
                hoveredAction = title
            } else if hoveredAction == title {
                hoveredAction = nil
            }
        }
        .animation(VPMotion.hover, value: hovering)
        .animation(VPMotion.badge, value: badge ?? 0)
    }
}
