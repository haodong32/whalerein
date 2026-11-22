// Question status and answer-status rows shown beside the cards.

import SwiftUI

struct QuestionStatusView: View {
    @Bindable var model: ConversationModel

    private var status: (key: L10nKey, symbol: String)? {
        switch model.questions.freshness {
        case .loading:
            return (.questionsLoading, "hourglass")
        case .current:
            if model.questions.truncatedForDisplay {
                return (.questionsTooLarge, "tray.full")
            }
            return model.questions.batches.isEmpty ? (.questionsEmpty, "checkmark.circle") : nil
        case .stale:
            return (.questionsStale, "clock.arrow.circlepath")
        case .unavailable:
            return (.questionsUnavailable, "exclamationmark.triangle")
        case .unsupported:
            return (.questionsUnsupported, "questionmark.circle")
        }
    }

    var body: some View {
        if let status {
            HStack(spacing: 6) {
                Image(systemName: status.symbol)
                    .accessibilityHidden(true)
                Text(status.key.key)
                    .font(.caption)
            }
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("questions.status")
            .accessibilityLabel(L10nKey.questionsTitle.key)
            .accessibilityValue(status.key.key)
            if model.questions.truncatedForDisplay {
                Button(L10nKey.questionsLoad.key) {
                    Task { await model.loadTruncatedQuestions() }
                }
                .buttonStyle(.bordered)
                .disabled(model.isLoadingTruncatedQuestions)
                .accessibilityIdentifier("questions.load")
            }
        }
    }
}

/// One pending batch with editable inputs and an explicit review step.

struct QuestionAnswerStatusView: View {
    @Bindable var model: ConversationModel

    private var statusKey: L10nKey? {
        if let key = model.questionErrorKey { return key }
        switch model.questionAnswerState {
        case .idle, .submitting:
            return nil
        case .submitted:
            return .questionsAnswerSubmitted
        case .uncertain:
            return .questionsAnswerUncertain
        case .failed:
            return .questionsAnswerRejected
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let statusKey {
                Text(statusKey.key)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("questions.answerState")
                    .accessibilityLabel(L10nKey.questionsTitle.key)
                    .accessibilityValue(statusKey.key)
            }
            if model.canCancelQuestionAnswer {
                Button(role: .destructive) {
                    Task { await model.cancelQuestionAnswer() }
                } label: {
                    Label(L10nKey.questionsCancelAnswer.key, systemImage: "xmark.circle")
                        .font(.caption)
                }
                .buttonStyle(.bordered)
                .accessibilityIdentifier("questions.cancelAnswer")
                .accessibilityLabel(L10nKey.questionsCancelAnswer.key)
                .accessibilityValue(Text(L10nKey.questionsAnswerUncertain.key))
            }
        }
    }
}
