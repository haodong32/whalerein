// Cloud sign-in and directory presentation model.
//
// The official hosted page is opened in `ASWebAuthenticationSession`; this
// model never renders credential fields. Core performs the authorization-code
// exchange and owns attempt state, so the model only drives presentation and
// fences callbacks against the AppModel activation that created it.

import Foundation
import Observation

/// The activation a Cloud model belongs to. A callback that arrives after an
/// instance switch no longer matches and is discarded.
struct CloudSignInFence: Equatable, Sendable {
    let instanceId: String
    let activationGeneration: UInt64
}

enum CloudSignInPhase: Equatable, Sendable {
    case signedOut
    case restoring
    case starting
    case authorizing
    case completing
    case signedIn(CloudAccount)

    var account: CloudAccount? {
        if case .signedIn(let account) = self { return account }
        return nil
    }

    var isSignedIn: Bool { account != nil }
}

/// Directory presentation states. `unavailable` is a failed read, never an
/// empty account.
enum CloudDirectoryState: Equatable, Sendable {
    case idle
    case loading
    case empty
    case loaded([CloudDirectoryHost])
    case unavailable(AppErrorCode)

    var hosts: [CloudDirectoryHost] {
        if case .loaded(let hosts) = self { return hosts }
        return []
    }
}

@MainActor
@Observable
final class CloudSignInModel {
    let fence: CloudSignInFence
    private let service: any CloudAccountService
    private let authenticator: any CloudWebAuthenticator
    /// AppModel supplies the live activation check. Tests can flip it to model
    /// a switch that happened while the system sheet was open.
    private let fenceIsCurrent: @MainActor () -> Bool

    /// Called only after Core confirmed the computer grant (or for an already
    /// authorized computer). It must never run for an eligible row alone.
    var onOpenHost: ((String) -> Void)?
    /// Called after a local sign-out or a confirmed remote revocation.
    var onSignedOut: (() -> Void)?

    private(set) var phase: CloudSignInPhase = .signedOut
    private(set) var directory: CloudDirectoryState = .idle
    private(set) var errorCode: AppErrorCode?
    private(set) var logoutPending = false
    private(set) var isAuthPresented = false
    private(set) var wasCancelled = false
    private(set) var isConnectingHostId: String?
    /// True while the system sheet owns the attempt. A completion that races a
    /// local cancellation is ignored rather than completing the old attempt.
    private var isAuthenticationActive = false

    init(
        service: any CloudAccountService,
        fence: CloudSignInFence,
        authenticator: (any CloudWebAuthenticator)? = nil,
        fenceIsCurrent: @escaping @MainActor () -> Bool = { true }
    ) {
        self.service = service
        self.fence = fence
        self.authenticator = authenticator ?? WebAuthenticationSessionRunner()
        self.fenceIsCurrent = fenceIsCurrent
    }

    var account: CloudAccount? { phase.account }
    var isBusy: Bool {
        switch phase {
        case .starting, .authorizing, .completing, .restoring:
            return true
        case .signedOut, .signedIn:
            return false
        }
    }

    private func stillCurrent() -> Bool {
        fenceIsCurrent()
    }

    // MARK: - Restore

    /// Restores only the locally retained session, then verifies it against
    /// Cloud. A revoked session leaves no signed-in state.
    func restore() async {
        guard phase == .signedOut else { return }
        phase = .restoring
        do {
            let status = try await service.cloudStatus()
            guard stillCurrent() else { return }
            logoutPending = status.logoutPending
            guard let account = status.account else {
                phase = .signedOut
                directory = .idle
                return
            }
            phase = .signedIn(account)
            await loadDirectory()
            await verifySession()
        } catch {
            guard stillCurrent() else { return }
            phase = .signedOut
            directory = .idle
            errorCode = Self.code(for: error)
        }
    }

    /// Explicit refresh of the directory. Never changes the signed-in account.
    func refreshDirectory() async {
        await loadDirectory()
    }

    /// Verifies the retained session against Cloud. Offline failures keep the
    /// local session; an authorization failure exits it.
    func verifySession() async {
        guard case .signedIn = phase else { return }
        do {
            let account = try await service.cloudRefreshStatus()
            guard stillCurrent() else { return }
            phase = .signedIn(account)
        } catch {
            let code = Self.code(for: error)
            guard code == .security || code == .revoked else { return }
            guard stillCurrent() else { return }
            logoutPending = true
            phase = .signedOut
            directory = .idle
            errorCode = .revoked
            onSignedOut?()
        }
    }

    // MARK: - Sign-in

    func signIn() async {
        guard !isBusy, phase == .signedOut else { return }
        errorCode = nil
        wasCancelled = false
        phase = .starting
        do {
            let attempt = try await service.cloudAuthStart()
            guard stillCurrent() else {
                await service.cloudAuthCancel()
                return
            }
            phase = .authorizing
            isAuthPresented = true
            isAuthenticationActive = true
            authenticator.authenticate(
                url: attempt.url,
                callbackURLScheme: CloudSignInModel.callbackScheme
            ) { [weak self] callbackURL, error in
                Task { @MainActor [weak self] in
                    await self?.authenticationDidFinish(callbackURL: callbackURL, error: error)
                }
            }
        } catch {
            guard stillCurrent() else { return }
            phase = .signedOut
            errorCode = Self.code(for: error)
        }
    }

    /// Cancels an in-flight system sheet and the Core attempt.
    func cancelSignIn() async {
        isAuthenticationActive = false
        authenticator.cancel()
        isAuthPresented = false
        await service.cloudAuthCancel()
        guard !phase.isSignedIn else { return }
        phase = .signedOut
        wasCancelled = true
        errorCode = nil
    }

    /// App-level deep-link entry. `ASWebAuthenticationSession` normally invokes
    /// the completion handler directly; this path exists for a callback that
    /// arrives through the app URL handler. A callback for an unrelated link or
    /// a replaced activation is ignored.
    func receiveCallback(_ url: URL) async {
        guard Self.isCloudCallback(url) else { return }
        guard stillCurrent() else { return }
        switch phase {
        case .authorizing, .completing, .starting:
            await complete(callbackURL: url)
        case .signedOut, .restoring, .signedIn:
            // A replayed or late callback can never disturb a current session.
            return
        }
    }

    private func authenticationDidFinish(callbackURL: URL?, error: Error?) async {
        guard isAuthenticationActive else { return }
        isAuthenticationActive = false
        isAuthPresented = false
        guard stillCurrent() else { return }
        if let error {
            await service.cloudAuthCancel()
            phase = .signedOut
            if cloudAuthenticationWasCancelled(error) {
                wasCancelled = true
                errorCode = nil
            } else {
                errorCode = .unavailable
            }
            return
        }
        guard let callbackURL, Self.isCloudCallback(callbackURL) else {
            await service.cloudAuthCancel()
            phase = .signedOut
            errorCode = .security
            return
        }
        await complete(callbackURL: callbackURL)
    }

    private func complete(callbackURL: URL) async {
        phase = .completing
        do {
            let account = try await service.cloudAuthComplete(callbackURL: callbackURL)
            guard stillCurrent() else { return }
            phase = .signedIn(account)
            logoutPending = false
            errorCode = nil
            await loadDirectory()
        } catch {
            guard stillCurrent() else { return }
            phase = .signedOut
            directory = .idle
            errorCode = Self.code(for: error)
        }
    }

    // MARK: - Directory

    func loadDirectory() async {
        guard case .signedIn = phase else { return }
        directory = .loading
        do {
            let hosts = try await service.cloudHosts()
            guard stillCurrent() else { return }
            directory = hosts.isEmpty ? .empty : .loaded(hosts)
        } catch {
            guard stillCurrent() else { return }
            directory = .unavailable(Self.code(for: error))
        }
    }

    /// Opens an authorized computer. An eligible or unavailable row never calls
    /// the open callback; connecting is a separate Core-confirmed action.
    func openHost(_ host: CloudDirectoryHost) {
        guard case .signedIn = phase, case .authorized = host.access else { return }
        onOpenHost?(host.hostId)
    }

    /// Connects an eligible computer through Core and only opens it after the
    /// directory confirms the grant.
    func connect(_ host: CloudDirectoryHost) async {
        guard case .signedIn = phase, host.access == .eligible else { return }
        guard isConnectingHostId == nil else { return }
        errorCode = nil
        isConnectingHostId = host.hostId
        do {
            _ = try await service.connectCloud(hostId: host.hostId)
            guard stillCurrent() else {
                isConnectingHostId = nil
                return
            }
            await loadDirectory()
            isConnectingHostId = nil
            guard case .loaded(let hosts) = directory,
                  hosts.contains(where: { $0.hostId == host.hostId && $0.enrolled }) else {
                return
            }
            onOpenHost?(host.hostId)
        } catch {
            isConnectingHostId = nil
            guard stillCurrent() else { return }
            errorCode = Self.code(for: error)
        }
    }

    // MARK: - Sign-out

    /// Local sign-out. `logoutPending` records that Cloud could not confirm the
    /// remote revocation; Core retries it on the next account request.
    func signOut() async {
        guard case .signedIn = phase else { return }
        do {
            let complete = try await service.cloudSignOut()
            guard stillCurrent() else { return }
            logoutPending = !complete
            phase = .signedOut
            directory = .idle
            errorCode = nil
            onSignedOut?()
        } catch {
            guard stillCurrent() else { return }
            errorCode = Self.code(for: error)
        }
    }

    // MARK: - Helpers

    static let callbackScheme = "whalerein"

    static func isCloudCallback(_ url: URL) -> Bool {
        url.scheme?.lowercased() == callbackScheme
            && url.host?.lowercased() == "auth"
            && url.path == "/callback"
    }

    private static func code(for error: Error) -> AppErrorCode {
        (error as? SessionServiceError)?.code ?? .internalError
    }
}
