// Deterministic fixture implementation of the session-management adapter.
// The state object is referenced (not copied) so a test can set an outcome
// before driving the model while the service keeps one consistent session
// table. No network, no host.

import Foundation

/// Configurable native outcomes for fixture-driven session management.
@MainActor
final class FixtureSessionManagementState {
    // Creation
    var createSupported = true
    /// `READY`, `READY_WITH_WARNING`, `FAILED` or `UNKNOWN`.
    var createDisposition = "READY"
    var createWarningCode: String?
    var createReasonCode = "WORKSPACE_NOT_FOUND"
    var createRetryable = true

    // Fork
    /// `READY`, `READY_WITH_WARNING`, `FAILED` or `UNKNOWN`.
    var forkDisposition = "READY"
    var forkWarningCode: String?
    var forkReasonCode = "SOURCE_NO_COMPLETE_TURN"
    var forkRetryable = false
    var forkDirectory: String? = "/work/whalerein"
    /// When set, the child summary read fails with this binding code.
    var forkDirectoryErrorCode: String?
    var childDetailFails = false

    // Model
    var catalogComplete = true
    var catalogFailure: ModelProviderFailure?
    var catalogUnavailable = false
    var catalogOffline = false
    var sessionModelUnavailable = false
    var defaultPersistence = "unconfirmed"
    /// `READY`, `REJECTED` or `UNKNOWN` command outcomes for selection.
    var selectionOutcome = "READY"
    var pending: ModelChoice?
    var pendingKnown = true
    var lastUsed: ModelChoice? = FixtureModelCatalog.next
    var next: ModelChoice? = FixtureModelCatalog.next
    var defaultSelection: ModelChoice? = FixtureModelCatalog.defaultSelection
    var defaultIsFallback = true

    /// Sessions created through this fixture, keyed by returned identity.
    private(set) var createdSessions: [SessionDetail] = []
    /// Recorded accepted selection per session.
    private(set) var selectedBySession: [String: ModelChoice] = [:]
    /// Inputs the create form actually submitted, for pass-through assertions.
    private(set) var lastCreateInput: SessionCreateInput?
    private(set) var createCount = 0
    private(set) var forkCount = 0
    private(set) var selectionCount = 0

    func selectedModel(for sessionId: String) -> ModelChoice? {
        selectedBySession[sessionId]
    }

    /// Fixture-only bookkeeping used by the adapter.
    func recordCreate(input: SessionCreateInput, detail: SessionDetail) {
        lastCreateInput = input
        createCount += 1
        createdSessions.append(detail)
    }

    func recordFork() {
        forkCount += 1
    }

    func recordSelection(sessionId: String, choice: ModelChoice) {
        selectionCount += 1
        selectedBySession[sessionId] = choice
    }
}

enum FixtureModelCatalog {
    static let low = ModelEffort(id: "low", name: "Low")
    static let medium = ModelEffort(id: "medium", name: "Medium")
    static let high = ModelEffort(id: "high", name: "High")

    static let defaultSelection = ModelChoice(
        provider: "anthropic",
        model: "sonnet",
        effort: "medium"
    )
    static let next = ModelChoice(provider: "anthropic", model: "sonnet", effort: "medium")

    static let providers: [ModelProvider] = [
        ModelProvider(
            id: "anthropic",
            name: "Anthropic",
            models: [
                ModelEntry(
                    id: "sonnet",
                    name: "Sonnet",
                    efforts: [low, medium, high],
                    defaultEffort: "medium"
                ),
                ModelEntry(
                    id: "opus",
                    name: "Opus",
                    efforts: [low, medium, high],
                    defaultEffort: "high"
                ),
            ]
        ),
        ModelProvider(
            id: "openai",
            name: "OpenAI",
            models: [
                ModelEntry(
                    id: "gpt",
                    name: "GPT",
                    efforts: [low, medium],
                    defaultEffort: "medium"
                ),
            ]
        ),
    ]

    @MainActor
    static func catalog(for state: FixtureSessionManagementState) throws -> ModelCatalog {
        if state.catalogUnavailable {
            throw SessionServiceError(.unsupportedCapability)
        }
        if state.catalogOffline {
            throw SessionServiceError(.hostOffline)
        }
        let failures = state.catalogFailure.map { [$0] } ?? []
        return ModelCatalog(
            defaultSelection: state.defaultSelection,
            providers: providers,
            failures: failures,
            complete: state.catalogComplete && failures.isEmpty,
            effectScope: "session_and_source_default_attempt"
        )
    }
}

extension FixtureSessionService: SessionManagementService {
    private var managementState: FixtureSessionManagementState { sessionManagement }

    func sessionCreateSupported(hostId: String) async throws -> Bool {
        managementState.createSupported
    }

    func createSession(
        hostId: String,
        input: SessionCreateInput
    ) async throws -> SessionCreateOutcome {
        let state = managementState
        let returnedId: String?
        switch state.createDisposition {
        case "FAILED":
            return SessionCreateOutcome(
                hostId: hostId,
                receipt: fixtureReceipt(
                    kind: .create,
                    hostId: hostId,
                    sessionId: nil,
                    disposition: "FAILED",
                    reasonCode: state.createReasonCode,
                    retryable: state.createRetryable
                ),
                sessionId: nil,
                session: nil,
                detailError: nil
            )
        case "UNKNOWN":
            return SessionCreateOutcome(
                hostId: hostId,
                receipt: fixtureReceipt(
                    kind: .create,
                    hostId: hostId,
                    sessionId: nil,
                    disposition: "UNKNOWN",
                    reasonCode: state.createReasonCode,
                    retryable: false,
                    reconcileMode: "PREFERRED_SESSION_ID",
                    reconcileKey: input.preferredSessionId ?? "s-created-unknown"
                ),
                sessionId: nil,
                session: nil,
                detailError: nil
            )
        default:
            // The Host reuses the preferred identity or assigns a new one.
            returnedId = input.preferredSessionId ?? "s-created-\(state.createCount + 1)"
        }
        guard let returnedId else {
            return SessionCreateOutcome(
                hostId: hostId,
                receipt: fixtureReceipt(
                    kind: .create,
                    hostId: hostId,
                    sessionId: nil,
                    disposition: "UNKNOWN",
                    reasonCode: "NATIVE_INTERRUPTED",
                    retryable: false
                ),
                sessionId: nil,
                session: nil,
                detailError: nil
            )
        }
        let detail = createdDetail(
            hostId: hostId,
            sessionId: returnedId,
            cwd: input.cwd ?? "/work/whalerein"
        )
        state.recordCreate(input: input, detail: detail)
        let disposition = state.createDisposition == "READY_WITH_WARNING"
            ? "READY_WITH_WARNING"
            : "READY"
        return SessionCreateOutcome(
            hostId: hostId,
            receipt: fixtureReceipt(
                kind: .create,
                hostId: hostId,
                sessionId: returnedId,
                disposition: disposition,
                warningCode: disposition == "READY_WITH_WARNING"
                    ? (state.createWarningCode ?? "workspace_attach_failed")
                    : nil
            ),
            sessionId: returnedId,
            session: detail,
            detailError: nil
        )
    }

    func sessionDetails(hostId: String, sessionId: String) async throws -> SessionDetail {
        if let created = managementState.createdSessions.last(where: {
            $0.hostId == hostId && $0.sessionId == sessionId
        }) {
            return created
        }
        if let row = FixtureCatalog.sessions(hostId: hostId).first(where: {
            $0.sessionId == sessionId
        }) {
            return SessionDetail(
                hostId: hostId,
                sessionId: row.sessionId,
                title: row.title,
                cwd: row.cwd,
                createdAtUnixMs: row.updatedAtUnixMs - 3_600_000,
                updatedAtUnixMs: row.updatedAtUnixMs,
                running: row.running,
                agentAvailable: true,
                blank: false,
                parentSessionId: "",
                origin: "native",
                historyCursor: "fixture-cursor-\(row.sessionId)",
                queuedPrompts: 0,
                management: SessionManagementCapabilities(modelSelection: true, sessionFork: true)
            )
        }
        if let row = FixtureCatalog.sessions(hostId: hostId).first,
           sessionId == "s-forked-child" {
            return SessionDetail(
                hostId: hostId,
                sessionId: sessionId,
                title: "Forked \(row.title)",
                cwd: row.cwd,
                createdAtUnixMs: row.updatedAtUnixMs,
                updatedAtUnixMs: row.updatedAtUnixMs,
                running: false,
                agentAvailable: true,
                blank: false,
                parentSessionId: row.sessionId,
                origin: "fork",
                historyCursor: "fixture-cursor-\(sessionId)",
                queuedPrompts: 0,
                management: SessionManagementCapabilities(modelSelection: true, sessionFork: true)
            )
        }
        throw SessionServiceError(.unavailable)
    }

    func forkSession(hostId: String, sessionId: String) async throws -> SessionForkOutcome {
        let state = managementState
        state.recordFork()
        switch state.forkDisposition {
        case "FAILED":
            return SessionForkOutcome(
                hostId: hostId,
                parentSessionId: sessionId,
                receipt: fixtureReceipt(
                    kind: .fork,
                    hostId: hostId,
                    sessionId: sessionId,
                    disposition: "FAILED",
                    reasonCode: state.forkReasonCode,
                    retryable: state.forkRetryable
                ),
                sessionId: nil,
                workingDirectory: nil,
                workingDirectoryError: nil,
                child: nil,
                childDetailError: nil
            )
        case "UNKNOWN":
            return SessionForkOutcome(
                hostId: hostId,
                parentSessionId: sessionId,
                receipt: fixtureReceipt(
                    kind: .fork,
                    hostId: hostId,
                    sessionId: sessionId,
                    disposition: "UNKNOWN",
                    reasonCode: state.forkReasonCode,
                    retryable: false,
                    reconcileMode: "PARENT_SESSION_ID",
                    reconcileKey: sessionId
                ),
                sessionId: nil,
                workingDirectory: nil,
                workingDirectoryError: nil,
                child: nil,
                childDetailError: nil
            )
        default:
            break
        }
        let childId = "\(sessionId)-fork-\(state.forkCount + 1)"
        let directoryError = state.forkDirectoryErrorCode
            .map { LiveSessionService.map(code: $0) }
        var directory = state.forkDirectory
        if directoryError != nil { directory = nil }
        let child: SessionDetail? = state.childDetailFails
            ? nil
            : SessionDetail(
                hostId: hostId,
                sessionId: childId,
                title: "Fork of \(sessionId)",
                cwd: directory ?? "",
                createdAtUnixMs: 0,
                updatedAtUnixMs: 0,
                running: false,
                agentAvailable: true,
                blank: true,
                parentSessionId: sessionId,
                origin: "fork",
                historyCursor: "fixture-cursor-\(childId)",
                queuedPrompts: 0,
                management: SessionManagementCapabilities(modelSelection: true, sessionFork: true)
            )
        let disposition = state.forkDisposition == "READY_WITH_WARNING"
            ? "READY_WITH_WARNING"
            : "READY"
        return SessionForkOutcome(
            hostId: hostId,
            parentSessionId: sessionId,
            receipt: fixtureReceipt(
                kind: .fork,
                hostId: hostId,
                sessionId: childId,
                disposition: disposition,
                warningCode: disposition == "READY_WITH_WARNING"
                    ? (state.forkWarningCode ?? "workspace_attach_failed")
                    : nil
            ),
            sessionId: childId,
            workingDirectory: directory,
            workingDirectoryError: directoryError,
            child: child,
            childDetailError: state.childDetailFails ? .unavailable : nil
        )
    }

    func modelCatalog(hostId: String) async throws -> ModelCatalog {
        try FixtureModelCatalog.catalog(for: managementState)
    }

    func readSessionModel(hostId: String, sessionId: String) async throws -> SessionModelState {
        let state = managementState
        if state.sessionModelUnavailable {
            throw SessionServiceError(.unsupportedCapability)
        }
        let selected = state.selectedModel(for: sessionId)
        return SessionModelState(
            pending: state.pending,
            lastUsed: state.lastUsed,
            defaultSelection: state.defaultSelection,
            defaultIsFallback: state.defaultIsFallback,
            next: selected ?? state.next,
            pendingKnown: state.pendingKnown
        )
    }

    func selectSessionModel(
        hostId: String,
        sessionId: String,
        choice: ModelChoice
    ) async throws -> ModelSelectionOutcome {
        let state = managementState
        state.recordSelection(sessionId: sessionId, choice: choice)
        let status: OperationStatus
        let commandStatus: CommandStatus
        switch state.selectionOutcome {
        case "REJECTED":
            status = .failed
            commandStatus = .rejected
        case "UNKNOWN":
            status = .uncertain
            commandStatus = .uncertain
        default:
            status = .confirmed
            commandStatus = .confirmed
        }
        let receipt = SessionMutationReceipt(
            kind: .model,
            hostId: hostId,
            sessionId: sessionId,
            status: status,
            command: CommandReceipt(
                commandId: status == .uncertain ? "" : "fixture-model-\(state.selectionCount)",
                requestId: "fixture-model-request-\(state.selectionCount)",
                status: commandStatus,
                delivery: nil
            ),
            lifecycle: nil
        )
        return ModelSelectionOutcome(
            hostId: hostId,
            sessionId: sessionId,
            receipt: receipt,
            selected: status == .failed ? nil : choice,
            effectScope: "session_and_source_default_attempt",
            defaultPersistence: state.defaultPersistence
        )
    }

    private func createdDetail(hostId: String, sessionId: String, cwd: String) -> SessionDetail {
        SessionDetail(
            hostId: hostId,
            sessionId: sessionId,
            title: "New session",
            cwd: cwd,
            createdAtUnixMs: 0,
            updatedAtUnixMs: 0,
            running: false,
            agentAvailable: true,
            blank: true,
            parentSessionId: "",
            origin: "native",
            historyCursor: "",
            queuedPrompts: 0,
            management: SessionManagementCapabilities(modelSelection: true, sessionFork: true)
        )
    }
}

/// Builds one fixture mutation receipt with deterministic command identity.
private func fixtureReceipt(
    kind: SessionMutationKind,
    hostId: String,
    sessionId: String?,
    disposition: String,
    warningCode: String? = nil,
    reasonCode: String? = nil,
    retryable: Bool = false,
    reconcileMode: String? = nil,
    reconcileKey: String? = nil
) -> SessionMutationReceipt {
    let lifecycle = SessionLifecycle(
        disposition: disposition,
        warningCode: warningCode,
        operationId: "fixture-\(kind.rawValue)-operation",
        phase: "COMPLETE",
        reasonCode: reasonCode,
        retryable: retryable,
        reconcileMode: reconcileMode,
        reconcileKey: reconcileKey
    )
    return SessionMutationReceipt(
        kind: kind,
        hostId: hostId,
        sessionId: sessionId,
        status: lifecycle.status,
        command: CommandReceipt(
            commandId: "fixture-\(kind.rawValue)-command",
            requestId: "fixture-\(kind.rawValue)-request",
            status: lifecycle.status == .failed ? .rejected : .confirmed,
            delivery: nil
        ),
        lifecycle: lifecycle
    )
}
