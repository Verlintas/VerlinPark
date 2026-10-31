import SwiftUI

struct SearchSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var scopeToProject = true
    @FocusState private var queryFocused: Bool

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 3) {
                    Text("Search")
                        .font(.title3.weight(.semibold))
                    Text("Find text in session messages and event payloads.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Toggle("This project", isOn: $scopeToProject)
                    .toggleStyle(.checkbox)
                    .font(.caption)
            }

            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .foregroundStyle(.secondary)
                TextField("Search messages and events", text: $model.searchQuery)
                    .textFieldStyle(.plain)
                    .font(.system(size: 14))
                    .focused($queryFocused)
                    .onSubmit {
                        Task { await runSearch() }
                    }
                if model.searchBusy {
                    ProgressView().controlSize(.small)
                }
                Button("Search") {
                    Task { await runSearch() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(model.searchQuery.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .background(.white.opacity(0.06), in: RoundedRectangle(cornerRadius: 9, style: .continuous))
            .overlay {
                RoundedRectangle(cornerRadius: 9, style: .continuous)
                    .strokeBorder(.white.opacity(0.08), lineWidth: 1)
            }

            if model.searchHits.isEmpty {
                VStack(spacing: 6) {
                    Image(systemName: "text.magnifyingglass")
                        .font(.system(size: 22, weight: .light))
                        .foregroundStyle(.tertiary)
                    Text(
                        model.searchQuery.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                            ? "Type a query to search across sessions."
                            : "No matches yet."
                    )
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVStack(spacing: 6) {
                        ForEach(model.searchHits) { hit in
                            Button {
                                Task {
                                    await model.openSearchHit(hit)
                                    dismiss()
                                }
                            } label: {
                                SearchHitRow(hit: hit)
                            }
                            .buttonStyle(.plain)
                        }
                    }
                }
            }

            HStack {
                Text(
                    model.searchTruncated
                        ? "Showing the newest matches. Narrow the query for more precision."
                        : "\(model.searchHits.count) match\(model.searchHits.count == 1 ? "" : "es")"
                )
                .font(.system(size: 9, design: .monospaced))
                .foregroundStyle(.tertiary)
                Spacer()
                Button("Close") {
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)
            }
        }
        .padding(20)
        .frame(width: 720, height: 520)
        .onAppear {
            queryFocused = true
            if !model.searchQuery.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                Task { await runSearch() }
            }
        }
        .task(id: debounceKey) {
            guard !model.searchQuery.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
                model.searchHits = []
                model.searchTruncated = false
                return
            }
            try? await Task.sleep(for: .milliseconds(250))
            guard !Task.isCancelled else { return }
            await runSearch()
        }
    }

    private var debounceKey: String {
        "\(scopeToProject)-\(model.searchQuery)"
    }

    private func runSearch() async {
        await model.runSearch(projectID: scopeToProject ? model.selectedProjectID : nil)
    }
}

private struct SearchHitRow: View {
    let hit: SearchHit

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Text(hit.source.uppercased())
                    .font(.system(size: 8, weight: .semibold, design: .monospaced))
                    .foregroundStyle(hit.source == "event" ? Color.orange : Color.cyan)
                    .padding(.horizontal, 5)
                    .padding(.vertical, 2)
                    .background(.white.opacity(0.06), in: Capsule())
                Text(hit.detail)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.secondary)
                Spacer()
                Text(hit.createdAt)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
            Text(hit.sessionGoal)
                .font(.system(size: 11, weight: .medium))
                .lineLimit(1)
            Text(hit.snippet)
                .font(.system(size: 9, design: .monospaced))
                .foregroundStyle(.secondary)
                .lineLimit(3)
                .multilineTextAlignment(.leading)
            Text(hit.sessionId)
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
        }
        .padding(9)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.white.opacity(0.035), in: RoundedRectangle(cornerRadius: 9, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .strokeBorder(.white.opacity(0.05), lineWidth: 1)
        }
    }
}
