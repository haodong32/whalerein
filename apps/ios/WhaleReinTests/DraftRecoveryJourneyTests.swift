import Testing
@testable import WhaleRein

@MainActor
private final class ConfirmedFixtureSendService: StubSessionService, OperationRecoveryService {
    func listRetainedOperations() async throws -> [OperationReceipt] { [] }

    func recoverSend(operationId: String, follow: Bool) async throws -> OperationReceipt {
        OperationReceipt(
            operationId: operationId, kind: .send,
            scope: OperationScope(instanceId: instanceId, hostId: "studio-mac", sessionId: "s-001"),
            status: .confirmed,
            receipt: CommandReceipt(
                commandId: "fixture-command", requestId: operationId, status: .confirmed,
                delivery: .queue, messageId: "fixture-message"
            ),
            reply: nil, content: nil, readError: nil
        )
    }

    func readMessage(_ reference: MessageReference) async throws -> ConversationMessage {
        throw SessionServiceError(.unavailable)
    }
}

@Suite("Draft recovery journeys")
@MainActor
struct DraftRecoveryJourneyTests {
    @Test("closing and releasing the conversation keeps its unsaved draft")
    func immediateModelDisposalStillFlushesTheDraft() async throws {
        let service = StubSessionService()
        let drafts = DraftService(service: service)
        var outgoing: ConversationModel? = await makeStartedConversation(service: service, drafts: drafts)
        weak var disposed = outgoing
        outgoing?.draft = "Fixture unsaved edit"
        outgoing?.draftChanged()
        outgoing?.close()
        outgoing = nil
        try #require(disposed == nil)

        let reopened = await makeStartedConversation(service: service, drafts: drafts)
        #expect(reopened.draft == "Fixture unsaved edit")
        #expect(try await service.loadDraft(hostId: "studio-mac", sessionId: "s-001") == "Fixture unsaved edit")
    }

    @Test("a later confirmation associates an accepted prompt with its committed echo")
    func lateDeliveryCorrelationReconcilesAcceptedSend() async {
        let service = StubSessionService()
        service.sendHandler = { _, _, _, delivery in
            CommandReceipt(
                commandId: "fixture-command", requestId: "fixture-request", status: .accepted,
                delivery: delivery
            )
        }
        let model = await makeStartedConversation(service: service)
        model.draft = "Fixture prompt"
        await model.send()
        #expect(model.lastSendStatus == .accepted)

        let echo = ConversationMessage(
            messageId: "fixture-message", role: .user,
            blocks: [ConversationBlock(blockId: "fixture-text", kind: .text, text: "Fixture prompt")],
            createdAtUnixMs: 1, deliveryId: "fixture-delivery"
        )
        model.handle(SessionEventEnvelope(
            hostId: "studio-mac", sessionId: "s-001", sequence: 2, event: .message(echo)
        ))
        model.handle(SessionEventEnvelope(
            hostId: "studio-mac", sessionId: "s-001", sequence: 3,
            event: .commandStatus(CommandReceipt(
                commandId: "fixture-command", requestId: "fixture-request", status: .confirmed,
                delivery: .queue, deliveryId: "fixture-delivery"
            ))
        ))

        #expect(model.messages.map(\.messageId) == [echo.messageId])
        #expect(service.sendCalls.count == 1)
    }

    @Test("manual recovery clears only the confirmed original prompt", arguments: [false, true])
    func recoveredSendSettlesOnlyItsDraft(retyped: Bool) async throws {
        let service = ConfirmedFixtureSendService()
        service.sendHandler = { _, _, _, delivery in
            CommandReceipt(
                commandId: "fixture-command", requestId: "fixture-request", status: .uncertain,
                delivery: delivery
            )
        }
        let model = await makeStartedConversation(service: service)
        model.draft = "Fixture prompt"
        await model.send()
        #expect(model.lastSendStatus == .uncertain)
        if retyped {
            model.draft = ""
            model.draft = "Fixture prompt"
            await model.persistDraftNow()
        }
        await model.recoverSendResult()

        #expect(model.lastSendStatus == .confirmed)
        #expect(service.sendCalls.count == 1)
        let expectedDraft = retyped ? "Fixture prompt" : ""
        #expect(model.draft == expectedDraft)
        #expect(try await service.loadDraft(hostId: "studio-mac", sessionId: "s-001") == (retyped ? expectedDraft : nil))
    }
}
