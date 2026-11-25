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
        messages: [ConversationMessage] = [],
        source: HistorySource = .online
    ) async -> ConversationModel {
        let model = await makeStartedConversation(service: service)
        if !messages.isEmpty {
            model.handle(
                SessionEventEnvelope(
                    hostId: "studio-mac",
                    sessionId: "s-001",
                    sequence: 1,
                    event: .historySnapshot(page(messages, source: source, continuation: "baseline"), replacing: true)
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
            case "baseline":
                // The older page may overlap live rows.
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
        #expect(model.history.provenance == .online)
        #expect(model.history.complete == false)
        #expect(model.messages.map(\.messageId) == ["m3", "m4"])

        await model.loadOlderHistory()
        #expect(model.messages.map(\.messageId) == ["m1", "m2", "m3", "m4"])
        #expect(Set(model.messages.map(\.messageId)).count == 4)
        #expect(model.history.complete)
        #expect(model.scroll.anchorMessageId == "m4")
        #expect(model.historyProvenanceKey == .conversationHistoryOnline)
        #expect(!model.canLoadOlderHistory)
    }

    @Test("cache provenance is explicit when a page comes from the encrypted cache")
    func cacheProvenance() async {
        let service = HistoryStubService()
        let model = await startedModel(service, messages: [message("m2", "two")], source: .cache)
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
        await model.loadOlderHistory()
        #expect(model.history.provenance == .cache)
        #expect(model.historyProvenanceKey == .conversationHistoryCache)
        #expect(model.messages.map(\.messageId) == ["m1", "m2"])
    }

    @Test("a stale continuation reopens observation without dropping readable rows")
    func staleContinuationReopensObservation() async {
        let service = HistoryStubService()
        let model = await startedModel(service, messages: [message("m3", "three")])
        model.draft = "retained draft"
        model.updateScroll(isAtLatest: false)
        model.noteTopVisibleMessage("m3")
        service.historyPageHandler = { _ in
            throw SessionServiceError(.staleHistory)
        }
        await model.loadOlderHistory()
        #expect(service.startWatchCalls.count == 2)
        #expect(model.messages.map(\.messageId) == ["m3"])
        #expect(model.history.continuation == nil)

        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac", sessionId: "s-001", sequence: 2,
                event: .historySnapshot(
                    page([message("m2", "two"), message("m3", "three")], continuation: "refreshed"),
                    replacing: false
                )
            )
        )
        #expect(model.messages.map(\.messageId) == ["m2", "m3"])
        #expect(model.history.continuation == "refreshed")
        #expect(model.scroll.anchorMessageId == "m3")
        #expect(model.draft == "retained draft")
    }

    @Test("initial non-scrollable content gets one automatic page, then asks for continuation", arguments: [false, true])
    func automaticFillBudget(initiallyEmpty: Bool) async {
        let service = HistoryStubService()
        let model = await makeStartedConversation(service: service)
        let initial = initiallyEmpty ? [] : [message("m3", "three")]
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac", sessionId: "s-001", sequence: 1,
                event: .historySnapshot(page(initial, continuation: "baseline"), replacing: true)
            )
        )
        service.historyPageHandler = { continuation in
            if continuation == "baseline" {
                return page([], continuation: "older")
            }
            return page([message("m1", "one")], complete: true)
        }
        await model.updateHistoryViewport(canScroll: false)
        await model.updateHistoryViewport(canScroll: false)
        #expect(service.historyPageCalls == ["baseline"])
        #expect(model.showsHistoryContinuation)
        await model.loadOlderHistory()
        #expect(model.messages.map(\.messageId) == (initiallyEmpty ? ["m1"] : ["m1", "m3"]))
        #expect(!model.showsHistoryContinuation)
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

    @Test("the page arrival keeps the row the reader moved to during the request")
    func keepsCurrentAnchorWhileReadIsHeld() async {
        let service = HistoryStubService()
        let model = await startedModel(service, messages: [message("m2", "two"), message("m3", "three")])
        model.updateScroll(isAtLatest: false)
        model.noteTopVisibleMessage("m3")
        let gate = SuspendedResponse<ConversationHistoryPage>()
        service.historyPageHandler = { _ in try await gate.wait() }
        let load = Task { await model.loadOlderHistory() }
        await gate.waitUntilStarted()

        model.noteTopVisibleMessage("m2")
        model.handle(SessionEventEnvelope(
            hostId: "studio-mac", sessionId: "s-001", sequence: 2,
            event: .replace(message: message("m2", "authoritative"))
        ))
        gate.resume(with: page([message("m1", "one"), message("m2", "older read")], complete: true))
        await load.value

        #expect(model.messages.map(\.messageId) == ["m1", "m2", "m3"])
        #expect(model.messages[1].plainText == "authoritative")
        #expect(model.scroll.anchorMessageId == "m2")
        #expect(!model.scroll.isAtLatest)
    }

    @Test("an older read failure is local and a manual retry reuses the same continuation")
    func retryKeepsTraversalAndRows() async {
        let service = HistoryStubService()
        let model = await startedModel(service, messages: [message("m2", "two")])
        service.historyPageHandler = { _ in throw SessionServiceError(.timeout) }
        await model.loadOlderHistory()

        #expect(model.messages.map(\.messageId) == ["m2"])
        #expect(model.history.errorKey == .errorTimeout)
        #expect(model.history.continuation == "baseline")
        #expect(model.canLoadOlderHistory)
        #expect(service.startWatchCalls.count == 1)

        service.historyPageHandler = { _ in page([message("m1", "one")], complete: true) }
        await model.loadOlderHistory()
        #expect(service.historyPageCalls == ["baseline", "baseline"])
        #expect(model.messages.map(\.messageId) == ["m1", "m2"])
        #expect(model.history.errorKey == nil)
        #expect(model.history.complete)
    }

    @Test("late old-cut pages cannot mutate a same-projection replacement")
    func supersededTraversalIsFenced() async {
        for fails in [false, true] {
            let service = HistoryStubService()
            let model = await startedModel(service, messages: [message("old", "old")])
            model.updateScroll(isAtLatest: false)
            model.noteTopVisibleMessage("old")
            let gate = SuspendedResponse<ConversationHistoryPage>()
            service.historyPageHandler = { _ in try await gate.wait() }
            let load = Task { await model.loadOlderHistory() }
            await gate.waitUntilStarted()

            await model.loadOlderHistory()
            #expect(service.historyPageCalls == ["baseline"])
            model.handle(
                SessionEventEnvelope(
                    hostId: "studio-mac", sessionId: "s-001", sequence: 2,
                    event: .historySnapshot(page([message("latest", "latest")], continuation: "new"), replacing: true)
                )
            )
            if fails {
                gate.fail(with: SessionServiceError(.staleHistory))
            } else {
                gate.resume(with: page([message("older", "older")], complete: true))
            }
            await load.value

            #expect(model.messages.map(\.messageId) == ["latest"])
            #expect(model.history.continuation == "new")
            #expect(!model.history.loading)
            #expect(!model.history.needsFreshTraversal)
            #expect(model.history.errorKey == nil)
            #expect(model.history.positionChanged)
            #expect(model.scroll.isAtLatest)
        }
    }

    @Test("reopening retains a readable window until an online baseline validates its replacement")
    func reopenedCacheDoesNotDisplaceVisibleHistory() async {
        let service = HistoryStubService()
        let model = await startedModel(service, messages: [message("keep", "original"), message("drop", "earlier")])
        model.updateScroll(isAtLatest: false)
        model.noteTopVisibleMessage("keep")
        model.draft = "composing"
        model.invalidateHistoryTraversal()

        model.applyHistorySnapshot(page([message("stale-cache", "cached")], source: .cache), replacing: true)
        #expect(model.messages.map(\.messageId) == ["keep", "drop"])
        #expect(model.history.continuation == nil)

        // This disposition only certifies the handoff from the unread cache
        // seed. It cannot certify a merge with our still-visible window.
        model.applyHistorySnapshot(
            page([message("keep", "canonical"), message("new", "online")], continuation: "validated"),
            replacing: false
        )
        #expect(model.messages.map(\.messageId) == ["keep", "new"])
        #expect(model.messages.first?.plainText == "canonical")
        #expect(model.history.continuation == "validated")
        #expect(model.scroll.anchorMessageId == "keep")
        #expect(!model.history.positionChanged)
        #expect(model.draft == "composing")
    }

    @Test("typed history failure retains readable rows and independent facets")
    func availabilityRetainsReadableWindow() async {
        let service = HistoryStubService()
        let model = await makeStartedConversation(service: service)
        #expect(model.loadState == .loading)

        func deliver(_ event: MessageEvent, _ sequence: Int) {
            model.handle(
                SessionEventEnvelope(hostId: "studio-mac", sessionId: "s-001", sequence: sequence, event: event)
            )
        }
        deliver(.historyUnavailable(.hostOffline), 1)
        #expect(model.loadState == .failed(.hostOffline))
        deliver(.historySnapshot(page([], complete: true), replacing: true), 2)
        #expect(model.loadState == .loaded && model.messages.isEmpty)
        deliver(.message(message("retained", "retained")), 3)
        deliver(.historyUnavailable(.timeout), 4)
        #expect(model.loadState == .loaded)
        #expect(model.messages.map(\.messageId) == ["retained"])
        #expect(model.history.errorKey == .errorTimeout)
    }

    @Test("same-session stale snapshot, error and terminal events are fenced")
    func staleEventsAreRejectedBeforeTheirSequence() async {
        let service = HistoryStubService()
        let model = await makeStartedConversation(service: service)
        let scope = SessionContext(hostId: "studio-mac", sessionId: "s-001")
        service.acceptsWatchEventHandler = { $0.belongsToActiveWatch(generation: 2, scope: scope) }
        let events: [(MessageEvent, Int, UInt64?)] = [
            (.historySnapshot(page([message("old", "old")], complete: true), replacing: true), 98, 1),
            (.historyUnavailable(.unavailable), 99, 1),
            (.watchEnded(reason: "old"), 100, 1),
            (.historySnapshot(page([message("new", "new")], complete: true), replacing: true), 1, 2),
            (.runtimeChanged(running: true), 2, nil)
        ]
        for (event, sequence, generation) in events {
            model.handle(
                SessionEventEnvelope(
                    hostId: "studio-mac", sessionId: "s-001", sequence: sequence,
                    event: event, watchGeneration: generation
                )
            )
        }
        #expect(model.messages.map(\.messageId) == ["new"])
        #expect(!model.watchEnded)
        #expect(model.history.errorKey == nil)
        #expect(model.capabilities.running)
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
        #expect(model.history.needsFreshTraversal)
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
