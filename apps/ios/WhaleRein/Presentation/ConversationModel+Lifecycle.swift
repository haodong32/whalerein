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
        for row in outgoingStore?.attach(self) ?? [] {
            optimisticMessages[row.message.messageId] = row.submission
            state.messages.append(row.message)
        }
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
        // A composer edit or an accepted send while the claim is in flight owns
        // the text now. The stored draft is older than that action and must not
        // resurrect text the user already sent or replaced.
        let editAtStart = draftEditID
        do {
            let claim = try await drafts.claim(hostId: hostId, sessionId: sessionId) { [weak self] editID, persisted in
                self?.applySubmittedDraftClear(editID: editID, persisted: persisted)
            }
            if isClosed {
                try await drafts.release(claim.lease, text: claim.text, editID: claim.editID)
                return
            }
            draftLease = claim.lease
            if draft.isEmpty,
               draftEditID == editAtStart,
               clearedDraftBeforeRestore != claim.editID {
                draft = claim.text
                draftEditID = claim.editID
            }
            clearedDraftBeforeRestore = nil
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

    /// Stops visible rendering and clock-driven updates for a brief interruption.
    /// The healthy watch, its cursor and the event subscription stay alive, so
    /// returning to active renders the current authorized view without a
    /// resubscription or an artificial offline state.
    func pauseForInactivity() {
        guard !isClosed, !isRenderingPaused else { return }
        isRenderingPaused = true
        stopApprovalTimer()
    }

    /// Restores rendering after a brief interruption. This transition performs
    /// no network work: the watch that stayed alive already delivered the
    /// latest authorized view.
    func resumeForInactivity() {
        guard !isClosed, isRenderingPaused, !isSuspended else { return }
        isRenderingPaused = false
        approvalClock = now()
        restartApprovalTimer()
    }

    /// True when the retained watch failed or its consumer ended on its own, so
    /// a foreground return must reconcile it once instead of presenting a dead
    /// conversation. The phase transition itself never sets this.
    var needsReconnectAfterInterruption: Bool {
        !isClosed && (watchEnded || consumeTask == nil)
    }

    /// Persists the draft before an instance switch. Unlike a background entry
    /// this does not stop the watch or pause rendering, so a failed switch
    /// leaves the current conversation live.
    func flushDraftForInstanceSwitch() async {
        guard !isClosed else { return }
        await persistDraftNow()
    }

    /// Persists the draft and suspends the Core watch exactly once for one
    /// actual background entry. The app-level event consumer stays alive, so
    /// the foreground resume starts one watch from the Core cursor without
    /// duplicating subscriptions. Repeated notifications are guarded here as
    /// well as by the ordered scene owner.
    func suspendForBackground() async {
        guard !isClosed, !isSuspended else { return }
        isSuspended = true
        isRenderingPaused = true
        await persistDraftNow()
        stopApprovalTimer()
        wait.close()
        service.stopWatch()
    }

    /// One foreground resume for the selected, authorized session. The watch is
    /// restarted exactly once and continues from Core's retained cursor; the
    /// existing event consumer is reused so no duplicate subscription or
    /// message is created.
    func resumeAfterForeground() async {
        guard !isClosed, isSuspended else { return }
        isSuspended = false
        isRenderingPaused = false
        approvalClock = now()
        restartApprovalTimer()
        watchEnded = false
        errorKey = nil
        lastAppliedSequence = -1
        ensureSubscription()
        guard !isReconnecting else { return }
        isReconnecting = true
        defer { isReconnecting = false }
        await startWatchConnection()
        onReconnected?()
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
        guard !isClosed else { return }
        isClosed = true
        outgoingStore?.detach(self, rows: messages.compactMap { message in
            optimisticMessages[message.messageId].map {
                ConversationOutgoingStore.Row(message: message, submission: $0)
            }
        })
        history.traversal &+= 1
        attachments.cancelAll()
        consumeTask?.cancel()
        consumeTask = nil
        stopApprovalTimer()
        wait.close()
        draftSaveTask?.cancel()
        draftSaveTask = nil
        let text = draft
        let editID = draftEditID
        let hostId = hostId
        let sessionId = sessionId
        let service = service
        let drafts = drafts
        let lease = draftLease
        closeDraftFlushDidComplete = false
        closeDraftFlushErrorKey = nil
        // The coordinator owns ordering and generation fencing. Releasing this
        // lease is a final save only while this conversation still owns the
        // session.
        closeDraftFlushTask = Task { [weak self] in
            do {
                if let lease {
                    try await drafts.release(lease, text: text, editID: editID)
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
