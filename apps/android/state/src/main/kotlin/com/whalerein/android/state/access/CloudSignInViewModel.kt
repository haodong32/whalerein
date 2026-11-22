package com.whalerein.android.state.access

import com.whalerein.android.state.core.AttemptId
import com.whalerein.android.state.core.Clock
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.Route
import com.whalerein.android.state.core.ScreenModel
import java.net.URI
import java.net.URLDecoder
import java.nio.charset.StandardCharsets
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch

enum class CloudSignInPhase {
    Idle,
    Starting,
    AwaitingBrowser,
    Completing,
    Recovering,
    Succeeded,
    Unconfirmed,
    Failed,
    Expired,
    Cancelled,
}

data class CloudSignInUiState(
    val phase: CloudSignInPhase = CloudSignInPhase.Idle,
    val attempt: CloudAttempt? = null,
    /** The last verified session, kept through an unrelated failed attempt. */
    val previousSession: CloudSession? = null,
    val session: CloudSession? = null,
    val destination: Route? = null,
    val error: CoreFailure? = null,
    val canRestart: Boolean = false,
)

/**
 * Drives the hosted native PKCE sign-in.
 *
 * The screen never renders a device code or a local credential form. Callback
 * validation is state's last line of defence: the redirect origin/path,
 * environment, state and code presence are checked before Core is asked to
 * exchange anything, and a second callback for the same attempt is ignored.
 */
class CloudSignInViewModel(
    private val instance: InstanceId,
    private val auth: CloudAuthPort,
    private val clock: Clock,
    private val diagnostics: DiagnosticSink,
    private val environment: String = "local",
    private val expectedRedirect: String = DEFAULT_REDIRECT,
    private val dispatcher: CoroutineDispatcher = Dispatchers.Default,
) : ScreenModel<CloudSignInUiState>(CloudSignInUiState()) {

    private val knownStates = mutableMapOf<String, AttemptId>()
    private val completedAttempts = mutableSetOf<AttemptId>()

    init {
        scope.launch(dispatcher) {
            bootstrap()
        }
    }

    /** Starts the hosted browser flow, or reports why an active one continues. */
    fun start() {
        scope.launch(dispatcher) {
            startInternal()
        }
    }

    /**
     * Explicitly replaces the pending attempt: the old attempt is cancelled
     * and fenced before a new attempt is created.
     */
    fun replaceAttempt() {
        scope.launch(dispatcher) {
            val previous = state.value.attempt
            if (previous != null) {
                guardedPortCall(diagnostics, AccessStage.Login, "login_replace_cancel", instance) {
                    auth.cancel(instance, previous.attemptId)
                }
                setState(
                    state.value.copy(
                        phase = CloudSignInPhase.Cancelled,
                        attempt = null,
                        error = null,
                        canRestart = true,
                    ),
                )
            }
            startInternal()
        }
    }

    private suspend fun startInternal() {
        val phase = state.value.phase
        if (phase != CloudSignInPhase.Idle &&
            phase != CloudSignInPhase.Failed &&
            phase != CloudSignInPhase.Expired &&
            phase != CloudSignInPhase.Cancelled
        ) {
            diagnostics.accessEvent(
                level = DiagnosticLevel.Info,
                event = "login_attempt_already_active",
                stage = AccessStage.Login,
                code = phase.name.lowercase(),
                instanceId = instance,
            )
            return
        }
        setState(state.value.copy(phase = CloudSignInPhase.Starting, error = null, destination = null))
        val result = guardedPortCall(diagnostics, AccessStage.Login, "login_start", instance) {
            auth.start(instance, environment)
        }
        when (result) {
            is Outcome.Success -> {
                val attempt = result.value
                if (!isSecureAuthorizationUrl(attempt.authorizationUrl)) {
                    failAttempt(
                        "authorization_url_insecure",
                        "The hosted sign-in endpoint is not secure.",
                    )
                    return
                }
                if (attempt.environment != environment || !isExpectedRedirect(attempt.redirectUri)) {
                    failAttempt(
                        "attempt_binding_mismatch",
                        "The sign-in attempt did not match this environment.",
                    )
                    return
                }
                knownStates[attempt.state] = attempt.attemptId
                setState(
                    state.value.copy(
                        phase = CloudSignInPhase.AwaitingBrowser,
                        attempt = attempt,
                        error = null,
                        canRestart = false,
                    ),
                )
                diagnostics.accessEvent(
                    level = DiagnosticLevel.Info,
                    event = "login_attempt_started",
                    stage = AccessStage.Login,
                    instanceId = instance,
                )
            }

            else -> {
                setState(
                    state.value.copy(
                        phase = CloudSignInPhase.Failed,
                        attempt = null,
                        error = result.asFailure(),
                        canRestart = true,
                    ),
                )
                diagnostics.accessEvent(
                    level = DiagnosticLevel.Warn,
                    event = "login_start_failed",
                    stage = AccessStage.Login,
                    code = result.asFailure().code,
                    instanceId = instance,
                )
            }
        }
    }

    /**
     * Handles a browser return. Invalid, replayed or stale callbacks never
     * reach the exchange and never replace the currently pending attempt.
     */
    fun handleCallback(callbackUri: String) {
        scope.launch(dispatcher) {
            if (state.value.phase == CloudSignInPhase.Succeeded) {
                rejectCallback("callback_replayed", "This sign-in callback was already used.")
                return@launch
            }
            val attempt = state.value.attempt
            if (attempt == null) {
                rejectCallback("callback_no_attempt", "There is no pending sign-in.")
                return@launch
            }
            if (attempt.attemptId in completedAttempts) {
                rejectCallback("callback_replayed", "This sign-in callback was already used.")
                return@launch
            }
            val callback = parseCallback(callbackUri)
            if (callback == null) {
                rejectCallback("callback_invalid", "The sign-in callback was not valid.")
                return@launch
            }
            if (!sameOriginAndPath(callback.uri, attempt.redirectUri)) {
                rejectCallback("callback_redirect", "The sign-in callback had the wrong destination.")
                return@launch
            }
            val environments = callback.parameters["environment"].orEmpty()
            if (environments.size != 1 || environments[0] != attempt.environment) {
                rejectCallback("callback_environment", "The sign-in callback had the wrong environment.")
                return@launch
            }
            val states = callback.parameters["state"].orEmpty()
            if (states.size != 1) {
                rejectCallback("callback_state", "The sign-in callback had no single state.")
                return@launch
            }
            if (states[0] != attempt.state) {
                val code = if (knownStates.containsKey(states[0])) {
                    "attempt_obsolete"
                } else {
                    "callback_state"
                }
                rejectCallback(code, "The sign-in callback did not match the pending attempt.")
                return@launch
            }
            val errors = callback.parameters["error"].orEmpty()
            if (errors.isNotEmpty()) {
                if (errors.size == 1 && errors[0] == "access_denied") {
                    cancelInternal("attempt_cancelled", "The sign-in was cancelled.")
                } else {
                    rejectCallback("callback_error", "The sign-in returned an error.")
                }
                return@launch
            }
            val codes = callback.parameters["code"].orEmpty()
            if (codes.size != 1 || codes[0].isBlank()) {
                rejectCallback("callback_code", "The sign-in callback had no single code.")
                return@launch
            }
            if (isExpired(attempt)) {
                expire(attempt)
                return@launch
            }
            if (state.value.attempt?.attemptId != attempt.attemptId) {
                rejectCallback("attempt_obsolete", "The sign-in attempt was replaced.")
                return@launch
            }
            setState(state.value.copy(phase = CloudSignInPhase.Completing, error = null))
            val result = try {
                auth.complete(instance, attempt.attemptId, callbackUri)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                diagnostics.accessEvent(
                    level = DiagnosticLevel.Error,
                    event = "login_exchange_exception",
                    stage = AccessStage.Login,
                    code = "unexpected_failure",
                    instanceId = instance,
                )
                CloudAuthResult.Unconfirmed(
                    CoreFailure("exchange_unconfirmed", "The sign-in result is unconfirmed.", true),
                )
            }
            applyResult(attempt, result, recoveryAllowed = true)
        }
    }

    /** Retries the credential lookup for an unconfirmed attempt. Never exchanges. */
    fun recover() {
        scope.launch(dispatcher) {
            val attempt = state.value.attempt ?: return@launch
            if (isExpired(attempt)) {
                expire(attempt)
                return@launch
            }
            if (state.value.phase == CloudSignInPhase.Recovering) return@launch
            if (state.value.phase != CloudSignInPhase.Unconfirmed) {
                diagnostics.accessEvent(
                    level = DiagnosticLevel.Info,
                    event = "login_recovery_not_pending",
                    stage = AccessStage.Login,
                    instanceId = instance,
                )
                return@launch
            }
            setState(state.value.copy(phase = CloudSignInPhase.Recovering, error = null))
            recoverInternal(attempt)
        }
    }

    /** Explicitly abandons the pending attempt so a fresh one may start. */
    fun cancel() {
        scope.launch(dispatcher) {
            val attempt = state.value.attempt
            if (attempt == null) {
                setState(
                    state.value.copy(
                        phase = CloudSignInPhase.Cancelled,
                        error = CoreFailure("attempt_cancelled", "The sign-in was cancelled."),
                        canRestart = true,
                    ),
                )
                return@launch
            }
            guardedPortCall(diagnostics, AccessStage.Login, "login_cancel", instance) {
                auth.cancel(instance, attempt.attemptId)
            }
            cancelInternal("attempt_cancelled", "The sign-in was cancelled.")
        }
    }

    private suspend fun bootstrap() {
        val current = guardedPortCall(diagnostics, AccessStage.Login, "login_current_session", instance) {
            auth.currentSession(instance)
        }
        if (current is Outcome.Success) {
            val session = current.value
            if (session != null) {
                setState(state.value.copy(previousSession = session, session = session))
            }
        }
        val pending = guardedPortCall(diagnostics, AccessStage.Login, "login_pending_attempt", instance) {
            auth.pendingAttempt(instance)
        }
        val attempt = pending.valueOrNull() ?: return
        knownStates[attempt.state] = attempt.attemptId
        if (isExpired(attempt)) {
            setState(state.value.copy(attempt = attempt))
            expire(attempt)
            return
        }
        if (state.value.phase == CloudSignInPhase.Idle && state.value.attempt == null) {
            setState(
                state.value.copy(
                    phase = CloudSignInPhase.AwaitingBrowser,
                    attempt = attempt,
                    error = null,
                ),
            )
            diagnostics.accessEvent(
                level = DiagnosticLevel.Info,
                event = "login_pending_attempt_restored",
                stage = AccessStage.Login,
                instanceId = instance,
            )
        }
    }

    private suspend fun applyResult(
        expected: CloudAttempt,
        result: CloudAuthResult,
        recoveryAllowed: Boolean,
    ) {
        when (result) {
            is CloudAuthResult.Success -> install(expected, result.session)
            is CloudAuthResult.Rejected -> {
                diagnostics.accessEvent(
                    level = DiagnosticLevel.Warn,
                    event = "login_exchange_rejected",
                    stage = AccessStage.Login,
                    code = result.code,
                    instanceId = instance,
                )
                if (result.code == "expired") {
                    expire(expected)
                } else {
                    failAttempt(result.code, "The sign-in was rejected.")
                }
            }

            is CloudAuthResult.Unconfirmed -> {
                diagnostics.accessEvent(
                    level = DiagnosticLevel.Warn,
                    event = "login_exchange_unconfirmed",
                    stage = AccessStage.Login,
                    code = result.failure.code,
                    instanceId = instance,
                )
                if (recoveryAllowed) {
                    setState(
                        state.value.copy(
                            phase = CloudSignInPhase.Recovering,
                            error = result.failure,
                        ),
                    )
                    recoverInternal(expected)
                } else {
                    setState(
                        state.value.copy(
                            phase = CloudSignInPhase.Unconfirmed,
                            attempt = expected,
                            error = result.failure,
                            canRestart = false,
                        ),
                    )
                }
            }

            CloudAuthResult.Cancelled -> cancelInternal(
                "attempt_cancelled",
                "The sign-in was cancelled.",
            )

            CloudAuthResult.Expired -> expire(expected)
        }
    }

    private suspend fun recoverInternal(expected: CloudAttempt) {
        val result = try {
            auth.recoverLostExchange(instance, expected.attemptId)
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            diagnostics.accessEvent(
                level = DiagnosticLevel.Error,
                event = "login_recovery_exception",
                stage = AccessStage.Login,
                code = "unexpected_failure",
                instanceId = instance,
            )
            CloudAuthResult.Unconfirmed(
                CoreFailure("recovery_unavailable", "The sign-in result is still unconfirmed.", true),
            )
        }
        if (state.value.attempt?.attemptId != expected.attemptId) {
            return
        }
        applyResult(expected, result, recoveryAllowed = false)
    }

    private fun install(expected: CloudAttempt, session: CloudSession) {
        if (state.value.attempt?.attemptId != expected.attemptId) {
            diagnostics.accessEvent(
                level = DiagnosticLevel.Warn,
                event = "login_stale_result_ignored",
                stage = AccessStage.Login,
                code = "attempt_obsolete",
                instanceId = instance,
            )
            return
        }
        if (session.instance != instance ||
            session.environment != expected.environment ||
            session.accountId.isBlank() ||
            session.workspaceId.isBlank() ||
            (expected.expectedAccountId != null &&
                session.accountId != expected.expectedAccountId) ||
            (expected.expectedWorkspaceId != null &&
                session.workspaceId != expected.expectedWorkspaceId)
        ) {
            diagnostics.accessEvent(
                level = DiagnosticLevel.Warn,
                event = "login_scope_mismatch",
                stage = AccessStage.Login,
                code = "callback_scope",
                instanceId = instance,
            )
            failAttempt("callback_scope", "The sign-in response did not match this app.")
            return
        }
        completedAttempts += expected.attemptId
        setState(
            state.value.copy(
                phase = CloudSignInPhase.Succeeded,
                attempt = null,
                previousSession = session,
                session = session,
                destination = Route.Directory,
                error = null,
                canRestart = false,
            ),
        )
        diagnostics.accessEvent(
            level = DiagnosticLevel.Info,
            event = "login_session_installed",
            stage = AccessStage.Login,
            instanceId = instance,
        )
    }

    private fun rejectCallback(code: String, message: String) {
        setState(state.value.copy(error = CoreFailure(code, message)))
        diagnostics.accessEvent(
            level = DiagnosticLevel.Warn,
            event = "login_callback_rejected",
            stage = AccessStage.Login,
            code = code,
            instanceId = instance,
        )
    }

    private fun failAttempt(code: String, message: String) {
        setState(
            state.value.copy(
                phase = CloudSignInPhase.Failed,
                attempt = null,
                error = CoreFailure(code, message),
                canRestart = true,
            ),
        )
        diagnostics.accessEvent(
            level = DiagnosticLevel.Warn,
            event = "login_attempt_failed",
            stage = AccessStage.Login,
            code = code,
            instanceId = instance,
        )
    }

    private fun cancelInternal(code: String, message: String) {
        setState(
            state.value.copy(
                phase = CloudSignInPhase.Cancelled,
                attempt = null,
                error = CoreFailure(code, message),
                canRestart = true,
            ),
        )
        diagnostics.accessEvent(
            level = DiagnosticLevel.Info,
            event = "login_attempt_cancelled",
            stage = AccessStage.Login,
            code = code,
            instanceId = instance,
        )
    }

    private fun expire(attempt: CloudAttempt) {
        if (state.value.attempt?.attemptId != attempt.attemptId) {
            return
        }
        setState(
            state.value.copy(
                phase = CloudSignInPhase.Expired,
                attempt = null,
                error = CoreFailure("attempt_expired", "The sign-in attempt expired.", true),
                canRestart = true,
            ),
        )
        diagnostics.accessEvent(
            level = DiagnosticLevel.Warn,
            event = "login_attempt_expired",
            stage = AccessStage.Login,
            code = "attempt_expired",
            instanceId = instance,
        )
    }

    private fun isExpired(attempt: CloudAttempt): Boolean =
        attempt.expiresAtEpochMillis <= clock.epochMillis()

    private fun isExpectedRedirect(candidate: String): Boolean {
        val uri = try {
            URI(candidate)
        } catch (_: Exception) {
            return false
        }
        return sameOriginAndPath(uri, expectedRedirect)
    }

    private fun isSecureAuthorizationUrl(url: String): Boolean {
        val uri = try {
            URI(url)
        } catch (_: Exception) {
            return false
        }
        return uri.scheme.equals("https", ignoreCase = true) && !uri.host.isNullOrBlank()
    }

    private fun sameOriginAndPath(candidate: URI, expected: String): Boolean {
        val expectedUri = try {
            URI(expected)
        } catch (_: Exception) {
            return false
        }
        return candidate.scheme.equals(expectedUri.scheme, ignoreCase = true) &&
            candidate.host.equals(expectedUri.host, ignoreCase = true) &&
            candidate.port == expectedUri.port &&
            candidate.path == expectedUri.path &&
            candidate.userInfo == null &&
            expectedUri.userInfo == null &&
            candidate.rawFragment == null
    }

    private fun parseCallback(raw: String): ParsedCallback? {
        val uri = try {
            URI(raw)
        } catch (_: Exception) {
            return null
        }
        if (!uri.isAbsolute || uri.rawQuery == null) return null
        val parameters = linkedMapOf<String, MutableList<String>>()
        val query = uri.rawQuery ?: return null
        if (query.isEmpty()) return null
        for (pair in query.split('&')) {
            if (pair.isEmpty()) continue
            val separator = pair.indexOf('=')
            val rawName = if (separator < 0) pair else pair.substring(0, separator)
            val rawValue = if (separator < 0) "" else pair.substring(separator + 1)
            val name = decodeComponent(rawName) ?: return null
            val value = decodeComponent(rawValue) ?: return null
            parameters.getOrPut(name) { mutableListOf() }.add(value)
        }
        return ParsedCallback(uri, parameters)
    }

    private fun decodeComponent(value: String): String? = try {
        URLDecoder.decode(value, StandardCharsets.UTF_8)
    } catch (_: IllegalArgumentException) {
        null
    }

    private data class ParsedCallback(
        val uri: URI,
        val parameters: Map<String, List<String>>,
    )

    private companion object {
        const val DEFAULT_REDIRECT = "https://app.whalerein.test/client/android/callback"
    }
}
