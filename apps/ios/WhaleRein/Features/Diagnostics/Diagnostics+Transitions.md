# Diagnostics transitions

| Event | From | To | Invariant |
| --- | --- | --- | --- |
| loadStarted | any | loading, share none | A new explicit capture invalidates any prepared file and increments `captureGeneration`. |
| loadSucceeded | loading | loaded | The captured document drives the summary, the full report and share. |
| loadFailed(code) | loading | failed(code) | No capture is retained and no stale share action remains. |
| shareStarted | loaded, share allows a new share | staging | One explicit tap; nothing is written before it. |
| shareStaged(url) | staging | presented(url) + count | Exactly one system sheet is presenting the protected copy. |
| shareFailed(code) | staging | failed(code) | A write failure opens no picker; the capture stays inspectable. |
| shareFinished(completed) | presented | finished(completed) | The temporary copy is deleted; status distinguishes destination from cancel. |
| shareDismissed | presented | none | Teardown of a sheet without a completion callback deletes the temporary copy. |
