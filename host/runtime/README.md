# WhaleRein Host Runtime

`whalerein-host` is the source-independent Node.js protocol endpoint. DSH is the
current connector; another Harness supplies a SourceAdapter and Host credentials.
The runtime does not import a DSH package or execute provider-specific parsing.

```javascript
import { HostRuntime } from 'whalerein-host'
const runtime = new HostRuntime({ config, adapter: source, credentials })
await runtime.start()
// The embedding connector owns shutdown and reload.
await runtime.stop()
```

The Source owns history and actions. Its normalized interface supports scoped
watches, opaque history cursors, fixed-cut pages, current state and explicit
commands. Optional approval queries fail with unsupported_capability by default.
Host credentials expose getCredential/setCredential independently of the Source.
Lost credentials with a recorded public key fail instead of rotating identity.

`SourceAdapter.execute` returns a confirmed native invocation receipt. Sources use
`DefinitiveExecutionError` only with evidence of a failed outcome or rejection
before invocation. Other errors leave control outcomes `unknown`, regardless of
their wire error code. The Host journals admission before invocation and stores a
terminal result only after encoding and persistence succeed. Lost replies and
completion-write failures never authorize another invocation of the same command.

## Managed cloud connection

`createConnectionManager` adds Host-specific service profiles and browser
approval around the same runtime. It owns a versioned registry of opaque
profiles, independent signing/Noise keys and credentials per profile,
cross-process activation ownership, the Cloud create/poll client, Host-owned
route validation and the enrollment/recovery journals.

```javascript
import { createConnectionManager } from 'whalerein-host/manager.js'
const manager = createConnectionManager({ stateRoot, credentials, source })
await manager.ready
manager.snapshot() // sanitized state, profiles and capabilities; never secrets
await manager.startConnect() // returns awaiting_approval with an approval URL
```

Production selects an official descriptor with no account connection; staging
is added only through `enableStaging()` and never activates by itself; custom
managed services are added with an explicit validated `apiOrigin`. Switching
stops the old profile's traffic and publishes the new selection atomically.
A confirmed enrollment and stored credential reconnect automatically after the
original approval deadline. `disconnect()` persists the local pause before
teardown; `remove()` only opens the verified management confirmation and does
not claim removal until scoped status reports an accepted/enforced intent.
The manager resolves the scoped Cloud removal state at cold start, before a
user-initiated connect or resume, and once after a definitive broker
authentication refusal. Only an active removal is polled to completion
(`removalPollMs`); a healthy connected profile has no background removal poll.
A terminal removal stops the transport, drops the transport credential and
projects `removed`. The Host id is the DSH profile id and never changes.
Reconnecting starts a fresh browser approval for the next enrollment
generation; Access re-enables the same Host id only for a strictly higher
revision with identical workspace and keys. Old credentials, grants and
revocation fences remain fenced to their generation, so the old grant is never
revived. A rejected removal leaves the live binding and credential intact. `snapshot().action` projects the runtime-selected next
action; settings refresh never starts or changes a connection and never
performs the removal read.

| Module | Responsibility |
| --- | --- |
| `contracts/source.js` | Source interface and normalized update validation. |
| `contracts/serialization.js` | Session-summary conversion to protocol messages. |
| `commands/` | Query/control routing, durable deduplication and unknown outcomes. |
| `events/` | Channel-scoped watches, leases, typed batching and per-device publication. |
| `security/` | Identity, Noise channels, grants and authenticated payloads. |
| `access/` | MQTT readiness, complete-message inline/reference relay and the Host-authority HTTPS ciphertext client. |
| `state/` | Public peer/command metadata, ciphertext outbox and OS-released instance lock. |
| `generated/` | Descriptor generated from the only schema, `proto/v1/whalerein.proto`. |

Each persistent runtime owns its state directory exclusively. A separate SQLite
transaction supplies the instance lock and releases on process exit, without
locking the operational database. Source metadata contains identifiers and view
positions, never another plaintext conversation log.

Source and subscription failures never revoke device grants. A read-only
version check every five seconds reconciles authoritative history, run,
projection, approval and question markers, including a lost final notification;
it never renews a lease or repeats content. An observation is owned by the
authenticated Broker connection instance and ends on that exact connection
close, channel replacement or Host restart. Live text coalescing is bounded to
20 ms and the receiving device's authenticated packet budget, not a fixed 8 KiB
cap. An oversized delta is split on UTF-8 boundaries into complete ordered
previews before sealing. Previews travel directly from the Host to the device
topic at QoS 0 and are never serialized behind another watch or another
recipient's acknowledgement.

## Delivery classes

The relay has two logical classes on the one authenticated broker connection;
the class is selected by topic and QoS, never by a second connection.

| Class | Topics | QoS | Semantics |
| --- | --- | --- | --- |
| control | `.../result`, `.../event`, `.../request` | 1 | Ordered, acknowledged and durable. Results and committed events stay on these topics and keep the inline or HTTPS-reference representation. |
| live | `.../d/{device_id}/live` | 0 | Fire-and-forget derived state published directly to the authorized device. Previews are inline `EventEnvelope`s only, have no acknowledgement, Access relay, outbox entry or retry, and are dropped when the connection is not ready or the bounded live queue is full. |

The final committed message or an authoritative HTTPS read supersedes every
preview. A preview is never published on the control event topic and control is
never queued behind a preview.

## Relay objects

One application message travels in one MQTT PUBLISH. It is inline when the full
encoded packet fits the smallest of the local profile, broker and advertised
Access limits. An oversized durable message is uploaded first to the Access
temporary ciphertext directory over HTTPS (`POST/PUT/finish`) using the same
Ed25519 key announced in `HostReady`, then published as a `PayloadReference`.
The relay client uses the existing `accessUrl` (or the managed
`accessOrigin`) configuration; no second identity or origin is introduced.

Use `./repo test host --coverage` from the repository root for this
package; see [repo commands](../../tools/repo/README.md) for other operations.
Unit coverage includes
this package's own source, not its test fixtures. `test/support/` owns the generic
broker, wire clients, channel helpers and Host harness. Host tests use a normalized
Source fixture and never import DSH. Connector tests supply their own adapter to
the Host harness; native DSH crash and projection checks stay in the DSH suite.
See [the contract](../../docs/architecture/harness-contract.md) for invariants and
[testing](../../docs/testing.md) for isolated integration and native release gates.
Run `pnpm install --frozen-lockfile` from the repository root to install both
Node.js packages. The DSH connector depends on Host through `workspace:*`;
the root `pnpm-lock.yaml` locks both packages. `pnpm pack` converts the workspace
dependency to the Host version for distribution, so install or publish Host
alongside the connector. No Pi entry or dependency is included.


## Grant and watch lifecycle

Access assigns a monotonically increasing grant generation at pairing COMMIT.
Peers retain that generation; revocation watermarks are kept across repairs.
A stale notice cannot remove a newer peer, and a revoked/superseded COMMIT is
rejected even if it is retried later. Legacy state migrates to generation zero.
Deploy Access and Host together when introducing this fencing contract.

Watch subscription installation rechecks source ownership after awaiting its
handle. If channel replacement or shutdown removed that source, the returned
handle closes immediately instead of becoming an untracked subscription.

## Command result retention

On startup and new command admissions, bounded lazy maintenance removes result
payloads for succeeded/failed commands whose request expired more than seven days
ago (up to 1,000 rows per sweep, at most once per minute). Command IDs, digests,
terminal status and error codes remain for deduplication. Accepted/unknown command
records and their evidence are not expired. A result recovery can therefore know
the final outcome after detailed result bytes are unavailable. Control tombstone
metadata still grows with command count; no finite deduplication window or unsafe
TTL deletion is implied.
