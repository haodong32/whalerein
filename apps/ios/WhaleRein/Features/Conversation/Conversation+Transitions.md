# Conversation transitions

Every state change goes through `ConversationReduce.reduce(state, action)`.
`ConversationModel` only turns user intents and service results into these
actions. This table is the review reference: an action that is not listed here
must not exist.

Dimensions: `lifecycle` (command), `queue`, `wait`, `approval` and `history`.

## Load and connection

| Action | From | To | Invariant |
| --- | --- | --- | --- |
| `loadStarted` | any | `isLoaded = false`, `loadError = nil` | A retry clears the previous error. |
| `loadFailed(code)` | any | `isLoaded = false`, `loadError = code` | One error dimension for the screen. |
| `contentLoaded` | any | `isLoaded = true`, `isLoading = false`, `loadError = nil` | The model has applied the accepted history snapshot. |
| `watchEnded` | any | `watchEnded = true`, `connection = .offline` | The screen offers reconnect; it never shows stale content as live. |
| `connectionChanged(status)` | any | `connection = status`; `.online` clears `watchEnded` | Connection is a projection of the watch, not a user toggle. |
| `capabilitiesLoaded(caps)` | any | `capabilities = caps`; unsupported queue becomes `unavailable` | A capability is declared once, never probed per view. |

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

## History and viewport

| Action | From | To | Invariant |
| --- | --- | --- | --- |
| `olderHistoryStarted` | Ready continuation | Older page in flight | Only one request may hold the same traversal. |
| `olderHistoryFinished` | In-flight traversal | Ready, exhausted or local error | An older read failure keeps visible rows and exposes a local retry. |
| `historyTraversalInvalidated` | Any traversal | Waiting for new source and cut | A generation check discards both a stale success and a stale failure. |

The observation supplies the first bounded snapshot. Its disposition decides
whether to retain previous pages or install a replacement. History readiness is
independent of questions and command feedback. The viewport follows the bottom
only at the latest edge and keeps a measured row offset while reading earlier.
The older edge requests a page on reader movement. When the initial content
cannot scroll, one additional automatic page may fill it before the user gets a
Continue Loading action.

## Content

The conversation model applies committed updates by message ID. Replay and
replacement update the corresponding row. Pending sends retain a separate
identity until Core supplies an authoritative message ID.
