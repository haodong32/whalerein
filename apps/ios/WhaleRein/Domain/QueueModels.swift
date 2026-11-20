// Pending-queue domain types. Core owns the ordered contents and the action
// advertisement for each item; iOS only renders them and submits a targeted
// action that Core revalidates.

import Foundation

/// How current the displayed queue is. Only `current` establishes that an empty
/// list is authoritative; stale, unavailable and unsupported never do.
enum QueueFreshness: String, Equatable, Sendable {
    case idle
    case current
    case stale
    case unavailable
    case unsupported
}

enum QueueAction: String, CaseIterable, Sendable {
    case edit
    case remove
    case promote
}

enum QueueBlock: Hashable, Sendable {
    case text(String)
    case attachment(mediaType: String, name: String, size: UInt64, available: Bool)
    case unavailable
}

struct QueueItem: Identifiable, Hashable, Sendable {
    let itemID: String
    let placement: String
    let deliveryID: String?
    let blocks: [QueueBlock]
    /// Lowercase actions advertised by the Source for this exact item.
    let actions: [QueueAction]

    var id: String { itemID }

    var hasAttachments: Bool {
        blocks.contains { block in
            if case .attachment = block { return true }
            return false
        }
    }

    /// Whole-content text used to seed an edit. Attachments are not editable
    /// and are replaced by a confirmed edit.
    var textContent: String {
        blocks.compactMap { block in
            if case .text(let text) = block { return text }
            return nil
        }.joined(separator: "\n")
    }

    func advertises(_ action: QueueAction) -> Bool { actions.contains(action) }
}

struct QueueSnapshot: Hashable, Sendable {
    let available: Bool
    let running: Bool
    let items: [QueueItem]
}

/// One targeted mutation and its native terminal receipt. `operationID` is the
/// Core command identity; it is not a second operation journal.
struct QueueMutation: Identifiable, Hashable, Sendable {
    let operationID: String
    let hostID: String
    let sessionID: String
    let itemID: String
    let action: QueueAction
    let receipt: CommandReceipt

    var id: String { operationID }
}
