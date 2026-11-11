import SwiftUI

/// Create a bounded auto-approval template from one reviewed approval. The
/// daemon refuses high-risk, remote, opaque host, and sensitive GUI approvals;
/// this sheet only collects the bounded scope inputs.
struct ApprovalTemplateSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let approval: Approval

    @State private var name = ""
    @State private var maxUses = 10
    @State private var expires = false
    @State private var expiresAt = Date().addingTimeInterval(7 * 24 * 60 * 60)

    private var riskLabel: String {
        approval.approvalRisk ?? "unknown"
    }

    private var canSave: Bool {
        !model.approvalTemplateBusy
            && maxUses > 0
            && maxUses <= 100_000
            && !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Approval template")
                    Text("Save as bounded template")
                        .font(.system(size: 15, weight: .semibold, design: .rounded))
                }
                Spacer()
                Text(riskLabel.uppercased())
                    .font(.system(size: 8, weight: .semibold, design: .monospaced))
                    .foregroundStyle(.orange)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 3)
                    .background(.orange.opacity(0.1), in: Capsule())
            }
            Text(
                "Templates auto-approve only explicitly scoped external or workspace_write calls. "
                    + "Destructive, host-state, remote, and sensitive GUI operations still require you. "
                    + "Deny rules always win."
            )
            .font(.system(size: 9, design: .monospaced))
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)

            VStack(alignment: .leading, spacing: 8) {
                LabeledContent("Tool") {
                    Text(approval.tool)
                        .font(.system(size: 10, design: .monospaced))
                        .textSelection(.enabled)
                }
                LabeledContent("Risk") {
                    Text(riskLabel)
                        .font(.system(size: 10, design: .monospaced))
                }
                if let taskId = approval.taskId {
                    LabeledContent("Task") {
                        Text(taskId)
                            .font(.system(size: 10, design: .monospaced))
                    }
                }
                LabeledContent("Scope") {
                    Text(previewScope(approval))
                        .font(.system(size: 9, design: .monospaced))
                        .textSelection(.enabled)
                        .lineLimit(3)
                }
                TextField("Template name", text: $name)
                    .textFieldStyle(.roundedBorder)
                Stepper("Max uses: \(maxUses)", value: $maxUses, in: 1 ... 100_000)
                Toggle("Expires", isOn: $expires)
                if expires {
                    DatePicker(
                        "Expiry",
                        selection: $expiresAt,
                        in: Date().addingTimeInterval(60)...,
                        displayedComponents: [.date, .hourAndMinute]
                    )
                }
            }

            if let status = model.approvalTemplateStatus, !status.isEmpty {
                Text(status)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(
                        status.lowercased().contains("created") ? Color.green : Color.orange
                    )
                    .fixedSize(horizontal: false, vertical: true)
            }

            HStack {
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button("Create template") {
                    Task {
                        await model.createApprovalTemplateFromApproval(
                            approval,
                            name: name.trimmingCharacters(in: .whitespacesAndNewlines),
                            maxUses: maxUses,
                            expiresAt: expires ? ISO8601DateFormatter().string(from: expiresAt) : nil
                        )
                        if model.approvalTemplateStatus?.hasPrefix("Created") == true {
                            dismiss()
                        }
                    }
                }
                .buttonStyle(.borderedProminent)
                .disabled(!canSave)
            }
        }
        .padding(20)
        .frame(width: 460)
        .onAppear {
            if name.isEmpty {
                name = "\(approval.tool) from approval"
            }
        }
    }

    private func previewScope(_ approval: Approval) -> String {
        let object = approval.input.objectValue
        if let path = object?["path"]?.stringValue {
            return path
        }
        if let paths = object?["paths"]?.arrayValue {
            return paths.compactMap(\.stringValue).joined(separator: ", ")
        }
        return "tool-wide (no path glob)"
    }
}

/// Project-scoped attention policy and template management. This is the
/// deterministic configuration surface backing the daemon RPCs; the approval
/// inbox itself never decides policy implicitly.
struct ApprovalOptimizerCard: View {
    @Environment(AppModel.self) private var model
    let project: Project

    @State private var policy = AttentionPolicy()

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Approval attention")
                    Text(project.name)
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.attentionPolicyBusy || model.approvalTemplateBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Refresh") {
                    Task { await reload() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
            }

            VStack(alignment: .leading, spacing: 8) {
                Toggle("Attention policy enabled", isOn: $policy.enabled)
                HStack(spacing: 12) {
                    Stepper(
                        "Max pending: \(policy.maxPendingPerSession)",
                        value: $policy.maxPendingPerSession,
                        in: 1 ... 500
                    )
                    Stepper(
                        "Coalesce window: \(policy.coalesceWindowSecs)s",
                        value: $policy.coalesceWindowSecs,
                        in: 0 ... 3600,
                        step: 30
                    )
                }
                HStack(spacing: 12) {
                    Stepper(
                        "Stale after: \(policy.staleAfterSecs)s",
                        value: $policy.staleAfterSecs,
                        in: 60 ... 604_800,
                        step: 60
                    )
                    Stepper(
                        "Notify window: \(policy.notifyCoalesceSecs)s",
                        value: $policy.notifyCoalesceSecs,
                        in: 0 ... 3600,
                        step: 15
                    )
                }
                Picker("Priority boost risk", selection: $policy.priorityBoostRisk) {
                    ForEach(["workspace_write", "external", "host_state", "destructive"], id: \.self) {
                        Text($0).tag($0)
                    }
                }
                .pickerStyle(.menu)
                Text(
                    "The boost risk gets a longer stale window and bypasses notification coalescing. "
                        + "Stale approvals expire (deny) and are never auto-approved. Identical pending requests "
                        + "coalesce into one row; one decision releases every current waiter."
                )
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
                .fixedSize(horizontal: false, vertical: true)
                HStack {
                    Button("Save policy") {
                        Task {
                            await model.saveAttentionPolicy(policy, projectID: project.id)
                            await reload()
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                    .disabled(model.attentionPolicyBusy)
                    if let status = model.attentionPolicyStatus, !status.isEmpty {
                        Text(status)
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.secondary)
                    }
                }
            }

            Divider().opacity(0.4)

            HStack {
                Text("Templates")
                    .font(.system(size: 10, weight: .semibold, design: .rounded))
                Spacer()
                Text("\(model.approvalTemplates.count)")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
            if model.approvalTemplates.isEmpty {
                Text(model.approvalTemplateStatus ?? "No approval templates.")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
            } else {
                ForEach(model.approvalTemplates) { template in
                    templateRow(template)
                }
            }
        }
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .task(id: project.id) {
            await reload()
        }
    }

    private func templateRow(_ template: ApprovalTemplate) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Text(template.name)
                    .font(.system(size: 9, weight: .medium))
                Text(template.action.uppercased())
                    .font(.system(size: 7, weight: .semibold, design: .monospaced))
                    .foregroundStyle(template.action == "deny" ? .red : .green)
                    .padding(.horizontal, 5)
                    .padding(.vertical, 1)
                    .background(
                        (template.action == "deny" ? Color.red : Color.green).opacity(0.1),
                        in: Capsule()
                    )
                Text(template.toolPattern)
                    .font(.system(size: 7.5, design: .monospaced))
                    .foregroundStyle(.secondary)
                Spacer()
                if !template.enabled {
                    Text("DISABLED")
                        .font(.system(size: 7, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
                if template.isExhausted {
                    Text("EXHAUSTED")
                        .font(.system(size: 7, design: .monospaced))
                        .foregroundStyle(.orange)
                }
                Button {
                    Task { await model.deleteApprovalTemplate(template) }
                } label: {
                    Image(systemName: "trash")
                        .font(.system(size: 8))
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .disabled(model.approvalTemplateBusy)
            }
            HStack(spacing: 8) {
                Text(template.riskLevels.joined(separator: ","))
                if !template.pathGlobs.isEmpty {
                    Text(template.pathGlobs.joined(separator: ", "))
                }
                Text(
                    template.remainingUses.map { "\($0) uses left" }
                        ?? "unlimited uses"
                )
                if let expiresAt = template.expiresAt {
                    Text("expires \(expiresAt.prefix(10))")
                }
            }
            .font(.system(size: 7.5, design: .monospaced))
            .foregroundStyle(.tertiary)
            .lineLimit(2)
        }
        .padding(8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 8))
    }

    private func reload() async {
        await model.loadAttentionPolicy(projectID: project.id)
        await model.loadApprovalTemplates(projectID: project.id)
        if let loadedPolicy = model.attentionPolicy {
            policy = loadedPolicy
        }
    }
}
