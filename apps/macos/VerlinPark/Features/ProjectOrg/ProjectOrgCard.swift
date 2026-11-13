import SwiftUI

/// Manage Dash project org section. The card is a bounded read model plus
/// explicit operator actions; all state lives in AppModel and every mutation
/// goes through a real S6 RPC.
struct ProjectOrgCard: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model

        VStack(alignment: .leading, spacing: 14) {
            header
            if let error = model.projectOrgError {
                ProjectOrgErrorBanner(error: error) {
                    Task { await model.loadProjectOrg() }
                }
            }
            if model.selectedProjectID == nil {
                ProjectOrgEmptyState(
                    icon: "person.3.sequence",
                    title: "Select a project",
                    detail: "The project org is scoped to one durable roster and never mixes projects."
                )
            } else if model.projectOrgSnapshot == nil {
                if model.projectOrgBusy {
                    ProjectOrgLoadingState()
                } else {
                    ProjectOrgEmptyState(
                        icon: "person.3.sequence",
                        title: "No org snapshot loaded",
                        detail: "Refresh to read the bounded Leader → Manager → Worker projection."
                    )
                }
            } else if let snapshot = model.projectOrgSnapshot {
                snapshotBody(snapshot)
            }
            actionNotice
        }
        .padding(16)
        .vpSurface(radius: 16)
        .vpHoverLift(scale: 1.002, lift: 1)
        .vpAppear(index: 0)
        .sheet(item: $model.projectOrgSheet) { sheet in
            ProjectOrgActionSheet(sheet: sheet)
                .environment(model)
        }
        .sheet(item: $model.projectOrgDetailSelection) { _ in
            ProjectOrgDetailSheet()
                .environment(model)
        }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 10) {
            VStack(alignment: .leading, spacing: 3) {
                SectionLabel(title: "Project org")
                Text(model.selectedProject?.name ?? "No project selected")
                    .font(.system(size: 14, weight: .semibold, design: .rounded))
                    .foregroundStyle(VPTheme.textPrimary)
                    .lineLimit(1)
                if let status = model.projectOrgStatus {
                    Text(L10n.t(status))
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                        .lineLimit(1)
                }
            }
            Spacer()
            if model.projectOrgBusy {
                ProgressView()
                    .controlSize(.small)
            }
            if let snapshot = model.projectOrgSnapshot {
                Button {
                    Task { await model.loadProjectOrg(includeRetired: !model.projectOrgIncludeRetired) }
                } label: {
                    Label(
                        model.projectOrgIncludeRetired ? "Retired shown" : "Retired hidden",
                        systemImage: model.projectOrgIncludeRetired ? "eye" : "eye.slash"
                    )
                    .font(VPTheme.mono(8))
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .help(
                    snapshot.counts.retired == 0
                        ? "No retired identities are recorded; retired evidence stays preserved."
                        : "Toggle retired evidence in the chart."
                )
                .accessibilityLabel(
                    model.projectOrgIncludeRetired
                        ? "Hide retired identities"
                        : "Show retired identities"
                )
                Button {
                    model.presentProjectOrgHire()
                } label: {
                    Label("Hire", systemImage: "person.badge.plus")
                        .font(VPTheme.mono(8))
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(!snapshot.enabled)
                .help(
                    snapshot.enabled
                        ? "Create an explicit approval-aware Manager/Worker hire request."
                        : "Enable the project identity policy before hiring."
                )
            }
            Button {
                Task { await model.loadProjectOrg() }
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
                    .font(VPTheme.mono(8))
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(model.selectedProjectID == nil || model.projectOrgBusy)
            .keyboardShortcut("r", modifiers: [.command, .shift])
            .help("Refresh the bounded org projection · ⇧⌘R")
        }
    }

    @ViewBuilder
    private var actionNotice: some View {
        if let notice = model.projectOrgActionNotice {
            HStack(spacing: 6) {
                Image(systemName: "checkmark.circle")
                    .foregroundStyle(VPTheme.success)
                Text(L10n.t(notice))
                    .font(VPTheme.mono(8.5))
                    .foregroundStyle(VPTheme.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 4)
                Button("Dismiss") {
                    model.projectOrgActionNotice = nil
                }
                .buttonStyle(.plain)
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
            }
            .padding(8)
            .background(VPTheme.success.opacity(0.06), in: RoundedRectangle(cornerRadius: 9))
            .overlay {
                RoundedRectangle(cornerRadius: 9)
                    .strokeBorder(VPTheme.success.opacity(0.18), lineWidth: 1)
            }
        }
        if let error = model.projectOrgActionError {
            ProjectOrgErrorBanner(error: error) {
                model.projectOrgActionError = nil
            }
        }
    }

    @ViewBuilder
    private func snapshotBody(_ snapshot: AgentOrgSnapshot) -> some View {
        ForEach(snapshot.warnings, id: \.self) { warning in
            ProjectOrgWarningBanner(message: warning)
        }
        if !snapshot.enabled {
            ProjectOrgNoticeBanner(
                icon: "shield.lefthalf.filled",
                color: VPTheme.warning,
                title: "Identity policy is disabled",
                detail: "Existing durable rows remain visible, but hiring and assignment gates stay closed until the project enables the policy."
            )
        }
        if snapshot.isEmpty {
            ProjectOrgEmptyState(
                icon: "person.crop.circle.badge.questionmark",
                title: "No identities yet",
                detail: "Bootstrap exactly one Leader with `vp agent-create <project> --role leader`, then hire Managers and Workers from this console."
            )
        }
        statsStrip(snapshot)
        if let leader = snapshot.leader {
            leaderChart(leader)
        } else {
            ProjectOrgNoticeBanner(
                icon: "crown",
                color: VPTheme.warning,
                title: "No live Leader",
                detail: "Managers and Workers without a valid parent remain visible below instead of disappearing."
            )
        }
        if !snapshot.unassigned.isEmpty {
            unassignedSection(snapshot.unassigned)
        }
        if !snapshot.departments.isEmpty {
            departmentStrip(snapshot.departments)
        }
        if !snapshot.pendingHireRequests.isEmpty || !snapshot.pendingRetirementProposals.isEmpty {
            pendingActions(snapshot)
        }
        if !snapshot.activeAssignments.isEmpty {
            crossSessionRoster(snapshot)
        }
        if !snapshot.workspaceSummaries.isEmpty {
            workspaceStrip(snapshot)
        }
    }

    private func statsStrip(_ snapshot: AgentOrgSnapshot) -> some View {
        let counts = snapshot.counts
        return ViewThatFits(in: .horizontal) {
            HStack(spacing: 10) {
                stat("Identities", "\(counts.identities)", icon: "person.3")
                stat("Live", "\(counts.liveIdentities)", icon: "waveform.path.ecg")
                stat("Assignments", "\(counts.activeAssignments)", icon: "link")
                stat("Workspaces", "\(counts.activeWorkspaces)", icon: "shippingbox")
                stat("Unread", "\(counts.unreadInbox)", icon: "tray.full")
                stat("Memory", "\(counts.memoryCount)", icon: "brain")
                stat("Pending", "\(counts.pendingOrgActions)", icon: "clock.badge.exclamationmark")
            }
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 10) {
                    stat("Identities", "\(counts.identities)", icon: "person.3")
                    stat("Live", "\(counts.liveIdentities)", icon: "waveform.path.ecg")
                    stat("Assignments", "\(counts.activeAssignments)", icon: "link")
                    stat("Workspaces", "\(counts.activeWorkspaces)", icon: "shippingbox")
                    stat("Unread", "\(counts.unreadInbox)", icon: "tray.full")
                    stat("Memory", "\(counts.memoryCount)", icon: "brain")
                    stat("Pending", "\(counts.pendingOrgActions)", icon: "clock.badge.exclamationmark")
                }
            }
        }
    }

    private func stat(_ label: String, _ value: String, icon: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Label(L10n.t(label).uppercased(), systemImage: icon)
                .font(VPTheme.mono(7, weight: .semibold))
                .foregroundStyle(VPTheme.textTertiary)
                .labelStyle(.titleAndIcon)
            Text(value)
                .font(VPTheme.mono(13, weight: .semibold))
                .foregroundStyle(VPTheme.textPrimary)
                .contentTransition(.numericText())
        }
        .padding(.horizontal, 9)
        .padding(.vertical, 7)
        .frame(minWidth: 72, alignment: .leading)
        .background(VPTheme.surfaceSunken.opacity(0.55), in: RoundedRectangle(cornerRadius: 9))
        .overlay {
            RoundedRectangle(cornerRadius: 9)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(Text(L10n.t(label)))
        .accessibilityValue(Text(value))
    }

    private func leaderChart(_ leader: AgentOrgNode) -> some View {
        VStack(spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("LEADER")
                        .font(VPTheme.mono(7.5, weight: .semibold))
                        .tracking(1.0)
                        .foregroundStyle(VPTheme.accent)
                    Text("Project owner · plan authority")
                        .font(VPTheme.mono(7.5))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                Spacer()
                Text("\(leader.reports.count) manager\(leader.reports.count == 1 ? "" : "s")")
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            ProjectOrgNodeCard(node: leader, compact: false)
                .frame(maxWidth: 520)
            if leader.reports.isEmpty {
                Text("No Managers report to the Leader yet.")
                    .font(VPTheme.mono(8.5))
                    .foregroundStyle(VPTheme.textTertiary)
                    .padding(.vertical, 4)
            } else {
                ScrollView(.horizontal, showsIndicators: true) {
                    HStack(alignment: .top, spacing: 12) {
                        ForEach(leader.reports) { manager in
                            ProjectOrgDepartmentColumn(manager: manager)
                        }
                    }
                    .padding(.horizontal, 2)
                    .padding(.vertical, 4)
                }
            }
        }
        .padding(12)
        .background(VPTheme.surfaceSunken.opacity(0.42), in: RoundedRectangle(cornerRadius: 13))
        .overlay {
            RoundedRectangle(cornerRadius: 13)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
    }

    private func unassignedSection(_ rows: [AgentOrgNode]) -> some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack {
                Text("UNASSIGNED")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .tracking(1.0)
                    .foregroundStyle(VPTheme.warning)
                Text("No parent in the active hierarchy")
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
                Spacer()
                Text("\(rows.count)")
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            ForEach(rows) { node in
                ProjectOrgNodeCard(node: node, compact: true)
            }
        }
        .padding(10)
        .background(VPTheme.warning.opacity(0.04), in: RoundedRectangle(cornerRadius: 11))
        .overlay {
            RoundedRectangle(cornerRadius: 11)
                .strokeBorder(VPTheme.warning.opacity(0.15), lineWidth: 1)
        }
    }

    private func departmentStrip(_ departments: [AgentOrgDepartment]) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("DEPARTMENTS")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .tracking(1.0)
                    .foregroundStyle(VPTheme.textTertiary)
                Spacer()
                Text("\(departments.count)")
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    ForEach(departments) { department in
                        VStack(alignment: .leading, spacing: 3) {
                            Text(department.name)
                                .font(VPTheme.mono(9, weight: .semibold))
                                .foregroundStyle(VPTheme.textPrimary)
                                .lineLimit(1)
                            Text(
                                "\(department.managerCount)M · \(department.workerCount)W · \(department.activeWorkers) active worker\(department.activeWorkers == 1 ? "" : "s")"
                            )
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.textTertiary)
                            if department.unreadInbox > 0 {
                                Text("\(department.unreadInbox) unread")
                                    .font(VPTheme.mono(7))
                                    .foregroundStyle(VPTheme.warning)
                            }
                        }
                        .padding(.horizontal, 9)
                        .padding(.vertical, 7)
                        .frame(minWidth: 126, alignment: .leading)
                        .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 8))
                        .overlay {
                            RoundedRectangle(cornerRadius: 8)
                                .strokeBorder(VPTheme.hairline, lineWidth: 1)
                        }
                        .accessibilityElement(children: .combine)
                        .accessibilityLabel(Text("Department \(department.name)"))
                    }
                }
            }
        }
    }

    private func pendingActions(_ snapshot: AgentOrgSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("PENDING OPERATOR ACTIONS")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .tracking(1.0)
                    .foregroundStyle(VPTheme.warning)
                Spacer()
                Text("\(snapshot.counts.pendingOrgActions)")
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            ForEach(snapshot.pendingHireRequests) { request in
                HStack(spacing: 8) {
                    Image(systemName: "person.badge.plus")
                        .font(.system(size: 10))
                        .foregroundStyle(VPTheme.accent)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(
                            "Hire \(request.role) · \(request.title ?? request.component ?? request.identityId)"
                        )
                        .font(VPTheme.mono(8.5, weight: .medium))
                        .foregroundStyle(VPTheme.textPrimary)
                        .lineLimit(1)
                        Text(
                            "requested \(request.requestedAt) · risk \(request.approvalRisk)\(request.origin == "imported" ? " · imported/read-only" : "")"
                        )
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                        .lineLimit(1)
                    }
                    Spacer()
                    Button("Approve") {
                        Task { await model.approveProjectOrgHire(requestID: request.id) }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(model.projectOrgActionBusy || request.origin == "imported")
                    Button("Reject") {
                        Task { await model.rejectProjectOrgHire(requestID: request.id) }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(model.projectOrgActionBusy || request.origin == "imported")
                }
                .padding(8)
                .background(VPTheme.accent.opacity(0.04), in: RoundedRectangle(cornerRadius: 8))
            }
            ForEach(snapshot.pendingRetirementProposals) { proposal in
                HStack(spacing: 8) {
                    Image(systemName: "exclamationmark.triangle")
                        .font(.system(size: 10))
                        .foregroundStyle(VPTheme.warning)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(
                            "Retirement proposal · \(snapshot.node(identityID: proposal.identityId)?.displayTitle ?? proposal.identityId)"
                        )
                        .font(VPTheme.mono(8.5, weight: .medium))
                        .foregroundStyle(VPTheme.textPrimary)
                        .lineLimit(1)
                        Text("\(proposal.reasonCode) · proposed \(proposal.proposedAt) · advisory evidence")
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.textTertiary)
                            .lineLimit(1)
                    }
                    Spacer()
                    Button("Approve") {
                        Task { await model.approveProjectOrgRetirement(proposalID: proposal.id) }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(model.projectOrgActionBusy || proposal.origin == "imported")
                    Button("Reject") {
                        Task { await model.rejectProjectOrgRetirement(proposalID: proposal.id) }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(model.projectOrgActionBusy || proposal.origin == "imported")
                    Button("Retire…") {
                        model.presentProjectOrgRetire(identityID: proposal.identityId)
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(model.projectOrgActionBusy || proposal.origin == "imported")
                }
                .padding(8)
                .background(VPTheme.warning.opacity(0.04), in: RoundedRectangle(cornerRadius: 8))
            }
        }
    }

    private func crossSessionRoster(_ snapshot: AgentOrgSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("CROSS-SESSION ROSTER")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .tracking(1.0)
                    .foregroundStyle(VPTheme.textTertiary)
                Spacer()
                Text("\(snapshot.activeAssignments.count) live")
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            ForEach(snapshot.activeAssignments.prefix(8)) { assignment in
                HStack(spacing: 8) {
                    VPStatusDot(
                        color: assignment.leaseActive ? VPTheme.success : VPTheme.warning,
                        pulsing: assignment.leaseActive,
                        size: 5
                    )
                    Text(snapshot.node(identityID: assignment.identityId)?.displayTitle ?? assignment
                        .identityId)
                        .font(VPTheme.mono(8.5, weight: .medium))
                        .foregroundStyle(VPTheme.textPrimary)
                        .lineLimit(1)
                    Text(assignment.role)
                        .font(VPTheme.mono(7.5))
                        .foregroundStyle(VPTheme.textTertiary)
                    Spacer()
                    Text(sessionLabel(assignment.sessionId))
                        .font(VPTheme.mono(7.5))
                        .foregroundStyle(VPTheme.textTertiary)
                        .lineLimit(1)
                    Text(assignment.leaseActive ? "lease active" : "lease expired")
                        .font(VPTheme.mono(7))
                        .foregroundStyle(assignment.leaseActive ? VPTheme.success : VPTheme.warning)
                }
                .padding(.vertical, 2)
            }
        }
    }

    private func workspaceStrip(_ snapshot: AgentOrgSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack {
                Text("WORKSPACES")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .tracking(1.0)
                    .foregroundStyle(VPTheme.textTertiary)
                Spacer()
                Text("\(snapshot.workspaceSummaries.count)")
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 7) {
                    ForEach(snapshot.workspaceSummaries.prefix(10)) { workspace in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(workspace.component)
                                .font(VPTheme.mono(8, weight: .semibold))
                                .foregroundStyle(VPTheme.textPrimary)
                            Text(workspace.branch)
                                .font(VPTheme.mono(6.5))
                                .foregroundStyle(VPTheme.textTertiary)
                                .lineLimit(1)
                                .truncationMode(.middle)
                            HStack(spacing: 4) {
                                VPStatusDot(
                                    color: workspace.leaseActive ? VPTheme.success : VPTheme.textTertiary,
                                    size: 4
                                )
                                Text(workspace.status)
                                    .font(VPTheme.mono(6.5))
                                    .foregroundStyle(VPTheme.textTertiary)
                            }
                        }
                        .padding(7)
                        .frame(minWidth: 120, alignment: .leading)
                        .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 8))
                        .overlay {
                            RoundedRectangle(cornerRadius: 8)
                                .strokeBorder(VPTheme.hairline, lineWidth: 1)
                        }
                        .help("Workspace branch and status. Absolute worktree paths are not shown.")
                    }
                }
            }
        }
    }

    private func sessionLabel(_ sessionID: String) -> String {
        if let session = model.allSessions.first(where: { $0.id == sessionID }) {
            return "\(session.goal) · \(String(sessionID.prefix(8)))"
        }
        return String(sessionID.prefix(12))
    }
}

private struct ProjectOrgDepartmentColumn: View {
    let manager: AgentOrgNode

    var body: some View {
        VStack(spacing: 8) {
            ProjectOrgNodeCard(node: manager, compact: true)
                .frame(width: 252)
            if manager.reports.isEmpty {
                Text("No component Workers")
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
                    .padding(.vertical, 4)
            } else {
                ForEach(manager.reports) { worker in
                    ProjectOrgNodeCard(node: worker, compact: true)
                        .frame(width: 252)
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("Department manager \(manager.displayTitle)"))
    }
}

/// One identity card in the org chart. It carries status, generation, active
/// session, workspace, inbox/memory, performance, and advisory retirement
/// badges; no prompt or tool content is represented.
struct ProjectOrgNodeCard: View {
    @Environment(AppModel.self) private var model
    let node: AgentOrgNode
    let compact: Bool

    private var identity: AgentIdentity {
        node.identity
    }

    var body: some View {
        VStack(alignment: .leading, spacing: compact ? 6 : 8) {
            HStack(spacing: 7) {
                Image(systemName: VPTheme.roleIcon(identity.role))
                    .font(.system(size: compact ? 10 : 12, weight: .semibold))
                    .foregroundStyle(VPTheme.roleColor(identity.role))
                    .frame(width: 16)
                VStack(alignment: .leading, spacing: 1) {
                    Text(identity.displayName)
                        .font(VPTheme.mono(compact ? 9 : 10.5, weight: .semibold))
                        .foregroundStyle(VPTheme.textPrimary)
                        .lineLimit(1)
                    Text(subtitle)
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                        .lineLimit(1)
                }
                Spacer(minLength: 4)
                VPStatusDot(color: VPTheme.statusColor(identity.status), size: 5)
                Text(node.statusLabel)
                    .font(VPTheme.mono(7, weight: .medium))
                    .foregroundStyle(VPTheme.statusColor(identity.status))
            }

            HStack(spacing: 5) {
                badge("gen \(identity.generation)", color: VPTheme.textSecondary)
                if identity.isImported {
                    badge("imported", color: VPTheme.warning)
                }
                if node.isLiveAssignment, let assignment = node.activeAssignment {
                    badge("session \(String(assignment.sessionId.prefix(8)))", color: VPTheme.success)
                }
                if let workspace = node.workspace {
                    badge(
                        workspace.status == "active" ? "ws \(workspace.component)" : "ws \(workspace.status)",
                        color: workspace.leaseActive ? VPTheme.accent : VPTheme.textTertiary
                    )
                }
            }

            if !compact {
                HStack(spacing: 10) {
                    miniMetric("inbox", "\(node.unreadInbox)", icon: "tray.full")
                    miniMetric("memory", "\(node.memoryCount)", icon: "brain")
                    miniMetric(
                        "tasks",
                        node.performance.hasEvidence ? node.performance.taskSummary : "—",
                        icon: "checklist"
                    )
                    miniMetric(
                        "verifier",
                        node.performance.hasEvidence
                            ? "\(node.performance.verifierApprovals)/\(node.performance.verifierRejections)"
                            : "—",
                        icon: "checkmark.shield"
                    )
                }
            }

            if node.retirement.candidate || node.retirement.hasPendingProposal {
                HStack(spacing: 5) {
                    Image(systemName: "exclamationmark.triangle")
                        .font(.system(size: 8))
                    Text(
                        node.retirement.hasPendingProposal
                            ? "retirement pending"
                            : "advisory retirement candidate"
                    )
                    .font(VPTheme.mono(7))
                    if node.retirement.candidate {
                        Text("severity \(node.retirement.severity)")
                            .font(VPTheme.mono(6.5))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                }
                .foregroundStyle(VPTheme.warning)
            }

            HStack(spacing: 6) {
                Button {
                    Task { await model.openProjectOrgDetail(identityID: identity.id) }
                } label: {
                    Label("Detail", systemImage: "rectangle.stack.person.crop")
                        .font(VPTheme.mono(7.5))
                }
                .buttonStyle(.bordered)
                .controlSize(.mini)
                .help(
                    "Open assignments, inbox metadata, memory, performance, workspace, and lifecycle evidence."
                )

                Menu {
                    Button("View detail") {
                        Task { await model.openProjectOrgDetail(identityID: identity.id) }
                    }
                    if identity.role == "worker" || identity.role == "manager" {
                        Button("Promote…") {
                            model.presentProjectOrgPromote(identityID: identity.id)
                        }
                        .disabled(identity.isImported || identity.status == "retired")
                    }
                    if identity.role != "leader" {
                        Button("Transfer…") {
                            model.presentProjectOrgTransfer(identityID: identity.id)
                        }
                        .disabled(identity.isImported || identity.status == "retired")
                    }
                    if identity.status != "retired" {
                        Button("Retire…", role: .destructive) {
                            model.presentProjectOrgRetire(identityID: identity.id)
                        }
                        .disabled(identity.isImported)
                    }
                } label: {
                    Image(systemName: "ellipsis.circle")
                        .font(.system(size: 10))
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .frame(width: 20)
                .help("Identity actions")
                Spacer()
                if node.reportCount > 0 {
                    Text("\(node.reportCount) report\(node.reportCount == 1 ? "" : "s")")
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
        }
        .padding(compact ? 9 : 11)
        .background(
            identity.role == "leader"
                ? VPTheme.accent.opacity(0.06)
                : VPTheme.surface,
            in: RoundedRectangle(cornerRadius: 10)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 10)
                .strokeBorder(
                    identity.role == "leader"
                        ? VPTheme.accent.opacity(0.24)
                        : VPTheme.hairline,
                    lineWidth: 1
                )
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text(accessibilityLabel))
    }

    private var subtitle: String {
        var parts = [identity.role]
        if let department = identity.department, !department.isEmpty {
            parts.append(department)
        }
        if let component = identity.component, !component.isEmpty {
            parts.append(component)
        }
        if let model = identity.model, !model.isEmpty {
            parts.append(model)
        }
        return parts.joined(separator: " · ")
    }

    private var accessibilityLabel: String {
        var parts = [
            identity.displayName,
            identity.role,
            identity.status,
            "generation \(identity.generation)",
        ]
        if identity.isImported {
            parts.append("imported read-only")
        }
        if let assignment = node.activeAssignment, assignment.leaseActive {
            parts.append("active session \(assignment.sessionId)")
        }
        if node.unreadInbox > 0 {
            parts.append("\(node.unreadInbox) unread inbox")
        }
        if node.memoryCount > 0 {
            parts.append("\(node.memoryCount) memories")
        }
        if node.retirement.candidate {
            parts.append("advisory retirement candidate")
        }
        return parts.joined(separator: ", ")
    }

    private func badge(_ label: String, color: Color) -> some View {
        Text(label)
            .font(VPTheme.mono(6.5, weight: .medium))
            .foregroundStyle(color)
            .padding(.horizontal, 5)
            .padding(.vertical, 2)
            .background(color.opacity(0.08), in: Capsule())
            .overlay {
                Capsule().strokeBorder(color.opacity(0.18), lineWidth: 0.6)
            }
    }

    private func miniMetric(_ label: String, _ value: String, icon: String) -> some View {
        HStack(spacing: 3) {
            Image(systemName: icon)
                .font(.system(size: 7))
                .foregroundStyle(VPTheme.textTertiary)
            Text("\(label) \(value)")
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textSecondary)
        }
    }
}

struct ProjectOrgErrorBanner: View {
    let error: ProjectOrgErrorSurface
    let retry: (() -> Void)?

    var body: some View {
        HStack(alignment: .top, spacing: 7) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 10))
                .foregroundStyle(VPTheme.danger)
            VStack(alignment: .leading, spacing: 2) {
                Text(L10n.t(error.displayText))
                    .font(VPTheme.mono(8.5))
                    .foregroundStyle(VPTheme.danger)
                    .fixedSize(horizontal: false, vertical: true)
                if let code = error.code {
                    Text(code)
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
            Spacer(minLength: 4)
            if let retry {
                Button("Retry", action: retry)
                    .buttonStyle(.bordered)
                    .controlSize(.mini)
            }
        }
        .padding(8)
        .background(VPTheme.danger.opacity(0.06), in: RoundedRectangle(cornerRadius: 9))
        .overlay {
            RoundedRectangle(cornerRadius: 9)
                .strokeBorder(VPTheme.danger.opacity(0.20), lineWidth: 1)
        }
        .accessibilityElement(children: .combine)
    }
}

struct ProjectOrgWarningBanner: View {
    let message: String

    var body: some View {
        HStack(alignment: .top, spacing: 6) {
            Image(systemName: "info.circle")
                .font(.system(size: 9))
                .foregroundStyle(VPTheme.warning)
            Text(L10n.t(message))
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .background(VPTheme.warning.opacity(0.045), in: RoundedRectangle(cornerRadius: 8))
        .overlay {
            RoundedRectangle(cornerRadius: 8)
                .strokeBorder(VPTheme.warning.opacity(0.14), lineWidth: 1)
        }
    }
}

struct ProjectOrgNoticeBanner: View {
    let icon: String
    let color: Color
    let title: String
    let detail: String

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: icon)
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(color)
                .frame(width: 16)
            VStack(alignment: .leading, spacing: 2) {
                Text(L10n.t(title))
                    .font(VPTheme.mono(9, weight: .semibold))
                    .foregroundStyle(VPTheme.textPrimary)
                Text(L10n.t(detail))
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
        }
        .padding(9)
        .background(color.opacity(0.05), in: RoundedRectangle(cornerRadius: 9))
        .overlay {
            RoundedRectangle(cornerRadius: 9)
                .strokeBorder(color.opacity(0.16), lineWidth: 1)
        }
    }
}

struct ProjectOrgEmptyState: View {
    let icon: String
    let title: String
    let detail: String

    var body: some View {
        VStack(spacing: 8) {
            Image(systemName: icon)
                .font(.system(size: 24, weight: .light))
                .foregroundStyle(VPTheme.textTertiary)
            Text(L10n.t(title))
                .font(VPTheme.font(13, weight: .semibold, rounded: true))
                .foregroundStyle(VPTheme.textSecondary)
            Text(L10n.t(detail))
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: 460)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 24)
        .accessibilityElement(children: .combine)
    }
}

struct ProjectOrgLoadingState: View {
    var body: some View {
        HStack(spacing: 8) {
            ProgressView().controlSize(.small)
            Text("Reading the bounded org projection…")
                .font(VPTheme.mono(8.5))
                .foregroundStyle(VPTheme.textTertiary)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 26)
    }
}
