// Remote revocation, manual re-pairing and preference/Core disagreement.
// These are the states that previously dead-ended on a stale conversation.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Revocation and recovery routes")
@MainActor
struct RevocationAndRecoveryTests {
    private func makeApp(
        service: StubSessionService,
        configure: (inout AppPreferences) -> Void = { _ in },
        directory: URL = makeTempDirectory()
    ) -> (AppModel, PreferencesStore) {
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

    @Test("a revoked Host list blocks the conversation with a re-pair/forget route")
    func revokedOnRefresh() async {
        let service = StubSessionService()
        let (app, _) = makeApp(service: service) { $0.paired = true }
        await app.bootstrapConversation()
        #expect(app.conversation != nil)

        service.hostsHandler = { throw SessionServiceError(.revoked) }
        await app.refreshSwitcher()

        #expect(app.isRevoked)
        #expect(app.conversation == nil)
        #expect(app.rootState == .restoring)

        app.beginRepairAfterRevocation()
        #expect(!app.isRevoked)
        #expect(!app.isPaired)
        #expect(app.rootState == .restoring)
    }

    @Test("a revoked watch error enters the blocking state instead of a generic error")
    func revokedWatchError() async {
        let service = StubSessionService()
        let (app, _) = makeApp(service: service) { $0.paired = true }
        await app.bootstrapConversation()
        #expect(app.conversation != nil)
        await waitUntil { service.activeStreamCount == 1 }

        service.emit(.error(code: .revoked, message: "revoked"), sequence: 1)
        await waitUntil { app.isRevoked }

        #expect(app.isRevoked)
        #expect(app.conversation == nil)
    }

    @Test("forget after revocation clears local preferences and calls the service")
    func forgetAfterRevocation() async {
        let service = StubSessionService()
        var forgot = false
        service.forgetHandler = { forgot = true }
        let (app, preferences) = makeApp(service: service) {
            $0.paired = true
            $0.lastHostId = "studio-mac"
            $0.lastSessionId = "s-001"
        }
        await app.bootstrapConversation()
        app.enterRevoked()

        await app.forgetAfterRevocation()

        #expect(forgot)
        #expect(!app.isRevoked)
        #expect(!app.isPaired)
        #expect(!preferences.value.paired)
        #expect(preferences.value.lastHostId == nil)
        #expect(app.conversation == nil)
    }

    @Test("a paired preference with an unpaired Core falls back to onboarding")
    func preferenceCoreMismatch() async {
        let service = StubSessionService()
        service.hostsHandler = { throw SessionServiceError(.notPaired) }
        let (app, preferences) = makeApp(service: service) {
            $0.paired = true
            $0.lastHostId = "studio-mac"
            $0.lastSessionId = "s-001"
        }

        await app.bootstrapConversation()

        #expect(!app.isPaired)
        #expect(!preferences.value.paired)
        #expect(app.conversation == nil)
        #expect(app.rootState == .restoring)
    }

    @Test("an unpaired preference with a usable Core pairing is adopted instead of dead-ending")
    func adoptsCorePairingAfterCorruption() async throws {
        let directory = makeTempDirectory()
        try writePreferencesPayload("{ this is not json", in: directory)
        let preferences = PreferencesStore(directory: directory)
        #expect(preferences.recovery == .corrupt)
        #expect(preferences.value == AppPreferences())

        let service = StubSessionService()
        let app = AppModel(
            service: service,
            preferences: preferences,
            dataDirectory: directory,
            options: testLaunchOptions()
        )

        await app.launchRestore()

        #expect(app.isPaired)
        #expect(preferences.value.paired)
        #expect(app.conversation != nil)
        #expect(app.conversation?.sessionId == "s-001")
    }

    @Test("corrupt, partial and future preference payloads decode to safe defaults")
    func preferencePayloads() throws {
        let corruptDirectory = makeTempDirectory()
        try writePreferencesPayload("{ not json", in: corruptDirectory)
        let corrupt = PreferencesStore(directory: corruptDirectory)
        #expect(corrupt.value == AppPreferences())
        #expect(corrupt.recovery == .corrupt)

        let futureDirectory = makeTempDirectory()
        try writePreferencesPayload(
            #"{"schemaVersion":99,"paired":true,"lastHostId":"studio-mac"}"#,
            in: futureDirectory
        )
        let future = PreferencesStore(directory: futureDirectory)
        #expect(future.value == AppPreferences())
        #expect(future.recovery == .unsupportedVersion(99))

        let typeDirectory = makeTempDirectory()
        try writePreferencesPayload(#"{"paired":"yes"}"#, in: typeDirectory)
        let wrongType = PreferencesStore(directory: typeDirectory)
        #expect(wrongType.value == AppPreferences())
        #expect(wrongType.recovery == .corrupt)

        let partialDirectory = makeTempDirectory()
        try writePreferencesPayload(#"{"schemaVersion":2,"lastHostId":""}"#, in: partialDirectory)
        let partial = PreferencesStore(directory: partialDirectory)
        #expect(partial.value.schemaVersion == AppPreferences.currentSchemaVersion)
        #expect(partial.value.lastHostId == nil)
        #expect(partial.value.lastSessionId == nil)
        #expect(partial.recovery == nil)
    }

    @Test("a version-less payload migrates to the current schema and keeps the selection")
    func preferenceMigration() throws {
        let directory = makeTempDirectory()
        try writePreferencesPayload(
            #"{"paired":true,"lastHostId":"studio-mac","lastSessionId":"s-002","lastSessionTitle":"Build triage"}"#,
            in: directory
        )

        let preferences = PreferencesStore(directory: directory)

        #expect(preferences.value.schemaVersion == AppPreferences.currentSchemaVersion)
        #expect(preferences.value.paired)
        #expect(preferences.value.lastHostId == "studio-mac")
        #expect(preferences.value.lastSessionId == "s-002")
        #expect(preferences.value.lastSessionTitle == "Build triage")
        #expect(preferences.recovery == nil)
    }

    @Test("a partial payload without a Host drops the dependent session selection")
    func partialPayloadDropsSession() throws {
        let directory = makeTempDirectory()
        try writePreferencesPayload(
            #"{"paired":true,"lastSessionId":"s-002","lastSessionTitle":"Build triage"}"#,
            in: directory
        )

        let preferences = PreferencesStore(directory: directory)

        #expect(preferences.value.paired)
        #expect(preferences.value.lastHostId == nil)
        #expect(preferences.value.lastSessionId == nil)
        #expect(preferences.value.lastSessionTitle == nil)
    }

    @Test("manual pairing from settings adds a second Host and switches the root")
    func manualSecondHost() async throws {
        let service = StubSessionService()
        service.pairingStartHandler = { _ in fixturePrompt(hostId: "travel-laptop") }
        service.pairingDecideHandler = { _, _ in }
        let (app, preferences) = makeApp(service: service) {
            $0.paired = true
            $0.lastHostId = "studio-mac"
        }
        await app.bootstrapConversation()
        #expect(app.conversation?.hostId == "studio-mac")

        app.openManualPairing()
        #expect(app.showInvitationSheet)
        #expect(app.pairing.phase == .editing)

        app.pairing.updateInvitation(FixtureCatalog.invitationURL(hostId: "travel-laptop").absoluteString)
        await app.pairing.submit()
        await app.pairing.confirm()

        #expect(app.isPaired)
        #expect(preferences.value.lastHostId == "travel-laptop")
        await waitUntil { app.conversation?.hostId == "travel-laptop" }
        #expect(app.conversation?.hostId == "travel-laptop")
        #expect(!app.showInvitationSheet)
    }
}
