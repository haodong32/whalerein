package com.whalerein.android.corebridge.ports

import com.whalerein.android.corebridge.AttachmentView
import com.whalerein.android.corebridge.BlockView
import com.whalerein.android.corebridge.MessageView
import com.whalerein.android.corebridge.MobileException
import com.whalerein.android.corebridge.MobileException.ProtocolVersionIncompatible
import com.whalerein.android.state.conversation.MessageBlockKind
import com.whalerein.android.state.conversation.MessageRole
import com.whalerein.android.state.core.CoreFailure
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

class AdapterMappingTest {
    @Test
    @DisplayName("conversation projection keeps role, kind, identity and attachment fields")
    fun `conversation projection`() {
        val message = MessageView(
            messageId = "m-1",
            role = "assistant",
            blocks = listOf(
                BlockView("b-1", "text", "answer", null, null, null, null),
                BlockView("b-2", "reasoning", "hidden", null, null, null, null),
                BlockView(
                    "b-3", "attachment", "file.bin", null, null, null,
                    AttachmentView("application/octet-stream", "file.bin", 4uL, true, "ref-1"),
                ),
            ),
            createdAtUnixMs = 7,
        )
        val mapped = message.toStateMessage(sequence = 3)
        assertEquals("m-1", mapped.messageId)
        assertEquals(MessageRole.Assistant, mapped.role)
        assertEquals(7, mapped.createdAtMillis)
        assertEquals(3, mapped.sequence)
        assertEquals(MessageBlockKind.Text, mapped.blocks[0].kind)
        assertEquals(MessageBlockKind.Reasoning, mapped.blocks[1].kind)
        assertEquals(MessageBlockKind.Attachment, mapped.blocks[2].kind)
        assertEquals("ref-1", mapped.blocks[2].attachmentId)
        assertEquals("file.bin", mapped.blocks[2].fields["attachment_name"])
    }

    @Test
    @DisplayName("opaque Core epochs map to a stable non-zero reducer identity")
    fun `opaque epoch mapping`() {
        assertTrue(liveEpochId("epoch-a") != 0L)
        assertEquals(liveEpochId("epoch-a"), liveEpochId("epoch-a"))
        assertTrue(liveEpochId("epoch-a") != liveEpochId("epoch-b"))
        assertEquals(0L, liveEpochId(""))
    }

    @Test
    @DisplayName("binding failures map to redacted typed codes")
    fun `failure mapping`() {
        assertEquals("invalid_input", MobileException.InvalidInput("x").toCoreFailure().code)
        assertEquals("host_offline", MobileException.HostOffline("x").toCoreFailure().code)
        assertEquals("timeout", MobileException.Timeout("x").toCoreFailure().code)
        assertEquals(
            "protocol_incompatible",
            ProtocolVersionIncompatible("x").toCoreFailure().code,
        )
        assertTrue(IllegalStateException("boom").toCoreFailure().code == "internal")
        assertEquals(
            CoreFailure("security", "The operation was blocked for security.").code,
            MobileException.Security("x").toCoreFailure().code,
        )
    }
}
