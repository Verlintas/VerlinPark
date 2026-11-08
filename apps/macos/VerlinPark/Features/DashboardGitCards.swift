import SwiftUI

struct CheckpointsCard: View {
    @Environment(AppModel.self) private var model
    @State private var deleting: SessionCheckpoint?
    @State private var restoring: SessionCheckpoint?
    @State private var previewing: SessionCheckpoint?

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Session checkpoints")
                    Text(model.selectedSession?.goal ?? "No session selected")
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                        .lineLimit(1)
                }
                Spacer()
                if model.checkpointBusy || model.checkpointStatsBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Stats") {
                    Task { await model.loadCheckpointStats() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.selectedSession == nil || model.checkpointStatsBusy)
                Button("Prune >30d, keep 5") {
                    Task { await model.pruneCheckpoints() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.checkpointBusy || model.selectedSession == nil || model.checkpoints.isEmpty)
            }
            Text(
                "Checkpoints snapshot the active conversation and worktree, including uncommitted tracked/untracked non-ignored changes. Restore resets only the session branch/worktree to a checkpoint commit; fork branches and event history are unaffected."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)

            Divider().opacity(0.2)
            Text("AUTOMATIC RETENTION")
                .font(.system(size: 8, weight: .semibold, design: .monospaced))
                .foregroundStyle(.tertiary)
            HStack(spacing: 10) {
                Toggle("Enabled", isOn: $model.checkpointPolicyEnabled)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                VStack(alignment: .leading, spacing: 3) {
                    Text("Older than days")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    TextField("30", text: $model.checkpointPolicyOlderDaysText)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 9, design: .monospaced))
                        .frame(width: 60)
                }
                VStack(alignment: .leading, spacing: 3) {
                    Text("Keep latest")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    TextField("5", text: $model.checkpointPolicyKeepLatestText)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 9, design: .monospaced))
                        .frame(width: 50)
                }
                Spacer()
                if model.checkpointPolicyBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Save policy") {
                    Task { await model.saveCheckpointPolicy() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.checkpointPolicyBusy)
            }
            if let status = model.checkpointPolicyStatus {
                Text(status)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            }

            Divider().opacity(0.2)
            Text("MANUAL CLEANUP")
                .font(.system(size: 8, weight: .semibold, design: .monospaced))
                .foregroundStyle(.tertiary)

            if model.selectedSession == nil {
                Text("Select a session to inspect its checkpoints.")
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.tertiary)
            } else if model.checkpoints.isEmpty {
                Text("No checkpoints yet. Use Fork Session… to create one.")
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.tertiary)
            } else {
                ForEach(model.checkpoints.suffix(8).reversed()) { checkpoint in
                    HStack(alignment: .top, spacing: 8) {
                        VStack(alignment: .leading, spacing: 2) {
                            HStack(spacing: 6) {
                                Text(checkpoint.label)
                                    .font(.system(size: 10, weight: .medium, design: .monospaced))
                                    .lineLimit(1)
                                if checkpoint.dirty == true {
                                    Text("dirty")
                                        .font(.system(size: 8, weight: .semibold, design: .monospaced))
                                        .foregroundStyle(.orange)
                                }
                            }
                            Text(
                                "\(String(checkpoint.commit.prefix(8))) · \(checkpoint.messageCount) msgs · \(checkpoint.createdAt.prefix(19))"
                            )
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.secondary)
                        }
                        Spacer()
                        Menu {
                            Button("Preview changes") {
                                previewing = checkpoint
                            }
                            Button("Restore worktree") {
                                restoring = checkpoint
                            }
                            Divider()
                            Button("Delete", role: .destructive) {
                                deleting = checkpoint
                            }
                        } label: {
                            Image(systemName: "ellipsis.circle")
                                .font(.system(size: 11))
                        }
                        .menuStyle(.borderlessButton)
                        .fixedSize()
                        .help("Preview, restore, or delete checkpoint")
                        .disabled(model.checkpointBusy)
                    }
                }
                if model.checkpoints.count > 8 {
                    Text("Showing the 8 most recent of \(model.checkpoints.count).")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
            }
            if let notice = model.checkpointNotice {
                Text(notice)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
        }
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .vpAppear(index: 0)
        .confirmationDialog(
            "Delete checkpoint?",
            isPresented: Binding(
                get: { deleting != nil },
                set: {
                    if !$0 {
                        deleting = nil
                    }
                }
            ),
            presenting: deleting
        ) { checkpoint in
            Button("Delete \(checkpoint.label)", role: .destructive) {
                Task { await model.deleteCheckpoint(checkpoint.id) }
                deleting = nil
            }
            Button("Cancel", role: .cancel) { deleting = nil }
        } message: { _ in
            Text(
                "This removes the checkpoint event and its snapshot ref. Existing fork branches are unaffected."
            )
        }
        .confirmationDialog(
            "Restore worktree to checkpoint?",
            isPresented: Binding(
                get: { restoring != nil },
                set: {
                    if !$0 {
                        restoring = nil
                    }
                }
            ),
            presenting: restoring
        ) { checkpoint in
            Button("Restore \(String(checkpoint.commit.prefix(8)))", role: .destructive) {
                Task { await model.restoreCheckpoint(checkpoint.id) }
                restoring = nil
            }
            Button("Cancel", role: .cancel) { restoring = nil }
        } message: { checkpoint in
            Text(
                "Resets the session branch and clean worktree to \(String(checkpoint.commit.prefix(8))). Later commits remain reachable through Git reflog; the session must be idle and clean."
            )
        }
        .sheet(item: $previewing) { checkpoint in
            CheckpointDiffSheet(checkpoint: checkpoint)
                .environment(model)
        }
    }
}

private struct CheckpointDiffSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let checkpoint: SessionCheckpoint

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 3) {
                    Text("Checkpoint diff")
                        .font(.system(size: 20, weight: .semibold, design: .rounded))
                    Text(checkpoint.label)
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(.secondary)
                    Text(
                        "Current branch → \(String(checkpoint.commit.prefix(8))) · tracked files only"
                    )
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
                }
                Spacer()
                Button("Close") { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
            .padding(20)

            Divider().opacity(0.3)

            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    content
                }
                .padding(20)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .frame(width: 780, height: 640)
        .background(.ultraThinMaterial)
        .task(id: checkpoint.id) {
            await model.loadCheckpointDiff(checkpoint.id)
        }
    }

    @ViewBuilder
    private var content: some View {
        if model.checkpointDiffBusy {
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text("Comparing current branch with the checkpoint…")
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
        } else if let notice = model.checkpointDiffNotice {
            VStack(alignment: .leading, spacing: 8) {
                Text(notice)
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.red)
                Button("Retry") {
                    Task { await model.loadCheckpointDiff(checkpoint.id) }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
            }
        } else if let diff = model.checkpointDiff {
            HStack(spacing: 18) {
                LabeledContent("Current", value: String(diff.currentCommit.prefix(12)))
                LabeledContent("Checkpoint", value: String(diff.checkpointCommit.prefix(12)))
                LabeledContent("Branch", value: diff.currentBranch)
            }
            .font(.system(size: 9, design: .monospaced))

            if !diff.clean {
                HStack(spacing: 8) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange)
                    Text(
                        "The worktree is dirty. Restore requires a clean worktree; commit or discard changes first."
                    )
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.orange)
                }
                .padding(10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(.orange.opacity(0.10), in: RoundedRectangle(cornerRadius: 9))
            }

            if diff.files.isEmpty {
                Text("No tracked content differences; the checkpoint already matches the current branch.")
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.secondary)
            } else {
                VStack(spacing: 4) {
                    ForEach(diff.files) { file in
                        HStack(spacing: 8) {
                            Text(file.path)
                                .font(.system(size: 9, design: .monospaced))
                                .lineLimit(1)
                            Spacer()
                            if file.binary {
                                Text("binary")
                                    .font(.system(size: 8, design: .monospaced))
                                    .foregroundStyle(.secondary)
                            } else {
                                Text("+\(file.additions)")
                                    .font(.system(size: 8, design: .monospaced))
                                    .foregroundStyle(.green)
                                Text("-\(file.deletions)")
                                    .font(.system(size: 8, design: .monospaced))
                                    .foregroundStyle(.red)
                            }
                        }
                        .padding(7)
                        .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 7))
                    }
                }
            }

            if !diff.patch.isEmpty {
                DisclosureGroup("Patch preview") {
                    Text(String(diff.patch.prefix(40000)))
                        .font(.system(size: 9, design: .monospaced))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.top, 6)
                    if diff.truncated || diff.patch.count > 40000 {
                        Text("Patch preview is clipped; the daemon caps the full patch at 200k characters.")
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.tertiary)
                    }
                }
                .font(.system(size: 10, weight: .medium, design: .monospaced))
            }
        } else {
            Text("Select a session with this checkpoint to preview the restore diff.")
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(.tertiary)
        }
    }
}

struct ForksCard: View {
    @Environment(AppModel.self) private var model
    @State private var showingCompare = false
    @State private var showingTree = false
    @State private var merging: SessionLineageEntry?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Fork lineage")
                    Text(model.selectedSession?.goal ?? "No session selected")
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                        .lineLimit(1)
                }
                Spacer()
                if model.lineageBusy || model.forkMergeBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Tree") {
                    showingTree = true
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.selectedSession == nil)
                Button("Refresh") {
                    Task { await model.loadLineage() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.lineageBusy || model.selectedSession == nil)
            }
            Text(
                "Parent, sibling, and child sessions created through checkpoint forks. Compare committed branch differences without changing either worktree."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)

            if model.selectedSession == nil {
                Text("Select a session to inspect its fork lineage.")
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.tertiary)
            } else if let lineage = model.sessionLineage {
                lineageSection("PARENT", entries: lineage.parent.map { [$0] } ?? [], mergeable: false)
                lineageSection("SIBLINGS", entries: lineage.siblings, mergeable: false)
                lineageSection("FORKS", entries: lineage.children, mergeable: true)
                if lineage.parent == nil, lineage.siblings.isEmpty, lineage.children.isEmpty {
                    Text("No fork lineage. Use Session → Fork Session… to start one.")
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
            } else if !model.lineageBusy {
                Text("Lineage unavailable.")
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
            if let error = model.compareError {
                Text(error)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.orange)
                    .lineLimit(2)
            }
            if let notice = model.forkMergeNotice {
                Text(notice)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
        }
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .vpAppear(index: 0)
        .sheet(isPresented: $showingCompare) {
            CompareSessionsSheet()
                .environment(model)
        }
        .sheet(isPresented: $showingTree) {
            ForkTreeSheet()
                .environment(model)
        }
        .confirmationDialog(
            "Merge fork into this session?",
            isPresented: Binding(
                get: { merging != nil },
                set: {
                    if !$0 {
                        merging = nil
                    }
                }
            ),
            presenting: merging
        ) { entry in
            Button("Merge \(entry.session.goal)") {
                Task { await model.mergeFork(entry.session.id) }
                merging = nil
            }
            Button("Cancel", role: .cancel) { merging = nil }
        } message: { _ in
            Text(
                "Merges the fork's committed branch into the current session. The source worktree must be clean; conflicts abort without changing the source branch."
            )
        }
    }

    @ViewBuilder
    private func lineageSection(
        _ title: String,
        entries: [SessionLineageEntry],
        mergeable: Bool
    ) -> some View {
        if !entries.isEmpty {
            Text(title)
                .font(.system(size: 8, weight: .semibold, design: .monospaced))
                .foregroundStyle(.tertiary)
            ForEach(entries) { entry in
                HStack(alignment: .top, spacing: 8) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(entry.session.goal)
                            .font(.system(size: 10, weight: .medium, design: .monospaced))
                            .lineLimit(1)
                        Text(
                            "\(entry.session.status) · \(entry.commit.map { String($0.prefix(8)) } ?? "no commit") · \(entry.checkpointID.map { String($0.prefix(8)) } ?? "no checkpoint")"
                        )
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.secondary)
                    }
                    Spacer()
                    Button("Open") {
                        model.selectSession(entry.session.id)
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    Button("Compare") {
                        Task {
                            await model.compareSessions(otherID: entry.session.id)
                            showingCompare = true
                        }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(model.compareBusy)
                    if mergeable {
                        Button("Merge") {
                            merging = entry
                        }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.small)
                        .disabled(model.forkMergeBusy || model.compareBusy)
                    }
                }
            }
        }
    }
}

struct ProjectForkForestCard: View {
    @Environment(AppModel.self) private var model
    @State private var showingForest = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Project fork forest")
                    Text(model.selectedProject?.name ?? "No project selected")
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.projectForkForestBusy || model.forkAuditBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Forest") { showingForest = true }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(model.selectedProject == nil)
                Button("Audit") {
                    Task { await model.auditProjectForks() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.forkAuditBusy || model.selectedProject == nil)
                Button("Repair") {
                    Task { await model.repairProjectForks() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.forkAuditBusy || model.selectedProject == nil)
            }
            Text(
                "All roots, fork descendants, checkpoint counts, and integrity issues for this project. Repair backfills missing fork links and checkpoint refs; it never rewrites historical events."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)
            if let forest = model.projectForkForest {
                HStack(spacing: 14) {
                    Text("\(forest.total) sessions")
                    Text("\(forest.roots.count) roots")
                    Text("\(forest.checkpoints) checkpoints")
                    if forest.dirtyCheckpoints > 0 {
                        Text("\(forest.dirtyCheckpoints) dirty").foregroundStyle(.orange)
                    }
                    if forest.truncated {
                        Text("truncated").foregroundStyle(.orange)
                    }
                }
                .font(.system(size: 9, design: .monospaced))
                .foregroundStyle(.secondary)
            }
            if let audit = model.forkAudit {
                Text(
                    "\(audit.issues.count) issues · \(audit.checkpoints) checkpoints · \(audit.sessions) sessions"
                )
                .font(.system(size: 9, design: .monospaced))
                .foregroundStyle(audit.issues.isEmpty ? Color.secondary : Color.orange)
            }
            if let notice = model.forkRepairNotice {
                Text(notice)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
        }
        .padding(14)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .vpAppear(index: 0)
        .sheet(isPresented: $showingForest) {
            ProjectForkForestSheet()
                .environment(model)
        }
    }
}

struct ProjectForkForestSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Project fork forest")
                        .font(.system(size: 18, weight: .semibold, design: .rounded))
                    Text("All session roots and checkpoint descendants")
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
                Spacer()
                if model.projectForkForestBusy || model.forkAuditBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Audit") {
                    Task { await model.auditProjectForks() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                Button("Repair") {
                    Task { await model.repairProjectForks() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                Button("Done") { dismiss() }
                    .keyboardShortcut(.defaultAction)
            }
            .padding(18)
            Divider().opacity(0.35)
            HSplitView {
                ScrollView {
                    if let forest = model.projectForkForest {
                        VStack(alignment: .leading, spacing: 10) {
                            HStack(spacing: 14) {
                                Text("\(forest.total) sessions")
                                Text("\(forest.roots.count) roots")
                                Text("\(forest.checkpoints) checkpoints")
                                if forest.truncated {
                                    Text("truncated").foregroundStyle(.orange)
                                }
                            }
                            .font(.system(size: 10, design: .monospaced))
                            ForEach(forest.roots) { root in
                                ForkTreeRowView(node: root, depth: 0)
                            }
                        }
                        .padding(18)
                    } else {
                        ProgressView().controlSize(.small)
                            .frame(maxWidth: .infinity, maxHeight: .infinity)
                    }
                }
                .frame(minWidth: 420)
                VStack(alignment: .leading, spacing: 8) {
                    Text("INTEGRITY AUDIT")
                        .font(.system(size: 9, weight: .semibold, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    if let audit = model.forkAudit {
                        Text(
                            "\(audit.issues.count) issues · \(audit.sessions) sessions · \(audit.checkpoints) checkpoints"
                        )
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(audit.issues.isEmpty ? Color.secondary : Color.orange)
                        ScrollView {
                            VStack(alignment: .leading, spacing: 8) {
                                ForEach(audit.issues) { issue in
                                    VStack(alignment: .leading, spacing: 2) {
                                        HStack(spacing: 6) {
                                            Text(issue.kind)
                                                .font(.system(
                                                    size: 9,
                                                    weight: .semibold,
                                                    design: .monospaced
                                                ))
                                                .foregroundStyle(issue.repairable ? Color.orange : Color.red)
                                            Text(issue.repairable ? "repairable" : "manual")
                                                .font(.system(size: 8, design: .monospaced))
                                                .foregroundStyle(.tertiary)
                                        }
                                        Text(issue.detail)
                                            .font(.system(size: 9, design: .monospaced))
                                            .foregroundStyle(.secondary)
                                            .fixedSize(horizontal: false, vertical: true)
                                    }
                                    .padding(8)
                                    .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 8))
                                }
                            }
                        }
                    } else {
                        Text("Run Audit to inspect fork links and checkpoint refs.")
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundStyle(.secondary)
                    }
                    if let notice = model.forkRepairNotice {
                        Text(notice)
                            .font(.system(size: 9, design: .monospaced))
                            .foregroundStyle(.secondary)
                    }
                }
                .padding(18)
                .frame(minWidth: 300)
            }
        }
        .frame(width: 980, height: 700)
        .onAppear {
            Task {
                await model.loadProjectForkForest()
                await model.auditProjectForks()
            }
        }
    }
}

struct ForkTreeSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Fork tree")
                        .font(.system(size: 18, weight: .semibold, design: .rounded))
                    Text("Recursive checkpoint descendants")
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
                Spacer()
                if model.forkTreeBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Refresh") {
                    Task { await model.loadForkTree() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                Button("Done") { dismiss() }
                    .keyboardShortcut(.defaultAction)
            }
            .padding(18)
            Divider().opacity(0.35)
            if let tree = model.forkTree {
                HStack(spacing: 14) {
                    Text("\(tree.total) sessions")
                        .font(.system(size: 10, design: .monospaced))
                    if tree.truncated {
                        Text("truncated")
                            .font(.system(size: 9, design: .monospaced))
                            .foregroundStyle(.orange)
                    }
                }
                .padding(.horizontal, 18)
                .padding(.top, 10)
                ScrollView {
                    ForkTreeRowView(node: tree.root, depth: 0)
                        .padding(18)
                }
            } else if model.forkTreeBusy {
                ProgressView()
                    .controlSize(.small)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                StudioEmptyView(
                    icon: "point.3.connected.trianglepath.dotted",
                    title: "No tree loaded",
                    detail: "Select a session and refresh."
                )
            }
        }
        .frame(width: 760, height: 620)
        .onAppear {
            Task { await model.loadForkTree() }
        }
    }
}

struct ForkTreeRowView: View {
    @Environment(AppModel.self) private var model
    let node: SessionForkTreeNode
    let depth: Int

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .top, spacing: 8) {
                Text(String(repeating: "  ", count: min(depth, 12)))
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.tertiary)
                VStack(alignment: .leading, spacing: 2) {
                    Text(node.session.goal)
                        .font(.system(
                            size: 11,
                            weight: depth == 0 ? .semibold : .regular,
                            design: .monospaced
                        ))
                        .lineLimit(1)
                    Text(
                        "\(node.session.status) · \(node.commit.map { String($0.prefix(8)) } ?? "no commit") · \(node.checkpointID.map { String($0.prefix(8)) } ?? "no checkpoint")"
                    )
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
                }
                Spacer()
                if node.session.id != model.selectedSessionID {
                    Button("Open") {
                        model.selectSession(node.session.id)
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                }
            }
            ForEach(node.children) { child in
                ForkTreeRowView(node: child, depth: depth + 1)
            }
        }
    }
}

struct CompareSessionsSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Compare sessions")
                        .font(.system(size: 18, weight: .semibold, design: .rounded))
                    Text("Committed branch difference")
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button("Done") { dismiss() }
                    .keyboardShortcut(.defaultAction)
            }
            .padding(18)
            Divider().opacity(0.35)
            if model.compareBusy {
                ProgressView()
                    .controlSize(.small)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let error = model.compareError {
                StudioEmptyView(
                    icon: "exclamationmark.triangle",
                    title: "Comparison unavailable",
                    detail: error
                )
            } else if let result = model.compareResult {
                VStack(alignment: .leading, spacing: 10) {
                    HStack(spacing: 16) {
                        Text(
                            "\(String(result.leftCommit.prefix(8))) → \(String(result.rightCommit.prefix(8)))"
                        )
                        .font(.system(size: 10, design: .monospaced))
                        Text("\(result.files.count) files")
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundStyle(.secondary)
                        if result.truncated {
                            Text("patch truncated")
                                .font(.system(size: 9, design: .monospaced))
                                .foregroundStyle(.orange)
                        }
                    }
                    ScrollView(.horizontal) {
                        HStack(spacing: 12) {
                            ForEach(result.files) { file in
                                Text(
                                    "\(file.path) +\(file.binary ? "bin" : "\(file.additions)") / -\(file.binary ? "bin" : "\(file.deletions)")"
                                )
                                .font(.system(size: 9, design: .monospaced))
                                .foregroundStyle(file.binary ? Color.orange : Color.secondary)
                            }
                        }
                    }
                    .frame(maxHeight: 40)
                    Divider().opacity(0.3)
                    if result.patch.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                        StudioEmptyView(
                            icon: "equal.circle",
                            title: "No committed difference",
                            detail: "The session branches point at the same tree."
                        )
                    } else {
                        ScrollView([.vertical, .horizontal]) {
                            Text(result.patch)
                                .font(.system(size: 10, design: .monospaced))
                                .textSelection(.enabled)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(12)
                        }
                        .background(.black.opacity(0.18), in: RoundedRectangle(cornerRadius: 10))
                    }
                }
                .padding(18)
            } else {
                ProgressView()
                    .controlSize(.small)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .frame(width: 820, height: 680)
    }
}
