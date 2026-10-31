import Foundation

struct SessionDiffFile: Identifiable, Hashable, Sendable {
    let path: String
    let additions: Int
    let deletions: Int
    let lines: [String]

    var id: String {
        path
    }

    var patch: String {
        lines.joined(separator: "\n")
    }
}

enum SessionDiffParser {
    static func parse(_ patch: String) -> [SessionDiffFile] {
        var files: [SessionDiffFile] = []
        var currentLines: [String] = []
        var currentDiffPath = ""
        var currentOldPath: String?
        var currentNewPath: String?
        var additions = 0
        var deletions = 0
        var inHunk = false

        func flush() {
            guard !currentLines.isEmpty else { return }
            let path = currentNewPath
                ?? currentOldPath
                ?? (currentDiffPath.isEmpty ? "unknown" : currentDiffPath)
            files.append(
                SessionDiffFile(
                    path: path,
                    additions: additions,
                    deletions: deletions,
                    lines: currentLines
                )
            )
            currentLines = []
            currentDiffPath = ""
            currentOldPath = nil
            currentNewPath = nil
            additions = 0
            deletions = 0
            inHunk = false
        }

        for rawLine in patch.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = String(rawLine)
            if line.hasPrefix("diff --git ") {
                flush()
                currentDiffPath = parseDiffGitPath(line)
                currentLines = [line]
                continue
            }
            guard !currentLines.isEmpty else { continue }
            currentLines.append(line)
            if line.hasPrefix("@@") {
                inHunk = true
            } else if !inHunk, line.hasPrefix("--- ") {
                currentOldPath = parsePrefixedPath(line, prefix: "--- ")
            } else if !inHunk, line.hasPrefix("+++ ") {
                currentNewPath = parsePrefixedPath(line, prefix: "+++ ")
            } else if inHunk, line.hasPrefix("+") {
                additions += 1
            } else if inHunk, line.hasPrefix("-") {
                deletions += 1
            }
        }
        flush()
        return files
    }

    private static func parseDiffGitPath(_ line: String) -> String {
        guard let range = line.range(of: " b/", options: .backwards) else {
            return ""
        }
        return String(line[range.upperBound...])
    }

    private static func parsePrefixedPath(_ line: String, prefix: String) -> String? {
        let value = String(line.dropFirst(prefix.count))
        if value == "/dev/null" {
            return nil
        }
        if value.hasPrefix("a/") || value.hasPrefix("b/") {
            return String(value.dropFirst(2))
        }
        return value
    }
}
