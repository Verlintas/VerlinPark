import SwiftUI

/// One action sheet for the project org console. The view is declarative: every
/// mutation is a real RPC through AppModel and every failure surfaces unchanged.
struct ProjectOrgActionSheet: View {
    @Environment(AppModel.self) private var model
    let sheet: ProjectOrgSheet

    var body: some View {
        switch sheet {
        case .hire:
            ProjectOrgHireSheet()
                .environment(model)
        case .promote:
            ProjectOrgPromoteSheet()
                .environment(model)
        case .transfer:
            ProjectOrgTransferSheet()
                .environment(model)
        case .retire:
            ProjectOrgRetireSheet()
                .environment(model)
        }
    }
}

private struct ProjectOrgSheetHeader: View {
    let eyebrow: String
    let title: String
    let detail: String

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(L10n.t(eyebrow).uppercased())
                .font(VPTheme.mono(8, weight: .semibold))
                .tracking(1.1)
                .foregroundStyle(VPTheme.accent)
            Text(L10n.t(title))
                .font(VPTheme.font(19, weight: .semibold, rounded: true))
                .foregroundStyle(VPTheme.textPrimary)
            Text(L10n.t(detail))
                .font(VPTheme.font(10))
                .foregroundStyle(VPTheme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}

private struct ProjectOrgSheetError: View {
    let error: ProjectOrgErrorSurface?

    var body: some View {
        if let error {
            HStack(alignment: .top, spacing: 6) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.system(size: 9))
                    .foregroundStyle(VPTheme.danger)
                Text(L10n.t(error.displayText))
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.danger)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 0)
            }
            .padding(8)
            .background(VPTheme.danger.opacity(0.06), in: RoundedRectangle(cornerRadius: 8))
            .overlay {
                RoundedRectangle(cornerRadius: 8)
                    .strokeBorder(VPTheme.danger.opacity(0.18), lineWidth: 1)
            }
        }
    }
}

private struct ProjectOrgFieldLabel: View {
    let text: String
    var detail: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(L10n.t(text).uppercased())
                .font(VPTheme.mono(7.5, weight: .semibold))
                .tracking(0.7)
                .foregroundStyle(VPTheme.textTertiary)
            if let detail {
                Text(L10n.t(detail))
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary.opacity(0.85))
            }
        }
    }
}

// MARK: - Hire

struct ProjectOrgHireSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    private var policy: AgentIdentityPolicy {
        model.projectOrgSnapshot?.policy ?? .disabledDefault
    }

    private var parents: [AgentIdentity] {
        let snapshot = model.projectOrgSnapshot
        if model.projectOrgHireDraft.normalizedRole == "manager" {
            return snapshot?.leaderChoices ?? []
        }
        return snapshot?.managerChoices ?? []
    }

    private var canSubmit: Bool {
        !model.projectOrgActionBusy
            && model.projectOrgHireDraft.validationError() == nil
            && model.selectedProjectID != nil
    }

    var body: some View {
        @Bindable var model = model

        VStack(alignment: .leading, spacing: 14) {
            ProjectOrgSheetHeader(
                eyebrow: "Hire",
                title: "Add a Manager or Worker",
                detail: "This creates a durable project identity only. It does not create a session, assignment, workspace, inbox row, or memory. The Leader is bootstrapped separately."
            )
            ProjectOrgSheetError(error: model.projectOrgActionError)
            if !policy.enabled {
                ProjectOrgNoticeBanner(
                    icon: "shield.lefthalf.filled",
                    color: VPTheme.warning,
                    title: "Identity policy disabled",
                    detail: "The daemon will reject this hire until the project enables the identity policy."
                )
            }
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 4) {
                    ProjectOrgFieldLabel(text: "Role", detail: "Manager or Worker")
                    Picker("Role", selection: $model.projectOrgHireDraft.role) {
                        Text("Worker").tag("worker")
                        Text("Manager").tag("manager")
                    }
                    .labelsHidden()
                    .pickerStyle(.segmented)
                }
                VStack(alignment: .leading, spacing: 4) {
                    ProjectOrgFieldLabel(
                        text: "Parent",
                        detail: model.projectOrgHireDraft.normalizedRole == "manager"
                            ? "A Manager reports to the Leader"
                            : "A Worker reports to a Manager"
                    )
                    Picker("Parent", selection: $model.projectOrgHireDraft.parentIdentityID) {
                        Text("Daemon resolves when unambiguous").tag("")
                        ForEach(parents) { identity in
                            Text("\(identity.displayName) · \(identity.role)").tag(identity.id)
                        }
                    }
                    .labelsHidden()
                    .pickerStyle(.menu)
                }
            }
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 4) {
                    ProjectOrgFieldLabel(text: "Title", detail: "Shown as the org chart name")
                    TextField("e.g. Auth Technical Lead", text: $model.projectOrgHireDraft.title)
                        .textFieldStyle(.plain)
                        .vpFrostedField()
                }
                VStack(alignment: .leading, spacing: 4) {
                    ProjectOrgFieldLabel(text: "Profile", detail: "Explicit role profile")
                    Picker("Profile", selection: $model.projectOrgHireDraft.profile) {
                        Text("coding").tag("coding")
                        Text("research").tag("research")
                        Text("verification").tag("verification")
                        Text("generic").tag("generic")
                    }
                    .labelsHidden()
                    .pickerStyle(.menu)
                }
            }
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 4) {
                    ProjectOrgFieldLabel(text: "Department", detail: "Project org lane")
                    TextField("e.g. Platform", text: $model.projectOrgHireDraft.department)
                        .textFieldStyle(.plain)
                        .vpFrostedField()
                }
                VStack(alignment: .leading, spacing: 4) {
                    ProjectOrgFieldLabel(text: "Component", detail: "Worker ownership area")
                    TextField("e.g. auth", text: $model.projectOrgHireDraft.component)
                        .textFieldStyle(.plain)
                        .vpFrostedField()
                }
            }
            VStack(alignment: .leading, spacing: 4) {
                ProjectOrgFieldLabel(
                    text: "Model reference",
                    detail: "Metadata only; it never routes a provider call by itself"
                )
                TextField(
                    "Leave empty to use the configured role model",
                    text: $model.projectOrgHireDraft.model
                )
                .textFieldStyle(.plain)
                .vpFrostedField()
            }
            VStack(alignment: .leading, spacing: 6) {
                Toggle("Approve immediately", isOn: $model.projectOrgHireDraft.approved)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                    .disabled(!policy.hireRequiresApproval)
                Text(
                    policy.hireRequiresApproval
                        ? L10n.t(model.projectOrgHireDraft.approvalNote)
                        : L10n
                        .t(
                            "Approval is not required by policy; this explicit submit creates the identity."
                        )
                )
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
                Text(
                    "Caps: \(policy.maxManagers) Managers · \(policy.maxWorkers) Workers. Caps and hierarchy are revalidated by the daemon."
                )
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
            }
            .padding(9)
            .background(VPTheme.surfaceSunken.opacity(0.5), in: RoundedRectangle(cornerRadius: 9))
            if let error = model.projectOrgHireDraft.validationError() {
                Text(L10n.t(error))
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.danger)
            }
            footer
        }
        .padding(20)
        .frame(width: 620)
        .background { VPCrystalBackdrop() }
        .tint(VPTheme.accent)
        .interactiveDismissDisabled(model.projectOrgActionBusy)
        .onChange(of: model.projectOrgHireDraft.role) { _, _ in
            model.projectOrgHireRoleChanged()
        }
    }

    private var footer: some View {
        HStack {
            Button("Cancel") {
                model.dismissProjectOrgSheet()
                dismiss()
            }
            .keyboardShortcut(.cancelAction)
            .disabled(model.projectOrgActionBusy)
            Spacer()
            Button {
                Task { await model.submitProjectOrgHire() }
            } label: {
                Label(
                    model.projectOrgActionBusy ? "Submitting…" : "Submit hire",
                    systemImage: "person.badge.plus"
                )
            }
            .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
            .keyboardShortcut(.defaultAction)
            .disabled(!canSubmit)
        }
    }
}

// MARK: - Promote

struct ProjectOrgPromoteSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    private var identity: AgentIdentity? {
        model.projectOrgIdentity(model.projectOrgPromoteDraft.identityID)
    }

    private var targets: [(String, String)] {
        switch model.projectOrgPromoteDraft.currentRole {
        case "worker": [("manager", "Manager")]
        case "manager": [("leader", "Leader")]
        default: []
        }
    }

    private var canSubmit: Bool {
        identity != nil
            && !model.projectOrgActionBusy
            && model.projectOrgPromoteDraft.validationError() == nil
    }

    var body: some View {
        @Bindable var model = model

        VStack(alignment: .leading, spacing: 14) {
            ProjectOrgSheetHeader(
                eyebrow: "Promote",
                title: "Change an identity's role",
                detail: "Only Worker→Manager and Manager→Leader are supported. Promotion is approval-classified, increments the generation once, and is never automatic."
            )
            ProjectOrgSheetError(error: model.projectOrgActionError)
            if let identity {
                identityStrip(identity)
            }
            VStack(alignment: .leading, spacing: 5) {
                ProjectOrgFieldLabel(text: "Target role", detail: "The current role cannot be kept")
                Picker("Target role", selection: $model.projectOrgPromoteDraft.targetRole) {
                    ForEach(targets, id: \.0) { role, label in
                        Text(L10n.t(label)).tag(role)
                    }
                }
                .labelsHidden()
                .pickerStyle(.segmented)
            }
            if model.projectOrgPromoteDraft.targetRole == "manager" {
                VStack(alignment: .leading, spacing: 5) {
                    ProjectOrgFieldLabel(
                        text: "Leader parent",
                        detail: "A promoted Manager reports to the Leader"
                    )
                    Picker("Leader parent", selection: $model.projectOrgPromoteDraft.parentIdentityID) {
                        Text("Daemon resolves when exactly one Leader exists").tag("")
                        ForEach(model.projectOrgSnapshot?.leaderChoices ?? []) { leader in
                            Text(leader.displayName).tag(leader.id)
                        }
                    }
                    .labelsHidden()
                    .pickerStyle(.menu)
                }
            }
            VStack(alignment: .leading, spacing: 5) {
                ProjectOrgFieldLabel(text: "Title", detail: "Optional org chart title update")
                TextField("Leave empty to keep the current title", text: $model.projectOrgPromoteDraft.title)
                    .textFieldStyle(.plain)
                    .vpFrostedField()
            }
            VStack(alignment: .leading, spacing: 5) {
                Toggle("Force release active work", isOn: $model.projectOrgPromoteDraft.force)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                Text(L10n.t(model.projectOrgPromoteDraft.warning))
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.warning)
                    .fixedSize(horizontal: false, vertical: true)
                Text(
                    "Force releases assignments and archives workspaces through their normal events; it never deletes evidence or interrupts an in-flight provider stream."
                )
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
            }
            .padding(9)
            .background(VPTheme.warning.opacity(0.04), in: RoundedRectangle(cornerRadius: 9))
            Text("Submitting records an explicit approved: true decision. There is no auto-promotion.")
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
            footer
        }
        .padding(20)
        .frame(width: 580)
        .background { VPCrystalBackdrop() }
        .tint(VPTheme.accent)
        .interactiveDismissDisabled(model.projectOrgActionBusy)
    }

    private func identityStrip(_ identity: AgentIdentity) -> some View {
        HStack(spacing: 8) {
            Image(systemName: VPTheme.roleIcon(identity.role))
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(VPTheme.roleColor(identity.role))
            VStack(alignment: .leading, spacing: 1) {
                Text(identity.displayName)
                    .font(VPTheme.mono(9, weight: .semibold))
                    .foregroundStyle(VPTheme.textPrimary)
                Text("\(identity.role) · gen \(identity.generation) · \(identity.id)")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(1)
            }
            Spacer()
            Text(identity.status)
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.statusColor(identity.status))
        }
        .padding(9)
        .background(VPTheme.surfaceSunken.opacity(0.5), in: RoundedRectangle(cornerRadius: 9))
    }

    private var footer: some View {
        HStack {
            Button("Cancel") {
                model.dismissProjectOrgSheet()
                dismiss()
            }
            .keyboardShortcut(.cancelAction)
            .disabled(model.projectOrgActionBusy)
            Spacer()
            Button {
                Task { await model.submitProjectOrgPromote() }
            } label: {
                Label(
                    model.projectOrgActionBusy ? "Promoting…" : "Approve promotion",
                    systemImage: "arrow.up.circle"
                )
            }
            .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
            .keyboardShortcut(.defaultAction)
            .disabled(!canSubmit)
        }
    }
}

// MARK: - Transfer

struct ProjectOrgTransferSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    private var identity: AgentIdentity? {
        model.projectOrgIdentity(model.projectOrgTransferDraft.identityID)
    }

    private var parentChoices: [AgentIdentity] {
        (model.projectOrgSnapshot?.parentChoices ?? [])
            .filter { $0.id != model.projectOrgTransferDraft.identityID }
    }

    var body: some View {
        @Bindable var model = model

        VStack(alignment: .leading, spacing: 14) {
            ProjectOrgSheetHeader(
                eyebrow: "Transfer",
                title: "Move an identity without changing its role",
                detail: "Department, component, title, and parent are org changes. Transfer is approval-classified, increments the generation, and active work is a conflict unless force is set."
            )
            ProjectOrgSheetError(error: model.projectOrgActionError)
            if let identity {
                HStack(spacing: 8) {
                    Image(systemName: VPTheme.roleIcon(identity.role))
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(VPTheme.roleColor(identity.role))
                    VStack(alignment: .leading, spacing: 1) {
                        Text(identity.displayName)
                            .font(VPTheme.mono(9, weight: .semibold))
                            .foregroundStyle(VPTheme.textPrimary)
                        Text("\(identity.role) · gen \(identity.generation) · id \(identity.id)")
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.textTertiary)
                            .lineLimit(1)
                    }
                    Spacer()
                    Text(identity.status)
                        .font(VPTheme.mono(7.5))
                        .foregroundStyle(VPTheme.statusColor(identity.status))
                }
                .padding(9)
                .background(VPTheme.surfaceSunken.opacity(0.5), in: RoundedRectangle(cornerRadius: 9))
            }
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 4) {
                    ProjectOrgFieldLabel(text: "Department", detail: "Empty clears the field")
                    TextField("Department", text: $model.projectOrgTransferDraft.department)
                        .textFieldStyle(.plain)
                        .vpFrostedField()
                }
                VStack(alignment: .leading, spacing: 4) {
                    ProjectOrgFieldLabel(text: "Component", detail: "Empty clears the field")
                    TextField("Component", text: $model.projectOrgTransferDraft.component)
                        .textFieldStyle(.plain)
                        .vpFrostedField()
                }
            }
            VStack(alignment: .leading, spacing: 4) {
                ProjectOrgFieldLabel(text: "Title", detail: "Empty clears the field")
                TextField("Title", text: $model.projectOrgTransferDraft.title)
                    .textFieldStyle(.plain)
                    .vpFrostedField()
            }
            VStack(alignment: .leading, spacing: 4) {
                ProjectOrgFieldLabel(text: "Parent", detail: "Leader or Manager in this project")
                Picker("Parent", selection: $model.projectOrgTransferDraft.parentIdentityID) {
                    Text("No change").tag(identity?.parentIdentityId ?? "")
                    ForEach(parentChoices) { parent in
                        Text("\(parent.displayName) · \(parent.role)").tag(parent.id)
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                Toggle("Clear the parent explicitly", isOn: $model.projectOrgTransferDraft.clearParent)
                    .toggleStyle(.checkbox)
                    .controlSize(.small)
                    .font(VPTheme.mono(8))
                    .disabled(model.projectOrgTransferDraft
                        .parentIdentityID != (identity?.parentIdentityId ?? ""))
            }
            VStack(alignment: .leading, spacing: 5) {
                Toggle("Force release active work", isOn: $model.projectOrgTransferDraft.force)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                Text(L10n.t(model.projectOrgTransferDraft.approvalNote))
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.warning)
                    .fixedSize(horizontal: false, vertical: true)
                Text(
                    "Force releases assignments and archives workspaces; branches, memory, and event evidence are preserved."
                )
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
            }
            .padding(9)
            .background(VPTheme.warning.opacity(0.04), in: RoundedRectangle(cornerRadius: 9))
            Text("Only changed fields are sent; an untouched field is omitted, not overwritten.")
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
            footer
        }
        .padding(20)
        .frame(width: 600)
        .background { VPCrystalBackdrop() }
        .tint(VPTheme.accent)
        .interactiveDismissDisabled(model.projectOrgActionBusy)
        .onChange(of: model.projectOrgTransferDraft.parentIdentityID) { _, _ in
            model.projectOrgTransferParentChanged()
        }
    }

    private var footer: some View {
        HStack {
            Button("Cancel") {
                model.dismissProjectOrgSheet()
                dismiss()
            }
            .keyboardShortcut(.cancelAction)
            .disabled(model.projectOrgActionBusy)
            Spacer()
            Button {
                Task { await model.submitProjectOrgTransfer() }
            } label: {
                Label(
                    model.projectOrgActionBusy ? "Transferring…" : "Approve transfer",
                    systemImage: "arrow.left.arrow.right"
                )
            }
            .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
            .keyboardShortcut(.defaultAction)
            .disabled(
                identity == nil
                    || model.projectOrgActionBusy
                    || model.projectOrgTransferDraft.identityID.isEmpty
            )
        }
    }
}

// MARK: - Retire

struct ProjectOrgRetireSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    private var identity: AgentIdentity? {
        model.projectOrgIdentity(model.projectOrgRetireDraft.identityID)
    }

    private var detail: AgentIdentityDetail? {
        guard model.projectOrgDetail?.identity.id == model.projectOrgRetireDraft.identityID else {
            return nil
        }
        return model.projectOrgDetail
    }

    private var successorChoices: [AgentIdentity] {
        let snapshot = model.projectOrgSnapshot
        return (snapshot?.liveNodes.map(\.identity) ?? [])
            .filter {
                $0.id != model.projectOrgRetireDraft.identityID
                    && $0.status != "retired"
                    && !$0.isImported
            }
    }

    private var attachableProposals: [AgentRetirementProposal] {
        detail?.retirementProposals.filter { $0.status == "approved" } ?? []
    }

    private var requiresApproval: Bool {
        model.projectOrgRetireDraft.requiresApproval
    }

    private var canSubmit: Bool {
        identity != nil
            && !model.projectOrgActionBusy
            && model.projectOrgRetireDraft.validationError() == nil
            && !model.projectOrgRetireDraft.identityID.isEmpty
    }

    var body: some View {
        @Bindable var model = model

        VStack(alignment: .leading, spacing: 14) {
            ProjectOrgSheetHeader(
                eyebrow: "Retire",
                title: "Retire an identity",
                detail: "Retirement is terminal and irreversible from this console. Evidence, branches, memory, and performance are preserved, and the same id cannot be rehired."
            )
            ProjectOrgSheetError(error: model.projectOrgActionError)
            if let identity {
                HStack(spacing: 8) {
                    Image(systemName: VPTheme.roleIcon(identity.role))
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(VPTheme.roleColor(identity.role))
                    VStack(alignment: .leading, spacing: 1) {
                        Text(identity.displayName)
                            .font(VPTheme.mono(9, weight: .semibold))
                            .foregroundStyle(VPTheme.textPrimary)
                        Text(
                            "\(identity.role) · \(identity.status) · gen \(identity.generation) · \(identity.id)"
                        )
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                        .lineLimit(1)
                    }
                    Spacer()
                }
                .padding(9)
                .background(VPTheme.danger.opacity(0.04), in: RoundedRectangle(cornerRadius: 9))
                .overlay {
                    RoundedRectangle(cornerRadius: 9)
                        .strokeBorder(VPTheme.danger.opacity(0.15), lineWidth: 1)
                }
            }
            ProjectOrgNoticeBanner(
                icon: "exclamationmark.octagon",
                color: VPTheme.danger,
                title: "Terminal operation",
                detail: L10n.t(model.projectOrgRetireDraft.warning)
            )
            if let detail, !detail.retirementCandidates.isEmpty {
                VStack(alignment: .leading, spacing: 4) {
                    ProjectOrgFieldLabel(
                        text: "Retirement evidence",
                        detail: "Deterministic advisory evidence"
                    )
                    ForEach(detail.retirementCandidates) { candidate in
                        Text("severity \(candidate.severity) · \(candidate.reasonSummary)")
                            .font(VPTheme.mono(7.5))
                            .foregroundStyle(VPTheme.warning)
                    }
                }
                .padding(9)
                .background(VPTheme.warning.opacity(0.04), in: RoundedRectangle(cornerRadius: 8))
            }
            if !attachableProposals.isEmpty {
                VStack(alignment: .leading, spacing: 4) {
                    ProjectOrgFieldLabel(
                        text: "Attach proposal",
                        detail: "An approved proposal satisfies the approval requirement"
                    )
                    Picker("Attach proposal", selection: $model.projectOrgRetireDraft.proposalID) {
                        Text("No proposal").tag("")
                        ForEach(attachableProposals) { proposal in
                            Text("\(proposal.id) · \(proposal.status)").tag(proposal.id)
                        }
                    }
                    .labelsHidden()
                    .pickerStyle(.menu)
                }
            }
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 4) {
                    ProjectOrgFieldLabel(
                        text: "Successor",
                        detail: "Same project, non-retired, same or subordinate role"
                    )
                    Picker("Successor", selection: $model.projectOrgRetireDraft.successorID) {
                        Text("No successor").tag("")
                        ForEach(successorChoices) { candidate in
                            Text("\(candidate.displayName) · \(candidate.role)").tag(candidate.id)
                        }
                    }
                    .labelsHidden()
                    .pickerStyle(.menu)
                }
                VStack(alignment: .leading, spacing: 4) {
                    ProjectOrgFieldLabel(
                        text: "Force release",
                        detail: "Release assignments and archive workspaces"
                    )
                    Toggle("Release active work", isOn: $model.projectOrgRetireDraft.force)
                        .toggleStyle(.switch)
                        .controlSize(.small)
                }
            }
            VStack(alignment: .leading, spacing: 4) {
                ProjectOrgFieldLabel(text: "Reason", detail: "Bounded audit label, not secret content")
                TextField("e.g. Performance review completed", text: $model.projectOrgRetireDraft.reason)
                    .textFieldStyle(.plain)
                    .vpFrostedField()
            }
            if requiresApproval {
                VStack(alignment: .leading, spacing: 5) {
                    Toggle(
                        "I explicitly approve this retirement",
                        isOn: $model.projectOrgRetireDraft.approved
                    )
                    .toggleStyle(.switch)
                    .controlSize(.small)
                    .disabled(!model.projectOrgRetireDraft.proposalID.isEmpty)
                    Text(
                        model.projectOrgRetireDraft.proposalID.isEmpty
                            ? "Approval is required by policy. The daemon rejects retirement without an explicit approved decision or an approved proposal; a pending proposal is not enough."
                            : "An attached proposal still needs to be approved before it satisfies the retirement gate. The daemon revalidates."
                    )
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.warning)
                    .fixedSize(horizontal: false, vertical: true)
                }
                .padding(9)
                .background(VPTheme.danger.opacity(0.04), in: RoundedRectangle(cornerRadius: 8))
            } else {
                Text(
                    "Retirement approval is disabled by project policy; the explicit submit is still required."
                )
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
            }
            if let error = model.projectOrgRetireDraft.validationError() {
                Text(L10n.t(error))
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.danger)
            }
            footer
        }
        .padding(20)
        .frame(width: 600)
        .background { VPCrystalBackdrop() }
        .tint(VPTheme.accent)
        .interactiveDismissDisabled(model.projectOrgActionBusy)
    }

    private var footer: some View {
        HStack {
            Button("Cancel") {
                model.dismissProjectOrgSheet()
                dismiss()
            }
            .keyboardShortcut(.cancelAction)
            .disabled(model.projectOrgActionBusy)
            Spacer()
            Button {
                Task { await model.submitProjectOrgRetire() }
            } label: {
                Label(
                    model.projectOrgActionBusy ? "Retiring…" : "Retire identity",
                    systemImage: "person.crop.circle.badge.minus"
                )
            }
            .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.danger))
            .keyboardShortcut(.defaultAction)
            .disabled(!canSubmit)
        }
    }
}
