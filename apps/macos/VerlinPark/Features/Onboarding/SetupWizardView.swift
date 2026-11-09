import SwiftUI

/// First-run setup. The app should be useful immediately after install, so the
/// wizard collects only the two things that cannot be inferred: a model
/// endpoint/key and a project folder. Everything else keeps working defaults.
struct SetupWizardView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    let onFinish: () -> Void

    @State private var step = 0
    @State private var selectedPreset = "deepseek"
    @State private var didLoad = false

    private struct Preset: Identifiable {
        let id: String
        let title: String
        let kind: String
        let baseURL: String
        let model: String
        let context: String
        let needsKey: Bool
    }

    private let presets: [Preset] = [
        Preset(
            id: "deepseek",
            title: "DeepSeek",
            kind: "openai-compatible",
            baseURL: "https://api.deepseek.com",
            model: "deepseek-chat",
            context: "128000",
            needsKey: true
        ),
        Preset(
            id: "openai",
            title: "OpenAI",
            kind: "openai-compatible",
            baseURL: "https://api.openai.com/v1",
            model: "gpt-4o-mini",
            context: "128000",
            needsKey: true
        ),
        Preset(
            id: "anthropic",
            title: "Anthropic",
            kind: "anthropic",
            baseURL: "https://api.anthropic.com/v1",
            model: "claude-3-5-sonnet-latest",
            context: "200000",
            needsKey: true
        ),
        Preset(
            id: "lmstudio",
            title: "LM Studio",
            kind: "openai-compatible",
            baseURL: "http://127.0.0.1:1234/v1",
            model: "",
            context: "32000",
            needsKey: false
        ),
        Preset(
            id: "ollama",
            title: "Ollama",
            kind: "openai-compatible",
            baseURL: "http://127.0.0.1:11434/v1",
            model: "",
            context: "32000",
            needsKey: false
        ),
    ]

    private var preset: Preset? {
        presets.first { $0.id == selectedPreset }
    }

    private var isLocal: Bool {
        let url = model.providerBaseURL.lowercased()
        return url.contains("127.0.0.1") || url.contains("localhost") || preset?.needsKey == false
    }

    private var providerReady: Bool {
        model.agentInfo?.configured == true
    }

    private var projectReady: Bool {
        !model.projects.isEmpty
    }

    var body: some View {
        @Bindable var model = model

        VStack(spacing: 0) {
            header
            VPHairline()

            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    switch step {
                    case 0:
                        welcome
                    case 1:
                        providerStep
                    case 2:
                        projectStep
                    default:
                        doneStep
                    }
                }
                .padding(24)
                .frame(maxWidth: 620, alignment: .leading)
            }

            VPHairline()
            footer
        }
        .frame(width: 760, height: 620)
        .background { VPCrystalBackdrop() }
        .onAppear(perform: loadDefaults)
    }

    private var header: some View {
        HStack(spacing: 10) {
            ZStack {
                Circle()
                    .fill(VPTheme.accent.opacity(0.12))
                    .frame(width: 34, height: 34)
                Image(systemName: "person.3.sequence.fill")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(VPTheme.accent)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(L10n.t("Welcome to VerlinPark"))
                    .font(VPTheme.font(16, weight: .semibold))
                Text(L10n.t("Three steps to a working agent team"))
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            Spacer()
            Button(L10n.t("Skip setup")) {
                finish()
            }
            .buttonStyle(.borderless)
            .foregroundStyle(VPTheme.textTertiary)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
    }

    private var welcome: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(L10n
                .t(
                    "VerlinPark runs a local multi-agent studio. You do not need to configure budgets, team sizes, policies, or model presets before the first run."
                ))
                .font(VPTheme.font(12))
                .foregroundStyle(VPTheme.textSecondary)
            ForEach(welcomeItems, id: \.title) { item in
                HStack(alignment: .top, spacing: 10) {
                    Image(systemName: item.icon)
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(VPTheme.accent)
                        .frame(width: 18)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(item.title)
                            .font(VPTheme.font(11, weight: .semibold))
                        Text(item.detail)
                            .font(VPTheme.mono(8))
                            .foregroundStyle(VPTheme.textTertiary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
        }
    }

    private var welcomeItems: [(icon: String, title: String, detail: String)] {
        [
            (
                "cpu",
                L10n.t("Connect one model"),
                L10n
                    .t(
                        "Pick a provider; the base URL, model, and context window are filled for you. One API key is enough to start."
                    )
            ),
            (
                "folder",
                L10n.t("Choose a project"),
                L10n
                    .t(
                        "Agents work in isolated session worktrees. The project checkout stays untouched until you explicitly land a session."
                    )
            ),
            (
                "checkmark.shield",
                L10n.t("Safe defaults"),
                L10n
                    .t(
                        "Smart approvals, budgeted sessions, a Verifier gate, and a deterministic Integrator are on by default."
                    )
            ),
        ]
    }

    private var providerStep: some View {
        @Bindable var model = model

        return VStack(alignment: .leading, spacing: 12) {
            Text(L10n.t("Connect a model"))
                .font(VPTheme.font(14, weight: .semibold))
            Text(L10n
                .t(
                    "Only the provider and model are required. Context limits and compaction defaults are calculated automatically."
                ))
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)

            Picker(L10n.t("Provider"), selection: $selectedPreset) {
                ForEach(presets) { preset in
                    Text(preset.title).tag(preset.id)
                }
                Text(L10n.t("Custom endpoint")).tag("custom")
            }
            .onChange(of: selectedPreset) { _, value in
                guard didLoad else { return }
                apply(value)
            }

            if selectedPreset == "custom" {
                TextField(L10n.t("Base URL"), text: $model.providerBaseURL)
                    .textFieldStyle(.plain).vpFrostedField()
            }
            TextField(L10n.t("Model"), text: $model.providerModel)
                .textFieldStyle(.plain).vpFrostedField()
            if !isLocal {
                SecureField(
                    model.providerHasStoredKey
                        ? L10n.t("Replace stored API key")
                        : L10n.t("API key"),
                    text: $model.providerAPIKey
                )
                .textFieldStyle(.plain).vpFrostedField()
                Text(L10n.t("Stored in Keychain; sent only to the local daemon."))
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
            }

            HStack(spacing: 8) {
                Button(L10n.t("Save & test")) {
                    Task {
                        await model.saveProviderSettings()
                        if providerReady {
                            await model.probeProvider()
                        }
                    }
                }
                .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                .disabled(model.providerBusy || model.providerModel.isEmpty)
                if model.providerBusy || model.providerProbeBusy {
                    ProgressView().controlSize(.small)
                }
                Spacer()
                if providerReady {
                    Label(L10n.t("Connected"), systemImage: "checkmark.circle.fill")
                        .font(VPTheme.mono(8, weight: .semibold))
                        .foregroundStyle(VPTheme.success)
                }
            }
            if let status = model.providerStatus, !status.isEmpty {
                Text(status).font(VPTheme.mono(8)).foregroundStyle(VPTheme.textTertiary)
            }
            if let status = model.providerProbeStatus, !status.isEmpty {
                Text(status).font(VPTheme.mono(8)).foregroundStyle(VPTheme.textTertiary)
            }
        }
    }

    private var projectStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(L10n.t("Choose a project"))
                .font(VPTheme.font(14, weight: .semibold))
            Text(L10n
                .t(
                    "Pick a Git repository or a folder. Session worktrees are created under VerlinPark's data directory; the original checkout remains read-only to agents."
                ))
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)

            Button {
                Task { await model.addProject() }
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "folder.badge.plus")
                    Text(L10n.t("Choose project folder…"))
                }
            }
            .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))

            if model.projects.isEmpty {
                Text(L10n.t("No project yet."))
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
            } else {
                ForEach(model.projects) { project in
                    HStack(spacing: 8) {
                        Image(systemName: "folder.fill")
                            .foregroundStyle(VPTheme.plan)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(project.name)
                                .font(VPTheme.font(10.5, weight: .medium))
                            Text(project.root)
                                .font(VPTheme.mono(7))
                                .foregroundStyle(VPTheme.textTertiary)
                                .lineLimit(1)
                                .truncationMode(.middle)
                        }
                        Spacer()
                        Image(systemName: "checkmark.circle.fill")
                            .foregroundStyle(VPTheme.success)
                    }
                    .padding(9)
                    .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
                }
            }
        }
    }

    private var doneStep: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(L10n.t("You're ready"))
                .font(VPTheme.font(16, weight: .semibold))
            Text(L10n
                .t(
                    "Create a session and describe the outcome. The Leader will use a TeamPlan when the work benefits from multiple agents, and keep it direct when the task is small."
                ))
                .font(VPTheme.font(12))
                .foregroundStyle(VPTheme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
            VStack(alignment: .leading, spacing: 6) {
                summaryRow(providerReady, L10n.t("Model provider"))
                summaryRow(projectReady, L10n.t("Project"))
                summaryRow(true, L10n.t("Smart approvals · Verifier gate · deterministic Integrator"))
            }
        }
    }

    private var footer: some View {
        HStack {
            if step > 0 {
                Button(L10n.t("Back")) {
                    withAnimation(VPMotion.snap) { step -= 1 }
                }
                .buttonStyle(.bordered)
            }
            Spacer()
            Text("\(step + 1) / 4")
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
            Button(step == 3 ? L10n.t("Start building") : L10n.t("Continue")) {
                advance()
            }
            .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
            .disabled(step == 1 && !providerReady)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
    }

    private func summaryRow(_ ok: Bool, _ text: String) -> some View {
        HStack(spacing: 6) {
            Image(systemName: ok ? "checkmark.circle.fill" : "circle")
                .foregroundStyle(ok ? VPTheme.success : VPTheme.textTertiary)
            Text(text)
                .font(VPTheme.mono(8.5))
                .foregroundStyle(VPTheme.textSecondary)
        }
    }

    private func loadDefaults() {
        let url = model.providerBaseURL.lowercased()
        if url.contains("deepseek") {
            selectedPreset = "deepseek"
        } else if url.contains("api.openai.com") {
            selectedPreset = "openai"
        } else if url.contains("anthropic") {
            selectedPreset = "anthropic"
        } else if url.contains("127.0.0.1:1234") {
            selectedPreset = "lmstudio"
        } else if url.contains("127.0.0.1:11434") {
            selectedPreset = "ollama"
        } else if !url.isEmpty {
            selectedPreset = "custom"
        }
        if model.providerModel.isEmpty, let preset {
            apply(preset.id)
        }
        didLoad = true
    }

    private func apply(_ id: String) {
        guard let preset = presets.first(where: { $0.id == id }) else { return }
        model.providerKind = preset.kind
        model.providerBaseURL = preset.baseURL
        model.providerModel = preset.model
        model.providerContextWindowText = preset.context
    }

    private func advance() {
        switch step {
        case 0:
            step = 1
        case 1:
            step = 2
        case 2:
            step = 3
        default:
            finish()
        }
    }

    private func finish() {
        onFinish()
        dismiss()
    }
}
