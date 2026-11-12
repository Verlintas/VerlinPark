import SwiftUI

/// Read-only OTel GenAI projection status plus explicit local export/test
/// actions. Nothing here starts a background exporter, and no prompt, tool
/// argument, tool result, file content, or secret is displayed or sent by the
/// app itself.
struct OtelStatusSection: View {
    @Environment(AppModel.self) private var model
    let status: OtelStatusReport

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                SectionLabel(title: "OTel projection")
                Spacer()
                Text(status.enabled ? "EXPLICIT" : "OFF")
                    .font(VPTheme.mono(7, weight: .semibold))
                    .padding(.horizontal, 5)
                    .padding(.vertical, 2)
                    .foregroundStyle(status.enabled ? VPTheme.accent : VPTheme.textTertiary)
                    .background(
                        (status.enabled ? VPTheme.accent : VPTheme.textTertiary).opacity(0.10),
                        in: Capsule()
                    )
            }
            Text(status.countsLabel)
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textSecondary)
            Text(status.endpointLabel)
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textTertiary)
                .lineLimit(1)
                .truncationMode(.middle)
            Text(status.endpointLoopback ? "Loopback-only collector" : "Endpoint rejected")
                .font(VPTheme.mono(7))
                .foregroundStyle(
                    status.endpointLoopback ? VPTheme.textTertiary : VPTheme.warning
                )
            if let message = model.otelMessage {
                Text(message)
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack(spacing: 8) {
                Button {
                    Task { await model.exportOtelTraces() }
                } label: {
                    Label("Export", systemImage: "arrow.up.doc")
                        .font(VPTheme.mono(8))
                }
                .buttonStyle(.borderless)
                .disabled(
                    !status.enabled || model.selectedSessionID == nil || model.otelBusy
                )
                .help("Explicitly POST this session's bounded OTel projection to the local collector.")
                Button {
                    Task { await model.testOtelCollector() }
                } label: {
                    Label("Test collector", systemImage: "waveform.path.ecg")
                        .font(VPTheme.mono(8))
                }
                .buttonStyle(.borderless)
                .disabled(model.otelBusy)
                .help("Send one synthetic span; no session content is included.")
                if model.otelBusy {
                    ProgressView()
                        .controlSize(.mini)
                }
            }
            Text("No background exporter. Counts exclude prompts, tool arguments, results, and file content.")
                .font(VPTheme.mono(6))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(8)
        .background(
            VPTheme.surfaceRaised.opacity(0.35),
            in: RoundedRectangle(cornerRadius: 8, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
    }
}
