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
    static var groups: [KeyboardShortcutGroup] {
        [
            KeyboardShortcutGroup(
                title: L10n.t("Global"),
                entries: [
                    KeyboardShortcutEntry(title: L10n.t("Command palette"), key: "K", modifiers: [.command]),
                    KeyboardShortcutEntry(
                        title: L10n.t("Focus mode"),
                        key: "F",
                        modifiers: [.command, .shift]
                    ),
                    KeyboardShortcutEntry(
                        title: L10n.t("Toggle inspector"),
                        key: "I",
                        modifiers: [.command, .option]
                    ),
                    KeyboardShortcutEntry(
                        title: L10n.t("Reset window layout"),
                        key: "0",
                        modifiers: [.command, .option]
                    ),
                    KeyboardShortcutEntry(title: L10n.t("Search messages"), key: "F", modifiers: [.command]),
                    KeyboardShortcutEntry(
                        title: L10n.t("Refresh current panel"),
                        key: "R",
                        modifiers: [.command]
                    ),
                    KeyboardShortcutEntry(
                        title: L10n.t("Manage Dash"),
                        key: "D",
                        modifiers: [.command, .shift]
                    ),
                    KeyboardShortcutEntry(title: L10n.t("New session"), key: "N", modifiers: [.command]),
                    KeyboardShortcutEntry(title: L10n.t("Chat with Aviiya"), key: "1", modifiers: [.command]),
                    KeyboardShortcutEntry(
                        title: L10n.t("Add project"),
                        key: "O",
                        modifiers: [.command, .shift]
                    ),
                    KeyboardShortcutEntry(
                        title: L10n.t("Keyboard shortcuts"),
                        key: "/",
                        modifiers: [.command]
                    ),
                ]
            ),
            KeyboardShortcutGroup(
                title: L10n.t("Navigation"),
                entries: [
                    KeyboardShortcutEntry(title: L10n.t("Move through palette"), key: "↑ / ↓", modifiers: []),
                    KeyboardShortcutEntry(title: L10n.t("Open highlighted result"), key: "↵", modifiers: []),
                    KeyboardShortcutEntry(title: L10n.t("Dismiss overlay"), key: "esc", modifiers: []),
                    KeyboardShortcutEntry(
                        title: L10n.t("Switch dashboard section"),
                        key: "1–9",
                        modifiers: [.command]
                    ),
                    KeyboardShortcutEntry(
                        title: L10n.t("Switch settings section"),
                        key: "1–8",
                        modifiers: [.command]
                    ),
                ]
            ),
            KeyboardShortcutGroup(
                title: L10n.t("Session"),
                entries: [
                    KeyboardShortcutEntry(title: L10n.t("New session"), key: "N", modifiers: [.command]),
                    KeyboardShortcutEntry(
                        title: L10n.t("Focus message composer"),
                        key: "L",
                        modifiers: [.command]
                    ),
                    KeyboardShortcutEntry(title: L10n.t("Send message"), key: "↵", modifiers: []),
                    KeyboardShortcutEntry(title: L10n.t("New line in composer"), key: "⇧↵", modifiers: []),
                ]
            ),
            KeyboardShortcutGroup(
                title: L10n.t("Project"),
                entries: [
                    KeyboardShortcutEntry(
                        title: L10n.t("Add project"),
                        key: "O",
                        modifiers: [.command, .shift]
                    ),
                ]
            ),
        ]
    }
}

struct KeyboardShortcutsSheet: View {
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 3) {
                    Text("SHORTCUTS")
                        .font(VPTheme.mono(8, weight: .semibold))
                        .tracking(1.2)
                        .foregroundStyle(VPTheme.accent)
                    Text("Keyboard shortcuts")
                        .font(VPTheme.font(20, weight: .semibold, rounded: true))
                    Text("Move through the workspace without leaving the keyboard.")
                        .font(VPTheme.mono(8.5))
                        .foregroundStyle(VPTheme.textSecondary)
                }
                Spacer()
                Button("Close") { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
            .padding(20)

            VPHairline()

            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    ForEach(KeyboardShortcutsCatalog.groups) { group in
                        shortcutSection(group)
                    }
                }
                .padding(20)
            }
        }
        .frame(width: 640, height: 620)
        .background { VPCrystalBackdrop() }
        .tint(VPTheme.accent)
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
                    .background(VPTheme.surface.opacity(0.55), in: RoundedRectangle(cornerRadius: 7))
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
            .font(VPTheme.mono(9, weight: .semibold))
            .foregroundStyle(VPTheme.textSecondary)
            .frame(minWidth: 18)
            .padding(.horizontal, 4)
            .padding(.vertical, 2)
            .background(VPTheme.surfaceRaised, in: RoundedRectangle(cornerRadius: 4))
            .overlay {
                RoundedRectangle(cornerRadius: 4)
                    .strokeBorder(VPTheme.hairline, lineWidth: 1)
            }
    }
}
