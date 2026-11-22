package com.whalerein.android.state.support

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome

/** Writes a confirmed export through the platform document picker. */
interface JournalExporter {
    suspend fun write(fileName: String, jsonl: ByteArray): Outcome<ExportWriteResult>
}

sealed interface ExportWriteResult {
    data class Completed(val fileName: String) : ExportWriteResult

    /** The provider may have accepted the bytes; completion is unknown. */
    data class Unconfirmed(val failure: CoreFailure) : ExportWriteResult
}

/**
 * Reads and clears Core's separate last-failure summary. Its data is optional
 * export metadata only; it must never become a second timeline row.
 */
interface CoreFailureSummaryPort {
    suspend fun lastFailure(instance: InstanceId): Outcome<CoreFailureSummary?>

    suspend fun clear(instance: InstanceId): Outcome<Unit>
}
