import SwiftUI

/// Pure action-gating rules for the durable context checkpoint surface. The
/// daemon remains authoritative; these rules only disable an obviously unsafe
/// button and surface the preflight refusal returned by the RPC.
enum ContextCheckpointGate {
    static func restoreDisabledReason(
        sessionStatus: String,
        busy: Bool,
        detail: ContextCheckpointDetailResult?
    ) -> String? {
        if busy {
            return "Another checkpoint operation is running."
        }
        if ["running", "queued", "paused", "waiting_input"].contains(sessionStatus) {
            return "Restore is available when the session is idle, failed, or cancelled."
        }
        if sessionStatus == "imported" {
            return "Imported archives are read-only."
        }
        if let detail, detail.isActive {
            return "This boundary is already active."
        }
        if let detail, let error = detail.preflight.errors.first {
            return error
        }
        return nil
    }

    static func needsForce(_ detail: ContextCheckpointDetailResult?) -> Bool {
        detail?.preflight.warnings.isEmpty == false
    }

    static func tokenDeltaLabel(_ checkpoint: ContextCheckpoint) -> String {
        let delta = checkpoint.tokenDelta
        return delta > 0 ? "−\(tokenLabel(delta))" : "no change"
    }

    static func tokenLabel(_ value: Int) -> String {
        if value >= 1_000_000 {
            return String(format: "%.1fM", Double(value) / 1_000_000)
        }
        if value >= 1000 {
            return String(format: "%.1fk", Double(value) / 1000)
        }
        return "\(value)"
    }

    static func summaryPreview(_ checkpoint: ContextCheckpoint, limit: Int = 120) -> String {
        let text = (checkpoint.summary ?? checkpoint.label)
            .split(whereSeparator: \.isWhitespace)
            .joined(separator: " ")
        guard !text.isEmpty else { return checkpoint.label }
        if text.count <= limit {
            return text
        }
        return String(text.prefix(limit)) + "…"
    }
}

/// Shared list used by the Stage context popover and the Manage Dash context
/// card. It never performs a restore without a daemon preflight: the model
/// fetches detail, refuses on preflight errors, and records the warning when a
/// threshold advisory requires `force`.
struct ContextCheckpointSection: View {
    enum Style: Equatable {
        case popover
        case dashboard
    }

    @Environment(AppModel.self) private var model
    let style: Style

    private var sessionStatus: String {
        model.selectedSession?.status ?? "draft"
    }

    private var activeSession: Bool {
        ["running", "queued", "paused", "waiting_input"].contains(sessionStatus)
    }

    private var sorted: [ContextCheckpoint] {
        model.contextCheckpoints.reversed()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: style == .popover ? 8 : 10) {
            header
            if style == .dashboard {
                retentionRow
            }
            if sorted.isEmpty {
                Text("No durable context checkpoints yet. One is captured after each compaction.")
                    .font(style == .popover ? .caption2 : VPTheme.mono(8))
                    .foregroundStyle(style == .popover ? .secondary : VPTheme.textTertiary)
            } else {
                ForEach(sorted) { checkpoint in
                    row(checkpoint)
                }
            }
            if let notice = model.contextCheckpointNotice {
                Text(notice)
                    .font(style == .popover ? .caption2 : VPTheme.mono(8))
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
        }
    }

    private var header: some View {
        HStack(spacing: 8) {
            Text("CONTEXT CHECKPOINTS")
                .font(
                    style == .popover
                        ? .system(size: 9, weight: .semibold, design: .monospaced)
                        : VPTheme.mono(8, weight: .semibold)
                )
                .foregroundStyle(VPTheme.textTertiary)
            Text("\(sorted.count)")
                .font(style == .popover ? .caption2 : VPTheme.mono(8))
                .foregroundStyle(.secondary)
            Spacer()
            Button(model.contextCheckpointBusy ? "Capturing…" : "Capture") {
                Task { await model.createContextCheckpoint() }
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(activeSession || model.contextCheckpointBusy)
            .help("Capture the active summary boundary without running a prompt")
            if style == .dashboard, sorted.count > 1 {
                Button("Prune") {
                    Task { await model.pruneContextCheckpoints(keepLatest: 1) }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.contextCheckpointBusy)
                .help("Prune older checkpoints; the latest active checkpoint is protected")
            }
        }
    }

    @ViewBuilder
    private var retentionRow: some View {
        if let policy = model.contextCheckpointPolicy {
            HStack(spacing: 8) {
                Text("Retention")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                Text(
                    "sweep \(policy.enabled ? "on" : "off") · max \(policy.maxCheckpoints) · age \(policy.maxAgeDays)d"
                )
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textSecondary)
                Spacer()
                Button(policy.enabled ? "Disable sweep" : "Enable sweep") {
                    Task {
                        await model.setContextCheckpointRetention(enabled: !policy.enabled)
                    }
                }
                .buttonStyle(.borderless)
                .controlSize(.mini)
                .disabled(model.contextCheckpointBusy)
                .help("Project-scoped automatic checkpoint pruning")
                Menu("Max") {
                    ForEach([5, 10, 20, 50], id: \.self) { value in
                        Button("\(value) checkpoints") {
                            Task {
                                await model.setContextCheckpointRetention(maxCheckpoints: value)
                            }
                        }
                    }
                }
                .menuStyle(.borderlessButton)
                .controlSize(.mini)
                .fixedSize()
                .disabled(model.contextCheckpointBusy)
                Menu("Age") {
                    ForEach([7, 30, 90], id: \.self) { value in
                        Button("\(value) days") {
                            Task {
                                await model.setContextCheckpointRetention(maxAgeDays: value)
                            }
                        }
                    }
                    Button("No age limit") {
                        Task {
                            await model.setContextCheckpointRetention(maxAgeDays: 0)
                        }
                    }
                }
                .menuStyle(.borderlessButton)
                .controlSize(.mini)
                .fixedSize()
                .disabled(model.contextCheckpointBusy)
            }
        }
    }

    private func row(_ checkpoint: ContextCheckpoint) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Text(checkpoint.mode.uppercased())
                    .font(style == .popover ? .system(size: 8, weight: .semibold, design: .monospaced) :
                        VPTheme.mono(
                            7,
                            weight: .semibold
                        ))
                    .padding(.horizontal, 5)
                    .padding(.vertical, 2)
                    .foregroundStyle(VPTheme.accent)
                    .background(VPTheme.accent.opacity(0.12), in: Capsule())
                Text(checkpoint.label)
                    .font(style == .popover ? .system(size: 10, weight: .medium) : VPTheme.mono(
                        9,
                        weight: .semibold
                    ))
                    .lineLimit(1)
                Spacer()
                Text(ContextCheckpointGate.tokenDeltaLabel(checkpoint))
                    .font(style == .popover ? .system(size: 9, design: .monospaced) : VPTheme.mono(8))
                    .foregroundStyle(.secondary)
            }
            Text(ContextCheckpointGate.summaryPreview(checkpoint))
                .font(style == .popover ? .caption2 : VPTheme.mono(8))
                .foregroundStyle(.secondary)
                .lineLimit(style == .popover ? 2 : 1)
            if style == .dashboard {
                HStack(spacing: 10) {
                    Text(
                        "rev \(checkpoint.planRevision) · \(checkpoint.model) · \(Self.shortDate(checkpoint.createdAt))"
                    )
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(1)
                    Spacer()
                    actions(checkpoint)
                }
            } else {
                actions(checkpoint)
            }
        }
        .padding(style == .popover ? 0 : 8)
        .background {
            if style == .dashboard {
                RoundedRectangle(cornerRadius: 9, style: .continuous)
                    .fill(VPTheme.surfaceRaised.opacity(0.45))
            }
        }
        .task {
            if model.contextCheckpointDetails[checkpoint.id] == nil {
                _ = await model.loadContextCheckpointDetail(checkpoint.id)
            }
        }
    }

    private func actions(_ checkpoint: ContextCheckpoint) -> some View {
        HStack(spacing: 8) {
            let detail = model.contextCheckpointDetails[checkpoint.id]
            let disabledReason = ContextCheckpointGate.restoreDisabledReason(
                sessionStatus: sessionStatus,
                busy: model.contextCheckpointBusy,
                detail: detail
            )
            Button(ContextCheckpointGate.needsForce(detail) ? "Restore anyway" : "Restore") {
                Task { await model.restoreContextCheckpoint(checkpoint.id) }
            }
            .buttonStyle(.bordered)
            .controlSize(.mini)
            .disabled(disabledReason != nil)
            .help(disabledReason ??
                "Rewrite the active context boundary; messages and worktree stay untouched")
            Button {
                Task { await model.deleteContextCheckpoint(checkpoint.id) }
            } label: {
                Image(systemName: "trash")
                    .font(.system(size: 9, weight: .semibold))
            }
            .buttonStyle(.borderless)
            .controlSize(.mini)
            .disabled(model.contextCheckpointBusy)
            .help("Delete this checkpoint. Messages and worktree state are kept.")
        }
    }

    private static func shortDate(_ value: String) -> String {
        guard let date = ISO8601DateFormatter().date(from: value) else { return value }
        return date.formatted(date: .abbreviated, time: .shortened)
    }
}
