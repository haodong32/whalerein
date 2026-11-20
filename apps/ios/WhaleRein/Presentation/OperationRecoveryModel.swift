// Settings and conversation operation-recovery presentation.
//
// One model owns the retained-operation list and the single coalesced, read-only
// refresh used by foreground return and reconnect. Automatic refresh filters by
// the exact instance/Host/session scope, performs only the Core status lookup
// for each unresolved send, and never sends a message or resumes pairing.
// Manual recovery stays available for every retained operation, including other
// sessions and control or pairing operations.

import Foundation
import Observation

@MainActor
@Observable
final class OperationRecoveryModel {
    private(set) var operations: [OperationReceipt] = []
    private(set) var isLoading = false
    private(set) var recoveringOperationId: String?
    private(set) var readingOperationId: String?
    private(set) var errorKey: L10nKey?
    private(set) var lastRefreshErrorKey: L10nKey?
    /// Number of status lookups issued by automatic refreshes. Tests assert one
    /// lookup per unresolved send per transition.
    private(set) var automaticLookupCount = 0
    private(set) var automaticRefreshCount = 0

    let instanceId: String
    private let service: any OperationRecoveryService
    private var refreshTask: Task<RefreshOutcome, Never>?

    private struct RefreshOutcome: Sendable {
        let all: [OperationReceipt]
        let updated: [OperationReceipt]
    }

    init(service: any OperationRecoveryService, instanceId: String) {
        self.service = service
        self.instanceId = instanceId
    }

    var isEmpty: Bool { operations.isEmpty }

    func operations(for scope: OperationScope) -> [OperationReceipt] {
        operations.filter { $0.scope == scope }
    }

    func receipt(for operationId: String) -> OperationReceipt? {
        operations.first { $0.operationId == operationId }
    }

    /// Loads the retained list. Local-only in both adapters.
    func load() async {
        guard !isLoading else { return }
        isLoading = true
        defer { isLoading = false }
        do {
            operations = try await service.listRetainedOperations()
                .sorted { $0.operationId < $1.operationId }
            errorKey = nil
        } catch {
            errorKey = Self.errorKey(for: error)
        }
    }

    /// Manual recovery from Settings. Uses the original operation identity and
    /// never creates a new operation.
    @discardableResult
    func recover(operationId: String) async -> OperationReceipt? {
        guard recoveringOperationId == nil else { return nil }
        recoveringOperationId = operationId
        errorKey = nil
        defer { recoveringOperationId = nil }
        do {
            let receipt = try await service.recoverSend(operationId: operationId, follow: true)
            upsert(receipt)
            return receipt
        } catch {
            errorKey = Self.errorKey(for: error)
            return nil
        }
    }

    /// Explicit exact reread for a confirmed reference. Never resubmits.
    @discardableResult
    func readExactReply(operationId: String) async -> ConversationMessage? {
        guard readingOperationId == nil,
              let stored = receipt(for: operationId),
              let reference = stored.confirmedReference else { return nil }
        readingOperationId = operationId
        errorKey = nil
        defer { readingOperationId = nil }
        do {
            let message = try await service.readMessage(reference)
            upsert(
                OperationReceipt(
                    operationId: stored.operationId,
                    kind: stored.kind,
                    scope: stored.scope,
                    status: stored.status,
                    receipt: stored.receipt,
                    reply: stored.reply,
                    content: message,
                    readError: nil
                )
            )
            return message
        } catch {
            upsert(stored.replacing(readError: (error as? SessionServiceError)?.code.rawValue ?? "read_failed"))
            errorKey = Self.errorKey(for: error)
            return nil
        }
    }

    /// One read-only refresh for the visible instance/Host/session. Overlapping
    /// triggers for the same transition await the same task, so each unresolved
    /// send receives exactly one status lookup.
    func refreshVisibleOperations(in scope: OperationScope) async {
        if let refreshTask {
            _ = await refreshTask.value
            return
        }
        let service = self.service
        let task = Task { () -> RefreshOutcome in
            do {
                let retained = try await service.listRetainedOperations()
                let unresolved = retained.filter {
                    $0.kind == .send
                        && $0.scope == scope
                        && $0.status.isUnresolved
                }
                var updated: [OperationReceipt] = []
                for operation in unresolved {
                    do {
                        // Status-only path: Core's send recovery lookup.
                        let receipt = try await service.recoverSend(
                            operationId: operation.operationId,
                            follow: false
                        )
                        // A late result from another scope never enters the
                        // visible view; keep the original entry instead.
                        if receipt.scope == scope {
                            updated.append(receipt)
                        } else {
                            updated.append(operation)
                        }
                    } catch {
                        // A failed lookup leaves the operation listed and
                        // recoverable rather than pretending it resolved.
                        updated.append(operation)
                    }
                }
                return RefreshOutcome(all: retained, updated: updated)
            } catch {
                return RefreshOutcome(all: [], updated: [])
            }
        }
        refreshTask = task
        let outcome = await task.value
        automaticRefreshCount += 1
        automaticLookupCount += outcome.updated.count
        // Other sessions and control/pairing operations stay visible for manual
        // recovery in Settings.
        for receipt in outcome.all where self.receipt(for: receipt.operationId) == nil {
            operations.append(receipt)
        }
        operations.sort { $0.operationId < $1.operationId }
        for receipt in outcome.updated {
            upsert(mergingContent(into: receipt))
        }
        if refreshTask == task {
            refreshTask = nil
        }
    }

    /// Applies a manual or automatic recovery result to the visible conversation.
    func applyRecoveredOperations(
        in scope: OperationScope,
        to conversation: ConversationModel
    ) {
        conversation.applyRecoveredOperations(operations(for: scope))
    }

    func clearError() {
        errorKey = nil
    }

    /// Keep locally read content and a recorded read failure when a status-only
    /// lookup returns the same operation without content.
    private func mergingContent(into incoming: OperationReceipt) -> OperationReceipt {
        guard let existing = receipt(for: incoming.operationId) else { return incoming }
        return OperationReceipt(
            operationId: incoming.operationId,
            kind: incoming.kind,
            scope: incoming.scope,
            status: incoming.status,
            receipt: incoming.receipt ?? existing.receipt,
            reply: incoming.reply ?? existing.reply,
            content: incoming.content ?? existing.content,
            readError: incoming.readError ?? existing.readError
        )
    }

    private func upsert(_ receipt: OperationReceipt) {
        if let index = operations.firstIndex(where: { $0.operationId == receipt.operationId }) {
            operations[index] = receipt
        } else {
            operations.append(receipt)
            operations.sort { $0.operationId < $1.operationId }
        }
    }

    private static func errorKey(for error: Error) -> L10nKey {
        let code = (error as? SessionServiceError)?.code ?? .internalError
        return L10nKey.errorKey(for: code)
    }
}
