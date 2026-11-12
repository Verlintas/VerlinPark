import SwiftUI

/// Read-only daemon LaunchAgent supervision status.
///
/// Installation is deliberately a CLI action (`vp daemon-agent install`) so a
/// GUI click can never silently create a login item. This card reports the
/// user-level plist, launchd load state, and whether launchd owns the running
/// daemon. It never offers sudo and never starts or stops the daemon.
struct DaemonSupervisionCard: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Daemon supervision")
                    Text("User LaunchAgent · explicit install only")
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.daemonAgentBusy {
                    ProgressView().controlSize(.small)
                }
                Button {
                    Task { await model.loadDaemonAgentStatus() }
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .buttonStyle(VPGlassIconButtonStyle(size: 24))
                .help("Refresh launchctl status without starting the daemon")
                .disabled(model.daemonAgentBusy)
            }

            if let status = model.daemonAgentStatus {
                HStack(spacing: 8) {
                    statusPill(status.state, tint: tint(status))
                    statusPill(status.loaded ? "loaded" : "not loaded", tint: VPTheme.textSecondary)
                    statusPill(
                        status.disabled ? "disabled" : "enabled",
                        tint: status.disabled ? VPTheme.warning : VPTheme.success
                    )
                    statusPill(
                        status.supervised ? "launchd-owned" : "external",
                        tint: status.supervised ? VPTheme.accent : VPTheme.warning
                    )
                    Spacer()
                }
                if let pid = status.pid {
                    Text("pid \\(pid) · \\(status.label)")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textSecondary)
                }
                Text(status.plistPath)
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
                    .textSelection(.enabled)
                if !status.detail.isEmpty {
                    Text(status.detail)
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textSecondary)
                }
                if !status.launchctlAvailable {
                    Text("launchctl was unavailable; only the plist presence is known.")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.warning)
                }
            } else {
                Text("Status unavailable. The daemon keeps running without a LaunchAgent.")
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textSecondary)
            }

            Text(
                "Run `vp daemon-agent install` to create "
                    + "~/Library/LaunchAgents/com.verlinpark.daemon.plist, then "
                    + "`vp daemon-agent status` or `uninstall`. No sudo, no automatic installation, "
                    + "and no password prompt from this app."
            )
            .font(VPTheme.mono(7.5))
            .foregroundStyle(VPTheme.textTertiary)
            .fixedSize(horizontal: false, vertical: true)
        }
        .padding(14)
        .background(VPTheme.surfaceRaised.opacity(0.4), in: RoundedRectangle(cornerRadius: 14))
        .overlay {
            RoundedRectangle(cornerRadius: 14)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .task {
            if model.daemonAgentStatus == nil {
                await model.loadDaemonAgentStatus()
            }
        }
    }

    private func tint(_ status: DaemonAgentStatus) -> Color {
        switch status.state {
        case "running":
            status.supervised ? VPTheme.success : VPTheme.warning
        case "loaded", "installed":
            VPTheme.info
        case "disabled":
            VPTheme.warning
        case "unknown":
            VPTheme.textSecondary
        default:
            VPTheme.textTertiary
        }
    }

    private func statusPill(_ text: String, tint: Color) -> some View {
        Text(text.uppercased())
            .font(VPTheme.mono(6.5, weight: .semibold))
            .tracking(0.5)
            .foregroundStyle(tint)
            .padding(.horizontal, 6)
            .padding(.vertical, 3)
            .background(tint.opacity(0.12), in: Capsule())
    }
}
