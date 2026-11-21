// Presentation state for session creation, details, fork and model selection.
// Values and outcomes are owned by Core through the service adapter; this model
// only sequences requests, validation and presentation states.

import Foundation
import Observation

struct SessionScope: Hashable, Sendable {
    let hostId: String
    let sessionId: String
}

/// Client-side create validation. Core remains authoritative for every value;
/// these two rules only prevent an obviously unusable submission.
enum SessionCreateValidation: Equatable, Sendable {
    case bothLocations
    case relativeHostPath

    var messageKey: L10nKey {
        switch self {
        case .bothLocations: return .sessionsCreateLocationConflict
        case .relativeHostPath: return .sessionsCreatePathInvalid
        }
    }
}

/// Form values for Host-scoped creation. Values are forwarded unchanged; an
/// empty field is omitted, not defaulted.
struct SessionCreateForm: Equatable, Sendable {
    var useDefaults = true
    var workspaceId = ""
    var cwd = ""
    var agentPreset = ""
    var preferredSessionId = ""

    var input: SessionCreateInput {
        guard !useDefaults else { return .defaults }
        return SessionCreateInput(
            cwd: cwd.isEmpty ? nil : cwd,
            workspaceId: workspaceId.isEmpty ? nil : workspaceId,
            agentPreset: agentPreset.isEmpty ? nil : agentPreset,
            preferredSessionId: preferredSessionId.isEmpty ? nil : preferredSessionId
        )
    }

    var validation: SessionCreateValidation? {
        guard !useDefaults else { return nil }
        let trimmedWorkspace = workspaceId.trimmingCharacters(in: .whitespaces)
        let trimmedCwd = cwd.trimmingCharacters(in: .whitespaces)
        if !trimmedWorkspace.isEmpty && !trimmedCwd.isEmpty {
            return .bothLocations
        }
        if !trimmedCwd.isEmpty && !Self.isAbsoluteHostPath(trimmedCwd) {
            return .relativeHostPath
        }
        return nil
    }

    /// POSIX, drive-letter and UNC forms the Host accepts. Blank values are
    /// handled separately; this only answers "is it absolute".
    static func isAbsoluteHostPath(_ value: String) -> Bool {
        if value.hasPrefix("/") || value.hasPrefix("\\\\") { return true }
        let scalars = Array(value)
        guard scalars.count >= 3, scalars[1] == ":" else { return false }
        return scalars[2] == "\\" || scalars[2] == "/"
    }
}

enum SessionCreateState: Equatable {
    case idle
    case creating
    case result(SessionCreateOutcome)
    case rejected(SessionCreateValidation)
    case failed(AppErrorCode)
}

enum SessionDetailsState: Equatable {
    case idle
    case loading
    case loaded(SessionDetail)
    case unavailable(AppErrorCode)
}

enum SessionForkState: Equatable {
    case idle
    case forking
    case result(SessionForkOutcome)
    case failed(AppErrorCode)
}

enum ModelSelectionState: Equatable {
    case idle
    case submitting
    case result(ModelSelectionOutcome)
    case stale
    case failed(AppErrorCode)
}

@MainActor
@Observable
final class SessionManagementModel {
    private let service: any SessionManagementService

    // Create
    var createForm = SessionCreateForm()
    private(set) var createSupport: [String: Bool] = [:]
    private(set) var createState: SessionCreateState = .idle

    // Details
    private(set) var detailsScope: SessionScope?
    private(set) var detailsState: SessionDetailsState = .idle

    // Fork
    private(set) var forkScope: SessionScope?
    private(set) var forkState: SessionForkState = .idle

    // Model
    private(set) var modelScope: SessionScope?
    private(set) var catalog: ModelCatalog?
    private(set) var catalogError: AppErrorCode?
    private(set) var sessionModel: SessionModelState?
    private(set) var sessionModelError: AppErrorCode?
    private(set) var isModelLoading = false
    private(set) var selectionState: ModelSelectionState = .idle
    /// Set when a selection was refused because the loaded catalog no longer
    /// lists the requested choice.
    private(set) var isCatalogStale = false
    /// A post-selection read failed; the accepted receipt stays visible.
    private(set) var modelRefreshError: AppErrorCode?

    /// Disclosed before the first submission: the native operation also asks
    /// DSH to update the default model for future sessions.
    var defaultEffectDisclosureKey: L10nKey { .modelDefaultEffectDisclosure }

    init(service: any SessionManagementService) {
        self.service = service
    }

    /// Instance that owns the active adapter; used for the shared receipt scope.
    var instanceId: String { service.instanceId }

    /// Shared retained-operation projection for one mutation receipt.
    func sharedReceipt(for receipt: SessionMutationReceipt) -> OperationReceipt {
        receipt.operationReceipt(instanceId: instanceId)
    }

    // MARK: - Create

    func canCreate(hostId: String) -> Bool { createSupport[hostId] == true }

    func createSupportKnown(hostId: String) -> Bool { createSupport[hostId] != nil }

    /// Probe the Host capability once per Host. A failed probe is not treated as
    /// support; it disables creation until a later refresh succeeds.
    func loadCreateSupport(hostId: String) async {
        guard createSupport[hostId] == nil else { return }
        do {
            createSupport[hostId] = try await service.sessionCreateSupported(hostId: hostId)
        } catch {
            createSupport[hostId] = false
        }
    }

    func refreshCreateSupport(hostId: String) async {
        createSupport[hostId] = nil
        await loadCreateSupport(hostId: hostId)
    }

    func resetCreate() {
        createForm = SessionCreateForm()
        createState = .idle
    }

    @discardableResult
    func submitCreate(hostId: String) async -> SessionCreateOutcome? {
        if let validation = createForm.validation {
            createState = .rejected(validation)
            return nil
        }
        guard canCreate(hostId: hostId) else {
            createState = .failed(
                createSupportKnown(hostId: hostId) ? .unsupportedCapability : .unavailable
            )
            return nil
        }
        createState = .creating
        do {
            let outcome = try await service.createSession(
                hostId: hostId,
                input: createForm.input
            )
            createState = .result(outcome)
            return outcome
        } catch {
            createState = .failed((error as? SessionServiceError)?.code ?? .internalError)
            return nil
        }
    }

    // MARK: - Details

    func loadDetails(hostId: String, sessionId: String) async {
        let scope = SessionScope(hostId: hostId, sessionId: sessionId)
        if detailsScope != scope {
            detailsState = .idle
            detailsScope = scope
        }
        detailsState = .loading
        do {
            detailsState = .loaded(
                try await service.sessionDetails(hostId: hostId, sessionId: sessionId)
            )
        } catch {
            detailsState = .unavailable((error as? SessionServiceError)?.code ?? .internalError)
        }
    }

    /// Reloads a confirmed identity without discarding it. Used after an
    /// uncertain create result to look the session up under the original
    /// command.
    func refreshDetails(hostId: String, sessionId: String) async -> SessionDetail? {
        do {
            let detail = try await service.sessionDetails(
                hostId: hostId,
                sessionId: sessionId
            )
            let scope = SessionScope(hostId: hostId, sessionId: sessionId)
            detailsScope = scope
            detailsState = .loaded(detail)
            return detail
        } catch {
            return nil
        }
    }

    // MARK: - Fork

    func isForkConfirmed(for scope: SessionScope) -> Bool {
        guard forkScope == scope, case .result(let outcome) = forkState else { return false }
        return outcome.isConfirmed
    }

    func resetFork() {
        forkState = .idle
        forkScope = nil
    }

    @discardableResult
    func forkSession(hostId: String, sessionId: String) async -> SessionForkOutcome? {
        let scope = SessionScope(hostId: hostId, sessionId: sessionId)
        if forkScope != scope {
            forkScope = scope
            forkState = .idle
        }
        // A confirmed fork is never submitted again from the same action; the
        // child already exists and its identity is authoritative.
        if case .result(let existing) = forkState, existing.isConfirmed {
            return existing
        }
        forkState = .forking
        do {
            let outcome = try await service.forkSession(hostId: hostId, sessionId: sessionId)
            forkState = .result(outcome)
            return outcome
        } catch {
            forkState = .failed((error as? SessionServiceError)?.code ?? .internalError)
            return nil
        }
    }

    // MARK: - Model

    func loadModel(hostId: String, sessionId: String) async {
        let scope = SessionScope(hostId: hostId, sessionId: sessionId)
        if modelScope != scope {
            modelScope = scope
            catalog = nil
            sessionModel = nil
            catalogError = nil
            sessionModelError = nil
            selectionState = .idle
            isCatalogStale = false
            modelRefreshError = nil
        }
        isModelLoading = true
        // The catalog and the passive session observation are independent: one
        // failing must not hide the other.
        do {
            catalog = try await service.modelCatalog(hostId: hostId)
            catalogError = nil
        } catch {
            catalog = nil
            catalogError = (error as? SessionServiceError)?.code ?? .internalError
        }
        do {
            sessionModel = try await service.readSessionModel(
                hostId: hostId,
                sessionId: sessionId
            )
            sessionModelError = nil
        } catch {
            sessionModelError = (error as? SessionServiceError)?.code ?? .internalError
        }
        isModelLoading = false
    }

    func refreshSessionModel(hostId: String, sessionId: String) async {
        do {
            sessionModel = try await service.readSessionModel(
                hostId: hostId,
                sessionId: sessionId
            )
            sessionModelError = nil
            modelRefreshError = nil
        } catch {
            modelRefreshError = (error as? SessionServiceError)?.code ?? .internalError
        }
    }

    /// Submit exactly one selection after checking the loaded catalog still
    /// contains it. The caller discloses the default effect before this call.
    @discardableResult
    func selectModel(
        hostId: String,
        sessionId: String,
        choice: ModelChoice
    ) async -> ModelSelectionOutcome? {
        if case .submitting = selectionState { return nil }
        guard let catalog, choice.isContained(in: catalog) else {
            isCatalogStale = true
            selectionState = .stale
            return nil
        }
        selectionState = .submitting
        isCatalogStale = false
        do {
            let outcome = try await service.selectSessionModel(
                hostId: hostId,
                sessionId: sessionId,
                choice: choice
            )
            selectionState = .result(outcome)
            await refreshSessionModel(hostId: hostId, sessionId: sessionId)
            return outcome
        } catch {
            selectionState = .failed((error as? SessionServiceError)?.code ?? .internalError)
            return nil
        }
    }

    func clearStaleCatalog() {
        isCatalogStale = false
        if case .stale = selectionState {
            selectionState = .idle
        }
    }
}
