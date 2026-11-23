package com.whalerein.android.corebridge.trust

import java.security.KeyStore
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import java.util.Base64

/**
 * Android platform trust for Core's system-trust consumer (HTTPS and MQTT).
 *
 * Rust cannot enumerate Android's system store, so this helper reads
 * `AndroidCAStore` and returns the anchors as one PEM bundle. Core receives that
 * bundle as constructor data and uses it for system mode; nothing is written to
 * disk and no process-global certificate path is changed. Custom-CA instances
 * ignore it and trust their own bundle. The bundle contains only public
 * certificates; it is never logged and never exported.
 *
 * Device-only: Android's system trust store is not available to JVM tests.
 */
object AndroidTrustRoots {
    private const val ANDROID_CA_STORE = "AndroidCAStore"
    private const val PEM_HEADER = "-----BEGIN CERTIFICATE-----"
    private const val PEM_FOOTER = "-----END CERTIFICATE-----"

    /**
     * Read the platform CA certificates as one PEM bundle for Core.
     *
     * Returns the bytes directly; the caller passes them to the Core
     * constructor. Nothing is persisted and no environment variable is set.
     */
    fun platformPem(): ByteArray {
        val pem = platformCertificatesPem()
        if (pem.isEmpty()) throw java.io.IOException("no platform CA certificates")
        return pem
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
