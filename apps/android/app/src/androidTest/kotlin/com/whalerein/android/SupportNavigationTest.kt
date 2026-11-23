package com.whalerein.android

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.whalerein.android.navigation.AppNavHost
import com.whalerein.android.platform.applinks.AppLinkRouter
import com.whalerein.android.state.access.StartupAuthorization
import com.whalerein.android.state.access.StartupSnapshot
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.ui.theme.WhaleReinTheme
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Fresh-start navigation: a signed-out launch lands on the directory and the
 * standalone, account, retained-operation and diagnostics entries are reachable
 * from there. Cancelling or pressing back returns to the directory.
 */
@RunWith(AndroidJUnit4::class)
class SupportNavigationTest {
    @get:Rule
    val rule = createAndroidComposeRule<ComponentActivity>()

    private val context = InstrumentationRegistry.getInstrumentation().targetContext
    private fun text(id: Int) = context.getString(id)

    private fun launchSignedOut() {
        val dependencies = FakeAppDependencies().apply {
            startupOutcome = Outcome.Success(
                StartupSnapshot(authorization = StartupAuthorization.SignedOut),
            )
            fakeDirectory.listOutcome = Outcome.Failure(
                com.whalerein.android.state.core.CoreFailure(
                    "security",
                    "The operation was blocked for security.",
                ),
            )
            fakeDirectory.sessionOutcome = Outcome.Success(null)
        }
        val router = AppLinkRouter(RecordingDiagnosticSink())
        rule.setContent {
            WhaleReinTheme {
                AppNavHost(
                    dependencies = dependencies,
                    appLinks = router,
                    platform = null,
                    onRestartGraph = {},
                )
            }
        }
        waitFor(text(R.string.access_directory_title))
    }

    private fun waitFor(label: String) {
        rule.waitUntil(timeoutMillis = 5_000) {
            rule.onAllNodesWithText(label).fetchSemanticsNodes().isNotEmpty()
        }
        rule.onNodeWithText(label).assertIsDisplayed()
    }

    private fun pressBack() {
        rule.runOnUiThread {
            rule.activity.onBackPressedDispatcher.onBackPressed()
        }
        rule.waitForIdle()
    }

    @Test
    fun `AC-28_signed_out_directory_reaches_standalone_and_back`() {
        launchSignedOut()

        // Cloud sign-in stays available beside the Standalone entry.
        rule.onNodeWithText(text(R.string.access_directory_sign_in)).assertIsDisplayed()
        rule.onNodeWithText(text(R.string.access_directory_standalone_entry)).performClick()
        waitFor(text(R.string.access_standalone_title))

        pressBack()
        waitFor(text(R.string.access_directory_title))
    }

    @Test
    fun `AC-28_directory_reaches_account_and_operations`() {
        launchSignedOut()

        rule.onNodeWithText(text(R.string.access_directory_account_entry)).performClick()
        waitFor(text(R.string.support_account_title))
        pressBack()
        waitFor(text(R.string.access_directory_title))

        rule.onNodeWithText(text(R.string.access_directory_operations_entry)).performClick()
        waitFor(text(R.string.support_operations_title))
        pressBack()
        waitFor(text(R.string.access_directory_title))
    }

    @Test
    fun `AC-28_directory_reaches_diagnostics`() {
        launchSignedOut()

        rule.onNodeWithText(text(R.string.access_directory_diagnostics_entry)).performClick()
        waitFor(text(R.string.support_diagnostics_title))
        pressBack()
        waitFor(text(R.string.access_directory_title))
    }
}
