import SwiftUI

func planBudgetLabel(tokens: Int?, cost: Double?) -> String? {
    var parts: [String] = []
    if let tokens {
        parts.append("\(tokens) tok")
    }
    if let cost {
        parts.append(String(format: "$%.2f", cost))
    }
    return parts.isEmpty ? nil : parts.joined(separator: " / ")
}

struct PlanTaskRow: View {
    let task: PlanTask
    let status: String

    var body: some View {
        HStack(alignment: .top, spacing: 7) {
            Image(systemName: icon)
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(color)
                .frame(width: 12)
                .padding(.top, 1)
                .contentTransition(.symbolEffect(.replace))
                .animation(VPMotion.snap, value: status)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 5) {
                    Text(task.id)
                        .font(.system(size: 8, weight: .medium, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    Text(task.title)
                        .font(.system(size: 10))
                        .lineLimit(2)
                }
                HStack(spacing: 5) {
                    Text(task.department)
                    if let profile = task.profile {
                        Text("· \(profile)")
                    }
                    if let priority = task.priority, priority != 0 {
                        Text("· P\(priority)")
                            .foregroundStyle(priority > 0 ? Color.orange : Color.secondary)
                    }
                    if !task.deps.isEmpty {
                        Text("· deps \(task.deps.joined(separator: ","))")
                    }
                    if let budget = planBudgetLabel(
                        tokens: task.tokenBudget,
                        cost: task.costBudgetUsd
                    ) {
                        Text("· \(budget)")
                    }
                }
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
            }
            Spacer(minLength: 0)
            Text(status)
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(color)
        }
        .padding(7)
        .background(.white.opacity(0.025), in: RoundedRectangle(cornerRadius: 8))
        .vpHoverHighlight(radius: 8)
    }

    private var icon: String {
        switch status {
        case "running": "circle.lefthalf.filled"
        case "completed": "checkmark.circle.fill"
        case "failed", "blocked", "cancelled": "xmark.octagon"
        default: "circle"
        }
    }

    private var color: Color {
        switch status {
        case "running": VPTheme.accent
        case "completed": VPTheme.success
        case "failed", "blocked", "cancelled": VPTheme.danger
        default: VPTheme.textTertiary
        }
    }
}
