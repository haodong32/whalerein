package com.whalerein.android

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertIsSelected
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.whalerein.android.ui.access.DebugEnvironmentPicker
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class DebugEnvironmentPickerTest {
    @get:Rule
    val compose = createComposeRule()

    // AC-05.debug-environment: the debug-only sign-in switch exposes all three
    // Cloud environments and reports the selected value.
    @Test
    fun AC_05_debug_environment_lists_production_staging_and_local() {
        var selected by mutableStateOf("production")
        compose.setContent {
            DebugEnvironmentPicker(
                current = selected,
                hasCa = false,
                busy = false,
                error = null,
                onSelect = { selected = it },
                onChooseCa = {},
                onApply = {},
            )
        }
        compose.onNodeWithText("Production").assertIsSelected()
        compose.onNodeWithText("Staging").performClick()
        compose.onNodeWithText("Staging").assertIsSelected()
        compose.onNodeWithText("Local").performClick()
        compose.onNodeWithText("Local").assertIsSelected()
    }
}
