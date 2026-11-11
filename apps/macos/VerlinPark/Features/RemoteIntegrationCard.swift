import SwiftUI

/// Manage Dash → Git. Remote integration is opt-in and operator-triggered; this
/// card is a read-only status surface for the project policy and remote-sourced
/// merge-queue entries. It never starts an integration from the UI.
struct RemoteIntegrationCard: View {
    @Environment(AppModel.self) private var model
    let project: Project

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Remote integration")
                    Text(project.name)
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if let status = model.remoteIntegrationStatus {
                    remoteBadge(status)
                }
                if model.remoteIntegrationBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Refresh") {
                    Task { await model.loadRemoteIntegrationStatus() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.remoteIntegrationBusy)
            }
            if let status = model.remoteIntegrationStatus {
                policyRow(status)
                if status.retainedRefs > 0 {
                    HStack(spacing: 8) {
                        Button("Prune merged refs") {
                            Task { await model.cleanupRemoteIntegrationRefs() }
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                        .disabled(model.remoteIntegrationBusy)
                        .help(
                            "Conflict/failed refs are retained for retry and require `vp remote-integration-cleanup --force`."
                        )
                        if let message = model.remoteIntegrationStatusMessage {
                            Text(message)
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                if status.entries.isEmpty {
                    Text(
                        "No remote bundle has entered this project's merge queue. Use `vp remote-integrate` with explicit confirmation after enabling the policy."
                    )
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
                } else {
                    ForEach(Array(status.entries.prefix(10))) { entry in
                        entryRow(entry)
                    }
                    if status.entries.count > 10 {
                        Text("… and \(status.entries.count - 10) more remote entries")
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.tertiary)
                    }
                }
                Text(status.environmentCaveat)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    .fixedSize(horizontal: false, vertical: true)
            } else if let message = model.remoteIntegrationStatusMessage {
                Text(message)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            } else {
                ProgressView().controlSize(.small)
            }
        }
        .padding(14)
        .background(
            .white.opacity(0.035),
            in: RoundedRectangle(cornerRadius: 14, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .vpAppear(index: 0)
    }

    private func remoteBadge(_ status: RemoteIntegrationStatusResult) -> some View {
        let active = status.pending + status.conflicts
        let color: Color =
            if !status.policy.enabled {
                .secondary
            } else if status.conflicts > 0 {
                .orange
            } else if active > 0 {
                .cyan
            } else {
                .green
            }
        return Text(
            !status.policy.enabled
                ? "OFF" : "\(status.total) REMOTE"
        )
        .font(.system(size: 8, weight: .semibold, design: .monospaced))
        .foregroundStyle(color)
        .padding(.horizontal, 7)
        .padding(.vertical, 3)
        .background(color.opacity(0.12), in: Capsule())
        .overlay {
            Capsule().strokeBorder(color.opacity(0.25), lineWidth: 1)
        }
        .help(
            "Remote queue: \(status.pending) pending, \(status.conflicts) conflicts, \(status.merged) merged. Local verification does not reproduce the remote host."
        )
    }

    private func policyRow(_ status: RemoteIntegrationStatusResult) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 12) {
                metric("Policy", status.policy.enabled ? "enabled" : "disabled")
                metric(
                    "Verifier",
                    status.policy.requireVerifier ? "required" : "optional"
                )
                metric(
                    "Cap",
                    ByteCountFormatter.string(
                        fromByteCount: Int64(status.policy.maxBundleBytes),
                        countStyle: .file
                    )
                )
                metric("Refs", status.policy.keepImportRefs ? "retained" : "pruned")
                Spacer()
            }
            if !status.policy.allowedHosts.isEmpty {
                Text("allowed: " + status.policy.allowedHosts.joined(separator: ", "))
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
            }
            if status.retainedRefs > 0 {
                Text(
                    "\(status.retainedRefs) temporary ref(s) retained for retry or inspection"
                )
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
            }
        }
    }

    private func metric(_ title: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(title)
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
            Text(value)
                .font(.system(size: 9, weight: .medium, design: .monospaced))
        }
    }

    private func entryRow(_ entry: RemoteIntegrationQueueEntry) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 7) {
                Circle()
                    .fill(color(for: entry.status))
                    .frame(width: 5, height: 5)
                Text(entry.status.uppercased())
                    .font(.system(size: 8, weight: .semibold, design: .monospaced))
                    .foregroundStyle(color(for: entry.status))
                Text("\(entry.remoteHost ?? "?")/\(entry.remoteBranch ?? "?")")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                Spacer()
                Text(String(entry.commit.prefix(7)))
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
                if entry.attempt > 1 {
                    Text("attempt \(entry.attempt)")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
            }
            if let sha = entry.bundleSha256 {
                Text("bundle \(String(sha.prefix(16)))…")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
            if let imported = entry.importedRef {
                Text(imported)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            if let approved = entry.verifierApproved {
                Text(
                    "verifier \(approved ? "approved" : "rejected")"
                        + (entry.verifierSummary.map { ": \($0)" } ?? "")
                )
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(approved ? .green : .orange)
                .lineLimit(2)
            }
            if let detail = entry.detail, !detail.isEmpty {
                Text(detail)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(3)
            }
        }
        .padding(.vertical, 3)
    }

    private func color(for status: String) -> Color {
        switch status {
        case "merged": .green
        case "conflict": .orange
        case "failed", "interrupted": .red
        case "queued", "merging", "checking": .cyan
        default: .secondary
        }
    }
}
