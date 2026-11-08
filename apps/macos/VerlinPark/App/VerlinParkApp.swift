import SwiftUI

@main
struct VerlinParkApp: App {
    @State private var model = AppModel()
    @State private var brand = BrandStore()
    @State private var memory = MemoryMonitor()
    @State private var language = LanguageStore()
    @State private var windowState = WindowStateStore()
    @State private var layoutStore = WorkspaceLayoutStore()
    @State private var pinnedStore = PinnedItemsStore()
    @State private var commandRouter = WorkspaceCommandRouter()

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(model)
                .environment(brand)
                .environment(memory)
                .environment(language)
                .environment(windowState)
                .environment(layoutStore)
                .environment(pinnedStore)
                .environment(commandRouter)
                .environment(\.locale, language.locale)
                .task {
                    memory.start()
                    await model.start()
                }
        }
        .windowStyle(.hiddenTitleBar)
        .defaultSize(width: 1320, height: 860)
        .commands {
            CommandGroup(replacing: .newItem) {
                Button(L10n.t("New Session")) {
                    commandRouter.send(.newSession)
                }
                .keyboardShortcut("n", modifiers: [.command])

                Button(L10n.t("Chat with Aviiya")) {
                    model.selectResident()
                }
                .keyboardShortcut("1", modifiers: [.command])

                Button(L10n.t("Add Project…")) {
                    commandRouter.send(.addProject)
                }
                .keyboardShortcut("o", modifiers: [.command, .shift])
            }

            CommandMenu(L10n.t("Workspace")) {
                Button(L10n.t("Command Palette")) {
                    commandRouter.send(.commandPalette)
                }
                .keyboardShortcut("k", modifiers: [.command])

                Button(L10n.t("Search Messages & Events")) {
                    commandRouter.send(.search)
                }
                .keyboardShortcut("f", modifiers: [.command])

                Button(L10n.t("Manage Dash")) {
                    commandRouter.send(.dashboard)
                }
                .keyboardShortcut("d", modifiers: [.command, .shift])

                Button(L10n.t("Notifications")) {
                    commandRouter.send(.notifications)
                }

                Button(L10n.t("Keyboard Shortcuts")) {
                    commandRouter.send(.shortcuts)
                }
                .keyboardShortcut("/", modifiers: [.command])

                Divider()

                Button(L10n.t("Toggle Focus Mode")) {
                    commandRouter.send(.focusMode)
                }
                .keyboardShortcut("f", modifiers: [.command, .shift])

                Button(L10n.t("Toggle Inspector")) {
                    commandRouter.send(.inspector)
                }
                .keyboardShortcut("i", modifiers: [.command, .option])

                Button(L10n.t("Reset Window Layout")) {
                    commandRouter.send(.resetLayout)
                }
                .keyboardShortcut("0", modifiers: [.command, .option])

                Divider()

                Button(L10n.t("Switch Language")) {
                    commandRouter.send(.toggleLanguage)
                }

                Button(L10n.t("Refresh")) {
                    commandRouter.send(.refresh)
                }
                .keyboardShortcut("r", modifiers: [.command])
            }
        }

        Settings {
            SettingsView()
                .environment(brand)
                .environment(model)
                .environment(language)
                .environment(layoutStore)
                .environment(\.locale, language.locale)
        }
    }
}
