import SwiftUI

struct NewSessionSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var goal = ""
    @State private var budget = 200_000
    @State private var priority = 0
    @State private var costBudget = ""
    @State private var timeBudget = ""
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
        .background(VPTheme.canvas)
        .tint(VPTheme.accent)
        .onAppear {
            budget = model.defaultTokenBudget
            goalFocused = true
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

    private var budgetSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            sectionLabel("Run budgets", detail: "Checked at turn and tool boundaries.")
            HStack(alignment: .top, spacing: 10) {
                budgetCard(
                    icon: "number",
                    title: "Tokens",
                    hint: "Input + output ceiling."
                ) {
                    TextField("200000", value: $budget, format: .number)
                        .textFieldStyle(.plain)
                        .font(VPTheme.mono(11))
                }
                budgetCard(
                    icon: "dollarsign.circle",
                    title: "USD",
                    hint: "Needs role prices."
                ) {
                    TextField("optional", text: $costBudget)
                        .textFieldStyle(.plain)
                        .font(VPTheme.mono(11))
                }
                budgetCard(
                    icon: "clock",
                    title: "Wall clock",
                    hint: "Seconds per run."
                ) {
                    TextField("optional", text: $timeBudget)
                        .textFieldStyle(.plain)
                        .font(VPTheme.mono(11))
                }
            }
        }
        .vpAppear(index: 2)
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
                Text(title.uppercased())
                    .font(VPTheme.mono(8, weight: .semibold))
                    .tracking(0.7)
                    .foregroundStyle(VPTheme.textSecondary)
            }
            content()
                .foregroundStyle(VPTheme.textPrimary)
                .padding(.horizontal, 9)
                .padding(.vertical, 7)
                .vpFieldSurface(radius: 8)
            Text(hint)
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
                    await model.createSession(
                        goal: goal,
                        tokenBudget: budget,
                        costBudgetUSD: Double(costBudget),
                        timeBudgetSecs: Int(timeBudget),
                        priority: priority
                    )
                    dismiss()
                }
            } label: {
                Label("Create session", systemImage: "arrow.right")
            }
            .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accent))
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
            Text(detail)
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
        }
    }

    private var isCreateDisabled: Bool {
        model.isBusy ||
            goal.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ||
            model.selectedProject == nil
    }
}
