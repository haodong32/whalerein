package com.whalerein.android.state.conversation

/**
 * Destination of one conversation focus id.
 *
 * A focus id can name either a pending input or a committed message. Opening it
 * as pending input resolves which one it is; only a proven miss falls through to
 * the exact-history reread.
 */
sealed interface ConversationFocusDispatch {
    data object None : ConversationFocusDispatch

    data object PendingInput : ConversationFocusDispatch

    data class ExactMessage(val messageId: String) : ConversationFocusDispatch
}

/**
 * Decides the dispatch for [focusItemId] once the pending-input lookup settled.
 *
 * [focusedItemId] must match so a stale detail read for another item can never
 * trigger an exact read. Loading/Changed/Unavailable stay undecided instead of
 * guessing a different destination.
 */
fun dispatchConversationFocus(
    focusItemId: String?,
    focusedItemId: String?,
    detail: PendingDetailUiState,
): ConversationFocusDispatch {
    if (focusItemId == null || focusedItemId != focusItemId) {
        return ConversationFocusDispatch.None
    }
    return when (detail) {
        is PendingDetailUiState.Loaded ->
            if (detail.item.itemId == focusItemId) {
                ConversationFocusDispatch.PendingInput
            } else {
                ConversationFocusDispatch.None
            }

        PendingDetailUiState.Missing -> ConversationFocusDispatch.ExactMessage(focusItemId)
        PendingDetailUiState.None,
        PendingDetailUiState.Loading,
        is PendingDetailUiState.Changed,
        is PendingDetailUiState.Unavailable,
        -> ConversationFocusDispatch.None
    }
}
