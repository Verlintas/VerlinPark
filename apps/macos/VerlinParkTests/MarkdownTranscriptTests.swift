import Foundation
import SwiftUI
@testable import VerlinPark
import XCTest

final class MarkdownTranscriptTests: XCTestCase {
    func testParsesCommonAssistantBlocks() {
        let source = """
        # Heading

        A **bold** paragraph
        continues here.

        - one
        - two
          - nested

        3. third
        4. fourth

        > quoted
        > second line

        ---

        ```swift
        let value = 1
        ```
        """

        let blocks = MarkdownDocument.parse(source)
        XCTAssertEqual(blocks.count, 7)
        guard case let .heading(level, heading) = blocks[0] else {
            return XCTFail("expected heading")
        }
        XCTAssertEqual(level, 1)
        XCTAssertEqual(heading, "Heading")

        guard case let .paragraph(paragraph) = blocks[1] else {
            return XCTFail("expected paragraph")
        }
        XCTAssertEqual(paragraph, "A **bold** paragraph continues here.")

        guard case let .bulletList(items) = blocks[2] else {
            return XCTFail("expected bullet list")
        }
        XCTAssertEqual(items.map(\.text), ["one", "two", "nested"])
        XCTAssertEqual(items.map(\.depth), [0, 0, 1])

        guard case let .orderedList(start, orderedItems) = blocks[3] else {
            return XCTFail("expected ordered list")
        }
        XCTAssertEqual(start, 3)
        XCTAssertEqual(orderedItems.map(\.text), ["third", "fourth"])

        guard case let .quote(quote) = blocks[4] else {
            return XCTFail("expected quote")
        }
        XCTAssertEqual(quote, "quoted\nsecond line")
        XCTAssertEqual(blocks[5], .rule)

        guard case let .codeBlock(language, code) = blocks[6] else {
            return XCTFail("expected code block")
        }
        XCTAssertEqual(language, "swift")
        XCTAssertEqual(code, "let value = 1")
    }

    func testParsesSimpleTablesAndUnclosedFences() {
        let table = """
        | Name | Value |
        | --- | ---: |
        | alpha | 1 |
        | beta | 2 |
        """
        let tableBlocks = MarkdownDocument.parse(table)
        guard case let .table(headers, rows) = tableBlocks.first else {
            return XCTFail("expected table")
        }
        XCTAssertEqual(headers, ["Name", "Value"])
        XCTAssertEqual(rows, [["alpha", "1"], ["beta", "2"]])

        let unpiped = MarkdownDocument.parse("""
        Name | Value
        --- | ---
        alpha | 1
        """)
        guard case let .table(unpipedHeaders, unpipedRows) = unpiped.first else {
            return XCTFail("expected a table without leading pipes")
        }
        XCTAssertEqual(unpipedHeaders, ["Name", "Value"])
        XCTAssertEqual(unpipedRows, [["alpha", "1"]])

        let unclosed = MarkdownDocument.parse("```python\nprint(1)")
        guard case let .codeBlock(language, code) = unclosed.first else {
            return XCTFail("expected partial code block")
        }
        XCTAssertEqual(language, "python")
        XCTAssertEqual(code, "print(1)")
    }

    func testInlineMarkdownAndCitationLinksAreBounded() throws {
        let citation = SourceCitation(
            key: "c1",
            sourceID: "source-1",
            sourceTitle: "Notes",
            chunkID: "chunk-1",
            chunkNo: 1,
            heading: nil,
            excerpt: "excerpt",
            score: 1,
            origin: "test"
        )
        let attributed = MarkdownInline.attributed(
            "Use **bold**, `code`, and [c1].",
            citations: [citation]
        )
        XCTAssertEqual(String(attributed.characters), "Use bold, code, and [c1].")

        let runs = Array(attributed.runs)
        let bold = try XCTUnwrap(runs.first { String(attributed[$0.range].characters) == "bold" })
        XCTAssertTrue(bold.inlinePresentationIntent?.contains(.stronglyEmphasized) == true)

        let code = try XCTUnwrap(runs.first { String(attributed[$0.range].characters) == "code" })
        XCTAssertTrue(code.inlinePresentationIntent?.contains(.code) == true)
        XCTAssertNotNil(attributed[code.range].font)

        let marker = try XCTUnwrap(runs.first { String(attributed[$0.range].characters) == "[c1]" })
        XCTAssertEqual(marker.link, URL(string: "verlinpark://citation/c1"))

        XCTAssertEqual(
            MarkdownInline.citationMarkdown("[c1](https://example.test)", citations: [citation]),
            "[c1](https://example.test)"
        )

        XCTAssertTrue(try MarkdownLinkPolicy
            .allowsExternalOpen(XCTUnwrap(URL(string: "https://example.test"))))
        XCTAssertTrue(try MarkdownLinkPolicy
            .allowsExternalOpen(XCTUnwrap(URL(string: "http://example.test"))))
        XCTAssertFalse(try MarkdownLinkPolicy
            .allowsExternalOpen(XCTUnwrap(URL(string: "file:///tmp/secret"))))
        XCTAssertFalse(try MarkdownLinkPolicy
            .allowsExternalOpen(XCTUnwrap(URL(string: "javascript:alert(1)"))))
        XCTAssertFalse(try MarkdownLinkPolicy
            .allowsExternalOpen(XCTUnwrap(URL(string: "ftp://example.test"))))
    }

    func testCharacterAndBlockBudgetsAppendOneTruncationMarker() {
        let long = MarkdownDocument.parse(
            String(repeating: "a", count: 40),
            maxCharacters: 12
        )
        guard case let .paragraph(text) = long.first else {
            return XCTFail("expected truncated paragraph")
        }
        XCTAssertEqual(text, String(repeating: "a", count: 12))
        XCTAssertEqual(long.last, .truncated)

        let many = (0 ..< 10).map { "Paragraph \($0)" }.joined(separator: "\n\n")
        let blocks = MarkdownDocument.parse(many, maxBlocks: 3)
        XCTAssertEqual(blocks.count, 4)
        XCTAssertEqual(blocks.last, .truncated)

        let manyItems = (0 ..< 250).map { "- item \($0)" }.joined(separator: "\n")
        let listBlocks = MarkdownDocument.parse(manyItems)
        guard case let .bulletList(items) = listBlocks.first else {
            return XCTFail("expected a capped bullet list")
        }
        XCTAssertEqual(items.count, 200)
        XCTAssertEqual(listBlocks.last, .truncated)
    }

    func testEmptyTextDoesNotProduceBlocks() {
        XCTAssertTrue(MarkdownDocument.parse("").isEmpty)
        XCTAssertTrue(MarkdownDocument.parse("\n\n").isEmpty)
    }

    func testTranscriptDisplayPreferenceDefaultsAndNormalization() {
        XCTAssertEqual(TranscriptDisplayPreference.defaultUserFormat, .plainText)
        XCTAssertEqual(TranscriptDisplayPreference.defaultAgentFormat, .markdown)
        XCTAssertEqual(
            TranscriptDisplayPreference.format(rawValue: "markdown", fallback: .plainText),
            .markdown
        )
        XCTAssertEqual(
            TranscriptDisplayPreference.format(rawValue: "plain_text", fallback: .markdown),
            .plainText
        )
        XCTAssertEqual(
            TranscriptDisplayPreference.format(rawValue: "legacy-value", fallback: .markdown),
            .markdown
        )
        XCTAssertEqual(
            TranscriptDisplayPreference.format(rawValue: "", fallback: .plainText),
            .plainText
        )
        XCTAssertEqual(TranscriptDisplayFormat.plainText.rawValue, "plain_text")
        XCTAssertEqual(TranscriptDisplayFormat.markdown.rawValue, "markdown")
    }
}
