// Block-level Markdown segmentation for committed assistant text.
//
// CommonMark block structure is rendered with native SwiftUI controls instead
// of showing block markers literally: headings, paragraphs, unordered/ordered
// lists, block quotes, horizontal rules, pipe tables and fenced code. Inline
// emphasis, strong, code spans and links are handled by AttributedString.
//
// Link policy: only http, https and mailto links are interactive. Every other
// scheme is displayed as plain text. Remote images are replaced by their alt
// text and are never fetched; raw HTML is shown literally, not rendered.

import Foundation

struct MarkdownListItem: Equatable, Hashable, Sendable {
    let text: String
    let depth: Int
}

struct MarkdownSegment: Identifiable, Equatable, Hashable, Sendable {
    enum Kind: Equatable, Hashable, Sendable {
        case text(String)
        case heading(level: Int, text: String)
        case list(ordered: Bool, items: [MarkdownListItem])
        case quote(String)
        case rule
        case code(language: String?, content: String)
        case table(headers: [String], rows: [[String]])
    }

    let id: Int
    let kind: Kind
}

/// Cached parse result for one committed message body. The transcript re-renders
/// on every streaming preview or scroll; reparsing every body each time made
/// long histories quadratic in practice.
final class MarkdownDocument: @unchecked Sendable {
    let segments: [MarkdownSegment]
    init(segments: [MarkdownSegment]) {
        self.segments = segments
    }
}

enum MarkdownParser {
    private static let cache = NSCache<NSString, MarkdownDocument>()
    private static let cacheLock = NSLock()
    private static var cacheHits = 0
    private static var cacheMisses = 0

    static func parse(_ markdown: String) -> [MarkdownSegment] {
        var kinds: [MarkdownSegment.Kind] = []
        var paragraph: [String] = []
        var listItems: [MarkdownListItem] = []
        var listOrdered = false
        var quoteLines: [String] = []
        var codeLines: [String] = []
        var codeLanguage: String?
        var inCode = false

        func flushParagraph() {
            let text = paragraph
                .joined(separator: "\n")
                .trimmingCharacters(in: .whitespacesAndNewlines)
            paragraph.removeAll()
            guard !text.isEmpty else { return }
            kinds.append(.text(text))
        }

        func flushList() {
            guard !listItems.isEmpty else { return }
            kinds.append(.list(ordered: listOrdered, items: listItems))
            listItems.removeAll()
        }

        func flushQuote() {
            let text = quoteLines
                .joined(separator: "\n")
                .trimmingCharacters(in: .whitespacesAndNewlines)
            quoteLines.removeAll()
            guard !text.isEmpty else { return }
            kinds.append(.quote(text))
        }

        func flushAll() {
            flushParagraph()
            flushList()
            flushQuote()
        }

        let lines = markdown.components(separatedBy: "\n")
        var index = 0
        while index < lines.count {
            let raw = lines[index]
            let trimmed = raw.trimmingCharacters(in: .whitespaces)

            if trimmed.hasPrefix("```") {
                if inCode {
                    kinds.append(
                        .code(
                            language: codeLanguage,
                            content: codeLines.joined(separator: "\n")
                        )
                    )
                    codeLines.removeAll()
                    codeLanguage = nil
                    inCode = false
                } else {
                    flushAll()
                    inCode = true
                    let language = String(trimmed.dropFirst(3))
                        .trimmingCharacters(in: .whitespaces)
                    codeLanguage = language.isEmpty ? nil : language
                }
                index += 1
                continue
            }

            if inCode {
                codeLines.append(raw)
                index += 1
                continue
            }

            if trimmed.isEmpty {
                flushAll()
                index += 1
                continue
            }

            if let parsedTable = table(in: lines, start: index) {
                flushAll()
                kinds.append(parsedTable.kind)
                index = parsedTable.next
                continue
            }

            if let heading = heading(in: trimmed) {
                flushAll()
                kinds.append(.heading(level: heading.level, text: heading.text))
                index += 1
                continue
            }

            if isRule(trimmed) {
                flushAll()
                kinds.append(.rule)
                index += 1
                continue
            }

            if trimmed.hasPrefix(">") {
                flushParagraph()
                flushList()
                var quoted = String(trimmed.dropFirst())
                if quoted.hasPrefix(" ") {
                    quoted.removeFirst()
                }
                quoteLines.append(quoted)
                index += 1
                continue
            }

            flushQuote()

            if let item = listItem(in: raw) {
                flushParagraph()
                if listItems.isEmpty {
                    listOrdered = item.ordered
                } else if listOrdered != item.ordered {
                    flushList()
                    listOrdered = item.ordered
                }
                listItems.append(MarkdownListItem(text: item.text, depth: item.depth))
                index += 1

                // Consume indented continuation lines so wrapped list items do
                // not become separate paragraphs.
                while index < lines.count {
                    let nextRaw = lines[index]
                    let nextTrimmed = nextRaw.trimmingCharacters(in: .whitespaces)
                    if nextTrimmed.isEmpty { break }
                    if listItem(in: nextRaw) != nil
                        || nextTrimmed.hasPrefix("```")
                        || heading(in: nextTrimmed) != nil
                        || isRule(nextTrimmed)
                        || nextTrimmed.hasPrefix(">") {
                        break
                    }
                    guard leadingSpaces(in: nextRaw) >= 2, !listItems.isEmpty else {
                        break
                    }
                    let last = listItems.removeLast()
                    listItems.append(
                        MarkdownListItem(
                            text: last.text + " " + nextTrimmed,
                            depth: last.depth
                        )
                    )
                    index += 1
                }
                continue
            }

            flushList()
            paragraph.append(trimmed)
            index += 1
        }

        if inCode {
            // An unterminated fence is shown as ordinary text instead of
            // swallowing the rest of the message into a code block.
            flushAll()
            var fallback = "```"
            if let codeLanguage { fallback += codeLanguage }
            let text = ([fallback] + codeLines)
                .joined(separator: "\n")
                .trimmingCharacters(in: .whitespacesAndNewlines)
            if !text.isEmpty {
                kinds.append(.text(text))
            }
        } else {
            flushAll()
        }

        return kinds.enumerated().map { MarkdownSegment(id: $0.offset, kind: $0.element) }
    }

    /// Returns the cached blocks for a message body, parsing it only on the
    /// first render. Call after production code with `cached(_:)`.
    static func cached(_ markdown: String) -> [MarkdownSegment] {
        let key = markdown as NSString
        if let document = cache.object(forKey: key) {
            cacheLock.lock()
            cacheHits += 1
            cacheLock.unlock()
            return document.segments
        }
        let segments = parse(markdown)
        cache.setObject(MarkdownDocument(segments: segments), forKey: key)
        cacheLock.lock()
        cacheMisses += 1
        cacheLock.unlock()
        return segments
    }

    static func cacheStatistics() -> (hits: Int, misses: Int) {
        cacheLock.lock()
        defer { cacheLock.unlock() }
        return (cacheHits, cacheMisses)
    }

    static func resetCacheForTesting() {
        cache.removeAllObjects()
        cacheLock.lock()
        cacheHits = 0
        cacheMisses = 0
        cacheLock.unlock()
    }

    /// Inline-only Markdown conversion. Falls back to plain text so malformed
    /// content never hides the message. Links outside the documented policy are
    /// flattened to text, and image syntax becomes its alt text.
    static func attributed(_ text: String) -> AttributedString {
        let sanitized = replacingImagesWithAltText(text)
        guard var attributed = try? AttributedString(
            markdown: sanitized,
            options: AttributedString.MarkdownParsingOptions(
                interpretedSyntax: .inlineOnlyPreservingWhitespace
            )
        ) else {
            return AttributedString(sanitized)
        }
        removeDisallowedLinks(from: &attributed)
        return attributed
    }

    static func isAllowedLink(_ url: URL) -> Bool {
        guard let scheme = url.scheme?.lowercased() else { return false }
        return scheme == "http" || scheme == "https" || scheme == "mailto"
    }

    private static func removeDisallowedLinks(from attributed: inout AttributedString) {
        var disallowed: [Range<AttributedString.Index>] = []
        for run in attributed.runs {
            if let link = run.link, !isAllowedLink(link) {
                disallowed.append(run.range)
            }
        }
        for range in disallowed {
            attributed[range].link = nil
        }
    }

    private static func replacingImagesWithAltText(_ text: String) -> String {
        guard text.contains("![") else { return text }
        let pattern = #"!\[([^\]]*)\]\([^)]*\)"#
        guard let expression = try? NSRegularExpression(pattern: pattern) else { return text }
        let range = NSRange(text.startIndex..<text.endIndex, in: text)
        return expression.stringByReplacingMatches(
            in: text,
            range: range,
            withTemplate: "$1"
        )
    }

    // MARK: - Tables

    /// Parses a GitHub-style pipe table starting at `start`. Returns nil when the
    /// header is not followed by a valid delimiter row, so a paragraph that
    /// merely contains a pipe is left alone.
    private static func table(
        in lines: [String],
        start: Int
    ) -> (kind: MarkdownSegment.Kind, next: Int)? {
        guard start + 1 < lines.count else { return nil }
        let headerLine = lines[start]
        guard headerLine.contains("|") else { return nil }
        let headers = splitTableRow(headerLine)
        guard headers.count >= 1 else { return nil }

        let delimiterCells = splitTableRow(lines[start + 1])
        guard delimiterCells.count == headers.count,
              delimiterCells.allSatisfy(isTableDelimiter) else {
            return nil
        }

        var rows: [[String]] = []
        var index = start + 2
        while index < lines.count {
            let line = lines[index]
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            guard !trimmed.isEmpty, trimmed.contains("|") else { break }
            var cells = splitTableRow(line)
            if cells.count < headers.count {
                cells.append(contentsOf: Array(repeating: "", count: headers.count - cells.count))
            } else if cells.count > headers.count {
                cells = Array(cells.prefix(headers.count))
            }
            rows.append(cells)
            index += 1
        }
        return (.table(headers: headers, rows: rows), index)
    }

    static func splitTableRow(_ line: String) -> [String] {
        var body = line.trimmingCharacters(in: .whitespaces)
        if body.hasPrefix("|") {
            body.removeFirst()
        }
        if body.hasSuffix("|"), !body.hasSuffix("\\|") {
            body.removeLast()
        }
        var cells: [String] = []
        var current = ""
        var escaped = false
        for character in body {
            if escaped {
                current.append(character)
                escaped = false
            } else if character == "\\" {
                escaped = true
            } else if character == "|" {
                cells.append(current.trimmingCharacters(in: .whitespaces))
                current = ""
            } else {
                current.append(character)
            }
        }
        cells.append(current.trimmingCharacters(in: .whitespaces))
        return cells
    }

    private static func isTableDelimiter(_ cell: String) -> Bool {
        var value = cell.trimmingCharacters(in: .whitespaces)
        if value.hasPrefix(":") { value.removeFirst() }
        if value.hasSuffix(":") { value.removeLast() }
        return !value.isEmpty && value.allSatisfy { $0 == "-" }
    }

    private static func heading(in line: String) -> (level: Int, text: String)? {
        let hashes = line.prefix(while: { $0 == "#" }).count
        guard (1...6).contains(hashes) else { return nil }
        let remainder = line.dropFirst(hashes)
        guard remainder.first == " " else { return nil }
        let text = remainder.trimmingCharacters(in: .whitespaces)
        guard !text.isEmpty else { return nil }
        return (hashes, text)
    }

    private static func isRule(_ line: String) -> Bool {
        let compact = line.filter { !$0.isWhitespace }
        guard compact.count >= 3 else { return false }
        guard let first = compact.first, "-*_".contains(first) else { return false }
        return compact.allSatisfy { $0 == first }
    }

    private static func listItem(in line: String) -> (ordered: Bool, depth: Int, text: String)? {
        let depth = min(4, leadingSpaces(in: line) / 2)
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return nil }

        for marker in ["- ", "* ", "+ "] where trimmed.hasPrefix(marker) {
            let text = String(trimmed.dropFirst(marker.count))
                .trimmingCharacters(in: .whitespaces)
            guard !text.isEmpty else { return nil }
            return (false, depth, text)
        }

        var digits = ""
        var rest = trimmed[...]
        while let first = rest.first, first.isNumber {
            digits.append(first)
            rest = rest.dropFirst()
        }
        guard !digits.isEmpty,
              let separator = rest.first,
              separator == "." || separator == ")",
              rest.dropFirst().first == " "
        else {
            return nil
        }
        let text = String(rest.dropFirst(2)).trimmingCharacters(in: .whitespaces)
        guard !text.isEmpty else { return nil }
        return (true, depth, text)
    }

    private static func leadingSpaces(in line: String) -> Int {
        var count = 0
        for character in line {
            if character == " " {
                count += 1
            } else if character == "\t" {
                count += 4
            } else {
                break
            }
        }
        return count
    }
}
