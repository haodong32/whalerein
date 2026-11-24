# WhaleRein Core Architecture

[Overview](../architecture.md) · [CLI](cli.md) · [Contract](harness-contract.md)

Core is the self-contained Rust client endpoint. Clients provide a `SecretStore`
and can register a `DiagnosticSink`. Harness adapters run on Hosts.

Consumers use `api`, the normalized `conversation` types, and the independent
`history` module. Transport, cache, security, pairing, command, synchronization
and runtime modules are private.
`api` re-exports the supported secret stores, diagnostic events, connection state
and metric types. Repository-only protocol
drivers opt into `protocol-testing` and use the explicit `testing` exports.

Client-facing values are Core-owned. Conversation messages, content blocks,
attempt references and watch state are Core types exported by `conversation`;
the versioned wire messages are decoded inside `access`, `cache` and `sync` and
converted at those boundaries, so consumers never construct protocol types. A
session operation takes an explicit `SessionKey(host_id, session_id)`; there is
no ambient Host scope on a `Core`.
Callers resolve a key once with `operation_session_key`, passing an explicit
Host where provided. Without one, Core uses the selected ready Host or a single
eligible Host from a complete listing. Callers pass the resolved key to each
session operation. An operation re-checks that the key's Host is still locally
authorized before any I/O, so a stale or substituted key fails closed rather
than routing a request to another Host. Delivery choices and question phases are
Core enums; the versioned wire enums are converted at the protocol boundary.

| Module | Responsibility |
| --- | --- |
| `api` | Pairing, commands and the typed session-observation interface; one overall deadline. |
| `observation` | One private observation owner: profile lease, transport pump, independent Conversation and Questions reconciliation. |
| `runtime` | Channels, connection state and reconnect policy. |
| `security`, `pairing` | Persistent device identity, Noise and authenticated payloads. |
| `commands` | Idempotent submission/result recovery and command projection writes. |
| `sync` | Typed payload/scope validation and committed-view/cursor writes. |
| `conversation` | Bounded provisional Attempts, typed blocks, one tool activity per call, order, resets and final-message precedence. |
| `history` | Independent bounded committed-history pages, exact message references, source/cache selection and projection fencing. |
| `cache` | Encrypted Host-scoped messages, summaries, snapshot staging and command metadata. |
| `access` | HTTPS for the durable command lane, plus the single device MQTT connection with the MQTT 5 query lane, the complete-message inline/reference control relay and direct QoS 0 live preview reception. |
| `diagnostics` | Redacted live events, bounded operation history and exports. |
| `account` | Cloud App sessions, verified account scope, regional permission renewal, and durable sign-out retries. |
| `api/instance.rs` | Explicit client TOML, instance manifests, scoped trust, and SecretStore namespaces. |

`api/core.rs` owns the client facade. Its unit tests live in `api/core/`,
while `observation/` owns session observation and `api/watch.rs` keeps only the
shared key/history helpers and the thin CLI-facing adapter used by existing
callers. Observation tests live with the module and in `api/tail_tests.rs`. Facade tests are grouped by cache, command recovery, pairing,
diagnostics and approval/revocation behavior, with shared setup in `test_support.rs`.

### Client instance

CLI and mobile each select a configuration path and open Core with
`Core::open(path, secret_store)`. CLI defaults to
`~/.whalerein/config.toml`. A native mobile client can create or update
its sandbox file through `Core::write_config`. Core validates only the file
it receives. Both platforms use the same versioned TOML contract.

```toml
version = 1
service = "whalerein_cloud"
data_dir = "./cloud"

# Optional; omitted means the packaged "default" profile.
[transport]
profile = "default" # default | compact | roomy
# Local overrides are validated with the same relations as Access/Host.
# [transport.overrides]
# envelope_bytes = 16777216
```

The Cloud service defaults to official production. Developers can select
`cloud_environment = "local"` or `"staging"`. Cloud API and issuer
origins use the baked-in service catalog. The local environment requires the
platform's mkcert root. Standalone uses `service = "standalone"` and learns
the regional endpoint and Host identity during invitation pairing. A
Standalone configuration can select a PEM CA through
`standalone_ca_certificate`. Core resolves relative data and CA paths against
the selected configuration file's directory.

An open validates the file and any existing instance manifest without creating
state. Login and connection initialize a fresh data directory when needed.
The manifest binds a random instance ID to the service authority. The runtime
stores regional configuration, Host pins, cached data and operations in the
instance's `state/` directory. Device identity, transport credentials, cache
keys, native account tokens and sensitive handshake material use
instance-scoped SecretStore keys. Different directories create independent
instances. A directory containing pre-cutover state or incompatible authority
requires a fresh `data_dir`. Settle outstanding operations with the preceding
client before switching.

### Cloud account and pairing

Cloud owns native App sessions and the provider device authorization exchange.
Core starts the exchange at the configured Cloud API, verifies that the browser
URL belongs to the configured issuer, and polls with a separate attempt
credential. The account response identifies the issuer, subject and workspace.
Core checks this binding against the instance manifest whenever it uses the
account. Opaque credentials stay in SecretStore, with local mutations serialized
through an OS-released instance lock.

Core uses the live Cloud session to obtain renewable regional permission. It
proves possession of the existing Device key for issuance and uses a distinct
regional credential for each App session. HTTP and MQTT use that credential
alongside the existing Host grant and pin. A regional outage may interrupt an
eligible connection while the Cloud login stays valid. Cached permission works
through its current deadline. Route changes require the selected authority to
confirm a matching Host grant and fingerprint.

Core persists sign-out and its session-scoped retry credential in shared
instance state before contacting Cloud. Running processes check the current
session before sending requests or delivering live observation events.
A later login can use the existing Noise identity and eligible Host grants with
its new regional credential. Standalone continues to use its invitation-created
Device credential and local authorization.

Cloud pairing records a recoverable operation per Host before requesting a
signed authorization. The Noise peer must match the authorized Host key, and
Core saves the pinned Host only after Access confirms activation. Sensitive
material stays in SecretStore so recovery can reconstruct the original HELLO
or repeat the exact FINISH. An operation begun under an earlier Cloud session
is reconciled through Access using current authorization. Connecting to
another Host remains available while an earlier operation is unresolved.

### Session keys

A SessionKey is `(host_id, session_id)`. Resolve an unqualified session ID
against the current authorized Host set. With multiple Hosts, every source
session list must be available and complete; multiple matches are ambiguous,
and an unavailable list cannot prove uniqueness. An explicit Host is checked
against current authorization. Each independently paired Host has its own
confirmed fingerprint; discovery cannot replace a pinned identity.
Source history cursors are opaque strings;
Core checks continuity and never interprets DSH sequence numbers or native JSON.
Committed changes and cursors update atomically. Snapshot pages stage separately
and replace a complete view only on successful completion. Source branch/reset
failures request a new snapshot without deleting pairing or the previous cache.

Session listings have one order for remote and cache-only views: most recently
updated first, then session identity, so an offline list does not jump when the
Host reconnects. A complete remote listing replaces the cached rows for that
Host, and deleted sessions disappear from the offline view. Archived sessions
keep their row and the source's `archived` flag, so an archived view stays
available offline; clients hide those rows by default. An
explicit `sessions --host` accepts only a locally authorized Host identity; an
unknown or unpinned Host is `not_found`, never a reason to pair again. That Host
argument describes the listing; it is not an ambient scope that changes other
operations, which take the key resolved for them.

One `observe_session` call selects Conversation, Questions or both on the same
device transport owner and delivers producer-scoped `SessionEvent`s. Conversation
covers committed history, live Attempts, runtime and Approvals. Questions is an
independent facet: a questions-only observation does not read history, require a
history head, or fail Conversation when its read is unsupported. A question
descriptor carries a source observation epoch and monotonic revision; Core reads
authoritative pages only when that stamp changes and installs a collection only
when every page certifies the same complete stamp. Stale, Unavailable,
Unsupported, Loading and Current-empty remain distinct, and observing or listing
never acquires a native answer claim.
Watching subscribes MQTT before registering a source Watch and synchronizing its
fixed history cut. A bounded background pump continues processing control and
query traffic while HTTPS object reads run. Channel/watch replacement rejects
old updates. The observation is bound to the authenticated Broker connection
reference and observation generation, not to a renewable lease. Every five
seconds Core issues one read-only `CheckVersions{session_id}` query per actively
observed Host, with at most one check in flight; Host coalesces overlapping
checks by session and facet and returns `VersionChecks`. A lost final
notification is therefore repaired from authoritative markers without
subsequent event traffic, and the check never extends the observation.
A cold-cache snapshot is committed locally without being reported as new output;
only changes after the synchronized cut and later live updates are delivered.
A one-shot `session watch` forces a fresh snapshot and reports the complete
committed count even when the cursor is current.
A reconnect or a `BatchOutcome::Gap` recovers the missed committed messages and
delivers each unseen message identity exactly once, deduplicated against what
this viewer already printed. The same dedupe prevents a replayed batch or a
snapshot that overlaps live output from printing a message twice.
One client instance owns one active observation. The observation is bound to
one authenticated connection generation. A competing watch fails with
`conflict` while send and wait remain available. CLI processes using the same
Device can hold independent Host channels, so a send and a live watch can
coexist on the same connection owner.
Closing or replacing an observation stops its recovery,
staging and scoped delivery while a separate answer command can continue.

### Independent history reads

`history_page` reads one latest-first bounded page through the same `HistoryRead`
request and validation used by synchronization. It creates no watch lease, MQTT
subscription, `SyncService` state or cache write. The first response fixes a
Source cut; Core binds the validated Host/session scope, projection, cut, next
position and origin into an opaque continuation. Later pages keep that scope and
cut, and an expired, changed or unavailable history returns `snapshot_required`
instead of restarting silently. Messages are chronological within the page and
`complete` means the oldest authorized message was reached. `HistoryMode::Cache`
reads the eligible encrypted local view explicitly under the same projection and
visibility fence; replacement or eviction invalidates its continuation.

`history_message` resolves one scoped `MessageReference` through an exact
selection and validates its session, projection and canonical message identity.
A stale reference, changed projection or unavailable message is an explicit
failure. The same module owns both paths so live commits, pages and exact reads
share authorization, identity checks and canonical decoding.

### Confirmed send follow-up

`follow_send_operation` keeps the durable operation association, deadline and
polling while returning only proof-backed state: `Pending`, `Committed` with a
canonical `MessageReference`, `InputRequired` with evidenced `InputKind` values,
or `Uncertain`. Message bodies remain in history. The DSH adapter proves that a
settled reply belongs to the original submission and derives the reference from
the same committed projection used for live and paged history; ambiguous,
missing or late unrelated evidence stays pending or uncertain. Pending input is
reported only for the owning active operation and reuses wait-state
classification.

`follow_send_operation_with_content` additionally reads the confirmed reference
through `history_message` inside the remaining deadline. A failed read keeps the
`Committed` reply intact and returns a stable redacted `read_error`; callers
must not resubmit because content was unavailable. Core records distinct
`send_reply_confirmed`, `send_input` and `send_reply_read` stages with no message
bodies.

Interrupt follows an accepted stop to its terminal receipt, as send does. An
accepted stop without terminal confirmation is reported as unknown, and a
terminal failure as an error; neither claims that the Agent stopped. An
interrupted Attempt end is visible to a one-shot session watcher. Fork
reports the confirmed child working directory, or an explicit reason why that
second read failed, while keeping the successful fork identity.

Core delivers typed ViewUpdates to consumers. Only committed changes enter the
cache; provisional text never advances a history cursor. The conversation reducer
owns sequence filtering, bounded completed-Attempt tracking, final precedence,
checked block kinds and separate answer/reasoning preview prefixes. Clients do
not implement protocol sequencing again. `ViewUpdate::ToolActivity` carries the
single reducer activity per call, and committed tool occurrences absorb through
the same reducer for pages and live views.

Core also owns verified, cancellable attachment download from a scoped
occurrence reference once the Source provides it. The CLI download command
surface is reserved for that path; in this build it reports the capability as
unavailable and reads no bytes.

The client `[transport]` profile is loaded from the packaged generated Rust
artifact and validated before any connection use: the complete MQTT packet
budget, logical envelope, aggregate reservations, timeout ordering and the
minimum control message must all fit. Invalid relations fail `Core::open`.
Changes take effect on restart. The relay uses one coordinated epoch
(`relay_version = 5`): every received `EventEnvelope`, direct live preview,
query reply and `RequestStateNotice` must advertise it, and a peer with any
other epoch is rejected with `protocol_version_incompatible`. Core subscribes to
its control event and notice topics at QoS 1 and to its own Device live topic
`.../d/{device_id}/live` at QoS 0. A live preview must carry inline ciphertext
at epoch 5; a reference, missing nonce or foreign epoch is rejected. Live
frames feed the same observation pipeline and sequence/duplicate path as control
events, with no acknowledgement, retransmission or control blocking: a gap or
loss marks only the preview incomplete, and the committed message or an
authoritative HTTPS read repairs the view. An inline ciphertext or sealed body
is used directly. A control `*_reference` is fetched with the Device bearer from
the Access HTTPS directory and its exact reserved length and SHA-256 are
verified before AEAD or business handling; a failed, truncated, longer or
corrupt fetch is terminal for that message and never a completed result.
`ChannelHello` advertises the device
receive budget with its own monotonic channel generation, and the Device
publishes the same advertisement on its `.../ready` topic after every successful
broker connect. `ChannelReady` must name a known packaged profile and carry a
relation-valid budget; a valid override of that profile is accepted without
clamping, and the profile remains the base label. Outbound requests use the
lower of the local and peer envelope budgets; a sealed command body above the
effective inline request budget is reserved, uploaded and finished on Access
before the same request identity is submitted with `body_reference` and a
matching `payload_digest`. `result_max_bytes` is bound to the request and every
stored result is read under that exact bound. No content is written to transport
diagnostics.

Within one Core instance there is exactly one MQTT connection owner per Host. It
carries command notices, control events, direct previews and query replies for
every consumer of that Host, including a foreground command wait; it no longer
opens an auxiliary foreground receiver. The owner subscribes to
`.../d/{device_id}/reply` before publishing a query and keeps a waiter registry
keyed by request id and connection generation. A reply for another request,
device, Host or generation never completes a waiter. Reconnect fails pending
waiters with a retryable transport error, and a safe read is reissued within the
remaining caller deadline. Post-pairing Noise now runs on this query lane rather
than an HTTPS handshake. The command lane keeps HTTPS submission, exact
heartbeat and authoritative result read, and its notice handling stays on the
same owner.

The initial device transport has one active watch per Core instance. Competing
calls fail explicitly rather than opening clients with the same MQTT identity.
Timeout/callback cancellation releases admission and aborts the pump; normal
completion attempts bounded Unwatch cleanup, and connection close, ordered
disconnect or snapshot reconciliation bounds abandoned observations.
Multi-session MQTT multiplexing is deferred until required by a real consumer.

A client installation identity is the per-instance id plus its Noise static
keypair. It is created lazily on first pairing and persisted before any network
request; grants, transport tokens and reconnects do not change it. Revocation
clears authorization while preserving the identity; explicit forget removes it.
There is no silent key rotation. An explicit client rotation first revokes
every Host grant, then asks the account service to replace the key while
preserving the client id, and finally stores the new key locally. The client
must re-pair every Host. Resetting without rotation creates a new client id and
keypair. The account service registers the installation on first pairing and
can revoke every Host grant it owns without changing any Host identity.

Queries may retry after channel loss. Controls keep their original command IDs
and use result recovery instead of blind execution retries. Confirmed revocation
clears authorization/cache material but preserves device identity. Explicit forget
removes identity. A successful `revoke()` returns `RevocationOutcome` with the
device ID and an explicit `Recorded` or `Unverified` status for retained
unconfirmed command or pairing identities. A confirmed passive revocation with unverified
retention returns `RevokedRetentionUnverified` (wire code `revoked`). A missing
cache can mean either no cache was ever created or its identities were lost;
Core does not infer which. The snapshot uses the same initialized SQLite
connection only after checking profile provenance; an unavailable or unproven
connection is not read. Token and cache-key entries are removed from SecretStore
before waiting for transient database contention; an already-live Cache may keep
its in-memory key copy until its references are released. Transport, source and
decoding errors are not grant revocation.

Windows cleanup must not initialize a missing cache to infer retention. A cold
existing cache is opened read-only without migration before remote revocation;
its SQLite native handle, the pinned profile and the singly linked database
entry must match their volume and 128-bit file identifiers. Missing or unproven
data retains an unverified marker. Reparse points and unexpected SQLite sidecars
are rejected. Cache reads on Windows check the same confirmed revocation fence
as Unix. An obsolete connection releases its handle without discarding the
durable journal; authoritative cleanup closes the database before unlinking it.
Failure to retain a pending pairing identity cannot be reported as a saved
snapshot merely because the command snapshot succeeded.

Confirmed global revocation removes any retained pairing FINISH credential after
saving only its redacted identity. The instance retains an opaque revocation
epoch after transient cleanup markers are removed. Pairing captures this epoch
before HELLO and checks it under admission both before retaining FINISH and before
installing credentials; a late result from a prior epoch cannot overwrite a newer
pairing.
Before sending pairing FINISH, Core stores the exact confirmed request and Host
pin in SecretStore. Its locally generated random transport credential is bound by
hash into the Noise payload. `operation recover` reuses the original request after
response loss or restart, checks the endpoint, Host pin and Device key, and
commits the verified regional credentials. Core removes the pending material
after the receipt and selected Host are durable. Explicit forget also removes
the pending material.

Pairing persists the Noise-authenticated Host fingerprint. Fresh channels check
that pin after Noise message 2 and before sending the device's message 3. A key
mismatch keeps pairing, credentials and recovery metadata intact; it never implies
revocation. Missing or changed pins require verification before Core opens a
channel.

Before the first control HTTP submission, Core durably records its Access URL,
Host/device, confirmed Host fingerprint, request ID and command ID as an uncertain
intent, without plaintext command content. Retries reuse the same encrypted request
and IDs. Only a failure proven
to precede every HTTP send removes that intent; lost receipts, cancellation and
process restart retain it. `pending_commands()` exposes recovery identities and
`recover_pending_command(request_id)` reads the outcome over a fresh authenticated
channel without submitting another control. Recovery refuses a different device
or Access endpoint/Host fingerprint and rejects mismatched request identities.

Pairing creates the cache encryption key under the instance identity lock
before publishing regional state. SQLite schema inspection and updates run in
one write transaction so concurrent Core processes serialize initialization.

A foreground send attaches its scoped MQTT receiver before submission, then
waits for state changes instead of polling status. It reconciles the original
request once on attach and on reconnect, reads the complete result over HTTPS
only on a matching notice, and reserves one final authoritative lookup before an
authorized wait would end unconfirmed; no repeating status loop runs while the
receiver stays connected. An exact-request heartbeat renews only that request
while the wait is live, stops on completion, cancellation or a terminal
response, and never treats a result read as renewal. An accepted receipt is not
confirmed execution.

Derived development cache schemas can be rebuilt; credentials and command outcome
metadata are retained. OS keyring, TLS and real-source behavior require their own
release acceptance; isolated fixture evidence is not a deployment claim.

Cache schema version 3 adds projection identity to conversation views, staged
views, committed messages, staged messages and history receipts, plus a
per-cache install ID, a monotonic cache generation and per-view generation.
The migration is additive: existing version-2 rows stay in place with an empty
projection identity and command recovery, session summaries, hosts and pairing
stay untouched. An empty or changed projection is fenced on every read,
including offline reads, until a complete replacement installs the active
projection. A cache with no installation for a session returns a finite empty
cache page; a continuation that refers to a replaced or evicted installation
fails with `snapshot_required`.

Conversation message/staging payloads have a 128 MiB logical cache budget. Cache
writes evict the oldest stored complete views with their cursors and receipts,
never individual messages beneath a retained cursor. Replacement and eviction
bump the cache generation so an old continuation cannot walk a new namespace. The view being written is
protected; an oversized replacement rolls back, discards its staging and reports
`payload_too_large`, retaining the previous committed view. Opening a cache also
trims old views. This is a retained-payload budget, not a physical SQLite file-size
limit; freed pages are reused. Session summaries and durable command recovery
metadata are separate and are not discarded by history eviction. Offline cached
messages may therefore be unavailable for an evicted view; online access reloads
an authoritative snapshot.

Conversation drafts are an additive cache table scoped by explicit
`(host_id, session_id)`. Draft text is sealed with the profile cache key and
AEAD associated data binding the schema and both identities before it reaches
SQLite; the database and `SecretStore` never hold draft plaintext. Empty text
deletes the row, the accepted size is bounded at 256 KiB, and the profile must
be paired. Confirmed revocation and explicit forget delete the cache file, so
they remove drafts and their key with the rest of the local conversation store.

`Core::open_with_diagnostic_sink` registers a receiver for one client instance.
Callers can reserve the trace ID for initialization events with `DiagnosticContext`
and pass that context to `Core::open_with_diagnostics`.
`Diagnostics::record` assigns the time, trace ID and severity. The same event
feeds the sink and, for operation stages, the bounded failure history. The
sink receives an immutable event on the producing thread and may be called
concurrently. The caller controls delivery and formatting.

`diagnostics_preview()` renders the same local, redacted evidence as
`export_diagnostics` without writing a file or opening a channel. It keeps the
existing policy of redacting Host identities to ordinal labels and exposes only
the last failure code, while including schema/version, the Access address class
already present in doctor output, command counts, connection state and metrics.
Message content, tool parameters, drafts, credentials, invitation tokens and
private keys are excluded.


Use a fresh instance for the cutover. Its identity, pins and operation history
belong to that instance. Preserve the previous data directory and credentials
for the preceding release, and recover uncertain operations with the release
that submitted them.

A cache is rebuildable local state; pairing lives in configuration and the
`SecretStore`. A missing or replaced cache key and a non-SQLite cache file are
quarantined and rebuilt automatically without changing the paired identity. A
busy cache or a cache written by a newer schema is refused with `cache_error`
instead of being destroyed, so a downgrade cannot silently discard newer
storage. The last recovery reason is recorded in `cache-recovery.json` and is
readable through `Core::cache_recovery_reason` without opening the cache.
