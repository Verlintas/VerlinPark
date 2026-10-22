import SwiftUI

struct RootView: View {
    @Environment(AppModel.self) private var model
    @Environment(BrandStore.self) private var brand
    @Environment(MemoryMonitor.self) private var memory

    @State private var showDashboard = false
    @State private var showNewSession = false
    @State private var studioTab: StudioTab = .collab

    var body: some View {
        ZStack {
            GlassBackground(material: .underWindowBackground)

            HStack(spacing: 0) {
                RailView(showDashboard: $showDashboard)
                    .frame(width: 254)

                Rectangle()
                    .fill(.white.opacity(0.07))
                    .frame(width: 1)

                StageView(showingNewSession: $showNewSession)
                    .frame(maxWidth: .infinity)

                Rectangle()
                    .fill(.white.opacity(0.07))
                    .frame(width: 1)

                StudioView(tab: $studioTab)
                    .frame(width: 366)
            }
            .padding(.top, 30)
        }
        .frame(minWidth: 1120, minHeight: 720)
        .preferredColorScheme(.dark)
        .background {
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
        .onChange(of: brand.config.displayName) { _, _ in
            NSApp.mainWindow?.title = brand.config.effectiveName
        }
        .sheet(isPresented: $showDashboard) {
            DashboardView()
                .environment(model)
                .environment(brand)
                .environment(memory)
        }
        .sheet(isPresented: $showNewSession) {
            NewSessionSheet()
                .environment(model)
                .environment(brand)
        }
    }
}
