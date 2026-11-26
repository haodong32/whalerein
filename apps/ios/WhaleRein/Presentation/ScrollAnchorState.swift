// Pure scroll-keeping state for the conversation. The view reports whether the
// user is at the latest row; this type decides whether new output may move the
// scroll position and tracks the jump-to-latest affordance.

import Foundation

struct ScrollAnchorState: Equatable, Sendable {
    private(set) var isAtLatest = true
    private(set) var unreadCount = 0
    private(set) var anchorMessageId: String?
    private(set) var layoutRevision = 0

    mutating func markLayoutChange() {
        layoutRevision &+= 1
    }

    mutating func update(isAtLatest: Bool) {
        self.isAtLatest = isAtLatest
        if isAtLatest {
            unreadCount = 0
            anchorMessageId = nil
        }
    }

    mutating func noteTopVisibleMessage(_ messageId: String?) {
        guard !isAtLatest else { return }
        anchorMessageId = messageId
    }

    /// Returns true when the transcript should follow the new content.
    mutating func recordIncoming(isOwnMessage: Bool) -> Bool {
        if isAtLatest {
            return true
        }
        if !isOwnMessage {
            unreadCount += 1
        }
        return false
    }

    mutating func jumpToLatest() {
        update(isAtLatest: true)
    }
}

/// The first surviving previously visible row preserves its offset.
struct TranscriptRowPosition: Equatable, Sendable {
    let id: String
    let top: CGFloat
    let bottom: CGFloat

    static func correction(anchors: [Self], current: [Self]) -> CGFloat? {
        let positions = Dictionary(current.map { ($0.id, $0.top) }, uniquingKeysWith: { first, _ in first })
        for anchor in anchors {
            if let top = positions[anchor.id] {
                return top - anchor.top
            }
        }
        return nil
    }

    static func trailingSpace(from current: CGFloat, maximum: CGFloat, correction: CGFloat) -> CGFloat {
        guard correction > 0.5 else { return 0 }
        return max(0, current + correction - max(0, maximum))
    }

    static func restorationOffset(from current: CGFloat, maximum: CGFloat, correction: CGFloat) -> CGFloat? {
        guard abs(correction) > 0.5 else { return nil }
        let target = min(max(0, maximum), max(0, current + correction))
        return abs(target - current) > 0.5 ? target : nil
    }
}
