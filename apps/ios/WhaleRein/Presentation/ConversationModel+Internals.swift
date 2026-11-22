// Conversation internals behaviour, split out of the model file.

import Foundation

@MainActor
extension ConversationModel {
    // MARK: - UI state

    func toggleExpansion(_ blockId: String) {
        if expandedBlockIds.contains(blockId) {
            expandedBlockIds.remove(blockId)
        } else {
            expandedBlockIds.insert(blockId)
        }
    }

    func selectDelivery(_ choice: DeliveryChoice) {
        guard DeliveryPolicy.isSupported(choice, for: capabilities) else { return }
        delivery = choice
    }

    func draftChanged() {
        draftSaveTask?.cancel()
        let text = draft
        let hostId = hostId
        let sessionId = sessionId
        draftSaveTask = Task { [service] in
            try? await Task.sleep(for: .milliseconds(120))
            if Task.isCancelled { return }
            try? await service.saveDraft(hostId: hostId, sessionId: sessionId, text: text)
        }
    }

    func persistDraftNow() async {
        draftSaveTask?.cancel()
        draftSaveTask = nil
        try? await service.saveDraft(hostId: hostId, sessionId: sessionId, text: draft)
    }

    func updateScroll(isAtLatest: Bool) {
        scroll.update(isAtLatest: isAtLatest)
    }

    func noteTopVisibleMessage(_ messageId: String?) {
        scroll.noteTopVisibleMessage(messageId)
    }

    func jumpToLatest() {
        scroll.jumpToLatest()
        requestScroll(animated: true)
    }

    // MARK: - Internals

    @discardableResult
    func appendOptimisticUserMessage(text: String, receipt: CommandReceipt) -> String? {
        // The committed echo may have arrived before the send response (Core can
        // deliver the message event first). In that case the authoritative row
        // already exists and a local placeholder would be a duplicate.
        if let committedId = receipt.messageId,
           messages.contains(where: { $0.messageId == committedId }) {
            return nil
        }
        let messageId = "local-\(receipt.commandId)"
        guard !messages.contains(where: { $0.messageId == messageId }) else { return nil }
        optimisticMessages[messageId] = OptimisticMessage(
            localId: messageId,
            commandId: receipt.commandId,
            text: text,
            committedMessageId: receipt.messageId
        )
        let message = ConversationMessage(
            messageId: messageId,
            role: .user,
            blocks: [
                ConversationBlock(
                    blockId: "\(messageId)-block-0",
                    kind: .text,
                    text: text
                )
            ],
            createdAtUnixMs: Int64(now().timeIntervalSince1970 * 1000)
        )
        state.messages.append(message)
        _ = scroll.recordIncoming(isOwnMessage: true)
        requestScroll(animated: true)
        return messageId
    }

    /// Removes the optimistic row that stands in for this committed echo and
    /// returns its index, so the committed message can take the same position.
    /// Matching uses the Core message id when the receipt supplied one; only
    /// Sources that never echo a correlation id fall back to exact text. Text
    /// fallback must not merge two distinct authoritative identities.
    func reconcileUserEcho(_ message: ConversationMessage) -> Int? {
        let echoText = normalizedText(message.plainText)
        let match = messages.firstIndex { local in
            guard let pending = optimisticMessages[local.messageId] else { return false }
            if let committed = pending.committedMessageId {
                return committed == message.messageId
            }
            return normalizedText(pending.text) == echoText
        }
        guard let match else { return nil }
        let local = state.messages.remove(at: match)
        optimisticMessages.removeValue(forKey: local.messageId)
        return match
    }

    func normalizedText(_ text: String) -> String {
        text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    func draftMatchesSubmission(_ submittedText: String) -> Bool {
        draft.trimmingCharacters(in: .whitespacesAndNewlines) == submittedText
    }

    /// Settles the one uncertain command with a terminal receipt.
    func settleUncertainCommand(with receipt: CommandReceipt) {
        guard case .uncertain(let command, let inFlight) = state.lifecycle,
              inFlight.commandId == receipt.commandId else { return }
        state = ConversationReduce.reduce(state, .commandSettled(command, receipt))
    }

    func applyCommandStatus(_ receipt: CommandReceipt) {
        if case .uncertain(let command, let inFlight) = state.lifecycle,
           inFlight.commandId == receipt.commandId {
            if receipt.status == .uncertain {
                state.lastReceipt = receipt
            } else {
                state = ConversationReduce.reduce(state, .commandSettled(command, receipt))
                if receipt.status == .confirmed || receipt.status == .accepted {
                    let submitted = inFlightSendText
                    if let submitted, draftMatchesSubmission(submitted) {
                        clearDraftAfterSubmission()
                    }
                }
                inFlightSendText = nil
            }
            return
        }
        if let pendingStop, pendingStop.commandId == receipt.commandId {
            let submissionEpoch = pendingStopSubmissionEpoch ?? authoritativeRunEpoch
            applyStopReceipt(receipt, submissionEpoch: submissionEpoch)
            return
        }
        // The method response may still be in flight; remember the early
        // settlement so the return can apply it instead of discarding it.
        rememberEarlyCommandStatus(receipt)
    }

    func applyStopReceipt(_ receipt: CommandReceipt, submissionEpoch: Int) {
        lastStopStatus = receipt.status
        switch receipt.status {
        case .confirmed:
            pendingStop = nil
            pendingStopSubmissionEpoch = nil
            stopSettled = true
            // Settle the operation, but only let it stop the run it was
            // requested for. A newer authoritative run remains running.
            if submissionEpoch == authoritativeRunEpoch {
                updateCapabilities(running: false)
                previewText = nil
                previewAttemptId = nil
            }
        case .accepted, .uncertain:
            pendingStop = receipt
            pendingStopSubmissionEpoch = submissionEpoch
            stopSettled = false
        case .rejected, .cancelled:
            pendingStop = nil
            pendingStopSubmissionEpoch = nil
            stopSettled = true
        }
    }

    func preferredStatus(_ early: CommandStatus, over returned: CommandStatus) -> CommandStatus {
        switch returned {
        case .confirmed, .rejected, .cancelled:
            return returned
        case .accepted, .uncertain:
            return early
        }
    }

    func rememberEarlyCommandStatus(_ receipt: CommandReceipt) {
        if earlyCommandStatuses[receipt.commandId] == nil {
            earlyCommandStatusOrder.append(receipt.commandId)
        }
        earlyCommandStatuses[receipt.commandId] = receipt.status
        while earlyCommandStatusOrder.count > 32 {
            let oldest = earlyCommandStatusOrder.removeFirst()
            earlyCommandStatuses.removeValue(forKey: oldest)
        }
    }

    func takeEarlyCommandStatus(for commandId: String) -> CommandStatus? {
        earlyCommandStatusOrder.removeAll { $0 == commandId }
        return earlyCommandStatuses.removeValue(forKey: commandId)
    }

    func clearDraftAfterSubmission() {
        draft = ""
        draftSaveTask?.cancel()
        draftSaveTask = nil
        Task { [service, hostId, sessionId] in
            try? await service.clearDraft(hostId: hostId, sessionId: sessionId)
        }
    }

    func clearDraftNow() async {
        draft = ""
        draftSaveTask?.cancel()
        draftSaveTask = nil
        try? await service.clearDraft(hostId: hostId, sessionId: sessionId)
    }

    func updateCapabilities(running: Bool) {
        var next = capabilities
        next.running = running
        applyCapabilities(next)
    }

    func applyCapabilities(_ next: SessionCapabilities) {
        let wasRunning = capabilities.running
        state = ConversationReduce.reduce(state, .capabilitiesLoaded(next))
        queue.applyCapabilities(next)
        let options = DeliveryPolicy.choices(for: capabilities)
        // A run-state change returns the affordance to the ordinary queue
        // default. An explicit choice otherwise persists while it is supported.
        if !options.contains(delivery) || capabilities.running != wasRunning {
            delivery = DeliveryPolicy.defaultChoice(for: capabilities) ?? .send
        }
        onStatusChange?()
    }

    // MARK: - Approval expiry clock

    func restartApprovalTimer(fastVerification: Bool = true) {
        stopApprovalTimer()
        let pending = approvals.filter { $0.state == .pending }
        guard !pending.isEmpty else { return }
        let current = now()
        approvalClock = current
        let nextExpiry = pending.map(\.expiresAt).min() ?? current
        let remaining = nextExpiry.timeIntervalSince(current)
        let allTrusted = pending.allSatisfy { trustedPendingApprovalIds.contains($0.approvalId) }
        // When the local clock already says the request is expired but the Host
        // still reports it pending, verify with the Host once, then recheck at a
        // low frequency instead of spinning. Offline uses the slow path too.
        let delay: Duration
        if remaining > 0 {
            delay = .milliseconds(max(50, Int(remaining * 1000)))
        } else if fastVerification && connection.isOnline && !allTrusted {
            delay = .milliseconds(250)
        } else {
            delay = .seconds(30)
        }
        approvalExpiryTask = Task { [weak self] in
            try? await Task.sleep(for: delay)
            guard let self, !Task.isCancelled else { return }
            self.approvalClock = self.now()
            self.approvalRefreshCount += 1
            await self.refreshApprovals(restartTimer: false)
            self.restartApprovalTimer(fastVerification: false)
        }
    }

    func stopApprovalTimer() {
        approvalExpiryTask?.cancel()
        approvalExpiryTask = nil
    }

    func requestScroll(animated: Bool) {
        scrollRequestCounter += 1
        scrollRequest = ConversationScrollRequest(id: scrollRequestCounter, animated: animated)
    }
}
