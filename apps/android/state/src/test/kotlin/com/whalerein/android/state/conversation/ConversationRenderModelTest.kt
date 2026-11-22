package com.whalerein.android.state.conversation

import com.whalerein.android.state.fixtures.conversation.ConversationFixtures
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

class ConversationRenderModelTest : ConversationTestBase() {
    @Test
    @DisplayName("AC-12.rich multilingual markdown nested lists wide table and code stay readable")
    fun ac12Rich() {
        val markdown = """
            # 标题

            - one
              - nested
            - two

            | 左 | 右 |
            | --- | --- |
            | 表 | 值 |

            안녕하세요 **world**
        """.trimIndent()
        val code = "fun main() {\n    println(\"hi\")\n}"
        val message = ConversationFixtures.message(
            messageId = "m1",
            sequence = 1,
            blocks = listOf(
                ConversationFixtures.block("text-1", MessageBlockKind.Text, markdown),
                ConversationFixtures.block(
                    "code-1",
                    MessageBlockKind.Code,
                    code,
                    mapOf(MessageBlock.FIELD_LANGUAGE to "kotlin"),
                ),
            ),
        )
        val rendered = ConversationRenderModel.render(state(message))

        val text = rendered.items.single() as RenderedMessageItem
        val markdownBlock = text.blocks.first { it.blockId == "text-1" }
        assertTrue(markdownBlock.markdown)
        assertTrue(markdownBlock.text.contains("标题"))
        assertTrue(markdownBlock.text.contains("- nested"))
        assertTrue(markdownBlock.text.contains("| 表 | 值 |"))
        assertTrue(markdownBlock.text.contains("안녕하세요"))
        val codeBlock = text.blocks.first { it.blockId == "code-1" }
        assertFalse(codeBlock.markdown)
        assertTrue(codeBlock.selectable)
        assertEquals("kotlin", codeBlock.codeLanguage)
        assertEquals(ConversationItemKey.Committed("m1"), text.key)
        // Wide-table horizontal scrolling and wrapping are Compose/device checks;
        // this projection preserves the table content the renderer receives.
    }

    @Test
    @DisplayName("AC-12.copy code copy text and message copy stay exact")
    fun ac12Copy() {
        val code = "line 1\n  indent\n\"quoted\""
        val message = ConversationFixtures.message(
            messageId = "m1",
            sequence = 1,
            blocks = listOf(
                ConversationFixtures.block("reason-1", MessageBlockKind.Reasoning, "secret reasoning"),
                ConversationFixtures.block("text-1", MessageBlockKind.Text, "visible answer"),
                ConversationFixtures.block("code-1", MessageBlockKind.Code, code),
            ),
        )
        val rendered = ConversationRenderModel.render(state(message))
        val item = rendered.items.single() as RenderedMessageItem
        val codeBlock = item.blocks.first { it.blockId == "code-1" }
        assertEquals(code, codeBlock.copyText)
        assertEquals(code, codeBlock.text)
        assertTrue(item.copyText.contains("visible answer"))
        assertTrue(item.copyText.contains(code))
        assertFalse(item.copyText.contains("secret reasoning"))
        // Long-press selection and the original clipboard payload are device
        // checks; this asserts the exact copy source the UI hands to Compose.
    }

    @Test
    @DisplayName("AC-12.safe-content links raw HTML images and unknown blocks are constrained")
    fun ac12SafeContent() {
        val raw = "[safe](https://example.com/a) [bad](javascript:alert(1)) " +
            "[mail](mailto:a@b.c) [data](data:text/html,x) " +
            "![remote alt](https://example.com/p.png) <b>raw</b>"
        val message = ConversationFixtures.message(
            messageId = "m1",
            sequence = 1,
            blocks = listOf(
                ConversationFixtures.block("text-1", MessageBlockKind.Text, raw),
                ConversationFixtures.block("unknown-1", MessageBlockKind.Unknown, "<script>x</script>"),
                ConversationFixtures.block("unknown-2", MessageBlockKind.Unknown, ""),
            ),
        )
        val rendered = ConversationRenderModel.render(state(message))
        val item = rendered.items.single() as RenderedMessageItem
        val text = item.blocks.first { it.blockId == "text-1" }

        assertEquals(listOf("https", "mailto"), text.links.map { it.scheme })
        assertEquals("https://example.com/a", text.links.first().url)
        assertEquals(2, text.blockedLinkCount)
        assertFalse(text.text.contains("javascript:"))
        assertFalse(text.text.contains("data:text/html"))
        assertTrue(text.text.contains("bad"))
        assertEquals(listOf(RemoteImageAlt("remote alt")), text.images)
        assertFalse(text.text.contains("https://example.com/p.png"))
        assertTrue(text.rawHtmlAsText)
        assertTrue(text.text.contains("<b>raw</b>"))

        val unknownWithText = item.blocks.first { it.blockId == "unknown-1" }
        assertTrue(unknownWithText.unknownFallback)
        assertFalse(unknownWithText.markdown)
        assertTrue(unknownWithText.rawHtmlAsText)
        val emptyUnknown = item.blocks.first { it.blockId == "unknown-2" }
        assertTrue(emptyUnknown.unknownFallback)
        assertEquals("", emptyUnknown.text)
        assertEquals(2, rendered.unknownBlockCount)
        assertEquals(2, rendered.blockedLinkCount)
    }

    @Test
    @DisplayName("AC-12.controls reasoning notice attachment and tool output are separately controlled")
    fun ac12Controls() {
        val message = ConversationFixtures.message(
            messageId = "m1",
            sequence = 1,
            blocks = listOf(
                ConversationFixtures.block("reason-1", MessageBlockKind.Reasoning, "why"),
                ConversationFixtures.block(
                    "notice-1",
                    MessageBlockKind.Notice,
                    "notice body",
                    mapOf(
                        MessageBlock.FIELD_NOTICE_KIND to "warning",
                        MessageBlock.FIELD_TITLE to "Heads up",
                    ),
                ),
                ConversationFixtures.block(
                    "attachment-1",
                    MessageBlockKind.Attachment,
                    "report.pdf",
                    mapOf(
                        MessageBlock.FIELD_ATTACHMENT_ID to "att-1",
                        MessageBlock.FIELD_ATTACHMENT_NAME to "report.pdf",
                        MessageBlock.FIELD_ATTACHMENT_AVAILABLE to "false",
                    ),
                ),
            ),
        )
        val state = ConversationState(
            scope = ConversationFixtures.session(),
            generation = 1,
            committed = listOf(message),
            toolActivities = listOf(
                ToolActivity(
                    callId = "call-1",
                    name = "shell",
                    state = ToolActivityState.Completed,
                    summary = "3 files changed",
                    fullOutput = "full output",
                ),
                ToolActivity(
                    callId = "call-2",
                    name = "lookup",
                    state = ToolActivityState.Completed,
                    summary = "summary only",
                ),
            ),
        )

        val hidden = ConversationRenderModel.render(state)
        assertEquals(1, hidden.reasoningBlockCount)
        val hiddenMessage = hidden.items.first() as RenderedMessageItem
        val reasoning = hiddenMessage.blocks.first { it.blockId == "reason-1" }
        assertTrue(reasoning.reasoning)
        assertFalse(reasoning.visible)
        val notice = hiddenMessage.blocks.first { it.blockId == "notice-1" }
        assertEquals("warning", notice.notice?.noticeKind)
        assertEquals("Heads up", notice.notice?.title)
        assertEquals("notice body", notice.notice?.body)
        val attachment = hiddenMessage.blocks.first { it.blockId == "attachment-1" }
        val attachmentInfo = requireNotNull(attachment.attachment)
        assertFalse(attachmentInfo.available)
        assertEquals("report.pdf", attachmentInfo.name)

        val tools = hidden.items.filterIsInstance<RenderedToolItem>()
        assertTrue(tools.first { it.callId == "call-1" }.expandable)
        assertFalse(tools.first { it.callId == "call-1" }.expanded)
        assertFalse(tools.first { it.callId == "call-2" }.expandable)

        val expanded = ConversationRenderModel.render(
            state,
            RenderOptions(reasoningEnabled = true, expandedToolCalls = setOf("call-1")),
        )
        val visibleReasoning = (expanded.items.first() as RenderedMessageItem)
            .blocks.first { it.blockId == "reason-1" }
        assertTrue(visibleReasoning.visible)
        val expandedTool = expanded.items.filterIsInstance<RenderedToolItem>()
            .first { it.callId == "call-1" }
        assertTrue(expandedTool.expanded)
        assertEquals("full output", expandedTool.displayText)
    }

    @Test
    @DisplayName("AC-13.locale-theme render projection carries no locale-dependent chrome")
    fun ac13LocaleTheme() {
        val message = ConversationFixtures.textMessage("m1", "same content", sequence = 1)
        val english = ConversationRenderModel.render(state(message), RenderOptions(localeTag = "en"))
        val chinese = ConversationRenderModel.render(state(message), RenderOptions(localeTag = "zh-rCN"))
        // Visible chrome strings are resolved by the app resources, not here.
        assertEquals(english, chinese)
    }

    private fun state(message: Message): ConversationState = ConversationState(
        scope = ConversationFixtures.session(),
        generation = 1,
        committed = listOf(message),
    )
}
