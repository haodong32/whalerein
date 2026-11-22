# Pairing transitions

| Event | From | To | Invariant |
| --- | --- | --- | --- |
| inputChanged | any | editing (clears hints) | Editing clears the previous parse error. |
| pastedNothing | any | hint only | Empty paste never reaches the service. |
| parseFailed(error) | any | editing + parseError | Local validation runs before the service. |
| linkQueued | busy or prompt | hint only | A deep link during a step is queued, not dropped. |
| submitStarted | not busy | busy + validating | A second submit is ignored. |
| submitSucceeded(prompt) | validating | awaitingConfirmation | The fingerprint is shown before any decision. |
| submitFailed(code) | validating | failed(code) | One error; resume stays available. |
| confirmStarted | awaitingConfirmation | busy + confirming | One decision at a time. |
| approved(hostId) | confirming | paired(hostId) | Pairing completes only after the service confirms. |
| declined | awaitingConfirmation | declined | The link is not reused. |
| cancelled | any | editing | Cancelling clears the error but keeps the text. |
| reset | any | initial | A fresh invitation flow. |
