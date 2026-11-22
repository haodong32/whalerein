package com.whalerein.android.state.access

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.InstanceKind
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.Route
import com.whalerein.android.state.core.ScreenModel
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch

data class WelcomeUiState(
    val loading: Boolean = true,
    val freshInstall: Boolean = false,
    val activeProfile: Profile? = null,
    val profiles: List<Profile> = emptyList(),
    val defaultRoute: Route = Route.SignIn,
    val choices: List<Route> = listOf(Route.SignIn, Route.StandaloneSetup),
    val destination: Route? = null,
    val error: CoreFailure? = null,
)

/**
 * Chooses the first screen for the current installation.
 *
 * A fresh install defaults to official Cloud, never to a device-code or local
 * credential form. A signed-out, unpaired or expired profile keeps an explicit
 * route choice instead of silently reusing stale authorization.
 */
class WelcomeViewModel(
    private val profiles: ProfilePort,
    private val startup: StartupPort,
    private val diagnostics: DiagnosticSink,
    private val dispatcher: CoroutineDispatcher = Dispatchers.Default,
) : ScreenModel<WelcomeUiState>(WelcomeUiState()) {

    init {
        scope.launch(dispatcher) {
            loadInternal()
        }
    }

    fun choose(route: Route) {
        if (route !in state.value.choices) {
            setState(
                state.value.copy(
                    error = CoreFailure("route_not_available", "That entry is not available."),
                ),
            )
            return
        }
        setState(state.value.copy(destination = route, error = null))
        diagnostics.accessEvent(
            level = DiagnosticLevel.Info,
            event = "startup_welcome_choice",
            stage = AccessStage.Startup,
            code = routeCode(route),
        )
    }

    fun continueWithCloud() {
        choose(state.value.defaultRoute)
    }

    private suspend fun loadInternal() {
        setState(state.value.copy(loading = true, error = null))
        val listed = guardedPortCall(diagnostics, AccessStage.Startup, "welcome_profiles") {
            profiles.list()
        }
        val activeResult = guardedPortCall(diagnostics, AccessStage.Startup, "welcome_active_profile") {
            profiles.active()
        }
        if (listed !is Outcome.Success || activeResult !is Outcome.Success) {
            val failure = if (listed !is Outcome.Success) listed.asFailure() else activeResult.asFailure()
            setState(
                state.value.copy(
                    loading = false,
                    freshInstall = false,
                    error = failure,
                ),
            )
            diagnostics.accessEvent(
                level = DiagnosticLevel.Warn,
                event = "startup_welcome_unavailable",
                stage = AccessStage.Startup,
                code = failure.code,
            )
            return
        }
        val allProfiles = listed.value
        val active = activeResult.value
        val freshInstall = allProfiles.isEmpty() && active == null
        val route = decideRoute(active, freshInstall)
        val choices = buildList {
            add(route)
            if (route != Route.SignIn) add(Route.SignIn)
            if (route != Route.StandaloneSetup) add(Route.StandaloneSetup)
            if (!freshInstall) add(Route.Profiles)
        }.distinct()
        setState(
            state.value.copy(
                loading = false,
                freshInstall = freshInstall,
                activeProfile = active,
                profiles = allProfiles,
                defaultRoute = route,
                choices = choices,
                destination = null,
                error = null,
            ),
        )
        diagnostics.accessEvent(
            level = DiagnosticLevel.Info,
            event = "startup_welcome_ready",
            stage = AccessStage.Startup,
            code = when {
                freshInstall -> "fresh_install"
                route == Route.SignIn -> "signed_out"
                route == Route.StandaloneSetup -> "unpaired_standalone"
                else -> "authorized"
            },
        )
    }

    private suspend fun decideRoute(active: Profile?, freshInstall: Boolean): Route {
        if (freshInstall || active == null) return Route.SignIn
        return when (active.kind) {
            InstanceKind.Cloud -> {
                val snapshot = guardedPortCall(diagnostics, AccessStage.Startup, "welcome_restore") {
                    startup.restore(active.instance)
                }.valueOrNull()
                when (snapshot?.authorization) {
                    StartupAuthorization.Authorized,
                    null -> Route.Directory

                    StartupAuthorization.SignedOut,
                    StartupAuthorization.Expired,
                    StartupAuthorization.Revoked,
                    StartupAuthorization.Unavailable -> Route.SignIn

                    StartupAuthorization.Unpaired -> Route.StandaloneSetup
                }
            }

            InstanceKind.Standalone -> {
                val snapshot = guardedPortCall(diagnostics, AccessStage.Startup, "welcome_restore") {
                    startup.restore(active.instance)
                }.valueOrNull()
                if (snapshot?.authorization == StartupAuthorization.Unpaired) {
                    Route.StandaloneSetup
                } else {
                    Route.Directory
                }
            }
        }
    }

    private fun routeCode(route: Route): String = when (route) {
        Route.SignIn -> "sign_in"
        Route.Directory -> "directory"
        Route.StandaloneSetup -> "standalone_setup"
        Route.Profiles -> "profiles"
        else -> "other"
    }
}
