import CoreGraphics
import Foundation

/// A live, deterministic projection of the agent org tree.
///
/// The graph is built from the same agents, plan, task queue, and event window
/// the rest of the app already has. It deliberately has no daemon-side mutable
/// state: every render is a pure function of session facts, so the tree can
/// animate from one truthful snapshot to the next without a second source of
/// truth.
struct CollabGraphMetrics: Equatable {
    let totalAgents: Int
    let runningAgents: Int
    let queuedAgents: Int
    let idleAgents: Int
    let failedAgents: Int
    let totalTasks: Int
    let completedTasks: Int
    let activeTasks: Int
    let blockedTasks: Int
    let readyTasks: Int
    let mergeQueueDepth: Int
    let mergeConflicts: Int
    let attentionCount: Int
    let planPresent: Bool
    let planRevision: Int

    var planProgress: Double {
        guard totalTasks > 0 else { return 0 }
        return Double(completedTasks) / Double(totalTasks)
    }

    var headline: String {
        var parts = ["\(runningAgents) active", "\(totalAgents) agents"]
        if planPresent {
            parts.append("\(completedTasks)/\(totalTasks) tasks")
        }
        if attentionCount > 0 {
            parts.append("\(attentionCount) attention")
        }
        return parts.joined(separator: " · ")
    }

    var health: String {
        if failedAgents > 0 || blockedTasks > 0 || mergeConflicts > 0 {
            return "attention"
        }
        if runningAgents > 0 || activeTasks > 0 {
            return "running"
        }
        return "idle"
    }
}

struct CollabGraphNode: Identifiable, Equatable {
    enum Kind: String, Comparable {
        case leader
        case manager
        case worker
        case verifier
        case labour
        case group
        case plannedTask

        static func < (lhs: Kind, rhs: Kind) -> Bool {
            rank(lhs) < rank(rhs)
        }

        static func rank(_ kind: Kind) -> Int {
            switch kind {
            case .leader: 0
            case .manager: 1
            case .worker: 2
            case .verifier: 3
            case .labour: 4
            case .group: 5
            case .plannedTask: 6
            }
        }
    }

    let id: String
    let kind: Kind
    let parentID: String?
    let name: String
    let title: String?
    let status: String
    let taskStatus: String?
    let taskID: String?
    let taskTitle: String?
    let department: String?
    let objective: String?
    let profile: String?
    let model: String
    let branch: String?
    let activity: String?
    let activeTool: String?
    let deps: [String]
    let acceptance: [String]
    let writeScope: [String]
    let attempts: UInt32
    let lastError: String?
    let ready: Bool
    let blockedBy: [String]
    let verifierStatus: String?
    let nextAttemptAt: String?
    let progress: Double?
    let isPlaceholder: Bool
    let size: CGSize

    var depth = 0
    var position = CGPoint.zero
    var childCount = 0
    var activeDescendants = 0
    var totalDescendants = 0

    var statusColorKey: String {
        taskStatus ?? status
    }

    var isRunning: Bool {
        ["running", "verifying", "merging", "thinking", "pausing", "cancelling"]
            .contains(status.lowercased())
            || ["running", "queued", "pausing", "cancelling"].contains(taskStatus?.lowercased() ?? "")
    }

    var displayTitle: String? {
        guard let title, title != name else { return nil }
        return title
    }

    var taskLine: String? {
        if let taskID, let taskTitle, !taskTitle.isEmpty {
            return "\(taskID) · \(taskTitle)"
        }
        if let taskTitle, !taskTitle.isEmpty {
            return taskTitle
        }
        return nil
    }
}

struct CollabGraphEdge: Identifiable, Equatable {
    let id: String
    let parentID: String
    let childID: String
    let status: String
    let active: Bool
    let from: CGPoint
    let to: CGPoint
}

struct CollabGraphLayout: Equatable {
    let nodes: [CollabGraphNode]
    let edges: [CollabGraphEdge]
    let size: CGSize
    let metrics: CollabGraphMetrics
    let rootID: String?
    let signature: String

    var isEmpty: Bool {
        nodes.isEmpty
    }

    static func empty(goal: String) -> CollabGraphLayout {
        CollabGraphLayout(
            nodes: [],
            edges: [],
            size: .zero,
            metrics: CollabGraphMetrics(
                totalAgents: 0,
                runningAgents: 0,
                queuedAgents: 0,
                idleAgents: 0,
                failedAgents: 0,
                totalTasks: 0,
                completedTasks: 0,
                activeTasks: 0,
                blockedTasks: 0,
                readyTasks: 0,
                mergeQueueDepth: 0,
                mergeConflicts: 0,
                attentionCount: 0,
                planPresent: false,
                planRevision: 0
            ),
            rootID: nil,
            signature: "empty:\(goal)"
        )
    }
}

enum CollabGraphBuilder {
    private static let cardPadding: CGFloat = 28
    private static let siblingGap: CGFloat = 22
    private static let levelGap: CGFloat = 108

    private struct NodeSeed {
        let id: String
        let kind: CollabGraphNode.Kind
        let parentID: String?
        let name: String
        let title: String?
        let status: String
        let taskStatus: String?
        let taskID: String?
        let taskTitle: String?
        let department: String?
        let objective: String?
        let profile: String?
        let model: String
        let branch: String?
        let activity: String?
        let activeTool: String?
        let deps: [String]
        let acceptance: [String]
        let writeScope: [String]
        let attempts: UInt32
        let lastError: String?
        let ready: Bool
        let blockedBy: [String]
        let verifierStatus: String?
        let nextAttemptAt: String?
        let progress: Double?
        let isPlaceholder: Bool
        let size: CGSize
    }

    static func build(
        agents: [AgentRecord],
        plan: TeamPlan?,
        statuses: [String: String],
        scheduled: [SchedulerTask],
        events: [Event],
        sessionStatus: String,
        goal: String,
        mergeQueue: [MergeQueueEntry],
        includePlanned: Bool = true,
        orchestration: OrchestrationSnapshot? = nil
    ) -> CollabGraphLayout {
        let planTasks = plan?.tasks ?? []
        let taskByID = Dictionary(uniqueKeysWithValues: planTasks.map { ($0.id, $0) })
        let departments = plan?.departments ?? []
        var statusMap = mergedStatuses(plan: plan, events: statuses, scheduled: scheduled)
        for task in orchestration?.tasks ?? [] {
            statusMap[task.taskId] = task.status
        }
        let activity = activityByAgent(events)
        let eventTaskMap = taskAssignmentFromEvents(events)
        let scheduledByTask = Dictionary(
            uniqueKeysWithValues: scheduled.map { ($0.taskID, $0) }
        )
        let orchestrationByTask = Dictionary(
            uniqueKeysWithValues: (orchestration?.tasks ?? []).map { ($0.taskId, $0) }
        )

        let realAgents = agents.filter {
            ["leader", "manager", "worker", "verifier", "labour"].contains($0.role)
        }
        let agentByID = Dictionary(uniqueKeysWithValues: realAgents.map { ($0.id, $0) })

        var childIDs: [String: [String]] = [:]
        var seeds: [String: NodeSeed] = [:]

        let leader = realAgents
            .filter { $0.role == "leader" }
            .sorted { lhs, rhs in
                let left = leaderStatusRank(lhs.status)
                let right = leaderStatusRank(rhs.status)
                if left != right {
                    return left < right
                }
                if lhs.createdAt != rhs.createdAt {
                    return lhs.createdAt < rhs.createdAt
                }
                return lhs.id < rhs.id
            }
            .first

        let rootID: String
        if let leader {
            rootID = leader.id
        } else if !realAgents.isEmpty || (includePlanned && (!planTasks.isEmpty || !departments.isEmpty)) {
            rootID = "__team_root__"
            seeds[rootID] = NodeSeed(
                id: rootID,
                kind: .group,
                parentID: nil,
                name: "Team",
                title: nil,
                status: sessionStatus,
                taskStatus: nil,
                taskID: nil,
                taskTitle: nil,
                department: nil,
                objective: goal,
                profile: nil,
                model: "",
                branch: nil,
                activity: "waiting for the Leader",
                activeTool: nil,
                deps: [],
                acceptance: [],
                writeScope: [],
                attempts: 0,
                lastError: nil,
                ready: false,
                blockedBy: [],
                verifierStatus: nil,
                nextAttemptAt: nil,
                progress: nil,
                isPlaceholder: true,
                size: size(for: .group)
            )
        } else {
            return .empty(goal: goal)
        }

        if let leader {
            seeds[leader.id] = seed(
                for: leader,
                task: nil,
                taskStatus: nil,
                department: nil,
                objective: goal,
                activity: activity[leader.id],
                scheduled: nil,
                orchestration: nil,
                statuses: statusMap,
                planTasks: planTasks
            )
        }

        let managerByTitle: [String: AgentRecord] = realAgents
            .filter { $0.role == "manager" }
            .reduce(into: [:]) { partial, agent in
                for candidate in [agent.title, agent.name].compactMap(\.self) {
                    partial[candidate.lowercased()] = agent
                }
            }

        // Real agents: attach to their actual parent, with a synthetic
        // unassigned branch when the parent is missing.
        let unassignedID = "__unassigned_agents__"
        var needsUnassigned = false
        for agent in realAgents.sorted(by: agentOrder) {
            // The runtime has one Leader per session. If stale/duplicate leader
            // rows exist after a crash, render only the healthiest one as root.
            if agent.role == "leader" {
                continue
            }
            let taskID = agent.taskId ?? eventTaskMap.taskByAgent[agent.id]
            let task = taskID.flatMap { taskByID[$0] }
            let department = agent.department
                ?? eventTaskMap.departmentByAgent[agent.id]
                ?? task?.department
            let manager = departments.first {
                $0.name.caseInsensitiveCompare(department ?? "") == .orderedSame
                    || $0.managerTitle.caseInsensitiveCompare(department ?? "") == .orderedSame
            }
            let resolvedParent: String = {
                if let parentID = agent.parentId, agentByID[parentID] != nil {
                    return parentID
                }
                if agent.role == "manager" {
                    return rootID
                }
                if agent.role == "verifier" {
                    if let manager {
                        return managerByTitle[manager.managerTitle.lowercased()]?.id
                            ?? managerByTitle[manager.name.lowercased()]?.id
                            ?? rootID
                    }
                    return rootID
                }
                if agent.role == "worker" {
                    if let manager {
                        return managerByTitle[manager.managerTitle.lowercased()]?.id
                            ?? managerByTitle[manager.name.lowercased()]?.id
                            ?? rootID
                    }
                    return rootID
                }
                if agent.role == "labour", let parentID = agent.parentId, agentByID[parentID] != nil {
                    return parentID
                }
                needsUnassigned = true
                return unassignedID
            }()
            let objective = agent.role == "leader"
                ? goal
                : departments.first {
                    $0.name.caseInsensitiveCompare(department ?? "") == .orderedSame
                        || $0.managerTitle.caseInsensitiveCompare(department ?? "") == .orderedSame
                }?.objective
            seeds[agent.id] = seed(
                for: agent,
                task: task,
                taskStatus: taskID.flatMap { statusMap[$0] },
                department: department,
                objective: objective,
                activity: activity[agent.id],
                scheduled: taskID.flatMap { scheduledByTask[$0] },
                orchestration: taskID.flatMap { orchestrationByTask[$0] },
                statuses: statusMap,
                planTasks: planTasks
            )
            childIDs[resolvedParent, default: []].append(agent.id)
        }

        if needsUnassigned {
            seeds[unassignedID] = NodeSeed(
                id: unassignedID,
                kind: .group,
                parentID: rootID,
                name: "Unassigned",
                title: nil,
                status: "idle",
                taskStatus: nil,
                taskID: nil,
                taskTitle: nil,
                department: nil,
                objective: "Agents without a live parent",
                profile: nil,
                model: "",
                branch: nil,
                activity: nil,
                activeTool: nil,
                deps: [],
                acceptance: [],
                writeScope: [],
                attempts: 0,
                lastError: nil,
                ready: false,
                blockedBy: [],
                verifierStatus: nil,
                nextAttemptAt: nil,
                progress: nil,
                isPlaceholder: true,
                size: size(for: .group)
            )
            childIDs[rootID, default: []].append(unassignedID)
        }

        // Planned departments without a live Manager become dashed nodes, so
        // the tree reflects the intended org before lazy creation catches up.
        var managerNodeIDByDepartment: [String: String] = [:]
        if includePlanned {
            for department in departments {
                let manager = managerByTitle[department.managerTitle.lowercased()]
                    ?? managerByTitle[department.name.lowercased()]
                if let manager {
                    managerNodeIDByDepartment[department.name] = manager.id
                    continue
                }
                let id = "__planned_manager__\(department.name)"
                managerNodeIDByDepartment[department.name] = id
                seeds[id] = NodeSeed(
                    id: id,
                    kind: .manager,
                    parentID: rootID,
                    name: department.managerTitle,
                    title: department.managerTitle,
                    status: "planned",
                    taskStatus: nil,
                    taskID: nil,
                    taskTitle: nil,
                    department: department.name,
                    objective: department.objective,
                    profile: department.profile,
                    model: "",
                    branch: nil,
                    activity: "will be created when work is ready",
                    activeTool: nil,
                    deps: [],
                    acceptance: [],
                    writeScope: [],
                    attempts: 0,
                    lastError: nil,
                    ready: false,
                    blockedBy: [],
                    verifierStatus: nil,
                    nextAttemptAt: nil,
                    progress: departmentProgress(
                        department: department.name,
                        planTasks: planTasks,
                        statuses: statusMap
                    ),
                    isPlaceholder: true,
                    size: size(for: .manager)
                )
                childIDs[rootID, default: []].append(id)
            }
        }

        // Plan tasks without a live Worker stay visible as ghost cards. They
        // are the future work the scheduler is holding, not fake agents.
        let ownedTasks = Set(realAgents.compactMap { $0.taskId ?? eventTaskMap.taskByAgent[$0.id] })
        if includePlanned {
            for task in planTasks where !ownedTasks.contains(task.id) {
                let taskStatus = statusMap[task.id] ?? "pending"
                if ["completed", "skipped"].contains(taskStatus) {
                    continue
                }
                let id = "__planned_task__\(task.id)"
                let managerID = managerNodeIDByDepartment[task.department] ?? rootID
                let orchestrationTask = orchestrationByTask[task.id]
                seeds[id] = NodeSeed(
                    id: id,
                    kind: .plannedTask,
                    parentID: managerID,
                    name: task.title.isEmpty ? task.id : task.title,
                    title: task.id,
                    status: statusMap[task.id] ?? "pending",
                    taskStatus: statusMap[task.id] ?? "pending",
                    taskID: task.id,
                    taskTitle: task.title,
                    department: task.department,
                    objective: nil,
                    profile: task.profile,
                    model: "",
                    branch: nil,
                    activity: orchestrationTask?.ready == true
                        ? "ready to dispatch"
                        : (orchestrationTask?.blockedBy.isEmpty == false
                            ? "blocked by dependencies"
                            : "waiting for a Worker"),
                    activeTool: nil,
                    deps: task.deps,
                    acceptance: task.acceptance,
                    writeScope: task.writeScope,
                    attempts: UInt32(clamping: orchestrationTask?.attempts ?? 0),
                    lastError: orchestrationTask?.lastError,
                    ready: orchestrationTask?.ready ?? false,
                    blockedBy: orchestrationTask?.blockedBy ?? [],
                    verifierStatus: orchestrationTask?.verifierStatus,
                    nextAttemptAt: orchestrationTask?.nextAttemptAt,
                    progress: nil,
                    isPlaceholder: true,
                    size: size(for: .plannedTask)
                )
                childIDs[managerID, default: []].append(id)
            }
        }

        // Stable sibling order: tier first, then title/name/id.
        for (parent, ids) in childIDs {
            childIDs[parent] = ids.sorted { lhs, rhs in
                guard let left = seeds[lhs], let right = seeds[rhs] else { return lhs < rhs }
                if left.kind != right.kind {
                    return left.kind < right.kind
                }
                let leftLabel = left.title ?? left.name
                let rightLabel = right.title ?? right.name
                if leftLabel != rightLabel {
                    return leftLabel.localizedStandardCompare(rightLabel) == .orderedAscending
                }
                return lhs < rhs
            }
        }

        // Build mutable nodes and compute subtree statistics.
        var nodes = seeds.map { _, seed in
            CollabGraphNode(
                id: seed.id,
                kind: seed.kind,
                parentID: seed.parentID,
                name: seed.name,
                title: seed.title,
                status: seed.status,
                taskStatus: seed.taskStatus,
                taskID: seed.taskID,
                taskTitle: seed.taskTitle,
                department: seed.department,
                objective: seed.objective,
                profile: seed.profile,
                model: seed.model,
                branch: seed.branch,
                activity: seed.activity,
                activeTool: seed.activeTool,
                deps: seed.deps,
                acceptance: seed.acceptance,
                writeScope: seed.writeScope,
                attempts: seed.attempts,
                lastError: seed.lastError,
                ready: seed.ready,
                blockedBy: seed.blockedBy,
                verifierStatus: seed.verifierStatus,
                nextAttemptAt: seed.nextAttemptAt,
                progress: seed.progress,
                isPlaceholder: seed.isPlaceholder,
                size: seed.size
            )
        }
        let nodeLookup = Dictionary(uniqueKeysWithValues: nodes.map { ($0.id, $0) })
        for index in nodes.indices {
            let id = nodes[index].id
            let children = childIDs[id] ?? []
            let allDescendants = descendants(of: id, childIDs: childIDs, nodes: nodeLookup)
            nodes[index].childCount = children.count
            nodes[index].activeDescendants = allDescendants.count { node in
                ["running", "queued", "verifying"].contains(node.status.lowercased())
                    || ["running", "queued"].contains(node.taskStatus?.lowercased() ?? "")
            }
            nodes[index].totalDescendants = allDescendants.count
        }

        let layout = layoutTree(
            rootID: rootID,
            nodes: &nodes,
            childIDs: childIDs
        )
        let nodeByID = Dictionary(uniqueKeysWithValues: nodes.map { ($0.id, $0) })
        let edges = childIDs.flatMap { parent, children in
            children.compactMap { childID -> CollabGraphEdge? in
                guard let parentNode = nodeByID[parent], let child = nodeByID[childID] else {
                    return nil
                }
                let from = CGPoint(
                    x: parentNode.position.x + parentNode.size.width / 2,
                    y: parentNode.position.y
                )
                let to = CGPoint(
                    x: child.position.x - child.size.width / 2,
                    y: child.position.y
                )
                let active = ["running", "queued", "verifying"].contains(child.status.lowercased())
                    || ["running", "queued"].contains(child.taskStatus?.lowercased() ?? "")
                return CollabGraphEdge(
                    id: "\(parent)->\(childID)",
                    parentID: parent,
                    childID: childID,
                    status: child.statusColorKey,
                    active: active,
                    from: from,
                    to: to
                )
            }
        }

        let visibleAgents = realAgents.filter {
            $0.role != "leader" || $0.id == leader?.id
        }
        let metrics = metrics(
            agents: visibleAgents,
            plan: plan,
            statuses: statusMap,
            mergeQueue: mergeQueue,
            events: events
        )
        let signature = nodes
            .sorted { $0.id < $1.id }
            .map { "\($0.id):\($0.status):\($0.taskStatus ?? "-"):\($0.activity ?? "-")" }
            .joined(separator: "|")

        return CollabGraphLayout(
            nodes: nodes.sorted { lhs, rhs in
                if lhs.depth != rhs.depth {
                    return lhs.depth < rhs.depth
                }
                if lhs.position.x != rhs.position.x {
                    return lhs.position.x < rhs.position.x
                }
                return lhs.id < rhs.id
            },
            edges: edges.sorted { $0.id < $1.id },
            size: layout,
            metrics: metrics,
            rootID: rootID,
            signature: signature
        )
    }

    private static func seed(
        for agent: AgentRecord,
        task: PlanTask?,
        taskStatus: String?,
        department: String?,
        objective: String?,
        activity: AgentActivity?,
        scheduled: SchedulerTask?,
        orchestration: OrchestrationTask?,
        statuses: [String: String],
        planTasks: [PlanTask]
    ) -> NodeSeed {
        let kind: CollabGraphNode.Kind = switch agent.role {
        case "leader": .leader
        case "manager": .manager
        case "verifier": .verifier
        case "labour": .labour
        default: .worker
        }
        let taskID = agent.taskId ?? (kind == .worker || kind == .verifier || kind == .labour
            ? task?.id
            : nil)
        let resolvedTask = task ?? taskID.flatMap { id in planTasks.first { $0.id == id } }
        let departmentName = department ?? resolvedTask?.department
        let progress = kind == .manager
            ? departmentProgress(
                department: departmentName,
                planTasks: planTasks,
                statuses: statuses
            )
            : nil
        return NodeSeed(
            id: agent.id,
            kind: kind,
            parentID: agent.parentId,
            name: agent.name,
            title: agent.title,
            status: agent.status,
            taskStatus: taskStatus ?? resolvedTask.map { statuses[$0.id] ?? "pending" },
            taskID: taskID ?? resolvedTask?.id,
            taskTitle: resolvedTask?.title,
            department: departmentName,
            objective: objective,
            profile: agent.profile,
            model: agent.model,
            branch: agent.branch,
            activity: activity?.text,
            activeTool: activity?.tool,
            deps: resolvedTask?.deps ?? [],
            acceptance: resolvedTask?.acceptance ?? [],
            writeScope: resolvedTask?.writeScope ?? [],
            attempts: UInt32(clamping: orchestration?.attempts ?? 0) > 0
                ? UInt32(clamping: orchestration?.attempts ?? 0)
                : (scheduled?.attempts ?? 0),
            lastError: orchestration?.lastError ?? scheduled?.lastError,
            ready: orchestration?.ready ?? false,
            blockedBy: orchestration?.blockedBy ?? [],
            verifierStatus: orchestration?.verifierStatus,
            nextAttemptAt: orchestration?.nextAttemptAt ?? scheduled?.nextAttemptAt,
            progress: progress,
            isPlaceholder: false,
            size: size(for: kind)
        )
    }

    private static func size(for kind: CollabGraphNode.Kind) -> CGSize {
        switch kind {
        case .leader: CGSize(width: 238, height: 82)
        case .manager: CGSize(width: 204, height: 72)
        case .worker: CGSize(width: 184, height: 70)
        case .verifier: CGSize(width: 160, height: 58)
        case .labour: CGSize(width: 154, height: 54)
        case .group: CGSize(width: 168, height: 58)
        case .plannedTask: CGSize(width: 170, height: 62)
        }
    }

    private static func layoutTree(
        rootID: String,
        nodes: inout [CollabGraphNode],
        childIDs: [String: [String]]
    ) -> CGSize {
        let index = Dictionary(uniqueKeysWithValues: nodes.indices.map { (nodes[$0].id, $0) })
        var measured: [String: CGFloat] = [:]
        var depths: [String: Int] = [:]
        var levelWidths: [Int: CGFloat] = [:]
        var maxDepth = 0

        func assignDepth(_ id: String, _ depth: Int) {
            guard index[id] != nil else { return }
            depths[id] = depth
            maxDepth = max(maxDepth, depth)
            levelWidths[depth] = max(levelWidths[depth] ?? 0, nodes[index[id]!].size.width)
            for child in childIDs[id] ?? [] {
                assignDepth(child, depth + 1)
            }
        }

        func measure(_ id: String) -> CGFloat {
            if let value = measured[id] {
                return value
            }
            guard let position = index[id] else { return 0 }
            let children = childIDs[id] ?? []
            let ownHeight = nodes[position].size.height
            guard !children.isEmpty else {
                measured[id] = ownHeight
                return ownHeight
            }
            let childHeight = children.enumerated().reduce(CGFloat.zero) { partial, pair in
                partial + measure(pair.element) + (pair.offset == 0 ? 0 : siblingGap)
            }
            let height = max(ownHeight, childHeight)
            measured[id] = height
            return height
        }

        assignDepth(rootID, 0)
        var levelLeft: [Int: CGFloat] = [:]
        var cursor = cardPadding
        for depth in 0 ... maxDepth {
            let width = levelWidths[depth] ?? 160
            levelLeft[depth] = cursor
            cursor += width + 68
        }
        let contentWidth = cursor - 68 + cardPadding

        func place(_ id: String, top: CGFloat, depth: Int) {
            guard let position = index[id] else { return }
            let height = measure(id)
            let levelWidth = levelWidths[depth] ?? nodes[position].size.width
            let left = levelLeft[depth] ?? cardPadding
            let centerX = left + levelWidth / 2
            let centerY = top + height / 2
            nodes[position].depth = depth
            nodes[position].position = CGPoint(x: centerX, y: centerY)
            let children = childIDs[id] ?? []
            guard !children.isEmpty else { return }
            let childrenHeight = children.enumerated().reduce(CGFloat.zero) { partial, pair in
                partial + measure(pair.element) + (pair.offset == 0 ? 0 : siblingGap)
            }
            var childTop = top + (height - childrenHeight) / 2
            for child in children {
                place(child, top: childTop, depth: depth + 1)
                childTop += measure(child) + siblingGap
            }
        }

        let rootHeight = measure(rootID)
        place(rootID, top: cardPadding, depth: 0)
        let maxY = nodes.map { $0.position.y + $0.size.height / 2 }.max() ?? 0
        let size = CGSize(
            width: contentWidth,
            height: rootHeight + cardPadding * 2
        )
        _ = maxY
        return size
    }

    private static func descendants(
        of id: String,
        childIDs: [String: [String]],
        nodes: [String: CollabGraphNode]
    ) -> [CollabGraphNode] {
        var result: [CollabGraphNode] = []
        var visited = Set<String>()
        func walk(_ id: String) {
            for child in childIDs[id] ?? [] where !visited.contains(child) {
                visited.insert(child)
                if let node = nodes[child] {
                    result.append(node)
                }
                walk(child)
            }
        }
        walk(id)
        return result
    }

    private static func departmentProgress(
        department: String?,
        planTasks: [PlanTask],
        statuses: [String: String]
    ) -> Double? {
        guard let department else { return nil }
        let tasks = planTasks.filter { $0.department == department }
        guard !tasks.isEmpty else { return nil }
        let completed = tasks.count {
            let status = statuses[$0.id]
            return status == "completed" || status == "skipped"
        }
        return Double(completed) / Double(tasks.count)
    }

    private static func mergedStatuses(
        plan: TeamPlan?,
        events: [String: String],
        scheduled: [SchedulerTask]
    ) -> [String: String] {
        var result = [String: String]()
        for task in scheduled {
            result[task.taskID] = task.status
        }
        for task in plan?.tasks ?? [] where result[task.id] == nil {
            result[task.id] = "pending"
        }
        // Event status is the freshest value between task.queue reloads.
        for (taskID, status) in events {
            result[taskID] = status
        }
        return result
    }

    private struct EventAssignments {
        var taskByAgent: [String: String] = [:]
        var departmentByAgent: [String: String] = [:]
    }

    private static func taskAssignmentFromEvents(_ events: [Event]) -> EventAssignments {
        var assignments = EventAssignments()
        for event in events.sorted(by: { $0.seq < $1.seq }) where event.type == "agent.spawned" {
            guard let payload = event.payload.objectValue,
                  let agentID = payload["agent_id"]?.stringValue
            else { continue }
            if let taskID = payload["plan_task_id"]?.stringValue
                ?? payload["task_id"]?.stringValue
            {
                assignments.taskByAgent[agentID] = taskID
            }
            if let department = payload["department"]?.stringValue {
                assignments.departmentByAgent[agentID] = department
            }
        }
        return assignments
    }

    private struct AgentActivity {
        let text: String?
        let tool: String?
        let seq: UInt64
    }

    private static func activityByAgent(_ events: [Event]) -> [String: AgentActivity] {
        var result: [String: AgentActivity] = [:]
        for event in events.sorted(by: { $0.seq < $1.seq }) {
            guard let payload = event.payload.objectValue,
                  let agentID = payload["agent_id"]?.stringValue
            else { continue }
            var text: String?
            var tool: String?
            switch event.type {
            case "tool.started":
                tool = payload["name"]?.stringValue
                text = tool.map { "using \($0)" }
            case "tool.finished":
                text = payload["name"]?.stringValue.map { "\($0) finished" }
            case "agent.started":
                text = "thinking"
            case "agent.message":
                text = "reported a result"
            case "agent.turn_finished":
                text = "turn complete"
            case "agent.spawned":
                text = "joined the team"
            case "agent.finished":
                text = "finished"
            case "agent.failed":
                text = "failed"
            case "agent.cancelled":
                text = "cancelled"
            case "agent.capacity_wait":
                text = "waiting for capacity"
            case "agent.capacity_ready":
                text = "capacity available"
            case "agent.budget_exceeded":
                text = "budget stop"
            case "permission.denied":
                text = "permission denied"
            case "coordination.note":
                let action = payload["action"]?.stringValue ?? "note"
                if let message = payload["message"]?.stringValue {
                    text = "\(action): \(message)"
                } else {
                    text = action
                }
            case "plan_task.started":
                text = payload["task_id"]?.stringValue.map { "started \($0)" }
            case "plan_task.finished":
                text = payload["task_id"]?.stringValue.map { "finished \($0)" }
            default:
                break
            }
            if let existing = result[agentID], existing.seq > event.seq {
                continue
            }
            let nextTool: String? = if event.type == "tool.started" {
                tool
            } else if event.type == "tool.finished" {
                nil
            } else {
                result[agentID]?.tool
            }
            result[agentID] = AgentActivity(
                text: text ?? result[agentID]?.text,
                tool: nextTool,
                seq: event.seq
            )
        }
        return result
    }

    private static func metrics(
        agents: [AgentRecord],
        plan: TeamPlan?,
        statuses: [String: String],
        mergeQueue: [MergeQueueEntry],
        events: [Event]
    ) -> CollabGraphMetrics {
        let tasks = plan?.tasks ?? []
        let completed = tasks.count {
            ["completed", "skipped"].contains(statuses[$0.id] ?? "")
        }
        let active = tasks.count {
            ["running", "queued", "paused", "cancelling", "pausing"]
                .contains(statuses[$0.id] ?? "")
        }
        let blocked = tasks.count { statuses[$0.id] == "blocked" }
        let ready = tasks.count { task in
            let status = statuses[task.id] ?? "pending"
            guard ["pending", "interrupted", "failed", "cancelled"].contains(status) else {
                return false
            }
            return task.deps.allSatisfy {
                ["completed", "skipped"].contains(statuses[$0] ?? "")
            }
        }
        let failedAgents = agents.count {
            ["failed", "cancelled", "interrupted", "blocked"].contains($0.status)
        }
        let conflicts = mergeQueue.count { $0.status == "conflict" }
        let queueDepth = mergeQueue.count {
            ["queued", "merging", "checking", "conflict", "failed", "interrupted"]
                .contains($0.status)
        }
        let attention = failedAgents + blocked + conflicts
        return CollabGraphMetrics(
            totalAgents: agents.count,
            runningAgents: agents.count { ["running", "verifying", "merging"].contains($0.status) },
            queuedAgents: agents.count { $0.status == "queued" },
            idleAgents: agents.count { ["idle", "done", "completed"].contains($0.status) },
            failedAgents: failedAgents,
            totalTasks: tasks.count,
            completedTasks: completed,
            activeTasks: active,
            blockedTasks: blocked,
            readyTasks: ready,
            mergeQueueDepth: queueDepth,
            mergeConflicts: conflicts,
            attentionCount: attention,
            planPresent: plan != nil,
            planRevision: events.count { $0.type == "team_plan.submitted" }
        )
    }

    private static func leaderStatusRank(_ status: String) -> Int {
        switch status.lowercased() {
        case "running": 0
        case "queued": 1
        case "idle", "done", "completed": 2
        case "paused", "waiting_input": 3
        case "failed", "cancelled", "interrupted", "blocked": 4
        default: 5
        }
    }

    private static func agentOrder(_ lhs: AgentRecord, _ rhs: AgentRecord) -> Bool {
        let left = lhs.title ?? lhs.name
        let right = rhs.title ?? rhs.name
        if left != right {
            return left.localizedStandardCompare(right) == .orderedAscending
        }
        return lhs.id < rhs.id
    }
}
