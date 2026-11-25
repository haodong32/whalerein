// Controlled cache-to-online handoff while the transcript contains a
// previously loaded older page and the user submits a correlated send.

import Foundation
import Testing
@testable import WhaleRein

private func cacheMessage(
    _ id: String,
    text: String? = nil,
    role: MessageRole = .assistant
) -> ConversationMessage {
    ConversationMessage(
        messageId: id,
        role: role,
        blocks: [ConversationBlock(blockId: "\(id)-block-0", kind: .text, text: text ?? id)],
        createdAtUnixMs: 1
    )
}

private func cacheEnvelope(_ event: MessageEvent, sequence: Int) -> SessionEventEnvelope {
    SessionEventEnvelope(
        hostId: "studio-mac",
        sessionId: "s-001",
        sequence: sequence,
        event: event
    )
}

@Suite("Conversation history continuity")
@MainActor
struct ConversationHistoryContinuityTests {
    @Test("a continuous snapshot and send echo keep the long transcript and paged rows")
    func continuousSnapshotsAndEchoesKeepTranscript() async {
        let service = HistoryStubService()
        service.sendHandler = { _, _, text, delivery in
            CommandReceipt(
                commandId: "cmd-cache-1",
                requestId: "req-cache-1",
                status: .accepted,
                delivery: delivery,
                messageId: "msg-cache-user"
            )
        }
        let model = await makeStartedConversation(service: service)
        let previous = FixtureCatalog.conversation(sessionId: "s-001")
        model.handle(cacheEnvelope(.historySnapshot(
            ConversationHistoryPage(
                source: .cache, projection: "projection-1", complete: false, messages: previous,
                activities: [], continuation: "cached-cut"
            ),
            replacing: true
        ), sequence: 1))
        #expect(model.messages.count == FixtureCatalog.transcriptCount)

        // One paged row and a live preview both belong to the old view; a
        // reset would discard them, an incremental append must not.
        service.historyPageHandler = { _ in
            ConversationHistoryPage(
                source: .cache,
                projection: "projection-1",
                complete: false,
                messages: [cacheMessage("older-1")],
                activities: [],
                continuation: "cursor-older"
            )
        }
        await model.loadOlderHistory()
        #expect(model.messages.first?.messageId == "older-1")
        model.handle(
            cacheEnvelope(
                .preview(attemptId: "attempt-1", text: "streaming", replace: true, incomplete: true),
                sequence: 2
            )
        )

        model.draft = "sent"
        await model.send()
        #expect(model.messages.count == FixtureCatalog.transcriptCount + 2)

        let committed = cacheMessage("msg-cache-user", text: "sent", role: .user)
        model.handle(cacheEnvelope(.historySnapshot(
            ConversationHistoryPage(
                source: .online, projection: "projection-1", complete: false, messages: previous,
                activities: [], continuation: "online-cut"
            ),
            replacing: false
        ), sequence: 3))
        model.handle(cacheEnvelope(.message(committed), sequence: 4))

        #expect(model.messages.count == FixtureCatalog.transcriptCount + 2)
        #expect(model.messages.first?.messageId == "older-1")
        #expect(model.messages.last?.messageId == "msg-cache-user")
        #expect(!model.messages.contains { $0.messageId.hasPrefix("local-") })
        #expect(model.history.provenance == .online)
        #expect(model.history.continuation == "online-cut")
        #expect(model.previewText == "streaming", "a tail append must not blank the live reply")
    }
}
