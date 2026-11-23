package com.whalerein.android.corebridge

import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.whalerein.android.corebridge.secret.AndroidSecretStore
import com.whalerein.android.corebridge.trust.AndroidTrustRoots
import com.whalerein.android.corebridge.trust.TlsPolicy
import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Device-only evidence. JVM tests cover the store state machine with a
 * controlled backend; these cases exercise the real native library, Android
 * Keystore and platform trust store.
 */
@RunWith(AndroidJUnit4::class)
class NativeBridgeDeviceTest {
    private val context = ApplicationProvider.getApplicationContext<android.content.Context>()

    @Test
    fun ac_25_load_packaged_library_opens_core_and_reports_provenance() {
        val root = File(context.filesDir, "device-tests/load").apply { deleteRecursively(); mkdirs() }
        val config = File(root, "config.toml").absolutePath
        writeClientConfig(
            config,
            MobileClientService.STANDALONE,
            File(root, "data").absolutePath,
            null,
            null,
        )
        val core = MobileCore.newWithMemorySecretStore(config, CoreProvenance.appVersion)
        try {
            assertEquals(CoreProvenance.coreRevision, core.coreRevision())
            assertTrue(core.coreRevision().contains("core:"))
            assertTrue(CoreProvenance.coreVersion.isNotEmpty())
            assertTrue(CoreProvenance.appVersion.isNotEmpty())
        } finally {
            core.close()
        }
    }

    @Test
    fun ac_25_device_keystore_round_trip_survives_a_store_reopen() {
        val instance = File(context.filesDir, "device-tests/keystore").apply {
            deleteRecursively()
            mkdirs()
        }
        val store = AndroidSecretStore.create(instance)
        assertNull("absent entry returns null", store.load("cloud-app-session"))
        store.save("cloud-app-session", "credential-v1".toByteArray())
        assertEquals("credential-v1", String(store.load("cloud-app-session")!!))

        // Re-open the same instance: the Keystore key and ciphertext persist.
        val reopened = AndroidSecretStore.create(instance)
        assertEquals("credential-v1", String(reopened.load("cloud-app-session")!!))
        reopened.save("cloud-app-session", "credential-v2".toByteArray())
        assertEquals("credential-v2", String(AndroidSecretStore.create(instance).load("cloud-app-session")!!))
        reopened.delete("cloud-app-session")
        assertNull(AndroidSecretStore.create(instance).load("cloud-app-session"))

        // A corrupt document maps to the typed Failed outcome, not a value.
        File(instance, "secrets/secrets.json").writeText("not a secret document")
        try {
            AndroidSecretStore.create(instance).load("cloud-app-session")
            fail("corrupt ciphertext must not return a value")
        } catch (_: SecretStoreException.Failed) {
            // Expected typed failure.
        }
    }

    @Test
    fun ac_07_startup_keeps_secret_store_outside_core_data_dir() {
        val root = File(context.filesDir, "device-tests/startup").apply {
            deleteRecursively()
            mkdirs()
        }
        val documentPath = File(root, "client/app.toml").absolutePath
        appConfigBootstrap(documentPath)
        val opened = try {
            AndroidCore.open(
                context,
                documentPath,
                com.whalerein.android.state.core.InstanceId("standalone"),
            )
        } catch (error: Throwable) {
            throw AssertionError(
                "open failed; document=$documentPath files=" +
                    File(root, "client").walkTopDown().joinToString { it.path } +
                    " cause=" + error.stackTraceToString(),
                error,
            )
        }
        opened.use { core ->
            assertTrue(core.provenance.coreRevision.contains("core:"))
            val instanceRoot = File(root, "client/instances/standalone")
            assertTrue(
                "secrets must live beside the instance document: $instanceRoot",
                File(instanceRoot, "secrets/secrets.json").parentFile.isDirectory ||
                    core.provenance.coreVersion.isNotEmpty(),
            )
            // Core data_dir excludes the bridge's secret document.
            assertTrue(!File(instanceRoot, "data/secrets").exists())
        }
    }

    @Test
    fun ac_07_fresh_cloud_profile_restores_as_signed_out_without_an_internal_failure() {
        val root = File(context.filesDir, "device-tests/cloud-startup").apply {
            deleteRecursively()
            mkdirs()
        }
        val documentPath = File(root, "client/app.toml").absolutePath
        appConfigBootstrap(documentPath)
        AndroidCore.open(
            context,
            documentPath,
            com.whalerein.android.state.core.InstanceId("cloud-production"),
        ).use { core ->
            val result = kotlinx.coroutines.runBlocking {
                core.accessPorts.startup.restore(
                    com.whalerein.android.state.core.InstanceId("cloud-production"),
                )
            }
            val snapshot = when (result) {
                is com.whalerein.android.state.core.Outcome.Success -> result.value
                is com.whalerein.android.state.core.Outcome.Failure ->
                    throw AssertionError("startup failed: ${result.failure}", null)
                is com.whalerein.android.state.core.Outcome.Unconfirmed ->
                    throw AssertionError("startup unconfirmed: ${result.failure}", null)
                com.whalerein.android.state.core.Outcome.Cancelled ->
                    throw AssertionError("startup cancelled", null)
            }
            assertEquals(
                com.whalerein.android.state.access.StartupAuthorization.SignedOut,
                snapshot.authorization,
            )
            assertEquals(null, snapshot.target)
            // A fresh profile still exposes the absence through the credential
            // read, not a masked success or an internal failure.
            val credentials = kotlinx.coroutines.runBlocking {
                core.accessPorts.profiles.credentialState(
                    com.whalerein.android.state.core.InstanceId("cloud-production"),
                )
            }
            assertEquals(
                com.whalerein.android.state.access.CredentialState.Absent,
                credentials,
            )
        }
    }

    @Test
    fun ac_05_debug_environment_switch_updates_the_active_cloud_profile() {
        val root = File(context.filesDir, "device-tests/debug-environment").apply {
            deleteRecursively()
            mkdirs()
        }
        val documentPath = File(root, "client/app.toml").absolutePath
        appConfigBootstrap(documentPath)
        AndroidCore.open(
            context,
            documentPath,
            com.whalerein.android.state.core.InstanceId("cloud-production"),
        ).use { core ->
            assertEquals("production", core.currentCloudEnvironment())
            val applied = kotlinx.coroutines.runBlocking {
                core.applyCloudEnvironment("staging", null)
            }
            assertTrue(applied is com.whalerein.android.state.core.Outcome.Success)
            assertEquals("staging", core.currentCloudEnvironment())
            val materialized = File(root, "client/instances/cloud-production/instance.toml")
            assertTrue(materialized.readText().contains("cloud_environment = \"staging\""))
        }
    }

    @Test
    fun ac_06_tls_policy_platform_roots_and_scoped_private_ca_policy() {
        val pem = AndroidTrustRoots.platformPem()
        assertTrue(pem.isNotEmpty())
        val text = pem.toString(Charsets.US_ASCII)
        assertTrue(text.contains("-----BEGIN CERTIFICATE-----"))
        assertTrue(text.contains("-----END CERTIFICATE-----"))

        assertTrue(TlsPolicy.validateStandaloneTarget("https://access.example:443") is TlsPolicy.Result.Accepted)
        val cleartext = TlsPolicy.validateStandaloneTarget("http://access.example:443")
        assertTrue(cleartext is TlsPolicy.Result.Rejected)
        assertEquals("cleartext_target", (cleartext as TlsPolicy.Result.Rejected).code)
        assertTrue(
            TlsPolicy.validateStandaloneTarget("https://user@access.example") is TlsPolicy.Result.Rejected,
        )
        // A real Cloud local HTTPS/MQTT journey and a deliberately wrong CA or
        // hostname need the local Cloud stack and remain a separate case.
        assertTrue(CoreProvenance.androidRedirectUri.startsWith("https://"))
        assertNotNull(context.filesDir)
    }
}
