# Conversation transitions

Every state change goes through `ConversationReduce.reduce(state, action)`.
`ConversationModel` only turns user intents and service results into these
actions. This table is the review reference: an action that is not listed here
must not exist.

Dimensions: `lifecycle` (command), `queue`, `wait`, `approval`, `history`,
`provenance`.

## Load and connection

| Action | From | To | Invariant |
| --- | --- | --- | --- |
| `loadStarted` | any | `isLoaded = false`, `loadError = nil` | A retry clears the previous error. |
| `loadFailed(code)` | any | `isLoaded = false`, `loadError = code` | One error dimension for the screen. |
| `snapshot(messages, approvals)` | any | `isLoaded = true`, `loadError = nil`, `connection = .online`, `watchEnded = false` | A pending approval absent from the snapshot becomes `.none`. |
| `watchEnded` | any | `watchEnded = true`, `connection = .offline` | The screen offers reconnect; it never shows stale content as live. |
| `connectionChanged(status)` | any | `connection = status`; `.online` clears `watchEnded` | Connection is a projection of the watch, not a user toggle. |
| `capabilitiesLoaded(caps)` | any | `capabilities = caps`; unsupported queue/history become `unavailable` | A capability is declared once, never probed per view. |

## Command lifecycle

| Action | From | To | Invariant |
| --- | --- | --- | --- |
| `sendSubmitted(cmd)` / `stopSubmitted(cmd)` | `idle`, `delivered`, `rejected` | `submitted(cmd)` | A second submit while in flight is ignored (coalesced, not queued). |
| `sendAccepted(cmd, receipt)` / `stopAccepted` | `submitted(same cmd)` | `accepted(cmd, receipt)` | A result for a replaced command is dropped. |
| `sendDelivered(receipt)` / `stopDelivered` | any | `delivered(receipt)` | Terminal success is idempotent. |
| `sendUncertain(cmd, receipt)` | `submitted(same cmd)` | `uncertain(cmd, receipt)` | Keeps the intent and receipt for recovery; not settled. |
| `sendRejected(cmd, code)` / `stopRejected` | `submitted(same cmd)` | `rejected(cmd, code)` | Terminal failure carries exactly one error code. |

`canSend` requires a loaded screen, capabilities, and a settled lifecycle.
`canStop` requires `running` and interrupt support.

## Queue

| Action | From | To | Invariant |
| --- | --- | --- | --- |
| `queueLoaded(snapshot)` | any | `ready(snapshot)` | An empty ready queue is not "unavailable". |
| `queueUnavailable` | any | `unavailable` | Unsupported read is a state, not an error. |

## Wait

| Action | From | To | Invariant |
| --- | --- | --- | --- |
| `waitStarted(handle)` | `idle` | `active(handle)` | One wait attempt at a time. |
| `waitFinished(outcome)` | `active` | `finished(outcome)` | A finish without a start is ignored; a finished wait never revives. |

## Approval

| Action | From | To | Invariant |
| --- | --- | --- | --- |
| `approvalPresented(a)` | any | `pending(a)` | One card is actionable at a time. |
| `approvalRefreshed(a)` | `pending` | `pending(a)` | A pending-only refresh never resurrects a decided card. |
| `approvalExpired(a)` | `pending` | `stale(a)` | Expiry disables decisions but keeps the card visible. |
| `approvalResolving(a)` | `pending` | `resolving(a)` | A second decision is blocked while resolving. |
| `approvalResolved` | `resolving` | `none` | Success removes the card. |
| `approvalRejected(code)` | `resolving` | `pending(a)` | A rejected decision returns to actionable, not stale. |

## History

| Action | From | To | Invariant |
| --- | --- | --- | --- |
| `historyUnavailable` | any | `unavailable` | No page capability means no history affordance. |
| `historyCachedOnly` | any | `cachedOnly` | Cached content is labelled, never presented as live. |
| `historyLoaded(complete)` | any | `idle(complete:)` | A page establishes whether older content exists. |
| `historyLoadingMore` | `idle` | `loadingMore` | Cannot page from a non-idle history. |
| `historyFailed(code)` | any | `failed(code)` | A failed page keeps the traversal retryable. |

## Content

| Action | From | To | Invariant |
| --- | --- | --- | --- |
| `messageReceived(message, provenance)` | any | message replaced or appended by `messageId`; `provenance` set | A replay is idempotent by committed message id. |
| `messageReplaced(message)` | any | replaced by `messageId` | Updating content never duplicates a row. |
