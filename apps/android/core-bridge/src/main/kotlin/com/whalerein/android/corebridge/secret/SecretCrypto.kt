package com.whalerein.android.corebridge.secret

/**
 * Authenticated encryption for one profile instance.
 *
 * The implementation owns a hardware-backed key; callers only see
 * authenticated ciphertext. Implementations must be thread-safe.
 */
interface SecretCrypto {
    fun encrypt(plaintext: ByteArray): ByteArray

    fun decrypt(ciphertext: ByteArray): ByteArray
}

/** The key no longer exists or was permanently invalidated by the platform. */
class KeyInvalidatedException : Exception()

/** The platform crypto provider is temporarily unavailable. */
class CryptoUnavailableException : Exception()

/**
 * Persistence behind an encrypted secret store.
 *
 * [read] returns `null` only when the whole document is genuinely absent.
 * A temporary I/O failure must throw so callers can distinguish it from
 * absence. [writeAtomically] replaces the complete document or throws.
 */
interface SecretFileBackend {
    fun read(): ByteArray?

    fun writeAtomically(bytes: ByteArray)

    fun delete()

    fun exists(): Boolean
}

/** Redacted store outcome codes for the app journal. Never values or keys. */
fun interface SecretStoreDiagnostics {
    fun onEvent(code: String)
}

object SecretStoreCodes {
    const val ABSENT = "secret_absent"
    const val UNAVAILABLE = "secret_store_unavailable"
    const val KEY_INVALIDATED = "secret_key_invalidated"
    const val CORRUPT = "secret_ciphertext_corrupt"
}
