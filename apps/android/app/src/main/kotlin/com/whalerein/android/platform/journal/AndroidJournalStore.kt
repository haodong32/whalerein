package com.whalerein.android.platform.journal

import com.whalerein.android.state.support.JournalSchema
import com.whalerein.android.state.support.JournalStore
import com.whalerein.android.state.support.JournalStoreException
import com.whalerein.android.state.support.JournalStoredFile
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.File
import java.io.FileOutputStream

/**
 * Private, backup-excluded journal storage.
 *
 * The directory lives under the app's private `noBackupFilesDir`, so neither
 * cloud backup nor device transfer carries the journal. Each scope has one
 * file whose name comes from [JournalSchema]; the store never logs the file
 * name or its content.
 */
class AndroidJournalStore(private val directory: File) : JournalStore {
    override suspend fun list(): List<JournalStoredFile> = withContext(Dispatchers.IO) {
        try {
            ensureDirectory()
            directory.listFiles()
                ?.filter { it.isFile && it.name.startsWith(FILE_PREFIX) && it.name.endsWith(JournalSchema.FILE_SUFFIX) }
                ?.map { file ->
                    JournalStoredFile(
                        fileId = file.name,
                        scopeId = scopeOf(file.name),
                        modifiedEpochMillis = file.lastModified(),
                        sizeBytes = file.length(),
                    )
                }
                ?.sortedBy { it.modifiedEpochMillis }
                ?: emptyList()
        } catch (error: Exception) {
            throw JournalStoreException("journal list failed", error)
        }
    }

    override suspend fun read(fileId: String): String? = withContext(Dispatchers.IO) {
        val file = resolve(fileId)
        try {
            if (!file.isFile) return@withContext null
            file.readText(Charsets.UTF_8)
        } catch (error: Exception) {
            throw JournalStoreException("journal read failed", error)
        }
    }

    override suspend fun append(fileId: String, text: String) = withContext(Dispatchers.IO) {
        val file = resolve(fileId)
        try {
            ensureDirectory()
            FileOutputStream(file, true).use { stream ->
                stream.write(text.toByteArray(Charsets.UTF_8))
                stream.flush()
                stream.fd.sync()
            }
        } catch (error: Exception) {
            throw JournalStoreException("journal append failed", error)
        }
    }

    override suspend fun replace(fileId: String, text: String) = withContext(Dispatchers.IO) {
        val file = resolve(fileId)
        try {
            ensureDirectory()
            val temporary = File(directory, "${file.name}.tmp")
            FileOutputStream(temporary, false).use { stream ->
                stream.write(text.toByteArray(Charsets.UTF_8))
                stream.flush()
                stream.fd.sync()
            }
            if (!temporary.renameTo(file)) {
                file.delete()
                if (!temporary.renameTo(file)) {
                    throw JournalStoreException("journal replace rename failed", null)
                }
            }
        } catch (error: JournalStoreException) {
            throw error
        } catch (error: Exception) {
            throw JournalStoreException("journal replace failed", error)
        }
    }

    override suspend fun delete(fileId: String) = withContext(Dispatchers.IO) {
        val file = resolve(fileId)
        try {
            if (file.exists() && !file.delete()) {
                throw JournalStoreException("journal delete failed", null)
            }
        } catch (error: JournalStoreException) {
            throw error
        } catch (error: Exception) {
            throw JournalStoreException("journal delete failed", error)
        }
    }

    private fun resolve(fileId: String): File {
        if (!isSafeFileId(fileId)) {
            throw JournalStoreException("invalid journal file id", null)
        }
        return File(directory, fileId)
    }

    private fun ensureDirectory() {
        if (!directory.isDirectory && !directory.mkdirs() && !directory.isDirectory) {
            throw JournalStoreException("journal directory unavailable", null)
        }
    }

    private fun scopeOf(fileName: String): String =
        fileName.removePrefix(FILE_PREFIX).removeSuffix(JournalSchema.FILE_SUFFIX)

    private fun isSafeFileId(fileId: String): Boolean =
        fileId.startsWith(FILE_PREFIX) &&
            fileId.endsWith(JournalSchema.FILE_SUFFIX) &&
            !fileId.contains("..") &&
            fileId.none { it == '/' || it == '\\' }

    private companion object {
        const val FILE_PREFIX = "journal-"
    }
}
