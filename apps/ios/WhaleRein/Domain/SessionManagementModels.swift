// Session-management domain values shared by the live binding adapter, the
// fixtures and presentation. These types only describe Core-owned outcomes;
// they never re-derive creation, fork, catalog or selection semantics.

import Foundation

// MARK: - Details

/// Capabilities this feature needs from the Source. `nil` means the Source did
/// not report capabilities at all; callers must not synthesize support.
struct SessionManagementCapabilities: Hashable, Sendable {
    let modelSelection: Bool
    let sessionFork: Bool
}

/// One Host-qualified session summary. The identity is included explicitly so
/// a details view can never mix two Hosts that reuse a session ID.
struct SessionDetail: Identifiable, Hashable, Sendable {
    let hostId: String
    let sessionId: String
    let title: String
    let cwd: String
    let createdAtUnixMs: Int64
    let updatedAtUnixMs: Int64
    let running: Bool
    let agentAvailable: Bool
    let blank: Bool
    let parentSessionId: String
    let origin: String
    let historyCursor: String
    let queuedPrompts: Int
    /// Source-reported capabilities for management actions.
    let management: SessionManagementCapabilities?

    var id: String { "\(hostId)/\(sessionId)" }

    var updatedAt: Date { Date(timeIntervalSince1970: Double(updatedAtUnixMs) / 1000) }
    var createdAt: Date { Date(timeIntervalSince1970: Double(createdAtUnixMs) / 1000) }

    /// Summary used to open the conversation root. It reuses the selected
    /// Host/session scope instead of creating a second session cache.
    var summary: SessionSummary {
        SessionSummary(
            sessionId: sessionId,
            title: title,
            updatedAtUnixMs: updatedAtUnixMs,
            cwd: cwd,
            running: running
        )
    }
}

// MARK: - Mutation receipts

enum SessionMutationKind: String, Hashable, Sendable {
    case create
    case fork
    case model
}

// Session mutations present their lifecycle through the shared
// `OperationStatus` values, so one status vocabulary is used by the
// conversation, Settings and this feature.

extension OperationStatus {
    /// Feature-localized label for a mutation receipt.
    var sessionLabelKey: L10nKey {
        switch self {
        case .submitted: return .sessionsReceiptSubmitted
        case .accepted: return .sessionsReceiptAccepted
        case .uncertain: return .sessionsReceiptUncertain
        case .failed: return .sessionsReceiptFailed
        case .confirmed: return .sessionsReceiptConfirmed
        }
    }
}

/// Lifecycle fields reported by Core for create and fork. Values stay optional
/// so an absent field is never shown as an empty verified value.
struct SessionLifecycle: Hashable, Sendable {
    let disposition: String
    let warningCode: String?
    let operationId: String?
    let phase: String?
    let reasonCode: String?
    let retryable: Bool
    let reconcileMode: String?
    let reconcileKey: String?

    init(
        disposition: String,
        warningCode: String? = nil,
        operationId: String? = nil,
        phase: String? = nil,
        reasonCode: String? = nil,
        retryable: Bool = false,
        reconcileMode: String? = nil,
        reconcileKey: String? = nil
    ) {
        self.disposition = disposition
        self.warningCode = warningCode
        self.operationId = operationId
        self.phase = phase
        self.reasonCode = reasonCode
        self.retryable = retryable
        self.reconcileMode = reconcileMode
        self.reconcileKey = reconcileKey
    }

    /// Core's lifecycle disposition mapped onto the shared operation status.
    var status: OperationStatus {
        switch disposition.uppercased() {
        case "READY", "READY_WITH_WARNING": return .confirmed
        case "FAILED": return .failed
        default: return .uncertain
        }
    }
}

/// One mutation outcome presented through the shared receipt shape.
///
/// Adapter point: when the foundation operation-recovery types
/// (`OperationReceipt` / `OperationStatus`) are available, this value is
/// converted into them without changing the mutation's operation identity.
struct SessionMutationReceipt: Identifiable, Hashable, Sendable {
    let kind: SessionMutationKind
    let hostId: String
    /// The session the mutation was submitted against, when one existed.
    let sessionId: String?
    let status: OperationStatus
    let command: CommandReceipt
    let lifecycle: SessionLifecycle?

    /// Adapter point to the shared retained-operation projection. Session
    /// mutations stay in Core's command journal; no second journal is created.
    /// Create without an existing session has no scope until the Host returns
    /// one, so it maps to a nil scope instead of an invented identity.
    func operationReceipt(instanceId: String) -> OperationReceipt {
        OperationReceipt(
            operationId: operationId,
            kind: .control,
            scope: sessionId.map {
                OperationScope(instanceId: instanceId, hostId: hostId, sessionId: $0)
            },
            status: status,
            receipt: command,
            reply: nil,
            content: nil,
            readError: nil
        )
    }

    var id: String { operationId }

    /// Stable identity to recover the operation under. A no-op selection has
    /// no journaled command identity; it stays empty instead of being invented.
    var operationId: String {
        if let operationId = lifecycle?.operationId, !operationId.isEmpty {
            return operationId
        }
        return command.commandId
    }

    var warningCode: String? { lifecycle?.warningCode }
    var reasonCode: String? { lifecycle?.reasonCode }
    var phase: String? { lifecycle?.phase }
    var retryable: Bool { lifecycle?.retryable ?? false }
    var reconcileMode: String? { lifecycle?.reconcileMode }
    var reconcileKey: String? { lifecycle?.reconcileKey }
    var isNoOp: Bool { command.commandId.isEmpty }
}

// MARK: - Create and fork outcomes

struct SessionCreateInput: Hashable, Sendable {
    let cwd: String?
    let workspaceId: String?
    let agentPreset: String?
    let preferredSessionId: String?

    static let defaults = SessionCreateInput(
        cwd: nil,
        workspaceId: nil,
        agentPreset: nil,
        preferredSessionId: nil
    )
}

struct SessionCreateOutcome: Hashable, Sendable {
    let hostId: String
    let receipt: SessionMutationReceipt
    /// Identity returned by the Host. `nil` for a failure or an unreconciled
    /// result; never inferred from the preferred ID.
    let sessionId: String?
    /// Authoritative detail read after a confirmed create. A failure here does
    /// not invalidate the confirmed identity.
    let session: SessionDetail?
    let detailError: AppErrorCode?

    /// Summary the caller can open. Falls back to the confirmed identity when
    /// the follow-up detail read failed.
    var openableSummary: SessionSummary? {
        if let session { return session.summary }
        guard let sessionId, receipt.status == .confirmed else { return nil }
        return SessionSummary(
            sessionId: sessionId,
            title: sessionId,
            updatedAtUnixMs: 0,
            cwd: "",
            running: false
        )
    }

}

struct SessionForkOutcome: Hashable, Sendable {
    let hostId: String
    let parentSessionId: String
    let receipt: SessionMutationReceipt
    let sessionId: String?
    /// Confirmed child working directory, shared with the parent.
    let workingDirectory: String?
    /// Why the directory read failed. The fork still succeeded.
    let workingDirectoryError: AppErrorCode?
    let child: SessionDetail?
    let childDetailError: AppErrorCode?

    var isConfirmed: Bool {
        receipt.status == .confirmed && sessionId != nil
    }

    /// Whether another fork attempt is allowed. A confirmed fork is never
    /// offered again under the same button.
    var canRetry: Bool {
        receipt.status == .failed && receipt.retryable
    }

    var openableSummary: SessionSummary? {
        if let child { return child.summary }
        guard isConfirmed, let sessionId else { return nil }
        return SessionSummary(
            sessionId: sessionId,
            title: sessionId,
            updatedAtUnixMs: 0,
            cwd: workingDirectory ?? "",
            running: false
        )
    }
}

// MARK: - Model catalog

struct ModelChoice: Identifiable, Hashable, Sendable {
    let provider: String
    let model: String
    let effort: String?

    var id: String { "\(provider)/\(model)/\(effort ?? "")" }

    /// Whether the choice is present in `catalog` with this exact effort.
    func isContained(in catalog: ModelCatalog) -> Bool {
        guard let catalogProvider = catalog.providers.first(where: { $0.id == provider }),
              let entry = catalogProvider.models.first(where: { $0.id == model }) else {
            return false
        }
        guard let effort else { return true }
        return entry.efforts.contains { $0.id == effort }
    }
}

struct ModelEffort: Identifiable, Hashable, Sendable {
    let id: String
    let name: String
}

struct ModelEntry: Identifiable, Hashable, Sendable {
    let id: String
    let name: String
    let efforts: [ModelEffort]
    let defaultEffort: String

    func defaultChoice(in provider: ModelProvider) -> ModelChoice {
        ModelChoice(
            provider: provider.id,
            model: id,
            effort: defaultEffort.isEmpty ? nil : defaultEffort
        )
    }
}

struct ModelProvider: Identifiable, Hashable, Sendable {
    let id: String
    let name: String
    let models: [ModelEntry]
}

struct ModelProviderFailure: Identifiable, Hashable, Sendable {
    let id: String
    let name: String
    let code: String
}

/// Authoritative model catalog. `complete == false` means at least one provider
/// failed; the successful providers stay selectable.
struct ModelCatalog: Hashable, Sendable {
    let defaultSelection: ModelChoice?
    let providers: [ModelProvider]
    let failures: [ModelProviderFailure]
    let complete: Bool
    let effectScope: String

    /// Stable snapshot identity used to detect a stale catalog before selecting.
    var fingerprint: String {
        providers
            .map { provider in
                provider.id + ":" + provider.models
                    .map { model in
                        model.id + "[" + model.efforts.map(\.id).joined(separator: ",") + "]"
                            + (model.defaultEffort.isEmpty ? "" : "=\(model.defaultEffort)")
                    }
                    .joined(separator: ",")
            }
            .joined(separator: "|")
    }

    var providerNamesWithFailures: [String] { failures.map(\.name) }
}

/// Passive session model observation. Every source value stays distinct.
struct SessionModelState: Hashable, Sendable {
    let pending: ModelChoice?
    let lastUsed: ModelChoice?
    let defaultSelection: ModelChoice?
    let defaultIsFallback: Bool
    let next: ModelChoice?
    let pendingKnown: Bool
}

struct ModelSelectionOutcome: Hashable, Sendable {
    let hostId: String
    let sessionId: String
    let receipt: SessionMutationReceipt
    let selected: ModelChoice?
    let effectScope: String?
    let defaultPersistence: String?

    /// Whether the session accepted the change. Core reports this through the
    /// command status, independently of source-default persistence.
    var sessionAccepted: Bool {
        receipt.status == .confirmed || receipt.status == .accepted
    }

    /// Core says the source default may not have changed.
    var defaultPersistenceUnconfirmed: Bool {
        defaultPersistence?.lowercased() == "unconfirmed"
    }

    /// False when Core treated the selection as a no-op, so no command was
    /// journaled and no recovery identity exists.
    var submitted: Bool { !receipt.isNoOp }
}
