// Settings account and computer presentation: typed instance status, the
// Core-backed default computer, and scoped Cloud app sign-ins.
//
// The model never holds credentials. It asks Core for the locally retained
// current Cloud session identity so the listing can label this app, and it
// reuses the Cloud sign-in branch's `CloudAccountService` session methods for
// exactly scoped single or account-wide revocation with pending results.

import Foundation
import Observation

enum AccountSessionScope: Equatable, Sendable {
    case one(String)
    case all
}

struct AccountAppSession: Identifiable, Equatable, Sendable {
    let sessionId: String
    let instanceId: String
    let expiresAt: UInt64
    var state: CloudAccountSessionState
    let isCurrent: Bool

    var id: String { sessionId }
    var expiresAtDate: Date { Date(timeIntervalSince1970: TimeInterval(expiresAt)) }
    var isActive: Bool { state == .active }

    var stateKey: L10nKey {
        switch state {
        case .active: return .settingsAppSessionsActive
        case .pending: return .settingsAppSessionsPendingState
        case .revoked: return .settingsAppSessionsRevokedState
        }
    }
}

/// Authoritative result of a scoped revocation. `scope` is what the user asked
/// for; `state` is what Cloud/Core reported, so a pending account-wide result is
/// never presented as a confirmed single sign-out.
struct AccountSessionRevocation: Equatable, Sendable {
    let scope: AccountSessionScope
    let state: CloudAccountSessionState
    let affectedSessionIds: [String]

    var isPending: Bool { state == .pending }
    var isConfirmed: Bool { state == .revoked }
}

@MainActor
@Observable
final class AccountSettingsModel {
    enum LoadState: Equatable {
        case idle
        case loading
        case loaded
        case failed(AppErrorCode)
    }

    /// One business state; the legacy names below are read-only projections.
    private(set) var state = AccountState()

    private(set) var hosts: [HostSummary] = []

    var status: InstanceStatus? { state.status.value }
    var statusState: LoadState { Self.legacy(state.status) }
    var statusError: AppErrorCode? { state.status.error }
    var defaultHostId: String? { state.defaultHostId }
    var defaultHostError: AppErrorCode? { state.defaultHostError }
    var sessions: [AccountAppSession] { state.sessions.value ?? [] }
    var sessionsState: LoadState { Self.legacy(state.sessions) }
    var sessionsError: AppErrorCode? { state.sessions.error }
    var revokingScope: AccountSessionScope? {
        if case .revoking(let scope) = state.revocation { return scope }
        return nil
    }
    var lastRevocation: AccountSessionRevocation? {
        if case .settled(let result) = state.revocation { return result }
        return nil
    }
    var statusKey: L10nKey? { state.statusKey }

    private static func legacy<Value: Equatable & Sendable>(_ load: Load<Value>) -> LoadState {
        switch load {
        case .idle: .idle
        case .loading: .loading
        case .loaded: .loaded
        case .failed(let code): .failed(code)
        }
    }

    private let service: any SessionService
    private let statusService: (any InstanceStatusService)?
    private let cloud: (any CloudAccountService)?
    private var currentSessionId: String?

    /// Called only when the current app session is affected (single current
    /// revocation or any account-wide revocation).
    var onSignedOut: (() -> Void)?

    init(service: any SessionService) {
        self.service = service
        self.statusService = service as? any InstanceStatusService
        self.cloud = service as? any CloudAccountService
    }

    var isCloud: Bool { status?.service == .cloud }
    var isSignedIn: Bool { status?.accountState == .signedIn }
    var canManageSessions: Bool { isCloud && isSignedIn && cloud != nil }
    var isBusy: Bool { revokingScope != nil }

    var statusSubtitleKey: L10nKey? {
        guard let status else { return nil }
        if status.accountState == .signedOut && status.remoteSignoutPending {
            return .settingsStatusPendingSignOut
        }
        return nil
    }

    // MARK: - Load

    /// Loads typed status first so unavailable/offline sections render even when
    /// the Host or account listing fails independently.
    func load() async {
        await loadStatus()
        await loadHosts()
        await loadSessions()
    }

    func loadStatus() async {
        guard let statusService else {
            state = AccountReduce.reduce(state, .statusFailed(.unavailable))
            return
        }
        state = AccountReduce.reduce(state, .statusLoadStarted)
        do {
            let loaded = try await statusService.instanceStatus()
            state = AccountReduce.reduce(state, .statusLoaded(loaded))
        } catch {
            state = AccountReduce.reduce(state, .statusFailed(Self.code(for: error)))
        }
    }

    func loadHosts() async {
        guard let status else { return }
        guard status.accountState != .signedOut,
              status.deviceAccess != .signInRequired,
              status.deviceAccess != .unpaired else {
            hosts = []
            return
        }
        do {
            hosts = try await service.hosts()
            state = AccountReduce.reduce(state, .defaultHostSucceeded)
        } catch {
            hosts = []
            let code = Self.code(for: error)
            if code != .notPaired {
                state = AccountReduce.reduce(state, .defaultHostFailed(code))
            }
        }
    }

    func loadSessions() async {
        guard let cloud, let status else {
            state = AccountReduce.reduce(state, .sessionsLoaded([]))
            return
        }
        guard status.service == .cloud, status.accountState == .signedIn else {
            state = AccountReduce.reduce(state, .sessionsLoaded([]))
            return
        }
        state = AccountReduce.reduce(state, .sessionsLoadStarted)
        do {
            let current = try await statusService?.currentCloudSessionId()
            currentSessionId = current
            let listed = try await cloud.cloudAccountSessions()
            let sessions = listed.map { session in
                AccountAppSession(
                    sessionId: session.sessionId,
                    instanceId: session.instanceId,
                    expiresAt: session.expiresAt,
                    state: session.state,
                    isCurrent: session.sessionId == current
                )
            }
            state = AccountReduce.reduce(state, .sessionsLoaded(sessions))
        } catch {
            state = AccountReduce.reduce(state, .sessionsFailed(Self.code(for: error)))
        }
    }

    // MARK: - Default computer

    func selectDefaultHost(_ hostId: String) async {
        guard let statusService, !isBusy else { return }
        state = AccountReduce.reduce(state, .message(nil))
        state = AccountReduce.reduce(state, .defaultHostSelected(hostId))
        do {
            try await statusService.selectDefaultHost(hostId: hostId)
            state = AccountReduce.reduce(state, .defaultHostSelected(hostId))
            state = AccountReduce.reduce(state, .message(.settingsDefaultSaved))
            await loadStatus()
        } catch {
            state = AccountReduce.reduce(state, .defaultHostFailed(Self.code(for: error)))
        }
    }

    // MARK: - Cloud app sign-ins

    func revoke(session: AccountAppSession) async {
        guard let cloud, !isBusy else { return }
        await revoke(scope: .one(session.sessionId)) {
            try await cloud.cloudRevokeSession(id: session.sessionId)
        }
    }

    func revokeAllSessions() async {
        guard let cloud, !isBusy else { return }
        await revoke(scope: .all) {
            try await cloud.cloudRevokeAllSessions()
        }
    }

    private func revoke(
        scope: AccountSessionScope,
        operation: () async throws -> CloudAccountSessionState
    ) async {
        state = AccountReduce.reduce(state, .revokeStarted(scope))
        state = AccountReduce.reduce(state, .message(nil))
        do {
            let reportedState = try await operation()
            let affected: [String]
            switch scope {
            case .one(let id):
                affected = [id]
            case .all:
                affected = sessions.map(\.sessionId)
            }
            let result = AccountSessionRevocation(
                scope: scope,
                state: reportedState,
                affectedSessionIds: affected
            )
            state = AccountReduce.reduce(state, .revokeSettled(result))
            let affectsCurrent: Bool
            switch scope {
            case .one(let id):
                affectsCurrent = id == currentSessionId
            case .all:
                affectsCurrent = true
            }
            state = AccountReduce.reduce(
                state,
                .message(
                    reportedState == .pending
                        ? .settingsAppSessionsPending
                        : .settingsAppSessionsRevoked
                )
            )
            if affectsCurrent {
                currentSessionId = nil
                onSignedOut?()
            }
        } catch {
            state = AccountReduce.reduce(state, .revokeFailed(scope, Self.code(for: error)))
        }
    }

    private static func code(for error: Error) -> AppErrorCode {
        (error as? SessionServiceError)?.code ?? .internalError
    }
}
