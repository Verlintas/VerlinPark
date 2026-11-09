import SwiftUI

struct RootView: View {
    @Environment(AppModel.self) private var model
    @Environment(BrandStore.self) private var brand
    @Environment(MemoryMonitor.self) private var memory
    @Environment(LanguageStore.self) private var language
    @Environment(WindowStateStore.self) private var windowState
    @Environment(WorkspaceLayoutStore.self) private var layoutStore
    @Environment(WorkspaceCommandRouter.self) private var commandRouter

    @State private var showDashboard = false
    @State private var showNewSession = false
    @State private var showSearch = false
    @State private var showShortcuts = false
    @State private var showNotifications = false
    @State private var showCommandPalette = false
    @State private var focusMode = false
    @State private var studioTab: StudioTab = .collab
    @State private var showSetupWizard = false
    @AppStorage("verlinpark.onboarding.completed") private var onboardingCompleted = false

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
            .sheet(isPresented: $showSetupWizard) {
                SetupWizardView {
                    onboardingCompleted = true
                }
                .environment(model)
            }
            .overlay { commandPaletteOverlay }
            .overlay(alignment: .topTrailing) { toastOverlay }
            .onChange(of: commandRouter.sequence) { _, _ in
                handle(commandRouter.lastCommand)
            }
            .animation(VPMotion.snap, value: showCommandPalette)
            .animation(VPMotion.smooth, value: focusMode)
            .task {
                guard !onboardingCompleted else { return }
                try? await Task.sleep(for: .seconds(1.2))
                if !onboardingCompleted {
                    showSetupWizard = true
                }
            }
    }

    private var workspace: some View {
        ZStack {
            VPAmbientBackground()

            GeometryReader { proxy in
                let layout = WorkspaceLayout(
                    railWidth: CGFloat(layoutStore.railWidth),
                    inspectorWidth: CGFloat(layoutStore.inspectorWidth),
                    inspectorVisible: layoutStore.inspectorVisible
                )
                .resolved(windowWidth: proxy.size.width, focusMode: focusMode)

                HStack(spacing: 0) {
                    if layout.railVisible {
                        RailView(
                            showDashboard: $showDashboard,
                            showCommandPalette: $showCommandPalette,
                            showingNotifications: $showNotifications,
                            showingNewSession: $showNewSession
                        )
                        .frame(width: layout.railWidth)
                        .transition(.vpSidebar)
                        .vpAppear(index: 0, animation: VPMotion.smooth)

                        VPSplitHandle(
                            value: railWidthBinding,
                            range: WorkspaceLayout.railRange,
                            direction: .rightIncreases,
                            onReset: {
                                layoutStore.railWidth = Double(WorkspaceLayout.defaultRailWidth)
                            }
                        )
                        .transition(.opacity)
                    }

                    StageView(
                        showingNewSession: $showNewSession,
                        showingSearch: $showSearch,
                        inspectorVisible: inspectorBinding
                    )
                    .frame(maxWidth: .infinity)
                    .vpAppear(index: 1, animation: VPMotion.smooth)

                    if layout.inspectorVisible {
                        VPSplitHandle(
                            value: inspectorWidthBinding,
                            range: WorkspaceLayout.inspectorRange,
                            direction: .leftIncreases,
                            onReset: {
                                layoutStore.inspectorWidth = Double(WorkspaceLayout.defaultInspectorWidth)
                            }
                        )
                        .transition(.opacity)

                        StudioView(tab: $studioTab)
                            .frame(width: layout.inspectorWidth)
                            .transition(.vpInspector)
                            .vpAppear(index: 2, animation: VPMotion.smooth)
                    }
                }
                .padding(.top, 36)
                .animation(VPMotion.smooth, value: focusMode)
                .animation(VPMotion.smooth, value: layoutStore.inspectorVisible)
            }
        }
    }

    private var railWidthBinding: Binding<CGFloat> {
        Binding(
            get: { CGFloat(layoutStore.railWidth) },
            set: { layoutStore.railWidth = Double($0) }
        )
    }

    private var inspectorWidthBinding: Binding<CGFloat> {
        Binding(
            get: { CGFloat(layoutStore.inspectorWidth) },
            set: { layoutStore.inspectorWidth = Double($0) }
        )
    }

    private var inspectorBinding: Binding<Bool> {
        Binding(
            get: { layoutStore.inspectorVisible },
            set: { layoutStore.inspectorVisible = $0 }
        )
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
            windowState.install(window)
        }
    }

    private var topChrome: some View {
        TopChromeView(
            showCommandPalette: $showCommandPalette,
            focusMode: $focusMode,
            inspectorVisible: inspectorBinding,
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
                toggleInspector: { layoutStore.inspectorVisible.toggle() },
                toggleFocusMode: { focusMode.toggle() },
                resetLayout: {
                    windowState.reset()
                    layoutStore.reset()
                },
                applyLayoutPreset: { preset in
                    layoutStore.apply(preset)
                },
                toggleLanguage: {
                    language.select(
                        language.language == .english ? .simplifiedChinese : .english
                    )
                }
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

    private func handle(_ command: WorkspaceCommand?) {
        guard let command else { return }
        switch command {
        case .commandPalette:
            showCommandPalette = true
        case .newSession:
            showNewSession = true
        case .search:
            showSearch = true
        case .dashboard:
            showDashboard = true
        case .notifications:
            showNotifications = true
        case .shortcuts:
            showShortcuts = true
        case .focusMode:
            withAnimation(VPMotion.smooth) {
                focusMode.toggle()
            }
        case .inspector:
            withAnimation(VPMotion.smooth) {
                if focusMode {
                    focusMode = false
                    layoutStore.inspectorVisible = true
                } else {
                    layoutStore.inspectorVisible.toggle()
                }
            }
        case .resetLayout:
            withAnimation(VPMotion.smooth) {
                focusMode = false
            }
            layoutStore.reset()
            windowState.reset()
        case .addProject:
            Task { await model.addProject() }
        case .refresh:
            Task { await model.refresh() }
        case .toggleLanguage:
            language.select(
                language.language == .english ? .simplifiedChinese : .english
            )
        }
    }
}
