// Retained-send recovery: the operation result, exact-reply read and the
// message-level actions offered after an uncertain send.

import Foundation

@MainActor
extension ConversationModel {
    // MARK: - Operation recovery

    /// Manual recovery for the original send. Uses Core's recorded operation
    /// identity and never submits another message.
    func recoverSendResult() async {
        guard let operationId = lastSendOperationId, !isRecoveringOperation else { return }
        guard let recovery = service as? any OperationRecoveryService else {
            operationErrorKey = .errorIntegrationUnavailable
            return
        }
        isRecoveringOperation = true
        operationErrorKey = nil
        defer { isRecoveringOperation = false }
        do {
            let receipt = try await recovery.recoverSend(operationId: operationId, follow: true)
            applyOperationReceipt(receipt)
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            operationErrorKey = L10nKey.errorKey(for: code)
            reportFatalIfNeeded(code)
        }
    }

    /// Explicit exact reread for a confirmed reference. A failed read keeps the
    /// confirmation and never resubmits.
    func readExactReply() async {
        guard let receipt = operationResult,
              let reference = receipt.confirmedReference,
              !isReadingExactReply else { return }
        guard let recovery = service as? any OperationRecoveryService else {
            operationErrorKey = .errorIntegrationUnavailable
            return
        }
        guard reference.hostId == hostId, reference.sessionId == sessionId else {
            operationErrorKey = .errorOperationScope
            return
        }
        isReadingExactReply = true
        defer { isReadingExactReply = false }
        do {
            let message = try await recovery.readMessage(reference)
            insertCommittedMessage(message)
            operationResult = OperationReceipt(
                operationId: receipt.operationId,
                kind: receipt.kind,
                scope: receipt.scope,
                status: receipt.status,
                receipt: receipt.receipt,
                reply: receipt.reply,
                content: message,
                readError: nil
            )
            operationReadErrorKey = nil
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            operationErrorKey = L10nKey.errorKey(for: code)
            operationReadErrorKey = .conversationOperationReadFailed
        }
    }

    /// On a cold launch the model has no in-memory operation identity. Core's
    /// durable journal does, so adopt the single unresolved send recorded for
    /// this exact instance/Host/session and read its exact result. This never
    /// submits the message again; with several unresolved candidates the app
    /// leaves selection to the explicit recovery surface instead of guessing.
    func recoverRetainedSendIfNeeded() async {
        guard lastSendOperationId == nil,
              operationResult == nil,
              let recovery = service as? any OperationRecoveryService else { return }
        do {
            let retained = try await recovery.listRetainedOperations()
            let candidates = retained.filter {
                $0.kind == .send
                    && $0.scope == OperationScope(
                        instanceId: service.instanceId,
                        hostId: hostId,
                        sessionId: sessionId
                    )
                    && $0.status.isUnresolved
            }
            guard candidates.count == 1, let candidate = candidates.first else { return }
            lastSendOperationId = candidate.operationId
            operationResult = candidate
            let receipt = try await recovery.recoverSend(
                operationId: candidate.operationId,
                follow: true
            )
            applyOperationReceipt(receipt)
        } catch {
            // Leave the original operation listed for the Settings recovery
            // path; a failed startup lookup cannot fabricate a result.
            operationErrorKey = nil
        }
    }

    /// Applies results from the one coalesced visible-session refresh. Only the
    /// original send is updated; a foreign scope is rejected. After a relaunch
    /// the first refresh may be the only place that provides the durable
    /// operation identity, so one unambiguous unresolved send for this exact
    /// scope is adopted rather than dropped.
    func applyRecoveredOperations(_ receipts: [OperationReceipt]) {
        if lastSendOperationId == nil {
            let scope = OperationScope(
                instanceId: service.instanceId,
                hostId: hostId,
                sessionId: sessionId
            )
            let candidates = receipts.filter {
                $0.kind == .send && $0.scope == scope && $0.status.isUnresolved
            }
            guard candidates.count == 1 else { return }
            lastSendOperationId = candidates[0].operationId
        }
        guard let operationId = lastSendOperationId else { return }
        guard let receipt = receipts.first(where: { $0.operationId == operationId }) else {
            return
        }
        applyOperationReceipt(receipt)
    }

    func applyOperationReceipt(_ receipt: OperationReceipt) {
        guard receipt.kind == .send else { return }
        guard let scope = receipt.scope,
              scope.hostId == hostId,
              scope.sessionId == sessionId else {
            operationErrorKey = .errorOperationScope
            return
        }
        operationResult = receipt
        operationErrorKey = nil
        if let content = receipt.content {
            insertCommittedMessage(content)
        }
        if receipt.readError != nil {
            operationReadErrorKey = .conversationOperationReadFailed
        } else if receipt.content != nil {
            operationReadErrorKey = nil
        }
        if receipt.status == .confirmed || receipt.status == .failed {
            if let commandReceipt = receipt.receipt {
                settleUncertainCommand(with: commandReceipt)
            }
            inFlightSendText = nil
        }
    }

    /// Deduplicates committed messages by stable identity, including a recovered
    /// reply that also arrived through the live watch.
    func insertCommittedMessage(_ message: ConversationMessage) {
        if let index = messages.firstIndex(where: { $0.messageId == message.messageId }) {
            if messages[index] != message {
                state.messages[index] = message
            }
            return
        }
        reconcileUserEcho(message)
        state.messages.append(message)
    }
}
