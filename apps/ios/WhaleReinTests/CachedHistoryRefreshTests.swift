import Foundation
import Testing
@testable import WhaleRein

private func cachedMessage(_ id: String, text: String? = nil) -> ConversationMessage {
    ConversationMessage(
        messageId: id,
        role: .assistant,
        blocks: [
            ConversationBlock(blockId: "\(id)-block-0", kind: .text, text: text ?? id)
        ],
        createdAtUnixMs: 1
    )
}

@Suite("Cached history refresh")
struct CachedHistoryRefreshTests {
    @Test("an appended committed message stays incremental")
    func appendedMessageDoesNotReplaceTheTranscript() {
        let previous = [cachedMessage("m1"), cachedMessage("m2")]
        let current = previous + [cachedMessage("m3")]

        #expect(
            CachedHistoryRefresh.resolve(previous: previous, current: current)
                == .appended([cachedMessage("m3")])
        )
    }

    @Test("an unchanged cache list is ignored")
    func unchangedCacheList() {
        let previous = [cachedMessage("m1"), cachedMessage("m2")]

        #expect(
            CachedHistoryRefresh.resolve(previous: previous, current: previous)
                == .unchanged
        )
    }

    @Test("the cold seed still replaces the empty transcript")
    func coldSeedReplaces() {
        let current = [cachedMessage("m1"), cachedMessage("m2")]

        #expect(
            CachedHistoryRefresh.resolve(previous: [], current: current)
                == .replaced
        )
    }

    @Test("removal or reordering still replaces the transcript")
    func nonPrefixChangeReplaces() {
        let previous = [cachedMessage("m1"), cachedMessage("m2")]

        #expect(
            CachedHistoryRefresh.resolve(
                previous: previous,
                current: [cachedMessage("m2"), cachedMessage("m3")]
            ) == .replaced
        )
        #expect(
            CachedHistoryRefresh.resolve(
                previous: previous,
                current: [cachedMessage("m1")]
            ) == .replaced
        )
    }

    @Test("projection never turns a tail append into a reset")
    func projectionOfAppendIsIncremental() {
        let committed = cachedMessage("m3")
        let refresh = CachedHistoryRefresh.resolve(
            previous: [cachedMessage("m1"), cachedMessage("m2")],
            current: [cachedMessage("m1"), cachedMessage("m2"), committed]
        )

        let events = refresh.events(current: [cachedMessage("m1"), cachedMessage("m2"), committed], approvals: [])
        #expect(events.count == 1)
        guard case .message(let message) = events[0] else {
            Issue.record("a tail append must project to a message event")
            return
        }
        #expect(message.messageId == "m3")
    }

    @Test("projection of a replacement is one reset")
    func projectionOfReplacementIsReset() {
        let current = [cachedMessage("m2")]
        let refresh = CachedHistoryRefresh.resolve(
            previous: [cachedMessage("m1"), cachedMessage("m2")],
            current: current
        )

        let events = refresh.events(current: current, approvals: [])
        #expect(events.count == 1)
        guard case .reset(let messages, _) = events[0] else {
            Issue.record("a replacement must project to a reset")
            return
        }
        #expect(messages.map(\.messageId) == ["m2"])
    }
}
