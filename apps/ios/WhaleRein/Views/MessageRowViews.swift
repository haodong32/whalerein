// Typed native rows for user, assistant, tool, reasoning, notice, attachment
// and question content. Stable identifiers come from message and block IDs;
// expansion state lives in the conversation model so streaming updates do not
// collapse open details. Reasoning is hidden unless the App-section preference
// is on.

import SwiftUI

struct MessageRowView: View {
    @Bindable var model: ConversationModel
    let message: ConversationMessage
    var reasoningVisible: Bool = false

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
            ForEach(
                ConversationContent.visibleBlocks(
                    message.blocks,
                    reasoningVisible: reasoningVisible
                )
            ) { block in
                blockContent(block)
            }
        }
        .frame(maxWidth: 620, alignment: .leading)
    }

    @ViewBuilder
    private func blockContent(_ block: ConversationBlock) -> some View {
        switch block.kind {
        case .text:
            MarkdownView(text: block.text, messageId: message.messageId)
        case .reasoning:
            if reasoningVisible {
                ReasoningBlockView(block: block)
            }
        case .tool:
            ToolBlockView(model: model, block: block)
        case .notice:
            noticeContent(block.text)
        case .attachment:
            if let attachment = block.attachment {
                AttachmentBlockView(
                    model: model,
                    attachment: attachment,
                    hostId: model.hostId,
                    sessionId: model.sessionId
                )
            }
        case .question:
            questionContent(block.text)
        }
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

struct ReasoningBlockView: View {
    let block: ConversationBlock

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label(L10n.conversationReasoning.key, systemImage: "brain")
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
            Text(verbatim: block.text)
                .font(.callout)
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            Color(.secondarySystemBackground),
            in: RoundedRectangle(cornerRadius: 10)
        )
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("reasoning.\(block.blockId)")
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
                    Text(verbatim: activity?.name ?? block.toolName ?? L10n.conversationToolResult.string)
                        .font(.subheadline.weight(.semibold))
                } icon: {
                    Image(systemName: "wrench.and.screwdriver")
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 8)
                if let activity {
                    Text(Self.stateKey(for: activity.state).key)
                        .font(.caption)
                        .foregroundStyle(activity.state == .failed ? .red : .secondary)
                        .accessibilityIdentifier("tool.state.\(block.blockId)")
                }
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

            if let activity {
                if !activity.stages.isEmpty {
                    Text(verbatim: activity.stages.joined(separator: " · "))
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("tool.stages.\(block.blockId)")
                }
                if let failure = activity.failureCode {
                    Text(verbatim: failure)
                        .font(.caption2.monospaced())
                        .foregroundStyle(.red)
                        .accessibilityIdentifier("tool.failure.\(block.blockId)")
                }
                if let progress = activity.progress, !progress.isEmpty, activity.result.isEmpty {
                    Text(verbatim: progress)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("tool.progress.\(block.blockId)")
                }
            }

            if expanded {
                ScrollView(.horizontal, showsIndicators: false) {
                    VStack(alignment: .leading, spacing: 8) {
                        if let activity, !activity.result.isEmpty {
                            ForEach(Array(activity.result.enumerated()), id: \.offset) { _, part in
                                switch part {
                                case .text(let text):
                                    Text(verbatim: text)
                                        .font(.system(.footnote, design: .monospaced))
                                        .textSelection(.enabled)
                                case .attachment(let attachment):
                                    AttachmentBlockView(
                                        model: model,
                                        attachment: attachment,
                                        hostId: model.hostId,
                                        sessionId: model.sessionId
                                    )
                                }
                            }
                        } else {
                            Text(verbatim: block.text)
                                .font(.system(.footnote, design: .monospaced))
                                .textSelection(.enabled)
                        }
                    }
                    .padding(10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .accessibilityLabel(L10n.conversationToolOutput.key)
                    .accessibilityIdentifier("tool.output.\(block.blockId)")
                    .accessibilityFocused($detailsFocused)
                    .onAppear {
                        detailsFocused = true
                    }
                }
                .background(Color(.tertiarySystemBackground), in: RoundedRectangle(cornerRadius: 8))
                .accessibilityIdentifier("tool.details.\(block.blockId)")
            } else {
                Text(verbatim: activity?.displayText ?? block.text)
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

    private var activity: ToolActivity? { block.toolActivity }

    static func stateKey(for state: ToolState) -> L10nKey {
        switch state {
        case .preparing: return .conversationToolPreparing
        case .running: return .conversationToolRunning
        case .succeeded: return .conversationToolSucceeded
        case .failed: return .conversationToolFailed
        case .interrupted: return .conversationToolInterrupted
        case .abandoned: return .conversationToolAbandoned
        case .unknown: return .conversationToolUnknown
        }
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

struct ReasoningPreviewRow: View {
    let text: String

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label(L10n.conversationReasoningPreview.key, systemImage: "brain")
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
            Text(verbatim: text)
                .font(.callout)
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12))
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("conversation.reasoningPreview")
        .accessibilityValue(Text(text))
    }
}

struct LiveToolActivityRow: View {
    let activity: ToolActivity

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 8) {
                ProgressView()
                    .controlSize(.small)
                Text(verbatim: activity.name)
                    .font(.subheadline.weight(.semibold))
                Spacer(minLength: 8)
                Text(ToolBlockView.stateKey(for: activity.state).key)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            if let summary = activity.summary ?? activity.progress {
                Text(verbatim: summary)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12))
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("tool.live.\(activity.callId)")
    }
}
