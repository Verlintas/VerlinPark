import SwiftUI

/// The Collab surface is an orchestration monitor: who exists, what tier they
/// belong to, and whether they are running. Plans, transcripts, and tool detail
/// stay in the Stage and Preview surfaces.
struct CollabGraphView: View {
    @Environment(AppModel.self) private var model

    private var snapshot: CollabSnapshot {
        CollabSnapshot.build(
            agents: model.agents,
            plan: model.teamPlan,
            statuses: model.planTaskStatuses,
            sessionStatus: model.selectedSession?.status ?? "idle",
            goal: model.selectedSession?.goal ?? L10n.t("Waiting for a session")
        )
    }

    var body: some View {
        let snapshot = snapshot
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                CollabSummaryHeader(snapshot: snapshot)
                    .padding(.bottom, 2)

                if let leader = snapshot.leader {
                    CollabLeaderRow(node: leader, goal: snapshot.goal)
                } else if snapshot.isEmpty {
                    CollabEmptyState()
                } else {
                    CollabLeaderPlaceholder(goal: snapshot.goal)
                }

                ForEach(snapshot.managers) { group in
                    OrgConnector()
                        .vpAppear(index: 1)
                    CollabManagerGroupView(group: group)
                }

                if !snapshot.unassignedWorkers.isEmpty {
                    OrgConnector()
                    CollabTierSection(
                        title: L10n.t("Workers"),
                        icon: "person.3",
                        tint: VPTheme.info,
                        nodes: snapshot.unassignedWorkers
                    )
                }

                if !snapshot.verifiers.isEmpty {
                    OrgConnector()
                    CollabTierSection(
                        title: L10n.t("Verifiers"),
                        icon: "checkmark.shield",
                        tint: VPTheme.success,
                        nodes: snapshot.verifiers
                    )
                }

                if !snapshot.labour.isEmpty {
                    OrgConnector()
                    CollabTierSection(
                        title: L10n.t("Labour"),
                        icon: "hammer",
                        tint: VPTheme.warning,
                        nodes: snapshot.labour
                    )
                }
            }
            .padding(.horizontal, 12)
            .padding(.top, 4)
            .padding(.bottom, 16)
        }
    }
}

private struct CollabSummaryHeader: View {
    let snapshot: CollabSnapshot

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack(spacing: 7) {
                Text(L10n.t("ORCHESTRATION"))
                    .font(VPTheme.mono(8, weight: .semibold))
                    .tracking(0.9)
                    .foregroundStyle(VPTheme.textTertiary)
                Rectangle()
                    .fill(VPTheme.hairline)
                    .frame(height: 1)
                if snapshot.runningAgents > 0 {
                    VPStatusDot(color: VPTheme.success, pulsing: true, size: 4)
                }
                Text(snapshot.summary)
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
                    .contentTransition(.numericText())
            }

            if snapshot.hasPlan {
                HStack(spacing: 7) {
                    GeometryReader { proxy in
                        ZStack(alignment: .leading) {
                            Capsule().fill(VPTheme.hairline)
                            Capsule()
                                .fill(VPTheme.plan.opacity(0.7))
                                .frame(width: max(2, proxy.size.width * snapshot.taskFraction))
                        }
                    }
                    .frame(height: 3)

                    Text("\(snapshot.completedTasks)/\(snapshot.totalTasks)")
                        .font(VPTheme.mono(7.5))
                        .foregroundStyle(VPTheme.textTertiary)
                        .frame(width: 34, alignment: .trailing)
                        .contentTransition(.numericText())
                }
            }
        }
        .padding(.bottom, 8)
    }
}

private struct CollabLeaderRow: View {
    let node: CollabAgentNode
    let goal: String

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: "crown")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(VPTheme.accent)
                    .frame(width: 16)
                    .padding(.top, 1)
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(node.name)
                            .font(VPTheme.font(12.5, weight: .semibold))
                            .foregroundStyle(VPTheme.textPrimary)
                        Text(L10n.t("LEADER"))
                            .font(VPTheme.mono(7, weight: .semibold))
                            .tracking(0.7)
                            .foregroundStyle(VPTheme.accent)
                        Spacer(minLength: 0)
                        StatusPill(status: node.status)
                    }
                    Text(goal)
                        .font(VPTheme.font(10))
                        .foregroundStyle(VPTheme.textSecondary)
                        .lineLimit(2)
                    HStack(spacing: 5) {
                        Text(node.model)
                        if let branch = node.branch {
                            Text("· \(branch)")
                                .lineLimit(1)
                                .truncationMode(.middle)
                        }
                    }
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
                }
            }
            .padding(.vertical, 7)
            .overlay(alignment: .bottom) {
                VPHairline()
            }
        }
    }
}

private struct CollabLeaderPlaceholder: View {
    let goal: String

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "crown")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(VPTheme.textTertiary)
                .frame(width: 16)
                .padding(.top, 1)
            VStack(alignment: .leading, spacing: 2) {
                Text(L10n.t("Leader"))
                    .font(VPTheme.font(12.5, weight: .semibold))
                    .foregroundStyle(VPTheme.textSecondary)
                Text(goal)
                    .font(VPTheme.font(10))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(2)
            }
            Spacer(minLength: 0)
        }
        .padding(.vertical, 7)
        .overlay(alignment: .bottom) {
            VPHairline()
        }
    }
}

private struct CollabManagerGroupView: View {
    let group: CollabManagerGroup

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            CollabAgentRow(node: group.manager, tint: VPTheme.plan)
            if !group.workers.isEmpty {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(group.workers) { worker in
                        CollabAgentRow(node: worker, tint: VPTheme.info, indented: true)
                    }
                }
                .padding(.leading, 15)
                .overlay(alignment: .leading) {
                    Rectangle()
                        .fill(VPTheme.hairline)
                        .frame(width: 1)
                        .padding(.leading, 5)
                }
            }
        }
        .padding(.vertical, 4)
        .overlay(alignment: .bottom) {
            VPHairline()
        }
    }
}

private struct CollabTierSection: View {
    let title: String
    let icon: String
    let tint: Color
    let nodes: [CollabAgentNode]

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 6) {
                Image(systemName: icon)
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(tint)
                Text(title.uppercased())
                    .font(VPTheme.mono(8, weight: .semibold))
                    .tracking(0.8)
                    .foregroundStyle(VPTheme.textSecondary)
                Text("\(nodes.count)")
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
                Spacer()
            }
            .padding(.top, 6)
            .padding(.bottom, 2)

            ForEach(nodes) { node in
                CollabAgentRow(node: node, tint: tint)
            }
        }
        .padding(.bottom, 4)
        .overlay(alignment: .bottom) {
            VPHairline()
        }
    }
}

private struct CollabAgentRow: View {
    let node: CollabAgentNode
    let tint: Color
    var indented = false

    var body: some View {
        HStack(alignment: .top, spacing: 7) {
            VPStatusDot(
                color: VPTheme.statusColor(node.status),
                pulsing: node.isRunning,
                size: 4
            )
            .padding(.top, 5)

            VStack(alignment: .leading, spacing: 1.5) {
                HStack(spacing: 5) {
                    Text(node.name)
                        .font(VPTheme.font(10.5, weight: .medium))
                        .foregroundStyle(VPTheme.textPrimary)
                    if let title = node.displayTitle {
                        Text(title)
                            .font(VPTheme.mono(7.5))
                            .foregroundStyle(VPTheme.textTertiary)
                            .lineLimit(1)
                    }
                    Spacer(minLength: 4)
                    Text(node.status)
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.statusColor(node.status))
                }

                HStack(spacing: 4) {
                    Text(node.roleLabel)
                        .foregroundStyle(tint.opacity(0.85))
                    if let profile = node.profile {
                        Text("· \(profile)")
                    }
                    if let branch = node.branch {
                        Text("· \(branch)")
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                }
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
            }
        }
        .padding(.vertical, 4)
        .padding(.leading, indented ? 4 : 0)
    }
}

private struct CollabEmptyState: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(L10n.t("No agents yet"))
                .font(VPTheme.font(11.5, weight: .medium))
                .foregroundStyle(VPTheme.textSecondary)
            Text(L10n.t("The Leader will build the org from the next team plan."))
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.vertical, 16)
    }
}

struct OrgConnector: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var drawn = false

    var body: some View {
        VStack(spacing: 0) {
            Rectangle()
                .fill(VPTheme.hairline)
                .frame(width: 1, height: 8)
            Image(systemName: "chevron.down")
                .font(.system(size: 6, weight: .bold))
                .foregroundStyle(VPTheme.textTertiary)
            Rectangle()
                .fill(VPTheme.hairline)
                .frame(width: 1, height: 8)
        }
        .scaleEffect(y: drawn || reduceMotion ? 1 : 0.01, anchor: .top)
        .opacity(drawn || reduceMotion ? 1 : 0)
        .animation(
            VPMotion.resolved(VPMotion.smooth, reduceMotion: reduceMotion),
            value: drawn
        )
        .onAppear { drawn = true }
        .frame(maxWidth: .infinity)
    }
}
