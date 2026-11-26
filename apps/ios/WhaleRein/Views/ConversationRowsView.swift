// Transcript rows for one conversation screen. Kept out of the screen shell so
// each view file stays inside the size budget.

import SwiftUI

struct ConversationRowsView: View {
    let model: ConversationModel
    let reasoningVisible: Bool

    static let bottomAnchor = "conversation.bottom"

    @State private var markedFirstRender = false

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            content
        }
        .onAppear {
            guard !markedFirstRender else { return }
            markedFirstRender = true
            AppSignposts.markFirstConversation()
        }
    }

    @ViewBuilder
    private var content: some View {
        if model.messages.isEmpty, model.livePreview == nil, model.history.complete {
            Text(L10n.conversationEmpty.key)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .center)
                .padding(.vertical, 40)
                .accessibilityIdentifier("conversation.empty")
        }

        ConversationHistoryHeaderView(model: model)

        ForEach(model.messages.map { PresentedMessage(id: model.presentationID(for: $0.messageId), message: $0) }) { row in
            let message = row.message
            MessageRowView(
                model: model,
                message: message,
                reasoningVisible: reasoningVisible
            )
            .transcriptRowPosition(row.id)
            .id(row.id)
        }

        ForEach(model.visibleLiveToolActivities, id: \.callId) { activity in
            LiveToolActivityRow(activity: activity)
        }

        if let preview = model.livePreview {
            LivePreviewRow(text: preview)
        }

        if reasoningVisible, let reasoning = model.reasoningPreviewText {
            ReasoningPreviewRow(text: reasoning)
        }

        QuestionStatusView(model: model)

        ForEach(model.questionBatches) { batch in
            QuestionCardView(model: model, batch: batch)
                .transcriptRowPosition("question-\(batch.batchID)")
                .id("question-\(batch.batchID)")
        }

        QuestionAnswerStatusView(model: model)

        ForEach(model.approvals) { approval in
            ApprovalCardView(model: model, approval: approval)
                .transcriptRowPosition("approval-\(approval.approvalId)")
                .id("approval-\(approval.approvalId)")
        }

        Color.clear.frame(height: 1).id(Self.bottomAnchor)
            .accessibilityHidden(true)
    }
}

private struct PresentedMessage: Identifiable {
    let id: String
    let message: ConversationMessage
}

struct TranscriptGeometry: Equatable {
    var offset: CGFloat = 0
    var extent: CGFloat = 0
    var viewport: CGFloat = 0

    var nearLatest: Bool { extent - offset <= 36 }
    var canScroll: Bool { extent > 1 }
}

struct TranscriptRowsPreference: PreferenceKey {
    static let space = "conversation.viewport"
    static let defaultValue: [TranscriptRowPosition] = []

    static func reduce(value: inout [TranscriptRowPosition], nextValue: () -> [TranscriptRowPosition]) {
        value.append(contentsOf: nextValue())
    }
}

extension View {
    func transcriptRowPosition(_ id: String) -> some View {
        background {
            GeometryReader { proxy in
                let frame = proxy.frame(in: .named(TranscriptRowsPreference.space))
                Color.clear.preference(
                    key: TranscriptRowsPreference.self,
                    value: [TranscriptRowPosition(id: id, top: frame.minY, bottom: frame.maxY)]
                )
            }
        }
    }
}

#if DEBUG
#Preview("Conversation rows fixture") {
    let directory = ScenarioBuilder.temporaryDirectory()
    if let scenario = ScenarioCatalog.scenario(id: "conversation.baseline") {
        ConversationRowsView(
            model: ScenarioBuilder.conversationModel(for: scenario, directory: directory),
            reasoningVisible: false
        )
        .padding()
    }
}
#endif
