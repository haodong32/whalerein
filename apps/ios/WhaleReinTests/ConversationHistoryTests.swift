// Older-history paging, scroll anchoring, deduplication, exact reread and
// typed content behavior. Covers the presentation seam with deterministic
// service doubles: no live Host or hardware is required.

import Foundation
import Testing
@testable import WhaleRein

@MainActor
final class HistoryStubService: StubSessionService, ConversationHistoryService {
    var historyPageHandler: ((String?) async throws -> ConversationHistoryPage)?
    var exactHandler: ((MessageReference) async throws -> ConversationMessage)?
    private(set) var historyPageCalls: [String?] = []
    private(set) var exactCalls: [MessageReference] = []

    func historyPage(
        hostId: String,
        sessionId: String,
        source: HistorySource,
        continuation: String?,
        maxMessages: Int,
        maxBytes: Int
    ) async throws -> ConversationHistoryPage {
        historyPageCalls.append(continuation)
        guard let historyPageHandler else {
            throw SessionServiceError(.unavailable)
        }
        return try await historyPageHandler(continuation)
    }

    func historyMessage(_ reference: MessageReference) async throws -> ConversationMessage {
        exactCalls.append(reference)
        guard let exactHandler else {
            throw SessionServiceError(.unavailable)
        }
        return try await exactHandler(reference)
    }
}

private func message(_ id: String, _ text: String, role: MessageRole = .assistant) -> ConversationMessage {
    ConversationMessage(
        messageId: id,
        role: role,
        blocks: [ConversationBlock(blockId: "\(id)-block-0", kind: .text, text: text)],
        createdAtUnixMs: 1
    )
}

private func page(
    _ messages: [ConversationMessage],
    source: HistorySource = .online,
    projection: String = "projection-1",
    complete: Bool = false,
    continuation: String? = nil
) -> ConversationHistoryPage {
    ConversationHistoryPage(
        source: source,
        projection: projection,
        complete: complete,
        messages: messages,
        activities: [],
        continuation: continuation
    )
}

private func reference(
    id: String,
    projection: String = "projection-1"
) -> MessageReference {
    MessageReference(
        hostId: "studio-mac",
        sessionId: "s-001",
        messageId: id,
        sourceLocator: "locator-\(id)",
        projectionId: projection
    )
}

@Suite("Conversation history")
@MainActor
struct ConversationHistoryTests {
    private func startedModel(
        _ service: HistoryStubService,
        messages: [ConversationMessage] = []
    ) async -> ConversationModel {
        let model = await makeStartedConversation(service: service)
        if !messages.isEmpty {
            model.handle(
                SessionEventEnvelope(
                    hostId: "studio-mac",
                    sessionId: "s-001",
                    sequence: 1,
                    event: .reset(messages: messages, approvals: [])
                )
            )
        }
        return model
    }

    @Test("older pages prepend in order, deduplicate live rows and keep the anchor")
    func pagesPrependInOrder() async {
        let service = HistoryStubService()
        let model = await startedModel(
            service,
            messages: [message("m3", "three"), message("m4", "four")]
        )
        model.updateScroll(isAtLatest: false)
        model.noteTopVisibleMessage("m4")

        service.historyPageHandler = { continuation in
            switch continuation {
            case nil:
                // The latest page overlaps the live rows.
                return page(
                    [message("m3", "three"), message("m4", "four")],
                    continuation: "cursor-1"
                )
            case "cursor-1":
                return page(
                    [message("m1", "one"), message("m2", "two")],
                    complete: true
                )
            default:
                throw SessionServiceError(.staleHistory)
            }
        }

        await model.loadOlderHistory()
        #expect(model.historyProvenance == .online)
        #expect(model.historyComplete == false)
        #expect(model.messages.map(\.messageId) == ["m3", "m4"])

        await model.loadOlderHistory()
        #expect(model.messages.map(\.messageId) == ["m1", "m2", "m3", "m4"])
        #expect(Set(model.messages.map(\.messageId)).count == 4)
        #expect(model.historyComplete)
        #expect(model.historyPrepend?.anchorMessageId == "m4")
        #expect(model.historyProvenanceKey == .conversationHistoryOnline)
        #expect(!model.canLoadOlderHistory)
    }

    @Test("cache provenance is explicit when a page comes from the encrypted cache")
    func cacheProvenance() async {
        let service = HistoryStubService()
        let model = await startedModel(service, messages: [message("m2", "two")])
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 2,
                event: .watchEnded(reason: "offline")
            )
        )
        service.historyPageHandler = { _ in
            page([message("m1", "one")], source: .cache, complete: true)
        }
        #expect(model.canLoadOlderHistory)
        await model.loadOlderHistory(cacheFallback: true)
        #expect(model.historyProvenance == .cache)
        #expect(model.historyProvenanceKey == .conversationHistoryCache)
        #expect(model.messages.map(\.messageId) == ["m1", "m2"])
    }

    @Test("a stale continuation offers a fresh traversal without applying old content")
    func staleContinuationOffersFreshTraversal() async {
        let service = HistoryStubService()
        let model = await startedModel(service, messages: [message("m3", "three")])
        service.historyPageHandler = { _ in
            throw SessionServiceError(.staleHistory)
        }
        await model.loadOlderHistory()
        #expect(model.historyNeedsFreshTraversal)
        #expect(model.canLoadFreshHistory)
        #expect(model.messages.map(\.messageId) == ["m3"])

        service.historyPageHandler = { _ in
            page(
                [message("m1", "one"), message("m2", "two"), message("m3", "three")],
                complete: true
            )
        }
        await model.loadFreshHistory()
        #expect(!model.historyNeedsFreshTraversal)
        #expect(model.messages.map(\.messageId) == ["m1", "m2", "m3"])
    }

    @Test("a page that returns after the conversation closed is dropped")
    func latePageAfterCloseIsDropped() async throws {
        let service = HistoryStubService()
        let model = await startedModel(service, messages: [message("m2", "two")])
        let gate = SuspendedResponse<ConversationHistoryPage>()
        service.historyPageHandler = { _ in
            try await gate.wait()
        }
        let load = Task { await model.loadOlderHistory() }
        await gate.waitUntilStarted()
        model.close()
        gate.resume(with: page([message("m1", "one")], complete: true))
        await load.value
        #expect(model.messages.map(\.messageId) == ["m2"])
    }

    @Test("exact reread replaces the authorized message and rejects a stale projection")
    func exactRereadPreservesScope() async {
        let service = HistoryStubService()
        let model = await startedModel(service, messages: [message("m2", "two")])
        service.historyPageHandler = { _ in
            page([message("m1", "one"), message("m2", "two")], complete: true)
        }
        await model.loadOlderHistory()

        let updated = message("m2", "two, canonical")
        service.exactHandler = { _ in updated }
        await model.rereadMessage(reference(id: "m2"))
        #expect(model.messages.last?.plainText == "two, canonical")
        #expect(service.exactCalls.count == 1)

        let before = model.messages
        await model.rereadMessage(reference(id: "m2", projection: "projection-old"))
        #expect(model.historyNeedsFreshTraversal)
        #expect(model.messages == before)
        #expect(service.exactCalls.count == 1)
    }

    @Test("reasoning is hidden by default and shown only when enabled")
    func reasoningVisibilityFilter() {
        let blocks = [
            ConversationBlock(blockId: "text", kind: .text, text: "answer"),
            ConversationBlock(blockId: "reasoning", kind: .reasoning, text: "thinking"),
            ConversationBlock(
                blockId: "attachment",
                kind: .attachment,
                text: "report.txt",
                attachment: AttachmentBlock(
                    mediaType: "text/plain",
                    name: "report.txt",
                    size: 12,
                    available: true,
                    attachmentRef: "attachment-1"
                )
            )
        ]
        let hidden = ConversationContent.visibleBlocks(blocks, reasoningVisible: false)
        #expect(hidden.map(\.blockId) == ["text", "attachment"])
        let shown = ConversationContent.visibleBlocks(blocks, reasoningVisible: true)
        #expect(shown.map(\.blockId) == ["text", "reasoning", "attachment"])
    }

    @Test("reasoning preview is typed, hidden with its key, and settles on the committed message")
    func reasoningPreviewSettles() async {
        let service = HistoryStubService()
        let model = await startedModel(service, messages: [message("m1", "one")])
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 2,
                event: .reasoningPreview(
                    attemptId: "attempt-1",
                    text: "considering",
                    replace: true,
                    incomplete: true
                )
            )
        )
        #expect(model.reasoningPreviewText == "considering")

        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 3,
                event: .message(
                    ConversationMessage(
                        messageId: "m2",
                        role: .assistant,
                        blocks: [
                            ConversationBlock(blockId: "m2-r", kind: .reasoning, text: "considering"),
                            ConversationBlock(blockId: "m2-t", kind: .text, text: "settled")
                        ],
                        createdAtUnixMs: 2
                    )
                )
            )
        )
        #expect(model.reasoningPreviewText == nil)
        #expect(model.messages.last?.messageId == "m2")
    }

    @Test("live tool activity settles only when its committed block arrives")
    func liveToolActivitySettles() async {
        let service = HistoryStubService()
        let model = await startedModel(service, messages: [message("m1", "one")])
        let activity = ToolActivity(
            callId: "call-1",
            name: "bash",
            summary: "./repo test core",
            state: .running,
            stages: ["preparation", "execution"],
            progress: "running 12 tests",
            failureCode: nil,
            result: [],
            callMissing: false
        )
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 2,
                event: .toolActivity(activity)
            )
        )
        #expect(model.visibleLiveToolActivities.map(\.callId) == ["call-1"])

        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 3,
                event: .message(
                    ConversationMessage(
                        messageId: "m2",
                        role: .assistant,
                        blocks: [
                            ConversationBlock(
                                blockId: "m2-tool",
                                kind: .tool,
                                text: "settled",
                                toolName: "bash",
                                toolActivity: ToolActivity(
                                    callId: "call-1",
                                    name: "bash",
                                    summary: "./repo test core",
                                    state: .succeeded,
                                    stages: ["settlement"],
                                    progress: nil,
                                    failureCode: nil,
                                    result: [.text("ok")],
                                    callMissing: false
                                )
                            )
                        ],
                        createdAtUnixMs: 3
                    )
                )
            )
        )
        #expect(model.visibleLiveToolActivities.isEmpty)
    }

    @Test("committed fixture messages keep typed tool structure")
    func committedToolStructure() {
        let messages = FixtureCatalog.conversation(sessionId: "s-001", now: Date())
        let toolMessage = messages.first { $0.role == .tool }
        let block = toolMessage?.blocks.first
        #expect(block?.kind == .tool)
        #expect(block?.toolActivity?.callId.isEmpty == false)
        #expect(block?.toolActivity?.name == "bash")
        #expect(block?.toolActivity?.stages.contains("execution") == true)
        #expect(block?.toolActivity?.result.isEmpty == false)
    }
}
