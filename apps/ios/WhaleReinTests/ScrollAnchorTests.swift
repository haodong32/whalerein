// Reading-position and jump-to-latest behavior while output streams.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Scroll anchoring")
@MainActor
struct ScrollAnchorTests {
    @Test("new output follows only when the reader is at the latest row")
    func followsOnlyAtLatest() {
        var state = ScrollAnchorState()
        let followsFromLatest = state.recordIncoming(isOwnMessage: false)
        #expect(followsFromLatest)
        #expect(state.unreadCount == 0)

        state.update(isAtLatest: false)
        state.noteTopVisibleMessage("msg-0042")
        let followsWhileReading = state.recordIncoming(isOwnMessage: false)
        #expect(!followsWhileReading)
        #expect(state.unreadCount == 1)
        #expect(state.anchorMessageId == "msg-0042")

        let followsAgain = state.recordIncoming(isOwnMessage: false)
        #expect(!followsAgain)
        #expect(state.unreadCount == 2)
        #expect(state.anchorMessageId == "msg-0042")
    }

    @Test("the user's own message preserves earlier reading")
    func ownMessagePreservesReading() {
        var state = ScrollAnchorState()
        state.update(isAtLatest: false)
        state.noteTopVisibleMessage("previous")
        let follows = state.recordIncoming(isOwnMessage: true)
        #expect(!follows)
        #expect(!state.isAtLatest)
        #expect(state.unreadCount == 0)
        #expect(state.anchorMessageId == "previous")
    }

    @Test("a prepend preserves partial row offset and uses a visible neighbor if its anchor disappears")
    func preservesMeasuredOffset() {
        let anchors = [
            TranscriptRowPosition(id: "first", top: -31, bottom: 53),
            TranscriptRowPosition(id: "neighbor", top: 67, bottom: 134)
        ]
        #expect(TranscriptRowPosition.correction(
            anchors: anchors,
            current: [TranscriptRowPosition(id: "first", top: 189, bottom: 273)]
        ) == 220)
        #expect(TranscriptRowPosition.correction(
            anchors: anchors,
            current: [TranscriptRowPosition(id: "neighbor", top: 4, bottom: 71)]
        ) == -63)
        #expect(TranscriptRowPosition.correction(anchors: anchors, current: []) == nil)
    }

    @Test("restoring a reading offset clamps at both bounds and skips no-op scrolls")
    func restorationClampsWithoutFeedback() {
        #expect(TranscriptRowPosition.restorationOffset(from: 0, maximum: 100, correction: -16) == nil)
        #expect(TranscriptRowPosition.restorationOffset(from: 100, maximum: 100, correction: 24) == nil)
        #expect(TranscriptRowPosition.restorationOffset(from: 45, maximum: 100, correction: 72) == 100)
        #expect(TranscriptRowPosition.restorationOffset(from: 45, maximum: 100, correction: 0.2) == nil)
    }

    @Test("a removed terminal row reserves only the space needed to keep the reading position")
    func terminalRemovalCanKeepItsAnchor() {
        let reserve = TranscriptRowPosition.trailingSpace(from: 400, maximum: 400, correction: 276)
        #expect(reserve == 276)
        #expect(TranscriptRowPosition.restorationOffset(
            from: 400, maximum: 400 + reserve, correction: 276
        ) == 676)
        #expect(TranscriptRowPosition.trailingSpace(from: 400, maximum: 800, correction: 276) == 0)
        #expect(TranscriptRowPosition.trailingSpace(from: 100, maximum: 90, correction: -4) == 0)
    }

    @Test("jumping to latest clears unread state and the anchor")
    func jumpResets() {
        var state = ScrollAnchorState()
        state.update(isAtLatest: false)
        state.noteTopVisibleMessage("msg-0100")
        _ = state.recordIncoming(isOwnMessage: false)
        state.jumpToLatest()
        #expect(state.isAtLatest)
        #expect(state.unreadCount == 0)
        #expect(state.anchorMessageId == nil)
    }

    @Test("the fifty-first row and a viewport resize preserve the same fractional offset")
    func preservesRowBoundaryAndInsetChange() async {
        let service = StubSessionService()
        let model = await makeStartedConversation(service: service)
        let messages = (0..<50).map { index in
            ConversationMessage(messageId: "row-\(index)", role: .assistant, blocks: [], createdAtUnixMs: Int64(index))
        }
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac", sessionId: "s-001", sequence: 1,
                event: .reset(messages: messages, approvals: [])
            )
        )
        model.updateScroll(isAtLatest: false)
        model.noteTopVisibleMessage("row-27")
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac", sessionId: "s-001", sequence: 2,
                event: .message(ConversationMessage(
                    messageId: "row-50", role: .assistant, blocks: [], createdAtUnixMs: 50
                ))
            )
        )
        #expect(model.messages.count == 51)
        #expect(model.scroll.anchorMessageId == "row-27")
        #expect(!model.scroll.isAtLatest)
        let previous = TranscriptRowPosition(id: "row-27", top: -23.5, bottom: 44)
        let afterResize = TranscriptRowPosition(id: "row-27", top: 36.5, bottom: 104)
        #expect(TranscriptRowPosition.correction(anchors: [previous], current: [afterResize]) == 60)
    }

    @Test("streaming previews do not move a reader who scrolled up")
    func previewDoesNotMoveReader() async {
        let service = StubSessionService()
        let model = await makeStartedConversation(service: service)
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 1,
                event: .reset(messages: [], approvals: [])
            )
        )
        #expect(model.scroll.isAtLatest)

        model.updateScroll(isAtLatest: false)
        model.noteTopVisibleMessage("msg-0100")
        let requestBefore = model.scrollRequest?.id

        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 2,
                event: .preview(attemptId: "a", text: "new output", replace: true, incomplete: true)
            )
        )
        #expect(model.livePreview == "new output")
        #expect(model.scroll.isAtLatest == false)
        #expect(model.scroll.unreadCount == 1)
        #expect(model.scroll.anchorMessageId == "msg-0100")
        #expect(model.scrollRequest?.id == requestBefore)
        #expect(model.showJumpToLatest)
    }

    @Test("jump-to-latest issues a scroll request and clears unread state")
    func jumpRequestsScroll() async {
        let service = StubSessionService()
        let model = await makeStartedConversation(service: service)
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 1,
                event: .reset(messages: [], approvals: [])
            )
        )
        model.updateScroll(isAtLatest: false)
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 2,
                event: .preview(attemptId: "a", text: "chunk", replace: true, incomplete: true)
            )
        )
        let before = model.scrollRequest?.id
        model.jumpToLatest()

        #expect(model.scroll.isAtLatest)
        #expect(model.scroll.unreadCount == 0)
        #expect((model.scrollRequest?.id ?? 0) > (before ?? 0))
        #expect(!model.showJumpToLatest)
    }

    @Test("expansion, focus state and the draft survive streaming updates")
    func stateSurvivesStreaming() async {
        let service = StubSessionService()
        let model = await makeStartedConversation(service: service)
        let toolMessage = ConversationMessage(
            messageId: "msg-tool",
            role: .tool,
            blocks: [
                ConversationBlock(blockId: "block-tool", kind: .tool, text: "long output", toolName: "bash")
            ],
            createdAtUnixMs: 1
        )
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 1,
                event: .reset(messages: [toolMessage], approvals: [])
            )
        )
        model.toggleExpansion("block-tool")
        model.draft = "half typed"
        #expect(model.isExpanded("block-tool"))

        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 2,
                event: .preview(attemptId: "a", text: "chunk", replace: true, incomplete: true)
            )
        )
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 3,
                event: .message(
                    ConversationMessage(
                        messageId: "msg-new",
                        role: .assistant,
                        blocks: [ConversationBlock(blockId: "b", kind: .text, text: "done")],
                        createdAtUnixMs: 2
                    )
                )
            )
        )

        #expect(model.isExpanded("block-tool"))
        #expect(model.draft == "half typed")
        #expect(model.messages.count == 2)
    }
}
