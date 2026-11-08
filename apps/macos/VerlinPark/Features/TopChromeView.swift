import SwiftUI

/// Slim draggable chrome for the full-size window: breadcrumb, connection,
/// command palette, and focus mode all stay reachable without opening a menu.
struct TopChromeView: View {
    @Environment(AppModel.self) private var model
    @Environment(MemoryMonitor.self) private var memory
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Binding var showCommandPalette: Bool
    @Binding var focusMode: Bool
    let openShortcuts: () -> Void
    let openNotifications: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            breadcrumb
                .padding(.leading, 78)

            Spacer(minLength: 12)

            HStack(spacing: 8) {
                Label(memory.text, systemImage: "memorychip")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    .contentTransition(.numericText())
                    .animation(VPMotion.snap, value: memory.text)

                Button {
                    showCommandPalette = true
                } label: {
                    HStack(spacing: 5) {
                        Image(systemName: "command")
                            .font(.system(size: 9, weight: .semibold))
                        Text("Jump")
                            .font(.system(size: 9, weight: .medium, design: .monospaced))
                        Text("⌘K")
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.tertiary)
                    }
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                    .background(.white.opacity(0.045), in: Capsule())
                    .overlay {
                        Capsule().strokeBorder(.white.opacity(0.07), lineWidth: 1)
                    }
                }
                .buttonStyle(VPPressableButtonStyle(scale: 0.96))
                .help("Jump to a project, session, or action · ⌘K")

                Button(action: openNotifications) {
                    Image(systemName: model.unreadNoticeCount > 0 ? "bell.badge" : "bell")
                        .symbolEffect(.bounce, value: model.unreadNoticeCount)
                }
                .buttonStyle(VPGlassIconButtonStyle(size: 24, tint: .secondary))
                .overlay(alignment: .topTrailing) {
                    if model.unreadNoticeCount > 0 {
                        VPCountBadge(
                            count: model.unreadNoticeCount,
                            tint: .orange,
                            compact: true
                        )
                        .offset(x: 5, y: -4)
                    }
                }
                .help("Notifications")

                Button(action: openShortcuts) {
                    Image(systemName: "questionmark")
                }
                .buttonStyle(VPGlassIconButtonStyle(size: 24, tint: .secondary))
                .help("Keyboard shortcuts · ⌘/")

                Button {
                    withAnimation(VPMotion.smooth) {
                        focusMode.toggle()
                    }
                } label: {
                    Image(systemName: focusMode
                        ? "arrow.down.right.and.arrow.up.left"
                        : "arrow.up.left.and.arrow.down.right")
                }
                .buttonStyle(VPGlassIconButtonStyle(size: 24, tint: .secondary))
                .help(focusMode ? "Exit focus mode" : "Hide rail and studio · ⇧⌘F")
            }
        }
        .padding(.horizontal, 14)
        .frame(maxWidth: .infinity)
        .frame(height: 30)
        .background(.black.opacity(0.12))
        .overlay(alignment: .bottom) {
            VPGradientRule()
                .frame(height: 1)
        }
        .background(WindowDragArea())
    }

    private var breadcrumb: some View {
        HStack(spacing: 7) {
            VPStatusDot(
                color: model.connection == .connected ? .green : .orange,
                pulsing: model.connection == .connecting,
                size: 5
            )
            if let project = model.selectedProject {
                Text(project.name)
                    .font(.system(size: 9, weight: .medium, design: .monospaced))
                if let session = model.selectedSession {
                    Text("/")
                        .foregroundStyle(.quaternary)
                    Text(session.goal)
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                } else {
                    Text("/")
                        .foregroundStyle(.quaternary)
                    Text("no session")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
            } else {
                Text(model.mode == .resident ? "Resident · Aviiya bridge" : "No project")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
        }
        .contentTransition(.opacity)
        .animation(VPMotion.snap, value: model.selectedProjectID)
        .animation(VPMotion.snap, value: model.selectedSessionID)
        .lineLimit(1)
        .truncationMode(.middle)
        .frame(maxWidth: 420, alignment: .leading)
    }
}
