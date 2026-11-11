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

A SessionKey is `(host_id, session_id)`. Source history cursors are opaque strings;
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
removes identity. Transport, source and decoding errors are not grant revocation.

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
