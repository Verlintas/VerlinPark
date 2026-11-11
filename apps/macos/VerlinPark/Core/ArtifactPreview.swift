import Foundation

// MARK: - Classification

/// The preview surface a published artifact can use. Classification is purely
/// descriptive: it never opens, executes, or transforms the artifact.
enum ArtifactPreviewKind: String, CaseIterable, Sendable {
    case image
    case pdf
    case video
    case audio
    case text
    case binary
    case archive
    case other
}

/// Deterministic classification from daemon-provided MIME type and the stored
/// display name. The optional sample is used only for a UTF-8 fallback when the
/// MIME/extension pair is unknown; it is never executed.
enum ArtifactPreviewClassifier {
    static func classify(
        mimeType: String,
        fileName: String,
        contentSample: Data? = nil
    ) -> ArtifactPreviewKind {
        let mime = normalizedMime(mimeType)
        let fileExtension = (fileName as NSString).pathExtension.lowercased()

        if archiveMimeTypes.contains(mime) || archiveExtensions.contains(fileExtension) {
            return .archive
        }
        if mime == "application/pdf" || fileExtension == "pdf" {
            return .pdf
        }
        if mime.hasPrefix("image/") || imageExtensions.contains(fileExtension) {
            return .image
        }
        if mime.hasPrefix("video/") || videoExtensions.contains(fileExtension) {
            return .video
        }
        if mime.hasPrefix("audio/") || audioExtensions.contains(fileExtension) {
            return .audio
        }
        if isTextLike(mime) || textExtensions.contains(fileExtension) {
            return .text
        }
        if binaryMimeTypes.contains(mime) || binaryExtensions.contains(fileExtension) {
            return .binary
        }

        if let contentSample, !contentSample.isEmpty {
            if String(data: contentSample, encoding: .utf8) != nil {
                // Unknown MIME/extension with UTF-8 bytes: show it as text. This
                // is a display decision only; the bytes are never executed.
                return .text
            }
            return .binary
        }

        if mime.isEmpty, fileExtension.isEmpty {
            return .other
        }
        return .binary
    }

    static func normalizedMime(_ mimeType: String) -> String {
        mimeType
            .split(separator: ";", maxSplits: 1)
            .first
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() } ?? ""
    }

    private static func isTextLike(_ mime: String) -> Bool {
        if mime.hasPrefix("text/") {
            return true
        }
        return textLikeMimeTypes.contains(mime)
            || mime.hasSuffix("+json")
            || mime.hasSuffix("+xml")
            || mime.hasSuffix("+yaml")
            || mime.hasSuffix("+toml")
    }

    private static let archiveMimeTypes: Set<String> = [
        "application/zip",
        "application/x-zip-compressed",
        "application/x-tar",
        "application/gzip",
        "application/x-gzip",
        "application/x-bzip2",
        "application/x-xz",
        "application/x-7z-compressed",
        "application/vnd.rar",
        "application/x-rar-compressed",
        "application/zstd",
        "application/x-apple-diskimage",
        "application/java-archive",
    ]

    private static let archiveExtensions: Set<String> = [
        "zip", "tar", "gz", "tgz", "bz2", "xz", "7z", "rar", "zst", "jar", "war", "dmg",
    ]

    private static let imageExtensions: Set<String> = [
        "png", "jpg", "jpeg", "gif", "webp", "heic", "heif", "tif", "tiff", "bmp", "ico", "svg",
        "avif",
    ]

    private static let videoExtensions: Set<String> = [
        "mp4", "m4v", "mov", "webm", "avi", "mkv", "mpg", "mpeg", "ts",
    ]

    private static let audioExtensions: Set<String> = [
        "mp3", "m4a", "aac", "wav", "aiff", "aif", "flac", "ogg", "opus", "caf",
    ]

    private static let textExtensions: Set<String> = [
        "txt", "text", "md", "markdown", "json", "jsonl", "xml", "yaml", "yml",
        "toml", "csv", "tsv", "log", "html", "htm", "css", "js", "mjs", "cjs",
        "ts", "tsx", "jsx", "py", "rs", "go", "java", "kt", "kts", "c", "h",
        "cc", "cpp", "cxx", "hpp", "m", "mm", "swift", "sh", "bash", "zsh",
        "fish", "sql", "ini", "cfg", "conf", "env", "plist", "rtf", "graphql",
        "proto", "diff", "patch",
    ]

    private static let binaryExtensions: Set<String> = [
        "bin", "dat", "exe", "dll", "so", "dylib", "o", "a", "class", "wasm",
        "pkg", "msi", "deb", "rpm", "iso", "img",
    ]

    private static let textLikeMimeTypes: Set<String> = [
        "application/json",
        "application/ld+json",
        "application/xml",
        "application/xhtml+xml",
        "application/yaml",
        "application/x-yaml",
        "application/toml",
        "application/x-toml",
        "application/csv",
        "application/sql",
        "application/javascript",
        "application/x-javascript",
        "application/ecmascript",
        "application/x-sh",
        "application/x-shellscript",
        "application/x-httpd-php",
        "application/graphql",
        "application/rtf",
        "application/x-plist",
        "image/svg+xml",
    ]

    private static let binaryMimeTypes: Set<String> = [
        "application/octet-stream",
        "application/x-mach-binary",
        "application/x-executable",
        "application/x-sharedlib",
        "application/vnd.microsoft.portable-executable",
        "application/wasm",
    ]
}

// MARK: - Inline content

enum ArtifactInlineContentError: Error, Equatable {
    case exceedsLimit(Int)
    case invalidBase64
}

extension ArtifactInlineContentError: LocalizedError {
    var errorDescription: String? {
        switch self {
        case .exceedsLimit:
            L10n.t("Inline data exceeds the 100 MB preview limit.")
        case .invalidBase64:
            L10n.t("Invalid inline base64 data.")
        }
    }
}

/// Inline artifacts are normally plain UTF-8 content. Bundle/import APIs may
/// also carry `data_base64`; both paths are bounded before decoding or paging.
enum ArtifactInlineContent {
    static let maxDecodedBytes = 100 * 1024 * 1024

    static func bytes(
        content: String,
        dataBase64: String?,
        limit: Int = maxDecodedBytes
    ) throws -> Data {
        if let dataBase64, !dataBase64.isEmpty {
            return try decodedBase64(dataBase64, limit: limit)
        }
        let data = Data(content.utf8)
        guard data.count <= limit else {
            throw ArtifactInlineContentError.exceedsLimit(data.count)
        }
        return data
    }

    static func decodedBase64(_ encoded: String, limit: Int = maxDecodedBytes) throws -> Data {
        // Estimate before decoding so a hostile or accidental multi-gigabyte
        // string cannot force a huge allocation just to be rejected afterward.
        let estimatedBytes = (encoded.count / 4) * 3 + 3
        guard estimatedBytes <= limit else {
            throw ArtifactInlineContentError.exceedsLimit(estimatedBytes)
        }
        guard let data = Data(base64Encoded: encoded, options: .ignoreUnknownCharacters) else {
            throw ArtifactInlineContentError.invalidBase64
        }
        guard data.count <= limit else {
            throw ArtifactInlineContentError.exceedsLimit(data.count)
        }
        return data
    }
}

// MARK: - Daemon-supplied file paths

enum ArtifactPreviewError: Error, Equatable {
    case invalidDaemonPath
}

extension ArtifactPreviewError: LocalizedError {
    var errorDescription: String? {
        switch self {
        case .invalidDaemonPath:
            L10n.t("The daemon returned an invalid artifact path.")
        }
    }
}

/// Only daemon-supplied absolute paths are accepted. The UI never builds a
/// filesystem path from an artifact display name.
enum ArtifactLocalFile {
    static func storedURL(for artifact: Artifact) -> URL? {
        guard artifact.storage == "file",
              let path = artifact.path?.trimmingCharacters(in: .whitespacesAndNewlines),
              !path.isEmpty,
              (path as NSString).isAbsolutePath
        else {
            return nil
        }
        return URL(fileURLWithPath: path, isDirectory: false)
    }
}

enum ArtifactFileReader {
    static func readPrefix(_ limit: Int, from url: URL) throws -> Data {
        guard limit > 0 else { return Data() }
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        return try handle.read(upToCount: limit) ?? Data()
    }

    static func readRange(_ range: Range<Int>, from url: URL) throws -> Data {
        guard !range.isEmpty else { return Data() }
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        try handle.seek(toOffset: UInt64(range.lowerBound))
        return try handle.read(upToCount: range.count) ?? Data()
    }
}

// MARK: - Hex paging

enum ArtifactHexSource: Sendable {
    case file(URL)
    case inline(Data)
}

struct ArtifactHexLine: Equatable, Identifiable, Sendable {
    let offset: Int
    let hex: String
    let ascii: String

    var id: Int {
        offset
    }

    /// Offset, hex bytes, and the printable ASCII column. The hex column is
    /// padded so a short final line still aligns its `|...|` ASCII column.
    var text: String {
        String(format: "%08x  ", offset)
            + hex.padding(toLength: 47, withPad: " ", startingAt: 0)
            + "  |" + ascii + "|"
    }
}

enum ArtifactHexFormatter {
    static let bytesPerLine = 16

    static func lines(
        for data: Data,
        startingAt offset: Int = 0,
        bytesPerLine: Int = bytesPerLine
    ) -> [ArtifactHexLine] {
        let bytes = [UInt8](data)
        let strideBy = max(1, bytesPerLine)
        return stride(from: 0, to: bytes.count, by: strideBy).map { start in
            let end = min(start + strideBy, bytes.count)
            let chunk = bytes[start ..< end]
            let hex = chunk.map { String(format: "%02x", $0) }.joined(separator: " ")
            let ascii = chunk.map { byte in
                (32 ... 126).contains(byte) ? String(UnicodeScalar(byte)) : "."
            }.joined()
            return ArtifactHexLine(offset: offset + start, hex: hex, ascii: ascii)
        }
    }

    static func text(for data: Data, startingAt offset: Int = 0) -> String {
        lines(for: data, startingAt: offset)
            .map(\.text)
            .joined(separator: "\n")
    }
}

enum ArtifactHexPager {
    static let pageSize = 64 * 1024

    static func pageCount(totalBytes: Int, pageSize: Int = pageSize) -> Int {
        let strideBy = max(1, pageSize)
        guard totalBytes > 0 else { return 1 }
        return (totalBytes + strideBy - 1) / strideBy
    }

    /// Clamps a requested page into the valid range and returns the byte range
    /// that should be read. A zero-byte artifact has exactly one empty page.
    static func pageRange(
        page: Int,
        totalBytes: Int,
        pageSize: Int = pageSize
    ) -> Range<Int> {
        let strideBy = max(1, pageSize)
        guard totalBytes > 0 else { return 0 ..< 0 }
        let lastPage = pageCount(totalBytes: totalBytes, pageSize: strideBy) - 1
        let clampedPage = min(max(0, page), lastPage)
        let start = clampedPage * strideBy
        return start ..< min(start + strideBy, totalBytes)
    }

    static func read(source: ArtifactHexSource, range: Range<Int>) throws -> Data {
        switch source {
        case let .file(url):
            return try ArtifactFileReader.readRange(range, from: url)
        case let .inline(data):
            let lower = min(max(0, range.lowerBound), data.count)
            let upper = min(max(lower, range.upperBound), data.count)
            return data.subdata(in: lower ..< upper)
        }
    }
}

// MARK: - Cache and temporary files

/// Preview-only materialization lives under the app cache directory, never in
/// the daemon data directory or /tmp. `ArtifactTemporaryFile` removes its file
/// when the owning preview view is dismissed or replaced.
enum ArtifactPreviewCache {
    static func directory() throws -> URL {
        let base = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first
            ?? FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Caches", isDirectory: true)
        let directory = base
            .appendingPathComponent("com.verlintas.VerlinPark", isDirectory: true)
            .appendingPathComponent("ArtifactPreviews", isDirectory: true)
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: true
        )
        try? FileManager.default.setAttributes(
            [.posixPermissions: 0o700],
            ofItemAtPath: directory.path
        )
        return directory
    }

    static func writeTemporary(data: Data, mimeType: String) throws -> URL {
        let directory = try directory()
        let name = "preview-\(UUID().uuidString).\(fileExtension(for: mimeType))"
        let url = directory.appendingPathComponent(name, isDirectory: false)
        try data.write(to: url, options: .atomic)
        try? FileManager.default.setAttributes(
            [.posixPermissions: 0o600],
            ofItemAtPath: url.path
        )
        return url
    }

    static func writeCached(data: Data, artifactID: String, mimeType: String) throws -> URL {
        let directory = try directory()
        let safeID = artifactID.filter {
            $0.isLetter || $0.isNumber || $0 == "-" || $0 == "_"
        }
        let base = safeID.isEmpty ? UUID().uuidString : safeID
        let url = directory.appendingPathComponent(
            "artifact-\(base).\(fileExtension(for: mimeType))",
            isDirectory: false
        )
        try data.write(to: url, options: .atomic)
        try? FileManager.default.setAttributes(
            [.posixPermissions: 0o600],
            ofItemAtPath: url.path
        )
        return url
    }

    static func remove(_ url: URL) {
        try? FileManager.default.removeItem(at: url)
    }

    /// Best-effort sweep for previews left behind by a previous crash. Explicit
    /// dismiss/deinit cleanup remains the primary lifecycle.
    static func sweepStaleFiles(olderThan interval: TimeInterval = 24 * 60 * 60) {
        guard let directory = try? directory(),
              let urls = try? FileManager.default.contentsOfDirectory(
                  at: directory,
                  includingPropertiesForKeys: [.contentModificationDateKey],
                  options: [.skipsHiddenFiles]
              )
        else {
            return
        }
        let cutoff = Date().addingTimeInterval(-interval)
        for url in urls {
            let modified = (try? url.resourceValues(forKeys: [.contentModificationDateKey]))
                .flatMap(\.contentModificationDate)
            if let modified, modified < cutoff {
                try? FileManager.default.removeItem(at: url)
            }
        }
    }

    static func fileExtension(for mimeType: String) -> String {
        let mime = ArtifactPreviewClassifier.normalizedMime(mimeType)
        switch mime {
        case "image/png": return "png"
        case "image/jpeg": return "jpg"
        case "image/gif": return "gif"
        case "image/webp": return "webp"
        case "image/heic": return "heic"
        case "image/heif": return "heif"
        case "image/tiff": return "tiff"
        case "image/bmp": return "bmp"
        case "image/svg+xml": return "svg"
        case "application/pdf": return "pdf"
        case "application/zip", "application/x-zip-compressed": return "zip"
        case "application/x-tar": return "tar"
        case "application/gzip", "application/x-gzip": return "gz"
        case "application/x-7z-compressed": return "7z"
        case "application/x-rar-compressed", "application/vnd.rar": return "rar"
        case "video/mp4": return "mp4"
        case "video/quicktime": return "mov"
        case "video/webm": return "webm"
        case "audio/mpeg": return "mp3"
        case "audio/mp4", "audio/m4a": return "m4a"
        case "audio/wav", "audio/x-wav": return "wav"
        case "audio/aiff": return "aiff"
        case "audio/flac": return "flac"
        case "audio/ogg": return "ogg"
        case "application/json", "application/ld+json": return "json"
        case "application/xml", "text/xml": return "xml"
        case "text/markdown": return "md"
        case "text/csv": return "csv"
        case "text/html": return "html"
        case "text/plain": return "txt"
        default: return "dat"
        }
    }
}

final class ArtifactTemporaryFile {
    let url: URL

    init(data: Data, mimeType: String) throws {
        url = try ArtifactPreviewCache.writeTemporary(data: data, mimeType: mimeType)
    }

    deinit {
        ArtifactPreviewCache.remove(url)
    }

    func remove() {
        ArtifactPreviewCache.remove(url)
    }
}

enum ArtifactPreviewFileName {
    /// A safe default name for NSSavePanel. This is a display suggestion, not a
    /// path: the panel owns the destination directory.
    static func suggested(for artifact: Artifact) -> String {
        var name = artifact.name
            .replacingOccurrences(of: "/", with: "-")
            .replacingOccurrences(of: ":", with: "-")
            .trimmingCharacters(in: .whitespacesAndNewlines)
        if name.isEmpty || name == "." || name == ".." {
            name = "artifact"
        }
        if (name as NSString).pathExtension.isEmpty {
            let fileExtension = ArtifactPreviewCache.fileExtension(for: artifact.mimeType)
            if fileExtension != "dat" {
                name += ".\(fileExtension)"
            }
        }
        return String(name.prefix(200))
    }
}
