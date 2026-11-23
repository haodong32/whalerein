package com.whalerein.android

import android.Manifest
import android.os.Build
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.whalerein.android.platform.lan.LanAwarePairingPort
import com.whalerein.android.platform.lan.LanPermissionGateway
import com.whalerein.android.state.access.LanPermissionState
import com.whalerein.android.state.access.StandaloneTarget
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assume.assumeTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Real platform permission behavior on API 37 for a LAN Standalone target.
 *
 * The test controls the runtime permission through UiAutomation, so it verifies
 * granted/denied/revoked states from the actual package manager rather than a
 * fake. Older emulators skip with an explicit reason because the permission does
 * not exist before API 37.
 */
@RunWith(AndroidJUnit4::class)
class LanPermissionDeviceTest {
    @get:Rule
    val rule = createAndroidComposeRule<SupportTestActivity>()

    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val packageName = instrumentation.targetContext.packageName
    private val instance = InstanceId("instance-a")

    @Test
    fun `AC-28_lan_permission_grant_deny_and_revoke_for_a_lan_target`() {
        assumeTrue(
            "ACCESS_LOCAL_NETWORK and its runtime prompt exist only on API 37+",
            Build.VERSION.SDK_INT >= 37,
        )
        runCatching {
            instrumentation.uiAutomation.revokeRuntimePermission(
                packageName,
                Manifest.permission.ACCESS_LOCAL_NETWORK,
            )
        }
        val gateway = rule.activity.lanPermission

        assertEquals(LanPermissionState.Unknown, runBlocking { gateway.state() })

        // Deny through the real system prompt. Back cancels the one-shot
        // permission request, which the platform delivers as a denial.
        val denial = CompletableDeferred<LanPermissionState>()
        val requester = Thread {
            runBlocking { denial.complete(gateway.request()) }
        }
        requester.start()
        Thread.sleep(1_500)
        instrumentation.sendKeyDownUpSync(android.view.KeyEvent.KEYCODE_BACK)
        assertEquals(
            LanPermissionState.Denied,
            runBlocking { withTimeout(10_000) { denial.await() } },
        )
        assertEquals(LanPermissionState.Denied, runBlocking { gateway.state() })

        // Grant through the platform, then confirm no second prompt is shown.
        instrumentation.uiAutomation.grantRuntimePermission(
            packageName,
            Manifest.permission.ACCESS_LOCAL_NETWORK,
        )
        assertEquals(LanPermissionState.Granted, runBlocking { gateway.state() })
        assertEquals(LanPermissionState.Granted, runBlocking { gateway.request() })

        // Revocation is observable after the user previously answered.
        instrumentation.uiAutomation.revokeRuntimePermission(
            packageName,
            Manifest.permission.ACCESS_LOCAL_NETWORK,
        )
        assertEquals(LanPermissionState.Revoked, runBlocking { gateway.state() })
    }

    @Test
    fun `AC-28_public_target_never_constructs_a_lan_prompt`() {
        val counting = CountingLanGateway()
        val port = LanAwarePairingPort(FakePairingPort(), counting)

        assertEquals(
            Outcome.Success(com.whalerein.android.state.access.TransportValidation.Valid),
            runBlocking {
                port.validateTarget(instance, StandaloneTarget("https://cloud.whalerein.example"))
            },
        )
        assertEquals(LanPermissionState.NotRequired, runBlocking { port.lanPermission() })
        assertEquals(
            LanPermissionState.NotRequired,
            runBlocking { port.requestLanPermission() },
        )
        assertEquals(0, counting.calls.get())

        // A LAN target is the only path that consults the permission gateway.
        runBlocking {
            port.validateTarget(instance, StandaloneTarget("https://192.168.1.10:8443"))
        }
        runBlocking { port.lanPermission() }
        assertEquals(1, counting.calls.get())
    }

    private class CountingLanGateway : LanPermissionGateway {
        val calls = AtomicInteger()
        override suspend fun state(): LanPermissionState {
            calls.incrementAndGet()
            return LanPermissionState.Granted
        }

        override suspend fun request(): LanPermissionState {
            calls.incrementAndGet()
            return LanPermissionState.Granted
        }
    }

    private class RecordingLanGateway : LanPermissionGateway {
        override suspend fun state(): LanPermissionState = LanPermissionState.Unknown

        override suspend fun request(): LanPermissionState = LanPermissionState.Denied
    }
}
