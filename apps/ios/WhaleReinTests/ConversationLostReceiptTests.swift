// Lost send/stop receipt recovery. The operation identity lives in Core's
// durable journal, not in the presentation model, so a relaunch recovers the
// original send by scope and reads its exact receipt/reply without submitting
// the message again. A definitive rejection leaves no recoverable operation and
// keeps the draft for retry. A pending Stop keeps its exact identity across a
// watch replacement until one authoritative settlement arrives.

import Foundation
import Testing
@testable import WhaleRein

@MainActor
private final class DurableOperationCore {
    var operations: [String: OperationReceipt] = [:]
    var order: [String] = []
    var sendStatus: CommandStatus = .uncertain
    var sendError: SessionServiceError?
    /// What a status lookup reports. `nil` keeps the operation unresolved.
    var resolvedStatus: OperationStatus?
    var replyText = "exact recovered reply"
    /// Draft storage survives a simulated relaunch, like the real file store.
    var drafts: [String: String] = [:]
    private(set) var sendCount = 0
    private(set) var recoverCalls: [(operationId: String, follow: Bool)] = []
    private var counter = 0

    func send(
        hostId: String,
        sessionId: String,
        delivery: DeliveryChoice
    ) throws -> CommandReceipt {
        sendCount += 1
        if let sendError { throw sendError }
        counter += 1
        let operationId = String(format: "op-%03d", counter)
        let receipt = CommandReceipt(
            commandId: "cmd-\(operationId)",
            requestId: operationId,
            status: sendStatus,
            delivery: delivery
        )
        if sendStatus != .rejected {
            let status: OperationStatus = sendStatus == .accepted ? .accepted : .uncertain
            operations[operationId] = OperationReceipt(
                operationId: operationId,
                kind: .send,
                scope: OperationScope(
                    instanceId: instanceId,
                    hostId: hostId,
                    sessionId: sessionId
                ),
                status: status,
                receipt: receipt,
                reply: .pending,
                content: nil,
                readError: nil
            )
            order.append(operationId)
        }
        return receipt
    }

    var instanceId = "relaunch-instance"

    func recover(operationId: String, follow: Bool) throws -> OperationReceipt {
        recoverCalls.append((operationId, follow))
        guard var stored = operations[operationId] else {
            throw SessionServiceError(.unavailable)
        }
        if let resolvedStatus {
            var receipt = stored.receipt
            if receipt != nil {
                receipt?.status = resolvedStatus == .confirmed ? .confirmed : .uncertain
            }
            stored = stored.replacing(
                status: resolvedStatus,
                receipt: receipt,
                reply: resolvedStatus == .confirmed
                    ? .committed(reference: reference(for: replyID(operationId)))
                    : .uncertain
            )
        }
        if follow, stored.status == .confirmed {
            stored = stored.replacing(
                content: message(replyID(operationId), text: replyText)
            )
        }
        operations[operationId] = stored
        return stored
    }

    func replyID(_ operationId: String) -> String { "reply-\(operationId)" }

    func draftKey(hostId: String, sessionId: String) -> String {
        AppStorageLocations.sessionKey(hostId: hostId, sessionId: sessionId)
    }
}

private func message(_ id: String, text: String) -> ConversationMessage {
    ConversationMessage(
        messageId: id,
        role: .assistant,
        blocks: [ConversationBlock(blockId: "\(id)-block-0", kind: .text, text: text)],
        createdAtUnixMs: 1
    )
}

private func reference(for messageId: String) -> MessageReference {
    MessageReference(
        hostId: "studio-mac",
        sessionId: "s-001",
        messageId: messageId,
        sourceLocator: "locator-\(messageId)",
        projectionId: "projection-1"
    )
}

@MainActor
private final class DurableRecoveryService: StubSessionService, OperationRecoveryService {
    let core: DurableOperationCore

    init(core: DurableOperationCore) {
        self.core = core
        super.init()
        instanceId = core.instanceId
    }

    override func send(
        hostId: String,
        sessionId: String,
        text: String,
        delivery: DeliveryChoice
    ) async throws -> CommandReceipt {
        try core.send(hostId: hostId, sessionId: sessionId, delivery: delivery)
    }

    override func saveDraft(hostId: String, sessionId: String, text: String) async throws {
        let key = core.draftKey(hostId: hostId, sessionId: sessionId)
        if text.isEmpty {
            core.drafts.removeValue(forKey: key)
        } else {
            core.drafts[key] = text
        }
    }

    override func loadDraft(hostId: String, sessionId: String) async throws -> String? {
        core.drafts[core.draftKey(hostId: hostId, sessionId: sessionId)]
    }

    override func clearDraft(hostId: String, sessionId: String) async throws {
        core.drafts.removeValue(forKey: core.draftKey(hostId: hostId, sessionId: sessionId))
    }

    func listRetainedOperations() async throws -> [OperationReceipt] {
        core.order.compactMap { core.operations[$0] }
    }

    func recoverSend(operationId: String, follow: Bool) async throws -> OperationReceipt {
        try core.recover(operationId: operationId, follow: follow)
    }

    func readMessage(_ reference: MessageReference) async throws -> ConversationMessage {
        guard let content = core.operations.values
            .compactMap(\.content)
            .first(where: { $0.messageId == reference.messageId }) else {
            throw SessionServiceError(.unavailable)
        }
        return content
    }
}

@Suite("Conversation lost receipts")
@MainActor
struct ConversationLostReceiptTests {
    @Test("a relaunch recovers the confirmed original send and exact reply once")
    func relaunchRecoversConfirmedSend() async {
        let core = DurableOperationCore()
        core.sendStatus = .uncertain
        let first = DurableRecoveryService(core: core)
        let firstModel = await makeStartedConversation(service: first)
        firstModel.draft = "ship once"
        await firstModel.persistDraftNow()
        await firstModel.send()

        guard let operationId = firstModel.lastSendOperationId else {
            Issue.record("the original send was not retained")
            return
        }
        #expect(firstModel.inFlightSend?.status == .uncertain)
        #expect(core.sendCount == 1)
        firstModel.close()
        await waitUntil { firstModel.closeDraftFlushDidComplete }

        // The Host accepted the original send while the app was gone.
        core.resolvedStatus = .confirmed
        let second = DurableRecoveryService(core: core)
        let secondModel = await makeStartedConversation(service: second)

        #expect(secondModel.lastSendOperationId == operationId)
        #expect(core.recoverCalls.map(\.operationId) == [operationId])
        #expect(core.recoverCalls.map(\.follow) == [true])
        #expect(core.sendCount == 1, "recovery must not submit the message again")
        #expect(secondModel.operationResult?.status == .confirmed)
        #expect(secondModel.operationResult?.confirmedReference?.messageId == "reply-\(operationId)")
        #expect(secondModel.messages.filter { $0.messageId == "reply-\(operationId)" }.count == 1)
        #expect(secondModel.messages.first?.plainText == core.replyText)
    }

    @Test("a still unknown send stays recoverable after relaunch and settles once")
    func relaunchKeepsUnconfirmedSendRecoverable() async {
        let core = DurableOperationCore()
        core.sendStatus = .uncertain
        let first = DurableRecoveryService(core: core)
        let firstModel = await makeStartedConversation(service: first)
        firstModel.draft = "keep looking"
        await firstModel.send()
        guard let operationId = firstModel.lastSendOperationId else {
            Issue.record("the original send was not retained")
            return
        }
        firstModel.close()
        await waitUntil { firstModel.closeDraftFlushDidComplete }

        // The status lookup cannot resolve it yet.
        let second = DurableRecoveryService(core: core)
        let secondModel = await makeStartedConversation(service: second)
        #expect(secondModel.lastSendOperationId == operationId)
        #expect(secondModel.operationResult?.status == .uncertain)
        #expect(secondModel.canRecoverSend)
        #expect(core.sendCount == 1)
        #expect(secondModel.messages.isEmpty)

        // Reconnecting must not resubmit the original message.
        await secondModel.reconnectWatch()
        #expect(core.sendCount == 1)

        // The explicit recovery action resolves the original identity.
        core.resolvedStatus = .confirmed
        await secondModel.recoverSendResult()
        #expect(core.sendCount == 1)
        #expect(secondModel.operationResult?.status == .confirmed)
        #expect(secondModel.messages.filter { $0.messageId == "reply-\(operationId)" }.count == 1)
    }

    @Test("a definitively rejected receipt creates no recoverable operation")
    func rejectedReceiptIsNotRecoverable() async {
        let core = DurableOperationCore()
        core.sendStatus = .rejected
        let first = DurableRecoveryService(core: core)
        let firstModel = await makeStartedConversation(service: first)
        firstModel.draft = "retry me"
        await firstModel.persistDraftNow()
        await firstModel.send()

        #expect(firstModel.lastSendOperationId == nil)
        #expect(core.operations.isEmpty)
        #expect(core.sendCount == 1)
        firstModel.close()
        await waitUntil { firstModel.closeDraftFlushDidComplete }

        let second = DurableRecoveryService(core: core)
        let secondModel = await makeStartedConversation(service: second)
        #expect(secondModel.lastSendOperationId == nil)
        #expect(secondModel.operationResult == nil)
        #expect(core.sendCount == 1)
        #expect(secondModel.draft == "retry me", "a definitive failure keeps the draft for retry")
    }

    @Test("a definitively failed submission keeps the draft and invents no operation")
    func thrownFailureIsNotRecoverable() async {
        let core = DurableOperationCore()
        core.sendError = SessionServiceError(.invalidInput)
        let first = DurableRecoveryService(core: core)
        let firstModel = await makeStartedConversation(service: first)
        firstModel.draft = "still mine"
        await firstModel.persistDraftNow()
        await firstModel.send()

        #expect(firstModel.lastSendOperationId == nil)
        #expect(core.operations.isEmpty)
        #expect(core.sendCount == 1)
        firstModel.close()
        await waitUntil { firstModel.closeDraftFlushDidComplete }

        let second = DurableRecoveryService(core: core)
        let secondModel = await makeStartedConversation(service: second)
        #expect(secondModel.lastSendOperationId == nil)
        #expect(secondModel.operationResult == nil)
        #expect(secondModel.draft == "still mine")
        #expect(core.recoverCalls.isEmpty)
    }

    @Test("several unresolved sends are not guessed at after relaunch")
    func ambiguousRecoveryDoesNotGuess() async {
        let core = DurableOperationCore()
        core.sendStatus = .uncertain
        _ = try? core.send(hostId: "studio-mac", sessionId: "s-001", delivery: .send)
        _ = try? core.send(hostId: "studio-mac", sessionId: "s-001", delivery: .send)
        #expect(core.order.count == 2)

        let service = DurableRecoveryService(core: core)
        let model = await makeStartedConversation(service: service)

        // With more than one unresolved candidate the startup path leaves the
        // choice to the explicit recovery surface instead of picking one.
        #expect(model.lastSendOperationId == nil)
        #expect(model.operationResult == nil)
        #expect(core.recoverCalls.isEmpty)
    }

    @Test("a pending Stop keeps its exact identity across a watch replacement")
    func stopReceiptSurvivesReconnect() async {
        let service = StubSessionService()
        service.capabilitiesHandler = { _, _ in .running }
        let receipt = CommandReceipt(
            commandId: "cmd-stop-lost",
            requestId: "req-stop-lost",
            status: .uncertain,
            delivery: nil
        )
        service.stopHandler = { _, _ in receipt }
        let model = await makeStartedConversation(service: service)

        await model.stopRun()
        #expect(model.pendingStop?.commandId == "cmd-stop-lost")
        #expect(!model.canStop)

        await model.suspendForBackground()
        await model.resumeAfterForeground()

        // The watch reset that follows must not forget the original Stop intent.
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 1,
                event: .reset(messages: [], approvals: [])
            )
        )
        #expect(model.pendingStop?.commandId == "cmd-stop-lost")
        #expect(!model.canStop)
        #expect(service.stopCalls.count == 1)

        var settled = receipt
        settled.status = .confirmed
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 2,
                event: .commandStatus(settled)
            )
        )
        #expect(model.stopSettled)
        #expect(model.lastStopStatus == .confirmed)
        #expect(model.pendingStop == nil)
        #expect(service.stopCalls.count == 1)
    }
}
