import SwiftUI

/// Renders a string with character ranges emphasized in the accent color.
/// Ranges are character offsets so callers do not need to leak String indices.
struct VPHighlightedText: View {
    let text: String
    let ranges: [Range<Int>]

    var body: some View {
        compiled
            .accessibilityLabel(text)
    }

    private var compiled: Text {
        let characters = Array(text)
        guard !characters.isEmpty else { return Text(text) }
        let merged = Self.merged(ranges, count: characters.count)
        guard !merged.isEmpty else { return Text(text) }

        var result = Text("")
        var cursor = 0
        for range in merged {
            if range.lowerBound > cursor {
                result = result + Text(String(characters[cursor ..< range.lowerBound]))
            }
            result = result + Text(String(characters[range]))
                .foregroundColor(VPTheme.accent)
                .bold()
            cursor = range.upperBound
        }
        if cursor < characters.count {
            result = result + Text(String(characters[cursor...]))
        }
        return result
    }

    private static func merged(_ ranges: [Range<Int>], count: Int) -> [Range<Int>] {
        let clipped = ranges
            .map {
                max(0, $0.lowerBound) ..< min(count, $0.upperBound)
            }
            .filter { $0.lowerBound < $0.upperBound }
            .sorted { $0.lowerBound < $1.lowerBound }
        var result: [Range<Int>] = []
        for range in clipped {
            if let last = result.last, range.lowerBound <= last.upperBound {
                result[result.count - 1] = last.lowerBound ..< max(last.upperBound, range.upperBound)
            } else {
                result.append(range)
            }
        }
        return result
    }
}
