# WhaleRein Access Architecture

[System overview](../architecture.md) · [Plugin](dsh-plugin.md) · [Setup](../../access/README.md)

Access is a Go monolith for transport authorization, pairing registration,
durable request/status copies, and encrypted relay. PostgreSQL owns durable
server state; EMQX supplies transport and connection facts.

This page describes the current implementation. The proposed
[deployment and identity redesign](access-evolution.md) replaces the five-table
constraint and process-local workflow assumptions to support external identity,
independent regions and replaceable instances. It is not implemented yet.

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
broker -> EMQX
```

Handlers do not access SQL/MQTT directly. `cmd/server` wires callbacks;
`auth` and `broker` do not import each other.

## Persistence

| PostgreSQL table | Contents |
|---|---|
| `hosts` | Host identity, protocol metadata, pinned authorization public key. |
| `devices` | Device identity/fingerprint and unique transport-token hash. |
| `grants` | Host/device authorization, revocation, replay acknowledgement. |
| `pairing_tokens` | Invitation hash, expiry, use marker, failure count. |
| `requests` | Bodies/status, command identity/digest, forwarding lease/retries. |

There are five tables. Idempotent migrations run under an advisory lock.
A partial unique `(host_id, device_id, command_id)` index enforces control dedupe.
Forwarding work stays on the request row; revocation replay stays on the grant.

HELLO correlation, transient MQTT calls, presence caches, and signature replay
windows are in memory. Access restart loses these; pairing restarts from HELLO,
and a still-valid host signature can be accepted again.

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

Final statuses cannot be overwritten; `unknown` may become succeeded/failed.
Expiry cleanup removes bodies but retains identity, digest, and status. Late
results cannot restore expired bodies. Access never generates DSH success.

## Pairing and revocation

Invitations accept an admin bearer or a host signature checked against the key
pinned from authenticated HostReady. Invalid supplied bearers do not fall through
to signature authentication. Only token hashes are stored.

HELLO checks invitation and host presence. FINISH/AUTHORIZE stage Host peer
metadata. Access atomically consumes the invitation, writes the pinned device/grant
and queues an idempotent COMMIT in the existing PostgreSQL outbox. The grant's
monotonic `grant_generation` increments in that transaction; direct delivery and
the durable COMMIT carry exactly that generation. Failed repair
never revokes a previously active grant as compensation. The Host activates only
after COMMIT; revocation also removes pending activation state. Bounded exact
pairing receipts handle response loss without persisting plaintext credentials.

Global device revocation commits all affected grants, disconnects the device,
and sends host notices. ACK sets `replayed_at`; pending notices replay on
HostReady, followed by the final batch that opens the host handshake gate.
Host-signed revocation removes only that host's grant and attempts the same
incarnation-bound notice when online. Local settings cleanup uses the generation
observed before its HTTP request, so it cannot remove a concurrently repaired peer.

Revocation notices and ACK updates are bound to each device's grant generation,
not a shared batch timestamp. Host revocation watermarks survive re-pairing;
old notices preserve newer peers and old COMMIT retries cannot restore revoked
incarnations. Existing grants migrate as generation zero.

Presence updates reject timestamps older than the cached connection fact. Cached
freshness and optional REST fallback do not authorize grants.

## Broker and listeners

Internal auth/ACL/presence callbacks use a separate listener and shared header.
ACLs restrict principals to authorized topics. Presence comes from broker `$SYS`
events, HostReady, or configured webhook/REST support. `$SYS` is not retained;
restart recovery cannot rely on it alone. Active disconnect uses EMQX REST/ctl.

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
