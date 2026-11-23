package com.whalerein.android

import android.content.ContentProvider
import android.content.ContentValues
import android.content.Context
import android.database.Cursor
import android.net.Uri
import android.os.ParcelFileDescriptor
import java.io.File
import java.io.FileNotFoundException

/**
 * Debug-only document provider for instrumentation tests.
 *
 * It lives in the app process and package, so the app's real
 * `ContentResolver.openOutputStream` path can be exercised without the system
 * Files picker. It is compiled only into debug variants and is not exported.
 */
class TestDocumentProvider : ContentProvider() {
    override fun onCreate(): Boolean = true

    override fun openFile(uri: Uri, mode: String): ParcelFileDescriptor {
        if (failWrites && mode.startsWith("w")) throw FileNotFoundException("write denied")
        val file = fileFor(uri)
        file.parentFile?.mkdirs()
        val flags = when {
            mode.startsWith("w") -> ParcelFileDescriptor.MODE_CREATE or
                ParcelFileDescriptor.MODE_TRUNCATE or
                ParcelFileDescriptor.MODE_WRITE_ONLY
            else -> ParcelFileDescriptor.MODE_READ_ONLY
        }
        return ParcelFileDescriptor.open(file, flags)
    }

    override fun query(
        uri: Uri,
        projection: Array<out String>?,
        selection: String?,
        selectionArgs: Array<out String>?,
        sortOrder: String?,
    ): Cursor? = null

    override fun getType(uri: Uri): String = "application/octet-stream"

    override fun insert(uri: Uri, values: ContentValues?): Uri? = null

    override fun delete(uri: Uri, selection: String?, selectionArgs: Array<out String>?): Int = 0

    override fun update(
        uri: Uri,
        values: ContentValues?,
        selection: String?,
        selectionArgs: Array<out String>?,
    ): Int = 0

    private fun fileFor(uri: Uri): File =
        File(directory(requireNotNull(context)), uri.lastPathSegment ?: "document")

    companion object {
        const val AUTHORITY = "com.whalerein.android.test.documents"

        @Volatile
        var failWrites: Boolean = false

        fun directory(context: Context): File = File(context.cacheDir, "test-documents")

        fun reset(context: Context) {
            failWrites = false
            directory(context).deleteRecursively()
        }

        fun fileFor(context: Context, uri: Uri): File =
            File(directory(context), uri.lastPathSegment ?: "document")

        fun read(context: Context, uri: Uri): String =
            fileFor(context, uri).readText(Charsets.UTF_8)
    }
}
