// Conversation lifecycle: start, suspend, resume, reconnect and close.

import Foundation

@MainActor
extension ConversationModel {
    // MARK: - Lifecycle

    func start() async {
        guard loadState != .loading, loadState != .loaded else { return }
        state = ConversationReduce.reduce(state, .loadStarted)
        errorKey = nil
        watchEnded = false
        lastAppliedSequence = -1
        approvalClock = now()
        questions = .loading
        questionReview = nil
        questionErrorKey = nil
        questionAnswerState = .idle
        questionCancelReceipt = nil
        ensureSubscription()
        // A draft read is an enhancement, not a precondition for the watch. A
        // storage failure must never leave the user with a conversation that
        // never opens.
        do {
            draft = try await service.loadDraft(hostId: hostId, sessionId: sessionId) ?? ""
            draftLoadErrorKey = nil
        } catch {
            draftLoadErrorKey = .errorDraftUnavailable
        }
        do {
            let loaded = try await service.capabilities(hostId: hostId, sessionId: sessionId)
            state = ConversationReduce.reduce(state, .capabilitiesLoaded(loaded))
            if capabilities.running {
                delivery = .steer
            }
            queue.applyCapabilities(capabilities)
            try await service.startWatch(hostId: hostId, sessionId: sessionId)
            await queue.refresh()
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            state = ConversationReduce.reduce(state, .loadFailed(code))
            errorKey = L10nKey.errorKey(for: code)
            reportFatalIfNeeded(code)
            return
        }
        // Core retains the original send operation. After a relaunch the app
        // has no in-memory identity, so recover the one unresolved send for
        // this exact scope from that durable journal. Recovery is read-only and
        // never submits the message again.
        await recoverRetainedSendIfNeeded()
    }

    /// Flushes the pending draft and stops the watch before the app is
    /// backgrounded. The local transcript is kept so a resume can render it
    /// immediately while the fresh watch resets it.
    func suspendForBackground() async {
        guard !isSuspended else { return }
        isSuspended = true
        await persistDraftNow()
        stopApprovalTimer()
        consumeTask?.cancel()
        consumeTask = nil
        wait.close()
        service.stopWatch()
    }

    /// Restarts the watch and resyncs the visible session after returning to the
    /// foreground, instead of leaving a half-open stream and stale rows.
    func resumeAfterForeground() async {
        guard isSuspended else { return }
        isSuspended = false
        watchEnded = false
        errorKey = nil
        lastAppliedSequence = -1
        approvalClock = now()
        questions = .loading
        questionReview = nil
        questionErrorKey = nil
        preserveQuestionCancellation()
        if consumeTask == nil {
            ensureSubscription()
        }
        await restartWatch()
    }

    /// Recovery path for a watch that ended on its own. The composer disables
    /// Send while ended and offers this reconnect action.
    func reconnectWatch() async {
        guard !isReconnecting else { return }
        watchEnded = false
        errorKey = nil
        lastAppliedSequence = -1
        approvalClock = now()
        questions = .loading
        questionReview = nil
        questionErrorKey = nil
        preserveQuestionCancellation()
        consumeTask?.cancel()
        consumeTask = nil
        ensureSubscription()
        await restartWatch()
        onReconnected?()
    }

    func close() {
        isClosed = true
        historyGeneration &+= 1
        attachments.cancelAll()
        consumeTask?.cancel()
        consumeTask = nil
        stopApprovalTimer()
        wait.close()
        draftSaveTask?.cancel()
        draftSaveTask = nil
        closeDraftFlushDidComplete = false
        closeDraftFlushErrorKey = nil
        let text = draft
        let hostId = hostId
        let sessionId = sessionId
        let service = service
        // A draft is only 120 ms behind the keyboard. Flush it synchronously
        // enough that switching sessions, leaving the app or killing it does
        // not lose the last edit. The outcome is observable so a failed flush
        // is not reported as a saved draft.
        Task { [weak self] in
            do {
                try await service.saveDraft(hostId: hostId, sessionId: sessionId, text: text)
                self?.closeDraftFlushErrorKey = nil
            } catch {
                self?.closeDraftFlushErrorKey = .errorDraftUnavailable
            }
            self?.closeDraftFlushDidComplete = true
        }
        service.stopWatch()
    }
}
