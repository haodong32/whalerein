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

    @Test("the user's own message always returns to the latest output")
    func ownMessageFollows() {
        var state = ScrollAnchorState()
        state.update(isAtLatest: false)
        let follows = state.recordIncoming(isOwnMessage: true)
        #expect(follows)
        #expect(state.isAtLatest)
        #expect(state.unreadCount == 0)
        #expect(state.anchorMessageId == nil)
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
