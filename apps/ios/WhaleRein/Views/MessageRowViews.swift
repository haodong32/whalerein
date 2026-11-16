// Typed native rows for user, assistant, tool, notice and question content.
// Stable identifiers come from message and block IDs; expansion state lives in
// the conversation model so streaming updates do not collapse open details.

import SwiftUI

struct MessageRowView: View {
    @Bindable var model: ConversationModel
    let message: ConversationMessage

    var body: some View {
        Group {
            switch message.role {
            case .user:
                userRow
            case .notice:
                noticeRow
            case .tool:
                toolRowContent
            default:
                assistantRow
            }
        }
        .frame(maxWidth: .infinity, alignment: message.role == .user ? .trailing : .leading)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("message.\(message.messageId)")
    }

    private var userRow: some View {
        VStack(alignment: .trailing, spacing: 4) {
            ForEach(message.blocks) { block in
                Text(verbatim: block.text)
                    .font(.body)
                    .textSelection(.enabled)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
                    .background(Color.accentColor.opacity(0.14), in: RoundedRectangle(cornerRadius: 16))
                    .accessibilityLabel(Text(block.text))
            }
        }
        .frame(maxWidth: 560, alignment: .trailing)
    }

    private var assistantRow: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(message.blocks) { block in
                switch block.kind {
                case .text:
                    MarkdownView(text: block.text, messageId: message.messageId)
                case .tool:
                    ToolBlockView(
                        model: model,
                        block: block
                    )
                case .notice:
                    noticeContent(block.text)
                case .question:
                    questionContent(block.text)
                }
            }
        }
        .frame(maxWidth: 620, alignment: .leading)
    }

    private var noticeRow: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(message.blocks) { block in
                noticeContent(block.text)
            }
        }
    }

    private var toolRowContent: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(message.blocks) { block in
                ToolBlockView(model: model, block: block)
            }
        }
    }

    private func noticeContent(_ text: String) -> some View {
        Label {
            Text(verbatim: text)
                .font(.footnote)
                .foregroundStyle(.secondary)
        } icon: {
            Image(systemName: "info.circle")
                .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .combine)
    }

    private func questionContent(_ text: String) -> some View {
        Label {
            Text(verbatim: text)
                .font(.callout)
        } icon: {
            Image(systemName: "questionmark.bubble")
                .foregroundStyle(.tint)
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
    }
}

struct ToolBlockView: View {
    @Bindable var model: ConversationModel
    let block: ConversationBlock
    @AccessibilityFocusState private var detailsFocused: Bool

    private var expanded: Bool { model.isExpanded(block.blockId) }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Label {
                    Text(verbatim: block.toolName ?? L10n.conversationToolResult.string)
                        .font(.subheadline.weight(.semibold))
                } icon: {
                    Image(systemName: "wrench.and.screwdriver")
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 8)
                Button {
                    model.toggleExpansion(block.blockId)
                } label: {
                    Text(expanded ? L10n.conversationHideDetails.key : L10n.conversationShowDetails.key)
                        .font(.caption)
                }
                .glassButtonStyle()
                .controlSize(.small)
                .accessibilityIdentifier("tool.expand.\(block.blockId)")
                .accessibilityValue(Text(expanded ? L10n.conversationHideDetails.key : L10n.conversationShowDetails.key))
                .accessibilityHint(L10n.conversationToolOutput.key)
            }

            if expanded {
                ScrollView(.horizontal, showsIndicators: false) {
                    Text(verbatim: block.text)
                        .font(.system(.footnote, design: .monospaced))
                        .textSelection(.enabled)
                        .padding(10)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .accessibilityLabel(L10n.conversationToolOutput.key)
                        .accessibilityIdentifier("tool.output.\(block.blockId)")
                        .accessibilityFocused($detailsFocused)
                        .onAppear {
                            // Move VoiceOver focus to the expanded output so a
                            // long tool result is announced instead of leaving
                            // focus on the toggle it just activated.
                            detailsFocused = true
                        }
                }
                .background(Color(.tertiarySystemBackground), in: RoundedRectangle(cornerRadius: 8))
                .accessibilityIdentifier("tool.details.\(block.blockId)")
            } else {
                Text(verbatim: block.text)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                    .truncationMode(.tail)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12))
        .accessibilityElement(children: .contain)
    }
}

struct LivePreviewRow: View {
    let text: String

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                ProgressView()
                    .controlSize(.small)
                Text(L10n.conversationPreview.key)
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
            }
            Text(verbatim: text)
                .font(.body)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12))
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("conversation.preview")
        .accessibilityLabel(Text(L10n.conversationPreview.string))
        .accessibilityValue(Text(text))
    }
}
