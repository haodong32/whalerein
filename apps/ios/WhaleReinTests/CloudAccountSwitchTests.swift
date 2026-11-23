// Cloud account switching after an offline sign-out.
//
// The user signs out of account A while Cloud is unreachable, relaunches, and
// later signs in as B. A's directory, retained operations, cached sessions and
// drafts must stay in A's instance scope, and a delayed revocation of A must not
// touch the active B session.
//
// Two services with their own directory/preferences model the two accounts; the
// fake authenticator drives the exact completion path the production runner
// uses, so no system sheet or live Cloud is required.

import AuthenticationServices
import Foundation
import Testing
@testable import WhaleRein

// MARK: - Scoped account service

/// A SessionService + CloudAccountService whose state belongs to one account.
/// Session, host and draft state is real per instance, so the assertions are
/// about scope identity rather than a canned success.
@MainActor
private final class ScopedCloudService: StubSessionService, CloudAccountService, OperationRecoveryService {
    var account: CloudAccount?
    var directoryHosts: [CloudDirectoryHost]
    var accountSessions: [CloudAccountSession] = []
    var retained: [OperationReceipt] = []

    var startError: AppErrorCode?
    var signOutResult = true
    var refreshError: AppErrorCode?
    var hostsError: AppErrorCode?

    private(set) var signOutCount = 0
    private(set) var startCount = 0
    private(set) var completeCount = 0
    private(set) var cancelCount = 0

    let attempt: CloudAuthAttempt
    let issuedAccount: CloudAccount

    init(
        instanceId: String,
        account: CloudAccount?,
        directoryHosts: [CloudDirectoryHost],
        issuedAccount: CloudAccount,
        hosts: [HostSummary],
        sessions: [SessionSummary]
    ) {
        self.account = account
        self.directoryHosts = directoryHosts
        self.issuedAccount = issuedAccount
        self.attempt = CloudAuthAttempt(
            url: URL(string: "https://login.whalerein.test/oidc/auth?state=\(instanceId)")!,
            attemptId: String(instanceId.unicodeScalars.map { String($0.value, radix: 16) }.joined().prefix(32))
        )
        super.init()
        self.instanceId = instanceId
        self.hostsHandler = { hosts }
        self.sessionsHandler = { _ in sessions }
        self.cachedSessionsHandler = { _ in sessions }
    }

    func cloudAuthStart() async throws -> CloudAuthAttempt {
        startCount += 1
        if let startError { throw SessionServiceError(startError) }
        return attempt
    }

    func cloudAuthComplete(callbackURL: URL) async throws -> CloudAccount {
        completeCount += 1
        account = issuedAccount
        return issuedAccount
    }

    func cloudAuthCancel() async {
        cancelCount += 1
    }

    func cloudStatus() async throws -> CloudAccountStatus {
        CloudAccountStatus(account: account, logoutPending: logoutPending)
    }

    private(set) var logoutPending = false

    func cloudRefreshStatus() async throws -> CloudAccount {
        if let refreshError { throw SessionServiceError(refreshError) }
        guard let account else { throw SessionServiceError(.security) }
        return account
    }

    func cloudSignOut() async throws -> Bool {
        signOutCount += 1
        // Core clears the local session first; an unreachable Cloud leaves a
        // pending remote revocation instead of failing the local sign-out.
        account = nil
        logoutPending = !signOutResult
        return signOutResult
    }

    func cloudAccountSessions() async throws -> [CloudAccountSession] { accountSessions }

    func cloudRevokeSession(id: String) async throws -> CloudAccountSessionState {
        accountSessions.removeAll { $0.sessionId == id }
        return .revoked
    }

    func cloudRevokeAllSessions() async throws -> CloudAccountSessionState {
        accountSessions.removeAll()
        return .revoked
    }

    func cloudHosts() async throws -> [CloudDirectoryHost] {
        if let hostsError { throw SessionServiceError(hostsError) }
        return directoryHosts
    }

    func connectCloud(hostId: String) async throws -> CloudConnectResult {
        guard let index = directoryHosts.firstIndex(where: { $0.hostId == hostId }) else {
            throw SessionServiceError(.conflict)
        }
        let host = directoryHosts[index]
        directoryHosts[index] = CloudDirectoryHost(
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
        return CloudConnectResult(
            operationId: "op-\(hostId)",
            hostId: hostId,
            deviceId: "device-1",
            fingerprint: FixtureCatalog.fingerprint
        )
    }

    // MARK: OperationRecoveryService

    func listRetainedOperations() async throws -> [OperationReceipt] { retained }

    func recoverSend(operationId: String, follow: Bool) async throws -> OperationReceipt {
        guard let receipt = retained.first(where: { $0.operationId == operationId }) else {
            throw SessionServiceError(.unavailable)
        }
        return receipt
    }

    func readMessage(_ reference: MessageReference) async throws -> ConversationMessage {
        throw SessionServiceError(.unavailable)
    }
}

// MARK: - Fixtures

private final class SwitchingAuthenticator: CloudWebAuthenticator {
    private(set) var presentedURL: URL?
    private var completion: (@MainActor (URL?, Error?) -> Void)?

    func authenticate(
        url: URL,
        callbackURLScheme: String,
        ephemeral: Bool,
        completion: @escaping @MainActor (URL?, Error?) -> Void
    ) {
        presentedURL = url
        self.completion = completion
    }

    func cancel() {}

    func complete(with url: URL) {
        completion?(url, nil)
        completion = nil
    }
}

private let cloudCallback = URL(string: "whalerein://auth/callback?code=code-1&state=state-1")!

private func account(_ id: String, email: String) -> CloudAccount {
    CloudAccount(
        accountId: id,
        email: email,
        issuer: "https://login.whalerein.test/oidc",
        subject: "subject-\(id)",
        workspaceId: "ws-\(id)"
    )
}

private func directoryHost(_ id: String, displayName: String) -> CloudDirectoryHost {
    CloudDirectoryHost(
        hostId: id,
        displayName: displayName,
        enrolled: true,
        online: true,
        presence: "online",
        observation: "active",
        enrollmentState: "enrolled",
        pairingEligible: false,
        deviceAccess: "granted"
    )
}

private func hostSummary(_ id: String) -> HostSummary {
    HostSummary(
        hostId: id,
        online: true,
        fingerprint: FixtureCatalog.fingerprint,
        protocolVersion: "1"
    )
}

private func sessionSummary(_ id: String, title: String) -> SessionSummary {
    SessionSummary(
        sessionId: id,
        title: title,
        updatedAtUnixMs: 1_700_000_000_000,
        cwd: "/work/\(id)"
    )
}

private func cloudInstance(
    id: String,
    environment: AppCloudEnvironment,
    directory: URL
) -> AppInstance {
    AppInstance(
        id: id,
        service: .cloud,
        environment: environment,
        dataDirectory: directory.appendingPathComponent("data", isDirectory: true),
        configPath: directory.appendingPathComponent("instance.toml"),
        hasStandaloneCA: false,
        standaloneCAPath: nil
    )
}

private func message(_ id: String, text: String) -> ConversationMessage {
    ConversationMessage(
        messageId: id,
        role: .assistant,
        blocks: [
            ConversationBlock(blockId: "block-\(id)", kind: .text, text: text)
        ],
        createdAtUnixMs: 1_700_000_000_000
    )
}

private func operation(
    id: String,
    instanceId: String,
    hostId: String,
    sessionId: String,
    status: OperationStatus
) -> OperationReceipt {
    OperationReceipt(
        operationId: id,
        kind: .send,
        scope: OperationScope(instanceId: instanceId, hostId: hostId, sessionId: sessionId),
        status: status,
        receipt: nil,
        reply: nil,
        content: nil,
        readError: nil
    )
}

// MARK: - Tests

@Suite("Cloud account switching")
@MainActor
struct CloudAccountSwitchTests {
    @Test("offline sign-out clears only A's local account and keeps a pending revocation")
    func offlineSignOutKeepsPendingRevocation() async {
        let directory = makeTempDirectory()
        let accountA = account("acct-a", email: "a@whalerein.test")
        let serviceA = ScopedCloudService(
            instanceId: "cloud-production",
            account: accountA,
            directoryHosts: [directoryHost("studio-a", displayName: "A Studio")],
            issuedAccount: accountA,
            hosts: [hostSummary("studio-a")],
            sessions: [sessionSummary("s-001", title: "A session")]
        )
        serviceA.signOutResult = false // Cloud unreachable.
        serviceA.retained = [
            operation(id: "op-a", instanceId: "cloud-production", hostId: "studio-a", sessionId: "s-001", status: .uncertain)
        ]

        let preferencesA = PreferencesStore(directory: directory)
        preferencesA.update {
            $0.paired = true
            $0.lastHostId = "studio-a"
            $0.lastSessionId = "s-001"
            $0.lastSessionTitle = "A session"
        }
        let options = testLaunchOptions(["-ui-fixtures", "-ui-test-reset"])
        let app = AppShellModel(
            service: serviceA,
            preferences: preferencesA,
            dataDirectory: directory,
            options: options,
            activeInstance: cloudInstance(id: "cloud-production", environment: .production, directory: directory)
        )
        await app.launchRestore()
        await waitUntil { app.conversation != nil }
        #expect(app.cloudSignIn?.account == accountA)
        #expect(app.conversation?.sessionId == "s-001")
        await waitUntil { app.conversation?.loadState == .loaded }

        app.conversation?.draft = "draft only account A may see"
        await app.conversation?.suspendForBackground()

        await app.signOutCloud()

        #expect(app.cloudSignIn?.phase == .signedOut)
        #expect(app.cloudSignIn?.logoutPending == true)
        #expect(serviceA.signOutCount == 1)
        #expect(!app.isPaired)
        #expect(!preferencesA.value.paired)
        #expect(preferencesA.value.lastHostId == nil)
        #expect(preferencesA.value.lastSessionId == nil)
        #expect(app.conversation == nil)
        // The pending work stays retained in A's service; it is not deleted.
        #expect(serviceA.savedDrafts.values.contains("draft only account A may see"))
        #expect(serviceA.retained.count == 1)
    }

    @Test("signing in as B after A's offline sign-out shows B's account and no A state")
    func accountBSeesOnlyItsOwnState() async throws {
        let directory = makeTempDirectory()
        let accountA = account("acct-a", email: "a@whalerein.test")
        let accountB = account("acct-b", email: "b@whalerein.test")

        // Both accounts use the same session ID on purpose: identity must
        // include the account/Host scope, not just the session row.
        let serviceA = ScopedCloudService(
            instanceId: "cloud-production",
            account: accountA,
            directoryHosts: [directoryHost("studio-a", displayName: "A Studio")],
            issuedAccount: accountA,
            hosts: [hostSummary("studio-a")],
            sessions: [sessionSummary("s-001", title: "A session")]
        )
        serviceA.retained = [
            operation(id: "op-a", instanceId: "cloud-production", hostId: "studio-a", sessionId: "s-001", status: .uncertain)
        ]
        serviceA.signOutResult = false // Offline sign-out leaves a pending revocation.
        serviceA.emit(
            .reset(messages: [message("a-1", text: "A private history")], approvals: []),
            hostId: "studio-a",
            sessionId: "s-001",
            sequence: 1
        )

        let serviceB = ScopedCloudService(
            instanceId: "cloud-staging",
            account: nil,
            directoryHosts: [directoryHost("studio-b", displayName: "B Studio")],
            issuedAccount: accountB,
            hosts: [hostSummary("studio-b")],
            sessions: [sessionSummary("s-001", title: "B session")]
        )
        serviceB.retained = [
            operation(id: "op-b", instanceId: "cloud-staging", hostId: "studio-b", sessionId: "s-001", status: .confirmed)
        ]
        // A's draft lives in A's own service scope.
        try? await serviceA.saveDraft(hostId: "studio-a", sessionId: "s-001", text: "A private draft")

        // A signs out offline and relaunches signed out.
        let modelA = CloudSignInModel(
            service: serviceA,
            fence: CloudSignInFence(instanceId: "cloud-production", activationGeneration: 1),
            authenticator: SwitchingAuthenticator()
        )
        await modelA.restore()
        #expect(modelA.phase.isSignedIn)
        await modelA.signOut()
        #expect(modelA.phase == .signedOut)
        #expect(modelA.logoutPending)

        // B signs in through the sheet boundary after connectivity returns.
        let authenticatorB = SwitchingAuthenticator()
        let modelB = CloudSignInModel(
            service: serviceB,
            fence: CloudSignInFence(instanceId: "cloud-staging", activationGeneration: 2),
            authenticator: authenticatorB
        )
        await modelB.signIn()
        authenticatorB.complete(with: cloudCallback)
        await waitUntil { modelB.phase.isSignedIn }

        #expect(modelB.account == accountB)
        #expect(modelB.directory.hosts.map(\.hostId) == ["studio-b"])
        #expect(!modelB.directory.hosts.contains { $0.hostId == "studio-a" })
        // A's directory and pending revocation are not shown in B's session.
        #expect(modelA.account == nil)
        #expect(modelA.directory == .idle)
        #expect(modelA.logoutPending)
        #expect(serviceB.completeCount == 1)

        // B's shell state is built from B's service and preferences only.
        let directoryB = makeTempDirectory()
        let preferencesB = PreferencesStore(directory: directoryB)
        preferencesB.update {
            $0.paired = true
            $0.lastHostId = "studio-b"
            $0.lastSessionId = "s-001"
            $0.lastSessionTitle = "B session"
        }
        serviceB.account = accountB
        let appB = AppShellModel(
            service: serviceB,
            preferences: preferencesB,
            dataDirectory: directoryB,
            options: testLaunchOptions(["-ui-fixtures", "-ui-test-reset"]),
            activeInstance: cloudInstance(id: "cloud-staging", environment: .staging, directory: directoryB)
        )
        await appB.launchRestore()
        await waitUntil { appB.conversation?.loadState == .loaded }
        serviceB.emit(
            .reset(messages: [message("b-1", text: "B public history")], approvals: []),
            hostId: "studio-b",
            sessionId: "s-001",
            sequence: 1
        )
        await pump(50)

        #expect(appB.conversation?.hostId == "studio-b")
        #expect(appB.conversation?.sessionId == "s-001")
        #expect(appB.hostList.hosts.map(\.hostId) == ["studio-b"])
        #expect(appB.sessionList.sessions.map(\.sessionId) == ["s-001"])
        #expect(appB.conversation?.messages.map(\.messageId) == ["b-1"])
        #expect(appB.conversation?.draft.isEmpty == true)

        // Operation recovery is scoped to B's instance/Host/session.
        await appB.operationRecovery?.refreshVisibleOperations(
            in: OperationScope(instanceId: "cloud-staging", hostId: "studio-b", sessionId: "s-001")
        )
        let bOperations = appB.operationRecovery?.operations(for:
            OperationScope(instanceId: "cloud-staging", hostId: "studio-b", sessionId: "s-001")
        ) ?? []
        #expect(bOperations.map(\.operationId) == ["op-b"])
        #expect(!bOperations.contains { $0.operationId == "op-a" })
        // A's draft cannot appear under the same session ID in B's scope.
        #expect(try await serviceB.loadDraft(hostId: "studio-b", sessionId: "s-001") == nil)
        #expect(try await serviceA.loadDraft(hostId: "studio-a", sessionId: "s-001") == "A private draft")
    }

    @Test("a delayed revocation of A is fenced and leaves the active B session untouched")
    func delayedRevocationOfALeavesBActive() async throws {
        let root = makeTempDirectory()
        let document = AppStorageLocations.appConfigurationURL(in: root)
        let store = try AppConfigurationStore(documentPath: document.path)
        try store.save(AppInstanceInput(
            id: "cloud-staging",
            service: .cloud,
            environment: .staging,
            dataDirectory: "instances/cloud-staging/data",
            standaloneCAPEM: nil
        ))
        try store.activate(id: "cloud-production")

        let accountA = account("acct-a", email: "a@whalerein.test")
        let accountB = account("acct-b", email: "b@whalerein.test")
        let serviceA = ScopedCloudService(
            instanceId: "cloud-production",
            account: accountA,
            directoryHosts: [directoryHost("studio-a", displayName: "A Studio")],
            issuedAccount: accountA,
            hosts: [hostSummary("studio-a")],
            sessions: [sessionSummary("s-001", title: "A session")]
        )
        let serviceB = ScopedCloudService(
            instanceId: "cloud-staging",
            account: accountB,
            directoryHosts: [directoryHost("studio-b", displayName: "B Studio")],
            issuedAccount: accountB,
            hosts: [hostSummary("studio-b")],
            sessions: [sessionSummary("s-001", title: "B session")]
        )

        func preferences(for id: String) -> PreferencesStore {
            let directory = AppStorageLocations.instanceDirectory(in: root, instanceId: id)
            try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            return PreferencesStore(directory: directory)
        }
        let preferencesA = preferences(for: "cloud-production")
        preferencesA.update {
            $0.paired = true
            $0.lastHostId = "studio-a"
            $0.lastSessionId = "s-001"
        }
        let preferencesB = preferences(for: "cloud-staging")
        preferencesB.update {
            $0.paired = true
            $0.lastHostId = "studio-b"
            $0.lastSessionId = "s-001"
        }

        let app = AppShellModel(
            service: serviceA,
            preferences: preferencesA,
            dataDirectory: root,
            options: testLaunchOptions(),
            configuration: store,
            activeInstance: store.activeInstance,
            serviceFactory: { instance in
                instance.id == "cloud-staging" ? serviceB : serviceA
            },
            preferencesFactory: { instance in
                preferences(for: instance.id)
            }
        )
        await app.launchRestore()
        await waitUntil { app.conversation?.hostId == "studio-a" }
        let modelA = try #require(app.cloudSignIn)
        #expect(modelA.account == accountA)
        let generationBefore = app.activationGeneration

        let error = await app.switchInstance(id: "cloud-staging")
        #expect(error == nil)
        #expect(app.activationGeneration > generationBefore)
        #expect(app.activeInstance?.id == "cloud-staging")
        #expect(app.conversation?.hostId == "studio-b")
        #expect(app.conversation?.sessionId == "s-001")
        #expect(app.cloudSignIn?.account == accountB)
        #expect(app.cloudSignIn?.directory.hosts.map(\.hostId) == ["studio-b"])

        // A's delayed revocation arrives after the switch. Its fence is stale,
        // so it can neither sign B out nor clear B's preferences.
        serviceA.refreshError = .security
        await modelA.verifySession()

        #expect(modelA.phase == .signedIn(accountA))
        #expect(modelA.errorCode == nil)
        #expect(app.cloudSignIn?.account == accountB)
        #expect(app.cloudSignIn?.phase.isSignedIn == true)
        #expect(app.isPaired)
        #expect(app.preferences.value.paired)
        #expect(app.preferences.value.lastHostId == "studio-b")
        #expect(app.preferences.value.lastSessionId == "s-001")
        #expect(app.activeInstance?.id == "cloud-staging")
    }
}
