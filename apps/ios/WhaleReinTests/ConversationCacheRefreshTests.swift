// The post-send cache refresh path, exercised with stubs only: a committed
// cache tail append must reach the conversation as incremental messages, not as
// a transcript-replacing reset. The reset path blanked a 300+ row lazy
// conversation until its rows were re-realised.

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

@Suite("Post-send cache refresh")
@MainActor
struct ConversationCacheRefreshTests {
    @Test("a cache tail append projects to incremental messages, not a reset")
    func tailAppendProjectsIncrementally() {
        let previous = FixtureCatalog.conversation(sessionId: "s-001")
        let committed = cacheMessage("msg-cache-user", text: "sent", role: .user)
        let current = previous + [committed]

        let refresh = CachedHistoryRefresh.resolve(previous: previous, current: current)
        #expect(refresh == .appended([committed]))

        let events = refresh.events(current: current, approvals: [])
        #expect(events.count == 1)
        guard case .message(let message) = events[0] else {
            Issue.record("a tail append must not become a transcript reset")
            return
        }
        #expect(message.messageId == committed.messageId)
    }

    @Test("a post-send cache append keeps the long transcript and paged rows")
    func postSendCacheAppendKeepsTranscript() async {
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
        model.handle(cacheEnvelope(.reset(messages: previous, approvals: []), sequence: 1))
        #expect(model.messages.count == FixtureCatalog.transcriptCount)

        // One paged row and a live preview both belong to the old view; a
        // reset would discard them, an incremental append must not.
        service.historyPageHandler = { _ in
            ConversationHistoryPage(
                source: .online,
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
        let current = previous + [committed]
        let refresh = CachedHistoryRefresh.resolve(previous: previous, current: current)
        for (offset, event) in refresh.events(current: current, approvals: []).enumerated() {
            model.handle(cacheEnvelope(event, sequence: 3 + offset))
        }

        #expect(model.messages.count == FixtureCatalog.transcriptCount + 2)
        #expect(model.messages.first?.messageId == "older-1")
        #expect(model.messages.last?.messageId == "msg-cache-user")
        #expect(!model.messages.contains { $0.messageId.hasPrefix("local-") })
        #expect(model.pagedMessageIds == ["older-1"], "a tail append must not discard paged history")
        #expect(model.historyContinuation == "cursor-older")
        #expect(model.previewText == "streaming", "a tail append must not blank the live reply")
    }
}
