package com.whalerein.android

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.whalerein.android.state.conversation.ConversationItemKey
import com.whalerein.android.state.conversation.MessageBlockKind
import com.whalerein.android.state.conversation.MessageRole
import com.whalerein.android.state.conversation.RenderedAttachment
import com.whalerein.android.state.conversation.RenderedBlock
import com.whalerein.android.state.conversation.RenderedMessageItem
import com.whalerein.android.ui.conversation.MessageContent
import com.whalerein.android.ui.routes.AttachmentSaveRoute
import com.whalerein.android.ui.theme.WhaleReinTheme
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Attachment entry: the message row opens the registered save route. */
@RunWith(AndroidJUnit4::class)
class AttachmentEntryTest {
    @get:Rule
    val rule = createAndroidComposeRule<SupportTestActivity>()

    private val context = InstrumentationRegistry.getInstrumentation().targetContext
    private fun text(id: Int) = context.getString(id)

    @Test
    fun `AC-28_attachment_row_offers_save_for_an_available_attachment`() {
        var savedId: String? = null
        rule.setContent {
            WhaleReinTheme {
                MessageContent(
                    item = messageWithAttachment(available = true),
                    expandedToolCalls = emptySet(),
                    onToggleTool = {},
                    onSaveAttachment = { savedId = it },
                )
            }
        }

        rule.onNodeWithText(text(R.string.conversation_attachment_save)).performClick()
        rule.waitForIdle()
        assertEquals("attachment-1", savedId)
    }

    @Test
    fun `AC-28_unavailable_attachment_does_not_offer_save`() {
        rule.setContent {
            WhaleReinTheme {
                MessageContent(
                    item = messageWithAttachment(available = false),
                    expandedToolCalls = emptySet(),
                    onToggleTool = {},
                    onSaveAttachment = {},
                )
            }
        }

        assertTrue(
            rule.onAllNodesWithText(text(R.string.conversation_attachment_save))
                .fetchSemanticsNodes()
                .isEmpty(),
        )
    }

    @Test
    fun `AC-28_registered_save_route_renders_the_attachment_sheet`() {
        val dependencies = FakeAppDependencies()
        rule.setContent {
            WhaleReinTheme {
                AttachmentSaveRoute(
                    dependencies = dependencies,
                    session = TEST_SESSION,
                    attachmentId = "attachment-1",
                    onBack = {},
                )
            }
        }
        rule.waitUntil(5_000) {
            rule.onAllNodesWithText(text(R.string.support_attachment_title))
                .fetchSemanticsNodes().isNotEmpty()
        }
        rule.onNodeWithText(text(R.string.support_attachment_title)).assertIsDisplayed()
        rule.onNodeWithText("notes.txt").assertIsDisplayed()
    }

    @Test
    fun `AC-28_document_provider_write_persists_bytes_and_reports_failure`() {
        val gateway = rule.activity.documents
        val uri = android.net.Uri.parse("content://com.whalerein.android.test.documents/attachment")
        TestDocumentProvider.reset(context)

        assertTrue(runBlocking { gateway.write(uri, "verified bytes".toByteArray()) })
        assertEquals("verified bytes", TestDocumentProvider.read(context, uri))

        TestDocumentProvider.failWrites = true
        assertFalse(runBlocking { gateway.write(uri, "second write".toByteArray()) })
        assertEquals("verified bytes", TestDocumentProvider.read(context, uri))
    }

    private fun messageWithAttachment(available: Boolean): RenderedMessageItem =
        RenderedMessageItem(
            key = ConversationItemKey.Committed("message-1"),
            messageId = "message-1",
            role = MessageRole.Assistant,
            blocks = listOf(
                RenderedBlock(
                    blockId = "block-1",
                    kind = MessageBlockKind.Attachment,
                    text = "",
                    markdown = false,
                    selectable = false,
                    copyText = "",
                    attachment = RenderedAttachment(
                        attachmentId = "attachment-1",
                        name = "notes.txt",
                        available = available,
                    ),
                ),
            ),
            copyText = "",
        )
}
