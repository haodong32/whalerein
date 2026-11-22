package com.whalerein.android.state.conversation

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.BeforeEach

/**
 * Installs the test main dispatcher used by `viewModelScope`.
 *
 * Tests pass the same [dispatcher] to `runTest` so scheduled work, the screen
 * model scope and the test scope share one deterministic scheduler.
 */
@OptIn(ExperimentalCoroutinesApi::class)
abstract class ConversationTestBase {
    protected val dispatcher = StandardTestDispatcher()

    @BeforeEach
    fun installMainDispatcher() {
        Dispatchers.setMain(dispatcher)
    }

    @AfterEach
    fun resetMainDispatcher() {
        Dispatchers.resetMain()
    }
}
