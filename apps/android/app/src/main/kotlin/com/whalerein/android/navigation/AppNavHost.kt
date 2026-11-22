package com.whalerein.android.navigation

import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.runtime.getValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.navigation3.rememberViewModelStoreNavEntryDecorator
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider
import androidx.navigation3.runtime.rememberNavBackStack
import androidx.navigation3.runtime.rememberSaveableStateHolderNavEntryDecorator
import androidx.navigation3.ui.NavDisplay
import com.whalerein.android.AppDependencies
import com.whalerein.android.platform.ActivityPlatform
import com.whalerein.android.platform.applinks.AppLinkRouter
import com.whalerein.android.state.access.ExternalEntryResolver
import com.whalerein.android.state.core.Route
import com.whalerein.android.state.core.SessionRef
import com.whalerein.android.ui.routes.AccountRoute
import com.whalerein.android.ui.routes.AttachmentSaveRoute
import com.whalerein.android.ui.routes.CloudSignInRoute
import com.whalerein.android.ui.routes.ConversationRoute
import com.whalerein.android.ui.routes.DiagnosticsRoute
import com.whalerein.android.ui.routes.DirectoryRoute
import com.whalerein.android.ui.routes.OperationsRoute
import com.whalerein.android.ui.routes.ProfileSettingsRoute
import com.whalerein.android.ui.routes.ProfilesRoute
import com.whalerein.android.ui.routes.SessionToolsRoute
import com.whalerein.android.ui.routes.SessionsRoute
import com.whalerein.android.ui.routes.StandaloneSetupRoute
import com.whalerein.android.ui.routes.StartupRoute
import com.whalerein.android.ui.routes.WelcomeRoute

/**
 * One Activity, one typed Navigation 3 back stack.
 *
 * Destination keys are saved by `rememberNavBackStack`, and the entry view-model
 * store is scoped per back-stack entry, so process recreation, rotation and
 * resize keep the active conversation, draft and observation generation.
 */
@Composable
fun AppNavHost(
    dependencies: AppDependencies,
    appLinks: AppLinkRouter,
    platform: ActivityPlatform?,
) {
    val backStack = rememberNavBackStack(StartupKey)
    val twoPane = isTwoPaneWindow()
    val strategy = remember(twoPane) { ListDetailSceneStrategy<NavKey>(twoPane) }
    val callback by appLinks.callback.collectAsStateWithLifecycle()
    val externalTarget by appLinks.externalTarget.collectAsStateWithLifecycle()
    val resolver = remember(dependencies) {
        ExternalEntryResolver(dependencies.externalEntry, dependencies.diagnosticSink)
    }

    fun navigate(key: NavKey) {
        if (backStack.lastOrNull() == key) return
        backStack.add(key)
    }

    fun navigate(route: Route) {
        navigate(RouteNavMapper.toKey(route))
    }

    fun replaceRoot(key: NavKey) {
        if (backStack.isEmpty()) {
            backStack.add(key)
        } else {
            backStack[0] = key
            while (backStack.size > 1) backStack.removeAt(backStack.lastIndex)
        }
    }

    fun goBack() {
        if (backStack.size > 1) backStack.removeAt(backStack.lastIndex)
    }

    LaunchedEffect(callback) {
        if (callback == null) return@LaunchedEffect
        if (backStack.lastOrNull() !is SignInKey) navigate(SignInKey)
    }

    LaunchedEffect(externalTarget) {
        val target = externalTarget ?: return@LaunchedEffect
        appLinks.consumeExternalTarget()
        val resolution = resolver.resolve(target)
        val route = resolution.route
        if (route == null) {
            if (backStack.lastOrNull() !is DirectoryKey) navigate(DirectoryKey)
        } else {
            navigate(route)
        }
    }

    NavDisplay(
        backStack = backStack,
        onBack = ::goBack,
        entryDecorators = listOf(
            rememberSaveableStateHolderNavEntryDecorator(),
            rememberViewModelStoreNavEntryDecorator(),
        ),
        sceneStrategies = listOf(strategy),
        entryProvider = entryProvider<NavKey> {
            entry<StartupKey> {
                StartupRoute(
                    dependencies = dependencies,
                    onResolved = { route, _ ->
                        if (callback != null) {
                            replaceRoot(SignInKey)
                        } else {
                            replaceRoot(RouteNavMapper.toKey(route))
                        }
                    },
                    onOpenDiagnostics = {
                        replaceRoot(DiagnosticsKey)
                    },
                )
            }
            entry<WelcomeKey> {
                WelcomeRoute(dependencies = dependencies, onNavigate = ::navigate)
            }
            entry<SignInKey> {
                CloudSignInRoute(
                    dependencies = dependencies,
                    appLinks = appLinks,
                    platform = platform,
                    onNavigate = ::navigate,
                )
            }
            entry<DirectoryKey>(
                metadata = mapOf(NavRoles.ROLE_KEY to NavRoles.LIST),
            ) {
                DirectoryRoute(dependencies = dependencies, onNavigate = ::navigate)
            }
            entry<StandaloneSetupKey> {
                StandaloneSetupRoute(
                    dependencies = dependencies,
                    platform = platform,
                    onNavigate = ::navigate,
                )
            }
            entry<ProfilesKey> {
                ProfilesRoute(
                    dependencies = dependencies,
                    onOpenProfile = { instance ->
                        navigate(ProfileSettingsKey(instance.value))
                    },
                    onNavigate = ::navigate,
                )
            }
            entry<ProfileSettingsKey> { key ->
                ProfileSettingsRoute(
                    dependencies = dependencies,
                    instanceId = com.whalerein.android.state.core.InstanceId(key.instanceId),
                    onNavigate = ::navigate,
                )
            }
            entry<SessionsKey>(
                metadata = mapOf(NavRoles.ROLE_KEY to NavRoles.LIST),
            ) { key ->
                SessionsRoute(
                    dependencies = dependencies,
                    instanceId = com.whalerein.android.state.core.InstanceId(key.instanceId),
                    hostId = com.whalerein.android.state.core.HostId(key.hostId),
                    onBack = ::goBack,
                    onOpenSession = { session ->
                        navigate(RouteNavMapper.toKey(Route.Conversation(session)))
                    },
                )
            }
            entry<ConversationKey>(
                metadata = mapOf(NavRoles.ROLE_KEY to NavRoles.DETAIL),
            ) { key ->
                ConversationRoute(
                    dependencies = dependencies,
                    session = RouteNavMapper.sessionRef(key),
                    focusItemId = key.focusItemId,
                    openPendingSheet = key.pendingSheet,
                    onBack = ::goBack,
                    onOpenTools = { navigate(RouteNavMapper.toKey(Route.SessionTools(RouteNavMapper.sessionRef(key)))) },
                )
            }
            entry<SessionToolsKey>(
                metadata = mapOf(NavRoles.ROLE_KEY to NavRoles.DETAIL),
            ) { key ->
                SessionToolsRoute(
                    dependencies = dependencies,
                    session = RouteNavMapper.sessionRef(key),
                    onBack = ::goBack,
                )
            }
            entry<AttachmentSaveKey>(
                metadata = mapOf(NavRoles.ROLE_KEY to NavRoles.DETAIL),
            ) { key ->
                AttachmentSaveRoute(
                    dependencies = dependencies,
                    session = RouteNavMapper.sessionRef(key),
                    attachmentId = key.attachmentId,
                    onBack = ::goBack,
                )
            }
            entry<OperationsKey> {
                OperationsRoute(dependencies = dependencies, onBack = ::goBack)
            }
            entry<AccountKey> {
                AccountRoute(dependencies = dependencies, onBack = ::goBack)
            }
            entry<DiagnosticsKey> {
                DiagnosticsRoute(dependencies = dependencies, onBack = ::goBack)
            }
        },
    )
}

/** Resolves an external hint once and never caches an old authorization. */
internal fun SessionRef.toConversationKey(focusItemId: String? = null): ConversationKey =
    ConversationKey(
        instanceId = host.instance.value,
        hostId = host.hostId.value,
        sessionId = sessionId.value,
        focusItemId = focusItemId,
    )
