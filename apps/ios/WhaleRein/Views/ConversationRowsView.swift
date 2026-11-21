// Transcript rows for one conversation screen. Kept out of the screen shell so
// each view file stays inside the size budget.

import SwiftUI

struct ConversationRowsView: View {
    let model: ConversationModel
    let reasoningVisible: Bool

    static let bottomAnchor = "conversation.bottom"

    var body: some View {
        if model.messages.isEmpty && model.livePreview == nil {
            Text(L10n.conversationEmpty.key)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .center)
                .padding(.vertical, 40)
                .accessibilityIdentifier("conversation.empty")
        }

        ConversationHistoryHeaderView(model: model)

        ForEach(model.messages) { message in
            MessageRowView(
                model: model,
                message: message,
                reasoningVisible: reasoningVisible
            )
            .id(message.messageId)
        }

        ForEach(model.visibleLiveToolActivities, id: \.callId) { activity in
            LiveToolActivityRow(activity: activity)
                .id("tool.live.\(activity.callId)")
        }

        if let preview = model.livePreview {
            LivePreviewRow(text: preview)
                .id("conversation.previewRow")
        }

        if reasoningVisible, let reasoning = model.reasoningPreviewText {
            ReasoningPreviewRow(text: reasoning)
                .id("conversation.reasoningPreviewRow")
        }

        QuestionStatusView(model: model)

        ForEach(model.questionBatches) { batch in
            QuestionCardView(model: model, batch: batch)
                .id("question-\(batch.batchID)")
        }

        QuestionAnswerStatusView(model: model)

        ForEach(model.approvals) { approval in
            ApprovalCardView(model: model, approval: approval)
                .id("approval-\(approval.approvalId)")
        }

        Color.clear
            .frame(height: 1)
            .id(Self.bottomAnchor)
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
