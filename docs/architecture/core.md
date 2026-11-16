# WhaleRein Core Architecture

[Overview](../architecture.md) · [CLI](cli.md) · [Contract](harness-contract.md)

Core is the self-contained Rust client endpoint. SecretStore is its only external
injection interface; Harness adapters run on Hosts and never enter Core.

Consumers use `api` and the normalized `conversation` types. Transport, cache,
security, pairing, command, synchronization and runtime modules are private.
`api` re-exports the supported secret stores, connection state and metric types;
clients do not construct internal protocol services. Repository-only protocol
drivers opt into `protocol-testing` and use the explicit `testing` exports.

Client-facing values are Core-owned. Conversation messages, content blocks,
attempt references and watch state are Core types exported by `conversation`;
the versioned wire messages are decoded inside `access`, `cache` and `sync` and
converted at those boundaries, so consumers never construct protocol types. A
session operation takes an explicit `SessionKey(host_id, session_id)`; there is
no ambient Host scope on a `Core`.
Callers resolve a key once with `session_key` (unqualified, rejected when the
authorized listing cannot prove uniqueness) or `scoped_session_key` (explicit
Host, checked against the current Access listing), then pass that value to every
session operation. An operation re-checks that the key's Host is still locally
authorized before any I/O, so a stale or substituted key fails closed rather
than routing a request to another Host. Delivery choices and question phases are
Core enums; the versioned wire enums are converted at the protocol boundary.

| Module | Responsibility |
| --- | --- |
| `api` | Pairing, commands and typed watch orchestration; one overall deadline. |
| `runtime` | Channels, connection state and reconnect policy. |
| `security`, `pairing` | Persistent device identity, Noise and authenticated payloads. |
| `commands` | Idempotent submission/result recovery and command projection writes. |
| `sync` | Typed payload/scope validation and committed-view/cursor writes. |
| `conversation` | Bounded provisional Attempts, order, resets and final-message precedence. |
| `cache` | Encrypted Host-scoped messages, summaries, snapshot staging and command metadata. |
| `access` | HTTPS and device MQTT, including opaque fragmentation. |
| `diagnostics` | Bounded redacted operation history and exports. |
| `profiles` | Local profile registry, selection and cross-process activation. |
| `account` | Validated account-service descriptors and opaque account binding. |

`api/core.rs` owns the client facade. Its unit tests live in `api/core/`,
while watch orchestration and its focused tests remain in `api/watch.rs` and
`api/tail_tests.rs`. Facade tests are grouped by cache, command recovery, pairing,
diagnostics and approval/revocation behavior, with shared setup in `test_support.rs`.

### Profiles and activation

A profile is one local state directory with one device identity and one service
authority. Standalone profiles keep the historical flat layout in place. Managed
profiles live under `profiles/<opaque-id>/`, so moving or renaming a profile
directory is unsupported: the device identity secret is keyed by the canonical
profile path, and cache provenance binds the database to that directory.

`profiles.json` holds non-secret metadata: schema, revision, activation
generation, the active profile, and each profile's name, mode, service
description and optional opaque account binding. It is installed with an
atomic temp-and-rename write after the previous write is durable. A corrupt,
unknown-schema or future registry is a hard configuration error; Core never
resets it. The legacy layout is recognized by the existing config, cache,
revocation, pending and lock artifacts, and by the profile-scoped identity
secret; it stays at the data-directory root and is never reinterpreted as
account-managed.

A managed profile whose state directory is missing, symlinked or not a plain
directory is a hard configuration error. Core refuses to recreate it, because a
replacement directory would silently mint a new device identity and lose the
existing pins, cache provenance and pending recovery. Legacy state is likewise
kept in place and never moved.

A fresh directory has no profile. Its ordinary account entry selects official
production, but invitation pairing remains available and Core constructs no
account client. Account descriptors are metadata only until the feasibility
gates pass; official presets cannot override endpoints, and a custom descriptor
must provide distinct HTTPS (or loopback test) API, issuer and optional Access
origins plus a public native client identifier.

Every open `Core` holds a shared OS activation lock. The shared lease is acquired
before the active selection is resolved, so an open either observes a switch that
already completed or holds the lease that prevents a later one; it never opens a
selection the lease does not cover. A profile switch takes the exclusive form and
fails with `conflict` while any client process is running, so a long watch or
in-flight operation stops old traffic before the new selection is published. The
lock is released on crash; there is no daemon or owner record. A switch preserves
pending journals and never deletes local profile state. An older binary ignores
the registry, so managed selections report a rollback warning and downgrade
requires stopping those clients.

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
Host, and deleted or archived sessions disappear from the offline view. An
explicit `sessions --host` accepts only a locally authorized Host identity; an
unknown or unpinned Host is `not_found`, never a reason to pair again. That Host
argument describes the listing; it is not an ambient scope that changes other
operations, which take the key resolved for them.

Watching subscribes MQTT before registering a source Watch and synchronizing its
fixed history cut. A bounded background pump continues processing receipts during
HTTP history reads. Channel/watch replacement rejects old updates. RefreshWatch
reconciles the source history head and current state every five seconds; a lost
final notification can therefore recover without subsequent event traffic.
A cold-cache snapshot is committed locally without being reported as new output;
only changes after the synchronized cut and later live updates are delivered.
An explicit `sync` remains the way to read and count committed history, so
`tail --once` does not exit on the oldest replayed message; it forces a fresh
snapshot and reports the complete committed count even when the cursor is
current.
A reconnect or a `BatchOutcome::Gap` recovers the missed committed messages and
delivers each unseen message identity exactly once, deduplicated against what
this viewer already printed. The same dedupe prevents a replayed batch or a
snapshot that overlaps live output from printing a message twice.
One device profile owns one active watch. The watch holds an OS-released profile
lease, so a second `tail` fails deterministically with `conflict` instead of
silently replacing the first subscription; `send`, `wait` and other commands do
not take that lease.

Interrupt follows an accepted stop to its terminal receipt, as send does. An
accepted stop without terminal confirmation is reported as unknown, and a
terminal failure as an error; neither claims that the Agent stopped. An
interrupted attempt end is visible tail output, so `tail --once` reports the
stop instead of timing out with no output. Fork
reports the confirmed child working directory, or an explicit reason why that
second read failed, while keeping the successful fork identity.

Core delivers typed ViewUpdates to consumers. Only committed changes enter the
cache; provisional text never advances a history cursor. The conversation reducer
owns sequence filtering, bounded completed-Attempt tracking, final precedence and
exact-prefix information. Clients do not implement protocol sequencing again.

The initial device transport has one active watch per Core instance. Competing
calls fail explicitly rather than opening clients with the same MQTT identity.
Timeout/callback cancellation releases admission and aborts the pump; normal
completion attempts bounded Unwatch cleanup, with lease expiry as fallback.
Multi-session MQTT multiplexing is deferred until required by a real consumer.

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
saving only its redacted identity. The profile also retains an opaque revocation
epoch after transient cleanup markers are removed. Pairing captures this epoch
before HELLO and checks it under admission both before retaining FINISH and before
installing credentials; a late result from a prior epoch cannot overwrite a newer
pairing. A pending record bound to the same completed pre-epoch marker is migrated
under admission; unproven records are not assigned a later epoch. Stop older local
clients before relying on the epoch: older binaries do not check it.

Before sending pairing FINISH, Core stores the exact confirmed request and Host
pin in SecretStore. Its locally generated random transport credential is bound by
hash into the Noise payload. `resume_pairing()` reuses that request after response
loss or restart, verifies the reply binding and commits local credentials/config.
It never switches endpoint, Host pin or device key to recover another operation.
Successful completion removes the pending secret; explicit forget removes it too.

Pairing persists the Noise-authenticated Host fingerprint. Fresh channels check
that pin after Noise message 2 and before sending the device's message 3. A key
mismatch keeps pairing, credentials and recovery metadata intact; it never implies
revocation. Legacy profiles without a pin can inspect diagnostics or revoke, but
must explicitly revoke and pair again before opening a channel. Core never silently
replaces a missing or changed pin.

Before the first control HTTP submission, Core durably records its Access URL,
Host/device, confirmed Host fingerprint, request ID and command ID as an uncertain
intent, without plaintext command content. Retries reuse the same encrypted request
and IDs. Only a failure proven
to precede every HTTP send removes that intent; lost receipts, cancellation and
process restart retain it. `pending_commands()` exposes recovery identities and
`recover_pending_command(request_id)` reads the outcome over a fresh authenticated
channel without submitting another control. Recovery refuses a different device
or Access endpoint/Host fingerprint and rejects mismatched request identities.
Legacy journal rows lacking routing identity are retained but cannot be
automatically attributed to a Host.

Pairing creates the cache encryption key under the profile identity lock before
publishing paired configuration. Legacy first-use key creation uses the same
profile lock; SQLite schema inspection and migration run in one write transaction
so concurrent Core instances do not race ALTERs. A per-instance mutex alone is
not a profile-wide initialization guard.

Send waits through `received`/`host_accepted` command-result replies until a
terminal outcome or timeout. An accepted receipt is not confirmed execution.

Derived development cache schemas can be rebuilt; credentials and command outcome
metadata are retained. OS keyring, TLS and real-source behavior require their own
release acceptance; isolated fixture evidence is not a deployment claim.

Conversation message/staging payloads have a 128 MiB logical cache budget. Cache
writes evict the oldest stored complete views with their cursors and receipts,
never individual messages beneath a retained cursor. The view being written is
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

`diagnostics_preview()` renders the same local, redacted evidence as
`export_diagnostics` without writing a file or opening a channel. It keeps the
existing policy of redacting Host identities to ordinal labels and exposes only
the last failure code, while including schema/version, the Access address class
already present in doctor output, command counts, connection state and metrics.
Message content, tool parameters, drafts, credentials, invitation tokens and
private keys are excluded.


The storage and wire formats are unchanged by the explicit-key cutover. A
profile written by the new binary reopens under the previous binary (verified
with a synthetic post-cutover fixture read through the pre-cutover API), so a
stopped-client rollback keeps identity, pins, drafts and command intents.

A cache is rebuildable local state; pairing lives in configuration and the
`SecretStore`. A missing or replaced cache key and a non-SQLite cache file are
quarantined and rebuilt automatically without changing the paired identity. A
busy cache or a cache written by a newer schema is refused with `cache_error`
instead of being destroyed, so a downgrade cannot silently discard newer
storage. The last recovery reason is recorded in `cache-recovery.json` and is
readable through `Core::cache_recovery_reason` without opening the cache.
