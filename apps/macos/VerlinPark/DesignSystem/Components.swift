import AppKit
import SwiftUI

struct ConnectionBadge: View {
    let state: AppModel.ConnectionState

    var body: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(color)
                .frame(width: 6, height: 6)
            Text(label)
                .font(.system(size: 10, weight: .medium, design: .monospaced))
                .foregroundStyle(.secondary)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(.ultraThinMaterial, in: Capsule())
        .overlay {
            Capsule().strokeBorder(.white.opacity(0.08), lineWidth: 1)
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
        Text(status.uppercased())
            .font(.system(size: 9, weight: .semibold, design: .monospaced))
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(color.opacity(0.16), in: Capsule())
            .foregroundStyle(color)
    }

    private var color: Color {
        switch status.lowercased() {
        case "done", "completed", "connected": .green
        case "failed", "cancelled", "offline": .red
        case "running", "verifying", "merging", "thinking": .orange
        case "paused": .yellow
        case "waiting", "idle": .secondary
        default: .secondary
        }
    }
}

struct ErrorBanner: View {
    let message: String
    let dismiss: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange)
            Text(message)
                .font(.callout)
                .lineLimit(2)
            Spacer()
            Button("Dismiss", action: dismiss)
                .buttonStyle(.link)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(.orange.opacity(0.3), lineWidth: 1)
        }
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
                .font(.system(size: 10))
            Text(memory.text)
                .font(.system(size: 10, design: .monospaced))
        }
        .foregroundStyle(.tertiary)
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
                .frame(width: 26, height: 26)
                .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        }
        .buttonStyle(.plain)
        .help(help)
    }
}
