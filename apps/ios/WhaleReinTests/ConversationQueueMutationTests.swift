// Queue mutation outcomes: confirmed, unconfirmed and rejected edit/withdraw/
// promote receipts. The controller must keep an unsaved edit recoverable while
// the result is unknown, refresh the authoritative Host queue before treating
// an ambiguous change as applied, and surface a rejection instead of clearing
// the buffer as if it succeeded.

import Foundation
import Testing
@testable import WhaleRein

@MainActor
private func queueItem(
    id: String = "queue-1",
    blocks: [QueueBlock] = [.text("original text")],
    actions: [QueueAction] = [.edit, .remove, .promote]
) -> QueueItem {
    QueueItem(
        itemID: id,
        placement: "queued",
        deliveryID: "delivery-\(id)",
        blocks: blocks,
        actions: actions
    )
}

@MainActor
private func currentQueue(_ items: [QueueItem], running: Bool = true) -> QueueSnapshot {
    QueueSnapshot(available: true, running: running, items: items)
}

@MainActor
private func queueMutation(
    itemID: String = "queue-1",
    action: QueueAction,
    text: String = "",
    status: CommandStatus
) -> QueueMutation {
    let receipt = CommandReceipt(
        commandId: "cmd-\(action.rawValue)",
        requestId: "req-\(action.rawValue)",
        status: status,
        delivery: nil
    )
    let operationStatus: OperationStatus
    switch status {
    case .confirmed: operationStatus = .confirmed
    case .accepted: operationStatus = .accepted
    case .rejected, .cancelled: operationStatus = .failed
    case .uncertain: operationStatus = .uncertain
    }
    return QueueMutation(
        itemID: itemID,
        action: action,
        operation: OperationReceipt(
            operationId: receipt.commandId,
            kind: .control,
            scope: OperationScope(
                instanceId: "test",
                hostId: "studio-mac",
                sessionId: "s-001"
            ),
            status: operationStatus,
            receipt: receipt,
            reply: nil,
            content: nil,
            readError: nil
        )
    )
}

@Suite("Queue mutation outcomes")
@MainActor
struct ConversationQueueMutationTests {
    @Test("a confirmed edit clears the buffer and replaces the Host queue")
    func confirmedEditApplies() async {
        let service = StubSessionService()
        let original = queueItem()
        let replacement = queueItem(blocks: [.text("replacement")])
        var reads = 0
        service.pendingQueueHandler = { _, _ in
            reads += 1
            return reads == 1 ? currentQueue([original]) : currentQueue([replacement])
        }
        service.updateQueueHandler = { _, _, itemID, action, _ in
            queueMutation(itemID: itemID, action: action, status: .confirmed)
        }
        let model = await makeStartedConversation(service: service)
        await waitUntil { model.queue.isCurrent }

        model.queue.beginEdit(original.itemID)
        model.queue.editText = "replacement"
        await model.queue.submitEdit()

        #expect(service.updateQueueCalls.count == 1)
        #expect(model.queue.lastMutation?.receipt?.status == .confirmed)
        #expect(model.queue.mutationErrorKey == nil)
        #expect(model.queue.editItemID == nil)
        #expect(model.queue.editText.isEmpty)
        #expect(model.queue.items.first?.blocks == [.text("replacement")])
        #expect(reads >= 2, "the controller must refresh the Host queue after a mutation")
    }

    @Test("an unconfirmed edit keeps the unsaved text and waits for the Host queue")
    func unconfirmedEditStaysRecoverable() async {
        let service = StubSessionService()
        let original = queueItem()
        let replacement = queueItem(blocks: [.text("replacement")])
        var applied = false
        var reads = 0
        service.pendingQueueHandler = { _, _ in
            reads += 1
            return currentQueue([applied ? replacement : original])
        }
        service.updateQueueHandler = { _, _, itemID, action, _ in
            queueMutation(itemID: itemID, action: action, status: .uncertain)
        }
        let model = await makeStartedConversation(service: service)
        await waitUntil { model.queue.isCurrent }

        model.queue.beginEdit(original.itemID)
        model.queue.editText = "replacement"
        await model.queue.submitEdit()

        // The receipt is uncertain: the edit is not applied locally and the
        // user's unsaved text is still there for a later decision.
        #expect(model.queue.lastMutation?.receipt?.status == .uncertain)
        #expect(model.queue.editItemID == original.itemID)
        #expect(model.queue.editText == "replacement")
        #expect(model.queue.editErrorKey == .queueMutationUncertain)
        #expect(model.queue.items.first?.blocks == [.text("original text")])
        #expect(reads >= 2, "an ambiguous change must be checked against the Host queue")

        // A later authoritative read that shows the change settles what the
        // UI displays; the controller never invented it locally.
        applied = true
        await model.queue.refresh()
        #expect(model.queue.items.first?.blocks == [.text("replacement")])
    }

    @Test("a rejected edit is surfaced and never treated as applied")
    func rejectedEditIsNotDropped() async {
        let service = StubSessionService()
        let original = queueItem()
        service.pendingQueueHandler = { _, _ in currentQueue([original]) }
        service.updateQueueHandler = { _, _, itemID, action, _ in
            queueMutation(itemID: itemID, action: action, status: .rejected)
        }
        let model = await makeStartedConversation(service: service)
        await waitUntil { model.queue.isCurrent }

        model.queue.beginEdit(original.itemID)
        model.queue.editText = "replacement"
        await model.queue.submitEdit()

        #expect(model.queue.lastMutation?.receipt?.status == .rejected)
        #expect(model.queue.mutationErrorKey == .queueMutationFailed)
        #expect(model.queue.editErrorKey == .queueMutationFailed)
        #expect(model.queue.editItemID == original.itemID)
        #expect(model.queue.editText == "replacement")
        #expect(model.queue.items.first?.blocks == [.text("original text")])
    }

    @Test("a confirmed withdraw keeps its receipt when the refresh fails")
    func confirmedWithdrawKeepsReceipt() async {
        let service = StubSessionService()
        let original = queueItem()
        var reads = 0
        service.pendingQueueHandler = { _, _ in
            reads += 1
            if reads == 1 { return currentQueue([original]) }
            throw SessionServiceError(.unavailable)
        }
        service.updateQueueHandler = { _, _, itemID, action, _ in
            queueMutation(itemID: itemID, action: action, status: .confirmed)
        }
        let model = await makeStartedConversation(service: service)
        await waitUntil { model.queue.isCurrent }

        await model.queue.withdraw(original.itemID)

        #expect(model.queue.lastMutation?.action == .remove)
        #expect(model.queue.lastMutation?.receipt?.status == .confirmed)
        #expect(model.queue.freshness == .stale)
        #expect(model.queue.items == [original])
        #expect(model.queue.mutationErrorKey == nil)
    }

    @Test("an unconfirmed withdraw does not remove the item locally")
    func unconfirmedWithdrawKeepsItem() async {
        let service = StubSessionService()
        let original = queueItem()
        service.pendingQueueHandler = { _, _ in currentQueue([original]) }
        service.updateQueueHandler = { _, _, itemID, action, _ in
            queueMutation(itemID: itemID, action: action, status: .uncertain)
        }
        let model = await makeStartedConversation(service: service)
        await waitUntil { model.queue.isCurrent }

        await model.queue.withdraw(original.itemID)

        #expect(model.queue.lastMutation?.receipt?.status == .uncertain)
        #expect(model.queue.items == [original])
        #expect(model.queue.freshness == .current)
    }

    @Test("a rejected promote reports failure and refreshes the Host queue")
    func rejectedPromoteReportsFailure() async {
        let service = StubSessionService()
        let first = queueItem(id: "queue-1")
        let second = queueItem(id: "queue-2")
        var reads = 0
        service.pendingQueueHandler = { _, _ in
            reads += 1
            return currentQueue([first, second])
        }
        service.updateQueueHandler = { _, _, itemID, action, _ in
            queueMutation(itemID: itemID, action: action, status: .rejected)
        }
        let model = await makeStartedConversation(service: service)
        await waitUntil { model.queue.isCurrent }

        await model.queue.promote(second.itemID)

        #expect(model.queue.lastMutation?.action == .promote)
        #expect(model.queue.lastMutation?.receipt?.status == .rejected)
        #expect(model.queue.mutationErrorKey == .queueMutationFailed)
        #expect(model.queue.items.map(\.itemID) == ["queue-1", "queue-2"])
        #expect(reads >= 2)
    }
}
