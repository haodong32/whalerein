# WhaleRein Core Architecture

[Overview](../architecture.md) · [CLI](cli.md) · [Contract](harness-contract.md)

Core is the self-contained Rust client endpoint. SecretStore is its only external
injection interface; Harness adapters run on Hosts and never enter Core.

Consumers use `api` and the normalized `conversation` types. Transport, cache,
security, pairing, command, synchronization and runtime modules are private.
`api` re-exports the supported secret stores, connection state and metric types;
clients do not construct internal protocol services. Repository-only protocol
drivers opt into `protocol-testing` and use the explicit `testing` exports.

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

`api/core.rs` owns the client facade. Its unit tests live in `api/core/`,
while watch orchestration and its focused tests remain in `api/watch.rs` and
`api/tail_tests.rs`. Facade tests are grouped by cache, command recovery, pairing,
diagnostics and approval/revocation behavior, with shared setup in `test_support.rs`.

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

Watching subscribes MQTT before registering a source Watch and synchronizing its
fixed history cut. A bounded background pump continues processing receipts during
HTTP history reads. Channel/watch replacement rejects old updates. RefreshWatch
reconciles the source history head and current state every five seconds; a lost
final notification can therefore recover without subsequent event traffic.

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

Windows cleanup must not initialize a missing cache to infer retention. It uses
only the already initialized connection, and a missing cache retains an
unverified marker. Cache reads on Windows check the same confirmed revocation
fence as Unix, including when an older Core still holds a cached connection.
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
