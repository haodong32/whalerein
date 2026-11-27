// App lifecycle: inactive shields and pauses without touching the healthy
// watch; background flushes the draft, stops the watch once and records a
// resume marker; foreground starts one watch from the Core cursor and resyncs
// the visible session. The visible-watch tests use the deterministic stub
// service.

import Foundation
import Testing
@testable import WhaleRein

@Suite("App lifecycle")
@MainActor
struct AppLifecycleTests {
    private func makeApp(
        service: StubSessionService,
        configure: (inout AppPreferences) -> Void = { _ in }
    ) -> (AppShellModel, PreferencesStore) {
        let directory = makeTempDirectory()
        let preferences = PreferencesStore(directory: directory)
        preferences.update(configure)
        let app = AppShellModel(
            service: service,
            preferences: preferences,
            dataDirectory: directory,
            options: testLaunchOptions()
        )
        return (app, preferences)
    }

    @Test("background flushes the draft, shields content, stops the watch once and records a resume marker")
    func backgroundFlushesAndShields() async {
        let service = StubSessionService()
        let (app, _) = makeApp(service: service) { $0.paired = true }
        await app.bootstrapConversation()
        await waitUntil { app.conversation?.eventSubscriptionCount == 1 }
        service.emit(.reset(messages: [], approvals: []), sequence: 1)
        await waitUntil { app.conversation?.loadState == .loaded }
        #expect(app.conversation?.sessionId == "s-001")

        app.conversation?.draft = "half-typed"
        await app.enterBackground()

        #expect(app.isPrivacyShieldVisible)
        #expect(app.resumeMarker?.hostId == "studio-mac")
        #expect(app.resumeMarker?.sessionId == "s-001")
        #expect(service.stopWatchCount == 1)
        #expect(app.conversation?.isSuspended == true)
        #expect(app.conversation?.isRenderingPaused == true)
        #expect(app.conversation?.eventSubscriptionCount == 1)

        let key = AppStorageLocations.sessionKey(hostId: "studio-mac", sessionId: "s-001")
        await waitUntil { service.savedDrafts[key] == "half-typed" }
        #expect(service.savedDrafts[key] == "half-typed")
    }

    @Test("foreground restarts the watch from the cursor and applies the new reset instead of a stale stream")
    func foregroundResyncs() async {
        let service = StubSessionService()
        let (app, _) = makeApp(service: service) { $0.paired = true }
        await app.bootstrapConversation()
        await waitUntil { service.activeStreamCount == 2 }
        service.emit(
            .reset(messages: [], approvals: []),
            sequence: 1
        )
        await waitUntil { app.conversation?.loadState == .loaded }
        let conversation = app.conversation

        await app.enterBackground()
        service.emit(.watchEnded(reason: "stopped"), sequence: 2)
        await waitUntil { app.conversation?.watchEnded == true }
        await app.becomeActive()

        #expect(!app.isPrivacyShieldVisible)
        #expect(service.startWatchCalls.count == 2)
        #expect(app.conversation === conversation)
        #expect(app.conversation?.isSuspended == false)
        #expect(app.conversation?.isRenderingPaused == false)
        #expect(app.conversation?.watchEnded == false)
        #expect(app.conversation?.lastAppliedSequence == -1)
        #expect(app.conversation?.eventSubscriptionCount == 1)

        let resumed = ConversationMessage(
            messageId: "msg-resumed",
            role: .assistant,
            blocks: [ConversationBlock(blockId: "msg-resumed-block-0", kind: .text, text: "resynced")],
            createdAtUnixMs: 1
        )
        service.emit(.reset(messages: [resumed], approvals: []), sequence: 3)
        await waitUntil { app.conversation?.messages.map(\.messageId) == ["msg-resumed"] }
        #expect(app.conversation?.messages.map(\.messageId) == ["msg-resumed"])
        #expect(app.conversation?.loadState == .loaded)
    }

    @Test("inactive is shielded synchronously and keeps the healthy watch and probe")
    func scenePhaseShield() async {
        let service = StubSessionService()
        let (app, _) = makeApp(service: service) { $0.paired = true }
        await app.bootstrapConversation()
        await waitUntil { app.conversation?.eventSubscriptionCount == 1 }
        service.emit(.reset(messages: [], approvals: []), sequence: 1)
        await waitUntil { app.conversation?.loadState == .loaded }
        let startWatches = service.startWatchCalls.count
        let monitorStarts = service.startHostMonitorCalls.count

        let inactive = app.scenePhaseDidChange(.inactive)
        // The shield is up before anything is awaited.
        #expect(app.isPrivacyShieldVisible)
        #expect(app.conversation?.isRenderingPaused == true)
        await inactive.value
        #expect(service.stopWatchCount == 0)
        #expect(service.stopHostMonitorCount == 0)
        #expect(service.startWatchCalls.count == startWatches)
        #expect(service.startHostMonitorCalls.count == monitorStarts)
        #expect(app.connectivity.isOnline("studio-mac"))

        await app.handleScenePhase(.active)
        #expect(!app.isPrivacyShieldVisible)
        #expect(app.conversation?.isRenderingPaused == false)
        #expect(service.stopWatchCount == 0)
        #expect(service.startWatchCalls.count == startWatches)
        #expect(service.eventStreamCount == 2)
    }

    @Test("closing a conversation flushes a draft that is still inside the debounce window")
    func closeFlushesDraft() async {
        let service = StubSessionService()
        let model = await makeStartedConversation(service: service)
        model.draft = "close me now"
        model.draftChanged()

        model.close()

        let key = AppStorageLocations.sessionKey(hostId: "studio-mac", sessionId: "s-001")
        await waitUntil { service.savedDrafts[key] == "close me now" }
        #expect(service.savedDrafts[key] == "close me now")
    }

    @Test("suspending stops the watch once and resuming reuses the one subscription")
    func subscriptionLifecycle() async {
        let service = StubSessionService()
        let model = await makeStartedConversation(service: service)
        #expect(service.eventStreamCount == 1)
        #expect(service.activeStreamCount == 1)
        #expect(service.startWatchCalls.count == 1)

        await model.suspendForBackground()
        #expect(service.stopWatchCount == 1)
        #expect(service.eventStreamCount == 1)
        #expect(model.isSuspended)

        await model.resumeAfterForeground()
        #expect(service.startWatchCalls.count == 2)
        #expect(service.eventStreamCount == 1)
        #expect(service.activeStreamCount == 1)
        #expect(!model.isSuspended)
        #expect(!model.watchEnded)
    }
}