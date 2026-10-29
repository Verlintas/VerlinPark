import SwiftUI

struct DashboardView: View {
    @Environment(AppModel.self) private var model
    @Environment(BrandStore.self) private var brand
    @Environment(MemoryMonitor.self) private var memory
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 3) {
                    Text("Manage Dash")
                        .font(.system(size: 24, weight: .semibold, design: .rounded))
                    Text("Agents, computer lease, cost, and local state")
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button("Done") { dismiss() }
                    .keyboardShortcut(.defaultAction)
            }
            .padding(22)

            Divider().opacity(0.35)

            ScrollView {
                LazyVGrid(
                    columns: [
                        GridItem(.flexible(), spacing: 12),
                        GridItem(.flexible(), spacing: 12),
                    ],
                    spacing: 12
                ) {
                    metric(
                        "Workspace",
                        value: "\(model.projects.count) projects",
                        detail: "\(model.sessions.count) sessions · \(model.events.count) events in view"
                    )
                    metric(
                        "Agents",
                        value: "\(model.agents.count) known",
                        detail: "Defaults 4 / 6 / 12 · caps 5 / 10 / 40"
                    )
                    metric(
                        "Computer",
                        value: "not wired",
                        detail: "GUI/browser lease is design-only · shell/worktrees run in parallel"
                    )
                    metric(
                        "Memory",
                        value: memory.text,
                        detail: "app + daemon resident"
                    )
                    metric(
                        "Resident",
                        value: "Aviiya",
                        detail: "external · NovaBAIC bridge reserved · no local runtime"
                    )
                    metric(
                        "Brand",
                        value: brand.config.effectiveName,
                        detail: "\(brand.config.style.title) mark"
                    )
                }
                .padding(18)

                VStack(alignment: .leading, spacing: 8) {
                    SectionLabel(title: "Next runtime")
                    HStack(spacing: 10) {
                        Image(systemName: "hammer")
                            .foregroundStyle(.green)
                        VStack(alignment: .leading, spacing: 2) {
                            Text("Next · Merge queue + Integrator")
                                .font(.system(size: 12, weight: .medium))
                            Text(
                                "Approved branches merge serially today; queue, retry, and post-merge checks are next."
                            )
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundStyle(.secondary)
                        }
                        Spacer()
                    }
                    .padding(12)
                    .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 12))
                }
                .padding(.horizontal, 18)
                .padding(.bottom, 18)
            }
        }
        .frame(width: 760, height: 600)
        .background(.ultraThinMaterial)
    }

    private func metric(_ title: String, value: String, detail: String) -> some View {
        VStack(alignment: .leading, spacing: 7) {
            SectionLabel(title: title)
            Text(value)
                .font(.system(size: 20, weight: .semibold, design: .rounded))
            Text(detail)
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(.secondary)
                .lineLimit(2)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
    }
}
