import Testing
@testable import WhaleRein

@MainActor
private final class GatedDraftClearStorage: StubSessionService {
    let clearGate = SuspendedResponse<Void>()

    override func clearDraft(hostId: String, sessionId: String) async throws {
        try await clearGate.wait()
        try await super.clearDraft(hostId: hostId, sessionId: sessionId)
    }
}

private extension DraftService {
    func saveAfterSignalingStart(
        _ lease: Lease, text: String, started: AsyncStream<Void>.Continuation
    ) async throws {
        started.yield(())
        // This call runs on the same actor and assigns the new draft before
        // suspending to wait for the in-flight clear.
        try await setAndFlush(lease, text: text)
    }
}

@Suite("Send and draft interleavings")
@MainActor
struct SendDraftInterleavingTests {
    @Test("a draft typed while clearing a sent prompt survives reopening")
    func editDuringStorageClearSurvivesReopening() async throws {
        let service = GatedDraftClearStorage()
        let drafts = DraftService(service: service)
        let claim = try await drafts.claim(hostId: "studio-mac", sessionId: "s-001")
        try await drafts.setAndFlush(claim.lease, text: "Fixture submitted draft")

        let clear = Task {
            try await drafts.clearIfCurrent(claim.lease, matching: "Fixture submitted draft")
        }
        await service.clearGate.waitUntilStarted()
        let (started, signal) = AsyncStream<Void>.makeStream()
        let edit = Task {
            try await drafts.saveAfterSignalingStart(
                claim.lease, text: "Fixture next draft", started: signal
            )
        }
        var starts = started.makeAsyncIterator()
        await starts.next()
        service.clearGate.resume(with: ())
        try await clear.value
        try await edit.value

        let reopened = try await drafts.claim(hostId: "studio-mac", sessionId: "s-001")
        #expect(reopened.text == "Fixture next draft")
    }

    @Test("an accepted send clears its restored draft after a quick round trip")
    func sendSettlesAfterReopeningSameConversation() async throws {
        let service = StubSessionService()
        let drafts = DraftService(service: service)
        let sendGate = SuspendedResponse<CommandReceipt>()
        service.sendHandler = { _, _, _, _ in try await sendGate.wait() }
        let original = await makeStartedConversation(service: service, drafts: drafts)
        original.draft = "Fixture submitted draft"
        let send = Task { await original.send() }
        await sendGate.waitUntilStarted()
        original.close()
        let other = await makeStartedConversation(service: service, sessionId: "s-002", drafts: drafts)
        other.close()
        let reopened = await makeStartedConversation(service: service, drafts: drafts)

        sendGate.resume(with: CommandReceipt(
            commandId: "command-1", requestId: "request-1", status: .accepted,
            delivery: .queue, messageId: "committed-1"
        ))
        await send.value

        #expect(reopened.draft.isEmpty)
        #expect(try await service.loadDraft(hostId: "studio-mac", sessionId: "s-001") == nil)
    }

    @Test("an identical draft retyped on reopening is a new edit")
    func sameTextNewEditSurvivesOldAcceptance() async throws {
        let service = StubSessionService()
        let drafts = DraftService(service: service)
        let sendGate = SuspendedResponse<CommandReceipt>()
        service.sendHandler = { _, _, _, _ in try await sendGate.wait() }
        let original = await makeStartedConversation(service: service, drafts: drafts)
        original.draft = "Fixture prompt"
        let send = Task { await original.send() }
        await sendGate.waitUntilStarted()
        original.close()
        let reopened = await makeStartedConversation(service: service, drafts: drafts)
        #expect(reopened.draft == "Fixture prompt")
        reopened.draft = ""
        reopened.draft = "Fixture prompt"
        reopened.draftChanged()

        sendGate.resume(with: CommandReceipt(
            commandId: "command-1", requestId: "request-1", status: .accepted, delivery: .queue
        ))
        await send.value
        #expect(reopened.draft == "Fixture prompt")
        await reopened.persistDraftNow()
        #expect(try await service.loadDraft(hostId: "studio-mac", sessionId: "s-001") == "Fixture prompt")
    }

    @Test("a send receipt does not mark a completed turn as running again")
    func completedTurnBeforeSendReceiptStaysIdle() async {
        let service = StubSessionService()
        let sendGate = SuspendedResponse<CommandReceipt>()
        service.sendHandler = { _, _, _, _ in try await sendGate.wait() }
        let model = await makeStartedConversation(service: service)
        model.draft = "Fixture prompt"

        let send = Task { await model.send() }
        await sendGate.waitUntilStarted()
        model.handle(SessionEventEnvelope(
            hostId: "studio-mac", sessionId: "s-001", sequence: 2,
            event: .attemptEnded(attemptId: "fixture-attempt", outcome: .completed)
        ))
        sendGate.resume(with: CommandReceipt(
            commandId: "command-1", requestId: "request-1", status: .accepted, delivery: .queue
        ))
        await send.value

        #expect(model.lastSendStatus == .accepted)
        #expect(!model.isRunning)
    }
}
