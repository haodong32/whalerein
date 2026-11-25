// Read-only committed-history adapter boundary. Paging and exact reads are
// independent one-shot Core operations; they never replace or restart the live
// watch. The feature owns this protocol so the shared SessionService surface
// stays free of feature-specific methods.

import Foundation

enum ConversationHistoryLimit {
    static let messages = 50
    static let bytes = 256 * 1024
}

@MainActor
protocol ConversationHistoryService: AnyObject {
    /// One latest-first bounded page. `continuation` is opaque and scoped to
    /// the original Host/session and content projection.
    func historyPage(
        hostId: String,
        sessionId: String,
        source: HistorySource,
        continuation: String?,
        maxMessages: Int,
        maxBytes: Int
    ) async throws -> ConversationHistoryPage

    /// Resolve one exact scoped committed message. A stale or foreign
    /// reference fails instead of returning different content.
    func historyMessage(_ reference: MessageReference) async throws -> ConversationMessage
}
