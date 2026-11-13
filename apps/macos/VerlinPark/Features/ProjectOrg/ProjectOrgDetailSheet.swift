import SwiftUI

enum ProjectOrgDetailTab: String, CaseIterable, Identifiable {
    case overview
    case assignments
    case inbox
    case memory
    case performance
    case workspace
    case lifecycle

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .overview: "Overview"
        case .assignments: "Assignments"
        case .inbox: "Inbox"
        case .memory: "Memory"
        case .performance: "Performance"
        case .workspace: "Workspace"
        case .lifecycle: "Lifecycle"
        }
    }

    var icon: String {
        switch self {
        case .overview: "person.text.rectangle"
        case .assignments: "link"
        case .inbox: "tray.full"
        case .memory: "brain"
        case .performance: "chart.bar.xaxis"
        case .workspace: "shippingbox"
        case .lifecycle: "arrow.triangle.2.circlepath"
        }
    }
}

/// Identity console sheet. Every tab is a bounded read; mutations are explicit
/// buttons that open the same approval-aware sheets used by the org card.
struct ProjectOrgDetailSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var tab: ProjectOrgDetailTab = .overview

    private var selection: ProjectOrgIdentitySelection? {
        model.projectOrgDetailSelection
    }

    var body: some View {
        @Bindable var model = model

        VStack(alignment: .leading, spacing: 0) {
            header
            VPHairline()
            tabStrip
            VPHairline()
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    if let error = model.projectOrgDetailError {
                        ProjectOrgErrorBanner(error: error) {
                            Task {
                                if let id = selection?.identityID {
                                    await model.loadProjectOrgDetail(identityID: id)
                                }
                            }
                        }
                    }
                    tabContent
                        .id(tab)
                        .transition(.vpContent)
                }
                .padding(18)
                .frame(maxWidth: .infinity, alignment: .leading)
                .animation(
                    VPMotion.resolved(VPMotion.smooth, reduceMotion: reduceMotion),
                    value: tab
                )
            }
        }
        .frame(width: 820, height: 700)
        .background { VPCrystalBackdrop() }
        .tint(VPTheme.accent)
        .task {
            if let id = selection?.identityID {
                await model.loadProjectOrgDetail(identityID: id)
            }
        }
        .sheet(item: $model.projectOrgSheet) { sheet in
            ProjectOrgActionSheet(sheet: sheet)
                .environment(model)
        }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 12) {
            if let detail = model.projectOrgDetail {
                ProjectOrgAvatar(identity: detail.identity)
                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 6) {
                        Text(detail.identity.displayName)
                            .font(VPTheme.font(20, weight: .semibold, rounded: true))
                            .foregroundStyle(VPTheme.textPrimary)
                            .lineLimit(1)
                        Text(detail.identity.role.uppercased())
                            .font(VPTheme.mono(7, weight: .semibold))
                            .tracking(0.8)
                            .foregroundStyle(VPTheme.roleColor(detail.identity.role))
                            .padding(.horizontal, 5)
                            .padding(.vertical, 2)
                            .background(
                                VPTheme.roleColor(detail.identity.role).opacity(0.10),
                                in: Capsule()
                            )
                    }
                    Text(identitySubtitle(detail.identity))
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textSecondary)
                        .lineLimit(2)
                    HStack(spacing: 6) {
                        VPStatusDot(color: VPTheme.statusColor(detail.identity.status), size: 5)
                        Text(detail.identity.status)
                            .font(VPTheme.mono(8))
                            .foregroundStyle(VPTheme.statusColor(detail.identity.status))
                        Text("gen \(detail.identity.generation)")
                            .font(VPTheme.mono(8))
                            .foregroundStyle(VPTheme.textTertiary)
                        if detail.readOnly {
                            Text("read-only")
                                .font(VPTheme.mono(7))
                                .foregroundStyle(VPTheme.warning)
                        }
                        if detail.truncated {
                            Text("bounded lists")
                                .font(VPTheme.mono(7))
                                .foregroundStyle(VPTheme.textTertiary)
                        }
                    }
                }
            } else {
                VStack(alignment: .leading, spacing: 3) {
                    Text("Identity detail")
                        .font(VPTheme.font(20, weight: .semibold, rounded: true))
                    Text(selection?.identityID ?? "No identity selected")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                        .lineLimit(1)
                }
            }
            Spacer()
            if model.projectOrgDetailBusy {
                ProgressView().controlSize(.small)
            }
            if let detail = model.projectOrgDetail {
                if detail.canPromote {
                    Button("Promote…") {
                        model.presentProjectOrgPromote(identityID: detail.identity.id)
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(model.projectOrgActionBusy)
                }
                if detail.canTransfer {
                    Button("Transfer…") {
                        model.presentProjectOrgTransfer(identityID: detail.identity.id)
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(model.projectOrgActionBusy)
                }
                if detail.canRetire {
                    Button("Retire…") {
                        model.presentProjectOrgRetire(identityID: detail.identity.id)
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(VPTheme.danger)
                    .controlSize(.small)
                    .disabled(model.projectOrgActionBusy)
                }
            }
            Button {
                Task { await model.loadProjectOrgDetail() }
            } label: {
                Image(systemName: "arrow.clockwise")
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(model.projectOrgDetailBusy)
            .help("Refresh this identity")
            Button("Done") { dismiss() }
                .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                .keyboardShortcut(.cancelAction)
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 15)
        .background(VPTheme.surfaceSunken)
    }

    private var tabStrip: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 4) {
                ForEach(ProjectOrgDetailTab.allCases) { item in
                    Button {
                        tab = item
                    } label: {
                        HStack(spacing: 4) {
                            Image(systemName: item.icon)
                                .font(.system(size: 9, weight: .semibold))
                            Text(L10n.t(item.title))
                                .font(VPTheme.mono(8.5, weight: tab == item ? .semibold : .regular))
                        }
                        .foregroundStyle(tab == item ? VPTheme.textPrimary : VPTheme.textSecondary)
                        .padding(.horizontal, 9)
                        .padding(.vertical, 6)
                        .background {
                            if tab == item {
                                RoundedRectangle(cornerRadius: 8)
                                    .fill(VPTheme.accent.opacity(0.10))
                                    .overlay {
                                        RoundedRectangle(cornerRadius: 8)
                                            .strokeBorder(VPTheme.accent.opacity(0.20), lineWidth: 1)
                                    }
                            }
                        }
                    }
                    .buttonStyle(VPPressableButtonStyle(scale: 0.98))
                    .accessibilityAddTraits(tab == item ? .isSelected : [])
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 7)
        }
    }

    @ViewBuilder
    private var tabContent: some View {
        if let detail = model.projectOrgDetail {
            switch tab {
            case .overview:
                overviewTab(detail)
            case .assignments:
                assignmentsTab(detail)
            case .inbox:
                inboxTab(detail)
            case .memory:
                memoryTab(detail)
            case .performance:
                performanceTab(detail)
            case .workspace:
                workspaceTab(detail)
            case .lifecycle:
                lifecycleTab(detail)
            }
        } else if model.projectOrgDetailBusy {
            ProjectOrgLoadingState()
        } else {
            ProjectOrgEmptyState(
                icon: "person.crop.circle.badge.questionmark",
                title: "No identity loaded",
                detail: "Close the sheet and select an identity from the project org."
            )
        }
    }

    private func overviewTab(_ detail: AgentIdentityDetail) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            if detail.readOnly, let reason = detail.readOnlyReason {
                ProjectOrgNoticeBanner(
                    icon: "lock",
                    color: VPTheme.warning,
                    title: "Read-only identity",
                    detail: reason
                )
            }
            ForEach(detail.warnings, id: \.self) { warning in
                ProjectOrgWarningBanner(message: warning)
            }
            detailMetricGrid(detail)
            HStack(alignment: .top, spacing: 12) {
                detailPanel("Assignment", icon: "link") {
                    if let assignment = detail.node.activeAssignment {
                        detailRow("session", assignment.sessionId, mono: true)
                        detailRow("lease", assignment.leaseActive ? "active" : "expired")
                        detailRow("assigned", assignment.assignedAt)
                        detailRow(
                            "history",
                            "\(assignment.assignmentCount) assignments · \(assignment.sessionCount) sessions"
                        )
                    } else {
                        Text("No active session assignment.")
                            .font(VPTheme.mono(8))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                }
                detailPanel("Workspace", icon: "shippingbox") {
                    if let workspace = detail.node.workspace {
                        detailRow("component", workspace.component)
                        detailRow("branch", workspace.branch, mono: true)
                        detailRow("status", workspace.status)
                        detailRow("lease", workspace.leaseActive ? "active" : "idle")
                    } else {
                        Text("No component workspace.")
                            .font(VPTheme.mono(8))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                }
            }
            HStack(alignment: .top, spacing: 12) {
                detailPanel("Org position", icon: "point.3.connected.trianglepath.dotted") {
                    detailRow("parent", detail.parentIdentity?.displayName ?? "none")
                    detailRow("successor", detail.successor?.displayName ?? "none")
                    detailRow("direct reports", "\(detail.reports.count)")
                    if let department = detail.identity.department {
                        detailRow("department", department)
                    }
                    if let component = detail.identity.component {
                        detailRow("component", component)
                    }
                }
                detailPanel("Policy", icon: "shield.lefthalf.filled") {
                    detailRow("identity policy", detail.policy.enabled ? "enabled" : "disabled")
                    detailRow(
                        "hire approval",
                        detail.policy.hireRequiresApproval ? "required" : "not required"
                    )
                    detailRow(
                        "retirement approval",
                        detail.policy.retirementRequiresApproval ? "required" : "not required"
                    )
                    detailRow(
                        "retirement evaluator",
                        detail.retirementPolicy.enabled ? "enabled" : "disabled"
                    )
                }
            }
            if !detail.reports.isEmpty {
                detailPanel("Direct reports", icon: "person.2") {
                    ForEach(detail.reports.prefix(12)) { report in
                        HStack(spacing: 6) {
                            Image(systemName: VPTheme.roleIcon(report.role))
                                .font(.system(size: 9))
                                .foregroundStyle(VPTheme.roleColor(report.role))
                            Text(report.displayName)
                                .font(VPTheme.mono(8.5))
                                .foregroundStyle(VPTheme.textPrimary)
                            Text(report.role)
                                .font(VPTheme.mono(7))
                                .foregroundStyle(VPTheme.textTertiary)
                            Spacer()
                            Text(report.status)
                                .font(VPTheme.mono(7))
                                .foregroundStyle(VPTheme.statusColor(report.status))
                            Button("Open") {
                                Task { await model.openProjectOrgDetail(identityID: report.id) }
                            }
                            .buttonStyle(.plain)
                            .font(VPTheme.mono(7.5))
                            .foregroundStyle(VPTheme.accent)
                        }
                        .padding(.vertical, 2)
                    }
                }
            }
        }
    }

    private func assignmentsTab(_ detail: AgentIdentityDetail) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            detailPanel("Recent assignments", icon: "link") {
                if detail.assignments.isEmpty {
                    Text("No assignment evidence.")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                } else {
                    ForEach(detail.assignments.prefix(30)) { assignment in
                        HStack(spacing: 7) {
                            VPStatusDot(
                                color: assignment.isLive ? VPTheme.success : VPTheme.textTertiary,
                                size: 5
                            )
                            VStack(alignment: .leading, spacing: 1) {
                                Text(assignment.sessionId)
                                    .font(VPTheme.mono(8.5, weight: .medium))
                                    .foregroundStyle(VPTheme.textPrimary)
                                    .lineLimit(1)
                                Text(
                                    "\(assignment.status) · assigned \(assignment.assignedAt) · heartbeat \(assignment.heartbeatAt)"
                                )
                                .font(VPTheme.mono(7))
                                .foregroundStyle(VPTheme.textTertiary)
                                .lineLimit(1)
                            }
                            Spacer()
                            Text(assignment.leaseActive ? "lease active" : "lease expired")
                                .font(VPTheme.mono(7))
                                .foregroundStyle(
                                    assignment.leaseActive ? VPTheme.success : VPTheme.warning
                                )
                        }
                        .padding(8)
                        .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 8))
                    }
                }
            }
            detailPanel("Assignment rules", icon: "info.circle") {
                Text(
                    "Assignments are explicit S2 operator actions. The lease token never leaves the daemon; expired leases may still read as active until the 30-second sweep."
                )
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private func inboxTab(_ detail: AgentIdentityDetail) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 12) {
                inboxCount("unread", detail.inboxCounts.unread, color: VPTheme.warning)
                inboxCount("read", detail.inboxCounts.read, color: VPTheme.info)
                inboxCount("done", detail.inboxCounts.done, color: VPTheme.success)
                inboxCount("expired", detail.inboxCounts.expired, color: VPTheme.textTertiary)
                Spacer()
                Button(
                    model.projectOrgIncludeInboxContent
                        ? "Hide message summaries"
                        : "Show message summaries"
                ) {
                    Task {
                        await model.loadProjectOrgDetail(
                            identityID: detail.identity.id,
                            includeInboxContent: !model.projectOrgIncludeInboxContent
                        )
                    }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .help(
                    "Metadata is always shown; summaries are a short known-field preview, never the raw payload."
                )
            }
            detailPanel("Inbox metadata", icon: "tray.full") {
                if detail.inbox.isEmpty {
                    Text("No inbox rows.")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                } else {
                    ForEach(detail.inbox.prefix(30)) { message in
                        VStack(alignment: .leading, spacing: 2) {
                            HStack(spacing: 6) {
                                Text(message.kind)
                                    .font(VPTheme.mono(7, weight: .semibold))
                                    .foregroundStyle(VPTheme.accent)
                                    .padding(.horizontal, 5)
                                    .padding(.vertical, 1)
                                    .background(VPTheme.accent.opacity(0.08), in: Capsule())
                                Text(message.status)
                                    .font(VPTheme.mono(7))
                                    .foregroundStyle(
                                        message.isUnread ? VPTheme.warning : VPTheme.textTertiary
                                    )
                                Text("from \(message.fromLabel)")
                                    .font(VPTheme.mono(7))
                                    .foregroundStyle(VPTheme.textTertiary)
                                Spacer()
                                Text(message.createdAt)
                                    .font(VPTheme.mono(6.5))
                                    .foregroundStyle(VPTheme.textTertiary)
                            }
                            if let summary = message.summary {
                                Text(summary)
                                    .font(VPTheme.mono(8.5))
                                    .foregroundStyle(VPTheme.textSecondary)
                                    .lineLimit(2)
                            }
                            if let session = message.sessionId {
                                Text("session \(session)")
                                    .font(VPTheme.mono(6.5))
                                    .foregroundStyle(VPTheme.textTertiary)
                                    .lineLimit(1)
                            }
                        }
                        .padding(7)
                        .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 8))
                    }
                }
            }
            Text(
                "Pull-based delivery is unchanged. This console does not mark messages read or wake the identity."
            )
            .font(VPTheme.mono(7.5))
            .foregroundStyle(VPTheme.textTertiary)
        }
    }

    private func memoryTab(_ detail: AgentIdentityDetail) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Text("\(detail.memories.count) recent of \(detail.memoryTotal) visible")
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textSecondary)
                Spacer()
                Text("agent + department + project scopes")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            if detail.memories.isEmpty {
                detailPanel("Memory", icon: "brain") {
                    Text("No visible memory rows.")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            } else {
                ForEach(detail.memories.prefix(24)) { memory in
                    VStack(alignment: .leading, spacing: 3) {
                        HStack(spacing: 5) {
                            Text(memory.scope)
                                .font(VPTheme.mono(7, weight: .semibold))
                                .foregroundStyle(scopeColor(memory.scope))
                            Text(memory.effectiveStatus)
                                .font(VPTheme.mono(7))
                                .foregroundStyle(VPTheme.textTertiary)
                            if memory.pinned == true {
                                Image(systemName: "pin.fill")
                                    .font(.system(size: 6))
                                    .foregroundStyle(VPTheme.accent)
                            }
                            if memory.isImported {
                                Text("imported")
                                    .font(VPTheme.mono(6.5))
                                    .foregroundStyle(VPTheme.warning)
                            }
                            Spacer()
                            Text(memory.updatedAt ?? memory.createdAt ?? "")
                                .font(VPTheme.mono(6.5))
                                .foregroundStyle(VPTheme.textTertiary)
                        }
                        Text(memory.content)
                            .font(VPTheme.mono(8.5))
                            .foregroundStyle(VPTheme.textSecondary)
                            .fixedSize(horizontal: false, vertical: true)
                        if !memory.evidenceLabelParts.isEmpty {
                            Text(memory.evidenceLabelParts.joined(separator: " · "))
                                .font(VPTheme.mono(6.5))
                                .foregroundStyle(VPTheme.textTertiary)
                                .lineLimit(1)
                        }
                    }
                    .padding(8)
                    .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 8))
                    .overlay {
                        RoundedRectangle(cornerRadius: 8)
                            .strokeBorder(VPTheme.hairline, lineWidth: 1)
                    }
                }
            }
            Text("Memory is untrusted evidence with provenance, not authorization or instructions.")
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
        }
    }

    private func performanceTab(_ detail: AgentIdentityDetail) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            detailPanel("Derived performance ledger", icon: "chart.bar.xaxis") {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 120), spacing: 8)], spacing: 8) {
                    performanceMetric("Completed", detail.performance.completedTasks)
                    performanceMetric("Failed", detail.performance.failedTasks)
                    performanceMetric("Consecutive", detail.performance.consecutiveFailures)
                    performanceMetric("Retries", detail.performance.retries)
                    performanceMetric("Verifier ✓", detail.performance.verifierApprovals)
                    performanceMetric("Verifier ×", detail.performance.verifierRejections)
                    performanceMetric("Input", detail.performance.inputTokens ?? 0)
                    performanceMetric("Output", detail.performance.outputTokens ?? 0)
                }
                HStack(spacing: 12) {
                    Text("cost \(String(format: "$%.3f", detail.performance.costUsd))")
                    if let last = detail.performance.lastActiveAt {
                        Text("last active \(last)")
                    }
                }
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
            }
            if let reasons = detail.performance.reasonCounts, !reasons.isEmpty {
                detailPanel("Diagnosis reasons", icon: "stethoscope") {
                    ForEach(Array(reasons.enumerated()), id: \.offset) { _, reason in
                        HStack {
                            Text(reason.code)
                                .font(VPTheme.mono(8))
                                .foregroundStyle(VPTheme.textSecondary)
                            Spacer()
                            Text("\(reason.count)")
                                .font(VPTheme.mono(8, weight: .semibold))
                                .foregroundStyle(VPTheme.warning)
                        }
                    }
                }
            }
            if let evidence = detail.performance.evidence, !evidence.isEmpty {
                detailPanel("Evidence", icon: "doc.text.magnifyingglass") {
                    ForEach(Array(evidence.prefix(10).enumerated()), id: \.offset) { _, item in
                        HStack(spacing: 6) {
                            Text(item.kind)
                                .font(VPTheme.mono(7, weight: .semibold))
                                .foregroundStyle(VPTheme.accent)
                            Text(item.reference)
                                .font(VPTheme.mono(7.5))
                                .foregroundStyle(VPTheme.textSecondary)
                                .lineLimit(1)
                            Spacer()
                            if let reason = item.reason {
                                Text(reason)
                                    .font(VPTheme.mono(7))
                                    .foregroundStyle(VPTheme.textTertiary)
                            }
                        }
                        .padding(.vertical, 1)
                    }
                }
            }
            Text("Counts are deterministic attribution evidence, not a ranking, score, or merge gate.")
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
        }
    }

    private func workspaceTab(_ detail: AgentIdentityDetail) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            if detail.workspaces.isEmpty {
                detailPanel("Workspaces", icon: "shippingbox") {
                    Text("No Worker component workspace is recorded for this identity.")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            } else {
                ForEach(detail.workspaces) { workspace in
                    detailPanel(workspace.component, icon: "shippingbox") {
                        detailRow("branch", workspace.branch, mono: true)
                        detailRow("status", workspace.status)
                        detailRow("lease", workspace.leaseActive ? "active" : "idle")
                        if let expires = workspace.leaseExpiresAt {
                            detailRow("lease expires", expires)
                        }
                        if let session = workspace.lastSessionId {
                            detailRow("last session", session)
                        }
                        if workspace.workspaceCount > 1 {
                            detailRow("component workspaces", "\(workspace.workspaceCount)")
                        }
                    }
                }
            }
            Text(
                "Absolute worktree paths are intentionally not shown. Workspace land, archive, and retire remain explicit operator/CLI actions."
            )
            .font(VPTheme.mono(7.5))
            .foregroundStyle(VPTheme.textTertiary)
        }
    }

    private func lifecycleTab(_ detail: AgentIdentityDetail) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            detailPanel("Policy and approvals", icon: "shield.lefthalf.filled") {
                detailRow(
                    "identity policy",
                    detail.policy.enabled ? "enabled" : "disabled"
                )
                detailRow(
                    "hire approval",
                    detail.policy.hireRequiresApproval ? "required" : "not required"
                )
                detailRow(
                    "retirement approval",
                    detail.policy.retirementRequiresApproval ? "required" : "not required"
                )
                detailRow(
                    "retirement evaluator",
                    detail.retirementPolicy.enabled ? "enabled" : "disabled"
                )
                detailRow("imported", detail.identity.isImported ? "yes" : "no")
                detailRow("read-only", detail.readOnly ? "yes" : "no")
            }
            if !detail.retirementCandidates.isEmpty {
                detailPanel("Advisory retirement candidates", icon: "exclamationmark.triangle") {
                    ForEach(detail.retirementCandidates) { candidate in
                        VStack(alignment: .leading, spacing: 3) {
                            HStack {
                                Text("severity \(candidate.severity)")
                                    .font(VPTheme.mono(8, weight: .semibold))
                                    .foregroundStyle(VPTheme.warning)
                                Spacer()
                                Button("Propose retirement") {
                                    Task {
                                        await model
                                            .proposeProjectOrgRetirement(identityID: detail.identity.id)
                                    }
                                }
                                .buttonStyle(.bordered)
                                .controlSize(.mini)
                                .disabled(detail.readOnly || model.projectOrgActionBusy)
                            }
                            Text(candidate.reasonSummary)
                                .font(VPTheme.mono(7.5))
                                .foregroundStyle(VPTheme.textSecondary)
                            Text(
                                "Deterministic evidence only. Approval and a separate retire action remain required."
                            )
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.textTertiary)
                        }
                        .padding(7)
                        .background(VPTheme.warning.opacity(0.04), in: RoundedRectangle(cornerRadius: 8))
                    }
                }
            }
            detailPanel("Hire requests", icon: "person.badge.plus") {
                if detail.hireRequests.isEmpty {
                    Text("No hire requests for this reserved identity.")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                } else {
                    ForEach(detail.hireRequests) { request in
                        HStack(spacing: 6) {
                            Text(request.status)
                                .font(VPTheme.mono(7, weight: .semibold))
                                .foregroundStyle(
                                    request.status == "pending" ? VPTheme.warning : VPTheme.textTertiary
                                )
                            Text("\(request.role) · \(request.title ?? request.component ?? "")")
                                .font(VPTheme.mono(7.5))
                                .foregroundStyle(VPTheme.textSecondary)
                                .lineLimit(1)
                            Spacer()
                            if request.status == "pending" {
                                Button("Approve") {
                                    Task { await model.approveProjectOrgHire(requestID: request.id) }
                                }
                                .buttonStyle(.bordered)
                                .controlSize(.mini)
                                .disabled(request.origin == "imported" || model.projectOrgActionBusy)
                                Button("Reject") {
                                    Task { await model.rejectProjectOrgHire(requestID: request.id) }
                                }
                                .buttonStyle(.bordered)
                                .controlSize(.mini)
                                .disabled(request.origin == "imported" || model.projectOrgActionBusy)
                            }
                        }
                    }
                }
            }
            detailPanel("Retirement proposals", icon: "person.crop.circle.badge.minus") {
                if detail.retirementProposals.isEmpty {
                    Text("No retirement proposals.")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                } else {
                    ForEach(detail.retirementProposals) { proposal in
                        HStack(spacing: 6) {
                            Text(proposal.status)
                                .font(VPTheme.mono(7, weight: .semibold))
                                .foregroundStyle(
                                    proposal.status == "pending" ? VPTheme.warning : VPTheme.textTertiary
                                )
                            Text(proposal.reasonCode)
                                .font(VPTheme.mono(7.5))
                                .foregroundStyle(VPTheme.textSecondary)
                            Text(proposal.proposedAt)
                                .font(VPTheme.mono(6.5))
                                .foregroundStyle(VPTheme.textTertiary)
                            Spacer()
                            if proposal.status == "pending" {
                                Button("Approve") {
                                    Task {
                                        await model.approveProjectOrgRetirement(proposalID: proposal.id)
                                    }
                                }
                                .buttonStyle(.bordered)
                                .controlSize(.mini)
                                .disabled(proposal.origin == "imported" || model.projectOrgActionBusy)
                                Button("Reject") {
                                    Task {
                                        await model.rejectProjectOrgRetirement(proposalID: proposal.id)
                                    }
                                }
                                .buttonStyle(.bordered)
                                .controlSize(.mini)
                                .disabled(proposal.origin == "imported" || model.projectOrgActionBusy)
                                Button("Retire…") {
                                    model.presentProjectOrgRetire(identityID: detail.identity.id)
                                }
                                .buttonStyle(.bordered)
                                .controlSize(.mini)
                                .disabled(detail.readOnly || model.projectOrgActionBusy)
                            }
                        }
                    }
                }
            }
            detailPanel("Employment evidence", icon: "clock.arrow.circlepath") {
                if detail.employmentEvents.isEmpty {
                    Text("No employment events recorded.")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                } else {
                    ForEach(detail.employmentEvents) { event in
                        HStack(spacing: 6) {
                            Text(event.shortKind)
                                .font(VPTheme.mono(7, weight: .semibold))
                                .foregroundStyle(VPTheme.accent)
                            Spacer()
                            Text(event.createdAt)
                                .font(VPTheme.mono(6.5))
                                .foregroundStyle(VPTheme.textTertiary)
                        }
                        .padding(.vertical, 1)
                    }
                }
            }
            HStack {
                if detail.canPromote {
                    Button("Promote…") {
                        model.presentProjectOrgPromote(identityID: detail.identity.id)
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                }
                if detail.canTransfer {
                    Button("Transfer…") {
                        model.presentProjectOrgTransfer(identityID: detail.identity.id)
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                }
                Spacer()
                if detail.canRetire {
                    Button("Retire identity…") {
                        model.presentProjectOrgRetire(identityID: detail.identity.id)
                    }
                    .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.danger))
                    .controlSize(.small)
                    .disabled(model.projectOrgActionBusy)
                }
            }
        }
    }

    private func detailMetricGrid(_ detail: AgentIdentityDetail) -> some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 130), spacing: 8)], spacing: 8) {
            overviewMetric(
                "Inbox",
                "\(detail.inboxCounts.unread)",
                detail: "unread",
                icon: "tray.full",
                color: detail.inboxCounts.unread > 0 ? VPTheme.warning : VPTheme.textTertiary
            )
            overviewMetric(
                "Memory",
                "\(detail.memoryTotal)",
                detail: "visible",
                icon: "brain",
                color: VPTheme.info
            )
            overviewMetric(
                "Tasks",
                detail.performance.completedTasks > 0
                    ? "\(detail.performance.completedTasks)✓ \(detail.performance.failedTasks)×"
                    : "—",
                detail: "completed/failed",
                icon: "checklist",
                color: VPTheme.success
            )
            overviewMetric(
                "Verifier",
                "\(detail.performance.verifierApprovals)/\(detail.performance.verifierRejections)",
                detail: "approvals/rejections",
                icon: "checkmark.shield",
                color: VPTheme.plan
            )
            overviewMetric(
                "Workspaces",
                "\(detail.workspaces.count)",
                detail: detail.workspaces.first?.component ?? "none",
                icon: "shippingbox",
                color: VPTheme.accent
            )
            overviewMetric(
                "Retirement",
                detail.retirementCandidates.isEmpty ? "—" : "\(detail.retirementCandidates.count)",
                detail: detail.retirementCandidates.isEmpty ? "no candidate" : "advisory candidate",
                icon: "exclamationmark.triangle",
                color: detail.retirementCandidates.isEmpty ? VPTheme.textTertiary : VPTheme.warning
            )
        }
    }

    private func overviewMetric(
        _ title: String,
        _ value: String,
        detail: String,
        icon: String,
        color: Color
    ) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Label(L10n.t(title).uppercased(), systemImage: icon)
                .font(VPTheme.mono(7, weight: .semibold))
                .foregroundStyle(VPTheme.textTertiary)
            Text(value)
                .font(VPTheme.mono(13, weight: .semibold))
                .foregroundStyle(VPTheme.textPrimary)
            Text(detail)
                .font(VPTheme.mono(6.5))
                .foregroundStyle(color)
                .lineLimit(1)
        }
        .padding(9)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(VPTheme.surfaceSunken.opacity(0.55), in: RoundedRectangle(cornerRadius: 9))
        .overlay {
            RoundedRectangle(cornerRadius: 9)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
    }

    private func performanceMetric(_ label: String, _ value: Int) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(L10n.t(label).uppercased())
                .font(VPTheme.mono(6.5, weight: .semibold))
                .foregroundStyle(VPTheme.textTertiary)
            Text("\(value)")
                .font(VPTheme.mono(11, weight: .semibold))
                .foregroundStyle(VPTheme.textPrimary)
        }
        .padding(7)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(VPTheme.surfaceSunken.opacity(0.45), in: RoundedRectangle(cornerRadius: 7))
    }

    private func inboxCount(_ label: String, _ value: Int, color: Color) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(L10n.t(label).uppercased())
                .font(VPTheme.mono(6.5, weight: .semibold))
                .foregroundStyle(VPTheme.textTertiary)
            Text("\(value)")
                .font(VPTheme.mono(11, weight: .semibold))
                .foregroundStyle(color)
        }
    }

    private func detailPanel(
        _ title: String,
        icon: String,
        @ViewBuilder content: () -> some View
    ) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Label(L10n.t(title), systemImage: icon)
                .font(VPTheme.mono(8.5, weight: .semibold))
                .foregroundStyle(VPTheme.textSecondary)
            content()
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(VPTheme.surfaceSunken.opacity(0.40), in: RoundedRectangle(cornerRadius: 10))
        .overlay {
            RoundedRectangle(cornerRadius: 10)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
    }

    private func detailRow(_ label: String, _ value: String, mono: Bool = false) -> some View {
        HStack(alignment: .top, spacing: 6) {
            Text(L10n.t(label))
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
                .frame(width: 112, alignment: .leading)
            Text(value)
                .font(mono ? VPTheme.mono(7.5) : VPTheme.font(9))
                .foregroundStyle(VPTheme.textSecondary)
                .lineLimit(3)
                .truncationMode(.middle)
            Spacer(minLength: 0)
        }
    }

    private func identitySubtitle(_ identity: AgentIdentity) -> String {
        var parts = [identity.role]
        if let department = identity.department, !department.isEmpty {
            parts.append(department)
        }
        if let component = identity.component, !component.isEmpty {
            parts.append(component)
        }
        if let profile = identity.profile, !profile.isEmpty {
            parts.append("profile \(profile)")
        }
        if let model = identity.model, !model.isEmpty {
            parts.append(model)
        }
        parts.append("id \(identity.id)")
        return parts.joined(separator: " · ")
    }

    private func scopeColor(_ scope: String) -> Color {
        switch scope {
        case "agent": VPTheme.info
        case "department": VPTheme.plan
        case "project": VPTheme.success
        default: VPTheme.textSecondary
        }
    }
}

struct ProjectOrgAvatar: View {
    let identity: AgentIdentity

    var body: some View {
        ZStack {
            Circle()
                .fill(VPTheme.avatarGradient(seed: identity.id))
            Text(initials)
                .font(VPTheme.mono(11, weight: .semibold))
                .foregroundStyle(.white)
        }
        .frame(width: 40, height: 40)
        .overlay {
            Circle().strokeBorder(.white.opacity(0.14), lineWidth: 1)
        }
        .accessibilityHidden(true)
    }

    private var initials: String {
        let source = identity.title ?? identity.component ?? identity.role
        let words = source.split(separator: " ").prefix(2)
        let letters = words.compactMap(\.first).map(String.init).joined()
        return letters.isEmpty ? "?" : letters.uppercased()
    }
}
