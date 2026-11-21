// Live Cloud account adapter over the Rust Core binding.
//
// Every binding call runs off the main thread through `call`, so the account
// flow never blocks the UI while Cloud or the system sheet is in flight. The
// extension only maps typed views; Core owns attempt identity, state, PKCE
// material and credential storage.

import Foundation
import WhaleReinMobile

extension LiveSessionService: CloudAccountService {
    func cloudAuthStart() async throws -> CloudAuthAttempt {
        let core = self.core
        let view = try await call { try core.cloudAuthStart() }
        guard let url = URL(string: view.url), !view.attemptId.isEmpty else {
            throw SessionServiceError(.unavailable)
        }
        return CloudAuthAttempt(url: url, attemptId: view.attemptId)
    }

    func cloudAuthComplete(callbackURL: URL) async throws -> CloudAccount {
        let core = self.core
        let view = try await call {
            try core.cloudAuthComplete(callbackUrl: callbackURL.absoluteString)
        }
        return Self.map(account: view)
    }

    func cloudAuthCancel() async {
        let core = self.core
        _ = try? await call { try core.cloudAuthCancel() }
    }

    func cloudStatus() async throws -> CloudAccountStatus {
        let core = self.core
        let view = try await call { try core.cloudStatus() }
        return CloudAccountStatus(
            account: view.account.map(Self.map(account:)),
            logoutPending: view.logoutPending
        )
    }

    func cloudRefreshStatus() async throws -> CloudAccount {
        let core = self.core
        return Self.map(account: try await call { try core.cloudRefreshStatus() })
    }

    func cloudSignOut() async throws -> Bool {
        let core = self.core
        return try await call { try core.cloudSignOut() }
    }

    func cloudAccountSessions() async throws -> [CloudAccountSession] {
        let core = self.core
        let views = try await call { try core.cloudAccountSessions() }
        return views.map { view in
            CloudAccountSession(
                sessionId: view.sessionId,
                instanceId: view.instanceId,
                expiresAt: view.expiresAt,
                state: CloudAccountSessionState(rawValue: view.state) ?? .pending
            )
        }
    }

    func cloudRevokeSession(id: String) async throws -> CloudAccountSessionState {
        let core = self.core
        let state = try await call { try core.cloudRevokeSession(sessionId: id) }
        return CloudAccountSessionState(rawValue: state) ?? .pending
    }

    func cloudRevokeAllSessions() async throws -> CloudAccountSessionState {
        let core = self.core
        let state = try await call { try core.cloudRevokeAllSessions() }
        return CloudAccountSessionState(rawValue: state) ?? .pending
    }

    func cloudHosts() async throws -> [CloudDirectoryHost] {
        let core = self.core
        let views = try await call { try core.cloudHosts() }
        return views.map { view in
            CloudDirectoryHost(
                hostId: view.hostId,
                displayName: view.displayName,
                enrolled: view.enrolled,
                online: view.online,
                presence: view.presence,
                observation: view.observation,
                enrollmentState: view.enrollmentState,
                pairingEligible: view.pairingEligible,
                deviceAccess: view.deviceAccess
            )
        }
    }

    func connectCloud(hostId: String) async throws -> CloudConnectResult {
        let core = self.core
        let view = try await call { try core.connectCloud(hostId: hostId) }
        // Core returned only after the grant was confirmed and recovery state
        // was persisted. The access URL is deliberately not mapped.
        return CloudConnectResult(
            operationId: view.operationId,
            hostId: view.hostId,
            deviceId: view.deviceId,
            fingerprint: view.fingerprint
        )
    }

    private static func map(account view: CloudAccountView) -> CloudAccount {
        CloudAccount(
            accountId: view.accountId,
            email: view.email,
            issuer: view.issuer,
            subject: view.subject,
            workspaceId: view.workspaceId
        )
    }
}
