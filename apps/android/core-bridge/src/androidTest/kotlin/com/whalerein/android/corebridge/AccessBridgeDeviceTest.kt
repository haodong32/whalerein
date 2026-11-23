package com.whalerein.android.corebridge

import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.whalerein.android.state.access.CaImportState
import com.whalerein.android.state.access.ExternalAuthorization
import com.whalerein.android.state.access.ExternalTarget
import com.whalerein.android.state.access.StartupAuthorization
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SessionId
import com.whalerein.android.state.core.SessionRef
import java.io.File
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Access behavior that needs the packaged native Core and the Android file
 * sandbox. Without a Cloud account or a paired Host these cases prove the
 * denial and persistence paths; the live sign-in and pairing journeys still
 * require the local Cloud/Standalone stack.
 */
@RunWith(AndroidJUnit4::class)
class AccessBridgeDeviceTest {
    private val context = ApplicationProvider.getApplicationContext<android.content.Context>()

    @Test
    fun ac_25_signed_out_cold_launch_never_restores_a_recorded_target() = runBlocking {
        val root = files("access-startup")
        val document = bootstrap(root)
        val instance = InstanceId("cloud-production")
        AndroidCore.open(context, document, instance).use { core ->
            val target = SessionRef(
                HostRef(instance, HostId("host-1")),
                SessionId("session-1"),
            )
            core.accessPorts.startup.recordTarget(target)

            val snapshot = (core.accessPorts.startup.restore(instance) as Outcome.Success).value

            assertEquals(StartupAuthorization.SignedOut, snapshot.authorization)
            assertNull(snapshot.target)
            assertNull(snapshot.host)
        }
    }

    @Test
    fun ac_25_external_entry_denies_an_unpaired_host_without_route_detail() = runBlocking {
        val root = files("access-external")
        val document = bootstrap(root)
        val instance = InstanceId("cloud-production")
        AndroidCore.open(context, document, instance).use { core ->
            val decision = core.accessPorts.externalEntry.authorize(
                ExternalTarget(hostId = HostId("host-1"), sessionId = "session-1"),
            )
            assertEquals(ExternalAuthorization.Unauthorized, decision)
        }
    }

    @Test
    fun ac_25_imported_ca_is_applied_to_the_intended_standalone_instance() = runBlocking {
        val root = files("access-ca")
        val document = bootstrap(root)
        appConfigSetActive(document, "standalone")
        val instance = InstanceId("standalone")
        AndroidCore.open(context, document, instance).use { core ->
            val ready = core.accessPorts.pairing.importCa(instance, VALID_PEM)
            assertEquals(Outcome.Success(CaImportState.Ready(instance)), ready)
            val view = appConfigRead(document).instances.first { it.id == "standalone" }
            assertTrue("Core materialized the CA reference", view.hasStandaloneCa)
            assertTrue(File(view.standaloneCaPath!!).isFile)

            val rejected = core.accessPorts.pairing.importCa(instance, "not a certificate")
            assertEquals(
                Outcome.Success(CaImportState.Rejected("ca_invalid")),
                rejected,
            )

            val wrongInstance = core.accessPorts.pairing.importCa(
                InstanceId("cloud-production"),
                VALID_PEM,
            )
            assertEquals(
                Outcome.Success(CaImportState.Rejected("ca_scope_mismatch")),
                wrongInstance,
            )
        }
    }

    @Test
    fun ac_25_last_target_record_is_scoped_to_the_instance_directory() = runBlocking {
        val root = files("access-store")
        val document = bootstrap(root)
        val instance = InstanceId("cloud-production")
        val other = InstanceId("standalone")
        AndroidCore.open(context, document, instance).use { core ->
            val target = SessionRef(
                HostRef(instance, HostId("host-1")),
                SessionId("session-1"),
            )
            core.accessPorts.startup.recordTarget(target)
            val store = core.accessStore
            assertEquals(target, store.readTarget(instance))
            assertNull(store.readTarget(other))
        }
    }

    private fun files(name: String): File =
        File(context.filesDir, "device-tests/$name").apply {
            deleteRecursively()
            mkdirs()
        }

    private fun bootstrap(root: File): String {
        val document = File(root, "client/app.toml").absolutePath
        appConfigBootstrap(document)
        return document
    }

    private companion object {
        /** Public test CA used by the Core/mobile-binding configuration tests. */
        const val VALID_PEM = """-----BEGIN CERTIFICATE-----
MIIBazCCARKgAwIBAgIUCYEJf6AWvrWHzQnqALYWDNrwYfgwCgYIKoZIzj0EAwIw
HDEaMBgGA1UEAwwRV2hhbGVSZWluIFRlc3QgQ0EwHhcNMjYxMTE0MDQzMDA2WhcN
MzYxMTExMDQzMDA2WjAcMRowGAYDVQQDDBFXaGFsZVJlaW4gVGVzdCBDQTBZMBMG
ByqGSM49AgEGCCqGSM49AwEHA0IABIDzEtmehsRaR0Vx65NWD4y1kflMTDFblTA8
64kOENsLgnPx+O1z8BV4JzL5+wqRd/gcxvR5WULQOuxjCoDpm4ejMjAwMB0GA1Ud
DgQWBBQ0nEJTr/TR1J9+vLqmx9euDviHNzAPBgNVHRMBAf8EBTADAQH/MAoGCCqG
SM49BAMCA0cAMEQCIEyFy7p0NegnWrzl41pLP89cCp9itTXSeJ8I13iKpm28AiAR
kg3kmwrRgJT2uxIVgusk+cBIKzWjxMwkC7uvy8RsrQ==
-----END CERTIFICATE-----
"""
    }
}
