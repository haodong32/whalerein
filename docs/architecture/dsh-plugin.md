# WhaleRein for DSH Architecture

[Overview](../architecture.md) · [Shared Host](../../host/runtime/README.md) ·
[Contract](harness-contract.md) · [Setup](../../host/dsh/README.md)

The DSH package is the current Harness connector. It owns Cordis lifecycle,
settings and all native DSH interpretation. `whalerein-host` owns the reusable
protocol endpoint, networking, cryptography, watched publication and state.

The package root exposes Cordis lifecycle, `createHost` and `resolveConfig`.
The `./adapter` export exposes the connector contract and adapter factory.
Host storage, security and MQTT types are imported from `whalerein-host`, not
re-exported by the connector. Repository test support is not published.

| Location | Responsibility |
| --- | --- |
| `index.js`, `cordis.patch.yml` | Cordis registration, entry configuration and runtime construction. |
| `src/config.js` | DSH profile configuration and stable existing state paths. |
| `src/credentials.js` | DSH credential-store binding, separate from conversation APIs. |
| `src/adapter/base.js` | Source contract and native operation adaptation. |
| `src/adapter/real.js`, `fake.js` | Native source interaction and the isolated test authority. |
| `src/adapter/projection.js` | Normalized visible message blocks; no provider replay data. |
| `src/adapter/history.js` | Session/view-bound opaque cursors and fixed history cuts. |
| `src/adapter/conversation.js`, `assistant-stream.js` | Native assistant frames to normalized Attempt updates. |
| `src/adapter/errors.js` | Native error interpretation into stable public codes. |
| `src/settings.js`, `pairing.js`, `connection-test.js`, `client.js` | DSH settings, signed administrative operations and bounded diagnostics/probes. |

DSH remains authoritative for its sessions. The adapter listens to committed
`session/event` and transient `agent/assistant-stream` independently. It emits
normalized Messages, text deltas/resets, Attempt outcomes and state invalidations.
Native revision/index handling and cumulative/native block completion checks stay
here. Source-specific fields never require Core or CLI JSON parsing.

Native history positions remain inside the adapter. ReadHistory pins a source cut,
budgets encoded Messages and advances across filtered internal records. A native
regression changes the view and live epoch; old cursors require a new snapshot.
Normal snapshot reads do not allocate new IDs for existing Messages. Approval and
runtime state are read separately and do not receive synthetic history positions.

The current native history API may materialize a source snapshot. Byte-bounded
pages bound protocol messages, not that native allocation. Incomplete snapshots
fail explicitly; retained delta ranges are accepted only when proven available.
When a live snapshot is tail-truncated, its retained lower bound comes from that
actual tail, not from the full native array.
State refresh uses the native live head directly when available rather than
materializing conversation content on every heartbeat.

Host keys stay in the original DSH credential namespace. Peer/command/revocation
state paths are preserved during extraction. The shared Host has no credential
methods on SourceAdapter and no second plaintext session log. One persistent
runtime owns each instance directory through an OS-released SQLite lock.

Settings retain the DSH-specific presentation and allowlisted diagnostic projection.
Source readiness comes from the generic Host diagnostic `source` field. Missing
client recovery diagnostics stay unavailable. Connection tests probe drafts
without changing the profile or replacing the live connection. Tokens, raw errors
and message/tool content never enter diagnostics.

No Pi entry, launcher, runtime registry or dependency is implemented. Another
Harness supplies its own SourceAdapter and credential binding to the shared Host.
Actual DSH lifecycle, credential behavior and local/remote approval races remain
native release gates beyond FakeDSH tests.
