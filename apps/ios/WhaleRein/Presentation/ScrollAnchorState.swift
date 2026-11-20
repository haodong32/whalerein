// Pure scroll-keeping state for the conversation. The view reports whether the
// user is at the latest row; this type decides whether new output may move the
// scroll position and tracks the jump-to-latest affordance.

import Foundation

struct ScrollAnchorState: Equatable, Sendable {
    private(set) var isAtLatest = true
    private(set) var unreadCount = 0
    private(set) var anchorMessageId: String?

    mutating func update(isAtLatest: Bool) {
        if isAtLatest {
            self.isAtLatest = true
            unreadCount = 0
        } else {
            self.isAtLatest = false
        }
    }

    mutating func noteTopVisibleMessage(_ messageId: String?) {
        guard !isAtLatest else { return }
        anchorMessageId = messageId
    }

    /// Returns true when the transcript should follow the new content.
    mutating func recordIncoming(isOwnMessage: Bool) -> Bool {
        if isOwnMessage {
            isAtLatest = true
            unreadCount = 0
            anchorMessageId = nil
            return true
        }
        if isAtLatest {
            return true
        }
        unreadCount += 1
        return false
    }

    /// Captures the row that must stay visually stable when older messages are
    /// prepended. Unlike `noteTopVisibleMessage`, this also applies while the
    /// transcript is at the latest row, because loading older content implies
    /// the reader is at the top.
    mutating func capturePrependAnchor(_ messageId: String?) {
        anchorMessageId = messageId
    }

    mutating func jumpToLatest() {
        isAtLatest = true
        unreadCount = 0
        anchorMessageId = nil
    }

    mutating func reset() {
        isAtLatest = true
        unreadCount = 0
        anchorMessageId = nil
    }
}
