// Cloud account domain boundary.
//
// Presentation code only depends on this protocol. LiveSessionService owns the
// real Core exchange; FixtureSessionService supplies deterministic directory
// states. No raw credential, revocation token, PKCE verifier or Cloud state is
// represented here: Core keeps all of them inside its SecretStore.

import Foundation

/// The verified Cloud account and workspace bound to the active instance.
struct CloudAccount: Equatable, Sendable, Identifiable {
    let accountId: String
    let email: String
    let issuer: String
    let subject: String
    let workspaceId: String

    var id: String { accountId }
}

/// One pending authorization-code attempt. Only the URL the system sheet opens
/// and the opaque attempt identity cross the service boundary.
struct CloudAuthAttempt: Equatable, Sendable {
    let url: URL
    let attemptId: String
}

/// Local account state used for launch restore and Settings.
struct CloudAccountStatus: Equatable, Sendable {
    let account: CloudAccount?
    let logoutPending: Bool
}

/// One computer in the signed-in account's Cloud directory.
struct CloudDirectoryHost: Identifiable, Equatable, Sendable {
    let hostId: String
    let displayName: String
    let enrolled: Bool
    let online: Bool
    let presence: String
    let observation: String
    let enrollmentState: String
    let pairingEligible: Bool
    let deviceAccess: String

    var id: String { hostId }

    /// Presentation-only classification. The app still asks Core to confirm a
    /// grant before opening any session; `eligible` is never treated as a grant.
    var access: CloudHostAccess {
        if enrolled {
            return .authorized(online: online)
        }
        if pairingEligible {
            return .eligible
        }
        return .unavailable
    }
}

enum CloudHostAccess: Equatable, Sendable {
    case authorized(online: Bool)
    case eligible
    case unavailable
}

struct CloudAccountSession: Identifiable, Equatable, Sendable {
    let sessionId: String
    let instanceId: String
    let expiresAt: UInt64
    let state: CloudAccountSessionState

    var id: String { sessionId }
}

enum CloudAccountSessionState: String, Equatable, Sendable {
    case active
    case pending
    case revoked
}

/// Result of a successfully confirmed Cloud computer connection. Core persists
/// recovery state first; the access URL and device token stay inside Core.
struct CloudConnectResult: Equatable, Sendable {
    let operationId: String?
    let hostId: String
    let deviceId: String
    let fingerprint: String
}

/// Cloud account operations. Implemented by LiveSessionService (Core binding)
/// and FixtureSessionService (deterministic unit/UI fixtures).
@MainActor
protocol CloudAccountService: AnyObject {
    func cloudAuthStart() async throws -> CloudAuthAttempt
    func cloudAuthComplete(callbackURL: URL) async throws -> CloudAccount
    func cloudAuthCancel() async

    func cloudStatus() async throws -> CloudAccountStatus
    func cloudRefreshStatus() async throws -> CloudAccount
    func cloudSignOut() async throws -> Bool

    func cloudAccountSessions() async throws -> [CloudAccountSession]
    func cloudRevokeSession(id: String) async throws -> CloudAccountSessionState
    func cloudRevokeAllSessions() async throws -> CloudAccountSessionState

    func cloudHosts() async throws -> [CloudDirectoryHost]
    func connectCloud(hostId: String) async throws -> CloudConnectResult
}
