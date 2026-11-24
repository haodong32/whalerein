package com.whalerein.android

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.ui.Modifier
import androidx.core.view.WindowCompat
import com.whalerein.android.platform.ActivityPlatform
import com.whalerein.android.ui.AppRoot
import com.whalerein.android.ui.theme.WhaleReinTheme

/**
 * The single Activity.
 *
 * It owns the system intents (hosted sign-in callback and future external
 * session taps), registers the picker/permission launchers before composition,
 * and delegates all navigation to the typed Navigation 3 back stack.
 */
class MainActivity : ComponentActivity() {
    private lateinit var platform: ActivityPlatform

    private val container: AppContainer
        get() = (application as WhaleReinApplication).container

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Debug builds defer the production graph so the Gallery can cold start
        // without Core. The graph must exist before platform services attach or
        // an App Link is dispatched.
        val container = (application as WhaleReinApplication).ensureGraph()
        platform = ActivityPlatform(this, container.diagnostics.sink)
        container.platform.attach(platform)
        container.appLinks.dispatch(intent)
        WindowCompat.enableEdgeToEdge(window)
        setContent {
            WhaleReinTheme {
                Surface(
                    modifier = Modifier.fillMaxSize(),
                    color = MaterialTheme.colorScheme.background,
                ) {
                    AppRoot(container = container, platform = platform)
                }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        container.appLinks.dispatch(intent)
    }

    override fun onDestroy() {
        container.platform.detach(platform)
        super.onDestroy()
    }
}
