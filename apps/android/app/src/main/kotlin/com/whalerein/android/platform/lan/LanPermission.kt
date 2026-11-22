package com.whalerein.android.platform.lan

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import androidx.activity.ComponentActivity
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.ContextCompat
import com.whalerein.android.state.access.LanPermissionState
import com.whalerein.android.state.access.PairingPort
import com.whalerein.android.state.access.StandaloneTarget
import com.whalerein.android.state.access.TransportValidation
import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import java.net.URI
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** Narrow seam so the graph stays independent of the hosting Activity. */
interface LanPermissionGateway {
    suspend fun state(): LanPermissionState

    suspend fun request(): LanPermissionState
}

/**
 * `ACCESS_LOCAL_NETWORK` for a Standalone target that resolves to a LAN address.
 *
 * The permission is not a shipping requirement on releases before it exists:
 * those return [LanPermissionState.NotRequired] and never show a prompt. Public
 * Cloud never constructs this path.
 */
class ActivityLanPermission(
    private val activity: ComponentActivity,
    private val diagnostics: DiagnosticSink,
) : LanPermissionGateway {
    private var pending: CompletableDeferred<LanPermissionState>? = null
    private var asked = false

    private val launcher: ActivityResultLauncher<String> =
        activity.registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
            val deferred = synchronized(this) {
                val current = pending
                pending = null
                asked = true
                current
            }
            if (granted) {
                emit("lan_permission_granted")
                deferred?.complete(LanPermissionState.Granted)
            } else {
                val state = if (activity.shouldShowRequestPermissionRationale(PERMISSION)) {
                    LanPermissionState.Denied
                } else {
                    LanPermissionState.Denied
                }
                emit("lan_permission_denied")
                deferred?.complete(state)
            }
        }

    override suspend fun state(): LanPermissionState {
        if (!permissionExists()) return LanPermissionState.NotRequired
        val granted = ContextCompat.checkSelfPermission(activity, PERMISSION) ==
            PackageManager.PERMISSION_GRANTED
        if (granted) return LanPermissionState.Granted
        return if (asked) LanPermissionState.Revoked else LanPermissionState.Unknown
    }

    override suspend fun request(): LanPermissionState {
        if (!permissionExists()) return LanPermissionState.NotRequired
        if (ContextCompat.checkSelfPermission(activity, PERMISSION) ==
            PackageManager.PERMISSION_GRANTED
        ) {
            return LanPermissionState.Granted
        }
        val deferred = CompletableDeferred<LanPermissionState>()
        synchronized(this) {
            if (pending != null) return LanPermissionState.Unknown
            pending = deferred
        }
        withContext(Dispatchers.Main.immediate) {
            launcher.launch(PERMISSION)
        }
        return deferred.await()
    }

    private fun permissionExists(): Boolean = Build.VERSION.SDK_INT >= LOCAL_NETWORK_API

    private fun emit(event: String) {
        diagnostics.emit(
            DiagnosticEvent(
                level = DiagnosticLevel.Info,
                event = event,
                stage = "transport",
            ),
        )
    }

    private companion object {
        const val PERMISSION = "android.permission.ACCESS_LOCAL_NETWORK"
        const val LOCAL_NETWORK_API = 37
    }
}

/**
 * Pairing adapter seam that only surfaces LAN permission state for a target that
 * actually resolves to a local/private address.
 */
class LanAwarePairingPort(
    private val delegate: PairingPort,
    private val lan: LanPermissionGateway,
) : PairingPort by delegate {
    private var lanExpected: Boolean? = null

    override suspend fun validateTarget(
        instance: InstanceId,
        target: StandaloneTarget,
    ): Outcome<TransportValidation> {
        lanExpected = isPrivateTarget(target.url)
        return delegate.validateTarget(instance, target)
    }

    override suspend fun lanPermission(): LanPermissionState {
        if (lanExpected != true) return LanPermissionState.NotRequired
        return lan.state()
    }

    override suspend fun requestLanPermission(): LanPermissionState {
        if (lanExpected != true) return LanPermissionState.NotRequired
        return lan.request()
    }

    internal fun isPrivateTarget(url: String): Boolean = try {
        val host = URI(url).host?.lowercase() ?: return false
        host == "localhost" ||
            host == "::1" ||
            host.endsWith(".local") ||
            host.startsWith("127.") ||
            host.startsWith("10.") ||
            isPrivate172(host) ||
            host.startsWith("192.168.") ||
            host.startsWith("169.254.") ||
            host.startsWith("fe80:") ||
            host.startsWith("fc") ||
            host.startsWith("fd")
    } catch (_: Exception) {
        false
    }

    private fun isPrivate172(host: String): Boolean {
        if (!host.startsWith("172.")) return false
        val second = host.removePrefix("172.").substringBefore('.').toIntOrNull() ?: return false
        return second in 16..31
    }
}
