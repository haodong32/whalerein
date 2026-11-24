# WhaleRein Access Architecture

[System overview](../architecture.md) · [Plugin](dsh-plugin.md) · [Setup](../../access/README.md)

Access is a Go monolith for transport authorization, pairing registration,
durable request/status copies, and encrypted relay. PostgreSQL owns durable
server state; the pinned Mosquitto 2.1.4 line (with the WhaleRein broker
plugin) supplies transport and connection facts.

Access implements Standalone mode in one process with installation-scoped resources.
External account mode, separate API/relay deployments and regional automation
remain later work in the [deployment and identity design](access-evolution.md).
Unknown modes fail configuration validation; there is no external-account fallback.

## Modules

| Location under `access/` | Responsibility |
|---|---|
| `cmd/server/` | Configuration, migration, wiring, listeners, workers, shutdown. |
| `internal/httpapi/` | Public/internal routes, protobuf JSON, safe errors. |
| `internal/auth/` | Credentials, grants, ACL decisions, host signatures, revocation. |
| `internal/pairing/` | Invitations, HELLO/FINISH, grant creation, host authorization. |
| `internal/requests/` | Validation, dedupe, forwarding leases/retries, result application. |
| `internal/events/` | Route opaque events by outer device ID. |
| `internal/broker/` | MQTT, topics, transient correlation, presence, disconnect. |
| `internal/payload/` | Node-local temporary ciphertext admission, staged write, hard expiry, reader leases, reconciliation and cleanup. |
| `internal/store/` | SQL, transactions, migrations, cleanup. |
| `internal/clientaddr/` | Ingress client address resolution, windowed pseudonyms, log redaction. |
| `internal/config/`, `internal/diagnostics/` | Validated configuration, redacted logs, metrics. |

```text
httpapi -> auth, pairing, requests, payload
pairing / requests / events -> store, broker
payload -> store, quota directory
store -> PostgreSQL
broker -> Mosquitto plugin
```

Handlers do not access SQL/MQTT directly. `cmd/server` wires callbacks;
`auth` and `broker` do not import each other.

## Client address

The public TLS ingress establishes the client address once and writes the
single-valued `X-Whalerein-Client-IP` header, overwriting any inbound value.
Access trusts that header only when `RemoteAddr` is inside an explicitly configured
trusted proxy prefix; an empty prefix list is a direct deployment and the header
is ignored. Access never parses `X-Forwarded-For` or `X-Real-IP`.

`internal/httpapi` resolves one identity per request and keeps the raw address in
memory only. Access records carry a windowed HMAC pseudonym (`client_ref`),
its window (`client_ref_window`), and how the address was selected
(`client_addr_status`). The log sink additionally redacts any address literal
written through it, so a later handler or library error cannot leak one. The
master key is an environment secret; without it, records still omit the address
and use the `[client]` placeholder.

## Client TLS trust

Core exposes two mutually exclusive trust modes to its HTTPS and MQTT
transports. The decision is made once from configuration and the same
`rustls::ClientConfig` shape is used for both connections:

| Mode | Configuration | Trust set |
|---|---|---|
| System (default) | no CA configured | Apple/Windows platform verifier; elsewhere the platform certificate files, or the portable Mozilla roots when the platform exposes no store. A client that must extract the platform store itself (Android) supplies those anchors to Core as construction data instead of a process-global path |
| Custom | explicit `ca_pem` / `ca_file` | exactly the supplied CA bundle; it replaces the platform roots |

Custom mode follows the `--cacert` convention used by curl, Git, Go and
requests: importing a private CA is a replacement, not an addition. A client
that needs public and private roots trusts an explicit bundle containing both.
There is no silent fallback from system verification to custom roots.

## Persistence

| PostgreSQL table | Contents |
|---|---|
| `hosts` | Host identity, protocol metadata, pinned authorization public key. |
| `external_enrollments` | Current generation, keys, broker credential hash, operation and disabled state. |
| `external_removals` | Generation-scoped removal target, frozen revocations and enforcement receipt. |
| `external_device_revocations` | Generation-scoped client revocation fences; never cleared by a re-add. |
| `devices` | Pinned device identity; credentials have a single authority in `device_credentials`. |
| `device_credentials` | Independently issued transport-token hashes for a device. |
| `pairing_operations` | Opaque HELLO reply, FINISH digest, token-free receipt and committed grant generation. |
| `pairing_outcomes` | Retained FINISH identity, grant binding and verified COMMIT confirmation metadata. |
| `schema_migrations` | Applied migration names and SHA-256 checksums. |
| `grants` | Host/device authorization, enrollment generation, revocation and replay acknowledgement. |
| `pairing_tokens` | Invitation hash, expiry, use marker, failure count. |
| `requests` | Bodies/status, relay epoch, inline or opaque reference representation, command identity/digest, forwarding lease/retries, exact client-lease state/deadline, pending cancellation replay and durable notice intent. |
| `temporary_ciphertext_objects` | Node-owned temporary ciphertext reservation, scope, state, exact length/digest, hard expiry, reader count, cleanup retries and grant fence revisions. |
| `temporary_ciphertext_leases` | Idempotent reader leases that keep capacity charged until close. |

Application tables use checksummed, apply-once migrations under an advisory lock.
Unversioned installations are adopted without deletion; changed historical
migrations or a database with unknown newer migrations prevent startup.
A partial unique `(host_id, device_id, command_id)` index enforces control dedupe.
Forwarding work stays on the request row; revocation replay stays on the grant.
Temporary ciphertext capacity is charged from reservation until successful file
deletion: partial, ready and pending-cleanup bytes count against per-object,
per-host and per-node limits, and only a committed object is readable.
Migrations remove the former fixed `local` workspace table and the duplicate
`devices.auth_hash`; no account/tenant model is pre-installed in Standalone mode.

An external enrollment is a stable Host id plus a generation. Removing a
generation disables it and revokes its grants; a fresh approval may re-enable
the same id only with a strictly higher revision, the same workspace and the
same Host keys. Old credentials, grants and device revocation fences remain
bound to the removed generation. A grant records its enrollment generation and
the authority view refuses a grant whose generation is not the current ready
one. Standalone grants use generation zero.

Pairing operation receipts are durable. Transient MQTT correlation, presence
caches and signature replay windows remain in memory. A still-valid host signature
can be accepted again after restart. On reconnect Access applies a sequenced broker
connection snapshot, which repairs Hosts and devices whose non-retained connection
events were missed during the restart. There is no HTTP/REST presence lookup.

## Forwarding

```text
HostRequest -> authenticate grant -> validate -> dedupe
            -> offline: persist host_offline, do not publish
            -> online: persist received/pending -> lease -> MQTT publish
HostResult  -> validate topic host -> conditionally update the same request
```

Leases use `FOR UPDATE SKIP LOCKED`. Publication happens outside the transaction.
A crash between publish and recording success can repeat delivery; plugin dedupe
prevents repeated execution. `published_once` prevents an uncertain delivery from
being reported as never forwarded after retry exhaustion.
An ambiguous publish error can occur after the broker accepted data; exhausting
retries without a confirmed publication therefore leaves the result `unknown`.
Under the existing wire contract, a later authenticated `succeeded` or `failed`
result can settle it; a late `host_accepted` or `rejected` does not. The latter
may describe a duplicate delivery rejected before Host command deduplication,
so it does not prove an earlier delivery had no effect. A definitive
transport preflight rejection applies only when a database-locked request has
known forwarding history, no recorded publication, and exactly one lease;
legacy history and overlapping or retried leases remain `unknown`.
Internal pairing COMMIT outbox entries continue retrying through transport
preflight failures until their deadline, independently of the normal retry budget.

Expired handshake/query records are deleted in batches of 2,000 after a 24-hour
grace. Command and pairing-activation identities are excluded from that sweep;
their dedupe/recovery metadata remains even after body cleanup. This deliberately
retains small control tombstones rather than weakening uncertain-outcome safety.

Final statuses cannot be overwritten; `unknown` may become succeeded/failed.
Expiry cleanup removes bodies but retains identity, digest, and status. Late
results cannot restore expired bodies. Access never generates DSH success.

## Exact request liveness and cancellation

A non-terminal one-shot request owns one client lease on its existing request
row. Only an explicit `POST /v1/requests/{id}/heartbeat` from the exact
authenticated device and session renews it. `GET /v1/requests/{id}`, result
reads, observation traffic, sibling heartbeats and foreign devices never renew
it. Renewal uses database time, cannot pass the original `expires_at`, and
cannot revive a terminal or elapsed lease. A non-renewal is a typed `404`, `410`
or `409` response, never a silent `204`. A lease that is absent on a historical
row is unknown, not expired.

A bounded sweep atomically moves one elapsed live lease to an irreversible
`expired` state, stops forwarding, and records a retryable pending cancellation
on the same row. The published notice is bounded and scoped to the exact
Host/device/request/command/digest/deadline; it contains no question or answer
content. Publish success is not native release, and a notice that cannot be
published replays after restart or broker recovery. A late read may return the
recorded outcome but cannot reopen execution. Authorization revocation remains
independent and stronger than transport or lease liveness.

## Temporary ciphertext storage

Oversized relay payloads and attachment parts use one dedicated quota directory
per Access node. PostgreSQL records the opaque object id, owning node/owner,
complete scope (host, optional device/session/channel, exactly one request,
message or event identity, purpose and part), grant fence revisions, state,
charged and exact bytes, SHA-256 digest, hard expiry, reader count and bounded
cleanup retry state. Capacity is charged from reservation until the file is
deleted: partial, ready and expired bytes all count against per-object,
per-host and per-node limits plus object and active-upload counts.

Admission reserves atomically under node- and host-scoped advisory locks and
also checks actual filesystem headroom against the configured reserve. A body
is staged in a same-volume temp file, capped at the reserved length, digest
verified, fsynced and atomically renamed before the metadata commit to `ready`.
A reserved object is never readable, and only a committed object can be opened.
Opening records an idempotent reader lease; the charge survives expiry and
active readers and is released once after file deletion. Expiry is fixed at
reservation and can only be shortened by the parent deadline or retention cap;
retries and fetches never extend it.

Startup reconciliation repairs stale reservations, publishes a verified file
that was renamed before a metadata commit, fences ready rows whose file is
missing, and removes orphan files without live metadata in bounded batches.
Cleanup failures remain charged and retry with a bounded backoff. The package
emits bounded aggregate metrics only; object ids, paths, content and
credentials never appear in logs or metrics.

## Pairing and revocation

Invitations accept an admin bearer or a host signature checked against the key
pinned from authenticated HostReady. Invalid supplied bearers do not fall through
to signature authentication. Only token hashes are stored.

HELLO checks invitation and host presence and saves the opaque reply. Core
creates a random transport credential, binds its SHA-256 into encrypted FINISH,
and saves the confirmed FINISH and Host pin in SecretStore before transmission.
The Host verifies the forwarded credential hash against FINISH and durably stages
its exact replay binding. FINISH/AUTHORIZE stage Host peer metadata. Access
atomically consumes the invitation, writes the pinned device/grant, persists a
token-free operation receipt and queues an idempotent COMMIT in the PostgreSQL
outbox. The grant's
monotonic `grant_generation` increments in that transaction; direct delivery and
the durable COMMIT carry exactly that generation. Failed repair
never revokes a previously active grant as compensation. The Host activates only
after COMMIT; revocation also removes pending activation state. Bounded exact
pairing receipts handle response loss without persisting plaintext credentials.
Access marks Host confirmation only after validating the persisted COMMIT and
Host result under the device fence. The retained COMMIT-binding proof is derived
from the database transaction, not an independent signature. A legacy active
outcome without verifiable COMMIT evidence remains unconfirmed; observation
does not delete its credential or rewrite the historical confirmation flag.
`operation recover` reuses the original FINISH and credential after client or
Access restart. Recovery checks the exact request, expiry and active grant generation; it cannot
revive a revoked or superseded grant. Restarting a pre-FINISH Noise exchange may
require a new attempt; no Noise private state is stored by Access.

Cloud Device pairing uses `POST /v1/enrollments/devices/pairing` with
`authorization` and `claim` objects. The signed authorization binds the issuer,
regional audience, workspace, region, enrollment revision, Host keys, Device
Noise key, and operation ID. The claim ID equals that operation ID. Account
bearer credentials do not authorize Access operations. The Cloud Noise prologue
uses the lowercase hex SHA-256 of these newline-delimited values in order:
`whalerein/v1/cloud-pairing`, issuer, audience, workspace ID, region ID,
enrollment revision, Host ID, Device ID, operation ID, lowercase hex Host Noise
key, and lowercase hex Device Noise key.

Cloud and Standalone pairing records have separate hash namespaces. Access and
the Host match the Device key against the Noise transcript. Cloud grant
admission remains closed until the persisted COMMIT has an authenticated Host
acknowledgement. Recovery presents the same FINISH and current authorization
for the same scope. A conflicting operation does not replace an existing grant.

Cloud reads one grant with an Access admin credential at
`GET /v1/enrollments/hosts/{host_id}/devices/{device_id}?workspaceId=...`.
A known workspace and Host with no grant returns HTTP 404 with
`{"state":"missing_grant"}`. Pending activation and pending revocation return
`pending`, and confirmed states return `ready` or `revoked`.
The corresponding `POST .../revocation?workspaceId=...` records a durable
fence before enforcing denial. It returns HTTP 204 after regional enforcement.
The fence persists when revocation races an authorization that has yet to
produce a grant.

Global device revocation removes all device credential hashes and commits all
affected grants, disconnects the device,
and sends host notices. ACK sets `replayed_at`; pending notices replay on
HostReady, followed by the final batch that opens the host handshake gate.
Host-signed revocation removes only that host's grant and attempts the same
incarnation-bound notice when online. It preserves credentials for other live
grants; revoking the last grant removes the credentials. Local settings cleanup uses the generation
observed before its HTTP request, so it cannot remove a concurrently repaired peer.

Revocation notices and ACK updates are bound to each device's grant generation,
not a shared batch timestamp. Host revocation watermarks survive re-pairing;
old notices preserve newer peers and old COMMIT retries cannot restore revoked
incarnations. Existing grants migrate as generation zero.

Presence updates reject timestamps older than the cached connection fact. Cached
freshness never authorizes grants. A Host is online only after `HostReady`
confirms its request subscription, and cached observations expire after
`presence.cache_ttl` unless a broker event or snapshot refreshes them.

## Broker and listeners

The broker plugin delegates CONNECT validation to the internal listener and
installs the returned per-connection policy locally; publish, subscribe,
delivery and Will decisions never call Access. Presence comes from ordered
broker connection notifications plus a sequence-bounded snapshot, with the
device last will and HostReady as existing fallbacks. Active disconnect uses the
private `$CONTROL/whalerein/v1` interface. See
[Mosquitto line](mosquitto.md).

Connection-test clients use username `probe:<host_id>` and a random client ID
`whalerein-probe-<32 lowercase hex digits>`. Access validates the same provisioning
token as host authentication, but returns a distinct `probe` principal, calls no
host-registration storage operation, and denies every publish/subscribe topic.
Broker presence ignores probe usernames and client IDs, including callbacks that
supply only one of them. This convention must ship with the plugin probe client;
old Access versions reject it. Successful login proves credential acceptance,
not the normal host's topic permissions or protocol execution.

`/healthz` reports liveness; `/readyz` requires PostgreSQL and subscribed MQTT;
`/metrics` exposes bounded labels. Public routes are defined in
[`server.go`](../../access/internal/httpapi/server.go); encoding is defined by the
[wire contract](../../proto/v1/WIRE.md).

See the [wire contract](../../proto/v1/WIRE.md#logical-mqtt-transport) for the
complete-packet relay. Access publishes one MQTT 5 PUBLISH per application
message and requires `relay_version=4` on every request, result, event, live
preview, readiness proof and receive advertisement. A hop computes the exact
encoded packet for its topic and publishes inline when the packet fits the
minimum of the local profile, the broker CONNACK/configured packet limit and the
receiver advertisement. An oversized durable body is staged as exact ciphertext
and published only as a packet-sized reference after staging completes; a
reference that cannot itself fit fails explicitly with no publish.

For the QoS 1 control class Access owns the forwarding order. Each MQTT
connection has one FIFO queue per control destination topic, a bounded global
in-flight semaphore and bounded waiting bytes, so a blocked flow cannot stall an
unrelated small message. There are no application fragments, reassembly
buffers, transfer receipts or device ACK topics.

The QoS 0 live class bypasses that control queue. Access subscribes to the Host
live topic (`h/+/live`), authenticates the actual publisher, requires an active
grant and republishes the same opaque `EventEnvelope` to the authenticated
Device `.../d/{device_id}/live` topic using only the outer `device_id`. It adds
no durable row, outbox entry, receipt or notice and never waits for an
acknowledgement. A bounded set of live handoff slots drops the preview on local
backpressure instead of blocking control delivery; a disconnected Access MQTT
session or a rejected QoS 0 publish also drops it. `/metrics` exposes
`live_preview_dropped_total{reason}` with the bounded reasons `disconnected`,
`backpressure` and `publish`; no host, device, request or content identity is
attached.

## Durable request notices

The transaction that applies an authoritative status change also writes a
notice intent on the same request row. Publication happens only after that
commit. A bounded `FOR UPDATE SKIP LOCKED` worker claims due intents, coalesces
intermediate transitions into the latest stored state and publishes one
packet-sized `RequestStateNotice` with the request/command identity, status,
update time and — only while the exact result object is readable — an opaque
`result_reference`. A terminal transition stays pending until it is published,
and a publication failure is retried with capped backoff from durable state.
Duplicate notices are harmless, a stale notice never reopens an expired request
or a closed execution, and no request/command identity, reference or content is
logged. `/metrics` exposes only the pending count, oldest intent age and retry
count.

Broker ACLs follow the same topics: a Device publishes only its own
`.../d/{device_id}/ready` advertisement and last will; it subscribes to its own
event, notice and live topics and never publishes an acknowledgement. A Host
publishes its own result, event and live topics. Access publishes the request,
device event, device notice and device live topics and subscribes to results,
events, Host live previews, device advertisements and last wills. Traffic on the
Host live and Device live topics is QoS 0; control topics stay QoS 1. The plugin
policy is topic-based, so Access, Host and Core select the QoS by topic.
Per-connection receive advertisements are keyed by the broker-derived connection
generation, so a sibling reconnect never clears another receiver's budget; a
grant or App-session revocation fences all of them.

Conversation payloads remain opaque to Access. The typed v1 Harness contract changes Host/Core domain handling, not grant ownership or server session authority. Bulk MQTT admission waits within bounded count/byte/context limits; controls bypass bulk admission.

A valid epoch-4 HostReady re-establishes transport capability after an Access
restart even when the Host stayed connected. Access answers it with one
`REQUEST_KIND_ACCESS_READY` request carrying Access's own receive advertisement
and refreshes it only when the effective budget or connection generation
changes. No persistent capability table or periodic Host heartbeat is required.
