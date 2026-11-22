package com.whalerein.android.state.access

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.ScreenModel
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch

enum class ProfileScopedData { Draft, Trust, Credential, RecoveryRecord }

sealed interface ProfileActivationState {
    data object Idle : ProfileActivationState

    data class Activating(val instance: InstanceId) : ProfileActivationState

    data class Active(val profile: Profile) : ProfileActivationState

    data class Failed(val failure: CoreFailure) : ProfileActivationState
}

data class ProfilesUiState(
    val loading: Boolean = true,
    val profiles: List<Profile> = emptyList(),
    val activeProfile: Profile? = null,
    /** Bumped only by a successful activation; stale callbacks carry an older value. */
    val generation: Long = 0,
    val activation: ProfileActivationState = ProfileActivationState.Idle,
    val credentials: Map<InstanceId, CredentialState> = emptyMap(),
    val storageScopes: Map<InstanceId, ProfileStorageScope> = emptyMap(),
    val appliedScopedData: Map<InstanceId, Set<ProfileScopedData>> = emptyMap(),
    val newInstall: Boolean = false,
    val error: CoreFailure? = null,
)

/**
 * Owns profile listing, validation, activation and scoped callback fencing.
 *
 * A failed activation keeps the previous instance. A successful activation
 * increments [ProfilesUiState.generation]; observations, callbacks and scoped
 * data carrying an older generation or another instance are ignored and never
 * applied to the new profile.
 */
class ProfilesViewModel(
    private val profiles: ProfilePort,
    private val diagnostics: DiagnosticSink,
    private val dispatcher: CoroutineDispatcher = Dispatchers.Default,
) : ScreenModel<ProfilesUiState>(ProfilesUiState()) {

    init {
        scope.launch(dispatcher) {
            loadInternal()
        }
    }

    fun refresh() {
        scope.launch(dispatcher) {
            loadInternal()
        }
    }

    fun activate(instance: InstanceId) {
        scope.launch(dispatcher) {
            if (state.value.activation is ProfileActivationState.Activating) return@launch
            val previous = state.value.activeProfile
            setState(
                state.value.copy(
                    activation = ProfileActivationState.Activating(instance),
                    error = null,
                ),
            )
            val result = guardedPortCall(diagnostics, AccessStage.Profile, "profile_activate", instance) {
                profiles.activate(instance)
            }
            when (result) {
                is Outcome.Success -> {
                    val active = result.value.active
                    if (active.instance != instance) {
                        setState(
                            state.value.copy(
                                activeProfile = previous,
                                activation = ProfileActivationState.Failed(
                                    CoreFailure(
                                        "profile_activation_scope",
                                        "The activated profile did not match the request.",
                                    ),
                                ),
                                error = CoreFailure(
                                    "profile_activation_scope",
                                    "The activated profile did not match the request.",
                                ),
                            ),
                        )
                        diagnostics.accessEvent(
                            level = DiagnosticLevel.Warn,
                            event = "profile_activation_scope_mismatch",
                            stage = AccessStage.Profile,
                            code = "profile_activation_scope",
                            instanceId = instance,
                        )
                        return@launch
                    }
                    val nextGeneration = state.value.generation + 1
                    val updatedProfiles = state.value.profiles.map {
                        it.copy(active = it.instance == instance)
                    }.let { existing ->
                        if (existing.any { it.instance == instance }) {
                            existing
                        } else {
                            existing + active.copy(active = true)
                        }
                    }
                    setState(
                        state.value.copy(
                            profiles = updatedProfiles,
                            activeProfile = active,
                            generation = nextGeneration,
                            activation = ProfileActivationState.Active(active),
                            appliedScopedData = state.value.appliedScopedData
                                .filterKeys { it == instance },
                            newInstall = false,
                            error = null,
                        ),
                    )
                    diagnostics.accessEvent(
                        level = DiagnosticLevel.Info,
                        event = "profile_activated",
                        stage = AccessStage.Profile,
                        instanceId = instance,
                    )
                }

                else -> {
                    val failure = result.asFailure()
                    setState(
                        state.value.copy(
                            activeProfile = previous,
                            activation = ProfileActivationState.Failed(failure),
                            error = failure,
                        ),
                    )
                    diagnostics.accessEvent(
                        level = DiagnosticLevel.Warn,
                        event = "profile_activation_failed",
                        stage = AccessStage.Profile,
                        code = failure.code,
                        instanceId = instance,
                    )
                }
            }
        }
    }

    fun validateAndSave(profile: Profile) {
        scope.launch(dispatcher) {
            val validation = guardedPortCall(
                diagnostics,
                AccessStage.Profile,
                "profile_validate",
                profile.instance,
            ) {
                profiles.validate(profile)
            }
            val validated = validation.valueOrNull()
            if (validated !is ProfileValidation.Valid) {
                val code = (validated as? ProfileValidation.Invalid)?.code ?: "profile_invalid"
                setState(
                    state.value.copy(
                        error = CoreFailure(code, "The profile is not valid."),
                    ),
                )
                return@launch
            }
            val saved = guardedPortCall(
                diagnostics,
                AccessStage.Profile,
                "profile_save",
                profile.instance,
            ) {
                profiles.save(validated.profile)
            }
            when (saved) {
                is Outcome.Success -> {
                    val stored = saved.value
                    val updated = state.value.profiles
                        .filterNot { it.instance == stored.instance } + stored
                    setState(state.value.copy(profiles = updated, error = null))
                }

                else -> setState(state.value.copy(error = saved.asFailure()))
            }
        }
    }

    fun remove(instance: InstanceId) {
        scope.launch(dispatcher) {
            val result = guardedPortCall(diagnostics, AccessStage.Profile, "profile_remove", instance) {
                profiles.remove(instance)
            }
            when (result) {
                is Outcome.Success -> {
                    val wasActive = state.value.activeProfile?.instance == instance
                    val newActive = if (wasActive) null else state.value.activeProfile
                    val updated = state.value.profiles.filterNot { it.instance == instance }
                    setState(
                        state.value.copy(
                            profiles = updated,
                            activeProfile = newActive,
                            generation = if (wasActive) state.value.generation + 1 else state.value.generation,
                            appliedScopedData = state.value.appliedScopedData - instance,
                            newInstall = updated.isEmpty() && newActive == null,
                            error = null,
                        ),
                    )
                    diagnostics.accessEvent(
                        level = DiagnosticLevel.Info,
                        event = "profile_removed",
                        stage = AccessStage.Profile,
                        instanceId = instance,
                    )
                }

                else -> setState(state.value.copy(error = result.asFailure()))
            }
        }
    }

    fun refreshCredentials(instance: InstanceId) {
        scope.launch(dispatcher) {
            val credential = try {
                profiles.credentialState(instance)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                diagnostics.accessEvent(
                    level = DiagnosticLevel.Warn,
                    event = "profile_credential_unavailable",
                    stage = AccessStage.Profile,
                    code = "unexpected_failure",
                    instanceId = instance,
                )
                CredentialState.Unavailable
            }
            setState(
                state.value.copy(
                    credentials = state.value.credentials + (instance to credential),
                ),
            )
            when (credential) {
                CredentialState.Unavailable -> diagnostics.accessEvent(
                    level = DiagnosticLevel.Warn,
                    event = "profile_credential_unavailable",
                    stage = AccessStage.Profile,
                    code = "unavailable",
                    instanceId = instance,
                )

                is CredentialState.Invalid -> diagnostics.accessEvent(
                    level = DiagnosticLevel.Error,
                    event = "profile_credential_invalid",
                    stage = AccessStage.Profile,
                    code = credential.code,
                    instanceId = instance,
                )

                CredentialState.Absent,
                CredentialState.Present -> Unit
            }
        }
    }

    fun refreshStorageScope(instance: InstanceId) {
        scope.launch(dispatcher) {
            val result = guardedPortCall(
                diagnostics,
                AccessStage.Profile,
                "profile_storage_scope",
                instance,
            ) {
                profiles.storageScope(instance)
            }
            when (result) {
                is Outcome.Success -> setState(
                    state.value.copy(
                        storageScopes = state.value.storageScopes + (instance to result.value),
                    ),
                )

                else -> setState(state.value.copy(error = result.asFailure()))
            }
        }
    }

    /**
     * Applies one scoped observation only while its instance and generation are
     * current. Returns whether the data was applied.
     */
    fun applyScopedData(
        instance: InstanceId,
        generation: Long,
        data: ProfileScopedData,
    ): Boolean {
        val accepted = isCurrent(instance, generation)
        if (!accepted) {
            diagnostics.accessEvent(
                level = DiagnosticLevel.Warn,
                event = "profile_stale_scope_rejected",
                stage = AccessStage.Profile,
                code = "stale_scope",
                instanceId = instance,
            )
            return false
        }
        val existing = state.value.appliedScopedData[instance].orEmpty()
        setState(
            state.value.copy(
                appliedScopedData = state.value.appliedScopedData + (instance to (existing + data)),
            ),
        )
        return true
    }

    /** Accepts an authorization callback only for the current instance/generation. */
    fun acceptCallback(instance: InstanceId, generation: Long): Boolean {
        val accepted = isCurrent(instance, generation)
        if (!accepted) {
            diagnostics.accessEvent(
                level = DiagnosticLevel.Warn,
                event = "profile_stale_callback_rejected",
                stage = AccessStage.Profile,
                code = "stale_callback",
                instanceId = instance,
            )
        }
        return accepted
    }

    fun isCurrent(instance: InstanceId, generation: Long): Boolean {
        val active = state.value.activeProfile ?: return false
        return active.instance == instance && state.value.generation == generation
    }

    private suspend fun loadInternal() {
        setState(state.value.copy(loading = true, error = null))
        val listed = guardedPortCall(diagnostics, AccessStage.Profile, "profile_list") {
            profiles.list()
        }
        val activeResult = guardedPortCall(diagnostics, AccessStage.Profile, "profile_active") {
            profiles.active()
        }
        if (listed !is Outcome.Success) {
            setState(
                state.value.copy(
                    loading = false,
                    newInstall = false,
                    error = listed.asFailure(),
                ),
            )
            return
        }
        if (activeResult !is Outcome.Success) {
            setState(
                state.value.copy(
                    loading = false,
                    newInstall = false,
                    error = activeResult.asFailure(),
                ),
            )
            return
        }
        val allProfiles = listed.value
        val active = activeResult.value
        val scopes = buildMap {
            for (profile in allProfiles) {
                val scope = guardedPortCall(
                    diagnostics,
                    AccessStage.Profile,
                    "profile_storage_scope",
                    profile.instance,
                ) {
                    profiles.storageScope(profile.instance)
                }.valueOrNull()
                if (scope != null) put(profile.instance, scope)
            }
        }
        setState(
            state.value.copy(
                loading = false,
                profiles = allProfiles,
                activeProfile = active,
                storageScopes = scopes,
                newInstall = allProfiles.isEmpty() && active == null,
                error = null,
            ),
        )
    }
}
