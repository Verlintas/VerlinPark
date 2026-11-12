import SwiftUI

// MARK: - Shared typed-error presentation

/// Typed RPC failures carry the Q3 code and hint in `error.data`. The UI keeps
/// the human message primary and shows the stable code as a secondary badge so
/// an operator can distinguish a missing host tool from a rejected entry.
private func typedErrorPresentation(_ error: Error) -> (message: String, code: String?, hint: String?) {
    if let core = error as? CoreError {
        return (core.localizedDescription, core.typedCode, core.typedHint)
    }
    return (error.localizedDescription, nil, nil)
}

private func archiveByteCount(_ bytes: Int) -> String {
    ByteCountFormatter.string(fromByteCount: Int64(bytes), countStyle: .file)
}

private func archiveDuration(_ seconds: Double?) -> String {
    guard let seconds, seconds.isFinite, seconds >= 0 else { return "—" }
    let total = Int(seconds.rounded())
    if total >= 3600 {
        return String(format: "%d:%02d:%02d", total / 3600, (total % 3600) / 60, total % 60)
    }
    return String(format: "%d:%02d", total / 60, total % 60)
}

private struct TypedErrorBanner: View {
    let message: String
    let code: String?
    let hint: String?
    let retry: (() -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 6) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(VPTheme.warning)
                Text(message)
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 4)
                if let code {
                    Text(code)
                        .font(VPTheme.mono(7, weight: .semibold))
                        .foregroundStyle(VPTheme.warning)
                        .padding(.horizontal, 5)
                        .padding(.vertical, 2)
                        .background(VPTheme.warning.opacity(0.10), in: Capsule())
                }
                if let retry {
                    Button(L10n.t("Retry"), action: retry)
                        .buttonStyle(.plain)
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.accent)
                }
            }
            if let hint, !hint.isEmpty {
                Text(hint)
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(8)
        .background(VPTheme.warning.opacity(0.06), in: RoundedRectangle(cornerRadius: 8))
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(VPTheme.warning.opacity(0.18), lineWidth: 1)
        )
    }
}

// MARK: - Archive browser

/// Safe archive browser with explicit entry selection. Unsafe entries are
/// visible but cannot be selected; extraction only publishes daemon-owned
/// content-addressed artifacts and never opens a workspace path.
struct ArchiveBrowserView: View {
    @Environment(AppModel.self) private var model
    let artifact: Artifact

    @State private var listing: ArtifactArchiveListResult?
    @State private var selection = Set<String>()
    @State private var filter = ""
    @State private var loading = false
    @State private var extracting = false
    @State private var errorMessage: String?
    @State private var errorCode: String?
    @State private var errorHint: String?
    @State private var extraction: ArtifactArchiveExtractResult?

    var body: some View {
        VStack(spacing: 0) {
            header
            VPHairline()
            if let errorMessage {
                TypedErrorBanner(
                    message: errorMessage,
                    code: errorCode,
                    hint: errorHint,
                    retry: { Task { await load() } }
                )
                .padding(.horizontal, 14)
                .padding(.top, 10)
            }
            if loading, listing == nil {
                loadingState
            } else if let listing {
                entries(listing)
            } else {
                emptyState
            }
        }
        .task(id: artifact.id) {
            await load()
        }
    }

    private var header: some View {
        HStack(spacing: 9) {
            Image(systemName: "archivebox")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(VPTheme.textSecondary)
            VStack(alignment: .leading, spacing: 2) {
                Text(artifact.name)
                    .font(VPTheme.font(12, weight: .semibold))
                    .lineLimit(1)
                if let listing {
                    Text(
                        "\(listing.format) · \(listing.entryCount) entries · \(archiveByteCount(listing.totalUncompressedBytes)) uncompressed · \(listing.safeFileCount) safe files"
                    )
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
                } else {
                    Text(L10n.t("Reading the bounded archive directory…"))
                        .font(VPTheme.mono(7.5))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
            Spacer(minLength: 6)
            if let listing {
                if listing.truncated {
                    Label(L10n.t("Truncated at cap"), systemImage: "exclamationmark.circle")
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.warning)
                }
                Button {
                    selectSafeFiles(listing)
                } label: {
                    Label(L10n.t("Select safe files"), systemImage: "checkmark.circle")
                        .font(VPTheme.mono(8))
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(extracting || listing.safeFileCount == 0)

                Button {
                    Task { await extract(includePreviews: true) }
                } label: {
                    if extracting {
                        ProgressView().controlSize(.mini)
                    } else {
                        Label(L10n.t("Extract selected"), systemImage: "arrow.down.doc")
                            .font(VPTheme.mono(8))
                    }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(extracting || selection.isEmpty)
            }
            Button {
                Task { await load() }
            } label: {
                Image(systemName: "arrow.clockwise")
                    .font(.system(size: 10, weight: .semibold))
            }
            .buttonStyle(VPGlassIconButtonStyle(size: 24, tint: VPTheme.textSecondary))
            .help(L10n.t("Reload archive listing"))
            .disabled(loading || extracting)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 11)
    }

    private func entries(_ listing: ArtifactArchiveListResult) -> some View {
        VStack(spacing: 0) {
            filterBar(listing)
            Divider().opacity(0.25)
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    if visibleEntries(listing).isEmpty {
                        Text(L10n.t("No entries match the filter."))
                            .font(VPTheme.mono(8))
                            .foregroundStyle(VPTheme.textTertiary)
                            .padding(14)
                    }
                    ForEach(visibleEntries(listing)) { entry in
                        ArchiveEntryRow(
                            entry: entry,
                            isSelected: selection.contains(entry.path),
                            toggle: { selected in
                                if selected {
                                    selection.insert(entry.path)
                                } else {
                                    selection.remove(entry.path)
                                }
                            }
                        )
                        Divider().opacity(0.12)
                    }
                }
            }
            if let extraction {
                VPHairline()
                extractionFooter(extraction)
            } else if !selection.isEmpty {
                VPHairline()
                HStack(spacing: 7) {
                    Text(
                        L10n.format(
                            "%d selected · extraction writes only daemon scratch and published artifacts",
                            selection.count
                        )
                    )
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
                    Spacer()
                    Text(
                        L10n.t(
                            "Traversal, absolute paths, links, devices, duplicates, bombs, and encrypted archives are rejected before extraction."
                        )
                    )
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(2)
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 8)
            }
        }
    }

    private func filterBar(_ listing: ArtifactArchiveListResult) -> some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 9))
                .foregroundStyle(VPTheme.textTertiary)
            TextField(L10n.t("Filter entries"), text: $filter)
                .textFieldStyle(.plain)
                .font(VPTheme.mono(8))
            if !filter.isEmpty {
                Button {
                    filter = ""
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .font(.system(size: 9))
                }
                .buttonStyle(.plain)
                .foregroundStyle(VPTheme.textTertiary)
            }
            Spacer()
            Text(
                "\(listing.limits.maxEntries) entries · \(archiveByteCount(listing.limits.maxEntryBytes))/entry · \(archiveByteCount(listing.limits.maxTotalBytes)) total · \(listing.limits.maxCompressionRatio):1 ratio"
            )
            .font(VPTheme.mono(6.8))
            .foregroundStyle(VPTheme.textTertiary)
            .lineLimit(1)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
    }

    private func visibleEntries(_ listing: ArtifactArchiveListResult) -> [ArchiveEntry] {
        let needle = filter.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !needle.isEmpty else { return listing.entries }
        return listing.entries.filter {
            $0.path.lowercased().contains(needle) || $0.kind.lowercased().contains(needle)
        }
    }

    private var loadingState: some View {
        VStack(spacing: 8) {
            ProgressView().controlSize(.small)
            Text(L10n.t("Listing archive entries…"))
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textSecondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(24)
    }

    private var emptyState: some View {
        VStack(spacing: 8) {
            Image(systemName: "archivebox")
                .font(.system(size: 24, weight: .light))
                .foregroundStyle(VPTheme.textTertiary)
            Text(L10n.t("Archive listing is unavailable."))
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textSecondary)
            Button(L10n.t("Retry")) {
                Task { await load() }
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(24)
    }

    private func extractionFooter(_ extraction: ArtifactArchiveExtractResult) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 6) {
                Image(systemName: "checkmark.seal")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(VPTheme.success)
                Text(
                    L10n.format(
                        "Extracted %d entries · %@ · %d published artifacts · scratch removed",
                        extraction.entries.count,
                        archiveByteCount(extraction.totalBytes),
                        extraction.publishedArtifacts.count
                    )
                )
                .font(VPTheme.mono(8, weight: .semibold))
                .foregroundStyle(VPTheme.textSecondary)
                Spacer()
                Button(L10n.t("Dismiss")) {
                    self.extraction = nil
                    selection.removeAll()
                }
                .buttonStyle(.plain)
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.accent)
            }
            ForEach(extraction.entries.prefix(12)) { entry in
                HStack(spacing: 6) {
                    Text(entry.path)
                        .font(VPTheme.mono(7.5))
                        .foregroundStyle(VPTheme.textSecondary)
                        .lineLimit(1)
                    Spacer()
                    if let artifactID = entry.publishedArtifactId {
                        Text(artifactID)
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.accent)
                            .lineLimit(1)
                    } else if let reason = entry.skippedReason {
                        Text(reason)
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.warning)
                    }
                }
            }
            if extraction.entries.count > 12 {
                Text(L10n.format("+%d more entries", extraction.entries.count - 12))
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            ForEach(extraction.warnings.prefix(4), id: \.self) { warning in
                Text("warning: \(warning)")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(2)
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 9)
        .background(VPTheme.success.opacity(0.05))
    }

    private func selectSafeFiles(_ listing: ArtifactArchiveListResult) {
        selection = Set(
            listing.entries
                .filter { $0.safe && $0.kind == "file" }
                .prefix(listing.limits.maxEntries)
                .map(\.path)
        )
    }

    private func load() async {
        loading = true
        defer { loading = false }
        do {
            listing = try await model.artifactArchiveList(artifactID: artifact.id)
            selection.removeAll()
            extraction = nil
            errorMessage = nil
            errorCode = nil
            errorHint = nil
        } catch {
            let typed = typedErrorPresentation(error)
            errorMessage = typed.message
            errorCode = typed.code
            errorHint = typed.hint
        }
    }

    private func extract(includePreviews: Bool) async {
        guard !selection.isEmpty else { return }
        extracting = true
        defer { extracting = false }
        do {
            extraction = try await model.artifactArchiveExtract(
                artifactID: artifact.id,
                entries: Array(selection).sorted(),
                all: false,
                includePreviews: includePreviews
            )
            errorMessage = nil
            errorCode = nil
            errorHint = nil
            model.showToast(
                L10n.t("Archive entries extracted"),
                detail: L10n.format(
                    "%d published artifact(s)",
                    extraction?.publishedArtifacts.count ?? 0
                ),
                kind: .success
            )
        } catch {
            let typed = typedErrorPresentation(error)
            errorMessage = typed.message
            errorCode = typed.code
            errorHint = typed.hint
            model.showToast(
                L10n.t("Archive extraction failed"),
                detail: typed.message,
                kind: .error
            )
        }
    }
}

private struct ArchiveEntryRow: View {
    let entry: ArchiveEntry
    let isSelected: Bool
    let toggle: (Bool) -> Void

    var body: some View {
        HStack(spacing: 8) {
            Toggle(
                isOn: Binding(
                    get: { isSelected },
                    set: { toggle($0) }
                )
            ) {
                EmptyView()
            }
            .toggleStyle(.checkbox)
            .labelsHidden()
            .disabled(!entry.safe)
            Image(systemName: icon)
                .font(.system(size: 9))
                .foregroundStyle(entry.safe ? VPTheme.textSecondary : VPTheme.warning)
                .frame(width: 13)
            Text(entry.path)
                .font(VPTheme.mono(8))
                .foregroundStyle(entry.safe ? VPTheme.textPrimary : VPTheme.warning)
                .lineLimit(1)
                .truncationMode(.middle)
                .help(entry.path)
            Spacer(minLength: 6)
            if let reason = entry.unsafeReason {
                Text(reason)
                    .font(VPTheme.mono(6.8))
                    .foregroundStyle(VPTheme.warning)
                    .padding(.horizontal, 4)
                    .padding(.vertical, 1)
                    .background(VPTheme.warning.opacity(0.08), in: Capsule())
            }
            Text(entry.kind)
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textTertiary)
            Text(archiveByteCount(entry.sizeBytes))
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textTertiary)
                .frame(width: 64, alignment: .trailing)
            if let compressed = entry.compressedSize {
                Text("\(compressed)")
                    .font(VPTheme.mono(6.8))
                    .foregroundStyle(VPTheme.textTertiary.opacity(0.8))
                    .frame(width: 54, alignment: .trailing)
            } else {
                Text("—")
                    .font(VPTheme.mono(6.8))
                    .foregroundStyle(VPTheme.textTertiary.opacity(0.6))
                    .frame(width: 54, alignment: .trailing)
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 5)
        .contentShape(Rectangle())
        .onTapGesture {
            guard entry.safe else { return }
            toggle(!isSelected)
        }
    }

    private var icon: String {
        switch entry.kind {
        case "directory": "folder"
        case "symlink": "link"
        case "hardlink": "link.badge.plus"
        case "device": "bolt.horizontal"
        case "fifo": "arrow.left.arrow.right"
        default: "doc"
        }
    }
}

// MARK: - Media card

/// Explicit media metadata and transcode surface. The card loads metadata only
/// when opened, shows the real probe source, and requires a confirmation alert
/// before running ffmpeg.
struct ArtifactMediaInfoCard: View {
    @Environment(AppModel.self) private var model
    let artifact: Artifact

    @State private var info: MediaInfoResult?
    @State private var loading = false
    @State private var errorMessage: String?
    @State private var errorCode: String?
    @State private var errorHint: String?
    @State private var selectedFormat = "mp4"
    @State private var duration = 60
    @State private var confirming = false
    @State private var transcoding = false
    @State private var transcodeResult: MediaTranscodeResult?

    private let durationOptions = [15, 30, 60, 120, 300, 600]

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            header
            if let errorMessage {
                TypedErrorBanner(
                    message: errorMessage,
                    code: errorCode,
                    hint: errorHint,
                    retry: { Task { await load() } }
                )
            }
            if let info {
                metadata(info)
            } else if loading {
                HStack(spacing: 6) {
                    ProgressView().controlSize(.mini)
                    Text(L10n.t("Probing media metadata…"))
                        .font(VPTheme.mono(7.5))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
            controls
            if let transcodeResult {
                transcodeFooter(transcodeResult)
            }
        }
        .padding(10)
        .background(.white.opacity(0.025), in: RoundedRectangle(cornerRadius: 9))
        .overlay(
            RoundedRectangle(cornerRadius: 9)
                .stroke(.white.opacity(0.06), lineWidth: 1)
        )
        .task(id: artifact.id) {
            selectedFormat = defaultFormat
            await load()
        }
        .alert(
            L10n.t("Transcode this media artifact?"),
            isPresented: $confirming
        ) {
            Button(L10n.t("Cancel"), role: .cancel) {}
            Button(L10n.t("Transcode")) {
                Task { await transcode() }
            }
        } message: {
            Text(
                L10n.format(
                    "Runs the local ffmpeg binary and writes a new content-addressed artifact. Output: %@, duration cap %d seconds, byte cap enforced by the artifact store. No workspace file is written and no network request is made.",
                    selectedFormat,
                    duration
                )
            )
        }
    }

    private var header: some View {
        HStack(spacing: 6) {
            Image(systemName: "waveform.badge.magnifyingglass")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(VPTheme.plan)
            Text(L10n.t("Media metadata"))
                .font(VPTheme.mono(8, weight: .semibold))
                .foregroundStyle(VPTheme.textSecondary)
            if let info {
                Text(info.info.source)
                    .font(VPTheme.mono(6.8, weight: .semibold))
                    .foregroundStyle(
                        info.info.source == "ffprobe" ? VPTheme.accent : VPTheme.warning
                    )
                    .padding(.horizontal, 4)
                    .padding(.vertical, 1)
                    .background(
                        (info.info.source == "ffprobe" ? VPTheme.accent : VPTheme.warning)
                            .opacity(0.09),
                        in: Capsule()
                    )
            }
            Spacer()
            if loading {
                ProgressView().controlSize(.mini)
            }
            Button {
                Task { await load() }
            } label: {
                Image(systemName: "arrow.clockwise")
                    .font(.system(size: 9))
            }
            .buttonStyle(.plain)
            .foregroundStyle(VPTheme.textTertiary)
            .help(L10n.t("Re-probe metadata"))
            .disabled(loading || transcoding)
        }
    }

    private func metadata(_ result: MediaInfoResult) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 8) {
                metadataPill(result.info.container ?? L10n.t("Unknown container"))
                metadataPill(archiveDuration(result.info.durationSecs))
                if let width = result.info.width, let height = result.info.height {
                    metadataPill("\(width) × \(height)")
                }
                if let bitRate = result.info.bitRate, bitRate > 0 {
                    metadataPill("\(bitRate / 1000) kb/s")
                }
                Spacer()
                Text(archiveByteCount(result.sizeBytes))
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            ForEach(result.info.streams.prefix(6), id: \.index) { stream in
                HStack(spacing: 5) {
                    Image(systemName: stream.codecType == "audio" ? "waveform" : "film")
                        .font(.system(size: 8))
                        .foregroundStyle(VPTheme.textTertiary)
                        .frame(width: 12)
                    Text("\(stream.codecType) · \(stream.codecName)")
                        .font(VPTheme.mono(7.5))
                        .foregroundStyle(VPTheme.textSecondary)
                    if let width = stream.width, let height = stream.height {
                        Text("\(width)×\(height)")
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                    if let sampleRate = stream.sampleRate {
                        Text("\(sampleRate) Hz")
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                    if let channels = stream.channels {
                        Text("\(channels) ch")
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                    if let language = stream.language, !language.isEmpty {
                        Text(language)
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                    Spacer()
                    Text(archiveDuration(stream.durationSecs))
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
            ForEach(result.info.warnings.prefix(3), id: \.self) { warning in
                Text("warning: \(warning)")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.warning)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private func metadataPill(_ text: String) -> some View {
        Text(text)
            .font(VPTheme.mono(7, weight: .semibold))
            .foregroundStyle(VPTheme.textSecondary)
            .padding(.horizontal, 5)
            .padding(.vertical, 2)
            .background(.white.opacity(0.05), in: Capsule())
    }

    private var controls: some View {
        HStack(spacing: 7) {
            Text(L10n.t("Transcode"))
                .font(VPTheme.mono(7.5, weight: .semibold))
                .foregroundStyle(VPTheme.textTertiary)
            Picker("", selection: $selectedFormat) {
                ForEach(formats, id: \.self) { format in
                    Text(format).tag(format)
                }
            }
            .labelsHidden()
            .pickerStyle(.menu)
            .controlSize(.small)
            .frame(width: 80)
            Picker("", selection: $duration) {
                ForEach(durationOptions, id: \.self) { seconds in
                    Text("\(seconds)s").tag(seconds)
                }
            }
            .labelsHidden()
            .pickerStyle(.menu)
            .controlSize(.small)
            .frame(width: 68)
            Spacer()
            Button {
                confirming = true
            } label: {
                if transcoding {
                    ProgressView().controlSize(.mini)
                } else {
                    Label(L10n.t("Transcode…"), systemImage: "wand.and.stars")
                        .font(VPTheme.mono(8))
                }
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(transcoding || loading || info == nil)
        }
    }

    private func transcodeFooter(_ result: MediaTranscodeResult) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 5) {
                Image(systemName: "checkmark.circle")
                    .font(.system(size: 9))
                    .foregroundStyle(VPTheme.success)
                Text(
                    L10n.format(
                        "Created %@ · %@ · %@",
                        result.artifact.id,
                        archiveByteCount(result.sizeBytes),
                        archiveDuration(result.durationSecs)
                    )
                )
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textSecondary)
                .lineLimit(1)
                Spacer()
                Text("\(result.format) · \(result.elapsedMs) ms")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            if let provenance = result.artifact.provenance {
                Text(
                    "source \(provenance.sourceArtifactId ?? "—") · output \(provenance.outputFormat ?? result.format) · \(provenance.tool ?? "ffmpeg")"
                )
                .font(VPTheme.mono(6.8))
                .foregroundStyle(VPTheme.textTertiary)
                .lineLimit(1)
            }
            ForEach(result.warnings.prefix(3), id: \.self) { warning in
                Text("warning: \(warning)")
                    .font(VPTheme.mono(6.8))
                    .foregroundStyle(VPTheme.warning)
                    .lineLimit(2)
            }
        }
        .padding(.top, 2)
    }

    private var formats: [String] {
        let mime = ArtifactPreviewClassifier.normalizedMime(artifact.mimeType)
        if mime.hasPrefix("image/") {
            return ["png", "jpeg"]
        }
        if mime.hasPrefix("audio/") {
            return ["mp3", "wav", "mp4"]
        }
        return ["mp4", "webm", "mp3", "wav", "png", "jpeg"]
    }

    private var defaultFormat: String {
        let mime = ArtifactPreviewClassifier.normalizedMime(artifact.mimeType)
        if mime.hasPrefix("image/") {
            return "png"
        }
        if mime.hasPrefix("audio/") {
            return "mp3"
        }
        return "mp4"
    }

    private func load() async {
        loading = true
        defer { loading = false }
        do {
            info = try await model.mediaInfo(artifactID: artifact.id)
            errorMessage = nil
            errorCode = nil
            errorHint = nil
        } catch {
            let typed = typedErrorPresentation(error)
            errorMessage = typed.message
            errorCode = typed.code
            errorHint = typed.hint
        }
    }

    private func transcode() async {
        transcoding = true
        defer { transcoding = false }
        do {
            let result = try await model.mediaTranscode(
                artifactID: artifact.id,
                format: selectedFormat,
                maxDurationSecs: duration
            )
            transcodeResult = result
            errorMessage = nil
            errorCode = nil
            errorHint = nil
            model.showToast(
                L10n.t("Media transcoded"),
                detail: result.artifact.id,
                kind: .success
            )
        } catch {
            let typed = typedErrorPresentation(error)
            errorMessage = typed.message
            errorCode = typed.code
            errorHint = typed.hint
            model.showToast(
                L10n.t("Transcode failed"),
                detail: typed.message,
                kind: .error
            )
        }
    }
}
