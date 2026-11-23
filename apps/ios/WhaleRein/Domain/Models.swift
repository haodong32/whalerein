// Domain types mirroring the Swift/Rust binding contract for the iOS client.
// Keep this file free of UI imports: presentation models and fixtures depend on
// it, and the future LiveSessionService maps UniFFI records into these types.

import Foundation

// MARK: - Host connection

/// One authoritative connection snapshot for a Host. `nil` from
/// `SessionService.hostConnection(hostId:)` means the connection was never
/// opened, which is distinct from an `offline` snapshot.
struct HostConnectionSnapshot: Equatable, Hashable, Sendable {
    let hostId: String
    /// Core's connection state name, e.g. `connected`, `connecting`, `offline`.
    let state: String
    /// Terminal failure code, present only for terminal failures.
    let code: String?
    let sinceUnixMs: UInt64
}

// MARK: - Hosts and pairing

struct HostSummary: Identifiable, Hashable, Codable, Sendable {
    let hostId: String
    let online: Bool
    let fingerprint: String
    let protocolVersion: String

    var id: String { hostId }
}

struct PairingPrompt: Identifiable, Hashable, Sendable {
    let hostId: String
    let accessURL: URL
    let fingerprint: String
    let fingerprintDisplay: String
    /// Presentation addition: the confirmation window the user can act within.
    let expiresAt: Date

    var id: String { "\(hostId)/\(fingerprint)" }
}

struct Invitation: Hashable, Sendable {
    let accessURL: URL
    let token: String
    let hostHint: String?
    let expiresAt: Date?
}

enum InvitationParseError: Error, Equatable, Sendable {
    case empty
    case malformed
    case unsupportedScheme
    case missingToken
    case invalidAccessURL
    case expired
}

// MARK: - Sessions

struct SessionSummary: Identifiable, Hashable, Codable, Sendable {
    let sessionId: String
    let title: String
    let updatedAtUnixMs: Int64
    /// Working directory reported by the Source. Used only to group the session
    /// list by workspace; it is never shown as a row subtitle.
    let cwd: String
    /// True while the Source reports an active turn for this session.
    var running: Bool = false
    /// The source's own default listing hides this session. The switcher hides
    /// archived rows unless the user asks to see them.
    var archived: Bool = false

    var id: String { sessionId }

    var updatedAt: Date { Date(timeIntervalSince1970: Double(updatedAtUnixMs) / 1000) }
}

struct SessionCapabilities: Hashable, Sendable {
    var running: Bool
    var supportsSteer: Bool
    var supportsQueue: Bool
    /// True only when the Source reports interrupt support. Stop is additionally
    /// gated by `running`.
    var supportsInterrupt: Bool = false
    /// Per-action queue capabilities. A read-only queue stays visible while a
    /// mutation path is unsupported.
    var supportsQueueRead: Bool = false
    var supportsQueueEdit: Bool = false
    var supportsQueueRemove: Bool = false
    var supportsQueuePromote: Bool = false
    /// Independent bounded committed-history pages.
    var supportsHistoryPage: Bool = true
    /// Exact committed-message reads by reference.
    var supportsHistoryExact: Bool = true
    /// Typed attachment metadata and verified downloads.
    var supportsAttachments: Bool = true
    /// The Source can stream displayable reasoning blocks.
    var supportsReasoning: Bool = true
    /// The Source reports typed tool activities.
    var supportsToolActivity: Bool = true

    static let idle = SessionCapabilities(
        running: false,
        supportsSteer: true,
        supportsQueue: true,
        supportsInterrupt: true,
        supportsQueueRead: true,
        supportsQueueEdit: true,
        supportsQueueRemove: true,
        supportsQueuePromote: true
    )
    static let running = SessionCapabilities(
        running: true,
        supportsSteer: true,
        supportsQueue: true,
        supportsInterrupt: true,
        supportsQueueRead: true,
        supportsQueueEdit: true,
        supportsQueueRemove: true,
        supportsQueuePromote: true
    )
}

struct SessionContext: Hashable, Sendable {
    let hostId: String
    let sessionId: String
}

// MARK: - Conversation

enum MessageRole: String, Codable, Sendable {
    case user
    case assistant
    case tool
    case notice
    case system
}

enum BlockKind: String, Codable, Sendable {
    case text
    case reasoning
    case tool
    case notice
    case attachment
    case question
}

/// One ordered, displayable part of a settled tool result. Attachment parts
/// keep the opaque occurrence reference so the row can request a verified
/// download without flattening the result to text.
enum ToolResultPart: Hashable, Sendable {
    case text(String)
    case attachment(AttachmentBlock)
}

enum ToolState: String, Codable, Sendable {
    case preparing
    case running
    case succeeded
    case failed
    case interrupted
    case abandoned
    case unknown
}

/// Core-normalized tool activity. The call and result occurrences are already
/// merged by Core; the app never correlates them itself.
struct ToolActivity: Hashable, Sendable {
    let callId: String
    let name: String
    let summary: String?
    let state: ToolState
    let stages: [String]
    let progress: String?
    let failureCode: String?
    let result: [ToolResultPart]
    let callMissing: Bool

    var displayText: String {
        let parts = result.map { part -> String in
            switch part {
            case .text(let text): return text
            case .attachment(let attachment): return attachment.name
            }
        }
        if !parts.isEmpty { return parts.joined(separator: "\n") }
        return summary ?? progress ?? ""
    }
}

/// One canonical committed attachment occurrence. `attachmentRef` is opaque
/// Source evidence, never a path or authorization token.
struct AttachmentBlock: Hashable, Sendable {
    let mediaType: String
    let name: String
    let size: Int64
    let available: Bool
    let attachmentRef: String
}

struct ConversationBlock: Identifiable, Hashable, Sendable {
    let blockId: String
    let kind: BlockKind
    let text: String
    let toolName: String?
    let toolActivity: ToolActivity?
    let attachment: AttachmentBlock?

    var id: String { blockId }

    init(
        blockId: String,
        kind: BlockKind,
        text: String,
        toolName: String? = nil,
        toolActivity: ToolActivity? = nil,
        attachment: AttachmentBlock? = nil
    ) {
        self.blockId = blockId
        self.kind = kind
        self.text = text
        self.toolName = toolName
        self.toolActivity = toolActivity
        self.attachment = attachment
    }
}

struct ConversationMessage: Identifiable, Hashable, Sendable {
    let messageId: String
    let role: MessageRole
    let blocks: [ConversationBlock]
    let createdAtUnixMs: Int64

    var id: String { messageId }

    var plainText: String { blocks.map(\.text).joined(separator: "\n") }

}

// MARK: - Committed history

/// Scoped reference to one canonical committed message. Resolve its body with
/// an exact Core read; the reference itself never carries content.
struct MessageReference: Hashable, Sendable {
    let hostId: String
    let sessionId: String
    let messageId: String
    let sourceLocator: String
    let projectionId: String

    var session: SessionContext {
        SessionContext(hostId: hostId, sessionId: sessionId)
    }
}

enum HistorySource: String, Codable, Sendable {
    case online
    case cache
}

/// Explicit provenance for a committed-history view.
enum HistoryProvenance: String, Codable, Sendable {
    case online
    case cache
    case unavailable
}

struct ConversationHistoryPage: Sendable {
    let source: HistorySource
    let projection: String
    let complete: Bool
    let messages: [ConversationMessage]
    let activities: [ToolActivity]
    let continuation: String?
}

// MARK: - Approvals

enum ApprovalState: String, Codable, Sendable {
    case pending
    case allowed
    case rejected
    case expired
    case changed
    /// The Source or Core reports that the approval's session is gone. A card
    /// stays on screen with an accurate explanation instead of degrading into a
    /// permanently unapprovable "changed" request.
    case unavailable
    /// A pending request disappeared from the Host's pending list because
    /// another device decided it. The card stays visible but is not actionable.
    case resolvedElsewhere

    /// Terminal states must survive a pending-only refresh. Core's approval
    /// listing returns only pending requests, so replacing the local list with
    /// that result would erase the card the user just decided.
    var isTerminal: Bool { self != .pending }
}

struct Approval: Identifiable, Hashable, Sendable {
    let approvalId: String
    let sessionId: String
    let toolName: String
    let callId: String
    let reason: String
    let version: Int
    let expiresAtUnixMs: Int64
    let digestHex: String
    var state: ApprovalState

    var id: String { approvalId }

    var expiresAt: Date { Date(timeIntervalSince1970: Double(expiresAtUnixMs) / 1000) }

    var isActionable: Bool { state == .pending }
}

/// Identifies the exact approval variant the user reviewed. A decision is only
/// submitted when both the version and digest still match this value.
struct ApprovalReview: Hashable, Sendable {
    let approvalId: String
    let version: Int
    let digestHex: String

    init(approval: Approval) {
        approvalId = approval.approvalId
        version = approval.version
        digestHex = approval.digestHex
    }
}

// MARK: - Pending questions

/// Readiness of the question facet. Only `current` establishes that no pending
/// questions exist; every other state is explicitly not an empty collection.
enum QuestionFreshness: String, Codable, Sendable {
    case loading
    case current
    case stale
    case unavailable
    case unsupported

    init(bindingLabel: String) {
        self = QuestionFreshness(rawValue: bindingLabel.lowercased()) ?? .unavailable
    }
}

struct QuestionOption: Identifiable, Hashable, Sendable {
    let label: String
    let description: String?

    var id: String { label }
}

struct QuestionSubject: Hashable, Sendable {
    let name: String
    let icon: String?
}

struct QuestionIntent: Hashable, Sendable {
    let kind: String
    let approve: String
    let reject: String?
    let always: String?
    let callID: String?
    let subject: QuestionSubject?
    let displayQuestion: [String: String]?
    let displayDetail: [String: String]?
    let displayApprove: [String: String]?
}

struct QuestionItem: Identifiable, Hashable, Sendable {
    let id: String
    let question: String
    let detail: String?
    let header: String?
    let options: [QuestionOption]
    let multiSelect: Bool
    let intent: QuestionIntent?
}

/// The exact native guard a submission must revalidate. Observation freshness
/// never substitutes for these fields.
struct QuestionGuard: Hashable, Sendable {
    let sessionID: String
    let batchID: String
    let sourceEpoch: String
    let version: UInt64
    let digestHex: String
    let phase: String

    var isAnswerablePhase: Bool {
        phase == "QUESTION_OPEN" || phase == "QUESTION_CONTINUED"
    }
}

struct QuestionBatch: Identifiable, Hashable, Sendable {
    let sessionID: String
    let batchID: String
    let sourceEpoch: String
    let version: UInt64
    let digestHex: String
    let phase: String
    let questions: [QuestionItem]
    let answerable: Bool
    let contentUnavailable: Bool
    /// True only when the card may offer an answer after explicit review.
    var canAnswer: Bool {
        answerable && !contentUnavailable && guardValue.isAnswerablePhase
    }

    var id: String { batchID }

    var guardValue: QuestionGuard {
        QuestionGuard(
            sessionID: sessionID,
            batchID: batchID,
            sourceEpoch: sourceEpoch,
            version: version,
            digestHex: digestHex,
            phase: phase
        )
    }
}

struct QuestionStamp: Hashable, Sendable {
    let observationEpoch: String
    let revision: UInt64
}

struct QuestionsState: Hashable, Sendable {
    var freshness: QuestionFreshness
    var stamp: QuestionStamp?
    var batches: [QuestionBatch]
    var error: String?
    /// Core view was too large to inline into the event queue; content must be
    /// loaded on demand. Never interpreted as an empty collection.
    var truncatedForDisplay: Bool = false
    /// Authoritative batch count even when content is not inlined.
    var batchCount: Int = 0

    static let loading = QuestionsState(
        freshness: .loading,
        stamp: nil,
        batches: [],
        error: nil
    )

    var isCurrent: Bool { freshness == .current }
}

struct QuestionPage: Hashable, Sendable {
    let batches: [QuestionBatch]
    let nextCursor: String
    let stamp: QuestionStamp?
    let complete: Bool
}

struct QuestionAnswer: Hashable, Sendable {
    let id: String
    let selected: [String]
    let custom: String?
}

/// Immutable snapshot of the reviewed guard and inputs. Submission compares it
/// with the currently observed batch and refuses a changed or disappeared one.
struct QuestionReview: Hashable, Sendable {
    let guardValue: QuestionGuard
    let answers: [QuestionAnswer]
}

enum QuestionAnswerState: Equatable, Sendable {
    case idle
    case submitting
    case submitted(CommandStatus)
    case uncertain(commandId: String)
    case failed
}

// MARK: - Commands

enum DeliveryChoice: String, Codable, CaseIterable, Sendable {
    case send
    case steer
    case queue
}

enum CommandStatus: String, Codable, Sendable {
    case accepted
    case uncertain
    case confirmed
    case rejected
    case cancelled
}

struct CommandReceipt: Identifiable, Hashable, Sendable {
    let commandId: String
    let requestId: String
    var status: CommandStatus
    let delivery: DeliveryChoice?
    /// Committed conversation message id reported by Core, when it already
    /// exists at receipt time. Used to reconcile the optimistic bubble with the
    /// committed echo instead of rendering both.
    var messageId: String? = nil
    /// Delivery id reported by Core for idempotency-aware reconciliation.
    var deliveryId: String? = nil

    var id: String { commandId }
}

// MARK: - Diagnostics

enum DiagnosticStageKind: String, Codable, Sendable {
    case https
    case mqtt
}

enum DiagnosticStageStatus: String, Codable, Sendable {
    case ok
    case failed
    case idle
}

struct DiagnosticStage: Identifiable, Hashable, Sendable {
    let hostId: String
    let kind: DiagnosticStageKind
    let status: DiagnosticStageStatus
    let correlationId: String?
    let failureCode: String?

    var id: String { "\(hostId).\(kind.rawValue)" }
}

struct DiagnosticsReport: Hashable, Sendable {
    var json: String
    var redactionFields: [String]
    var lastFailureCode: String?
    var appVersion: String
    var coreVersion: String
    var stages: [DiagnosticStage]
    var generatedAt: Date
}

// MARK: - Events

enum AttemptOutcome: String, Codable, Sendable {
    case completed
    case cancelled
    case failed
}

enum MessageEvent: Sendable {
    /// A replaced local cache snapshot. The questions facet remains valid.
    case reset(messages: [ConversationMessage], approvals: [Approval])
    /// A fresh Core watch baseline. Cached history is only a seed; the old
    /// question observation is no longer authoritative.
    case watchReset(messages: [ConversationMessage], approvals: [Approval])
    case message(ConversationMessage)
    case replace(message: ConversationMessage)
    case preview(attemptId: String, text: String, replace: Bool, incomplete: Bool)
    /// Reasoning preview is a distinct typed identity from the answer preview.
    case reasoningPreview(attemptId: String, text: String, replace: Bool, incomplete: Bool)
    /// One material change to a canonical tool activity already merged by Core.
    case toolActivity(ToolActivity)
    case attemptEnded(attemptId: String, outcome: AttemptOutcome)
    /// Host-scoped, unscoped connection transition. Delivered with no session
    /// open and never dropped when a watch is replaced.
    case hostConnection(hostId: String, state: String, code: String?, sinceUnixMs: UInt64)
    case capabilities(SessionCapabilities)
    case approvals([Approval])
    case approvalChanged(Approval)
    case questions(QuestionsState)
    case commandStatus(CommandReceipt)
    case error(code: AppErrorCode, message: String)
    case watchEnded(reason: String)
}

/// Envelope carrying the watch identity and ordering of a `MessageEvent`.
/// Presentation models drop any event whose host/session no longer matches the
/// active watch, or whose sequence is not newer than the last one applied.
/// A host-scoped `hostConnection` event carries an empty `sessionId` because it
/// is not owned by any watch.
struct SessionEventEnvelope: Sendable {
    let hostId: String
    let sessionId: String
    let sequence: Int
    let event: MessageEvent
}

// MARK: - Errors

enum AppErrorCode: String, CaseIterable, Codable, Sendable {
    case invalidInvitation
    case invitationExpired
    case pairingDeclined
    case fingerprintMismatch
    case notPaired
    case revoked
    case hostOffline
    case unavailable
    case conflict
    case timeout
    case cancelled
    case security
    case internalError
    case unsupportedDelivery
    case staleApproval
    case draftUnavailable
    case integrationUnavailable
    /// The Host speaks an incompatible protocol line. Recovery is an upgrade,
    /// not a retry.
    case protocolVersionIncompatible
    /// The Host does not implement a capability this client requires.
    case unsupportedCapability
    /// The device Keychain is locked or unavailable (for example before the
    /// first unlock after a reboot). Recoverable after unlocking; never a tight
    /// retry loop.
    case keychainUnavailable
    /// The app configuration document is invalid or could not be saved. The
    /// previous committed selection and runtime stay in place.
    case configuration
    /// A history continuation or exact reference is stale, expired or belongs
    /// to another scope or content projection. A fresh traversal is required.
    case staleHistory
    /// Core rejected the request shape. This is not a pairing failure.
    case invalidInput
}

struct SessionServiceError: Error, Equatable, LocalizedError, Sendable {
    let code: AppErrorCode
    let detail: String?

    init(_ code: AppErrorCode, detail: String? = nil) {
        self.code = code
        self.detail = detail
    }

    var errorDescription: String? { L10nKey.errorKey(for: code).string }
}
