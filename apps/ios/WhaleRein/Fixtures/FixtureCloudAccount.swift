// Deterministic Cloud account fixtures.
//
// The fixture covers the directory states the UI must distinguish without any
// network: authorized-online, authorized-offline, eligible-to-connect and
// unavailable computers. It never produces credentials or a real browser.

import Foundation

@MainActor
final class FixtureCloudAccountState {
    private(set) var account: CloudAccount?
    private(set) var logoutPending = false
    var directory: [CloudDirectoryHost]
    var startError: AppErrorCode?
    var completeError: AppErrorCode?
    var statusError: AppErrorCode?
    var directoryError: AppErrorCode?

    private(set) var pendingAttempt: CloudAuthAttempt?
    private(set) var connectedHostIds: [String] = []

    private(set) var signInCount = 0
    private let switchAccounts: Bool

    init(options: LaunchOptions = LaunchOptions()) {
        switchAccounts = options.cloudSwitchAccounts
        account = options.seedCloudSignedIn ? FixtureCatalog.cloudAccount : nil
        directory = options.cloudDirectoryEmpty ? [] : FixtureCatalog.cloudHosts
        if options.cloudDirectoryUnavailable {
            directoryError = .unavailable
        }
        if options.cloudStartFailure {
            startError = .unavailable
        }
        if options.cloudCompleteFailure {
            completeError = .security
        }
        if options.cloudRevoked {
            statusError = .security
        }
    }

    func start() throws -> CloudAuthAttempt {
        if let startError { throw SessionServiceError(startError) }
        let base = ProcessInfo.processInfo.environment["UI_CLOUD_AUTH_BASE"]
            ?? "https://login.whalerein.test"
        let attempt = CloudAuthAttempt(
            url: URL(string: "\(base)/authorize?state=fixture-state")!,
            attemptId: "0123456789abcdef0123456789abcdef"
        )
        pendingAttempt = attempt
        return attempt
    }

    func complete(callbackURL: URL) throws -> CloudAccount {
        if let completeError { throw SessionServiceError(completeError) }
        guard let pendingAttempt else {
            throw SessionServiceError(.security)
        }
        let components = URLComponents(url: callbackURL, resolvingAgainstBaseURL: false)
        let state = components?.queryItems?.first(where: { $0.name == "state" })?.value
        guard callbackURL.scheme == "whalerein",
              callbackURL.host == "auth",
              callbackURL.path == "/callback",
              state == "fixture-state",
              components?.queryItems?.contains(where: { $0.name == "code" }) == true else {
            throw SessionServiceError(.security)
        }
        self.pendingAttempt = nil
        signInCount += 1
        let signedIn = switchAccounts && signInCount > 1
            ? FixtureCatalog.cloudSecondAccount
            : FixtureCatalog.cloudAccount
        account = signedIn
        logoutPending = false
        return signedIn
    }

    func cancel() {
        pendingAttempt = nil
    }

    func status() throws -> CloudAccountStatus {
        if let statusError { throw SessionServiceError(statusError) }
        return CloudAccountStatus(account: account, logoutPending: logoutPending)
    }

    func refresh() throws -> CloudAccount {
        guard let account else { throw SessionServiceError(.security) }
        return account
    }

    /// Local sign-out always clears the fixture account; remote confirmation is
    /// deliberately left pending so Settings can render that state.
    func signOut() -> Bool {
        endLocalSession(pending: true)
        return false
    }

    /// Ends the locally retained sign-in after a scoped app-session revocation.
    /// A pending account-wide result keeps Core's remote sign-out record.
    func endLocalSession(pending: Bool) {
        account = nil
        logoutPending = pending
    }

    func hosts() throws -> [CloudDirectoryHost] {
        if let directoryError { throw SessionServiceError(directoryError) }
        return directory
    }

    func connect(hostId: String) throws -> CloudConnectResult {
        guard let index = directory.firstIndex(where: {
            $0.hostId == hostId && $0.pairingEligible && !$0.enrolled
        }) else {
            throw SessionServiceError(.conflict)
        }
        let host = directory[index]
        directory[index] = CloudDirectoryHost(
            hostId: host.hostId,
            displayName: host.displayName,
            enrolled: true,
            online: true,
            presence: host.presence,
            observation: host.observation,
            enrollmentState: "enrolled",
            pairingEligible: false,
            deviceAccess: "granted"
        )
        connectedHostIds.append(hostId)
        return CloudConnectResult(
            operationId: "fixture-connect-\(hostId)",
            hostId: hostId,
            deviceId: "fixture-device",
            fingerprint: FixtureCatalog.fingerprint
        )
    }
}

@MainActor
extension FixtureSessionService: CloudAccountService {
    /// Stable identity of the app sign-in this fixture instance holds. Settings
    /// compares it with the listing to label the current row.
    static let fixtureCurrentSessionId = "sess-fixture-ios"

    func cloudAuthStart() async throws -> CloudAuthAttempt {
        try cloudState.start()
    }

    func cloudAuthComplete(callbackURL: URL) async throws -> CloudAccount {
        try cloudState.complete(callbackURL: callbackURL)
    }

    func cloudAuthCancel() async {
        cloudState.cancel()
    }

    func cloudStatus() async throws -> CloudAccountStatus {
        try cloudState.status()
    }

    func cloudRefreshStatus() async throws -> CloudAccount {
        try cloudState.refresh()
    }

    func cloudSignOut() async throws -> Bool {
        cloudState.signOut()
    }

    func cloudAccountSessions() async throws -> [CloudAccountSession] {
        guard cloudState.account != nil else { throw SessionServiceError(.security) }
        return [
            CloudAccountSession(
                sessionId: Self.fixtureCurrentSessionId,
                instanceId: "ios",
                expiresAt: 1_800_000_000,
                state: .active
            ),
            CloudAccountSession(
                sessionId: "sess-fixture-other",
                instanceId: "ipad",
                expiresAt: 1_800_000_600,
                state: .active
            ),
        ]
    }

    func cloudRevokeSession(id: String) async throws -> CloudAccountSessionState {
        guard cloudState.account != nil else { throw SessionServiceError(.security) }
        guard ["sess-fixture-ios", "sess-fixture-other"].contains(id) else {
            throw SessionServiceError(.unavailable)
        }
        if id == Self.fixtureCurrentSessionId {
            // Revoking the current app sign-in also ends the local session.
            cloudState.endLocalSession(pending: false)
        }
        return .revoked
    }

    func cloudRevokeAllSessions() async throws -> CloudAccountSessionState {
        guard cloudState.account != nil else { throw SessionServiceError(.security) }
        // Cloud may accept the account-wide revocation without confirming every
        // session; Settings must present the pending scope and sign out locally.
        cloudState.endLocalSession(pending: true)
        return .pending
    }

    func cloudHosts() async throws -> [CloudDirectoryHost] {
        try cloudState.hosts()
    }

    func connectCloud(hostId: String) async throws -> CloudConnectResult {
        try cloudState.connect(hostId: hostId)
    }
}
