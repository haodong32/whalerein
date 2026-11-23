// Live Host/session isolation.
//
// Switching conversations must fence the previous scope: events, drafts, cached
// rows and operation results belong to the exact instance/Host/session that
// produced them. Equal session IDs on different Hosts are a normal case, so
// identity never collapses to the session ID alone.
//
// The loopback stack exposes one fixture session on one Host, so cross-session
// switching is exercised at the model/state boundary here and the real
// Core-scoped draft key is asserted in the Stack plan. This suite states that
// fixture limitation instead of claiming a live multi-session pass.

import Foundation
import Testing
@testable import WhaleRein

// MARK: - Scoped operation service

@MainActor
private final class ScopedOperationService: StubSessionService, OperationRecoveryService {
    var retained: [OperationReceipt] = []

    func listRetainedOperations() async throws -> [OperationReceipt] { retained }

    func recoverSend(operationId: String, follow: Bool) async throws -> OperationReceipt {
        guard let receipt = retained.first(where: { $0.operationId == operationId }) else {
            throw SessionServiceError(.unavailable)
        }
        return receipt
    }

    func readMessage(_ reference: MessageReference) async throws -> ConversationMessage {
        throw SessionServiceError(.unavailable)
    }
}

// MARK: - Fixtures

private func message(_ id: String, text: String) -> ConversationMessage {
    ConversationMessage(
        messageId: id,
        role: .assistant,
        blocks: [ConversationBlock(blockId: "block-\(id)", kind: .text, text: text)],
        createdAtUnixMs: 1_700_000_000_000
    )
}

private func operation(
    id: String,
    instanceId: String,
    hostId: String,
    sessionId: String
) -> OperationReceipt {
    OperationReceipt(
        operationId: id,
        kind: .send,
        scope: OperationScope(instanceId: instanceId, hostId: hostId, sessionId: sessionId),
        status: .uncertain,
        receipt: nil,
        reply: nil,
        content: nil,
        readError: nil
    )
}

private func envelope(
    hostId: String,
    sessionId: String,
    sequence: Int,
    event: MessageEvent
) -> SessionEventEnvelope {
    SessionEventEnvelope(hostId: hostId, sessionId: sessionId, sequence: sequence, event: event)
}

// MARK: - Tests

@Suite("Host and session isolation")
@MainActor
struct HostSessionIsolationTests {
    @Test("equal session IDs on two Hosts keep independent transcripts and watches")
    func equalSessionIdsOnDifferentHostsStayIndependent() async {
        let service = StubSessionService()
        let studio = await makeStartedConversation(
            service: service,
            hostId: "studio-mac",
            sessionId: "s-001"
        )
        let laptop = await makeStartedConversation(
            service: service,
            hostId: "travel-laptop",
            sessionId: "s-001"
        )

        service.emit(
            .reset(messages: [message("a-1", text: "studio history")], approvals: []),
            hostId: "studio-mac",
            sessionId: "s-001",
            sequence: 1
        )
        service.emit(
            .reset(messages: [message("b-1", text: "laptop history")], approvals: []),
            hostId: "travel-laptop",
            sessionId: "s-001",
            sequence: 1
        )
        await pump(50)

        #expect(studio.messages.map(\.messageId) == ["a-1"])
        #expect(laptop.messages.map(\.messageId) == ["b-1"])

        // A late event for the studio scope must not reach the laptop model even
        // though both sessions share the same ID.
        service.emit(
            .message(message("a-2", text: "late studio message")),
            hostId: "studio-mac",
            sessionId: "s-001",
            sequence: 5
        )
        await pump(50)

        #expect(studio.messages.map(\.messageId) == ["a-1", "a-2"])
        #expect(laptop.messages.map(\.messageId) == ["b-1"])
    }

    @Test("a late reset, approval or watch end from the previous scope cannot mutate the new conversation")
    func latePreviousScopeEventsAreFenced() async {
        let service = StubSessionService()
        let previous = await makeStartedConversation(
            service: service,
            hostId: "studio-mac",
            sessionId: "s-001"
        )
        previous.handle(envelope(
            hostId: "studio-mac",
            sessionId: "s-001",
            sequence: 1,
            event: .reset(
                messages: [message("old-1", text: "old transcript")],
                approvals: [testApproval(id: "approval-old", sessionId: "s-001")]
            )
        ))
        #expect(previous.messages.map(\.messageId) == ["old-1"])

        // New conversation with the same session ID on another Host.
        let current = await makeStartedConversation(
            service: service,
            hostId: "travel-laptop",
            sessionId: "s-001"
        )
        current.handle(envelope(
            hostId: "travel-laptop",
            sessionId: "s-001",
            sequence: 1,
            event: .reset(messages: [message("new-1", text: "new transcript")], approvals: [])
        ))
        #expect(current.messages.map(\.messageId) == ["new-1"])
        #expect(current.approvals.isEmpty)

        // Delivered through the shared stream and directly to the new model:
        // both paths must reject the previous Host/session scope.
        let late = [
            envelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 50,
                event: .reset(
                    messages: [message("leak", text: "late reset")],
                    approvals: [testApproval(id: "approval-leak", sessionId: "s-001")]
                )
            ),
            envelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 51,
                event: .approvals([testApproval(id: "approval-leak-2", sessionId: "s-001")])
            ),
            envelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 52,
                event: .watchEnded(reason: "closed")
            ),
        ]
        for event in late {
            service.emit(event.event, hostId: event.hostId, sessionId: event.sessionId, sequence: event.sequence)
            current.handle(event)
        }
        await pump(50)

        #expect(current.messages.map(\.messageId) == ["new-1"])
        #expect(current.approvals.isEmpty)
        #expect(!current.watchEnded)
        #expect(current.connection != .offline)
    }

    @Test("drafts are scoped by Host and session, so equal session IDs keep separate text")
    func draftsAreHostAndSessionScoped() async {
        let service = StubSessionService()
        let studio = await makeStartedConversation(
            service: service,
            hostId: "studio-mac",
            sessionId: "s-001"
        )
        studio.draft = "studio draft"
        await studio.suspendForBackground()

        let laptop = await makeStartedConversation(
            service: service,
            hostId: "travel-laptop",
            sessionId: "s-001"
        )
        #expect(laptop.draft.isEmpty)
        laptop.draft = "laptop draft"
        await laptop.suspendForBackground()

        #expect(service.savedDrafts.count == 2)
        let studioKey = AppStorageLocations.sessionKey(hostId: "studio-mac", sessionId: "s-001")
        let laptopKey = AppStorageLocations.sessionKey(hostId: "travel-laptop", sessionId: "s-001")
        #expect(studioKey != laptopKey)
        #expect(service.savedDrafts[studioKey] == "studio draft")
        #expect(service.savedDrafts[laptopKey] == "laptop draft")

        // Reopening each scope restores only its own draft.
        let reopenedStudio = await makeStartedConversation(
            service: service,
            hostId: "studio-mac",
            sessionId: "s-001"
        )
        let reopenedLaptop = await makeStartedConversation(
            service: service,
            hostId: "travel-laptop",
            sessionId: "s-001"
        )
        #expect(reopenedStudio.draft == "studio draft")
        #expect(reopenedLaptop.draft == "laptop draft")
    }

    @Test("retained operation results with equal session IDs stay in their exact Host scope")
    func operationResultsAreHostScoped() async {
        let service = ScopedOperationService()
        service.retained = [
            operation(id: "op-studio", instanceId: "cloud-production", hostId: "studio-mac", sessionId: "s-001"),
            operation(id: "op-laptop", instanceId: "cloud-production", hostId: "travel-laptop", sessionId: "s-001"),
            operation(id: "op-other-instance", instanceId: "cloud-staging", hostId: "studio-mac", sessionId: "s-001"),
        ]
        let model = OperationRecoveryModel(service: service, instanceId: "cloud-production")

        await model.refreshVisibleOperations(
            in: OperationScope(instanceId: "cloud-production", hostId: "studio-mac", sessionId: "s-001")
        )

        let studio = model.operations(for:
            OperationScope(instanceId: "cloud-production", hostId: "studio-mac", sessionId: "s-001")
        )
        let laptop = model.operations(for:
            OperationScope(instanceId: "cloud-production", hostId: "travel-laptop", sessionId: "s-001")
        )
        let otherInstance = model.operations(for:
            OperationScope(instanceId: "cloud-staging", hostId: "studio-mac", sessionId: "s-001")
        )

        #expect(studio.map(\.operationId) == ["op-studio"])
        #expect(laptop.map(\.operationId) == ["op-laptop"])
        #expect(otherInstance.map(\.operationId) == ["op-other-instance"])
        // Automatic lookup only follows unresolved sends in the visible scope.
        #expect(model.automaticLookupCount == 1)
    }

    @Test("switching sessions fences late events and refreshes the cached Host scope")
    func sessionSwitchFencesLateEventsAndCache() async throws {
        let service = StubSessionService()
        let directory = makeTempDirectory()
        let preferences = PreferencesStore(directory: directory)
        preferences.update {
            $0.paired = true
            $0.lastHostId = "studio-mac"
            $0.lastSessionId = "s-001"
        }
        let app = AppShellModel(
            service: service,
            preferences: preferences,
            dataDirectory: directory,
            options: testLaunchOptions()
        )
        await app.bootstrapConversation()
        let studio = try #require(app.conversation)
        studio.handle(envelope(
            hostId: "studio-mac",
            sessionId: "s-001",
            sequence: 1,
            event: .reset(messages: [message("studio-1", text: "studio")], approvals: [])
        ))
        studio.draft = "unsent studio draft"

        await app.selectHostInSwitcher("travel-laptop")
        #expect(app.sessionList.hostId == "travel-laptop")
        #expect(app.sessionList.sessions.map(\.sessionId) == ["s-101", "s-102"])
        let laptopSummary = try #require(app.sessionList.sessions.first { $0.sessionId == "s-101" })
        app.selectSessionInSwitcher(laptopSummary)
        let laptop = try #require(app.conversation)
        #expect(laptop.hostId == "travel-laptop")
        #expect(laptop.sessionId == "s-101")
        #expect(laptop.draft.isEmpty)
        await waitUntil { laptop.loadState == .loaded }
        await pump(50)
        // The outgoing conversation's draft was flushed to its own scope.
        #expect(service.savedDrafts[
            AppStorageLocations.sessionKey(hostId: "studio-mac", sessionId: "s-001")
        ] == "unsent studio draft")

        // Old-scope events still arrive after the switch.
        service.emit(
            .reset(
                messages: [message("studio-late", text: "late studio")],
                approvals: [testApproval(id: "approval-late", sessionId: "s-001")]
            ),
            hostId: "studio-mac",
            sessionId: "s-001",
            sequence: 40
        )
        service.emit(.watchEnded(reason: "closed"), hostId: "studio-mac", sessionId: "s-001", sequence: 41)
        await pump(50)

        #expect(laptop.messages.isEmpty)
        #expect(laptop.approvals.isEmpty)
        #expect(!laptop.watchEnded)

        // The new scope's own event is applied.
        service.emit(
            .reset(messages: [message("laptop-1", text: "laptop")], approvals: []),
            hostId: "travel-laptop",
            sessionId: "s-101",
            sequence: 1
        )
        await pump(50)
        #expect(laptop.messages.map(\.messageId) == ["laptop-1"])
    }
}
