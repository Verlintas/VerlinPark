import AppKit
import SwiftUI

struct ConnectionBadge: View {
    let state: AppModel.ConnectionState

    var body: some View {
        HStack(spacing: 6) {
            VPStatusDot(
                color: color,
                pulsing: state == .connecting,
                size: 5
            )
            Text(L10n.t(label))
                .font(VPTheme.mono(8, weight: .medium))
                .foregroundStyle(VPTheme.textSecondary)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(VPTheme.surface, in: Capsule())
        .overlay {
            Capsule().strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
    }

    private var color: Color {
        switch state {
        case .connected: .green
        case .connecting: .orange
        case .idle: .secondary
        case .failed: .red
        }
    }

    private var label: String {
        switch state {
        case .connected: "connected"
        case .connecting: "connecting"
        case .idle: "idle"
        case .failed: "offline"
        }
    }
}

struct StatusPill: View {
    let status: String

    var body: some View {
        HStack(spacing: 5) {
            VPStatusDot(
                color: VPTheme.statusColor(status),
                pulsing: ["running", "verifying", "merging", "thinking"].contains(
                    status.lowercased()
                ),
                size: 4
            )
            Text(L10n.t(status).uppercased())
                .font(VPTheme.mono(8, weight: .semibold))
                .tracking(0.4)
        }
        .padding(.horizontal, 7)
        .padding(.vertical, 3)
        .background(VPTheme.statusColor(status).opacity(0.13), in: Capsule())
        .foregroundStyle(VPTheme.statusColor(status))
        .overlay {
            Capsule().strokeBorder(VPTheme.statusColor(status).opacity(0.24), lineWidth: 1)
        }
    }
}

/// A deliberately generous close affordance. Small 8pt glyphs with an 18pt
/// hit target are hard to click, especially in tab strips that sit on dense
/// glass surfaces. This keeps a bold glyph, a visible circular surface, and a
/// full 24-28pt target without adding visual noise when idle.
struct VPCloseButton: View {
    var size: CGFloat = 24
    var help: String = L10n.t("Close")
    var tint: Color = VPTheme.textSecondary
    let action: () -> Void
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            Image(systemName: "xmark")
                .font(.system(size: max(9, size * 0.44), weight: .bold))
                .foregroundStyle(hovering ? VPTheme.textPrimary : tint)
                .frame(width: size, height: size)
                .background(
                    hovering ? VPTheme.surfaceHover : VPTheme.surface.opacity(0.72),
                    in: Circle()
                )
                .overlay {
                    Circle()
                        .strokeBorder(
                            hovering ? VPTheme.accent.opacity(0.30) : VPTheme.hairline,
                            lineWidth: 1
                        )
                }
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .help(help)
        .accessibilityLabel(help)
        .onHover { hovering = $0 }
        .scaleEffect(hovering && !reduceMotion ? 1.06 : 1)
        .animation(VPMotion.resolved(VPMotion.hover, reduceMotion: reduceMotion), value: hovering)
    }
}

struct ErrorBanner: View {
    let message: String
    let dismiss: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(VPTheme.warning)
            Text(message)
                .font(VPTheme.font(11, weight: .medium))
                .foregroundStyle(VPTheme.textPrimary)
                .lineLimit(2)
            Spacer()
            VPCloseButton(size: 24, help: L10n.t("Dismiss")) {
                dismiss()
            }
        }
        .padding(.horizontal, 13)
        .padding(.vertical, 10)
        .background(VPTheme.surfaceRaised, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(VPTheme.warning.opacity(0.28), lineWidth: 1)
        }
        .shadow(color: .black.opacity(0.32), radius: 20, y: 9)
        .padding(.horizontal, 18)
        .padding(.top, 6)
    }
}

struct WindowDragArea: NSViewRepresentable {
    func makeNSView(context _: Context) -> NSView {
        let view = DragNSView()
        view.translatesAutoresizingMaskIntoConstraints = false
        return view
    }

    func updateNSView(_: NSView, context _: Context) {}
}

private final class DragNSView: NSView {
    override var mouseDownCanMoveWindow: Bool {
        true
    }
}

struct WindowAccessor: NSViewRepresentable {
    var configure: (NSWindow) -> Void

    func makeNSView(context _: Context) -> NSView {
        let view = NSView()
        DispatchQueue.main.async {
            if let window = view.window {
                configure(window)
            }
        }
        return view
    }

    func updateNSView(_ view: NSView, context _: Context) {
        DispatchQueue.main.async {
            if let window = view.window {
                configure(window)
            }
        }
    }
}

struct MemoryPill: View {
    @Environment(MemoryMonitor.self) private var memory

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: "memorychip")
                .font(.system(size: 9))
            Text(memory.text)
                .font(VPTheme.mono(8))
        }
        .foregroundStyle(VPTheme.textTertiary)
    }
}

struct CompactIconButton: View {
    let icon: String
    let help: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: icon)
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(VPTheme.textSecondary)
                .frame(width: 26, height: 26)
                .background(
                    VPTheme.surface,
                    in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                )
                .overlay {
                    RoundedRectangle(cornerRadius: 8, style: .continuous)
                        .strokeBorder(VPTheme.hairline, lineWidth: 1)
                }
        }
        .buttonStyle(VPPressableButtonStyle(scale: 0.92))
        .help(help)
    }
}
