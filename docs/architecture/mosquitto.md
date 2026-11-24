# Mosquitto Line Architecture

[Access](access.md) · [Deployment](../../deploy/README.md) · [Wire contract](../../proto/v1/WIRE.md)

The line runs one active Access and one pinned Mosquitto per line, with
PostgreSQL external and authoritative. Mosquitto 2.1.4 owns MQTT connections,
topic routing and local access enforcement. A narrow native plugin
(`deploy/mosquitto/plugin`) owns the boundary that Mosquitto does not provide:
deferred credential validation against Access, revision-fenced policy,
connection snapshots and notifications, and targeted disconnects.

## Why a plugin

Upstream Dynamic Security stores its own client credentials and roles. That
would duplicate device credential authority in the broker and it cannot consult
Access asynchronously during CONNECT. The Mosquitto 2.1.4 plugin interface
provides the required hooks directly:

| Capability | Native interface |
|---|---|
| Deferred CONNECT validation | `MOSQ_EVT_BASIC_AUTH` with `MOSQ_ERR_AUTH_DELAYED` and `mosquitto_complete_basic_auth()` |
| Local publish, subscribe and delivery checks | `MOSQ_EVT_ACL_CHECK` with `MOSQ_ACL_WRITE`, `MOSQ_ACL_SUBSCRIBE` and `MOSQ_ACL_READ` |
| Trusted ingress identity | `MOSQ_EVT_MESSAGE_IN`, which stamps the authenticated connection reference and rejects a client-supplied one |
| Final delivery fence | `MOSQ_EVT_MESSAGE_OUT`, which re-checks the receiving connection, target and current policy after queueing |
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
| `remove` | `client`, optional `conn`, optional `kick` | Invalidates one client policy. With `conn`, a delayed removal cannot clear a same-ID replacement. |
| `deny` | `device`, `generation`, optional `host`, optional `conn`, optional `kick` | Fences a device-wide or Host-scoped revocation, removes superseded ACL entries and optionally disconnects matching clients. With `conn`, a delayed scope cannot fence a replacement that already holds newer authority. |
| `kick` | `client` (optional `conn`) or `username` | Disconnects the current connection. With `conn`, a delayed kick is reported absent instead of closing a same-ID replacement. |
| `snapshot` | none | Returns the current connection registry with its sequence boundary. |

Replies are published to `whalerein/v1/control/reply` and presence notifications
to `whalerein/v1/control/events`. Both topics are denied to every principal
except the Access identity. A reply carries the request id, `ok` or `error`, and
the applied sequence/revision.

## Connection identity

Every accepted connection receives a reference `<broker-epoch>:<conn-id>`. The
broker epoch is a random 16-hexadecimal value generated once per broker process;
`conn-id` is monotonic within that process. A broker restart therefore
invalidates every earlier reference. Events and snapshots carry both the numeric
`conn` and the `ref` string, and `disconnected`/`snapshot` reconciliation close
only observations bound to the exact reference.

`MOSQ_EVT_MESSAGE_IN` stamps `whalerein-connection=<ref>` on every external
PUBLISH after authorization and before the message is stored. A client that
supplies that reserved property, or `whalerein-target`, is rejected with
`not_authorized`; the connection stays open. Access is the only principal that
sets `whalerein-target`, naming the exact connection admitted for a queued
reply. `MOSQ_EVT_MESSAGE_OUT` re-checks the receiver's current policy and, for a
directed reply, requires the target to equal the receiver's current reference.
A queued or in-flight delivery therefore cannot outlive a grant, a replaced
connection or a broker epoch.

Message Expiry Interval is a broker-owned MQTT 5 property: Mosquitto stores it
outside the published property list and re-adds the remaining value at delivery.
It discards a queued copy whose delivery has not begun by then. Application
deadlines are still enforced by Access, Host and Core from the original
`expires_at_unix_ms`; the broker does not replace that contract.

## Presence

The plugin emits `connected` and `disconnected` events with a monotonic
sequence, the broker epoch, the client id, principal kind, host and device scope,
and the exact connection reference. Access applies events in sequence order and
treats a delayed event for a superseded connection (different `ref`) as stale.

On Access reconnect the plugin answers `snapshot` with the broker epoch, the
connection registry and the sequence boundary observed under the same lock that
assigns sequences. Access discards notification sequences at or below the
boundary, so a snapshot followed by later notifications cannot lose or duplicate
a transition. A device Will is not used: ordered broker lifecycle events plus
snapshot repair establish scoped disconnects, and a Will cannot name the
connection instance it replaced. Host `HostReady` confirms the request
subscription before a Host is treated as online. There is no HTTP/REST presence
fallback.

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
  remains on it; one device connection can hold several Host scopes. A
  connection-scoped `deny`/`remove`/`kick` targets that exact instance and
  cannot affect a same-ID replacement.
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

The broker image builds Mosquitto 2.1.4 from the pinned upstream release and
the plugin against the matching headers (`deploy/docker/mosquitto.Dockerfile`).
The build applies one narrow broker patch, kept in
`deploy/mosquitto/patches/loop-tick-schedule.patch`: Mosquitto 2.1.4 resets its
loop timeout each iteration but re-arms it only when a plugin tick callback is
due, so a network event arriving between ticks can leave deferred CONNECT
completion waiting for an unrelated event. The patch re-arms the loop for the
earliest pending plugin tick. The listener keeps the existing address and TLS
identity. External readiness requires PostgreSQL, the Access MQTT session, a
completed policy reconciliation and a confirmed control ping. Epoch 5 changes
the topic grammar (it adds the Device `query`/`reply` pair and retires the Host
`.../live` relay); device credentials and URLs stay the same, so the cut is a
broker/plugin replacement plus the planned reconnect window.
