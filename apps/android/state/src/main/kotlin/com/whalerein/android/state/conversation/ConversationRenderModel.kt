package com.whalerein.android.state.conversation

/**
 * Pure conversation → render projection for Compose.
 *
 * The projection owns the content-safety policy so it is testable on the JVM:
 * - only `http`, `https` and `mailto` links survive as actionable links;
 * - remote Markdown images become alternate text and never a fetchable URL;
 * - raw HTML is marked as text and never interpreted;
 * - an unknown block has an explicit safe fallback marker;
 * - tool output is expandable only when the Source supplied a full result;
 * - reasoning is hidden by default and only shown when explicitly enabled.
 *
 * It produces no localized chrome strings: the app resolves those from
 * resources, so the same projection serves English and Simplified Chinese.
 */
data class RenderOptions(
    val reasoningEnabled: Boolean = false,
    val expandedToolCalls: Set<String> = emptySet(),
    /** Reserved for callers; the projection never localizes conversation data. */
    val localeTag: String = "en",
)

data class SafeLink(val label: String, val url: String, val scheme: String)

/** Remote images are reduced to alt text; the URL is intentionally dropped. */
data class RemoteImageAlt(val alt: String)

data class RenderedNotice(
    val noticeKind: String,
    val title: String?,
    val body: String,
)

data class RenderedAttachment(
    val attachmentId: String,
    val name: String?,
    val available: Boolean,
)

data class RenderedBlock(
    val blockId: String,
    val kind: MessageBlockKind,
    /** Safe text/markdown handed to the renderer. */
    val text: String,
    val markdown: Boolean,
    val selectable: Boolean,
    /** Exact copy payload: code stays byte-for-byte identical to [MessageBlock.text]. */
    val copyText: String,
    val links: List<SafeLink> = emptyList(),
    val blockedLinkCount: Int = 0,
    val images: List<RemoteImageAlt> = emptyList(),
    val rawHtmlAsText: Boolean = false,
    val unknownFallback: Boolean = false,
    val codeLanguage: String? = null,
    val notice: RenderedNotice? = null,
    val attachment: RenderedAttachment? = null,
    val reasoning: Boolean = false,
    /** False when a reasoning block is currently hidden. */
    val visible: Boolean = true,
)

sealed interface RenderedItem {
    val key: ConversationItemKey
}

data class RenderedMessageItem(
    override val key: ConversationItemKey,
    val messageId: String,
    val role: MessageRole,
    val blocks: List<RenderedBlock>,
    val copyText: String,
) : RenderedItem

data class RenderedPreviewItem(
    override val key: ConversationItemKey,
    val attemptId: String,
    val block: RenderedBlock,
    val incomplete: Boolean,
) : RenderedItem

data class RenderedToolItem(
    override val key: ConversationItemKey,
    val callId: String,
    val name: String,
    val state: ToolActivityState,
    val summary: String,
    val fullOutput: String?,
    val expandable: Boolean,
    val expanded: Boolean,
) : RenderedItem {
    val displayText: String get() = if (expanded && fullOutput != null) fullOutput else summary
}

data class RenderedConversation(
    val items: List<RenderedItem>,
    val reasoningBlockCount: Int,
    val unknownBlockCount: Int,
    val blockedLinkCount: Int,
)

object ConversationRenderModel {
    private val allowedSchemes = setOf("http", "https", "mailto")

    private val imagePattern = Regex("""!\[([^\]]*)]\(([^)\s]+)(?:\s+"[^"]*")?\)""")
    private val linkPattern = Regex("""(?<!!)\[([^\]]*)]\(([^)\s]+)(?:\s+"[^"]*")?\)""")
    private val autolinkPattern = Regex("""<((?:https?|mailto):[^>\s]+)>""")
    private val htmlPattern = Regex("""</?[a-zA-Z][^>]*>""")

    fun render(
        state: ConversationState,
        options: RenderOptions = RenderOptions(),
    ): RenderedConversation = renderItems(state.toItems(), options)

    fun renderItems(
        items: List<ConversationItem>,
        options: RenderOptions = RenderOptions(),
    ): RenderedConversation {
        var reasoningCount = 0
        var unknownCount = 0
        var blockedCount = 0
        val rendered = items.map { item ->
            when (item) {
                is CommittedMessageItem -> {
                    val blocks = item.message.blocks.map { block ->
                        val projected = renderBlock(block, options)
                        if (projected.reasoning) reasoningCount++
                        if (projected.unknownFallback) unknownCount++
                        blockedCount += projected.blockedLinkCount
                        projected
                    }
                    RenderedMessageItem(
                        key = item.key,
                        messageId = item.message.messageId,
                        role = item.message.role,
                        blocks = blocks,
                        copyText = messageCopyText(item.message),
                    )
                }

                is StreamingPreviewItem -> {
                    val projected = renderBlock(
                        MessageBlock(
                            blockId = item.preview.blockId,
                            kind = item.preview.kind,
                            text = item.preview.text,
                        ),
                        options,
                    )
                    if (projected.reasoning) reasoningCount++
                    if (projected.unknownFallback) unknownCount++
                    blockedCount += projected.blockedLinkCount
                    RenderedPreviewItem(
                        key = item.key,
                        attemptId = item.preview.attemptId.value,
                        block = projected,
                        incomplete = item.preview.incomplete,
                    )
                }

                is ToolActivityItem -> {
                    val activity = item.activity
                    RenderedToolItem(
                        key = item.key,
                        callId = activity.callId,
                        name = activity.name,
                        state = activity.state,
                        summary = activity.summary,
                        fullOutput = activity.fullOutput,
                        expandable = activity.fullOutput != null,
                        expanded = activity.callId in options.expandedToolCalls,
                    )
                }
            }
        }
        return RenderedConversation(
            items = rendered,
            reasoningBlockCount = reasoningCount,
            unknownBlockCount = unknownCount,
            blockedLinkCount = blockedCount,
        )
    }

    fun renderBlock(
        block: MessageBlock,
        options: RenderOptions = RenderOptions(),
    ): RenderedBlock {
        val reasoning = block.isReasoning
        val unknown = block.kind == MessageBlockKind.Unknown
        val rawHtml = htmlPattern.containsMatchIn(block.text)
        val sanitized = if (block.kind == MessageBlockKind.Code) {
            SanitizedText(block.text, emptyList(), emptyList(), 0)
        } else {
            sanitize(block.text)
        }
        val markdown = when (block.kind) {
            MessageBlockKind.Text, MessageBlockKind.Reasoning -> true
            else -> false
        }
        val notice = if (block.kind == MessageBlockKind.Notice) {
            RenderedNotice(
                noticeKind = block.noticeKind ?: "notice",
                title = block.fields[MessageBlock.FIELD_TITLE],
                body = sanitized.text,
            )
        } else {
            null
        }
        val attachment = if (block.kind == MessageBlockKind.Attachment) {
            RenderedAttachment(
                attachmentId = block.attachmentId ?: block.blockId,
                name = block.fields[MessageBlock.FIELD_ATTACHMENT_NAME],
                available = block.fields[MessageBlock.FIELD_ATTACHMENT_AVAILABLE] != "false",
            )
        } else {
            null
        }
        val visible = !reasoning || options.reasoningEnabled
        return RenderedBlock(
            blockId = block.blockId,
            kind = block.kind,
            text = if (unknown && block.text.isBlank()) "" else sanitized.text,
            markdown = markdown && !unknown,
            selectable = block.kind != MessageBlockKind.Notice || unknown,
            copyText = when (block.kind) {
                MessageBlockKind.Code -> block.text
                else -> sanitized.text
            },
            links = sanitized.links,
            blockedLinkCount = sanitized.blockedLinkCount,
            images = sanitized.images,
            rawHtmlAsText = rawHtml,
            unknownFallback = unknown,
            codeLanguage = block.language,
            notice = notice,
            attachment = attachment,
            reasoning = reasoning,
            visible = visible,
        )
    }

    fun messageCopyText(message: Message): String = message.blocks
        .filterNot { it.isReasoning }
        .map { block ->
            if (block.kind == MessageBlockKind.Code) block.text else sanitize(block.text).text
        }
        .filter { it.isNotBlank() }
        .joinToString("\n\n")

    private data class SanitizedText(
        val text: String,
        val links: List<SafeLink>,
        val images: List<RemoteImageAlt>,
        val blockedLinkCount: Int,
    )

    private fun sanitize(input: String): SanitizedText {
        if (input.isEmpty()) {
            return SanitizedText(input, emptyList(), emptyList(), 0)
        }
        val links = mutableListOf<SafeLink>()
        val images = mutableListOf<RemoteImageAlt>()
        var blocked = 0

        var result = imagePattern.replace(input) { match ->
            val alt = match.groupValues[1]
            images += RemoteImageAlt(alt)
            alt
        }
        result = linkPattern.replace(result) { match ->
            val label = match.groupValues[1]
            val url = match.groupValues[2].trim()
            val scheme = schemeOf(url)
            if (scheme != null && scheme in allowedSchemes) {
                links += SafeLink(label, url, scheme)
                match.value
            } else {
                blocked++
                label
            }
        }
        result = autolinkPattern.replace(result) { match ->
            val url = match.groupValues[1]
            val scheme = schemeOf(url)
            if (scheme != null && scheme in allowedSchemes) {
                links += SafeLink(url, url, scheme)
                match.value
            } else {
                blocked++
                url
            }
        }
        return SanitizedText(result, links, images, blocked)
    }

    private fun schemeOf(url: String): String? {
        val separator = url.indexOf(':')
        if (separator <= 0) {
            return null
        }
        val scheme = url.substring(0, separator).lowercase()
        val valid = scheme.all { it in 'a'..'z' || it in '0'..'9' || it == '+' || it == '-' }
        return scheme.takeIf { valid }
    }
}
