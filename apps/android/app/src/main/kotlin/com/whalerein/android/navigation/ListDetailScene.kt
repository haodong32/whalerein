@file:OptIn(androidx.compose.material3.adaptive.ExperimentalMaterial3AdaptiveApi::class)

package com.whalerein.android.navigation

import androidx.compose.material3.adaptive.currentWindowAdaptiveInfo
import androidx.compose.material3.adaptive.layout.AnimatedPane
import androidx.compose.material3.adaptive.layout.ListDetailPaneScaffold
import androidx.compose.material3.adaptive.layout.PaneAdaptedValue
import androidx.compose.material3.adaptive.layout.ThreePaneScaffoldValue
import androidx.compose.material3.adaptive.layout.calculatePaneScaffoldDirective
import androidx.compose.runtime.Composable
import androidx.navigation3.runtime.NavEntry
import androidx.navigation3.scene.Scene
import androidx.navigation3.scene.SceneStrategy
import androidx.navigation3.scene.SceneStrategyScope
import androidx.window.core.layout.WindowSizeClass

/** Navigation metadata key marking the role a destination may play. */
object NavRoles {
    const val ROLE_KEY = "com.whalerein.android.nav.role"
    const val LIST = "list"
    const val DETAIL = "detail"
}

/**
 * Shows the previous list destination and the current conversation in a
 * Material 3 adaptive list-detail scaffold when the window is at least medium.
 *
 * On compact windows the strategy returns null, so Navigation 3 falls back to a
 * single-pane scene. The same back stack is used in both layouts, which keeps
 * selection, view models and the draft across resize and rotation.
 */
class ListDetailSceneStrategy<T : Any>(
    private val twoPane: Boolean,
) : SceneStrategy<T> {
    override fun SceneStrategyScope<T>.calculateScene(entries: List<NavEntry<T>>): Scene<T>? {
        if (!twoPane) return null
        if (entries.size < 2) return null
        val detail = entries.last()
        val list = entries[entries.lastIndex - 1]
        if (detail.metadata[NavRoles.ROLE_KEY] != NavRoles.DETAIL) return null
        if (list.metadata[NavRoles.ROLE_KEY] != NavRoles.LIST) return null
        return ListDetailScene(
            key = detail.contentKey,
            listEntry = list,
            detailEntry = detail,
            previousEntries = entries.dropLast(1),
        )
    }
}

/** True when Material 3 adaptive would use more than one pane. */
@Composable
fun isTwoPaneWindow(): Boolean =
    currentWindowAdaptiveInfo().windowSizeClass
        .isWidthAtLeastBreakpoint(WindowSizeClass.WIDTH_DP_MEDIUM_LOWER_BOUND)

private data class ListDetailScene<T : Any>(
    override val key: Any,
    val listEntry: NavEntry<T>,
    val detailEntry: NavEntry<T>,
    override val previousEntries: List<NavEntry<T>>,
) : Scene<T> {
    override val entries: List<NavEntry<T>> = listOf(listEntry, detailEntry)

    override val content: @Composable () -> Unit = {
        ListDetailPaneScaffold(
            directive = calculatePaneScaffoldDirective(currentWindowAdaptiveInfo()),
            value = ThreePaneScaffoldValue(
                primary = PaneAdaptedValue.Expanded,
                secondary = PaneAdaptedValue.Expanded,
                tertiary = PaneAdaptedValue.Hidden,
            ),
            listPane = { AnimatedPane { listEntry.Content() } },
            detailPane = { AnimatedPane { detailEntry.Content() } },
        )
    }
}
