import AppKit
import SwiftUI

/// The one user-consented write surface: local standard + forensic bundles for
/// a developer. It never uploads, never accepts a path, and never mutates
/// VerlinPark state.
struct CustosDiagnosticsSheet: View {
    @Environment(CustosModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Text("DIAGNOSTICS")
                    .font(CustosTheme.mono(11, weight: .semibold))
                    .tracking(1.8)
                    .foregroundStyle(CustosTheme.ice)
                Spacer()
                Text("LOCAL · MANUAL · NO UPLOAD")
                    .font(CustosTheme.mono(8))
                    .tracking(0.8)
                    .foregroundStyle(CustosTheme.textTertiary)
            }

            Text(
                "Generate two local bundles: standard (minimal, redacted) and "
                    + "forensic (more state/log detail). Both exclude provider keys, "
                    + "prompts, messages, tool output, file contents, screen content, "
                    + "cookies, full paths, process argv, and stack samples. Nothing is "
                    + "uploaded or sent automatically."
            )
            .font(CustosTheme.body(12))
            .foregroundStyle(CustosTheme.textSecondary)
            .lineSpacing(3)

            if model.isGeneratingDiagnostics {
                HStack(spacing: 10) {
                    ProgressView()
                        .controlSize(.small)
                    Text("WRITING LOCAL BUNDLES")
                        .font(CustosTheme.mono(9, weight: .semibold))
                        .tracking(0.9)
                        .foregroundStyle(CustosTheme.iceDim)
                }
            }

            if let result = model.diagnostics {
                VStack(alignment: .leading, spacing: 8) {
                    Text("REPORT \(result.reportID)")
                        .font(CustosTheme.mono(8, weight: .semibold))
                        .foregroundStyle(CustosTheme.textTertiary)
                    if let path = result.standardPath {
                        DiagnosticPathRow(label: "STANDARD", path: path)
                    }
                    if let path = result.forensicPath {
                        DiagnosticPathRow(label: "FORENSIC", path: path)
                    }
                }
                .padding(12)
                .background(CustosTheme.panelRaised)
                .overlay {
                    Rectangle().strokeBorder(CustosTheme.rule, lineWidth: 1)
                }
            }

            if let error = model.diagnosticsError {
                Text(error)
                    .font(CustosTheme.body(11))
                    .foregroundStyle(CustosTheme.critical)
                    .fixedSize(horizontal: false, vertical: true)
            }

            HStack(spacing: 10) {
                Button("Generate standard + forensic bundles") {
                    Task { await model.generateDiagnostics() }
                }
                .disabled(model.isGeneratingDiagnostics)

                if let result = model.diagnostics {
                    Button("Reveal in Finder") {
                        NSWorkspace.shared.activateFileViewerSelecting([
                            URL(fileURLWithPath: result.directory, isDirectory: true),
                        ])
                    }
                }

                Spacer()

                Button("Close") { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
        }
        .padding(24)
        .frame(width: 620)
        .background(CustosTheme.obsidian)
        .preferredColorScheme(.dark)
    }
}

private struct DiagnosticPathRow: View {
    let label: String
    let path: String

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text(label)
                .font(CustosTheme.mono(8, weight: .semibold))
                .tracking(0.7)
                .foregroundStyle(CustosTheme.iceDim)
                .frame(width: 64, alignment: .leading)
            Text(path)
                .font(CustosTheme.mono(9))
                .foregroundStyle(CustosTheme.textSecondary)
                .lineLimit(2)
                .truncationMode(.middle)
                .textSelection(.enabled)
        }
    }
}
