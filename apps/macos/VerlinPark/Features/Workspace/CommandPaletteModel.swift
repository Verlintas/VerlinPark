import Foundation

struct CommandPaletteCandidate: Equatable, Sendable {
    let id: String
    let title: String
    let subtitle: String
    let keywords: String
}

struct RankedCommand: Equatable, Sendable {
    let id: String
    let score: Int
    let titleRanges: [Range<Int>]
    let subtitleRanges: [Range<Int>]
}

enum CommandPaletteRanker {
    static func rank(
        _ candidates: [CommandPaletteCandidate],
        query: String,
        recents: [String]
    ) -> [RankedCommand] {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            return candidates.enumerated()
                .map { index, candidate in
                    RankedCommand(
                        id: candidate.id,
                        score: recencyBonus(candidate.id, recents: recents) - index,
                        titleRanges: [],
                        subtitleRanges: []
                    )
                }
                .sorted { lhs, rhs in
                    if lhs.score == rhs.score {
                        return candidateOrder(lhs.id, rhs.id, candidates: candidates)
                    }
                    return lhs.score > rhs.score
                }
        }

        return candidates.compactMap { candidate in
            guard let match = bestMatch(trimmed, candidate: candidate) else { return nil }
            return RankedCommand(
                id: candidate.id,
                score: match.score + recencyBonus(candidate.id, recents: recents),
                titleRanges: match.titleRanges,
                subtitleRanges: match.subtitleRanges
            )
        }
        .sorted { lhs, rhs in
            if lhs.score == rhs.score {
                return candidateOrder(lhs.id, rhs.id, candidates: candidates)
            }
            return lhs.score > rhs.score
        }
    }

    static func score(_ query: String, candidate: String) -> Int? {
        match(query, in: candidate)?.score
    }

    static func highlightRanges(_ query: String, in candidate: String) -> [Range<Int>] {
        match(query, in: candidate)?.ranges ?? []
    }

    private static func candidateOrder(
        _ lhs: String,
        _ rhs: String,
        candidates: [CommandPaletteCandidate]
    ) -> Bool {
        let left = candidates.firstIndex { $0.id == lhs } ?? Int.max
        let right = candidates.firstIndex { $0.id == rhs } ?? Int.max
        return left < right
    }

    private static func recencyBonus(_ id: String, recents: [String]) -> Int {
        guard let index = recents.firstIndex(of: id) else { return 0 }
        return max(0, 190 - index * 14)
    }

    private struct Match {
        let score: Int
        let ranges: [Range<Int>]
    }

    private struct CandidateMatch {
        let score: Int
        let titleRanges: [Range<Int>]
        let subtitleRanges: [Range<Int>]
    }

    private static func bestMatch(
        _ query: String,
        candidate: CommandPaletteCandidate
    ) -> CandidateMatch? {
        if let titleMatch = match(query, in: candidate.title, base: 1200) {
            return CandidateMatch(
                score: titleMatch.score,
                titleRanges: titleMatch.ranges,
                subtitleRanges: []
            )
        }
        if let subtitleMatch = match(query, in: candidate.subtitle, base: 620) {
            return CandidateMatch(
                score: subtitleMatch.score,
                titleRanges: [],
                subtitleRanges: subtitleMatch.ranges
            )
        }
        if match(query, in: candidate.keywords, base: 300) != nil {
            return CandidateMatch(score: 300, titleRanges: [], subtitleRanges: [])
        }
        return nil
    }

    private static func match(_ query: String, in candidate: String, base: Int = 1000) -> Match? {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !needle.isEmpty else { return Match(score: 0, ranges: []) }
        let haystack = candidate.lowercased()

        if haystack == needle {
            return Match(score: base + 240, ranges: [0 ..< candidate.count])
        }
        if haystack.hasPrefix(needle) {
            return Match(
                score: base + 170 - max(0, candidate.count - needle.count),
                ranges: [0 ..< needle.count]
            )
        }

        if let wordPrefix = wordPrefixRange(needle, in: candidate) {
            let characters = Array(candidate)
            let exactWord = wordPrefix.upperBound == characters.count
                || !characters[wordPrefix.upperBound].isLetter
                && !characters[wordPrefix.upperBound].isNumber
            return Match(
                score: base + 110 - wordPrefix.lowerBound + (exactWord ? 40 : 0),
                ranges: [wordPrefix]
            )
        }

        if let range = candidate.range(of: query, options: [.caseInsensitive, .diacriticInsensitive]) {
            let start = candidate.distance(from: candidate.startIndex, to: range.lowerBound)
            let length = candidate.distance(from: range.lowerBound, to: range.upperBound)
            return Match(score: base + 40 - start, ranges: [start ..< (start + length)])
        }

        if let ranges = subsequenceRanges(needle, in: candidate),
           let first = ranges.first,
           let last = ranges.last
        {
            let gaps = max(
                0,
                last.upperBound - first.lowerBound - ranges.reduce(0) { $0 + $1.count }
            )
            let boundaryBonus = ranges.filter {
                $0.lowerBound == 0
                    || isWordBoundary(candidate, index: $0.lowerBound)
            }.count * 8
            return Match(score: base - 250 - gaps * 3 + boundaryBonus, ranges: ranges)
        }

        return nil
    }

    private static func wordPrefixRange(_ needle: String, in candidate: String) -> Range<Int>? {
        let characters = Array(candidate)
        var index = 0
        while index < characters.count {
            if index == 0 || !characters[index - 1].isLetter && !characters[index - 1].isNumber,
               String(characters[index...]).lowercased().hasPrefix(needle)
            {
                return index ..< (index + needle.count)
            }
            index += 1
        }
        return nil
    }

    private static func subsequenceRanges(_ needle: String, in candidate: String) -> [Range<Int>]? {
        let characters = Array(candidate)
        let wanted = Array(needle)
        var ranges: [Range<Int>] = []
        var wantedIndex = 0
        for (index, character) in characters.enumerated() {
            guard wantedIndex < wanted.count else { break }
            if String(character).lowercased() == String(wanted[wantedIndex]) {
                if let last = ranges.last, last.upperBound == index {
                    ranges[ranges.count - 1] = last.lowerBound ..< (index + 1)
                } else {
                    ranges.append(index ..< (index + 1))
                }
                wantedIndex += 1
            }
        }
        return wantedIndex == wanted.count ? ranges : nil
    }

    private static func isWordBoundary(_ candidate: String, index: Int) -> Bool {
        let characters = Array(candidate)
        guard index > 0, index < characters.count else { return true }
        let previous = characters[index - 1]
        return !previous.isLetter && !previous.isNumber
    }
}

enum PaletteRecents {
    static let storageKey = "verlinpark.commandPalette.recents"
    static let limit = 8

    static func load() -> [String] {
        UserDefaults.standard.stringArray(forKey: storageKey) ?? []
    }

    @discardableResult
    static func record(_ id: String, in existing: [String]) -> [String] {
        var next = existing.filter { $0 != id }
        next.insert(id, at: 0)
        next = Array(next.prefix(limit))
        UserDefaults.standard.set(next, forKey: storageKey)
        return next
    }
}
