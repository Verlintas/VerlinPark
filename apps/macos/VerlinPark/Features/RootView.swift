import SwiftUI

struct RootView: View {
    @Environment(AppModel.self) private var model
    @Environment(BrandStore.self) private var brand
    @Environment(MemoryMonitor.self) private var memory
    @Environment(LanguageStore.self) private var language
    @Environment(\.openSettings) private var openSettings

    @State private var showDashboard = false
    @State private var showNewSession = false
    @State private var showSearch = false
    @State private var showShortcuts = false
    @State private var showNotifications = false
    @State private var showCommandPalette = false
    @State private var focusMode = false
    @AppStorage("verlinpark.inspector.visible") private var inspectorVisible = true
    @State private var studioTab: StudioTab = .collab

    var body: some View {
        workspace
            .environment(\.locale, language.locale)
            .id(language.language.rawValue)
            .frame(minWidth: 1120, minHeight: 720)
            .preferredColorScheme(.dark)
            .tint(VPTheme.accent)
            .background { windowConfiguration }
            .overlay(alignment: .top) { topChrome }
            .overlay(alignment: .topTrailing) { errorBanner }
            .animation(VPMotion.snap, value: model.lastError)
            .onChange(of: brand.config.displayName) { _, _ in
                NSApp.mainWindow?.title = brand.config.effectiveName
            }
            .sheet(isPresented: $showDashboard) { dashboardSheet }
            .sheet(isPresented: $showNewSession) { newSessionSheet }
            .sheet(isPresented: $showSearch) { searchSheet }
            .sheet(isPresented: $showShortcuts) {
                KeyboardShortcutsSheet()
                    .environment(model)
            }
            .sheet(isPresented: $showNotifications) {
                NotificationCenterView()
                    .environment(model)
            }
            .overlay { commandPaletteOverlay }
            .overlay(alignment: .topTrailing) { toastOverlay }
            .overlay(alignment: .topTrailing) { focusExitOverlay }
            .background { keyboardShortcuts }
            .animation(VPMotion.snap, value: showCommandPalette)
            .animation(VPMotion.smooth, value: focusMode)
    }

    private var workspace: some View {
        ZStack {
            VPAmbientBackground()

            HStack(spacing: 0) {
                if !focusMode {
                    RailView(
                        showDashboard: $showDashboard,
                        showCommandPalette: $showCommandPalette,
                        showingNotifications: $showNotifications,
                        showingNewSession: $showNewSession
                    )
                    .frame(width: 272)
                    .transition(.vpSidebar)
                    .vpAppear(index: 0, animation: VPMotion.smooth)

                    Rectangle()
                        .fill(VPTheme.hairline)
                        .frame(width: 1)
                        .transition(.opacity)
                }

                StageView(
                    showingNewSession: $showNewSession,
                    showingSearch: $showSearch,
                    inspectorVisible: $inspectorVisible
                )
                .frame(maxWidth: .infinity)
                .vpAppear(index: 1, animation: VPMotion.smooth)

                if !focusMode, inspectorVisible {
                    Rectangle()
                        .fill(VPTheme.hairline)
                        .frame(width: 1)
                        .transition(.opacity)

                    StudioView(tab: $studioTab)
                        .frame(width: 382)
                        .transition(.vpInspector)
                        .vpAppear(index: 2, animation: VPMotion.smooth)
                }
            }
            .padding(.top, 36)
            .animation(VPMotion.smooth, value: focusMode)
            .animation(VPMotion.smooth, value: inspectorVisible)
        }
    }

    private var windowConfiguration: some View {
        WindowAccessor { window in
            window.title = brand.config.effectiveName
            window.titleVisibility = .hidden
            window.titlebarAppearsTransparent = true
            window.styleMask.insert(.fullSizeContentView)
            window.isOpaque = false
            window.backgroundColor = .clear
            window.isMovableByWindowBackground = true
        }
    }

    private var topChrome: some View {
        TopChromeView(
            showCommandPalette: $showCommandPalette,
            focusMode: $focusMode,
            inspectorVisible: $inspectorVisible,
            openShortcuts: { showShortcuts = true },
            openNotifications: { showNotifications = true }
        )
        .environment(model)
        .environment(memory)
        .zIndex(1)
    }

    @ViewBuilder
    private var errorBanner: some View {
        if let error = model.lastError {
            ErrorBanner(message: error) {
                model.clearError()
            }
            .frame(maxWidth: 460)
            .padding(.top, 48)
            .padding(.trailing, 16)
            .transition(.vpTop)
            .zIndex(2)
        }
    }

    private var dashboardSheet: some View {
        DashboardView()
            .environment(model)
            .environment(brand)
            .environment(memory)
    }

    private var newSessionSheet: some View {
        NewSessionSheet()
            .environment(model)
            .environment(brand)
    }

    private var searchSheet: some View {
        SearchSheet()
            .environment(model)
            .environment(brand)
    }

    @ViewBuilder
    private var commandPaletteOverlay: some View {
        if showCommandPalette {
            CommandPaletteView(
                isPresented: $showCommandPalette,
                openDashboard: { showDashboard = true },
                openNewSession: { showNewSession = true },
                openSearch: { showSearch = true },
                openShortcuts: { showShortcuts = true },
                openNotifications: { showNotifications = true },
                toggleInspector: { inspectorVisible.toggle() },
                toggleFocusMode: { focusMode.toggle() }
            )
            .environment(model)
            .environment(brand)
            .zIndex(20)
            .transition(.opacity)
        }
    }

    private var toastOverlay: some View {
        VPToastStack(toasts: model.toasts, dismiss: model.dismissToast)
            .padding(.trailing, 18)
            .padding(.top, 78)
            .zIndex(30)
    }

    @ViewBuilder
    private var focusExitOverlay: some View {
        if focusMode {
            Button {
                withAnimation(VPMotion.smooth) {
                    focusMode = false
                }
            } label: {
                Label("Exit Focus", systemImage: "arrow.down.right.and.arrow.up.left")
            }
            .buttonStyle(VPGlassIconButtonStyle(size: 30, tint: .secondary))
            .padding(.top, 40)
            .padding(.trailing, 18)
            .transition(.vpScaleFade)
            .help("Show the rail and studio")
        }
    }

    private var keyboardShortcuts: some View {
        ZStack {
            Button("") { showCommandPalette = true }
                .keyboardShortcut("k", modifiers: .command)
            Button("") {
                withAnimation(VPMotion.smooth) {
                    focusMode.toggle()
                }
            }
            .keyboardShortcut("f", modifiers: [.command, .shift])
            Button("") { showNewSession = true }
                .keyboardShortcut("n", modifiers: .command)
            Button("") { showDashboard = true }
                .keyboardShortcut("d", modifiers: [.command, .shift])
            Button("") { showShortcuts = true }
                .keyboardShortcut("/", modifiers: .command)
            Button("") { inspectorVisible.toggle() }
                .keyboardShortcut("i", modifiers: [.command, .option])
        }
        .opacity(0)
        .frame(width: 0, height: 0)
        .accessibilityHidden(true)
    }
}
