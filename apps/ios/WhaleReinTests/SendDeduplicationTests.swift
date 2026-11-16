// Optimistic send reconciliation. Core may echo the committed user message with
// its own id; that echo must replace the local bubble instead of adding a second
// one. Sources that never echo keep exactly one local bubble.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Optimistic send deduplication")
@MainActor
struct SendDeduplicationTests {
    private func userMessage(id: String, text: String) -> ConversationMessage {
        ConversationMessage(
            messageId: id,
            role: .user,
            blocks: [ConversationBlock(blockId: "\(id)-block-0", kind: .text, text: text)],
            createdAtUnixMs: 1
        )
    }

    private func envelope(
        _ event: MessageEvent,
        sequence: Int
    ) -> SessionEventEnvelope {
        SessionEventEnvelope(
            hostId: "studio-mac",
            sessionId: "s-001",
            sequence: sequence,
            event: event
        )
    }

    @Test("a Core echo with the receipt message id replaces the optimistic bubble")
    func echoByMessageId() async {
        let service = StubSessionService()
        let receipt = CommandReceipt(
            commandId: "cmd-echo-1",
            requestId: "req-echo-1",
            status: .accepted,
            delivery: .send,
            messageId: "msg-core-1",
            deliveryId: "del-1"
        )
        service.sendHandler = { _, _, _, _ in receipt }
        let model = await makeStartedConversation(service: service)
        model.draft = "hello echo"

        await model.send()
        #expect(model.messages.count == 1)
        #expect(model.messages[0].messageId == "local-cmd-echo-1")

        model.handle(envelope(.message(userMessage(id: "msg-core-1", text: "hello echo")), sequence: 2))

        #expect(model.messages.count == 1, "The committed echo must not add a second bubble")
        #expect(model.messages[0].messageId == "msg-core-1")
    }

    @Test("an echo without a correlation id is matched by its exact text")
    func echoByText() async {
        let service = StubSessionService()
        service.sendHandler = { _, _, _, delivery in
            CommandReceipt(
                commandId: "cmd-echo-2",
                requestId: "req-echo-2",
                status: .accepted,
                delivery: delivery
            )
        }
        let model = await makeStartedConversation(service: service)
        model.draft = "text matched"

        await model.send()
        model.handle(envelope(.message(userMessage(id: "msg-core-2", text: "text matched")), sequence: 2))

        #expect(model.messages.count == 1)
        #expect(model.messages[0].messageId == "msg-core-2")
    }

    @Test("a duplicate echo cannot add a second committed row")
    func duplicateEchoIgnored() async {
        let service = StubSessionService()
        let receipt = CommandReceipt(
            commandId: "cmd-echo-3",
            requestId: "req-echo-3",
            status: .accepted,
            delivery: .send,
            messageId: "msg-core-3"
        )
        service.sendHandler = { _, _, _, _ in receipt }
        let model = await makeStartedConversation(service: service)
        model.draft = "duplicate"

        await model.send()
        let echo = userMessage(id: "msg-core-3", text: "duplicate")
        model.handle(envelope(.message(echo), sequence: 2))
        model.handle(envelope(.replace(message: echo), sequence: 3))
        model.handle(envelope(.message(echo), sequence: 4))

        #expect(model.messages.count == 1)
        #expect(model.messages[0].messageId == "msg-core-3")
    }

    @Test("a Host that never echoes keeps exactly one local bubble")
    func nonEchoKeepsLocalBubble() async {
        let service = StubSessionService()
        let receipt = CommandReceipt(
            commandId: "cmd-local-1",
            requestId: "req-local-1",
            status: .accepted,
            delivery: .send
        )
        service.sendHandler = { _, _, _, _ in receipt }
        let model = await makeStartedConversation(service: service)
        model.draft = "no echo"

        await model.send()
        var settled = receipt
        settled.status = .confirmed
        model.handle(envelope(.commandStatus(settled), sequence: 2))

        #expect(model.messages.count == 1)
        #expect(model.messages[0].messageId == "local-cmd-local-1")

        // A later unrelated user message must still append normally.
        model.handle(envelope(.message(userMessage(id: "msg-other", text: "another")), sequence: 3))
        #expect(model.messages.map(\.messageId) == ["local-cmd-local-1", "msg-other"])
    }

    @Test("an echo of a different text does not remove the pending bubble")
    func differentTextDoesNotMatch() async {
        let service = StubSessionService()
        service.sendHandler = { _, _, _, delivery in
            CommandReceipt(
                commandId: "cmd-echo-4",
                requestId: "req-echo-4",
                status: .accepted,
                delivery: delivery
            )
        }
        let model = await makeStartedConversation(service: service)
        model.draft = "expected"

        await model.send()
        model.handle(envelope(.message(userMessage(id: "msg-other", text: "different")), sequence: 2))

        #expect(model.messages.count == 2)
        #expect(model.messages.map(\.messageId) == ["local-cmd-echo-4", "msg-other"])
    }

    @Test("an uncertain send stays reconciled when the echo and settlement race")
    func uncertainEchoReconciliation() async {
        let service = StubSessionService()
        let receipt = CommandReceipt(
            commandId: "cmd-uncertain",
            requestId: "req-uncertain",
            status: .uncertain,
            delivery: .send,
            messageId: "msg-committed"
        )
        service.sendHandler = { _, _, _, _ in receipt }
        let model = await makeStartedConversation(service: service)
        model.draft = "race"

        await model.send()
        #expect(model.inFlightSend != nil)
        #expect(model.messages.count == 1)

        model.handle(envelope(.message(userMessage(id: "msg-committed", text: "race")), sequence: 2))
        var settled = receipt
        settled.status = .confirmed
        model.handle(envelope(.commandStatus(settled), sequence: 3))

        #expect(model.messages.count == 1)
        #expect(model.messages[0].messageId == "msg-committed")
        #expect(model.inFlightSend == nil)
        #expect(model.draft.isEmpty)
    }
}
