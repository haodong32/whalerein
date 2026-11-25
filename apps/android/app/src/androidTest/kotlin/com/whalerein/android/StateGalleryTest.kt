package com.whalerein.android

import android.content.Intent
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollToNode
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.whalerein.android.gallery.ScenarioActivity
import com.whalerein.android.gallery.ScenarioCatalog
import com.whalerein.android.gallery.StateGalleryActivity
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * State Gallery device coverage: one catalog, launcher entry, conversation
 * navigation and scenario isolation.
 *
 * The tests launch the debug Activities directly so they exercise the same
 * catalog and fake graph a developer sees, not a parallel fixture.
 */
@RunWith(AndroidJUnit4::class)
class StateGalleryTest {
    @get:Rule
    val rule = createEmptyComposeRule()

    private val context get() = InstrumentationRegistry.getInstrumentation().targetContext

    // AC-gallery.catalog: the launcher lists every stable scenario ID.
    @Test
    fun StateGallery_catalog_lists_every_stable_scenario() {
        val gallery = ActivityScenario.launch<StateGalleryActivity>(
            Intent(context, StateGalleryActivity::class.java),
        )
        try {
            rule.waitUntil(TIMEOUT_MILLIS) {
                rule.onAllNodesWithTag("gallery.scenario.${ScenarioCatalog.all.first().id}")
                    .fetchSemanticsNodes().isNotEmpty()
            }
            ScenarioCatalog.all.forEach { scenario ->
                rule.onNodeWithTag("gallery.scenarios").performScrollToNode(
                    hasTestTag("gallery.scenario.${scenario.id}"),
                )
                rule.onNodeWithTag("gallery.scenario.${scenario.id}")
                    .assertIsDisplayed()
                rule.onNodeWithText(scenario.title).assertIsDisplayed()
            }
        } finally {
            gallery.close()
        }
    }

    // AC-gallery.navigation: selecting a session opens the production
    // conversation and Back returns to the production list.
    @Test
    fun StateGallery_populated_session_navigates_and_back_returns_to_list() {
        val scenario = ActivityScenario.launch<ScenarioActivity>(
            ScenarioActivity.intent(context, "sessions.populated"),
        )
        try {
            rule.waitUntil(TIMEOUT_MILLIS) { hasText("Release readiness") }
            rule.onNodeWithText("Release readiness").performClick()
            rule.waitUntil(TIMEOUT_MILLIS) {
                hasText("Summarize the release readiness check.")
            }
            rule.onNodeWithText("Summarize the release readiness check.").assertIsDisplayed()

            scenario.onActivity { activity ->
                activity.onBackPressedDispatcher.onBackPressed()
            }
            val sessionsTitle = context.getString(R.string.conversation_sessions_title)
            rule.waitUntil(TIMEOUT_MILLIS) { hasText(sessionsTitle) }
            rule.onNodeWithText(sessionsTitle).assertIsDisplayed()
        } finally {
            scenario.close()
        }
    }

    // AC-gallery.question: a deterministic question batch opens with its
    // options and open-answer field.
    @Test
    fun StateGallery_question_scenario_shows_prompt_and_options() {
        val question = ActivityScenario.launch<ScenarioActivity>(
            ScenarioActivity.intent(context, "conversation.question"),
        )
        try {
            rule.waitUntil(TIMEOUT_MILLIS) { hasText(QUESTION_PROMPT) }
            rule.onNodeWithText(QUESTION_PROMPT).assertIsDisplayed()
            rule.onNodeWithText("Staging").assertIsDisplayed()
            rule.onNodeWithText("Production").assertIsDisplayed()
            rule.onNodeWithText(QUESTION_OPEN_PROMPT).assertIsDisplayed()

            // An applied answer closes the review and clears the fixed entry.
            rule.onNodeWithText("Staging").performClick()
            rule.onNodeWithText(context.getString(R.string.conversation_submit_answers))
                .performClick()
            rule.waitUntil(TIMEOUT_MILLIS) { !hasText(QUESTION_PROMPT) }
            rule.onNodeWithText(QUESTION_PROMPT).assertDoesNotExist()
        } finally {
            question.close()
        }
    }

    // AC-gallery.approval: an approved item leaves the review and the entry.
    @Test
    fun StateGallery_approval_decision_applies_and_closes_the_review() {
        val approval = ActivityScenario.launch<ScenarioActivity>(
            ScenarioActivity.intent(context, "conversation.approval"),
        )
        try {
            rule.waitUntil(TIMEOUT_MILLIS) { hasText(APPROVAL_TITLE) }
            rule.onNodeWithText(context.getString(R.string.conversation_approve))
                .performClick()
            rule.waitUntil(TIMEOUT_MILLIS) { !hasText(APPROVAL_TITLE) }
            rule.onNodeWithText(APPROVAL_TITLE).assertDoesNotExist()
        } finally {
            approval.close()
        }
    }

    // AC-gallery.streaming: incremental previews collapse into the committed
    // final message, which replaces the provisional block.
    @Test
    fun StateGallery_streaming_replaces_preview_with_final_message() {
        val streaming = ActivityScenario.launch<ScenarioActivity>(
            ScenarioActivity.intent(context, "conversation.streaming"),
        )
        try {
            rule.waitUntil(TIMEOUT_MILLIS) { hasText(STREAMING_PREVIEW_TEXT) }
            rule.waitUntil(TIMEOUT_MILLIS) { hasText(STREAMING_FINAL_TEXT) }
            rule.onNodeWithText(STREAMING_FINAL_TEXT).assertIsDisplayed()
            rule.onNodeWithText(STREAMING_PREVIEW_TEXT).assertDoesNotExist()
        } finally {
            streaming.close()
        }
    }

    // AC-gallery.reset: closing a streaming scenario disposes its stack and
    // subscriptions; a following approval scenario never sees its late events.
    @Test
    fun StateGallery_scenario_switch_drops_previous_stream_events() {
        val streaming = ActivityScenario.launch<ScenarioActivity>(
            ScenarioActivity.intent(context, "conversation.streaming"),
        )
        try {
            // Close while the scripted stream still has events left to deliver.
            rule.waitUntil(TIMEOUT_MILLIS) { hasText(STREAMING_PREVIEW_TEXT) }
        } finally {
            streaming.close()
        }

        val approval = ActivityScenario.launch<ScenarioActivity>(
            ScenarioActivity.intent(context, "conversation.approval"),
        )
        try {
            rule.waitUntil(TIMEOUT_MILLIS) { hasText(APPROVAL_TITLE) }
            rule.onNodeWithText(APPROVAL_TITLE).assertIsDisplayed()
            // Give any late stream event from the closed scenario time to land.
            Thread.sleep(1_500)
            rule.onNodeWithText(STREAMING_FINAL_TEXT).assertDoesNotExist()
            rule.onNodeWithText(STREAMING_PREVIEW_TEXT).assertDoesNotExist()
        } finally {
            approval.close()
        }
    }

    private fun hasText(text: String): Boolean =
        rule.onAllNodesWithText(text).fetchSemanticsNodes().isNotEmpty()

    private companion object {
        const val TIMEOUT_MILLIS = 10_000L
        const val STREAMING_PREVIEW_TEXT = "Checking the pipelines and"
        const val STREAMING_FINAL_TEXT = "Release is ready: all checks passed."
        const val APPROVAL_TITLE = "Run the deployment script?"
        const val QUESTION_PROMPT = "Which environment should the Host deploy to?"
        const val QUESTION_OPEN_PROMPT = "What should the release note mention?"
    }
}