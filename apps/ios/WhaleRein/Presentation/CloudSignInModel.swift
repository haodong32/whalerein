// Cloud sign-in and directory presentation model.
//
// The official hosted page is opened in `ASWebAuthenticationSession`; this
// model never renders credential fields. Core performs the authorization-code
// exchange and owns attempt state, so the model only drives presentation and
// fences callbacks against the AppShellModel activation that created it.

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
    /// AppShellModel supplies the live activation check. Tests can flip it to model
    /// a switch that happened while the system sheet was open.
    private let fenceIsCurrent: @MainActor () -> Bool

    /// Called only after Core confirmed the computer grant (or for an already
    /// authorized computer). It must never run for an eligible row alone.
    var onOpenHost: ((String) -> Void)?
    /// Called after a local sign-out or a confirmed remote revocation.
    var onSignedOut: (() -> Void)?

    /// One business state. The legacy names below are read-only projections.
    private(set) var state = CloudState()

    var phase: CloudSignInPhase { state.phase }
    var directory: CloudDirectoryState { state.directory }
    var errorCode: AppErrorCode? { state.errorCode }
    var logoutPending: Bool { state.logoutPending }
    var isAuthPresented: Bool { state.authPresented }
    var wasCancelled: Bool { state.wasCancelled }

    private func apply(_ event: CloudEvent) {
        state = CloudReduce.reduce(state, event)
    }
    /// Set by an explicit local sign-out so the next primary sign-in opens a
    /// clean provider sheet instead of resuming the previous account.

    var isConnectingHostId: String? { state.connectingHostId }
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
        // A retry after an unavailable Keychain must clear the previous failure;
        // otherwise a successfully restored account would still render the old
        // error guidance.
        state.errorCode = nil
        apply(.restoreStarted)
        do {
            let status = try await service.cloudStatus()
            guard stillCurrent() else { return }
            state.errorCode = nil
            apply(.restored(account: status.account, logoutPending: status.logoutPending))
            guard status.account != nil else { return }
            await loadDirectory()
            await verifySession()
        } catch {
            guard stillCurrent() else { return }
            apply(.restoreFailed(Self.code(for: error)))
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
            apply(.sessionVerified(account))
        } catch {
            let code = Self.code(for: error)
            guard code == .security || code == .revoked else { return }
            guard stillCurrent() else { return }
            apply(.sessionRevoked)
            onSignedOut?()
        }
    }

    // MARK: - Sign-in

    /// Every sign-in starts from a clean system sheet. There is one signed-out
    /// state, one button, and no silent reuse of the provider's previous
    /// account.
    func signIn() async {
        guard !isBusy, phase == .signedOut else { return }
        apply(.signInStarted)
        do {
            let attempt = try await service.cloudAuthStart()
            guard stillCurrent() else {
                await service.cloudAuthCancel()
                return
            }
            apply(.authorizing)
            isAuthenticationActive = true
            authenticator.authenticate(
                url: attempt.url,
                callbackURLScheme: CloudSignInModel.callbackScheme,
                ephemeral: true
            ) { [weak self] callbackURL, error in
                Task { @MainActor [weak self] in
                    await self?.authenticationDidFinish(callbackURL: callbackURL, error: error)
                }
            }
        } catch {
            guard stillCurrent() else { return }
            apply(.signInFailed(Self.code(for: error)))
        }
    }

    /// Cancels an in-flight system sheet and the Core attempt.
    func cancelSignIn() async {
        isAuthenticationActive = false
        authenticator.cancel()
        apply(.authSheetDismissed)
        await service.cloudAuthCancel()
        guard !phase.isSignedIn else { return }
        apply(.signInCancelled)
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
        apply(.authSheetDismissed)
        guard stillCurrent() else { return }
        if let error {
            await service.cloudAuthCancel()
            if cloudAuthenticationWasCancelled(error) {
                apply(.signInCancelled)
            } else {
                apply(.signInFailed(.unavailable))
            }
            return
        }
        guard let callbackURL, Self.isCloudCallback(callbackURL) else {
            await service.cloudAuthCancel()
            apply(.signInFailed(.security))
            return
        }
        await complete(callbackURL: callbackURL)
    }

    private func complete(callbackURL: URL) async {
        apply(.completionStarted)
        do {
            let account = try await service.cloudAuthComplete(callbackURL: callbackURL)
            guard stillCurrent() else { return }
            apply(.completed(account))
            await loadDirectory()
        } catch {
            guard stillCurrent() else { return }
            apply(.completionFailed(Self.code(for: error)))
        }
    }

    // MARK: - Directory

    func loadDirectory() async {
        guard case .signedIn = phase else { return }
        apply(.directoryStarted)
        do {
            let hosts = try await service.cloudHosts()
            guard stillCurrent() else { return }
            apply(.directoryLoaded(hosts))
        } catch {
            guard stillCurrent() else { return }
            apply(.directoryFailed(Self.code(for: error)))
        }
    }

    /// Opens an authorized computer. An eligible or unavailable row never calls
    /// the open callback; connecting is a separate Core-confirmed action.
    func openHost(_ host: CloudDirectoryHost) {
        guard case .signedIn = phase, case .authorized = host.access else { return }
        onOpenHost?(host.hostId)
    }

    /// Connects an eligible computer through Core and only opens it after the
    /// directory confirms enrollment and the device grant.
    func connect(_ host: CloudDirectoryHost) async {
        guard case .signedIn = phase, host.access == .eligible else { return }
        guard isConnectingHostId == nil else { return }
        apply(.connectStarted(host.hostId))
        do {
            _ = try await service.connectCloud(hostId: host.hostId)
            guard stillCurrent() else {
                apply(.connectFinished)
                return
            }
            await loadDirectory()
            apply(.connectFinished)
            guard let connected = directory.hosts.first(where: { $0.hostId == host.hostId }),
                  connected.enrolled,
                  case .authorized = connected.access else {
                return
            }
            onOpenHost?(host.hostId)
        } catch {
            apply(.connectFailed(Self.code(for: error)))
            guard stillCurrent() else { return }
        }
    }

    // MARK: - Sign-out

    /// Marks the locally retained account signed out after a scoped app-session
    /// revocation. Core already owns the pending remote sign-out record, so this
    /// only clears presentation; it never issues a second revocation.
    func markSignedOutLocally(pending: Bool) {
        apply(.signedOutLocally(pending: pending))
    }

    /// Local sign-out. `logoutPending` records that Cloud could not confirm the
    /// remote revocation; Core retries it on the next account request.
    func signOut() async {
        guard case .signedIn = phase else { return }
        do {
            let complete = try await service.cloudSignOut()
            guard stillCurrent() else { return }
            apply(.signedOut(pending: !complete))
            onSignedOut?()
        } catch {
            guard stillCurrent() else { return }
            apply(.errorRaised(Self.code(for: error)))
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
