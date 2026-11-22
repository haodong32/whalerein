# Diagnostics transitions

| Event | From | To | Invariant |
| --- | --- | --- | --- |
| loadStarted | any | loading | A retry clears the previous failure. |
| loadSucceeded | any | loaded | The report drives every read-only section. |
| loadFailed(code) | any | failed(code) | One error for the screen. |
| shareRequested | loaded | confirming | Nothing is written before confirmation. |
| shareStarted | confirming | exporting | Export writes the app-private snapshot. |
| shareExported(url) | exporting | ready(url) + count | The share sheet receives the staging file. |
| shareCancelled | confirming or idle | cancelled | Cancel never writes a file. |
| shareFailed(code) | any | failed(code) | The preview stays available for retry. |
| shareDismissed | any | none | Clears the shared file reference. |
