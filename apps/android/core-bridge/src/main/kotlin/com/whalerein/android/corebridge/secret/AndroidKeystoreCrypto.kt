package com.whalerein.android.corebridge.secret

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.security.keystore.KeyProperties
import java.security.KeyStore
import java.security.SecureRandom
import java.security.UnrecoverableKeyException
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * AES-256-GCM key in the Android Keystore, one alias per profile instance.
 *
 * The alias is derived from a hash of the canonical instance directory, so
 * switching profiles cannot decrypt another instance's document and a
 * reinstall with the same path starts from a fresh platform key.
 */
class AndroidKeystoreCrypto(
    private val alias: String,
    private val random: SecureRandom = SecureRandom(),
) : SecretCrypto {
    private val lock = Any()
    @Volatile
    private var cached: SecretKey? = null

    override fun encrypt(plaintext: ByteArray): ByteArray {
        val key = key()
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, key)
        val iv = cipher.iv
        val ciphertext = cipher.doFinal(plaintext)
        return ByteArray(iv.size + ciphertext.size).also { output ->
            System.arraycopy(iv, 0, output, 0, iv.size)
            System.arraycopy(ciphertext, 0, output, iv.size, ciphertext.size)
        }
    }

    override fun decrypt(ciphertext: ByteArray): ByteArray {
        if (ciphertext.size <= IV_BYTES) throw IllegalArgumentException("truncated ciphertext")
        val key = key()
        return try {
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(
                Cipher.DECRYPT_MODE,
                key,
                GCMParameterSpec(TAG_BITS, ciphertext, 0, IV_BYTES),
            )
            cipher.doFinal(ciphertext, IV_BYTES, ciphertext.size - IV_BYTES)
        } catch (_: KeyPermanentlyInvalidatedException) {
            cached = null
            throw KeyInvalidatedException()
        } catch (error: UnrecoverableKeyException) {
            cached = null
            throw KeyInvalidatedException()
        }
    }

    private fun key(): SecretKey {
        cached?.let { return it }
        synchronized(lock) {
            cached?.let { return it }
            val keyStore = try {
                KeyStore.getInstance(PROVIDER).apply { load(null) }
            } catch (_: Exception) {
                throw CryptoUnavailableException()
            }
            try {
                val existing = keyStore.getKey(alias, null) as? SecretKey
                if (existing != null) {
                    cached = existing
                    return existing
                }
            } catch (_: UnrecoverableKeyException) {
                throw KeyInvalidatedException()
            } catch (_: KeyPermanentlyInvalidatedException) {
                throw KeyInvalidatedException()
            } catch (_: Exception) {
                throw CryptoUnavailableException()
            }
            val generated = try {
                KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, PROVIDER).apply {
                    init(
                        KeyGenParameterSpec.Builder(
                            alias,
                            KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
                        )
                            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                            .setKeySize(256)
                            .setRandomizedEncryptionRequired(true)
                            .build()
                    )
                }.generateKey()
            } catch (_: Exception) {
                throw CryptoUnavailableException()
            }
            cached = generated
            return generated
        }
    }

    companion object {
        private const val PROVIDER = "AndroidKeyStore"
        private const val TRANSFORMATION = "AES/GCM/NoPadding"
        private const val IV_BYTES = 12
        private const val TAG_BITS = 128

        /**
         * Stable alias for the instance's canonical private directory. Uses a
         * SHA-256 prefix so no path or profile identity appears in the alias.
         */
        fun aliasFor(instanceDirectory: java.io.File): String {
            val canonical = try {
                instanceDirectory.canonicalPath
            } catch (_: Exception) {
                instanceDirectory.absolutePath
            }
            val digest = java.security.MessageDigest.getInstance("SHA-256")
                .digest(canonical.toByteArray(Charsets.UTF_8))
            return "whalerein.secret." + digest.take(16).joinToString("") { "%02x".format(it) }
        }
    }
}
