// Markdown rendering for committed assistant text. Block structure is rendered
// with native SwiftUI views and inline emphasis with AttributedString. Fenced
// code blocks keep a monospaced view with a Copy control; pipe tables render as
// a native grid instead of a wall of literal pipes.

import SwiftUI

struct MarkdownView: View {
    let text: String
    let messageId: String

    var body: some View {
        let segments = MarkdownParser.cached(text)
        return VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(segments.enumerated()), id: \.element.id) { index, segment in
                switch segment.kind {
                case .text(let value):
                    Text(MarkdownParser.attributed(value))
                        .font(.body)
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                case .heading(let level, let value):
                    Text(MarkdownParser.attributed(value))
                        .font(headingFont(level))
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                        .accessibilityAddTraits(.isHeader)
                        .padding(.top, level <= 2 ? 6 : 2)
                case .list(let ordered, let items):
                    MarkdownListView(ordered: ordered, items: items)
                case .quote(let value):
                    HStack(alignment: .top, spacing: 10) {
                        RoundedRectangle(cornerRadius: 1.5)
                            .fill(Color(.tertiaryLabel))
                            .frame(width: 3)
                            .accessibilityHidden(true)
                        Text(MarkdownParser.attributed(value))
                            .font(.body)
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityElement(children: .combine)
                case .rule:
                    Divider()
                        .padding(.vertical, 2)
                        .accessibilityHidden(true)
                case .code(let language, let content):
                    CodeBlockView(
                        content: content,
                        language: language,
                        blockKey: codeKey(index: index, segments: segments)
                    )
                case .table(let headers, let rows):
                    MarkdownTableView(
                        headers: headers,
                        rows: rows,
                        tableKey: tableKey(index: index, segments: segments)
                    )
                }
            }
        }
    }

    /// Code blocks are numbered independently of the surrounding block
    /// structure so adding a heading or list never changes a copy identifier.
    private func codeKey(index: Int, segments: [MarkdownSegment]) -> String {
        let ordinal = segments.prefix(index).reduce(0) { count, segment in
            if case .code = segment.kind { return count + 1 }
            return count
        } + 1
        return "\(messageId)-\(ordinal)"
    }

    private func tableKey(index: Int, segments: [MarkdownSegment]) -> String {
        let ordinal = segments.prefix(index).reduce(0) { count, segment in
            if case .table = segment.kind { return count + 1 }
            return count
        } + 1
        return "\(messageId)-\(ordinal)"
    }

    private func headingFont(_ level: Int) -> Font {
        switch level {
        case 1: return .title2.weight(.bold)
        case 2: return .title3.weight(.bold)
        case 3: return .headline
        default: return .subheadline.weight(.semibold)
        }
    }
}

/// Native grid for GitHub-style pipe tables. Cells keep inline Markdown but the
/// literal `|` markers never reach the transcript.
struct MarkdownTableView: View {
    let headers: [String]
    let rows: [[String]]
    let tableKey: String

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            row(headers, isHeader: true)
            Divider()
            ForEach(Array(rows.enumerated()), id: \.offset) { index, cells in
                row(cells, isHeader: false)
                if index < rows.count - 1 {
                    Divider()
                }
            }
        }
        .padding(10)
        .background(Color(.tertiarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
        .overlay(
            RoundedRectangle(cornerRadius: 10)
                .stroke(Color(.separator), lineWidth: 0.5)
        )
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("markdown.table.\(tableKey)")
    }

    private func row(_ cells: [String], isHeader: Bool) -> some View {
        HStack(alignment: .top, spacing: 12) {
            ForEach(Array(cells.enumerated()), id: \.offset) { _, cell in
                Text(MarkdownParser.attributed(cell))
                    .font(isHeader ? .caption.weight(.semibold) : .callout)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .textSelection(.enabled)
            }
        }
        .padding(.vertical, isHeader ? 6 : 5)
        .accessibilityElement(children: .combine)
    }
}

private struct MarkdownListView: View {
    let ordered: Bool
    let items: [MarkdownListItem]

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(Array(items.enumerated()), id: \.offset) { index, item in
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(verbatim: marker(index: index, depth: item.depth))
                        .font(.body.monospacedDigit())
                        .foregroundStyle(.secondary)
                        .frame(minWidth: 18, alignment: .trailing)
                        .accessibilityHidden(true)
                    Text(MarkdownParser.attributed(item.text))
                        .font(.body)
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .padding(.leading, CGFloat(item.depth) * 16)
                .accessibilityElement(children: .combine)
            }
        }
    }

    private func marker(index: Int, depth: Int) -> String {
        if ordered {
            return "\(index + 1)."
        }
        switch depth {
        case 0: return "•"
        case 1: return "◦"
        default: return "▪"
        }
    }
}

struct CodeBlockView: View {
    let content: String
    let language: String?
    let blockKey: String

    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                Text(verbatim: language ?? L10n.conversationCodeBlock.string)
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(.secondary)
                    .accessibilityLabel(L10n.conversationCodeBlock.key)
                    .accessibilityValue(Text(language ?? L10n.conversationCodePlain.string))
                    .accessibilityIdentifier("code.header.\(blockKey)")
                Spacer()
                Button {
                    PlatformPasteboard.setString(content)
                    copied = true
                } label: {
                    Label(L10n.conversationCopyCode.key, systemImage: "doc.on.doc")
                        .font(.caption)
                }
                .glassButtonStyle()
                .controlSize(.small)
                .accessibilityIdentifier("code.copy.\(blockKey)")
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)

            ScrollView(.horizontal, showsIndicators: false) {
                Text(verbatim: content)
                    .font(.system(.footnote, design: .monospaced))
                    .textSelection(.enabled)
                    .padding(12)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }

            if copied {
                Label(L10n.commonCopied.key, systemImage: "checkmark.circle.fill")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(.horizontal, 12)
                    .padding(.bottom, 8)
                    .accessibilityIdentifier("code.copied")
                    .accessibilityLabel(L10n.commonCopied.key)
            }
        }
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
        .overlay(
            RoundedRectangle(cornerRadius: 10)
                .stroke(Color(.separator), lineWidth: 0.5)
        )
        .accessibilityElement(children: .contain)
    }
}
