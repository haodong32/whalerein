package com.whalerein.android.corebridge.secret

import com.whalerein.android.corebridge.SecretStore
import com.whalerein.android.corebridge.SecretStoreException
import java.util.Base64
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

/**
 * Android Keystore backed implementation of the shared UniFFI [SecretStore].
 *
 * Every profile instance owns one authenticated encryption key and one
 * encrypted document. Entry updates are serialized per instance and the whole
 * document is replaced atomically, so a crash or write failure cannot expose a
 * partially updated entry set.
 *
 * Outcomes are distinct:
 * - absent document or key: [load] returns `null`;
 * - temporary platform/storage failure: `SecretStoreException.Unavailable`;
 * - invalidated key or corrupt ciphertext: `SecretStoreException.Failed`.
 *
 * The store never logs or inspects a value beyond length and round-trip
 * integrity. Its diagnostics carry fixed codes only.
 */
class EncryptedSecretStore(
    private val crypto: SecretCrypto,
    private val files: SecretFileBackend,
    private val diagnostics: SecretStoreDiagnostics? = null,
) : SecretStore {
    private val lock = ReentrantLock()

    // One document per instance. Reading it is cheap and bounded by the
    // entries Core stores (keys, tokens and PKCE material, not message bodies).
    private fun readDocument(): MutableMap<String, String>? {
        val bytes = try {
            files.read()
        } catch (_: Exception) {
            unavailable()
        } ?: return null
        return decode(bytes)
    }

    private fun decode(bytes: ByteArray): MutableMap<String, String> {
        try {
            val root = Json.parseToJsonElement(String(bytes, Charsets.UTF_8)).jsonObject
            if (root["version"]?.jsonPrimitive?.intOrNull != FORMAT_VERSION) corrupt()
            val entries = root["entries"]?.jsonObject ?: return mutableMapOf()
            val result = mutableMapOf<String, String>()
            for ((key, value) in entries) {
                val text = value.jsonPrimitive.contentOrNull
                if (key.isEmpty() || text.isNullOrEmpty()) corrupt()
                result[key] = text
            }
            return result
        } catch (error: SecretStoreException) {
            throw error
        } catch (_: Exception) {
            corrupt()
        }
    }

    private fun encode(entries: Map<String, String>): ByteArray {
        val encoded = buildJsonObject {
            put("version", FORMAT_VERSION)
            put(
                "entries",
                JsonObject(entries.mapValues { (_, value) -> JsonPrimitive(value) }),
            )
        }
        return encoded.toString().toByteArray(Charsets.UTF_8)
    }

    override fun load(key: String): ByteArray? = lock.withLock {
        val document = readDocument() ?: run {
            diagnostics?.onEvent(SecretStoreCodes.ABSENT)
            return@withLock null
        }
        val stored = document[key] ?: run {
            diagnostics?.onEvent(SecretStoreCodes.ABSENT)
            return@withLock null
        }
        val ciphertext = try {
            Base64.getDecoder().decode(stored)
        } catch (_: IllegalArgumentException) {
            corrupt()
        }
        try {
            crypto.decrypt(ciphertext)
        } catch (_: KeyInvalidatedException) {
            keyInvalidated()
        } catch (_: CryptoUnavailableException) {
            unavailable()
        } catch (_: Exception) {
            corrupt()
        }
    }

    override fun save(key: String, value: ByteArray) = lock.withLock {
        if (key.isEmpty()) throw SecretStoreException.Failed()
        val encrypted = try {
            crypto.encrypt(value)
        } catch (_: KeyInvalidatedException) {
            keyInvalidated()
        } catch (_: CryptoUnavailableException) {
            unavailable()
        }
        val document = readDocument() ?: mutableMapOf()
        val updated = document.toMutableMap()
        updated[key] = Base64.getEncoder().encodeToString(encrypted)
        try {
            files.writeAtomically(encode(updated))
        } catch (_: Exception) {
            unavailable()
        }
        Unit
    }

    override fun delete(key: String) = lock.withLock {
        val document = readDocument()
        if (document == null || !document.containsKey(key)) {
            return@withLock
        }
        val updated = document.toMutableMap()
        updated.remove(key)
        try {
            files.writeAtomically(encode(updated))
        } catch (_: Exception) {
            unavailable()
        }
        Unit
    }

    private fun unavailable(): Nothing {
        diagnostics?.onEvent(SecretStoreCodes.UNAVAILABLE)
        throw SecretStoreException.Unavailable()
    }

    private fun corrupt(): Nothing {
        diagnostics?.onEvent(SecretStoreCodes.CORRUPT)
        throw SecretStoreException.Failed()
    }

    private fun keyInvalidated(): Nothing {
        diagnostics?.onEvent(SecretStoreCodes.KEY_INVALIDATED)
        throw SecretStoreException.Failed()
    }

    companion object {
        private const val FORMAT_VERSION = 1
    }
}
