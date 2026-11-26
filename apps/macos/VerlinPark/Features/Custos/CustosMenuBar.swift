import AppKit
import SwiftUI

/// Menu-bar presence for the singleton Custos window. It mirrors the state
/// symbol and offers only open/close; it is not an operational surface.
struct CustosMenuBarLabel: View {
    @Environment(CustosModel.self) private var model

    var body: some View {
        Image(systemName: model.menuBarSymbol)
            .accessibilityLabel(model.menuBarAccessibilityLabel)
            .task { model.start() }
    }
}

struct CustosMenuBarMenu: View {
    @Environment(CustosModel.self) private var model
    @Environment(\.openWindow) private var openWindow
    @Environment(\.dismissWindow) private var dismissWindow

    var body: some View {
        Text("CUSTOS  /  \(model.stateWord)")
        Text(model.protectionLabel)
        Divider()
        Button("Open CUSTOS") {
            openWindow(id: "custos")
            NSApp.activate()
        }
        Button("Close CUSTOS") {
            dismissWindow(id: "custos")
        }
    }
}

/// Adds a single Window-menu entry for the singleton Custos window.
struct CustosCommands: Commands {
    @Environment(\.openWindow) private var openWindow

    var body: some Commands {
        CommandGroup(after: .windowList) {
            Divider()
            Button("CUSTOS") {
                openWindow(id: "custos")
                NSApp.activate()
            }
            .keyboardShortcut("u", modifiers: [.command, .shift])
        }
    }
}
