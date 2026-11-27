// Scene lifecycle: the ordered owner that separates a brief inactive
// interruption from an actual background suspension.
//
// The covered behaviour is model-level: the shield and rendering pause are
// synchronous, inactivity performs no network work and keeps the watch, a real
// background entry persists the draft and stops the watch exactly once, and a
// foreground return resumes once from the Core cursor. Rapid or repeated
// notifications coalesce, and a stale asynchronous completion can never reveal
// content, resume work or refresh a surface for a newer phase.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Scene lifecycle")
@MainActor
struct SceneLifecycleTests {
    /// Builds a paired shell whose first conversation is loaded through the
    /// deterministic stub service.
    private func makeLoadedApp(
        service: StubSessionService,
        draft: String? = nil
    ) async -> AppShellModel {
        let directory = makeTempDirectory()
        let preferences = PreferencesStore(directory: directory)
        preferences.update {
            $0.paired = true
            $0.lastHostId = "studio-mac"
            $0.lastSessionId = "s-001"
            $0.lastSessionTitle = "Release checklist"
        }
        let app = AppShellModel(
            service: service,
            preferences: preferences,
            dataDirectory: directory,
            options: testLaunchOptions()
        )
        await app.bootstrapConversation()
        await waitUntil { app.conversation?.eventSubscriptionCount == 1 }
        service.emit(.reset(messages: [], approvals: []), sequence: 1)
        await waitUntil { app.conversation?.loadState == .loaded }
        app.conversation?.draft = draft ?? ""
        return app
    }

    private func message(_ id: String) -> ConversationMessage {
        ConversationMessage(
            messageId: id,
            role: .assistant,
            blocks: [ConversationBlock(blockId: "\(id)-block-0", kind: .text, text: id)],
            createdAtUnixMs: 1
        )
    }

    // MARK: - Ordered owner

    @Test("repeated notifications coalesce into one transition")
    func repeatedNotificationsCoalesce() {
        var scene = AppSceneLifecycle()
        #expect(scene.accept(.active) == nil)

        let inactive = scene.accept(.inactive)
        #expect(inactive?.generation == 1)
        #expect(scene.accept(.inactive) == nil)

        let background = scene.accept(.background)
        #expect(background?.generation == 2)
        #expect(background?.resumesObservation == false)
        #expect(scene.accept(.background) == nil)

        let active = scene.accept(.active)
        #expect(active?.generation == 3)
        #expect(active?.resumesObservation == true)
        #expect(scene.accept(.active) == nil)
        #expect(scene.generation == 3)
        #expect(scene.observationSuspended)

        // A newer notification invalidates the older transition.
        #expect(!scene.isCurrent(background!))
        scene.observationDidResume()
        #expect(!scene.observationSuspended)
    }

    @Test("inactive then active performs no watch or subscription work")
    func inactiveActiveNoChurn() async {
        let service = StubSessionService()
        let app = await makeLoadedApp(service: service)
        let startWatches = service.startWatchCalls.count
        let monitorStarts = service.startHostMonitorCalls.count

        let inactive = app.scenePhaseDidChange(.inactive)
        // The synchronous part runs before the test awaits anything.
        #expect(app.isPrivacyShieldVisible)
        #expect(app.conversation?.isRenderingPaused == true)
        #expect(app.scenePhase == .inactive)
        await inactive.value

        #expect(service.stopWatchCount == 0)
        #expect(service.stopHostMonitorCount == 0)
        #expect(service.startWatchCalls.count == startWatches)
        #expect(service.startHostMonitorCalls.count == monitorStarts)
        #expect(app.connectivity.status(for: "studio-mac") == .connected)
        #expect(app.conversation?.watchEnded == false)

        // A live update still advances the authorized cursor while covered.
        service.emit(.message(message("while-inactive")), sequence: 2)
        await waitUntil { app.conversation?.messages.map(\.messageId) == ["while-inactive"] }
        #expect(app.conversation?.lastAppliedSequence == 2)

        await app.handleScenePhase(.active)
        #expect(!app.isPrivacyShieldVisible)
        #expect(app.conversation?.isRenderingPaused == false)
        #expect(app.conversation?.messages.map(\.messageId) == ["while-inactive"])
        #expect(service.stopWatchCount == 0)
        #expect(service.startWatchCalls.count == startWatches)
        #expect(service.eventStreamCount == 2)
        #expect(app.connectivity.status(for: "studio-mac") == .connected)
    }

    @Test("background persists the draft and stops the watch exactly once")
    func backgroundSuspendsOnce() async {
        let service = StubSessionService()
        let app = await makeLoadedApp(service: service, draft: "only in memory")

        let first = app.scenePhaseDidChange(.background)
        #expect(app.isPrivacyShieldVisible)
        #expect(app.conversation?.isRenderingPaused == true)
        let duplicate = app.scenePhaseDidChange(.background)
        await first.value
        await duplicate.value

        #expect(service.stopWatchCount == 1)
        #expect(service.stopHostMonitorCount == 1)
        #expect(service.saveDraftCount == 1)
        #expect(app.conversation?.isSuspended == true)
        #expect(app.resumeMarker?.sessionId == "s-001")
        let key = AppStorageLocations.sessionKey(hostId: "studio-mac", sessionId: "s-001")
        #expect(service.savedDrafts[key] == "only in memory")
    }

    @Test("foreground resumes once and does not duplicate subscriptions or messages")
    func foregroundResumesOnce() async {
        let service = StubSessionService()
        let app = await makeLoadedApp(service: service, draft: "queued text")
        await app.enterBackground()
        #expect(service.stopWatchCount == 1)

        let first = app.scenePhaseDidChange(.active)
        let duplicate = app.scenePhaseDidChange(.active)
        await first.value
        await duplicate.value

        #expect(!app.isPrivacyShieldVisible)
        #expect(app.conversation?.isSuspended == false)
        #expect(service.stopWatchCount == 1)
        #expect(service.startWatchCalls.count == 2)
        #expect(app.conversation?.eventSubscriptionCount == 1)
        #expect(app.conversation?.lastAppliedSequence == -1)

        // The restarted watch sees the same committed row once, and a repeated
        // delivery of one message id replaces instead of duplicating it.
        service.emit(.message(message("m-1")), sequence: 3)
        service.emit(.message(message("m-1")), sequence: 4)
        await waitUntil { app.conversation?.messages.count == 1 }
        #expect(app.conversation?.messages.map(\.messageId) == ["m-1"])
    }

    @Test("rapid repeated notifications settle on the newest phase without churn")
    func rapidNotificationsSettle() async {
        let service = StubSessionService()
        let app = await makeLoadedApp(service: service, draft: "rapid")
        var tasks: [Task<Void, Never>] = []
        for phase in [
            AppScenePhase.inactive, .inactive, .active, .active,
            .background, .background, .active, .active,
        ] {
            tasks.append(app.scenePhaseDidChange(phase))
        }
        for task in tasks {
            await task.value
        }

        #expect(app.scenePhase == .active)
        #expect(!app.isPrivacyShieldVisible)
        #expect(app.conversation?.isSuspended == false)
        #expect(service.stopWatchCount == 1)
        #expect(service.startWatchCalls.count == 2)
        #expect(app.conversation?.eventSubscriptionCount == 1)
    }

    @Test("background shields and pauses before the draft persistence completes")
    func shieldPrecedesBackgroundWork() async {
        let service = StubSessionService()
        let app = await makeLoadedApp(service: service, draft: "pending text")

        let background = app.scenePhaseDidChange(.background)
        // Nothing has been awaited yet: the shield is up and the ordered work
        // has not started.
        #expect(app.isPrivacyShieldVisible)
        #expect(app.conversation?.isRenderingPaused == true)
        #expect(service.saveDraftCount == 0)
        await background.value
        #expect(service.saveDraftCount == 1)
    }

    @Test("inactive pauses the approval clock and active restarts it")
    func inactivePausesTimers() async {
        let service = StubSessionService()
        let model = await makeStartedConversation(service: service)
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 1,
                event: .approvals([testApproval()])
            )
        )
        await waitUntil { model.approvalExpiryTask != nil }

        model.pauseForInactivity()
        #expect(model.isRenderingPaused)
        #expect(model.approvalExpiryTask == nil)

        // A live update while paused must not restart the clock-driven timer.
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 2,
                event: .approvals([testApproval()])
            )
        )
        #expect(model.approvalExpiryTask == nil)

        model.resumeForInactivity()
        #expect(!model.isRenderingPaused)
        #expect(model.approvalExpiryTask != nil)
    }

    // MARK: - Stale completions

    @Test("a stale foreground completion cannot reveal content after a newer inactive")
    func staleForegroundCompletionIsIgnored() async {
        let service = StubSessionService()
        let app = await makeLoadedApp(service: service)
        let gate = SuspendedResponse<Void>()
        service.startWatchHandler = { _, _ in
            // The first watch is the launch one; gate the resume watch.
            if service.startWatchCalls.count > 1 {
                try await gate.wait()
            }
        }
        await app.enterBackground()
        let monitorStarts = service.startHostMonitorCalls.count

        let resume = app.scenePhaseDidChange(.active)
        await gate.waitUntilStarted()
        #expect(app.isPrivacyShieldVisible, "the shield stays up until the resume reconciles")

        // A newer inactive notification wins while the resume is in flight.
        let interrupt = app.scenePhaseDidChange(.inactive)
        #expect(app.isPrivacyShieldVisible)
        gate.resume(with: ())
        await resume.value
        await interrupt.value

        #expect(app.scenePhase == .inactive)
        #expect(app.isPrivacyShieldVisible, "a stale resume must not reveal the shielded scene")
        #expect(service.startHostMonitorCalls.count == monitorStarts)
        #expect(service.startWatchCalls.count == 2)
        #expect(app.conversation?.eventSubscriptionCount == 1)

        // Returning to active for real completes the pending resume once.
        await app.handleScenePhase(.active)
        #expect(!app.isPrivacyShieldVisible)
        #expect(app.conversation?.isSuspended == false)
        #expect(service.startWatchCalls.count == 2)
        #expect(service.stopWatchCount == 1)
    }

    @Test("a newer background during a resume suspends exactly once for that entry")
    func backgroundDuringResumeSuspendsAgainOnce() async {
        let service = StubSessionService()
        let app = await makeLoadedApp(service: service)
        let gate = SuspendedResponse<Void>()
        service.startWatchHandler = { _, _ in
            if service.startWatchCalls.count > 1 {
                try await gate.wait()
            }
        }
        await app.enterBackground()
        #expect(service.stopWatchCount == 1)

        let resume = app.scenePhaseDidChange(.active)
        await gate.waitUntilStarted()
        let backgroundAgain = app.scenePhaseDidChange(.background)
        #expect(app.isPrivacyShieldVisible)
        gate.resume(with: ())
        await resume.value
        await backgroundAgain.value

        #expect(app.scenePhase == .background)
        #expect(app.isPrivacyShieldVisible)
        #expect(app.conversation?.isSuspended == true)
        // One stop for each real background entry; the resume in between
        // started the watch once.
        #expect(service.stopWatchCount == 2)
        #expect(service.startWatchCalls.count == 2)
        #expect(app.conversation?.eventSubscriptionCount == 1)
    }

    // MARK: - Real failures

    @Test("a real transport failure on resume surfaces instead of staying shielded")
    func transportLossSurfaces() async {
        let service = StubSessionService()
        let app = await makeLoadedApp(service: service)
        service.startWatchHandler = { _, _ in
            if service.startWatchCalls.count > 1 {
                throw SessionServiceError(.timeout)
            }
        }
        await app.enterBackground()

        await app.handleScenePhase(.active)

        #expect(!app.isPrivacyShieldVisible)
        #expect(app.conversation?.watchEnded == true)
        #expect(app.conversation?.errorKey != nil)
        #expect(service.startWatchCalls.count == 2)
        #expect(service.stopWatchCount == 1)
    }

    @Test("a real Host disconnect during inactivity is still reported")
    func hostDisconnectDuringInactivitySurfaces() async {
        let service = StubSessionService()
        let app = await makeLoadedApp(service: service)
        await app.handleScenePhase(.inactive)

        service.emitHostConnection(hostId: "studio-mac", state: "offline")
        await waitUntil { !app.connectivity.isOnline("studio-mac") }

        await app.handleScenePhase(.active)
        #expect(!app.connectivity.isOnline("studio-mac"))
        #expect(app.conversation?.isRenderingPaused == false)
    }

    @Test("revocation while inactive never reopens work on the foreground return")
    func revocationWhileInactiveDoesNotResume() async {
        let service = StubSessionService()
        let app = await makeLoadedApp(service: service)
        let startWatches = service.startWatchCalls.count

        await app.handleScenePhase(.inactive)
        #expect(app.isPrivacyShieldVisible)
        service.emit(.error(code: .revoked, message: "revoked"), sequence: 2)
        await waitUntil { app.isRevoked }
        #expect(app.isPrivacyShieldVisible, "the inactive scene stays covered")

        await app.handleScenePhase(.active)

        #expect(!app.isPrivacyShieldVisible)
        #expect(app.isRevoked)
        #expect(app.conversation == nil)
        #expect(service.startWatchCalls.count == startWatches)
        #expect(app.rootState == .restoring)
    }

    @Test("revocation discovered by the resume blocks the surface without a refresh")
    func revocationDuringResumeDoesNotRefresh() async {
        let service = StubSessionService()
        let app = await makeLoadedApp(service: service)
        service.startWatchHandler = { _, _ in
            if service.startWatchCalls.count > 1 {
                throw SessionServiceError(.revoked)
            }
        }
        await app.enterBackground()
        let hostsFetches = service.hostsFetchCount

        await app.handleScenePhase(.active)

        #expect(app.isRevoked)
        #expect(app.conversation == nil)
        #expect(!app.isPrivacyShieldVisible)
        #expect(service.startWatchCalls.count == 2)
        #expect(service.hostsFetchCount == hostsFetches, "a revoked resume must not refresh the switcher")
    }
}