// Older-history recovery after a watch reset. A reset replaces the transcript
// and discards paged rows, so the old continuation, projection and completion
// state must be cleared: otherwise a previously exhausted pagination would
// suppress reaching older history from the smaller snapshot. An exact reread
// that finishes after the reset must not restore the old projection.

import Foundation
import Testing
@testable import WhaleRein

private func historyMessage(_ id: String) -> ConversationMessage {
    ConversationMessage(
        messageId: id,
        role: .assistant,
        blocks: [ConversationBlock(blockId: "\(id)-block-0", kind: .text, text: id)],
        createdAtUnixMs: 1
    )
}

private func historyPage(
    _ ids: [String],
    projection: String = "projection-1",
    complete: Bool = false,
    continuation: String? = nil
) -> ConversationHistoryPage {
    ConversationHistoryPage(
        source: .online,
        projection: projection,
        complete: complete,
        messages: ids.map(historyMessage),
        activities: [],
        continuation: continuation
    )
}

private func historyReference(id: String, projection: String = "projection-1") -> MessageReference {
    MessageReference(
        hostId: "studio-mac",
        sessionId: "s-001",
        messageId: id,
        sourceLocator: "locator-\(id)",
        projectionId: projection
    )
}

@Suite("Conversation history reset")
@MainActor
struct ConversationHistoryResetTests {
    private func startedModel(
        _ service: HistoryStubService,
        messages: [String]
    ) async -> ConversationModel {
        let model = await makeStartedConversation(service: service)
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 1,
                event: .reset(messages: messages.map(historyMessage), approvals: [])
            )
        )
        return model
    }

    @Test("after exhausting pagination a smaller reset still reaches every older message")
    func exhaustedPaginationRecoversAfterReset() async {
        let service = HistoryStubService()
        let model = await startedModel(service, messages: ["m5", "m6"])
        service.historyPageHandler = { continuation in
            switch continuation {
            case nil:
                return historyPage(["m5", "m6"], continuation: "cursor-1")
            case "cursor-1":
                return historyPage(["m3", "m4"], continuation: "cursor-2")
            case "cursor-2":
                return historyPage(["m1", "m2"], complete: true)
            default:
                throw SessionServiceError(.staleHistory)
            }
        }

        await model.loadOlderHistory()
        await model.loadOlderHistory()
        await model.loadOlderHistory()
        #expect(model.historyComplete)
        #expect(Set(model.messages.map(\.messageId)) == ["m1", "m2", "m3", "m4", "m5", "m6"])
        #expect(!model.canLoadOlderHistory)

        // The reconnect snapshot is much smaller and replaced the transcript.
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 2,
                event: .reset(messages: [historyMessage("m6")], approvals: [])
            )
        )
        #expect(model.messages.map(\.messageId) == ["m6"])
        #expect(!model.historyComplete, "the old completion flag must not survive a new snapshot")
        #expect(model.historyContinuation == nil)
        #expect(model.historyProjection == nil)
        #expect(model.canLoadOlderHistory)

        // A fresh traversal from the new snapshot reaches all older messages.
        service.historyPageHandler = { continuation in
            switch continuation {
            case nil:
                return historyPage(["m5", "m6"], continuation: "fresh-1")
            case "fresh-1":
                return historyPage(["m3", "m4"], continuation: "fresh-2")
            case "fresh-2":
                return historyPage(["m1", "m2"], complete: true)
            default:
                throw SessionServiceError(.staleHistory)
            }
        }
        let callsBeforeReset = service.historyPageCalls.count
        await model.loadOlderHistory()
        await model.loadOlderHistory()
        await model.loadOlderHistory()
        #expect(Set(model.messages.map(\.messageId)) == ["m1", "m2", "m3", "m4", "m5", "m6"])
        #expect(model.historyComplete)
        let freshCalls = Array(service.historyPageCalls.dropFirst(callsBeforeReset))
        #expect(freshCalls.contains("fresh-1"))
        // The exhausted old continuations are never reused.
        #expect(!freshCalls.contains("cursor-2"))
    }

    @Test("an exact reread finishing after a reset cannot restore the old projection")
    func lateExactRereadAfterResetIsDropped() async {
        let service = HistoryStubService()
        let model = await startedModel(service, messages: ["m2"])
        service.historyPageHandler = { _ in
            historyPage(["m1", "m2"], complete: true)
        }
        await model.loadOlderHistory()

        let gate = SuspendedResponse<ConversationMessage>()
        service.exactHandler = { _ in try await gate.wait() }
        let read = Task { await model.rereadMessage(historyReference(id: "m1")) }
        await gate.waitUntilStarted()

        // Reconnect replaces the transcript while the exact read is in flight.
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 2,
                event: .reset(messages: [historyMessage("m9")], approvals: [])
            )
        )
        gate.resume(with: historyMessage("m1"))
        await read.value

        #expect(model.messages.map(\.messageId) == ["m9"])
        #expect(model.historyErrorKey == nil)
        #expect(model.historyProjection == nil)
    }
}
