package com.whalerein.android

import android.content.Context
import android.content.res.Configuration
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.whalerein.android.state.access.WelcomeUiState
import com.whalerein.android.ui.access.WelcomeScreen
import com.whalerein.android.ui.theme.WhaleReinTheme
import java.util.Locale
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class LocaleThemeTest {
    @get:Rule
    val rule = createComposeRule()

    // AC-13.locale-theme: English and Simplified Chinese, light and dark.
    @Test
    fun AC_13_locale_theme_renders_both_locales_and_appearances() {
        val base = InstrumentationRegistry.getInstrumentation().targetContext
        val english = base.createConfigurationContext(
            Configuration(base.resources.configuration).apply { setLocale(Locale.ENGLISH) },
        )
        val chinese = base.createConfigurationContext(
            Configuration(base.resources.configuration).apply {
                setLocale(Locale.SIMPLIFIED_CHINESE)
            },
        )
        val locale = mutableStateOf<Context>(english)
        val dark = mutableStateOf(false)
        rule.setContent {
            val context = locale.value
            CompositionLocalProvider(
                LocalContext provides context,
                LocalConfiguration provides context.resources.configuration,
            ) {
                WhaleReinTheme(darkTheme = dark.value) {
                    WelcomeScreen(
                        state = WelcomeUiState(loading = false),
                        onContinue = {},
                        onNavigate = {},
                    )
                }
            }
        }
        rule.onNodeWithText(english.getString(R.string.access_welcome_title)).assertIsDisplayed()
        dark.value = true
        rule.waitForIdle()
        rule.onNodeWithText(english.getString(R.string.access_welcome_title)).assertIsDisplayed()
        locale.value = chinese
        rule.waitForIdle()
        rule.onNodeWithText(chinese.getString(R.string.access_welcome_title)).assertIsDisplayed()
        dark.value = false
        rule.waitForIdle()
        rule.onNodeWithText(chinese.getString(R.string.access_welcome_title)).assertIsDisplayed()
    }
}
