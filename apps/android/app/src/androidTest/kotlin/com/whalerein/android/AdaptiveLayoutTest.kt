package com.whalerein.android

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.Text
import androidx.compose.material3.adaptive.ExperimentalMaterial3AdaptiveApi
import androidx.compose.material3.adaptive.Posture
import androidx.compose.material3.adaptive.WindowAdaptiveInfo
import androidx.compose.material3.adaptive.layout.ListDetailPaneScaffold
import androidx.compose.material3.adaptive.layout.PaneAdaptedValue
import androidx.compose.material3.adaptive.layout.ThreePaneScaffoldValue
import androidx.compose.material3.adaptive.layout.calculatePaneScaffoldDirective
import androidx.compose.runtime.Composable
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.whalerein.android.state.conversation.ConversationViewModel
import com.whalerein.android.ui.conversation.ConversationScreen
import com.whalerein.android.ui.theme.WhaleReinTheme
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import androidx.window.core.layout.WindowSizeClass

@RunWith(AndroidJUnit4::class)
class AdaptiveLayoutTest {
    @get:Rule
    val rule = createComposeRule()

    @Test
    fun AC_13_adaptive_narrow_and_wide_keep_active_conversation_and_draft() {
        val dependencies = FakeAppDependencies()
        val viewModel = ConversationViewModel(
            observation = dependencies.observation,
            history = dependencies.history,
            composer = dependencies.composer,
            wait = dependencies.wait,
            commands = dependencies.commands,
            pendingInput = dependencies.pendingInput,
            drafts = dependencies.drafts,
            clock = dependencies.clock,
            ids = dependencies.ids,
            diagnostics = dependencies.diagnosticSink,
            queue = dependencies.modelsQueue,
        )
        viewModel.open(TEST_SESSION)
        viewModel.updateDraft("draft survives layout changes")
        val wide = mutableStateOf(false)
        rule.setContent {
            WhaleReinTheme {
                AdaptiveConversationFixture(
                    viewModel = viewModel,
                    wide = wide.value,
                )
            }
        }
        rule.onNodeWithText("draft survives layout changes").assertIsDisplayed()
        wide.value = true
        rule.waitForIdle()
        rule.onNodeWithText("session-list-pane").assertIsDisplayed()
        rule.onNodeWithText("draft survives layout changes").assertIsDisplayed()
        wide.value = false
        rule.waitForIdle()
        rule.onNodeWithText("draft survives layout changes").assertIsDisplayed()
    }
}

@OptIn(ExperimentalMaterial3AdaptiveApi::class)
@Composable
private fun AdaptiveConversationFixture(
    viewModel: ConversationViewModel,
    wide: Boolean,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val listState = rememberLazyListState()
    val conversation: @Composable () -> Unit = {
        ConversationScreen(
            state = state,
            listState = listState,
            onBack = {},
            onOpenTools = {},
            onOpenPendingInput = {},
            onLoadOlder = {},
            onLoadCached = {},
            onRetryHistory = {},
            onDraftChange = viewModel::updateDraft,
            onSelectMode = viewModel::selectDeliveryMode,
            onSelectFollowMode = viewModel::selectFollowMode,
            onSubmit = {},
            onStop = {},
            onWait = {},
            onCancelWait = {},
            onRecoverSend = {},
            onRetryContentRead = {},
            onConsumeAnnouncement = {},
            modifier = Modifier.fillMaxSize(),
        )
    }
    if (!wide) {
        conversation()
        return
    }
    val info = WindowAdaptiveInfo(
        windowSizeClass = WindowSizeClass(widthDp = 1200f, heightDp = 800f),
        windowPosture = Posture(),
    )
    ListDetailPaneScaffold(
        directive = calculatePaneScaffoldDirective(info),
        value = ThreePaneScaffoldValue(
            primary = PaneAdaptedValue.Expanded,
            secondary = PaneAdaptedValue.Expanded,
            tertiary = PaneAdaptedValue.Hidden,
        ),
        listPane = {
            Box(modifier = Modifier.fillMaxSize()) {
                Text("session-list-pane")
            }
        },
        detailPane = { conversation() },
    )
}
