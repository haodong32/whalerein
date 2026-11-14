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
target-environment verification work beyond FakeDSH tests.

## Production capabilities

Model catalog/read/select, pending-queue read/edit/remove/steer, session create,
fork, wait, question read/open/continued and delivery queue/steer are advertised
by default. Advertisement is feature detection: the real adapter inspects the
native services bound to its Cordis context and advertises an operation group
only when the corresponding native service support is present. It is not tied
to an allowlisted DSH package version or platform. Missing native support
reports `unsupported_capability`; it never silently degrades to another
operation or fabricates a result.

The shipped profile schema exposes two switches, both defaulting to true.
`pending_queue_enabled` gates the pending-queue read/edit/remove/steer group.
`enable_open_question_broker` gates the open-question broker used to answer
open questions. Setting either key to false stops the guarded operations from
being advertised; other groups are unaffected. Question discovery and continued
answers follow native service support independently of the open-question
broker.

Queue and steer delivery are advertised whenever the native prompt service is
callable. The exact-runtime check is no longer an advertisement condition and
does not select a fallback delivery mode; the adapter advertises the modes the
native source actually provides.

Model selection still uses the native controller and returns its selected value
with the source-default save attempt marked unconfirmed. The Host journals
command results and never retries a possibly executed selection as a new action.

Native evidence is scoped, not universal. Strong queue/steer evidence exists
only for the standard DSH CLI runtime on Linux or macOS arm64 with Node 25.9.0,
DSH launcher/session-controller/app-boot 0.2.2-rc.1 and Cordis 4.1.0. Other
platforms and versions are open but unverified, including Windows native
behavior; a repeatable macOS runner has not been established. Isolated adapter,
model and question tests or fixtures are not platform validation. "Advertised"
therefore means usable when the native services exist; it does not assert
verified behavior on every environment.


## Presence and claim release

Access resolves a device last will or `$SYS` disconnect into a host-scoped
`whalerein/v1/h/<host_id>/presence` event. Host subscribes only to its exact
presence topic; on `device_offline` it clears that device's live channel and
aborts its request/model/question scopes. The adapter's existing `finally`
releases the native question `attachWait`; there is no separate claim registry,
lease, token or protocol claim field. TTL remains the fallback when presence is
absent. The broker-to-Access presence edge is unit-tested but not yet verified
end-to-end against Mosquitto in the integration harness.


## HTTP claim lease

One-shot CLI commands submit and poll through the Access HTTP API; they do not
hold a device MQTT session. Access therefore treats a non-terminal request's
result polling as the client liveness signal: `Submit` records a claim, every
`GET /v1/requests/{id}` renews it, and a 10s silent lease renewed by the CLI 5s heartbeat publishes a bounded
`request_stale` event on the host's exact presence topic. Host aborts the
matching request scope; the adapter `finally` releases the native question
claim. MQTT will/keepalive remains for long-lived event connections only. TTL
is still the fallback.
