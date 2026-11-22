// Fixture operation-recovery adapter.
//
// The fixture keeps a deterministic in-memory journal so unit and UI flows can
// exercise listing, manual recovery, reply references and exact rereads without
// Core. It never sends or resumes pairing during a recovery lookup.

import Foundation

@MainActor
final class FixtureOperationJournal {
    private var order: [String] = []
    private var entries: [String: OperationReceipt] = [:]
    private var contents: [String: ConversationMessage] = [:]
    private var commandToOperation: [String: String] = [:]

    func recordSend(
        receipt: CommandReceipt,
        hostId: String,
        sessionId: String,
        instanceId: String
    ) {
        let operationId = receipt.requestId
        remember(operationId)
        commandToOperation[receipt.commandId] = operationId
        entries[operationId] = OperationReceipt(
            operationId: operationId,
            kind: .send,
            scope: OperationScope(
                instanceId: instanceId,
                hostId: hostId,
                sessionId: sessionId
            ),
            status: Self.operationStatus(receipt.status),
            receipt: receipt,
            reply: nil,
            content: nil,
            readError: nil
        )
    }

    func recordControl(
        receipt: CommandReceipt,
        hostId: String,
        sessionId: String,
        instanceId: String
    ) {
        let operationId = receipt.requestId
        remember(operationId)
        commandToOperation[receipt.commandId] = operationId
        entries[operationId] = OperationReceipt(
            operationId: operationId,
            kind: .control,
            scope: OperationScope(
                instanceId: instanceId,
                hostId: hostId,
                sessionId: sessionId
            ),
            status: Self.operationStatus(receipt.status),
            receipt: receipt,
            reply: nil,
            content: nil,
            readError: nil
        )
    }

    func settle(commandId: String, status: CommandStatus) {
        guard let operationId = commandToOperation[commandId],
              let stored = entries[operationId] else { return }
        var receipt = stored.receipt
        receipt?.status = status
        entries[operationId] = stored.replacing(
            status: Self.operationStatus(status),
            receipt: receipt
        )
    }

    func all() -> [OperationReceipt] {
        order.compactMap { entries[$0] }.sorted { $0.operationId < $1.operationId }
    }

    func receipt(_ operationId: String) -> OperationReceipt? {
        entries[operationId]
    }

    func content(for messageId: String) -> ConversationMessage? {
        contents[messageId]
    }

    func store(_ receipt: OperationReceipt) {
        remember(receipt.operationId)
        entries[receipt.operationId] = receipt
        if let content = receipt.content {
            contents[content.messageId] = content
        }
    }

    func reset() {
        order.removeAll()
        entries.removeAll()
        contents.removeAll()
        commandToOperation.removeAll()
    }

    private func remember(_ operationId: String) {
        if !order.contains(operationId) {
            order.append(operationId)
        }
    }

    private static func operationStatus(_ status: CommandStatus) -> OperationStatus {
        switch status {
        case .accepted:
            return .accepted
        case .uncertain:
            return .uncertain
        case .confirmed:
            return .confirmed
        case .rejected:
            return .failed
        case .cancelled:
            return .failed
        }
    }
}

extension FixtureSessionService: OperationRecoveryService {
    func listRetainedOperations() async throws -> [OperationReceipt] {
        try requirePairedForRecovery()
        return operationJournal.all()
    }

    func recoverSend(operationId: String, follow: Bool) async throws -> OperationReceipt {
        try requirePairedForRecovery()
        guard let stored = operationJournal.receipt(operationId) else {
            throw SessionServiceError(.unavailable, detail: "operation is not retained")
        }
        // Control operations expose the recorded result without touching the
        // conversation.
        guard stored.kind == .send else {
            if let receipt = stored.receipt {
                var settled = receipt
                settled.status = .confirmed
                let updated = stored.replacing(status: .confirmed, receipt: settled)
                operationJournal.store(updated)
                return updated
            }
            return stored
        }
        // An unresolved result stays unresolved; a lookup never invents success.
        guard stored.status == .accepted || stored.status == .confirmed else {
            return stored
        }
        guard let scope = stored.scope else { return stored }
        let operationMessageId = "fixture-reply-\(operationId)"
        let reference = MessageReference(
            hostId: scope.hostId,
            sessionId: scope.sessionId,
            messageId: operationMessageId,
            sourceLocator: "fixture-locator-\(operationMessageId)",
            projectionId: "fixture-projection"
        )
        let content = operationJournal.content(for: operationMessageId)
            ?? ConversationMessage(
                messageId: operationMessageId,
                role: .assistant,
                blocks: [
                    ConversationBlock(
                        blockId: "\(operationMessageId)-block-0",
                        kind: .text,
                        text: "Fixture reply for the recovered send."
                    )
                ],
                createdAtUnixMs: Int64(Date().timeIntervalSince1970 * 1000)
            )
        var receipt = stored.receipt
        receipt?.status = .confirmed
        let updated = OperationReceipt(
            operationId: operationId,
            kind: .send,
            scope: scope,
            status: .confirmed,
            receipt: receipt,
            reply: .committed(reference: reference),
            content: follow ? content : stored.content,
            readError: nil
        )
        operationJournal.store(updated)
        return updated
    }

    func readMessage(_ reference: MessageReference) async throws -> ConversationMessage {
        try requirePairedForRecovery()
        if let content = operationJournal.content(for: reference.messageId) {
            return content
        }
        throw SessionServiceError(.unavailable, detail: "message content is not available")
    }

    private func requirePairedForRecovery() throws {
        if isRevokedForRecovery { throw SessionServiceError(.revoked) }
        guard isPairedForRecovery else { throw SessionServiceError(.notPaired) }
    }
}
