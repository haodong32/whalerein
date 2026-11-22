package com.whalerein.android.corebridge.secret

import com.whalerein.android.corebridge.SecretStoreException
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test
import com.whalerein.android.corebridge.trust.AndroidTrustRoots

/**
 * Deterministic JVM coverage of the store state machine. The controlled
 * backend and crypto replace Android Keystore and file I/O; the real Keystore
 * read/write/delete path is device-only and lives in androidTest.
 */
class EncryptedSecretStoreTest {
    private class FakeCrypto(
        var invalidated: Boolean = false,
        var unavailable: Boolean = false,
        var corrupt: Boolean = false,
    ) : SecretCrypto {
        override fun encrypt(plaintext: ByteArray): ByteArray {
            if (invalidated) throw KeyInvalidatedException()
            if (unavailable) throw CryptoUnavailableException()
            return plaintext.reversedArray()
        }

        override fun decrypt(ciphertext: ByteArray): ByteArray {
            if (invalidated) throw KeyInvalidatedException()
            if (unavailable) throw CryptoUnavailableException()
            if (corrupt || ciphertext.isEmpty()) throw IllegalStateException("bad ciphertext")
            return ciphertext.reversedArray()
        }
    }

    private class FakeBackend : SecretFileBackend {
        var document: ByteArray? = null
        var failReads = false
        var failWrites = false
        var writes = 0

        override fun read(): ByteArray? {
            if (failReads) throw java.io.IOException("read failed")
            return document?.copyOf()
        }

        override fun writeAtomically(bytes: ByteArray) {
            if (failWrites) throw java.io.IOException("write failed")
            writes += 1
            document = bytes.copyOf()
        }

        override fun delete() {
            document = null
        }

        override fun exists(): Boolean = document != null
    }

    private fun store(
        crypto: SecretCrypto = FakeCrypto(),
        backend: SecretFileBackend = FakeBackend(),
        diagnostics: SecretStoreDiagnostics? = null,
    ) = EncryptedSecretStore(crypto, backend, diagnostics)

    @Test
    @DisplayName("AC-07.keystore-absent returns null without creating a document")
    fun `ac_07_keystore_absent_returns_null_without_creating_a_document`() {
        val backend = FakeBackend()
        val store = store(backend = backend)
        assertNull(store.load("cloud-app-session"))
        assertEquals(null, backend.document)
        assertEquals(0, backend.writes)
    }

    @Test
    @DisplayName("AC-07.keystore-absent returns null for an absent key in an existing document")
    fun `ac_07_keystore_absent_returns_null_for_an_absent_key_in_an_existing_document`() {
        val store = store()
        store.save("first", "one".toByteArray())
        assertNull(store.load("second"))
    }

    @Test
    @DisplayName("save load and delete round trip without exposing plaintext")
    fun `save_load_and_delete_round_trip_without_exposing_plaintext`() {
        val backend = FakeBackend()
        val store = store(backend = backend)
        store.save("key", "secret-value".toByteArray())
        assertArrayEquals("secret-value".toByteArray(), store.load("key"))
        val document = String(backend.document!!, Charsets.UTF_8)
        assertTrue(!document.contains("secret-value"), "ciphertext only on disk")
        store.delete("key")
        assertNull(store.load("key"))
        // Deleting an absent key is a no-op.
        store.delete("key")
    }

    @Test
    @DisplayName("AC-07.keystore-unavailable reports temporary failure and preserves the previous document")
    fun `ac_07_keystore_unavailable_reports_temporary_failure_and_preserves_the_previous_document`() {
        val backend = FakeBackend()
        val store = store(backend = backend)
        store.save("key", "value".toByteArray())
        val previous = backend.document?.copyOf()

        val failingWrite = store(backend = backend.apply { failWrites = true })
        assertThrows(SecretStoreException.Unavailable::class.java) {
            failingWrite.save("key", "new-value".toByteArray())
        }
        assertArrayEquals(previous, backend.document)

        val failingRead = store(backend = backend.apply { failWrites = false; failReads = true })
        assertThrows(SecretStoreException.Unavailable::class.java) {
            failingRead.load("key")
        }
        assertArrayEquals(previous, backend.document)
    }

    @Test
    @DisplayName("AC-07.keystore-invalid reports an invalidated key and keeps the previous scope")
    fun `ac_07_keystore_invalid_reports_an_invalidated_key_and_keeps_the_previous_scope`() {
        val backend = FakeBackend()
        val crypto = FakeCrypto()
        val store = store(crypto = crypto, backend = backend)
        store.save("key", "value".toByteArray())
        val previous = backend.document?.copyOf()

        crypto.invalidated = true
        assertThrows(SecretStoreException.Failed::class.java) { store.load("key") }
        assertThrows(SecretStoreException.Failed::class.java) {
            store.save("key", "other".toByteArray())
        }
        assertArrayEquals(previous, backend.document)
    }

    @Test
    @DisplayName("AC-07.keystore-invalid reports corrupt ciphertext and a corrupt document")
    fun `ac_07_keystore_invalid_reports_corrupt_ciphertext_and_a_corrupt_document`() {
        val backend = FakeBackend()
        val crypto = FakeCrypto()
        val store = store(crypto = crypto, backend = backend)
        store.save("key", "value".toByteArray())
        crypto.corrupt = true
        assertThrows(SecretStoreException.Failed::class.java) { store.load("key") }

        backend.document = "not json".toByteArray()
        assertThrows(SecretStoreException.Failed::class.java) { store.load("key") }
        assertThrows(SecretStoreException.Failed::class.java) {
            store.save("key", "value".toByteArray())
        }
    }

    @Test
    @DisplayName("concurrent entry updates are serialized and never lose an entry")
    fun `concurrent_entry_updates_are_serialized_and_never_lose_an_entry`() {
        val store = store()
        val workers = 16
        val entries = 32
        val pool = Executors.newFixedThreadPool(workers)
        val start = CountDownLatch(1)
        val done = CountDownLatch(entries)
        repeat(entries) { index ->
            pool.execute {
                start.await()
                try {
                    store.save("key-$index", "value-$index".toByteArray())
                } finally {
                    done.countDown()
                }
            }
        }
        start.countDown()
        assertTrue(done.await(10, TimeUnit.SECONDS))
        pool.shutdownNow()
        for (index in 0 until entries) {
            assertArrayEquals("value-$index".toByteArray(), store.load("key-$index"))
        }
    }

    @Test
    @DisplayName("concurrent readers and writers keep atomic entry views")
    fun `concurrent_readers_and_writers_keep_atomic_entry_views`() {
        val store = store()
        val stop = AtomicBoolean(false)
        val failure = java.util.concurrent.atomic.AtomicReference<Throwable?>()
        val reader = Thread {
            try {
                while (!stop.get()) {
                    store.load("shared")
                }
            } catch (error: Throwable) {
                failure.set(error)
            }
        }
        reader.start()
        repeat(64) { index ->
            store.save("shared", "value-$index".toByteArray())
        }
        stop.set(true)
        reader.join(5_000)
        assertNull(failure.get())
        assertArrayEquals("value-63".toByteArray(), store.load("shared"))
    }

    @Test
    @DisplayName("diagnostics carry fixed codes only")
    fun `diagnostics_carry_fixed_codes_only`() {
        val codes = mutableListOf<String>()
        val backend = FakeBackend()
        val store = store(backend = backend, diagnostics = { codes += it })
        store.load("missing")
        store.save("key", "value".toByteArray())
        backend.document = "broken".toByteArray()
        assertThrows(SecretStoreException.Failed::class.java) { store.load("key") }
        assertEquals(
            listOf(SecretStoreCodes.ABSENT, SecretStoreCodes.CORRUPT),
            codes,
        )
    }

    @Test
    @DisplayName("native root PEM assembly keeps certificate boundaries")
    fun `native_root_pem_assembly`() {
        val pem = String(
            AndroidTrustRoots.pemBundle(listOf(byteArrayOf(1, 2, 3), byteArrayOf(4, 5))),
        )
        assertEquals(2, pem.split("-----BEGIN CERTIFICATE-----").size - 1)
        assertEquals(2, pem.split("-----END CERTIFICATE-----").size - 1)
        assertTrue(pem.contains("\nAQID\n"))
        assertTrue(AndroidTrustRoots.pemBundle(emptyList()).isEmpty())
    }

    @Test
    @DisplayName("keystore aliases are stable per instance and distinct across instances")
    fun `keystore_aliases_are_stable_per_instance_and_distinct_across_instances`() {
        val first = java.io.File("/tmp/instance-a")
        val second = java.io.File("/tmp/instance-b")
        assertEquals(
            AndroidKeystoreCrypto.aliasFor(first),
            AndroidKeystoreCrypto.aliasFor(java.io.File("/tmp/instance-a/.")),
        )
        assertTrue(AndroidKeystoreCrypto.aliasFor(first) != AndroidKeystoreCrypto.aliasFor(second))
        assertTrue(AndroidKeystoreCrypto.aliasFor(first).startsWith("whalerein.secret."))
        assertTrue(!AndroidKeystoreCrypto.aliasFor(first).contains("instance-a"))
    }
}
