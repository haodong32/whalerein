package com.whalerein.android.gallery

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.core.view.WindowCompat
import com.whalerein.android.ui.theme.WhaleReinTheme

/**
 * Debug-only launcher surface.
 *
 * Each row starts a [ScenarioActivity] with fresh in-memory dependencies, so a
 * scenario can never observe navigation or subscriptions from an earlier run.
 */
class StateGalleryActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        WindowCompat.enableEdgeToEdge(window)
        setContent {
            WhaleReinTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    StateGalleryScreen(
                        scenarios = ScenarioCatalog.all,
                        onOpen = ::openScenario,
                    )
                }
            }
        }
    }

    private fun openScenario(scenario: GalleryScenario) {
        startActivity(ScenarioActivity.intent(this, scenario.id))
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun StateGalleryScreen(
    scenarios: List<GalleryScenario>,
    onOpen: (GalleryScenario) -> Unit,
) {
    Scaffold(
        modifier = Modifier.fillMaxSize(),
        topBar = {
            TopAppBar(title = { Text("WhaleRein State Gallery") })
        },
    ) { padding ->
        LazyColumn(
            modifier = Modifier
                .padding(padding)
                .fillMaxSize()
                .testTag("gallery.scenarios"),
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            item {
                Text(
                    text = "Offline Debug scenarios rendered by the production Compose screens. " +
                        "Every row opens fresh deterministic fixtures; no Core or Host is opened.",
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
            items(scenarios, key = { it.id }) { scenario ->
                Card(
                    modifier = Modifier
                        .fillMaxWidth()
                        .testTag("gallery.scenario.${scenario.id}")
                        .clickable { onOpen(scenario) },
                ) {
                    Column(
                        modifier = Modifier.padding(16.dp),
                        verticalArrangement = Arrangement.spacedBy(4.dp),
                    ) {
                        Text(
                            text = scenario.title,
                            style = MaterialTheme.typography.titleMedium,
                        )
                        Text(
                            text = scenario.id,
                            style = MaterialTheme.typography.labelMedium,
                            fontFamily = FontFamily.Monospace,
                        )
                        Text(
                            text = scenario.notes,
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                }
            }
        }
    }
}