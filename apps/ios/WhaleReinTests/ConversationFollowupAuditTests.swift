import Testing
@testable import WhaleRein

@MainActor
private final class DelayedDraftRestoreService: StubSessionService {
    let restore = SuspendedResponse<String?>()

    override func loadDraft(hostId: String, sessionId: String) async throws -> String? {
        try await restore.wait()
    }
}

@Suite("Conversation follow-up journeys")
@MainActor
struct ConversationFollowupAuditTests {
    private func event(_ payload: MessageEvent, sequence: Int) -> SessionEventEnvelope {
        SessionEventEnvelope(hostId: "studio-mac", sessionId: "s-001", sequence: sequence, event: payload)
    }

    private func receipt() -> CommandReceipt {
        CommandReceipt(
            commandId: "fixture-command", requestId: "fixture-request", status: .accepted,
            delivery: .queue, deliveryId: "fixture-delivery"
        )
    }

    @Test("an exact read correlates an outgoing bubble with its committed user message")
    func exactHistoryReadReconciles() async {
        let service = HistoryStubService()
        service.sendHandler = { _, _, _, _ in receipt() }
        let model = await makeStartedConversation(service: service)
        model.draft = "Fixture prompt"
        await model.send()
        let committed = ConversationMessage(
            messageId: "fixture-committed", role: .user,
            blocks: [ConversationBlock(blockId: "body", kind: .text, text: "Fixture prompt")],
            createdAtUnixMs: 1, deliveryId: "fixture-delivery"
        )
        service.exactHandler = { _ in committed }
        await model.rereadMessage(MessageReference(
            hostId: "studio-mac", sessionId: "s-001", messageId: committed.messageId,
            sourceLocator: "fixture-locator", projectionId: "fixture-view"
        ))
        #expect(model.messages.map(\.messageId) == [committed.messageId])
    }

    @Test("late delivery metadata on an already visible committed message removes the duplicate",
          arguments: [false, true])
    func replayWithDeliveryIdentityReconciles(replacement: Bool) async {
        let service = StubSessionService()
        service.sendHandler = { _, _, _, _ in receipt() }
        let model = await makeStartedConversation(service: service)
        model.draft = "Fixture prompt"
        await model.send()
        var committed = ConversationMessage(
            messageId: "fixture-committed", role: .user,
            blocks: [ConversationBlock(blockId: "body", kind: .text, text: "Fixture prompt")],
            createdAtUnixMs: 1
        )
        model.handle(event(.message(committed), sequence: 2))
        committed.deliveryId = "fixture-delivery"
        model.handle(event(replacement ? .replace(message: committed) : .message(committed), sequence: 3))
        #expect(model.messages.map(\.messageId) == [committed.messageId])
    }

    @Test("a send accepted before draft restoration finishes leaves the composer empty")
    func sendWhileDraftIsRestoringClearsSubmittedText() async {
        let service = DelayedDraftRestoreService()
        service.sendHandler = { _, _, _, _ in receipt() }
        let model = ConversationModel(
            hostId: "studio-mac", sessionId: "s-001", sessionTitle: "Fixture",
            service: service, connectivity: makeTestConnectivity()
        )
        let opening = Task { await model.start() }
        await service.restore.waitUntilStarted()
        model.handle(event(.historySnapshot(ConversationHistoryPage(
            source: .online, projection: "fixture-view", complete: true,
            messages: [], activities: [], continuation: nil
        ), replacing: true), sequence: 1))
        model.draft = "Fixture prompt"
        #expect(model.canSend)
        await model.send()
        #expect(model.lastSendStatus == .accepted)
        #expect(model.draft.isEmpty)
        service.restore.resume(with: "Older fixture draft")
        await opening.value
        #expect(model.draft.isEmpty)
    }
}
