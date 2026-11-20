// Live session-management adapter. Every value comes from the Rust binding;
// this file only maps typed records into the app's domain values so the views
// can stay testable against fixtures.

import Foundation
import WhaleReinMobile

extension LiveSessionService: SessionManagementService {
    func sessionCreateSupported(hostId: String) async throws -> Bool {
        let core = bindingCore
        do {
            return try await bindingCall { try core.sessionCreateSupported(hostId: hostId) }
        } catch {
            throw Self.sessionError(error)
        }
    }

    func createSession(hostId: String, input: SessionCreateInput) async throws -> SessionCreateOutcome {
        let core = bindingCore
        let view: SessionCreateResultView
        do {
            view = try await bindingCall {
                try core.createSession(
                    hostId: hostId,
                    input: SessionCreateInputView(
                        cwd: input.cwd,
                        workspaceId: input.workspaceId,
                        agentPreset: input.agentPreset,
                        preferredSessionId: input.preferredSessionId
                    )
                )
            }
        } catch {
            throw Self.sessionError(error)
        }
        let status = Self.mutationStatus(forDisposition: view.disposition)
        let lifecycle = SessionLifecycle(
            disposition: view.disposition,
            warningCode: view.warningCode,
            operationId: view.operationId,
            phase: view.phase,
            reasonCode: view.reasonCode,
            retryable: view.retryable,
            reconcileMode: view.reconcileMode,
            reconcileKey: view.reconcileKey
        )
        let receipt = SessionMutationReceipt(
            kind: .create,
            hostId: hostId,
            sessionId: view.sessionId,
            status: status,
            command: Self.commandReceipt(view.receipt),
            lifecycle: lifecycle
        )
        // A confirmed identity survives a failed follow-up detail read; the
        // failure is reported beside the identity, not as a create failure.
        var detail: SessionDetail?
        var detailError: AppErrorCode?
        if status == .confirmed, let sessionId = view.sessionId {
            do {
                detail = try await sessionDetails(hostId: hostId, sessionId: sessionId)
            } catch {
                detailError = (error as? SessionServiceError)?.code ?? .internalError
            }
        }
        return SessionCreateOutcome(
            hostId: hostId,
            receipt: receipt,
            sessionId: view.sessionId,
            session: detail,
            detailError: detailError
        )
    }

    func sessionDetails(hostId: String, sessionId: String) async throws -> SessionDetail {
        let core = bindingCore
        do {
            let view = try await bindingCall {
                try core.sessionDetails(hostId: hostId, sessionId: sessionId)
            }
            return Self.detail(view)
        } catch {
            throw Self.sessionError(error)
        }
    }

    func forkSession(hostId: String, sessionId: String) async throws -> SessionForkOutcome {
        let core = bindingCore
        let view: ForkResultView
        do {
            view = try await bindingCall {
                try core.forkSessionOutcome(hostId: hostId, sessionId: sessionId)
            }
        } catch {
            throw Self.sessionError(error)
        }
        let status = Self.mutationStatus(forDisposition: view.disposition)
        let lifecycle = SessionLifecycle(
            disposition: view.disposition,
            warningCode: view.warningCode,
            operationId: view.operationId,
            phase: view.phase,
            reasonCode: view.reasonCode,
            retryable: view.retryable,
            reconcileMode: view.reconcileMode,
            reconcileKey: view.reconcileKey
        )
        let receipt = SessionMutationReceipt(
            kind: .fork,
            hostId: hostId,
            sessionId: view.sessionId ?? sessionId,
            status: status,
            command: Self.commandReceipt(view.receipt),
            lifecycle: lifecycle
        )
        var child: SessionDetail?
        var childError: AppErrorCode?
        if status == .confirmed, let childId = view.sessionId {
            do {
                child = try await sessionDetails(hostId: hostId, sessionId: childId)
            } catch {
                childError = (error as? SessionServiceError)?.code ?? .internalError
            }
        }
        return SessionForkOutcome(
            hostId: hostId,
            parentSessionId: view.parentSessionId ?? sessionId,
            receipt: receipt,
            sessionId: view.sessionId,
            workingDirectory: view.workingDirectory,
            workingDirectoryError: view.workingDirectoryError.map { LiveSessionService.map(code: $0) },
            child: child,
            childDetailError: childError
        )
    }

    func modelCatalog(hostId: String) async throws -> ModelCatalog {
        let core = bindingCore
        do {
            let view = try await bindingCall { try core.modelCatalog(hostId: hostId) }
            return Self.catalog(view)
        } catch {
            throw Self.sessionError(error)
        }
    }

    func readSessionModel(hostId: String, sessionId: String) async throws -> SessionModelState {
        let core = bindingCore
        do {
            let view = try await bindingCall {
                try core.readSessionModel(hostId: hostId, sessionId: sessionId)
            }
            return Self.sessionModel(view)
        } catch {
            throw Self.sessionError(error)
        }
    }

    func selectSessionModel(
        hostId: String,
        sessionId: String,
        choice: ModelChoice
    ) async throws -> ModelSelectionOutcome {
        let core = bindingCore
        let view: ModelSelectionResultView
        do {
            view = try await bindingCall {
                try core.selectSessionModel(
                    hostId: hostId,
                    sessionId: sessionId,
                    provider: choice.provider,
                    model: choice.model,
                    effort: choice.effort
                )
            }
        } catch {
            throw Self.sessionError(error)
        }
        let receipt = SessionMutationReceipt(
            kind: .model,
            hostId: hostId,
            sessionId: sessionId,
            status: Self.mutationStatus(forCommandStatus: view.receipt.status),
            command: Self.commandReceipt(view.receipt),
            lifecycle: nil
        )
        return ModelSelectionOutcome(
            hostId: hostId,
            sessionId: sessionId,
            receipt: receipt,
            selected: view.selected.map(Self.choice),
            effectScope: view.effectScope,
            defaultPersistence: view.defaultPersistence
        )
    }

    // MARK: - Mapping

    private static func commandReceipt(_ view: CommandView) -> CommandReceipt {
        CommandReceipt(
            commandId: view.commandId,
            requestId: view.requestId,
            status: commandStatus(view.status),
            delivery: nil,
            messageId: view.messageId,
            deliveryId: view.deliveryId
        )
    }

    private static func commandStatus(_ status: String) -> CommandStatus {
        switch status.lowercased() {
        case "succeeded", "confirmed": return .confirmed
        case "received", "host_accepted", "accepted": return .accepted
        case "rejected", "failed": return .rejected
        case "cancelled": return .cancelled
        default: return .uncertain
        }
    }

    private static func mutationStatus(forCommandStatus status: String) -> SessionMutationStatus {
        switch commandStatus(status) {
        case .confirmed: return .confirmed
        case .accepted: return .accepted
        case .rejected, .cancelled: return .failed
        case .uncertain: return .uncertain
        }
    }

    static func mutationStatus(forDisposition disposition: String) -> SessionMutationStatus {
        switch disposition.uppercased() {
        case "READY", "READY_WITH_WARNING": return .confirmed
        case "FAILED": return .failed
        default: return .uncertain
        }
    }

    private static func detail(_ view: SessionDetailView) -> SessionDetail {
        SessionDetail(
            hostId: view.hostId,
            sessionId: view.sessionId,
            title: view.title,
            cwd: view.cwd,
            createdAtUnixMs: view.createdAtUnixMs,
            updatedAtUnixMs: view.updatedAtUnixMs,
            running: view.running,
            agentAvailable: view.agentAvailable,
            blank: view.blank,
            parentSessionId: view.parentSessionId,
            origin: view.origin,
            historyCursor: view.historyCursor,
            queuedPrompts: Int(view.queuedPrompts),
            management: view.capabilities.map {
                SessionManagementCapabilities(
                    modelSelection: $0.modelSelection,
                    sessionFork: $0.sessionFork
                )
            }
        )
    }

    private static func choice(_ view: ModelChoiceView) -> ModelChoice {
        ModelChoice(provider: view.provider, model: view.model, effort: view.effort)
    }

    private static func catalog(_ view: ModelCatalogView) -> ModelCatalog {
        ModelCatalog(
            defaultSelection: view.defaultSelection.map(choice),
            providers: view.providers.map { provider in
                ModelProvider(
                    id: provider.id,
                    name: provider.name,
                    models: provider.models.map { model in
                        ModelEntry(
                            id: model.id,
                            name: model.name,
                            efforts: model.efforts.map { ModelEffort(id: $0.id, name: $0.name) },
                            defaultEffort: model.defaultEffort
                        )
                    }
                )
            },
            failures: view.failures.map {
                ModelProviderFailure(id: $0.id, name: $0.name, code: $0.code)
            },
            complete: view.complete,
            effectScope: view.effectScope
        )
    }

    private static func sessionModel(_ view: SessionModelView) -> SessionModelState {
        SessionModelState(
            pending: view.pending.map(choice),
            lastUsed: view.lastUsed.map(choice),
            defaultSelection: view.defaultSelection.map(choice),
            defaultIsFallback: view.defaultIsFallback,
            next: view.next.map(choice),
            pendingKnown: view.pendingKnown
        )
    }

    private static func sessionError(_ error: Error) -> SessionServiceError {
        if let serviceError = error as? SessionServiceError { return serviceError }
        let code = (error as? MobileError).map { LiveSessionService.map(code: $0.code) }
        return SessionServiceError(code ?? .internalError)
    }
}
