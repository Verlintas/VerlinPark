import SwiftUI

struct KeyboardShortcutEntry: Identifiable {
    let id = UUID()
    let title: String
    let key: String
    let modifiers: EventModifiers
}

struct KeyboardShortcutGroup: Identifiable {
    var id: String {
        title
    }

    let title: String
    let entries: [KeyboardShortcutEntry]
}

enum KeyboardShortcutsCatalog {
    static let groups: [KeyboardShortcutGroup] = [
        KeyboardShortcutGroup(
            title: "Global",
            entries: [
                KeyboardShortcutEntry(title: "Command palette", key: "K", modifiers: [.command]),
                KeyboardShortcutEntry(title: "Focus mode", key: "F", modifiers: [.command, .shift]),
                KeyboardShortcutEntry(title: "Search messages", key: "F", modifiers: [.command]),
                KeyboardShortcutEntry(title: "Refresh current panel", key: "R", modifiers: [.command]),
                KeyboardShortcutEntry(title: "Manage Dash", key: "D", modifiers: [.command, .shift]),
                KeyboardShortcutEntry(title: "New session", key: "N", modifiers: [.command]),
                KeyboardShortcutEntry(title: "Chat with Aviiya", key: "1", modifiers: [.command]),
                KeyboardShortcutEntry(title: "Add project", key: "O", modifiers: [.command, .shift]),
                KeyboardShortcutEntry(title: "Keyboard shortcuts", key: "/", modifiers: [.command]),
            ]
        ),
        KeyboardShortcutGroup(
            title: "Navigation",
            entries: [
                KeyboardShortcutEntry(title: "Move through palette", key: "↑ / ↓", modifiers: []),
                KeyboardShortcutEntry(title: "Open highlighted result", key: "↵", modifiers: []),
                KeyboardShortcutEntry(title: "Dismiss overlay", key: "esc", modifiers: []),
                KeyboardShortcutEntry(title: "Switch dashboard section", key: "1–9", modifiers: [.command]),
                KeyboardShortcutEntry(title: "Switch settings section", key: "1–7", modifiers: [.command]),
            ]
        ),
        KeyboardShortcutGroup(
            title: "Session",
            entries: [
                KeyboardShortcutEntry(title: "New session", key: "N", modifiers: [.command]),
                KeyboardShortcutEntry(title: "Focus message composer", key: "L", modifiers: [.command]),
                KeyboardShortcutEntry(title: "Send message", key: "↵", modifiers: []),
                KeyboardShortcutEntry(title: "New line in composer", key: "⇧↵", modifiers: []),
            ]
        ),
        KeyboardShortcutGroup(
            title: "Project",
            entries: [
                KeyboardShortcutEntry(title: "Add project", key: "O", modifiers: [.command, .shift]),
            ]
        ),
    ]
}

struct KeyboardShortcutsSheet: View {
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 3) {
                    Text("Keyboard shortcuts")
                        .font(.system(size: 20, weight: .semibold, design: .rounded))
                    Text("Move through the workspace without leaving the keyboard.")
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button("Close") { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
            .padding(20)

            VPGradientRule()
                .frame(height: 1)

            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    ForEach(KeyboardShortcutsCatalog.groups) { group in
                        shortcutSection(group)
                    }
                }
                .padding(20)
            }
        }
        .frame(width: 620, height: 600)
        .background(.ultraThinMaterial)
    }

    private func shortcutSection(_ group: KeyboardShortcutGroup) -> some View {
        VStack(alignment: .leading, spacing: 7) {
            SectionLabel(title: group.title)
            VStack(spacing: 2) {
                ForEach(Array(group.entries.enumerated()), id: \.element.id) { index, entry in
                    HStack(spacing: 10) {
                        Text(entry.title)
                            .font(.system(size: 11))
                        Spacer()
                        ShortcutKeys(key: entry.key, modifiers: entry.modifiers)
                    }
                    .padding(.horizontal, 9)
                    .padding(.vertical, 6)
                    .background(.white.opacity(0.025), in: RoundedRectangle(cornerRadius: 7))
                    .vpHoverHighlight(radius: 7)
                    .vpAppear(index: index, animation: VPMotion.hover)
                }
            }
        }
    }
}

private struct ShortcutKeys: View {
    let key: String
    let modifiers: EventModifiers

    var body: some View {
        HStack(spacing: 3) {
            if modifiers.contains(.command) {
                ShortcutKey("⌘")
            }
            if modifiers.contains(.shift) {
                ShortcutKey("⇧")
            }
            if modifiers.contains(.option) {
                ShortcutKey("⌥")
            }
            ShortcutKey(key)
        }
    }
}

private struct ShortcutKey: View {
    let label: String

    init(_ label: String) {
        self.label = label
    }

    var body: some View {
        Text(label)
            .font(.system(size: 9, weight: .semibold, design: .monospaced))
            .foregroundStyle(.secondary)
            .frame(minWidth: 18)
            .padding(.horizontal, 4)
            .padding(.vertical, 2)
            .background(.white.opacity(0.06), in: RoundedRectangle(cornerRadius: 4))
            .overlay {
                RoundedRectangle(cornerRadius: 4)
                    .strokeBorder(.white.opacity(0.07), lineWidth: 1)
            }
    }
}
