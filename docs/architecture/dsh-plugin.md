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
| `src/config.js` | DSH profile validation and manual connection settings. |
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

Archive state is a source fact, not a Session record field. The DSH adapter reads
the workspace registry's `archivedSessionIds` when it lists Sessions and marks
the normalized summary's `archived` flag. That flag belongs to the
harness-neutral summary contract: another adapter maps its own default-hidden
state into it, and a source without the concept reports false. Core transports
and caches the flag without filtering, so a client can hide archived Sessions by
default and still offer an explicit archived view. The Host listing never drops
them, and `archived` never stands in for deletion or inaccessibility.

Native history positions remain inside the adapter. ReadHistory pins a source cut,
budgets encoded Messages and advances across filtered internal records. A native
regression changes the view and live epoch; old cursors require a new snapshot.
Normal snapshot reads do not allocate new IDs for existing Messages. Approval and
runtime state are read separately and do not receive synthetic history positions.

The current native history API may materialize a source snapshot. Byte-bounded
pages bound protocol messages, not that native allocation. Incomplete snapshots
fail explicitly; retained delta ranges are accepted only when proven available.
A snapshot stages complete passive pages pinned to one cut; it no longer slices a
fixed 5,000-event tail. When the page safety cap makes a snapshot incomplete, its
retained lower bound comes from the actual staged range, not a synthesized tail.
State refresh uses the native live head directly when available rather than
materializing conversation content on every heartbeat.

Questions stay source-owned. Reading, listing, observing or editing a question
never acquires a native answer claim; only an explicit answer submission claims
a live handle for the duration of that answer. A session interrupt releases that
claim and settles only the in-flight answer, so a later question is never
permanently `conflict`.

The adapter owns the collection stamp: an observation epoch that changes when
its incarnation or exact native owner object/session changes, and a revision
that advances only when normalized inventory (ordered membership, content,
guards, phase, answerability or unavailable metadata) changes. Unchanged reads
and unrelated token/runtime output keep the same stamp. Broker-only questions
participate even without a committed Message, and native question insertion,
removal, phase/local resolution and root replacement invalidate the inventory.
A question page certifies the complete inventory it was sampled from and is
complete only on its final page; the stamp is not answer authority.
Question list cursors are authenticated by device and session, persist a per-Host
key in the Host state store, and resume from the last delivered batch identity
rather than from a whole-snapshot fingerprint. Adding, answering or removing a
batch between pages therefore changes the continuation point, not the cursor
validity; a cursor from a lost key generation is reported as `snapshot_required`
(restart from the first page) instead of a misleading conflict.

Model selection is a session mutation with the same command deadline as queue and
answer controls. Re-selecting the session's effective model is a no-op at the CLI
and FakeDSH source level, a selection never rewrites another session's model, and
the next turn is observed through the source's `last_used` projection rather than
the receipt echo.

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
presence topic. On `device_offline` it clears that Device's MQTT observations.
Concurrent HTTP channels and requests keep their own lifetimes. An admitted
answer keeps its exact request authority until cancellation, lease expiry or
the hard request deadline. Applicable grant/device
revocation remains stronger and fences both observation and execution. The
adapter's existing `finally` releases the native question `attachWait`; there is
no separate claim registry, lease, token or protocol claim field. TTL remains the
fallback when presence is absent. The broker-to-Access presence edge is unit-tested but not yet verified
end-to-end against Mosquitto in the integration harness.


## HTTP claim lease

One-shot CLI commands submit and poll through the Access HTTP API; they do not
hold a device MQTT session. Liveness is exact and durable on the request row:
only the authenticated device's own `GET /v1/requests/{id}` or
`POST /v1/requests/{id}/heartbeat` renews that request's 10-second lease, bounded
by its original deadline, using database time. Other requests, other devices and
observation traffic cannot renew it. A bounded sweep atomically expires an
elapsed lease exactly once, records retryable cancellation metadata on the same
row, stops forwarding a closed request, and publishes a bounded exact-scope
`request_stale` notice on the host's presence topic. Publication alone is not
proof of native release; pending notices replay after restart or broker failure.
Host validates the exact host/device/request/command/digest/deadline binding,
fences a not-yet-admitted request through its command journal, and cancels only
that execution. MQTT will/keepalive remains for long-lived event connections
only, and the hard request deadline remains the final fallback.

## Profile-owned connection configuration

The DSH profile user layer is the only source of connection selection and
parameters. `service` selects `whalerein_cloud`, `custom`, or `standalone`.
`cloud_environment` is an internal deployment selector defaulting to production;
ordinary settings expose no staging/local picker. `custom` contains a strict
account-service definition and scoped system/file TLS settings. Standalone keeps
its manual connection fields. See the [configuration contract](../../host/dsh/README.md#configuration).

`src/service-config.js` validates these values and constructs an immutable
in-memory snapshot for the shared Host resolver and packaged official catalog.
It never reads a TOML file, infers configuration from the identity registry or
rewrites old settings. Removed selectors are rejected; the plugin accepts only
the new connection selection contract. Core/CLI file configuration and the shared
environment catalog contract are unchanged.

The managed Host id is the persisted DSH profile id, not the hostname. Its
signing and Noise keys belong to that profile and survive removal. A removed
Host re-adds through a new enrollment generation and a fresh approval, keeping
the same id and keys; the previous credentials and grants stay fenced. A
removal that is still in flight blocks a new approval. `host_label` is display
metadata only and never changes identity.

The Host manager accepts read-only profile-owned snapshots. It never refreshes
these from disk or accepts configuration/profile switching mutations in this
mode. On startup, the profile selects an existing matching authority or a new
isolated identity; the registry stores identity and recovery facts rather than
another configuration choice. A failed resolution cannot resume an old authority.
Stopping/reloading releases the existing activation ownership before another
runtime can use the same state root. Credentials and state paths are preserved.

The client edits the profile with DSH's atomic revisioned namespace writer and
reports that a plugin reload is needed. The remote service no longer exposes
profile switching, test-environment enabling, custom-profile creation/deletion or
TOML writers. It projects no inactive profile list. Custom definitions share the
same TLS resolution as Host, with no process-global trust mutation. Real DSH
persistence, reload ordering and browser behavior require native integration
verification in addition to isolated settings tests.
