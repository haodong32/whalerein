// Conversation events behaviour, split out of the model file.

import Foundation

@MainActor
extension ConversationModel {
    // MARK: - Events

    /// Replaces the transcript from a cache snapshot or a fresh watch baseline.
    /// A watch baseline also invalidates the old question observation, while a
    /// cache-only replacement leaves the live question stream in place.
    private func applyReset(
        messages: [ConversationMessage],
        approvals: [Approval],
        resetQuestions: Bool
    ) {
        let carriedOptimistic = optimisticMessages
        let optimisticRows = state.messages.filter {
            optimisticMessages[$0.messageId] != nil
        }
        var previousUserTextCounts: [String: Int] = [:]
        for message in state.messages
        where message.role == .user && optimisticMessages[message.messageId] == nil {
            let text = normalizedText(message.plainText)
            previousUserTextCounts[text, default: 0] += 1
        }
        state.messages = messages
        liveMessageIds = Set(messages.map(\.messageId))
        pagedMessageIds.removeAll()
        historyGeneration &+= 1
        isLoadingOlderHistory = false
        // The new snapshot replaces the transcript, so every paged row is
        // gone. Its continuation, projection and completion flag described
        // the old traversal and must not suppress reaching those older
        // messages from the new snapshot.
        historyContinuation = nil
        historyProjection = nil
        historySource = nil
        historyComplete = false
        historyNeedsFreshTraversal = false
        historyProvenance = nil
        historyErrorKey = nil
        liveToolActivities.removeAll()
        reasoningPreviewText = nil
        trustedPendingApprovalIds.removeAll()
        approvalStatusRefresh.removeAll()
        mergeApprovals(approvals, missingPendingState: .resolvedElsewhere)
        // A reconnect reset can arrive after an accepted send but before
        // its committed echo. Re-attach those optimistic rows instead of
        // making the user's message vanish, unless the snapshot already
        // contains the committed identity/text.
        optimisticMessages = carriedOptimistic.filter { _, pending in
            if let committed = pending.committedMessageId,
               messages.contains(where: { $0.messageId == committed }) {
                return false
            }
            let text = normalizedText(pending.text)
            // Count identical committed user messages: one more than the
            // previous transcript proves this send was already committed.
            // An older message with the same text must not hide a second,
            // distinct send.
            let committedCount = messages.filter {
                $0.role == .user && normalizedText($0.plainText) == text
            }.count
            return committedCount <= (previousUserTextCounts[text] ?? 0)
        }
        for row in optimisticRows where optimisticMessages[row.messageId] != nil {
            if !state.messages.contains(where: { $0.messageId == row.messageId }) {
                state.messages.append(row)
            }
        }
        previewText = nil
        previewAttemptId = nil
        authoritativeRunEpoch += 1
        state = ConversationReduce.reduce(state, .contentLoaded)
        approvalClock = now()
        scroll.reset()
        restartApprovalTimer()

        if resetQuestions {
            questions = .loading
            questionReview = nil
            settledQuestionReviews.removeAll()
            settledQuestionAnswers.removeAll()
            questionErrorKey = nil
            if case .submitted = questionAnswerState {
                questionAnswerState = .idle
            }
        }
    }

    func ensureSubscription() {
        guard consumeTask == nil else { return }
        consumeEvents()
    }

    func consumeEvents() {
        consumeTask?.cancel()
        consumeGeneration += 1
        let generation = consumeGeneration
        eventSubscriptionCount += 1
        let stream = service.makeEventStream()
        consumeTask = Task { [weak self] in
            for await envelope in stream {
                guard let self else { return }
                if Task.isCancelled { return }
                self.handle(envelope)
            }
            guard let self, self.consumeGeneration == generation else { return }
            self.consumeTask = nil
        }
    }

    /// Applies one envelope. Events from another Host/session or an older
    /// sequence are dropped so a late update from a previous watch cannot leak.
    func handle(_ envelope: SessionEventEnvelope) {
        guard envelope.hostId == hostId, envelope.sessionId == sessionId else { return }
        guard envelope.sequence > lastAppliedSequence else { return }
        lastAppliedSequence = envelope.sequence

        switch envelope.event {
        case .reset(let messages, let approvals):
            applyReset(messages: messages, approvals: approvals, resetQuestions: false)

        case .watchReset(let messages, let approvals):
            applyReset(messages: messages, approvals: approvals, resetQuestions: true)

        case .message(let message):
            liveMessageIds.insert(message.messageId)
            pagedMessageIds.remove(message.messageId)
            settleAuthoritativeContent(message)
            // A committed message identity is idempotent: a replay must not
            // touch (or consume) any optimistic row.
            guard !messages.contains(where: { $0.messageId == message.messageId }) else { return }
            let echoIndex = message.role == .user ? reconcileUserEcho(message) : nil
            if let echoIndex {
                state.messages.insert(message, at: min(echoIndex, state.messages.count))
            } else {
                state.messages.append(message)
            }
            if message.role == .assistant {
                previewText = nil
                previewAttemptId = nil
            }
            if scroll.recordIncoming(isOwnMessage: false) {
                requestScroll(animated: false)
            }

        case .replace(let message):
            liveMessageIds.insert(message.messageId)
            pagedMessageIds.remove(message.messageId)
            settleAuthoritativeContent(message)
            if let index = messages.firstIndex(where: { $0.messageId == message.messageId }) {
                state.messages[index] = message
            } else if message.role == .user, let echoIndex = reconcileUserEcho(message) {
                state.messages.insert(message, at: min(echoIndex, state.messages.count))
            } else {
                state.messages.append(message)
            }
            if message.role == .assistant {
                previewText = nil
                previewAttemptId = nil
            }
            if scroll.recordIncoming(isOwnMessage: false) {
                requestScroll(animated: false)
            }

        case .preview(let attemptId, let text, _, _):
            previewAttemptId = attemptId
            previewText = text
            if scroll.recordIncoming(isOwnMessage: false) {
                requestScroll(animated: false)
            }

        case .reasoningPreview(_, let text, _, _):
            reasoningPreviewText = text.isEmpty ? nil : text

        case .toolActivity(let activity):
            if let index = liveToolActivities.firstIndex(where: { $0.callId == activity.callId }) {
                liveToolActivities[index] = activity
            } else {
                liveToolActivities.append(activity)
            }

        case .attemptEnded:
            previewText = nil
            previewAttemptId = nil
            reasoningPreviewText = nil
            authoritativeRunEpoch += 1
            updateCapabilities(running: false)
            Task { await queue.refresh() }

        case .hostConnection:
            // Host connection is authoritative at app level; a session watch
            // neither owns nor overrides it.
            break

        case .capabilities(let capabilities):
            // Any capabilities refresh advances the epoch, including an
            // approval-only or same-running update. A Stop submitted just before
            // such an update is therefore treated as stale on its confirmed
            // return, leaving running/preview for one transient frame; the next
            // authoritative idle state self-corrects it. Narrowing this would
            // require an attempt/run identity on the capabilities event.
            authoritativeRunEpoch += 1
            applyCapabilities(capabilities)

        case .approvals(let approvals):
            mergeApprovals(approvals, missingPendingState: .resolvedElsewhere)
            approvalClock = now()
            restartApprovalTimer()

        case .approvalChanged(let approval):
            guard approval.sessionId == sessionId else { return }
            applyServerApproval(approval)
            approvalClock = now()
            restartApprovalTimer()

        case .questions(let state):
            applyQuestions(state)

        case .commandStatus(let receipt):
            applyCommandStatus(receipt)

        case .error(let code, _):
            if code == .revoked {
                onFatalError?(code)
            } else if code == .notPaired {
                onFatalError?(code)
            } else {
                errorKey = .commonError
            }

        case .watchEnded:
            watchEnded = true
            stopApprovalTimer()
        }
    }

    /// User-initiated content load for a Core view that was too large to inline.
    /// This is not a discovery loop: observation still owns freshness and the
    /// stamp; only the oversized content is paged on demand.
    func loadTruncatedQuestions() async {
        guard questions.truncatedForDisplay, !isLoadingTruncatedQuestions else { return }
        isLoadingTruncatedQuestions = true
        defer { isLoadingTruncatedQuestions = false }
        var collected: [QuestionBatch] = []
        var cursor = ""
        do {
            for _ in 0..<64 {
                let page = try await service.listQuestions(
                    hostId: hostId,
                    sessionId: sessionId,
                    cursor: cursor
                )
                collected.append(contentsOf: page.batches)
                cursor = page.nextCursor
                if cursor.isEmpty { break }
            }
            questions.batches = collected
            questions.batchCount = collected.count
            questions.truncatedForDisplay = false
            questions.error = nil
            questionErrorKey = nil
        } catch {
            questionErrorKey = .questionsUnavailable
        }
    }

    func applyQuestions(_ state: QuestionsState) {
        // Keep edits only for a batch whose exact guard still matches.
        var guards: [String: QuestionGuard] = [:]
        for batch in state.batches {
            guards[batch.batchID] = batch.guardValue
        }
        questionDrafts = questionDrafts.filter { batchId, draft in
            guards[batchId] == draft.guardValue
        }
        // Settled cards belong to one observed guard. A new guard or a removed
        // batch releases its read-only snapshot; the global answer status is
        // kept as the submission confirmation.
        let settledBefore = settledQuestionReviews
        let observedSettled = settledBefore.filter { batchId, review in
            guards[batchId] == review.guardValue
        }
        settledQuestionReviews = observedSettled
        settledQuestionAnswers = settledQuestionAnswers.filter { batchId, _ in
            observedSettled[batchId] != nil
        }
        if let answered = answeredQuestionBatchId,
           settledBefore[answered] != nil,
           observedSettled[answered] == nil {
            if guards[answered] == nil {
                // The answered batch is gone. An uncertain command can no
                // longer target a pending card and must not block new ones;
                // a submitted banner stays as the confirmation.
                switch questionAnswerState {
                case .uncertain:
                    questionAnswerState = .idle
                    answeredQuestionBatchId = nil
                case .submitted:
                    answeredQuestionBatchId = nil
                default:
                    break
                }
            } else {
                // The guard changed, not the batch. An uncertain command still
                // owns its original cancel identity; a submitted receipt no
                // longer describes the new question.
                switch questionAnswerState {
                case .submitted:
                    questionAnswerState = .idle
                    answeredQuestionBatchId = nil
                default:
                    break
                }
            }
        }
        var reviewInvalidated = false
        if let review = questionReview {
            let batchID = review.guardValue.batchID
            switch guards[batchID] {
            case let observed? where observed != review.guardValue:
                questionReview = nil
                questionDrafts[batchID] = nil
                switch questionAnswerState {
                case .submitted:
                    // A newer guard is a different question; return to an
                    // answerable state instead of showing the old receipt.
                    questionAnswerState = .idle
                case .uncertain:
                    // The original answer may still complete; keep its cancel
                    // affordance and let recovery settle it.
                    break
                default:
                    questionErrorKey = .questionsGuardChanged
                    reviewInvalidated = true
                }
            case nil:
                // The Host removed the answered batch. The card is gone, but
                // keep the submitted banner as the answer confirmation.
                questionReview = nil
                questionDrafts[batchID] = nil
                switch questionAnswerState {
                case .submitted, .uncertain:
                    break
                default:
                    questionErrorKey = .questionsGuardChanged
                    reviewInvalidated = true
                }
            default:
                break
            }
        }
        questions = state
        // An answered batch left and a different one is now pending. Do not
        // label the new card with the previous answer's confirmation.
        if questionReview == nil, settledQuestionReviews.isEmpty,
           !state.batches.isEmpty, case .submitted = questionAnswerState {
            questionAnswerState = .idle
        }
        if state.freshness == .current && !reviewInvalidated {
            questionErrorKey = nil
        }
    }
}
