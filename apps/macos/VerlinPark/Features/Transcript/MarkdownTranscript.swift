import Foundation
import SwiftUI

/// Bounded block model for assistant Markdown. It deliberately covers the
/// common authoring subset (headings, paragraphs, lists, quotes, rules, fenced
/// code, and simple tables) instead of claiming full CommonMark support.
enum MarkdownBlock: Equatable, Sendable {
    case heading(level: Int, text: String)
    case paragraph(String)
    case bulletList([MarkdownListItem])
    case orderedList(start: Int, items: [MarkdownListItem])
    case codeBlock(language: String?, code: String)
    case quote(String)
    case rule
    case table(headers: [String], rows: [[String]])
    case truncated
}

struct MarkdownListItem: Equatable, Sendable {
    let text: String
    let depth: Int
}

enum MarkdownDocument {
    static let defaultMaxBlocks = 400
    static let defaultMaxCharacters = 80000

    /// Parse a bounded subset of Markdown into renderable blocks. The output is
    /// deterministic and never interprets raw HTML or loads remote content.
    static func parse(
        _ raw: String,
        maxBlocks: Int = defaultMaxBlocks,
        maxCharacters: Int = defaultMaxCharacters
    ) -> [MarkdownBlock] {
        let normalized = raw
            .replacingOccurrences(of: "\r\n", with: "\n")
            .replacingOccurrences(of: "\r", with: "\n")
        let inputTruncated = normalized.count > maxCharacters
        let text = inputTruncated ? String(normalized.prefix(maxCharacters)) : normalized
        let lines = text.components(separatedBy: "\n")
        var blocks: [MarkdownBlock] = []
        var blockTruncated = false
        var index = 0

        func append(_ block: MarkdownBlock) -> Bool {
            guard blocks.count < maxBlocks else {
                blockTruncated = true
                return false
            }
            blocks.append(block)
            return true
        }

        while index < lines.count {
            let line = lines[index]
            let trimmed = line.trimmingCharacters(in: .whitespaces)

            if trimmed.isEmpty {
                index += 1
                continue
            }

            if let fence = fenceStart(trimmed) {
                var codeLines: [String] = []
                var closed = false
                index += 1
                while index < lines.count {
                    let candidate = lines[index]
                    let candidateTrimmed = candidate.trimmingCharacters(in: .whitespaces)
                    if isFenceEnd(candidateTrimmed, fence: fence) {
                        closed = true
                        index += 1
                        break
                    }
                    codeLines.append(candidate)
                    index += 1
                }
                let code = codeLines.joined(separator: "\n")
                let language = fence.language?
                    .split(whereSeparator: { $0 == " " || $0 == "\t" })
                    .first
                    .map(String.init)
                if !append(.codeBlock(language: language, code: code)) {
                    break
                }
                if !closed {
                    // An unclosed fence is still a code block; the renderer
                    // shows the partial content while streaming.
                }
                continue
            }

            if let heading = heading(line) {
                if !append(.heading(level: heading.level, text: heading.text)) {
                    break
                }
                index += 1
                continue
            }

            if isRule(trimmed) {
                if !append(.rule) {
                    break
                }
                index += 1
                continue
            }

            if isQuoteStart(trimmed) {
                var quoteLines: [String] = []
                while index < lines.count {
                    let candidate = lines[index]
                    let candidateTrimmed = candidate.trimmingCharacters(in: .whitespaces)
                    guard candidateTrimmed.first == ">" else { break }
                    var content = candidateTrimmed.dropFirst()
                    if content.first == " " {
                        content = content.dropFirst()
                    }
                    quoteLines.append(String(content))
                    index += 1
                }
                if !append(.quote(quoteLines.joined(separator: "\n"))) {
                    break
                }
                continue
            }

            if let marker = listItem(line) {
                var items: [MarkdownListItem] = []
                let ordered = marker.ordered
                let start = marker.number
                while index < lines.count, let current = listItem(lines[index]),
                      current.ordered == ordered
                {
                    guard items.count < 200 else {
                        blockTruncated = true
                        break
                    }
                    items.append(MarkdownListItem(text: current.text, depth: current.depth))
                    index += 1
                }
                let block: MarkdownBlock = ordered
                    ? .orderedList(start: start, items: items)
                    : .bulletList(items)
                if !append(block) || blockTruncated {
                    break
                }
                continue
            }

            if trimmed.contains("|"),
               index + 1 < lines.count,
               isTableSeparator(lines[index + 1])
            {
                let headers = tableCells(line)
                index += 2
                var rows: [[String]] = []
                while index < lines.count {
                    let candidate = lines[index]
                    guard candidate.trimmingCharacters(in: .whitespaces).contains("|") else {
                        break
                    }
                    rows.append(tableCells(candidate))
                    index += 1
                    if rows.count >= 200 {
                        blockTruncated = true
                        break
                    }
                }
                if !append(.table(headers: headers, rows: rows)) || blockTruncated {
                    break
                }
                continue
            }

            var paragraph: [String] = []
            while index < lines.count {
                let candidate = lines[index]
                let candidateTrimmed = candidate.trimmingCharacters(in: .whitespaces)
                if candidateTrimmed.isEmpty
                    || isBlockStart(candidate, next: index + 1 < lines.count ? lines[index + 1] : nil)
                {
                    break
                }
                paragraph.append(candidateTrimmed)
                index += 1
            }
            if !paragraph.isEmpty {
                if !append(.paragraph(paragraph.joined(separator: " "))) {
                    break
                }
            } else {
                // Defensive progress for an unrecognized line that also looked
                // like a block start.
                index += 1
            }
        }

        if inputTruncated || blockTruncated {
            blocks.append(.truncated)
        }
        return blocks
    }

    private struct Fence {
        let character: Character
        let length: Int
        let language: String?
    }

    private static func fenceStart(_ trimmed: String) -> Fence? {
        guard let character = trimmed.first, character == "`" || character == "~" else {
            return nil
        }
        let length = trimmed.prefix(while: { $0 == character }).count
        guard length >= 3 else { return nil }
        let rest = trimmed.dropFirst(length).trimmingCharacters(in: .whitespaces)
        return Fence(character: character, length: length, language: rest.isEmpty ? nil : rest)
    }

    private static func isFenceEnd(_ trimmed: String, fence: Fence) -> Bool {
        guard trimmed.first == fence.character else { return false }
        let length = trimmed.prefix(while: { $0 == fence.character }).count
        return length >= fence.length && trimmed.dropFirst(length).trimmingCharacters(in: .whitespaces)
            .isEmpty
    }

    private static func heading(_ line: String) -> (level: Int, text: String)? {
        let trimmed = line.drop(while: { $0 == " " })
        let hashes = trimmed.prefix(while: { $0 == "#" }).count
        guard (1 ... 6).contains(hashes) else { return nil }
        let rest = trimmed.dropFirst(hashes)
        guard rest.isEmpty || rest.first == " " else { return nil }
        var text = rest.trimmingCharacters(in: .whitespaces)
        text = text.replacingOccurrences(
            of: "\\s+#+\\s*$",
            with: "",
            options: .regularExpression
        )
        return (hashes, text)
    }

    private static func isRule(_ trimmed: String) -> Bool {
        let compact = trimmed.filter { $0 != " " && $0 != "\t" }
        guard compact.count >= 3, let first = compact.first,
              first == "-" || first == "*" || first == "_" else { return false }
        return compact.allSatisfy { $0 == first }
    }

    private static func isQuoteStart(_ trimmed: String) -> Bool {
        trimmed.first == ">"
    }

    private struct ListMarker {
        let ordered: Bool
        let number: Int
        let depth: Int
        let text: String
    }

    private static func listItem(_ line: String) -> ListMarker? {
        let expanded = line.replacingOccurrences(of: "\t", with: "    ")
        let leading = expanded.prefix(while: { $0 == " " }).count
        let rest = expanded.dropFirst(leading)
        guard let first = rest.first else { return nil }

        if first == "-" || first == "*" || first == "+" {
            let after = rest.dropFirst()
            guard after.first == " " else { return nil }
            let text = after.trimmingCharacters(in: .whitespaces)
            guard !text.isEmpty else { return nil }
            return ListMarker(
                ordered: false,
                number: 1,
                depth: min(leading / 2, 3),
                text: text
            )
        }

        let digits = rest.prefix(while: { $0.isNumber })
        guard !digits.isEmpty, let number = Int(digits), number > 0 else { return nil }
        let afterDigits = rest.dropFirst(digits.count)
        guard let delimiter = afterDigits.first, delimiter == "." || delimiter == ")" else {
            return nil
        }
        let afterDelimiter = afterDigits.dropFirst()
        guard afterDelimiter.first == " " else { return nil }
        let text = afterDelimiter.trimmingCharacters(in: .whitespaces)
        guard !text.isEmpty else { return nil }
        return ListMarker(
            ordered: true,
            number: number,
            depth: min(leading / 2, 3),
            text: text
        )
    }

    private static func isBlockStart(_ line: String, next: String?) -> Bool {
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty {
            return true
        }
        if fenceStart(trimmed) != nil {
            return true
        }
        if heading(line) != nil {
            return true
        }
        if isRule(trimmed) {
            return true
        }
        if isQuoteStart(trimmed) {
            return true
        }
        if listItem(line) != nil {
            return true
        }
        if trimmed.contains("|"), let next, isTableSeparator(next) {
            return true
        }
        return false
    }

    private static func isTableSeparator(_ line: String) -> Bool {
        let cells = tableCells(line)
        guard !cells.isEmpty else { return false }
        return cells.allSatisfy { cell in
            var body = cell.trimmingCharacters(in: .whitespaces)
            if body.hasPrefix(":") {
                body.removeFirst()
            }
            if body.hasSuffix(":") {
                body.removeLast()
            }
            return body.count >= 3 && body.allSatisfy { $0 == "-" }
        }
    }

    private static func tableCells(_ line: String) -> [String] {
        var text = line.trimmingCharacters(in: .whitespaces)
        if text.hasPrefix("|") {
            text.removeFirst()
        }
        if text.hasSuffix("|") {
            text.removeLast()
        }
        var cells: [String] = []
        var current = ""
        var escaped = false
        for character in text {
            if escaped {
                current.append(character)
                escaped = false
                continue
            }
            if character == "\\" {
                escaped = true
                continue
            }
            if character == "|" {
                cells.append(current.trimmingCharacters(in: .whitespaces))
                current = ""
            } else {
                current.append(character)
            }
        }
        if escaped {
            current.append("\\")
        }
        cells.append(current.trimmingCharacters(in: .whitespaces))
        return cells
    }
}

enum MarkdownLinkPolicy {
    /// Assistant output is untrusted. Only ordinary web links may leave the
    /// app; custom schemes such as `file:` or `javascript:` are discarded.
    static func allowsExternalOpen(_ url: URL) -> Bool {
        let scheme = url.scheme?.lowercased() ?? ""
        return scheme == "http" || scheme == "https"
    }
}

enum MarkdownInline {
    /// Inline Markdown with citation markers converted into links. The parser is
    /// bounded to inline syntax; block structure is handled by
    /// `MarkdownDocument`.
    static func attributed(
        _ text: String,
        citations: [SourceCitation],
        size: CGFloat = 12.5
    ) -> AttributedString {
        let source = citationMarkdown(text, citations: citations)
        var attributed: AttributedString
        do {
            attributed = try AttributedString(
                markdown: source,
                options: .init(
                    interpretedSyntax: .inlineOnlyPreservingWhitespace,
                    failurePolicy: .returnPartiallyParsedIfPossible
                )
            )
        } catch {
            attributed = AttributedString(text)
        }

        let codeRanges = attributed.runs.compactMap { run -> Range<AttributedString.Index>? in
            guard let intent = run.inlinePresentationIntent, intent.contains(.code) else {
                return nil
            }
            return run.range
        }
        for range in codeRanges {
            attributed[range].font = VPTheme.mono(max(9, size - 1))
            attributed[range].foregroundColor = VPTheme.accentSoft
            attributed[range].backgroundColor = VPTheme.surfaceRaised
        }
        return attributed
    }

    /// Convert `[c1]` into a Markdown link whose visible label stays `[c1]`.
    /// Already-linked markers and fenced code are not rewritten by this helper.
    static func citationMarkdown(_ text: String, citations: [SourceCitation]) -> String {
        guard !citations.isEmpty else { return text }
        var result = text
        for citation in citations {
            let key = citation.key
            let escaped = NSRegularExpression.escapedPattern(for: key)
            let pattern = "(?<!\\[)\\[\(escaped)\\](?!\\()"
            guard let regex = try? NSRegularExpression(pattern: pattern) else {
                result = result.replacingOccurrences(
                    of: "[\(key)]",
                    with: "[[\(key)]](verlinpark://citation/\(key))"
                )
                continue
            }
            let range = NSRange(result.startIndex..., in: result)
            result = regex.stringByReplacingMatches(
                in: result,
                range: range,
                withTemplate: "[[\(key)]](verlinpark://citation/\(key))"
            )
        }
        return result
    }
}

/// Assistant transcript renderer. It never executes remote content, never
/// loads images, and applies a fixed block budget so a pathological response
/// cannot create an unbounded view tree.
struct MarkdownTranscriptView: View {
    let text: String
    var citations: [SourceCitation] = []

    var body: some View {
        let blocks = MarkdownDocument.parse(text)
        VStack(alignment: .leading, spacing: 8) {
            ForEach(blocks.indices, id: \.self) { index in
                blockView(blocks[index])
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .textSelection(.enabled)
        .tint(VPTheme.accent)
    }

    private func inline(_ text: String, size: CGFloat = 12.5) -> AttributedString {
        MarkdownInline.attributed(text, citations: citations, size: size)
    }

    @ViewBuilder
    private func blockView(_ block: MarkdownBlock) -> some View {
        switch block {
        case let .heading(level, text):
            Text(inline(text, size: headingSize(level)))
                .font(VPTheme.font(headingSize(level), weight: .semibold))
                .foregroundStyle(VPTheme.textPrimary)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.top, level <= 2 ? 3 : 1)

        case let .paragraph(text):
            Text(inline(text))
                .font(VPTheme.font(12.5))
                .foregroundStyle(VPTheme.textPrimary)
                .lineSpacing(2.5)
                .fixedSize(horizontal: false, vertical: true)

        case let .bulletList(items):
            listView(items, ordered: false, start: 1)

        case let .orderedList(start, items):
            listView(items, ordered: true, start: start)

        case let .codeBlock(language, code):
            codeBlockView(language: language, code: code)

        case let .quote(text):
            HStack(alignment: .top, spacing: 8) {
                Rectangle()
                    .fill(VPTheme.accent.opacity(0.45))
                    .frame(width: 2)
                Text(inline(text))
                    .font(VPTheme.font(12, weight: .regular))
                    .foregroundStyle(VPTheme.textSecondary)
                    .lineSpacing(2)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .fixedSize(horizontal: false, vertical: true)

        case .rule:
            Rectangle()
                .fill(VPTheme.hairline)
                .frame(height: 1)
                .padding(.vertical, 2)

        case let .table(headers, rows):
            tableView(headers: headers, rows: rows)

        case .truncated:
            HStack(spacing: 5) {
                Image(systemName: "ellipsis.circle")
                    .font(.system(size: 8, weight: .semibold))
                Text("Response truncated for display")
                    .font(VPTheme.mono(8))
            }
            .foregroundStyle(VPTheme.textTertiary)
        }
    }

    private func headingSize(_ level: Int) -> CGFloat {
        switch level {
        case 1: 15
        case 2: 13.5
        default: 12.5
        }
    }

    private func listView(
        _ items: [MarkdownListItem],
        ordered: Bool,
        start: Int
    ) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(items.indices, id: \.self) { index in
                let item = items[index]
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(ordered ? "\(start + index)." : "•")
                        .font(VPTheme.mono(10, weight: .semibold))
                        .foregroundStyle(VPTheme.accent)
                        .frame(minWidth: 16, alignment: .trailing)
                    Text(inline(item.text))
                        .font(VPTheme.font(12.5))
                        .foregroundStyle(VPTheme.textPrimary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .padding(.leading, CGFloat(item.depth) * 14)
            }
        }
    }

    private func codeBlockView(language: String?, code: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            if let language, !language.isEmpty {
                Text(language.uppercased())
                    .font(VPTheme.mono(6.5, weight: .semibold))
                    .tracking(0.5)
                    .foregroundStyle(VPTheme.textTertiary)
            }
            ScrollView(.horizontal, showsIndicators: true) {
                Text(code.isEmpty ? " " : code)
                    .font(VPTheme.mono(10.5))
                    .foregroundStyle(VPTheme.textPrimary)
                    .lineSpacing(2)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: true, vertical: false)
                    .padding(9)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                VPTheme.surfaceSunken,
                in: RoundedRectangle(cornerRadius: 7, style: .continuous)
            )
            .overlay {
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .strokeBorder(VPTheme.hairline, lineWidth: 1)
            }
        }
    }

    private func tableView(headers: [String], rows: [[String]]) -> some View {
        let columns = max(headers.count, 1)
        return ScrollView(.horizontal, showsIndicators: true) {
            Grid(alignment: .leading, horizontalSpacing: 14, verticalSpacing: 5) {
                GridRow {
                    ForEach(0 ..< columns, id: \.self) { column in
                        Text(inline(column < headers.count ? headers[column] : ""))
                            .font(VPTheme.mono(9.5, weight: .semibold))
                            .foregroundStyle(VPTheme.textSecondary)
                            .frame(minWidth: 48, alignment: .leading)
                    }
                }
                ForEach(rows.indices, id: \.self) { rowIndex in
                    GridRow {
                        ForEach(0 ..< columns, id: \.self) { column in
                            Text(inline(column < rows[rowIndex].count ? rows[rowIndex][column] : ""))
                                .font(VPTheme.mono(9.5))
                                .foregroundStyle(VPTheme.textPrimary)
                                .frame(minWidth: 48, alignment: .leading)
                        }
                    }
                }
            }
            .padding(9)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            VPTheme.surfaceSunken,
            in: RoundedRectangle(cornerRadius: 7, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
    }
}
