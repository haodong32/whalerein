// Typed content selection shared by the conversation rows and its tests.
// Reasoning is present in the model but hidden unless the App-section
// preference is on; every other typed block is always preserved.

import Foundation

enum ConversationContent {
    static func visibleBlocks(
        _ blocks: [ConversationBlock],
        reasoningVisible: Bool
    ) -> [ConversationBlock] {
        guard !reasoningVisible else { return blocks }
        return blocks.filter { $0.kind != .reasoning }
    }

    /// Stable provenance label for an older-history page.
    static func provenanceKey(_ provenance: HistoryProvenance) -> L10nKey {
        switch provenance {
        case .online: return .conversationHistoryOnline
        case .cache: return .conversationHistoryCache
        case .unavailable: return .conversationHistoryUnavailable
        }
    }
}
