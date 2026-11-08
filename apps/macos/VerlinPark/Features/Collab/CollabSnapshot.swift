import Foundation

/// A read-only projection of the org chart for the Collab surface.
///
/// Collab is a status view, not a second plan editor. This model groups agents
/// by their actual parent relationship and derives only the counters the
/// surface needs. It keeps task details out of the panel while still making the
/// current orchestration state legible.
struct CollabAgentNode: Identifiable, Equatable {
    let id: String
    let role: String
    let name: String
    let title: String?
    let parentId: String?
    let status: String
    let profile: String?
    let model: String
    let branch: String?

    var roleLabel: String {
        role.uppercased()
    }

    var displayTitle: String? {
        guard let title, title != name else { return nil }
        return title
    }

    var isRunning: Bool {
        status == "running"
    }

    static func make(_ agent: AgentRecord) -> CollabAgentNode {
        CollabAgentNode(
            id: agent.id,
            role: agent.role,
            name: agent.name,
            title: agent.title,
            parentId: agent.parentId,
            status: agent.status,
            profile: agent.profile,
            model: agent.model,
            branch: agent.branch
        )
    }
}

struct CollabManagerGroup: Identifiable, Equatable {
    let manager: CollabAgentNode
    let workers: [CollabAgentNode]

    var id: String {
        manager.id
    }
}

struct CollabSnapshot: Equatable {
    let leader: CollabAgentNode?
    let managers: [CollabManagerGroup]
    let unassignedWorkers: [CollabAgentNode]
    let verifiers: [CollabAgentNode]
    let labour: [CollabAgentNode]
    let totalAgents: Int
    let runningAgents: Int
    let totalTasks: Int
    let completedTasks: Int
    let activeTasks: Int
    let hasPlan: Bool
    let sessionStatus: String
    let goal: String

    var isEmpty: Bool {
        leader == nil && managers.isEmpty && unassignedWorkers.isEmpty
            && verifiers.isEmpty && labour.isEmpty
    }

    var taskFraction: Double {
        guard totalTasks > 0 else { return 0 }
        return Double(completedTasks) / Double(totalTasks)
    }

    var summary: String {
        var parts = [
            "\(runningAgents) active",
            "\(totalAgents) agent\(totalAgents == 1 ? "" : "s")",
        ]
        if hasPlan {
            parts.append("\(completedTasks)/\(totalTasks) tasks")
        }
        return parts.joined(separator: " · ")
    }

    static func build(
        agents: [AgentRecord],
        plan: TeamPlan?,
        statuses: [String: String],
        sessionStatus: String,
        goal: String
    ) -> CollabSnapshot {
        let nodes = agents.map(CollabAgentNode.make)
        let leader = nodes.first { $0.role == "leader" }
        let managers = nodes
            .filter { $0.role == "manager" }
            .sorted(by: nodeOrder)
        let workers = nodes
            .filter { $0.role == "worker" }
            .sorted(by: nodeOrder)
        let groups = managers.map { manager in
            CollabManagerGroup(
                manager: manager,
                workers: workers.filter { $0.parentId == manager.id }
            )
        }
        let assignedWorkerIDs = Set(groups.flatMap { $0.workers.map(\.id) })
        let unassignedWorkers = workers.filter { !assignedWorkerIDs.contains($0.id) }
        let verifiers = nodes
            .filter { $0.role == "verifier" }
            .sorted(by: nodeOrder)
        let labour = nodes
            .filter { $0.role == "labour" }
            .sorted(by: nodeOrder)
        let allWorkers = groups.flatMap(\.workers) + unassignedWorkers
        let totalAgents = (leader == nil ? 0 : 1) + managers.count + allWorkers.count
            + verifiers.count + labour.count
        let runningAgents = [leader].compactMap(\.self).count(where: \.isRunning)
            + managers.count(where: \.isRunning)
            + allWorkers.count(where: \.isRunning)
            + verifiers.count(where: \.isRunning)
            + labour.count(where: \.isRunning)
        let tasks = plan?.tasks ?? []
        let completedTasks = tasks.count {
            let status = statuses[$0.id]
            return status == "completed" || status == "skipped"
        }
        let activeTasks = tasks.count {
            let status = statuses[$0.id]
            return status == "running" || status == "queued"
        }
        return CollabSnapshot(
            leader: leader,
            managers: groups,
            unassignedWorkers: unassignedWorkers,
            verifiers: verifiers,
            labour: labour,
            totalAgents: totalAgents,
            runningAgents: runningAgents,
            totalTasks: tasks.count,
            completedTasks: completedTasks,
            activeTasks: activeTasks,
            hasPlan: plan != nil,
            sessionStatus: sessionStatus,
            goal: goal
        )
    }

    private static func nodeOrder(_ lhs: CollabAgentNode, _ rhs: CollabAgentNode) -> Bool {
        let left = lhs.title ?? lhs.name
        let right = rhs.title ?? rhs.name
        if left == right {
            return lhs.id < rhs.id
        }
        return left.localizedStandardCompare(right) == .orderedAscending
    }
}
