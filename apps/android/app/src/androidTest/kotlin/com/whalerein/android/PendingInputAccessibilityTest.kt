package com.whalerein.android

import androidx.compose.runtime.getValue
import androidx.compose.ui.test.assertHasClickAction
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.whalerein.android.state.conversation.Guard
import com.whalerein.android.state.conversation.PendingApproval
import com.whalerein.android.state.conversation.PendingInputViewModel
import com.whalerein.android.state.conversation.PendingItem
import com.whalerein.android.ui.conversation.PendingInputSheet
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class PendingInputAccessibilityTest {
    @get:Rule
    val rule = createComposeRule()

    // AC-13.accessibility: pending-input approval controls are reachable.
    @Test
    fun AC_13_accessibility_pending_input_controls_are_reachable() {
        val dependencies = FakeAppDependencies()
        dependencies.fakePendingInput.items = listOf(
            PendingItem.Approval(
                PendingApproval(
                    approvalId = "approval-1",
                    title = "Run the command?",
                    body = "The Host is asking for approval.",
                    guard = Guard("guard-1"),
                ),
            ),
        )
        val viewModel = PendingInputViewModel(
            port = dependencies.pendingInput,
            clock = dependencies.clock,
            ids = dependencies.ids,
            diagnostics = dependencies.diagnosticSink,
        )
        viewModel.open(TEST_SESSION)
        viewModel.openItem("approval-1")
        rule.setContent {
            val state by viewModel.state.collectAsStateWithLifecycle()
            PendingInputSheet(
                state = state,
                onDismiss = {},
                onApprove = {},
                onReject = {},
                onToggleOption = { _, _ -> },
                onAnswerText = { _, _ -> },
                onSubmitAnswers = {},
                onCancelAnswer = {},
            )
        }
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        rule.waitForIdle()
        rule.onNodeWithText(context.getString(R.string.conversation_approve))
            .assertIsDisplayed()
            .assertHasClickAction()
        rule.onNodeWithText(context.getString(R.string.conversation_reject))
            .assertIsDisplayed()
            .assertHasClickAction()
    }
}
