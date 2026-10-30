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

                if let project = model.selectedProject {
                    ShellPolicyCard(project: project)
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
