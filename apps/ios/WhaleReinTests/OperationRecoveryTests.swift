// Focused tests for the shared operation receipt and recovery interface:
// coalesced visible-session refresh, exact scope, unresolved manual recovery,
// confirmed references with failed content reads, and read-only auto lookup.

import Foundation
import Testing
@testable import WhaleRein

@MainActor
private final class RecoveryStubService: StubSessionService, OperationRecoveryService {
    var retained: [OperationReceipt] = []
    var recovered: [String: OperationReceipt] = [:]
    var readContents: [String: ConversationMessage] = [:]
    var listGate: SuspendedResponse<[OperationReceipt]>?

    private(set) var listCount = 0
    private(set) var recoverCalls: [(operationId: String, follow: Bool)] = []
    private(set) var readCalls: [MessageReference] = []

    func listRetainedOperations() async throws -> [OperationReceipt] {
        listCount += 1
        if let listGate {
            return try await listGate.wait()
        }
        return retained
    }

    func recoverSend(operationId: String, follow: Bool) async throws -> OperationReceipt {
        recoverCalls.append((operationId, follow))
        if let receipt = recovered[operationId] {
            return receipt
        }
        guard let stored = retained.first(where: { $0.operationId == operationId }) else {
            throw SessionServiceError(.unavailable)
        }
        return stored
    }

    func readMessage(_ reference: MessageReference) async throws -> ConversationMessage {
        readCalls.append(reference)
        guard let content = readContents[reference.messageId] else {
            throw SessionServiceError(.unavailable)
        }
        return content
    }
}

private func testScope(
    instanceId: String = "fixtures",
    hostId: String = "studio-mac",
    sessionId: String = "s-001"
) -> OperationScope {
    OperationScope(instanceId: instanceId, hostId: hostId, sessionId: sessionId)
}

private func operation(
    id: String,
    kind: OperationKind = .send,
    scope: OperationScope? = testScope(),
    status: OperationStatus,
    receipt: CommandReceipt? = nil,
    reply: MessageReply? = nil,
    content: ConversationMessage? = nil,
    readError: String? = nil
) -> OperationReceipt {
    OperationReceipt(
        operationId: id,
        kind: kind,
        scope: scope,
        status: status,
        receipt: receipt,
        reply: reply,
        content: content,
        readError: readError
    )
}

private func reference(for messageId: String, scope: OperationScope = testScope()) -> MessageReference {
    MessageReference(
        hostId: scope.hostId,
        sessionId: scope.sessionId,
        messageId: messageId,
        sourceLocator: "locator-\(messageId)",
        projectionId: "projection-1"
    )
}

private func message(_ id: String, text: String, role: MessageRole = .assistant) -> ConversationMessage {
    ConversationMessage(
        messageId: id,
        role: role,
        blocks: [ConversationBlock(blockId: "\(id)-block-0", kind: .text, text: text)],
        createdAtUnixMs: 1
    )
}

@Suite("Operation recovery")
@MainActor
struct OperationRecoveryTests {
    @Test("overlapping foreground and reconnect triggers share one status lookup")
    func overlappingTriggersCoalesce() async {
        let service = RecoveryStubService()
        let scope = testScope()
        let stored = operation(id: "op-1", status: .accepted)
        service.retained = [stored]
        service.recovered["op-1"] = operation(
            id: "op-1",
            status: .confirmed,
            receipt: CommandReceipt(
                commandId: "cmd-1",
                requestId: "op-1",
                status: .confirmed,
                delivery: .queue
            ),
            reply: .committed(reference: reference(for: "reply-1")),
            content: message("reply-1", text: "exact reply")
        )
        let gate = SuspendedResponse<[OperationReceipt]>()
        service.listGate = gate
        let model = OperationRecoveryModel(service: service, instanceId: scope.instanceId)

        async let foreground: Void = model.refreshVisibleOperations(in: scope)
        await gate.waitUntilStarted()
        async let reconnect: Void = model.refreshVisibleOperations(in: scope)
        await pump(20)
        gate.resume(with: service.retained)
        await foreground
        await reconnect

        #expect(service.listCount == 1)
        #expect(service.recoverCalls.count == 1)
        #expect(service.recoverCalls.first?.follow == false)
        #expect(model.automaticLookupCount == 1)
        #expect(model.automaticRefreshCount == 1)
        #expect(model.receipt(for: "op-1")?.status == .confirmed)
    }

    @Test("visible refresh filters the exact instance, Host and session")
    func exactScopeFiltering() async {
        let service = RecoveryStubService()
        let scope = testScope()
        let visible = operation(id: "visible", status: .uncertain)
        let otherSession = operation(
            id: "other-session",
            scope: testScope(sessionId: "s-002"),
            status: .uncertain
        )
        let otherHost = operation(
            id: "other-host",
            scope: testScope(hostId: "studio-mini"),
            status: .uncertain
        )
        let otherInstance = operation(
            id: "other-instance",
            scope: testScope(instanceId: "cloud-production"),
            status: .uncertain
        )
        let alreadyConfirmed = operation(id: "confirmed", status: .confirmed)
        let control = operation(id: "control", kind: .control, status: .uncertain)
        service.retained = [
            visible,
            otherSession,
            otherHost,
            otherInstance,
            alreadyConfirmed,
            control
        ]
        service.recovered["visible"] = operation(id: "visible", status: .confirmed)
        let model = OperationRecoveryModel(service: service, instanceId: scope.instanceId)

        await model.refreshVisibleOperations(in: scope)

        #expect(service.recoverCalls.map(\.operationId) == ["visible"])
        #expect(model.receipt(for: "visible")?.status == .confirmed)
        // Operations from other sessions, Hosts and instances stay listed for
        // manual recovery, and are never queried automatically.
        #expect(model.receipt(for: "other-session")?.status == .uncertain)
        #expect(model.receipt(for: "other-host")?.status == .uncertain)
        #expect(model.receipt(for: "other-instance")?.status == .uncertain)
        #expect(model.receipt(for: "control")?.status == .uncertain)
        #expect(model.receipt(for: "confirmed")?.status == .confirmed)
        #expect(model.operations(for: scope).map(\.operationId) == ["confirmed", "control", "visible"])
    }

    @Test("an unresolved lookup stays listed and remains manually recoverable")
    func unresolvedStaysManual() async {
        let service = RecoveryStubService()
        let scope = testScope()
        service.retained = [operation(id: "op-uncertain", status: .uncertain)]
        // The status lookup cannot settle it yet.
        service.recovered["op-uncertain"] = operation(id: "op-uncertain", status: .uncertain)
        let model = OperationRecoveryModel(service: service, instanceId: scope.instanceId)

        await model.refreshVisibleOperations(in: scope)
        #expect(model.receipt(for: "op-uncertain")?.status == .uncertain)
        #expect(model.automaticLookupCount == 1)

        service.recovered["op-uncertain"] = operation(
            id: "op-uncertain",
            status: .confirmed,
            reply: .committed(reference: reference(for: "reply-2"))
        )
        let manuallyRecovered = await model.recover(operationId: "op-uncertain")
        #expect(manuallyRecovered?.status == .confirmed)
        #expect(model.receipt(for: "op-uncertain")?.status == .confirmed)
        #expect(service.recoverCalls.last?.follow == true)
    }

    @Test("a confirmed reference with a failed content read keeps its confirmation")
    func confirmedWithFailedRead() async {
        let service = RecoveryStubService()
        let scope = testScope()
        let confirmedReference = reference(for: "reply-3")
        service.retained = [
            operation(
                id: "op-read",
                status: .confirmed,
                reply: .committed(reference: confirmedReference),
                readError: "not_found"
            )
        ]
        let model = OperationRecoveryModel(service: service, instanceId: scope.instanceId)
        await model.load()

        #expect(model.receipt(for: "op-read")?.status == .confirmed)
        #expect(model.receipt(for: "op-read")?.readError == "not_found")
        #expect(model.receipt(for: "op-read")?.canReadExactReply == true)

        // A failed reread keeps the same confirmation.
        #expect(await model.readExactReply(operationId: "op-read") == nil)
        #expect(model.receipt(for: "op-read")?.status == .confirmed)
        #expect(model.receipt(for: "op-read")?.readError != nil)

        // Once the content is readable, the explicit reread fills it in.
        let exact = message("reply-3", text: "recovered reply")
        service.readContents["reply-3"] = exact
        let read = await model.readExactReply(operationId: "op-read")
        #expect(read == exact)
        #expect(model.receipt(for: "op-read")?.content == exact)
        #expect(model.receipt(for: "op-read")?.readError == nil)
        #expect(model.receipt(for: "op-read")?.canReadExactReply == false)
    }

    @Test("automatic foreground refresh never sends or resumes pairing")
    func automaticRefreshIsReadOnly() async {
        let service = RecoveryStubService()
        let directory = makeTempDirectory()
        let preferences = PreferencesStore(directory: directory)
        preferences.update { $0.paired = true }
        let app = AppModel(
            service: service,
            preferences: preferences,
            dataDirectory: directory,
            options: testLaunchOptions()
        )
        await app.bootstrapConversation()
        await pump(120)
        #expect(app.conversation != nil)

        app.conversation?.draft = "send once"
        await app.conversation?.send()
        let operationId = app.conversation?.lastSendOperationId
        #expect(operationId != nil)

        let scope = testScope(instanceId: service.instanceId)
        service.retained = [
            operation(
                id: operationId ?? "missing",
                scope: scope,
                status: .uncertain
            )
        ]
        service.recovered[operationId ?? "missing"] = operation(
            id: operationId ?? "missing",
            scope: scope,
            status: .confirmed,
            reply: .committed(reference: reference(for: "reply-live", scope: scope)),
            content: message("reply-live", text: "live recovered reply")
        )

        await app.enterBackground()
        await app.becomeActive()

        #expect(service.recoverCalls.map(\.operationId) == [operationId])
        #expect(service.recoverCalls.first?.follow == false)
        // The explicit user send happened, but automatic lookup added none.
        #expect(service.sendCalls.count == 1)
        #expect(service.stopCalls.isEmpty)
        #expect(service.pairingStartCount == 0)
        #expect(service.pairingDecideArguments.isEmpty)
        #expect(app.conversation?.operationResult?.status == .confirmed)
        #expect(app.conversation?.messages.contains { $0.messageId == "reply-live" } == true)
    }

    @Test("a reconnect makes one additional read-only lookup for unresolved sends")
    func reconnectRefresh() async {
        let service = RecoveryStubService()
        let directory = makeTempDirectory()
        let preferences = PreferencesStore(directory: directory)
        preferences.update { $0.paired = true }
        let app = AppModel(
            service: service,
            preferences: preferences,
            dataDirectory: directory,
            options: testLaunchOptions()
        )
        await app.bootstrapConversation()
        await pump(120)
        app.conversation?.draft = "reconnect"
        await app.conversation?.send()
        guard let operationId = app.conversation?.lastSendOperationId else {
            Issue.record("no original operation identity")
            return
        }
        let scope = testScope(instanceId: service.instanceId)
        let unresolved = operation(id: operationId, scope: scope, status: .uncertain)
        service.retained = [unresolved]
        service.recovered[operationId] = unresolved

        await app.enterBackground()
        await app.becomeActive()
        #expect(service.recoverCalls.count == 1)

        await app.conversation?.reconnectWatch()
        await waitUntil { service.recoverCalls.count == 2 }
        #expect(service.recoverCalls.count == 2)
        #expect(service.recoverCalls.allSatisfy { !$0.follow })
        #expect(service.sendCalls.count == 1)
        #expect(service.pairingStartCount == 0)
        #expect(service.pairingDecideArguments.isEmpty)
        #expect(app.conversation?.operationResult?.status == .uncertain)
        #expect(app.operationRecovery?.receipt(for: operationId)?.status == .uncertain)
    }

    @Test("the conversation rejects a foreign scope and deduplicates an exact reply")
    func conversationScopeAndDeduplication() async {
        let service = RecoveryStubService()
        let scope = testScope()
        let model = ConversationModel(
            hostId: scope.hostId,
            sessionId: scope.sessionId,
            sessionTitle: "Recovery",
            service: service
        )
        await model.start()
        service.emit(.reset(messages: [], approvals: []), hostId: scope.hostId, sessionId: scope.sessionId, sequence: 1)
        await pump(30)

        model.draft = "hello"
        await model.send()
        let operationId = model.lastSendOperationId
        #expect(operationId != nil)

        let foreign = operation(
            id: operationId ?? "missing",
            scope: testScope(hostId: "other-host"),
            status: .confirmed,
            reply: .committed(reference: reference(for: "foreign-reply")),
            content: message("foreign-reply", text: "FOREIGN")
        )
        model.applyRecoveredOperations([foreign])
        #expect(model.operationErrorKey == .errorOperationScope)
        #expect(model.messages.contains { $0.messageId == "foreign-reply" } == false)

        let confirmedReference = reference(for: "reply-dedupe", scope: scope)
        let exact = message("reply-dedupe", text: "EXACT-REPLY")
        model.applyRecoveredOperations([
            operation(
                id: operationId ?? "missing",
                scope: scope,
                status: .confirmed,
                reply: .committed(reference: confirmedReference),
                content: exact
            )
        ])
        #expect(model.messages.filter { $0.messageId == "reply-dedupe" }.count == 1)
        // The same committed identity arriving through the live watch is not
        // rendered twice.
        service.emit(
            .message(exact),
            hostId: scope.hostId,
            sessionId: scope.sessionId,
            sequence: 2
        )
        await pump(30)
        #expect(model.messages.filter { $0.messageId == "reply-dedupe" }.count == 1)
    }

    @Test("fixture adapter lists the original send and reads its exact reply")
    func fixtureAdapterEvidence() async throws {
        let service = try await makePairedFixture()
        try await service.startWatch(hostId: "studio-mac", sessionId: "s-001")
        let sent = try await service.send(
            hostId: "studio-mac",
            sessionId: "s-001",
            text: "fixture recovery",
            delivery: .send
        )
        let listed = try await service.listRetainedOperations()
        #expect(listed.count == 1)
        #expect(listed.first?.operationId == sent.requestId)
        #expect(listed.first?.kind == .send)
        #expect(listed.first?.scope == OperationScope(
            instanceId: service.instanceId,
            hostId: "studio-mac",
            sessionId: "s-001"
        ))

        let recovered = try await service.recoverSend(operationId: sent.requestId, follow: true)
        #expect(recovered.status == .confirmed)
        guard let confirmed = recovered.confirmedReference else {
            Issue.record("fixture recovery did not return a confirmed reference")
            return
        }
        #expect(recovered.content != nil)
        let read = try await service.readMessage(confirmed)
        #expect(read == recovered.content)
    }
}
