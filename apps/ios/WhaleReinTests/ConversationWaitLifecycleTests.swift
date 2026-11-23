// Wait start/cancel lifecycle through the conversation model. A wait is an
// observation: cancelling one attempt must release exactly that native wait
// while the conversation watch and running Host work stay active. Results from
// an attempt that was reset or replaced must never settle a newer wait.

import Foundation
import Testing
@testable import WhaleRein

private func waitResult(
    waitID: String,
    outcome: WaitOutcome,
    errorCode: String? = nil
) -> WaitResult {
    WaitResult(
        waitID: waitID,
        hostID: "studio-mac",
        sessionID: "s-001",
        outcome: outcome,
        state: WaitSnapshot(active: false, running: true, queueAvailable: true, pendingInput: []),
        errorCode: errorCode
    )
}

@Suite("Conversation wait lifecycle")
@MainActor
struct ConversationWaitLifecycleTests {
    @Test("cancelling a wait leaves the watch and running work active")
    func cancelLeavesObservationRunning() async {
        let service = StubSessionService()
        service.capabilitiesHandler = { _, _ in .running }
        let gate = SuspendedResponse<WaitResult?>()
        service.takeWaitHandler = { _, _ in try await gate.wait() }
        let model = await makeStartedConversation(service: service)
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 1,
                event: .reset(messages: [], approvals: [])
            )
        )
        let streamCount = service.eventStreamCount
        let startWatchCount = service.startWatchCalls.count
        #expect(model.isRunning)

        let start = Task { await model.wait.start(timeoutMs: 5_000) }
        await gate.waitUntilStarted()
        await start.value
        #expect(model.wait.isWaiting)
        #expect(model.wait.waitID == "wait-1")

        await model.wait.cancel()
        #expect(service.cancelWaitCalls == ["wait-1"])

        gate.resume(with: waitResult(waitID: "wait-1", outcome: .cancelled))
        await waitUntil { model.wait.phase == .settled }

        #expect(model.wait.result?.outcome == .cancelled)
        #expect(model.isRunning, "cancelling a wait must not stop running work")
        #expect(!model.watchEnded)
        #expect(service.stopWatchCount == 0)
        #expect(service.eventStreamCount == streamCount)
        #expect(service.startWatchCalls.count == startWatchCount)
    }

    @Test("cancellation uses the exact started native wait identity")
    func cancelUsesStartedIdentity() async {
        let service = StubSessionService()
        var nextID = 40
        service.beginWaitHandler = { _, _, _ in
            nextID += 1
            return "native-wait-\(nextID)"
        }
        let firstGate = SuspendedResponse<WaitResult?>()
        var takeCount = 0
        service.takeWaitHandler = { waitID, _ in
            takeCount += 1
            if waitID == "native-wait-41" {
                return try await firstGate.wait()
            }
            return waitResult(waitID: waitID, outcome: .idle)
        }
        let model = await makeStartedConversation(service: service)

        let firstStart = Task { await model.wait.start(timeoutMs: 1_000) }
        await firstGate.waitUntilStarted()
        await firstStart.value
        #expect(model.wait.waitID == "native-wait-41")
        await model.wait.cancel()
        #expect(service.cancelWaitCalls == ["native-wait-41"])

        firstGate.resume(with: waitResult(waitID: "native-wait-41", outcome: .cancelled))
        await waitUntil { model.wait.phase == .settled }

        await model.wait.start(timeoutMs: 1_000)
        #expect(model.wait.waitID == "native-wait-42")
        await model.wait.cancel()
        #expect(service.cancelWaitCalls == ["native-wait-41", "native-wait-42"])
        await waitUntil { model.wait.phase == .settled }
        #expect(takeCount == 2)
    }

    @Test("a late result from a reset attempt cannot settle a newer wait")
    func lateResetResultCannotSettleNewWait() async {
        let service = StubSessionService()
        let firstGate = SuspendedResponse<WaitResult?>()
        let secondGate = SuspendedResponse<WaitResult?>()
        service.takeWaitHandler = { waitID, _ in
            if waitID == "wait-1" {
                return try await firstGate.wait()
            }
            return try await secondGate.wait()
        }
        let model = await makeStartedConversation(service: service)

        await model.wait.start(timeoutMs: 1_000)
        #expect(model.wait.waitID == "wait-1")
        model.wait.reset()
        await model.wait.start(timeoutMs: 1_000)
        #expect(model.wait.waitID == "wait-2")

        // The old attempt settles after a new attempt has already started.
        firstGate.resume(with: waitResult(waitID: "wait-1", outcome: .idle))
        await pump(50)

        #expect(model.wait.waitID == "wait-2")
        #expect(model.wait.isWaiting)
        #expect(model.wait.result == nil)

        secondGate.resume(with: waitResult(waitID: "wait-2", outcome: .idle))
        await waitUntil { model.wait.phase == .settled }
        #expect(model.wait.result?.waitID == "wait-2")
        #expect(model.wait.result?.outcome == .idle)
    }

    @Test("a start that finishes after reset releases the exact native wait")
    func lateStartAfterResetIsReleased() async {
        let service = StubSessionService()
        let gate = SuspendedResponse<String>()
        service.beginWaitHandler = { _, _, _ in try await gate.wait() }
        let model = await makeStartedConversation(service: service)

        let start = Task { await model.wait.start(timeoutMs: 1_000) }
        await gate.waitUntilStarted()
        model.wait.reset()
        gate.resume(with: "native-wait-late")
        await start.value

        #expect(model.wait.phase == .idle)
        #expect(model.wait.waitID == nil)
        #expect(service.cancelWaitCalls == ["native-wait-late"])
        #expect(service.takeWaitCalls.isEmpty)
    }

    @Test("closing the conversation releases only its wait attempt")
    func closeReleasesExactWait() async {
        let service = StubSessionService()
        let gate = SuspendedResponse<WaitResult?>()
        service.takeWaitHandler = { _, _ in try await gate.wait() }
        let model = await makeStartedConversation(service: service)

        await model.wait.start(timeoutMs: 1_000)
        model.close()

        await waitUntil { service.cancelWaitCalls == ["wait-1"] }
        #expect(service.cancelWaitCalls == ["wait-1"])
        #expect(model.wait.phase == .idle)
        #expect(service.stopWatchCount == 1)
    }
}
