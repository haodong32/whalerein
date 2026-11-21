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
    var canMutate: Bool {
        canRead && !isMutating && !isRefreshing
            && !items.isEmpty
    }

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
                // An unavailable answer never establishes an empty queue.
                freshness = items.isEmpty ? .unavailable : .stale
                running = false
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
            running = false
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
            // Keep the confirmed operation receipt even if the refresh fails.
            lastMutation = mutation
            mutationErrorKey = nil
            await refresh()
            if action == .edit, editItemID == itemID {
                clearEdit()
            }
            return true
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
        if !saved, lastMutation == nil {
            editErrorKey = mutationErrorKey ?? .errorInternal
        }
    }

    func clearMutationStatus() {
        mutationErrorKey = nil
        lastMutation = nil
    }

    private func clearEdit() {
        editItemID = nil
        editText = ""
        editReplacesAttachments = false
        editErrorKey = nil
    }
}
