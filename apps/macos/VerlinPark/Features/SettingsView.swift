import AppKit
import SwiftUI

struct SettingsView: View {
    @Environment(BrandStore.self) private var brand
    @Environment(AppModel.self) private var model

    var body: some View {
        TabView {
            BrandSettingsView()
                .tabItem { Label("Brand", systemImage: "paintpalette") }
            ResidentSettingsView()
                .tabItem { Label("Resident", systemImage: "sparkles") }
            ProvidersSettingsView()
                .tabItem { Label("Providers", systemImage: "network") }
            McpSettingsView()
                .tabItem { Label("MCP", systemImage: "puzzlepiece.extension") }
            AboutSettingsView()
                .tabItem { Label("About", systemImage: "info.circle") }
        }
        .frame(width: 680, height: 600)
        .preferredColorScheme(.dark)
        .background(.ultraThinMaterial)
    }
}

struct BrandSettingsView: View {
    @Environment(BrandStore.self) private var brand
    @State private var showingFlagPicker = false

    var body: some View {
        @Bindable var brand = brand

        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                HStack {
                    VStack(alignment: .leading, spacing: 3) {
                        Text("Brand")
                            .font(.system(size: 22, weight: .semibold, design: .rounded))
                        Text(
                            "The knockout wordmark in the top-left. The .app bundle stays VerlinPark for signing and updates."
                        )
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary)
                    }
                    Spacer()
                    BrandMark(config: brand.config, fontSize: 20)
                }

                VStack(alignment: .leading, spacing: 8) {
                    SectionLabel(title: "Display name")
                    TextField("VerlinPark", text: $brand.config.displayName)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 13))
                    HStack(spacing: 6) {
                        ForEach(BrandConfig.presets, id: \.self) { preset in
                            Button(preset) {
                                brand.config.displayName = preset
                            }
                            .buttonStyle(.plain)
                            .font(.system(size: 10))
                            .padding(.horizontal, 8)
                            .padding(.vertical, 5)
                            .background(.white.opacity(0.05), in: Capsule())
                            .overlay {
                                Capsule().strokeBorder(.white.opacity(0.06), lineWidth: 1)
                            }
                        }
                    }
                }

                VStack(alignment: .leading, spacing: 9) {
                    SectionLabel(title: "Mark background")
                    Picker("", selection: $brand.config.style) {
                        ForEach(BrandStyle.allCases) { style in
                            Label(style.title, systemImage: style.icon).tag(style)
                        }
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()

                    switch brand.config.style {
                    case .solid:
                        ColorPicker(
                            "Solid color",
                            selection: colorBinding(
                                get: { brand.config.solidHex },
                                set: { brand.config.solidHex = $0 }
                            )
                        )
                    case .gradient:
                        ColorPicker(
                            "Start",
                            selection: colorBinding(
                                get: { brand.config.gradientStartHex },
                                set: { brand.config.gradientStartHex = $0 }
                            )
                        )
                        ColorPicker(
                            "End",
                            selection: colorBinding(
                                get: { brand.config.gradientEndHex },
                                set: { brand.config.gradientEndHex = $0 }
                            )
                        )
                        HStack {
                            Text("Angle")
                                .font(.system(size: 11))
                            Slider(value: $brand.config.gradientAngle, in: 0 ... 360, step: 5)
                            Text("\(Int(brand.config.gradientAngle))°")
                                .font(.system(size: 10, design: .monospaced))
                                .frame(width: 34, alignment: .trailing)
                        }
                    case .flag:
                        HStack(spacing: 10) {
                            RoundedRectangle(cornerRadius: 7, style: .continuous)
                                .fill(.clear)
                                .frame(width: 58, height: 36)
                                .overlay {
                                    FlagSwatch(selection: brand.config.flag)
                                        .clipShape(RoundedRectangle(cornerRadius: 7, style: .continuous))
                                }
                            VStack(alignment: .leading, spacing: 2) {
                                Text(flagTitle)
                                    .font(.system(size: 11, weight: .medium))
                                Text("Regions · Pride collection")
                                    .font(.system(size: 9, design: .monospaced))
                                    .foregroundStyle(.secondary)
                                if let source = brand.config.flag.sourceURL {
                                    Link("Wikimedia Commons source", destination: source)
                                        .font(.system(size: 9))
                                }
                            }
                            Spacer()
                            Button("Choose…") {
                                showingFlagPicker = true
                            }
                            Text("Stripe flags stretch; patterned flags preserve aspect and crop.")
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(.tertiary)
                        }
                    }
                }

                HStack {
                    Button("Reset brand") {
                        brand.reset()
                    }
                    .font(.system(size: 11))
                    Spacer()
                    Text("Live preview")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }

                BrandMark(config: brand.config, fontSize: 26)
                    .frame(maxWidth: .infinity, alignment: .center)
                    .padding(.vertical, 12)
            }
            .padding(24)
        }
        .sheet(isPresented: $showingFlagPicker) {
            FlagPickerView(selection: $brand.config.flag)
                .environment(brand)
        }
    }

    private var flagTitle: String {
        switch brand.config.flag {
        case let .region(code):
            Locale.current.localizedString(forRegionCode: code) ?? code
        case let .pride(flag):
            flag.title
        }
    }

    private func colorBinding(
        get: @escaping () -> String,
        set: @escaping (String) -> Void
    ) -> Binding<Color> {
        Binding(
            get: { Color(hex: get()) },
            set: { set($0.hexString) }
        )
    }
}

struct FlagPickerView: View {
    @Binding var selection: FlagSelection
    @Environment(\.dismiss) private var dismiss

    enum Category: String, CaseIterable, Identifiable {
        case regions
        case pride
        var id: String {
            rawValue
        }

        var title: String {
            switch self {
            case .regions: "Regions"
            case .pride: "Pride"
            }
        }
    }

    @State private var category: Category = .regions
    @State private var search = ""

    private var options: [FlagOption] {
        let source = category == .regions ? FlagCatalog.regions : FlagCatalog.pride
        return FlagCatalog.search(search, in: source)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                VStack(alignment: .leading, spacing: 3) {
                    Text("Choose a flag")
                        .font(.title2.weight(.semibold))
                    Text(
                        "\(FlagCatalog.regions.count) regions · \(FlagCatalog.pride.count) pride flags"
                    )
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.secondary)
                }
                Spacer()
                Button("Cancel") { dismiss() }
            }

            HStack(spacing: 10) {
                Picker("", selection: $category) {
                    ForEach(Category.allCases) { category in
                        Text(category.title).tag(category)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(width: 220)

                TextField("Search", text: $search)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 11))
            }

            ScrollView {
                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: 104), spacing: 10)],
                    spacing: 10
                ) {
                    ForEach(options) { option in
                        Button {
                            selection = option.selection
                            dismiss()
                        } label: {
                            VStack(spacing: 5) {
                                RoundedRectangle(cornerRadius: 6, style: .continuous)
                                    .fill(.clear)
                                    .frame(height: 56)
                                    .overlay {
                                        FlagSwatch(selection: option.selection)
                                            .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
                                    }
                                    .overlay {
                                        RoundedRectangle(cornerRadius: 6, style: .continuous)
                                            .strokeBorder(
                                                isSelected(option) ? Color.green : Color.white.opacity(0.08),
                                                lineWidth: isSelected(option) ? 2 : 1
                                            )
                                    }
                                Text(option.title)
                                    .font(.system(size: 9))
                                    .lineLimit(1)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.vertical, 4)
            }

            Text("Region flags: FlagKit (MIT) · Pride flags: Wikimedia Commons (see THIRD_PARTY_NOTICES)")
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
        }
        .padding(22)
        .frame(width: 720, height: 560)
        .background(.ultraThinMaterial)
    }

    private func isSelected(_ option: FlagOption) -> Bool {
        switch (selection, option.selection) {
        case let (.region(a), .region(b)): a == b
        case let (.pride(a), .pride(b)): a == b
        default: false
        }
    }
}

struct ResidentSettingsView: View {
    private let bridge = UnavailableResidentAgent()

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(spacing: 12) {
                Image(systemName: "sparkles")
                    .font(.system(size: 18, weight: .bold))
                    .foregroundStyle(.green)
                    .frame(width: 38, height: 38)
                    .background(.green.opacity(0.12), in: Circle())
                VStack(alignment: .leading, spacing: 3) {
                    Text(bridge.displayName)
                        .font(.system(size: 18, weight: .semibold))
                    Text("External resident · NovaBAIC")
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
                Spacer()
                StatusPill(status: bridge.status.rawValue)
            }

            Divider()

            Text(
                "Aviiya is not implemented inside VerlinPark. This surface is reserved for the future NovaBAIC bridge, which will provide the resident persona, memory, tools, and cross-device sessions."
            )
            .font(.callout)
            .foregroundStyle(.secondary)

            VStack(alignment: .leading, spacing: 7) {
                LabeledContent("Runtime", value: "Not connected")
                LabeledContent("Persona", value: "NovaBAIC")
                LabeledContent("Project org", value: "Independent")
            }
            .font(.system(size: 11, design: .monospaced))

            Link(destination: URL(string: "https://github.com/Verlintas/NovaBAIC")!) {
                Label("Open NovaBAIC on GitHub", systemImage: "arrow.up.right.square")
            }
            .font(.system(size: 11))

            Spacer()
        }
        .padding(28)
    }
}

struct ProvidersSettingsView: View {
    @Environment(AppModel.self) private var model
    @AppStorage("verlinpark.session.tokenBudget") private var tokenBudget = 200_000

    private func defaultBaseURL(for kind: String) -> String {
        kind == "anthropic"
            ? "https://api.anthropic.com/v1"
            : "https://api.openai.com/v1"
    }

    var body: some View {
        @Bindable var model = model

        Form {
            Section("Leader model") {
                Picker("API", selection: $model.providerKind) {
                    Text("OpenAI-compatible").tag("openai-compatible")
                    Text("Anthropic").tag("anthropic")
                }
                .onChange(of: model.providerKind) { oldValue, newValue in
                    if model.providerBaseURL.isEmpty
                        || model.providerBaseURL == defaultBaseURL(for: oldValue)
                    {
                        model.providerBaseURL = defaultBaseURL(for: newValue)
                    }
                }
                TextField("Base URL", text: $model.providerBaseURL)
                    .textFieldStyle(.roundedBorder)
                TextField("Model", text: $model.providerModel)
                    .textFieldStyle(.roundedBorder)
                SecureField("API key", text: $model.providerAPIKey)
                    .textFieldStyle(.roundedBorder)
                Text(
                    "The key is stored in Keychain and sent only to the local daemon. It is never written to SQLite or logs."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
                HStack {
                    Button("Save & Connect") {
                        Task { await model.saveProviderSettings() }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(model.providerBusy)
                    Button("Test connection") {
                        Task { await model.probeProvider() }
                    }
                    .buttonStyle(.bordered)
                    .disabled(model.providerProbeBusy || !(model.agentInfo?.configured ?? false))
                    if model.providerBusy || model.providerProbeBusy {
                        ProgressView().controlSize(.small)
                    }
                    Spacer()
                    if let info = model.agentInfo {
                        Text(info.configured ? (info.model ?? "configured") : "not configured")
                            .font(.system(size: 11, design: .monospaced))
                            .foregroundStyle(info.configured ? Color.green : Color.orange)
                    }
                }
                if let status = model.providerStatus {
                    Text(status)
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
                if let status = model.providerProbeStatus {
                    Text(status)
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
            }

            Section("Role model overrides") {
                Text(
                    "Blank uses the base Leader model. Prices are USD per 1M tokens and are only used when a USD session budget is set."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
                RoleModelRow(title: "Leader", config: $model.roleModels.leader)
                RoleModelRow(title: "Manager", config: $model.roleModels.manager)
                RoleModelRow(title: "Worker", config: $model.roleModels.worker)
                RoleModelRow(title: "Verifier", config: $model.roleModels.verifier)
                RoleModelRow(title: "Labour", config: $model.roleModels.labour)
            }

            Section("Global capacity") {
                if let capacity = model.agentInfo?.capacity {
                    LabeledContent(
                        "Workers",
                        value: "\(capacity.workerAvailable) free / \(capacity.workerLimit)"
                    )
                    LabeledContent(
                        "Labour",
                        value: "\(capacity.labourAvailable) free / \(capacity.labourLimit)"
                    )
                } else {
                    Text("Capacity is unavailable until the daemon is connected.")
                        .foregroundStyle(.secondary)
                }
                Text(
                    "Daemon-wide Worker/Labour admission. Per-session caps still apply. Set VERLINPARK_GLOBAL_WORKERS and VERLINPARK_GLOBAL_LABOUR on the daemon to change the limits."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
                Button("Refresh") {
                    Task { await model.refreshAgentInfo() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
            }

            Section("Local endpoints") {
                LabeledContent("LM Studio", value: "http://127.0.0.1:1234/v1")
                LabeledContent("Ollama", value: "http://127.0.0.1:11434/v1")
                LabeledContent("llama.cpp", value: "http://127.0.0.1:8080/v1")
                Text("Local endpoints usually need no API key. Any OpenAI-compatible endpoint works.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Section("Session defaults") {
                TextField("Token budget", value: $tokenBudget, format: .number)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 160)
                Text(
                    "Input + output token budget for new sessions. Existing sessions keep their own budget. Concurrent agent batches may overshoot by one turn."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }

            Section("Models") {
                LabeledContent("Routing", value: "Manual only")
                LabeledContent("Budgets", value: "Per session · department · task")
                Text("No hidden model switching. The user chooses models and budgets.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
        .padding(24)
    }
}

private struct RoleModelRow: View {
    let title: String
    @Binding var config: RoleModelConfig

    var body: some View {
        HStack(spacing: 8) {
            Text(title)
                .font(.system(size: 11, weight: .medium))
                .frame(width: 64, alignment: .leading)
            TextField("base model", text: modelBinding)
                .textFieldStyle(.roundedBorder)
                .font(.system(size: 11, design: .monospaced))
            TextField("in $/1M", text: priceBinding(\.inputUsdPerMillion))
                .textFieldStyle(.roundedBorder)
                .font(.system(size: 11, design: .monospaced))
                .frame(width: 80)
            TextField("out $/1M", text: priceBinding(\.outputUsdPerMillion))
                .textFieldStyle(.roundedBorder)
                .font(.system(size: 11, design: .monospaced))
                .frame(width: 80)
        }
    }

    private var modelBinding: Binding<String> {
        Binding(
            get: { config.model ?? "" },
            set: { config.model = $0.trimmingCharacters(in: .whitespaces).isEmpty ? nil : $0 }
        )
    }

    private func priceBinding(
        _ keyPath: WritableKeyPath<RoleModelConfig, Double?>
    ) -> Binding<String> {
        Binding(
            get: {
                guard let value = config[keyPath: keyPath] else { return "" }
                return String(format: "%.6g", value)
            },
            set: {
                let trimmed = $0.trimmingCharacters(in: .whitespaces)
                config[keyPath: keyPath] = trimmed.isEmpty ? nil : Double(trimmed)
            }
        )
    }
}

struct AboutSettingsView: View {
    @Environment(BrandStore.self) private var brand

    var body: some View {
        VStack(spacing: 16) {
            BrandMark(config: brand.config, fontSize: 28)
            Text("VerlinPark")
                .font(.title2.weight(.semibold))
            Text("A native macOS workbench for agent teams.")
                .font(.callout)
                .foregroundStyle(.secondary)
            Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 7) {
                GridRow { Text("Version"); Text("0.1.0-dev").foregroundStyle(.secondary) }
                GridRow { Text("License"); Text("GPL-3.0-only").foregroundStyle(.secondary) }
                GridRow { Text("Protocol"); Text("1").foregroundStyle(.secondary) }
            }
            .font(.system(size: 11, design: .monospaced))
            Spacer()
            Text("Agent teams, not chat tabs.")
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(.tertiary)
        }
        .padding(28)
    }
}
