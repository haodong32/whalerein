# WhaleRein deployment assets

Standalone self-hosting uses one open-source Access program, PostgreSQL and Mosquitto.
External account mode is later work in that same program. This directory contains the Docker Compose stack only.
Core, Access and the DSH plugin do not read Compose settings.

This is the current standalone stack, not a verified multi-instance deployment.
The proposed [Access redesign](../docs/architecture/access-evolution.md) defines
future regional roles and automation separately from these working instructions.
Shutdown, bounded log rotation, request-retention capacity, PostgreSQL failure
handling, backup/restore validation and the rolling-upgrade window are documented
in [deployment operations](../docs/deploy.md).

```text
Client (whalerein CLI)
  │ HTTPS + MQTT 5
  ▼
WhaleRein Access ── PostgreSQL
  │ MQTT 5 (data + private $CONTROL)
  ▼
Mosquitto 2.1.4 + WhaleRein plugin ── DSH host (whalerein-dsh plugin + DSH)
```

## Layout

```text
deploy/
├─ compose/stack.yaml               # PostgreSQL 18 + Mosquitto + Access
├─ compose/access.yaml              # Access config mounted into the stack
├─ mosquitto/mosquitto.conf         # Broker and plugin options
├─ mosquitto/plugin/                # Native broker plugin (C)
├─ docker/mosquitto.Dockerfile      # Pinned Mosquitto image with the plugin
├─ docker/mosquitto-entrypoint.sh   # Materializes broker-local secrets
├─ docker/access.Dockerfile         # Access server image
├─ tests/rolling-upgrade-matrix.sh  # static + Docker migration preflight
└─ postgres/init/01-test-db.sql     # creates whalerein_test on a fresh volume
```

The Compose stack mounts a named `whalerein-access-payloads` volume at
`/var/lib/whalerein/payloads` for disposable node-local ciphertext. It is not a
backup target: PostgreSQL owns all admission, expiry and command state.

## One-command stack

Access and Mosquitto share one private control path. The broker plugin validates
every external CONNECT asynchronously against Access `POST /internal/broker/connect`
and installs the returned policy for that connection. Access manages policy,
presence snapshots and targeted disconnects over the private
`$CONTROL/whalerein/v1` topic and receives ordered connection notifications on
`whalerein/v1/control/events`. The internal Access MQTT identity authenticates
locally against `WHALEREIN_BROKER_PASSWORD`, so startup and recovery never depend
on Access's own HTTP listener. Policy validity is 15 s and Access refreshes it
every 5 s; a lost control path therefore fails closed instead of serving stale
authority.

```bash
docker compose -f deploy/compose/stack.yaml up -d --build
docker compose -f deploy/compose/stack.yaml ps
curl -sS http://127.0.0.1:8080/readyz
```

Compose owns server-stack lifecycle. Run `up -d --build` to check current image
inputs through Docker's build cache. For native Access debugging, start only its
dependencies with `docker compose -f deploy/compose/stack.yaml up -d postgres mosquitto`
and follow [Access development](../access/README.md). Project processes are not
started by the repository task tool.

The published ports are loopback-only:

```text
Access HTTP      http://127.0.0.1:8080
Access internal  127.0.0.1:8081
PostgreSQL 18    127.0.0.1:55432
Mosquitto MQTT   127.0.0.1:1883
```

Stop the stack while preserving PostgreSQL data (broker state is disposable):

```bash
docker compose -f deploy/compose/stack.yaml down
```

`down` keeps the named `whalerein-access-payloads` volume. Its ciphertext is
disposable and is not part of backup/restore; remove the volume explicitly only
when decommissioning the node.

## PostgreSQL

Access owns its versioned schema in `access/internal/store/migrations`.
`schema_migrations` records names and checksums. The schema includes the local
workspace, identities, credential hashes, grants, pairing operations and requests. The Compose
service uses `postgres:18-alpine` with trust authentication on the Docker
network and creates `whalerein_dev`. `postgres/init/01-test-db.sql` creates
`whalerein_test` on a fresh data volume. Access applies pending migrations at
startup before `/readyz` turns green.

```bash
docker compose -f deploy/compose/stack.yaml up -d postgres
docker compose -f deploy/compose/stack.yaml stop postgres
```

## Mosquitto

`deploy/mosquitto/mosquitto.conf` configures:

- MQTT 5 plaintext listener on 1883 with anonymous access disabled.
- The WhaleRein broker plugin built from `deploy/mosquitto/plugin`, pinned to
  Mosquitto 2.1.4 headers and image.
- Deferred CONNECT validation against `POST /internal/broker/connect`.
- Broker-local ACL enforcement for publish, subscribe, delivery and Will.

Device clients connect with a 10s MQTT keepalive and a non-retained QoS 1 last
will on `whalerein/v1/presence/device/<device_id>`. The broker validates the Will
topic against the connection policy and denies a topic outside it. Access drains
device offline events from ordered broker notifications and keeps the existing
device last-will and HostReady fallbacks when a notification is lost. Hosts
subscribe only to their exact presence topic; devices may publish only their own
will topic, and neither gets a wildcard permission.

The broker reads `WHALEREIN_BROKER_PASSWORD` and `WHALEREIN_INTERNAL_TOKEN` from
its container environment; `deploy/docker/mosquitto-entrypoint.sh` materializes
them as root-owned, read-only files under `/tmp` before the broker starts. The
same values are mounted into Access. A device credential never appears in broker
configuration.

## Access

The image is built from `deploy/docker/access.Dockerfile`. Its config is
`deploy/compose/access.yaml`. Secrets come from the Compose environment:

| Variable | Purpose |
|---|---|
| `WHALEREIN_DATABASE_URL` | PostgreSQL URL |
| `WHALEREIN_ADMIN_TOKEN` | Administrator bearer token for invitations |
| `WHALEREIN_HOST_PROVISIONING_TOKEN` | Pre-shared token a host uses for broker auth |
| `WHALEREIN_BROKER_PASSWORD` | Access's own broker credential |
| `WHALEREIN_INTERNAL_TOKEN` | Fixed header shared with the broker plugin |
| `WHALEREIN_TRANSPORT_PROFILE` | Packaged transport profile (`default`, `compact` or `roomy`); must match the Host and clients |
| `WHALEREIN_MQTT_MAX_PACKET_SIZE` | Rendered Mosquitto PUBLISH ceiling (default 1 MiB) |

Development defaults are in `deploy/compose/stack.yaml`. Replace them before
sharing the stack.

## Smoke tests

```bash
curl --fail --silent --show-error http://127.0.0.1:8080/healthz
curl --fail --silent --show-error http://127.0.0.1:8080/readyz
pg_isready -h 127.0.0.1 -p 55432
```

`/healthz` reports process liveness. `/readyz` additionally requires PostgreSQL
(including a write probe), an established subscribed MQTT session, a reconciled
broker presence snapshot, a confirmed policy reconciliation, and — when
`storage.enabled` is true — a writable payload directory with at least
`storage.free_space_reserve_bytes` free. It does not prove the relay epoch of a
connected Host or client, ingress TLS, or broker ACL correctness; those need the
epoch and policy checks in [the cutover procedure](#coordinated-relay-cutover-epoch-5).

## Transport operations

`proto/v1/transport_profiles.json` is the single packaged profile vocabulary
shared by Access, the Host runtime and clients. The standalone Compose stack
selects a profile in [`compose/access.yaml`](compose/access.yaml) through
`WHALEREIN_TRANSPORT_PROFILE` and may override individual values. Access, the
Host and every client must select the same profile; a mismatch is a coordinated
hard-cut failure, not a silent downgrade.

| Limit | Default profile | Owner and behavior |
|---|---|---|
| Complete encoded MQTT packet | 64 KiB | Access, Host, Core and broker. The whole PUBLISH counts, not only ciphertext. |
| Logical envelope | 16 MiB | Access, Host, Core and clients. Independent of packet size. |
| Bulk HTTPS transfers / reserved ciphertext | 4 / 32 MiB | Access admission and HTTP. |
| Upload or fetch timeout | 120 s | Access transport and relay directory. |
| Receive Maximum / send in-flight | 16 / 16 | Access MQTT 5 CONNECT and ordered control publisher. A CONNACK value can only lower the in-flight bound. Live QoS 0 has no in-flight acknowledgement. |
| Per-destination queue / waiting bytes | 1024 / 32 MiB | Access ordered control publisher. Exhaustion is a typed error, never a blocked sibling flow. |
| Retained encrypted result | 16 MiB | Access HTTP/storage; derived from the profile envelope. |
| HTTP result body | 24 MiB | Access HTTP and ingress; base64 expansion plus JSON framing. |
| Broker PUBLISH ceiling | 1 MiB | Mosquitto `max_packet_size`; equals packaged `brokerPacketLimitBytes`. |

Both delivery classes use the same authenticated broker connection and carry
exactly one complete message per MQTT PUBLISH. Control is QoS 1: ordered,
acknowledged and durable, with the inline-or-HTTPS-reference representation.
Live is QoS 0: fire-and-forget, lossy and coalescible, never a reference, never
durable, and superseded by the final committed message.

```text
whalerein/v1/h/{host_id}/request        control
whalerein/v1/h/{host_id}/result         control
whalerein/v1/h/{host_id}/event          control
whalerein/v1/h/{host_id}/live           live
whalerein/v1/h/{host_id}/d/{device_id}/event   control
whalerein/v1/h/{host_id}/d/{device_id}/notice  control
whalerein/v1/h/{host_id}/d/{device_id}/ready   control
whalerein/v1/h/{host_id}/d/{device_id}/live    live
```

Access publishes `request`, the Device control topics (`event`, `notice`) and
the Device `live` topic; a Host publishes its own `result`, `event` and `live`;
a Device publishes its own `ready` `ReceiveAdvertisement` after each
authenticated connect and budget change. Access registers each advertisement
against the broker-derived connection generation. There is no acknowledgement
topic. Access republishes a Host `live` preview unchanged to the authenticated
Device `live` topic at QoS 0, with no durable row, outbox entry, receipt or
retry; on local backpressure it drops the preview rather than delaying control.

Run `whalerein-access --check-config --config <file>` to print the resolved
`transport:` and `storage:` lines. It fails before any listener opens for an
unknown profile, a non-positive or overflowing value, a packet budget above
`brokerPacketLimitBytes`, an envelope below the packet budget, reserved bytes
below one envelope, invalid queue bounds, invalid storage limits, a
`storage.upload_timeout` above `storage.retention`, or an unwritable payload
directory. Configuration changes take effect on process restart only; reconnect
clients after a change so both directions renegotiate.

The limits are independent and must stay aligned:

1. **Broker PUBLISH ceiling.** `WHALEREIN_MQTT_MAX_PACKET_SIZE` (default
   1048576) is rendered by `docker/mosquitto-entrypoint.sh`. The packaged
   `brokerPacketLimitBytes` is also 1 MiB, and every profile packet budget must
   not exceed it. Rendering a lower broker value lowers the effective hop;
   Access takes the minimum of its profile, the packaged ceiling and the
   broker's CONNACK `maximum_packet_size`.
2. **Packet and envelope budgets.** Access `transport:` and the matching Host
   and client profiles. An inline body that does not fit the complete encoded
   packet is stored and sent as a reference, provided the reference itself fits.
3. **HTTP result body.** `http.max_body_bytes`, derived when omitted. A reverse
   proxy in front of Access must allow at least this body size and the relay
   upload bound below.
4. **Retained result bounds.** `requests.max_result_bytes` must not exceed the
   logical envelope, and `http.max_body_bytes` must fit its encoded form.
   Access refuses a stored result that cannot be returned without changing its
   durable outcome.

### Temporary ciphertext directory

Oversized relay payloads and attachment parts use the Access node's payload
volume. The Compose config enables it with development values that match the
packaged default profile:

| Key | Compose development value | Meaning |
|---|---|---|
| `storage.enabled` | `true` | Admission is refused when the directory is unavailable. |
| `storage.root_dir` | `/var/lib/whalerein/payloads` | Dedicated quota directory; Access creates `objects/` and `tmp/`. |
| `storage.node_id` / `owner_id` | `access-1` / `access-1` | Stable node identity and opaque reference owner. Keep `node_id` unique per node. |
| `storage.max_object_bytes` | 16777216 (16 MiB) | One ciphertext object; keep it at or below the profile envelope (16 MiB). |
| `storage.max_host_bytes` | 268435456 (256 MiB) | Charged bytes for one Host. |
| `storage.max_node_bytes` | 1073741824 (1 GiB) | Charged bytes for the node. |
| `storage.max_objects` | 1024 | Reserved, ready and expired objects combined. |
| `storage.max_active_uploads` | 16 | Concurrent upload bodies; keep the profile `bulk_concurrency` within it. |
| `storage.free_space_reserve_bytes` | 1073741824 (1 GiB) | No admission below this free-space headroom. |
| `storage.retention` | 24h | Hard lifetime cap; a parent deadline can only shorten it. |
| `storage.cleanup_interval` / `batch` | 30s / 100 | Bounded expiry cleanup. |
| `storage.upload_timeout` / `read_timeout` | 2m / 60s | One upload or fetch; upload timeout must not exceed retention. |

The profile envelope remains the upload bound; keep `max_object_bytes` at or
below it. Access does not currently cross-check that relation at startup. If
storage is disabled, `/readyz` has no storage check and any message that needs a
reference fails at admission; keep it enabled whenever the profile envelope can
exceed the packet budget. These are development values, not a capacity plan. Size `max_node_bytes` plus
`free_space_reserve_bytes` inside the provisioned volume, and keep the directory
on one dedicated filesystem. Do not place the payload directory on a bind mount
that the unprivileged Access user cannot write; the Compose image creates the
path with matching ownership for a new named volume. A pre-existing volume or
bind mount must be chowned to the Access container user before startup.

The five routes are:

```text
POST   /v1/relay/objects
PUT    /v1/relay/objects/{object_id}
POST   /v1/relay/objects/{object_id}/finish
GET    /v1/relay/objects/{object_id}
DELETE /v1/relay/objects/{object_id}
```

Every request carries relay epoch 5; `PUT`, `GET` and `DELETE` use the
`WhaleRein-Relay-Epoch: 4` header. Host authority uses the existing Ed25519 Host
key; a Device uses its bearer token and bound grant/session/channel/purpose
scope. A reference is published only after the exact ciphertext is readable. A
wrong-node request returns `409 conflict` with `reason=wrong_owner_path` and the
authoritative `WhaleRein-Owner-Node` header.

### Public ingress requirements (operator inputs)

The repository contains no production ingress template. The operator must
provide and verify all of the following for the public Access origin:

1. **TLS termination** for `public_url`, with the certificate/trust chain used by
   Hosts and clients. The Compose loopback HTTP stack is development-only.
2. **Route exposure** for all five relay routes under the same origin (or their
   node-scoped owner base path), preserving method, body, headers and streaming
   responses.
3. **Body-size limits** at least:
   - `max(http.max_body_bytes, storage.max_object_bytes)` for the origin, so
     `POST` reserve/finish JSON up to the envelope and `PUT` ciphertext up to
     one object are accepted;
   - the encoded HTTP result body bound for ordinary result reads.
   A proxy left at nginx's 1 MiB default rejects normal envelopes.
4. **Request/response timeouts** at least the configured
   `storage.upload_timeout` and `storage.read_timeout` (plus a small grace),
   and at least the profile `transfer_timeout`. Access raises its own HTTP
   read/write bounds for storage routes, but an edge that closes earlier still
   breaks an upload or fetch.
5. **Node-scoped owner base paths** for a multi-node deployment. A reservation
   returns `owner_node_id` and `upload_path`; the caller resolves that owner to
   its configured base path (for example a per-node host or an ingress prefix).
   A single node can serve its own path directly. The repository has no
   multi-node ingress, DNS or certificate automation.

The same operator decision covers broker TLS (`mqtts://`), certificates, DNS,
and credential distribution. Only the loopback plaintext MQTT/HTTP ports are
defined in this repository.

### Flow control and observability

The control class is QoS 1 with the negotiated Receive Maximum and connection
flow control. Each Access connection has one FIFO queue per control destination
topic, a bounded global in-flight semaphore and bounded waiting bytes, so a
blocked bulk flow cannot stall an unrelated small message. A failed control
publication is retried only at the same complete-message identity; duplicates
are handled by the existing `request_id`, `command_id`/digest and typed
event-sequence rules.

The live class is QoS 0 on the two live topics and bypasses the control queue.
The Host keeps a bounded per-connection live queue (64 packets / 256 KiB) and
coalesces text on a 20 ms window; when the queue bound is reached it drops the
preview instead of blocking control. Access bounds concurrent live handoffs and
drops on local backpressure. No live preview is written to a durable row,
outbox, receipt or notice, and none waits for an acknowledgement.

Operational signals: `transport_rejections_total{code}`,
`transport_rejected_bytes_total{code}`, the relay queue/staging counters,
`live_preview_dropped_total{reason}` (Access side; bounded reasons
`disconnected`, `backpressure`, `publish`), and `payload_free_bytes`,
`payload_headroom_bytes`, `payload_ready_bytes`, `payload_reserved_bytes`,
`payload_expired_backlog_seconds`, `payload_admission_refusals_total`,
`payload_cleanup_*` and `payload_reconcile_actions_total`. Alert on sustained
live drops if preview latency matters; they never indicate control loss. No
metric label contains a host, device, request, topic, object path or content
identity.

Validate the static deployment inputs and the schema matrix with
`deploy/tests/rolling-upgrade-matrix.sh`. The static preflight runs without
Docker and checks the storage volume/config, relay topics, profile/broker
alignment and epoch constants. The Docker portion runs `--check-config`, checks
the rendered geometry, and proves an old binary rejects the newer migration
ledger.

## Coordinated relay cutover (epoch 5)

This release replaces the MQTT relay contract in one hard cut. Application
protocol stays v1, but `relay_version=5` is the only accepted relay epoch. Every
Access, broker ACL/plugin, Host, Core, CLI, mobile binding and client artifact
must come from the same release revision. An epoch-3 or epoch-4 peer is rejected
with `protocol_version_incompatible`; there is no mixed rollout.

### Supported artifact set

Build every artifact below from one release revision:

- Access binary or `whalerein-access` image (`deploy/docker/access.Dockerfile`);
- Mosquitto image and WhaleRein plugin (`deploy/docker/mosquitto.Dockerfile`,
  `deploy/mosquitto/`), including the epoch-5 control, query and direct-preview
  topic policy;
- shared Host runtime and DSH adapter (`host/runtime`, `host/dsh`);
- Core library and CLI (`core`, `apps/cli`);
- mobile bindings and any shipped iOS build (`apps/mobile-bindings`,
  `apps/ios`);
- `proto/v1/transport_profiles.json` and the generated Go/Rust/Node profile
  artifacts from the same revision.

These paths are the supported client/runtime set in this repository. Other
builds are not covered by this contract. Pairing credentials, Noise identities,
pins, grants and command-recovery identities survive the cut; relay epoch 5
changes the transport representation and adds the transient query lane and
direct previews, not those application identities.

### 1. Prepare inputs

1. Provision the payload volume/filesystem, permissions and capacity from the
   table above. Confirm free space exceeds `max_node_bytes` plus the reserve.
   The query and preview classes add no volume, filesystem or capacity
   requirement.
2. Configure production TLS, the public HTTPS origin, the five relay routes,
   body limits and upload/read timeouts. Configure broker TLS and hostnames.
   The query and preview classes add no TLS route, listener or service; they use
   the existing broker connection.
3. Set the same transport profile (or the same explicit overrides) on Access,
   Host and clients. Keep every packet budget at or below the broker ceiling.
4. Deploy the matching broker configuration/plugin and confirm the nine relay
   topics: `request`, `result`, `event`, device `event`, `notice`, `ready`,
   `query`, `reply` and `live`, with no Host `live` topic and no acknowledgement
   topic. Clients use QoS 1 control/query and direct QoS 0 previews; the plugin
   ACL is topic-based. Do not duplicate or re-derive ACLs outside the
   authenticated policy. Confirm the plugin stamps `whalerein-connection` and
   rejects a client-supplied copy.
5. Record the release artifact digests and take a coordinated PostgreSQL backup
   using [Backup and restore](#backup-and-restore).
6. Record unresolved operations: `request_id`, `command_id`, `payload_digest`,
   status and whether native execution is possible. Do not resend them with new
   identities.

### 2. Quiesce old admissions and drain transient state

Stop accepting new client submissions (at the ingress or by stopping clients)
and keep the old Access and Host running long enough for in-flight requests to
reach a terminal state or expire to `unknown`. The relay keeps no durable
fragment directory to migrate. Record unresolved operations before forcing them
terminal, then stop the old Host and Access and keep PostgreSQL. Do not delete or
rewrite grants, keys, pins, pairing records, command identities or
proved/unknown outcomes. The new Access never replays an acknowledgement or a
delivery admitted under another epoch.

### 3. Deploy the epoch-5 set

Start PostgreSQL, then the broker, then Access with the prepared config. Wait for
`/readyz` to report database, broker and storage readiness. Start the Host
runtime/DSH plugin, then the supported clients. Start order prevents a Host or
client from establishing a policy under an incomplete broker.

### 4. Gate readiness on the matching epoch

All of these must hold before reopening traffic:

- `/readyz` is green (database write probe, MQTT session, presence, policy and
  storage headroom);
- Access accepted the Host's `HostReady` with `relay_version=5` and a valid
  `ReceiveAdvertisement`;
- each client connected with epoch 5, subscribed its own `event`, `notice`,
  `reply` and `live` topics, and published `ready`; Access registered the new
  connection generation and Broker connection reference;
- an epoch-3/epoch-4 client or other old artifact is rejected at admission with
  `protocol_version_incompatible` and no business delivery;
- the broker policy shows the nine topics, the deployed peers use QoS 1
  control/query and direct QoS 0 previews, and no acknowledgement or Host live
  topic is accepted;
- a session query reaches Host over the query lane and its reply only reaches the
  admitted device connection; the retired post-pairing HTTPS handshake and query
  polling routes fail before dispatch;
- payload free space remains above the reserve.

`/readyz` alone does not prove the Host or client epoch, ingress TLS, body limits
or broker ACLs. This repository has no standalone epoch-readiness endpoint, so
record the Host/client epoch evidence from their diagnostics and the broker
policy test instead of inferring it from `/readyz`. Observe
`live_preview_dropped_total{reason}` for direct previews: drops show preview
loss only and never indicate control loss.

### 5. Reattach notifications and recover

Reconnect Host and clients, let each Device publish a fresh `ready`
advertisement, and rerun the original watch/history reads. A foreground command
that was interrupted is recovered by its original identity with
`whalerein operation recover <request-id>` (or the equivalent Core
`GetCommandResult` path); do not resend it with a new ID. Reconcile notifications
from authoritative state and treat missing bodies as `unknown`/`payload_too_large`
according to the terminal status, not as success.

### Rollback

A rollback that runs both contracts is unsupported. A rollback requires all of the
following, in order:

1. Fenced admission: stop new submissions and keep the old/new boundary closed.
2. Quiesced receivers: stop Host and clients so no receiver observes both
   contracts.
3. Matching old artifacts and broker policy for the entire set.
4. A verified recovery path for commands admitted under the new release.

Because epoch 5 applies the new migrations, an old binary refuses the newer
migration ledger. Rollback therefore means restoring the coordinated pre-cut
backup with the matching older artifacts and running the documented restore
invalidation/re-enrollment, or deploying an operator-built binary that supports
the new schema. It is not an in-place binary swap.
Any command executed after that backup cannot be proved by the restored
database: preserve its original identity and report `unknown` rather than
claiming it did not run.

### Verified versus operator procedure

Verified in this repository: configuration parsing and validation for the
storage and transport keys, storage readiness/headroom behavior, broker topic and
ACL policy tests, relay epoch rejection, the static deployment preflight, and the
migration-adjacency gate. The Docker matrix additionally proves an old binary
rejects a newer ledger.

Operator procedure, not proven here: production TLS termination, DNS and
multi-node owner paths, ingress body/timeout configuration, volume provisioning
and real disk-pressure drills, coordinated backup/restore of a live installation,
broker HA, Cloud adoption inputs, and the AC-19 netem measurement. Treat the
procedure as a rehearsal checklist until it has been executed on the target
environment.

### Dependent Cloud adoption inputs

Cloud adoption is a separate deployment change. It depends on these inputs,
which are not defined in this repository:

- public HTTPS and MQTT(S) endpoints, certificates and DNS for each Access node;
- a node storage contract (volume class, size, node identity, free-space
  reserve, snapshot/backup and disposal policy);
- per-node owner base paths and routing for the five relay routes;
- quota, TLS and body/timeout ingress policy mapped to the settings above;
- the artifact registry/release digests and supported client distribution set;
- external account service trust and regional identity configuration.

Do not infer Cloud endpoints, storage classes or paths from the standalone
Compose values.

## Standalone setup and credential loss

Set `mode: standalone` in the Access YAML. No account registration, official
service, login key or license server is required. Keep one Access process per
installation. Official Cloud regions use explicit external account mode with
the configured account service trust and regional identity.

For a deployment beyond the loopback development stack, create independent random
administrator, Host provisioning, broker and callback credentials using a secret
manager or `openssl rand -hex 32`. Store them outside Git, protect the configuration
files, configure TLS and replace the matching broker and Access credentials.
The Host provisioning credential authorizes first registration in this local
installation. Confirm the Host's Noise fingerprint when pairing each client.

If a FINISH response is lost, keep the same client data directory and SecretStore
and run `whalerein operation recover <id>` before `pairing.claim_ttl` expires. This never needs
the invitation URI on the command line again. If the pending operation expired or
SecretStore was lost, revoke the old device through Host/admin access, then
explicitly forget the local identity and use a fresh invitation. Deleting local
files is not remote revocation. Lost administrator/provisioning secrets are
replaced by the deployment operator through configuration; Access has no account
password-reset endpoint.

## Backup and restore

Protect and back up all of these independently:

- PostgreSQL, including the migration ledger, grants, revocations, pairing
  operations and request/outcome records. Use PostgreSQL backup tools and verify
  restoration in an isolated installation.
- Host state, including peer keys, revocation generations and command deduplication
  records, plus the Host's private identity in its configured credential store.
- Client SecretStore and configuration if local identity recovery is required.
  Access backups cannot reconstruct device private keys or plaintext credentials.
- Deployment secrets and TLS trust material, outside the application database.

For a coordinated backup, stop Access and the Host plugin before taking endpoint
state snapshots, and preserve their provenance together. Restart recovery with
retained state is distinct from restoring an older backup: an old database may
predate a security revocation or a completed command. Never expose such a restore
to clients immediately or blindly resume its queued requests.

The conservative historical-restore procedure is:

1. Stop Access and the Host plugin; restore PostgreSQL into an isolated target.
   Restore/preserve Host command deduplication records and identity. Do not use a
   command journal older than the executions whose outcomes must be recovered.
2. Before starting Access, invalidate restored credentials and invitations and
   stop pending forwarding in one transaction, using the SQL below.
3. Verify configured trust/endpoints and migration history, then start Access and
   the Host plugin. Revocation replay must complete before new Host channels.
4. Re-enroll clients using fresh device identities and new invitations. Do not
   silently lower a Host's stored revocation generation to make an old identity
   work. Preserve uncertain command records for explicit source-side review;
   restoring state is not proof that a control never executed.

Run only against the restored Access database/schema, never an active installation:

```sql
BEGIN;
UPDATE grants SET revoked_at = COALESCE(revoked_at, now()),
                  replayed_at = NULL, updated_at = now();
DELETE FROM device_credentials;
DELETE FROM pairing_operations;
UPDATE pairing_tokens SET used_at = COALESCE(used_at, now()), updated_at = now();
UPDATE requests SET forward_state = 'none',
    status = CASE WHEN status IN ('received', 'host_accepted') THEN 'unknown' ELSE status END,
    updated_at = now();
COMMIT;
```

Do not edit applied migrations or remove ledger entries to force a binary rollback.
Use an operator-built binary that supports the new schema, or restore a
coordinated backup with the explicit
re-enrollment procedure. Production TLS, historical backup recovery and platform
credential-store behavior require their own environment-specific acceptance.
