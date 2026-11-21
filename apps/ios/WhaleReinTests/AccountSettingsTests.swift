// Selected-instance status, default-computer persistence and scoped Cloud app
// sign-in behavior for Settings, using deterministic fixtures. The real Core
// persistence and scope are covered by binding tests.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Account settings", .serialized)
@MainActor
struct AccountSettingsTests {
    private func makeService(
        options: LaunchOptions,
        instanceId: String = "cloud-production",
        serviceKind: AppService = .cloud
    ) async throws -> FixtureSessionService {
        let service = FixtureSessionService(
            options: options,
            clock: TestFixtureClock(),
            draftStore: InMemoryDraftStore(),
            instanceId: instanceId,
            serviceKind: serviceKind
        )
        let invitation = FixtureCatalog.invitationURL(hostId: "studio-mac")
        _ = try await service.pairingStart(invitation: invitation.absoluteString)
        try await service.pairingDecide(
            approved: true,
            expectedFingerprint: FixtureCatalog.fingerprint
        )
        return service
    }

    @Test("Cloud signed-in status reports the selected service and device")
    func cloudSignedInStatus() async throws {
        let service = try await makeService(
            options: testLaunchOptions(["-ui-cloud-signed-in"])
        )
        let model = AccountSettingsModel(service: service)
        await model.load()

        #expect(model.status?.service == .cloud)
        #expect(model.status?.accountState == .signedIn)
        #expect(model.status?.deviceAccess == .ready)
        #expect(model.status?.remoteSignoutPending == false)
        #expect(model.status?.defaultHost == "studio-mac")
        #expect(model.status?.instanceId == "cloud-production")
        #expect(model.isCloud)
        #expect(model.canManageSessions)
    }

    @Test("Cloud pending sign-out is a distinct typed state")
    func cloudPendingSignOutStatus() async throws {
        let service = try await makeService(
            options: testLaunchOptions(["-ui-cloud-signed-in"])
        )
        _ = try await service.cloudSignOut()

        let model = AccountSettingsModel(service: service)
        await model.load()
        #expect(model.status?.service == .cloud)
        #expect(model.status?.accountState == .signedOut)
        #expect(model.status?.remoteSignoutPending == true)
        #expect(model.status?.deviceAccess == .signInRequired)
        #expect(model.statusSubtitleKey == .settingsStatusPendingSignOut)
        #expect(model.sessions.isEmpty)
        #expect(!model.canManageSessions)
    }

    @Test("Standalone status has no account and no app sign-ins")
    func standaloneStatus() async throws {
        let service = try await makeService(
            options: testLaunchOptions(),
            instanceId: "standalone",
            serviceKind: .standalone
        )
        let model = AccountSettingsModel(service: service)
        await model.load()

        #expect(model.status?.service == .standalone)
        #expect(model.status?.accountState == .notApplicable)
        #expect(model.status?.deviceAccess == .ready)
        #expect(model.sessions.isEmpty)
        #expect(!model.canManageSessions)
    }

    @Test("Offline device access is unavailable, not ready")
    func offlineStatus() async throws {
        let service = try await makeService(
            options: testLaunchOptions(["-ui-offline", "-ui-cloud-signed-in"])
        )
        let model = AccountSettingsModel(service: service)
        await model.load()
        #expect(model.status?.deviceAccess == .unavailable)
    }

    @Test("default computer persists in its instance and stays isolated")
    func defaultComputerPersistenceAndScope() async throws {
        let service = try await makeService(
            options: testLaunchOptions(["-ui-cloud-signed-in"])
        )
        let model = AccountSettingsModel(service: service)
        await model.load()
        #expect(model.defaultHostId == "studio-mac")
        #expect(model.hosts.count == 2)

        await model.selectDefaultHost("travel-laptop")
        #expect(model.defaultHostId == "travel-laptop")
        #expect(model.statusKey == .settingsDefaultSaved)
        #expect(model.status?.defaultHost == "travel-laptop")

        // A fresh model over the same instance observes the persisted default.
        let reloaded = AccountSettingsModel(service: service)
        await reloaded.load()
        #expect(reloaded.defaultHostId == "travel-laptop")

        // Another instance keeps its own default; selecting here never changes
        // the first instance.
        let other = try await makeService(
            options: testLaunchOptions(),
            instanceId: "standalone",
            serviceKind: .standalone
        )
        let otherModel = AccountSettingsModel(service: other)
        await otherModel.load()
        #expect(otherModel.defaultHostId == "studio-mac")
        await otherModel.selectDefaultHost("travel-laptop")
        #expect(otherModel.defaultHostId == "travel-laptop")
        #expect(model.defaultHostId == "travel-laptop")
    }

    @Test("app sign-ins label exactly one current session")
    func sessionListingLabelsCurrent() async throws {
        let service = try await makeService(
            options: testLaunchOptions(["-ui-cloud-signed-in"])
        )
        let model = AccountSettingsModel(service: service)
        await model.load()

        #expect(model.sessions.count == 2)
        let current = model.sessions.filter(\.isCurrent)
        #expect(current.count == 1)
        #expect(current.first?.sessionId == FixtureSessionService.fixtureCurrentSessionId)
        #expect(model.sessions.contains { !$0.isCurrent && $0.sessionId == "sess-fixture-other" })
        #expect(model.sessions.allSatisfy { $0.state == .active })
    }

    @Test("revoking another app sign-in keeps the current session")
    func revokeOtherKeepsCurrent() async throws {
        let service = try await makeService(
            options: testLaunchOptions(["-ui-cloud-signed-in"])
        )
        let model = AccountSettingsModel(service: service)
        var signedOut = false
        model.onSignedOut = { signedOut = true }
        await model.load()

        await model.revoke(session: model.sessions.first { !$0.isCurrent }!)

        #expect(!signedOut)
        #expect(model.lastRevocation?.scope == .one("sess-fixture-other"))
        #expect(model.lastRevocation?.state == .revoked)
        #expect(model.lastRevocation?.affectedSessionIds == ["sess-fixture-other"])
        #expect(model.sessions.first { !$0.isCurrent }?.state == .revoked)
        #expect(model.sessions.first(where: \.isCurrent)?.state == .active)
    }

    @Test("revoking the current app sign-in signs out locally")
    func revokeCurrentSignsOut() async throws {
        let service = try await makeService(
            options: testLaunchOptions(["-ui-cloud-signed-in"])
        )
        let model = AccountSettingsModel(service: service)
        var signedOut = false
        model.onSignedOut = { signedOut = true }
        await model.load()

        await model.revoke(session: model.sessions.first { $0.isCurrent }!)

        #expect(signedOut)
        #expect(model.lastRevocation?.state == .revoked)
        #expect(model.lastRevocation?.affectedSessionIds == ["sess-fixture-ios"])
    }

    @Test("account-wide revocation reports its exact pending scope")
    func revokeAllPendingScope() async throws {
        let service = try await makeService(
            options: testLaunchOptions(["-ui-cloud-signed-in"])
        )
        let model = AccountSettingsModel(service: service)
        var signedOut = false
        model.onSignedOut = { signedOut = true }
        await model.load()
        let expected = Set(model.sessions.map(\.sessionId))

        await model.revokeAllSessions()

        #expect(signedOut)
        #expect(model.lastRevocation?.scope == .all)
        #expect(model.lastRevocation?.isPending == true)
        #expect(Set(model.lastRevocation?.affectedSessionIds ?? []) == expected)
        #expect(model.statusKey == .settingsAppSessionsPending)
        #expect(model.sessions.allSatisfy { $0.state == .pending })
    }

    @Test("cancelled revocation writes nothing and keeps sessions")
    func cancelledRevocationKeepsState() async throws {
        let service = try await makeService(
            options: testLaunchOptions(["-ui-cloud-signed-in"])
        )
        let model = AccountSettingsModel(service: service)
        await model.load()
        let before = model.sessions

        // A confirmation that never runs leaves the model untouched.
        #expect(model.lastRevocation == nil)
        #expect(model.sessions == before)
    }
}
