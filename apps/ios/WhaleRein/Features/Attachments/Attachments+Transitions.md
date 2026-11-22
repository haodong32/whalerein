# Attachment transitions

| Event | From | To | Invariant |
| --- | --- | --- | --- |
| startRequested(ref) | idle or failed | preparing | A running transfer is not restarted. |
| downloadStarted(ref, id) | preparing | transferring | One download identity per transfer. |
| progressUpdated(ref, bytes) | transferring | transferring | Progress never decreases the phase. |
| verificationStarted(ref) | transferring | verifying | Only verified files are shared. |
| completed(ref) | verifying | completed | Duplicate terminal events are ignored. |
| failed(ref, code) | any active | failed(code) | One error per transfer. |
| cancelled(ref) | any active | cancelled | Distinct from a failure. |
| exportRecorded(ref, outcome) | completed | completed + outcome | Export requires a verified download. |
| exportCleared(ref) | completed | completed | Clears the share result. |
