// Deep-link arrival semantics: a link that arrives while pairing is busy is
// queued with visible feedback instead of being silently dropped. No push
// notifications exist, so the in-app URL path is the only delivery path and is
// covered here at the model boundary.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Deep-link pairing queue")
@MainActor
struct DeepLinkQueueTests {
    @Test("a link arriving during validation is queued and processed after the first attempt")
    func queuedDuringValidation() async {
        let service = StubSessionService()
        service.pairingStartHandler = { invitation in
            try? await Task.sleep(for: .milliseconds(80))
            let host = invitation.contains("travel-laptop") ? "travel-laptop" : "studio-mac"
            return fixturePrompt(hostId: host)
        }
        service.pairingDecideHandler = { _, _ in }
        let model = PairingModel(service: service)
        let first = FixtureCatalog.invitationURL(hostId: "studio-mac")
        let second = FixtureCatalog.invitationURL(hostId: "travel-laptop")

        let firstAttempt = Task { await model.handleOpenURL(first) }
        try? await Task.sleep(for: .milliseconds(20))
        #expect(model.isBusy)

        await model.handleOpenURL(second)
        #expect(model.queuedLinkKey == .pairingLinkQueued)
        #expect(model.invitationText == first.absoluteString, "The in-flight link must not be clobbered")

        await firstAttempt.value
        #expect(model.prompt?.hostId == "studio-mac")
        #expect(model.queuedLinkKey == .pairingLinkQueued)

        await model.decline()
        await waitUntil { service.pairingStartCount == 2 }
        #expect(service.pairingStartCount == 2)
        #expect(model.prompt?.hostId == "travel-laptop")
        #expect(model.invitationText == second.absoluteString)
        #expect(model.queuedLinkKey == nil)
    }

    @Test("a successful pairing clears a queued link instead of starting a second flow")
    func queuedLinkClearedOnSuccess() async {
        let service = StubSessionService()
        service.pairingStartHandler = { invitation in
            try? await Task.sleep(for: .milliseconds(60))
            let host = invitation.contains("travel-laptop") ? "travel-laptop" : "studio-mac"
            return fixturePrompt(hostId: host)
        }
        service.pairingDecideHandler = { _, _ in }
        let model = PairingModel(service: service)
        let first = FixtureCatalog.invitationURL(hostId: "studio-mac")
        let second = FixtureCatalog.invitationURL(hostId: "travel-laptop")

        let firstAttempt = Task { await model.handleOpenURL(first) }
        try? await Task.sleep(for: .milliseconds(20))
        await model.handleOpenURL(second)
        await firstAttempt.value
        #expect(model.queuedLinkKey == .pairingLinkQueued)

        await model.confirm()

        #expect(model.queuedLinkKey == nil)
        #expect(service.pairingStartCount == 1, "Confirming must not start the queued link")
    }

    @Test("a paired app opens the pairing sheet for an incoming link")
    func pairedAppOpensSheet() async {
        let service = StubSessionService()
        service.pairingStartHandler = { _ in fixturePrompt() }
        service.pairingDecideHandler = { _, _ in }
        let directory = makeTempDirectory()
        let preferences = PreferencesStore(directory: directory)
        preferences.update { $0.paired = true }
        let app = AppShellModel(
            service: service,
            preferences: preferences,
            dataDirectory: directory,
            options: testLaunchOptions()
        )
        app.router = AppRouter()
        await app.bootstrapConversation()
        #expect(app.conversation != nil)
        #expect(!app.showInvitationSheet)

        await app.handleOpenURL(FixtureCatalog.invitationURL(hostId: "studio-mac"))

        #expect(app.showInvitationSheet)
        #expect(app.pairing.prompt != nil)
        #expect(app.isLaunched)
    }

    @Test("a system link arriving during a paired cold launch is replayed, not dropped")
    func coldLaunchLinkIsReplayed() async {
        let service = StubSessionService()
        service.pairingStartHandler = { _ in fixturePrompt() }
        service.pairingDecideHandler = { _, _ in }
        let hostsGate = SuspendedResponse<[HostSummary]>()
        service.hostsHandler = { try await hostsGate.wait() }
        let directory = makeTempDirectory()
        let preferences = PreferencesStore(directory: directory)
        preferences.update { $0.paired = true }
        let app = AppShellModel(
            service: service,
            preferences: preferences,
            dataDirectory: directory,
            options: testLaunchOptions()
        )
        app.router = AppRouter()
        #expect(!app.isLaunched)

        let launch = Task { await app.bootstrapConversation() }
        await hostsGate.waitUntilStarted()
        let link = FixtureCatalog.invitationURL(hostId: "studio-mac")
        await app.handleOpenURL(link)

        // The restore is still in flight, so the link is held instead of
        // starting a flow that the restore would dismiss.
        #expect(app.pendingInvitationLink == link)
        #expect(app.pairing.prompt == nil)

        hostsGate.resume(with: FixtureCatalog.hosts)
        await launch.value
        await waitUntil { app.pairing.prompt != nil }

        #expect(app.isLaunched)
        #expect(app.pendingInvitationLink == nil)
        #expect(app.pairing.prompt?.hostId == "studio-mac")
        #expect(app.showInvitationSheet)
    }

    @Test("an unpaired cold-launch link starts pairing after restore completes")
    func unpairedColdLaunchLinkIsReplayed() async {
        let service = StubSessionService()
        service.pairingStartHandler = { _ in fixturePrompt() }
        service.pairingDecideHandler = { _, _ in }
        service.hostsHandler = { throw SessionServiceError(.notPaired) }
        let directory = makeTempDirectory()
        let app = AppShellModel(
            service: service,
            preferences: PreferencesStore(directory: directory),
            dataDirectory: directory,
            options: testLaunchOptions()
        )
        app.router = AppRouter()
        let link = FixtureCatalog.invitationURL(hostId: "studio-mac")

        await app.handleOpenURL(link)
        #expect(app.pendingInvitationLink == link)
        #expect(app.pairing.prompt == nil)

        await app.launchRestore()
        await waitUntil { app.pairing.prompt != nil }

        #expect(app.pendingInvitationLink == nil)
        #expect(app.pairing.prompt?.hostId == "studio-mac")
        #expect(app.pairing.invitationText == link.absoluteString)
    }

    @Test("an unpaired app starts pairing directly from an incoming link")
    func unpairedAppStartsPairing() async {
        let service = StubSessionService()
        service.pairingStartHandler = { _ in fixturePrompt() }
        service.pairingDecideHandler = { _, _ in }
        let directory = makeTempDirectory()
        let app = AppShellModel(
            service: service,
            preferences: PreferencesStore(directory: directory),
            dataDirectory: directory,
            options: testLaunchOptions()
        )
        app.finishLaunch()

        await app.handleOpenURL(FixtureCatalog.invitationURL(hostId: "studio-mac"))

        #expect(!app.showInvitationSheet)
        #expect(app.pairing.prompt?.hostId == "studio-mac")
        #expect(app.pairing.invitationText == FixtureCatalog.invitationURL(hostId: "studio-mac").absoluteString)
    }
}
