// Composer send/stop behavior: ordinary Send uses the native queue, explicit
// Steer and start-if-idle are honored, unsupported modes keep the draft, and
// uncertain results reconcile exactly once.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Conversation commands")
@MainActor
struct ConversationCommandTests {
    @Test("idle send starts work, clears the persisted draft and defaults to Queue")
    func idleSend() async throws {
        let service = try await makePairedFixture()
        let model = await makeStartedConversation(service: service)
        model.draft = "ship the release"
        model.draftChanged()
        await pump(150)

        await model.send()

        #expect(model.lastSendStatus == .accepted)
        #expect(!model.messages.contains { $0.role == .user && $0.plainText == "ship the release" })
        #expect(model.draft.isEmpty)
        #expect(model.isRunning)
        #expect(model.delivery == .queue)
        let persisted = try await service.loadDraft(hostId: "studio-mac", sessionId: "s-001")
        #expect(persisted == nil)
    }

    @Test("running send uses the selected Steer and Queue modes")
    func runningModes() async throws {
        let service = try await makePairedFixture()
        let model = await makeStartedConversation(service: service)
        model.draft = "first"
        await model.send()
        #expect(model.delivery == .queue)

        model.draft = "second"
        model.selectDelivery(.steer)
        await model.send()
        #expect(model.lastSendStatus == .accepted)
        #expect(model.delivery == .steer)

        model.draft = "third"
        model.selectDelivery(.queue)
        await model.send()
        #expect(model.lastSendStatus == .accepted)
    }

    @Test("an unsupported mode keeps the draft and reports a localized error")
    func unsupportedModeKeepsDraft() async throws {
        let service = StubSessionService()
        service.capabilitiesHandler = { _, _ in
            SessionCapabilities(running: false, supportsSteer: false, supportsQueue: false)
        }
        let model = await makeStartedConversation(service: service)
        model.draft = "keep me"
        model.delivery = .queue

        await model.send()

        #expect(service.sendCalls.isEmpty)
        #expect(model.draft == "keep me")
        #expect(model.sendErrorKey == .conversationDeliveryUnsupported)
    }

    @Test("a send error keeps the draft available")
    func sendFailureKeepsDraft() async throws {
        let service = StubSessionService()
        service.sendHandler = { _, _, _, _ in throw SessionServiceError(.timeout) }
        let model = await makeStartedConversation(service: service)
        model.draft = "retry me"

        await model.send()

        #expect(model.draft == "retry me")
        #expect(model.sendErrorKey == .errorTimeout)
        #expect(model.lastSendStatus == nil)
    }

    @Test("a rejected send keeps the draft intact and retryable")
    func rejectedSendKeepsDraftForRetry() async throws {
        let service = StubSessionService()
        let rejected = CommandReceipt(
            commandId: "cmd-reject",
            requestId: "req-reject",
            status: .rejected,
            delivery: .send
        )
        service.sendHandler = { _, _, _, _ in rejected }
        let model = await makeStartedConversation(service: service)
        model.draft = "keep me"
        await model.persistDraftNow()

        await model.send()

        #expect(model.lastSendStatus == .rejected)
        #expect(model.pendingSubmission == nil)
        #expect(model.draft == "keep me")
        #expect(model.canSend)
        #expect(model.messages.isEmpty, "A rejected send must not add an optimistic row")
        let persisted = try await service.loadDraft(hostId: "studio-mac", sessionId: "s-001")
        #expect(persisted == "keep me")

        // The same text can be retried and only succeeds once.
        service.sendHandler = { _, _, _, delivery in
            CommandReceipt(
                commandId: "cmd-retry",
                requestId: "req-retry",
                status: .accepted,
                delivery: delivery,
                messageId: "msg-retry"
            )
        }
        await model.send()
        #expect(service.sendCalls.count == 2)
        #expect(service.sendCalls[1].2 == "keep me")
        #expect(model.lastSendStatus == .accepted)
        #expect(model.pendingSubmission == nil)
    }

    @Test("an uncertain send keeps the draft until the original request settles")
    func uncertainSendReconciliation() async throws {
        let service = StubSessionService()
        let receipt = CommandReceipt(commandId: "cmd-u1", requestId: "req-u1", status: .uncertain, delivery: .send)
        service.sendHandler = { _, _, _, _ in receipt }
        let model = await makeStartedConversation(service: service)
        model.draft = "uncertain text"

        await model.send()
        #expect(model.lastSendStatus == .uncertain)
        #expect(model.inFlightSend?.commandId == "cmd-u1")
        #expect(model.draft == "uncertain text")

        var settled = receipt
        settled.status = .confirmed
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 50,
                event: .commandStatus(settled)
            )
        )
        #expect(model.lastSendStatus == .confirmed)
        #expect(model.inFlightSend == nil)
        #expect(model.draft.isEmpty)

        let messageCount = model.messages.count
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 51,
                event: .commandStatus(settled)
            )
        )
        #expect(model.messages.count == messageCount)
        #expect(model.lastSendStatus == .confirmed)
    }

    @Test("stop stays pending until the Host settles it")
    func stopSettlement() async throws {
        let service = StubSessionService()
        let stopReceipt = CommandReceipt(commandId: "cmd-s1", requestId: "req-s1", status: .uncertain, delivery: nil)
        service.capabilitiesHandler = { _, _ in .running }
        service.stopHandler = { _, _ in stopReceipt }
        let model = await makeStartedConversation(service: service)

        await model.stopRun()
        #expect(model.lastStopStatus == .uncertain)
        #expect(model.pendingStop?.commandId == "cmd-s1")
        #expect(!model.stopSettled)
        #expect(model.stopStatusKey == .conversationStopPending)

        var settled = stopReceipt
        settled.status = .confirmed
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 60,
                event: .commandStatus(settled)
            )
        )
        #expect(model.stopSettled)
        #expect(model.lastStopStatus == .confirmed)
        #expect(!model.isRunning)
        #expect(model.stopStatusKey == .conversationStopConfirmed)
    }

    @Test("an unknown command settlement is ignored")
    func unknownSettlementIgnored() async throws {
        let service = StubSessionService()
        service.capabilitiesHandler = { _, _ in .running }
        let model = await makeStartedConversation(service: service)

        let unknown = CommandReceipt(commandId: "cmd-other", requestId: "req-other", status: .confirmed, delivery: nil)
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 70,
                event: .commandStatus(unknown)
            )
        )
        #expect(model.lastSendStatus == nil)
        #expect(model.lastStopStatus == nil)
        #expect(model.isRunning)
    }

    @Test("a settled attempt ends the running state and clears the preview")
    func attemptEnded() async throws {
        let service = StubSessionService()
        let model = await makeStartedConversation(service: service)
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 80,
                event: .preview(attemptId: "attempt-1", text: "streaming", replace: true, incomplete: true)
            )
        )
        #expect(model.livePreview == "streaming")
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 81,
                event: .attemptEnded(attemptId: "attempt-1", outcome: .completed)
            )
        )
        #expect(model.livePreview == nil)
        #expect(!model.isRunning)
    }

    // MARK: - Stop settlement ordering (G03)

    private func envelope(_ event: MessageEvent, _ sequence: Int) -> SessionEventEnvelope {
        SessionEventEnvelope(hostId: "studio-mac", sessionId: "s-001", sequence: sequence, event: event)
    }

    @Test("a confirmed stop event delivered before the receipt settles the stop exactly once")
    func stopEventBeforeReceiptSettles() async {
        let service = StubSessionService()
        service.capabilitiesHandler = { _, _ in .running }
        let gate = SuspendedResponse<CommandReceipt>()
        service.stopHandler = { _, _ in try await gate.wait() }
        let model = await makeStartedConversation(service: service)
        model.handle(envelope(.preview(attemptId: "attempt-1", text: "working", replace: true, incomplete: true), 5))

        let stopTask = Task { await model.stopRun() }
        await gate.waitUntilStarted()
        #expect(service.stopCalls.count == 1)
        #expect(model.pendingStop == nil)

        let receipt = CommandReceipt(
            commandId: "cmd-stop-early",
            requestId: "req-stop-early",
            status: .confirmed,
            delivery: nil
        )
        // Core queues commandStatus before returning the command view.
        model.handle(envelope(.commandStatus(receipt), 6))
        gate.resume(with: receipt)
        await stopTask.value

        #expect(model.stopSettled)
        #expect(model.lastStopStatus == .confirmed)
        #expect(model.stopStatusKey == .conversationStopConfirmed)
        #expect(model.pendingStop == nil)
        #expect(!model.isRunning)
        #expect(model.livePreview == nil)
        #expect(service.stopCalls.count == 1)

        // The next authoritative run re-enables Stop, and a fresh Stop reaches
        // the service instead of being blocked by stale pending admission.
        model.handle(envelope(.capabilities(.running), 7))
        #expect(model.isRunning)
        #expect(model.canStop)
        let second = CommandReceipt(
            commandId: "cmd-stop-second",
            requestId: "req-stop-second",
            status: .confirmed,
            delivery: nil
        )
        service.stopHandler = { _, _ in second }
        await model.stopRun()
        #expect(service.stopCalls.count == 2)
        #expect(model.lastStopStatus == .confirmed)
        #expect(model.pendingStop == nil)
    }

    @Test("an old stop confirmation does not clear a newer authoritative run")
    func lateStopConfirmationPreservesNewerRun() async {
        let service = StubSessionService()
        service.capabilitiesHandler = { _, _ in .running }
        let gate = SuspendedResponse<CommandReceipt>()
        service.stopHandler = { _, _ in try await gate.wait() }
        let model = await makeStartedConversation(service: service)
        model.handle(envelope(.preview(attemptId: "attempt-old", text: "old run", replace: true, incomplete: true), 5))

        let stopTask = Task { await model.stopRun() }
        await gate.waitUntilStarted()

        // A newer run becomes authoritative while the old Stop response is
        // still in flight.
        model.handle(envelope(.capabilities(.running), 6))
        model.handle(envelope(.preview(attemptId: "attempt-new", text: "new run", replace: true, incomplete: true), 7))
        #expect(model.isRunning)

        let receipt = CommandReceipt(
            commandId: "cmd-stop-old",
            requestId: "req-stop-old",
            status: .confirmed,
            delivery: nil
        )
        gate.resume(with: receipt)
        await stopTask.value

        #expect(model.stopSettled)
        #expect(model.lastStopStatus == .confirmed)
        #expect(model.pendingStop == nil)
        #expect(model.isRunning, "The older Stop must not stop the newer run")
        #expect(model.livePreview == "new run")
        #expect(model.canStop)

        let second = CommandReceipt(
            commandId: "cmd-stop-new",
            requestId: "req-stop-new",
            status: .confirmed,
            delivery: nil
        )
        service.stopHandler = { _, _ in second }
        await model.stopRun()
        #expect(service.stopCalls.count == 2)
        #expect(!model.isRunning)
    }

    @Test("a confirmed stop receipt settles without waiting for a duplicate event")
    func confirmedStopReceiptWithoutDuplicateEvent() async {
        let service = StubSessionService()
        service.capabilitiesHandler = { _, _ in .running }
        let receipt = CommandReceipt(
            commandId: "cmd-stop-immediate",
            requestId: "req-stop-immediate",
            status: .confirmed,
            delivery: nil
        )
        service.stopHandler = { _, _ in receipt }
        let model = await makeStartedConversation(service: service)
        model.handle(envelope(.preview(attemptId: "attempt-1", text: "working", replace: true, incomplete: true), 5))

        await model.stopRun()

        #expect(model.stopSettled)
        #expect(model.lastStopStatus == .confirmed)
        #expect(model.pendingStop == nil)
        #expect(!model.isRunning)
        #expect(model.livePreview == nil)

        // A duplicate confirmation is idempotent and cannot double-apply.
        model.handle(envelope(.commandStatus(receipt), 6))
        #expect(model.stopSettled)
        #expect(model.lastStopStatus == .confirmed)
        #expect(model.pendingStop == nil)
        #expect(service.stopCalls.count == 1)
    }

    // MARK: - Pending submission coalescing (Q04)

    @Test("the suspension barrier rejects a duplicate wait instead of deadlocking")
    func suspensionBarrierRejectsDuplicateWait() async throws {
        let gate = SuspendedResponse<Int>()
        let first = Task { try await gate.wait() }
        await gate.waitUntilStarted()

        do {
            _ = try await gate.wait()
            Issue.record("A second wait must throw instead of overwriting the first continuation")
        } catch is SuspendedResponse<Int>.AlreadyWaiting {
            // Expected: the test can fail cleanly instead of hanging.
        }

        gate.resume(with: 7)
        #expect(try await first.value == 7)
    }

    @Test("a repeated Send while the first submission is pending issues one command")
    func repeatedSendWhilePendingIsCoalesced() async {
        let service = StubSessionService()
        let gate = SuspendedResponse<CommandReceipt>()
        service.sendHandler = { _, _, _, _ in try await gate.wait() }
        let model = await makeStartedConversation(service: service)
        model.draft = "one intent"

        let first = Task { await model.send() }
        await gate.waitUntilStarted()
        #expect(model.pendingSubmission == .send)
        #expect(!model.canSend)
        #expect(service.sendCalls.count == 1)
        #expect(model.draft == "one intent")

        // A second tap in the submission window is coalesced even though the
        // composer text changed, so it issues no command.
        model.draft = "second thought"
        await model.send()
        #expect(service.sendCalls.count == 1)
        #expect(model.pendingSubmission == .send)

        gate.resume(with: CommandReceipt(
            commandId: "cmd-q04-send",
            requestId: "req-q04-send",
            status: .accepted,
            delivery: .send,
            messageId: "msg-q04-send"
        ))
        await first.value

        #expect(service.sendCalls.count == 1)
        #expect(model.pendingSubmission == nil)
        #expect(model.lastSendStatus == .accepted)
        // Only the submitted text was consumed; the newer draft survives.
        #expect(model.draft == "second thought")
        #expect(model.canSend)
    }

    @Test("a repeated Stop while the first submission is pending issues one command")
    func repeatedStopWhilePendingIsCoalesced() async {
        let service = StubSessionService()
        service.capabilitiesHandler = { _, _ in .running }
        let gate = SuspendedResponse<CommandReceipt>()
        service.stopHandler = { _, _ in try await gate.wait() }
        let model = await makeStartedConversation(service: service)
        model.handle(envelope(.preview(attemptId: "attempt-q04", text: "working", replace: true, incomplete: true), 5))

        let first = Task { await model.stopRun() }
        await gate.waitUntilStarted()
        #expect(model.pendingSubmission == .stop)
        #expect(!model.canStop)

        await model.stopRun()
        #expect(service.stopCalls.count == 1)
        #expect(model.pendingSubmission == .stop)

        let receipt = CommandReceipt(
            commandId: "cmd-q04-stop",
            requestId: "req-q04-stop",
            status: .uncertain,
            delivery: nil
        )
        gate.resume(with: receipt)
        await first.value

        #expect(service.stopCalls.count == 1)
        #expect(model.pendingSubmission == nil)
        #expect(model.pendingStop?.commandId == "cmd-q04-stop")
        #expect(!model.canStop)
        #expect(!model.stopSettled)

        var settled = receipt
        settled.status = .confirmed
        model.handle(envelope(.commandStatus(settled), 6))
        #expect(model.stopSettled)
        #expect(!model.isRunning)
    }

    @Test("a definite Send failure releases the gate and a later tap sends once")
    func sendFailureReenablesRetry() async {
        let service = StubSessionService()
        service.sendHandler = { _, _, _, _ in throw SessionServiceError(.timeout) }
        let model = await makeStartedConversation(service: service)
        model.draft = "retry me"

        await model.send()
        #expect(service.sendCalls.count == 1)
        #expect(model.pendingSubmission == nil)
        #expect(model.sendErrorKey == .errorTimeout)
        #expect(model.draft == "retry me")
        #expect(model.canSend)

        service.sendHandler = { _, _, _, delivery in
            CommandReceipt(
                commandId: "cmd-retry",
                requestId: "req-retry",
                status: .accepted,
                delivery: delivery
            )
        }
        await model.send()
        #expect(service.sendCalls.count == 2)
        #expect(service.sendCalls[1].2 == "retry me")
        #expect(model.lastSendStatus == .accepted)
        #expect(model.pendingSubmission == nil)
    }

    @Test("a definite Stop failure allows retry and its late settlement cannot act on the new intent")
    func stopFailureRetryIgnoresLateSettlement() async throws {
        let service = StubSessionService()
        service.capabilitiesHandler = { _, _ in .running }
        let firstGate = SuspendedResponse<CommandReceipt>()
        let retryGate = SuspendedResponse<CommandReceipt>()
        var attempts = 0
        service.stopHandler = { _, _ in
            attempts += 1
            if attempts == 1 {
                _ = try await firstGate.wait()
                throw SessionServiceError(.timeout)
            }
            return try await retryGate.wait()
        }
        let model = await makeStartedConversation(service: service)

        let first = Task { await model.stopRun() }
        await firstGate.waitUntilStarted()
        #expect(model.pendingSubmission == .stop)

        // Core can queue the command status for the first Stop before the
        // failing method call returns.
        let firstReceipt = CommandReceipt(
            commandId: "cmd-q04-first",
            requestId: "req-q04-first",
            status: .confirmed,
            delivery: nil
        )
        model.handle(envelope(.commandStatus(firstReceipt), 5))

        firstGate.resume(with: firstReceipt)
        await first.value

        // A definite failure clears the intent and allows a retry.
        #expect(model.pendingSubmission == nil)
        #expect(model.sendErrorKey == .errorTimeout)
        #expect(model.canStop)

        let retry = Task { await model.stopRun() }
        await retryGate.waitUntilStarted()
        #expect(service.stopCalls.count == 2)
        #expect(model.pendingSubmission == .stop)
        #expect(!model.canStop)

        // A late settlement of the failed first command must not settle or
        // clear the new submission.
        model.handle(envelope(.commandStatus(firstReceipt), 6))
        #expect(model.pendingSubmission == .stop)
        #expect(model.pendingStop == nil)
        #expect(model.lastStopStatus == nil)
        #expect(!model.stopSettled)

        let retryReceipt = CommandReceipt(
            commandId: "cmd-q04-retry",
            requestId: "req-q04-retry",
            status: .uncertain,
            delivery: nil
        )
        retryGate.resume(with: retryReceipt)
        await retry.value

        #expect(model.pendingSubmission == nil)
        #expect(model.pendingStop?.commandId == "cmd-q04-retry")
        #expect(model.lastStopStatus == .uncertain)
        #expect(!model.canStop)
    }
}
