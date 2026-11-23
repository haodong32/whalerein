// Conversation recovery: watch-ended reconnect, clock-driven approval expiry,
// offline and concurrent approval decisions, retry without duplicate
// subscriptions, and refresh semantics for concurrent loads and the cached
// banner.

import Foundation
import Testing
@testable import WhaleRein

@MainActor
private final class MutableDate {
    var date: Date
    init(_ date: Date) {
        self.date = date
    }
}

@Suite("Conversation recovery")
@MainActor
struct ConversationRecoveryTests {
    private func envelope(_ event: MessageEvent, sequence: Int) -> SessionEventEnvelope {
        SessionEventEnvelope(hostId: "studio-mac", sessionId: "s-001", sequence: sequence, event: event)
    }

    private func hostEnvelope(state: String, code: String? = nil) -> SessionEventEnvelope {
        SessionEventEnvelope(
            hostId: "studio-mac",
            sessionId: "",
            sequence: 1,
            event: .hostConnection(
                hostId: "studio-mac",
                state: state,
                code: code,
                sinceUnixMs: 0
            )
        )
    }

    // MARK: - IOS-07 watch recovery

    @Test("a watch-ended conversation disables Send and reconnects on demand")
    func watchEndedRecovery() async {
        let service = StubSessionService()
        let model = await makeStartedConversation(service: service)
        model.handle(envelope(.reset(messages: [], approvals: []), sequence: 1))
        model.draft = "queued text"
        #expect(model.canSend)

        model.handle(envelope(.watchEnded(reason: "ended"), sequence: 2))
        #expect(model.watchEnded)
        #expect(!model.canSend, "Send must be disabled while the watch has ended")
        // A session watch ending does not make the Host connection offline.
        #expect(model.connectivity.status(for: "studio-mac") == .connected)

        await model.reconnectWatch()

        #expect(!model.watchEnded)
        #expect(service.startWatchCalls.count == 2)
        #expect(model.lastAppliedSequence == -1)
        // Reconnecting a watch does not fabricate a Host status either.
        #expect(model.connectivity.status(for: "studio-mac") == .connected)

        model.handle(envelope(.reset(messages: [], approvals: []), sequence: 1))
        #expect(model.loadState == .loaded)
        #expect(model.canSend)
    }

    // MARK: - IOS-08 / APPR-16 approval expiry

    @Test("the approval timer refreshes at the deadline and the Host removes the request")
    func timerDrivenExpiry() async {
        let service = StubSessionService()
        let model = await makeStartedConversation(service: service)
        let approval = testApproval(expiresAt: Date().addingTimeInterval(0.2))
        service.approvalsHandler = { _, _ in [] }

        model.handle(envelope(.reset(messages: [], approvals: [approval]), sequence: 1))
        #expect(model.isApprovalActionable(approval))

        await waitUntil(timeout: 5) { model.approvals.first?.state == .expired }
        #expect(model.approvals.first?.state == .expired)
        #expect(model.approvalRefreshCount >= 1, "Expiry must advance on a clock, not only on redraw")
        #expect(!model.isApprovalActionable(model.approvals[0]))
    }

    @Test("a fast device clock does not disable a request the Host still reports pending")
    func fastClockDoesNotDisableValidApproval() async {
        let service = StubSessionService()
        let clock = MutableDate(Date().addingTimeInterval(600))
        let model = ConversationModel(
            hostId: "studio-mac",
            sessionId: "s-001",
            sessionTitle: "Release checklist",
            service: service,
            connectivity: makeTestConnectivity(),
            now: { clock.date }
        )
        await model.start()
        await pump(40)

        let approval = testApproval(expiresAt: Date().addingTimeInterval(-60))
        model.handle(envelope(.reset(messages: [], approvals: [approval]), sequence: 1))
        #expect(model.isApprovalExpired(approval), "Without Host confirmation the local clock still hides the decision")

        service.approvalsHandler = { _, _ in [approval] }
        await model.refreshApprovals()

        #expect(!model.isApprovalExpired(approval))
        #expect(model.isApprovalActionable(approval))

        service.resolveHandler = { _, _, id, _, _, allow in
            var updated = approval
            updated.state = allow ? .allowed : .rejected
            _ = id
            return updated
        }
        await model.resolveApproval(approval, allow: true)
        #expect(service.resolveCalls.count == 1)
        #expect(model.approvals.first?.state == .allowed)
    }

    // MARK: - IOS-09 approval actionability

    @Test("offline decisions are disabled with a reason")
    func offlineApprovalDisabled() async {
        let service = StubSessionService()
        let model = await makeStartedConversation(service: service)
        let approval = testApproval()
        model.handle(envelope(.reset(messages: [], approvals: [approval]), sequence: 1))
        #expect(model.isApprovalActionable(approval))

        model.connectivity.handle(hostEnvelope(state: "offline"))
        #expect(!model.isApprovalActionable(approval))
        #expect(model.approvalActionDisabledReasonKey == .approvalOffline)

        await model.resolveApproval(approval, allow: true)
        #expect(service.resolveCalls.isEmpty, "An offline decision must not be sent")
        #expect(model.approvalErrorKey == .approvalOffline)

        model.connectivity.handle(hostEnvelope(state: "connected"))
        #expect(model.isApprovalActionable(approval))
    }

    @Test("a decision in flight blocks every other approval card")
    func concurrentApprovalsBlocked() async {
        let service = StubSessionService()
        let first = testApproval(id: "approval-a", digest: "digest-a")
        let second = testApproval(id: "approval-b", digest: "digest-b")
        service.resolveHandler = { _, _, _, _, _, allow in
            try? await Task.sleep(for: .milliseconds(60))
            var updated = first
            updated.state = allow ? .allowed : .rejected
            return updated
        }
        let model = await makeStartedConversation(service: service)
        model.handle(envelope(.reset(messages: [], approvals: [first, second]), sequence: 1))

        let task = Task { await model.resolveApproval(first, allow: true) }
        await waitUntil { model.resolvingApprovalId == "approval-a" }

        #expect(!model.isApprovalActionable(second))
        #expect(model.approvalActionDisabledReasonKey == .approvalBusy)

        await task.value
        #expect(model.resolvingApprovalId == nil)
        #expect(model.isApprovalActionable(second))
    }

    // MARK: - IOS-11 retry without duplicate subscription

    @Test("load failure then retry succeeds without creating a second event subscription")
    func retryLoadSingleSubscription() async {
        let service = StubSessionService()
        var attempts = 0
        service.startWatchHandler = { _, _ in
            attempts += 1
            if attempts == 1 {
                throw SessionServiceError(.timeout)
            }
        }
        let model = await makeStartedConversation(service: service)

        #expect(model.loadState == .failed(.timeout))
        #expect(model.errorKey == .errorTimeout)
        #expect(service.eventStreamCount == 1)

        await model.start()

        #expect(model.loadState != .failed(.timeout))
        #expect(service.startWatchCalls.count == 2)
        #expect(service.eventStreamCount == 1, "A retry must reuse the existing consumer")
        #expect(service.activeStreamCount == 1)

        let message = ConversationMessage(
            messageId: "msg-retry",
            role: .assistant,
            blocks: [ConversationBlock(blockId: "msg-retry-block-0", kind: .text, text: "recovered")],
            createdAtUnixMs: 1
        )
        service.emit(.reset(messages: [message], approvals: []), sequence: 1)
        await waitUntil { model.messages.map(\.messageId) == ["msg-retry"] }
        #expect(model.messages.map(\.messageId) == ["msg-retry"])
        #expect(model.loadState == .loaded)
    }

    @Test("closing a conversation terminates its subscription")
    func closeTerminatesSubscription() async {
        let service = StubSessionService()
        let model = await makeStartedConversation(service: service)
        #expect(service.activeStreamCount == 1)

        model.close()
        await waitUntil { service.activeStreamCount == 0 }
        #expect(service.activeStreamCount == 0)
    }

    // MARK: - IOS-12 refresh semantics

    @Test("a concurrent Host refresh is coalesced into a second fetch instead of being dropped")
    func concurrentHostRefresh() async {
        let service = StubSessionService()
        service.hostsHandler = {
            try? await Task.sleep(for: .milliseconds(80))
            return FixtureCatalog.hosts
        }
        let model = HostListModel(
            service: service,
            preferences: PreferencesStore(directory: makeTempDirectory())
        )

        async let first: Void = model.load()
        try? await Task.sleep(for: .milliseconds(10))
        async let second: Void = model.load()
        _ = await (first, second)

        #expect(service.hostsFetchCount == 2)
        #expect(model.hosts.count == 2)
        #expect(model.loadState == .loaded)
    }

    @Test("a newer Host session load wins over an older in-flight load")
    func concurrentSessionLoadUsesLatestHost() async {
        let service = StubSessionService()
        service.sessionsHandler = { hostId in
            try? await Task.sleep(for: .milliseconds(80))
            return FixtureCatalog.sessions(hostId: hostId)
        }
        let model = SessionListModel(
            service: service,
            preferences: PreferencesStore(directory: makeTempDirectory())
        )

        async let first: Void = model.load(hostId: "studio-mac")
        try? await Task.sleep(for: .milliseconds(10))
        async let second: Void = model.load(hostId: "travel-laptop")
        _ = await (first, second)

        #expect(service.sessionsFetchCount == 2)
        #expect(model.hostId == "travel-laptop")
        #expect(model.sessions.map(\.sessionId) == ["s-101", "s-102"])
        #expect(model.loadState == .loaded)
    }

    @Test("the cached banner clears after a live refresh recovers")
    func cachedBannerClearsAfterRecovery() async {
        let service = StubSessionService()
        service.sessionsHandler = { _ in throw SessionServiceError(.hostOffline) }
        service.cachedSessionsHandler = { hostId in FixtureCatalog.sessions(hostId: hostId) }
        let directory = makeTempDirectory()
        let preferences = PreferencesStore(directory: directory)
        preferences.update {
            $0.paired = true
            $0.lastHostId = "travel-laptop"
            $0.lastSessionId = "s-102"
        }
        let app = AppShellModel(
            service: service,
            preferences: preferences,
            dataDirectory: directory,
            options: testLaunchOptions()
        )

        await app.bootstrapConversation()
        #expect(app.isOfflineCached)
        #expect(app.sessionList.isShowingCached)
        #expect(app.sessionList.hostId == "travel-laptop")

        service.sessionsHandler = { hostId in FixtureCatalog.sessions(hostId: hostId) }
        await app.refreshSwitcher()

        #expect(!app.sessionList.isShowingCached)
        #expect(!app.isOfflineCached, "The cached banner must clear once live sessions load")
    }

    @Test("a cached session list clears its own indicator on a successful reload")
    func cachedIndicatorClears() async {
        let service = StubSessionService()
        service.sessionsHandler = { _ in throw SessionServiceError(.hostOffline) }
        let model = SessionListModel(
            service: service,
            preferences: PreferencesStore(directory: makeTempDirectory())
        )

        await model.load(hostId: "travel-laptop")
        #expect(model.isShowingCached)

        service.sessionsHandler = { hostId in FixtureCatalog.sessions(hostId: hostId) }
        await model.load(hostId: "travel-laptop")
        #expect(!model.isShowingCached)
        #expect(model.loadState == .loaded)
    }
}
