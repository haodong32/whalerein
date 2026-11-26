// Exhaustive state-transition tests for the conversation reducer. No service,
// no UI and no Core: every case constructs its own state and event.

import XCTest
@testable import WhaleRein

final class ConversationReduceTests: XCTestCase {
    // MARK: - Builders

    private func approval(
        _ id: String = "approval-1",
        version: Int = 3,
        state: ApprovalState = .pending
    ) -> Approval {
        Approval(
            approvalId: id,
            sessionId: "s-001",
            toolName: "shell",
            callId: "call-1",
            reason: "Run tests",
            version: version,
            expiresAtUnixMs: 4_000_000_000_000,
            digestHex: "abc123",
            state: state
        )
    }

    private func receipt(_ commandId: String = "cmd-1", status: CommandStatus = .accepted) -> CommandReceipt {
        CommandReceipt(
            commandId: commandId,
            requestId: "req-1",
            status: status,
            delivery: .queue
        )
    }

    private func command(_ id: String = "cmd-1", kind: PendingCommand.Kind = .send(text: "hi")) -> PendingCommand {
        PendingCommand(id: id, kind: kind, submittedAt: Date(timeIntervalSince1970: 0))
    }

    private func queue(available: Bool = true, items: Int = 0) -> QueueSnapshot {
        QueueSnapshot(available: available, running: true, items: [])
    }

    private func capabilities(
        running: Bool = true,
        steer: Bool = true,
        interrupt: Bool = true,
        queueRead: Bool = true,
        historyPage: Bool = true
    ) -> SessionCapabilities {
        SessionCapabilities(
            running: running,
            supportsSteer: steer,
            supportsQueue: queueRead,
            supportsInterrupt: interrupt,
            supportsQueueRead: queueRead,
            supportsHistoryPage: historyPage
        )
    }

    // MARK: - Load

    func testLoadFailureIsOneErrorDimension() {
        let state = ConversationReduce.reduce(.init(), .loadFailed(.hostOffline))
        XCTAssertEqual(state.loadError, .hostOffline)
        XCTAssertFalse(state.isLoaded)
    }

    func testContentLoadedClearsLoadError() {
        let failed = ConversationReduce.reduce(.init(), .loadFailed(.timeout))
        let loaded = ConversationReduce.reduce(failed, .contentLoaded)
        XCTAssertTrue(loaded.isLoaded)
        XCTAssertFalse(loaded.isLoading)
        XCTAssertNil(loaded.loadError)
    }

    func testCapabilitiesRemoveUnsupportedQueue() {
        var state = ConversationState()
        state.queue = .ready(queue())
        let reduced = ConversationReduce.reduce(
            state,
            .capabilitiesLoaded(capabilities(queueRead: false, historyPage: false))
        )
        XCTAssertEqual(reduced.queue, .unavailable)
    }

    // MARK: - Commands

    func testSendLifecycleGoesSubmittedAcceptedDelivered() {
        let pending = command()
        var state = ConversationState()
        state.isLoaded = true
        state.capabilities = capabilities()

        state = ConversationReduce.reduce(state, .sendSubmitted(pending))
        XCTAssertEqual(state.lifecycle, .submitted(pending))
        XCTAssertFalse(state.canSend)

        state = ConversationReduce.reduce(state, .sendAccepted(pending, receipt()))
        XCTAssertEqual(state.lifecycle, .accepted(pending, receipt()))

        state = ConversationReduce.reduce(state, .sendDelivered(receipt()))
        XCTAssertEqual(state.lifecycle, .delivered(receipt()))
        XCTAssertTrue(state.canSend)
    }

    func testSecondSubmissionIsCoalescedWhileInFlight() {
        var state = ConversationState()
        state.isLoaded = true
        state.capabilities = capabilities()
        let first = command("cmd-1")
        let second = command("cmd-2")
        state = ConversationReduce.reduce(state, .sendSubmitted(first))
        state = ConversationReduce.reduce(state, .sendSubmitted(second))
        XCTAssertEqual(state.lifecycle, .submitted(first))
    }

    func testUncertainKeepsTheIntentAndReceipt() {
        var state = ConversationState()
        let pending = command()
        state = ConversationReduce.reduce(state, .sendSubmitted(pending))
        state = ConversationReduce.reduce(state, .sendUncertain(pending, receipt()))
        XCTAssertEqual(state.lifecycle, .uncertain(pending, receipt()))
        XCTAssertFalse(state.lifecycle.isSettled)
        XCTAssertEqual(state.lifecycle.pending, pending)
    }

    func testRejectedIsSettledAndCarriesOneErrorCode() {
        var state = ConversationState()
        let pending = command()
        state = ConversationReduce.reduce(state, .sendSubmitted(pending))
        state = ConversationReduce.reduce(state, .sendRejected(pending, .hostOffline))
        XCTAssertEqual(state.lifecycle, .rejected(pending, .hostOffline))
        XCTAssertTrue(state.lifecycle.isSettled)
        XCTAssertNil(state.lifecycle.receipt)
    }

    func testCommandSettledReleasesAnUncertainCommand() {
        var state = ConversationState()
        let pending = command()
        state = ConversationReduce.reduce(state, .sendSubmitted(pending))
        state = ConversationReduce.reduce(state, .sendUncertain(pending, receipt()))
        XCTAssertEqual(state.lifecycle, .uncertain(pending, receipt()))

        let settled = receipt(status: .confirmed)
        state = ConversationReduce.reduce(state, .commandSettled(pending, settled))
        XCTAssertEqual(state.lifecycle, .delivered(settled))
        XCTAssertEqual(state.lastReceipt, settled)
        XCTAssertTrue(state.lifecycle.isSettled)
        XCTAssertNil(state.commandError)
    }

    func testSettledRejectedReceiptIsNotALocalError() {
        var state = ConversationState()
        let pending = command()
        state = ConversationReduce.reduce(state, .sendSubmitted(pending))
        let rejected = receipt(status: .rejected)
        state = ConversationReduce.reduce(state, .commandSettled(pending, rejected))
        XCTAssertEqual(state.lastReceipt, rejected)
        XCTAssertNil(state.commandError, "a settlement is not a local failure")
    }

    func testLocalCommandErrorUsesOneErrorDimension() {
        var state = ConversationState()
        state = ConversationReduce.reduce(state, .commandErrorRaised(.unsupportedDelivery))
        XCTAssertEqual(state.commandError, .unsupportedDelivery)
        XCTAssertEqual(state.lifecycle, .idle)
    }

    func testLateResultForReplacedCommandIsIgnored() {
        var state = ConversationState()
        let first = command("cmd-1")
        let second = command("cmd-2")
        state = ConversationReduce.reduce(state, .sendSubmitted(first))
        state = ConversationReduce.reduce(state, .sendRejected(first, .timeout))
        state = ConversationReduce.reduce(state, .sendSubmitted(second))
        let stale = ConversationReduce.reduce(state, .sendAccepted(first, receipt("cmd-1")))
        XCTAssertEqual(stale.lifecycle, .submitted(second))
    }

    func testStopUsesTheSameSingleLifecycle() {
        var state = ConversationState()
        let stop = command("stop-1", kind: .stop)
        state = ConversationReduce.reduce(state, .stopSubmitted(stop))
        XCTAssertEqual(state.lifecycle, .submitted(stop))
        state = ConversationReduce.reduce(state, .stopRejected(stop, .unsupportedDelivery))
        XCTAssertEqual(state.lifecycle, .rejected(stop, .unsupportedDelivery))
    }

    func testCanSendRequiresLoadedCapabilitiesAndSettledCommand() {
        var state = ConversationState()
        XCTAssertFalse(state.canSend)
        state.isLoaded = true
        state.capabilities = capabilities(running: true, steer: false)
        XCTAssertFalse(state.canSend, "a running session without steer cannot accept input")
        state.capabilities = capabilities(running: false)
        XCTAssertTrue(state.canSend)
        state.lifecycle = .submitted(command())
        XCTAssertFalse(state.canSend)
    }

    // MARK: - Approvals

    func testApprovalLifecyclePendingResolvingResolved() {
        let pending = approval()
        var state = ConversationReduce.reduce(.init(), .approvalPresented(pending))
        XCTAssertEqual(state.approval, .pending(pending))
        XCTAssertTrue(state.canDecideApproval)

        state = ConversationReduce.reduce(state, .approvalResolving(pending))
        XCTAssertEqual(state.approval, .resolving(pending))
        XCTAssertFalse(state.canDecideApproval)

        state = ConversationReduce.reduce(state, .approvalResolved)
        XCTAssertEqual(state.approval, .none)
    }

    func testRefreshDoesNotResurrectDecidedApproval() {
        var state = ConversationReduce.reduce(.init(), .approvalPresented(approval()))
        state = ConversationReduce.reduce(state, .approvalResolving(approval()))
        state = ConversationReduce.reduce(state, .approvalResolved)
        let refreshed = ConversationReduce.reduce(state, .approvalRefreshed(approval()))
        XCTAssertEqual(refreshed.approval, .none)
    }

    func testRejectedDecisionReturnsToActionable() {
        var state = ConversationReduce.reduce(.init(), .approvalPresented(approval()))
        state = ConversationReduce.reduce(state, .approvalResolving(approval()))
        state = ConversationReduce.reduce(state, .approvalRejected(.staleApproval))
        XCTAssertEqual(state.approval, .pending(approval()))
        XCTAssertTrue(state.canDecideApproval)
    }

    func testExpiredApprovalIsStaleAndNotActionable() {
        var state = ConversationReduce.reduce(.init(), .approvalPresented(approval()))
        state = ConversationReduce.reduce(state, .approvalExpired(approval()))
        XCTAssertEqual(state.approval, .stale(approval()))
        XCTAssertFalse(state.canDecideApproval)
    }

    // MARK: - Wait, queue, history

    func testWaitCannotStartTwiceAndKeepsOutcome() {
        let handle = WaitHandle(id: "wait-1", timeoutMs: 5_000)
        var state = ConversationReduce.reduce(.init(), .waitStarted(handle))
        XCTAssertEqual(state.wait, .active(handle))
        let second = ConversationReduce.reduce(
            state,
            .waitStarted(WaitHandle(id: "wait-2", timeoutMs: 5_000))
        )
        XCTAssertEqual(second.wait, .active(handle))

        state = ConversationReduce.reduce(state, .waitFinished(.inputRequired))
        XCTAssertEqual(state.wait, .finished(.inputRequired))
    }

    func testWaitFinishWithoutStartIsIgnored() {
        let state = ConversationReduce.reduce(.init(), .waitFinished(.timeout))
        XCTAssertEqual(state.wait, .idle)
    }

    func testQueueUnavailableIsDistinctFromEmptyQueue() {
        var state = ConversationReduce.reduce(.init(), .queueLoaded(queue(items: 0)))
        if case .ready = state.queue {} else { XCTFail("expected ready") }
        state = ConversationReduce.reduce(state, .queueUnavailable)
        XCTAssertEqual(state.queue, .unavailable)
    }

    func testHistoryTransitions() {
        var state = ConversationReduce.reduce(.init(), .olderHistoryStarted)
        XCTAssertFalse(state.historyFacet.loading)
        state.historyFacet.continuation = "known-cut"
        state = ConversationReduce.reduce(state, .olderHistoryStarted)
        XCTAssertTrue(state.historyFacet.loading)
        state = ConversationReduce.reduce(state, .historyTraversalInvalidated)
        XCTAssertFalse(state.historyFacet.loading)
        XCTAssertNil(state.historyFacet.continuation)
        XCTAssertEqual(state.historyFacet.traversal, 1)
    }
}
