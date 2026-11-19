// Composition-root wiring: launch restores the last conversation, the switcher
// changes Host/session without a second navigation path, and forget/revoke
// clears conversation and switcher state.

import Foundation
import Testing
@testable import WhaleRein

@Suite("App composition")
@MainActor
struct AppModelTests {
    private func makeApp(
        service: StubSessionService,
        configure: (inout AppPreferences) -> Void = { _ in }
    ) -> (AppModel, PreferencesStore) {
        let directory = makeTempDirectory()
        let preferences = PreferencesStore(directory: directory)
        preferences.update(configure)
        let app = AppModel(
            service: service,
            preferences: preferences,
            dataDirectory: directory,
            options: testLaunchOptions()
        )
        return (app, preferences)
    }

    @Test("paired launch state is seeded only for reset UI fixtures")
    func pairedFixtureLaunchOptions() {
        let paired = "-ui-fixture-paired"
        let fixtures = "-ui-fixtures"
        let reset = "-ui-test-reset"

        #expect(testLaunchOptions([fixtures, reset, paired]).seedPairedFixture)
        #expect(!testLaunchOptions([fixtures, paired]).seedPairedFixture)
        #expect(!testLaunchOptions([reset, paired]).seedPairedFixture)
        #expect(!testLaunchOptions([fixtures, reset]).seedPairedFixture)
        #expect(!testLaunchOptions([fixtures, reset, paired, "-integration-fixtures"]).seedPairedFixture)
    }

    @Test("confirming pairing marks the device paired and opens a conversation")
    func pairingWiring() async {
        let service = StubSessionService()
        service.pairingStartHandler = { _ in fixturePrompt() }
        service.pairingDecideHandler = { _, _ in }
        let (app, preferences) = makeApp(service: service)

        app.pairing.updateInvitation(FixtureCatalog.invitationURL(hostId: "studio-mac").absoluteString)
        await app.pairing.submit()
        await app.pairing.confirm()

        #expect(app.isPaired)
        #expect(preferences.value.paired)
        #expect(preferences.value.lastHostId == "studio-mac")
        await waitUntil { app.conversation != nil }
        #expect(app.conversation?.hostId == "studio-mac")
        #expect(app.conversation?.sessionId == "s-001")
        #expect(app.rootState == .ready)
    }

    @Test("paired launch restores the last session and opens its conversation")
    func restoresLastSession() async {
        let service = StubSessionService()
        let (app, preferences) = makeApp(service: service) {
            $0.paired = true
            $0.lastHostId = "studio-mac"
            $0.lastSessionId = "s-002"
            $0.lastSessionTitle = "Build triage"
        }

        await app.bootstrapConversation()

        #expect(app.conversation?.hostId == "studio-mac")
        #expect(app.conversation?.sessionId == "s-002")
        #expect(app.conversation?.sessionTitle == "Build triage")
        #expect(app.selectedHostId == "studio-mac")
        #expect(app.rootState == .ready)
        #expect(!app.showSwitcher)
        #expect(preferences.value.lastSessionId == "s-002")
    }

    @Test("a missing last session falls back to the most recent session and persists it")
    func missingLastSessionFallsBack() async {
        let service = StubSessionService()
        let (app, preferences) = makeApp(service: service) {
            $0.paired = true
            $0.lastHostId = "studio-mac"
            $0.lastSessionId = "s-deleted"
        }

        await app.bootstrapConversation()

        #expect(app.conversation?.sessionId == "s-001")
        #expect(app.conversation?.sessionTitle == "Release checklist")
        #expect(preferences.value.lastSessionId == "s-001")
        #expect(preferences.value.lastSessionTitle == "Release checklist")
    }

    @Test("an unknown remembered Host selects an authorized Host, preferring online")
    func unknownHostPrefersOnline() async {
        let service = StubSessionService()
        let offline = FixtureCatalog.hosts[1]
        let online = FixtureCatalog.hosts[0]
        service.hostsHandler = { [offline, online] }
        let (app, preferences) = makeApp(service: service) {
            $0.paired = true
            $0.lastHostId = "retired-mac"
        }

        await app.bootstrapConversation()

        #expect(app.selectedHostId == "studio-mac")
        #expect(app.conversation?.hostId == "studio-mac")
        #expect(app.conversation?.sessionId == "s-001")
        #expect(preferences.value.lastHostId == "studio-mac")
        #expect(app.sessionList.hostId == "studio-mac")
    }

    @Test("an offline Host uses cached sessions and opens the cached last session")
    func offlineHostUsesCachedSessions() async {
        let service = StubSessionService()
        service.sessionsHandler = { _ in throw SessionServiceError(.hostOffline) }
        let (app, preferences) = makeApp(service: service) {
            $0.paired = true
            $0.lastHostId = "travel-laptop"
            $0.lastSessionId = "s-102"
        }

        await app.bootstrapConversation()

        #expect(app.conversation?.hostId == "travel-laptop")
        #expect(app.conversation?.sessionId == "s-102")
        #expect(app.isOfflineCached)
        #expect(app.sessionList.isShowingCached)
        #expect(app.sessionList.sessions.map(\.sessionId) == ["s-101", "s-102"])
        #expect(preferences.value.lastSessionId == "s-102")
    }

    @Test("switching Host in the switcher keeps the open conversation until a session is chosen")
    func switcherHostSwitchKeepsConversation() async throws {
        let service = StubSessionService()
        let (app, preferences) = makeApp(service: service) { $0.paired = true }
        await app.bootstrapConversation()
        #expect(app.conversation?.sessionId == "s-001")

        app.showSessionSwitcher()
        #expect(app.showSwitcher)

        await app.selectHostInSwitcher("travel-laptop")

        #expect(app.selectedHostId == "travel-laptop")
        #expect(app.sessionList.hostId == "travel-laptop")
        #expect(app.sessionList.sessions.map(\.sessionId) == ["s-101", "s-102"])
        #expect(app.conversation?.hostId == "studio-mac")
        #expect(app.conversation?.sessionId == "s-001")

        let session = try #require(app.sessionList.sessions.first { $0.sessionId == "s-101" })
        app.selectSessionInSwitcher(session)

        #expect(app.conversation?.hostId == "travel-laptop")
        #expect(app.conversation?.sessionId == "s-101")
        #expect(!app.showSwitcher)
        #expect(preferences.value.lastHostId == "travel-laptop")
        #expect(preferences.value.lastSessionId == "s-101")
    }

    @Test("switcher session failures expose a typed error without changing the conversation")
    func switcherErrorState() async {
        let service = StubSessionService()
        let (app, _) = makeApp(service: service) { $0.paired = true }
        await app.bootstrapConversation()

        service.sessionsHandler = { _ in throw SessionServiceError(.unavailable) }
        service.cachedSessionsHandler = { _ in throw SessionServiceError(.unavailable) }
        await app.selectHostInSwitcher("travel-laptop")

        #expect(app.sessionList.loadState == .failed(.unavailable))
        #expect(app.sessionList.errorKey == .errorUnavailable)
        #expect(app.sessionList.sessions.isEmpty)
        #expect(app.conversation?.sessionId == "s-001")
    }

    @Test("switcher empty sessions expose the loaded empty state")
    func switcherEmptyState() async {
        let service = StubSessionService()
        let (app, _) = makeApp(service: service) { $0.paired = true }
        await app.bootstrapConversation()

        service.sessionsHandler = { _ in [] }
        await app.selectHostInSwitcher("travel-laptop")

        #expect(app.sessionList.loadState == .loaded)
        #expect(app.sessionList.sessions.isEmpty)
        #expect(app.conversation?.sessionId == "s-001")
    }

    @Test("launch without any Host exposes the no-Host empty state")
    func noHostsEmptyState() async {
        let service = StubSessionService()
        service.hostsHandler = { [] }
        let (app, _) = makeApp(service: service) { $0.paired = true }

        await app.bootstrapConversation()

        #expect(app.conversation == nil)
        #expect(app.rootState == .empty(.noHosts))
    }

    @Test("launch without any session exposes the no-session empty state")
    func noSessionsEmptyState() async {
        let service = StubSessionService()
        service.sessionsHandler = { _ in [] }
        let (app, _) = makeApp(service: service) { $0.paired = true }

        await app.bootstrapConversation()

        #expect(app.conversation == nil)
        #expect(app.rootState == .empty(.noSessions))
    }

    @Test("forget resets the root back to the invitation flow")
    func forgetResetsRoot() async {
        let service = StubSessionService()
        let (app, preferences) = makeApp(service: service) { $0.paired = true }
        await app.bootstrapConversation()
        app.showSessionSwitcher()
        app.showSettings = true
        #expect(app.conversation != nil)

        await app.settings.perform(.forget)

        #expect(!app.isPaired)
        #expect(!preferences.value.paired)
        #expect(app.conversation == nil)
        #expect(!app.showSwitcher)
        #expect(app.selectedHostId == nil)
        #expect(app.hostList.hosts.isEmpty)
        #expect(app.sessionList.sessions.isEmpty)
        #expect(app.rootState == .restoring)
        #expect(!app.showSettings)
        #expect(app.settings.statusKey == .settingsForgotten)
    }

    @Test("revoke clears conversation, Host and switcher state")
    func revokeResetsRoot() async {
        let service = StubSessionService()
        let (app, preferences) = makeApp(service: service) { $0.paired = true }
        await app.bootstrapConversation()
        app.showSessionSwitcher()

        await app.settings.perform(.revoke)

        #expect(!app.isPaired)
        #expect(!preferences.value.paired)
        #expect(app.conversation == nil)
        #expect(!app.showSwitcher)
        #expect(app.selectedHostId == nil)
        #expect(app.hostList.hosts.isEmpty)
        #expect(app.sessionList.sessions.isEmpty)
        #expect(app.settings.statusKey == .settingsRevoked)
    }
}
