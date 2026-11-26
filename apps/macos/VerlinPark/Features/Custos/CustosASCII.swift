import Foundation

/// The accepted Custos wordmark and its three motion moments.
///
/// Only this logotype is character art. Everything else in the Custos window
/// is normal SwiftUI layout. The wordmark is pure ASCII by design; no Unicode
/// block glyphs, box drawing, cursor glyphs, or scanline are used.
enum CustosASCII {
    /// Four-row accepted wordmark. There is deliberately no small diamond
    /// before the word.
    static let wordmark: [String] = [
        #"  ___         _"#,
        #" / __|_  _ __| |_ ___ ___"#,
        #"| (__| || (_-<  _/ _ (_-<"#,
        #" \___|\_,_/__/\__\___/__/"#,
    ]

    static let wordmarkText = wordmark.joined(separator: "\n")

    /// Eight print-head frames. The last frame is always the exact accepted
    /// wordmark, with no trailing cursor.
    static let bootFrames: [[String]] = bootCuts.map(reveal) + [wordmark]

    static let bootFrameStrings: [String] = bootFrames.map { $0.joined(separator: "\n") }

    /// Seven controlled jitter frames. The final frame is normal and static.
    /// There is no scanline and no looping animation.
    static let criticalFrames: [[String]] = [
        wordmark,
        wordmark.map { " " + $0 },
        wordmark.map(leftShift),
        [
            wordmark[0],
            "  " + wordmark[1],
            wordmark[2],
            wordmark[3],
        ],
        [
            wordmark[0],
            wordmark[1],
            substituteFirst(in: wordmark[2], target: "(_-<", replacement: "(_#<"),
            wordmark[3],
        ],
        [
            wordmark[0],
            wordmark[1],
            wordmark[2],
            wordmark[3] + "#",
        ],
        wordmark,
    ]

    static let criticalFrameStrings: [String] = criticalFrames.map { $0.joined(separator: "\n") }

    // MARK: - Generation

    private static let bootCuts = [0, 4, 8, 12, 16, 20, 24]

    private static func reveal(cut: Int) -> [String] {
        wordmark.map { row in
            let characters = Array(row)
            if cut < characters.count {
                return String(characters.prefix(cut)) + "|"
            }
            return row + String(repeating: " ", count: cut - characters.count) + "|"
        }
    }

    private static func leftShift(_ row: String) -> String {
        row.hasPrefix(" ") ? String(row.dropFirst()) : row
    }

    private static func substituteFirst(in row: String, target: String, replacement: String) -> String {
        guard let range = row.range(of: target) else { return row }
        return row.replacingCharacters(in: range, with: replacement)
    }
}
