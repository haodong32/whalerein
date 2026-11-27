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
        guard let lease = draftLease else { return }
        draftSaveTask?.cancel()
        let text = draft
        let editID = draftEditID
        draftSaveTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(120))
            guard !Task.isCancelled, let self else { return }
            do {
                try await self.drafts.setAndFlush(lease, text: text, editID: editID)
                guard !Task.isCancelled else { return }
                self.draftPersistenceErrorKey = nil
            } catch {
                guard !Task.isCancelled else { return }
                self.draftPersistenceErrorKey = .errorDraftUnavailable
            }
        }
    }

    func persistDraftNow() async {
        let pending = draftSaveTask
        pending?.cancel()
        draftSaveTask = nil
        _ = await pending?.value
        guard let lease = draftLease else { return }
        do {
            try await drafts.setAndFlush(lease, text: draft, editID: draftEditID)
            draftPersistenceErrorKey = nil
        } catch {
            // The typed text stays in memory; only its restart durability is
            // uncertain, which the view reports from this key.
            draftPersistenceErrorKey = .errorDraftUnavailable
        }
    }

    func updateScroll(isAtLatest: Bool) {
        guard scroll.isAtLatest != isAtLatest else { return }
        scroll.update(isAtLatest: isAtLatest)
    }

    func noteTopVisibleMessage(_ messageId: String?) {
        guard !scroll.isAtLatest, scroll.anchorMessageId != messageId else { return }
        scroll.noteTopVisibleMessage(messageId)
    }

    func jumpToLatest() {
        scroll.jumpToLatest()
        requestScroll(animated: true)
    }

    // MARK: - Internals

    func presentationID(for messageID: String) -> String {
        committedPresentationIDs[messageID] ?? messageID
    }

    func appendPendingSend(_ command: PendingCommand, text: String, queuedBehindRun: Bool) {
        let messageID = "local-\(command.id)"
        optimisticMessages[messageID] = OptimisticMessage(queuedBehindRun: queuedBehindRun)
        state.messages.append(
            ConversationMessage(
                messageId: messageID,
                role: .user,
                blocks: [
                    ConversationBlock(blockId: "\(messageID)-block-0", kind: .text, text: text)
                ],
                createdAtUnixMs: Int64(command.submittedAt.timeIntervalSince1970 * 1000)
            )
        )
        scroll.markLayoutChange()
        _ = scroll.recordIncoming(isOwnMessage: true)
    }

    func settlePendingSend(_ command: PendingCommand, receipt: CommandReceipt) {
        settlePendingSend(localID: "local-\(command.id)", receipt: receipt)
    }

    func settlePendingSend(localID: String, receipt: CommandReceipt) {
        guard var pending = optimisticMessages[localID] else { return }
        guard receipt.status != .rejected, receipt.status != .cancelled else {
            removePendingSend(localID: localID)
            return
        }
        pending.commandId = receipt.commandId
        pending.committedMessageId = receipt.messageId ?? pending.committedMessageId
        pending.deliveryId = receipt.deliveryId ?? pending.deliveryId
        if messages.contains(where: { $0.role == .user && pending.matches($0) }) {
            removePendingSend(localID: localID)
        } else {
            optimisticMessages[localID] = pending
        }
    }

    func removePendingSend(_ command: PendingCommand) {
        removePendingSend(localID: "local-\(command.id)")
    }

    func removePendingSend(localID: String) {
        optimisticMessages.removeValue(forKey: localID)
        state.messages.removeAll { $0.messageId == localID }
        scroll.markLayoutChange()
    }

    /// A committed echo inherits the pending presentation identity only when
    /// a receipt explicitly associated it with this send.
    @discardableResult
    func reconcileUserEcho(_ message: ConversationMessage) -> Int? {
        guard message.role == .user, let index = messages.firstIndex(where: {
            optimisticMessages[$0.messageId]?.matches(message) == true
        }) else { return nil }
        let local = state.messages.remove(at: index)
        let pending = optimisticMessages.removeValue(forKey: local.messageId)
        committedPresentationIDs[message.messageId] = local.messageId
        scroll.markLayoutChange()
        return pending?.preservesPosition == true ? index : nil
    }

    /// Settles the original uncertain command and, on success, its draft.
    @discardableResult
    func settleUncertainCommand(with receipt: CommandReceipt) -> Task<Void, Never>? {
        guard case .uncertain(let command, let inFlight) = state.lifecycle,
              inFlight.commandId == receipt.commandId else { return nil }
        if receipt.status == .uncertain {
            state.lastReceipt = receipt
            settlePendingSend(command, receipt: receipt)
            return nil
        }
        state = ConversationReduce.reduce(state, .commandSettled(command, receipt))
        settlePendingSend(command, receipt: receipt)
        let submission = inFlightDraftSubmission
        inFlightDraftSubmission = nil
        guard let submission, receipt.status == .confirmed || receipt.status == .accepted else { return nil }
        if !isClosed, draftEditID == submission.editID {
            // The composer settles with the confirmation. Keep the ticket
            // until storage acknowledges it or the reader types a new edit.
            draftSaveTask?.cancel()
            draftSaveTask = nil
            draft = ""
            draftEditID = submission.editID
        }
        return Task { await settleSubmittedDraft(submission) }
    }

    func applyCommandStatus(_ receipt: CommandReceipt) {
        if case .uncertain(_, let inFlight) = state.lifecycle, inFlight.commandId == receipt.commandId {
            settleUncertainCommand(with: receipt)
            return
        }
        if let pendingStop, pendingStop.commandId == receipt.commandId {
            let submissionEpoch = pendingStopSubmissionEpoch ?? authoritativeRunEpoch
            applyStopReceipt(receipt, submissionEpoch: submissionEpoch)
            return
        }
        if let localID = optimisticMessages.first(where: { $0.value.commandId == receipt.commandId })?.key {
            settlePendingSend(localID: localID, receipt: receipt)
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
        earlyCommandStatuses[receipt.commandId] = receipt
        while earlyCommandStatusOrder.count > 32 {
            let oldest = earlyCommandStatusOrder.removeFirst()
            earlyCommandStatuses.removeValue(forKey: oldest)
        }
    }

    func takeEarlyCommandStatus(for commandId: String) -> CommandReceipt? {
        earlyCommandStatusOrder.removeAll { $0 == commandId }
        return earlyCommandStatuses.removeValue(forKey: commandId)
    }

    func settleSubmittedDraft(_ submission: DraftService.Submission?) async {
        guard let submission else { return }
        if isClosed {
            await closeDraftFlushTask?.value
        } else if draftEditID != submission.editID {
            await persistDraftNow()
        }
        // The coordinator reports a storage failure to the current owner.
        try? await drafts.clearSubmitted(submission)
    }

    func applySubmittedDraftClear(editID: UUID, persisted: Bool) {
        guard !isClosed else { return }
        if draftLease == nil, persisted {
            clearedDraftBeforeRestore = editID
        }
        guard draftEditID == editID else { return }
        draftSaveTask?.cancel()
        draftSaveTask = nil
        draft = ""
        draftPersistenceErrorKey = persisted ? nil : .errorDraftUnavailable
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
        // Visible rendering is paused for an inactive or shielded scene. A
        // pending expiry must not keep redrawing the covered screen; the timer
        // restarts when the scene becomes active again.
        guard !isRenderingPaused else { return }
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
        } else if fastVerification && connectivity.isOnline(hostId) && !allTrusted {
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
