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

    /// Applies results from the one coalesced visible-session refresh. Only the
    /// original send is updated; a foreign scope is rejected.
    func applyRecoveredOperations(_ receipts: [OperationReceipt]) {
        guard let operationId = lastSendOperationId else { return }
        guard let receipt = receipts.first(where: { $0.operationId == operationId }) else {
            return
        }
        applyOperationReceipt(receipt)
    }

    private func applyOperationReceipt(_ receipt: OperationReceipt) {
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
    private func insertCommittedMessage(_ message: ConversationMessage) {
        if let index = messages.firstIndex(where: { $0.messageId == message.messageId }) {
            if messages[index] != message {
                state.messages[index] = message
            }
            return
        }
        let echoIndex = message.role == .user ? reconcileUserEcho(message) : nil
        if let echoIndex {
            state.messages.insert(message, at: min(echoIndex, state.messages.count))
        } else {
            state.messages.append(message)
        }
    }
}
