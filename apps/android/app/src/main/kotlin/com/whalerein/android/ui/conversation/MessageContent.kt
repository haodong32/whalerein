package com.whalerein.android.ui.conversation

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import com.mikepenz.markdown.m3.Markdown
import com.whalerein.android.R
import com.whalerein.android.state.conversation.MessageBlockKind
import com.whalerein.android.state.conversation.RenderedAttachment
import com.whalerein.android.state.conversation.RenderedBlock
import com.whalerein.android.state.conversation.RenderedMessageItem
import com.whalerein.android.state.conversation.RenderedNotice
import com.whalerein.android.state.conversation.RenderedPreviewItem
import com.whalerein.android.state.conversation.RenderedToolItem
import com.whalerein.android.state.conversation.RenderedItem

/** Stateless rendering of one conversation item from the render projection. */
@Composable
fun MessageContent(
    item: RenderedItem,
    expandedToolCalls: Set<String>,
    onToggleTool: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    when (item) {
        is RenderedMessageItem -> RenderedMessage(item, modifier)
        is RenderedPreviewItem -> RenderedPreview(item, modifier)
        is RenderedToolItem -> ToolCard(
            item = item,
            expanded = item.callId in expandedToolCalls,
            onToggle = { onToggleTool(item.callId) },
            modifier = modifier,
        )
    }
}

@Composable
fun RenderedMessage(item: RenderedMessageItem, modifier: Modifier = Modifier) {
    Column(
        modifier = modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(
            text = stringResource(roleLabel(item.role)),
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.primary,
        )
        item.blocks.forEach { block ->
            if (block.visible) {
                BlockContent(block)
            }
        }
    }
}

@Composable
fun RenderedPreview(item: RenderedPreviewItem, modifier: Modifier = Modifier) {
    Surface(
        modifier = modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        color = MaterialTheme.colorScheme.surfaceVariant,
        shape = MaterialTheme.shapes.medium,
    ) {
        Column(
            modifier = Modifier.padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            if (item.block.visible) {
                BlockContent(item.block)
            }
            if (item.incomplete) {
                Text(
                    text = stringResource(R.string.conversation_streaming_incomplete),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@Composable
fun BlockContent(block: RenderedBlock, modifier: Modifier = Modifier) {
    when (block.kind) {
        MessageBlockKind.Code -> CodeBlock(block, modifier)
        MessageBlockKind.Notice -> NoticeBlock(
            notice = requireNotNull(block.notice),
            modifier = modifier,
        )

        MessageBlockKind.Attachment -> AttachmentRow(
            attachment = requireNotNull(block.attachment),
            modifier = modifier,
        )

        MessageBlockKind.Unknown -> UnknownBlock(block, modifier)
        MessageBlockKind.Text,
        MessageBlockKind.Reasoning,
        MessageBlockKind.Tool,
        -> MarkdownBlock(block, modifier)
    }
}

@Composable
fun MarkdownBlock(block: RenderedBlock, modifier: Modifier = Modifier) {
    Column(modifier = modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        if (block.markdown && !block.rawHtmlAsText) {
            // Wide tables can exceed a narrow window; the whole Markdown block
            // scrolls horizontally so no column is clipped.
            Markdown(
                content = block.text,
                modifier = Modifier
                    .fillMaxWidth()
                    .horizontalScroll(rememberScrollState()),
            )
        } else {
            // Raw HTML and non-Markdown blocks stay literal selectable text.
            SelectionContainer {
                Text(text = block.text, style = MaterialTheme.typography.bodyLarge)
            }
        }
        if (block.links.isNotEmpty()) {
            SafeLinkRow(block)
        }
    }
}

@Composable
private fun SafeLinkRow(block: RenderedBlock) {
    val uriHandler = LocalUriHandler.current
    Row(
        modifier = Modifier.horizontalScroll(rememberScrollState()),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        block.links.forEach { link ->
            AssistChip(
                onClick = { uriHandler.openUri(link.url) },
                label = {
                    Text(
                        text = stringResource(R.string.conversation_open_link, link.label),
                    )
                },
            )
        }
    }
}

@Composable
fun CodeBlock(block: RenderedBlock, modifier: Modifier = Modifier) {
    Card(
        modifier = modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surfaceVariant,
        ),
    ) {
        SelectionContainer {
            Text(
                text = block.text,
                fontFamily = FontFamily.Monospace,
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier
                    .horizontalScroll(rememberScrollState())
                    .padding(12.dp),
            )
        }
    }
}

@Composable
fun NoticeBlock(notice: RenderedNotice, modifier: Modifier = Modifier) {
    Card(
        modifier = modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.tertiaryContainer,
        ),
    ) {
        Column(modifier = Modifier.padding(12.dp)) {
            Text(
                text = notice.title ?: stringResource(noticeLabel(notice.noticeKind)),
                style = MaterialTheme.typography.titleSmall,
            )
            Text(text = notice.body, style = MaterialTheme.typography.bodyMedium)
        }
    }
}

@Composable
fun AttachmentRow(attachment: RenderedAttachment, modifier: Modifier = Modifier) {
    Row(
        modifier = modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(
            text = attachment.name ?: attachment.attachmentId,
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.weight(1f),
        )
        Text(
            text = stringResource(
                if (attachment.available) {
                    R.string.conversation_attachment_available
                } else {
                    R.string.conversation_attachment_unavailable
                },
            ),
            style = MaterialTheme.typography.labelMedium,
        )
    }
}

@Composable
fun UnknownBlock(block: RenderedBlock, modifier: Modifier = Modifier) {
    if (block.text.isBlank()) {
        Text(
            text = stringResource(R.string.conversation_unsupported_block),
            style = MaterialTheme.typography.bodyMedium,
            modifier = modifier,
        )
    } else {
        SelectionContainer {
            Text(text = block.text, style = MaterialTheme.typography.bodyMedium, modifier = modifier)
        }
    }
}

@Composable
fun ToolCard(
    item: RenderedToolItem,
    expanded: Boolean,
    onToggle: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Card(modifier = modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp)) {
        Column(
            modifier = Modifier.padding(12.dp).widthIn(min = 0.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Text(
                text = item.name,
                style = MaterialTheme.typography.titleSmall,
            )
            SelectionContainer {
                Text(
                    text = item.displayText,
                    style = MaterialTheme.typography.bodySmall,
                    fontFamily = FontFamily.Monospace,
                )
            }
            if (item.expandable) {
                TextButton(onClick = onToggle) {
                    Text(
                        text = stringResource(
                            if (expanded) {
                                R.string.conversation_tool_collapse
                            } else {
                                R.string.conversation_tool_expand
                            },
                        ),
                    )
                }
            }
        }
    }
}

private fun roleLabel(role: com.whalerein.android.state.conversation.MessageRole): Int = when (role) {
    com.whalerein.android.state.conversation.MessageRole.User -> R.string.conversation_role_user
    com.whalerein.android.state.conversation.MessageRole.Assistant -> R.string.conversation_role_assistant
    com.whalerein.android.state.conversation.MessageRole.Tool -> R.string.conversation_role_tool
    com.whalerein.android.state.conversation.MessageRole.System -> R.string.conversation_role_system
}

private fun noticeLabel(kind: String): Int = when (kind) {
    "warning" -> R.string.conversation_notice_warning
    "error" -> R.string.conversation_notice_error
    "info" -> R.string.conversation_notice_info
    else -> R.string.conversation_notice_default
}
