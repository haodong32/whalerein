# WhaleRein Access Architecture

[System overview](../architecture.md) · [Plugin](dsh-plugin.md) · [Setup](../../access/README.md)

Access is a Go monolith for transport authorization, pairing registration,
durable request/status copies, and encrypted relay. PostgreSQL owns durable
server state; the pinned Mosquitto 2.1.2 line (with the WhaleRein broker
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
| `internal/store/` | SQL, transactions, migrations, cleanup. |
| `internal/config/`, `internal/diagnostics/` | Validated configuration, redacted logs, metrics. |

```text
httpapi -> auth, pairing, requests
pairing / requests / events -> store, broker
store -> PostgreSQL
broker -> Mosquitto plugin
```

Handlers do not access SQL/MQTT directly. `cmd/server` wires callbacks;
`auth` and `broker` do not import each other.

## Persistence

| PostgreSQL table | Contents |
|---|---|
| `hosts` | Host identity, protocol metadata, pinned authorization public key. |
| `devices` | Pinned device identity; credentials have a single authority in `device_credentials`. |
| `device_credentials` | Independently issued transport-token hashes for a device. |
| `pairing_operations` | Opaque HELLO reply, FINISH digest, token-free receipt and committed grant generation. |
| `pairing_outcomes` | Retained FINISH identity, grant binding and verified COMMIT confirmation metadata. |
| `schema_migrations` | Applied migration names and SHA-256 checksums. |
| `grants` | Host/device authorization, revocation, replay acknowledgement. |
| `pairing_tokens` | Invitation hash, expiry, use marker, failure count. |
| `requests` | Bodies/status, command identity/digest, forwarding lease/retries. |

Nine tables use checksummed, apply-once migrations under an advisory lock.
Unversioned installations are adopted without deletion; changed historical
migrations or a database with unknown newer migrations prevent startup.
A partial unique `(host_id, device_id, command_id)` index enforces control dedupe.
Forwarding work stays on the request row; revocation replay stays on the grant.
Migrations remove the former fixed `local` workspace table and the duplicate
`devices.auth_hash`; no account/tenant model is pre-installed in Standalone mode.

Pairing operation receipts are durable. Transient MQTT correlation, presence
caches and signature replay windows remain in memory. A still-valid host signature
can be accepted again after restart. Broker REST presence lookup supports discovery
of Hosts whose non-retained connection events were missed during restart.

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
`pair-resume` reuses the original FINISH and credential after client/Access restart.
Recovery checks the exact request, expiry and active grant generation; it cannot
revive a revoked or superseded grant. Restarting a pre-FINISH Noise exchange may
require a new attempt; no Noise private state is stored by Access.

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
freshness and optional REST fallback do not authorize grants.

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

See the [wire contract](../../proto/v1/WIRE.md#logical-mqtt-transport) for transport framing.

Conversation payloads remain opaque to Access. The typed v1 Harness contract changes Host/Core domain handling, not grant ownership or server session authority. Bulk MQTT admission waits within bounded count/byte/context limits; controls bypass bulk admission.

Completed v2-framed Host replies re-establish transport capability after an Access
restart even when the Host stayed connected. A small HELLO can bootstrap an unknown
peer, but capability must be confirmed before pairing advances to grant creation.
No persistent capability table or periodic Host heartbeat is required.
