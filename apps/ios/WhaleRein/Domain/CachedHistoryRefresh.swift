// Decides how one committed-cache refresh reaches the conversation.
//
// A cache refresh that only appends committed messages must stay incremental:
// broadcasting a full snapshot for it makes the whole transcript replace
// itself after a send, which briefly blanks a lazy long conversation. A
// genuinely different view (initial seed, eviction, reorder, or source reset)
// still needs the replacing reset.

import Foundation

enum CachedHistoryRefresh: Equatable {
    case unchanged
    case appended([ConversationMessage])
    case replaced

    static func resolve(
        previous: [ConversationMessage],
        current: [ConversationMessage]
    ) -> CachedHistoryRefresh {
        let previousIds = previous.map(\.messageId)
        let currentIds = current.map(\.messageId)
        if previousIds == currentIds { return .unchanged }
        // With a committed prefix, the tail is newly committed live output.
        // An empty previous list is the cold seed and must establish the view
        // through a reset instead of pretending every row was live output.
        if !previousIds.isEmpty,
           currentIds.count > previousIds.count,
           currentIds.prefix(previousIds.count).elementsEqual(previousIds) {
            return .appended(Array(current.suffix(from: previousIds.count)))
        }
        return .replaced
    }

    /// Projects one refresh onto the wire events a conversation consumes.
    /// Keeping this pure lets the exact post-send sequence be tested with a
    /// stub snapshot and no Core.
    func events(
        current: [ConversationMessage],
        approvals: [Approval]
    ) -> [MessageEvent] {
        switch self {
        case .unchanged:
            return []
        case .appended(let additions):
            return additions.map { .message($0) }
        case .replaced:
            return [.reset(messages: current, approvals: approvals)]
        }
    }
}
