package com.whalerein.android.corebridge.trust

import com.whalerein.android.corebridge.installNativeTrustRoots
import java.io.File
import java.security.KeyStore
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import java.util.Base64

/**
 * Android platform trust for Core's system-trust consumer (HTTPS and MQTT).
 *
 * Core's system-trust path uses `rustls-native-certs` for HTTPS and MQTT.
 * Android has no OpenSSL certificate directory, so this helper materializes
 * `AndroidCAStore` into a private PEM bundle and installs it before Core opens.
 * Custom-CA instances replace the system roots with their own bundle instead.
 * The bundle contains only public certificates; it is never logged and never
 * exported.
 *
 * Device-only: Android's system trust store is not available to JVM tests.
 */
object AndroidTrustRoots {
    private const val ANDROID_CA_STORE = "AndroidCAStore"
    private const val PEM_HEADER = "-----BEGIN CERTIFICATE-----"
    private const val PEM_FOOTER = "-----END CERTIFICATE-----"

    /**
     * Extract the platform CA certificates and install them for the process.
     * Returns the private PEM path so callers can report preparation without
     * exposing certificate contents.
     */
    fun install(filesDir: File): File {
        val directory = File(filesDir, "trust").apply {
            if (!exists() && !mkdirs()) throw java.io.IOException("trust directory is unavailable")
        }
        val target = File(directory, "native-roots.pem")
        val pem = platformCertificatesPem()
        if (pem.isEmpty()) throw java.io.IOException("no platform CA certificates")
        val temporary = File.createTempFile(".native-roots.", ".tmp", directory)
        try {
            temporary.writeBytes(pem)
            if (!temporary.renameTo(target)) {
                temporary.copyTo(target, overwrite = true)
                temporary.delete()
            }
        } finally {
            temporary.delete()
        }
        installNativeTrustRoots(target.absolutePath)
        return target
    }

    /** Pure PEM assembly helper, unit tested on the JVM. */
    fun pemBundle(certificates: List<ByteArray>): ByteArray {
        val builder = StringBuilder()
        val encoder = Base64.getMimeEncoder(64, byteArrayOf('\n'.code.toByte()))
        for (certificate in certificates) {
            if (certificate.isEmpty()) continue
            builder.append(PEM_HEADER).append('\n')
            builder.append(String(encoder.encode(certificate), Charsets.US_ASCII)).append('\n')
            builder.append(PEM_FOOTER).append('\n')
        }
        return builder.toString().toByteArray(Charsets.US_ASCII)
    }

    private fun platformCertificatesPem(): ByteArray {
        val certificates = mutableListOf<ByteArray>()
        try {
            val store = KeyStore.getInstance(ANDROID_CA_STORE).apply { load(null) }
            val aliases = store.aliases()
            val factory = CertificateFactory.getInstance("X.509")
            while (aliases.hasMoreElements()) {
                val alias = aliases.nextElement()
                val certificate = store.getCertificate(alias) as? X509Certificate ?: continue
                // Skip expired or not-yet-valid roots; they cannot verify peers.
                try {
                    certificate.checkValidity()
                } catch (_: Exception) {
                    continue
                }
                certificates += factory.generateCertificate(certificate.encoded.inputStream()).encoded
            }
        } catch (error: Exception) {
            throw java.io.IOException("platform trust store is unavailable", error)
        }
        return pemBundle(certificates)
    }
}
