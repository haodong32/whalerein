# Mosquitto Line Architecture

[Access](access.md) · [Deployment](../../deploy/README.md) · [Wire contract](../../proto/v1/WIRE.md)

The line runs one active Access and one pinned Mosquitto per line, with
PostgreSQL external and authoritative. Mosquitto 2.1.2 owns MQTT connections,
topic routing and local access enforcement. A narrow native plugin
(`deploy/mosquitto/plugin`) owns the boundary that Mosquitto does not provide:
deferred credential validation against Access, revision-fenced policy,
connection snapshots and notifications, and targeted disconnects.

## Why a plugin

Upstream Dynamic Security stores its own client credentials and roles. That
would duplicate device credential authority in the broker and it cannot consult
Access asynchronously during CONNECT. The Mosquitto 2.1.2 plugin interface
provides the required hooks directly:

| Capability | Native interface |
|---|---|
| Deferred CONNECT validation | `MOSQ_EVT_BASIC_AUTH` with `MOSQ_ERR_AUTH_DELAYED` and `mosquitto_complete_basic_auth()` |
| Local publish, subscribe and delivery checks | `MOSQ_EVT_ACL_CHECK` with `MOSQ_ACL_WRITE`, `MOSQ_ACL_SUBSCRIBE` and `MOSQ_ACL_READ` |
| Connection notifications | `MOSQ_EVT_CONNECT`, `MOSQ_EVT_DISCONNECT`, `MOSQ_EVT_CLIENT_OFFLINE` |
| Private management | `MOSQ_EVT_CONTROL` on `$CONTROL/whalerein/v1` |
| Targeted disconnect | `mosquitto_kick_client_by_clientid()` and `mosquitto_kick_client_by_username()` |
| Snapshot | Plugin-maintained connection registry indexed by a monotonic sequence |
| Completion scheduling | `MOSQ_EVT_TICK` |

## Connection admission and policy

An external Core, Host or device CONNECT carries its existing username and
password. The plugin validates the client-id/username shape locally, then posts
the credentials to Access `POST /internal/broker/connect` from a bounded worker
pool. The broker event loop never performs network I/O; the result is applied
from `MOSQ_EVT_TICK`, where `mosquitto_complete_basic_auth()` is called.

The Access response installs one connection policy:

```text
whalerein/1
ok
kind <host|device|probe>
host <host id or empty>
device <device id or empty>
revision <monotonic policy revision>
ready <0|1>
will <device will topic>            # device connections only
acl <write|read|subscribe> <grant revision> <host or -> <topic filter>
...
```

`acl` entries are the complete decision set for the connection. Grants that are
not listed are absent for this session. Each entry carries the durable grant
revision it was derived from, so a revocation can fence exactly the entries it
supersedes. Every `CONNECT`, `publish`, `subscribe` and delivery decision after
admission is local; the broker makes no per-operation request to Access.

The internal Access identity authenticates locally against a broker
configuration credential (`plugin_opt_access_password_file`) so the control path
survives Access restarts and Access HTTP outages. Device credentials never
appear in broker configuration or in plugin state.

## Managing policy

Access is the only principal allowed to publish on `$CONTROL/whalerein/v1`.
Control requests and replies use the same versioned line protocol.

| Operation | Fields | Effect |
|---|---|---|
| `ping` | `revision`, `ttl` | Refreshes the broker-local policy validity window and acknowledges the applied revision. |
| `set` | `client`, `kind`, `host`, `device`, `revision`, `ttl`, `acl...` | Replaces one connected client policy; older revisions are refused as stale. |
| `remove` | `client`, optional `kick` | Invalidates one client policy. |
| `deny` | `device`, `generation`, optional `host`, optional `kick` | Fences a device-wide or Host-scoped revocation, removes superseded ACL entries and optionally disconnects matching clients. |
| `kick` | `client` or `username` | Disconnects the current connection. |
| `snapshot` | none | Returns the current connection registry with its sequence boundary. |

Replies are published to `whalerein/v1/control/reply` and presence notifications
to `whalerein/v1/control/events`. Both topics are denied to every principal
except the Access identity. A reply carries the request id, `ok` or `error`, and
the applied sequence/revision.

## Presence

The plugin emits `connected` and `disconnected` events with a monotonic
sequence, the client id, principal kind, host and device scope, and a
connection identity that changes on every accepted connection. Access applies
events in sequence order and treats a delayed event for a superseded connection
as stale.

On Access reconnect the plugin answers `snapshot` with the connection registry
and the sequence boundary observed under the same lock that assigns sequences.
Access discards notification sequences at or below the boundary, so a snapshot
followed by later notifications cannot lose or duplicate a transition.
The device last will is a fallback only while presence is unknown: it does not
identify the connection that ended, so it cannot override a live broker
connection. Host `HostReady` confirms the request subscription before a Host is
treated as online, and the next sequenced snapshot repairs a lost notification.
There is no HTTP/REST presence fallback.

## Bounds and failure behavior

- Connection policy and presence state are session-bound and in memory. A
  broker restart drops all of it; Access rebuilds policy from PostgreSQL before
  external traffic is reopened.
- The policy validity window (`policy_ttl_ms`, 15 s by default) is refreshed by
  Access `ping` messages. When Access cannot confirm freshness, affected ACL
  checks fail closed and new external connections are denied.
- A revocation is complete only after PostgreSQL commits, the plugin
  acknowledges the matching fence, and the superseded connection is closed or
  confirmed absent. Access keeps its own admission closed until then. A
  Host-scoped `deny` closes a device connection only when no other Host's grant
  remains on it; one device connection can hold several Host scopes.
- Reconciliation projects only connections present in the broker snapshot and
  skips a policy whose durable revision already matches, so historical grants
  cannot turn each pass into an O(all grants) control sweep.
- The listener enables `set_tcp_nodelay`. Streaming acceptance measured ~40 ms
  visible-update batching without it (Nagle plus delayed ACK on small relayed
  frames), which distorted the 100 ms and 20 ms cadence profiles.
- Control replies are bounded to one MQTT message; a connection policy is
  limited to 1024 ACL entries and a control request body to 128 KiB.
- Notification loss while Access is disconnected is repaired by the next
  snapshot; the broker never invents current state.

## Deployment

The broker image builds Mosquitto 2.1.2 from the pinned upstream release and
the plugin against the matching headers (`deploy/docker/mosquitto.Dockerfile`).
The build applies one narrow broker patch, kept in
`deploy/mosquitto/patches/loop-tick-schedule.patch`: Mosquitto 2.1.2 resets its
loop timeout each iteration but re-arms it only when a plugin tick callback is
due, so a network event arriving between ticks can leave deferred CONNECT
completion waiting for an unrelated event. The patch re-arms the loop for the
earliest pending plugin tick. The listener keeps the existing
address, TLS identity and topic grammar. External readiness requires PostgreSQL,
the Access MQTT session, a completed policy reconciliation and a confirmed
control ping. Device credentials, URLs and topic names are unchanged, so a hard
cut is a broker replacement plus the planned reconnect window.
