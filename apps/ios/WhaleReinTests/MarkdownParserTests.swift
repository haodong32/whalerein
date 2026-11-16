// Native Markdown segmentation and inline attribution.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Markdown")
struct MarkdownParserTests {
    @Test("parses a single fenced code block with its language")
    func singleCodeBlock() {
        let text = """
        Before text.

        ```swift
        let value = 42
        print(value)
        ```

        After text.
        """
        let segments = MarkdownParser.parse(text)
        #expect(segments.count == 3)
        guard case .text(let before) = segments[0].kind else {
            Issue.record("expected leading text")
            return
        }
        #expect(before.contains("Before text."))
        guard case .code(let language, let content) = segments[1].kind else {
            Issue.record("expected code block")
            return
        }
        #expect(language == "swift")
        #expect(content == "let value = 42\nprint(value)")
        guard case .text(let after) = segments[2].kind else {
            Issue.record("expected trailing text")
            return
        }
        #expect(after.contains("After text."))
    }

    @Test("parses multiple code blocks with stable segment ids")
    func multipleBlocks() {
        let text = """
        ```bash
        echo one
        ```

        between

        ```json
        {"two": 2}
        ```
        """
        let segments = MarkdownParser.parse(text)
        #expect(segments.count == 3)
        #expect(segments.map(\.id) == [0, 1, 2])
        #expect(segments.filter { if case .code = $0.kind { return true } else { return false } }.count == 2)
    }

    @Test("a code block without a language still parses")
    func noLanguage() {
        let segments = MarkdownParser.parse("```\nplain\n```")
        #expect(segments.count == 1)
        guard case .code(let language, let content) = segments[0].kind else {
            Issue.record("expected code block")
            return
        }
        #expect(language == nil)
        #expect(content == "plain")
    }

    @Test("an unterminated fence falls back to text instead of swallowing content")
    func unterminatedFence() {
        let segments = MarkdownParser.parse("text\n```swift\nlet x = 1")
        #expect(segments.count == 2)
        let combined = segments.compactMap { segment -> String? in
            if case .text(let value) = segment.kind { return value }
            return nil
        }.joined(separator: "\n")
        #expect(combined.contains("let x = 1"))
        #expect(combined.contains("```swift"))
    }

    @Test("plain text becomes a single text segment")
    func plainText() {
        let segments = MarkdownParser.parse("Just a sentence with **bold**.")
        #expect(segments.count == 1)
        #expect(segments[0].kind == .text("Just a sentence with **bold**."))
    }

    @Test("empty input produces no segments")
    func empty() {
        #expect(MarkdownParser.parse("").isEmpty)
        #expect(MarkdownParser.parse("\n\n").isEmpty)
    }

    @Test("inline attribution renders bold Markdown without throwing")
    func inlineAttribution() {
        let attributed = MarkdownParser.attributed("A **bold** word")
        #expect(String(attributed.characters).contains("bold"))
    }

    @Test("headings, lists, quotes and rules become block segments")
    func blockStructure() {
        let text = """
        ## Release summary

        The final verification run **passed** with `core`:

        - All unit suites passed.
        - The simulator UI plan completed.
        - [x] Device evidence remains outstanding.

        1. Build the framework.
        2. Run the plan.

        > Review the full log before closing the session.

        ---
        """
        let segments = MarkdownParser.parse(text)
        #expect(segments.count == 6)

        guard case .heading(let level, let heading) = segments[0].kind else {
            Issue.record("expected heading")
            return
        }
        #expect(level == 2)
        #expect(heading == "Release summary")

        guard case .text(let paragraph) = segments[1].kind else {
            Issue.record("expected paragraph")
            return
        }
        #expect(paragraph.contains("**passed**"))
        #expect(paragraph.contains("`core`"))

        guard case .list(let unordered, let items) = segments[2].kind else {
            Issue.record("expected unordered list")
            return
        }
        #expect(!unordered)
        #expect(items.map(\.text) == [
            "All unit suites passed.",
            "The simulator UI plan completed.",
            "[x] Device evidence remains outstanding.",
        ])

        guard case .list(let ordered, let orderedItems) = segments[3].kind else {
            Issue.record("expected ordered list")
            return
        }
        #expect(ordered)
        #expect(orderedItems.count == 2)

        guard case .quote(let quote) = segments[4].kind else {
            Issue.record("expected quote")
            return
        }
        #expect(quote == "Review the full log before closing the session.")

        #expect(segments[5].kind == .rule)
    }

    @Test("nested and wrapped list items keep their depth and text")
    func wrappedLists() {
        let segments = MarkdownParser.parse("""
        - first item that wraps
          onto a second line
          - nested item
        """)
        guard case .list(let ordered, let items) = segments.first?.kind else {
            Issue.record("expected a list")
            return
        }
        #expect(!ordered)
        #expect(items.count == 2)
        #expect(items[0].text == "first item that wraps onto a second line")
        #expect(items[0].depth == 0)
        #expect(items[1].text == "nested item")
        #expect(items[1].depth == 1)
    }

    @Test("blank lines separate paragraphs without leaking block markers")
    func paragraphSeparation() {
        let segments = MarkdownParser.parse("First paragraph.\n\nSecond paragraph.")
        #expect(segments.count == 2)
        #expect(segments[0].kind == .text("First paragraph."))
        #expect(segments[1].kind == .text("Second paragraph."))
    }

    @Test("an ordered list without a blank line still closes the paragraph")
    func paragraphToListWithoutBlankLine() {
        let segments = MarkdownParser.parse("Steps:\n1. one\n2. two")
        #expect(segments.count == 2)
        #expect(segments[0].kind == .text("Steps:"))
        guard case .list(let ordered, let items) = segments[1].kind else {
            Issue.record("expected ordered list")
            return
        }
        #expect(ordered)
        #expect(items.map(\.text) == ["one", "two"])
    }
}
