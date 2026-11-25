// Committed-history traversal: older pages, fresh traversal and exact reads.
//
// Kept out of the model file so the facade stays navigable; it still mutates
// the one conversation state through the properties declared there.

import Foundation

@MainActor
extension ConversationModel {
    // MARK: - Committed history

    var history: HistoryFacet {
        get { state.historyFacet }
        set { state.historyFacet = newValue }
    }

    var canLoadOlderHistory: Bool {
        !isClosed
            && historyService != nil
            && capabilities.supportsHistoryPage
            && loadState == .loaded
            && !history.complete
            && !history.loading
            && !history.needsFreshTraversal
            && history.continuation != nil
    }

    var canLoadFreshHistory: Bool {
        capabilities.supportsHistoryPage
            && history.needsFreshTraversal
            && !history.loading
            && loadState == .loaded
    }

    var historyProvenanceKey: L10nKey? {
        switch history.provenance {
        case .online: return .conversationHistoryOnline
        case .cache: return .conversationHistoryCache
        case .unavailable: return .conversationHistoryUnavailable
        case nil: return nil
        }
    }

    /// Live tool previews stay until their authoritative committed block
    /// arrives; a committed identity always wins.
    var visibleLiveToolActivities: [ToolActivity] {
        let committed = Set(
            messages.flatMap { message in
                message.blocks.compactMap { $0.toolActivity?.callId }
            }
        )
        return liveToolActivities.filter { !committed.contains($0.callId) }
    }

    var showsHistoryContinuation: Bool {
        !history.canScroll && history.didAutoFill && canLoadOlderHistory && history.errorKey == nil
    }

    func updateHistoryViewport(canScroll: Bool) async {
        if history.canScroll != canScroll { history.canScroll = canScroll }
        guard !canScroll, !history.didAutoFill, canLoadOlderHistory, history.errorKey == nil else { return }
        history.didAutoFill = true
        await loadOlderHistory()
    }

    /// Loads one continuation from the currently accepted observation. A
    /// change of source or cut invalidates the outstanding traversal.
    func loadOlderHistory() async {
        guard canLoadOlderHistory,
              let source = history.source, let continuation = history.continuation,
              let historyAPI = historyService else { return }
        let generation = history.traversal
        state = ConversationReduce.reduce(state, .olderHistoryStarted)
        defer {
            if generation == history.traversal {
                state = ConversationReduce.reduce(state, .olderHistoryFinished)
            }
        }
        do {
            let page = try await historyAPI.historyPage(
                hostId: hostId,
                sessionId: sessionId,
                source: source,
                continuation: continuation,
                maxMessages: ConversationHistoryLimit.messages,
                maxBytes: ConversationHistoryLimit.bytes
            )
            guard generation == history.traversal, !isClosed else { return }
            guard page.projection == history.projection, page.source == source else {
                throw SessionServiceError(.staleHistory)
            }
            guard page.complete || (page.continuation != nil && page.continuation != continuation) else {
                throw SessionServiceError(.unavailable)
            }
            applyHistoryPage(page)
        } catch {
            guard generation == history.traversal, !isClosed else { return }
            handleHistoryFailure(error)
            if history.needsFreshTraversal, !history.didAutomaticallyRebase {
                history.didAutomaticallyRebase = true
                history.loading = false
                await loadFreshHistory()
            }
        }
    }

    func loadFreshHistory() async {
        guard canLoadFreshHistory, !isReconnecting else { return }
        invalidateHistoryTraversal()
        history.errorKey = nil
        await restartWatch()
        if watchEnded {
            history.needsFreshTraversal = true
            history.errorKey = errorKey ?? .commonError
        }
    }

    func invalidateHistoryTraversal() {
        state = ConversationReduce.reduce(state, .historyTraversalInvalidated)
    }

    func applyHistorySnapshot(_ page: ConversationHistoryPage, replacing requestedReplacement: Bool) {
        if requestedReplacement, page.source == .cache, loadState == .loaded {
            // A re-opened observation cannot prove continuity with the
            // existing display from a cached seed. Keep the readable window
            // until the new observation validates its online baseline.
            history.awaitsOnlineReplacement = true
            return
        }
        let replacing = requestedReplacement || history.awaitsOnlineReplacement
        history.awaitsOnlineReplacement = false
        let previousAnchor = scroll.anchorMessageId
        let replacesCurrentWindow = replacing && loadState == .loaded
        let pageIDs = Set(page.messages.map(\.messageId))
        let startsTraversal = replacing
            || history.source != page.source
            || history.projection != page.projection
            || history.baselineContinuation != page.continuation
        if startsTraversal { invalidateHistoryTraversal() }

        if replacing {
            previewText = nil
            previewAttemptId = nil
            reasoningPreviewText = nil
            liveToolActivities.removeAll()
            liveMessageIds = page.source == .online ? pageIDs : []
            // Prefer the observable order over comparing the device and Host
            // clocks when the replacement still contains a following row.
            var successorID: String?
            var pendingSuccessors: [String: String] = [:]
            let previouslyVisible = state.messages
            for row in previouslyVisible.reversed() {
                if pageIDs.contains(row.messageId) {
                    successorID = row.messageId
                } else if optimisticMessages[row.messageId] != nil, let successorID {
                    pendingSuccessors[row.messageId] = successorID
                }
            }
            for message in page.messages {
                reconcileUserEcho(message)
            }
            state.messages = page.messages
            for pending in previouslyVisible {
                guard let submission = optimisticMessages[pending.messageId] else { continue }
                let successor = pendingSuccessors[pending.messageId].flatMap { id in
                    state.messages.firstIndex { $0.messageId == id }
                }
                let inferred = submission.queuedBehindRun ? nil : state.messages.firstIndex {
                    $0.createdAtUnixMs > pending.createdAtUnixMs
                }
                let next = successor ?? inferred ?? state.messages.endIndex
                optimisticMessages[pending.messageId]?.preservesPosition =
                    !submission.queuedBehindRun && next != state.messages.endIndex
                state.messages.insert(pending, at: next)
            }
            committedPresentationIDs = committedPresentationIDs.filter { pageIDs.contains($0.key) }
        } else {
            let currentIDs = Set(messages.map(\.messageId))
            // Core has certified the two windows share coverage. Anchor
            // insertions to their nearest successor already in the window.
            var successorID: String?
            for message in page.messages.reversed() {
                if currentIDs.contains(message.messageId) {
                    if page.source == .online, !liveMessageIds.contains(message.messageId),
                       let index = messages.firstIndex(where: { $0.messageId == message.messageId }) {
                        state.messages[index] = message
                    }
                    successorID = message.messageId
                } else {
                    reconcileUserEcho(message)
                    if let successorID, let index = messages.firstIndex(where: { $0.messageId == successorID }) {
                        state.messages.insert(message, at: index)
                    } else {
                        state.messages.append(message)
                    }
                    successorID = message.messageId
                }
            }
            if page.source == .online {
                liveMessageIds.formUnion(pageIDs)
            }
        }

        history.projection = page.projection
        history.source = page.source
        history.provenance = page.source == .cache ? .cache : .online
        if startsTraversal {
            history.complete = page.complete
            history.continuation = page.complete ? nil : page.continuation
            history.baselineContinuation = page.continuation
        }
        history.errorKey = nil
        history.needsFreshTraversal = false
        errorKey = nil
        state = ConversationReduce.reduce(state, .contentLoaded)
        if replacesCurrentWindow, !scroll.isAtLatest,
           !containsTranscriptRow(previousAnchor) {
            history.positionChanged = true
            jumpToLatest()
        } else {
            scroll.markLayoutChange()
        }
    }

    private func containsTranscriptRow(_ id: String?) -> Bool {
        guard let id else { return false }
        return messages.contains { presentationID(for: $0.messageId) == id }
            || questionBatches.contains { "question-\($0.batchID)" == id }
            || approvals.contains { "approval-\($0.approvalId)" == id }
    }

    /// Resolve one exact scoped committed message. Stale or foreign content is
    /// never applied: the model offers a fresh traversal instead.
    func rereadMessage(_ reference: MessageReference) async {
        guard reference.hostId == hostId, reference.sessionId == sessionId else {
            history.errorKey = .errorSecurity
            return
        }
        guard capabilities.supportsHistoryExact,
              let historyAPI = historyService else {
            history.errorKey = .errorUnsupportedCapability
            return
        }
        if let projection = history.projection, reference.projectionId != projection {
            history.needsFreshTraversal = true
            history.errorKey = nil
            return
        }
        let generation = history.traversal
        do {
            let message = try await historyAPI.historyMessage(reference)
            // A watch reset replaces the transcript while this read is in
            // flight. The old projection must not be restored into the new
            // snapshot.
            guard !isClosed,
                  generation == history.traversal,
                  reference.hostId == hostId,
                  reference.sessionId == sessionId else { return }
            if let index = messages.firstIndex(where: { $0.messageId == message.messageId }) {
                state.messages[index] = message
            } else {
                state.messages.append(message)
            }
            liveMessageIds.insert(message.messageId)
            settleAuthoritativeContent(message)
            history.errorKey = nil
        } catch {
            guard !isClosed, generation == history.traversal else { return }
            handleHistoryFailure(error)
        }
    }

    private func applyHistoryPage(_ page: ConversationHistoryPage) {
        var existing = Set(messages.map(\.messageId))
        let prepended = page.messages.filter { existing.insert($0.messageId).inserted }
        for message in prepended {
            reconcileUserEcho(message)
        }
        if !prepended.isEmpty {
            state.messages.insert(contentsOf: prepended, at: 0)
        }
        history.continuation = page.complete ? nil : page.continuation
        history.complete = page.complete
        history.errorKey = nil
        history.didAutomaticallyRebase = false
    }

    func handleHistoryFailure(_ error: Error) {
        let code = (error as? SessionServiceError)?.code ?? .internalError
        if code == .staleHistory {
            history.needsFreshTraversal = true
            history.continuation = nil
            history.errorKey = history.didAutomaticallyRebase ? .conversationHistoryChanged : nil
            return
        }
        history.errorKey = L10nKey.errorKey(for: code)
    }

    /// Settles any live preview whose authoritative committed identity arrived.
    func settleAuthoritativeContent(_ message: ConversationMessage) {
        let settledCallIds = Set(
            message.blocks.compactMap { $0.toolActivity?.callId }
        )
        if !settledCallIds.isEmpty {
            liveToolActivities.removeAll { settledCallIds.contains($0.callId) }
        }
        if message.role == .assistant {
            previewText = nil
            previewAttemptId = nil
            reasoningPreviewText = nil
        }
    }
}
