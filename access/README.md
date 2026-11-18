# WhaleRein Access

Internal design: [Access architecture](../docs/architecture/access.md).

WhaleRein Access is the Standalone-mode Go modular monolith for transport
authorization, one-time pairing, `HostRequest` copies, and encrypted message
relay. It owns one PostgreSQL database and one MQTT 5 broker session. It does
not execute DSH business logic and never parses encrypted payloads.

The wire contract is `proto/v1/WIRE.md` and `proto/v1/whalerein.proto`. HTTPS
uses proto3 JSON; MQTT uses binary protobuf. There is no second schema.

## Layout

```text
access/
├─ cmd/server                 process wiring, listeners, signal handling
└─ internal/
   ├─ config                  YAML + ${ENV} configuration
   ├─ httpapi                 public and internal routes, protojson
   ├─ auth                    transport authorization, ACL, revocation
   ├─ pairing                 invitations and HELLO/FINISH registration
   ├─ requests                request copy, dedupe and forwarding lease
   ├─ events                  device event routing by outer device_id
   ├─ broker                  MQTT 5 client, presence, callbacks support
   ├─ store                   versioned PostgreSQL state and migrations
   └─ diagnostics             logs, /healthz, /readyz, /metrics
```

Dependency direction: `httpapi -> auth,pairing,requests`; `requests,events ->
store,broker`; `pairing -> store,broker`; `auth -> store`; `auth` never imports
`broker`, `broker` never imports `auth`, and `httpapi` never touches
PostgreSQL or MQTT directly.

## PostgreSQL schema

All Access replicas must run the same version during a schema upgrade. Stop old
replicas before applying migration 008 and start only versions that enforce its
pairing fences; older binaries do not honor those fences. The migration invalidates
unfinished pre-upgrade pairing intents whose ordering cannot be proven; clients
must use a fresh invitation after receiving the explicit invalidation response.
Already committed pairing receipts keep their existing recovery rules.

Before migration 010, stop all older Access processes; older code does not
verify the durable COMMIT binding before trusting Host confirmation. Do not
run old and new Access binaries together. The migration ledger prevents an
older binary from starting against the newer schema. There is no lossless
downgrade procedure for the new proof state; do not remove the migration or
proof columns to bypass that check.

Pairing admission checks the original invitation, pending intent and COMMIT
deadlines again after Host replies and database lock waits. Its final transaction
check uses the later of the current Access and PostgreSQL clocks; FINISH also
preserves its earlier Access observation. A clock that runs ahead can cause early rejection;
the check does not promise that the physical database commit finishes before the
deadline.

Migration 010 also records whether request/result bodies have been cleared.
New rows start uncleared; expiry cleanup marks every eligible row even when both
bodies were already empty. Older rows have unknown retention history. A legacy
row with a nonempty request body can still accept an in-deadline result; when
the legacy request body is empty, Access withholds the result body because it
cannot distinguish an originally empty request from one already cleaned. The
status and independently verified pairing confirmation can still advance, but
the client may have to report an unknown result. The marker does not delete
credentials or infer a failed business operation.

Access retains minimal completed-pairing outcome evidence independently of
invitation, operation and Host-request cleanup. It includes only opaque IDs,
token and credential hashes, FINISH digest and grant generation. No automatic
outcome deletion endpoint is provided; records remain until an explicit
operator-controlled data removal. Global or Host-scoped revocation does not
delete them. An active grant alone does not establish Host COMMIT confirmation.
After validating a Host COMMIT acknowledgement, Access records its confirmation
under the same device revocation fence. A matching original FINISH can be
recovered after cleanup only while that original grant is active and the Host
confirmation and public-key binding are durable. The proof is derived by Access
from its trusted database state inside the device-fenced transaction; it is not
an independent cryptographic signature. Broker topic and request/result identity
checks authenticate the Host result before confirmation is recorded. COMMIT
binding and Host acknowledgement have separate durable proof flags. A legacy
confirmation is trusted only if both the retained outbox and a matching timely
Host acknowledgement can be verified, or their proofs were recorded before the
bodies were cleaned. If a valid COMMIT remains but its ACK was not recorded,
Access can retry that same COMMIT without creating another grant.
If that evidence is missing, the active outcome remains unconfirmed and the
client keeps its pending FINISH. Existing credentials and records are not
deleted merely because proof is missing.
Uncommitted legacy intents without a previously authenticated FINISH commitment
cannot be reconstructed after cleanup and remain unknown to the client.

`store.Migrate` applies checksummed migrations once under an advisory lock.
It adopts existing unversioned installations without deleting their data and
rejects modified migration history or a newer database schema. Tables:

| Table | Purpose |
|---|---|
| `hosts` | Host identity, fingerprint, protocol version |
| `devices` | Pinned device identity |
| `device_credentials` | Transport hashes; a new Host does not invalidate existing credentials |
| `pairing_operations` | Durable public handshake data and token-free completion receipts |
| `schema_migrations` | Applied migration names, checksums and timestamps |
| `grants` | Live/revoked device-to-host authorization and replay state |
| `pairing_tokens` | SHA-256 invitation hash, expiry, failure count, used_at |
| `requests` | the one request/status copy plus forwarding lease |

Important constraints and indexes:

- `device_credentials.auth_hash` unique; devices store no duplicate credential hash.
- `pairing_tokens.token_hash` primary key (unique).
- `requests (host_id, device_id, command_id)` unique where `command_id <> ''`.
- partial `requests (forward_after)` index where `forward_state = 'pending'`.
- partial `grants (host_id)` index where revoked and not replayed.
- `requests (expires_at)` index for body-clearing cleanup.

Presence remains broker-derived. No users, subscriptions or payment records
are stored. Standalone is the default. External account mode validates signed
Host and Device authorization against the configured issuer and region.

Cleanup runs every `cleanup.interval`:

- expired pairing operations and `pairing_tokens` are deleted after `pairing_token_retention`;
- expired requests lose both body columns;
- handshake/query rows are deleted in batches of 2,000 once they have been expired
  for 24 hours; control and activation records retain IDs/digests/status for
  deduplication and recovery.

## Configuration

Copy `access/config.example.yaml` to `access/config.yaml`. Secrets are
substituted from the environment with `${NAME}` or `${NAME:-default}`. `$$`
escapes a literal dollar sign. Missing required variables fail startup.

```bash
export WHALEREIN_DATABASE_URL='postgres://postgres@127.0.0.1:55432/whalerein_access?sslmode=disable'
export WHALEREIN_ADMIN_TOKEN='replace-me'
export WHALEREIN_HOST_PROVISIONING_TOKEN='replace-me-too'
export WHALEREIN_BROKER_PASSWORD='replace-me-three'
export WHALEREIN_INTERNAL_TOKEN='replace-me-four'
export WHALEREIN_BROKER_URL='tcp://127.0.0.1:1883'
```

Configuration summary:

| Key | Meaning |
|---|---|
| `mode` | `standalone` (default) or `external` with configured trust and region |
| `listen` / `internal_listen` | public and internal listener addresses |
| `public_url` | public Access URL used in pairing URIs |
| `database_url` | PostgreSQL DSN |
| `admin_token` | admin bearer for an invitation for any host, and for `DELETE /v1/devices/{id}`. A host mints its own invitation and revokes its own grant with an Ed25519 signature |
| `host_provisioning_token` | shared deployment provisioning token |
| `broker.url` | Access MQTT URL (`tcp://`, `tls://`, ...) |
| `broker.public_url` | MQTT URL advertised in `whalerein://pair?...&mqtt=...` |
| `broker.username` / `broker.password` | Access broker credential |
| `broker.ca_file` / `server_name` | MQTT TLS verification; a CA file must exist and contain a PEM certificate, and `insecure_skip_verify` is rejected for a remote broker and when a CA file is set |
| `line.policy_ttl` | Broker policy validity sent with each update |
| `line.reconcile_interval` / `heartbeat_interval` | Policy reconcile and validity refresh cadence |
| `line.operation_timeout` | Bounded broker control operation |
| `admission.max_entries` / `ttl` / `load_timeout` | Local grant cache bounds |
| `admission.max_background_loads` | Bounded asynchronous cold loads |
| `internal.callback_token` | value of `X-Whalerein-Internal-Token` |
| `pairing.*` | invitation TTL, claim timeout/TTL, failure budget |
| `requests.*` | max TTL, forward interval, lease, retry, attempts, batch |
| `revocation.ack_timeout` | host acknowledgement wait |
| `presence.cache_ttl` | broker presence cache |
| `cleanup.*` | cleanup interval and token retention |
| `protocol.version` | supported wire version; only 1-1 is accepted, and any other value fails startup with the supported range |
| `log.level` | `debug`, `info`, `warn`, `error` |

Tokens, passwords, API keys and database URLs are never logged. `Redacted()`
is the only startup summary.

If `broker.public_url` is omitted, Access derives `mqtts://<public-host>:443`
from an `https` `public_url` (or `mqtt://<host>:1883` from `http`) and logs a
warning. Set it explicitly for self-hosted brokers and official 443/ALPN.

## Docker Compose

The Access server image is built by `deploy/docker/access.Dockerfile` and is
part of `deploy/compose/stack.yaml` together with PostgreSQL 18 and Mosquitto 2.1.2:

```bash
docker compose -f deploy/compose/stack.yaml up -d --build
curl -sS http://127.0.0.1:8080/readyz
```

Access runs its embedded migration before serving traffic; no separate
`createdb`/`psql` step is needed. The compose credentials are development
defaults and must be replaced for a shared deployment.

## Local run

Go 1.26+ is required. The root `go.mod` selects Go 1.27.4 for development when toolchain switching is enabled.
If Compose Access
is already running, stop it first with
`docker compose -f deploy/compose/stack.yaml stop access` to free its listeners.

```bash
# Dependencies only; the Access process runs natively below.
docker compose -f deploy/compose/stack.yaml up -d postgres mosquitto
# The compose file creates whalerein_dev and whalerein_test.

# Validate config and apply versioned migrations.
cp access/config.example.yaml access/config.yaml
# Set database_url to:
# postgres://postgres@127.0.0.1:55432/whalerein_dev?sslmode=disable
go run ./access/cmd/server -config access/config.yaml -check-config
go run ./access/cmd/server -config access/config.yaml -migrate-only

# Run public :8080 and internal :8081.
go run ./access/cmd/server -config access/config.yaml
```

Endpoints:

```text
GET    /healthz     process liveness, always 200 while serving
GET    /readyz      database ping plus write probe and established MQTT session
GET    /metrics     Prometheus text exposition
POST   /v1/pairing/invitations
POST   /v1/pairing/claims
GET    /v1/hosts
POST   /v1/hosts/{host_id}/requests
GET    /v1/requests/{request_id}
POST   /v1/requests/{request_id}/heartbeat
DELETE /v1/devices/{device_id}
POST   /v1/hosts/{host_id}/devices/{device_id}/revocation
POST   /v1/enrollments/hosts
POST   /v1/enrollments/devices/pairing
GET    /v1/enrollments/hosts/{host_id}/devices/{device_id}
POST   /v1/enrollments/hosts/{host_id}/devices/{device_id}/revocation
GET    /v1/enrollments/hosts/{host_id}
GET    /v1/operations/{operation_id}
```

The external account endpoints are registered in every build. A Standalone
deployment answers them with `501 unsupported_capability` instead of a `404`
that would look like a mistyped path. Compare the [wire contract](../proto/v1/WIRE.md)
for message details; the authoritative route table is
[`server.go`](internal/httpapi/server.go).

Method handling is explicit:

- A known path with the wrong method is `405` with an `Allow` header.
- `OPTIONS` on a known path is `204` with `Allow`; on an unknown path it is `404`.
- `HEAD` is served by the `GET` route with status and headers but no body.
- An unknown path is always `404 not_found`.

Internal routes are bound only on `internal_listen` and require the fixed
header `X-Whalerein-Internal-Token`:

```text
POST /internal/broker/connect
```

`POST /internal/broker/connect` is the only internal route: it answers one
deferred Mosquitto CONNECT with the versioned policy text. There is no
per-operation ACL or presence callback.

### Request bodies and version skew

An endpoint that accepts a JSON body requires `Content-Type: application/json`
(a `charset` parameter is accepted). A missing or different media type is
`415 invalid_argument` before any parsing, so a form-encoded retry is not
confused with malformed JSON.

Decoding follows the cross-version wire policy:

- a field added by a newer peer is ignored, so rolling upgrades do not fail;
- an enum name or numeric value this Access version does not know is
  `400 protocol_version_incompatible` (CLI exit 9), never a silent zero value;
- malformed JSON or a wrong field type is `400 invalid_argument`;
- a body over `http.max_body_bytes` is `413 payload_too_large` (CLI exit 5).

`GET /v1/hosts` is bounded. It accepts `limit` (1-500, default 500) and
`offset` (0-1000000) and reports the page in
`X-Whalerein-Hosts-Limit`, `-Offset`, `-Returned` and `-Truncated` headers, so
a device with more grants pages instead of receiving an unbounded response.

Revocation is deterministic and idempotent where the caller can be
re-authenticated: repeated admin revocation of an existing device answers
`204`, and a fresh Host-signed revocation of an already-revoked grant answers
`204`. A device that revoked itself can no longer authenticate with that
credential, so its retry answers `401 unauthorized`; clients should treat that
as the already-revoked terminal state. A valid device token for another device
is `403 forbidden`, and an unknown target is `404 not_found`.

HTTP errors are `ErrorResponse` JSON with a stable `ErrorCode` name. Public
requests may carry `X-Whalerein-Trace-Id`; Access echoes it only when it matches
`[A-Za-z0-9._:-]{1,128}` and otherwise generates a fresh id, so client text
cannot forge log records. Access logs include that trace id but never request
or result bodies.

Example:

```bash
curl -sS -X POST http://127.0.0.1:8080/v1/pairing/invitations \
  -H "Authorization: Bearer $WHALEREIN_ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"hostId":"host-1"}'
```

The invitation request returns 404 until the host has authenticated to the
broker at least once (`hosts` row created on first auth). The pairing URI
contains `v`, `access`, optional `mqtt`, `host`, the one-time `token`, and `exp`
(the invitation expiry in whole Unix seconds, also returned as
`expiresAtUnixMs`, so clients can reject an expired link locally).

## Mosquitto line

Access expects a Mosquitto 2.1.2 broker with the WhaleRein plugin described in
[the Mosquitto line architecture](../docs/architecture/mosquitto.md). The broker
validates CONNECT, publish, subscribe, delivery and Will decisions locally; it
does not ask Access per operation.

### Deferred CONNECT validation

The plugin posts form-encoded credentials to the internal listener and installs
the returned policy for that connection:

```http
POST http://127.0.0.1:8081/internal/broker/connect
Content-Type: application/x-www-form-urlencoded
X-Whalerein-Internal-Token: <internal.callback_token>

clientid=...&username=...&password=...&will=<optional will topic>
```

The response is a versioned policy with the principal, its complete ACL set,
the monotonic revision and (for devices) the authorized Will topic:

```text
whalerein/1
ok
kind device
device dev_123
revision 7
ready 1
will whalerein/v1/presence/device/dev_123
acl subscribe 7 host_1 whalerein/v1/h/host_1/d/dev_123/event
...
```

Any other response is a denial. Access accepts `username=access` with the broker
password, `username=host:{host_id}` with the provisioning token, and
`username=device:{device_id}` with a transport token that has at least one live
grant. A Will topic outside the device policy is denied.

### Local ACL rules

| Principal | Subscribe | Publish |
|---|---|---|
| Access (plugin-local) | results, events, acks, device wills, control replies/events | own-host requests, device events, host presence, `$CONTROL/whalerein/v1` |
| Host | own `whalerein/v1/h/{host}/request` and `presence` | own `result` and own `event` |
| Device | `whalerein/v1/h/{host}/d/{device}/event` for each live grant | own `.../ack` and own Will topic |
| Probe | none | none |

Host and device topics may not contain wildcards. A revoked grant removes its
local ACL entries and closes the superseded connection.

### Presence and revocation

Primary presence comes from ordered broker connection notifications on
`whalerein/v1/control/events`. On connect, Access requests a broker snapshot with
a sequence boundary, then applies later notifications in order. A delayed
notification for a superseded connection identity is ignored. The device last
will and the Host `HostReady` result remain the existing fallbacks when a
notification is lost. Presence stays unavailable until the first snapshot
establishes current state.

Revocation installs a local Access deny before the PostgreSQL write, then denies
the matching broker scope and closes the device connection through
`$CONTROL/whalerein/v1`. Completion requires the durable commit, the broker
acknowledgement and the closure. Without a control path the broker fails closed
after its bounded policy validity window.

### Active disconnect and revocation

Access issues private control operations on `$CONTROL/whalerein/v1`:

```text
op kick
username device:{device_id}
```

The plugin disconnects matching connections and acknowledges the operation. A
Host-scoped revocation denies that exact grant scope; a device-wide revocation
denies each affected grant and then disconnects the device.

Revocation writes `grants.revoked_at` first. Access also installed a local
admission deny before the write, so event forwarding and broker policy updates
stop immediately. Access then disconnects the device, publishes
`RevocationNotice` to the host, and writes `replayed_at` only after
`RevocationAck`. On every host Ready signal it replays all pending revocations
and publishes a final empty `RevocationNotice`; both batches are acknowledged.

### TLS

For production set TLS on both links:

```yaml
broker:
  url: "tls://mqtt.internal:8883"
  ca_file: "/etc/whalerein/ca.pem"
  server_name: "mqtt.internal"
  public_url: "mqtts://access.example.com:443"
```

Nginx terminates public TLS/ALPN `mqtt` on 443 and proxies to the broker; the
self-hosted deployment exposes an explicit `mqtts://host:8883` instead. Access
itself does not terminate client MQTT TLS.

## Pairing and recovery

1. Admin or the enrolled Host creates a one-time invitation. Access stores its
   hash, Host binding, expiry and failure budget.
2. HELLO relays Noise message 1 and persists the Host's opaque message 2 reply.
   Core derives the Host fingerprint from Noise and asks for confirmation.
3. Core generates a 32-byte base64url transport credential and includes its hash
   inside Noise FINISH. The confirmed request and Host pin are saved in SecretStore
   before transmission. Access forwards the hash; the Host verifies the encrypted
   binding and durably stages the peer plus an exact FINISH replay receipt.
4. After Host authorization, Access atomically consumes the invitation, writes
   identity/credential/grant state, saves a token-free completion receipt and queues
   generation-bound COMMIT. Only confirmed Host activation completes pairing.
5. If the response is lost, run `whalerein operation recover <id>` with the same data directory
   and SecretStore. It replays the original confirmed FINISH, not a new pairing.
   Access requires the same request and a live, unexpired grant generation.

Only hashes of invitations and transport credentials are persisted on Access.
Client keys, the pending FINISH and the transport credential live in SecretStore.
The pending operation is retained when a response, credential save or config save
fails. Config and logs contain no pairing token or transport credential.

Recovery is bounded by `pairing.claim_ttl`. If that deadline passed, revoke the
old device grant through Host/admin access, explicitly forget the unfinished local
identity, obtain a fresh invitation and pair again. Losing SecretStore also
requires explicit revoke/re-enrollment; an operation ID is not recovery authority.
A Host restart before it durably processed FINISH can require a new invitation
attempt. No private Noise state is persisted by Access.

Global device revocation invalidates all its credentials. Host-scoped revocation
preserves other live grants; the last revoked grant invalidates credentials, so a
later explicitly approved repair cannot resurrect old tokens. Successful pairing
with another Host preserves earlier credentials. Repairing the only Host rotates
its credentials; a repair with other live Hosts preserves their credentials. A new invitation is a deliberate
operator-authorized re-enrollment, not automatic restoration of revoked access.

## Request forwarding

`POST /v1/hosts/{host_id}/requests`:

1. authenticates the device bearer token and live grant (HTTP layer);
2. validates `expires_at`, protocol version, kind and command fields;
3. deduplicates `(host_id, device_id, command_id)`: same digest returns the
   existing row, a different digest returns `payload_digest_mismatch` without
   forwarding;
4. if broker presence says the host is offline, writes `host_offline` with
   `forward_state=none` and publishes nothing;
5. otherwise inserts `received`/`pending`, commits, then leases due rows with
   `SELECT ... FOR UPDATE SKIP LOCKED` and publishes binary protobuf outside
   the lease transaction;
6. records `published_once`; retries use the lease and attempt budget. Rows
   that were never published become `host_offline` after exhaustion; rows that
   were published stay `received` until expiry.

`HostResult` updates only its `request_id` row, is validated against the
result topic host, and follows exactly:

```text
received      -> host_offline, rejected, host_accepted, succeeded, failed, unknown
host_accepted -> succeeded, failed, unknown
unknown       -> succeeded, failed
```

`host_offline`, `rejected`, `succeeded` and `failed` are final. `result_body`
is stored verbatim while the row has not expired. A late result may still advance the status copy, but expiry cleanup never repopulates the bodies.

## Diagnostics

- `/healthz`: process liveness only.
- `/readyz`: database ping plus an established, subscribed MQTT session.
- `/metrics`: `http_request_duration_ms`, `http_request_errors_total{code}`,
  `host_request_duration_ms`, `host_request_errors_total`,
  `mqtt_route_errors_total`, `event_route_dropped_total`, `connected_hosts`,
  `connected_devices`, `revocation_latency_ms`,
  `request_forward_retries_total`.
- Logs are JSON with `trace_id`, request diagnostic id, status and duration.
  Host/device/request ids are masked with a per-process random HMAC salt. No
  token, password, body, ciphertext or private key is logged.

## Tests

Run from the repository root:

```bash
./repo check access
./repo test access
./repo test access --coverage
```

The [repo reference](../tools/repo/README.md) defines project selection and
coverage gates; native Go tools remain available for specialized debugging.

Database and MQTT integration tests run in the single Linux test container:

```bash
./repo test storage
./repo test transport
```

Pytest owns PostgreSQL, Mosquitto, fixture callbacks, and native driver execution.
Go integration assertions use the `integration` build tag and are excluded from
unit execution. Each database test uses an isolated schema; transport fixtures
own their listeners instead of reusing development Access credentials. No opt-in
environment flags or host service setup are needed. See the
[integration guide](../tests/integration/README.md).

Production TLS listeners, reverse-proxy ALPN and live broker administration
require deployment smoke tests. Package and FakeDSH integration results do not
establish those behaviors; see [testing policy](../docs/testing.md).

### Request and revocation diagnostics

Search `trace_id` using the CLI Diagnostic ID. Host result logs use the trace
stored with the request and include `code`, `status`, `kind` and `duration_ms`.
HTTP 200 only describes transport success; `host_results_total{status,code}`
tracks applied host results with bounded enum labels.

`revocation committed` is logged before delivery, including when the host is
offline. Its `source` distinguishes `device_request` and `host_settings`.
Pairing failures now roll back atomically rather than compensating with grant
revocation. The device endpoint also accepts an administrator, so
`device_request` identifies the entry point rather than a human actor. Replay
logs include original revocation timestamps instead of implying a new revoke.

### Stable identity and logical transport

Access reassembles/acknowledges opaque logical MQTT envelopes before dispatch,
using bounded state rather than raising broker limits. Device receivers may
publish receipts only on their own active-grant ACK topic. The HTTP request
budget is 24 MiB to accommodate base64-encoded logical envelopes up to 16 MiB.

Pairing consumes the invitation and writes device/grant/activation-outbox state
in one PostgreSQL transaction. The existing requests queue retries idempotent
host COMMIT delivery until its deadline, including after Access restart. IDs
cannot be rebound to another public-key fingerprint. Pairing recovery reuses
the identity; it never revokes an older active grant as compensation.

Access requires negotiated transport-v2 capability before fragment delivery;
keep it version-aligned with the plugin and clients.
