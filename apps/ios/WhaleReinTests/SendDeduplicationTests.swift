// The send presentation is stable while a receipt is pending and only
// reconciles against a committed identity supplied by that receipt.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Send deduplication")
@MainActor
struct SendDeduplicationTests {
    private func echo(_ id: String) -> ConversationMessage {
        ConversationMessage(
            messageId: id,
            role: .user,
            blocks: [ConversationBlock(blockId: id + "-text", kind: .text, text: "equal")],
            createdAtUnixMs: 1
        )
    }

    private func envelope(_ event: MessageEvent, _ sequence: Int) -> SessionEventEnvelope {
        SessionEventEnvelope(hostId: "studio-mac", sessionId: "s-001", sequence: sequence, event: event)
    }

    private func receipt(_ status: CommandStatus, messageID: String? = nil) -> CommandReceipt {
        CommandReceipt(
            commandId: "cmd", requestId: "req", status: status,
            delivery: .send, messageId: messageID
        )
    }

    @Test("a receipt-first echo reuses the pending row identity and does not move a reader")
    func receiptFirstPreservesReadingPosition() async throws {
        let service = StubSessionService()
        let gate = SuspendedResponse<CommandReceipt>()
        service.sendHandler = { _, _, _, _ in try await gate.wait() }
        let model = await makeStartedConversation(service: service)
        model.updateScroll(isAtLatest: false)
        model.noteTopVisibleMessage("visible")
        let before = model.scrollRequest?.id
        model.draft = "equal"

        let send = Task { await model.send() }
        await gate.waitUntilStarted()
        let local = try #require(model.messages.first?.messageId)
        #expect(local.hasPrefix("local-"))
        #expect(model.pendingSubmission == .send)
        #expect(model.scroll.anchorMessageId == "visible")
        #expect(model.scrollRequest?.id == before)

        gate.resume(with: receipt(.accepted, messageID: "same"))
        await send.value
        #expect(model.messages.map(\.messageId) == [local])
        model.handle(envelope(.message(echo("other")), 2))
        model.handle(envelope(.message(echo("same")), 3))
        model.handle(envelope(.message(echo("same")), 4))
        #expect(model.messages.map(\.messageId) == ["other", "same"])
        #expect(model.presentationID(for: "same") == local)
        #expect(model.scroll.anchorMessageId == "visible")
        #expect(model.scrollRequest?.id == before)
    }

    @Test("echo-first handling preserves both committed messages when another sender's text is equal")
    func echoFirstMatchesAuthoritativeID() async throws {
        let service = StubSessionService()
        let gate = SuspendedResponse<CommandReceipt>()
        service.sendHandler = { _, _, _, _ in try await gate.wait() }
        let model = await makeStartedConversation(service: service)
        model.draft = "equal"

        let send = Task { await model.send() }
        await gate.waitUntilStarted()
        let local = try #require(model.messages.first?.messageId)
        model.handle(envelope(.message(echo("other")), 2))
        model.handle(envelope(.message(echo("same")), 3))
        #expect(model.messages.map(\.messageId) == [local, "other", "same"])

        gate.resume(with: receipt(.accepted, messageID: "same"))
        await send.value
        #expect(model.messages.map(\.messageId) == ["other", "same"])
        #expect(model.presentationID(for: "other") == "other")
        #expect(model.presentationID(for: "same") == "same")
    }

    @Test("without association the receipt status and committed content remain independent")
    func textCannotSettlePending() async throws {
        let service = StubSessionService()
        let gate = SuspendedResponse<CommandReceipt>()
        service.sendHandler = { _, _, _, _ in try await gate.wait() }
        let model = await makeStartedConversation(service: service)
        model.draft = "equal"
        let send = Task { await model.send() }
        await gate.waitUntilStarted()
        let local = try #require(model.messages.first?.messageId)
        model.handle(envelope(.message(echo("other")), 2))
        #expect(model.messages.map(\.messageId) == [local, "other"])
        gate.resume(with: receipt(.accepted))
        await send.value
        #expect(model.lastSendStatus == .accepted)
        #expect(model.messages.map(\.messageId) == ["other"])
        #expect(model.presentationID(for: "other") == "other")
    }

    @Test("a rejected send removes the pending row and retains the draft")
    func rejectionRemovesPendingPresentation() async {
        let service = StubSessionService()
        service.sendHandler = { _, _, _, _ in
            CommandReceipt(commandId: "cmd", requestId: "req", status: .rejected, delivery: .send)
        }
        let model = await makeStartedConversation(service: service)
        model.draft = "equal"
        await model.send()
        #expect(model.messages.isEmpty)
        #expect(model.draft == "equal")
        #expect(model.lastSendStatus == .rejected)
    }

    @Test("uncertainty settles the original send when association and confirmation arrive later")
    func uncertainEchoReconciliation() async throws {
        let service = StubSessionService()
        service.sendHandler = { _, _, _, _ in
            CommandReceipt(commandId: "cmd", requestId: "req", status: .uncertain, delivery: .send)
        }
        let model = await makeStartedConversation(service: service)
        model.draft = "equal"

        await model.send()
        #expect(model.inFlightSend != nil)
        let local = try #require(model.messages.first?.messageId)
        model.handle(envelope(.message(echo("other")), 2))
        #expect(model.messages.map(\.messageId) == [local, "other"])

        model.handle(envelope(.commandStatus(receipt(.confirmed, messageID: "same")), 3))
        model.handle(envelope(.message(echo("same")), 4))
        #expect(Set(model.messages.map(\.messageId)) == ["other", "same"])
        #expect(model.presentationID(for: "same") == local)
        #expect(model.inFlightSend == nil)
        #expect(model.draft.isEmpty)
    }
}
