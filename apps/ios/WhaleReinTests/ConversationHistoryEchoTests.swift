import Testing
@testable import WhaleRein

@Suite("Outgoing echoes discovered through history reads")
@MainActor
struct ConversationHistoryEchoTests {
    @Test("an older page merges the accepted prompt with its committed echo")
    func olderPageReconcilesAcceptedPrompt() async throws {
        let service = HistoryStubService()
        service.sendHandler = { _, _, _, _ in
            CommandReceipt(
                commandId: "command-1", requestId: "request-1", status: .accepted,
                delivery: .queue, messageId: "request-1", deliveryId: "delivery-1"
            )
        }
        let model = await makeStartedConversation(service: service)
        model.draft = "Ask about the fixture"
        await model.send()
        let pending = try #require(model.messages.first)

        var echo = ConversationMessage(
            messageId: "committed-prompt", role: .user,
            blocks: pending.blocks, createdAtUnixMs: pending.createdAtUnixMs
        )
        echo.deliveryId = "delivery-1"
        let reply = ConversationMessage(
            messageId: "committed-reply", role: .assistant,
            blocks: [ConversationBlock(blockId: "reply", kind: .text, text: "A fixture answer")],
            createdAtUnixMs: pending.createdAtUnixMs + 1
        )
        model.handle(SessionEventEnvelope(
            hostId: "studio-mac", sessionId: "s-001", sequence: 2,
            event: .historySnapshot(
                ConversationHistoryPage(
                    source: .online, projection: "projection-1", complete: false,
                    messages: [reply], activities: [], continuation: "older-page"
                ),
                replacing: true
            )
        ))
        #expect(model.messages.map(\.messageId) == [pending.messageId, reply.messageId])

        // The replacement baseline starts at the reply. The authoritative
        // outgoing row becomes available through the older continuation.
        service.historyPageHandler = { _ in
            ConversationHistoryPage(
                source: .online, projection: "projection-1", complete: true,
                messages: [echo], activities: [], continuation: nil
            )
        }
        await model.loadOlderHistory()

        #expect(model.messages.map(\.messageId) == [echo.messageId, reply.messageId])
        #expect(model.presentationID(for: echo.messageId) == pending.messageId)
    }

    @Test("a replacement does not reorder an accepted prompt around a known reply when clocks differ")
    func replacementPreservesPreviouslyObservedPromptOrder() async throws {
        let service = HistoryStubService()
        service.sendHandler = { _, _, _, _ in
            CommandReceipt(
                commandId: "command-1", requestId: "request-1", status: .accepted,
                delivery: .queue, messageId: "request-1", deliveryId: "delivery-1"
            )
        }
        let model = await makeStartedConversation(service: service)
        model.draft = "Ask about the fixture"
        await model.send()
        let pending = try #require(model.messages.first)
        let reply = ConversationMessage(
            messageId: "committed-reply", role: .assistant,
            blocks: [ConversationBlock(blockId: "reply", kind: .text, text: "A fixture answer")],
            createdAtUnixMs: pending.createdAtUnixMs - 5_000
        )
        model.handle(SessionEventEnvelope(
            hostId: "studio-mac", sessionId: "s-001", sequence: 2, event: .message(reply)
        ))
        #expect(model.messages.map(\.messageId) == [pending.messageId, reply.messageId])

        // The same committed row survives replacement. Its timestamp uses the
        // Host clock, so the already presented order is stronger evidence.
        model.handle(SessionEventEnvelope(
            hostId: "studio-mac", sessionId: "s-001", sequence: 3,
            event: .historySnapshot(
                ConversationHistoryPage(
                    source: .online, projection: "projection-1", complete: true,
                    messages: [reply], activities: [], continuation: nil
                ),
                replacing: true
            )
        ))

        #expect(model.messages.map(\.messageId) == [pending.messageId, reply.messageId])
    }

    @Test("a queued prompt is committed after the previous turn even when a snapshot replaced the transcript")
    func queuedEchoFollowsThePreviousTurnAfterReplacement() async throws {
        let service = HistoryStubService()
        service.sendHandler = { _, _, _, _ in
            CommandReceipt(
                commandId: "queued-command", requestId: "queued-request", status: .accepted,
                delivery: .queue, messageId: "queued-request", deliveryId: "queued-delivery"
            )
        }
        let model = await makeStartedConversation(service: service)
        model.updateCapabilities(running: true)
        model.draft = "Next fixture prompt"
        await model.send()
        let pending = try #require(model.messages.first)
        #expect(service.sendCalls.last?.3 == .queue)
        let firstReply = ConversationMessage(
            messageId: "previous-turn-reply", role: .assistant,
            blocks: [ConversationBlock(blockId: "reply", kind: .text, text: "First fixture reply")],
            createdAtUnixMs: pending.createdAtUnixMs + 1
        )
        model.handle(SessionEventEnvelope(
            hostId: "studio-mac", sessionId: "s-001", sequence: 2,
            event: .historySnapshot(
                ConversationHistoryPage(
                    source: .online, projection: "projection-1", complete: true,
                    messages: [firstReply], activities: [], continuation: nil
                ),
                replacing: true
            )
        ))
        var queuedEcho = ConversationMessage(
            messageId: "queued-prompt", role: .user,
            blocks: pending.blocks, createdAtUnixMs: pending.createdAtUnixMs + 2
        )
        queuedEcho.deliveryId = "queued-delivery"
        model.handle(SessionEventEnvelope(
            hostId: "studio-mac", sessionId: "s-001", sequence: 3, event: .message(queuedEcho)
        ))

        #expect(model.messages.map(\.messageId) == [firstReply.messageId, queuedEcho.messageId])
        #expect(model.presentationID(for: queuedEcho.messageId) == pending.messageId)
    }
}
