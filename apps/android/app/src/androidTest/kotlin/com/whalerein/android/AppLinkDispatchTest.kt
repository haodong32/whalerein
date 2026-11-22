package com.whalerein.android

import android.content.Intent
import android.net.Uri
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.whalerein.android.platform.applinks.AppLinkRouter
import com.whalerein.android.state.access.ExternalTarget
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.InstanceId
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class AppLinkDispatchTest {
    // AC-19.entry: callback and external target dispatch survive recreation and
    // never accept an unregistered origin.
    @Test
    fun AC_19_entry_dispatches_only_registered_callbacks_and_external_targets() {
        val diagnostics = RecordingDiagnosticSink()
        val router = AppLinkRouter(diagnostics)
        val valid = Uri.parse(
            "https://app.whalerein.test/client/android/callback?code=abc&state=def&environment=local",
        )
        assertTrue(router.submitCallback(valid))
        assertEquals(valid.toString(), router.callback.value)
        assertFalse(router.submitCallback(Uri.parse("https://evil.example/client/android/callback")))
        assertFalse(router.submitCallback(Uri.parse("http://app.whalerein.test/client/android/callback")))
        assertFalse(router.submitCallback(Uri.parse("https://app.whalerein.test/other")))

        // A recreated process receives the same Intent again; dispatch is idempotent.
        val intent = Intent(Intent.ACTION_VIEW, valid)
        val recreated = AppLinkRouter(RecordingDiagnosticSink())
        recreated.dispatch(intent)
        assertEquals(valid.toString(), recreated.callback.value)
        recreated.consumeCallback()
        assertNull(recreated.callback.value)

        val target = ExternalTarget(
            profileId = InstanceId("test-instance"),
            hostId = HostId("test-host"),
            sessionId = "test-session",
            itemId = "item-1",
        )
        val targetIntent = Intent(AppLinkRouter.ACTION_OPEN_SESSION).apply {
            putExtra(AppLinkRouter.EXTRA_PROFILE_ID, target.profileId?.value)
            putExtra(AppLinkRouter.EXTRA_HOST_ID, target.hostId?.value)
            putExtra(AppLinkRouter.EXTRA_SESSION_ID, target.sessionId)
            putExtra(AppLinkRouter.EXTRA_ITEM_ID, target.itemId)
        }
        router.dispatch(targetIntent)
        assertEquals(target, router.externalTarget.value)
        router.consumeExternalTarget()
        assertNull(router.externalTarget.value)
    }
}
