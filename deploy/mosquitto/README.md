# Mosquitto line

The deployed line runs one Access process and one Mosquitto 2.1.4 broker with
the WhaleRein plugin in `plugin/`. PostgreSQL stays external and authoritative.
See [the architecture](../../docs/architecture/mosquitto.md) for the policy,
presence and failure contract.

## Files

| File | Purpose |
|---|---|
| `mosquitto.conf` | Listener, plugin options, policy validity window |
| `plugin/whalerein.c` | Native plugin: deferred CONNECT validation, local ACLs, `$CONTROL` management, snapshot/notifications, targeted disconnect |
| `plugin/Makefile` | Local build against the pinned broker headers |
| `patches/loop-tick-schedule.patch` | Broker-local tick scheduling fix applied by both deployment images |
| `plugin/*.so` | Build output (Git-ignored) |
| `test/plugin_test.go` | Opt-in native capability fixture (`go test -tags mosquitto`) |

## Build and run

The Compose stack builds the image from `deploy/docker/mosquitto.Dockerfile`:

```bash
docker compose -f deploy/compose/stack.yaml up -d --build
```

Native fixture (requires local Mosquitto 2.1.4 headers and broker binary):

```bash
MOSQUITTO_INCLUDE=$(brew --prefix)/include \
  go test -tags mosquitto ./deploy/mosquitto/test -run TestNativeBrokerCapabilities -v
```

## Configuration

| Plugin option | Meaning |
|---|---|
| `access_username` | Internal Access MQTT identity |
| `access_password_file` | File containing the Access broker password |
| `access_auth_url` | Access connect-validation endpoint |
| `access_auth_token_file` | File containing the internal callback token |
| `auth_timeout_ms` | Bounded CONNECT validation timeout (50–10000) |
| `auth_workers` | Authentication worker threads |
| `policy_ttl_ms` | Broker-local policy validity (1000–120000) |

Broker wire limits come from the mounted `mosquitto.conf`, not a plugin option:

| Setting | Default | Meaning |
|---|---|---|
| `max_packet_size` | 1048576 (1 MiB) | Broker PUBLISH ceiling, rendered from `WHALEREIN_MQTT_MAX_PACKET_SIZE`. Every transport profile packet budget must not exceed it. |
| MQTT 5 Receive Maximum | negotiated | Mosquitto negotiates this per connection during CONNECT/CONNACK. This repository sets no separate broker Receive Maximum key. Access advertises its configured maximum (default 16) and honors a smaller CONNACK value. |

Access sends every policy update with a revision and TTL. The broker keeps the
last acknowledged revision per connection and refuses older revisions. When no
`ping` arrives within `policy_ttl_ms`, ACL checks fail closed and new external
connections are denied until Access confirms freshness.

## Readiness

External traffic is safe when:

1. PostgreSQL is reachable and Access migrations are applied.
2. Access has an established MQTT session.
3. Access applied a broker snapshot and reconciled committed policy.
4. The broker accepted a `ping` and answered the reconciliation requests.
5. When payload storage is enabled, Access reports a writable directory with
   free-space reserve.

`/readyz` on Access reports these conditions. A broker that never receives a
policy confirmation does not authorize external clients. `/readyz` does not
report a Host's or client's relay epoch; use the Host/client diagnostics and the
[cutover gate](../README.md#coordinated-relay-cutover-epoch-4).

## Operations

- Restarting Mosquitto drops only in-memory policy and presence state. Access
  re-applies policy from PostgreSQL before external traffic resumes; clients
  reconnect with their existing credentials.
- Restarting Access reconnects with the local broker account and rebuilds
  policy and presence from PostgreSQL and a broker snapshot.
- A broken control path fails closed after the validity window. Restore the
  Access MQTT session; no manual broker state repair is needed.
- Revocation is complete only after PostgreSQL commit, a broker acknowledgement
  and the connection closure. `$CONTROL/whalerein/v1` returns the applied
  generation and the number of matched connections.
- Schema rollback: the broker itself keeps its schema, but relay epoch 4
  adds Access migrations for temporary ciphertext and request notices. The
  older Access binary rejects the newer ledger, so rollback is a whole-release
  restore, not a broker-only restart against the migrated database.

## Relay topics and ACLs

The broker policy carries the complete-message relay topics and no legacy
acknowledgement or fragment topics. Both delivery classes use the same broker
connection and the same per-connection policy:

| Principal | Publish | Subscribe |
|---|---|---|
| Access | `.../request`, `.../d/{device_id}/event`, `.../d/{device_id}/notice`, `.../d/{device_id}/live`, `.../presence`, `$CONTROL/whalerein/v1` | `.../result`, `.../event`, `.../live`, `.../d/{device_id}/ready`, device last wills, control replies/events |
| Host | own `.../result`, own `.../event`, own `.../live` | own `.../request`, own `.../presence` |
| Device | own `.../d/{device_id}/ready`, own last will | own `.../d/{device_id}/event`, own `.../d/{device_id}/notice`, own `.../d/{device_id}/live` |

The control topics use QoS 1 and the Host live topic `.../live` and Device live
topic `.../d/{device_id}/live` use QoS 0. The plugin ACL is topic-based: it
authorizes the topic entry, while the client selects the publish/subscribe QoS.
Live traffic uses only QoS 0: Access subscribes `h/+/live` and publishes the
authenticated Device's `.../d/{device_id}/live`; a Host publishes only its own
`.../live`; a Device subscribes only its own `.../live`. Access republishes the
opaque live `EventEnvelope` without a durable row, outbox entry, receipt, notice
or retry. A full Access live handoff slot or a failed QoS 0 publish drops the
preview instead of blocking control traffic. The live class adds no broker
volume, TLS route or service: it is the same connection with two extra topic
shapes.

The former `.../d/{device_id}/ack` topic does not exist and is denied in both
directions. A grant revision change removes the old ACL entries, so a revoked
Device cannot publish its ready advertisement or receive live previews. Access
registers each advertisement against the broker-derived connection generation; a
sibling reconnect does not clear another receiver's limits. Access publishes
`notice` only from a durably committed status change, and a notice is not an
execution acknowledgement.

There is no fragment or transfer-receipt topic. A PUBLISH larger than
`max_packet_size` is a broker/protocol error; Access must select inline or a
reference before publishing, never truncate.

## Rollback

Rollback is a whole-release operation across the relay epoch, not a broker-only
change:

1. Fence admission and quiesce receivers (stop Host and clients) so no peer sees
   both contracts.
2. Stop Access and Mosquitto.
3. Deploy matching older Access, broker/plugin and client artifacts with a
   verified recovery for commands admitted under the new release. An old binary
   cannot use the epoch-4 migration ledger, so a plain restart against the same
   PostgreSQL state is not a rollback; restore the coordinated pre-cut backup
   with the documented invalidation/re-enrollment, or use an operator-built
   binary that supports the new schema.
4. Verify policy readiness, revoked grants, recovered/unknown command outcomes
   and presence before reopening traffic.

A stale PostgreSQL snapshot alone cannot be used: current revocations and
unfinished command state must survive, and post-backup executions remain
unproven.

## Limits

- One active Access and one broker per line.
- Connection policy: 1024 ACL entries, 128 KiB control request body.
- The line does not add broker clustering or automatic line migration.
