import SwiftUI

@main
struct VerlinParkApp: App {
    @State private var model = AppModel()
    @State private var brand = BrandStore()
    @State private var memory = MemoryMonitor()
    @State private var language = LanguageStore()

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(model)
                .environment(brand)
                .environment(memory)
                .environment(language)
                .environment(\.locale, language.locale)
                .task {
                    memory.start()
                    await model.start()
                }
        }
        .windowStyle(.hiddenTitleBar)
        .defaultSize(width: 1320, height: 860)
        .commands {
            CommandGroup(after: .newItem) {
                Button(L10n.t("Chat with Aviiya")) {
                    model.selectResident()
                }
                .keyboardShortcut("1", modifiers: [.command])

                Button(L10n.t("Add Project…")) {
                    Task { await model.addProject() }
                }
                .keyboardShortcut("o", modifiers: [.command, .shift])

                Button(L10n.t("Refresh")) {
                    Task { await model.refresh() }
                }
                .keyboardShortcut("r", modifiers: [.command])
            }
        }

        Settings {
            SettingsView()
                .environment(brand)
                .environment(model)
                .environment(language)
                .environment(\.locale, language.locale)
        }
    }
}
