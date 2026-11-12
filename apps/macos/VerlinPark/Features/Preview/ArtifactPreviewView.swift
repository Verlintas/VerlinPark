import AppKit
import AVFoundation
import AVKit
import PDFKit
import QuickLook
import SwiftUI

// MARK: - Shared presentation helpers

extension ArtifactPreviewKind {
    var title: String {
        switch self {
        case .image: L10n.t("Image")
        case .pdf: L10n.t("PDF")
        case .video: L10n.t("Video")
        case .audio: L10n.t("Audio")
        case .text: L10n.t("Text")
        case .binary: L10n.t("Binary")
        case .archive: L10n.t("Archive")
        case .other: L10n.t("Other")
        }
    }

    var icon: String {
        switch self {
        case .image: "photo"
        case .pdf: "doc.richtext"
        case .video: "film"
        case .audio: "waveform"
        case .text: "doc.text"
        case .binary: "number"
        case .archive: "archivebox"
        case .other: "doc"
        }
    }

    var tint: Color {
        switch self {
        case .image: VPTheme.info
        case .pdf: VPTheme.danger
        case .video: VPTheme.plan
        case .audio: VPTheme.resident
        case .text: VPTheme.accent
        case .binary: VPTheme.warning
        case .archive: VPTheme.textSecondary
        case .other: VPTheme.textTertiary
        }
    }
}

private func artifactByteCount(_ bytes: Int) -> String {
    guard bytes > 0 else { return "0 B" }
    return ByteCountFormatter.string(fromByteCount: Int64(bytes), countStyle: .file)
}

private func artifactDimensions(_ image: NSImage) -> String? {
    if let representation = image.representations.first,
       representation.pixelsWide > 0,
       representation.pixelsHigh > 0
    {
        return "\(representation.pixelsWide) × \(representation.pixelsHigh) px"
    }
    guard image.size.width > 0, image.size.height > 0 else { return nil }
    return "\(Int(image.size.width)) × \(Int(image.size.height)) pt"
}

private func artifactDuration(_ seconds: Double) -> String {
    guard seconds.isFinite, seconds >= 0 else { return "—" }
    let total = Int(seconds.rounded())
    let hours = total / 3600
    let minutes = (total % 3600) / 60
    let remainder = total % 60
    if hours > 0 {
        return String(format: "%d:%02d:%02d", hours, minutes, remainder)
    }
    return String(format: "%d:%02d", minutes, remainder)
}

// MARK: - Artifact row

struct ArtifactRow: View {
    @Environment(AppModel.self) private var model
    let artifact: Artifact
    @State private var preview: ArtifactPreviewData?
    @State private var showingVersions = false
    @State private var showingPreview = false
    @State private var quickLookURL: URL?
    @State private var temporaryFile: ArtifactTemporaryFile?
    @State private var wrapText = true

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            header
            if let preview {
                previewBody(preview)
            } else {
                HStack(spacing: 6) {
                    ProgressView().controlSize(.mini)
                    Text(L10n.t("Loading preview…"))
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
            footer
        }
        .padding(9)
        .background(.white.opacity(0.03), in: RoundedRectangle(cornerRadius: 9))
        .vpHoverLift(scale: 1.005, lift: 1)
        .task(id: artifact.id) {
            preview = await model.artifactPreviewData(artifact)
        }
        .sheet(isPresented: $showingVersions) {
            ArtifactVersionsSheet(artifact: artifact)
                .environment(model)
        }
        .sheet(isPresented: $showingPreview) {
            ArtifactPreviewSheet(artifact: artifact, initialPreview: preview)
                .environment(model)
        }
        .quickLookPreview($quickLookURL)
        .onChange(of: quickLookURL) { _, value in
            if value == nil {
                temporaryFile = nil
            }
        }
        .onDisappear {
            quickLookURL = nil
            temporaryFile = nil
        }
    }

    private var header: some View {
        HStack(spacing: 6) {
            if let kind = preview?.kind {
                Image(systemName: kind.icon)
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(kind.tint)
            }
            Text(artifact.name)
                .font(.system(size: 10, weight: .medium))
                .lineLimit(1)
            Text(artifact.kind)
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.secondary)
            Text(artifact.storage)
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
            if let compression = artifact.compression, compression != "none" {
                Text(compression)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
            Text("v\(artifact.version)")
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(VPTheme.textTertiary)
            if artifact.storage == "inline" {
                Button {
                    showingVersions = true
                } label: {
                    Image(systemName: "clock.arrow.circlepath")
                        .font(.system(size: 9))
                }
                .buttonStyle(.plain)
                .foregroundStyle(VPTheme.textTertiary)
                .help(L10n.t("Version history"))
            }
            Spacer()
            Text(artifact.agentName)
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(.tertiary)
                .lineLimit(1)
        }
    }

    @ViewBuilder
    private func previewBody(_ preview: ArtifactPreviewData) -> some View {
        if let error = preview.error {
            Label(error, systemImage: "exclamationmark.triangle")
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.warning)
                .fixedSize(horizontal: false, vertical: true)
        } else {
            switch preview.kind {
            case .image:
                if let image = preview.image {
                    Button {
                        showingPreview = true
                    } label: {
                        Image(nsImage: image)
                            .resizable()
                            .scaledToFit()
                            .frame(maxHeight: 180)
                            .clipShape(RoundedRectangle(cornerRadius: 7))
                    }
                    .buttonStyle(.plain)
                    if let dimensions = artifactDimensions(image) {
                        Text(dimensions)
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                }
            case .text:
                if let text = preview.text {
                    HStack(spacing: 6) {
                        Text(L10n.t("Text"))
                            .font(VPTheme.mono(7, weight: .semibold))
                            .foregroundStyle(VPTheme.textTertiary)
                        Spacer()
                        Button(wrapText ? L10n.t("No wrap") : L10n.t("Wrap")) {
                            wrapText.toggle()
                        }
                        .buttonStyle(.plain)
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                    }
                    ArtifactTextPreview(text: text, wrap: wrapText, maxHeight: 180)
                    if preview.truncated {
                        Text(L10n
                            .t("Preview truncated at 64 KB. Save or open the artifact for the full file."))
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                }
            case .binary:
                ArtifactHexPreview(
                    source: preview.hexSource,
                    totalBytes: preview.totalBytes,
                    artifactID: "\(artifact.id)-v\(artifact.version)",
                    compact: true
                )
            case .pdf, .video, .audio:
                mediaSummary(preview)
            case .archive:
                archiveSummary(preview)
            case .other:
                unsupportedSummary(preview)
            }
        }
    }

    private func archiveSummary(_: ArtifactPreviewData) -> some View {
        HStack(spacing: 8) {
            Image(systemName: "archivebox")
                .font(.system(size: 13))
                .foregroundStyle(VPTheme.textSecondary)
            VStack(alignment: .leading, spacing: 2) {
                Text(L10n.t("Browse archive"))
                    .font(VPTheme.font(10, weight: .medium))
                    .foregroundStyle(VPTheme.textPrimary)
                Text(L10n.t("List entries, inspect safe flags, and extract selected files into artifacts."))
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(2)
            }
            Spacer()
            Button(L10n.t("Browse")) {
                showingPreview = true
            }
            .buttonStyle(.plain)
            .font(VPTheme.mono(8))
            .foregroundStyle(VPTheme.accent)
        }
        .padding(7)
        .background(.white.opacity(0.025), in: RoundedRectangle(cornerRadius: 8))
    }

    private func mediaSummary(_ preview: ArtifactPreviewData) -> some View {
        HStack(spacing: 8) {
            ZStack {
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .fill(preview.kind.tint.opacity(0.10))
                    .frame(width: 30, height: 30)
                Image(systemName: preview.kind.icon)
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(preview.kind.tint)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(preview.kind.title)
                    .font(VPTheme.font(10, weight: .medium))
                    .foregroundStyle(VPTheme.textPrimary)
                Text(
                    "\(artifactByteCount(preview.totalBytes)) · \(L10n.t("Local file"))"
                )
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textTertiary)
            }
            Spacer()
            Button(L10n.t("Preview")) {
                showingPreview = true
            }
            .buttonStyle(.plain)
            .font(VPTheme.mono(8))
            .foregroundStyle(VPTheme.accent)
        }
        .padding(7)
        .background(.white.opacity(0.025), in: RoundedRectangle(cornerRadius: 8))
    }

    private func unsupportedSummary(_ preview: ArtifactPreviewData) -> some View {
        HStack(spacing: 8) {
            Image(systemName: preview.kind.icon)
                .font(.system(size: 13))
                .foregroundStyle(VPTheme.textTertiary)
            Text(
                preview.kind == .archive
                    ? L10n.t("Archive extraction is not available in this build.")
                    : L10n.t("No inline preview for this artifact type.")
            )
            .font(VPTheme.mono(8))
            .foregroundStyle(VPTheme.textSecondary)
            Spacer()
            Button(L10n.t("Preview")) {
                showingPreview = true
            }
            .buttonStyle(.plain)
            .font(VPTheme.mono(8))
            .foregroundStyle(VPTheme.accent)
        }
        .padding(7)
        .background(.white.opacity(0.025), in: RoundedRectangle(cornerRadius: 8))
    }

    private var footer: some View {
        HStack(spacing: 7) {
            Text(pathLabel)
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textTertiary)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: 4)
            Button {
                showingPreview = true
            } label: {
                Image(systemName: "eye")
            }
            .buttonStyle(VPGlassIconButtonStyle(size: 21, tint: VPTheme.textSecondary))
            .help(L10n.t("Preview artifact"))
            Button {
                presentQuickLook()
            } label: {
                Image(systemName: "eye.circle")
            }
            .buttonStyle(VPGlassIconButtonStyle(size: 21, tint: VPTheme.textSecondary))
            .help(L10n.t("Quick Look"))
            Menu {
                artifactActions
            } label: {
                Image(systemName: "ellipsis.circle")
                    .font(.system(size: 10, weight: .semibold))
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .help(L10n.t("Artifact actions"))
        }
    }

    private var pathLabel: String {
        if let source = preview?.sourceURL {
            return source.path
        }
        if let path = ArtifactLocalFile.storedURL(for: artifact)?.path {
            return path
        }
        return L10n.t("Inline artifact")
    }

    @ViewBuilder
    private var artifactActions: some View {
        Button {
            saveAs()
        } label: {
            Label(L10n.t("Save As…"), systemImage: "square.and.arrow.down")
        }
        Button {
            copySHA()
        } label: {
            Label(L10n.t("Copy SHA-256"), systemImage: "doc.on.doc")
        }
        Button {
            reveal()
        } label: {
            Label(L10n.t("Reveal in Finder"), systemImage: "folder")
        }
        Button {
            open()
        } label: {
            Label(L10n.t("Open"), systemImage: "arrow.up.forward.app")
        }
        if artifact.storage == "inline" {
            Divider()
            Button {
                showingVersions = true
            } label: {
                Label(L10n.t("Version history"), systemImage: "clock.arrow.circlepath")
            }
        }
    }

    private func ensureLocalFile() -> URL? {
        if let temporaryFile {
            return temporaryFile.url
        }
        if let source = preview?.sourceURL {
            if source.pathExtension.isEmpty,
               let data = try? Data(contentsOf: source)
            {
                do {
                    let file = try ArtifactTemporaryFile(
                        data: data,
                        mimeType: artifact.mimeType
                    )
                    temporaryFile = file
                    return file.url
                } catch {
                    model.showToast(
                        L10n.t("Preview unavailable."),
                        detail: error.localizedDescription,
                        kind: .warning
                    )
                }
            }
            return source
        }
        guard let data = preview?.inlineData else { return nil }
        do {
            let file = try ArtifactTemporaryFile(data: data, mimeType: artifact.mimeType)
            temporaryFile = file
            return file.url
        } catch {
            model.showToast(
                L10n.t("Preview unavailable."),
                detail: error.localizedDescription,
                kind: .warning
            )
            return nil
        }
    }

    private func presentQuickLook() {
        guard let url = ensureLocalFile() else { return }
        quickLookURL = url
    }

    private func open() {
        guard let url = ensureLocalFile() else { return }
        NSWorkspace.shared.open(url)
    }

    private func reveal() {
        guard let url = ensureLocalFile() else { return }
        NSWorkspace.shared.activateFileViewerSelecting([url])
    }

    private func saveAs() {
        guard let url = ensureLocalFile() else { return }
        let panel = NSSavePanel()
        panel.canCreateDirectories = true
        panel.nameFieldStringValue = ArtifactPreviewFileName.suggested(for: artifact)
        panel.title = L10n.t("Save Artifact")
        panel.prompt = L10n.t("Save")
        guard panel.runModal() == .OK, let destination = panel.url else { return }
        do {
            try Data(contentsOf: url).write(to: destination, options: .atomic)
            model.showToast(
                L10n.t("Artifact saved"),
                detail: destination.lastPathComponent,
                kind: .success
            )
        } catch {
            model.showToast(
                L10n.t("Save failed"),
                detail: error.localizedDescription,
                kind: .error
            )
        }
    }

    private func copySHA() {
        guard !artifact.sha256.isEmpty else { return }
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(artifact.sha256, forType: .string)
        model.showToast(
            L10n.t("SHA-256 copied"),
            detail: artifact.sha256,
            kind: .success
        )
    }
}

// MARK: - Full preview sheet

struct ArtifactPreviewSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let artifact: Artifact
    @State private var preview: ArtifactPreviewData?
    @State private var showingVersions = false
    @State private var wrap = true
    @State private var temporaryFile: ArtifactTemporaryFile?
    @State private var quickLookURL: URL?
    @State private var localFileError: String?

    init(artifact: Artifact, initialPreview: ArtifactPreviewData? = nil) {
        self.artifact = artifact
        _preview = State(initialValue: initialPreview)
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            VPHairline()
            HStack(spacing: 0) {
                content
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .opacity(preview == nil ? 0.72 : 1)
                    .animation(
                        VPMotion.resolved(VPMotion.hover, reduceMotion: reduceMotion),
                        value: preview == nil
                    )
                VPHairline(vertical: true)
                metadata
                    .frame(width: 286)
            }
        }
        .frame(minWidth: 860, idealWidth: 980, minHeight: 600, idealHeight: 760)
        .background { VPCrystalBackdrop() }
        .quickLookPreview($quickLookURL)
        .sheet(isPresented: $showingVersions) {
            ArtifactVersionsSheet(artifact: artifact)
                .environment(model)
        }
        .task(id: artifact.id) {
            let loaded = await model.artifactPreviewData(artifact)
            preview = loaded
            prepareLocalFile(loaded)
        }
        .onChange(of: quickLookURL) { _, value in
            if value == nil {
                temporaryFile = nil
            }
        }
        .onDisappear {
            quickLookURL = nil
            temporaryFile = nil
        }
    }

    private var header: some View {
        HStack(spacing: 10) {
            ZStack {
                RoundedRectangle(cornerRadius: 9, style: .continuous)
                    .fill((preview?.kind.tint ?? VPTheme.accent).opacity(0.12))
                    .frame(width: 34, height: 34)
                Image(systemName: preview?.kind.icon ?? "doc")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(preview?.kind.tint ?? VPTheme.accent)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(artifact.name)
                    .font(VPTheme.font(15, weight: .semibold))
                    .lineLimit(1)
                HStack(spacing: 6) {
                    Text(preview?.kind.title ?? artifact.kind)
                        .font(VPTheme.mono(8, weight: .semibold))
                        .foregroundStyle(preview?.kind.tint ?? VPTheme.textSecondary)
                    Text(artifact.mimeType)
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                    Text("v\(artifact.version)")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
            Spacer()
            if artifact.storage == "inline" {
                Button {
                    showingVersions = true
                } label: {
                    Label(L10n.t("Version history"), systemImage: "clock.arrow.circlepath")
                        .font(VPTheme.mono(8))
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
            }
            Button {
                presentQuickLook()
            } label: {
                Label(L10n.t("Quick Look"), systemImage: "eye.circle")
                    .font(VPTheme.mono(8))
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(preview == nil)
            VPCloseButton(size: 26, help: L10n.t("Close")) { dismiss() }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
    }

    @ViewBuilder
    private var content: some View {
        if let preview {
            if let error = preview.error {
                VStack(spacing: 9) {
                    Image(systemName: "exclamationmark.triangle")
                        .font(.system(size: 24))
                        .foregroundStyle(VPTheme.warning)
                    Text(error)
                        .font(VPTheme.mono(9))
                        .foregroundStyle(VPTheme.textSecondary)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: 360)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .padding(24)
            } else {
                switch preview.kind {
                case .image:
                    if let image = preview.image {
                        VStack(spacing: 8) {
                            ArtifactImagePreview(image: image)
                            HStack(spacing: 8) {
                                if let dimensions = artifactDimensions(image) {
                                    Text(dimensions)
                                        .font(VPTheme.mono(8))
                                        .foregroundStyle(VPTheme.textTertiary)
                                }
                                Spacer()
                                Text(artifactByteCount(preview.totalBytes))
                                    .font(VPTheme.mono(8))
                                    .foregroundStyle(VPTheme.textTertiary)
                            }
                            ArtifactMediaInfoCard(artifact: artifact)
                                .environment(model)
                        }
                        .padding(14)
                    }
                case .pdf:
                    localContent(preview)
                case .video, .audio:
                    VStack(spacing: 8) {
                        localContent(preview)
                        ArtifactMediaInfoCard(artifact: artifact)
                            .environment(model)
                    }
                    .padding(14)
                case .text:
                    textContent(preview)
                case .binary:
                    ArtifactHexPreview(
                        source: preview.hexSource,
                        totalBytes: preview.totalBytes,
                        artifactID: "\(artifact.id)-v\(artifact.version)",
                        compact: false
                    )
                    .padding(14)
                case .archive:
                    ArchiveBrowserView(artifact: artifact)
                        .environment(model)
                case .other:
                    unsupportedContent(preview)
                }
            }
        } else {
            VStack(spacing: 9) {
                ProgressView().controlSize(.small)
                Text(L10n.t("Loading preview…"))
                    .font(VPTheme.mono(9))
                    .foregroundStyle(VPTheme.textSecondary)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    @ViewBuilder
    private func localContent(_ preview: ArtifactPreviewData) -> some View {
        if let localFileError {
            Text(localFileError)
                .font(VPTheme.mono(9))
                .foregroundStyle(VPTheme.warning)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if let url = localURL {
            switch preview.kind {
            case .pdf:
                ArtifactPDFPreview(url: url)
                    .padding(14)
            case .video:
                ArtifactMediaPreview(url: url, isAudio: false)
                    .padding(14)
            case .audio:
                ArtifactMediaPreview(url: url, isAudio: true)
                    .padding(14)
            default:
                EmptyView()
            }
        } else {
            VStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text(L10n.t("Preparing local preview…"))
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private func textContent(_ preview: ArtifactPreviewData) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Text(L10n.t("Text preview"))
                    .font(VPTheme.mono(8, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                if preview.truncated {
                    Text(L10n.t("Truncated at 64 KB"))
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.warning)
                }
                Spacer()
                Toggle(isOn: $wrap) {
                    Text(L10n.t("Wrap"))
                        .font(VPTheme.mono(8))
                }
                .toggleStyle(.switch)
                .controlSize(.mini)
            }
            if let text = preview.text {
                ArtifactTextPreview(text: text, wrap: wrap, maxHeight: nil)
            }
        }
        .padding(14)
    }

    private func unsupportedContent(_ preview: ArtifactPreviewData) -> some View {
        VStack(spacing: 10) {
            Image(systemName: preview.kind.icon)
                .font(.system(size: 30, weight: .light))
                .foregroundStyle(VPTheme.textTertiary)
            Text(preview.kind.title)
                .font(VPTheme.font(13, weight: .semibold))
                .foregroundStyle(VPTheme.textSecondary)
            Text(
                preview.kind == .archive
                    ? L10n.t("Archive browser unavailable.")
                    : L10n.t("This artifact type has no inline preview. Use Quick Look, Open, or Save As.")
            )
            .font(VPTheme.mono(9))
            .foregroundStyle(VPTheme.textTertiary)
            .multilineTextAlignment(.center)
            .frame(maxWidth: 380)
            HStack(spacing: 8) {
                Button(L10n.t("Quick Look")) {
                    presentQuickLook()
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                Button(L10n.t("Open")) {
                    open()
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                Button(L10n.t("Save As…")) {
                    saveAs()
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(24)
    }

    private var metadata: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 13) {
                VPKicker(text: "Artifact metadata", trailing: preview?.kind.title)
                metadataRow("MIME", artifact.mimeType)
                metadataRow("Size", artifactByteCount(preview?.totalBytes ?? artifact.sizeBytes))
                metadataRow("Storage", storageTitle)
                metadataRow("Compression", artifact.compression ?? L10n.t("None"))
                metadataRow("Version", "v\(artifact.version)")
                metadataRow("Created", artifact.createdAt)
                shaRow
                if let path = preview?.sourceURL?.path ?? ArtifactLocalFile.storedURL(for: artifact)?.path {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(L10n.t("Source path"))
                            .font(VPTheme.mono(7, weight: .semibold))
                            .foregroundStyle(VPTheme.textTertiary)
                        Text(path)
                            .font(VPTheme.mono(7.5))
                            .foregroundStyle(VPTheme.textSecondary)
                            .lineLimit(3)
                            .truncationMode(.middle)
                            .textSelection(.enabled)
                    }
                }
                VPHairline()
                VStack(spacing: 6) {
                    actionButton("Quick Look", icon: "eye.circle") { presentQuickLook() }
                    actionButton("Open", icon: "arrow.up.forward.app") { open() }
                    actionButton("Save As…", icon: "square.and.arrow.down") { saveAs() }
                    actionButton("Reveal in Finder", icon: "folder") { reveal() }
                    actionButton("Copy SHA-256", icon: "doc.on.doc") { copySHA() }
                    if artifact.storage == "inline" {
                        actionButton("Version history", icon: "clock.arrow.circlepath") {
                            showingVersions = true
                        }
                    }
                }
                Text(
                    L10n.t(
                        "Previews read only the path reported by the daemon or a bounded inline copy. Artifacts are never executed, and no network request is made."
                    )
                )
                .font(VPTheme.mono(7))
                .foregroundStyle(VPTheme.textTertiary)
                .lineSpacing(2)
                .fixedSize(horizontal: false, vertical: true)
            }
            .padding(16)
        }
    }

    private func metadataRow(_ label: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(L10n.t(label))
                .font(VPTheme.mono(7, weight: .semibold))
                .foregroundStyle(VPTheme.textTertiary)
            Text(value.isEmpty ? "—" : value)
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textSecondary)
                .textSelection(.enabled)
                .lineLimit(3)
                .truncationMode(.middle)
        }
    }

    private var shaRow: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 5) {
                Text(L10n.t("SHA-256"))
                    .font(VPTheme.mono(7, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                Spacer()
                Button {
                    copySHA()
                } label: {
                    Image(systemName: "doc.on.doc")
                        .font(.system(size: 8))
                }
                .buttonStyle(.plain)
                .foregroundStyle(VPTheme.textTertiary)
                .help(L10n.t("Copy SHA-256"))
                .disabled(artifact.sha256.isEmpty)
            }
            Text(artifact.sha256.isEmpty ? "—" : artifact.sha256)
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textSecondary)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private func actionButton(
        _ title: String,
        icon: String,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(spacing: 6) {
                Image(systemName: icon)
                    .font(.system(size: 9, weight: .semibold))
                Text(L10n.t(title))
                    .font(VPTheme.mono(8))
                Spacer()
            }
            .frame(maxWidth: .infinity)
        }
        .buttonStyle(.bordered)
        .controlSize(.small)
    }

    private var storageTitle: String {
        switch artifact.storage {
        case "file": L10n.t("File")
        case "inline": L10n.t("Inline")
        default: artifact.storage
        }
    }

    private var localURL: URL? {
        preview?.sourceURL ?? temporaryFile?.url
    }

    private func prepareLocalFile(_ preview: ArtifactPreviewData) {
        guard preview.sourceURL == nil,
              temporaryFile == nil,
              [.pdf, .video, .audio].contains(preview.kind),
              let data = preview.inlineData
        else {
            return
        }
        do {
            temporaryFile = try ArtifactTemporaryFile(data: data, mimeType: artifact.mimeType)
        } catch {
            localFileError = error.localizedDescription
        }
    }

    private func ensureLocalFile() -> URL? {
        if let temporaryFile {
            return temporaryFile.url
        }
        if let source = preview?.sourceURL {
            if source.pathExtension.isEmpty,
               let data = try? Data(contentsOf: source)
            {
                do {
                    let file = try ArtifactTemporaryFile(
                        data: data,
                        mimeType: artifact.mimeType
                    )
                    temporaryFile = file
                    return file.url
                } catch {
                    localFileError = error.localizedDescription
                }
            }
            return source
        }
        guard let data = preview?.inlineData else { return nil }
        do {
            let file = try ArtifactTemporaryFile(data: data, mimeType: artifact.mimeType)
            temporaryFile = file
            return file.url
        } catch {
            localFileError = error.localizedDescription
            return nil
        }
    }

    private func presentQuickLook() {
        guard let url = ensureLocalFile() else { return }
        quickLookURL = url
    }

    private func open() {
        guard let url = ensureLocalFile() else { return }
        NSWorkspace.shared.open(url)
    }

    private func reveal() {
        guard let url = ensureLocalFile() else { return }
        NSWorkspace.shared.activateFileViewerSelecting([url])
    }

    private func saveAs() {
        guard let url = ensureLocalFile() else { return }
        let panel = NSSavePanel()
        panel.canCreateDirectories = true
        panel.nameFieldStringValue = ArtifactPreviewFileName.suggested(for: artifact)
        panel.title = L10n.t("Save Artifact")
        panel.prompt = L10n.t("Save")
        guard panel.runModal() == .OK, let destination = panel.url else { return }
        do {
            try Data(contentsOf: url).write(to: destination, options: .atomic)
            model.showToast(
                L10n.t("Artifact saved"),
                detail: destination.lastPathComponent,
                kind: .success
            )
        } catch {
            model.showToast(
                L10n.t("Save failed"),
                detail: error.localizedDescription,
                kind: .error
            )
        }
    }

    private func copySHA() {
        guard !artifact.sha256.isEmpty else { return }
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(artifact.sha256, forType: .string)
        model.showToast(
            L10n.t("SHA-256 copied"),
            detail: artifact.sha256,
            kind: .success
        )
    }
}

// MARK: - Text preview

struct ArtifactTextPreview: View {
    let text: String
    let wrap: Bool
    let maxHeight: CGFloat?

    var body: some View {
        ScrollView(wrap ? [.vertical] : [.vertical, .horizontal]) {
            Text(text)
                .font(VPTheme.mono(8.5))
                .foregroundStyle(VPTheme.textSecondary)
                .textSelection(.enabled)
                .lineSpacing(1)
                .frame(maxWidth: wrap ? .infinity : nil, alignment: .leading)
                .fixedSize(horizontal: !wrap, vertical: false)
                .padding(8)
        }
        .frame(maxHeight: maxHeight)
        .background(
            VPTheme.surfaceSunken.opacity(0.45),
            in: RoundedRectangle(cornerRadius: 8, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
    }
}

// MARK: - Image preview

private struct ArtifactImagePreview: View {
    let image: NSImage
    @State private var zoom: CGFloat = 1
    @State private var gestureStartZoom: CGFloat = 1

    var body: some View {
        GeometryReader { proxy in
            let base = fitSize(image.size, in: proxy.size)
            ScrollView([.horizontal, .vertical]) {
                Image(nsImage: image)
                    .resizable()
                    .interpolation(.high)
                    .frame(width: base.width * zoom, height: base.height * zoom)
                    .padding(8)
            }
            .background(
                VPTheme.surfaceSunken.opacity(0.55),
                in: RoundedRectangle(cornerRadius: 10, style: .continuous)
            )
            .overlay {
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .strokeBorder(VPTheme.hairline, lineWidth: 1)
            }
            .overlay(alignment: .bottomTrailing) {
                HStack(spacing: 5) {
                    Button {
                        zoom = clampZoom(zoom / 1.25)
                    } label: {
                        Image(systemName: "minus.magnifyingglass")
                    }
                    Text("\(Int((zoom * 100).rounded()))%")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textSecondary)
                        .frame(minWidth: 38)
                    Button {
                        zoom = clampZoom(zoom * 1.25)
                    } label: {
                        Image(systemName: "plus.magnifyingglass")
                    }
                    Button(L10n.t("Fit")) {
                        zoom = 1
                    }
                    .font(VPTheme.mono(8))
                }
                .buttonStyle(.plain)
                .padding(.horizontal, 8)
                .padding(.vertical, 5)
                .background(VPTheme.surfaceRaised.opacity(0.92), in: Capsule())
                .overlay { Capsule().strokeBorder(VPTheme.hairline, lineWidth: 1) }
                .padding(10)
            }
            .gesture(
                MagnifyGesture()
                    .onChanged { value in
                        zoom = clampZoom(gestureStartZoom * value.magnification)
                    }
                    .onEnded { _ in
                        gestureStartZoom = zoom
                    }
            )
        }
        .frame(minHeight: 320)
    }

    private func fitSize(_ imageSize: CGSize, in container: CGSize) -> CGSize {
        guard imageSize.width > 0, imageSize.height > 0 else {
            return CGSize(width: 1, height: 1)
        }
        let available = CGSize(
            width: max(1, container.width - 32),
            height: max(1, container.height - 32)
        )
        let scale = min(
            available.width / imageSize.width,
            available.height / imageSize.height,
            1
        )
        return CGSize(width: imageSize.width * scale, height: imageSize.height * scale)
    }

    private func clampZoom(_ value: CGFloat) -> CGFloat {
        min(max(value, 0.25), 8)
    }
}

// MARK: - PDF preview

private struct ArtifactPDFPreview: View {
    let url: URL
    @State private var document: PDFDocument?
    @State private var pageIndex = 0
    @State private var error: String?

    var body: some View {
        VStack(spacing: 8) {
            if let document {
                ArtifactPDFView(document: document, pageIndex: $pageIndex)
                    .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
                    .overlay {
                        RoundedRectangle(cornerRadius: 10, style: .continuous)
                            .strokeBorder(VPTheme.hairline, lineWidth: 1)
                    }
                HStack(spacing: 8) {
                    Button(L10n.t("Previous")) {
                        pageIndex = max(0, pageIndex - 1)
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(pageIndex <= 0 || document.pageCount == 0)
                    Text(
                        document.pageCount > 0
                            ? L10n.format(
                                "Page %lld of %lld",
                                pageIndex + 1,
                                document.pageCount
                            )
                            : L10n.t("No pages")
                    )
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textSecondary)
                    .frame(minWidth: 96)
                    Button(L10n.t("Next")) {
                        pageIndex = min(document.pageCount - 1, pageIndex + 1)
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .disabled(pageIndex >= document.pageCount - 1 || document.pageCount == 0)
                    Spacer()
                    Button(L10n.t("Open")) {
                        NSWorkspace.shared.open(url)
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                }
            } else if let error {
                Text(error)
                    .font(VPTheme.mono(9))
                    .foregroundStyle(VPTheme.warning)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ProgressView().controlSize(.small)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .task(id: url) {
            guard let loaded = PDFDocument(url: url) else {
                error = L10n.t("PDF preview unavailable.")
                return
            }
            document = loaded
            pageIndex = 0
            error = nil
        }
    }
}

private struct ArtifactPDFView: NSViewRepresentable {
    let document: PDFDocument
    @Binding var pageIndex: Int

    func makeNSView(context _: Context) -> PDFView {
        let view = PDFView()
        view.autoScales = true
        view.displayMode = .singlePage
        view.displaysPageBreaks = true
        view.backgroundColor = .clear
        view.document = document
        return view
    }

    func updateNSView(_ view: PDFView, context _: Context) {
        if view.document !== document {
            view.document = document
        }
        guard document.pageCount > 0 else { return }
        let clamped = min(max(0, pageIndex), document.pageCount - 1)
        if let page = document.page(at: clamped), view.currentPage !== page {
            view.go(to: page)
        }
    }
}

// MARK: - Video and audio preview

private struct ArtifactMediaPreview: View {
    let url: URL
    let isAudio: Bool
    @State private var player: AVPlayer?
    @State private var duration: Double?

    var body: some View {
        VStack(spacing: 8) {
            if let player {
                VideoPlayer(player: player)
                    .frame(minHeight: 260)
                    .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
                    .overlay {
                        RoundedRectangle(cornerRadius: 10, style: .continuous)
                            .strokeBorder(VPTheme.hairline, lineWidth: 1)
                    }
            } else {
                ProgressView().controlSize(.small)
            }
            HStack(spacing: 8) {
                Label(
                    isAudio ? L10n.t("Audio") : L10n.t("Video"),
                    systemImage: isAudio ? "waveform" : "film"
                )
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textSecondary)
                if let duration {
                    Text(L10n.format("Duration %@", artifactDuration(duration)))
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                Spacer()
                Text(url.lastPathComponent)
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .task(id: url) {
            let asset = AVURLAsset(url: url)
            if let loaded = try? await asset.load(.duration) {
                let seconds = loaded.seconds
                if seconds.isFinite, seconds >= 0 {
                    duration = seconds
                }
            }
            guard !Task.isCancelled else { return }
            player = AVPlayer(url: url)
        }
        .onDisappear {
            player?.pause()
        }
    }
}

// MARK: - Hex viewer

struct ArtifactHexPreview: View {
    let source: ArtifactHexSource?
    let totalBytes: Int
    let artifactID: String
    var compact = false
    @State private var page = 0
    @State private var lines: [ArtifactHexLine] = []
    @State private var loading = false
    @State private var error: String?

    private var pageCount: Int {
        ArtifactHexPager.pageCount(totalBytes: totalBytes)
    }

    private var currentPage: Int {
        min(max(0, page), pageCount - 1)
    }

    private var pageRange: Range<Int> {
        ArtifactHexPager.pageRange(page: currentPage, totalBytes: totalBytes)
    }

    private var displayedLines: [ArtifactHexLine] {
        compact ? Array(lines.prefix(12)) : lines
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 6) {
                Text("HEX")
                    .font(VPTheme.mono(7, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                Text(L10n.t("Binary · 64 KB pages"))
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
                Spacer()
                Text(L10n.format("Bytes %@–%@ of %@", rangeStart, rangeEnd, totalLabel))
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            if loading {
                HStack(spacing: 6) {
                    ProgressView().controlSize(.mini)
                    Text(L10n.t("Reading bytes…"))
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                .frame(maxWidth: .infinity, minHeight: 60)
            } else if let error {
                Text(error)
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.warning)
                    .frame(maxWidth: .infinity, minHeight: 60)
            } else {
                ScrollView([.horizontal, .vertical]) {
                    LazyVStack(alignment: .leading, spacing: 1) {
                        ForEach(displayedLines) { line in
                            Text(line.text)
                                .font(VPTheme.mono(7.5))
                                .foregroundStyle(VPTheme.textSecondary)
                                .textSelection(.enabled)
                                .fixedSize(horizontal: true, vertical: false)
                        }
                    }
                    .padding(6)
                }
                .frame(maxHeight: compact ? 150 : nil)
                .background(
                    VPTheme.surfaceSunken.opacity(0.5),
                    in: RoundedRectangle(cornerRadius: 7, style: .continuous)
                )
                .overlay {
                    RoundedRectangle(cornerRadius: 7, style: .continuous)
                        .strokeBorder(VPTheme.hairline, lineWidth: 1)
                }
            }
            HStack(spacing: 6) {
                Button {
                    page = max(0, currentPage - 1)
                } label: {
                    Image(systemName: "chevron.left")
                }
                .buttonStyle(.plain)
                .font(.system(size: 8, weight: .semibold))
                .foregroundStyle(currentPage > 0 ? VPTheme.textSecondary : VPTheme.textTertiary)
                .disabled(currentPage <= 0)
                Text(L10n.format("Page %lld of %lld", currentPage + 1, pageCount))
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textSecondary)
                Button {
                    page = min(pageCount - 1, currentPage + 1)
                } label: {
                    Image(systemName: "chevron.right")
                }
                .buttonStyle(.plain)
                .font(.system(size: 8, weight: .semibold))
                .foregroundStyle(currentPage < pageCount - 1 ? VPTheme.textSecondary : VPTheme.textTertiary)
                .disabled(currentPage >= pageCount - 1)
                Spacer()
                if totalBytes > 0 {
                    Text(L10n.format("%@ total", artifactByteCount(totalBytes)))
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
        }
        .task(id: "\(artifactID)-\(currentPage)-\(totalBytes)") {
            await loadPage()
        }
    }

    private var rangeStart: String {
        "\(pageRange.lowerBound)"
    }

    private var rangeEnd: String {
        pageRange.isEmpty ? "\(pageRange.lowerBound)" : "\(pageRange.upperBound - 1)"
    }

    private var totalLabel: String {
        "\(totalBytes)"
    }

    private func loadPage() async {
        guard let source else {
            lines = []
            error = L10n.t("No preview source.")
            return
        }
        loading = true
        error = nil
        let range = pageRange
        do {
            let data = try await Task.detached(priority: .userInitiated) {
                try ArtifactHexPager.read(source: source, range: range)
            }.value
            guard !Task.isCancelled else { return }
            lines = ArtifactHexFormatter.lines(for: data, startingAt: range.lowerBound)
            loading = false
        } catch {
            guard !Task.isCancelled else { return }
            lines = []
            self.error = error.localizedDescription
            loading = false
        }
    }
}
