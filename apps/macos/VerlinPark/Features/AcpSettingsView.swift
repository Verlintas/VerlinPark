import SwiftUI

struct AcpSettingsView: View {
    @Environment(AppModel.self) private var model
    @State private var name = ""
    @State private var transport: AcpTransport = .stdio
    @State private var command = ""
    @State private var argsText = ""
    @State private var envText = ""
    @State private var endpoint = ""
    @State private var authHeaderEnv = ""
    @State private var headersText = ""
    @State private var allowRemote = false
    @State private var connectTimeout = 30
    @State private var autoApprove = false
    @State private var timeout = 1800
    @State private var editingAgent: AcpAgent?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("External ACP agents")
                        .font(.system(size: 22, weight: .semibold, design: .rounded))
                    Text(
                        "Agents that speak the Agent Client Protocol. A stdio agent runs in an isolated worktree under the Seatbelt write sandbox with a scratch HOME; its permission requests use the risk-based approval policy and its commit is verified and merged like a Worker. A remote agent is a trusted external service reached over Streamable HTTP or WebSocket: it is not sandboxed, may act outside this worktree, and requires host-state approval for each delegation. Only configure trusted endpoints."
                    )
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
                }

                VStack(alignment: .leading, spacing: 8) {
                    SectionLabel(title: "Add agent")
                    HStack(spacing: 8) {
                        TextField("name", text: $name)
                            .textFieldStyle(.plain).vpFrostedField()
                            .frame(width: 150)
                        Picker("Transport", selection: $transport) {
                            ForEach(AcpTransport.allCases, id: \.self) { option in
                                Text(option.displayName).tag(option)
                            }
                        }
                        .pickerStyle(.segmented)
                        .labelsHidden()
                        .frame(maxWidth: 360)
                    }
                    if transport == .stdio {
                        TextField("command (for example npx)", text: $command)
                            .textFieldStyle(.plain).vpFrostedField()
                        TextEditor(text: $argsText)
                            .font(.system(size: 10, design: .monospaced))
                            .scrollContentBackground(.hidden)
                            .padding(6)
                            .frame(height: 54)
                            .background(
                                .black.opacity(0.16),
                                in: RoundedRectangle(cornerRadius: 8)
                            )
                            .overlay {
                                RoundedRectangle(cornerRadius: 8)
                                    .strokeBorder(.white.opacity(0.08), lineWidth: 1)
                            }
                        Text(
                            "Arguments, one per line. The agent is launched with HOME and XDG paths redirected into its task scratch directory; pass credentials explicitly as environment variables."
                        )
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                        TextEditor(text: $envText)
                            .font(.system(size: 10, design: .monospaced))
                            .scrollContentBackground(.hidden)
                            .padding(6)
                            .frame(height: 54)
                            .background(
                                .black.opacity(0.16),
                                in: RoundedRectangle(cornerRadius: 8)
                            )
                            .overlay {
                                RoundedRectangle(cornerRadius: 8)
                                    .strokeBorder(.white.opacity(0.08), lineWidth: 1)
                            }
                        Text(
                            "Environment, one KEY=VALUE per line. Values are stored in the local database and passed to the agent; prefer a short-lived token."
                        )
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    } else {
                        TextField(
                            transport == .streamableHTTP
                                ? "endpoint (https://agent.example/acp)"
                                : "endpoint (wss://agent.example/acp)",
                            text: $endpoint
                        )
                        .textFieldStyle(.plain).vpFrostedField()
                        Text(
                            "Remote agents are not sandboxed. VerlinPark never stores the auth value: auth-header-env names an environment variable read by the daemon, and header values may use ${ENV_VAR}."
                        )
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                        HStack(spacing: 8) {
                            TextField(
                                "auth header env (for example ACP_TOKEN)",
                                text: $authHeaderEnv
                            )
                            .textFieldStyle(.plain).vpFrostedField()
                            Stepper(
                                "Connect \(connectTimeout)s",
                                value: $connectTimeout,
                                in: 1 ... 300
                            )
                            .controlSize(.small)
                        }
                        TextEditor(text: $headersText)
                            .font(.system(size: 10, design: .monospaced))
                            .scrollContentBackground(.hidden)
                            .padding(6)
                            .frame(height: 54)
                            .background(
                                .black.opacity(0.16),
                                in: RoundedRectangle(cornerRadius: 8)
                            )
                            .overlay {
                                RoundedRectangle(cornerRadius: 8)
                                    .strokeBorder(.white.opacity(0.08), lineWidth: 1)
                            }
                        Text(
                            "Extra headers, one KEY=VALUE per line. Use ${ENV_VAR} for secrets; expanded values are never persisted."
                        )
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                        Toggle(
                            "Allow remote transport (required; trusting this endpoint is an explicit decision)",
                            isOn: $allowRemote
                        )
                        .toggleStyle(.switch)
                        .controlSize(.small)
                    }
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
                            let args = transport == .stdio
                                ? argsText
                                .split(whereSeparator: \.isNewline)
                                .map(String.init)
                                .filter { !$0.isEmpty }
                                : []
                            let env = transport == .stdio ? parsePairs(envText) : [:]
                            let headers = transport.isRemote ? parsePairs(headersText) : [:]
                            Task {
                                let added = await model.addAcpAgent(
                                    name: name,
                                    command: transport == .stdio ? command : "",
                                    args: args,
                                    env: env,
                                    autoApprove: autoApprove,
                                    timeoutSecs: timeout,
                                    transport: transport,
                                    endpoint: transport.isRemote
                                        ? endpoint.trimmingCharacters(in: .whitespaces)
                                        : nil,
                                    authHeaderEnv: transport.isRemote
                                        ? authHeaderEnv.trimmingCharacters(in: .whitespaces)
                                        : nil,
                                    headers: headers,
                                    connectTimeoutSecs: connectTimeout,
                                    allowRemote: transport.isRemote && allowRemote
                                )
                                if added {
                                    name = ""
                                    transport = .stdio
                                    command = ""
                                    argsText = ""
                                    envText = ""
                                    endpoint = ""
                                    authHeaderEnv = ""
                                    headersText = ""
                                    allowRemote = false
                                    connectTimeout = 30
                                    autoApprove = false
                                    timeout = 1800
                                }
                            }
                        }
                        .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                        .disabled(!canAdd)
                    }
                }
                .padding(14)
                .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14))
                .overlay {
                    RoundedRectangle(cornerRadius: 14)
                        .strokeBorder(.white.opacity(0.06), lineWidth: 1)
                }
                .vpHoverLift(scale: 1.002, lift: 1)
                .vpAppear(index: 0)

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
                            AcpAgentRow(agent: agent) {
                                editingAgent = agent
                            }
                        }
                    }
                }
            }
            .padding(22)
        }
        .task {
            await model.loadAcpAgents()
        }
        .sheet(item: $editingAgent) { agent in
            AcpAgentEditSheet(agent: agent)
                .environment(model)
        }
    }

    private var canAdd: Bool {
        guard !model.acpBusy, !name.trimmingCharacters(in: .whitespaces).isEmpty else {
            return false
        }
        if transport == .stdio {
            return !command.trimmingCharacters(in: .whitespaces).isEmpty
        }
        return allowRemote && !endpoint.trimmingCharacters(in: .whitespaces).isEmpty
    }

    private func parsePairs(_ text: String) -> [String: String] {
        var pairs: [String: String] = [:]
        for line in text.split(whereSeparator: \.isNewline) {
            let parts = line.split(separator: "=", maxSplits: 1)
            guard parts.count == 2 else { continue }
            let key = parts[0].trimmingCharacters(in: .whitespaces)
            let value = parts[1].trimmingCharacters(in: .whitespaces)
            if !key.isEmpty {
                pairs[key] = value
            }
        }
        return pairs
    }
}

private struct AcpAgentRow: View {
    @Environment(AppModel.self) private var model
    let agent: AcpAgent
    let onEdit: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text(agent.name)
                    .font(.system(size: 12, weight: .semibold))
                Text(agent.transport.displayName)
                    .font(.system(size: 8, weight: .semibold, design: .monospaced))
                    .foregroundStyle(agent.transport.isRemote ? .orange : .secondary)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(
                        (agent.transport.isRemote ? Color.orange : Color.white)
                            .opacity(0.12),
                        in: Capsule()
                    )
                Text(agent.summary)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
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
                Button("Edit", action: onEdit)
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
                if agent.transport.isRemote {
                    Text("connect \(agent.connectTimeoutSecs)s")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    if let auth = agent.authHeaderEnv, !auth.isEmpty {
                        Text("auth: $\(auth)")
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.tertiary)
                            .lineLimit(1)
                    }
                    if !agent.headers.isEmpty {
                        Text("headers: \(agent.headers.keys.sorted().joined(separator: ", "))")
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.tertiary)
                            .lineLimit(2)
                    }
                } else {
                    if !agent.args.isEmpty {
                        Text(agent.args.joined(separator: " "))
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.tertiary)
                            .lineLimit(2)
                    }
                    if !agent.env.isEmpty {
                        Text("env: \(agent.env.keys.sorted().joined(separator: ", "))")
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.tertiary)
                            .lineLimit(2)
                    }
                }
            }
            if let status = model.acpProbeStatus[agent.id] {
                Text(status)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .contentTransition(.opacity)
                    .animation(VPMotion.snap, value: status)
            }
        }
        .padding(10)
        .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 10))
        .vpHoverLift(scale: 1.004, lift: 1, glow: agent.enabled ? .green : .white)
        .vpAppear(index: 0)
    }
}

private struct AcpAgentEditSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let agent: AcpAgent

    @State private var name: String
    @State private var transport: AcpTransport
    @State private var command: String
    @State private var argsText: String
    @State private var envText: String
    @State private var endpoint: String
    @State private var authHeaderEnv: String
    @State private var headersText: String
    @State private var allowRemote: Bool
    @State private var connectTimeout: Int
    @State private var autoApprove: Bool
    @State private var timeout: Int
    @State private var enabled: Bool

    init(agent: AcpAgent) {
        self.agent = agent
        _name = State(initialValue: agent.name)
        _transport = State(initialValue: agent.transport)
        _command = State(initialValue: agent.command)
        _argsText = State(initialValue: agent.args.joined(separator: "\n"))
        _envText = State(initialValue: Self.pairsText(agent.env))
        _endpoint = State(initialValue: agent.endpoint ?? "")
        _authHeaderEnv = State(initialValue: agent.authHeaderEnv ?? "")
        _headersText = State(initialValue: Self.pairsText(agent.headers))
        _allowRemote = State(initialValue: agent.allowRemote)
        _connectTimeout = State(initialValue: agent.connectTimeoutSecs)
        _autoApprove = State(initialValue: agent.autoApprove)
        _timeout = State(initialValue: agent.timeoutSecs)
        _enabled = State(initialValue: agent.enabled)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                Text("Edit ACP agent")
                    .font(.system(size: 18, weight: .semibold, design: .rounded))
                Spacer()
                Toggle("Enabled", isOn: $enabled)
                    .toggleStyle(.switch)
                    .controlSize(.small)
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    HStack(spacing: 8) {
                        TextField("name", text: $name)
                            .textFieldStyle(.plain).vpFrostedField()
                        Picker("Transport", selection: $transport) {
                            ForEach(AcpTransport.allCases, id: \.self) { option in
                                Text(option.displayName).tag(option)
                            }
                        }
                        .pickerStyle(.segmented)
                        .labelsHidden()
                    }
                    if transport == .stdio {
                        TextField("command", text: $command)
                            .textFieldStyle(.plain).vpFrostedField()
                        TextEditor(text: $argsText)
                            .font(.system(size: 10, design: .monospaced))
                            .scrollContentBackground(.hidden)
                            .padding(6)
                            .frame(height: 50)
                            .background(.black.opacity(0.16), in: RoundedRectangle(cornerRadius: 8))
                        TextEditor(text: $envText)
                            .font(.system(size: 10, design: .monospaced))
                            .scrollContentBackground(.hidden)
                            .padding(6)
                            .frame(height: 50)
                            .background(.black.opacity(0.16), in: RoundedRectangle(cornerRadius: 8))
                        Text("Arguments and environment, one entry per line.")
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.tertiary)
                    } else {
                        TextField("endpoint", text: $endpoint)
                            .textFieldStyle(.plain).vpFrostedField()
                        HStack(spacing: 8) {
                            TextField("auth header env", text: $authHeaderEnv)
                                .textFieldStyle(.plain).vpFrostedField()
                            Stepper(
                                "Connect \(connectTimeout)s",
                                value: $connectTimeout,
                                in: 1 ... 300
                            )
                            .controlSize(.small)
                        }
                        TextEditor(text: $headersText)
                            .font(.system(size: 10, design: .monospaced))
                            .scrollContentBackground(.hidden)
                            .padding(6)
                            .frame(height: 50)
                            .background(.black.opacity(0.16), in: RoundedRectangle(cornerRadius: 8))
                        Text(
                            "Headers are KEY=VALUE per line; ${ENV_VAR} is expanded from the daemon environment and never shown here."
                        )
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                        Toggle("Allow remote transport", isOn: $allowRemote)
                            .toggleStyle(.switch)
                            .controlSize(.small)
                        Text("Remote agents are trusted external services and are not sandboxed.")
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.orange)
                    }
                    HStack(spacing: 12) {
                        Toggle("Auto-approve ACP permissions", isOn: $autoApprove)
                            .toggleStyle(.switch)
                            .controlSize(.small)
                        Stepper("Timeout \(timeout)s", value: $timeout, in: 30 ... 14400, step: 30)
                            .controlSize(.small)
                    }
                }
                .padding(.vertical, 4)
            }
            HStack {
                Spacer()
                if model.acpBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Cancel") { dismiss() }
                    .buttonStyle(.bordered)
                Button("Save") { save() }
                    .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                    .disabled(!canSave)
            }
        }
        .padding(20)
        .frame(width: 560, height: 520)
    }

    private var canSave: Bool {
        guard !model.acpBusy, !name.trimmingCharacters(in: .whitespaces).isEmpty else {
            return false
        }
        if transport == .stdio {
            return !command.trimmingCharacters(in: .whitespaces).isEmpty
        }
        return allowRemote && !endpoint.trimmingCharacters(in: .whitespaces).isEmpty
    }

    private func save() {
        let args = transport == .stdio
            ? argsText.split(whereSeparator: \.isNewline)
            .map(String.init)
            .filter { !$0.isEmpty }
            : []
        let env = transport == .stdio ? Self.parsePairs(envText) : [:]
        let headers = transport.isRemote ? Self.parsePairs(headersText) : [:]
        Task {
            let saved = await model.updateAcpAgentSettings(
                id: agent.id,
                name: name,
                command: transport == .stdio ? command : "",
                args: args,
                env: env,
                transport: transport,
                endpoint: transport.isRemote
                    ? endpoint.trimmingCharacters(in: .whitespaces)
                    : nil,
                authHeaderEnv: transport.isRemote
                    ? authHeaderEnv.trimmingCharacters(in: .whitespaces)
                    : nil,
                headers: headers,
                connectTimeoutSecs: connectTimeout,
                allowRemote: transport.isRemote && allowRemote,
                autoApprove: autoApprove,
                timeoutSecs: timeout,
                enabled: enabled
            )
            if saved {
                dismiss()
            }
        }
    }

    private static func pairsText(_ pairs: [String: String]) -> String {
        pairs.keys.sorted().map { "\($0)=\(pairs[$0] ?? "")" }.joined(separator: "\n")
    }

    private static func parsePairs(_ text: String) -> [String: String] {
        var pairs: [String: String] = [:]
        for line in text.split(whereSeparator: \.isNewline) {
            let parts = line.split(separator: "=", maxSplits: 1)
            guard parts.count == 2 else { continue }
            let key = parts[0].trimmingCharacters(in: .whitespaces)
            let value = parts[1].trimmingCharacters(in: .whitespaces)
            if !key.isEmpty {
                pairs[key] = value
            }
        }
        return pairs
    }
}
