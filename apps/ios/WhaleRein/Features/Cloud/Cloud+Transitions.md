# Cloud transitions

| Event | From | To | Invariant |
| --- | --- | --- | --- |
| restoreStarted | signedOut | restoring | Restore never interrupts a signed-in session. |
| restored(account, pending) | restoring | signedIn(account) or signedOut | A missing account resets the directory. |
| restoreFailed(code) | restoring | signedOut + error | The user can retry. |
| signInStarted | signedOut | starting | One attempt at a time. |
| authorizing | starting | authorizing + sheet | The system sheet owns the attempt. |
| signInCancelled | starting/authorizing | signedOut + cancelled | Cancel is not an error. |
| signInFailed(code) | starting/authorizing | signedOut + error | Distinct from cancellation. |
| completed(account) | completing | signedIn(account) | Pending sign-out clears. |
| directoryStarted | signedIn | loading | Directory reads never change the account. |
| directoryLoaded(hosts) | loading | empty or loaded | Empty is not unavailable. |
| directoryFailed(code) | loading | unavailable(code) | A retry keeps the account. |
| connectStarted(hostId) | signedIn | connecting | One computer at a time. |
| sessionRevoked | signedIn | signedOut + pending + revoked | Content is cleared; the pending remote sign-out stays visible. |
| signedOut(pending) | signedIn | signedOut + pending | A failed remote revocation is recorded, not hidden. |
