import AppKit
import SwiftUI
import UserNotifications

enum SettingsSection: String, CaseIterable, Identifiable {
    case general
    case brand
    case resident
    case providers
    case acp
    case mcp
    case notifications
    case about

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .general: L10n.t("General")
        case .brand: L10n.t("Brand")
        case .resident: L10n.t("Resident")
        case .providers: L10n.t("Providers")
        case .acp: L10n.t("ACP")
        case .mcp: L10n.t("MCP")
        case .notifications: L10n.t("Notifications")
        case .about: L10n.t("About")
        }
    }

    var detail: String {
        switch self {
        case .general: L10n.t("Language and appearance")
        case .brand: L10n.t("Wordmark, flags, and live preview")
        case .resident: L10n.t("External Aviiya bridge")
        case .providers: L10n.t("Connect a model; advanced overrides")
        case .acp: L10n.t("External stdio agents")
        case .mcp: L10n.t("Local and remote tool servers")
        case .notifications: L10n.t("Permission and background alerts")
        case .about: L10n.t("Version, updates, and license")
        }
    }

    var icon: String {
        switch self {
        case .general: "globe"
        case .brand: "paintpalette"
        case .resident: "cube.transparent"
        case .providers: "network"
        case .acp: "person.crop.rectangle.stack"
        case .mcp: "puzzlepiece.extension"
        case .notifications: "bell"
        case .about: "info.circle"
        }
    }
}

struct SettingsView: View {
    @Environment(BrandStore.self) private var brand
    @Environment(AppModel.self) private var model
    @Environment(LanguageStore.self) private var language
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var section: SettingsSection = .brand
    @State private var hoveredSection: SettingsSection?
    @Namespace private var selectionNamespace
    @AppStorage("verlinpark.settings.section") private var storedSection =
        SettingsSection.brand.rawValue

    var body: some View {
        HStack(spacing: 0) {
            sidebar
            VPHairline(vertical: true)
            content
                .id(section)
                .transition(.vpStage)
                .animation(
                    VPMotion.resolved(VPMotion.smooth, reduceMotion: reduceMotion),
                    value: section
                )
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .frame(width: 900, height: 680)
        .preferredColorScheme(.dark)
        .tint(VPTheme.accent)
        .environment(\.locale, language.locale)
        .id(language.language.rawValue)
        .background { VPCrystalBackdrop() }
        .onAppear {
            if let saved = SettingsSection(rawValue: storedSection) {
                section = saved
            }
        }
        .onChange(of: section) { _, newValue in
            storedSection = newValue.rawValue
        }
    }

    private var sidebar: some View {
        VStack(alignment: .leading, spacing: 5) {
            VStack(alignment: .leading, spacing: 4) {
                Text("PREFERENCES")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .tracking(1.2)
                    .foregroundStyle(VPTheme.textTertiary)
                BrandMark(config: brand.config, fontSize: 19, maxWidth: 174)
                Text("Settings")
                    .font(VPTheme.font(10))
                    .foregroundStyle(VPTheme.textSecondary)
            }
            .padding(.bottom, 14)

            ForEach(Array(SettingsSection.allCases.enumerated()), id: \.element.id) { index, item in
                Button {
                    section = item
                } label: {
                    HStack(spacing: 9) {
                        Image(systemName: item.icon)
                            .font(.system(size: 11, weight: .semibold))
                            .frame(width: 17)
                            .foregroundStyle(
                                section == item ? VPTheme.accent : VPTheme.textTertiary
                            )
                        VStack(alignment: .leading, spacing: 1) {
                            Text(item.title)
                                .font(VPTheme.font(11, weight: section == item ? .semibold : .medium))
                            Text(item.detail)
                                .font(VPTheme.mono(7.3))
                                .foregroundStyle(VPTheme.textTertiary)
                                .lineLimit(1)
                                .minimumScaleFactor(0.82)
                        }
                        Spacer(minLength: 4)
                    }
                    .foregroundStyle(
                        section == item ? VPTheme.textPrimary : VPTheme.textSecondary
                    )
                    .padding(.horizontal, 9)
                    .padding(.vertical, 7)
                    .background {
                        if section == item {
                            RoundedRectangle(cornerRadius: 9, style: .continuous)
                                .fill(VPTheme.accent.opacity(0.10))
                                .overlay {
                                    RoundedRectangle(cornerRadius: 9, style: .continuous)
                                        .strokeBorder(VPTheme.accent.opacity(0.20), lineWidth: 1)
                                }
                                .matchedGeometryEffect(
                                    id: "settings-selection",
                                    in: selectionNamespace
                                )
                        } else if hoveredSection == item {
                            RoundedRectangle(cornerRadius: 9, style: .continuous)
                                .fill(VPTheme.surface)
                        }
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(VPPressableButtonStyle(scale: 0.988))
                .keyboardShortcut(
                    KeyEquivalent(Character(String(index + 1))),
                    modifiers: .command
                )
                .onHover { hovering in
                    if hovering {
                        hoveredSection = item
                    } else if hoveredSection == item {
                        hoveredSection = nil
                    }
                }
                .animation(VPMotion.hover, value: hoveredSection)
                .animation(VPMotion.snap, value: section)
                .help("\(item.detail) · ⌘\(index + 1)")
            }

            Spacer(minLength: 10)

            VPHairline()
                .padding(.bottom, 8)

            HStack(spacing: 6) {
                VPStatusDot(
                    color: model.connection == .connected ? VPTheme.success : VPTheme.warning,
                    pulsing: model.connection == .connecting,
                    size: 5
                )
                Text(L10n.t(model.connection == .connected ? "daemon connected" : "daemon offline"))
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            Text(L10n.t("⌘K command palette · ⌘1–⌘8 sections"))
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary.opacity(0.8))
                .padding(.bottom, 2)
        }
        .padding(16)
        .frame(width: 248)
        .background(VPTheme.rail)
    }

    @ViewBuilder
    private var content: some View {
        switch section {
        case .general:
            GeneralSettingsView()
        case .brand:
            BrandSettingsView()
        case .resident:
            ResidentSettingsView()
        case .providers:
            ProvidersSettingsView()
        case .acp:
            AcpSettingsView()
        case .mcp:
            McpSettingsView()
        case .notifications:
            NotificationsSettingsView()
        case .about:
            AboutSettingsView()
        }
    }
}

struct GeneralSettingsView: View {
    @Environment(LanguageStore.self) private var language
    @Environment(WorkspaceLayoutStore.self) private var layout
    @Environment(AppModel.self) private var model
    @State private var showSetupWizard = false

    var body: some View {
        @Bindable var layout = layout
        return ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(L10n.t("General"))
                        .font(VPTheme.font(22, weight: .semibold, rounded: true))
                        .foregroundStyle(VPTheme.textPrimary)
                    Text(L10n.t("Interface language and the workspace appearance."))
                        .font(VPTheme.font(10.5))
                        .foregroundStyle(VPTheme.textSecondary)
                }

                VStack(alignment: .leading, spacing: 9) {
                    SectionLabel(title: L10n.t("Interface language"))
                    HStack(spacing: 10) {
                        ForEach(AppLanguage.allCases) { option in
                            languageCard(option)
                        }
                    }
                    Text(
                        L10n.t(
                            "Language changes apply immediately. Daemon output and long technical provider messages may remain in English."
                        )
                    )
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
                }

                VStack(alignment: .leading, spacing: 9) {
                    SectionLabel(title: L10n.t("Agent permissions"))
                    HStack(spacing: 10) {
                        ForEach(AgentApprovalMode.allCases) { mode in
                            permissionCard(mode)
                        }
                    }
                    Text(
                        L10n.t(
                            "Smart keeps routine edits, builds, and tests moving. It asks only before operations that can change host state. Unattended never waits for a person and denies those operations."
                        )
                    )
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
                    .fixedSize(horizontal: false, vertical: true)
                    HStack {
                        Button(L10n.t("Run setup guide…")) {
                            showSetupWizard = true
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                        Spacer()
                    }
                    if !model.approvalGrants.isEmpty {
                        VStack(alignment: .leading, spacing: 4) {
                            HStack {
                                Text(L10n.t("Always-allowed paths").uppercased())
                                    .font(VPTheme.mono(6.5, weight: .semibold))
                                    .tracking(0.35)
                                    .foregroundStyle(VPTheme.textTertiary)
                                Spacer()
                                if model.approvalGrantBusy {
                                    ProgressView().controlSize(.mini)
                                }
                            }
                            ForEach(model.approvalGrants.prefix(6)) { grant in
                                HStack(spacing: 6) {
                                    Image(systemName: "checkmark.shield")
                                        .font(.system(size: 8))
                                        .foregroundStyle(VPTheme.success)
                                    VStack(alignment: .leading, spacing: 1) {
                                        Text(grant
                                            .pathPrefix + (grant.includeChildren ? " (subfolders)" : ""))
                                            .font(VPTheme.mono(8))
                                            .foregroundStyle(VPTheme.textSecondary)
                                            .lineLimit(1)
                                        Text("\(grant.tool) · \(grant.createdBy)")
                                            .font(VPTheme.mono(6.5))
                                            .foregroundStyle(VPTheme.textTertiary)
                                    }
                                    Spacer()
                                    Button {
                                        Task { await model.deleteApprovalGrant(grant) }
                                    } label: {
                                        Image(systemName: "xmark")
                                            .font(.system(size: 7, weight: .semibold))
                                            .foregroundStyle(VPTheme.textTertiary)
                                    }
                                    .buttonStyle(.plain)
                                    .help(L10n.t("Remove grant"))
                                }
                                .padding(.horizontal, 7)
                                .padding(.vertical, 4)
                                .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 6))
                            }
                        }
                        .padding(.top, 4)
                    }
                }

                VStack(alignment: .leading, spacing: 9) {
                    SectionLabel(title: L10n.t("Appearance"))
                    HStack(spacing: 10) {
                        ZStack {
                            RoundedRectangle(cornerRadius: 10, style: .continuous)
                                .fill(
                                    LinearGradient(
                                        colors: [VPTheme.surfaceRaised, VPTheme.canvas],
                                        startPoint: .topLeading,
                                        endPoint: .bottomTrailing
                                    )
                                )
                                .frame(width: 54, height: 38)
                                .overlay {
                                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                                        .strokeBorder(VPTheme.edgeGradient, lineWidth: 1)
                                }
                            Image(systemName: "cube.transparent")
                                .font(.system(size: 14, weight: .light))
                                .foregroundStyle(VPTheme.accent)
                        }
                        VStack(alignment: .leading, spacing: 2) {
                            Text(L10n.t("Black crystal"))
                                .font(VPTheme.font(11.5, weight: .semibold))
                                .foregroundStyle(VPTheme.textPrimary)
                            Text(L10n.t("Obsidian glass, ice accent, specular edges."))
                                .font(VPTheme.mono(8))
                                .foregroundStyle(VPTheme.textTertiary)
                        }
                        Spacer()
                        Text(L10n.t("DARK ONLY"))
                            .font(VPTheme.mono(7, weight: .semibold))
                            .tracking(0.6)
                            .foregroundStyle(VPTheme.textTertiary)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 2)
                            .background(VPTheme.surfaceRaised, in: Capsule())
                    }
                    .padding(11)
                    .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 11, style: .continuous))
                    .overlay {
                        RoundedRectangle(cornerRadius: 11, style: .continuous)
                            .strokeBorder(VPTheme.hairline, lineWidth: 1)
                    }
                }

                VStack(alignment: .leading, spacing: 9) {
                    SectionLabel(title: L10n.t("Workspace layout"))
                    HStack(spacing: 10) {
                        ForEach(WorkspaceLayoutPreset.allCases) { preset in
                            layoutCard(preset)
                        }
                    }
                    Text(
                        L10n.t(
                            "Presets update the persisted rail and inspector sizes. Drag either divider or double-click it to reset one side."
                        )
                    )
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)

                    VStack(spacing: 7) {
                        widthControl(
                            L10n.t("Rail width"),
                            value: $layout.railWidth,
                            range: Double(WorkspaceLayout.railRange.lowerBound)
                                ... Double(WorkspaceLayout.railRange.upperBound)
                        )
                        widthControl(
                            L10n.t("Inspector width"),
                            value: $layout.inspectorWidth,
                            range: Double(WorkspaceLayout.inspectorRange.lowerBound)
                                ... Double(WorkspaceLayout.inspectorRange.upperBound)
                        )
                        Toggle(L10n.t("Show inspector"), isOn: $layout.inspectorVisible)
                            .toggleStyle(.switch)
                            .controlSize(.small)
                            .font(VPTheme.font(10, weight: .medium))
                    }
                    .padding(.top, 2)
                }

                VStack(alignment: .leading, spacing: 9) {
                    SectionLabel(title: L10n.t("Workspace"))
                    HStack(spacing: 14) {
                        Label(L10n.t("Isolated worktrees"), systemImage: "arrow.triangle.branch")
                        Label(L10n.t("Verifier gate"), systemImage: "checkmark.shield")
                        Label(L10n.t("Deterministic merge"), systemImage: "arrow.triangle.merge")
                    }
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
                }
            }
            .padding(24)
            .frame(maxWidth: 780, alignment: .leading)
            .frame(maxWidth: .infinity, alignment: .center)
        }
        .task {
            await model.loadApprovalGrants()
        }
        .sheet(isPresented: $showSetupWizard) {
            SetupWizardView {
                UserDefaults.standard.set(true, forKey: "verlinpark.onboarding.completed")
            }
            .environment(model)
        }
    }

    private func widthControl(
        _ title: String,
        value: Binding<Double>,
        range: ClosedRange<Double>
    ) -> some View {
        HStack(spacing: 10) {
            Text(title)
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textSecondary)
                .frame(width: 100, alignment: .leading)
            Slider(value: value, in: range)
                .controlSize(.small)
            Text("\(Int(value.wrappedValue)) pt")
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
                .frame(width: 48, alignment: .trailing)
                .contentTransition(.numericText())
        }
    }

    private func permissionCard(_ mode: AgentApprovalMode) -> some View {
        let selected = model.approvalMode == mode
        return Button {
            withAnimation(VPMotion.smooth) {
                model.approvalMode = mode
            }
            Task { await model.applyApprovalMode() }
        } label: {
            VStack(alignment: .leading, spacing: 7) {
                HStack {
                    Image(systemName: mode.icon)
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(selected ? permissionTint(mode) : VPTheme.textTertiary)
                    Spacer()
                    Image(systemName: selected ? "checkmark.circle.fill" : "circle")
                        .font(.system(size: 12))
                        .foregroundStyle(selected ? permissionTint(mode) : VPTheme.textTertiary)
                }
                Text(mode.title)
                    .font(VPTheme.font(11.5, weight: .semibold))
                    .foregroundStyle(VPTheme.textPrimary)
                Text(mode.detail)
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .padding(11)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                selected ? permissionTint(mode).opacity(0.08) : VPTheme.surface,
                in: RoundedRectangle(cornerRadius: 10, style: .continuous)
            )
            .overlay {
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .strokeBorder(
                        selected ? permissionTint(mode).opacity(0.30) : VPTheme.hairline,
                        lineWidth: 1
                    )
            }
        }
        .buttonStyle(VPPressableButtonStyle(scale: 0.99))
    }

    private func permissionTint(_ mode: AgentApprovalMode) -> Color {
        switch mode {
        case .smart: VPTheme.success
        case .strict: VPTheme.warning
        case .unattended: VPTheme.info
        }
    }

    private func layoutCard(_ preset: WorkspaceLayoutPreset) -> some View {
        let selected = layout.currentPreset == preset
        return Button {
            withAnimation(VPMotion.smooth) {
                layout.apply(preset)
            }
        } label: {
            VStack(alignment: .leading, spacing: 7) {
                HStack {
                    Image(systemName: preset.icon)
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(selected ? VPTheme.accent : VPTheme.textTertiary)
                    Spacer()
                    Image(systemName: selected ? "checkmark.circle.fill" : "circle")
                        .font(.system(size: 12))
                        .foregroundStyle(selected ? VPTheme.accent : VPTheme.textTertiary)
                }
                Text(preset.title)
                    .font(VPTheme.font(11.5, weight: .semibold))
                    .foregroundStyle(VPTheme.textPrimary)
                Text(preset.detail)
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .padding(11)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                selected ? VPTheme.accent.opacity(0.09) : VPTheme.surface,
                in: RoundedRectangle(cornerRadius: 10, style: .continuous)
            )
            .overlay {
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .strokeBorder(
                        selected ? VPTheme.accent.opacity(0.28) : VPTheme.hairline,
                        lineWidth: 1
                    )
            }
        }
        .buttonStyle(VPPressableButtonStyle(scale: 0.99))
    }

    private func languageCard(_ option: AppLanguage) -> some View {
        let selected = language.language == option
        return Button {
            language.select(option)
        } label: {
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(option.endonym)
                        .font(VPTheme.font(13, weight: .semibold))
                        .foregroundStyle(selected ? VPTheme.textPrimary : VPTheme.textSecondary)
                    Text(option == .english ? "English" : "Simplified Chinese")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                Spacer(minLength: 6)
                Image(systemName: selected ? "checkmark.circle.fill" : "circle")
                    .font(.system(size: 13))
                    .foregroundStyle(selected ? VPTheme.accent : VPTheme.textTertiary)
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                selected ? VPTheme.accent.opacity(0.09) : VPTheme.surface,
                in: RoundedRectangle(cornerRadius: 11, style: .continuous)
            )
            .overlay {
                RoundedRectangle(cornerRadius: 11, style: .continuous)
                    .strokeBorder(
                        selected ? VPTheme.accent.opacity(0.30) : VPTheme.hairline,
                        lineWidth: 1
                    )
            }
        }
        .buttonStyle(VPPressableButtonStyle(scale: 0.99))
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
                        .textFieldStyle(.plain).vpFrostedField()
                        .font(.system(size: 13))
                    HStack(spacing: 6) {
                        ForEach(BrandConfig.presets, id: \.self) { preset in
                            Button(preset) {
                                brand.config.displayName = preset
                            }
                            .buttonStyle(VPPressableButtonStyle(scale: 0.95))
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
                    VPSegmentedControl(
                        segments: BrandStyle.allCases.map {
                            VPSegment($0.rawValue, title: $0.title, icon: $0.icon)
                        },
                        selection: Binding(
                            get: { brand.config.style.rawValue },
                            set: { brand.config.style = BrandStyle(rawValue: $0) ?? .solid }
                        ),
                        compact: true
                    )

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
                    .vpAppear(index: 3, animation: VPMotion.emphasizedSpring)
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
                    .textFieldStyle(.plain).vpFrostedField()
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
                                                isSelected(option) ? VPTheme.accent : VPTheme.hairline,
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
        .background { VPCrystalBackdrop() }
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
                Image(systemName: "cube.transparent")
                    .font(.system(size: 18, weight: .bold))
                    .foregroundStyle(VPTheme.resident)
                    .frame(width: 38, height: 38)
                    .background(VPTheme.resident.opacity(0.12), in: Circle())
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
    @State private var showingModelPresets = false
    @State private var selectedPreset = "auto"
    @State private var showAdvanced = false
    @State private var didLoadPreset = false

    private struct ProviderPreset: Identifiable {
        let id: String
        let title: String
        let kind: String
        let baseURL: String
        let model: String
        let contextWindow: String
        let needsKey: Bool
        let detail: String
    }

    private let presets: [ProviderPreset] = [
        ProviderPreset(
            id: "deepseek",
            title: "DeepSeek",
            kind: "openai-compatible",
            baseURL: "https://api.deepseek.com",
            model: "deepseek-chat",
            contextWindow: "128000",
            needsKey: true,
            detail: "OpenAI-compatible endpoint. Context and model are prefilled; change the model to deepseek-flash if you prefer it."
        ),
        ProviderPreset(
            id: "openai",
            title: "OpenAI",
            kind: "openai-compatible",
            baseURL: "https://api.openai.com/v1",
            model: "gpt-4o-mini",
            contextWindow: "128000",
            needsKey: true,
            detail: "Official OpenAI endpoint."
        ),
        ProviderPreset(
            id: "anthropic",
            title: "Anthropic",
            kind: "anthropic",
            baseURL: "https://api.anthropic.com/v1",
            model: "claude-3-5-sonnet-latest",
            contextWindow: "200000",
            needsKey: true,
            detail: "Native Anthropic Messages API."
        ),
        ProviderPreset(
            id: "lmstudio",
            title: "LM Studio",
            kind: "openai-compatible",
            baseURL: "http://127.0.0.1:1234/v1",
            model: "",
            contextWindow: "32000",
            needsKey: false,
            detail: "Local OpenAI-compatible server. No API key is needed."
        ),
        ProviderPreset(
            id: "ollama",
            title: "Ollama",
            kind: "openai-compatible",
            baseURL: "http://127.0.0.1:11434/v1",
            model: "",
            contextWindow: "32000",
            needsKey: false,
            detail: "Local OpenAI-compatible server. Pick the model name you have pulled."
        ),
        ProviderPreset(
            id: "custom",
            title: "Custom endpoint",
            kind: "openai-compatible",
            baseURL: "",
            model: "",
            contextWindow: "128000",
            needsKey: true,
            detail: "Any OpenAI-compatible endpoint. Set the base URL, model, and key."
        ),
    ]

    private var currentPreset: ProviderPreset? {
        presets.first { $0.id == selectedPreset }
    }

    private var isLocalEndpoint: Bool {
        let url = model.providerBaseURL.lowercased()
        return url.contains("127.0.0.1")
            || url.contains("localhost")
            || url.contains("0.0.0.0")
            || (currentPreset?.needsKey == false)
    }

    private func defaultBaseURL(for kind: String) -> String {
        kind == "anthropic"
            ? "https://api.anthropic.com/v1"
            : "https://api.openai.com/v1"
    }

    private func detectPreset() -> String {
        let url = model.providerBaseURL.lowercased()
        if url.contains("deepseek") {
            return "deepseek"
        }
        if url.contains("api.openai.com") {
            return "openai"
        }
        if url.contains("anthropic") {
            return "anthropic"
        }
        if url.contains("127.0.0.1:1234") {
            return "lmstudio"
        }
        if url.contains("127.0.0.1:11434") {
            return "ollama"
        }
        return "custom"
    }

    private func inferredContext(_ modelName: String) -> String {
        let lower = modelName.lowercased()
        if lower.contains("claude") {
            return "200000"
        }
        if lower.contains("gemini") {
            return "1000000"
        }
        if lower.contains("deepseek") {
            return "128000"
        }
        if lower.contains("gpt-4o") || lower.contains("gpt-4.1") {
            return "128000"
        }
        if lower.hasPrefix("o1") || lower.hasPrefix("o3") || lower.hasPrefix("o4") {
            return "200000"
        }
        if lower.contains("qwen") {
            return "131072"
        }
        return currentPreset?.contextWindow ?? "128000"
    }

    private func applyPreset(_ id: String, force: Bool) {
        guard id != "auto", let preset = presets.first(where: { $0.id == id }) else { return }
        model.providerKind = preset.kind
        if force || model.providerBaseURL.isEmpty {
            model.providerBaseURL = preset.baseURL
        }
        if force || model.providerModel.isEmpty {
            model.providerModel = preset.model
        }
        if force || model.providerContextWindowText.isEmpty {
            model.providerContextWindowText = preset.contextWindow
        }
    }

    var body: some View {
        @Bindable var model = model

        Form {
            Section("Connect a model") {
                Picker("Provider", selection: $selectedPreset) {
                    Text("Auto-detect").tag("auto")
                    ForEach(presets) { preset in
                        Text(preset.title).tag(preset.id)
                    }
                }
                .onChange(of: selectedPreset) { _, value in
                    guard didLoadPreset else { return }
                    applyPreset(value, force: true)
                }

                TextField("Model", text: $model.providerModel)
                    .textFieldStyle(.plain).vpFrostedField()
                    .onChange(of: model.providerModel) { _, value in
                        guard didLoadPreset, !showAdvanced else { return }
                        model.providerContextWindowText = inferredContext(value)
                    }

                if !isLocalEndpoint {
                    SecureField(
                        model.providerHasStoredKey
                            ? "Replace stored API key"
                            : "Paste API key",
                        text: $model.providerAPIKey
                    )
                    .textFieldStyle(.plain).vpFrostedField()
                }

                HStack(spacing: 8) {
                    Button(L10n.t(model.providerHasStoredKey ? "Save & Reconnect" : "Save & Connect")) {
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
                        Text(info.configured ? (info.model ?? L10n.t("configured")) : L10n
                            .t("not configured"))
                            .font(.system(size: 11, design: .monospaced))
                            .foregroundStyle(info.configured ? Color.green : Color.orange)
                    }
                }

                Text(currentPreset?.detail ?? L10n
                    .t(
                        "Pick a preset; model and context are filled automatically. Only the API key is required for remote providers."
                    ))
                    .font(.caption)
                    .foregroundStyle(.secondary)

                if let status = model.providerStatus {
                    Text(status)
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .contentTransition(.opacity)
                        .animation(VPMotion.snap, value: status)
                }
                if let status = model.providerProbeStatus {
                    Text(status)
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .contentTransition(.opacity)
                        .animation(VPMotion.snap, value: status)
                }

                Toggle("Show advanced settings", isOn: $showAdvanced)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                Text(
                    "Advanced contains base URL, context window overrides, role models, team capacity, and session defaults. You normally do not need it."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }

            if showAdvanced {
                Section("Connection details") {
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
                        .textFieldStyle(.plain).vpFrostedField()
                    HStack(spacing: 8) {
                        TextField("Context window (tokens)", text: $model.providerContextWindowText)
                            .textFieldStyle(.plain).vpFrostedField()
                        Menu("Presets") {
                            Button("Off") { model.providerContextWindowText = "0" }
                            Button("8k") { model.providerContextWindowText = "8000" }
                            Button("16k") { model.providerContextWindowText = "16000" }
                            Button("32k") { model.providerContextWindowText = "32000" }
                            Button("64k") { model.providerContextWindowText = "64000" }
                            Button("128k") { model.providerContextWindowText = "128000" }
                            Button("200k") { model.providerContextWindowText = "200000" }
                            Button("1M") { model.providerContextWindowText = "1000000" }
                            Button("2M") { model.providerContextWindowText = "2000000" }
                        }
                        .menuStyle(.borderlessButton)
                        .fixedSize()
                        .controlSize(.small)
                    }
                    Text(
                        "Auto-filled from the selected provider/model. Change it only when your endpoint has a different window. 0 disables automatic context management."
                    )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    if !isLocalEndpoint {
                        SecureField(
                            model.providerHasStoredKey
                                ? "Replace stored API key"
                                : "API key",
                            text: $model.providerAPIKey
                        )
                        .textFieldStyle(.plain).vpFrostedField()
                        Text(
                            L10n.t(
                                model.providerHasStoredKey
                                    ? "A key is stored in Keychain. Leave this empty to keep it."
                                    : "The key is stored in Keychain and never written to SQLite or logs."
                            )
                        )
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    }
                    if model.providerHasStoredKey {
                        Button(L10n.t("Remove stored key")) {
                            model.clearStoredProviderKey()
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                    }
                }

                Section("Role model overrides") {
                    Text(
                        "Blank uses the base Leader model. Prices are USD per 1M tokens and are only used when a USD session budget is set. Per-role context window overrides the global value; blank uses the global model window."
                    )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    RoleModelRow(title: "Leader", config: $model.roleModels.leader)
                    RoleModelRow(title: "Manager", config: $model.roleModels.manager)
                    RoleModelRow(title: "Worker", config: $model.roleModels.worker)
                    RoleModelRow(title: "Verifier", config: $model.roleModels.verifier)
                    RoleModelRow(title: "Labour", config: $model.roleModels.labour)
                    Button("Manage Model Presets…") {
                        showingModelPresets = true
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                }

                Section("Team capacity") {
                    Stepper("Managers: \(model.orgManagers)", value: $model.orgManagers, in: 1 ... 5)
                    Stepper("Workers: \(model.orgWorkers)", value: $model.orgWorkers, in: 1 ... 10)
                    Stepper("Labour: \(model.orgLabour)", value: $model.orgLabour, in: 1 ... 40)
                    Text(
                        "Per-session hard caps. Defaults are 4 Managers / 6 Workers / 12 Labour; runtime limits are 5 / 10 / 40. Applying also re-saves the current provider configuration."
                    )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    Toggle("Auto-expand plan Workers", isOn: $model.autoExpandWorkers)
                        .toggleStyle(.switch)
                        .controlSize(.small)
                    Text(
                        "When a plan has more ready tasks than the base Worker cap, expand Workers up to the hard cap of 10 while at least 20% of the session token budget remains. Off by default."
                    )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    Button("Apply capacity") {
                        Task { await model.saveOrgCaps() }
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                    .disabled(model.providerBusy)
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
                        .textFieldStyle(.plain).vpFrostedField()
                        .frame(width: 160)
                    Text(
                        "Input + output token budget for new sessions. Existing sessions keep their own budget. Provider calls reserve before they start and settle from reported usage, so the cap holds across concurrent batches."
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
        }
        .formStyle(.grouped)
        .scrollContentBackground(.hidden)
        .padding(24)
        .frame(maxWidth: 920)
        .frame(maxWidth: .infinity)
        .sheet(isPresented: $showingModelPresets) {
            ModelPresetsSheet()
                .environment(model)
        }
        .onAppear {
            if selectedPreset == "auto" {
                selectedPreset = detectPreset()
            }
            if model.providerContextWindowText.isEmpty {
                model.providerContextWindowText = inferredContext(model.providerModel)
            }
            didLoadPreset = true
        }
    }
}

private struct ModelPresetsSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var confirmingDelete = false
    @State private var hoveredPresetID: String?
    @Namespace private var presetNamespace

    private let roles = ["leader", "manager", "worker", "verifier", "labour"]

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Model presets")
                        .font(.system(size: 18, weight: .semibold, design: .rounded))
                    Text("Reusable role metadata. No API key is stored.")
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
                Spacer()
                if model.modelPresetsBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Done") { dismiss() }
                    .keyboardShortcut(.defaultAction)
            }
            .padding(18)
            VPGradientRule()
                .frame(height: 1)
            HSplitView {
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Button("New") { model.newModelPresetDraft() }
                            .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                        Button("Refresh") {
                            Task { await model.loadModelPresets() }
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                    }
                    ScrollView {
                        VStack(alignment: .leading, spacing: 6) {
                            ForEach(model.modelPresets) { preset in
                                Button {
                                    model.selectModelPreset(preset)
                                } label: {
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(preset.name)
                                            .font(.system(size: 10, weight: .medium, design: .monospaced))
                                        Text("\(preset.provider) · \(preset.model)")
                                            .font(.system(size: 8, design: .monospaced))
                                            .foregroundStyle(.secondary)
                                    }
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .padding(7)
                                    .background {
                                        if model.selectedModelPresetID == preset.id {
                                            RoundedRectangle(cornerRadius: 7)
                                                .fill(VPTheme.accent.opacity(0.18))
                                                .matchedGeometryEffect(
                                                    id: "preset-selection",
                                                    in: presetNamespace
                                                )
                                        } else if hoveredPresetID == preset.id {
                                            RoundedRectangle(cornerRadius: 7)
                                                .fill(.white.opacity(0.05))
                                        }
                                    }
                                }
                                .buttonStyle(VPPressableButtonStyle(scale: 0.98))
                                .onHover { hovering in
                                    if hovering {
                                        hoveredPresetID = preset.id
                                    } else if hoveredPresetID == preset.id {
                                        hoveredPresetID = nil
                                    }
                                }
                                .animation(VPMotion.hover, value: hoveredPresetID)
                                .animation(VPMotion.snap, value: model.selectedModelPresetID)
                                .vpAppear(index: 0)
                            }
                            if model.modelPresets.isEmpty {
                                Text("No presets yet.")
                                    .font(.system(size: 9, design: .monospaced))
                                    .foregroundStyle(.tertiary)
                            }
                        }
                    }
                }
                .padding(16)
                .frame(minWidth: 220, maxWidth: 260)
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        Text("PRESET")
                            .font(.system(size: 9, weight: .semibold, design: .monospaced))
                            .foregroundStyle(.tertiary)
                        TextField("Name", text: $model.modelPresetName)
                            .textFieldStyle(.plain).vpFrostedField()
                        Picker("Provider", selection: $model.modelPresetProvider) {
                            Text("OpenAI-compatible").tag("openai-compatible")
                            Text("Anthropic").tag("anthropic")
                        }
                        TextField("Model", text: $model.modelPresetModel)
                            .textFieldStyle(.plain).vpFrostedField()
                        HStack(spacing: 8) {
                            TextField("Context window", text: $model.modelPresetContextText)
                                .textFieldStyle(.plain).vpFrostedField()
                            TextField("Input $/1M", text: $model.modelPresetInputPriceText)
                                .textFieldStyle(.plain).vpFrostedField()
                            TextField("Output $/1M", text: $model.modelPresetOutputPriceText)
                                .textFieldStyle(.plain).vpFrostedField()
                        }
                        TextField("Notes", text: $model.modelPresetNotes, axis: .vertical)
                            .textFieldStyle(.plain).vpFrostedField()
                            .lineLimit(2 ... 3)
                        HStack {
                            Button("Save preset") {
                                Task { await model.saveModelPreset() }
                            }
                            .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
                            .disabled(model.modelPresetsBusy)
                            Button("Delete") { confirmingDelete = true }
                                .buttonStyle(.bordered)
                                .disabled(model.selectedModelPresetID == nil || model.modelPresetsBusy)
                            Spacer()
                        }
                        Divider().opacity(0.25)
                        Text("APPLY TO ROLE")
                            .font(.system(size: 9, weight: .semibold, design: .monospaced))
                            .foregroundStyle(.tertiary)
                        HStack(spacing: 6) {
                            ForEach(roles, id: \.self) { role in
                                Button(role.capitalized) {
                                    if let preset = model.modelPresets.first(where: {
                                        $0.id == model.selectedModelPresetID
                                    }) {
                                        model.applyModelPreset(preset, to: role)
                                    }
                                }
                                .buttonStyle(.bordered)
                                .controlSize(.small)
                                .disabled(model.selectedModelPresetID == nil)
                            }
                        }
                        Text(
                            "Applying fills the role model/context/price fields. Provider and API key are unchanged; use Save & Connect on the Providers tab to persist."
                        )
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.secondary)
                        if let status = model.modelPresetsStatus {
                            Text(status)
                                .font(.system(size: 9, design: .monospaced))
                                .foregroundStyle(.secondary)
                                .contentTransition(.opacity)
                                .animation(VPMotion.snap, value: status)
                        }
                    }
                    .padding(18)
                }
                .frame(minWidth: 420)
            }
        }
        .frame(width: 760, height: 640)
        .onAppear {
            Task {
                await model.loadModelPresets()
                if model.modelPresets.isEmpty {
                    model.newModelPresetDraft()
                }
            }
        }
        .confirmationDialog(
            "Delete this model preset?",
            isPresented: $confirmingDelete
        ) {
            Button("Delete", role: .destructive) {
                Task { await model.deleteModelPreset() }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("This does not change the current provider or role configuration.")
        }
    }
}

private struct RoleModelRow: View {
    let title: String
    @Binding var config: RoleModelConfig

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 9) {
                Text(L10n.t(title))
                    .font(VPTheme.font(11, weight: .semibold))
                    .foregroundStyle(VPTheme.textSecondary)
                    .frame(width: 66, alignment: .leading)

                TextField("Override base model", text: modelBinding)
                    .textFieldStyle(.plain)
                    .font(VPTheme.mono(10))
                    .foregroundStyle(VPTheme.textPrimary)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 5)
                    .vpFieldSurface(radius: 7)
            }

            HStack(alignment: .top, spacing: 8) {
                metricField(
                    "Input $ / 1M",
                    placeholder: "0",
                    text: priceBinding(\.inputUsdPerMillion)
                )
                metricField(
                    "Output $ / 1M",
                    placeholder: "0",
                    text: priceBinding(\.outputUsdPerMillion)
                )
                metricField(
                    "Context window",
                    placeholder: "base",
                    text: contextBinding
                )
            }
            .padding(.leading, 75)
        }
        .padding(9)
        .background(
            VPTheme.surfaceSunken.opacity(0.55),
            in: RoundedRectangle(cornerRadius: 9, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .strokeBorder(VPTheme.edgeGradient, lineWidth: 1)
        }
    }

    private func metricField(
        _ label: String,
        placeholder: String,
        text: Binding<String>
    ) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(L10n.t(label).uppercased())
                .font(VPTheme.mono(7, weight: .medium))
                .tracking(0.5)
                .foregroundStyle(VPTheme.textTertiary)
            TextField(placeholder, text: text)
                .textFieldStyle(.plain)
                .font(VPTheme.mono(10))
                .foregroundStyle(VPTheme.textPrimary)
                .padding(.horizontal, 7)
                .padding(.vertical, 5)
                .vpFieldSurface(radius: 6)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var contextBinding: Binding<String> {
        Binding(
            get: {
                guard let value = config.contextWindow else { return "" }
                return String(value)
            },
            set: {
                let trimmed = $0.trimmingCharacters(in: .whitespaces)
                config.contextWindow =
                    (Int(trimmed).flatMap { $0 > 0 ? min($0, 10_000_000) : nil })
            }
        )
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

struct NotificationsSettingsView: View {
    @AppStorage(SystemNotificationPreference.key) private var enabled = true
    @State private var authorization = "Not requested"

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 4) {
                Text("macOS notifications")
                    .font(.system(size: 22, weight: .semibold, design: .rounded))
                Text(
                    "Approvals, task failures, integration failures, and stopped sessions can also appear as local notifications while VerlinPark is in the background."
                )
                .font(.system(size: 10))
                .foregroundStyle(.secondary)
            }
            Toggle("Send system notifications", isOn: $enabled)
                .toggleStyle(.switch)
                .controlSize(.small)
            HStack(spacing: 10) {
                Text("Permission: \(authorization)")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
                Button("Request permission") {
                    Task {
                        SystemNotifier.shared.requestAuthorization()
                        try? await Task.sleep(for: .milliseconds(500))
                        await refreshAuthorization()
                    }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                Button("Open System Settings") {
                    if let url = URL(
                        string: "x-apple.systempreferences:com.apple.preference.notifications"
                    ) {
                        NSWorkspace.shared.open(url)
                    }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
            }
            Text(
                "The in-app Rail always records notifications. System notifications are suppressed while the app is active and are deduplicated by the same notice id."
            )
            .font(.system(size: 9, design: .monospaced))
            .foregroundStyle(.tertiary)
            Spacer()
        }
        .padding(22)
        .task {
            await refreshAuthorization()
        }
    }

    private func refreshAuthorization() async {
        let label = await withCheckedContinuation { (continuation: CheckedContinuation<String, Never>) in
            UNUserNotificationCenter.current().getNotificationSettings { settings in
                let value = switch settings.authorizationStatus {
                case .authorized: "authorized"
                case .denied: "denied"
                case .notDetermined: "not determined"
                case .provisional: "provisional"
                case .ephemeral: "ephemeral"
                @unknown default: "unknown"
                }
                continuation.resume(returning: value)
            }
        }
        authorization = label
    }
}

struct AboutSettingsView: View {
    @Environment(BrandStore.self) private var brand
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(spacing: 16) {
            BrandMark(config: brand.config, fontSize: 28)
                .vpAppear(index: 0, animation: VPMotion.emphasizedSpring)
            Text("VerlinPark")
                .font(.title2.weight(.semibold))
            Text("A native macOS workbench for agent teams.")
                .font(.callout)
                .foregroundStyle(.secondary)
            Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 7) {
                GridRow {
                    Text("Version")
                    Text(model.updateResult?.currentVersion ?? "0.1.0-dev")
                        .foregroundStyle(.secondary)
                }
                GridRow { Text("License"); Text("GPL-3.0-only").foregroundStyle(.secondary) }
                GridRow { Text("Protocol"); Text("1").foregroundStyle(.secondary) }
            }
            .font(.system(size: 11, design: .monospaced))

            Divider().opacity(0.3).frame(maxWidth: 320)
            VStack(spacing: 8) {
                HStack(spacing: 10) {
                    Button("Check for Updates") {
                        Task { await model.checkForUpdates() }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(model.updateBusy)
                    if model.updateBusy {
                        ProgressView().controlSize(.small)
                    }
                }
                if let error = model.updateError {
                    Text(error)
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.orange)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: 360)
                } else if let update = model.updateResult {
                    if update.updateAvailable {
                        Text("Update available: \(update.latestVersion)")
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundStyle(.orange)
                            .contentTransition(.numericText())
                            .animation(VPMotion.snap, value: update.latestVersion)
                        if let url = update.releaseUrl,
                           let parsed = URL(string: url)
                        {
                            Button("Open Release Page") {
                                NSWorkspace.shared.open(parsed)
                            }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                        }
                        if update.dmgUrl != nil, update.sha256Url != nil {
                            HStack(spacing: 8) {
                                Button(model.updateDownloadBusy ? "Downloading…" : "Download DMG") {
                                    Task { await model.downloadUpdate() }
                                }
                                .buttonStyle(VPPrimaryButtonStyle(tint: .orange))
                                .disabled(model.updateDownloadBusy)
                                if model.updateDownloadBusy {
                                    ProgressView().controlSize(.small)
                                }
                            }
                        } else {
                            Text("Release does not provide both DMG and SHA-256 assets.")
                                .font(.system(size: 9, design: .monospaced))
                                .foregroundStyle(.tertiary)
                        }
                    } else {
                        Text("You're up to date.")
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundStyle(.secondary)
                    }
                    Text("\(update.repo) · latest \(update.latestVersion)")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    if let downloaded = model.updateDownloadResult {
                        Text("Saved \(downloaded.version) to \(downloaded.path)")
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.secondary)
                            .lineLimit(2)
                        Button("Show in Finder") {
                            NSWorkspace.shared.activateFileViewerSelecting([
                                URL(fileURLWithPath: downloaded.path),
                            ])
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                    }
                    if let error = model.updateDownloadError {
                        Text(error)
                            .font(.system(size: 9, design: .monospaced))
                            .foregroundStyle(.orange)
                            .multilineTextAlignment(.center)
                            .frame(maxWidth: 380)
                    }
                }
            }
            Spacer()
            Text("Agent teams, not chat tabs.")
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(.tertiary)
        }
        .padding(28)
    }
}
