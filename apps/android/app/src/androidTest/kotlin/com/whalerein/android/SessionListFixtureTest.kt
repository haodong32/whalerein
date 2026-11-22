package com.whalerein.android

import androidx.compose.runtime.getValue
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.whalerein.android.state.conversation.SessionListSnapshot
import com.whalerein.android.state.conversation.SessionListViewModel
import com.whalerein.android.state.conversation.SessionSummary
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.SessionId
import com.whalerein.android.ui.conversation.SessionListScreen
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class SessionListFixtureTest {
    @get:Rule
    val rule = createComposeRule()

    // AC-08.list-details: Host-qualified empty, populated and create-form states.
    @Test
    fun AC_08_session_list_empty_populated_and_create_form() {
        val dependencies = FakeAppDependencies()
        val port = dependencies.sessionList as FakeSessionListPort
        val host = HostRef(InstanceId("test-instance"), HostId("test-host"))
        port.snapshot = SessionListSnapshot(host = host, sessions = emptyList())
        val viewModel = SessionListViewModel(
            port = port,
            diagnostics = dependencies.diagnosticSink,
        )
        viewModel.open(host)
        rule.setContent {
            val state by viewModel.state.collectAsStateWithLifecycle()
            SessionListScreen(
                state = state,
                onBack = {},
                onRefresh = viewModel::refresh,
                onSelect = viewModel::select,
                onRefreshDetails = viewModel::refreshDetails,
                onShowCreate = viewModel::showCreateForm,
                onHideCreate = viewModel::hideCreateForm,
                onCreateInputChange = viewModel::updateCreateInput,
                onCreate = viewModel::create,
                onFork = viewModel::fork,
                onDismissDetails = viewModel::clearSelection,
                onClearOperation = viewModel::clearOperation,
            )
        }
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        rule.waitForIdle()
        rule.onNodeWithText(context.getString(R.string.conversation_sessions_empty))
            .assertIsDisplayed()

        port.snapshot = SessionListSnapshot(
            host = host,
            sessions = listOf(
                SessionSummary(
                    sessionId = SessionId("session-1"),
                    title = "First session",
                ),
            ),
        )
        rule.runOnUiThread { viewModel.refresh() }
        rule.waitForIdle()
        rule.onNodeWithText("First session").assertIsDisplayed()

        rule.onNodeWithText(context.getString(R.string.conversation_sessions_create))
            .assertIsDisplayed()
            .performClick()
        rule.waitForIdle()
        rule.onNodeWithText(context.getString(R.string.conversation_create_title))
            .assertIsDisplayed()
    }
}
