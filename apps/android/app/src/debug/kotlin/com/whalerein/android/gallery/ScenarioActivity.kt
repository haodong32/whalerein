package com.whalerein.android.gallery

import android.content.Context
import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.ui.Modifier
import androidx.core.view.WindowCompat
import com.whalerein.android.FakeAppDependencies
import com.whalerein.android.navigation.AppNavHost
import com.whalerein.android.platform.applinks.AppLinkRouter
import com.whalerein.android.ui.theme.WhaleReinTheme

/**
 * One Activity per scenario.
 *
 * The Activity store is the lifetime boundary for the scenario: the back stack,
 * view models, drafts and observation subscriptions all die with it, and the
 * fake graph is closed on destroy. Returning to the Gallery and choosing
 * another scenario therefore always starts from fresh fixtures.
 */
class ScenarioActivity : ComponentActivity() {
    private lateinit var scenario: GalleryScenario
    private lateinit var dependencies: FakeAppDependencies
    private lateinit var appLinks: AppLinkRouter

    override fun onCreate(savedInstanceState: Bundle?) {
        val resolved = intent.getStringExtra(ScenarioCatalog.EXTRA_SCENARIO_ID)
            ?.let(ScenarioCatalog::find)
        if (resolved == null) {
            super.onCreate(savedInstanceState)
            finish()
            return
        }
        scenario = resolved
        dependencies = scenario.create()
        appLinks = AppLinkRouter(dependencies.diagnosticSink)
        super.onCreate(savedInstanceState)
        title = scenario.title
        WindowCompat.enableEdgeToEdge(window)
        setContent {
            WhaleReinTheme {
                Surface(
                    modifier = Modifier.fillMaxSize(),
                    color = MaterialTheme.colorScheme.background,
                ) {
                    AppNavHost(
                        dependencies = dependencies,
                        appLinks = appLinks,
                        platform = null,
                        onRestartGraph = {},
                        initialBackStack = scenario.initialBackStack(),
                    )
                }
            }
        }
    }

    override fun onDestroy() {
        if (::dependencies.isInitialized) {
            dependencies.close()
        }
        super.onDestroy()
    }

    companion object {
        fun intent(context: Context, scenarioId: String): Intent =
            Intent(context, ScenarioActivity::class.java)
                .putExtra(ScenarioCatalog.EXTRA_SCENARIO_ID, scenarioId)
    }
}