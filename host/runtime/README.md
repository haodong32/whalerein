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

| Module | Responsibility |
| --- | --- |
| `contracts/source.js` | Source interface and normalized update validation. |
| `contracts/serialization.js` | Session-summary conversion to protocol messages. |
| `commands/` | Query/control routing, durable deduplication and unknown outcomes. |
| `events/` | Channel-scoped watches, leases, typed batching and per-device publication. |
| `security/` | Identity, Noise channels, grants and authenticated payloads. |
| `access/` | MQTT readiness and opaque logical fragmentation. |
| `state/` | Public peer/command metadata, ciphertext outbox and OS-released instance lock. |
| `generated/` | Descriptor generated from the only schema, `proto/v1/whalerein.proto`. |

Each persistent runtime owns its state directory exclusively. A separate SQLite
transaction supplies the instance lock and releases on process exit, without
locking the operational database. Source metadata contains identifiers and view
positions, never another plaintext conversation log.

Source and subscription failures never revoke device grants. A refresh every
five seconds reconciles authoritative history and state, including a lost final
notification. A watch lease expires after 30 seconds without refresh. Text
coalescing is bounded to 20 ms / 8 KiB; one slow recipient does not own another
recipient's publication queue.

Use `./repo test --project host --coverage` from the repository root for this
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
