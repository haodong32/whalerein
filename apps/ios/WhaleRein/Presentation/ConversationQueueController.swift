// Pending-queue presentation controller. Core owns contents, ordering and the
// action advertisement; this controller only keeps a current/stale/unavailable
// presentation, submits one targeted action at a time, and refreshes after a
// mutation without discarding a confirmed receipt when that refresh fails.

import Foundation
import Observation

@MainActor
@Observable
final class ConversationQueueController {
    let hostId: String
    let sessionId: String

    private(set) var freshness: QueueFreshness = .idle
    private(set) var items: [QueueItem] = []
    private(set) var running = false
    private(set) var lastMutation: QueueMutation?
    private(set) var mutationErrorKey: L10nKey?
    private(set) var lastRefreshError: AppErrorCode?
    private(set) var isRefreshing = false
    private(set) var isMutating = false
    /// Whole-content edit state. `editReplacesAttachments` stays true until the
    /// user cancels or confirms, so the warning cannot disappear unnoticed.
    private(set) var editItemID: String?
    private(set) var editReplacesAttachments = false
    private(set) var editErrorKey: L10nKey?
    var editText: String = ""

    private let service: any SessionService
    private var capabilities: SessionCapabilities?

    init(hostId: String, sessionId: String, service: any SessionService) {
        self.hostId = hostId
        self.sessionId = sessionId
        self.service = service
    }

    var isCurrent: Bool { freshness == .current }
    var isEmptyAndCurrent: Bool { freshness == .current && items.isEmpty }
    var canRead: Bool { capabilities?.supportsQueueRead ?? false }
    var editWarningVisible: Bool { editItemID != nil && editReplacesAttachments }

    func applyCapabilities(_ next: SessionCapabilities) {
        capabilities = next
        if !next.supportsQueueRead {
            // Keep the last observation visible but explicitly not current. It
            // is never replaced by an unsupported Host's empty answer.
            freshness = .unsupported
            editItemID = nil
            editReplacesAttachments = false
        }
    }

    func supports(_ action: QueueAction) -> Bool {
        guard let capabilities else { return false }
        switch action {
        case .edit: return capabilities.supportsQueueEdit
        case .remove: return capabilities.supportsQueueRemove
        case .promote: return capabilities.supportsQueuePromote
        }
    }

    /// Replace the displayed list only with a successful, available read.
    func refresh() async {
        guard let capabilities, capabilities.supportsQueueRead else {
            freshness = .unsupported
            return
        }
        guard !isRefreshing else { return }
        isRefreshing = true
        defer { isRefreshing = false }
        if freshness == .idle {
            freshness = .loading
        }
        do {
            let snapshot = try await service.pendingQueue(hostId: hostId, sessionId: sessionId)
            lastRefreshError = nil
            guard snapshot.available else {
                // An unavailable answer never establishes an empty queue and
                // never proves the turn stopped; keep the last known running
                // state with the last known list.
                freshness = items.isEmpty ? .unavailable : .stale
                return
            }
            items = snapshot.items
            running = snapshot.running
            freshness = .current
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            lastRefreshError = code
            if code == .unsupportedCapability {
                freshness = .unsupported
            } else {
                freshness = items.isEmpty ? .unavailable : .stale
            }
            // A failed read is not evidence that the run ended.
        }
    }

    /// Targeted action for one item. A stale menu whose item disappeared still
    /// submits: the Host's native receipt, not a local guess, owns that race.
    @discardableResult
    func mutate(itemID: String, action: QueueAction, text: String = "") async -> Bool {
        guard !isMutating else { return false }
        guard supports(action) else {
            mutationErrorKey = .queueActionUnsupported
            return false
        }
        if let item = items.first(where: { $0.itemID == itemID }), !item.advertises(action) {
            mutationErrorKey = .queueActionUnavailable
            return false
        }
        isMutating = true
        defer { isMutating = false }
        do {
            let mutation = try await service.updateQueueItem(
                hostId: hostId,
                sessionId: sessionId,
                itemId: itemID,
                action: action,
                text: text
            )
            // Keep the native receipt even if the follow-up read fails.
            lastMutation = mutation
            let status = mutation.receipt?.status ?? Self.status(for: mutation.operation.status)
            switch status {
            case .confirmed, .accepted:
                mutationErrorKey = nil
                await refresh()
                if action == .edit, editItemID == itemID {
                    clearEdit()
                }
                return true
            case .uncertain:
                // The change is not known to have happened. Keep an edit
                // recoverable and let the authoritative Host queue decide; a
                // failed read leaves the previous list marked stale.
                mutationErrorKey = nil
                await refresh()
                return false
            case .rejected, .cancelled:
                // A rejected receipt is a real outcome, not a silent success.
                mutationErrorKey = .queueMutationFailed
                await refresh()
                return false
            }
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            mutationErrorKey = L10nKey.errorKey(for: code)
            // A consumed item or changed source state is reflected by the next
            // authoritative read; never clear the list locally.
            await refresh()
            return false
        }
    }

    func withdraw(_ itemID: String) async {
        await mutate(itemID: itemID, action: .remove)
    }

    func promote(_ itemID: String) async {
        await mutate(itemID: itemID, action: .promote)
    }

    /// Start a whole-content edit. Attachments are not editable and are
    /// replaced by a confirmed edit, which the warning states before saving.
    func beginEdit(_ itemID: String) {
        guard supports(.edit),
              let item = items.first(where: { $0.itemID == itemID }) else {
            editErrorKey = .queueActionUnavailable
            return
        }
        guard item.advertises(.edit) else {
            editErrorKey = .queueActionUnavailable
            return
        }
        editItemID = itemID
        editText = item.textContent
        editReplacesAttachments = item.hasAttachments
        editErrorKey = nil
    }

    func cancelEdit() {
        clearEdit()
    }

    /// Save the whole-content replacement. Empty text is rejected locally; the
    /// Host remains the authority for every other guard.
    func submitEdit() async {
        guard let itemID = editItemID else { return }
        let text = editText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else {
            editErrorKey = .queueEditEmpty
            return
        }
        editErrorKey = nil
        let saved = await mutate(itemID: itemID, action: .edit, text: text)
        if !saved {
            // A rejected or unconfirmed result keeps the edit buffer and the
            // reason visible instead of dropping the user's unsaved text.
            editErrorKey = mutationErrorKey ?? .queueMutationUncertain
        }
    }

    /// A mutation whose native receipt is missing still carries the shared
    /// operation status; only a terminal success may clear local edit state.
    private static func status(for status: OperationStatus) -> CommandStatus {
        switch status {
        case .confirmed: return .confirmed
        case .accepted: return .accepted
        case .failed: return .rejected
        case .submitted, .uncertain: return .uncertain
        }
    }

    private func clearEdit() {
        editItemID = nil
        editText = ""
        editReplacesAttachments = false
        editErrorKey = nil
    }
}
