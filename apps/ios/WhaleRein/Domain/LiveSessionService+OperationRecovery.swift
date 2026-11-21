// Live operation-recovery adapter over the typed UniFFI export.
//
// This file is the only place that maps retained operation records into the app
// contract. It performs reads and the Core status lookup; it never sends a
// message or resumes pairing.

import Foundation
import WhaleReinMobile

extension LiveSessionService: OperationRecoveryService {
    func listRetainedOperations() async throws -> [OperationReceipt] {
        let core = self.core
        let sends = try await call { try core.listSendOperations() }
        let pending = try await call { try core.listPendingCommands() }
        return (sends + pending).map { Self.operationReceipt(from: $0, instanceId: instanceId) }
    }

    func recoverSend(operationId: String, follow: Bool) async throws -> OperationReceipt {
        let core = self.core
        // A retained control or pairing operation is recovered through Core's
        // pending-command path under the same original identity.
        let retained = try await call { try core.listPendingCommands() }
        if retained.contains(where: { $0.operationId == operationId }) {
            let view = try await call {
                try core.recoverPendingCommand(operationId: operationId)
            }
            return Self.operationReceipt(from: view, instanceId: instanceId)
        }
        // A status-only lookup is bounded tightly; a follow-up includes the
        // reply query and exact content read.
        let timeoutMs: UInt64 = follow ? 30_000 : 10_000
        let view = try await call {
            try core.recoverSendWithContent(
                operationId: operationId,
                follow: follow,
                timeoutMs: timeoutMs
            )
        }
        return Self.operationReceipt(from: view, instanceId: instanceId)
    }

    func readMessage(_ reference: MessageReference) async throws -> ConversationMessage {
        let core = self.core
        let binding = Self.bindingReference(reference)
        let view = try await call {
            try core.readHistoryMessage(reference: binding)
        }
        return Self.map(message: view)
    }

    static func bindingReference(_ reference: MessageReference) -> MessageReferenceView {
        MessageReferenceView(
            session: SessionKeyView(
                hostId: reference.hostId,
                sessionId: reference.sessionId
            ),
            messageId: reference.messageId,
            sourceLocator: reference.sourceLocator,
            projectionId: reference.projectionId
        )
    }

    static func operationReceipt(
        from view: RetainedOperationView,
        instanceId: String
    ) -> OperationReceipt {
        let scope: OperationScope?
        if let hostId = view.hostId,
           let sessionId = view.sessionId,
           !hostId.isEmpty,
           !sessionId.isEmpty {
            scope = OperationScope(
                instanceId: instanceId,
                hostId: hostId,
                sessionId: sessionId
            )
        } else {
            scope = nil
        }
        return OperationReceipt(
            operationId: view.operationId,
            kind: OperationKind(bindingLabel: view.kind),
            scope: scope,
            status: OperationStatus(bindingLabel: view.status),
            receipt: view.receipt.map { map(command: $0, delivery: nil) },
            reply: view.reply.map(map(reply:)),
            content: view.content.map(map(message:)),
            readError: view.readError
        )
    }

    static func map(reply view: MessageReplyView) -> MessageReply {
        switch view {
        case .pending:
            return .pending
        case .committed(let reference):
            return .committed(
                reference: MessageReference(
                    hostId: reference.session.hostId,
                    sessionId: reference.session.sessionId,
                    messageId: reference.messageId,
                    sourceLocator: reference.sourceLocator,
                    projectionId: reference.projectionId
                )
            )
        case .inputRequired(let kinds, let operation, _):
            return .inputRequired(kinds: kinds, operation: operation)
        case .uncertain:
            return .uncertain
        }
    }
}
