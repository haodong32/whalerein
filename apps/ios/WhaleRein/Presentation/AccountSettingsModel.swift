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

    private(set) var status: InstanceStatus?
    private(set) var statusState: LoadState = .idle
    private(set) var statusError: AppErrorCode?

    private(set) var hosts: [HostSummary] = []
    private(set) var defaultHostId: String?
    private(set) var defaultHostError: AppErrorCode?

    private(set) var sessions: [AccountAppSession] = []
    private(set) var sessionsState: LoadState = .idle
    private(set) var sessionsError: AppErrorCode?

    private(set) var revokingScope: AccountSessionScope?
    private(set) var lastRevocation: AccountSessionRevocation?
    private(set) var statusKey: L10nKey?

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
            statusState = .failed(.unavailable)
            statusError = .unavailable
            return
        }
        if statusState == .idle { statusState = .loading }
        do {
            let loaded = try await statusService.instanceStatus()
            status = loaded
            defaultHostId = loaded.defaultHost
            statusError = nil
            statusState = .loaded
        } catch {
            let code = Self.code(for: error)
            statusState = .failed(code)
            statusError = code
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
            defaultHostError = nil
        } catch {
            hosts = []
            let code = Self.code(for: error)
            if code != .notPaired {
                defaultHostError = code
            }
        }
    }

    func loadSessions() async {
        guard let cloud, let status else {
            sessions = []
            sessionsState = .loaded
            return
        }
        guard status.service == .cloud, status.accountState == .signedIn else {
            sessions = []
            sessionsState = .loaded
            sessionsError = nil
            return
        }
        sessionsState = .loading
        do {
            let current = try await statusService?.currentCloudSessionId()
            currentSessionId = current
            let listed = try await cloud.cloudAccountSessions()
            sessions = listed.map { session in
                AccountAppSession(
                    sessionId: session.sessionId,
                    instanceId: session.instanceId,
                    expiresAt: session.expiresAt,
                    state: session.state,
                    isCurrent: session.sessionId == current
                )
            }
            sessionsError = nil
            sessionsState = .loaded
        } catch {
            let code = Self.code(for: error)
            sessionsError = code
            sessionsState = .failed(code)
        }
    }

    // MARK: - Default computer

    func selectDefaultHost(_ hostId: String) async {
        guard let statusService, !isBusy else { return }
        statusKey = nil
        defaultHostError = nil
        do {
            try await statusService.selectDefaultHost(hostId: hostId)
            defaultHostId = hostId
            statusKey = .settingsDefaultSaved
            await loadStatus()
        } catch {
            defaultHostError = Self.code(for: error)
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
        revokingScope = scope
        statusKey = nil
        sessionsError = nil
        do {
            let state = try await operation()
            let affected: [String]
            switch scope {
            case .one(let id):
                affected = [id]
                if let index = sessions.firstIndex(where: { $0.sessionId == id }) {
                    sessions[index].state = state
                }
            case .all:
                affected = sessions.map(\.sessionId)
                for index in sessions.indices {
                    sessions[index].state = state
                }
            }
            lastRevocation = AccountSessionRevocation(
                scope: scope,
                state: state,
                affectedSessionIds: affected
            )
            let affectsCurrent: Bool
            switch scope {
            case .one(let id):
                affectsCurrent = id == currentSessionId
            case .all:
                affectsCurrent = true
            }
            statusKey = state == .pending
                ? .settingsAppSessionsPending
                : .settingsAppSessionsRevoked
            revokingScope = nil
            if affectsCurrent {
                currentSessionId = nil
                onSignedOut?()
            }
        } catch {
            revokingScope = nil
            sessionsError = Self.code(for: error)
        }
    }

    private static func code(for error: Error) -> AppErrorCode {
        (error as? SessionServiceError)?.code ?? .internalError
    }
}
