package com.whalerein.android

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.whalerein.android.state.conversation.ConversationViewModel
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class ObservationLifecycleTest {
    // AC-11.lifecycle: rotation/resume reuses one observation; leaving the
    // foreground stops exactly the scoped watch without a duplicate consumer.
    @Test
    fun AC_11_lifecycle_reuses_one_observation_across_visibility_changes() {
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
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        instrumentation.runOnMainSync {
            viewModel.open(TEST_SESSION)
            viewModel.onVisible()
            viewModel.onVisible()
        }
        instrumentation.waitForIdleSync()
        assertEquals(1, dependencies.fakeObservation.startCount)

        instrumentation.runOnMainSync { viewModel.onHidden() }
        instrumentation.waitForIdleSync()
        assertTrue(dependencies.fakeObservation.stopCount >= 1)

        instrumentation.runOnMainSync { viewModel.onVisible() }
        instrumentation.waitForIdleSync()
        assertEquals(2, dependencies.fakeObservation.startCount)
    }
}
