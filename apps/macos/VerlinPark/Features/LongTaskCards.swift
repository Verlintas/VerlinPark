import SwiftUI

/// Shared status line for long-task operator actions. The cards themselves stay
/// focused on their own state; RPC receipts and errors surface once here.
struct LongTaskStatusLine: View {
    @Environment(AppModel.self) private var model
    let message: String

    var body: some View {
        HStack(alignment: .top, spacing: 9) {
            Image(systemName: "arrow.triangle.2.circlepath")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(VPTheme.accent)
            Text(message)
                .font(VPTheme.mono(9))
                .foregroundStyle(VPTheme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 6)
            VPCloseButton(size: 22, help: L10n.t("Dismiss")) {
                model.clearLongTaskStatus()
            }
        }
        .padding(.horizontal, 11)
        .padding(.vertical, 8)
        .background(VPTheme.surfaceSunken, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
    }
}

struct RecoveryCard: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VPSurfaceCard(padding: 14) {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        SectionLabel(title: "Stall recovery")
                        Text("Policy-gated recovery intents for this session")
                            .font(.system(size: 13, weight: .semibold, design: .rounded))
                    }
                    Spacer()
                    if model.longTaskBusy {
                        ProgressView().controlSize(.small)
                    }
                    Button("Refresh") {
                        Task { await model.refreshRecoveries() }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(model.longTaskBusy)
                }

                if model.selectedSession == nil {
                    emptyState("Select a session to inspect stall recovery.")
                } else if model.recoveries.isEmpty {
                    emptyState(
                        "No recovery rows. Stall recovery is opt-in through the project orchestration policy (escalate or retry); observe only records stall observations."
                    )
                } else {
                    ForEach(model.recoveries) { recovery in
                        VPInsetCard(padding: 9) {
                            HStack(alignment: .top, spacing: 8) {
                                StatusPill(status: recovery.status)
                                VStack(alignment: .leading, spacing: 3) {
                                    HStack(spacing: 6) {
                                        Text(recovery.displayAction.uppercased())
                                            .font(VPTheme.mono(8, weight: .semibold))
                                            .foregroundStyle(VPTheme.accent)
                                        Text(recovery.taskId)
                                            .font(VPTheme.mono(8.5, weight: .semibold))
                                        Text("· \(recovery.department)")
                                            .font(VPTheme.mono(8))
                                            .foregroundStyle(VPTheme.textTertiary)
                                        Spacer(minLength: 4)
                                        Text(
                                            L10n.format(
                                                "attempts %lld · stalls %lld",
                                                recovery.attempts,
                                                recovery.lastStallCount
                                            )
                                        )
                                        .font(VPTheme.mono(8))
                                        .foregroundStyle(VPTheme.textTertiary)
                                    }
                                    Text(
                                        L10n.format(
                                            "requested %@ · updated %@",
                                            VPTheme.relativeTime(recovery.requestedAt),
                                            VPTheme.relativeTime(recovery.updatedAt)
                                        )
                                    )
                                    .font(VPTheme.mono(8))
                                    .foregroundStyle(VPTheme.textSecondary)
                                    if let nextActionAt = recovery.nextActionAt {
                                        Text(
                                            L10n.format(
                                                "next action %@",
                                                VPRelativeTime.absolute(nextActionAt)
                                            )
                                        )
                                        .font(VPTheme.mono(8))
                                        .foregroundStyle(VPTheme.textSecondary)
                                    }
                                    if let error = recovery.lastError, !error.isEmpty {
                                        Text(error)
                                            .font(VPTheme.mono(8))
                                            .foregroundStyle(VPTheme.danger)
                                            .lineLimit(2)
                                    }
                                }
                            }
                        }
                    }
                }

                Text(
                    "Recovery rows are read-only in the app. The daemon advances or escalates them; there is no recovery cancel RPC."
                )
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
            }
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .vpAppear(index: 3)
        .task(id: model.selectedSessionID) {
            await model.refreshRecoveries()
        }
    }

    private func emptyState(_ message: String) -> some View {
        Text(L10n.t(message))
            .font(VPTheme.mono(9))
            .foregroundStyle(VPTheme.textSecondary)
            .fixedSize(horizontal: false, vertical: true)
    }
}

struct ContinuationCard: View {
    @Environment(AppModel.self) private var model
    @State private var showingDefer = false
    @State private var pendingCancel: SessionContinuation?

    var body: some View {
        VPSurfaceCard(padding: 14) {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        SectionLabel(title: "Deferred continuations")
                        Text("Prompts that wake after watched tasks settle or a deadline")
                            .font(.system(size: 13, weight: .semibold, design: .rounded))
                    }
                    Spacer()
                    if model.longTaskBusy {
                        ProgressView().controlSize(.small)
                    }
                    Button("Refresh") {
                        Task { await model.loadContinuations() }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(model.longTaskBusy)
                    Button("Defer Continuation…") {
                        showingDefer = true
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(model.selectedSession == nil)
                }

                if model.selectedSession == nil {
                    emptyState("Select a session to inspect deferred continuations.")
                } else if model.continuations.isEmpty {
                    emptyState(
                        "No deferred continuations. Create one to add a prompt later without holding a run open."
                    )
                } else {
                    ForEach(model.continuations) { continuation in
                        continuationRow(continuation)
                    }
                }

                Text(
                    "A continuation never adds a plan task or bypasses dependencies, approvals, budgets, verification, or write scopes."
                )
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
            }
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .vpAppear(index: 4)
        .task(id: model.selectedSessionID) {
            await model.loadContinuations()
        }
        .sheet(isPresented: $showingDefer) {
            if let session = model.selectedSession {
                DeferSheet(session: session)
                    .environment(model)
            }
        }
        .confirmationDialog(
            "Cancel this continuation?",
            isPresented: Binding(
                get: { pendingCancel != nil },
                set: {
                    if !$0 {
                        pendingCancel = nil
                    }
                }
            ),
            titleVisibility: .visible
        ) {
            Button("Cancel Continuation", role: .destructive) {
                if let continuation = pendingCancel {
                    Task { await model.cancelContinuation(id: continuation.id) }
                }
                pendingCancel = nil
            }
            Button("Keep", role: .cancel) {}
        } message: {
            Text(
                "The deferred prompt will not run. This does not affect task scheduling or plan dependencies."
            )
        }
    }

    private func continuationRow(_ continuation: SessionContinuation) -> some View {
        VPInsetCard(padding: 9) {
            HStack(alignment: .top, spacing: 8) {
                StatusPill(status: continuation.status)
                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 6) {
                        Text(continuation.displayStatus.uppercased())
                            .font(VPTheme.mono(8, weight: .semibold))
                            .foregroundStyle(VPTheme.statusColor(continuation.status))
                        Text(
                            L10n.format(
                                "watch %@ · %@",
                                L10n.t(continuation.watchMode),
                                continuation.watchTaskIds.isEmpty
                                    ? L10n.t("none")
                                    : continuation.watchTaskIds.joined(separator: ", ")
                            )
                        )
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textSecondary)
                        .lineLimit(1)
                        Spacer(minLength: 4)
                        Text(
                            L10n.format(
                                "attempts %lld/%lld",
                                continuation.attempts,
                                continuation.maxAttempts
                            )
                        )
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                        if continuation.isCancelable {
                            Button("Cancel") {
                                pendingCancel = continuation
                            }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                            .disabled(
                                model.continuationActionBusyID == continuation.id
                                    || model.continuationSubmitBusy
                            )
                            .help(
                                continuation.status == "delivering"
                                    ? L10n.t(
                                        "Delivery has started; the daemon may reject cancellation."
                                    )
                                    : L10n.t("Cancel this deferred continuation")
                            )
                        }
                    }
                    Text(continuation.body)
                        .font(VPTheme.font(10))
                        .foregroundStyle(VPTheme.textPrimary)
                        .lineLimit(2)
                    HStack(spacing: 8) {
                        if let deadlineAt = continuation.deadlineAt {
                            Text(
                                L10n.format(
                                    "deadline %@",
                                    VPRelativeTime.absolute(deadlineAt)
                                )
                            )
                        }
                        if let wakeReason = continuation.wakeReason {
                            Text(L10n.format("wake %@", L10n.t(wakeReason)))
                        }
                        Text(
                            L10n.format(
                                "created %@",
                                VPTheme.relativeTime(continuation.createdAt)
                            )
                        )
                        if let readyAt = continuation.readyAt {
                            Text(L10n.format("ready %@", VPTheme.relativeTime(readyAt)))
                        }
                        if let nextAttemptAt = continuation.nextAttemptAt {
                            Text(
                                L10n.format(
                                    "next attempt %@",
                                    VPRelativeTime.absolute(nextAttemptAt)
                                )
                            )
                        }
                    }
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
                    if !continuation.outcomes.isEmpty {
                        Text(
                            continuation.outcomes
                                .prefix(4)
                                .map { "\($0.taskId): \($0.status)" }
                                .joined(separator: " · ")
                        )
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textSecondary)
                        .lineLimit(1)
                    }
                    if let error = continuation.lastError, !error.isEmpty {
                        Text(error)
                            .font(VPTheme.mono(8))
                            .foregroundStyle(VPTheme.danger)
                            .lineLimit(2)
                    }
                }
            }
        }
    }

    private func emptyState(_ message: String) -> some View {
        Text(L10n.t(message))
            .font(VPTheme.mono(9))
            .foregroundStyle(VPTheme.textSecondary)
            .fixedSize(horizontal: false, vertical: true)
    }
}

struct SteeringCard: View {
    @Environment(AppModel.self) private var model
    @State private var showingSteer = false
    @State private var pendingCancel: SteeringMessage?

    var body: some View {
        VPSurfaceCard(padding: 14) {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        SectionLabel(title: "Steering")
                        Text("Operator instructions for the active run")
                            .font(.system(size: 13, weight: .semibold, design: .rounded))
                    }
                    Spacer()
                    if model.longTaskBusy {
                        ProgressView().controlSize(.small)
                    }
                    Button("Refresh") {
                        Task { await model.loadSteering() }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(model.longTaskBusy)
                    Button("Steer Run…") {
                        showingSteer = true
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(model.selectedSession == nil)
                }

                if model.selectedSession == nil {
                    emptyState("Select a session to inspect steering.")
                } else if model.steeringMessages.isEmpty {
                    emptyState(
                        "No steering messages. Delivery happens at the target run's next provider boundary."
                    )
                } else {
                    ForEach(model.steeringMessages) { steering in
                        VPInsetCard(padding: 9) {
                            HStack(alignment: .top, spacing: 8) {
                                StatusPill(status: steering.status)
                                VStack(alignment: .leading, spacing: 3) {
                                    HStack(spacing: 6) {
                                        Text(steering.displayStatus.uppercased())
                                            .font(VPTheme.mono(8, weight: .semibold))
                                            .foregroundStyle(VPTheme.statusColor(steering.status))
                                        Text(targetLabel(steering))
                                            .font(VPTheme.mono(8.5, weight: .semibold))
                                        if let taskID = steering.taskId {
                                            Text("· \(taskID)")
                                                .font(VPTheme.mono(8))
                                                .foregroundStyle(VPTheme.textSecondary)
                                        }
                                        Spacer(minLength: 4)
                                        Text(
                                            L10n.format(
                                                "created %@",
                                                VPTheme.relativeTime(steering.createdAt)
                                            )
                                        )
                                        .font(VPTheme.mono(8))
                                        .foregroundStyle(VPTheme.textTertiary)
                                        if steering.isPending {
                                            Button("Cancel") {
                                                pendingCancel = steering
                                            }
                                            .buttonStyle(.bordered)
                                            .controlSize(.small)
                                            .disabled(
                                                model.steeringCancelBusyID == steering.id
                                                    || model.steeringSubmitBusy
                                            )
                                            .help("Cancel this pending steering message")
                                        }
                                    }
                                    Text(steering.body)
                                        .font(VPTheme.font(10))
                                        .foregroundStyle(VPTheme.textPrimary)
                                        .lineLimit(3)
                                    if let deliveredAt = steering.deliveredAt {
                                        Text(
                                            L10n.format(
                                                "delivered %@",
                                                VPTheme.relativeTime(deliveredAt)
                                            )
                                        )
                                        .font(VPTheme.mono(8))
                                        .foregroundStyle(VPTheme.textTertiary)
                                    }
                                }
                            }
                        }
                    }
                }

                Text(
                    "Steering is cooperative. It is persisted and delivered immediately before the target agent's next provider request; it cannot interrupt an in-flight model call or tool execution."
                )
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
            }
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .vpAppear(index: 5)
        .task(id: model.selectedSessionID) {
            await model.loadSteering()
        }
        .sheet(isPresented: $showingSteer) {
            if let session = model.selectedSession {
                SteerSheet(session: session)
                    .environment(model)
            }
        }
        .confirmationDialog(
            "Cancel this steering message?",
            isPresented: Binding(
                get: { pendingCancel != nil },
                set: {
                    if !$0 {
                        pendingCancel = nil
                    }
                }
            ),
            titleVisibility: .visible
        ) {
            Button("Cancel Steering", role: .destructive) {
                if let steering = pendingCancel {
                    Task { await model.cancelSteering(id: steering.id) }
                }
                pendingCancel = nil
            }
            Button("Keep", role: .cancel) {}
        } message: {
            Text(
                "A pending steering message will not be delivered. This does not interrupt the running agent."
            )
        }
    }

    private func targetLabel(_ steering: SteeringMessage) -> String {
        if let agent = model.agents.first(where: { $0.id == steering.agentId }) {
            return agent.name
        }
        return String(steering.agentId.prefix(8))
    }

    private func emptyState(_ message: String) -> some View {
        Text(L10n.t(message))
            .font(VPTheme.mono(9))
            .foregroundStyle(VPTheme.textSecondary)
            .fixedSize(horizontal: false, vertical: true)
    }
}

struct TaskStallCard: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VPSurfaceCard(padding: 14) {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        SectionLabel(title: "Task progress & stalls")
                        Text("Observation-only progress telemetry and stall signals")
                            .font(.system(size: 13, weight: .semibold, design: .rounded))
                    }
                    Spacer()
                    if model.longTaskBusy {
                        ProgressView().controlSize(.small)
                    }
                    Button("Refresh") {
                        Task { await model.refreshStalls() }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(model.longTaskBusy)
                }

                if model.selectedSession == nil {
                    emptyState("Select a session to inspect task progress.")
                } else {
                    thresholdNote

                    VStack(alignment: .leading, spacing: 5) {
                        SectionLabel(title: "Progress")
                        if model.taskProgress.isEmpty {
                            emptyState("No progress observations yet.")
                        } else {
                            ForEach(model.taskProgress.prefix(8)) { progress in
                                progressRow(progress)
                            }
                        }
                    }

                    VStack(alignment: .leading, spacing: 5) {
                        SectionLabel(title: "Stall observations")
                        if model.taskStalls.isEmpty {
                            emptyState("No stall observations.")
                        } else {
                            ForEach(model.taskStalls.prefix(8)) { stall in
                                stallRow(stall)
                            }
                        }
                    }
                }

                Text(
                    "A stall is an observation, not a failure. The daemon never cancels or retries a task from this signal alone."
                )
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
            }
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .vpAppear(index: 6)
        .task(id: model.selectedSessionID) {
            await model.refreshStalls()
        }
    }

    private var thresholdNote: some View {
        Text(
            model.stallThresholdSecs == 0
                ? L10n.t("Stall sweep disabled.")
                : L10n.format(
                    "Stall sweep threshold: %@ seconds. Observation only unless the project orchestration policy opts into recovery.",
                    String(model.stallThresholdSecs)
                )
        )
        .font(VPTheme.mono(8))
        .foregroundStyle(
            model.stallThresholdSecs == 0 ? VPTheme.warning : VPTheme.textTertiary
        )
    }

    private func progressRow(_ progress: PlanTaskProgress) -> some View {
        VPInsetCard(padding: 8) {
            HStack(spacing: 8) {
                Text(progress.taskId)
                    .font(VPTheme.mono(8.5, weight: .semibold))
                Text(
                    progress.lastProgressAt.map {
                        L10n.format("last progress %@", VPTheme.relativeTime($0))
                    } ?? L10n.t("no progress recorded")
                )
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textSecondary)
                if let turn = progress.lastTurn {
                    Text(L10n.format("turn %lld", turn))
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                if let tool = progress.lastTool {
                    Text(L10n.format("tool %@", tool))
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                Spacer(minLength: 4)
                Text(L10n.format("stalls %lld", progress.stallCount))
                    .font(VPTheme.mono(8))
                    .foregroundStyle(
                        progress.stallCount > 0 ? VPTheme.warning : VPTheme.textTertiary
                    )
            }
        }
    }

    private func stallRow(_ stall: TaskStall) -> some View {
        VPInsetCard(padding: 8) {
            HStack(alignment: .top, spacing: 8) {
                StatusPill(status: stall.status)
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(stall.taskId)
                            .font(VPTheme.mono(8.5, weight: .semibold))
                        Text(stall.title)
                            .font(VPTheme.font(9))
                            .lineLimit(1)
                        Text("· \(stall.department)")
                            .font(VPTheme.mono(8))
                            .foregroundStyle(VPTheme.textTertiary)
                        Spacer(minLength: 4)
                        Text(L10n.format("age %@s", String(stall.ageSecs)))
                            .font(VPTheme.mono(8))
                            .foregroundStyle(VPTheme.warning)
                    }
                    HStack(spacing: 8) {
                        Text(
                            L10n.format(
                                "last progress %@",
                                VPTheme.relativeTime(stall.lastProgressAt)
                            )
                        )
                        Text(L10n.format("stalls %lld", stall.stallCount))
                        if let turn = stall.lastTurn {
                            Text(L10n.format("turn %lld", turn))
                        }
                        if let tool = stall.lastTool {
                            Text(L10n.format("tool %@", tool))
                        }
                        Text(
                            L10n.format(
                                "observed %@",
                                VPTheme.relativeTime(stall.observedAt)
                            )
                        )
                    }
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
                }
            }
        }
    }

    private func emptyState(_ message: String) -> some View {
        Text(L10n.t(message))
            .font(VPTheme.mono(9))
            .foregroundStyle(VPTheme.textSecondary)
            .fixedSize(horizontal: false, vertical: true)
    }
}
