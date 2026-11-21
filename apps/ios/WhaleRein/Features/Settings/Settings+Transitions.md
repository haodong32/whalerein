# Settings transitions

| Event | From | To | Invariant |
| --- | --- | --- | --- |
| confirmationRequested | not performing | confirming(action) | A second request while performing is ignored. |
| confirmationCancelled | confirming | idle | Only the dialog closes; nothing is exported or cleared. |
| performStarted | confirming(same action) | performing(action) | A different action cannot start. |
| performSucceeded | performing(same action) | finished(action) + status | Success is terminal until the shell resets. |
| performFailed | performing | idle + errorCode | One error dimension; the action can be retried. |
| logExportStarted | idle | preparing | Re-entry while preparing is ignored. |
| logExportReady(url) | preparing | ready(url) | The staging file replaces the previous snapshot. |
| logExportFailed | preparing | failed | The user can retry; nothing was uploaded. |
| logExportDismissed | any | idle | Only clears the local export state. |
