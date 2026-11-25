// Cloud sign-in presentation: model transitions, cancellation, callback
// fencing, directory states, Core-confirmed connect and pending sign-out.
//
// The real system sheet cannot be automated in unit tests, so a fake
// authenticator drives the exact completion path the production runner uses.

import AuthenticationServices
import Foundation
import Testing
@testable import WhaleRein

@MainActor
private final class FakeAuthenticator: CloudWebAuthenticator {
    private(set) var presentedURL: URL?
    private(set) var callbackScheme: String?
    private(set) var lastEphemeral: Bool?
    private(set) var cancelCount = 0
    private var completion: (@MainActor (URL?, Error?) -> Void)?

    func authenticate(
        url: URL,
        callbackURLScheme: String,
        ephemeral: Bool,
        completion: @escaping @MainActor (URL?, Error?) -> Void
    ) {
        presentedURL = url
        callbackScheme = callbackURLScheme
        lastEphemeral = ephemeral
        self.completion = completion
    }

    func cancel() {
        cancelCount += 1
    }

    func complete(with url: URL?, error: Error? = nil) {
        let completion = self.completion
        self.completion = nil
        completion?(url, error)
    }
}

@MainActor
private final class StubCloudAccountService: CloudAccountService {
    let account = CloudAccount(
        accountId: "acct-cloud",
        email: "cloud@example.test",
        issuer: "https://login.whalerein.test/oidc",
        subject: "subject-cloud",
        workspaceId: "ws-cloud"
    )
    let attempt = CloudAuthAttempt(
        url: URL(string: "https://login.whalerein.test/oidc/auth?state=opaque")!,
        attemptId: "0123456789abcdef0123456789abcdef"
    )
    var status = CloudAccountStatus(account: nil, logoutPending: false)
    var hosts: [CloudDirectoryHost] = []
    var startError: AppErrorCode?
    var completeError: AppErrorCode?
    var refreshError: AppErrorCode?
    var hostsError: AppErrorCode?
    var signOutResult = true
    var connectError: AppErrorCode?
    var hostAfterConnect: CloudDirectoryHost?
    private(set) var startCount = 0
    private(set) var cancelCount = 0
    private(set) var completeCount = 0
    private(set) var completeURLs: [URL] = []
    private(set) var connectCount = 0
    private(set) var connectHostIds: [String] = []
    private(set) var signOutCount = 0

    func cloudAuthStart() async throws -> CloudAuthAttempt {
        startCount += 1
        if let startError { throw SessionServiceError(startError) }
        return attempt
    }

    func cloudAuthComplete(callbackURL: URL) async throws -> CloudAccount {
        completeCount += 1
        completeURLs.append(callbackURL)
        if let completeError { throw SessionServiceError(completeError) }
        return account
    }

    func cloudAuthCancel() async {
        cancelCount += 1
    }

    func cloudStatus() async throws -> CloudAccountStatus {
        status
    }

    func cloudRefreshStatus() async throws -> CloudAccount {
        if let refreshError { throw SessionServiceError(refreshError) }
        guard status.account != nil else { throw SessionServiceError(.security) }
        return status.account!
    }

    func cloudSignOut() async throws -> Bool {
        signOutCount += 1
        return signOutResult
    }

    func cloudAccountSessions() async throws -> [CloudAccountSession] { [] }

    func cloudRevokeSession(id: String) async throws -> CloudAccountSessionState { .revoked }

    func cloudRevokeAllSessions() async throws -> CloudAccountSessionState { .revoked }

    func cloudHosts() async throws -> [CloudDirectoryHost] {
        if let hostsError { throw SessionServiceError(hostsError) }
        return hosts
    }

    func connectCloud(hostId: String) async throws -> CloudConnectResult {
        connectCount += 1
        connectHostIds.append(hostId)
        if let connectError { throw SessionServiceError(connectError) }
        hosts = hosts.map { host in
            guard host.hostId == hostId else { return host }
            return hostAfterConnect ?? CloudDirectoryHost(
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
        }
        return CloudConnectResult(
            operationId: "op-1",
            hostId: hostId,
            deviceId: "device-1",
            fingerprint: FixtureCatalog.fingerprint
        )
    }
}

private func host(
    id: String,
    enrolled: Bool = true,
    online: Bool = true,
    eligible: Bool = false,
    enrollmentState: String? = nil,
    deviceAccess: String? = nil
) -> CloudDirectoryHost {
    CloudDirectoryHost(
        hostId: id,
        displayName: "Computer \(id)",
        enrolled: enrolled,
        online: online,
        presence: online ? "online" : "offline",
        observation: "",
        enrollmentState: enrollmentState ?? (enrolled ? "enrolled" : (eligible ? "eligible" : "unavailable")),
        pairingEligible: eligible,
        deviceAccess: deviceAccess ?? (enrolled ? "granted" : "none")
    )
}

private let callbackURL = URL(string: "whalerein://auth/callback?code=code-1&state=opaque")!

@Suite("Cloud sign-in")
@MainActor
struct CloudSignInModelTests {
    private func makeModel(
        service: StubCloudAccountService,
        authenticator: FakeAuthenticator? = nil,
        isCurrent: @escaping @MainActor () -> Bool = { true }
    ) -> CloudSignInModel {
        CloudSignInModel(
            service: service,
            fence: CloudSignInFence(instanceId: "cloud-production", activationGeneration: 3),
            authenticator: authenticator ?? FakeAuthenticator(),
            fenceIsCurrent: isCurrent
        )
    }

    @Test("disabled or ineligible computers cannot connect")
    func unavailableComputersHaveNoConnectAction() async {
        let service = StubCloudAccountService()
        service.status = CloudAccountStatus(account: service.account, logoutPending: false)
        service.hosts = [
            host(id: "disabled", enrollmentState: "enrollment_disabled", deviceAccess: "absent"),
            host(id: "disabled-grant", enrollmentState: "enrollment_disabled", deviceAccess: "ready"),
            host(id: "paused", enrollmentState: "enrollment_ready", deviceAccess: "pending"),
        ]
        let model = makeModel(service: service)
        await model.restore()

        #expect(model.directory.hosts.count == 3)
        for host in model.directory.hosts {
            #expect(host.access == .unavailable)
            await model.connect(host)
        }
        #expect(service.connectCount == 0)
    }

    @Test("sign-in opens the official URL in the system sheet with the Cloud callback scheme")
    func presentsOfficialSheet() async {
        let service = StubCloudAccountService()
        let authenticator = FakeAuthenticator()
        let model = makeModel(service: service, authenticator: authenticator)

        await model.signIn()

        #expect(service.startCount == 1)
        #expect(authenticator.presentedURL == service.attempt.url)
        #expect(authenticator.callbackScheme == "whalerein")
        #expect(authenticator.lastEphemeral == true)
        #expect(model.phase == .authorizing)
        #expect(model.isAuthPresented)
        #expect(model.errorCode == nil)
    }

    @Test("a sign-out followed by sign-in keeps the clean-sheet guarantee")
    func signOutThenSignInStaysClean() async {
        let service = StubCloudAccountService()
        let authenticator = FakeAuthenticator()
        let model = makeModel(service: service, authenticator: authenticator)

        await model.signIn()
        authenticator.complete(with: callbackURL)
        await waitUntil { model.phase.isSignedIn }
        await model.signOut()

        await model.signIn()
        #expect(authenticator.lastEphemeral == true)
    }

    @Test("the callback signs in and loads the Cloud directory")
    func completesSignIn() async {
        let service = StubCloudAccountService()
        service.hosts = [host(id: "studio-mac")]
        let authenticator = FakeAuthenticator()
        let model = makeModel(service: service, authenticator: authenticator)

        await model.signIn()
        authenticator.complete(with: callbackURL)
        await waitUntil { model.phase.isSignedIn }

        #expect(model.phase.account == service.account)
        #expect(service.completeCount == 1)
        #expect(service.completeURLs == [callbackURL])
        #expect(model.directory == .loaded(service.hosts))
        #expect(!model.isAuthPresented)
    }

    @Test("cancelling the system sheet returns to signed out without an error")
    func cancellation() async {
        let service = StubCloudAccountService()
        let authenticator = FakeAuthenticator()
        let model = makeModel(service: service, authenticator: authenticator)
        await model.signIn()

        let cancelled = NSError(
            domain: ASWebAuthenticationSessionErrorDomain,
            code: ASWebAuthenticationSessionError.canceledLogin.rawValue
        )
        authenticator.complete(with: nil, error: cancelled)
        await waitUntil { !model.isBusy }

        #expect(model.phase == .signedOut)
        #expect(model.wasCancelled)
        #expect(model.errorCode == nil)
        #expect(service.cancelCount == 1)
        #expect(service.completeCount == 0)
    }

    @Test("a callback for a replaced activation is fenced and never completes")
    func callbackFencing() async {
        let service = StubCloudAccountService()
        let authenticator = FakeAuthenticator()
        var current = true
        let model = makeModel(
            service: service,
            authenticator: authenticator,
            isCurrent: { current }
        )
        await model.signIn()
        current = false

        authenticator.complete(with: callbackURL)
        await pump()

        #expect(model.phase == .authorizing)
        #expect(!model.phase.isSignedIn)
        #expect(service.completeCount == 0)
        #expect(model.account == nil)
    }

    @Test("a callback for another instance or scheme is ignored")
    func unrelatedCallbackIgnored() async {
        let service = StubCloudAccountService()
        let authenticator = FakeAuthenticator()
        let model = makeModel(service: service, authenticator: authenticator)
        await model.signIn()

        await model.receiveCallback(URL(string: "whalerein://pair?token=secret")!)
        await model.receiveCallback(URL(string: "https://auth/callback?code=x&state=y")!)
        #expect(service.completeCount == 0)
        #expect(model.phase == .authorizing)
    }

    @Test("a state mismatch or failed exchange leaves the model signed out")
    func failedExchange() async {
        let service = StubCloudAccountService()
        service.completeError = .security
        let authenticator = FakeAuthenticator()
        let model = makeModel(service: service, authenticator: authenticator)
        await model.signIn()

        authenticator.complete(with: callbackURL)
        await waitUntil { !model.isBusy }

        #expect(model.phase == .signedOut)
        #expect(model.errorCode == .security)
        #expect(model.directory == .idle)
    }

    @Test("a cancelled Core attempt cannot be completed by a late callback")
    func cancelThenLateCallback() async {
        let service = StubCloudAccountService()
        let authenticator = FakeAuthenticator()
        let model = makeModel(service: service, authenticator: authenticator)
        await model.signIn()
        await model.cancelSignIn()

        authenticator.complete(with: callbackURL)
        await pump()

        #expect(model.phase == .signedOut)
        #expect(service.completeCount == 0)
        #expect(authenticator.cancelCount == 1)
        #expect(service.cancelCount == 1)
    }

    @Test("the app deep-link path completes an authorized attempt")
    func deepLinkCompletes() async {
        let service = StubCloudAccountService()
        let model = makeModel(service: service, authenticator: FakeAuthenticator())

        await model.signIn()
        await model.receiveCallback(callbackURL)

        #expect(model.phase.account == service.account)
        #expect(service.completeCount == 1)
        #expect(model.directory == .empty)
    }

    @Test("a replayed callback cannot disturb a signed-in session")
    func replayedCallbackIgnored() async {
        let service = StubCloudAccountService()
        service.hosts = [host(id: "studio-mac")]
        let authenticator = FakeAuthenticator()
        let model = makeModel(service: service, authenticator: authenticator)
        await model.signIn()
        authenticator.complete(with: callbackURL)
        await waitUntil { model.phase.isSignedIn }

        await model.receiveCallback(callbackURL)
        #expect(service.completeCount == 1)
        #expect(model.phase.account == service.account)
    }

    @Test("restore uses the local verified status and surfaces empty and unavailable directories")
    func restoreAndDirectoryStates() async {
        let service = StubCloudAccountService()
        service.status = CloudAccountStatus(account: service.account, logoutPending: false)
        service.hosts = []
        let model = makeModel(service: service)

        await model.restore()
        #expect(model.phase.account == service.account)
        #expect(model.directory == .empty)

        service.hostsError = .unavailable
        await model.refreshDirectory()
        #expect(model.directory == .unavailable(.unavailable))

        service.hostsError = nil
        service.hosts = [host(id: "studio-mac")]
        await model.refreshDirectory()
        #expect(model.directory == .loaded(service.hosts))
    }

    @Test("a remote revocation exits the restored session")
    func remoteRevocation() async {
        let service = StubCloudAccountService()
        service.status = CloudAccountStatus(account: service.account, logoutPending: false)
        service.refreshError = .security
        let model = makeModel(service: service)
        var signedOut = false
        model.onSignedOut = { signedOut = true }

        await model.restore()

        #expect(model.phase == .signedOut)
        #expect(model.errorCode == .revoked)
        #expect(model.logoutPending)
        #expect(signedOut)
    }

    @Test("an offline refresh keeps the local session")
    func offlineRefreshKeepsSession() async {
        let service = StubCloudAccountService()
        service.status = CloudAccountStatus(account: service.account, logoutPending: false)
        service.refreshError = .unavailable
        let model = makeModel(service: service)

        await model.restore()

        #expect(model.phase.account == service.account)
    }

    @Test("eligible rows never open; connect opens only after Core confirms the grant")
    func connectOnlyAfterGrant() async {
        let service = StubCloudAccountService()
        service.status = CloudAccountStatus(account: service.account, logoutPending: false)
        service.hosts = [host(id: "new-laptop", enrolled: false, eligible: true)]
        let model = makeModel(service: service)
        var opened: [String] = []
        model.onOpenHost = { opened.append($0) }
        await model.restore()

        let eligible = service.hosts[0]
        model.openHost(eligible)
        #expect(opened.isEmpty)

        await model.connect(eligible)
        #expect(service.connectCount == 1)
        #expect(service.connectHostIds == ["new-laptop"])
        #expect(opened == ["new-laptop"])
        #expect(model.directory.hosts.first?.enrolled == true)
    }

    @Test("a failed connect never opens the computer")
    func failedConnectNeverOpens() async {
        let service = StubCloudAccountService()
        service.status = CloudAccountStatus(account: service.account, logoutPending: false)
        service.hosts = [host(id: "new-laptop", enrolled: false, eligible: true)]
        service.connectError = .conflict
        let model = makeModel(service: service)
        var opened: [String] = []
        model.onOpenHost = { opened.append($0) }
        await model.restore()

        await model.connect(service.hosts[0])

        #expect(opened.isEmpty)
        #expect(model.errorCode == .conflict)
        #expect(model.isConnectingHostId == nil)
    }

    @Test("automatic open requires both enrollment and device authorization")
    func connectWithIncompleteDirectoryConfirmationDoesNotOpen() async {
        let eligible = host(id: "new-laptop", enrolled: false, eligible: true)
        let incompleteHosts = [
            host(id: eligible.hostId, enrolled: false, deviceAccess: "ready"),
            host(id: eligible.hostId, enrolled: true, deviceAccess: "pending"),
        ]
        for incomplete in incompleteHosts {
            let service = StubCloudAccountService()
            service.status = CloudAccountStatus(account: service.account, logoutPending: false)
            service.hosts = [eligible]
            service.hostAfterConnect = incomplete
            let model = makeModel(service: service)
            var opened: [String] = []
            model.onOpenHost = { opened.append($0) }
            await model.restore()

            await model.connect(eligible)

            #expect(service.connectHostIds == [eligible.hostId])
            #expect(model.directory.hosts == [incomplete])
            #expect(opened.isEmpty)
        }
    }

    @Test("local sign-out keeps a pending remote revocation")
    func signOutPending() async {
        let service = StubCloudAccountService()
        service.status = CloudAccountStatus(account: service.account, logoutPending: false)
        service.hosts = [host(id: "studio-mac")]
        service.signOutResult = false
        let model = makeModel(service: service)
        var signedOut = false
        model.onSignedOut = { signedOut = true }
        await model.restore()

        await model.signOut()

        #expect(model.phase == .signedOut)
        #expect(model.logoutPending)
        #expect(model.directory == .idle)
        #expect(signedOut)
        #expect(service.signOutCount == 1)
    }
}

@Suite("Cloud AppShellModel wiring")
@MainActor
struct CloudAppShellModelTests {
    private func cloudInstance(_ directory: URL) -> AppInstance {
        AppInstance(
            id: "cloud-production",
            service: .cloud,
            environment: .production,
            dataDirectory: directory,
            configPath: directory.appendingPathComponent("instance.toml"),
            hasStandaloneCA: false,
            standaloneCAPath: nil
        )
    }

    @Test("fixture launch restores the Cloud account and directory")
    func restoresCloudSession() async {
        let options = testLaunchOptions(["-ui-fixtures", "-ui-test-reset", "-ui-cloud-signed-in"])
        let directory = makeTempDirectory()
        let service = FixtureSessionService(
            options: options,
            draftStore: InMemoryDraftStore()
        )
        let app = AppShellModel(
            service: service,
            preferences: PreferencesStore(directory: directory),
            dataDirectory: directory,
            options: options,
            activeInstance: cloudInstance(directory)
        )

        #expect(app.cloudSignIn != nil)
        await app.launchRestore()
        await waitUntil { app.cloudSignIn?.phase.isSignedIn == true }

        #expect(app.cloudSignIn?.account?.email == FixtureCatalog.cloudAccount.email)
        #expect(app.cloudSignIn?.directory.hosts.count == FixtureCatalog.cloudHosts.count)
        #expect(!app.isPaired)
    }

    @Test("Cloud sign-out clears paired state and reports the pending remote revocation")
    func signOutClearsPairedState() async {
        let options = testLaunchOptions(["-ui-fixtures", "-ui-test-reset", "-ui-cloud-signed-in"])
        let directory = makeTempDirectory()
        let preferences = PreferencesStore(directory: directory)
        preferences.update {
            $0.paired = true
            $0.lastHostId = "studio-mac"
        }
        let service = FixtureSessionService(
            options: options,
            draftStore: InMemoryDraftStore(),
            initiallyPaired: true
        )
        let app = AppShellModel(
            service: service,
            preferences: preferences,
            dataDirectory: directory,
            options: options,
            activeInstance: cloudInstance(directory)
        )
        await app.launchRestore()
        await waitUntil { app.cloudSignIn?.phase.isSignedIn == true }

        await app.signOutCloud()

        #expect(app.cloudSignIn?.phase == .signedOut)
        #expect(app.cloudSignIn?.logoutPending == true)
        #expect(!app.isPaired)
        #expect(!preferences.value.paired)
        #expect(preferences.value.lastHostId == nil)
    }

    @Test("the Cloud callback is not offered to the invitation flow")
    func callbackNotTreatedAsInvitation() async {
        let options = testLaunchOptions(["-ui-fixtures", "-ui-test-reset", "-ui-cloud-signed-in"])
        let directory = makeTempDirectory()
        let app = AppShellModel(
            service: FixtureSessionService(options: options, draftStore: InMemoryDraftStore()),
            preferences: PreferencesStore(directory: directory),
            dataDirectory: directory,
            options: options,
            activeInstance: cloudInstance(directory)
        )
        await app.launchRestore()
        await waitUntil { app.cloudSignIn?.phase.isSignedIn == true }

        await app.handleOpenURL(URL(string: "whalerein://auth/callback?code=x&state=y")!)

        #expect(!app.showInvitationSheet)
        #expect(app.pairing.errorKey == nil)
        #expect(app.cloudSignIn?.phase.isSignedIn == true)
    }
}
