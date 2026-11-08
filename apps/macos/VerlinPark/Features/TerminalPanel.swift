import AppKit
import SwiftTerm
import SwiftUI

struct TerminalPanel: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                VPStatusDot(
                    color: statusColor,
                    pulsing: model.terminalStatus == "running",
                    size: 7
                )
                Text(model.terminalStatus)
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.secondary)
                if !model.terminalWorktree.isEmpty {
                    Text(model.terminalWorktree)
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.tertiary)
                        .lineLimit(1)
                }
                Spacer()
                if model.terminalBusy {
                    ProgressView().controlSize(.small)
                }
                if model.terminalID != nil {
                    Button("Close") {
                        Task { await model.closeTerminal() }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                } else {
                    Button("Open terminal") {
                        Task { await model.openTerminal() }
                    }
                    .buttonStyle(VPPrimaryButtonStyle(tint: .green))
                    .disabled(model.selectedSessionID == nil || model.terminalBusy)
                }
            }
            .padding(10)
            VPGradientRule()
                .frame(height: 1)
            if let terminalID = model.terminalID {
                TerminalSurface(
                    terminalID: terminalID,
                    data: model.terminalData,
                    onInput: { data in
                        Task { @MainActor in
                            await model.sendTerminalInput(data)
                        }
                    },
                    onResize: { cols, rows in
                        Task { @MainActor in
                            await model.resizeTerminal(cols: cols, rows: rows)
                        }
                    }
                )
                .id(terminalID)
                .transition(.vpContent)
                .background(SwiftUI.Color.black.opacity(0.18))
            } else {
                VStack(spacing: 10) {
                    VPFloatingSymbol(name: "terminal", size: 28, tint: .secondary)
                    Text("A full PTY terminal in the session worktree.")
                        .font(.system(size: 11, design: .monospaced))
                    Text(
                        "Runs under the same Seatbelt write sandbox and ShellPolicy as the agent shell. Full-screen TUI programs are supported by the terminal renderer; the daemon still owns the process."
                    )
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 420)
                    if let error = model.terminalError {
                        Text(error)
                            .font(.system(size: 9, design: .monospaced))
                            .foregroundStyle(.orange)
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .transition(.vpContent)
            }
        }
        .animation(VPMotion.smooth, value: model.terminalID)
        .task(id: model.terminalID) {
            guard model.terminalID != nil else { return }
            while !Task.isCancelled, model.terminalStatus == "running" {
                await model.pollTerminal()
                try? await Task.sleep(for: .milliseconds(100))
            }
        }
    }

    private var statusColor: SwiftUI.Color {
        switch model.terminalStatus {
        case "running": .green
        case "failed": .red
        case "exited": .secondary
        default: .secondary
        }
    }
}

private struct TerminalSurface: NSViewRepresentable {
    let terminalID: String
    let data: Data
    let onInput: (Data) -> Void
    let onResize: (Int, Int) -> Void

    func makeCoordinator() -> Coordinator {
        Coordinator(onInput: onInput, onResize: onResize)
    }

    func makeNSView(context: Context) -> TerminalView {
        let view = TerminalView(frame: .zero)
        view.terminalDelegate = context.coordinator
        view.font = NSFont.monospacedSystemFont(ofSize: 11, weight: .regular)
        view.nativeBackgroundColor = NSColor(calibratedWhite: 0.06, alpha: 1)
        view.nativeForegroundColor = NSColor(calibratedWhite: 0.92, alpha: 1)
        return view
    }

    func updateNSView(_ view: TerminalView, context: Context) {
        context.coordinator.onInput = onInput
        context.coordinator.onResize = onResize
        if data.count > context.coordinator.fedBytes {
            let start = data.startIndex + context.coordinator.fedBytes
            let slice = data[start...]
            view.feed(byteArray: ArraySlice([UInt8](slice)))
            context.coordinator.fedBytes = data.count
        } else if data.count < context.coordinator.fedBytes {
            context.coordinator.fedBytes = data.count
        }
    }

    final class Coordinator: NSObject, TerminalViewDelegate {
        var onInput: (Data) -> Void
        var onResize: (Int, Int) -> Void
        var fedBytes = 0

        init(onInput: @escaping (Data) -> Void, onResize: @escaping (Int, Int) -> Void) {
            self.onInput = onInput
            self.onResize = onResize
        }

        func sizeChanged(source _: TerminalView, newCols: Int, newRows: Int) {
            onResize(newCols, newRows)
        }

        func send(source _: TerminalView, data: ArraySlice<UInt8>) {
            onInput(Data(data))
        }

        func setTerminalTitle(source _: TerminalView, title _: String) {}
        func hostCurrentDirectoryUpdate(source _: TerminalView, directory _: String?) {}
        func scrolled(source _: TerminalView, position _: Double) {}

        func requestOpenLink(source _: TerminalView, link: String, params _: [String: String]) {
            guard let url = URL(string: link) else { return }
            NSWorkspace.shared.open(url)
        }

        func bell(source _: TerminalView) {
            NSSound.beep()
        }

        func clipboardCopy(source _: TerminalView, content: Data) {
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setData(content, forType: .string)
        }

        func clipboardRead(source _: TerminalView) -> Data? {
            NSPasteboard.general.data(forType: .string)
        }

        func iTermContent(source _: TerminalView, content _: ArraySlice<UInt8>) {}
        func rangeChanged(source _: TerminalView, startY _: Int, endY _: Int) {}
    }
}
