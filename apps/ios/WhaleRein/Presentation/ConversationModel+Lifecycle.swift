// Conversation lifecycle: start, suspend, resume, reconnect and close.

import Foundation

@MainActor
extension ConversationModel {
    // MARK: - Lifecycle

    func start() async {
        guard !isClosed, loadState != .loading, loadState != .loaded else { return }
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
        async let restoredDraft: Void = restoreConversationDraft()
        async let updatedCapabilities: Void = refreshConversationCapabilities(opening: true)
        do {
            try await service.startWatch(hostId: hostId, sessionId: sessionId)
        } catch {
            guard !isClosed else { return }
            let code = (error as? SessionServiceError)?.code ?? .internalError
            state = ConversationReduce.reduce(state, .loadFailed(code))
            errorKey = L10nKey.errorKey(for: code)
            reportFatalIfNeeded(code)
            return
        }
        _ = await (restoredDraft, updatedCapabilities)
        guard !isClosed else { return }
        await queue.refresh()
        // Core retains the original send operation. After a relaunch the app
        // has no in-memory identity, so recover the one unresolved send for
        // this exact scope from that durable journal. Recovery is read-only and
        // never submits the message again.
        await recoverRetainedSendIfNeeded()
    }

    private func restoreConversationDraft() async {
        do {
            let claim = try await drafts.claim(hostId: hostId, sessionId: sessionId)
            if isClosed {
                try await drafts.release(claim.lease, text: claim.text)
                return
            }
            draftLease = claim.lease
            if draft.isEmpty { draft = claim.text }
            draftLoadErrorKey = nil
        } catch {
            draftLoadErrorKey = .errorDraftUnavailable
        }
    }

    func refreshConversationCapabilities(opening: Bool = false) async {
        let runEpoch = authoritativeRunEpoch
        guard var loaded = try? await service.capabilities(hostId: hostId, sessionId: sessionId),
              !isClosed else { return }
        if runEpoch != authoritativeRunEpoch {
            loaded.running = capabilities.running
        }
        applyCapabilities(loaded)
        if opening, loaded.running, loaded.supportsSteer { delivery = .steer }
    }

    /// Flushes the draft while keeping the single observation alive. Core owns
    /// continuity even when iOS suspends the app's network execution.
    func suspendForBackground() async {
        guard !isSuspended else { return }
        isSuspended = true
        await persistDraftNow()
        stopApprovalTimer()
        wait.close()
    }

    func resumeAfterForeground() async {
        guard isSuspended else { return }
        isSuspended = false
        approvalClock = now()
        restartApprovalTimer()
        if watchEnded || consumeTask == nil {
            await reconnectWatch()
        } else {
            await queue.refresh()
        }
    }

    /// Recovery path for a watch that ended on its own. The composer disables
    /// Send while ended and offers this reconnect action.
    func reconnectWatch() async {
        guard !isClosed, !isReconnecting else { return }
        invalidateHistoryTraversal()
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
        history.traversal &+= 1
        attachments.cancelAll()
        consumeTask?.cancel()
        consumeTask = nil
        stopApprovalTimer()
        wait.close()
        draftSaveTask?.cancel()
        draftSaveTask = nil
        let text = draft
        let hostId = hostId
        let sessionId = sessionId
        let service = service
        let lease = draftLease
        closeDraftFlushDidComplete = false
        closeDraftFlushErrorKey = nil
        // The coordinator owns ordering and generation fencing. Releasing this
        // lease is a final save only while this conversation still owns the
        // session.
        Task { [weak self] in
            do {
                if let lease {
                    try await self?.drafts.release(lease, text: text)
                } else {
                    try await service.saveDraft(hostId: hostId, sessionId: sessionId, text: text)
                }
                self?.closeDraftFlushErrorKey = nil
            } catch {
                self?.closeDraftFlushErrorKey = .errorDraftUnavailable
            }
            self?.closeDraftFlushDidComplete = true
        }
        service.stopWatch()
    }
}
