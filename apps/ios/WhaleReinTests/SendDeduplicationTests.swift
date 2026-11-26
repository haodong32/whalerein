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
        #expect(model.messages.map(\.messageId) == [local, "other"])
        #expect(model.presentationID(for: "other") == "other")
    }

    @Test("an accepted send stays visible when only the assistant replies", arguments: [nil, "request-id"] as [String?])
    func acceptedSendStaysVisibleWithoutUserEcho(messageID: String?) async throws {
        let service = StubSessionService()
        let gate = SuspendedResponse<CommandReceipt>()
        service.sendHandler = { _, _, _, _ in try await gate.wait() }
        let model = await makeStartedConversation(service: service)
        model.draft = "Fixture prompt"

        let send = Task { await model.send() }
        await gate.waitUntilStarted()
        let localID = try #require(model.messages.first?.messageId)
        gate.resume(with: receipt(.accepted, messageID: messageID))
        await send.value
        #expect(model.lastSendStatus == .accepted)

        let reply = ConversationMessage(
            messageId: "assistant-reply",
            role: .assistant,
            blocks: [ConversationBlock(blockId: "reply-text", kind: .text, text: "Fixture reply")],
            createdAtUnixMs: 2
        )
        #expect(model.messages.map(\.messageId) == [localID])
        model.handle(envelope(.preview(attemptId: "reply", text: "Fixture preview", replace: true, incomplete: true), 2))
        #expect(model.livePreview == "Fixture preview")
        #expect(model.messages.map(\.messageId) == [localID])
        model.handle(envelope(.message(reply), 3))

        #expect(model.messages.map(\.messageId) == [localID, reply.messageId])
    }

    @Test("an early confirmed status without a message ID keeps the outgoing row")
    func earlyConfirmationWithoutUserEchoKeepsOutgoingRow() async throws {
        let service = StubSessionService()
        let gate = SuspendedResponse<CommandReceipt>()
        service.sendHandler = { _, _, _, _ in try await gate.wait() }
        let model = await makeStartedConversation(service: service)
        model.draft = "Fixture prompt"

        let send = Task { await model.send() }
        await gate.waitUntilStarted()
        let localID = try #require(model.messages.first?.messageId)
        model.handle(envelope(.commandStatus(receipt(.confirmed)), 2))
        gate.resume(with: receipt(.accepted))
        await send.value

        #expect(model.lastSendStatus == .confirmed)
        #expect(model.messages.map(\.messageId) == [localID])
    }

    @Test("an older send receipt leaves the next draft editable and persisted")
    func delayedReceiptPreservesTheNextDraft() async throws {
        let service = StubSessionService()
        let gate = SuspendedResponse<CommandReceipt>()
        service.sendHandler = { _, _, _, _ in try await gate.wait() }
        let model = await makeStartedConversation(service: service)
        model.draft = "Fixture prompt"

        let send = Task { await model.send() }
        await gate.waitUntilStarted()
        model.draft = "Next fixture draft"
        gate.resume(with: receipt(.accepted, messageID: "first-message"))
        await send.value

        #expect(model.draft == "Next fixture draft")
        let persisted = try await service.loadDraft(hostId: "studio-mac", sessionId: "s-001")
        #expect(persisted == "Next fixture draft")
        model.close()
        let reopened = await makeStartedConversation(service: service)
        #expect(reopened.draft == "Next fixture draft")
    }

    private func assistantReply(createdAt: Int64 = 2) -> ConversationMessage {
        ConversationMessage(
            messageId: "fixture-assistant",
            role: .assistant,
            blocks: [ConversationBlock(blockId: "answer", kind: .text, text: "Fixture reply")],
            createdAtUnixMs: createdAt
        )
    }

    @Test("a replacement snapshot keeps the accepted prompt before its answer")
    func replacementKeepsAcceptedPromptBeforeReply() async throws {
        let service = StubSessionService()
        service.sendHandler = { _, _, _, _ in receipt(.accepted, messageID: "user-echo") }
        let model = await makeStartedConversation(service: service)
        model.draft = "Fixture prompt"
        await model.send()
        let local = try #require(model.messages.last)
        let localID = local.messageId
        let reply = assistantReply(createdAt: local.createdAtUnixMs + 1)
        let page = ConversationHistoryPage(
            source: .online, projection: "replacement", complete: true,
            messages: [reply], activities: [], continuation: nil
        )
        model.handle(envelope(.historySnapshot(page, replacing: true), 2))
        #expect(model.messages.map(\.messageId) == [localID, reply.messageId])
        model.handle(envelope(.message(echo("user-echo")), 3))
        #expect(model.messages.map(\.messageId) == ["user-echo", reply.messageId])
        #expect(model.presentationID(for: "user-echo") == localID)
    }

    @Test("identical submissions correlate independently even when echoes arrive in reverse order")
    func twoIdenticalSendsKeepTheirOwnIdentity() async throws {
        let service = StubSessionService()
        var next = 0
        service.sendHandler = { _, _, _, _ in
            next += 1
            return CommandReceipt(
                commandId: "command-\(next)", requestId: "request-\(next)",
                status: .accepted, delivery: .queue, messageId: "echo-\(next)"
            )
        }
        let model = await makeStartedConversation(service: service)
        model.draft = "equal"
        await model.send()
        let first = try #require(model.messages.last?.messageId)
        model.draft = "equal"
        await model.send()
        let second = try #require(model.messages.last?.messageId)
        #expect(first != second)
        #expect(service.sendCalls.count == 2)

        model.handle(envelope(.message(echo("echo-2")), 2))
        #expect(model.messages.map(\.messageId) == [first, "echo-2"])
        model.handle(envelope(.message(echo("echo-1")), 3))
        #expect(Set(model.messages.map(\.messageId)) == ["echo-1", "echo-2"])
        #expect(model.messages.count == 2)
        #expect(model.presentationID(for: "echo-1") == first)
        #expect(model.presentationID(for: "echo-2") == second)
    }

    @Test("the first turn ending keeps the next queued send until its own echo")
    func queuedPromptSurvivesFirstTurn() async throws {
        let service = StubSessionService()
        service.sendHandler = { _, _, _, _ in receipt(.accepted, messageID: "queued-echo") }
        let model = await makeStartedConversation(service: service)
        model.updateCapabilities(running: true)
        model.draft = "equal"
        await model.send()
        let queued = try #require(model.messages.last?.messageId)
        #expect(model.lastSendStatus == .accepted)
        #expect(service.sendCalls.last?.3 == .queue)
        let reply = assistantReply()
        model.handle(envelope(.message(reply), 2))
        #expect(model.messages.contains { $0.messageId == queued })
        model.handle(envelope(.attemptEnded(attemptId: "first-turn", outcome: .completed), 3))
        #expect(model.messages.contains { $0.messageId == queued })
        model.handle(envelope(.message(echo("queued-echo")), 4))
        #expect(model.messages.map(\.messageId) == [reply.messageId, "queued-echo"])
        #expect(model.presentationID(for: "queued-echo") == queued)
    }

    @Test("switching away during a send isolates the draft and the original session recovers history")
    func switchAwayDuringSendAndReturn() async throws {
        let service = StubSessionService()
        let gate = SuspendedResponse<CommandReceipt>()
        service.sendHandler = { _, _, _, _ in try await gate.wait() }
        let original = await makeStartedConversation(service: service)
        original.draft = "equal"
        let send = Task { await original.send() }
        await gate.waitUntilStarted()
        original.close()

        let other = await makeStartedConversation(service: service, sessionId: "s-002")
        other.draft = "Other fixture draft"
        gate.resume(with: receipt(.accepted, messageID: "original-echo"))
        await send.value
        let user = echo("original-echo")
        let reply = assistantReply()
        other.handle(envelope(.message(user), 2))
        other.handle(envelope(.message(reply), 3))
        #expect(other.messages.isEmpty)
        #expect(other.draft == "Other fixture draft")
        other.close()

        let reopened = await makeStartedConversation(service: service)
        let page = ConversationHistoryPage(
            source: .online, projection: "reopened", complete: true,
            messages: [user, reply], activities: [], continuation: nil
        )
        reopened.handle(envelope(.historySnapshot(page, replacing: true), 2))
        #expect(reopened.messages.map(\.messageId) == [user.messageId, reply.messageId])
        #expect(reopened.draft.isEmpty)
    }

    @Test("a delivery ID reconciles a native receipt with a distinct committed ID", arguments: [false, true])
    func deliveryCorrelationWorksInEitherOrder(echoFirst: Bool) async {
        let service = StubSessionService()
        let gate = SuspendedResponse<CommandReceipt>()
        service.sendHandler = { _, _, _, _ in try await gate.wait() }
        let model = await makeStartedConversation(service: service)
        model.draft = "equal"

        let send = Task { await model.send() }
        await gate.waitUntilStarted()
        var committed = echo("committed-id")
        committed.deliveryId = "request-id"
        let event = envelope(.message(committed), 2)
        if echoFirst { model.handle(event) }
        gate.resume(with: CommandReceipt(
            commandId: "cmd", requestId: "request-id", status: .accepted,
            delivery: .queue, messageId: "request-id", deliveryId: "request-id"
        ))
        await send.value
        if !echoFirst { model.handle(event) }

        #expect(model.messages.map(\.messageId) == ["committed-id"])
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
