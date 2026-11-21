import SwiftUI

struct NewSessionSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var goal = ""
    @State private var priority = 0
    @State private var budgetDraft = SessionBudgetPresetDraft()
    @State private var budgetTouched = false
    @State private var budgetError: BudgetValidationIssue?
    @FocusState private var goalFocused: Bool

    var body: some View {
        VStack(spacing: 0) {
            header
            VPHairline()

            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    goalSection
                    prioritySection
                    budgetSection
                }
                .padding(22)
                .frame(maxWidth: .infinity, alignment: .leading)
            }

            VPHairline()
            footer
        }
        .frame(width: 660)
        .frame(maxHeight: 700)
        .background { VPCrystalBackdrop() }
        .tint(VPTheme.accent)
        .onAppear {
            budgetDraft = SessionBudgetPresetDraft(
                projectPolicy: model.projectAutoBudgetPolicy,
                defaultTokenBudget: model.defaultTokenBudget
            )
            goalFocused = true
        }
        .task {
            await model.loadProjectAutoBudgetPolicy()
            if !budgetTouched {
                budgetDraft.applyProjectDefaults(model.projectAutoBudgetPolicy)
            }
        }
        .onChange(of: budgetDraft) { _, _ in
            budgetError = nil
        }
    }

    // MARK: Header

    private var header: some View {
        HStack(alignment: .top, spacing: 12) {
            ZStack {
                RoundedRectangle(cornerRadius: 11, style: .continuous)
                    .fill(VPTheme.accentWash)
                    .frame(width: 42, height: 42)
                Image(systemName: "plus.bubble")
                    .font(.system(size: 16, weight: .semibold))
                    .foregroundStyle(VPTheme.accent)
            }

            VStack(alignment: .leading, spacing: 3) {
                Text("NEW RUN")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .tracking(1.3)
                    .foregroundStyle(VPTheme.accent)
                Text("Start a session")
                    .font(VPTheme.font(22, weight: .semibold, rounded: true))
                    .foregroundStyle(VPTheme.textPrimary)
                Text(
                    "Give the Leader one concrete goal. It starts in an isolated worktree and publishes its plan in Collab."
                )
                .font(VPTheme.font(10.5))
                .foregroundStyle(VPTheme.textSecondary)
            }

            Spacer(minLength: 8)

            if let project = model.selectedProject {
                VStack(alignment: .trailing, spacing: 2) {
                    Text("PROJECT")
                        .font(VPTheme.mono(7, weight: .semibold))
                        .tracking(0.8)
                        .foregroundStyle(VPTheme.textTertiary)
                    Text(project.name)
                        .font(VPTheme.font(10, weight: .medium))
                        .foregroundStyle(VPTheme.textSecondary)
                        .lineLimit(1)
                }
            }
        }
        .padding(22)
        .background(VPTheme.rail)
    }

    // MARK: Goal

    private var goalSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            sectionLabel("The brief", detail: "One outcome, not a checklist.")
            TextField(
                "e.g. Add team invitations across API, UI, and tests",
                text: $goal,
                axis: .vertical
            )
            .textFieldStyle(.plain)
            .font(VPTheme.font(13))
            .foregroundStyle(VPTheme.textPrimary)
            .lineLimit(3 ... 7)
            .focused($goalFocused)
            .padding(12)
            .vpFieldSurface(radius: 11, focused: goalFocused)
            .animation(VPMotion.snap, value: goalFocused)

            HStack(spacing: 6) {
                Image(systemName: "text.quote")
                    .font(.system(size: 8))
                Text(goal.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                    ? "Specific goals produce a tighter team plan."
                    : "\(goal.count) characters · the Leader will decompose this into a task DAG.")
            }
            .font(VPTheme.mono(8))
            .foregroundStyle(VPTheme.textTertiary)
        }
        .vpAppear(index: 0)
    }

    // MARK: Priority

    private var prioritySection: some View {
        VStack(alignment: .leading, spacing: 8) {
            sectionLabel(
                "Admission priority",
                detail: "Higher runs start first when the session cap is full."
            )
            VPSegmentedControl(
                segments: [
                    VPSegment(-10, title: "Low"),
                    VPSegment(0, title: "Normal"),
                    VPSegment(10, title: "High"),
                    VPSegment(50, title: "Urgent"),
                ],
                selection: $priority
            )
        }
        .vpAppear(index: 1)
    }

    // MARK: Budgets

    private var budgetValidation: BudgetValidationIssue? {
        budgetDraft.validation(basePolicy: model.projectAutoBudgetPolicy)
    }

    private var budgetSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            sectionLabel(
                "Budget preset",
                detail: "One decision. Auto is recommended for long tasks."
            )

            VPSegmentedControl(
                segments: [
                    VPSegment(.auto, title: "Auto (recommended)", icon: "wand.and.stars"),
                    VPSegment(.fixed, title: "Fixed", icon: "slider.horizontal.3"),
                    VPSegment(.unlimited, title: "Unlimited", icon: "infinity"),
                ],
                selection: Binding(
                    get: { budgetDraft.preset },
                    set: { value in
                        budgetDraft.preset = value
                        budgetTouched = true
                    }
                )
            )

            switch budgetDraft.preset {
            case .auto:
                autoBudgetFields
            case .fixed:
                fixedBudgetFields
            case .unlimited:
                unlimitedBudgetNote
            default:
                unknownBudgetNote
            }

            if let issue = budgetValidation ?? budgetError {
                HStack(alignment: .top, spacing: 7) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .font(.system(size: 9, weight: .semibold))
                        .foregroundStyle(VPTheme.warning)
                    Text(issue.displayText)
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.warning)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .padding(9)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(VPTheme.warning.opacity(0.08), in: RoundedRectangle(cornerRadius: 9))
                .overlay {
                    RoundedRectangle(cornerRadius: 9)
                        .strokeBorder(VPTheme.warning.opacity(0.22), lineWidth: 1)
                }
            }
        }
        .vpAppear(index: 2)
    }

    private var autoBudgetFields: some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack(alignment: .top, spacing: 10) {
                budgetCard(
                    icon: budgetDraft.capKind == .usd ? "dollarsign.circle" : "number",
                    title: budgetDraft.capKind == .usd ? "Maximum USD" : "Maximum tokens",
                    hint: "The runtime may extend only up to this cap."
                ) {
                    TextField(
                        budgetDraft.capKind == .usd ? "e.g. 25" : "e.g. 500000",
                        text: Binding(
                            get: { budgetDraft.autoCapText },
                            set: { value in
                                budgetDraft.autoCapText = value
                                budgetTouched = true
                            }
                        )
                    )
                    .textFieldStyle(.plain)
                    .font(VPTheme.mono(11))
                }
                budgetCard(
                    icon: "clock",
                    title: "Max wall clock",
                    hint: "Optional. Blank keeps the project default."
                ) {
                    TextField(
                        "optional seconds",
                        text: Binding(
                            get: { budgetDraft.autoWallClockText },
                            set: { value in
                                budgetDraft.autoWallClockText = value
                                budgetTouched = true
                            }
                        )
                    )
                    .textFieldStyle(.plain)
                    .font(VPTheme.mono(11))
                }
            }

            VPSegmentedControl(
                segments: [
                    VPSegment(BudgetAutoCapKind.usd, title: "Cap in USD", icon: "dollarsign"),
                    VPSegment(
                        BudgetAutoCapKind.tokens,
                        title: "Cap in tokens",
                        icon: "number"
                    ),
                ],
                selection: Binding(
                    get: { budgetDraft.capKind },
                    set: { value in
                        if value != budgetDraft.capKind {
                            budgetDraft.autoCapText = ""
                        }
                        budgetDraft.capKind = value
                        budgetTouched = true
                    }
                ),
                compact: true
            )

            Text("USD caps require configured role prices; token caps always apply.")
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)

            Text(
                "Auto starts within the project's initial budget and extends deterministically up to this hard cap. It never changes the model/provider, approvals, sandbox, Verifier, or merge policy."
            )
            .font(VPTheme.mono(8))
            .foregroundStyle(VPTheme.textSecondary)
            .fixedSize(horizontal: false, vertical: true)

            if let projectCap = inheritedAutoCapNote {
                Text(projectCap)
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if !model.projectAutoBudgetPolicy.enabled {
                Text(
                    "Project Auto Budget defaults are off. This session can still use Auto with the explicit cap above."
                )
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private var fixedBudgetFields: some View {
        HStack(alignment: .top, spacing: 10) {
            budgetCard(
                icon: "number",
                title: "Tokens",
                hint: "Input + output ceiling."
            ) {
                TextField("200000", text: $budgetDraft.fixedTokenText)
                    .textFieldStyle(.plain)
                    .font(VPTheme.mono(11))
            }
            budgetCard(
                icon: "dollarsign.circle",
                title: "USD",
                hint: "Needs role prices."
            ) {
                TextField("optional", text: $budgetDraft.fixedCostText)
                    .textFieldStyle(.plain)
                    .font(VPTheme.mono(11))
            }
            budgetCard(
                icon: "clock",
                title: "Wall clock",
                hint: "Seconds per run."
            ) {
                TextField("optional", text: $budgetDraft.fixedTimeText)
                    .textFieldStyle(.plain)
                    .font(VPTheme.mono(11))
            }
        }
    }

    private var unlimitedBudgetNote: some View {
        HStack(alignment: .top, spacing: 7) {
            Image(systemName: "infinity.circle.fill")
                .foregroundStyle(VPTheme.accent)
            Text(
                L10n.t(
                    "Unlimited ignores session, plan-task, department, and project budget gates. Capacity, approvals, verification, context compaction, and turn limits still apply. It is only used when you select it here."
                )
            )
            .font(VPTheme.mono(8))
            .foregroundStyle(VPTheme.textSecondary)
            .fixedSize(horizontal: false, vertical: true)
        }
        .padding(9)
        .background(VPTheme.accent.opacity(0.07), in: RoundedRectangle(cornerRadius: 9))
        .overlay {
            RoundedRectangle(cornerRadius: 9)
                .strokeBorder(VPTheme.accent.opacity(0.18), lineWidth: 1)
        }
    }

    private var unknownBudgetNote: some View {
        HStack(alignment: .top, spacing: 7) {
            Image(systemName: "questionmark.circle")
                .foregroundStyle(VPTheme.warning)
            Text(
                "This daemon reported an unknown budget mode. Choose Auto, Fixed, or Unlimited explicitly."
            )
            .font(VPTheme.mono(8))
            .foregroundStyle(VPTheme.textSecondary)
        }
        .padding(9)
        .background(VPTheme.warning.opacity(0.08), in: RoundedRectangle(cornerRadius: 9))
    }

    private var inheritedAutoCapNote: String? {
        let policy = model.projectAutoBudgetPolicy
        var parts: [String] = []
        if budgetDraft.capKind != .usd, let cap = policy.hardCostCapUSD {
            parts.append("USD \(BudgetFormatting.money(cap))")
        }
        if budgetDraft.capKind != .tokens, let cap = policy.hardTokenCap {
            parts.append("\(BudgetFormatting.tokens(cap)) tokens")
        }
        if parts.isEmpty { return nil }
        return "Other project hard caps still apply: " + parts.joined(separator: " · ") + "."
    }

    private func budgetCard(
        icon: String,
        title: String,
        hint: String,
        @ViewBuilder content: () -> some View
    ) -> some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack(spacing: 5) {
                Image(systemName: icon)
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                Text(L10n.t(title).uppercased())
                    .font(VPTheme.mono(8, weight: .semibold))
                    .tracking(0.7)
                    .foregroundStyle(VPTheme.textSecondary)
            }
            content()
                .foregroundStyle(VPTheme.textPrimary)
                .padding(.horizontal, 9)
                .padding(.vertical, 7)
                .vpFieldSurface(radius: 8)
            Text(L10n.t(hint))
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 11, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 11, style: .continuous)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
    }

    // MARK: Footer

    private var footer: some View {
        HStack(spacing: 10) {
            HStack(spacing: 5) {
                Image(systemName: "lock.shield")
                    .font(.system(size: 8))
                Text("isolated worktree · verifier gate")
            }
            .font(VPTheme.mono(8))
            .foregroundStyle(VPTheme.textTertiary)

            Spacer()

            Button("Cancel") {
                dismiss()
            }
            .buttonStyle(VPPressableButtonStyle(scale: 0.97))
            .keyboardShortcut(.cancelAction)

            Button {
                Task {
                    let arguments: SessionCreateBudgetArguments
                    do {
                        arguments = try budgetDraft.arguments(
                            basePolicy: model.projectAutoBudgetPolicy
                        )
                    } catch {
                        budgetError = BudgetValidationIssue.from(error)
                        return
                    }
                    let created = await model.createSession(
                        goal: goal,
                        tokenBudget: arguments.tokenBudget.flatMap { Int(exactly: $0) },
                        costBudgetUSD: arguments.costBudgetUSD,
                        timeBudgetSecs: arguments.timeBudgetSecs.flatMap { Int(exactly: $0) },
                        priority: priority,
                        unlimited: arguments.unlimited,
                        budgetMode: arguments.mode,
                        autoBudget: arguments.autoPolicy
                    )
                    if created {
                        dismiss()
                    }
                }
            } label: {
                Label("Create session", systemImage: "arrow.right")
            }
            .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
            .keyboardShortcut(.defaultAction)
            .disabled(isCreateDisabled)
        }
        .padding(.horizontal, 22)
        .padding(.vertical, 14)
        .background(VPTheme.rail)
    }

    private func sectionLabel(_ title: String, detail: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(title.uppercased())
                .font(VPTheme.mono(8.5, weight: .semibold))
                .tracking(1.0)
                .foregroundStyle(VPTheme.textSecondary)
            Text(L10n.t(detail))
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
        }
    }

    private var isCreateDisabled: Bool {
        model.isBusy ||
            goal.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ||
            model.selectedProject == nil ||
            budgetValidation != nil
    }
}
