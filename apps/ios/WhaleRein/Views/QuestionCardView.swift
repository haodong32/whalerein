// Pending-question presentation. Core owns discovery, freshness and the native
// guard; this view only renders that state and forwards explicit edits and an
// explicitly reviewed answer. There is no discovery timer.

import SwiftUI

/// Freshness / empty status for the question facet. Only a Core `current` view
/// with no batches is presented as "no pending questions".
struct QuestionCardView: View {
    @Bindable var model: ConversationModel
    let batch: QuestionBatch

    private var isReviewing: Bool {
        model.questionReview?.guardValue.batchID == batch.batchID
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Image(systemName: "questionmark.bubble")
                    .foregroundStyle(.tint)
                    .accessibilityHidden(true)
                Text(L10nKey.questionsTitle.key)
                    .font(.headline)
                Spacer()
                Text(String(format: L10nKey.questionsVersion.string, Int64(batch.version)))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("question.card.\(batch.batchID)")
            .accessibilityLabel(L10nKey.questionsTitle.key)
            .accessibilityValue(batch.batchID)

            Text(phaseKey.key)
                .font(.footnote.weight(.semibold))
                .padding(.horizontal, 8)
                .padding(.vertical, 4)
                .background(Color(.tertiarySystemFill), in: Capsule())
                .accessibilityIdentifier("question.phase.\(batch.batchID)")
                .accessibilityValue(phaseKey.key)

            if batch.contentUnavailable {
                Label(L10nKey.questionsContentUnavailable.key, systemImage: "doc.questionmark")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("question.unavailable.\(batch.batchID)")
            } else {
                ForEach(batch.questions) { item in
                    questionItem(item)
                }
            }

            if isReviewing, let review = model.questionReview {
                reviewSection(review)
            } else if batch.canAnswer && model.canInteractWithQuestions {
                Button {
                    _ = model.beginQuestionReview(batchId: batch.batchID)
                } label: {
                    Label(L10nKey.questionsReview.key, systemImage: "checkmark.bubble")
                }
                .buttonStyle(.borderedProminent)
                .accessibilityIdentifier("question.review.\(batch.batchID)")
                .accessibilityLabel(L10nKey.questionsReview.key)
            } else if batch.answerable && !batch.canAnswer {
                Label(L10nKey.questionsPhaseUnavailable.key, systemImage: "xmark.circle")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(14)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 14))
        .overlay(
            RoundedRectangle(cornerRadius: 14)
                .stroke(Color(.separator), lineWidth: 0.5)
        )
    }

    @ViewBuilder
    private func questionItem(_ item: QuestionItem) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            if let header = item.header, !header.isEmpty {
                Text(header)
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
            }
            Text(displayQuestion(item))
                .font(.subheadline.weight(.semibold))
            if let detail = item.detail, !detail.isEmpty {
                Text(detail)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            ForEach(Array(item.options.enumerated()), id: \.element.id) { index, option in
                optionButton(item: item, option: option, index: index)
            }
            TextField(
                L10nKey.questionsCustomPlaceholder.key,
                text: Binding(
                    get: { model.questionCustomText(batchId: batch.batchID, itemId: item.id) },
                    set: { model.setQuestionCustomText(batchId: batch.batchID, itemId: item.id, text: $0) }
                ),
                axis: .vertical
            )
            .lineLimit(1...4)
            .textFieldStyle(.roundedBorder)
            .accessibilityIdentifier("question.custom.\(item.id)")
            .accessibilityLabel(L10nKey.questionsCustomPlaceholder.key)
        }
        .padding(.vertical, 2)
    }

    private func optionButton(item: QuestionItem, option: QuestionOption, index: Int) -> some View {
        let selected = model.questionSelection(batchId: batch.batchID, itemId: item.id)
            .contains(option.label)
        return Button {
            model.toggleQuestionOption(
                batchId: batch.batchID,
                itemId: item.id,
                label: option.label
            )
        } label: {
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: selected ? "checkmark.circle.fill" : "circle")
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 2) {
                    Text(option.label)
                        .font(.subheadline)
                    if let description = option.description, !description.isEmpty {
                        Text(description)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                Spacer(minLength: 0)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("question.option.\(item.id).\(index)")
        .accessibilityLabel(Text(verbatim: "\(item.question) \(option.label)"))
        .accessibilityValue(
            Text(selected ? L10nKey.questionsSelected.string : L10nKey.questionsNotSelected.string)
        )
        .accessibilityAddTraits(selected ? [.isSelected] : [])
    }

    private func reviewSection(_ review: QuestionReview) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Label(L10nKey.questionsReviewTitle.key, systemImage: "checkmark.seal")
                .font(.subheadline.weight(.semibold))
            Text(L10nKey.questionsReviewBody.key)
                .font(.caption)
                .foregroundStyle(.secondary)
            ForEach(review.answers, id: \.id) { answer in
                Text(answerSummary(answer))
                    .font(.footnote)
                    .accessibilityIdentifier("question.review.answer.\(answer.id)")
            }
            HStack(spacing: 10) {
                Button(L10nKey.questionsSubmit.key) {
                    Task { await model.submitQuestionReview() }
                }
                .buttonStyle(.borderedProminent)
                .disabled(model.questionAnswerState == .submitting)
                .accessibilityIdentifier("question.submit.\(batch.batchID)")
                .accessibilityLabel(L10nKey.questionsSubmit.key)

                Button(L10nKey.questionsKeepEditing.key) {
                    model.cancelQuestionReview()
                }
                .buttonStyle(.bordered)
                .accessibilityIdentifier("question.keepEditing.\(batch.batchID)")
                .accessibilityLabel(L10nKey.questionsKeepEditing.key)
            }
        }
        .padding(10)
        .background(Color(.tertiarySystemFill), in: RoundedRectangle(cornerRadius: 10))
        // Keep the section addressable without collapsing its submit/keep-editing
        // children into one accessibility element.
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("question.review.section.\(batch.batchID)")
    }

    /// Answer status banner shared by all cards in the section.
    private var phaseKey: L10nKey {
        switch batch.phase {
        case "QUESTION_OPEN": return .questionsPhaseOpen
        case "QUESTION_CONTINUED": return .questionsPhaseContinued
        default: return .questionsPhaseUnavailable
        }
    }

    private func displayQuestion(_ item: QuestionItem) -> String {
        // Core already normalized native display maps; prefer the current locale
        // only when the Host supplied one, never inventing translated content.
        if let display = item.intent?.displayQuestion {
            for key in Locale.preferredLanguages {
                if let value = display[key], !value.isEmpty { return value }
            }
            let language = String(Locale.current.language.languageCode?.identifier ?? "")
            if let value = display[language], !value.isEmpty { return value }
        }
        return item.question
    }

    private func answerSummary(_ answer: QuestionAnswer) -> String {
        var parts = answer.selected
        if let custom = answer.custom, !custom.isEmpty {
            parts.append(custom)
        }
        let title = batch.questions.first(where: { $0.id == answer.id })?.question ?? answer.id
        return "\(title): \(parts.joined(separator: ", "))"
    }
}

/// Answer/error status for the question section.
