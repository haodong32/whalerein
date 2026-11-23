// Conversation questions behaviour, split out of the model file.

import Foundation

@MainActor
extension ConversationModel {
    // MARK: - Questions

    /// Current edits for one question item. Presentation reads this instead of
    /// owning selection state, so observation updates cannot wipe an edit for an
    /// unchanged batch guard.
    func questionSelection(batchId: String, itemId: String) -> Set<String> {
        questionDrafts[batchId]?.selected[itemId] ?? []
    }

    func questionCustomText(batchId: String, itemId: String) -> String {
        questionDrafts[batchId]?.custom[itemId] ?? ""
    }

    func toggleQuestionOption(batchId: String, itemId: String, label: String) {
        guard let batch = questions.batches.first(where: { $0.batchID == batchId }),
              batch.guardValue.isAnswerablePhase,
              let item = batch.questions.first(where: { $0.id == itemId }),
              item.options.contains(where: { $0.label == label }) else { return }
        var draft = questionDrafts[batchId]
            ?? QuestionDraft(guardValue: batch.guardValue)
        guard draft.guardValue == batch.guardValue else { return }
        var selected = draft.selected[itemId] ?? []
        if item.multiSelect {
            if selected.contains(label) {
                selected.remove(label)
            } else {
                selected.insert(label)
            }
        } else {
            selected = [label]
        }
        draft.selected[itemId] = selected
        questionDrafts[batchId] = draft
        questionErrorKey = nil
    }

    func setQuestionCustomText(batchId: String, itemId: String, text: String) {
        guard let batch = questions.batches.first(where: { $0.batchID == batchId }),
              batch.questions.contains(where: { $0.id == itemId }) else { return }
        var draft = questionDrafts[batchId]
            ?? QuestionDraft(guardValue: batch.guardValue)
        guard draft.guardValue == batch.guardValue else { return }
        draft.custom[itemId] = String(text.prefix(4_000))
        questionDrafts[batchId] = draft
        questionErrorKey = nil
    }

    /// Captures the exact reviewed guard and inputs. This is the explicit phase
    /// review: nothing is submitted until the caller confirms this snapshot.
    func beginQuestionReview(batchId: String) -> QuestionReview? {
        guard !hasUncertainQuestionAnswer else {
            questionErrorKey = .questionsAnswerUncertain
            return nil
        }
        guard canInteractWithQuestions,
              let batch = questions.batches.first(where: { $0.batchID == batchId }) else {
            questionErrorKey = .questionsUnavailable
            return nil
        }
        guard batch.canAnswer else {
            questionErrorKey = .questionsUnavailable
            return nil
        }
        let draft = questionDrafts[batchId]
            ?? QuestionDraft(guardValue: batch.guardValue)
        guard draft.guardValue == batch.guardValue else {
            questionErrorKey = .questionsGuardChanged
            return nil
        }
        var answers: [QuestionAnswer] = []
        for item in batch.questions {
            let allowed = Set(item.options.map(\.label))
            let selected = (draft.selected[item.id] ?? []).filter { allowed.contains($0) }
            let custom = (draft.custom[item.id] ?? "")
                .trimmingCharacters(in: .whitespacesAndNewlines)
            if selected.isEmpty && custom.isEmpty { continue }
            answers.append(
                QuestionAnswer(
                    id: item.id,
                    selected: selected.sorted(),
                    custom: custom.isEmpty ? nil : custom
                )
            )
        }
        guard !answers.isEmpty else {
            questionErrorKey = .questionsSelectAnswer
            return nil
        }
        let review = QuestionReview(guardValue: batch.guardValue, answers: answers)
        questionReview = review
        questionErrorKey = nil
        return review
    }

    func cancelQuestionReview() {
        questionReview = nil
    }

    /// Submit the reviewed snapshot. The task is independent of the visible
    /// observation: closing or replacing the watch never cancels it.
    func submitQuestionReview() async {
        guard let review = questionReview else { return }
        let batchID = review.guardValue.batchID
        // A second tap on the same batch must not clear the in-flight review
        // through the generic guard below or issue a duplicate answer command.
        switch questionAnswerState {
        case .submitting:
            return
        case .submitted, .uncertain:
            if answeredQuestionBatchId == batchID { return }
        case .idle, .failed:
            break
        }
        guard canInteractWithQuestions,
              let batch = questions.batches.first(where: { $0.batchID == batchID }),
              batch.guardValue == review.guardValue else {
            questionReview = nil
            questionAnswerState = .failed
            answeredQuestionBatchId = nil
            questionErrorKey = .questionsGuardChanged
            return
        }
        questionAnswerState = .submitting
        answeredQuestionBatchId = batchID
        questionCancelReceipt = nil
        questionErrorKey = nil
        do {
            let receipt = try await service.answerQuestion(
                hostId: hostId,
                sessionId: sessionId,
                batch: batch,
                answers: review.answers
            )
            switch receipt.status {
            case .uncertain:
                questionErrorKey = nil
                settleQuestionReview(
                    review,
                    batchID: batchID,
                    state: .uncertain(commandId: receipt.commandId)
                )
            case .rejected:
                // The Host refused this exact reviewed snapshot. Drop it so the
                // user must review a fresh answer instead of retrying a guard
                // that is already known to be stale.
                questionReview = nil
                questionAnswerState = .failed
                answeredQuestionBatchId = nil
                questionErrorKey = .questionsAnswerRejected
            default:
                questionErrorKey = nil
                settleQuestionReview(
                    review,
                    batchID: batchID,
                    state: .submitted(receipt.status)
                )
            }
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            questionAnswerState = .failed
            answeredQuestionBatchId = nil
            if code == .conflict || code == .staleApproval {
                // A changed Host guard is never repaired locally. Require a
                // fresh review of the new state before another submission.
                questionReview = nil
                questionErrorKey = .questionsGuardChanged
            } else {
                questionErrorKey = L10nKey.errorKey(for: code)
            }
            reportFatalIfNeeded(code)
        }
    }

    /// Moves a successfully submitted review into the per-batch settled view.
    /// The active editor is released so another batch can be reviewed without
    /// discarding this card.
    private func settleQuestionReview(
        _ review: QuestionReview,
        batchID: String,
        state: QuestionAnswerState
    ) {
        questionAnswerState = state
        answeredQuestionBatchId = batchID
        if questions.batches.contains(where: { $0.batchID == batchID }) {
            settledQuestionReviews[batchID] = review
            settledQuestionAnswers[batchID] = state
        }
        questionReview = nil
    }

    /// Requests cancellation of the one uncertain open answer attempt. When
    /// `false`, the answer may still complete; the UI must not promise a
    /// rollback and the original command identity stays eligible for recovery.
    func cancelQuestionAnswer() async {
        guard case .uncertain(let commandId) = questionAnswerState else { return }
        do {
            let receipt = try await service.requestQuestionAnswerCancel(
                hostId: hostId,
                sessionId: sessionId,
                commandId: commandId
            )
            questionCancelReceipt = receipt
            questionErrorKey = receipt.cancellationRequested
                ? .questionsCancelRequested
                : .questionsCancelUnconfirmed
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            questionErrorKey = L10nKey.errorKey(for: code)
        }
    }

    var canCancelQuestionAnswer: Bool {
        if case .uncertain = questionAnswerState {
            return !isSuspended && !watchEnded
        }
        return false
    }

    /// Keep an in-flight answer and its cancellation request across a watch
    /// replacement: a new observer continues the original attempt recovery.
    func preserveQuestionCancellation() {
        switch questionAnswerState {
        case .submitting, .submitted, .uncertain:
            // The answer task owns these states across a watch replacement.
            // Resetting a submitted review to idle would make its retained
            // card editable again and invite a duplicate answer.
            return
        case .idle, .failed:
            questionAnswerState = .idle
        }
    }
}
