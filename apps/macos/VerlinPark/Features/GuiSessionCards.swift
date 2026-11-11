import SwiftUI

/// Live unified GUI surface card. The browser and native computer helper share
/// one daemon lease; this card shows the holder, bounded FIFO, helper warmth,
/// TCC status, and the most recent action metadata. It never shows screen
/// content or typed text.
struct GuiSessionCard: View {
    @Environment(AppModel.self) private var model

    private var status: GuiStatusResult? {
        model.guiStatus
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "GUI surface")
                    Text("Browser + native computer · one lease")
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.guiStatusBusy {
                    ProgressView().controlSize(.small)
                }
                Button {
                    Task { await model.loadGuiStatus() }
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .buttonStyle(VPGlassIconButtonStyle(size: 24))
                .help("Refresh GUI holder, queue, helper status, and timeline")
                .disabled(model.guiStatusBusy)
            }

            if let error = model.guiStatusError {
                Text(error)
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(VPTheme.warning)
            }

            if let status {
                holderRow(status)
                if !status.queue.isEmpty {
                    queueRows(status.queue)
                }
                helperRows(status)
                timelineRows
            } else {
                Text("No GUI status yet. The first action or a refresh starts the status cache.")
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.secondary)
            }

            Text(
                "One daemon-wide GUI lease. A second session waits up to the project wait budget, then receives GUI_BUSY with the holder and a retry hint. The lease is released after the grace window while helpers stay warm. This is not a distributed lock or a cross-host queue."
            )
            .font(.system(size: 9.5, design: .monospaced))
            .foregroundStyle(.tertiary)
        }
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .vpAppear(index: 1)
        .task {
            await model.loadGuiStatus()
            await model.loadGuiTimeline()
        }
    }

    @ViewBuilder
    private func holderRow(_ status: GuiStatusResult) -> some View {
        if let holder = status.holder {
            HStack(spacing: 10) {
                Image(systemName: holder.surface == "browser" ? "globe" : "macwindow")
                    .foregroundStyle(VPTheme.accent)
                VStack(alignment: .leading, spacing: 2) {
                    Text("\(holder.agentName) · \(holder.surface)")
                        .font(.system(size: 11, weight: .semibold, design: .rounded))
                    Text(
                        "session \(holder.sessionId.prefix(8)) · lease gen \(holder.leaseGeneration) · expires \(holder.expiresAt)"
                    )
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
                }
                Spacer()
                if holder.sessionId != model.selectedSessionID {
                    Button("Release") {
                        Task { await model.releaseGuiLease() }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.mini)
                    .help("Operator release of a stuck GUI lease")
                }
                if holder.sessionId == model.selectedSessionID {
                    Text("this session")
                        .font(.system(size: 9, weight: .semibold, design: .monospaced))
                        .padding(.horizontal, 7)
                        .padding(.vertical, 3)
                        .background(VPTheme.accent.opacity(0.14), in: Capsule())
                        .foregroundStyle(VPTheme.accent)
                }
            }
            .padding(9)
            .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 9))
        } else {
            HStack(spacing: 6) {
                Image(systemName: "lock.open")
                Text("GUI lease free · helpers may stay warm")
            }
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)
        }
    }

    private func queueRows(_ queue: [GuiQueueInfo]) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text("WAITING · FIFO")
                .font(.system(size: 8, weight: .bold, design: .monospaced))
                .foregroundStyle(.tertiary)
            ForEach(queue, id: \.sessionId) { waiter in
                HStack(spacing: 8) {
                    Text("#\(waiter.position)")
                        .font(.system(size: 9, weight: .bold, design: .monospaced))
                        .foregroundStyle(VPTheme.warning)
                    Text("\(waiter.agentName) · \(waiter.surface)")
                        .font(.system(size: 9.5, design: .monospaced))
                    Text("session \(waiter.sessionId.prefix(8))")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.secondary)
                    Spacer()
                    Text("\(waiter.waitingMs) ms")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
            }
        }
    }

    private func helperRows(_ status: GuiStatusResult) -> some View {
        HStack(spacing: 14) {
            helperChip(
                icon: "macwindow",
                label: "computer",
                value: status.computer?.helperAvailable == true ? "ready" : "missing",
                ok: status.computer?.helperAvailable == true
            )
            helperChip(
                icon: "hand.raised",
                label: "AX",
                value: status.computer?.accessibility == true ? "granted" : "not granted",
                ok: status.computer?.accessibility == true
            )
            helperChip(
                icon: "rectangle.dashed",
                label: "screen",
                value: status.computer?.screenRecording == true ? "granted" : "not granted",
                ok: status.computer?.screenRecording == true
            )
            helperChip(
                icon: "globe",
                label: "browser",
                value: status.browserHelperAvailable ? "ready" : "missing",
                ok: status.browserHelperAvailable
            )
            if status.computer?.cached == true, let age = status.computer?.cacheAgeMs {
                Text("status cached \(age) ms")
                    .font(.system(size: 8.5, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
            Spacer()
            Text("warm \(status.warmComputerHelpers)+\(status.warmBrowserHelpers)")
                .font(.system(size: 8.5, design: .monospaced))
                .foregroundStyle(.secondary)
        }
    }

    private func helperChip(icon: String, label: String, value: String, ok: Bool) -> some View {
        HStack(spacing: 4) {
            Image(systemName: icon)
                .font(.system(size: 8, weight: .semibold))
            Text("\(label) \(value)")
                .font(.system(size: 8.5, design: .monospaced))
        }
        .foregroundStyle(ok ? VPTheme.success : VPTheme.warning)
        .padding(.horizontal, 6)
        .padding(.vertical, 3)
        .background(
            (ok ? VPTheme.success : VPTheme.warning).opacity(0.09),
            in: Capsule()
        )
    }

    @ViewBuilder
    private var timelineRows: some View {
        if !model.guiTimeline.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                Text("RECENT ACTIONS · NO SCREEN CONTENT")
                    .font(.system(size: 8, weight: .bold, design: .monospaced))
                    .foregroundStyle(.tertiary)
                ForEach(Array(model.guiTimeline.suffix(8).reversed()), id: \.id) { action in
                    HStack(spacing: 8) {
                        Text(action.surface == "browser" ? "WEB" : "AX")
                            .font(.system(size: 8, weight: .bold, design: .monospaced))
                            .foregroundStyle(action.surface == "browser" ? VPTheme.accent : VPTheme
                                .textSecondary)
                            .frame(width: 26, alignment: .leading)
                        Text(action.action)
                            .font(.system(size: 9, weight: .semibold, design: .monospaced))
                        if let app = action.targetApp {
                            Text(app)
                                .font(.system(size: 9, design: .monospaced))
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                        }
                        if action.sensitiveTarget {
                            Text("sensitive")
                                .font(.system(size: 8, weight: .semibold, design: .monospaced))
                                .foregroundStyle(VPTheme.warning)
                        }
                        Spacer()
                        Text("gen \(action.snapshotGeneration)/lease \(action.leaseGeneration)")
                            .font(.system(size: 8.5, design: .monospaced))
                            .foregroundStyle(.tertiary)
                        Text("\(action.latencyMs) ms")
                            .font(.system(size: 8.5, design: .monospaced))
                            .foregroundStyle(.secondary)
                        Text(action.errorCode ?? action.result)
                            .font(.system(size: 8.5, weight: .semibold, design: .monospaced))
                            .foregroundStyle(action.errorCode == nil ? VPTheme.success : VPTheme.warning)
                    }
                }
            }
        }
    }
}

/// Compact Stage popover for the unified GUI surface. The full card lives in
/// Manage Dash → Team; this keeps the current holder and recent actions one
/// click from the transcript header.
struct GuiStagePopover: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("GUI SESSION")
                        .font(.system(size: 8, weight: .bold, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    Text(statusLine)
                        .font(.system(size: 12, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.guiStatusBusy {
                    ProgressView().controlSize(.small)
                }
                Button {
                    Task { await model.loadGuiStatus() }
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .buttonStyle(VPGlassIconButtonStyle(size: 22))
                .disabled(model.guiStatusBusy)
            }
            if let error = model.guiStatusError {
                Text(error)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(VPTheme.warning)
            }
            if let holder = model.guiStatus?.holder {
                HStack(spacing: 6) {
                    Text("holder")
                        .font(.system(size: 8, weight: .bold, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    Text("\(holder.agentName) · \(holder.surface)")
                        .font(.system(size: 9, design: .monospaced))
                    Text("gen \(holder.leaseGeneration)")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.secondary)
                    Spacer()
                    if holder.sessionId != model.selectedSessionID {
                        Button("Release") {
                            Task { await model.releaseGuiLease() }
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.mini)
                        .help("Operator release of a stuck GUI lease")
                    }
                }
            } else {
                Text("Lease free · helpers may stay warm")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
            if let queue = model.guiStatus?.queue, !queue.isEmpty {
                ForEach(queue, id: \.sessionId) { waiter in
                    HStack(spacing: 6) {
                        Text("#\(waiter.position)")
                            .font(.system(size: 8.5, weight: .bold, design: .monospaced))
                            .foregroundStyle(VPTheme.warning)
                        Text("\(waiter.agentName) · \(waiter.surface)")
                            .font(.system(size: 9, design: .monospaced))
                        Spacer()
                        Text("\(waiter.waitingMs) ms")
                            .font(.system(size: 8.5, design: .monospaced))
                            .foregroundStyle(.secondary)
                    }
                }
            }
            Divider().opacity(0.2)
            Text("RECENT · NO SCREEN CONTENT")
                .font(.system(size: 7.5, weight: .bold, design: .monospaced))
                .foregroundStyle(.tertiary)
            if model.guiTimeline.isEmpty {
                Text("No actions for this session")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            } else {
                ForEach(Array(model.guiTimeline.suffix(5).reversed()), id: \.id) { action in
                    HStack(spacing: 6) {
                        Text(action.action)
                            .font(.system(size: 8.5, weight: .semibold, design: .monospaced))
                        if let target = action.targetApp {
                            Text(target)
                                .font(.system(size: 8.5, design: .monospaced))
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                        }
                        Spacer()
                        Text("gen \(action.snapshotGeneration)")
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.tertiary)
                        Text("\(action.latencyMs) ms")
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.secondary)
                        Text(action.errorCode ?? action.result)
                            .font(.system(size: 8, weight: .semibold, design: .monospaced))
                            .foregroundStyle(action.errorCode == nil ? VPTheme.success : VPTheme.warning)
                    }
                }
            }
            Text("One daemon-wide lease. Cross-host coordination is not supported.")
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
        }
        .padding(12)
        .frame(width: 330)
        .task {
            await model.loadGuiStatus()
            await model.loadGuiTimeline()
        }
    }

    private var statusLine: String {
        if let holder = model.guiStatus?.holder {
            if holder.sessionId == model.selectedSessionID {
                return "This session holds \(holder.surface)"
            }
            return "Held by \(holder.agentName)"
        }
        if let latest = model.guiTimeline.last {
            return "Last action: \(latest.action)"
        }
        return "No GUI activity"
    }
}

/// Project GUI policy editor. Grace and wait are bounded by the daemon; the
/// sensitive list extends (never silently disables) the built-in System
/// Settings / SecurityAgent / password-manager protections.
struct GuiPolicyCard: View {
    @Environment(AppModel.self) private var model
    let project: Project

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "GUI session policy")
                    Text(project.name)
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.guiPolicyBusy {
                    ProgressView().controlSize(.small)
                }
                Picker("", selection: $model.guiSensitiveMode) {
                    Text("Deny").tag(GuiSensitiveMode.deny)
                    Text("Require approval").tag(GuiSensitiveMode.requireApproval)
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(width: 220)
            }

            Text(
                "Sensitive native targets (System Settings, SecurityAgent, Keychain, 1Password, Bitwarden, LastPass, Dashlane, and additions below) are never silently automated. Deny blocks them; Require approval relies on the existing HostState/Destructive approval decision and still needs an explicit bundle or window target. Read capture of a sensitive target is denied unless the call names that target."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)

            HStack(spacing: 14) {
                policyField("Grace (s)", value: $model.guiGraceSecs, range: 0 ... 600)
                policyField("Wait (ms)", value: $model.guiWaitMs, range: 0 ... 60000)
                policyField("Status TTL (s)", value: $model.guiStatusTtlSecs, range: 5 ... 600)
                Spacer()
            }

            HStack(alignment: .top, spacing: 12) {
                sensitiveEditor(
                    title: "Extra sensitive bundle ids · one per line",
                    text: $model.guiSensitiveBundlesText
                )
                sensitiveEditor(
                    title: "Extra sensitive app/window patterns · one per line",
                    text: $model.guiSensitiveTitlesText
                )
            }

            HStack(spacing: 8) {
                Text("Built-in protections cannot be removed here; additions only extend them.")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
                Spacer()
                Button("Reset") {
                    Task { await model.resetGuiPolicy() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.guiPolicyBusy)
                Button("Save policy") {
                    Task { await model.saveGuiPolicy() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.guiPolicyBusy)
            }

            if let status = model.guiPolicyStatus {
                Text(status)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
        }
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .vpAppear(index: 0)
        .task(id: project.id) {
            await model.loadGuiPolicy()
        }
    }

    private func policyField(
        _ label: String,
        value: Binding<Int>,
        range: ClosedRange<Int>
    ) -> some View {
        HStack(spacing: 6) {
            Text(label)
                .font(.system(size: 9, design: .monospaced))
                .foregroundStyle(.secondary)
            Stepper(
                "\(value.wrappedValue)",
                value: value,
                in: range,
                step: range.upperBound > 1000 ? 500 : 1
            )
            .font(.system(size: 9, design: .monospaced))
        }
    }

    private func sensitiveEditor(title: String, text: Binding<String>) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title)
                .font(.system(size: 8.5, design: .monospaced))
                .foregroundStyle(.tertiary)
            TextEditor(text: text)
                .font(.system(size: 9.5, design: .monospaced))
                .scrollContentBackground(.hidden)
                .padding(5)
                .frame(height: 62)
                .background(.black.opacity(0.16), in: RoundedRectangle(cornerRadius: 7))
                .overlay {
                    RoundedRectangle(cornerRadius: 7)
                        .strokeBorder(.white.opacity(0.08), lineWidth: 1)
                }
        }
    }
}
