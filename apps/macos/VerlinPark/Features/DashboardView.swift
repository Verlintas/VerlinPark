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
                        "Scheduler",
                        value: "\(model.taskQueue?.tasks.count ?? 0) tasks",
                        detail: "\(runningTasks) running · \(model.taskQueue?.leases.count ?? 0) write leases"
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

                QuestionCenterCard()
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
                    MemoryCard(project: project)
                        .padding(.horizontal, 18)
                        .padding(.bottom, 12)
                    MemoryPolicyCard(project: project)
                        .padding(.horizontal, 18)
                        .padding(.bottom, 12)
                    TaskSchedulerCard()
                        .padding(.horizontal, 18)
                        .padding(.bottom, 12)
                    DoctorCard()
                        .padding(.horizontal, 18)
                        .padding(.bottom, 12)
                    WorktreeCleanupCard(project: project)
                        .padding(.horizontal, 18)
                        .padding(.bottom, 12)
                    CheckpointsCard()
                        .padding(.horizontal, 18)
                        .padding(.bottom, 12)
                    ForksCard()
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
                            Text("Next · ACP and read isolation")
                                .font(.system(size: 12, weight: .medium))
                            Text(
                                "The persistent global task-DAG queue and write leases are live. Next: an ACP external-agent bridge, full filesystem read confinement, and updater/tap upgrades."
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

    private var runningTasks: Int {
        model.taskQueue?.tasks.count(where: { $0.status == "running" }) ?? 0
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

private struct MemoryRow: View {
    @Environment(AppModel.self) private var model
    let memory: MemoryItem
    @State private var confirmDelete = false

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 6) {
                Text(memory.kind.uppercased())
                    .font(.system(size: 8, weight: .medium, design: .monospaced))
                    .foregroundStyle(.cyan)
                    .padding(.horizontal, 5)
                    .padding(.vertical, 1)
                    .background(.cyan.opacity(0.1), in: Capsule())
                Text(memory.title)
                    .font(.system(size: 9, weight: .medium))
                Spacer()
                Text(memory.agentName)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
                Button {
                    confirmDelete = true
                } label: {
                    Image(systemName: "trash")
                        .font(.system(size: 8))
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .help("Delete memory")
                .disabled(model.memoryBusy)
            }
            Text(memory.body)
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.secondary)
                .lineLimit(3)
            if !memory.tags.isEmpty {
                Text(memory.tags.map { "#\($0)" }.joined(separator: " "))
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 8))
        .confirmationDialog(
            "Delete this memory?",
            isPresented: $confirmDelete,
            titleVisibility: .visible
        ) {
            Button("Delete", role: .destructive) {
                Task { await model.deleteMemory(memory) }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(memory.title)
        }
    }
}

private struct MemoryPolicyCard: View {
    @Environment(AppModel.self) private var model
    let project: Project

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Memory retention")
                    Text(project.name)
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.memoryPolicyBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Save") {
                    Task { await model.saveMemoryPolicy() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.memoryPolicyBusy)
            }
            Toggle("Automatic prune", isOn: $model.memoryPolicyEnabled)
                .toggleStyle(.switch)
                .controlSize(.small)
            HStack(spacing: 8) {
                TextField("max items", text: $model.memoryPolicyMaxItems)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 9, design: .monospaced))
                    .frame(width: 110)
                TextField("max age days", text: $model.memoryPolicyMaxAgeDays)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 9, design: .monospaced))
                    .frame(width: 110)
                Text("leave blank to disable a limit")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
            Text(
                "Off by default. The daemon sweeps enabled policies periodically (default hourly) and emits event-backed deletions. At least one limit is required to enable."
            )
            .font(.system(size: 9, design: .monospaced))
            .foregroundStyle(.secondary)
            if let status = model.memoryPolicyStatus {
                Text(status)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .task {
            await model.loadMemoryPolicy()
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

            HStack(spacing: 16) {
                Toggle("Restrict home reads", isOn: $model.shellPolicyRestrictHome)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                    .help(
                        "Deny reads under HOME except the worktree, scratch, Git metadata, and known toolchain/cache paths."
                    )
                Toggle("Read jail (all users + volumes)", isOn: $model.shellPolicyReadJail)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                    .help(
                        "Deny reads under the home parent (normally /Users) and /Volumes except the worktree, scratch, Git metadata, toolchains, and read-deny exceptions. System paths remain readable."
                    )
            }

            Text(
                "Writes remain confined to the worktree, Git metadata, and runtime scratch. Network deny blocks outbound sockets. Restrict home reads denies HOME; Read jail also denies sibling user homes and /Volumes. Both allow the worktree, scratch, Git metadata, and known toolchains. Read-deny paths are additive and must be absolute or start with ~/. This is not a full filesystem read sandbox."
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

private struct CheckpointsCard: View {
    @Environment(AppModel.self) private var model
    @State private var deleting: SessionCheckpoint?

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Session checkpoints")
                    Text(model.selectedSession?.goal ?? "No session selected")
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                        .lineLimit(1)
                }
                Spacer()
                if model.checkpointBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Prune >30d, keep 5") {
                    Task { await model.pruneCheckpoints() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.checkpointBusy || model.selectedSession == nil || model.checkpoints.isEmpty)
            }
            Text(
                "Checkpoints snapshot the active conversation and worktree, including uncommitted tracked/untracked non-ignored changes. Forks already created keep their branch commits."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)

            Divider().opacity(0.2)
            Text("AUTOMATIC RETENTION")
                .font(.system(size: 8, weight: .semibold, design: .monospaced))
                .foregroundStyle(.tertiary)
            HStack(spacing: 10) {
                Toggle("Enabled", isOn: $model.checkpointPolicyEnabled)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                VStack(alignment: .leading, spacing: 3) {
                    Text("Older than days")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    TextField("30", text: $model.checkpointPolicyOlderDaysText)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 9, design: .monospaced))
                        .frame(width: 60)
                }
                VStack(alignment: .leading, spacing: 3) {
                    Text("Keep latest")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    TextField("5", text: $model.checkpointPolicyKeepLatestText)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 9, design: .monospaced))
                        .frame(width: 50)
                }
                Spacer()
                if model.checkpointPolicyBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Save policy") {
                    Task { await model.saveCheckpointPolicy() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.checkpointPolicyBusy)
            }
            if let status = model.checkpointPolicyStatus {
                Text(status)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            }

            Divider().opacity(0.2)
            Text("MANUAL CLEANUP")
                .font(.system(size: 8, weight: .semibold, design: .monospaced))
                .foregroundStyle(.tertiary)

            if model.selectedSession == nil {
                Text("Select a session to inspect its checkpoints.")
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.tertiary)
            } else if model.checkpoints.isEmpty {
                Text("No checkpoints yet. Use Fork Session… to create one.")
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.tertiary)
            } else {
                ForEach(model.checkpoints.suffix(8).reversed()) { checkpoint in
                    HStack(alignment: .top, spacing: 8) {
                        VStack(alignment: .leading, spacing: 2) {
                            HStack(spacing: 6) {
                                Text(checkpoint.label)
                                    .font(.system(size: 10, weight: .medium, design: .monospaced))
                                    .lineLimit(1)
                                if checkpoint.dirty == true {
                                    Text("dirty")
                                        .font(.system(size: 8, weight: .semibold, design: .monospaced))
                                        .foregroundStyle(.orange)
                                }
                            }
                            Text(
                                "\(String(checkpoint.commit.prefix(8))) · \(checkpoint.messageCount) msgs · \(checkpoint.createdAt.prefix(19))"
                            )
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.secondary)
                        }
                        Spacer()
                        Button {
                            deleting = checkpoint
                        } label: {
                            Image(systemName: "trash")
                                .font(.system(size: 10))
                        }
                        .buttonStyle(.plain)
                        .help("Delete checkpoint")
                        .disabled(model.checkpointBusy)
                    }
                }
                if model.checkpoints.count > 8 {
                    Text("Showing the 8 most recent of \(model.checkpoints.count).")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
            }
            if let notice = model.checkpointNotice {
                Text(notice)
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
        .confirmationDialog(
            "Delete checkpoint?",
            isPresented: Binding(
                get: { deleting != nil },
                set: {
                    if !$0 {
                        deleting = nil
                    }
                }
            ),
            presenting: deleting
        ) { checkpoint in
            Button("Delete \(checkpoint.label)", role: .destructive) {
                Task { await model.deleteCheckpoint(checkpoint.id) }
                deleting = nil
            }
            Button("Cancel", role: .cancel) { deleting = nil }
        } message: { _ in
            Text(
                "This removes the checkpoint event and its snapshot ref. Existing fork branches are unaffected."
            )
        }
    }
}

private struct ForksCard: View {
    @Environment(AppModel.self) private var model
    @State private var showingCompare = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Fork lineage")
                    Text(model.selectedSession?.goal ?? "No session selected")
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                        .lineLimit(1)
                }
                Spacer()
                if model.lineageBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Refresh") {
                    Task { await model.loadLineage() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.lineageBusy || model.selectedSession == nil)
            }
            Text(
                "Parent, sibling, and child sessions created through checkpoint forks. Compare committed branch differences without changing either worktree."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)

            if model.selectedSession == nil {
                Text("Select a session to inspect its fork lineage.")
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.tertiary)
            } else if let lineage = model.sessionLineage {
                lineageSection("PARENT", entries: lineage.parent.map { [$0] } ?? [])
                lineageSection("SIBLINGS", entries: lineage.siblings)
                lineageSection("FORKS", entries: lineage.children)
                if lineage.parent == nil, lineage.siblings.isEmpty, lineage.children.isEmpty {
                    Text("No fork lineage. Use Session → Fork Session… to start one.")
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
            } else if !model.lineageBusy {
                Text("Lineage unavailable.")
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
            if let error = model.compareError {
                Text(error)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.orange)
                    .lineLimit(2)
            }
        }
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .sheet(isPresented: $showingCompare) {
            CompareSessionsSheet()
                .environment(model)
        }
    }

    @ViewBuilder
    private func lineageSection(_ title: String, entries: [SessionLineageEntry]) -> some View {
        if !entries.isEmpty {
            Text(title)
                .font(.system(size: 8, weight: .semibold, design: .monospaced))
                .foregroundStyle(.tertiary)
            ForEach(entries) { entry in
                HStack(alignment: .top, spacing: 8) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(entry.session.goal)
                            .font(.system(size: 10, weight: .medium, design: .monospaced))
                            .lineLimit(1)
                        Text(
                            "\(entry.session.status) · \(entry.commit.map { String($0.prefix(8)) } ?? "no commit") · \(entry.checkpointID.map { String($0.prefix(8)) } ?? "no checkpoint")"
                        )
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.secondary)
                    }
                    Spacer()
                    Button("Open") {
                        model.selectSession(entry.session.id)
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    Button("Compare") {
                        Task {
                            await model.compareSessions(otherID: entry.session.id)
                            showingCompare = true
                        }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(model.compareBusy)
                }
            }
        }
    }
}

private struct CompareSessionsSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Compare sessions")
                        .font(.system(size: 18, weight: .semibold, design: .rounded))
                    Text("Committed branch difference")
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button("Done") { dismiss() }
                    .keyboardShortcut(.defaultAction)
            }
            .padding(18)
            Divider().opacity(0.35)
            if model.compareBusy {
                ProgressView()
                    .controlSize(.small)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let error = model.compareError {
                StudioEmptyView(
                    icon: "exclamationmark.triangle",
                    title: "Comparison unavailable",
                    detail: error
                )
            } else if let result = model.compareResult {
                VStack(alignment: .leading, spacing: 10) {
                    HStack(spacing: 16) {
                        Text(
                            "\(String(result.leftCommit.prefix(8))) → \(String(result.rightCommit.prefix(8)))"
                        )
                        .font(.system(size: 10, design: .monospaced))
                        Text("\(result.files.count) files")
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundStyle(.secondary)
                        if result.truncated {
                            Text("patch truncated")
                                .font(.system(size: 9, design: .monospaced))
                                .foregroundStyle(.orange)
                        }
                    }
                    ScrollView(.horizontal) {
                        HStack(spacing: 12) {
                            ForEach(result.files) { file in
                                Text(
                                    "\(file.path) +\(file.binary ? "bin" : "\(file.additions)") / -\(file.binary ? "bin" : "\(file.deletions)")"
                                )
                                .font(.system(size: 9, design: .monospaced))
                                .foregroundStyle(file.binary ? Color.orange : Color.secondary)
                            }
                        }
                    }
                    .frame(maxHeight: 40)
                    Divider().opacity(0.3)
                    if result.patch.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                        StudioEmptyView(
                            icon: "equal.circle",
                            title: "No committed difference",
                            detail: "The session branches point at the same tree."
                        )
                    } else {
                        ScrollView([.vertical, .horizontal]) {
                            Text(result.patch)
                                .font(.system(size: 10, design: .monospaced))
                                .textSelection(.enabled)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(12)
                        }
                        .background(.black.opacity(0.18), in: RoundedRectangle(cornerRadius: 10))
                    }
                }
                .padding(18)
            } else {
                ProgressView()
                    .controlSize(.small)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .frame(width: 820, height: 680)
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

private struct MemoryCard: View {
    @Environment(AppModel.self) private var model
    let project: Project
    @State private var confirmPrune = false

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Project memory")
                    Text(project.name)
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.memoryBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Refresh") {
                    Task { await model.loadMemories() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.memoryBusy)
                Button("Prune") {
                    confirmPrune = true
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.memoryBusy || model.memories.isEmpty)
                .help("Delete memories older than 30 days")
            }
            Text("Durable decisions, facts, conventions, and sources shared across sessions.")
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(.secondary)
            HStack(spacing: 8) {
                TextField("Search memories", text: $model.memoryQuery)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 9, design: .monospaced))
                    .onSubmit {
                        Task { await model.loadMemories() }
                    }
                Button("Search") {
                    Task { await model.loadMemories() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
            }
            if let session = model.selectedSession {
                VStack(alignment: .leading, spacing: 5) {
                    HStack(spacing: 6) {
                        TextField("kind (decision, fact, source)", text: $model.memoryNewKind)
                            .textFieldStyle(.roundedBorder)
                            .font(.system(size: 9, design: .monospaced))
                            .frame(width: 150)
                        TextField("title", text: $model.memoryNewTitle)
                            .textFieldStyle(.roundedBorder)
                            .font(.system(size: 9, design: .monospaced))
                    }
                    TextField("body", text: $model.memoryNewBody, axis: .vertical)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 9, design: .monospaced))
                        .lineLimit(2 ... 4)
                    HStack {
                        Text(session.goal)
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.tertiary)
                            .lineLimit(1)
                        Spacer()
                        Button("Add memory") {
                            Task { await model.createMemory() }
                        }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.small)
                        .disabled(model.memoryBusy)
                    }
                }
            } else {
                Text("Select a session to add a memory.")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
            if model.memories.isEmpty {
                Text(model.memoryStatus ?? "No project memories yet.")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            } else {
                ForEach(Array(model.memories.prefix(10))) { memory in
                    MemoryRow(memory: memory)
                }
                if model.memories.count > 10 {
                    Text("… and \(model.memories.count - 10) more memories.")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
            }
            if let status = model.memoryStatus, !model.memories.isEmpty {
                Text(status)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
        }
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .confirmationDialog(
            "Prune project memory?",
            isPresented: $confirmPrune,
            titleVisibility: .visible
        ) {
            Button("Delete memories older than 30 days", role: .destructive) {
                Task { await model.pruneMemories(olderThanDays: 30) }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Only memories created more than 30 days ago are removed. Recent memories are kept.")
        }
    }
}

private struct QuestionCenterCard: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Questions")
                    Text("Waiting for user input")
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.questionCenterBusy {
                    ProgressView().controlSize(.small)
                }
                Text("\(model.pendingQuestions.count) pending")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
                Button("Refresh") {
                    Task { await model.loadPendingQuestions() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.questionCenterBusy)
            }
            if model.pendingQuestions.isEmpty {
                Text(model.questionCenterStatus ?? "No pending questions.")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            } else {
                ForEach(model.pendingQuestions) { question in
                    PendingQuestionRow(question: question)
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
            await model.loadPendingQuestions()
        }
    }
}

private struct PendingQuestionRow: View {
    @Environment(AppModel.self) private var model
    let question: Question
    @State private var draft = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 6) {
                Text(question.agentName)
                    .font(.system(size: 9, weight: .medium))
                Text(
                    model.sessionDirectory[question.sessionId]?.goal
                        ?? String(question.sessionId.prefix(8))
                )
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
                .lineLimit(1)
                Spacer()
            }
            Text(question.question)
                .font(.system(size: 10))
                .fixedSize(horizontal: false, vertical: true)
            if !question.options.isEmpty {
                HStack(spacing: 6) {
                    ForEach(question.options, id: \.self) { option in
                        Button(option) {
                            Task { await model.answerGlobalQuestion(question.id, answer: option) }
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                    }
                }
            }
            HStack(spacing: 6) {
                TextField("Answer", text: $draft)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 9, design: .monospaced))
                    .onSubmit {
                        submit()
                    }
                Button("Send") {
                    submit()
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(9)
        .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 9))
    }

    private func submit() {
        let answer = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !answer.isEmpty else { return }
        draft = ""
        Task { await model.answerGlobalQuestion(question.id, answer: answer) }
    }
}

private struct TaskSchedulerCard: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Task scheduler")
                    Text("Persistent global task-DAG queue")
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.taskQueueBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Refresh") {
                    Task { await model.loadTaskQueue() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.taskQueueBusy)
            }

            HStack(spacing: 12) {
                Toggle("Auto-resume pending plans", isOn: $model.taskSchedulerEnabled)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                Toggle("One task per tick", isOn: $model.taskSchedulerDispatchTasks)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                    .help(
                        "Dispatch one ready task per daemon tick instead of resuming the entire stored plan"
                    )
                TextField("cooldown secs", text: $model.taskSchedulerCooldownText)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 9, design: .monospaced))
                    .frame(width: 90)
                Text("1–86400s")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
                if model.taskSchedulerPolicyBusy {
                    ProgressView().controlSize(.small)
                }
                Spacer()
                Button("Dispatch one task") {
                    Task { await model.dispatchScheduledTaskNow() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.taskQueueBusy || model.taskActionBusyID != nil)
                Button("Save policy") {
                    Task { await model.saveTaskSchedulerPolicy() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.taskSchedulerPolicyBusy)
            }

            if let status = model.taskSchedulerPolicyStatus {
                Text(status)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
            }

            Text(
                "Ready tasks are ordered by task priority plus session priority, with aging so low-priority work cannot starve. When auto-resume is enabled, the daemon can resume idle/failed plans after the cooldown. With one-task-per-tick enabled it claims and runs a single ready task, then releases the session so other sessions can make progress; the project cooldown still applies between dispatches, so use a small value such as 5s for continuous progress. Otherwise it resumes the whole stored plan. Write leases are stored by the daemon and enforce overlapping path scopes across sessions; the same session may share its worktree."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)

            if let queue = model.taskQueue {
                HStack(spacing: 14) {
                    schedulerCount("queued", queue.tasks.count(where: { $0.status == "pending" }))
                    schedulerCount("running", queue.tasks.count(where: { $0.status == "running" }))
                    schedulerCount("completed", queue.tasks.count(where: { $0.status == "completed" }))
                    schedulerCount("blocked", queue.tasks.count(where: { $0.status == "blocked" }))
                    schedulerCount(
                        "paused",
                        queue.tasks.count(where: { $0.status == "pausing" || $0.status == "paused" })
                    )
                    schedulerCount("skipped", queue.tasks.count(where: { $0.status == "skipped" }))
                    schedulerCount("leases", queue.leases.count)
                    Spacer()
                }
                if let status = model.taskActionStatus {
                    Text(status)
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
                if queue.tasks.isEmpty {
                    Text("No plan tasks are queued for the selected project.")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.tertiary)
                } else {
                    ForEach(queue.tasks.prefix(8)) { task in
                        HStack(spacing: 7) {
                            Circle()
                                .fill(statusColor(task.status))
                                .frame(width: 6, height: 6)
                            Text(task.taskID)
                                .font(.system(size: 9, weight: .medium, design: .monospaced))
                                .frame(width: 70, alignment: .leading)
                            Text(task.title)
                                .font(.system(size: 9))
                                .lineLimit(1)
                            Spacer()
                            Text(task.department)
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(.secondary)
                            Text("p\(task.priority)")
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(.tertiary)
                            Text(task.status)
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(statusColor(task.status))
                            if model.taskActionBusyID == task.id {
                                ProgressView()
                                    .controlSize(.mini)
                                    .frame(width: 16)
                            } else {
                                if canPause(task.status) {
                                    Button {
                                        Task { await model.pauseRunningTask(task) }
                                    } label: {
                                        Image(systemName: "pause.circle")
                                            .font(.system(size: 8))
                                    }
                                    .buttonStyle(.plain)
                                    .foregroundStyle(.yellow)
                                    .help("Pause this task at its next turn/tool boundary")
                                }
                                if canResume(task.status) {
                                    Button {
                                        Task { await model.resumePausedTask(task) }
                                    } label: {
                                        Image(systemName: "play.circle")
                                            .font(.system(size: 8))
                                    }
                                    .buttonStyle(.plain)
                                    .foregroundStyle(.green)
                                    .help("Resume this paused task")
                                }
                                if canCancel(task.status) {
                                    Button {
                                        Task { await model.cancelRunningTask(task) }
                                    } label: {
                                        Image(systemName: "stop.circle")
                                            .font(.system(size: 8))
                                    }
                                    .buttonStyle(.plain)
                                    .foregroundStyle(.red)
                                    .help("Request cancellation of this running task")
                                }
                                if canRetry(task.status) {
                                    Button {
                                        Task { await model.retryScheduledTask(task) }
                                    } label: {
                                        Image(systemName: "arrow.clockwise")
                                            .font(.system(size: 8))
                                    }
                                    .buttonStyle(.plain)
                                    .foregroundStyle(.cyan)
                                    .help("Retry this task")
                                }
                                if canSkip(task.status) {
                                    Button {
                                        Task { await model.skipScheduledTask(task) }
                                    } label: {
                                        Image(systemName: "forward.end")
                                            .font(.system(size: 8))
                                    }
                                    .buttonStyle(.plain)
                                    .foregroundStyle(.orange)
                                    .help("Skip this queued task")
                                }
                            }
                        }
                        .padding(.vertical, 2)
                    }
                    if queue.tasks.count > 8 {
                        Text("+\(queue.tasks.count - 8) more tasks")
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.tertiary)
                    }
                }
                if !queue.leases.isEmpty {
                    Divider().opacity(0.25)
                    Text("Write leases")
                        .font(.system(size: 8, weight: .medium, design: .monospaced))
                        .foregroundStyle(.secondary)
                    ForEach(queue.leases.prefix(5)) { lease in
                        HStack(spacing: 7) {
                            Image(systemName: "lock")
                                .font(.system(size: 8))
                                .foregroundStyle(.cyan)
                            Text(lease.agentID)
                                .font(.system(size: 8, design: .monospaced))
                                .lineLimit(1)
                            Text(lease.scopes.isEmpty ? "unrestricted" : lease.scopes.joined(separator: ", "))
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                            Spacer()
                        }
                    }
                }
            } else {
                Text(model.taskQueueStatus ?? "Loading scheduler state…")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .task(id: model.selectedProjectID) {
            await model.loadTaskQueue()
            await model.loadTaskSchedulerPolicy()
        }
    }

    private func schedulerCount(_ title: String, _ value: Int) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text("\(value)")
                .font(.system(size: 12, weight: .semibold, design: .rounded))
            Text(title)
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.secondary)
        }
    }

    private func canPause(_ status: String) -> Bool {
        status == "running"
    }

    private func canResume(_ status: String) -> Bool {
        ["pausing", "paused"].contains(status)
    }

    private func canCancel(_ status: String) -> Bool {
        ["running", "pausing", "paused"].contains(status)
    }

    private func canRetry(_ status: String) -> Bool {
        ["failed", "blocked", "interrupted", "cancelled", "skipped"].contains(status)
    }

    private func canSkip(_ status: String) -> Bool {
        ["pending", "failed", "blocked", "interrupted", "cancelled"].contains(status)
    }

    private func statusColor(_ status: String) -> Color {
        switch status {
        case "running": .green
        case "completed": .cyan
        case "failed", "blocked": .orange
        case "interrupted", "cancelling", "pausing", "paused": .yellow
        case "cancelled", "skipped": .secondary
        default: .secondary
        }
    }
}
