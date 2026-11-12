import SwiftUI

private enum SourceComposerMode: String, CaseIterable, Identifiable {
    case note
    case file
    case url
    case importFiles

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .note: L10n.t("Note")
        case .file: L10n.t("Project file")
        case .url: L10n.t("Web page")
        case .importFiles: L10n.t("Import folder")
        }
    }

    var icon: String {
        switch self {
        case .note: "note.text"
        case .file: "doc.text"
        case .url: "link"
        case .importFiles: "folder.badge.plus"
        }
    }

    var detail: String {
        switch self {
        case .note: L10n.t("Paste a brief, research note, or decision record.")
        case .file: L10n.t("Index one UTF-8 file from the project root.")
        case .url: L10n.t("Fetch a public page and keep a citable text snapshot.")
        case .importFiles: L10n.t("Index text-like files under a project folder.")
        }
    }
}

private enum SourceLibraryPane: String, CaseIterable, Identifiable {
    case library
    case citations

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .library: L10n.t("Library")
        case .citations: L10n.t("Citations")
        }
    }
}

struct SourcesPanel: View {
    @Environment(AppModel.self) private var model
    @State private var composerMode: SourceComposerMode?
    @State private var pendingDelete: ProjectSource?
    @State private var hoveredSourceID: String?
    @State private var pane: SourceLibraryPane = .library
    @State private var editingSource: ProjectSource?
    @State private var editingRetrievalPolicy = false

    private var query: String {
        model.sourceSearchQuery.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    var body: some View {
        @Bindable var model = model
        GeometryReader { proxy in
            let compact = proxy.size.width < 620
            if compact {
                if model.sourceSelected != nil {
                    detailPane(compact: true)
                } else {
                    listPane(compact: true)
                }
            } else {
                HStack(spacing: 0) {
                    listPane(compact: false)
                        .frame(width: min(340, max(250, proxy.size.width * 0.38)))
                    VPHairline(vertical: true)
                    detailPane(compact: false)
                }
            }
        }
        .task(id: model.selectedProjectID) {
            if model.selectedProjectID != nil {
                await model.loadSources()
            }
        }
        .task(id: query) {
            if query.isEmpty {
                await model.searchSourceContents("")
            } else {
                try? await Task.sleep(for: .milliseconds(260))
                guard !Task.isCancelled else { return }
                await model.searchSourceContents(query)
            }
        }
        .task(id: pane) {
            if pane == .citations {
                await model.loadSessionCitations()
            }
        }
        .sheet(item: $composerMode) { mode in
            SourceComposerSheet(mode: mode)
                .environment(model)
        }
        .sheet(item: $editingSource) { source in
            SourceMetadataSheet(source: source)
                .environment(model)
        }
        .sheet(isPresented: $editingRetrievalPolicy) {
            SourceRetrievalPolicySheet(policy: model.sourceRetrievalPolicy ?? .defaults)
                .environment(model)
        }
        .confirmationDialog(
            L10n.t("Delete this source?"),
            isPresented: Binding(
                get: { pendingDelete != nil },
                set: {
                    if !$0 {
                        pendingDelete = nil
                    }
                }
            )
        ) {
            Button(L10n.t("Delete"), role: .destructive) {
                if let source = pendingDelete {
                    Task { await model.deleteSource(source) }
                }
                pendingDelete = nil
            }
            Button(L10n.t("Cancel"), role: .cancel) { pendingDelete = nil }
        } message: {
            Text(pendingDelete?.title ?? "")
        }
    }

    private func listPane(compact _: Bool) -> some View {
        @Bindable var model = model
        return VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 7) {
                Image(systemName: "books.vertical")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(VPTheme.plan)
                VStack(alignment: .leading, spacing: 1) {
                    Text(L10n.t("Sources"))
                        .font(VPTheme.mono(9, weight: .semibold))
                        .foregroundStyle(VPTheme.textPrimary)
                    Text(sourceStatsLine)
                        .font(VPTheme.mono(6.8))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                Spacer()
                if model.sourcesBusy {
                    ProgressView().controlSize(.small)
                }
                Menu {
                    ForEach(SourceComposerMode.allCases) { mode in
                        Button {
                            composerMode = mode
                        } label: {
                            Label(mode.title, systemImage: mode.icon)
                        }
                    }
                } label: {
                    Image(systemName: "plus")
                        .font(.system(size: 9, weight: .semibold))
                        .frame(width: 22, height: 22)
                        .background(VPTheme.surface, in: Circle())
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .fixedSize()
                .help(L10n.t("Add a source"))
                Button {
                    Task { await model.loadSources() }
                } label: {
                    Image(systemName: "arrow.clockwise")
                        .font(.system(size: 9, weight: .medium))
                }
                .buttonStyle(.plain)
                .foregroundStyle(VPTheme.textTertiary)
                .help(L10n.t("Refresh sources"))
            }
            .padding(8)

            Picker("", selection: $pane) {
                ForEach(SourceLibraryPane.allCases) { item in
                    Text(item.title).tag(item)
                }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .controlSize(.mini)
            .padding(.horizontal, 8)
            .padding(.bottom, 6)

            if pane == .library {
                collectionFilters
            }

            if pane == .library {
                HStack(spacing: 6) {
                    Image(systemName: "magnifyingglass")
                        .font(.system(size: 8.5))
                        .foregroundStyle(VPTheme.textTertiary)
                    TextField(L10n.t("Search source contents"), text: $model.sourceSearchQuery)
                        .textFieldStyle(.plain)
                        .font(VPTheme.mono(9))
                    if model.sourceSearchBusy {
                        ProgressView().controlSize(.mini)
                    }
                }
                .padding(.horizontal, 8)
                .padding(.vertical, 6)
                .background(VPTheme.surface.opacity(0.45), in: RoundedRectangle(cornerRadius: 7))
                .padding(.horizontal, 8)
                .padding(.bottom, 4)

                HStack(spacing: 5) {
                    Picker("", selection: $model.sourceSearchMode) {
                        ForEach(SourceSearchMode.allCases) { mode in
                            Text(mode.title).tag(mode)
                        }
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .controlSize(.mini)
                    .onChange(of: model.sourceSearchMode) { _, _ in
                        guard !query.isEmpty else { return }
                        Task { await model.searchSourceContents(query) }
                    }
                    if let fallback = model.sourceSearchFallbackReason {
                        Text(fallback)
                            .font(VPTheme.mono(6.4))
                            .foregroundStyle(VPTheme.warning.opacity(0.9))
                            .lineLimit(1)
                            .help(fallback)
                    } else if !model.sourceSearchEffectiveMode.isEmpty {
                        Text(model.sourceSearchEffectiveMode)
                            .font(VPTheme.mono(6.4))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                }
                .padding(.horizontal, 8)
                .padding(.bottom, 6)
            }

            if pane == .library {
                sourceEmbeddingCard
                sourceRetrievalCard
            }

            VPHairline()

            if pane == .citations {
                citationList
            } else if query.isEmpty {
                sourceList
            } else {
                sourceSearchList
            }

            if let status = model.sourceStatus {
                Text(status)
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textSecondary)
                    .lineLimit(2)
                    .padding(8)
            }
        }
    }

    private var sourceStatsLine: String {
        guard let stats = model.sourceStats else {
            return L10n.t("Project knowledge")
        }
        return "\(stats.indexed) indexed · \(stats.chunks) chunks"
    }

    /// Bounded semantic index status. This card intentionally exposes counts
    /// and a stable fallback reason only: vectors, embedded text, endpoint
    /// values, and credentials never reach the client.
    private var sourceEmbeddingCard: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 5) {
                Image(systemName: "sparkle.magnifyingglass")
                    .font(.system(size: 8, weight: .semibold))
                    .foregroundStyle(
                        model.sourceEmbeddingStatus?.enabled == true
                            ? VPTheme.accent
                            : VPTheme.textTertiary
                    )
                Text(L10n.t("Semantic index"))
                    .font(VPTheme.mono(7.4, weight: .semibold))
                    .foregroundStyle(VPTheme.textSecondary)
                Spacer(minLength: 0)
                if model.sourceEmbeddingBusy {
                    ProgressView().controlSize(.mini)
                }
                Button {
                    Task { await model.reindexSourceEmbeddings() }
                } label: {
                    Image(systemName: "arrow.triangle.2.circlepath")
                        .font(.system(size: 8, weight: .medium))
                }
                .buttonStyle(.plain)
                .foregroundStyle(VPTheme.textTertiary)
                .help(L10n.t("Clear derived vectors and queue enabled sources"))
            }
            if let status = model.sourceEmbeddingStatus {
                HStack(spacing: 4) {
                    auditChip(
                        "\(status.indexedChunks)/\(status.indexableChunks) \(L10n.t("indexed"))",
                        tint: status.coverage >= 0.999 ? VPTheme.success : VPTheme.accent
                    )
                    auditChip(
                        "\(status.activeJobs) \(L10n.t("pending"))",
                        tint: status.activeJobs > 0 ? VPTheme.warning : VPTheme.textTertiary
                    )
                    auditChip(
                        "\(status.attentionJobs) \(L10n.t("failed"))",
                        tint: status.attentionJobs > 0 ? VPTheme.danger : VPTheme.textTertiary
                    )
                    Spacer(minLength: 0)
                }
                if let reason = status.semanticUnavailable {
                    Text(reason)
                        .font(VPTheme.mono(6.5))
                        .foregroundStyle(VPTheme.warning.opacity(0.9))
                        .lineLimit(1)
                        .help(reason)
                } else if let modelName = status.model {
                    Text("\(modelName) · \(status.dimensions) dims · \(status.staleChunks) stale")
                        .font(VPTheme.mono(6.4))
                        .foregroundStyle(VPTheme.textTertiary)
                        .lineLimit(1)
                }
                if let error = status.lastError {
                    Text(error)
                        .font(VPTheme.mono(6.2))
                        .foregroundStyle(VPTheme.danger.opacity(0.85))
                        .lineLimit(1)
                        .help(error)
                }
            } else {
                Text(L10n.t("Lexical only until a source embedding policy is enabled."))
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            if let message = model.sourceEmbeddingMessage {
                Text(message)
                    .font(VPTheme.mono(6.3))
                    .foregroundStyle(VPTheme.textSecondary)
                    .lineLimit(2)
            }
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 5)
        .background(VPTheme.surface.opacity(0.32), in: RoundedRectangle(cornerRadius: 6))
        .padding(.horizontal, 8)
        .padding(.bottom, 6)
    }

    /// Bounded retrieval v2 status. It exposes candidate path, generation,
    /// counts, and a stable fallback reason only. Centroid/vector bytes never
    /// reach the client.
    private var sourceRetrievalCard: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 5) {
                Image(systemName: "point.3.filled.connected.trianglepath.dotted")
                    .font(.system(size: 8, weight: .semibold))
                    .foregroundStyle(
                        model.sourceIndexStatus?.ready == true
                            ? VPTheme.success
                            : VPTheme.textTertiary
                    )
                Text(L10n.t("Retrieval v2"))
                    .font(VPTheme.mono(7.4, weight: .semibold))
                    .foregroundStyle(VPTheme.textSecondary)
                Spacer(minLength: 0)
                if model.sourceRetrievalBusy {
                    ProgressView().controlSize(.mini)
                }
                Button {
                    editingRetrievalPolicy = true
                } label: {
                    Image(systemName: "slider.horizontal.3")
                        .font(.system(size: 8, weight: .medium))
                }
                .buttonStyle(.plain)
                .foregroundStyle(VPTheme.textTertiary)
                .help(L10n.t("Retrieval policy"))
                Button {
                    Task { await model.rebuildSourceIndex() }
                } label: {
                    Image(systemName: "square.stack.3d.up")
                        .font(.system(size: 8, weight: .medium))
                }
                .buttonStyle(.plain)
                .foregroundStyle(VPTheme.textTertiary)
                .help(L10n.t("Rebuild the derived source index"))
            }
            if let status = model.sourceIndexStatus {
                HStack(spacing: 4) {
                    auditChip(
                        status.activePathLabel,
                        tint: status.ready ? VPTheme.success : VPTheme.textTertiary
                    )
                    auditChip(
                        "\(status.vectorCount) \(L10n.t("vectors"))",
                        tint: VPTheme.accent
                    )
                    auditChip(
                        "\(status.centroidCount) \(L10n.t("centroids"))",
                        tint: VPTheme.plan
                    )
                    auditChip(
                        "nprobe \(status.nprobe)",
                        tint: VPTheme.textTertiary
                    )
                    Spacer(minLength: 0)
                }
                if let generation = status.generation {
                    Text(
                        "\(L10n.t("generation")) \(generation) · \(status.assignedVectors)/\(status.vectorCount) \(L10n.t("assigned")) · \(status.nlist) \(L10n.t("clusters"))"
                    )
                    .font(VPTheme.mono(6.4))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(1)
                }
                if let reason = status.fallbackReason {
                    Text(reason)
                        .font(VPTheme.mono(6.5))
                        .foregroundStyle(VPTheme.warning.opacity(0.9))
                        .lineLimit(1)
                        .help(reason)
                }
                if let error = status.lastError {
                    Text(error)
                        .font(VPTheme.mono(6.2))
                        .foregroundStyle(VPTheme.danger.opacity(0.85))
                        .lineLimit(1)
                        .help(error)
                }
            } else {
                Text(L10n.t("No derived index status. Lexical retrieval remains available."))
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            if let policy = model.sourceRetrievalPolicy {
                Text(
                    "\(policy.annEnabled ? L10n.t("ANN") : L10n.t("Exact scan")) · \(policy.rerank.title) \(L10n.t("rerank")) · \(policy.queryExpansion.title) \(L10n.t("expansion"))"
                )
                .font(VPTheme.mono(6.4))
                .foregroundStyle(VPTheme.textTertiary)
                .lineLimit(1)
            }
            if let message = model.sourceRetrievalMessage {
                Text(message)
                    .font(VPTheme.mono(6.3))
                    .foregroundStyle(VPTheme.textSecondary)
                    .lineLimit(2)
            }
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 5)
        .background(VPTheme.surface.opacity(0.32), in: RoundedRectangle(cornerRadius: 6))
        .padding(.horizontal, 8)
        .padding(.bottom, 6)
    }

    private var citationList: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 3) {
                if let audit = model.citationAudit {
                    VStack(alignment: .leading, spacing: 3) {
                        HStack(spacing: 4) {
                            auditChip(
                                "\(audit.messagesWithCitations) cited",
                                tint: VPTheme.success
                            )
                            if audit.messagesWithoutCitations > 0 {
                                auditChip(
                                    "\(audit.messagesWithoutCitations) uncited",
                                    tint: VPTheme.textTertiary
                                )
                            }
                            auditChip(
                                "\(audit.uniqueSources) sources",
                                tint: VPTheme.accent
                            )
                            if audit.danglingKeyCount > 0 {
                                auditChip(
                                    "\(audit.danglingKeyCount) dangling",
                                    tint: VPTheme.danger
                                )
                            }
                            if audit.unusedCitationCount > 0 {
                                auditChip(
                                    "\(audit.unusedCitationCount) unused",
                                    tint: VPTheme.warning
                                )
                            }
                            Spacer(minLength: 0)
                        }
                        if audit.danglingKeyCount > 0 {
                            Text(
                                L10n.t(
                                    "Dangling markers reference keys that are not in the message evidence."
                                )
                            )
                            .font(VPTheme.mono(6.8))
                            .foregroundStyle(VPTheme.danger.opacity(0.85))
                        }
                    }
                    .padding(.horizontal, 7)
                    .padding(.vertical, 5)
                }
                if model.selectedSessionID == nil {
                    Text(L10n.t("Select a session to inspect its citations."))
                        .font(VPTheme.mono(8.5))
                        .foregroundStyle(VPTheme.textSecondary)
                        .padding(12)
                } else if model.sessionCitations.isEmpty, !model.sessionCitationsBusy {
                    Text(
                        model.sessionCitationsError
                            ?? L10n.t("No citations yet. Ask the team a source-grounded question.")
                    )
                    .font(VPTheme.mono(8.5))
                    .foregroundStyle(
                        model.sessionCitationsError == nil
                            ? VPTheme.textSecondary
                            : VPTheme.warning
                    )
                    .padding(12)
                }
                ForEach(model.sessionCitations) { record in
                    Button {
                        pane = .library
                        Task {
                            await model.loadSource(
                                record.citation.sourceID,
                                chunkNo: record.citation.chunkNo
                            )
                        }
                    } label: {
                        HStack(alignment: .top, spacing: 7) {
                            Text("[\(record.citation.key)]")
                                .font(VPTheme.mono(7, weight: .semibold))
                                .foregroundStyle(VPTheme.accent)
                                .frame(width: 28, alignment: .leading)
                            VStack(alignment: .leading, spacing: 2) {
                                HStack(spacing: 4) {
                                    Text(record.citation.sourceTitle)
                                        .font(VPTheme.mono(8.5, weight: .semibold))
                                        .foregroundStyle(VPTheme.textPrimary)
                                        .lineLimit(1)
                                    Text("chunk \(record.citation.chunkNo)")
                                        .font(VPTheme.mono(6.5))
                                        .foregroundStyle(VPTheme.textTertiary)
                                }
                                if let heading = record.citation.heading, !heading.isEmpty {
                                    Text(heading)
                                        .font(VPTheme.mono(7))
                                        .foregroundStyle(VPTheme.accent.opacity(0.85))
                                        .lineLimit(1)
                                }
                                Text(record.citation.excerpt)
                                    .font(VPTheme.mono(7.8))
                                    .foregroundStyle(VPTheme.textSecondary)
                                    .lineLimit(2)
                            }
                            Spacer(minLength: 0)
                            Text(String(record.messageID.prefix(8)))
                                .font(VPTheme.mono(6.2))
                                .foregroundStyle(VPTheme.textTertiary)
                        }
                        .padding(.horizontal, 7)
                        .padding(.vertical, 5)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .buttonStyle(VPPressableButtonStyle(scale: 0.99))
                }
            }
            .padding(.horizontal, 6)
            .padding(.vertical, 5)
        }
        .overlay {
            if model.sessionCitationsBusy, model.sessionCitations.isEmpty {
                ProgressView().controlSize(.small)
            }
        }
    }

    private var visibleSources: [ProjectSource] {
        guard let filter = model.sourceCollectionFilter else {
            return model.sources
        }
        if filter.isEmpty {
            return model.sources.filter { ($0.collection ?? "").isEmpty }
        }
        return model.sources.filter { $0.collection == filter }
    }

    private var collectionFilters: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 4) {
                collectionChip(
                    title: L10n.t("All"),
                    value: nil,
                    count: model.sourceStats?.sources ?? model.sources.count
                )
                collectionChip(
                    title: L10n.t("Uncollected"),
                    value: "",
                    count: model.sourceCollections
                        .first(where: { $0.name == nil })?
                        .sources ?? 0
                )
                ForEach(model.sourceCollections.filter { $0.name != nil }) { collection in
                    if let name = collection.name {
                        collectionChip(
                            title: name,
                            value: name,
                            count: collection.sources
                        )
                    }
                }
            }
            .padding(.horizontal, 8)
            .padding(.bottom, 7)
        }
    }

    private func collectionChip(
        title: String,
        value: String?,
        count: Int
    ) -> some View {
        let selected = model.sourceCollectionFilter == value
        return Button {
            Task { await model.setSourceCollectionFilter(value) }
        } label: {
            HStack(spacing: 3) {
                Text(title)
                    .lineLimit(1)
                Text("\(count)")
                    .foregroundStyle(
                        selected ? VPTheme.accent.opacity(0.8) : VPTheme.textTertiary
                    )
            }
            .font(VPTheme.mono(7, weight: selected ? .semibold : .regular))
            .foregroundStyle(selected ? VPTheme.accent : VPTheme.textSecondary)
            .padding(.horizontal, 6)
            .padding(.vertical, 3)
            .background(
                selected ? VPTheme.accent.opacity(0.12) : VPTheme.surface.opacity(0.45),
                in: Capsule()
            )
            .overlay {
                Capsule()
                    .strokeBorder(
                        selected ? VPTheme.accent.opacity(0.30) : VPTheme.hairline,
                        lineWidth: 1
                    )
            }
        }
        .buttonStyle(.plain)
        .help(value == nil ? L10n.t("Show all sources") : title)
    }

    private var sourceList: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 2) {
                ForEach(visibleSources) { source in
                    Button {
                        Task { await model.loadSource(source.id) }
                    } label: {
                        sourceRow(source)
                    }
                    .buttonStyle(VPPressableButtonStyle(scale: 0.99))
                    .onHover { hovering in
                        hoveredSourceID = hovering ? source.id : nil
                    }
                }
                if visibleSources.isEmpty, !model.sourcesBusy {
                    Text(
                        model.selectedProjectID == nil
                            ? L10n.t("Select a project to manage its knowledge.")
                            : model.sources.isEmpty
                            ? L10n.t("No sources yet. Add a note, file, URL, or import a folder.")
                            : L10n.t("No sources in this collection.")
                    )
                    .font(VPTheme.mono(8.5))
                    .foregroundStyle(VPTheme.textSecondary)
                    .padding(12)
                }
            }
            .padding(.horizontal, 6)
            .padding(.vertical, 5)
        }
    }

    private var sourceSearchList: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 2) {
                ForEach(model.sourceSearchResults) { hit in
                    Button {
                        Task {
                            await model.loadSource(hit.sourceID, chunkNo: hit.chunkNo)
                        }
                    } label: {
                        VStack(alignment: .leading, spacing: 3) {
                            HStack(spacing: 5) {
                                Text(hit.sourceTitle)
                                    .font(VPTheme.mono(8.5, weight: .semibold))
                                    .foregroundStyle(VPTheme.textPrimary)
                                    .lineLimit(1)
                                if let collection = hit.collection, !collection.isEmpty {
                                    Text(collection)
                                        .font(VPTheme.mono(6.2))
                                        .foregroundStyle(VPTheme.plan)
                                        .lineLimit(1)
                                }
                                Spacer(minLength: 0)
                                Text("chunk \(hit.chunkNo)")
                                    .font(VPTheme.mono(6.5))
                                    .foregroundStyle(VPTheme.textTertiary)
                            }
                            if let heading = hit.heading, !heading.isEmpty {
                                Text(heading)
                                    .font(VPTheme.mono(7))
                                    .foregroundStyle(VPTheme.accent.opacity(0.9))
                                    .lineLimit(1)
                            }
                            Text(hit.content.replacingOccurrences(of: "\n", with: " "))
                                .font(VPTheme.mono(8))
                                .foregroundStyle(VPTheme.textSecondary)
                                .lineLimit(2)
                        }
                        .padding(.horizontal, 7)
                        .padding(.vertical, 5)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .buttonStyle(VPPressableButtonStyle(scale: 0.99))
                }
                if model.sourceSearchResults.isEmpty, !model.sourceSearchBusy {
                    Text(model.sourceSearchError ?? L10n.t("No source contents match."))
                        .font(VPTheme.mono(8))
                        .foregroundStyle(
                            model.sourceSearchError == nil
                                ? VPTheme.textSecondary
                                : VPTheme.warning
                        )
                        .padding(12)
                }
                if model.sourceSearchDegraded, !model.sourceSearchResults.isEmpty {
                    Text(L10n.t("Showing lexical fallback matches."))
                        .font(VPTheme.mono(6.8))
                        .foregroundStyle(VPTheme.textTertiary)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 5)
                }
            }
            .padding(.horizontal, 6)
            .padding(.vertical, 5)
        }
    }

    private func sourceRow(_ source: ProjectSource) -> some View {
        HStack(alignment: .top, spacing: 7) {
            Image(systemName: sourceIcon(source))
                .font(.system(size: 9, weight: .medium))
                .foregroundStyle(sourceColor(source))
                .frame(width: 13)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 5) {
                    Text(source.title)
                        .font(VPTheme.mono(8.8, weight: .medium))
                        .foregroundStyle(VPTheme.textPrimary)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                    Text(source.status.uppercased())
                        .font(VPTheme.mono(6.2, weight: .semibold))
                        .foregroundStyle(sourceColor(source))
                }
                Text(
                    "\(source.kind)\(source.collection.map { " · \($0)" } ?? "") · \(source.chunkCount) chunks · \(formatSourceSize(source.sizeBytes))"
                )
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textTertiary)
                if let error = source.error, !error.isEmpty {
                    Text(error)
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.warning)
                        .lineLimit(2)
                }
            }
            if model.sourceActionBusyID == source.id {
                ProgressView().controlSize(.mini)
            }
        }
        .padding(.horizontal, 6)
        .padding(.vertical, 5)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background {
            if model.sourceSelectedID == source.id {
                RoundedRectangle(cornerRadius: 7).fill(VPTheme.accent.opacity(0.10))
            } else if hoveredSourceID == source.id {
                RoundedRectangle(cornerRadius: 7).fill(VPTheme.surface)
            }
        }
    }

    private func detailPane(compact: Bool) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            if let result = model.sourceSelected {
                HStack(spacing: 7) {
                    if compact {
                        Button {
                            model.sourceSelectedID = nil
                            model.sourceSelected = nil
                        } label: {
                            Image(systemName: "chevron.left")
                                .font(.system(size: 9, weight: .semibold))
                        }
                        .buttonStyle(VPGlassIconButtonStyle(size: 22, tint: VPTheme.textSecondary))
                        .help(L10n.t("Back to sources"))
                    }
                    Image(systemName: sourceIcon(result.source))
                        .font(.system(size: 11))
                        .foregroundStyle(sourceColor(result.source))
                    VStack(alignment: .leading, spacing: 1) {
                        Text(result.source.title)
                            .font(VPTheme.mono(10, weight: .semibold))
                            .foregroundStyle(VPTheme.textPrimary)
                            .lineLimit(1)
                        Text(
                            "\(result.source.kind) · \(result.totalChunks) chunks · \(result.source.status)"
                        )
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                    }
                    Spacer()
                    if model.sourceSelectedBusy {
                        ProgressView().controlSize(.small)
                    }
                    Button {
                        editingSource = result.source
                    } label: {
                        Image(systemName: "pencil")
                            .font(.system(size: 9, weight: .medium))
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(VPTheme.textTertiary)
                    .help(L10n.t("Edit source metadata"))
                    Button {
                        Task { await model.refreshSource(result.source) }
                    } label: {
                        Image(systemName: "arrow.clockwise")
                            .font(.system(size: 9, weight: .medium))
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(VPTheme.textTertiary)
                    .disabled(model.sourceActionBusyID == result.source.id)
                    .help(L10n.t("Refresh source"))
                    VPCloseButton(size: 22, help: L10n.t("Delete source")) {
                        pendingDelete = result.source
                    }
                }
                .padding(10)

                if let uri = result.source.uri, !uri.isEmpty {
                    Text(uri)
                        .font(VPTheme.mono(7.5))
                        .foregroundStyle(VPTheme.textTertiary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .padding(.horizontal, 10)
                        .padding(.bottom, 6)
                }
                VPHairline()

                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 8) {
                            ForEach(result.chunks) { chunk in
                                SourceChunkCard(
                                    chunk: chunk,
                                    highlighted: model.sourceSelectedChunk == chunk.chunkNo
                                )
                                .id(chunk.chunkNo)
                            }
                        }
                        .padding(10)
                    }
                    .onAppear {
                        if let chunk = model.sourceSelectedChunk {
                            proxy.scrollTo(chunk, anchor: .top)
                        }
                    }
                    .onChange(of: model.sourceSelectedChunk) { _, chunk in
                        if let chunk {
                            withAnimation(VPMotion.smooth) {
                                proxy.scrollTo(chunk, anchor: .top)
                            }
                        }
                    }
                }
            } else {
                StudioEmptyView(
                    icon: "books.vertical",
                    title: L10n.t("Project sources"),
                    detail: L10n.t(
                        "Add notes, project files, and public pages. Agents can search them and attach exact citations."
                    )
                )
            }
        }
    }

    private func auditChip(_ text: String, tint: Color) -> some View {
        Text(text)
            .font(VPTheme.mono(6.5, weight: .semibold))
            .foregroundStyle(tint)
            .padding(.horizontal, 5)
            .padding(.vertical, 1)
            .background(tint.opacity(0.10), in: Capsule())
    }

    private func sourceIcon(_ source: ProjectSource) -> String {
        switch source.kind {
        case "note": "note.text"
        case "url": "link"
        default: "doc.text"
        }
    }

    private func sourceColor(_ source: ProjectSource) -> Color {
        switch source.status {
        case "indexed": VPTheme.success
        case "failed": VPTheme.danger
        case "indexing", "queued": VPTheme.warning
        default: VPTheme.textTertiary
        }
    }

    private func formatSourceSize(_ bytes: UInt64) -> String {
        if bytes < 1024 {
            return "\(bytes) B"
        }
        if bytes < 1024 * 1024 {
            return String(format: "%.1f KB", Double(bytes) / 1024)
        }
        return String(format: "%.1f MB", Double(bytes) / (1024 * 1024))
    }
}

private struct SourceChunkCard: View {
    let chunk: SourceChunk
    let highlighted: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 6) {
                Text("[\(chunk.chunkNo)]")
                    .font(VPTheme.mono(7, weight: .semibold))
                    .foregroundStyle(VPTheme.accent)
                Text(chunk.heading?.isEmpty == false ? chunk.heading! : L10n.t("Untitled section"))
                    .font(VPTheme.mono(8, weight: .semibold))
                    .foregroundStyle(VPTheme.textSecondary)
                    .lineLimit(1)
                Spacer(minLength: 0)
                Text("\(chunk.tokenCount) tok")
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            Text(chunk.content)
                .font(VPTheme.mono(8.5))
                .foregroundStyle(VPTheme.textPrimary)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(9)
        .background(
            highlighted ? VPTheme.accent.opacity(0.10) : VPTheme.surface.opacity(0.45),
            in: RoundedRectangle(cornerRadius: 8)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 8)
                .strokeBorder(
                    highlighted ? VPTheme.accent.opacity(0.35) : VPTheme.hairline,
                    lineWidth: 1
                )
        }
    }
}

private struct SourceComposerSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let mode: SourceComposerMode
    @State private var busy = false
    @State private var errorMessage: String?

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 8) {
                Image(systemName: mode.icon)
                    .foregroundStyle(VPTheme.accent)
                VStack(alignment: .leading, spacing: 2) {
                    Text(mode.title)
                        .font(VPTheme.font(15, weight: .semibold))
                    Text(mode.detail)
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textSecondary)
                }
                Spacer()
                VPCloseButton(size: 24, help: L10n.t("Close")) { dismiss() }
            }

            switch mode {
            case .note:
                TextField(L10n.t("Source title"), text: $model.sourceDraftTitle)
                    .textFieldStyle(.roundedBorder)
                TextEditor(text: $model.sourceDraftContent)
                    .font(VPTheme.mono(10))
                    .scrollContentBackground(.hidden)
                    .frame(minHeight: 160)
                    .padding(6)
                    .background(VPTheme.surfaceSunken, in: RoundedRectangle(cornerRadius: 8))
                    .overlay {
                        RoundedRectangle(cornerRadius: 8)
                            .strokeBorder(VPTheme.hairline, lineWidth: 1)
                    }
            case .file:
                TextField(L10n.t("Path relative to the project root"), text: $model.sourceDraftPath)
                    .textFieldStyle(.roundedBorder)
                    .font(VPTheme.mono(10))
                TextField(L10n.t("Optional title"), text: $model.sourceDraftTitle)
                    .textFieldStyle(.roundedBorder)
            case .url:
                TextField(L10n.t("Public https:// URL"), text: $model.sourceDraftURL)
                    .textFieldStyle(.roundedBorder)
                    .font(VPTheme.mono(10))
                TextField(L10n.t("Optional title"), text: $model.sourceDraftTitle)
                    .textFieldStyle(.roundedBorder)
            case .importFiles:
                TextField(
                    L10n.t("Folder relative to the project root (optional)"),
                    text: $model.sourceDraftPath
                )
                .textFieldStyle(.roundedBorder)
                .font(VPTheme.mono(10))
            }

            TextField(
                L10n.t("Collection (optional)"),
                text: $model.sourceDraftCollection
            )
            .textFieldStyle(.roundedBorder)
            .font(VPTheme.mono(9))

            if let errorMessage {
                Text(errorMessage)
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.warning)
            } else if let status = model.sourceStatus {
                Text(status)
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textSecondary)
            }

            HStack {
                Spacer()
                Button(L10n.t("Cancel")) { dismiss() }
                    .buttonStyle(.bordered)
                Button(mode == .importFiles ? L10n.t("Import") : L10n.t("Add source")) {
                    Task { await submit() }
                }
                .buttonStyle(.borderedProminent)
                .disabled(busy)
            }
        }
        .padding(20)
        .frame(width: 520)
        .background { VPCrystalBackdrop() }
    }

    private func submit() async {
        busy = true
        defer { busy = false }
        let ok: Bool = switch mode {
        case .note:
            await model.addSourceNote()
        case .file:
            await model.addSourceFile()
        case .url:
            await model.addSourceURL()
        case .importFiles:
            await model.importProjectSources()
        }
        if ok {
            dismiss()
        } else {
            errorMessage = model.sourceStatus
        }
    }
}

private struct SourceMetadataSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let source: ProjectSource

    @State private var title: String
    @State private var collection: String
    @State private var labels: String
    @State private var busy = false
    @State private var error: String?

    init(source: ProjectSource) {
        self.source = source
        _title = State(initialValue: source.title)
        _collection = State(initialValue: source.collection ?? "")
        _labels = State(initialValue: source.labels.joined(separator: ", "))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Image(systemName: "pencil.and.list.clipboard")
                    .foregroundStyle(VPTheme.accent)
                VStack(alignment: .leading, spacing: 2) {
                    Text(L10n.t("Source metadata"))
                        .font(VPTheme.font(16, weight: .semibold))
                    Text(L10n.t("Collection and labels do not change the indexed content."))
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textSecondary)
                }
                Spacer()
                VPCloseButton(size: 24, help: L10n.t("Close")) { dismiss() }
            }

            TextField(L10n.t("Title"), text: $title)
                .textFieldStyle(.roundedBorder)
                .font(VPTheme.mono(10))

            HStack(spacing: 8) {
                TextField(L10n.t("Collection (empty for uncollected)"), text: $collection)
                    .textFieldStyle(.roundedBorder)
                    .font(VPTheme.mono(10))
                Menu {
                    Button(L10n.t("Uncollected")) { collection = "" }
                    Divider()
                    ForEach(model.sourceCollections.filter { $0.name != nil }) { item in
                        if let name = item.name {
                            Button(name) { collection = name }
                        }
                    }
                } label: {
                    Image(systemName: "chevron.down")
                        .font(.system(size: 9, weight: .semibold))
                        .frame(width: 26, height: 24)
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .fixedSize()
                .help(L10n.t("Choose an existing collection"))
            }

            TextField(L10n.t("Labels (comma separated)"), text: $labels)
                .textFieldStyle(.roundedBorder)
                .font(VPTheme.mono(10))

            if let error {
                Text(error)
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.warning)
                    .lineLimit(3)
            }

            HStack {
                Text("\(source.chunkCount) chunks · \(source.kind)")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
                Spacer()
                Button(L10n.t("Cancel")) { dismiss() }
                    .buttonStyle(.bordered)
                Button(L10n.t("Save")) {
                    Task { await save() }
                }
                .buttonStyle(.borderedProminent)
                .disabled(busy || title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(20)
        .frame(width: 520)
        .background { VPCrystalBackdrop() }
    }

    private func save() async {
        busy = true
        defer { busy = false }
        let ok = await model.updateSourceMetadata(
            source,
            title: title,
            collection: collection.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                ? nil
                : collection,
            labels: parseLabels(labels)
        )
        if ok {
            dismiss()
        } else {
            error = model.sourceStatus
        }
    }

    private func parseLabels(_ value: String) -> [String] {
        var output: [String] = []
        for item in value.split(whereSeparator: { $0 == "," || $0 == "\n" }) {
            let label = item.trimmingCharacters(in: .whitespacesAndNewlines)
            if !label.isEmpty, !output.contains(label) {
                output.append(label)
            }
        }
        return output
    }
}

/// Project retrieval v2 controls. Every value is clamped by the daemon; this
/// sheet only edits the persisted policy and never writes derived index state.
private struct SourceRetrievalPolicySheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    @State private var annEnabled: Bool
    @State private var annMinVectors: Int
    @State private var annNProbe: Int
    @State private var rerank: SourceRerankMode
    @State private var queryExpansion: SourceQueryExpansionMode
    @State private var maxExpandedTerms: Int
    @State private var busy = false

    init(policy: SourceRetrievalPolicy) {
        _annEnabled = State(initialValue: policy.annEnabled)
        _annMinVectors = State(initialValue: policy.annMinVectors)
        _annNProbe = State(initialValue: policy.annNProbe)
        _rerank = State(initialValue: policy.rerank)
        _queryExpansion = State(initialValue: policy.queryExpansion)
        _maxExpandedTerms = State(initialValue: policy.maxExpandedTerms)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Image(systemName: "slider.horizontal.3")
                    .foregroundStyle(VPTheme.accent)
                VStack(alignment: .leading, spacing: 2) {
                    Text(L10n.t("Source retrieval v2"))
                        .font(VPTheme.font(16, weight: .semibold))
                    Text(
                        L10n.t(
                            "ANN, rerank, and lexical expansion are deterministic. No model is selected here."
                        )
                    )
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textSecondary)
                }
                Spacer()
                VPCloseButton(size: 24, help: L10n.t("Close")) { dismiss() }
            }

            Toggle(isOn: $annEnabled) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(L10n.t("Derived ANN index"))
                        .font(VPTheme.mono(9, weight: .semibold))
                    Text(
                        L10n.t(
                            "Probes bounded IVF clusters; the exact cosine scan remains the fallback."
                        )
                    )
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
                }
            }
            .toggleStyle(.switch)

            HStack(spacing: 14) {
                Stepper(
                    "\(L10n.t("Minimum vectors")) \(annMinVectors)",
                    value: $annMinVectors,
                    in: 1 ... 100_000,
                    step: 16
                )
                .font(VPTheme.mono(8.5))
                Stepper(
                    "\(L10n.t("nprobe")) \(annNProbe)",
                    value: $annNProbe,
                    in: 1 ... 32
                )
                .font(VPTheme.mono(8.5))
            }
            .disabled(!annEnabled)

            VStack(alignment: .leading, spacing: 4) {
                Text(L10n.t("Rerank"))
                    .font(VPTheme.mono(8.5, weight: .semibold))
                    .foregroundStyle(VPTheme.textSecondary)
                Picker("", selection: $rerank) {
                    ForEach(SourceRerankMode.allCases) { mode in
                        Text(mode.title).tag(mode)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .controlSize(.small)
            }

            VStack(alignment: .leading, spacing: 4) {
                Text(L10n.t("Query expansion"))
                    .font(VPTheme.mono(8.5, weight: .semibold))
                    .foregroundStyle(VPTheme.textSecondary)
                Picker("", selection: $queryExpansion) {
                    ForEach(SourceQueryExpansionMode.allCases) { mode in
                        Text(mode.title).tag(mode)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .controlSize(.small)
                Stepper(
                    "\(L10n.t("Maximum expansion terms")) \(maxExpandedTerms)",
                    value: $maxExpandedTerms,
                    in: 1 ... 16
                )
                .font(VPTheme.mono(8.5))
                .disabled(queryExpansion == .off)
            }

            if let status = model.sourceIndexStatus {
                HStack(spacing: 6) {
                    Text(status.activePathLabel)
                        .font(VPTheme.mono(7, weight: .semibold))
                        .foregroundStyle(status.ready ? VPTheme.success : VPTheme.textTertiary)
                    Text("\(status.vectorCount) \(L10n.t("vectors"))")
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.accent)
                    if let reason = status.fallbackReason {
                        Text(reason)
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.warning)
                            .lineLimit(1)
                    }
                    Spacer(minLength: 0)
                }
            }

            HStack {
                Text(
                    L10n.t(
                        "Disabling ANN keeps the derived index on disk; it is never used until re-enabled."
                    )
                )
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textTertiary)
                Spacer()
                Button(L10n.t("Cancel")) { dismiss() }
                    .buttonStyle(.bordered)
                Button(L10n.t("Save")) {
                    Task { await save() }
                }
                .buttonStyle(.borderedProminent)
                .disabled(busy)
            }
        }
        .padding(20)
        .frame(width: 560)
        .background { VPCrystalBackdrop() }
    }

    private func save() async {
        busy = true
        defer { busy = false }
        let policy = SourceRetrievalPolicy(
            annEnabled: annEnabled,
            annMinVectors: annMinVectors,
            annNProbe: annNProbe,
            rerank: rerank,
            queryExpansion: queryExpansion,
            maxExpandedTerms: maxExpandedTerms
        )
        await model.updateSourceRetrievalPolicy(policy)
        dismiss()
    }
}
