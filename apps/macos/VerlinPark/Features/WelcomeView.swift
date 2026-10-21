import SwiftUI

struct WelcomeView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(spacing: 26) {
            Image(systemName: "tree.fill")
                .font(.system(size: 54, weight: .light))
                .foregroundStyle(.green)

            VStack(spacing: 8) {
                Text("VerlinPark")
                    .font(.system(size: 34, weight: .semibold, design: .rounded))
                Text("Agent teams, not chat tabs.")
                    .font(.title3)
                    .foregroundStyle(.secondary)
            }

            VStack(alignment: .leading, spacing: 14) {
                step(
                    number: "01",
                    title: "Open a repository",
                    detail: "VerlinPark creates an isolated worktree for every task."
                )
                step(
                    number: "02",
                    title: "Configure the team",
                    detail: "Choose models and budgets manually. No hidden routing."
                )
                step(
                    number: "03",
                    title: "Inspect everything",
                    detail: "Plans, messages, tool calls, diffs, and cost — all in one trace."
                )
            }
            .frame(maxWidth: 460, alignment: .leading)

            HStack(spacing: 12) {
                Button {
                    Task { await model.addProject() }
                } label: {
                    Label("Add Project…", systemImage: "folder.badge.plus")
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .disabled(model.connection != .connected)

                ConnectionBadge(state: model.connection)
            }

            if case let .failed(message) = model.connection {
                Text(message)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 520)
            } else {
                Text("The daemon owns sessions. Close the app; your agents keep working.")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(48)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func step(number: String, title: String, detail: String) -> some View {
        HStack(alignment: .top, spacing: 14) {
            Text(number)
                .font(.system(.caption, design: .monospaced).weight(.semibold))
                .foregroundStyle(.green)
                .frame(width: 28, alignment: .leading)
            VStack(alignment: .leading, spacing: 3) {
                Text(title)
                    .font(.headline)
                Text(detail)
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
        }
    }
}
