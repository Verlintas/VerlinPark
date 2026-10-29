import SwiftUI

struct NewSessionSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var goal = ""
    @State private var budget = 200_000

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            VStack(alignment: .leading, spacing: 6) {
                Text("New Session")
                    .font(.title2.weight(.semibold))
                Text(
                    "Give the Lead one concrete goal. It starts in an isolated worktree and publishes its plan in Collab."
                )
                .font(.callout)
                .foregroundStyle(.secondary)
            }

            TextField(
                "e.g. Add team invitations across API, UI, and tests",
                text: $goal,
                axis: .vertical
            )
            .textFieldStyle(.roundedBorder)
            .lineLimit(3 ... 6)

            VStack(alignment: .leading, spacing: 6) {
                Text("Token budget")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
                TextField("200000", value: $budget, format: .number)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 160)
                Text(
                    "The runtime stops the Leader when input + output tokens reach this limit. Concurrent batches may overshoot by one turn."
                )
                .font(.caption)
                .foregroundStyle(.tertiary)
            }

            HStack {
                if let project = model.selectedProject {
                    Label(project.name, systemImage: "folder")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button("Cancel", role: .cancel) {
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)

                Button("Create") {
                    Task {
                        await model.createSession(goal: goal, tokenBudget: budget)
                        dismiss()
                    }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(isCreateDisabled)
            }
        }
        .padding(24)
        .frame(width: 560)
        .onAppear {
            budget = model.defaultTokenBudget
        }
    }

    private var isCreateDisabled: Bool {
        model.isBusy ||
            goal.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ||
            model.selectedProject == nil
    }
}
