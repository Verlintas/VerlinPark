import Foundation

enum SearchSourceFilter: String, CaseIterable, Identifiable, Sendable {
    case all
    case messages
    case events

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .all: L10n.t("All")
        case .messages: L10n.t("Messages")
        case .events: L10n.t("Events")
        }
    }

    var icon: String? {
        switch self {
        case .all: nil
        case .messages: "bubble.left"
        case .events: "bolt.horizontal"
        }
    }

    func includes(_ hit: SearchHit) -> Bool {
        switch self {
        case .all: true
        case .messages: hit.source != "event"
        case .events: hit.source == "event"
        }
    }
}

struct SearchResultsModel: Equatable, Sendable {
    var source: SearchSourceFilter = .all

    func visible(_ hits: [SearchHit]) -> [SearchHit] {
        hits.filter { source.includes($0) }
    }

    func counts(_ hits: [SearchHit]) -> [SearchSourceFilter: Int] {
        Dictionary(
            uniqueKeysWithValues: SearchSourceFilter.allCases.map { filter in
                (filter, hits.count(where: { filter.includes($0) }))
            }
        )
    }

    func highlightedSnippet(
        _ hit: SearchHit,
        query: String,
        limit: Int
    ) -> [Range<Int>] {
        Self.ranges(of: query, in: hit.snippet, limit: limit)
    }

    func highlightedGoal(_ hit: SearchHit, query: String) -> [Range<Int>] {
        Self.ranges(of: query, in: hit.sessionGoal, limit: 80)
    }

    static func ranges(
        of query: String,
        in text: String,
        limit: Int = 120
    ) -> [Range<Int>] {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return [] }
        let characters = Array(text)
        let haystack = String(characters.prefix(max(0, limit))).lowercased()
        let needleLower = needle.lowercased()
        var result: [Range<Int>] = []
        var searchStart = haystack.startIndex
        while result.count < 8,
              let range = haystack.range(
                  of: needleLower,
                  range: searchStart ..< haystack.endIndex
              )
        {
            let start = haystack.distance(from: haystack.startIndex, to: range.lowerBound)
            let length = haystack.distance(from: range.lowerBound, to: range.upperBound)
            result.append(start ..< (start + length))
            searchStart = range.upperBound
        }
        return result
    }
}
