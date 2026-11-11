@testable import VerlinPark
import XCTest

final class ArtifactPreviewTests: XCTestCase {
    // MARK: Classification

    func testClassifiesCommonMimeTypesAndExtensions() {
        XCTAssertEqual(classify("image/png", "shot.png"), .image)
        XCTAssertEqual(classify("application/pdf", "report.pdf"), .pdf)
        XCTAssertEqual(classify("video/mp4", "clip.mp4"), .video)
        XCTAssertEqual(classify("audio/mpeg", "voice.mp3"), .audio)
        XCTAssertEqual(classify("text/plain", "notes.txt"), .text)
        XCTAssertEqual(classify("application/zip", "bundle.zip"), .archive)
        XCTAssertEqual(classify("application/json", "payload"), .text)
        XCTAssertEqual(classify("image/svg+xml", "icon.svg"), .image)
    }

    func testExtensionFallbackWorksWhenMimeIsEmpty() {
        XCTAssertEqual(classify("", "README.md"), .text)
        XCTAssertEqual(classify("", "archive.tar.gz"), .archive)
        XCTAssertEqual(classify("", "shot.PNG"), .image)
        XCTAssertEqual(classify("", "movie.mov"), .video)
        XCTAssertEqual(classify("", "sound.flac"), .audio)
    }

    func testMimeParametersAreIgnored() {
        XCTAssertEqual(classify("text/plain; charset=utf-8", "notes"), .text)
        XCTAssertEqual(classify("application/json; charset=UTF-8", "payload"), .text)
    }

    func testUTF8FallbackOnlyAppliesWhenNoKnownTypeWins() {
        let text = Data("hello artifact".utf8)
        XCTAssertEqual(classify("", "unknown", contentSample: text), .text)
        XCTAssertEqual(classify("application/octet-stream", "blob", contentSample: text), .binary)

        let binary = Data([0xFF, 0xFE, 0x00, 0x01])
        XCTAssertEqual(classify("", "unknown", contentSample: binary), .binary)
        XCTAssertEqual(classify("", "", contentSample: nil), .other)
        XCTAssertEqual(classify("application/octet-stream", "blob", contentSample: nil), .binary)
    }

    // MARK: Hex formatting

    func testHexLineContainsOffsetHexAndAsciiColumns() {
        let data = Data("Hello, artifact!".utf8)
        let lines = ArtifactHexFormatter.lines(for: data)
        XCTAssertEqual(lines.count, 1)
        XCTAssertEqual(lines[0].offset, 0)
        XCTAssertEqual(
            lines[0].hex,
            "48 65 6c 6c 6f 2c 20 61 72 74 69 66 61 63 74 21"
        )
        XCTAssertEqual(lines[0].ascii, "Hello, artifact!")
        XCTAssertTrue(lines[0].text.hasPrefix("00000000  "))
        XCTAssertTrue(lines[0].text.hasSuffix("|Hello, artifact!|"))
    }

    func testPartialFinalLineIsPaddedAndKeepsItsOffset() {
        let data = Data([0x41, 0x42, 0x43, 0x44])
        let lines = ArtifactHexFormatter.lines(for: data, startingAt: 16)
        XCTAssertEqual(lines.count, 1)
        XCTAssertEqual(lines[0].offset, 16)
        XCTAssertEqual(lines[0].hex, "41 42 43 44")
        XCTAssertEqual(lines[0].ascii, "ABCD")
        XCTAssertTrue(lines[0].text.hasPrefix("00000010  "))
        XCTAssertTrue(lines[0].text.hasSuffix("|ABCD|"))
        XCTAssertGreaterThan(lines[0].text.count, lines[0].hex.count)
    }

    func testNonPrintableBytesBecomeDots() {
        let lines = ArtifactHexFormatter.lines(
            for: Data([0x00, 0x1F, 0x20, 0x7E, 0x7F])
        )
        XCTAssertEqual(lines.count, 1)
        XCTAssertEqual(lines[0].ascii, ".. ~.")
        XCTAssertTrue(lines[0].text.hasSuffix("|.. ~.|"))
    }

    // MARK: Paging

    func testPageCountHandlesEmptyAndBoundarySizes() {
        XCTAssertEqual(ArtifactHexPager.pageCount(totalBytes: 0), 1)
        XCTAssertEqual(ArtifactHexPager.pageCount(totalBytes: 64 * 1024), 1)
        XCTAssertEqual(ArtifactHexPager.pageCount(totalBytes: 64 * 1024 + 1), 2)
        XCTAssertEqual(ArtifactHexPager.pageCount(totalBytes: -5), 1)
    }

    func testPageRangeClampsAndStopsAtTotalSize() {
        XCTAssertEqual(
            ArtifactHexPager.pageRange(page: 0, totalBytes: 64 * 1024 + 1),
            0 ..< 64 * 1024
        )
        XCTAssertEqual(
            ArtifactHexPager.pageRange(page: 1, totalBytes: 64 * 1024 + 1),
            64 * 1024 ..< 64 * 1024 + 1
        )
        XCTAssertEqual(
            ArtifactHexPager.pageRange(page: -7, totalBytes: 64 * 1024 + 1),
            0 ..< 64 * 1024
        )
        XCTAssertEqual(
            ArtifactHexPager.pageRange(page: 99, totalBytes: 64 * 1024 + 1),
            64 * 1024 ..< 64 * 1024 + 1
        )
        XCTAssertEqual(ArtifactHexPager.pageRange(page: 0, totalBytes: 0), 0 ..< 0)
    }

    func testCustomPageSizeClampsToTheLastPartialPage() {
        XCTAssertEqual(
            ArtifactHexPager.pageRange(page: 1, totalBytes: 10, pageSize: 4),
            4 ..< 8
        )
        XCTAssertEqual(
            ArtifactHexPager.pageRange(page: 2, totalBytes: 10, pageSize: 4),
            8 ..< 10
        )
        XCTAssertEqual(
            ArtifactHexPager.pageRange(page: 99, totalBytes: 10, pageSize: 4),
            8 ..< 10
        )
    }

    // MARK: Inline decoding

    func testInlineBase64Decodes() throws {
        let data = try ArtifactInlineContent.decodedBase64("aGVsbG8=")
        XCTAssertEqual(data, Data("hello".utf8))
    }

    func testInlineContentPrefersBase64OverContent() throws {
        let data = try ArtifactInlineContent.bytes(
            content: "ignored",
            dataBase64: "aGVsbG8="
        )
        XCTAssertEqual(data, Data("hello".utf8))
    }

    func testInlinePlainContentIsUTF8() throws {
        let data = try ArtifactInlineContent.bytes(
            content: "plain artifact",
            dataBase64: nil
        )
        XCTAssertEqual(data, Data("plain artifact".utf8))
    }

    func testInlineDecodeRejectsInvalidBase64() {
        XCTAssertThrowsError(try ArtifactInlineContent.decodedBase64("%%%not-base64%%%")) { error in
            XCTAssertEqual(error as? ArtifactInlineContentError, .invalidBase64)
        }
    }

    func testInlineDecodeRejectsOversizedInputBeforeDecoding() {
        XCTAssertThrowsError(try ArtifactInlineContent.decodedBase64("aGVsbG8=", limit: 4)) {
            error in
            guard case let ArtifactInlineContentError.exceedsLimit(estimated) = error else {
                return XCTFail("expected exceedsLimit, got \(error)")
            }
            XCTAssertGreaterThan(estimated, 4)
        }
        XCTAssertThrowsError(
            try ArtifactInlineContent.bytes(
                content: String(repeating: "a", count: 10),
                dataBase64: nil,
                limit: 5
            )
        ) { error in
            XCTAssertEqual(error as? ArtifactInlineContentError, .exceedsLimit(10))
        }
    }

    // MARK: Daemon path validation

    func testStoredFileURLAcceptsOnlyAbsoluteDaemonPathsForFileStorage() throws {
        let fileArtifact = try decodeArtifact(
            storage: "file",
            path: "/daemon/artifacts/blob"
        )
        XCTAssertEqual(
            ArtifactLocalFile.storedURL(for: fileArtifact)?.path,
            "/daemon/artifacts/blob"
        )

        let relative = try decodeArtifact(storage: "file", path: "relative/blob")
        XCTAssertNil(ArtifactLocalFile.storedURL(for: relative))

        let inline = try decodeArtifact(storage: "inline", path: "/daemon/artifacts/blob")
        XCTAssertNil(ArtifactLocalFile.storedURL(for: inline))
    }

    func testStoredFileURLDoesNotUseTheArtifactDisplayName() throws {
        let artifact = try decodeArtifact(
            storage: "file",
            path: nil,
            name: "../../etc/passwd"
        )
        XCTAssertNil(ArtifactLocalFile.storedURL(for: artifact))
    }

    private func classify(
        _ mime: String,
        _ name: String,
        contentSample: Data? = nil
    ) -> ArtifactPreviewKind {
        ArtifactPreviewClassifier.classify(
            mimeType: mime,
            fileName: name,
            contentSample: contentSample
        )
    }

    private func decodeArtifact(
        storage: String,
        path: String?,
        name: String = "artifact.bin"
    ) throws -> Artifact {
        var values: [String: Any] = [
            "id": "artifact-1",
            "session_id": "session-1",
            "agent_id": "agent-1",
            "agent_name": "Worker",
            "name": name,
            "kind": "file",
            "mime_type": "application/octet-stream",
            "content": "",
            "storage": storage,
            "size_bytes": 0,
            "sha256": "",
            "version": 1,
            "created_at": "2026-11-11T00:00:00Z",
        ]
        values["path"] = path
        let data = try JSONSerialization.data(withJSONObject: values)
        return try JSONDecoder().decode(Artifact.self, from: data)
    }
}
