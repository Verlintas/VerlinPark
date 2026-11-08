import SwiftUI

/// Slim draggable chrome for the full-size window. It stays visually quiet:
/// breadcrumb on the left, operational state on the right, and no toolbar
/// chrome that would compete with the stage.
struct TopChromeView: View {
    @Environment(AppModel.self) private var model
    @Environment(MemoryMonitor.self) private var memory
    @Environment(LanguageStore.self) private var language
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Binding var showCommandPalette: Bool
    @Binding var focusMode: Bool
    @Binding var inspectorVisible: Bool
    let openShortcuts: () -> Void
    let openNotifications: () -> Void

    var body: some View {
        HStack(spacing: 12) {
            breadcrumb
                .padding(.leading, 86)

            Spacer(minLength: 12)

            HStack(spacing: 7) {
                runtimeState
                memoryLabel

                divider

                languageMenu
                commandButton
                if !focusMode {
                    inspectorButton
                }
                notificationsButton
                shortcutsButton
                focusButton
            }
        }
        .padding(.horizontal, 14)
        .frame(maxWidth: .infinity)
        .frame(height: 36)
        .background {
            VPCrystalSurface(
                tone: VPTheme.rail,
                solidTone: VPTheme.solidRail,
                tintOpacity: 0.62,
                material: .thinMaterial
            )
        }
        .overlay(alignment: .bottom) {
            VPHairline()
        }
        .background(WindowDragArea())
    }

    // MARK: Breadcrumb

    private var breadcrumb: some View {
        HStack(spacing: 8) {
            if model.mode == .project, let project = model.selectedProject {
                RoundedRectangle(cornerRadius: 4, style: .continuous)
                    .fill(VPTheme.projectTint(seed: project.id).opacity(0.18))
                    .frame(width: 16, height: 16)
                    .overlay {
                        Text(String(project.name.prefix(1)).uppercased())
                            .font(.system(size: 7, weight: .bold, design: .rounded))
                            .foregroundStyle(VPTheme.projectTint(seed: project.id))
                    }

                Text(project.name)
                    .font(VPTheme.mono(9, weight: .medium))
                    .foregroundStyle(VPTheme.textSecondary)
                    .lineLimit(1)

                if let session = model.selectedSession {
                    Image(systemName: "chevron.right")
                        .font(.system(size: 6, weight: .bold))
                        .foregroundStyle(VPTheme.textTertiary)
                    Text(session.goal)
                        .font(VPTheme.font(10, weight: .medium))
                        .foregroundStyle(VPTheme.textPrimary)
                        .lineLimit(1)
                    StatusPill(status: session.status)
                } else {
                    Image(systemName: "chevron.right")
                        .font(.system(size: 6, weight: .bold))
                        .foregroundStyle(VPTheme.textTertiary)
                    Text(L10n.t("no session"))
                        .font(VPTheme.mono(9))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            } else {
                HStack(spacing: 6) {
                    Image(systemName: "cube.transparent")
                        .font(.system(size: 8, weight: .bold))
                        .foregroundStyle(VPTheme.resident)
                    Text(model.mode == .resident ? L10n.t("Resident · Aviiya bridge") : L10n.t("No project"))
                        .font(VPTheme.mono(9, weight: .medium))
                        .foregroundStyle(VPTheme.textSecondary)
                }
            }
        }
        .contentTransition(.opacity)
        .animation(motion(VPMotion.snap), value: model.selectedProjectID)
        .animation(motion(VPMotion.snap), value: model.selectedSessionID)
        .lineLimit(1)
        .truncationMode(.middle)
        .frame(maxWidth: 520, alignment: .leading)
    }

    // MARK: Runtime meters

    private var runtimeState: some View {
        HStack(spacing: 5) {
            VPStatusDot(
                color: model.connection == .connected ? VPTheme.success : VPTheme.warning,
                pulsing: model.connection == .connecting,
                size: 4
            )
            Text(connectionLabel)
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
        }
    }

    private var memoryLabel: some View {
        HStack(spacing: 4) {
            Image(systemName: "memorychip")
                .font(.system(size: 7))
            Text(memory.text)
                .font(VPTheme.mono(8))
                .contentTransition(.numericText())
        }
        .foregroundStyle(VPTheme.textTertiary)
        .animation(motion(VPMotion.snap), value: memory.text)
    }

    private var divider: some View {
        Rectangle()
            .fill(VPTheme.hairline)
            .frame(width: 1, height: 14)
            .padding(.horizontal, 1)
    }

    // MARK: Actions

    private var languageMenu: some View {
        Menu {
            ForEach(AppLanguage.allCases) { option in
                Button {
                    language.select(option)
                } label: {
                    if language.language == option {
                        Label(option.endonym, systemImage: "checkmark")
                    } else {
                        Text(option.endonym)
                    }
                }
            }
        } label: {
            HStack(spacing: 4) {
                Image(systemName: "globe")
                    .font(.system(size: 8, weight: .semibold))
                Text(language.language.shortLabel)
                    .font(VPTheme.mono(8, weight: .semibold))
            }
            .foregroundStyle(VPTheme.textSecondary)
            .padding(.horizontal, 7)
            .frame(height: 24)
            .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 7, style: .continuous))
            .overlay {
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .strokeBorder(VPTheme.edgeGradient, lineWidth: 1)
            }
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .help(L10n.t("Language"))
    }

    private var commandButton: some View {
        Button {
            showCommandPalette = true
        } label: {
            HStack(spacing: 5) {
                Image(systemName: "command")
                    .font(.system(size: 8, weight: .semibold))
                Text("Jump")
                    .font(VPTheme.mono(8, weight: .medium))
                VPKeyCap(text: "⌘K")
            }
            .foregroundStyle(VPTheme.textSecondary)
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(VPTheme.surface, in: Capsule())
            .overlay {
                Capsule().strokeBorder(VPTheme.hairline, lineWidth: 1)
            }
        }
        .buttonStyle(VPPressableButtonStyle(scale: 0.97))
        .help("Jump to a project, session, or action · ⌘K")
    }

    private var inspectorButton: some View {
        Button {
            withAnimation(motion(VPMotion.smooth)) {
                inspectorVisible.toggle()
            }
        } label: {
            Image(systemName: "sidebar.right")
                .symbolVariant(inspectorVisible ? .fill : .none)
        }
        .buttonStyle(
            VPGlassIconButtonStyle(
                size: 24,
                tint: inspectorVisible ? VPTheme.accent : VPTheme.textSecondary
            )
        )
        .help(inspectorVisible ? "Hide inspector · ⌥⌘I" : "Show inspector · ⌥⌘I")
    }

    private var notificationsButton: some View {
        Button(action: openNotifications) {
            Image(systemName: model.unreadNoticeCount > 0 ? "bell.badge" : "bell")
                .symbolEffect(.bounce, value: model.unreadNoticeCount)
        }
        .buttonStyle(VPGlassIconButtonStyle(size: 24, tint: VPTheme.textSecondary))
        .overlay(alignment: .topTrailing) {
            if model.unreadNoticeCount > 0 {
                Circle()
                    .fill(VPTheme.accent)
                    .frame(width: 5, height: 5)
                    .overlay {
                        Circle().strokeBorder(VPTheme.rail, lineWidth: 1.5)
                    }
                    .offset(x: 2, y: -1)
            }
        }
        .help("Notifications")
    }

    private var shortcutsButton: some View {
        Button(action: openShortcuts) {
            Image(systemName: "questionmark")
        }
        .buttonStyle(VPGlassIconButtonStyle(size: 24, tint: VPTheme.textSecondary))
        .help("Keyboard shortcuts · ⌘/")
    }

    @ViewBuilder
    private var focusButton: some View {
        if focusMode {
            Button {
                withAnimation(motion(VPMotion.smooth)) {
                    focusMode = false
                }
            } label: {
                HStack(spacing: 5) {
                    Image(systemName: "arrow.down.right.and.arrow.up.left")
                        .font(.system(size: 8, weight: .semibold))
                    Text(L10n.t("Exit Focus"))
                        .font(VPTheme.font(9, weight: .semibold))
                }
                .foregroundStyle(VPTheme.textPrimary)
                .padding(.horizontal, 8)
                .frame(height: 24)
                .background(VPTheme.surface, in: Capsule())
                .overlay {
                    Capsule().strokeBorder(VPTheme.edgeGradient, lineWidth: 1)
                }
            }
            .buttonStyle(VPPressableButtonStyle(scale: 0.96))
            .help(L10n.t("Show the rail and studio · ⇧⌘F"))
        } else {
            Button {
                withAnimation(motion(VPMotion.smooth)) {
                    focusMode = true
                }
            } label: {
                Image(systemName: "arrow.up.left.and.arrow.down.right")
            }
            .buttonStyle(VPGlassIconButtonStyle(size: 24, tint: VPTheme.textSecondary))
            .help(L10n.t("Hide rail and studio · ⇧⌘F"))
        }
    }

    private var connectionLabel: String {
        switch model.connection {
        case .connected: L10n.t("online")
        case .connecting: L10n.t("connecting")
        case .idle: L10n.t("idle")
        case .failed: L10n.t("offline")
        }
    }

    private func motion(_ animation: Animation) -> Animation? {
        VPMotion.resolved(animation, reduceMotion: reduceMotion)
    }
}
