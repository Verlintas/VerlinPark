import SwiftUI

struct McpSettingsView: View {
    @Environment(AppModel.self) private var model
    @State private var name = ""
    @State private var command = ""
    @State private var argsText = ""

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("MCP servers")
                        .font(.system(size: 22, weight: .semibold, design: .rounded))
                    Text(
                        "Stdio Model Context Protocol servers. They run as local processes outside the shell sandbox; only add trusted commands. Agent tool calls to MCP require approval."
                    )
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
                }

                VStack(alignment: .leading, spacing: 8) {
                    SectionLabel(title: "Add server")
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
                    Text("Arguments, one per line.")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    HStack {
                        Spacer()
                        if model.mcpBusy {
                            ProgressView().controlSize(.small)
                        }
                        Button("Add server") {
                            let args = argsText
                                .split(whereSeparator: \.isNewline)
                                .map(String.init)
                                .filter { !$0.isEmpty }
                            Task {
                                await model.addMcpServer(
                                    name: name,
                                    command: command,
                                    args: args
                                )
                                name = ""
                                command = ""
                                argsText = ""
                            }
                        }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.small)
                        .disabled(
                            model.mcpBusy
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
                            McpServerRow(server: server)
                        }
                    }
                }
            }
            .padding(22)
        }
        .task {
            await model.loadMcpServers()
        }
    }
}

private struct McpServerRow: View {
    @Environment(AppModel.self) private var model
    let server: McpServer

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text(server.name)
                    .font(.system(size: 12, weight: .semibold))
                Text(server.command)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
                Spacer()
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
            if !server.args.isEmpty {
                Text(server.args.joined(separator: " "))
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    .lineLimit(2)
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
    }
}
