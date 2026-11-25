import SwiftUI

struct MemoryRow: View {
    @Environment(AppModel.self) private var model
    let memory: MemoryItem
    @State private var confirmDelete = false

    private var statusColor: Color {
        switch memory.displayStatus {
        case "active": memory.isPinned ? VPTheme.accent : VPTheme.success
        case "stale": VPTheme.warning
        default: VPTheme.textTertiary
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                Text(memory.kind.uppercased())
                    .font(.system(size: 8, weight: .medium, design: .monospaced))
                    .foregroundStyle(.cyan)
                    .padding(.horizontal, 5)
                    .padding(.vertical, 1)
                    .background(.cyan.opacity(0.1), in: Capsule())
                Text(memory.title)
                    .font(.system(size: 9, weight: .medium))
                Spacer()
                Text(memory.displayStatus.uppercased())
                    .font(.system(size: 7.5, weight: .semibold, design: .monospaced))
                    .foregroundStyle(statusColor)
                    .padding(.horizontal, 5)
                    .padding(.vertical, 1)
                    .background(statusColor.opacity(0.1), in: Capsule())
                if model.memoryActionBusyID == memory.id {
                    ProgressView().controlSize(.mini)
                } else {
                    Button {
                        Task { await model.toggleMemoryPin(memory) }
                    } label: {
                        Image(systemName: memory.isPinned ? "pin.fill" : "pin")
                            .font(.system(size: 8))
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(memory.isPinned ? VPTheme.accent : .secondary)
                    .help(memory.isPinned ? "Unpin memory" : "Pin memory")
                    .disabled(model.memoryBusy)
                    if !memory.evidenceLabelParts.isEmpty {
                        Button {
                            Task { await model.verifyMemory(memory) }
                        } label: {
                            Image(systemName: "checkmark.seal")
                                .font(.system(size: 8))
                        }
                        .buttonStyle(.plain)
                        .foregroundStyle(.secondary)
                        .help("Verify evidence references")
                        .disabled(model.memoryBusy)
                    }
                }
                Button {
                    confirmDelete = true
                } label: {
                    Image(systemName: "trash")
                        .font(.system(size: 8))
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .help("Delete memory")
                .disabled(model.memoryBusy)
            }
            HStack(spacing: 8) {
                Text(memory.displaySource)
                Text(memory.agentName)
                if memory.evidenceCount > 0 {
                    Text("\(memory.evidenceCount) evidence")
                }
                if let useCount = memory.useCount, useCount > 0 {
                    Text("used \(useCount)×")
                }
                if let verified = memory.lastVerifiedAt {
                    Text("verified \(verified.prefix(10))")
                }
            }
            .font(.system(size: 7.5, design: .monospaced))
            .foregroundStyle(.tertiary)
            Text(memory.body)
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.secondary)
                .lineLimit(3)
            if !memory.evidenceLabelParts.isEmpty {
                Text(memory.evidenceLabelParts.joined(separator: " · "))
                    .font(.system(size: 7.5, design: .monospaced))
                    .foregroundStyle(VPTheme.accentDeep)
                    .lineLimit(2)
            }
            if let detail = memory.statusDetail, !detail.isEmpty, memory.displayStatus != "active" {
                Text(detail)
                    .font(.system(size: 7.5, design: .monospaced))
                    .foregroundStyle(statusColor)
                    .lineLimit(2)
            }
            if !memory.tags.isEmpty {
                Text(memory.tags.map { "#\($0)" }.joined(separator: " "))
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 8))
        .confirmationDialog(
            "Delete this memory?",
            isPresented: $confirmDelete,
            titleVisibility: .visible
        ) {
            Button("Delete", role: .destructive) {
                Task { await model.deleteMemory(memory) }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(memory.title)
        }
    }
}

struct MemoryPolicyCard: View {
    @Environment(AppModel.self) private var model
    let project: Project
    @State private var showAdvanced = false
    @State private var showEmbedding = false

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Memory retention")
                    Text(project.name)
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.memoryPolicyBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Save") {
                    Task { await model.saveMemoryPolicy() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.memoryPolicyBusy)
            }
            Toggle("Automatic prune", isOn: $model.memoryPolicyEnabled)
                .toggleStyle(.switch)
                .controlSize(.small)
            HStack(spacing: 8) {
                TextField("max items", text: $model.memoryPolicyMaxItems)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 9, design: .monospaced))
                    .frame(width: 110)
                TextField("max age days", text: $model.memoryPolicyMaxAgeDays)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 9, design: .monospaced))
                    .frame(width: 110)
                Text("leave blank to disable a limit")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
            Text(
                "Off by default. The daemon sweeps enabled policies periodically (default hourly) and emits event-backed deletions. Pinned memories are never pruned. At least one limit is required to enable."
            )
            .font(.system(size: 9, design: .monospaced))
            .foregroundStyle(.secondary)
            DisclosureGroup("Verification & retrieval", isExpanded: $showAdvanced) {
                VStack(alignment: .leading, spacing: 8) {
                    HStack(spacing: 8) {
                        TextField(
                            "task-result verify days",
                            text: $model.memoryPolicyTaskResultVerifyDays
                        )
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 9, design: .monospaced))
                        .frame(width: 150)
                        TextField(
                            "default verify days",
                            text: $model.memoryPolicyDefaultVerifyDays
                        )
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 9, design: .monospaced))
                        .frame(width: 150)
                        TextField("brief chars", text: $model.memoryPolicyBriefMaxChars)
                            .textFieldStyle(.roundedBorder)
                            .font(.system(size: 9, design: .monospaced))
                            .frame(width: 100)
                    }
                    Toggle("Supersede a same-title active memory", isOn: $model.memoryPolicyAutoSupersede)
                        .toggleStyle(.switch)
                        .controlSize(.small)
                    Toggle("Hourly stale sweep (opt-in)", isOn: $model.memoryPolicySweepStale)
                        .toggleStyle(.switch)
                        .controlSize(.small)
                    Text(
                        "Stale is a warning, not a fact check: verification only checks commit ancestry, path existence, and cheap record ids. URL evidence is reported unchecked. Sweeping marks expired active memories stale and never deletes them."
                    )
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
                }
                .padding(.top, 6)
            }
            .font(.system(size: 9, design: .monospaced))
            DisclosureGroup("Semantic retrieval (optional)", isExpanded: $showEmbedding) {
                VStack(alignment: .leading, spacing: 8) {
                    Toggle("Enable hybrid recall", isOn: $model.memoryEmbeddingEnabled)
                        .toggleStyle(.switch)
                        .controlSize(.small)
                    HStack(spacing: 8) {
                        TextField("embedding endpoint", text: $model.memoryEmbeddingEndpoint)
                            .textFieldStyle(.roundedBorder)
                            .font(.system(size: 9, design: .monospaced))
                        TextField("model", text: $model.memoryEmbeddingModel)
                            .textFieldStyle(.roundedBorder)
                            .font(.system(size: 9, design: .monospaced))
                            .frame(width: 150)
                    }
                    HStack(spacing: 8) {
                        TextField("dimensions", text: $model.memoryEmbeddingDimensions)
                            .textFieldStyle(.roundedBorder)
                            .font(.system(size: 9, design: .monospaced))
                            .frame(width: 86)
                        TextField("batch", text: $model.memoryEmbeddingBatchSize)
                            .textFieldStyle(.roundedBorder)
                            .font(.system(size: 9, design: .monospaced))
                            .frame(width: 60)
                        TextField("timeout s", text: $model.memoryEmbeddingTimeoutSecs)
                            .textFieldStyle(.roundedBorder)
                            .font(.system(size: 9, design: .monospaced))
                            .frame(width: 72)
                        TextField("candidates", text: $model.memoryEmbeddingMaxCandidates)
                            .textFieldStyle(.roundedBorder)
                            .font(.system(size: 9, design: .monospaced))
                            .frame(width: 74)
                        TextField("min similarity", text: $model.memoryEmbeddingMinSimilarity)
                            .textFieldStyle(.roundedBorder)
                            .font(.system(size: 9, design: .monospaced))
                            .frame(width: 92)
                    }
                    TextField(
                        "API key environment variable (name only)",
                        text: $model.memoryEmbeddingAPIKeyEnv
                    )
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 9, design: .monospaced))
                    Picker("Retrieval index", selection: $model.memoryEmbeddingIndex) {
                        Text("exact scan").tag(MemoryIndexMode.scan)
                        Text("IVF (opt-in)").tag(MemoryIndexMode.ivf)
                    }
                    .pickerStyle(.segmented)
                    .controlSize(.small)
                    .frame(width: 260)
                    if model.memoryEmbeddingIndex == .ivf {
                        HStack(spacing: 8) {
                            TextField("nlist", text: $model.memoryEmbeddingIVFNlist)
                                .textFieldStyle(.roundedBorder)
                                .font(.system(size: 9, design: .monospaced))
                                .frame(width: 64)
                            TextField("nprobe", text: $model.memoryEmbeddingIVFNprobe)
                                .textFieldStyle(.roundedBorder)
                                .font(.system(size: 9, design: .monospaced))
                                .frame(width: 64)
                            TextField(
                                "min vectors",
                                text: $model.memoryEmbeddingIVFMinVectors
                            )
                            .textFieldStyle(.roundedBorder)
                            .font(.system(size: 9, design: .monospaced))
                            .frame(width: 86)
                            TextField(
                                "rebuild growth",
                                text: $model.memoryEmbeddingIVFRebuildVectors
                            )
                            .textFieldStyle(.roundedBorder)
                            .font(.system(size: 9, design: .monospaced))
                            .frame(width: 100)
                        }
                    }
                    HStack(spacing: 8) {
                        Button("Test endpoint") {
                            Task { await model.testMemoryEmbeddings() }
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                        .disabled(model.memoryEmbeddingBusy || model.memoryPolicyBusy)
                        Button("Rebuild index") {
                            Task { await model.rebuildMemoryEmbeddings() }
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                        .disabled(model.memoryEmbeddingBusy || model.memoryPolicyBusy)
                        Button("Rebuild ANN index") {
                            Task { await model.rebuildMemoryEmbeddingIndex() }
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                        .disabled(!model.canRebuildMemoryEmbeddingIndex)
                        if model.memoryEmbeddingBusy {
                            ProgressView().controlSize(.small)
                        }
                    }
                    Text(
                        "Off by default. When enabled, memory titles, bodies, and tags are sent to the configured endpoint. The daemon never selects a model, stores a key, or returns vectors. Disabled mode makes zero embedding calls and recall stays lexical. Save before testing; a failed index never fails a memory write."
                    )
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    if let embedding = model.memoryEmbeddingsStatus {
                        HStack(spacing: 10) {
                            Text(verbatim: "status \(embedding.healthLabel)")
                            Text(verbatim: "coverage \(embedding.coverageLabel)")
                            Text(verbatim: "\(embedding.indexed)/\(embedding.indexable) indexed")
                            if embedding.pending > 0 {
                                Text(verbatim: "\(embedding.pending) pending")
                            }
                            if embedding.failed > 0 {
                                Text(verbatim: "\(embedding.failed) failed")
                                    .foregroundStyle(VPTheme.warning)
                            }
                            if embedding.invalid > 0 {
                                Text(verbatim: "\(embedding.invalid) invalid vectors")
                                    .foregroundStyle(VPTheme.warning)
                            }
                            if embedding.usage.calls > 0 {
                                Text(verbatim: "\(embedding.usage.calls) calls")
                            }
                        }
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.secondary)
                        if let error = embedding.lastError {
                            Text(error)
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(VPTheme.warning)
                                .lineLimit(3)
                        }
                    }
                    if let index = model.memoryEmbeddingIndexStatus {
                        HStack(spacing: 10) {
                            Text(verbatim: "index \(index.modeLabel) · \(index.statusLabel)")
                            Text(verbatim: index.generationLabel)
                            Text(verbatim: "coverage \(index.coverageLabel)")
                            Text(verbatim: "\(index.assignedVectors)/\(index.vectorCount) assigned")
                            if index.clusterCount > 0 {
                                Text(verbatim: "\(index.clusterCount) clusters")
                            }
                            if index.invalidAssignments > 0 {
                                Text(verbatim: "\(index.invalidAssignments) stale assignments")
                                    .foregroundStyle(VPTheme.warning)
                            }
                            Text(verbatim: "nprobe \(index.nprobe)")
                            if index.fallbackCount > 0 {
                                Text(verbatim: "\(index.fallbackCount) fallbacks")
                                    .foregroundStyle(VPTheme.warning)
                            }
                        }
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.secondary)
                        if let reason = index.fallbackReason {
                            Text(reason)
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(VPTheme.warning)
                                .lineLimit(2)
                        }
                        if let error = index.lastError {
                            Text(error)
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(VPTheme.warning)
                                .lineLimit(2)
                        }
                    }
                    if let result = model.memoryEmbeddingIndexRebuildResult {
                        Text(result.summaryLabel)
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(result.rebuilt ? VPTheme.success : .secondary)
                            .lineLimit(2)
                    }
                    if let result = model.memoryEmbeddingTestResult {
                        Text(
                            verbatim: result.ok
                                ? "probe ok · \(result.model ?? "model") · \(result.dimensions)d · \(result.latencyMs)ms"
                                : "probe failed · \(result.error ?? "unknown error")"
                        )
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(result.ok ? VPTheme.success : VPTheme.warning)
                        .lineLimit(3)
                    }
                    if let message = model.memoryEmbeddingStatusMessage {
                        Text(message)
                            .font(.system(size: 8, design: .monospaced))
                            .foregroundStyle(.tertiary)
                    }
                }
                .padding(.top, 6)
            }
            .font(.system(size: 9, design: .monospaced))
            if let status = model.memoryPolicyStatus {
                Text(status)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
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
        .task {
            await model.loadMemoryPolicy()
        }
    }
}

struct ProjectRunPolicyCard: View {
    @Environment(AppModel.self) private var model
    let project: Project

    @State private var showingEditor = false

    private var policy: ProjectRunPolicyView? {
        guard model.projectRunPolicyProjectID == project.id else { return nil }
        return model.projectRunPolicy
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            header
            if let policy {
                metrics(policy)
                Text(policy.autoRescueDetail)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(policy.autoRescue ? VPTheme.warning : .secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Text(policy.autoReallocateDetail)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(policy.autoReallocateEnvelopes ? VPTheme.warning : .secondary)
                    .fixedSize(horizontal: false, vertical: true)
                if let error = model.projectRunPolicyError {
                    Label(error.displayText, systemImage: "exclamationmark.triangle.fill")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(VPTheme.warning)
                        .fixedSize(horizontal: false, vertical: true)
                }
                if let result = model.projectRunPolicyResult,
                   result.projectID == project.id,
                   !result.isExplicit
                {
                    Text(
                        "No explicit project policy is stored; these are built-in defaults. Editing and saving creates one."
                    )
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(VPTheme.textTertiary)
                    .fixedSize(horizontal: false, vertical: true)
                }
                Text(
                    "Defaults only. This policy cannot widen approvals, sandbox, task contracts or write scope, budgets/capacity, Verifier, Integrator, or model routing. It does not change existing session budgets or start/resume a run."
                )
                .font(.system(size: 9.5, design: .monospaced))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            } else if let error = model.projectRunPolicyError {
                Label(error.displayText, systemImage: "exclamationmark.triangle.fill")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(VPTheme.warning)
                    .fixedSize(horizontal: false, vertical: true)
            } else if model.projectRunPolicyBusy {
                Text("Loading project RunPolicy defaults…")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            } else {
                Text("RunPolicy defaults unavailable. No defaults were invented.")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
            footer
        }
        .padding(14)
        .background(
            .white.opacity(0.035),
            in: RoundedRectangle(cornerRadius: 14, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.003, lift: 1)
        .vpAppear(index: 0)
        .task(id: project.id) {
            await model.loadProjectRunPolicy()
        }
        .sheet(isPresented: $showingEditor) {
            ProjectRunPolicySheet(project: project)
                .environment(model)
        }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 9) {
            Image(systemName: "dial.medium")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(VPTheme.accent)
                .frame(width: 18)
            VStack(alignment: .leading, spacing: 2) {
                SectionLabel(title: "Project RunPolicy")
                Text(project.name)
                    .font(.system(size: 13, weight: .semibold, design: .rounded))
                    .lineLimit(1)
            }
            Spacer(minLength: 4)
            if model.projectRunPolicyBusy {
                ProgressView().controlSize(.mini)
            }
            if let result = model.projectRunPolicyResult,
               result.projectID == project.id
            {
                Text(result.sourceLabel.uppercased())
                    .font(.system(size: 7, weight: .semibold, design: .monospaced))
                    .tracking(0.3)
                    .foregroundStyle(result.isExplicit ? VPTheme.success : VPTheme.textTertiary)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(
                        (result.isExplicit ? VPTheme.success : VPTheme.textTertiary)
                            .opacity(0.10),
                        in: Capsule()
                    )
            }
            if policy?.autoRescue == true {
                Text("AUTO RESCUE ON")
                    .font(.system(size: 7, weight: .semibold, design: .monospaced))
                    .tracking(0.3)
                    .foregroundStyle(VPTheme.warning)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(VPTheme.warning.opacity(0.10), in: Capsule())
                    .help(policy?.autoRescueDetail ?? "")
            }
            if policy?.autoReallocateEnvelopes == true {
                Text("AUTO ENVELOPES ON")
                    .font(.system(size: 7, weight: .semibold, design: .monospaced))
                    .tracking(0.3)
                    .foregroundStyle(VPTheme.warning)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(VPTheme.warning.opacity(0.10), in: Capsule())
                    .help(policy?.autoReallocateDetail ?? "")
            }
            if policy?.autoPersistEnvelopes == true {
                Text("AUTO PERSIST ON")
                    .font(.system(size: 7, weight: .semibold, design: .monospaced))
                    .tracking(0.3)
                    .foregroundStyle(VPTheme.warning)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(VPTheme.warning.opacity(0.10), in: Capsule())
                    .help(policy?.autoPersistDetail ?? "")
            }
        }
    }

    private func metrics(_ policy: ProjectRunPolicyView) -> some View {
        HStack(alignment: .top, spacing: 18) {
            policyMetric("Profile", policy.profileTitle)
            policyMetric("Reserve", policy.reservePercentLabel)
            policyMetric("Attention cap", policy.attentionLabel)
            policyMetric("No progress", policy.noProgressLabel)
            policyMetric("Auto rescue", policy.autoRescueLabel)
            policyMetric("Auto envelopes", policy.autoReallocateLabel)
            policyMetric("Auto persist", policy.autoPersistLabel)
        }
    }

    private func policyMetric(_ title: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title.uppercased())
                .font(.system(size: 7, weight: .semibold, design: .monospaced))
                .foregroundStyle(.tertiary)
            Text(value)
                .font(.system(size: 10, weight: .medium, design: .monospaced))
                .foregroundStyle(.primary)
                .lineLimit(1)
        }
        .frame(minWidth: 84, alignment: .leading)
    }

    private var footer: some View {
        HStack(spacing: 8) {
            if let result = model.projectRunPolicyResult,
               result.projectID == project.id,
               let updated = result.updatedLabel
            {
                Text(updated)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
            } else if let policy {
                Text(policy.stopPolicySummary)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
            }
            Spacer()
            Button("Refresh") {
                Task { await model.reloadProjectRunPolicy() }
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(model.projectRunPolicyBusy)
            Button("Edit defaults") {
                model.beginProjectRunPolicyEdit()
                showingEditor = true
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.small)
            .disabled(!model.canEditProjectRunPolicy || policy == nil)
        }
    }
}

struct ProjectRunPolicySheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let project: Project

    private var projectMatches: Bool {
        model.selectedProjectID == project.id
    }

    private var canSave: Bool {
        projectMatches
            && model.projectRunPolicyProjectID == project.id
            && model.projectRunPolicy != nil
            && !model.projectRunPolicyBusy
            && model.projectRunPolicyDraft.validationIssue == nil
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            VPHairline()
            ScrollView {
                VStack(alignment: .leading, spacing: 15) {
                    if !projectMatches {
                        Label(
                            "The selected project changed. Close this sheet and reopen it from the active project.",
                            systemImage: "arrow.triangle.2.circlepath"
                        )
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(VPTheme.warning)
                        .fixedSize(horizontal: false, vertical: true)
                    }
                    sourceLine
                    fields
                    stopPolicy
                    autoRescuePolicy
                    autoEnvelopePolicy
                    validationLine
                    feedback
                    note
                }
                .padding(20)
            }
            VPHairline()
            footer
        }
        .frame(width: 560)
        .frame(maxHeight: 680)
        .background { VPCrystalBackdrop() }
        .tint(VPTheme.accent)
        .onChange(of: model.projectRunPolicyDraft) { _, _ in
            model.clearProjectRunPolicyFeedback()
        }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 12) {
            ZStack {
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .fill(VPTheme.accentWash)
                    .frame(width: 40, height: 40)
                Image(systemName: "dial.medium")
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(VPTheme.accent)
            }
            VStack(alignment: .leading, spacing: 3) {
                Text("PROJECT RUNPOLICY")
                    .font(VPTheme.mono(7.5, weight: .semibold))
                    .tracking(1.2)
                    .foregroundStyle(VPTheme.accent)
                Text(project.name)
                    .font(VPTheme.font(18, weight: .semibold, rounded: true))
                    .foregroundStyle(VPTheme.textPrimary)
                    .lineLimit(2)
                Text("Default Run Contract values · explicit save only")
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            Spacer()
            Button("Cancel") { dismiss() }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .keyboardShortcut(.cancelAction)
        }
        .padding(20)
        .background(VPTheme.rail)
    }

    @ViewBuilder
    private var sourceLine: some View {
        if let result = model.projectRunPolicyResult, result.projectID == project.id {
            HStack(spacing: 8) {
                Text(result.sourceLabel.uppercased())
                    .font(VPTheme.mono(7, weight: .semibold))
                    .foregroundStyle(result.isExplicit ? VPTheme.success : VPTheme.textTertiary)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(
                        (result.isExplicit ? VPTheme.success : VPTheme.textTertiary)
                            .opacity(0.10),
                        in: Capsule()
                    )
                if let updated = result.updatedLabel {
                    Text(updated)
                        .font(VPTheme.mono(7.5))
                        .foregroundStyle(VPTheme.textTertiary)
                        .lineLimit(1)
                } else {
                    Text("Saving creates an explicit project policy; nothing is stored until you save.")
                        .font(VPTheme.mono(7.5))
                        .foregroundStyle(VPTheme.textTertiary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 4)
            }
        }
    }

    private var fields: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .top, spacing: 12) {
                VStack(alignment: .leading, spacing: 4) {
                    fieldTitle("Default profile")
                    Picker("Default profile", selection: valueBinding(\.profile)) {
                        ForEach(RunProfile.allCases) { profile in
                            Text(profile.title).tag(profile)
                        }
                    }
                    .labelsHidden()
                    .pickerStyle(.menu)
                    .frame(width: 150)
                }
                textField(
                    "Completion reserve",
                    text: valueBinding(\.completionReservePercentText),
                    suffix: "%",
                    help: "0–50%"
                )
                textField(
                    "Attention cap",
                    text: valueBinding(\.attentionCapText),
                    suffix: "intr.",
                    help: "1–1000"
                )
                textField(
                    "No-progress limit",
                    text: valueBinding(\.noProgressTurnsText),
                    suffix: "turns",
                    help: "0–1000"
                )
            }
            Text(
                "Completion reserve is held for verification, integration, and landing. Attention cap counts weighted operator interrupts. No-progress is the evidence-free turn window in the stop policy."
            )
            .font(VPTheme.mono(7.5))
            .foregroundStyle(VPTheme.textTertiary)
            .fixedSize(horizontal: false, vertical: true)
        }
        .disabled(!projectMatches || model.projectRunPolicyBusy)
    }

    private var stopPolicy: some View {
        VStack(alignment: .leading, spacing: 8) {
            Toggle(
                "Require acceptance evidence before stopping",
                isOn: valueBinding(\.requireEvidence)
            )
            .toggleStyle(.switch)
            .controlSize(.small)
            Toggle(
                "Allow explicit partial delivery",
                isOn: valueBinding(\.partialDelivery)
            )
            .toggleStyle(.switch)
            .controlSize(.small)
            Text(
                "The marginal-value ratio \(String(format: "%.2f", model.projectRunPolicyDraft.marginalValueRatio)) is preserved from the loaded policy; this sheet does not edit it."
            )
            .font(VPTheme.mono(7))
            .foregroundStyle(VPTheme.textTertiary)
            .fixedSize(horizontal: false, vertical: true)
        }
        .disabled(!projectMatches || model.projectRunPolicyBusy)
    }

    private var autoRescuePolicy: some View {
        VStack(alignment: .leading, spacing: 8) {
            Toggle(
                "Allow one automatic completion-reserve rescue per session",
                isOn: valueBinding(\.autoRescue)
            )
            .toggleStyle(.switch)
            .controlSize(.small)
            Text(
                "Explicit project opt-in. The daemon grants at most one automatic completion-reserve rescue per session, only for eligible fixed-budget near-completion work. It never adds budget, removes caps, changes model routing, or bypasses Verifier/Integrator; approvals, sandbox, and task contracts still apply. Off by default."
            )
            .font(VPTheme.mono(7))
            .foregroundStyle(VPTheme.textTertiary)
            .fixedSize(horizontal: false, vertical: true)
        }
        .disabled(!projectMatches || model.projectRunPolicyBusy)
    }

    private var autoEnvelopePolicy: some View {
        VStack(alignment: .leading, spacing: 8) {
            Toggle(
                "Reuse unused completed-task capacity at runtime",
                isOn: valueBinding(\.autoReallocateEnvelopes)
            )
            .toggleStyle(.switch)
            .controlSize(.small)
            Text(
                "Explicit project opt-in. Worker/Labour runtime task budgets may reuse unused token/cost capacity from completed tasks in the same department. The stored TeamPlan and department/session/project caps are unchanged; approvals, Verifier, and Integrator still apply, and a plan revision invalidates an old runtime override. Off by default."
            )
            .font(VPTheme.mono(7))
            .foregroundStyle(VPTheme.textTertiary)
            .fixedSize(horizontal: false, vertical: true)
            Toggle(
                "Persist recorded runtime envelopes before the next plan run",
                isOn: valueBinding(\.autoPersistEnvelopes)
            )
            .toggleStyle(.switch)
            .controlSize(.small)
            Text(
                "Explicit project opt-in. Still-valid recorded runtime envelopes are written into one plan revision before the next run. This records budgets the runtime already allowed; it does not create capacity, change a task status, or bypass approvals, Verifier, or Integrator. A failed persist never blocks the run. Off by default."
            )
            .font(VPTheme.mono(7))
            .foregroundStyle(VPTheme.textTertiary)
            .fixedSize(horizontal: false, vertical: true)
        }
        .disabled(!projectMatches || model.projectRunPolicyBusy)
    }

    private func valueBinding<Value>(
        _ keyPath: WritableKeyPath<ProjectRunPolicyDraft, Value>
    ) -> Binding<Value> {
        Binding(
            get: { model.projectRunPolicyDraft[keyPath: keyPath] },
            set: { model.projectRunPolicyDraft[keyPath: keyPath] = $0 }
        )
    }

    private func textField(
        _ title: String,
        text: Binding<String>,
        suffix: String,
        help: String
    ) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            fieldTitle(title)
            HStack(spacing: 4) {
                TextField(help, text: text)
                    .textFieldStyle(.plain)
                    .font(VPTheme.mono(10))
                    .vpFieldSurface(radius: 8)
                    .frame(width: 76)
                    .accessibilityLabel(title)
                Text(suffix)
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            Text(help)
                .font(VPTheme.mono(6.5))
                .foregroundStyle(VPTheme.textTertiary)
        }
    }

    private func fieldTitle(_ title: String) -> some View {
        Text(title.uppercased())
            .font(VPTheme.mono(6.5, weight: .semibold))
            .tracking(0.4)
            .foregroundStyle(VPTheme.textTertiary)
    }

    @ViewBuilder
    private var validationLine: some View {
        if !projectMatches {
            EmptyView()
        } else if let issue = model.projectRunPolicyDraft.validationIssue {
            Label(issue.displayText, systemImage: "exclamationmark.triangle.fill")
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.warning)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    @ViewBuilder
    private var feedback: some View {
        if let error = model.projectRunPolicyError {
            Label(error.displayText, systemImage: "exclamationmark.triangle.fill")
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.warning)
                .fixedSize(horizontal: false, vertical: true)
                .padding(9)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(VPTheme.warning.opacity(0.08), in: RoundedRectangle(cornerRadius: 9))
        } else if let status = model.projectRunPolicyStatus {
            Text(status)
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
                .padding(9)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(VPTheme.surfaceSunken.opacity(0.5), in: RoundedRectangle(cornerRadius: 9))
        }
    }

    private var note: some View {
        Text(
            "These values are project defaults only. They cannot widen approvals, sandbox, write scope or task contracts, budgets/capacity, Verifier, Integrator, or model routing. Saving does not top up or resume any session."
        )
        .font(VPTheme.mono(7.5))
        .foregroundStyle(VPTheme.textTertiary)
        .fixedSize(horizontal: false, vertical: true)
    }

    private var footer: some View {
        HStack(spacing: 8) {
            if model.projectRunPolicyBusy {
                ProgressView().controlSize(.small)
            }
            Text("No auto-save · daemon validates the policy")
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textTertiary)
            Spacer()
            Button("Cancel") { dismiss() }
                .keyboardShortcut(.cancelAction)
            Button {
                Task {
                    if await model.saveProjectRunPolicy() {
                        dismiss()
                    }
                }
            } label: {
                Label("Save defaults", systemImage: "checkmark.circle")
            }
            .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accentDeep))
            .keyboardShortcut(.defaultAction)
            .disabled(!canSave)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 13)
        .background(VPTheme.rail)
    }
}

struct ShellPolicyCard: View {
    @Environment(AppModel.self) private var model
    let project: Project

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Shell policy")
                    Text(project.name)
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.shellPolicyBusy {
                    ProgressView().controlSize(.small)
                }
                Picker("", selection: $model.shellPolicyNetwork) {
                    Text("Allow network").tag("allow")
                    Text("Deny network").tag("deny")
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(width: 210)
            }

            HStack(spacing: 16) {
                Toggle("Restrict home reads", isOn: $model.shellPolicyRestrictHome)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                    .help(
                        "Deny reads under HOME except the worktree, scratch, Git metadata, and known toolchain/cache paths."
                    )
                Toggle("Read jail (all users + volumes)", isOn: $model.shellPolicyReadJail)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                    .help(
                        "Deny reads under the home parent (normally /Users) and /Volumes except the worktree, scratch, Git metadata, toolchains, and read-deny exceptions. System paths remain readable."
                    )
            }

            Text(
                "Writes remain confined to the worktree, Git metadata, and runtime scratch. Network deny blocks outbound sockets. Restrict home reads denies HOME; Read jail also denies sibling user homes and /Volumes. Both allow the worktree, scratch, Git metadata, and known toolchains. Read-deny paths are additive and must be absolute or start with ~/. This is not a full filesystem read sandbox."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)

            TextEditor(text: $model.shellPolicyReadDenyText)
                .font(.system(size: 10, design: .monospaced))
                .scrollContentBackground(.hidden)
                .padding(6)
                .frame(height: 74)
                .background(.black.opacity(0.16), in: RoundedRectangle(cornerRadius: 8))
                .overlay {
                    RoundedRectangle(cornerRadius: 8)
                        .strokeBorder(.white.opacity(0.08), lineWidth: 1)
                }

            HStack(spacing: 8) {
                Text("One read-deny path per line · empty means no extra denials")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
                Spacer()
                Button("Reset") {
                    Task { await model.resetShellPolicy() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.shellPolicyBusy)
                Button("Save policy") {
                    Task { await model.saveShellPolicy() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.shellPolicyBusy)
            }

            if let status = model.shellPolicyStatus {
                Text(status)
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
        .task(id: project.id) {
            await model.loadShellPolicy()
        }
    }
}

struct IntegrationPolicyCard: View {
    @Environment(AppModel.self) private var model
    let project: Project

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Integration policy")
                    Text(project.name)
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.integrationPolicyBusy {
                    ProgressView().controlSize(.small)
                }
                Toggle("Auto-resolve conflicts", isOn: $model.integrationAutoResolve)
                    .toggleStyle(.switch)
                    .controlSize(.small)
            }

            Text(
                "When enabled, a conflict re-enters an internal resolver + Verifier without routine tool prompts. Host-level operations are still refused, and the shell sandbox applies. Attempts and tokens are capped; manual Auto-resolve remains available."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)

            HStack(spacing: 14) {
                HStack(spacing: 6) {
                    Text("Max automatic attempts")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.secondary)
                    Stepper(
                        "\(model.integrationMaxAutoAttempts)",
                        value: $model.integrationMaxAutoAttempts,
                        in: 0 ... 5
                    )
                    .font(.system(size: 9, design: .monospaced))
                }
                HStack(spacing: 6) {
                    Text("Max tokens / resolution")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.secondary)
                    TextField("session budget", text: $model.integrationMaxTokensText)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 120)
                        .font(.system(size: 9, design: .monospaced))
                }
                Spacer()
                Button("Reset") {
                    Task { await model.resetIntegrationPolicy() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.integrationPolicyBusy)
                Button("Save policy") {
                    Task { await model.saveIntegrationPolicy() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.integrationPolicyBusy)
            }

            if let status = model.integrationPolicyStatus {
                Text(status)
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
        .task(id: project.id) {
            await model.loadIntegrationPolicy()
        }
    }
}

struct ToolOutputPolicyCard: View {
    @Environment(AppModel.self) private var model
    let project: Project

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Tool-output policy")
                    Text(project.name)
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.toolOutputPolicyBusy {
                    ProgressView().controlSize(.small)
                }
                Toggle("Spill oversized results", isOn: $model.toolOutputEnabled)
                    .toggleStyle(.switch)
                    .controlSize(.small)
            }

            Text(
                "Full results are stored under the daemon data directory and replaced with a bounded preview. Handles are opaque and resolve only inside the originating session. Active sessions are protected from prune."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)

            HStack(spacing: 10) {
                policyField("Spill > bytes", text: $model.toolOutputSpillThresholdText)
                policyField("Preview bytes", text: $model.toolOutputPreviewBytesText)
                policyField("Turn budget", text: $model.toolOutputMaxTurnBytesText)
                policyField("Retention secs", text: $model.toolOutputRetentionSecsText)
                policyField("Max entries", text: $model.toolOutputMaxEntriesText)
                Spacer(minLength: 0)
            }

            HStack(spacing: 10) {
                Spacer()
                Button("Audit now") {
                    Task { await model.auditToolOutputs() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.toolOutputPolicyBusy)
                Button("Reset") {
                    Task { await model.resetToolOutputPolicy() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.toolOutputPolicyBusy)
                Button("Save policy") {
                    Task { await model.saveToolOutputPolicy() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.toolOutputPolicyBusy)
            }

            if let status = model.toolOutputPolicyStatus {
                Text(status)
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
        .task(id: project.id) {
            await model.loadToolOutputPolicy()
        }
    }

    private func policyField(_ title: String, text: Binding<String>) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title)
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
            TextField("required", text: text)
                .textFieldStyle(.roundedBorder)
                .font(.system(size: 9, design: .monospaced))
                .frame(width: 82)
        }
    }
}

struct ArtifactPolicyCard: View {
    @Environment(AppModel.self) private var model
    let project: Project

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Artifact policy")
                    Text(project.name)
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.artifactPolicyBusy {
                    ProgressView().controlSize(.small)
                }
                Toggle("Auto GC", isOn: $model.artifactAutoGc)
                    .toggleStyle(.switch)
                    .controlSize(.small)
            }

            Text(
                "Retention uses the oldest artifacts first. Binary artifacts are copied into the data directory; text artifacts stay inline in SQLite. Limits are optional."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)

            HStack(spacing: 10) {
                limitField("Max bytes", text: $model.artifactMaxBytesText)
                limitField("Max age secs", text: $model.artifactMaxAgeText)
                limitField("Max count", text: $model.artifactMaxCountText)
                Spacer()
                Button("GC now") {
                    Task { await model.gcArtifacts() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.artifactPolicyBusy)
                Button("Reset") {
                    Task { await model.resetArtifactPolicy() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.artifactPolicyBusy)
                Button("Save policy") {
                    Task { await model.saveArtifactPolicy() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.artifactPolicyBusy)
            }

            if let status = model.artifactPolicyStatus {
                Text(status)
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
        .task(id: project.id) {
            await model.loadArtifactPolicy()
        }
    }

    private func limitField(_ title: String, text: Binding<String>) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title)
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
            TextField("unlimited", text: text)
                .textFieldStyle(.roundedBorder)
                .font(.system(size: 9, design: .monospaced))
                .frame(width: 90)
        }
    }
}

struct MailboxPolicyCard: View {
    @Environment(AppModel.self) private var model
    let project: Project

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Mailbox retention")
                    Text(project.name)
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.mailboxPolicyBusy {
                    ProgressView().controlSize(.small)
                }
                Toggle("Auto GC", isOn: $model.mailboxAutoGc)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                Toggle("Delete unread", isOn: $model.mailboxDeleteUnread)
                    .toggleStyle(.switch)
                    .controlSize(.small)
            }

            Text(
                "Mailbox GC removes oldest messages first. Auto GC runs after sending or marking a message read. Unread messages are protected unless Delete unread is enabled. Limits are optional."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)

            HStack(spacing: 10) {
                limitField("Max age secs", text: $model.mailboxMaxAgeText)
                limitField("Max count", text: $model.mailboxMaxCountText)
                Spacer()
                Button("GC now") {
                    Task { await model.gcMailbox() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.mailboxPolicyBusy)
                Button("Reset") {
                    Task { await model.resetMailboxPolicy() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.mailboxPolicyBusy)
                Button("Save policy") {
                    Task { await model.saveMailboxPolicy() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.mailboxPolicyBusy)
            }

            if let status = model.mailboxPolicyStatus {
                Text(status)
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
        .task(id: project.id) {
            await model.loadMailboxPolicy()
        }
    }

    private func limitField(_ title: String, text: Binding<String>) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title)
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
            TextField("unlimited", text: text)
                .textFieldStyle(.roundedBorder)
                .font(.system(size: 9, design: .monospaced))
                .frame(width: 90)
        }
    }
}

struct ProjectBudgetCard: View {
    @Environment(AppModel.self) private var model
    let project: Project

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Project shared budget")
                    Text(project.name)
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.projectBudgetBusy {
                    ProgressView().controlSize(.small)
                }
            }
            Text(
                "A project-wide token/USD pool shared by all sessions. Concurrent calls reserve against it atomically. Empty means unlimited."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 3) {
                    Text("Token budget")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    TextField("unlimited", text: $model.projectTokenBudgetText)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 9, design: .monospaced))
                        .frame(width: 110)
                }
                VStack(alignment: .leading, spacing: 3) {
                    Text("USD budget")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    TextField("unlimited", text: $model.projectCostBudgetText)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 9, design: .monospaced))
                        .frame(width: 90)
                }
                Spacer()
                Button("Reset") {
                    Task { await model.resetProjectBudget() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.projectBudgetBusy)
                Button("Save budget") {
                    Task { await model.saveProjectBudget() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.projectBudgetBusy)
            }
            Text(
                "Spent: \(model.projectSpentTokens) tokens · \(String(format: "$%.4f", model.projectSpentCostUsd))"
            )
            .font(.system(size: 9, design: .monospaced))
            .foregroundStyle(.secondary)
            if let status = model.projectBudgetStatus {
                Text(status)
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
        .task(id: project.id) {
            await model.loadProjectBudget()
        }
    }
}

struct DoctorCard: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Diagnostics")
                    Text("Local doctor")
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.doctorBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Run diagnostics") {
                    Task { await model.runDoctor() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.doctorBusy)
            }
            Text(
                "Checks SQLite integrity, data permissions, project roots, worktrees, event mirrors, sandbox availability, and provider configuration."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)
            if let report = model.doctorReport {
                Text(report.ok ? "All checks passed" : "Warnings or errors found")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(report.ok ? Color.green : Color.orange)
                ForEach(report.checks) { check in
                    HStack(alignment: .top, spacing: 8) {
                        Text(check.status.uppercased())
                            .font(.system(size: 8, weight: .medium, design: .monospaced))
                            .foregroundStyle(color(for: check.status))
                            .frame(width: 44, alignment: .leading)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(check.name)
                                .font(.system(size: 9, weight: .medium))
                            Text(check.detail)
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(.secondary)
                                .textSelection(.enabled)
                        }
                    }
                }
            } else if let status = model.doctorStatus {
                Text(status)
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
    }

    private func color(for status: String) -> Color {
        switch status {
        case "ok": .green
        case "warn": .orange
        default: .red
        }
    }
}

struct WorktreeCleanupCard: View {
    @Environment(AppModel.self) private var model
    let project: Project

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    SectionLabel(title: "Worktree cleanup")
                    Text(project.name)
                        .font(.system(size: 13, weight: .semibold, design: .rounded))
                }
                Spacer()
                if model.cleanupBusy {
                    ProgressView().controlSize(.small)
                }
            }
            Text(
                "Terminal-session worktrees only. Dirty worktrees are skipped; branches are kept unless explicitly deleted."
            )
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(.secondary)
            if let stats = model.checkpointStats {
                Text(
                    "\(stats.checkpoints) checkpoints · \(stats.dirty) dirty · \(stats.refs) refs · ~\(ByteCountFormatter.string(fromByteCount: Int64(stats.snapshotBytes), countStyle: .file)) snapshot"
                )
                .font(.system(size: 9, design: .monospaced))
                .foregroundStyle(.secondary)
            }

            Divider().opacity(0.2)
            Text("AUTOMATIC RETENTION")
                .font(.system(size: 8, weight: .semibold, design: .monospaced))
                .foregroundStyle(.tertiary)
            HStack(spacing: 10) {
                Toggle("Enabled", isOn: $model.worktreePolicyEnabled)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                VStack(alignment: .leading, spacing: 3) {
                    Text("Older than days")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    TextField("7", text: $model.worktreePolicyOlderDaysText)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 9, design: .monospaced))
                        .frame(width: 70)
                }
                Toggle("Delete branches", isOn: $model.worktreePolicyDeleteBranches)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                Toggle("Keep failed", isOn: $model.worktreePolicyKeepFailed)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                Spacer()
                if model.worktreePolicyBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Save policy") {
                    Task { await model.saveWorktreePolicy() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.worktreePolicyBusy)
            }
            if let status = model.worktreePolicyStatus {
                Text(status)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            }

            Divider().opacity(0.2)
            Text("MANUAL CLEANUP")
                .font(.system(size: 8, weight: .semibold, design: .monospaced))
                .foregroundStyle(.tertiary)
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 3) {
                    Text("Older than days")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                    TextField("7", text: $model.cleanupOlderDaysText)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 9, design: .monospaced))
                        .frame(width: 70)
                }
                Toggle("Dry run", isOn: $model.cleanupDryRun)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                Toggle("Delete branches", isOn: $model.cleanupDeleteBranches)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                Toggle("Keep failed", isOn: $model.cleanupKeepFailed)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                Spacer()
                Button("Run cleanup") {
                    Task { await model.runWorktreeCleanup() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.cleanupBusy)
            }
            if let status = model.cleanupStatus {
                Text(status)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
            if let result = model.cleanupResult {
                ForEach(result.entries) { entry in
                    HStack(alignment: .top, spacing: 6) {
                        Text(entry.removed ? "removed" : "skipped")
                            .font(.system(size: 8, weight: .medium, design: .monospaced))
                            .foregroundStyle(entry.removed ? Color.green : Color.secondary)
                            .frame(width: 52, alignment: .leading)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(entry.sessionId)
                                .font(.system(size: 8, design: .monospaced))
                            Text("\(entry.branch) · \(entry.reason)")
                                .font(.system(size: 8, design: .monospaced))
                                .foregroundStyle(.secondary)
                                .lineLimit(2)
                        }
                    }
                }
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
    }
}
