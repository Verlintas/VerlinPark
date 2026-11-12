import SwiftUI

/// Test-result cache and impact-map surface.
///
/// The panel is deliberately metadata-only: output bodies stay behind the
/// daemon-owned `output_ref` and are never rendered from this view. Cache hits
/// shown in the transcript remain labeled by the runtime, not inferred here.
struct TestCachePanel: View {
    @Environment(AppModel.self) private var model
    @State private var confirmingClear = false
    @State private var showEntries = true

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                header
                if let stats = model.testCacheStats {
                    counters(stats)
                } else if model.testCacheBusy {
                    loading
                } else {
                    empty
                }
                policyEditor
                impactPreview
                entries
            }
            .padding(14)
        }
        .task {
            if model.testCacheStats == nil {
                await model.loadTestCache()
            }
        }
        .alert("Clear the test cache?", isPresented: $confirmingClear) {
            Button("Cancel", role: .cancel) {}
            Button("Clear metadata", role: .destructive) {
                Task { await model.clearTestCache() }
            }
        } message: {
            Text(
                "This removes cached test/lint/check metadata for this project. "
                    + "Output bodies are owned by tool-output retention and may already be gone."
            )
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                VPKicker(text: "Test cache", trailing: model.testCacheBusy ? "busy" : nil)
                Spacer()
                Button {
                    Task { await model.loadTestCache() }
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                        .font(VPTheme.mono(8, weight: .semibold))
                }
                .buttonStyle(VPPressableButtonStyle(scale: 0.95))
                .disabled(model.testCacheBusy)
            }
            Text(
                "Content-addressed reuse for run_project_task. A hit means no command ran: "
                    + "the command, argv, cwd, environment subset, profile, tool version, inputs, "
                    + "dependency closure, and impact generation all matched."
            )
            .font(VPTheme.font(9.5))
            .foregroundStyle(VPTheme.textSecondary)
            .fixedSize(horizontal: false, vertical: true)
            if let status = model.testCacheStatus {
                Text(status)
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.warning)
            }
        }
    }

    private func counters(_ stats: TestCacheStats) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 10) {
                metric("Entries", "\(stats.entries)", VPTheme.accent)
                metric("Pass", "\(stats.passed)", VPTheme.success)
                metric("Fail", "\(stats.failed)", VPTheme.danger)
                metric("Hits", "\(stats.totalHits)", VPTheme.info)
            }
            HStack(spacing: 10) {
                metric("Expired", "\(stats.expired)", VPTheme.warning)
                metric("Bodies", TestCacheFormat.bytes(stats.outputBytes), VPTheme.textSecondary)
                metric(
                    "Kinds",
                    stats.byKind
                        .sorted { $0.key < $1.key }
                        .map { "\($0.key)=\($0.value)" }
                        .joined(separator: " "),
                    VPTheme.textSecondary
                )
            }
            if let newest = stats.newestCreatedAt {
                Text("newest \(TestCacheFormat.timestamp(newest))")
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
            }
        }
        .padding(10)
        .background(VPTheme.surfaceRaised.opacity(0.4), in: RoundedRectangle(cornerRadius: 10))
    }

    private func metric(_ title: String, _ value: String, _ tint: Color) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title.uppercased())
                .font(VPTheme.mono(6.5, weight: .semibold))
                .tracking(0.5)
                .foregroundStyle(VPTheme.textTertiary)
            Text(value)
                .font(VPTheme.mono(10, weight: .semibold))
                .foregroundStyle(tint)
                .lineLimit(1)
                .minimumScaleFactor(0.7)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func boolBinding(_ keyPath: ReferenceWritableKeyPath<AppModel, Bool>) -> Binding<Bool> {
        Binding(
            get: { model[keyPath: keyPath] },
            set: { model[keyPath: keyPath] = $0 }
        )
    }

    private func stringBinding(_ keyPath: ReferenceWritableKeyPath<AppModel, String>) -> Binding<String> {
        Binding(
            get: { model[keyPath: keyPath] },
            set: { model[keyPath: keyPath] = $0 }
        )
    }

    private var policyEditor: some View {
        VStack(alignment: .leading, spacing: 8) {
            VPKicker(text: "Policy", trailing: nil)
            Toggle("Reuse test/lint/check results", isOn: boolBinding(\.testCacheEnabled))
                .font(VPTheme.font(10))
            Toggle("Reuse lint results", isOn: boolBinding(\.testCacheReuseLint))
                .font(VPTheme.font(10))
                .disabled(!model.testCacheEnabled)
            Toggle(
                "Allow the Integrator command to use the cache",
                isOn: boolBinding(\.testCacheIncludeIntegration)
            )
            .font(VPTheme.font(10))
            .disabled(!model.testCacheEnabled)
            Text(
                "Integrator cache use is off by default; enabling it never bypasses verification or merge gates."
            )
            .font(VPTheme.mono(7.5))
            .foregroundStyle(VPTheme.textTertiary)
            HStack(spacing: 8) {
                policyField("Max entries", text: stringBinding(\.testCacheMaxEntriesText))
                policyField("Retention days", text: stringBinding(\.testCacheRetentionDaysText))
                policyField("Output bytes", text: stringBinding(\.testCacheMaxOutputBytesText))
                policyField("Failed TTL (s)", text: stringBinding(\.testCacheFailedRetentionText))
            }
            HStack(alignment: .top, spacing: 10) {
                cacheScopeEditor(
                    title: "Extra env names · one per line",
                    text: stringBinding(\.testCacheExtraEnvText)
                )
                cacheScopeEditor(
                    title: "Extra worktree paths · one per line",
                    text: stringBinding(\.testCacheExtraPathsText)
                )
            }
            if let preview = model.testCacheScopePreview {
                HStack(spacing: 8) {
                    sourceBadge(
                        "\(preview.extraEnv) env",
                        preview.complete ? VPTheme.accent : VPTheme.warning
                    )
                    sourceBadge(
                        "\(preview.extraPaths) path(s)",
                        preview.complete ? VPTheme.accent : VPTheme.warning
                    )
                    sourceBadge(
                        preview.complete ? "key inputs complete" : "reuse disabled",
                        preview.complete ? VPTheme.success : VPTheme.danger
                    )
                    Spacer()
                }
                ForEach(preview.entries, id: \.self) { entry in
                    HStack(spacing: 6) {
                        Text(entry.kind == "env" ? "ENV" : "FILE")
                            .font(VPTheme.mono(6.5, weight: .semibold))
                            .foregroundStyle(entry.complete ? VPTheme.textTertiary : VPTheme.danger)
                        Text(entry.label)
                            .font(VPTheme.mono(8))
                            .foregroundStyle(entry.complete ? VPTheme.textSecondary : VPTheme.danger)
                            .lineLimit(1)
                        if let note = entry.note {
                            Text(note)
                                .font(VPTheme.mono(7))
                                .foregroundStyle(VPTheme.textTertiary)
                                .lineLimit(1)
                        }
                        Spacer()
                        Text("\(entry.bytes) B")
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                }
            }
            Text(
                "Extra env names and worktree-relative files participate in the key. "
                    + "Values are hashed only; unreadable, escaping, or oversized inputs fail closed."
            )
            .font(VPTheme.mono(7.5))
            .foregroundStyle(VPTheme.textTertiary)
            HStack {
                Button("Save policy") {
                    Task { await model.saveTestCachePolicy() }
                }
                .buttonStyle(VPPressableButtonStyle(scale: 0.95))
                .font(VPTheme.mono(8, weight: .semibold))
                .disabled(model.testCacheBusy)

                Spacer()

                Button("Prune preview") {
                    Task { await model.pruneTestCache(dryRun: true) }
                }
                .buttonStyle(VPPressableButtonStyle(scale: 0.95))
                .font(VPTheme.mono(8))

                Button("Prune expired") {
                    Task { await model.pruneTestCache(dryRun: false) }
                }
                .buttonStyle(VPPressableButtonStyle(scale: 0.95))
                .font(VPTheme.mono(8))

                Button("Clear…", role: .destructive) {
                    confirmingClear = true
                }
                .buttonStyle(VPPressableButtonStyle(scale: 0.95))
                .font(VPTheme.mono(8))
                .disabled(model.testCacheBusy)
            }
        }
        .padding(10)
        .background(VPTheme.surface.opacity(0.35), in: RoundedRectangle(cornerRadius: 10))
    }

    private func cacheScopeEditor(title: String, text: Binding<String>) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title)
                .font(VPTheme.mono(6.5, weight: .semibold))
                .foregroundStyle(VPTheme.textTertiary)
            TextEditor(text: text)
                .font(VPTheme.mono(8.5))
                .scrollContentBackground(.hidden)
                .padding(4)
                .frame(height: 58)
                .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 6))
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func policyField(_ label: String, text: Binding<String>) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(label.uppercased())
                .font(VPTheme.mono(6.5, weight: .semibold))
                .foregroundStyle(VPTheme.textTertiary)
            TextField("", text: text)
                .textFieldStyle(.plain)
                .font(VPTheme.mono(9))
                .padding(.horizontal, 6)
                .padding(.vertical, 4)
                .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 6))
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var impactPreview: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                VPKicker(text: "Impact map", trailing: model.testImpactBusy ? "mapping" : nil)
                Spacer()
                Button {
                    Task { await model.previewTestImpact() }
                } label: {
                    Label("Preview", systemImage: "point.3.connected.trianglepath.dotted")
                        .font(VPTheme.mono(8, weight: .semibold))
                }
                .buttonStyle(VPPressableButtonStyle(scale: 0.95))
                .disabled(model.testImpactBusy || model.selectedSessionID == nil)
            }
            Text(
                "Profile conventions, lexical reverse imports, already-running LSP references, "
                    + "and historical coverage. Mapping never executes tests and never claims completeness."
            )
            .font(VPTheme.mono(7.5))
            .foregroundStyle(VPTheme.textTertiary)
            if let result = model.testImpactResult {
                HStack(spacing: 8) {
                    sourceBadge("LSP", result.map.lspAvailable ? VPTheme.info : VPTheme.textTertiary)
                    sourceBadge(
                        "Lexical-only",
                        result.map.lexicalOnly ? VPTheme.warning : VPTheme.textTertiary
                    )
                    sourceBadge(
                        "\(result.map.targets.count) target(s)",
                        VPTheme.accent
                    )
                    if result.map.truncated {
                        sourceBadge("truncated", VPTheme.warning)
                    }
                    Spacer()
                }
                ForEach(result.map.targets) { target in
                    HStack(spacing: 8) {
                        Text(target.path)
                            .font(VPTheme.mono(8.5, weight: .medium))
                            .foregroundStyle(VPTheme.textPrimary)
                            .lineLimit(1)
                        sourceBadge(target.sourceLabel.uppercased(), tint(for: target.source))
                        Text("\(target.confidencePercent)%")
                            .font(VPTheme.mono(7.5))
                            .foregroundStyle(VPTheme.textTertiary)
                        Spacer()
                        Text(target.reason)
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.textTertiary)
                            .lineLimit(1)
                    }
                }
                ForEach(result.map.notes.prefix(4), id: \.self) { note in
                    Text(note)
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            } else if let status = model.testImpactStatus {
                Text(status)
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.warning)
            }
        }
        .padding(10)
        .background(VPTheme.surfaceRaised.opacity(0.25), in: RoundedRectangle(cornerRadius: 10))
    }

    private var entries: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button {
                withAnimation(.easeInOut(duration: 0.15)) { showEntries.toggle() }
            } label: {
                HStack {
                    VPKicker(text: "Cached results", trailing: "\(model.testCacheEntries.count)")
                    Spacer()
                    Image(systemName: showEntries ? "chevron.up" : "chevron.down")
                        .font(.system(size: 7, weight: .bold))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
            .buttonStyle(.plain)
            if showEntries {
                if model.testCacheEntries.isEmpty {
                    Text("No cached test results for this project yet.")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                } else {
                    ForEach(model.testCacheEntries) { entry in
                        entryRow(entry)
                    }
                }
            }
        }
        .padding(10)
        .background(VPTheme.surface.opacity(0.25), in: RoundedRectangle(cornerRadius: 10))
    }

    private func entryRow(_ entry: TestCacheEntry) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 8) {
                Text(entry.kind.uppercased())
                    .font(VPTheme.mono(7, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                Text(entry.isPass ? "PASS" : entry.status.uppercased())
                    .font(VPTheme.mono(7, weight: .semibold))
                    .foregroundStyle(entry.isPass ? VPTheme.success : VPTheme.danger)
                Text(entry.command)
                    .font(VPTheme.mono(8.5))
                    .foregroundStyle(VPTheme.textPrimary)
                    .lineLimit(1)
                Spacer()
                Text("\(entry.hitCount) hit(s)")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
                Text(TestCacheFormat.duration(entry.durationMs))
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            HStack(spacing: 8) {
                Text("key \(entry.shortCacheKey)")
                Text("inputs \(entry.shortInputsHash)")
                Text("exit \(entry.exitCode)")
                if entry.failureCount > 0 {
                    Text("\(entry.failureCount) failure(s)")
                        .foregroundStyle(VPTheme.danger)
                }
                if entry.outputBytes > 0 {
                    Text(TestCacheFormat.bytes(entry.outputBytes))
                }
                Spacer()
                Text(TestCacheFormat.timestamp(entry.createdAt))
            }
            .font(VPTheme.mono(7))
            .foregroundStyle(VPTheme.textTertiary)
            if !entry.coveredPaths.isEmpty {
                Text(entry.coveredPaths.prefix(4).joined(separator: " · "))
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary.opacity(0.85))
                    .lineLimit(1)
            }
        }
        .padding(7)
        .background(.white.opacity(0.025), in: RoundedRectangle(cornerRadius: 7))
    }

    private func sourceBadge(_ text: String, _ tint: Color) -> some View {
        Text(text)
            .font(VPTheme.mono(6.5, weight: .semibold))
            .tracking(0.4)
            .foregroundStyle(tint)
            .padding(.horizontal, 4)
            .padding(.vertical, 1)
            .background(tint.opacity(0.12), in: Capsule())
    }

    private func tint(for source: String) -> Color {
        switch source {
        case "lsp": VPTheme.info
        case "lexical": VPTheme.accent
        case "history": VPTheme.resident
        default: VPTheme.textSecondary
        }
    }

    private var loading: some View {
        HStack(spacing: 8) {
            ProgressView().controlSize(.mini)
            Text("Loading test-cache metadata…")
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
        }
        .padding(10)
    }

    private var empty: some View {
        Text("No test-cache metadata yet. Run a test, lint, or check task to populate it.")
            .font(VPTheme.mono(8))
            .foregroundStyle(VPTheme.textTertiary)
            .padding(10)
    }
}
