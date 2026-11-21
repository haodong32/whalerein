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

On relay epoch 3 the Host publishes complete packet-sized preview messages, not
fragments or acknowledged transfer frames. It takes the smallest usable outbound
budget across its local profile, Access's advertised receive budget and the
Device `ChannelHello.receive` budget, splits only on valid UTF-8 boundaries,
assigns contiguous normalized intervals and rechecks the fully encoded MQTT
PUBLISH before publication. A complete body that does not fit is published as a
reference and fetched over HTTPS. A Device publishes its `ready`
`ReceiveAdvertisement` after each authenticated connect and budget change; Access
publishes a packet-sized `RequestStateNotice` on the Device `notice` topic only
from durably committed state. A lost notice is repaired from authoritative state
and the watch lease refresh, and a notice is never proof that native execution
completed.

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

Reasoning is a checked block kind ordered alongside answer text. Core reports a
separate answer/reasoning preview prefix, so a reasoning prefix never offsets
visible answer text. CLI hides reasoning by default and renders a distinct typed
block only with `--show-reasoning`; default JSON omits hidden reasoning entirely.
A hidden reasoning-only preview or committed message does not satisfy visible
completion (`session watch --once`). This is a presentation choice; it never
changes authorization.

One active session observation per Core instance selects Conversation, Questions
or both over one Watch and one device MQTT client. Question observation is
independent of history readiness: a lost or unsupported history facet does not
hide a readable pending question. The source stamp (observation epoch plus
monotonic revision) drives a private same-stamp reducer; unchanged inventory and
token/runtime traffic do not re-read question content. Only a complete collection
with one stamp replaces the visible view, so mixed, regressed or partial pages
never become Current. The existing five-second refresh repairs a lost final
question hint without a client timer. This avoids competing device MQTT clients
rather than pre-building multi-session multiplexing. A visible preview
or committed Message can finish `tail --once`; lifecycle/state/reset metadata alone
cannot, and a marker-only deadline reports timeout.

## History and recovery

A SessionKey contains Host and session identity. Source cursors are opaque and
bound to the session/view/projection. HistoryBatch starts at the applied cursor
and advances it atomically with committed changes, even across filtered native
records. Duplicate batch identities are idempotent; conflicting canonical content
fails integrity validation. Receipt fingerprints are encrypted at rest and bounded.

HistoryRead opens/paginates a fixed cut. Every page and batch carries the active
content projection; a page from another projection or cut cannot continue the
traversal and requests a fresh snapshot. Snapshot pages stage in encrypted cache
under one projection and replace a complete view only after the final page.
Failed, interrupted or missing-range snapshots leave the old view and its
receipts in place. A new projection fences old positions, receipts, cache rows
and continuations; cached rows stay until an atomic replacement commits.
Replacement and eviction advance the cache generation so stale continuations
fail explicitly. Branch/regression invalidates old positions and provisional
state; it never revokes pairing. Buffered live commits are reconciled once
against the installed cut. Current DSH native reads may still materialize a
source snapshot. Page budgets do not claim to eliminate that native allocation;
incomplete source ranges fail explicitly.

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
a single indivisible Message can exceed that target. MQTT carries one complete
message per PUBLISH under the active profile; a body above the packet budget is
carried as a reference and read as a complete HTTPS ciphertext object. There is
no application fragmentation, reassembly, transfer receipt or device ACK topic.
The logical envelope ceiling remains 16 MiB in the default profile.

The Source owns conversation content; Host SQLite stores operational/identifier
metadata and pending ciphertext, not a second plaintext session log. A persistent
Host instance owns its directory through an OS-released SQLite transaction lock.
Host/device identity, key namespaces, grants and command outcomes survive the
unreleased-v1 replacement. Explicit forget remains the identity deletion action.

## Verification

Unit cases cover source-bound cursors, cut pagination, typed validation, live
coalescing, reasoning visibility and hidden-preview completion, independent
latest-first history/continuation/cache/exact reads, send confirmation with a
later read failure, slow-recipient isolation, pending approvals, encrypted
Host-scoped cache staging, receipt conflicts, conversation reduction, pump
cancellation and human/JSON output. Generated field contracts and Node/Rust vectors cover typed
history, state, Watch baselines, capabilities and live updates alongside security
vectors. Isolated stack cases use real CLI/Core, Access, PostgreSQL, Mosquitto and the
plugin with FakeDSH. Native DSH, OS keyring and deployment acceptance remain
separate gates; fixtures do not certify another Harness or a running profile.

AC-19 is a separate black-box gate. It requires the committed netem harness,
matched same-profile A/A and A/B runs at 0 ms and 280 ms, and a declared A/A
noise band and comparison procedure before candidates are inspected. Its two
primary metrics are the paired P95 longest visible gap and the largest output
share; final-output share is diagnostic, and the same two metrics must be
reported for the paired idle and concurrent bulk-read run with correct once-only
output. The v3 harness, comparator metrics and A/B artifact pinning are not
committed in this revision, so AC-19 remains blocked and no timing claim is made
from the existing pilot or unit evidence.
