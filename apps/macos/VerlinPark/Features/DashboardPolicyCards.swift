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
