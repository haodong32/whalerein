package com.whalerein.android.state.access

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.Route
import com.whalerein.android.state.core.ScreenModel
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch

enum class StandalonePhase {
    Idle,
    Reviewing,
    ImportingCa,
    ReadyToPair,
    Pairing,
    Paired,
    Cancelled,
}

data class StandaloneSetupUiState(
    val phase: StandalonePhase = StandalonePhase.Idle,
    val invitation: StandaloneInvitation? = null,
    val fingerprintMatches: Boolean? = null,
    val ca: CaImportState = CaImportState.NotRequired,
    val caScope: InstanceId? = null,
    val lan: LanPermissionState = LanPermissionState.NotRequired,
    val production: Boolean = true,
    val error: CoreFailure? = null,
    val pairedHost: HostRef? = null,
    val destination: Route? = null,
)

/**
 * Standalone invitation review, scoped CA import and LAN permission handling.
 *
 * Pairing is refused unless the reviewed fingerprint matches, a required CA is
 * already scoped to this instance, and a required LAN permission is granted.
 * A production target must be HTTPS; the Cloud-public path never asks for the
 * LAN permission because [lanRequired] is false for it.
 */
class StandaloneSetupViewModel(
    private val instance: InstanceId,
    private val pairing: PairingPort,
    private val diagnostics: DiagnosticSink,
    private val production: Boolean = true,
    private val lanRequired: Boolean = true,
    private val dispatcher: CoroutineDispatcher = Dispatchers.Default,
) : ScreenModel<StandaloneSetupUiState>(
    StandaloneSetupUiState(
        lan = if (lanRequired) LanPermissionState.Unknown else LanPermissionState.NotRequired,
        production = production,
    ),
) {

    fun loadInvitation(encoded: String) {
        scope.launch(dispatcher) {
            setState(state.value.copy(phase = StandalonePhase.Reviewing, error = null))
            val result = guardedPortCall(diagnostics, AccessStage.Transport, "standalone_review", instance) {
                pairing.reviewInvitation(encoded)
            }
            when (result) {
                is Outcome.Success -> {
                    val invitation = result.value
                    val permission = if (lanRequired) {
                        safeLanPermission()
                    } else {
                        LanPermissionState.NotRequired
                    }
                    setState(
                        state.value.copy(
                            phase = StandalonePhase.Reviewing,
                            invitation = invitation,
                            ca = if (invitation.caRequired) {
                                CaImportState.AwaitingSelection
                            } else {
                                CaImportState.NotRequired
                            },
                            caScope = null,
                            lan = permission,
                            fingerprintMatches = null,
                            error = null,
                        ),
                    )
                    diagnostics.accessEvent(
                        level = DiagnosticLevel.Info,
                        event = "transport_standalone_invitation_reviewed",
                        stage = AccessStage.Transport,
                        instanceId = instance,
                    )
                }

                else -> {
                    setState(
                        state.value.copy(
                            phase = StandalonePhase.Idle,
                            error = result.asFailure(),
                        ),
                    )
                    diagnostics.accessEvent(
                        level = DiagnosticLevel.Warn,
                        event = "transport_standalone_review_failed",
                        stage = AccessStage.Transport,
                        code = result.asFailure().code,
                        instanceId = instance,
                    )
                }
            }
        }
    }

    fun checkFingerprint(presented: String) {
        scope.launch(dispatcher) {
            val result = guardedPortCall(
                diagnostics,
                AccessStage.Transport,
                "standalone_fingerprint",
                instance,
            ) {
                pairing.checkFingerprint(instance, presented)
            }
            when (result) {
                is Outcome.Success -> {
                    val matches = result.value == FingerprintCheck.Match
                    setState(
                        state.value.copy(
                            fingerprintMatches = matches,
                            error = if (matches) {
                                null
                            } else {
                                CoreFailure(
                                    "fingerprint_mismatch",
                                    "The Host fingerprint does not match the invitation.",
                                )
                            },
                        ),
                    )
                    if (!matches) {
                        diagnostics.accessEvent(
                            level = DiagnosticLevel.Warn,
                            event = "transport_fingerprint_mismatch",
                            stage = AccessStage.Transport,
                            code = "fingerprint_mismatch",
                            instanceId = instance,
                        )
                    }
                }

                else -> setState(
                    state.value.copy(
                        fingerprintMatches = null,
                        error = result.asFailure(),
                    ),
                )
            }
        }
    }

    fun importCa(pem: String) {
        scope.launch(dispatcher) {
            if (state.value.invitation?.caRequired != true) {
                setState(state.value.copy(ca = CaImportState.NotRequired, error = null))
                return@launch
            }
            setState(state.value.copy(phase = StandalonePhase.ImportingCa, error = null))
            val result = guardedPortCall(diagnostics, AccessStage.Transport, "standalone_ca_import", instance) {
                pairing.importCa(instance, pem)
            }
            when (result) {
                is Outcome.Success -> {
                    val imported = result.value
                    if (imported is CaImportState.Ready && imported.instance != instance) {
                        setState(
                            state.value.copy(
                                phase = StandalonePhase.Reviewing,
                                ca = CaImportState.Rejected("ca_scope_mismatch"),
                                caScope = null,
                                error = CoreFailure(
                                    "ca_scope_mismatch",
                                    "The CA was not scoped to this profile.",
                                ),
                            ),
                        )
                        diagnostics.accessEvent(
                            level = DiagnosticLevel.Error,
                            event = "transport_ca_scope_mismatch",
                            stage = AccessStage.Transport,
                            code = "ca_scope_mismatch",
                            instanceId = instance,
                        )
                        return@launch
                    }
                    setState(
                        state.value.copy(
                            phase = StandalonePhase.Reviewing,
                            ca = imported,
                            caScope = (imported as? CaImportState.Ready)?.instance,
                            error = if (imported is CaImportState.Rejected) {
                                CoreFailure(imported.code, "The CA could not be imported.")
                            } else {
                                null
                            },
                        ),
                    )
                    if (imported is CaImportState.Rejected) {
                        diagnostics.accessEvent(
                            level = DiagnosticLevel.Warn,
                            event = "transport_ca_rejected",
                            stage = AccessStage.Transport,
                            code = imported.code,
                            instanceId = instance,
                        )
                    }
                }

                else -> setState(
                    state.value.copy(
                        phase = StandalonePhase.Reviewing,
                        ca = CaImportState.Rejected(result.asFailure().code),
                        error = result.asFailure(),
                    ),
                )
            }
        }
    }

    fun requestLanPermission() {
        scope.launch(dispatcher) {
            if (!lanRequired) {
                setState(state.value.copy(lan = LanPermissionState.NotRequired))
                return@launch
            }
            val permission = safeLanRequest()
            setState(
                state.value.copy(
                    lan = permission,
                    error = if (permission == LanPermissionState.Granted ||
                        permission == LanPermissionState.NotRequired
                    ) {
                        null
                    } else {
                        CoreFailure(
                            "lan_permission_${permission.name.lowercase()}",
                            "Local network permission is required for this Host.",
                        )
                    },
                ),
            )
            if (permission != LanPermissionState.Granted) {
                diagnostics.accessEvent(
                    level = DiagnosticLevel.Warn,
                    event = "transport_lan_permission",
                    stage = AccessStage.Transport,
                    code = permission.name.lowercase(),
                    instanceId = instance,
                )
            }
        }
    }

    fun refreshLanPermission() {
        scope.launch(dispatcher) {
            if (!lanRequired) {
                setState(state.value.copy(lan = LanPermissionState.NotRequired))
                return@launch
            }
            val permission = safeLanPermission()
            setState(
                state.value.copy(
                    lan = permission,
                    error = if (permission == LanPermissionState.Revoked) {
                        CoreFailure(
                            "lan_permission_revoked",
                            "Local network permission was revoked.",
                        )
                    } else {
                        state.value.error
                    },
                ),
            )
            if (permission == LanPermissionState.Revoked) {
                diagnostics.accessEvent(
                    level = DiagnosticLevel.Warn,
                    event = "transport_lan_permission_revoked",
                    stage = AccessStage.Transport,
                    code = "revoked",
                    instanceId = instance,
                )
            }
        }
    }

    fun validateTarget(url: String) {
        scope.launch(dispatcher) {
            if (production && !url.startsWith("https://", ignoreCase = true)) {
                setState(
                    state.value.copy(
                        error = CoreFailure(
                            "cleartext_rejected",
                            "Standalone requires HTTPS in this build.",
                        ),
                    ),
                )
                diagnostics.accessEvent(
                    level = DiagnosticLevel.Warn,
                    event = "transport_cleartext_rejected",
                    stage = AccessStage.Transport,
                    code = "cleartext_rejected",
                    instanceId = instance,
                )
                return@launch
            }
            val result = guardedPortCall(
                diagnostics,
                AccessStage.Transport,
                "standalone_target",
                instance,
            ) {
                pairing.validateTarget(instance, StandaloneTarget(url))
            }
            when (result) {
                is Outcome.Success -> {
                    when (val validation = result.value) {
                        TransportValidation.Valid -> setState(state.value.copy(error = null))
                        is TransportValidation.Rejected -> {
                            setState(
                                state.value.copy(
                                    error = CoreFailure(
                                        validation.code,
                                        "The Standalone connection could not be validated.",
                                    ),
                                ),
                            )
                            diagnostics.accessEvent(
                                level = DiagnosticLevel.Warn,
                                event = "transport_target_rejected",
                                stage = AccessStage.Transport,
                                code = validation.code,
                                instanceId = instance,
                            )
                        }
                    }
                }

                else -> setState(state.value.copy(error = result.asFailure()))
            }
        }
    }

    fun accept() {
        scope.launch(dispatcher) {
            if (state.value.phase == StandalonePhase.Paired ||
                state.value.phase == StandalonePhase.Pairing
            ) {
                return@launch
            }
            if (state.value.phase == StandalonePhase.Cancelled) {
                rejectPairing("invitation_cancelled", "Review a new invitation before pairing.")
                return@launch
            }
            val invitation = state.value.invitation
            if (invitation == null) {
                rejectPairing("invitation_missing", "Review an invitation before pairing.")
                return@launch
            }
            if (state.value.fingerprintMatches != true) {
                rejectPairing("fingerprint_unverified", "Verify the Host fingerprint before pairing.")
                return@launch
            }
            if (invitation.caRequired && state.value.ca !is CaImportState.Ready) {
                rejectPairing("ca_required", "Import the Host CA before pairing.")
                return@launch
            }
            if (lanRequired && state.value.lan != LanPermissionState.Granted) {
                rejectPairing(
                    "lan_permission_${state.value.lan.name.lowercase()}",
                    "Local network permission is required before pairing.",
                )
                return@launch
            }
            setState(state.value.copy(phase = StandalonePhase.Pairing, error = null))
            val result = guardedPortCall(diagnostics, AccessStage.Transport, "standalone_pair", instance) {
                pairing.decide(instance, true)
            }
            when (result) {
                is Outcome.Success -> {
                    when (val pairingResult = result.value) {
                        is PairingResult.Success -> {
                            setState(
                                state.value.copy(
                                    phase = StandalonePhase.Paired,
                                    pairedHost = pairingResult.host,
                                    destination = Route.HostSessions(pairingResult.host),
                                    error = null,
                                ),
                            )
                            diagnostics.accessEvent(
                                level = DiagnosticLevel.Info,
                                event = "transport_standalone_paired",
                                stage = AccessStage.Transport,
                                instanceId = instance,
                            )
                        }

                        is PairingResult.Rejected -> {
                            setState(
                                state.value.copy(
                                    phase = StandalonePhase.Reviewing,
                                    error = CoreFailure(
                                        pairingResult.code,
                                        "The Host rejected the pairing.",
                                    ),
                                ),
                            )
                            diagnostics.accessEvent(
                                level = DiagnosticLevel.Warn,
                                event = "transport_standalone_rejected",
                                stage = AccessStage.Transport,
                                code = pairingResult.code,
                                instanceId = instance,
                            )
                        }
                    }
                }

                else -> {
                    setState(
                        state.value.copy(
                            phase = StandalonePhase.Reviewing,
                            error = result.asFailure(),
                        ),
                    )
                    diagnostics.accessEvent(
                        level = DiagnosticLevel.Warn,
                        event = "transport_standalone_pair_failed",
                        stage = AccessStage.Transport,
                        code = result.asFailure().code,
                        instanceId = instance,
                    )
                }
            }
        }
    }

    fun reject() {
        scope.launch(dispatcher) {
            guardedPortCall(diagnostics, AccessStage.Transport, "standalone_reject", instance) {
                pairing.decide(instance, false)
            }
            setState(
                state.value.copy(
                    phase = StandalonePhase.Cancelled,
                    pairedHost = null,
                    destination = null,
                    error = null,
                ),
            )
            diagnostics.accessEvent(
                level = DiagnosticLevel.Info,
                event = "transport_standalone_cancelled",
                stage = AccessStage.Transport,
                instanceId = instance,
            )
        }
    }

    private suspend fun safeLanPermission(): LanPermissionState = try {
        pairing.lanPermission()
    } catch (cancelled: kotlinx.coroutines.CancellationException) {
        throw cancelled
    } catch (_: Exception) {
        diagnostics.accessEvent(
            level = DiagnosticLevel.Warn,
            event = "transport_lan_query_failed",
            stage = AccessStage.Transport,
            code = "unexpected_failure",
            instanceId = instance,
        )
        LanPermissionState.Unknown
    }

    private suspend fun safeLanRequest(): LanPermissionState = try {
        pairing.requestLanPermission()
    } catch (cancelled: kotlinx.coroutines.CancellationException) {
        throw cancelled
    } catch (_: Exception) {
        diagnostics.accessEvent(
            level = DiagnosticLevel.Warn,
            event = "transport_lan_request_failed",
            stage = AccessStage.Transport,
            code = "unexpected_failure",
            instanceId = instance,
        )
        LanPermissionState.Denied
    }

    private fun rejectPairing(code: String, message: String) {
        setState(state.value.copy(error = CoreFailure(code, message)))
        diagnostics.accessEvent(
            level = DiagnosticLevel.Warn,
            event = "transport_standalone_blocked",
            stage = AccessStage.Transport,
            code = code,
            instanceId = instance,
        )
    }
}
