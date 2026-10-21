import SwiftUI

struct NewSessionSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var goal = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            VStack(alignment: .leading, spacing: 6) {
                Text("New Session")
                    .font(.title2.weight(.semibold))
                Text("Give the Lead one concrete goal. You can review the plan before agents start.")
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
                        await model.createSession(goal: goal)
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
    }

    private var isCreateDisabled: Bool {
        model.isBusy ||
            goal.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ||
            model.selectedProject == nil
    }
}
