# Conversation streaming and history

[Contract](architecture/harness-contract.md) · [Core](architecture/core.md) ·
[DSH adapter](architecture/dsh-plugin.md) · [Wire](../proto/v1/WIRE.md)

Application protocol remains v1. The current implementation forwards typed
conversation state, not raw native event JSON. DSH is the only current connector;
other Harness implementations use the SourceAdapter seam in the shared Host.

## Ownership

The DSH adapter interprets native committed events and assistant-stream frames.
It emits stable Messages, opaque history positions and normalized live Attempt
updates. Native sequence arithmetic, payload shapes and error text stay there.
The Host owns watches, recipient selection, batching, bounded publication and
cryptography. Access relays opaque envelopes. Rust Core sync owns committed cache
writes, while its conversation reducer owns provisional state and final precedence.
CLI formats typed ViewUpdates and terminal line boundaries only.

## Live versus committed data

A LiveFrame identifies an epoch, Attempt, block and inclusive normalized sequence
interval. Started/delta/reset/ended are explicit oneof payloads. Deltas append
visible text; reset replaces a provisional block. Adjacent deltas can coalesce for
20 ms or an 8 KiB UTF-8 target without crossing identity/block/order boundaries.
Provider internal chunks do not create gaps in this normalized sequence.

A committed Message has a stable identity, role and ordered typed blocks. When
it settles a live Attempt, MessageUpsert carries the origin reference. That is the
single authoritative content/Attempt association. AttemptEnded reports outcome
without a redundant message-ID mapping. Source-restored history can omit old live
references after their epoch is gone.

Core never persists preview text or advances committed cursors with it. Duplicate
or stale live frames are filtered; gaps/reset/memory pressure mark the provisional
view incomplete. Final content wins, including when it arrives before the preview.
Bounded completed-Attempt metadata prevents late chunks reopening settled output.
CLI prints a suffix only when Core reports an exact UTF-8 prefix; otherwise it
shows the complete corrected Message. GUI consumers replace content by identity.
JSON output contains normalized block types and correlation, not native paths.

The current Core admits one active Watch per instance. It avoids competing device
MQTT clients rather than pre-building multi-session multiplexing. A visible preview
or committed Message can finish `tail --once`; lifecycle/state/reset metadata alone
cannot, and a marker-only deadline reports timeout.

## History and recovery

A SessionKey contains Host and session identity. Source cursors are opaque and
bound to the session/view/projection. HistoryBatch starts at the applied cursor
and advances it atomically with committed changes, even across filtered native
records. Duplicate batch identities are idempotent; conflicting canonical content
fails integrity validation. Receipt fingerprints are encrypted at rest and bounded.

HistoryRead opens/paginates a fixed cut. Snapshot pages stage in encrypted cache
and replace a complete view only after the final page. Failed/partial snapshots
leave the old view available. Branch/regression invalidates old positions and
provisional state; it never revokes pairing. Current DSH native reads may still
materialize a source snapshot. Page budgets do not claim to eliminate that native
allocation; incomplete source ranges fail explicitly.

Core subscribes MQTT before WatchSession and synchronization. A bounded background
pump continues handling receipts while HTTP queries execute. Typed updates wait
until the baseline is installed and reconcile against its cursor. The pump has
RAII cancellation; deadlines/callback stops cannot leave a detached subscription.

RefreshWatches every five seconds reconciles the authoritative source head and
current runtime/approval state, even if the last push notification was lost.
Runtime/approval revisions suppress stale state. Unwatch is attempted on normal
completion; channel cleanup or the 30-second lease bounds abandoned watches.
Preview gaps do not request historical token replay; the authoritative Message
repairs them. Approval decisions revalidate the real pending action at execution.

## Resource and identity policy

Per-recipient live and committed lanes are independent. One stalled receiver does
not block another. Preview admission is bounded to 64 publications / 256 KiB, with
a one-second queue-age target. History pages target 256 KiB plus a count cap;
a single indivisible Message can exceed that target. MQTT transparently fragments
complete encrypted envelopes into 60 KiB parts under a 64 KiB physical budget;
its logical envelope ceiling remains 16 MiB.

The Source owns conversation content; Host SQLite stores operational/identifier
metadata and pending ciphertext, not a second plaintext session log. A persistent
Host instance owns its directory through an OS-released SQLite transaction lock.
Host/device identity, key namespaces, grants and command outcomes survive the
unreleased-v1 replacement. Explicit forget remains the identity deletion action.

## Verification

Unit cases cover source-bound cursors, cut pagination, typed validation, live
coalescing, slow-recipient isolation, pending approvals, encrypted Host-scoped
cache staging, receipt conflicts, conversation reduction, pump cancellation and
human/JSON output. Generated field contracts and Node/Rust vectors cover typed
history, state, Watch baselines, capabilities and live updates alongside security
vectors. Isolated stack cases use real CLI/Core, Access, PostgreSQL, Mosquitto and the
plugin with FakeDSH. Native DSH, OS keyring and deployment acceptance remain
separate gates; fixtures do not certify another Harness or a running profile.
