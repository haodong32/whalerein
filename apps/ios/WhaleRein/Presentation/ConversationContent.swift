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
}
