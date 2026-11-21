// Shared operation receipt and recovery contract.
//
// Core owns the durable operation journal. This file only models the typed
// projection used by the conversation, Settings and dependent iOS features, so
// no feature creates a second journal. The values carry identities and results,
// never message content beyond an explicit history read.

import Foundation

/// The instance, Host and session an operation was submitted under. Recovery is
/// always scoped to the recorded original target.
struct OperationScope: Hashable, Sendable {
    let instanceId: String
    let hostId: String
    let sessionId: String
}

enum OperationKind: String, Codable, Hashable, Sendable {
    case send
    case control
    case pairing

    init(bindingLabel: String) {
        self = OperationKind(rawValue: bindingLabel.lowercased()) ?? .control
    }
}

/// Submission and recovery states shared by every operation kind.
enum OperationStatus: String, Codable, Hashable, Sendable {
    case submitted
    case accepted
    case uncertain
    case failed
    case confirmed

    init(bindingLabel: String) {
        self = OperationStatus(rawValue: bindingLabel.lowercased()) ?? .uncertain
    }

    /// States that still benefit from an explicit lookup. A confirmed or failed
    /// result is left alone by automatic refresh.
    var isUnresolved: Bool {
        self == .submitted || self == .accepted || self == .uncertain
    }
}

// `MessageReference` lives in Models.swift; history and recovery share the
// same scoped value.

/// Proof-backed reply outcome for the original send.
enum MessageReply: Hashable, Sendable {
    case pending
    case committed(reference: MessageReference)
    case inputRequired(kinds: [String], operation: String)
    case uncertain

    var confirmedReference: MessageReference? {
        if case .committed(let reference) = self { return reference }
        return nil
    }

    var inputKinds: [String]? {
        if case .inputRequired(let kinds, _) = self { return kinds }
        return nil
    }
}

/// One retained operation as presented to the app. Submission receipt, reply
/// reference, canonical content and content-read failure stay distinct.
struct OperationReceipt: Identifiable, Hashable, Sendable {
    let operationId: String
    let kind: OperationKind
    let scope: OperationScope?
    let status: OperationStatus
    let receipt: CommandReceipt?
    let reply: MessageReply?
    let content: ConversationMessage?
    let readError: String?

    var id: String { operationId }

    var confirmedReference: MessageReference? { reply?.confirmedReference }

    /// A confirmed reference may still need an explicit content read.
    var canReadExactReply: Bool { confirmedReference != nil && content == nil }

    var inputKinds: [String]? { reply?.inputKinds }

    func replacing(
        status: OperationStatus? = nil,
        receipt: CommandReceipt? = nil,
        reply: MessageReply? = nil,
        content: ConversationMessage? = nil,
        readError: String? = nil
    ) -> OperationReceipt {
        OperationReceipt(
            operationId: operationId,
            kind: kind,
            scope: scope,
            status: status ?? self.status,
            receipt: receipt ?? self.receipt,
            reply: reply ?? self.reply,
            content: content ?? self.content,
            readError: readError ?? self.readError
        )
    }
}

/// Read-only recovery surface implemented by the Live and Fixture adapters.
/// The automatic foreground/reconnect path uses only these methods.
protocol OperationRecoveryService: AnyObject {
    func listRetainedOperations() async throws -> [OperationReceipt]
    func recoverSend(operationId: String, follow: Bool) async throws -> OperationReceipt
    func readMessage(_ reference: MessageReference) async throws -> ConversationMessage
}
