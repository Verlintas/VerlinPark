import SwiftUI

struct AcpSettingsView: View {
    @Environment(AppModel.self) private var model
    @State private var name = ""
    @State private var command = ""
    @State private var argsText = ""
    @State private var autoApprove = false
    @State private var timeout = 1800

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("External ACP agents")
                        .font(.system(size: 22, weight: .semibold, design: .rounded))
                    Text(
                        "Agents that speak the Agent Client Protocol over stdio (for example Gemini CLI or Claude Code ACP). A delegated task runs in an isolated worktree under the Seatbelt write sandbox with a scratch HOME; its permission requests use the approval center, and its commit is verified and merged like a Worker. Only install trusted agents."
                    )
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
                }

                VStack(alignment: .leading, spacing: 8) {
                    SectionLabel(title: "Add agent")
                    HStack(spacing: 8) {
                        TextField("name", text: $name)
                            .textFieldStyle(.roundedBorder)
                            .frame(width: 150)
                        TextField("command (for example npx)", text: $command)
                            .textFieldStyle(.roundedBorder)
                    }
                    TextEditor(text: $argsText)
                        .font(.system(size: 10, design: .monospaced))
                        .scrollContentBackground(.hidden)
                        .padding(6)
                        .frame(height: 54)
                        .background(.black.opacity(0.16), in: RoundedRectangle(cornerRadius: 8))
                        .overlay {
                            RoundedRectangle(cornerRadius: 8)
                                .strokeBorder(.white.opacity(0.08), lineWidth: 1)
                        }
                    Text(
                        "Arguments, one per line. The agent is launched with HOME and XDG paths redirected into its task scratch directory; pass credentials explicitly as environment variables."
                    )
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    HStack(spacing: 12) {
                        Toggle("Auto-approve ACP permissions", isOn: $autoApprove)
                            .toggleStyle(.switch)
                            .controlSize(.small)
                        Stepper("Timeout \(timeout)s", value: $timeout, in: 30 ... 14400, step: 30)
                            .controlSize(.small)
                        Spacer()
                        if model.acpBusy {
                            ProgressView().controlSize(.small)
                        }
                        Button("Add agent") {
                            let args = argsText
                                .split(whereSeparator: \.isNewline)
                                .map(String.init)
                                .filter { !$0.isEmpty }
                            Task {
                                await model.addAcpAgent(
                                    name: name,
                                    command: command,
                                    args: args,
                                    autoApprove: autoApprove,
                                    timeoutSecs: timeout
                                )
                                name = ""
                                command = ""
                                argsText = ""
                                autoApprove = false
                                timeout = 1800
                            }
                        }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.small)
                        .disabled(
                            model.acpBusy
                                || name.trimmingCharacters(in: .whitespaces).isEmpty
                                || command.trimmingCharacters(in: .whitespaces).isEmpty
                        )
                    }
                }
                .padding(14)
                .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14))
                .overlay {
                    RoundedRectangle(cornerRadius: 14)
                        .strokeBorder(.white.opacity(0.06), lineWidth: 1)
                }

                VStack(alignment: .leading, spacing: 10) {
                    HStack {
                        SectionLabel(title: "Configured")
                        Spacer()
                        Button("Refresh") {
                            Task { await model.loadAcpAgents() }
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                    }
                    if model.acpAgents.isEmpty {
                        Text(model.acpStatus ?? "No ACP agents configured.")
                            .font(.system(size: 9, design: .monospaced))
                            .foregroundStyle(.secondary)
                    } else {
                        ForEach(model.acpAgents) { agent in
                            AcpAgentRow(agent: agent)
                        }
                    }
                }
            }
            .padding(22)
        }
        .task {
            await model.loadAcpAgents()
        }
    }
}

private struct AcpAgentRow: View {
    @Environment(AppModel.self) private var model
    let agent: AcpAgent

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text(agent.name)
                    .font(.system(size: 12, weight: .semibold))
                Text(agent.command)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                Spacer()
                Toggle(
                    "Enabled",
                    isOn: Binding(
                        get: { agent.enabled },
                        set: { enabled in
                            Task { await model.setAcpAgentEnabled(agent, enabled: enabled) }
                        }
                    )
                )
                .toggleStyle(.switch)
                .controlSize(.small)
                Toggle(
                    "Auto-approve",
                    isOn: Binding(
                        get: { agent.autoApprove },
                        set: { autoApprove in
                            Task { await model.setAcpAgentAutoApprove(agent, autoApprove: autoApprove) }
                        }
                    )
                )
                .toggleStyle(.switch)
                .controlSize(.small)
                Button("Probe") {
                    Task { await model.probeAcpAgent(id: agent.id) }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                Button("Remove") {
                    Task { await model.removeAcpAgent(id: agent.id) }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .tint(.red)
            }
            HStack(spacing: 10) {
                Text("timeout \(agent.timeoutSecs)s")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
                if !agent.args.isEmpty {
                    Text(agent.args.joined(separator: " "))
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                        .lineLimit(2)
                }
            }
            if let status = model.acpProbeStatus[agent.id] {
                Text(status)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
        }
        .padding(10)
        .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 10))
    }
}
