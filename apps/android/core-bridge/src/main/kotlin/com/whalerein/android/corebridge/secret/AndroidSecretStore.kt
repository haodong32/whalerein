package com.whalerein.android.corebridge.secret

import java.io.File

/** Builds the Keystore-backed store for one profile instance. */
object AndroidSecretStore {
    private const val DIRECTORY = "secrets"
    private const val DOCUMENT = "secrets.json"

    fun create(
        instanceDirectory: File,
        diagnostics: SecretStoreDiagnostics? = null,
    ): EncryptedSecretStore {
        val directory = File(instanceDirectory, DIRECTORY).apply {
            if (!exists() && !mkdirs()) throw java.io.IOException("secret directory is unavailable")
        }
        return EncryptedSecretStore(
            crypto = AndroidKeystoreCrypto(AndroidKeystoreCrypto.aliasFor(instanceDirectory)),
            files = FileSecretBackend(File(directory, DOCUMENT)),
            diagnostics = diagnostics,
        )
    }
}
