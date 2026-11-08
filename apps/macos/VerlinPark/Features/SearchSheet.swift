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
                    Text("SEARCH")
                        .font(VPTheme.mono(8, weight: .semibold))
                        .tracking(1.2)
                        .foregroundStyle(VPTheme.accent)
                    Text("Find anything the team said or did")
                        .font(VPTheme.font(20, weight: .semibold, rounded: true))
                    Text("Full-text search across session messages and raw event payloads.")
                        .font(VPTheme.mono(8.5))
                        .foregroundStyle(VPTheme.textSecondary)
                }
                Spacer()
                Toggle("This project", isOn: $scopeToProject)
                    .toggleStyle(.checkbox)
                    .font(VPTheme.mono(8.5))
            }

            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .foregroundStyle(VPTheme.textTertiary)
                TextField("Search messages and events", text: $model.searchQuery)
                    .textFieldStyle(.plain)
                    .font(VPTheme.font(14))
                    .foregroundStyle(VPTheme.textPrimary)
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
                .buttonStyle(VPPrimaryButtonStyle(tint: VPTheme.accent))
                .disabled(model.searchQuery.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            .padding(.horizontal, 11)
            .padding(.vertical, 9)
            .vpFieldSurface(radius: 10, focused: queryFocused)

            if model.searchHits.isEmpty {
                VStack(spacing: 6) {
                    VPFloatingSymbol(name: "text.magnifyingglass", size: 22, tint: .secondary)
                    Text(
                        model.searchQuery.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                            ? "Type a query to search across sessions."
                            : "No matches yet."
                    )
                    .font(VPTheme.mono(9))
                    .foregroundStyle(VPTheme.textSecondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVStack(spacing: 6) {
                        ForEach(Array(model.searchHits.enumerated()), id: \.element.id) { index, hit in
                            Button {
                                Task {
                                    await model.openSearchHit(hit)
                                    dismiss()
                                }
                            } label: {
                                SearchHitRow(hit: hit)
                            }
                            .buttonStyle(VPPressableButtonStyle(scale: 0.99))
                            .transition(.vpRise)
                            .vpAppear(index: index)
                        }
                    }
                    .animation(VPMotion.smooth, value: model.searchHits.count)
                }
            }

            HStack {
                Text(
                    model.searchTruncated
                        ? "Showing the newest matches. Narrow the query for more precision."
                        : "\(model.searchHits.count) match\(model.searchHits.count == 1 ? "" : "es")"
                )
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
                .contentTransition(.numericText())
                .animation(VPMotion.snap, value: model.searchHits.count)
                Spacer()
                Button("Close") {
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)
            }
        }
        .padding(22)
        .frame(width: 760, height: 560)
        .background(VPTheme.canvas)
        .tint(VPTheme.accent)
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
                    .font(VPTheme.mono(7.5, weight: .semibold))
                    .foregroundStyle(hit.source == "event" ? VPTheme.warning : VPTheme.info)
                    .padding(.horizontal, 5)
                    .padding(.vertical, 2)
                    .background(
                        (hit.source == "event" ? VPTheme.warning : VPTheme.info).opacity(0.10),
                        in: Capsule()
                    )
                Text(hit.detail)
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textSecondary)
                Spacer()
                Text(VPRelativeTime.label(hit.createdAt))
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            Text(hit.sessionGoal)
                .font(VPTheme.font(11, weight: .medium))
                .foregroundStyle(VPTheme.textPrimary)
                .lineLimit(1)
            Text(hit.snippet)
                .font(VPTheme.mono(9))
                .foregroundStyle(VPTheme.textSecondary)
                .lineLimit(3)
                .multilineTextAlignment(.leading)
            Text(hit.sessionId)
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
        .vpHoverLift(scale: 1.004, lift: 1, glow: VPTheme.info)
    }
}
