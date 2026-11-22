package com.whalerein.android.corebridge.secret

import java.io.File
import java.io.FileOutputStream
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import java.nio.file.StandardOpenOption

/**
 * One file per profile under private, backup-excluded app storage.
 *
 * Writes go to a sibling temporary file, are forced to disk, then replace the
 * document with an atomic rename. A failed write leaves the previous complete
 * document in place.
 */
class FileSecretBackend(private val file: File) : SecretFileBackend {
    override fun read(): ByteArray? {
        if (!file.exists()) return null
        return file.readBytes()
    }

    override fun writeAtomically(bytes: ByteArray) {
        val directory = file.parentFile
            ?: throw java.io.IOException("secret store has no parent directory")
        if (!directory.exists() && !directory.mkdirs()) {
            throw java.io.IOException("secret store directory is unavailable")
        }
        val temporary = File.createTempFile(".${file.name}.", ".tmp", directory)
        try {
            FileOutputStream(temporary).use { stream ->
                stream.write(bytes)
                stream.flush()
                stream.fd.sync()
            }
            try {
                Files.move(
                    temporary.toPath(),
                    file.toPath(),
                    StandardCopyOption.ATOMIC_MOVE,
                    StandardCopyOption.REPLACE_EXISTING,
                )
            } catch (_: java.nio.file.AtomicMoveNotSupportedException) {
                Files.move(
                    temporary.toPath(),
                    file.toPath(),
                    StandardCopyOption.REPLACE_EXISTING,
                )
            }
            forceDirectory(directory)
        } finally {
            temporary.delete()
        }
    }

    override fun delete() {
        if (file.exists() && !file.delete()) {
            throw java.io.IOException("secret store document could not be deleted")
        }
    }

    override fun exists(): Boolean = file.exists()

    private fun forceDirectory(directory: File) {
        try {
            FileChannel.open(directory.toPath(), StandardOpenOption.READ).use { it.force(true) }
        } catch (_: Exception) {
            // Directory fsync is best effort on Android; the file fsync and
            // atomic rename already protect the previous complete document.
        }
    }
}
