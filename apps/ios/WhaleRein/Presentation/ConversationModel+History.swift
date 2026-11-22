// Committed-history traversal: older pages, fresh traversal and exact reads.
//
// Kept out of the model file so the facade stays navigable; it still mutates
// the one conversation state through the properties declared there.

import Foundation

@MainActor
extension ConversationModel {
    // MARK: - Committed history

    var canLoadOlderHistory: Bool {
        capabilities.supportsHistoryPage
            && loadState == .loaded
            && !historyComplete
            && !isLoadingOlderHistory
            && !historyNeedsFreshTraversal
    }

    var canLoadFreshHistory: Bool {
        capabilities.supportsHistoryPage
            && historyNeedsFreshTraversal
            && !isLoadingOlderHistory
            && loadState == .loaded
    }

    var historyProvenanceKey: L10nKey? {
        switch historyProvenance {
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

    /// Loads one older bounded page. `cacheFallback` explicitly serves the
    /// encrypted local cache when the Host is unreachable, with cache
    /// provenance shown to the reader.
    func loadOlderHistory(cacheFallback: Bool = false) async {
        guard canLoadOlderHistory,
              let history = service as? any ConversationHistoryService else {
            if !capabilities.supportsHistoryPage {
                historyErrorKey = .errorUnsupportedCapability
            }
            return
        }
        let useCache = cacheFallback || historySource == .cache || !connection.isOnline
        // Switching source requires a fresh traversal; a continuation belongs
        // to one source, projection and scope.
        if let historySource, historySource != (useCache ? .cache : .online) {
            historyContinuation = nil
            historyProjection = nil
        }
        let generation = historyGeneration
        let anchor = scroll.anchorMessageId ?? messages.first?.messageId
        scroll.capturePrependAnchor(anchor)
        isLoadingOlderHistory = true
        historyErrorKey = nil
        defer {
            if generation == historyGeneration {
                isLoadingOlderHistory = false
            }
        }
        do {
            let page = try await history.historyPage(
                hostId: hostId,
                sessionId: sessionId,
                source: useCache ? .cache : .online,
                continuation: historyContinuation,
                maxMessages: historyPageMessages,
                maxBytes: historyPageBytes
            )
            // A replaced session/instance invalidates this late page.
            guard generation == historyGeneration, !isClosed else { return }
            applyHistoryPage(page, anchor: anchor)
        } catch {
            guard generation == historyGeneration, !isClosed else { return }
            handleHistoryFailure(error)
        }
    }

    /// Drops paged-only rows and starts a fresh latest-first traversal after a
    /// stale continuation or content projection. Live rows are preserved.
    func loadFreshHistory() async {
        guard canLoadFreshHistory else { return }
        historyGeneration &+= 1
        state.messages.removeAll { message in
            pagedMessageIds.contains(message.messageId)
                && !liveMessageIds.contains(message.messageId)
        }
        pagedMessageIds.removeAll()
        historyContinuation = nil
        historyProjection = nil
        historySource = nil
        historyComplete = false
        historyNeedsFreshTraversal = false
        historyErrorKey = nil
        await loadOlderHistory()
    }

    /// Resolve one exact scoped committed message. Stale or foreign content is
    /// never applied: the model offers a fresh traversal instead.
    func rereadMessage(_ reference: MessageReference) async {
        guard reference.hostId == hostId, reference.sessionId == sessionId else {
            historyErrorKey = .errorSecurity
            return
        }
        guard capabilities.supportsHistoryExact,
              let history = service as? any ConversationHistoryService else {
            historyErrorKey = .errorUnsupportedCapability
            return
        }
        if let historyProjection, reference.projectionId != historyProjection {
            historyNeedsFreshTraversal = true
            historyErrorKey = nil
            return
        }
        do {
            let message = try await history.historyMessage(reference)
            guard !isClosed,
                  reference.hostId == hostId,
                  reference.sessionId == sessionId else { return }
            if let index = messages.firstIndex(where: { $0.messageId == message.messageId }) {
                state.messages[index] = message
            } else {
                state.messages.append(message)
            }
            liveMessageIds.insert(message.messageId)
            pagedMessageIds.remove(message.messageId)
            settleAuthoritativeContent(message)
            historyErrorKey = nil
        } catch {
            handleHistoryFailure(error)
        }
    }

    func applyHistoryPage(_ page: ConversationHistoryPage, anchor: String?) {
        if let historyProjection, historyProjection != page.projection {
            historyNeedsFreshTraversal = true
            historyContinuation = nil
            return
        }
        historyProjection = page.projection
        historySource = page.source
        historyProvenance = page.source == .cache
            ? .cache
            : (connection.isOnline ? .online : .unavailable)
        let existing = Set(messages.map(\.messageId))
        var prepended: [ConversationMessage] = []
        for message in page.messages where !existing.contains(message.messageId) {
            prepended.append(message)
            pagedMessageIds.insert(message.messageId)
        }
        if !prepended.isEmpty {
            state.messages.insert(contentsOf: prepended, at: 0)
        }
        historyContinuation = page.complete ? nil : page.continuation
        historyComplete = page.complete
        historyErrorKey = nil
        if !prepended.isEmpty {
            historyPrepend = HistoryFacet.Prepend(
                id: (historyPrepend?.id ?? 0) + 1,
                anchorMessageId: anchor
            )
        }
    }

    func handleHistoryFailure(_ error: Error) {
        let code = (error as? SessionServiceError)?.code ?? .internalError
        if code == .staleHistory {
            historyNeedsFreshTraversal = true
            historyContinuation = nil
            historyProjection = nil
            historyErrorKey = nil
            return
        }
        historyErrorKey = L10nKey.errorKey(for: code)
        if code == .unavailable || code == .hostOffline {
            historyProvenance = .unavailable
        }
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
