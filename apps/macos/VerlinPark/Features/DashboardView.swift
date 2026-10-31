import SwiftUI

struct DashboardView: View {
    @Environment(AppModel.self) private var model
    @Environment(BrandStore.self) private var brand
    @Environment(MemoryMonitor.self) private var memory
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 3) {
                    Text("Manage Dash")
                        .font(.system(size: 24, weight: .semibold, design: .rounded))
                    Text("Agents, computer lease, cost, and local state")
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button("Done") { dismiss() }
                    .keyboardShortcut(.defaultAction)
            }
            .padding(22)

            Divider().opacity(0.35)

            ScrollView {
                LazyVGrid(
                    columns: [
                        GridItem(.flexible(), spacing: 12),
                        GridItem(.flexible(), spacing: 12),
                    ],
                    spacing: 12
                ) {
                    metric(
                        "Workspace",
                        value: "\(model.projects.count) projects",
                        detail: "\(model.sessions.count) sessions · \(model.events.count) events in view"
                    )
                    metric(
                        "Agents",
                        value: "\(model.agents.count) known",
                        detail: "Defaults 4 / 6 / 12 · caps 5 / 10 / 40"
                    )
                    metric(
                        "Computer",
                        value: "not wired",
                        detail: "GUI/browser lease is design-only · shell/worktrees run in parallel"
                    )
                    metric(
                        "Memory",
                        value: memory.text,
                        detail: "app + daemon resident"
                    )
                    metric(
                        "Resident",
                        value: "Aviiya",
                        detail: "external · NovaBAIC bridge reserved · no local runtime"
                    )
                    metric(
                        "Brand",
                        value: brand.config.effectiveName,
                        detail: "\(brand.config.style.title) mark"
                    )
                }
                .padding(18)

                ApprovalCenterCard()
                    .padding(.horizontal, 18)
                    .padding(.bottom, 12)

                if let project = model.selectedProject {
                    ShellPolicyCard(project: project)
                        .padding(.horizontal, 18)
                        .padding(.bottom, 12)
                    IntegrationPolicyCard(project: project)
                        .padding(.horizontal, 18)
                        .padding(.bottom, 12)
                    ProjectBudgetCard(project: project)
                        .padding(.horizontal, 18)
                        .padding(.bottom, 12)
                    ProjectUsageCard(project: project)
                        .padding(.horizontal, 18)
                        .padding(.bottom, 12)
                    DoctorCard()
                        .padding(.horizontal, 18)
                        .padding(.bottom, 12)
                    WorktreeCleanupCard(project: project)
                        .padding(.horizontal, 18)
                        .padding(.bottom, 12)
                    ArtifactPolicyCard(project: project)
                        .padding(.horizontal, 18)
                        .padding(.bottom, 12)
                    MailboxPolicyCard(project: project)
                        .padding(.horizontal, 18)
                        .padding(.bottom, 12)
                } else {
                    VStack(alignment: .leading, spacing: 6) {
                        SectionLabel(title: "Shell policy")
                        Text("Select a project to configure its shell permission policy.")
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(14)
                    .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14))
                    .padding(.horizontal, 18)
                    .padding(.bottom, 12)
                }

                VStack(alignment: .leading, spacing: 8) {
                    SectionLabel(title: "Next runtime")
                    HStack(spacing: 10) {
                        Image(systemName: "hammer")
                            .foregroundStyle(.green)
                        VStack(alignment: .leading, spacing: 2) {
                            Text("Next · Merge queue + Integrator")
                                .font(.system(size: 12, weight: .medium))
                            Text(
                                "Approved branches merge serially today; queue, retry, and post-merge checks are next."
                            )
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundStyle(.secondary)
                        }
                        Spacer()
                    }
                    .padding(12)
                    .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 12))
                }
                .padding(.horizontal, 18)
                .padding(.bottom, 18)
            }
        }
        .frame(width: 760, height: 600)
        .background(.ultraThinMaterial)
    }

    private func metric(_ title: String, value: String, detail: String) -> some View {
        VStack(alignment: .leading, spacing: 7) {
            SectionLabel(title: title)
            Text(value)
                .font(.system(size: 20, weight: .semibold, design: .rounded))
            Text(detail)
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(.secondary)
                .lineLimit(2)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
    }
}

private struct ShellPolicyCard: View {
    @Environment(AppModel.self) private var model
    let project: Project

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Shell policy")
                    Text(project.name)
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.shellPolicyBusy {
                    ProgressView().controlSize(.small)
                }
                Picker("", selection: $model.shellPolicyNetwork) {
                    Text("Allow network").tag("allow")
                    Text("Deny network").tag("deny")
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(width: 210)
            }

            Text(
                "Writes remain confined to the worktree, Git metadata, and runtime scratch. Network deny blocks outbound sockets. Read-deny paths are additive and must be absolute or start with ~/."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)

            TextEditor(text: $model.shellPolicyReadDenyText)
                .font(.system(size: 10, design: .monospaced))
                .scrollContentBackground(.hidden)
                .padding(6)
                .frame(height: 74)
                .background(.black.opacity(0.16), in: RoundedRectangle(cornerRadius: 8))
                .overlay {
                    RoundedRectangle(cornerRadius: 8)
                        .strokeBorder(.white.opacity(0.08), lineWidth: 1)
                }

            HStack(spacing: 8) {
                Text("One read-deny path per line · empty means no extra denials")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
                Spacer()
                Button("Reset") {
                    Task { await model.resetShellPolicy() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.shellPolicyBusy)
                Button("Save policy") {
                    Task { await model.saveShellPolicy() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.shellPolicyBusy)
            }

            if let status = model.shellPolicyStatus {
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
        .task(id: project.id) {
            await model.loadShellPolicy()
        }
    }
}

private struct IntegrationPolicyCard: View {
    @Environment(AppModel.self) private var model
    let project: Project

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Integration policy")
                    Text(project.name)
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.integrationPolicyBusy {
                    ProgressView().controlSize(.small)
                }
                Toggle("Auto-resolve conflicts", isOn: $model.integrationAutoResolve)
                    .toggleStyle(.switch)
                    .controlSize(.small)
            }

            Text(
                "When enabled, a conflict re-enters an internal resolver + Verifier without per-tool approvals. The shell sandbox still applies. Attempts and tokens are capped; manual Auto-resolve remains available."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)

            HStack(spacing: 14) {
                HStack(spacing: 6) {
                    Text("Max automatic attempts")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.secondary)
                    Stepper(
                        "\(model.integrationMaxAutoAttempts)",
                        value: $model.integrationMaxAutoAttempts,
                        in: 0 ... 5
                    )
                    .font(.system(size: 9, design: .monospaced))
                }
                HStack(spacing: 6) {
                    Text("Max tokens / resolution")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.secondary)
                    TextField("session budget", text: $model.integrationMaxTokensText)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 120)
                        .font(.system(size: 9, design: .monospaced))
                }
                Spacer()
                Button("Reset") {
                    Task { await model.resetIntegrationPolicy() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.integrationPolicyBusy)
                Button("Save policy") {
                    Task { await model.saveIntegrationPolicy() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.integrationPolicyBusy)
            }

            if let status = model.integrationPolicyStatus {
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
        .task(id: project.id) {
            await model.loadIntegrationPolicy()
        }
    }
}

private struct ArtifactPolicyCard: View {
    @Environment(AppModel.self) private var model
    let project: Project

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Artifact policy")
                    Text(project.name)
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.artifactPolicyBusy {
                    ProgressView().controlSize(.small)
                }
                Toggle("Auto GC", isOn: $model.artifactAutoGc)
                    .toggleStyle(.switch)
                    .controlSize(.small)
            }

            Text(
                "Retention uses the oldest artifacts first. Binary artifacts are copied into the data directory; text artifacts stay inline in SQLite. Limits are optional."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)

            HStack(spacing: 10) {
                limitField("Max bytes", text: $model.artifactMaxBytesText)
                limitField("Max age secs", text: $model.artifactMaxAgeText)
                limitField("Max count", text: $model.artifactMaxCountText)
                Spacer()
                Button("GC now") {
                    Task { await model.gcArtifacts() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.artifactPolicyBusy)
                Button("Reset") {
                    Task { await model.resetArtifactPolicy() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.artifactPolicyBusy)
                Button("Save policy") {
                    Task { await model.saveArtifactPolicy() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.artifactPolicyBusy)
            }

            if let status = model.artifactPolicyStatus {
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
        .task(id: project.id) {
            await model.loadArtifactPolicy()
        }
    }

    private func limitField(_ title: String, text: Binding<String>) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title)
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
            TextField("unlimited", text: text)
                .textFieldStyle(.roundedBorder)
                .font(.system(size: 9, design: .monospaced))
                .frame(width: 90)
        }
    }
}

private struct MailboxPolicyCard: View {
    @Environment(AppModel.self) private var model
    let project: Project

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Mailbox retention")
                    Text(project.name)
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.mailboxPolicyBusy {
                    ProgressView().controlSize(.small)
                }
                Toggle("Auto GC", isOn: $model.mailboxAutoGc)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                Toggle("Delete unread", isOn: $model.mailboxDeleteUnread)
                    .toggleStyle(.switch)
                    .controlSize(.small)
            }

            Text(
                "Mailbox GC removes oldest messages first. Auto GC runs after sending or marking a message read. Unread messages are protected unless Delete unread is enabled. Limits are optional."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)

            HStack(spacing: 10) {
                limitField("Max age secs", text: $model.mailboxMaxAgeText)
                limitField("Max count", text: $model.mailboxMaxCountText)
                Spacer()
                Button("GC now") {
                    Task { await model.gcMailbox() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.mailboxPolicyBusy)
                Button("Reset") {
                    Task { await model.resetMailboxPolicy() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.mailboxPolicyBusy)
                Button("Save policy") {
                    Task { await model.saveMailboxPolicy() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.mailboxPolicyBusy)
            }

            if let status = model.mailboxPolicyStatus {
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
        .task(id: project.id) {
            await model.loadMailboxPolicy()
        }
    }

    private func limitField(_ title: String, text: Binding<String>) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title)
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
            TextField("unlimited", text: text)
                .textFieldStyle(.roundedBorder)
                .font(.system(size: 9, design: .monospaced))
                .frame(width: 90)
        }
    }
}

private struct ProjectBudgetCard: View {
    @Environment(AppModel.self) private var model
    let project: Project

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Project shared budget")
                    Text(project.name)
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.projectBudgetBusy {
                    ProgressView().controlSize(.small)
                }
            }
            Text(
                "A project-wide token/USD pool shared by all sessions. Concurrent calls reserve against it atomically. Empty means unlimited."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 3) {
                    Text("Token budget")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    TextField("unlimited", text: $model.projectTokenBudgetText)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 9, design: .monospaced))
                        .frame(width: 110)
                }
                VStack(alignment: .leading, spacing: 3) {
                    Text("USD budget")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    TextField("unlimited", text: $model.projectCostBudgetText)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 9, design: .monospaced))
                        .frame(width: 90)
                }
                Spacer()
                Button("Reset") {
                    Task { await model.resetProjectBudget() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.projectBudgetBusy)
                Button("Save budget") {
                    Task { await model.saveProjectBudget() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.projectBudgetBusy)
            }
            Text(
                "Spent: \(model.projectSpentTokens) tokens · \(String(format: "$%.4f", model.projectSpentCostUsd))"
            )
            .font(.system(size: 9, design: .monospaced))
            .foregroundStyle(.secondary)
            if let status = model.projectBudgetStatus {
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
        .task(id: project.id) {
            await model.loadProjectBudget()
        }
    }
}

private struct DoctorCard: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Diagnostics")
                    Text("Local doctor")
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.doctorBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Run diagnostics") {
                    Task { await model.runDoctor() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.doctorBusy)
            }
            Text(
                "Checks SQLite integrity, data permissions, project roots, worktrees, event mirrors, sandbox availability, and provider configuration."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)
            if let report = model.doctorReport {
                Text(report.ok ? "All checks passed" : "Warnings or errors found")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(report.ok ? Color.green : Color.orange)
                ForEach(report.checks) { check in
                    HStack(alignment: .top, spacing: 8) {
                        Text(check.status.uppercased())
                            .font(.system(size: 8, weight: .medium, design: .monospaced))
                            .foregroundStyle(color(for: check.status))
                            .frame(width: 44, alignment: .leading)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(check.name)
                                .font(.system(size: 9, weight: .medium))
                            Text(check.detail)
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(.secondary)
                                .textSelection(.enabled)
                        }
                    }
                }
            } else if let status = model.doctorStatus {
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
    }

    private func color(for status: String) -> Color {
        switch status {
        case "ok": .green
        case "warn": .orange
        default: .red
        }
    }
}

private struct WorktreeCleanupCard: View {
    @Environment(AppModel.self) private var model
    let project: Project

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Worktree cleanup")
                    Text(project.name)
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.cleanupBusy {
                    ProgressView().controlSize(.small)
                }
            }
            Text(
                "Terminal-session worktrees only. Dirty worktrees are skipped; branches are kept unless explicitly deleted."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)

            Divider().opacity(0.2)
            Text("AUTOMATIC RETENTION")
                .font(.system(size: 8, weight: .semibold, design: .monospaced))
                .foregroundStyle(.tertiary)
            HStack(spacing: 10) {
                Toggle("Enabled", isOn: $model.worktreePolicyEnabled)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                VStack(alignment: .leading, spacing: 3) {
                    Text("Older than days")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    TextField("7", text: $model.worktreePolicyOlderDaysText)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 9, design: .monospaced))
                        .frame(width: 70)
                }
                Toggle("Delete branches", isOn: $model.worktreePolicyDeleteBranches)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                Toggle("Keep failed", isOn: $model.worktreePolicyKeepFailed)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                Spacer()
                if model.worktreePolicyBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Save policy") {
                    Task { await model.saveWorktreePolicy() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.worktreePolicyBusy)
            }
            if let status = model.worktreePolicyStatus {
                Text(status)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            }

            Divider().opacity(0.2)
            Text("MANUAL CLEANUP")
                .font(.system(size: 8, weight: .semibold, design: .monospaced))
                .foregroundStyle(.tertiary)
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 3) {
                    Text("Older than days")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    TextField("7", text: $model.cleanupOlderDaysText)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 9, design: .monospaced))
                        .frame(width: 70)
                }
                Toggle("Dry run", isOn: $model.cleanupDryRun)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                Toggle("Delete branches", isOn: $model.cleanupDeleteBranches)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                Toggle("Keep failed", isOn: $model.cleanupKeepFailed)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                Spacer()
                Button("Run cleanup") {
                    Task { await model.runWorktreeCleanup() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.cleanupBusy)
            }
            if let status = model.cleanupStatus {
                Text(status)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
            if let result = model.cleanupResult {
                ForEach(result.entries) { entry in
                    HStack(alignment: .top, spacing: 6) {
                        Text(entry.removed ? "removed" : "skipped")
                            .font(.system(size: 8, weight: .medium, design: .monospaced))
                            .foregroundStyle(entry.removed ? Color.green : Color.secondary)
                            .frame(width: 52, alignment: .leading)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(entry.sessionId)
                                .font(.system(size: 8, design: .monospaced))
                            Text("\(entry.branch) · \(entry.reason)")
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(.secondary)
                                .lineLimit(2)
                        }
                    }
                }
            }
        }
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
    }
}

private struct ProjectUsageCard: View {
    @Environment(AppModel.self) private var model
    let project: Project

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Usage by session")
                    Text(project.name)
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.projectUsageBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Refresh") {
                    Task { await model.loadProjectUsage() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.projectUsageBusy)
            }
            if let usage = model.projectUsage {
                HStack(spacing: 14) {
                    usageMetric("Calls", "\(usage.totals.calls)")
                    usageMetric(
                        "Tokens",
                        "\(usage.totals.inputTokens + usage.totals.outputTokens)"
                    )
                    usageMetric("Cost", String(format: "$%.4f", usage.totals.costUsd))
                    Spacer()
                    Text("\(usage.sessions.count) sessions")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
                if usage.sessions.isEmpty {
                    Text("No sessions yet.")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(Array(usage.sessions.prefix(8))) { session in
                        HStack(alignment: .top, spacing: 8) {
                            Text(session.status.uppercased())
                                .font(.system(size: 8, weight: .medium, design: .monospaced))
                                .foregroundStyle(color(for: session.status))
                                .frame(width: 62, alignment: .leading)
                            VStack(alignment: .leading, spacing: 1) {
                                Text(session.goal)
                                    .font(.system(size: 9, weight: .medium))
                                    .lineLimit(1)
                                Text(
                                    "calls \(session.calls) · tokens \(session.totalTokens) · $"
                                        + String(format: "%.4f", session.costUsd)
                                )
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(.tertiary)
                            }
                            Spacer(minLength: 0)
                        }
                    }
                    if usage.sessions.count > 8 {
                        Text(
                            "… and \(usage.sessions.count - 8) more sessions. Use `vp project-usage` for the full list."
                        )
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    }
                }
            } else if let status = model.projectUsageStatus {
                Text(status)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            } else {
                ProgressView().controlSize(.small)
            }
        }
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
    }

    private func usageMetric(_ title: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(title)
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
            Text(value)
                .font(.system(size: 12, weight: .semibold, design: .rounded))
                .monospacedDigit()
        }
    }

    private func color(for status: String) -> Color {
        switch status {
        case "idle", "completed": .green
        case "running", "queued", "paused": .orange
        case "failed", "cancelled": .red
        default: .secondary
        }
    }
}

private struct ApprovalCenterCard: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Approvals")
                    Text("Pending across projects")
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.approvalCenterBusy {
                    ProgressView().controlSize(.small)
                }
                Text("\(model.pendingApprovals.count) pending")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
                Button("Refresh") {
                    Task { await model.loadPendingApprovals() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.approvalCenterBusy)
            }
            if model.pendingApprovals.isEmpty {
                Text(model.approvalCenterStatus ?? "No pending approvals.")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            } else {
                ForEach(model.pendingApprovals) { approval in
                    VStack(alignment: .leading, spacing: 5) {
                        HStack(spacing: 6) {
                            Text(approval.tool.uppercased())
                                .font(.system(size: 8, weight: .semibold, design: .monospaced))
                                .foregroundStyle(.orange)
                                .padding(.horizontal, 5)
                                .padding(.vertical, 2)
                                .background(.orange.opacity(0.1), in: Capsule())
                            Text(approval.agentName ?? approval.agentRole ?? "agent")
                                .font(.system(size: 9, weight: .medium))
                            if let title = approval.agentTitle {
                                Text("· \(title)")
                                    .font(.system(size: 8, design: .monospaced))
                                    .foregroundStyle(.tertiary)
                            }
                            Spacer()
                            Text(
                                model.approvalSessionGoals[approval.sessionId]
                                    ?? String(approval.sessionId.prefix(8))
                            )
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.tertiary)
                            .lineLimit(1)
                        }
                        Text(inputSummary(approval))
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.secondary)
                            .lineLimit(3)
                            .textSelection(.enabled)
                        HStack(spacing: 6) {
                            Button("Approve") {
                                Task {
                                    await model.resolveGlobalApproval(approval.id, approved: true)
                                }
                            }
                            .buttonStyle(.borderedProminent)
                            .controlSize(.small)
                            .tint(.green)
                            .disabled(model.approvalCenterBusy)
                            Button("Deny") {
                                Task {
                                    await model.resolveGlobalApproval(approval.id, approved: false)
                                }
                            }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                            .disabled(model.approvalCenterBusy)
                        }
                    }
                    .padding(9)
                    .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 9))
                }
            }
        }
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .task {
            await model.loadPendingApprovals()
        }
    }

    private func inputSummary(_ approval: Approval) -> String {
        let object = approval.input.objectValue
        if let command = object?["command"]?.stringValue {
            return command
        }
        if let path = object?["path"]?.stringValue {
            return path
        }
        if let url = object?["url"]?.stringValue {
            return url
        }
        return approval.tool
    }
}
