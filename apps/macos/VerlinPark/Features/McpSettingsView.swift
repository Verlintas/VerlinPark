import SwiftUI

struct McpSettingsView: View {
    @Environment(AppModel.self) private var model
    @State private var name = ""
    @State private var transport = "stdio"
    @State private var command = ""
    @State private var argsText = ""
    @State private var envText = ""
    @State private var url = ""
    @State private var headersText = ""
    @State private var editing: McpServer?

    private var canAdd: Bool {
        guard !name.trimmingCharacters(in: .whitespaces).isEmpty, McpConfigParsing.pairsAreValid(envText),
              McpConfigParsing.pairsAreValid(headersText) else { return false }
        if transport == "stdio" {
            return !command.trimmingCharacters(in: .whitespaces).isEmpty
        }
        return !url.trimmingCharacters(in: .whitespaces).isEmpty
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("MCP servers")
                        .font(.system(size: 22, weight: .semibold, design: .rounded))
                    Text(
                        "Stdio servers run as trusted local processes. HTTP uses the Streamable HTTP transport; SSE uses the legacy HTTP+SSE transport. Header values may use ${ENV_VAR} indirection; sensitive headers are required to do so and are expanded from the daemon environment."
                    )
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
                }

                VStack(alignment: .leading, spacing: 8) {
                    SectionLabel(title: "Add server")
                    HStack(spacing: 8) {
                        TextField("name", text: $name)
                            .textFieldStyle(.plain).vpFrostedField()
                            .frame(width: 150)
                        Picker("Transport", selection: $transport) {
                            Text("Stdio").tag("stdio")
                            Text("HTTP").tag("http")
                            Text("SSE").tag("sse")
                        }
                        .pickerStyle(.segmented)
                        .frame(width: 210)
                    }
                    McpConfigEditor(
                        transport: transport,
                        command: $command,
                        argsText: $argsText,
                        envText: $envText,
                        url: $url,
                        headersText: $headersText
                    )
                    HStack {
                        if !McpConfigParsing.pairsAreValid(envText)
                            || !McpConfigParsing.pairsAreValid(headersText)
                        {
                            Text("Every non-empty line must be KEY=VALUE.")
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(.orange)
                        }
                        Spacer()
                        if model.mcpBusy {
                            ProgressView().controlSize(.small)
                        }
                        Button("Add server") {
                            Task {
                                let added = await model.addMcpServer(
                                    name: name,
                                    transport: transport,
                                    command: transport == "stdio" ? command : "",
                                    args: transport == "stdio"
                                        ? McpConfigParsing.arguments(argsText)
                                        : [],
                                    env: transport == "stdio"
                                        ? McpConfigParsing.pairs(envText)
                                        : [:],
                                    url: transport == "stdio" ? nil : url,
                                    headers: transport == "stdio"
                                        ? [:]
                                        : McpConfigParsing.pairs(headersText)
                                )
                                if added {
                                    name = ""
                                    command = ""
                                    argsText = ""
                                    envText = ""
                                    url = ""
                                    headersText = ""
                                }
                            }
                        }
                        .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                        .disabled(model.mcpBusy || !canAdd)
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

                if let status = model.mcpStatus, !model.mcpServers.isEmpty {
                    Text(status)
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.secondary)
                }

                VStack(alignment: .leading, spacing: 10) {
                    HStack {
                        SectionLabel(title: "Configured")
                        Spacer()
                        Button("Refresh") {
                            Task { await model.loadMcpServers() }
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                    }
                    if model.mcpServers.isEmpty {
                        Text(model.mcpStatus ?? "No MCP servers configured.")
                            .font(.system(size: 9, design: .monospaced))
                            .foregroundStyle(.secondary)
                    } else {
                        ForEach(model.mcpServers) { server in
                            McpServerRow(server: server) {
                                editing = server
                            }
                        }
                    }
                }
            }
            .padding(22)
        }
        .task {
            await model.loadMcpServers()
        }
        .sheet(item: $editing) { server in
            McpEditSheet(server: server)
                .environment(model)
        }
    }
}

private struct McpConfigEditor: View {
    let transport: String
    @Binding var command: String
    @Binding var argsText: String
    @Binding var envText: String
    @Binding var url: String
    @Binding var headersText: String

    var body: some View {
        Group {
            if transport == "stdio" {
                TextField("command (for example npx)", text: $command)
                    .textFieldStyle(.plain).vpFrostedField()
                pairEditor(
                    title: "Arguments, one per line.",
                    placeholder: "--root\n/tmp",
                    text: $argsText
                )
                pairEditor(
                    title: "Environment, KEY=VALUE per line. Values are stored in the local daemon DB.",
                    placeholder: "LOG_LEVEL=info",
                    text: $envText
                )
            } else {
                TextField("URL (https://host/acp or http://127.0.0.1:3000/sse)", text: $url)
                    .textFieldStyle(.plain).vpFrostedField()
                pairEditor(
                    title: "Headers, KEY=VALUE per line. Use ${ENV_VAR} for secrets.",
                    placeholder: "Authorization=Bearer ${MCP_TOKEN}\nX-Client=VerlinPark",
                    text: $headersText
                )
            }
        }
        .animation(VPMotion.snap, value: transport)
    }

    private func pairEditor(
        title: String,
        placeholder: String,
        text: Binding<String>
    ) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            TextEditor(text: text)
                .font(.system(size: 10, design: .monospaced))
                .scrollContentBackground(.hidden)
                .padding(6)
                .frame(height: 54)
                .background(.black.opacity(0.16), in: RoundedRectangle(cornerRadius: 8))
                .overlay {
                    RoundedRectangle(cornerRadius: 8)
                        .strokeBorder(.white.opacity(0.08), lineWidth: 1)
                }
                .overlay(alignment: .topLeading) {
                    if text.wrappedValue.isEmpty {
                        Text(placeholder)
                            .font(.system(size: 9, design: .monospaced))
                            .foregroundStyle(.tertiary)
                            .padding(.leading, 10)
                            .padding(.top, 8)
                            .allowsHitTesting(false)
                    }
                }
            Text(title)
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
        }
    }
}

private struct McpServerRow: View {
    @Environment(AppModel.self) private var model
    let server: McpServer
    let onEdit: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text(server.name)
                    .font(.system(size: 12, weight: .semibold))
                Text(server.transportKind.uppercased())
                    .font(.system(size: 8, weight: .semibold, design: .monospaced))
                    .padding(.horizontal, 5)
                    .padding(.vertical, 1)
                    .background(.white.opacity(0.08), in: Capsule())
                    .foregroundStyle(.secondary)
                Text(server.transportKind == "stdio"
                    ? server.command
                    : (server.url ?? "missing URL"))
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer()
                if model.mcpProbeBusy.contains(server.id) {
                    ProgressView().controlSize(.small)
                }
                Button("Probe") {
                    Task { await model.probeMcpServer(id: server.id) }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.mcpProbeBusy.contains(server.id))
                Button("Edit", action: onEdit)
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                Toggle(
                    "Enabled",
                    isOn: Binding(
                        get: { server.enabled },
                        set: { enabled in
                            Task { await model.setMcpServerEnabled(server, enabled: enabled) }
                        }
                    )
                )
                .toggleStyle(.switch)
                .controlSize(.small)
                Menu("Inspect") {
                    Button("Tools") {
                        Task { await model.loadMcpTools(serverID: server.id) }
                    }
                    Button("Resources") {
                        Task { await model.loadMcpResources(serverID: server.id) }
                    }
                    Button("Prompts") {
                        Task { await model.loadMcpPrompts(serverID: server.id) }
                    }
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
                .controlSize(.small)
                Button("Remove") {
                    Task { await model.removeMcpServer(id: server.id) }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .tint(.red)
            }
            if server.transportKind == "stdio", !server.args.isEmpty {
                Text(server.args.joined(separator: " "))
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    .lineLimit(2)
            }
            if server.transportKind != "stdio" {
                Text(
                    "\(server.headerCount) header(s) · secrets use ${ENV_VAR} expansion"
                )
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
            }
            if let status = model.mcpProbeStatus[server.id] {
                Text(status)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .contentTransition(.opacity)
                    .animation(VPMotion.snap, value: status)
            }
            if let status = model.mcpToolStatus[server.id] {
                Text(status)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
            if let tools = model.mcpTools[server.id] {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(tools) { tool in
                        Text("\(tool.name) · \(tool.description)")
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.secondary)
                            .lineLimit(2)
                    }
                }
                .padding(.leading, 8)
            }
            if let resources = model.mcpResources[server.id] {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(resources.resources) { resource in
                        Text("\(resource.name) <\(resource.uri)>")
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.secondary)
                            .lineLimit(2)
                    }
                    ForEach(resources.templates) { template in
                        Text("template \(template.name) <\(template.uriTemplate)>")
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.tertiary)
                            .lineLimit(2)
                    }
                }
                .padding(.leading, 8)
            }
            if let prompts = model.mcpPrompts[server.id] {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(prompts.prompts) { prompt in
                        Text("\(prompt.name) · \(prompt.description)")
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.secondary)
                            .lineLimit(2)
                    }
                }
                .padding(.leading, 8)
            }
        }
        .padding(10)
        .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 10))
        .vpHoverLift(scale: 1.004, lift: 1, glow: server.enabled ? .green : .white)
        .vpAppear(index: 0)
    }
}

private struct McpEditSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let server: McpServer
    @State private var name: String
    @State private var transport: String
    @State private var command: String
    @State private var argsText: String
    @State private var envText: String
    @State private var url: String
    @State private var headersText: String

    init(server: McpServer) {
        self.server = server
        _name = State(initialValue: server.name)
        _transport = State(initialValue: server.transportKind)
        _command = State(initialValue: server.command)
        _argsText = State(initialValue: server.args.joined(separator: "\n"))
        _envText = State(
            initialValue: server.env
                .map { "\($0.key)=\($0.value)" }
                .sorted()
                .joined(separator: "\n")
        )
        _url = State(initialValue: server.url ?? "")
        _headersText = State(
            initialValue: (server.headers ?? [:])
                .map { "\($0.key)=\($0.value)" }
                .sorted()
                .joined(separator: "\n")
        )
    }

    private var canSave: Bool {
        guard !name.trimmingCharacters(in: .whitespaces).isEmpty, McpConfigParsing.pairsAreValid(envText),
              McpConfigParsing.pairsAreValid(headersText) else { return false }
        if transport == "stdio" {
            return !command.trimmingCharacters(in: .whitespaces).isEmpty
        }
        return !url.trimmingCharacters(in: .whitespaces).isEmpty
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 3) {
                    Text("Edit MCP server")
                        .font(.system(size: 20, weight: .semibold, design: .rounded))
                    Text(server.id)
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
            .padding(20)

            Divider().opacity(0.3)

            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    HStack(spacing: 8) {
                        TextField("name", text: $name)
                            .textFieldStyle(.plain).vpFrostedField()
                        Picker("Transport", selection: $transport) {
                            Text("Stdio").tag("stdio")
                            Text("HTTP").tag("http")
                            Text("SSE").tag("sse")
                        }
                        .pickerStyle(.segmented)
                        .frame(width: 210)
                    }
                    McpConfigEditor(
                        transport: transport,
                        command: $command,
                        argsText: $argsText,
                        envText: $envText,
                        url: $url,
                        headersText: $headersText
                    )
                    if let status = model.mcpStatus {
                        Text(status)
                            .font(.system(size: 9, design: .monospaced))
                            .foregroundStyle(.secondary)
                    }
                }
                .padding(20)
            }

            Divider().opacity(0.3)
            HStack {
                if model.mcpBusy {
                    ProgressView().controlSize(.small)
                }
                Spacer()
                Button("Save") {
                    Task {
                        let saved = await model.updateMcpServer(
                            id: server.id,
                            name: name,
                            transport: transport,
                            command: transport == "stdio" ? command : "",
                            args: transport == "stdio"
                                ? McpConfigParsing.arguments(argsText)
                                : [],
                            env: transport == "stdio"
                                ? McpConfigParsing.pairs(envText)
                                : [:],
                            url: transport == "stdio" ? nil : url,
                            headers: transport == "stdio"
                                ? [:]
                                : McpConfigParsing.pairs(headersText),
                            enabled: server.enabled
                        )
                        if saved {
                            dismiss()
                        }
                    }
                }
                .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                .disabled(model.mcpBusy || !canSave)
            }
            .padding(20)
        }
        .frame(width: 680, height: 620)
        .background(.ultraThinMaterial)
    }
}

private enum McpConfigParsing {
    static func arguments(_ text: String) -> [String] {
        text.split(whereSeparator: \.isNewline)
            .map(String.init)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
    }

    static func pairs(_ text: String) -> [String: String] {
        var result: [String: String] = [:]
        for line in text.split(whereSeparator: \.isNewline) {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            guard let separator = trimmed.firstIndex(of: "=") else { continue }
            let key = String(trimmed[..<separator]).trimmingCharacters(in: .whitespaces)
            let value = String(trimmed[trimmed.index(after: separator)...])
                .trimmingCharacters(in: .whitespaces)
            if !key.isEmpty {
                result[key] = value
            }
        }
        return result
    }

    static func pairsAreValid(_ text: String) -> Bool {
        for line in text.split(whereSeparator: \.isNewline) {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.isEmpty {
                continue
            }
            guard let separator = trimmed.firstIndex(of: "=") else { return false }
            if trimmed[..<separator].trimmingCharacters(in: .whitespaces).isEmpty {
                return false
            }
        }
        return true
    }
}
