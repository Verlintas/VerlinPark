import SwiftUI

// MARK: - Stage runway card

/// Compact Stage surface for the selected session's budget mode, caps, spend,
/// deterministic forecast, auto-extension count, and pause/no-progress reason.
/// Every value comes from `session.budget.get`; the card never infers a budget.
struct StageBudgetCard: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let summary: BudgetRunwaySummary
    let onEdit: () -> Void

    @State private var expanded = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            header
            if expanded {
                Divider().overlay(VPTheme.hairline)
                metrics
                forecastRow
            }
            if summary.isPaused {
                pauseBanner
            }
            if let error = model.sessionBudgetError {
                HStack(alignment: .top, spacing: 6) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .font(.system(size: 8, weight: .semibold))
                    Text(error.displayText)
                        .font(VPTheme.mono(7.5))
                        .fixedSize(horizontal: false, vertical: true)
                }
                .foregroundStyle(VPTheme.warning)
            }
        }
        .padding(.horizontal, 11)
        .padding(.vertical, 8)
        .background(
            VPTheme.surface.opacity(0.82),
            in: RoundedRectangle(cornerRadius: 11, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 11, style: .continuous)
                .strokeBorder(tint.opacity(summary.needsAttention ? 0.32 : 0.14), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.001, lift: 1)
        .onAppear {
            if summary.isPaused || summary.tone == .critical {
                expanded = true
            }
        }
        .onChange(of: summary.isPaused) { _, paused in
            if paused {
                expanded = true
            }
        }
        .animation(
            VPMotion.resolved(VPMotion.snap, reduceMotion: reduceMotion),
            value: expanded
        )
    }

    private var tint: Color {
        switch summary.tone {
        case .normal: VPTheme.accent
        case .warning: VPTheme.warning
        case .critical: VPTheme.danger
        case .paused: VPTheme.warning
        case .unlimited: VPTheme.info
        }
    }

    private var header: some View {
        HStack(spacing: 8) {
            Image(systemName: icon)
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(tint)
                .frame(width: 12)
            Text("BUDGET")
                .font(VPTheme.mono(7, weight: .semibold))
                .tracking(1.0)
                .foregroundStyle(VPTheme.textTertiary)
            Text(summary.mode.label.uppercased())
                .font(VPTheme.mono(7, weight: .semibold))
                .tracking(0.5)
                .foregroundStyle(tint)
                .padding(.horizontal, 6)
                .padding(.vertical, 2)
                .background(tint.opacity(0.11), in: Capsule())
                .overlay {
                    Capsule().strokeBorder(tint.opacity(0.24), lineWidth: 1)
                }
            Text(summary.headline)
                .font(VPTheme.mono(8.5, weight: .medium))
                .foregroundStyle(VPTheme.textSecondary)
                .lineLimit(1)
            if summary.extensionCount > 0 {
                Label(
                    "\(summary.extensionCount) ext",
                    systemImage: "arrow.up.forward.circle"
                )
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textTertiary)
                .help("\(summary.extensionCount) deterministic auto-budget extension(s)")
            }
            if summary.isPaused {
                Text(summary.pauseReasonLabel ?? "AUTO-PAUSED")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.warning)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }
            Spacer(minLength: 6)
            if model.sessionBudgetBusy {
                ProgressView().controlSize(.mini)
            }
            Button {
                Task { await model.loadSessionBudget() }
            } label: {
                Image(systemName: "arrow.clockwise")
                    .font(.system(size: 8, weight: .semibold))
            }
            .buttonStyle(.plain)
            .foregroundStyle(VPTheme.textTertiary)
            .help("Refresh budget, spend, and forecast")
            .disabled(model.sessionBudgetBusy)
            Button {
                expanded.toggle()
            } label: {
                Image(systemName: expanded ? "chevron.up" : "chevron.down")
                    .font(.system(size: 8, weight: .semibold))
            }
            .buttonStyle(.plain)
            .foregroundStyle(VPTheme.textTertiary)
            .help(expanded ? "Hide budget details" : "Show budget details")
            Button("Raise budget…", action: onEdit)
                .buttonStyle(.bordered)
                .controlSize(.mini)
                .help(
                    "Set an explicit effective budget or hard cap. Never switches to unlimited automatically."
                )
        }
    }

    private var metrics: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .top, spacing: 12) {
                metric("Effective", summary.effectiveLabel, tint: VPTheme.textPrimary)
                metric("Hard cap", summary.hardCapLabel, tint: VPTheme.textSecondary)
                metric("Spent", summary.spentLabel, tint: VPTheme.textSecondary)
                metric("Remaining", summary.remainingLabel, tint: tint)
            }
            HStack(alignment: .top, spacing: 12) {
                metric("Forecast", summary.runwayLabel, tint: tint)
                metric("Source", summary.forecastSourceLabel, tint: VPTheme.textTertiary)
                metric(
                    "Auto-extended",
                    summary.extensionCount == 0
                        ? "Never"
                        : "\(summary.extensionCount) time(s)",
                    tint: summary.extensionCount > 0 ? VPTheme.accent : VPTheme.textTertiary
                )
                if let noProgress = summary.noProgressLabel {
                    metric("Progress guard", noProgress, tint: VPTheme.textTertiary)
                } else {
                    metric("Progress guard", "Not enabled", tint: VPTheme.textTertiary)
                }
            }
        }
    }

    private var forecastRow: some View {
        HStack(spacing: 10) {
            VPGradientProgress(
                value: summary.usedRatio,
                tint: tint,
                height: 4
            )
            .frame(maxWidth: 220)
            Text("\(Int((summary.usedRatio * 100).rounded()))% of the effective budget consumed")
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
            Spacer()
            if let critical = summary.criticalPathTurns {
                Text("critical path \(critical) turns")
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            if let updated = summary.updatedAt {
                Text("updated \(updated)")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(1)
            }
            if let reason = model.lastAutoBudgetExtensionReason {
                Text("last extension: \(BudgetFormatting.extensionReason(reason))")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(1)
            }
            Button("Recompute forecast") {
                Task { await model.loadSessionBudgetForecast() }
            }
            .buttonStyle(.plain)
            .font(VPTheme.mono(7.5))
            .foregroundStyle(VPTheme.textSecondary)
            .help("Read-only `session.budget.forecast`; no provider call.")
            .disabled(model.sessionBudgetBusy)
        }
    }

    private var pauseBanner: some View {
        HStack(alignment: .top, spacing: 9) {
            Image(systemName: "pause.circle.fill")
                .font(.system(size: 14, weight: .semibold))
                .foregroundStyle(VPTheme.warning)
            VStack(alignment: .leading, spacing: 2) {
                Text("AUTO-PAUSED")
                    .font(VPTheme.mono(7, weight: .semibold))
                    .tracking(0.5)
                    .foregroundStyle(VPTheme.warning)
                Text(summary.pauseReasonLabel ?? "The session paused at a safe boundary.")
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
                if let noProgress = summary.noProgressLabel {
                    Text(noProgress)
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
            Spacer(minLength: 8)
            if model.selectedSession?.status == "paused" {
                Button("Resume") {
                    Task { await model.resumeSession() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .help(summary.resumeTitle)
            }
            Button("Raise budget…", action: onEdit)
                .buttonStyle(.bordered)
                .controlSize(.small)
        }
        .padding(9)
        .background(VPTheme.warning.opacity(0.08), in: RoundedRectangle(cornerRadius: 9))
        .overlay {
            RoundedRectangle(cornerRadius: 9)
                .strokeBorder(VPTheme.warning.opacity(0.22), lineWidth: 1)
        }
    }

    private var icon: String {
        if summary.unlimited {
            return "infinity"
        }
        if summary.isPaused {
            return "pause.circle"
        }
        return summary.mode.isAuto ? "wand.and.stars" : "gauge.with.dots.needle.50percent"
    }

    private func metric(_ title: String, _ value: String, tint: Color) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(title.uppercased())
                .font(VPTheme.mono(6.5, weight: .semibold))
                .tracking(0.4)
                .foregroundStyle(VPTheme.textTertiary)
            Text(value)
                .font(VPTheme.mono(8, weight: .medium))
                .foregroundStyle(tint)
                .lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

// MARK: - Budget editor sheet

struct SessionBudgetSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let status: SessionBudgetStatus
    @State private var draft: SessionBudgetEditDraft
    @State private var localError: BudgetValidationIssue?

    init(status: SessionBudgetStatus) {
        self.status = status
        _draft = State(initialValue: SessionBudgetEditDraft(status: status))
    }

    private var validation: BudgetValidationIssue? {
        localError ?? draft.validation(status: status)
    }

    private var draftIssue: BudgetValidationIssue? {
        draft.validation(status: status)
    }

    private var imported: Bool {
        model.selectedSession?.status == "imported"
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            VPHairline()
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    spentSummary
                    modePicker
                    modeFields
                    if let validation {
                        Text(validation.displayText)
                            .font(VPTheme.mono(8))
                            .foregroundStyle(VPTheme.warning)
                            .fixedSize(horizontal: false, vertical: true)
                            .padding(9)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(
                                VPTheme.warning.opacity(0.08),
                                in: RoundedRectangle(cornerRadius: 9)
                            )
                    }
                    Text(
                        "Saving uses one explicit `session.budget.set` request. A cap below spend is rejected by the daemon; the app never substitutes Unlimited for a missing value."
                    )
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
                    .fixedSize(horizontal: false, vertical: true)
                }
                .padding(20)
            }
            VPHairline()
            footer
        }
        .frame(width: 580)
        .frame(maxHeight: 680)
        .background { VPCrystalBackdrop() }
        .tint(VPTheme.accent)
        .onChange(of: draft.mode) { _, mode in
            if mode.isAuto {
                prefillCapsIfNeeded()
            }
        }
        .onChange(of: draft) { _, _ in
            localError = nil
        }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 12) {
            ZStack {
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .fill(VPTheme.accentWash)
                    .frame(width: 40, height: 40)
                Image(systemName: "gauge.with.dots.needle.50percent")
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(VPTheme.accent)
            }
            VStack(alignment: .leading, spacing: 3) {
                Text("SESSION BUDGET")
                    .font(VPTheme.mono(7.5, weight: .semibold))
                    .tracking(1.2)
                    .foregroundStyle(VPTheme.accent)
                Text(
                    model.selectedSession?.goal
                        ?? "Set mode, effective budgets, and hard caps"
                )
                .font(VPTheme.font(18, weight: .semibold, rounded: true))
                .foregroundStyle(VPTheme.textPrimary)
                .lineLimit(2)
                Text(
                    "Session \(status.sessionID.prefix(8)) · changing a budget does not resume a paused run."
                )
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
            }
            Spacer()
            Button("Cancel") { dismiss() }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .keyboardShortcut(.cancelAction)
        }
        .padding(20)
        .background(VPTheme.rail)
    }

    private var spentSummary: some View {
        HStack(alignment: .top, spacing: 12) {
            summaryMetric("Spent", BudgetFormatting.combined(
                tokens: status.spentTokens,
                cost: status.spentCostUSD,
                seconds: status.elapsedSecs,
                empty: "Nothing spent"
            ))
            summaryMetric("Remaining", BudgetFormatting.combined(
                tokens: status.remainingTokens,
                cost: status.remainingCostUSD,
                seconds: status.remainingSecs,
                empty: "Unbounded"
            ))
            summaryMetric("Forecast", BudgetFormatting.runway(
                turns: status.forecast.runwayTurns,
                seconds: status.forecast.runwaySecs,
                cost: status.forecast.runwayCostUSD
            ))
        }
    }

    private var modePicker: some View {
        VStack(alignment: .leading, spacing: 7) {
            fieldLabel("Mode", detail: "Auto extends deterministically; Unlimited is explicit only.")
            VPSegmentedControl(
                segments: [
                    VPSegment(.auto, title: "Auto", icon: "wand.and.stars"),
                    VPSegment(.fixed, title: "Fixed", icon: "slider.horizontal.3"),
                    VPSegment(.unlimited, title: "Unlimited", icon: "infinity"),
                ],
                selection: $draft.mode
            )
        }
    }

    @ViewBuilder
    private var modeFields: some View {
        switch draft.mode {
        case .auto:
            VStack(alignment: .leading, spacing: 10) {
                HStack(alignment: .top, spacing: 10) {
                    numberField("Hard USD cap", text: $draft.hardCostCapText, placeholder: "No cap")
                    numberField("Hard token cap", text: $draft.hardTokenCapText, placeholder: "No cap")
                    numberField(
                        "Hard wall clock",
                        text: $draft.hardWallClockText,
                        placeholder: "No cap"
                    )
                }
                Text("At least one hard cap is required. The runtime never extends past these values.")
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
                HStack(alignment: .top, spacing: 10) {
                    numberField(
                        "Starting tokens",
                        text: $draft.tokenBudgetText,
                        placeholder: "From policy"
                    )
                    numberField(
                        "Starting USD",
                        text: $draft.costBudgetText,
                        placeholder: "From policy"
                    )
                    numberField(
                        "Starting wall clock",
                        text: $draft.timeBudgetText,
                        placeholder: "From policy"
                    )
                }
                Text("Blank starting values use the policy initial budgets or the hard cap.")
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
            }
        case .fixed:
            HStack(alignment: .top, spacing: 10) {
                numberField("Token budget", text: $draft.tokenBudgetText, placeholder: "Required")
                numberField("USD budget", text: $draft.costBudgetText, placeholder: "Optional")
                numberField(
                    "Wall clock",
                    text: $draft.timeBudgetText,
                    placeholder: "Optional"
                )
            }
        case .unlimited:
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(VPTheme.warning)
                Text(
                    "Unlimited bypasses session, plan-task, department, and project economic gates. Capacity, approvals, sandbox, verification, context, and turn limits still apply. This mode is only selected here by you."
                )
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
            }
            .padding(10)
            .background(VPTheme.warning.opacity(0.08), in: RoundedRectangle(cornerRadius: 9))
            .overlay {
                RoundedRectangle(cornerRadius: 9)
                    .strokeBorder(VPTheme.warning.opacity(0.22), lineWidth: 1)
            }
        default:
            Text("Unknown budget mode \(draft.mode.rawValue). Choose Auto, Fixed, or Unlimited.")
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.warning)
        }
    }

    private var footer: some View {
        HStack(spacing: 8) {
            if model.sessionBudgetBusy {
                ProgressView().controlSize(.small)
            }
            Text("No Retry · one explicit request")
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textTertiary)
            Spacer()
            Button("Cancel") { dismiss() }
                .keyboardShortcut(.cancelAction)
            Button {
                Task {
                    localError = nil
                    let saved = await model.saveSessionBudget(draft)
                    if saved {
                        dismiss()
                    } else {
                        localError = model.sessionBudgetError
                    }
                }
            } label: {
                Label("Save budget", systemImage: "checkmark")
            }
            .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
            .keyboardShortcut(.defaultAction)
            .disabled(model.sessionBudgetBusy || imported || draftIssue != nil)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 13)
        .background(VPTheme.rail)
    }

    private func prefillCapsIfNeeded() {
        if draft.hardTokenCapText.isEmpty {
            draft.hardTokenCapText = status.hardTokenCap.map { "\($0)" } ?? ""
        }
        if draft.hardCostCapText.isEmpty {
            draft.hardCostCapText = status.hardCostCapUSD.map {
                BudgetFormatting.plainNumber($0)
            } ?? ""
        }
        if draft.hardWallClockText.isEmpty {
            draft.hardWallClockText = status.hardWallClockSecs.map { "\($0)" } ?? ""
        }
    }

    private func summaryMetric(_ title: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title.uppercased())
                .font(VPTheme.mono(6.5, weight: .semibold))
                .tracking(0.4)
                .foregroundStyle(VPTheme.textTertiary)
            Text(value)
                .font(VPTheme.mono(8.5))
                .foregroundStyle(VPTheme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func fieldLabel(_ title: String, detail: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(title.uppercased())
                .font(VPTheme.mono(8, weight: .semibold))
                .tracking(0.7)
                .foregroundStyle(VPTheme.textSecondary)
            Text(detail)
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textTertiary)
        }
    }

    private func numberField(
        _ title: String,
        text: Binding<String>,
        placeholder: String
    ) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title.uppercased())
                .font(VPTheme.mono(6.5, weight: .semibold))
                .tracking(0.4)
                .foregroundStyle(VPTheme.textTertiary)
            TextField(placeholder, text: text)
                .textFieldStyle(.plain)
                .font(VPTheme.mono(10))
                .vpFieldSurface(radius: 8)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

// MARK: - Settings automation card

struct AutoBudgetSettingsCard: View {
    @Environment(AppModel.self) private var model

    private var hasProject: Bool {
        model.selectedProject != nil
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            header
            if hasProject {
                settingsForm
                advanced
                statusLine
                footer
            } else {
                Text("Select a project to configure project-scoped Auto Budget defaults.")
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textSecondary)
            }
        }
        .padding(12)
        .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
        .task {
            await model.loadProjectAutoBudgetPolicy()
        }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 9) {
            Image(systemName: "wand.and.stars")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(VPTheme.accent)
                .frame(width: 18)
            VStack(alignment: .leading, spacing: 2) {
                Text("Auto Budget")
                    .font(VPTheme.font(13, weight: .semibold, rounded: true))
                    .foregroundStyle(VPTheme.textPrimary)
                Text(
                    "Project defaults for new sessions. One hard cap bounds deterministic extension; existing sessions keep their copied caps."
                )
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 4)
            if model.projectAutoBudgetBusy {
                ProgressView().controlSize(.mini)
            }
            Text(model.projectAutoBudgetDraft.enabled ? "ON" : "OFF")
                .font(VPTheme.mono(7, weight: .semibold))
                .tracking(0.4)
                .foregroundStyle(
                    model.projectAutoBudgetDraft.enabled
                        ? VPTheme.success
                        : VPTheme.textTertiary
                )
                .padding(.horizontal, 6)
                .padding(.vertical, 2)
                .background(
                    (model.projectAutoBudgetDraft.enabled
                        ? VPTheme.success
                        : VPTheme.textTertiary).opacity(0.10),
                    in: Capsule()
                )
        }
    }

    private var settingsForm: some View {
        VStack(alignment: .leading, spacing: 10) {
            Toggle(
                "Enable Auto Budget for new sessions",
                isOn: valueBinding(\.enabled)
            )
            .toggleStyle(.switch)
            .controlSize(.small)
            .font(VPTheme.font(10.5, weight: .medium))

            HStack(alignment: .top, spacing: 10) {
                capField(
                    "Max USD",
                    text: valueBinding(\.hardCostCapText),
                    placeholder: "No cap"
                )
                capField(
                    "Max tokens",
                    text: valueBinding(\.hardTokenCapText),
                    placeholder: "No cap"
                )
                capField(
                    "Max wall clock",
                    text: valueBinding(\.hardWallClockText),
                    placeholder: "Optional"
                )
            }

            HStack(alignment: .top, spacing: 14) {
                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 6) {
                        Text("EXTEND BY")
                            .font(VPTheme.mono(6.5, weight: .semibold))
                            .tracking(0.4)
                            .foregroundStyle(VPTheme.textTertiary)
                        Text("\(Int((model.projectAutoBudgetDraft.extendRatio * 100).rounded()))%")
                            .font(VPTheme.mono(8))
                            .foregroundStyle(VPTheme.textSecondary)
                    }
                    Slider(
                        value: valueBinding(\.extendRatio),
                        in: 0.05 ... 1.0
                    )
                    .controlSize(.mini)
                    .frame(width: 190)
                }
                VStack(alignment: .leading, spacing: 3) {
                    Text("NO-PROGRESS GUARD")
                        .font(VPTheme.mono(6.5, weight: .semibold))
                        .tracking(0.4)
                        .foregroundStyle(VPTheme.textTertiary)
                    HStack(spacing: 6) {
                        Stepper(
                            value: valueBinding(\.noProgressTurns),
                            in: 0 ... 200
                        ) {
                            Text("\(model.projectAutoBudgetDraft.noProgressTurns) turns")
                                .font(VPTheme.mono(8))
                                .foregroundStyle(VPTheme.textSecondary)
                        }
                        .controlSize(.mini)
                    }
                }
                VStack(alignment: .leading, spacing: 3) {
                    Text("ACTION")
                        .font(VPTheme.mono(6.5, weight: .semibold))
                        .tracking(0.4)
                        .foregroundStyle(VPTheme.textTertiary)
                    Picker("Action", selection: valueBinding(\.noProgressAction)) {
                        ForEach(NoProgressAction.all) { action in
                            Text(action.label).tag(action)
                        }
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .frame(width: 150)
                }
            }

            Toggle(
                "Advertise only tools the task needs",
                isOn: valueBinding(\.autoToolBundle)
            )
            .toggleStyle(.switch)
            .controlSize(.small)
            .font(VPTheme.font(9.5))
            .help(
                "Deterministic tool-bundle selection by role and task kind. `tool_search` remains the explicit escape hatch."
            )
        }
    }

    private var advanced: some View {
        DisclosureGroup("Advanced limits") {
            VStack(alignment: .leading, spacing: 9) {
                HStack(alignment: .top, spacing: 10) {
                    capField(
                        "Initial tokens",
                        text: valueBinding(\.initialTokenBudgetText),
                        placeholder: "From hard cap"
                    )
                    capField(
                        "Initial USD",
                        text: valueBinding(\.initialCostBudgetText),
                        placeholder: "From hard cap"
                    )
                    capField(
                        "Initial wall clock",
                        text: valueBinding(\.initialWallClockText),
                        placeholder: "From hard cap"
                    )
                }
                HStack(alignment: .top, spacing: 10) {
                    numericField("Max multiplier", value: valueBinding(\.maxMultiplier))
                    numericField(
                        "Forecast calls",
                        value: valueBinding(\.forecastCalls).double
                    )
                    numericField(
                        "Min runway turns",
                        value: valueBinding(\.minRunwayTurns).double
                    )
                }
                HStack(alignment: .top, spacing: 10) {
                    numericField("Warn ratio", value: valueBinding(\.warnAtRatio))
                    numericField("Pause ratio", value: valueBinding(\.pauseAtRatio))
                    numericField(
                        "Turns per run",
                        value: valueBinding(\.maxTurnsPerRun).double
                    )
                }
                HStack(alignment: .top, spacing: 10) {
                    capField(
                        "Max total turns",
                        text: valueBinding(\.maxTotalTurnsText),
                        placeholder: "No limit"
                    )
                }
                Text(
                    "Ratios are fractions from 0 to 1. Pause must not exceed warn. Turns per run: 0 disables the per-run ceiling; explicit values are capped at 200 by the daemon. Disabled policies may exist without a cap, but enabling Auto requires one."
                )
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
            }
            .padding(.top, 8)
        }
        .font(VPTheme.mono(8.5, weight: .medium))
        .foregroundStyle(VPTheme.textSecondary)
    }

    @ViewBuilder
    private var statusLine: some View {
        if let error = model.projectAutoBudgetError {
            Text(error.displayText)
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.warning)
                .fixedSize(horizontal: false, vertical: true)
        } else if let status = model.projectAutoBudgetStatus {
            Text(status)
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.success)
                .lineLimit(2)
        }
        if let issue = model.projectAutoBudgetDraft.validation() {
            Text(issue.displayText)
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.warning)
                .fixedSize(horizontal: false, vertical: true)
        }
        if !model.projectAutoBudgetDraft.hardCostCapText.isEmpty
            || !model.projectAutoBudgetDraft.hardTokenCapText.isEmpty
            || !model.projectAutoBudgetDraft.hardWallClockText.isEmpty
        {
            Text(
                "Hard caps apply to each new auto session; edits here do not widen an existing session."
            )
            .font(VPTheme.mono(7))
            .foregroundStyle(VPTheme.textTertiary)
            .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var footer: some View {
        HStack(spacing: 8) {
            if let updated = model.projectAutoBudgetUpdatedAt {
                Text("updated \(updated)")
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(1)
            }
            Spacer()
            Button("Reload") {
                Task { await model.reloadProjectAutoBudgetPolicy() }
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(model.projectAutoBudgetBusy)
            Button("Save defaults") {
                Task { await model.saveProjectAutoBudgetPolicy() }
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.small)
            .disabled(
                model.projectAutoBudgetBusy
                    || model.projectAutoBudgetDraft.validation() != nil
            )
        }
    }

    private func capField(
        _ title: String,
        text: Binding<String>,
        placeholder: String
    ) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title.uppercased())
                .font(VPTheme.mono(6.5, weight: .semibold))
                .tracking(0.4)
                .foregroundStyle(VPTheme.textTertiary)
            TextField(placeholder, text: text)
                .textFieldStyle(.plain)
                .font(VPTheme.mono(9))
                .vpFieldSurface(radius: 7)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func numericField(_ title: String, value: Binding<Double>) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title.uppercased())
                .font(VPTheme.mono(6.5, weight: .semibold))
                .tracking(0.4)
                .foregroundStyle(VPTheme.textTertiary)
            TextField("0", value: value, format: .number)
                .textFieldStyle(.plain)
                .font(VPTheme.mono(9))
                .vpFieldSurface(radius: 7)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func valueBinding<Value>(
        _ keyPath: WritableKeyPath<AutoBudgetPolicyDraft, Value>
    ) -> Binding<Value> {
        Binding(
            get: { model.projectAutoBudgetDraft[keyPath: keyPath] },
            set: { model.projectAutoBudgetDraft[keyPath: keyPath] = $0 }
        )
    }
}

private extension Binding where Value == Int {
    /// `TextField(value:format:)` takes the same numeric type as its binding.
    var double: Binding<Double> {
        Binding<Double>(
            get: { Double(self.wrappedValue) },
            set: { self.wrappedValue = Int($0.rounded()) }
        )
    }
}
