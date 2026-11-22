package com.whalerein.android.state.conversation

/**
 * Stable Compose timeline identities and the pure page-insertion projection.
 *
 * A reading anchor is expressed as a stable item key plus the pixel offset from
 * that item's top. Prepending an older page therefore never changes what the
 * reader sees: the UI scrolls back to the same key and offset.
 */

sealed interface ConversationItemKey {
    data class Committed(val messageId: String) : ConversationItemKey

    data class Preview(val attemptId: String, val blockId: String) : ConversationItemKey

    data class ToolCard(val callId: String) : ConversationItemKey
}

sealed interface ConversationItem {
    val key: ConversationItemKey
}

data class CommittedMessageItem(val message: Message) : ConversationItem {
    override val key: ConversationItemKey get() = ConversationItemKey.Committed(message.messageId)
}

data class StreamingPreviewItem(val preview: PreviewBlock) : ConversationItem {
    override val key: ConversationItemKey
        get() = ConversationItemKey.Preview(preview.attemptId.value, preview.blockId)
}

data class ToolActivityItem(val activity: ToolActivity) : ConversationItem {
    override val key: ConversationItemKey get() = ConversationItemKey.ToolCard(activity.callId)
}

data class ReadingAnchor(
    val itemKey: ConversationItemKey,
    /** Pixel distance between the item top and the reader's viewport top. */
    val offsetFromItemTopPx: Int = 0,
)

data class PageInsertion(
    /** The full ordered list after the insertion. */
    val items: List<ConversationItem> = emptyList(),
    /** Keys added by the page, in display order. */
    val insertedKeys: List<ConversationItemKey> = emptyList(),
    /** The same reading anchor re-expressed against the new list. */
    val anchor: ReadingAnchor? = null,
    /** How many list positions the anchor moved because of the insertion. */
    val anchorIndexShift: Int = 0,
    val insertedCount: Int = 0,
)

fun List<ConversationItem>.committedMessages(): List<Message> =
    filterIsInstance<CommittedMessageItem>().map { it.message }

/**
 * Projects reducer state into stable, ordered conversation items.
 *
 * Committed messages come first in Source order, followed by live previews and
 * tool cards in insertion order. Committed identity is canonical, so a message
 * repeated by a live event and a paged replay yields exactly one item.
 */
fun ConversationState.toItems(): List<ConversationItem> {
    val committedItems = committed
        .sortedWith(compareBy<Message>({ it.sequence }, { it.messageId }))
        .map(::CommittedMessageItem)
    val previewItems = previews.sortedBy { it.order }.map(::StreamingPreviewItem)
    val toolItems = toolActivities.map(::ToolActivityItem)
    return committedItems + previewItems + toolItems
}

/**
 * Prepends [older] committed messages while keeping [anchor] stable.
 *
 * Duplicate canonical message ids are ignored, so a paged/live overlap never
 * creates a second row.
 */
fun insertOlderMessages(
    current: List<ConversationItem>,
    older: List<Message>,
    anchor: ReadingAnchor?,
): PageInsertion {
    val knownIds = current.filterIsInstance<CommittedMessageItem>().mapTo(mutableSetOf()) {
        it.message.messageId
    }
    val insertedMessages = older
        .filter { it.messageId !in knownIds }
        .distinctBy { it.messageId }
        .sortedWith(compareBy<Message>({ it.sequence }, { it.messageId }))

    if (insertedMessages.isEmpty()) {
        return PageInsertion(
            items = current,
            insertedKeys = emptyList(),
            anchor = anchor?.takeIf { key -> current.any { it.key == key.itemKey } },
        )
    }

    val liveItems = current.filterNot { it is CommittedMessageItem }
    val mergedCommitted = (current.committedMessages() + insertedMessages)
        .distinctBy { it.messageId }
        .sortedWith(compareBy<Message>({ it.sequence }, { it.messageId }))
        .map(::CommittedMessageItem)
    val merged = mergedCommitted + liveItems

    val oldAnchorIndex = anchor?.let { item ->
        current.indexOfFirst { it.key == item.itemKey }.takeIf { it >= 0 }
    }
    val newAnchorIndex = anchor?.let { item ->
        merged.indexOfFirst { it.key == item.itemKey }.takeIf { it >= 0 }
    }
    val shift = if (oldAnchorIndex != null && newAnchorIndex != null) {
        newAnchorIndex - oldAnchorIndex
    } else {
        0
    }

    return PageInsertion(
        items = merged,
        insertedKeys = insertedMessages.map { ConversationItemKey.Committed(it.messageId) },
        anchor = anchor?.takeIf { newAnchorIndex != null },
        anchorIndexShift = shift,
        insertedCount = insertedMessages.size,
    )
}
