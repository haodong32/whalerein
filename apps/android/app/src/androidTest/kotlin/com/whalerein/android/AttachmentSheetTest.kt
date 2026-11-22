package com.whalerein.android

import androidx.compose.runtime.getValue
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.whalerein.android.state.attachments.AttachmentSaveViewModel
import com.whalerein.android.state.attachments.AttachmentScope
import com.whalerein.android.ui.support.AttachmentSaveSheet
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class AttachmentSheetTest {
    @get:Rule
    val rule = createComposeRule()

    // AC-20.verified: verified staging reaches the picker writer; transfer and
    // save remain separate observable steps.
    @Test
    fun AC_20_verified_attachment_sheet_progresses_to_save() {
        val dependencies = FakeAppDependencies()
        val scope = AttachmentScope(TEST_SESSION.host.instance, TEST_SESSION, "attachment-1")
        val viewModel = AttachmentSaveViewModel(
            attachmentScope = scope,
            operationId = "attachment:test",
            port = dependencies.attachments,
            staging = dependencies.attachmentStaging,
            authorization = dependencies.attachmentAuthorization,
            writer = dependencies.destinationWriter,
            clock = dependencies.clock,
            diagnostics = dependencies.diagnosticSink,
            processId = "test-process",
        )
        runBlocking {
            viewModel.load()
            viewModel.startTransfer()
        }
        val coroutineScope = CoroutineScope(Dispatchers.Main + SupervisorJob())
        rule.setContent {
            val state by viewModel.state.collectAsStateWithLifecycle()
            AttachmentSaveSheet(
                state = state,
                onStartTransfer = {},
                onRetryTransfer = {},
                onBeginSave = { coroutineScope.launch { viewModel.beginSave() } },
                onCancelTransfer = {},
                onCancelSave = {},
                onDeadlineOverride = {},
                onDismiss = {},
            )
        }
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        rule.onNodeWithText(context.getString(R.string.support_attachment_save)).assertIsDisplayed()
        rule.onNodeWithText(context.getString(R.string.support_attachment_save)).performClick()
        rule.waitUntil(5_000) { viewModel.state.value.saved }
        rule.waitForIdle()
        assertEquals("hello", dependencies.fakeWriter.written?.second?.decodeToString())
    }
}
