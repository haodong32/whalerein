// Markdown block behavior for tables, the documented link/image policy and the
// parsed-block cache that keeps long transcripts from reparsing every body on
// each render.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Markdown tables and links", .serialized)
struct MarkdownTableTests {
    @Test("a pipe table parses into headers and rows without literal delimiters")
    func tableParsing() {
        let markdown = """
        | Name | Result |
        | --- | --- |
        | Unit | **Passed** |
        | UI | Passed |
        """
        let segments = MarkdownParser.parse(markdown)
        #expect(segments.count == 1)
        guard case .table(let headers, let rows) = segments[0].kind else {
            Issue.record("Expected one table segment")
            return
        }
        #expect(headers == ["Name", "Result"])
        #expect(rows == [["Unit", "**Passed**"], ["UI", "Passed"]])
    }

    @Test("a table followed by a paragraph keeps both blocks")
    func tableAndParagraph() {
        let markdown = """
        | Check | State |
        | :--- | ---: |
        | HTTPS | ok |
        | MQTT | failed |

        After the table.
        """
        let segments = MarkdownParser.parse(markdown)
        #expect(segments.count == 2)
        guard case .table(let headers, let rows) = segments[0].kind,
              case .text(let trailing) = segments[1].kind else {
            Issue.record("Expected table then text")
            return
        }
        #expect(headers == ["Check", "State"])
        #expect(rows.count == 2)
        #expect(trailing == "After the table.")
    }

    @Test("escaped pipes stay inside their cell")
    func escapedPipe() {
        #expect(MarkdownParser.splitTableRow("| a \\| b | c |") == ["a | b", "c"])
    }

    @Test("short rows are padded and long rows are truncated to the header width")
    func raggedRows() {
        let markdown = """
        | A | B | C |
        | --- | --- | --- |
        | 1 |
        | 1 | 2 | 3 | 4 |
        """
        let segments = MarkdownParser.parse(markdown)
        guard case .table(let headers, let rows) = segments[0].kind else {
            Issue.record("Expected a table")
            return
        }
        #expect(headers == ["A", "B", "C"])
        #expect(rows[0] == ["1", "", ""])
        #expect(rows[1] == ["1", "2", "3"])
    }

    @Test("a pipe paragraph without a delimiter row stays plain text")
    func malformedTableStaysText() {
        let segments = MarkdownParser.parse("| not | a table |\njust text")
        #expect(segments.count == 1)
        guard case .text(let text) = segments[0].kind else {
            Issue.record("Expected text")
            return
        }
        #expect(text.contains("|"))
    }

    @Test("only http, https and mailto links are interactive")
    func linkPolicy() {
        #expect(MarkdownParser.isAllowedLink(URL(string: "https://example.com")!))
        #expect(MarkdownParser.isAllowedLink(URL(string: "http://example.com")!))
        #expect(MarkdownParser.isAllowedLink(URL(string: "mailto:dev@example.com")!))
        #expect(!MarkdownParser.isAllowedLink(URL(string: "javascript:alert(1)")!))
        #expect(!MarkdownParser.isAllowedLink(URL(string: "file:///etc/passwd")!))
        #expect(!MarkdownParser.isAllowedLink(URL(string: "whalerein://pair?token=x")!))

        let attributed = MarkdownParser.attributed(
            "see [docs](https://example.com) and [bad](javascript:alert(1))"
        )
        let links = attributed.runs.compactMap(\.link)
        #expect(links.contains(URL(string: "https://example.com")!))
        #expect(!links.contains { $0.scheme?.lowercased() == "javascript" })
        #expect(String(attributed.characters).contains("bad"))
    }

    @Test("remote images become alt text and are never fetched")
    func imagePolicy() {
        let attributed = MarkdownParser.attributed(
            "Before ![a diagram](https://example.com/diagram.png) after"
        )
        let text = String(attributed.characters)
        #expect(text.contains("a diagram"))
        #expect(!text.contains("https://example.com/diagram.png"))
        #expect(attributed.runs.compactMap(\.link).isEmpty)
    }

    @Test("raw HTML is displayed literally and never rendered")
    func htmlPolicy() {
        let attributed = MarkdownParser.attributed("<b>bold</b> and <script>x</script>")
        let text = String(attributed.characters)
        #expect(text.contains("bold"))
        #expect(text.contains("script") || text.contains("<script>"))
    }

    @Test("unclosed markdown constructs fall back to visible text")
    func unclosedConstructs() {
        let unclosedLink = MarkdownParser.attributed("[unclosed](https://example.com")
        #expect(String(unclosedLink.characters).contains("unclosed"))

        let unclosedFence = MarkdownParser.parse("```swift\nlet value = 1")
        #expect(unclosedFence.count == 1)
        guard case .text(let text) = unclosedFence[0].kind else {
            Issue.record("An unterminated fence must not swallow the message")
            return
        }
        #expect(text.contains("let value = 1"))
    }

    @Test("repeated renders use the parsed-block cache")
    func cachedParse() {
        MarkdownParser.resetCacheForTesting()
        let body = "## Heading\n\nA **long** body with a table.\n\n| A | B |\n| --- | --- |\n| 1 | 2 |"

        let first = MarkdownParser.cached(body)
        let second = MarkdownParser.cached(body)

        #expect(first == second)
        let stats = MarkdownParser.cacheStatistics()
        #expect(stats.misses == 1)
        #expect(stats.hits == 1)
    }

    @Test("a 320-message transcript parses each body at most once across renders")
    func transcriptCache() {
        MarkdownParser.resetCacheForTesting()
        let messages = FixtureCatalog.conversation(
            sessionId: "s-001",
            hostId: "studio-mac",
            now: Date(timeIntervalSince1970: 1_800_000_000)
        )
        let bodies = messages.flatMap { $0.blocks.map(\.text) }

        for body in bodies {
            _ = MarkdownParser.cached(body)
        }
        for body in bodies {
            _ = MarkdownParser.cached(body)
        }

        let stats = MarkdownParser.cacheStatistics()
        let unique = Set(bodies).count
        #expect(stats.misses == unique, "Each unique body should parse only once")
        #expect(stats.hits == bodies.count * 2 - unique)
    }

    @Test("a long single-line message stays one text block")
    func longMessageBounded() {
        let body = String(repeating: "word ", count: 20_000)
        let segments = MarkdownParser.parse(body)
        #expect(segments.count == 1)
        guard case .text(let text) = segments[0].kind else {
            Issue.record("Expected one text segment")
            return
        }
        #expect(text.count == body.trimmingCharacters(in: .whitespacesAndNewlines).count)
    }

    @Test("many code blocks parse to exactly one segment each with stable ids")
    func manyCodeBlocks() {
        let body = (1...500)
            .map { "```swift\nlet value = \($0)\n```" }
            .joined(separator: "\n")
        let segments = MarkdownParser.parse(body)
        #expect(segments.count == 500)
        #expect(segments.enumerated().allSatisfy { $0.element.id == $0.offset })
    }
}
