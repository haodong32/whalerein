// Conversation commands behaviour, split out of the model file.

import Foundation

@MainActor
extension ConversationModel {
    // MARK: - Commands

    func send() async {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, inFlightSend == nil, pendingSubmission == nil, !watchEnded else { return }
        // Resolve the delivery mode before claiming the submission: an
        // unsupported mode is rejected locally and must not block the composer.
        let choice: DeliveryChoice
        do {
            choice = try DeliveryPolicy.resolve(delivery, for: capabilities)
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            state = ConversationReduce.reduce(state, .commandErrorRaised(code))
            reportFatalIfNeeded(code)
            return
        }
        let command = PendingCommand(
            id: UUID().uuidString,
            kind: .send(text: text),
            submittedAt: now()
        )
        state = ConversationReduce.reduce(state, .sendSubmitted(command))
        appendPendingSend(command, text: text)
        do {
            var receipt = try await service.send(
                hostId: hostId,
                sessionId: sessionId,
                text: text,
                delivery: choice
            )
            guard !isClosed else { return }
            // The submission settled: release the gate even though an uncertain
            // receipt continues to block Send until reconciliation.
            if let early = takeEarlyCommandStatus(for: receipt.commandId) {
                receipt.status = preferredStatus(early.status, over: receipt.status)
                receipt.messageId = receipt.messageId ?? early.messageId
                receipt.deliveryId = receipt.deliveryId ?? early.deliveryId
            }
            state.lastCommandKind = command.kind
            // A rejected send was definitively not accepted, so it has no
            // retained operation to recover and must not become the identity a
            // later relaunch tries to resolve.
            let admitted = receipt.status != .rejected && receipt.status != .cancelled
            lastSendOperationId = admitted ? receipt.requestId : nil
            operationResult = nil
            operationErrorKey = nil
            operationReadErrorKey = nil
            if receipt.status == .uncertain {
                state = ConversationReduce.reduce(state, .sendUncertain(command, receipt))
                inFlightSendText = text
            } else {
                state = ConversationReduce.reduce(state, .commandSettled(command, receipt))
                inFlightSendText = nil
            }
            settlePendingSend(command, receipt: receipt)
            if admitted, choice == .send || (choice == .queue && !capabilities.running) {
                // An idle native-queue submission starts the turn; a running
                // queue submission leaves the current run untouched.
                updateCapabilities(running: true)
            }
            if receipt.status == .uncertain {
                // Keep the current draft (which may already be a newer message)
                // until the original request is reconciled.
                await persistDraftNow()
            } else if admitted {
                // A successful older send consumes only the text it submitted.
                // Text typed while the response was in flight belongs to a
                // newer message and must survive.
                if draftMatchesSubmission(text) {
                    await clearDraftNow()
                } else {
                    await persistDraftNow()
                }
            }
        } catch {
            // A definite failure releases the gate so the same intent can be
            // retried; a late settlement of the failed command cannot re-arm it.
            let code = (error as? SessionServiceError)?.code ?? .internalError
            state = ConversationReduce.reduce(state, .sendRejected(command, code))
            removePendingSend(command)
            reportFatalIfNeeded(code)
        }
    }

    func stopRun() async {
        guard canStop else { return }
        // Capture the authoritative run this Stop was requested for. If a newer
        // run becomes authoritative before a late confirmation arrives, that
        // confirmation must settle this operation without clearing the new run.
        let submissionEpoch = authoritativeRunEpoch
        let command = PendingCommand(
            id: UUID().uuidString,
            kind: .stop,
            submittedAt: now()
        )
        state = ConversationReduce.reduce(state, .stopSubmitted(command))
        do {
            var receipt = try await service.stop(hostId: hostId, sessionId: sessionId)
            // The submission settled; an accepted or uncertain Stop remains
            // pending through `pendingStop` until the Host reconciles it.
            if let early = takeEarlyCommandStatus(for: receipt.commandId) {
                receipt.status = preferredStatus(early.status, over: receipt.status)
                receipt.messageId = receipt.messageId ?? early.messageId
                receipt.deliveryId = receipt.deliveryId ?? early.deliveryId
            }
            state = ConversationReduce.reduce(state, .commandSettled(command, receipt))
            applyStopReceipt(receipt, submissionEpoch: submissionEpoch)
            if receipt.status == .confirmed {
                // Interrupt is a current-turn control; the queue stays visible
                // and is refreshed from the source after the turn settles.
                await queue.refresh()
            }
        } catch {
            // A definite failure releases the gate so Stop can be retried. A
            // late settlement for the failed command is only remembered as an
            // early status and never resurrects this cleared intent.
            let code = (error as? SessionServiceError)?.code ?? .internalError
            state = ConversationReduce.reduce(state, .stopRejected(command, code))
            reportFatalIfNeeded(code)
        }
    }
}
