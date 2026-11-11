# WhaleRein Access

Internal design: [Access architecture](../docs/architecture/access.md).

WhaleRein Access is the first-version Go modular monolith for transport
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
   ├─ store                   the five PostgreSQL tables and migrations
   └─ diagnostics             logs, /healthz, /readyz, /metrics
```

Dependency direction: `httpapi -> auth,pairing,requests`; `requests,events ->
store,broker`; `pairing -> store,broker`; `auth -> store`; `auth` never imports
`broker`, `broker` never imports `auth`, and `httpapi` never touches
PostgreSQL or MQTT directly.

## PostgreSQL schema

`store.Migrate` applies idempotent migrations under an advisory lock. There is
no migration metadata table because the schema intentionally contains exactly
five tables:

| Table | Purpose |
|---|---|
| `hosts` | Host identity, fingerprint, protocol version |
| `devices` | Device identity, public-key fingerprint, SHA-256 transport hash |
| `grants` | Live/revoked device-to-host authorization and replay state |
| `pairing_tokens` | SHA-256 invitation hash, expiry, failure count, used_at |
| `requests` | the one request/status copy plus forwarding lease |

Important constraints and indexes:

- `devices.auth_hash` unique.
- `pairing_tokens.token_hash` primary key (unique).
- `requests (host_id, device_id, command_id)` unique where `command_id <> ''`.
- partial `requests (forward_after)` index where `forward_state = 'pending'`.
- partial `grants (host_id)` index where revoked and not replayed.
- `requests (expires_at)` index for body-clearing cleanup.

Presence is **not** stored in PostgreSQL. There is no users, queue, presence,
heartbeat, control-plane, or migration table.

Cleanup runs every `cleanup.interval`:

- expired `pairing_tokens` are deleted after `pairing_token_retention`;
- expired `requests` keep `request_id`, `command_id`, `payload_digest` and
  status but lose both `bytea` bodies.

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
# Optional EMQX management API:
# export WHALEREIN_EMQX_API_KEY='...'
# export WHALEREIN_EMQX_API_SECRET='...'
```

Configuration summary:

| Key | Meaning |
|---|---|
| `listen` / `internal_listen` | public and internal listener addresses |
| `public_url` | public Access URL used in pairing URIs |
| `database_url` | PostgreSQL DSN |
| `admin_token` | admin bearer for an invitation for any host, and for `DELETE /v1/devices/{id}`. A host mints its own invitation and revokes its own grant with an Ed25519 signature |
| `host_provisioning_token` | shared deployment provisioning token |
| `broker.url` | Access MQTT URL (`tcp://`, `tls://`, ...) |
| `broker.public_url` | MQTT URL advertised in `whalerein://pair?...&mqtt=...` |
| `broker.username` / `broker.password` | Access broker credential |
| `broker.ca_file` / `server_name` | MQTT TLS verification |
| `emqx.rest_url` / `api_key` / `api_secret` | optional EMQX v5 REST |
| `emqx.disconnect_mode` | `ctl` (default) or `rest` |
| `emqx.ctl_path` | `emqx` command used by ctl disconnect |
| `internal.callback_token` | value of `X-Whalerein-Internal-Token` |
| `pairing.*` | invitation TTL, claim timeout/TTL, failure budget |
| `requests.*` | max TTL, forward interval, lease, retry, attempts, batch |
| `revocation.ack_timeout` | host acknowledgement wait |
| `presence.cache_ttl` | broker presence cache |
| `cleanup.*` | cleanup interval and token retention |
| `protocol.version` | supported wire version (1) |
| `log.level` | `debug`, `info`, `warn`, `error` |

Tokens, passwords, API keys and database URLs are never logged. `Redacted()`
is the only startup summary.

If `broker.public_url` is omitted, Access derives `mqtts://<public-host>:443`
from an `https` `public_url` (or `mqtt://<host>:1883` from `http`) and logs a
warning. Set it explicitly for self-hosted brokers and official 443/ALPN.

## Docker Compose

The Access server image is built by `deploy/docker/access.Dockerfile` and is
part of `deploy/compose/stack.yaml` together with PostgreSQL 18 and EMQX 5.8:

```bash
docker compose -f deploy/compose/stack.yaml up -d --build
curl -sS http://127.0.0.1:8080/readyz
```

Access runs its embedded migration before serving traffic; no separate
`createdb`/`psql` step is needed. The compose credentials are development
defaults and must be replaced for a shared deployment.

## Local run

Go 1.26+ is required. The repository has one root `go.mod`. If Compose Access
is already running, stop it first with
`docker compose -f deploy/compose/stack.yaml stop access` to free its listeners.

```bash
# Dependencies only; the Access process runs natively below.
docker compose -f deploy/compose/stack.yaml up -d postgres emqx
# The compose file creates whalerein_dev and whalerein_test.

# Validate config and apply the five-table migration.
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
GET  /healthz     process liveness, always 200 while serving
GET  /readyz      DB ping and established MQTT session
GET  /metrics     Prometheus text exposition
POST /v1/pairing/invitations
POST /v1/pairing/claims
GET  /v1/hosts
POST /v1/hosts/{host_id}/requests
GET  /v1/requests/{request_id}
DELETE /v1/devices/{device_id}
```

Internal routes are bound only on `internal_listen` and require the fixed
header `X-Whalerein-Internal-Token`:

```text
POST /internal/broker/auth
POST /internal/broker/acl
POST /internal/broker/presence
```

HTTP errors are `ErrorResponse` JSON with a stable `ErrorCode` name. Public
requests may carry `X-Whalerein-Trace-Id`; Access echoes it and includes it in
logs and errors. Request and result bodies are never logged.

Example:

```bash
curl -sS -X POST http://127.0.0.1:8080/v1/pairing/invitations \
  -H "Authorization: Bearer $WHALEREIN_ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"hostId":"host-1"}'
```

The invitation request returns 404 until the host has authenticated to the
broker at least once (`hosts` row created on first auth). The pairing URI
contains `v`, `access`, optional `mqtt`, `host`, and the one-time `token`.

## EMQX configuration

Access expects an MQTT 5 broker with HTTP authentication, HTTP authorization,
`$SYS` client lifecycle messages, and either `emqx ctl clients kick` or the
EMQX v5 REST API. The broker is the only presence source.

### HTTP authentication

```http
POST http://127.0.0.1:8081/internal/broker/auth
Content-Type: application/json
X-Whalerein-Internal-Token: <internal.callback_token>

{"clientid":"${clientid}","username":"${username}","password":"${password}"}
```

Access accepts:

- `username=access` with the configured broker password (Access MQTT client);
- `username=host:{host_id}` with `clientid=whalerein-host-{host_id}` and the
  host provisioning token; the host row is inserted on first success;
- `username=device:{device_id}` with `clientid=whalerein-device-{device_id}` and
  a transport token whose SHA-256 matches `devices.auth_hash` and which has at
  least one live grant.

A successful response is HTTP 200:

```json
{"result":"allow","is_superuser":false}
```

Everything else returns `{"result":"deny"}`. Keep `is_superuser=false` so the
ACL callback is consulted.

### HTTP authorization

```http
POST http://127.0.0.1:8081/internal/broker/acl
Content-Type: application/json
X-Whalerein-Internal-Token: <internal.callback_token>

{"clientid":"${clientid}","username":"${username}","topic":"${topic}","action":"${action}"}
```

Expected `action` values are `publish` and `subscribe`; the response is
`{"result":"allow"}` or `{"result":"deny"}`.

The rule matrix is:

| Principal | Subscribe | Publish |
|---|---|---|
| Access | `whalerein/v1/h/+/result`, `whalerein/v1/h/+/event`, `whalerein/v1/h/+/d/+/ack`, `$SYS/brokers/+/clients/+/connected`, `$SYS/brokers/+/clients/+/disconnected` | own-host `request`, `.../d/{device}/event` |
| Host | own `whalerein/v1/h/{host}/request` | own `result` and own `event` |
| Device | `whalerein/v1/h/{host}/d/{device}/event` with a live grant | own `whalerein/v1/h/{host}/d/{device}/ack` with a live grant |

Host and device topics may not contain wildcards. Cross-host subscriptions are
denied. Access requires `$SYS` permission; remove any default EMQX file ACL
rule that denies `$SYS/#` to the Access user, and allow Access to publish to
`whalerein/v1/h/.../request` and `.../d/.../event`.

### Presence

Primary presence comes from EMQX system topics. Access subscribes to:

```text
$SYS/brokers/+/clients/+/connected
$SYS/brokers/+/clients/+/disconnected
```

Optional fallbacks:

- EMQX HTTP webhook `client.connected` / `client.disconnected` posted to
  `/internal/broker/presence` with the internal header;
- EMQX v5 REST `GET /api/v5/clients/{clientid}` when `emqx.rest_url`,
  `api_key` and `api_secret` are configured;
- the host `HostReady` result message on the result topic (empty `request_id`,
  `status=succeeded`, body `HostReady`).

`$SYS` is not retained. If Access restarts while hosts stay connected, it
cannot learn those hosts until they reconnect/HostReady or until the EMQX REST
query is configured. Configure REST for restart-resilient presence.

### Active disconnect and revocation

`emqx.disconnect_mode: ctl` executes:

```text
emqx ctl clients kick whalerein-device-{device_id}
```

`emqx.disconnect_mode: rest` requires `emqx.rest_url`, `api_key` and
`api_secret` and calls:

```text
DELETE /api/v5/clients/whalerein-device-{device_id}
```

Revocation writes `grants.revoked_at` first. ACLs are computed from the live
grant, so HTTP and MQTT authorization fail from commit onward. Access then
disconnects the device, publishes `RevocationNotice` to the host, and writes
`replayed_at` only after `RevocationAck`. On every host Ready signal it replays
all pending revocations and publishes a final empty `RevocationNotice`; both
batches are acknowledged.

### TLS

For production set TLS on both links:

```yaml
broker:
  url: "tls://mqtt.internal:8883"
  ca_file: "/etc/whalerein/ca.pem"
  server_name: "mqtt.internal"
  public_url: "mqtts://access.example.com:443"
emqx:
  rest_url: "https://emqx.internal:18083"
  ca_file: "/etc/whalerein/ca.pem"
```

Nginx terminates public TLS/ALPN `mqtt` on 443 and proxies to the broker; the
self-hosted deployment exposes an explicit `mqtts://host:8883` instead. Access
itself does not terminate client MQTT TLS.

## Pairing flow

1. Admin creates an invitation for an existing host. Access stores only
   `SHA-256(token)` and returns the URI once.
2. `HELLO` verifies token, host binding, expiry, failure budget and broker
   presence, then forwards a transient `PAIRING_CLAIM` `HostRequest` with
   in-memory correlation. No `requests` row and no device/grant rows are
   written.
3. `FINISH` requires the earlier HELLO claim in memory, `confirmed=true`, and a
   connected host. After the host confirms FINISH, Access writes `devices` and
   `grants` in one transaction, generates the device transport token, and
   stores only its SHA-256.
4. Access publishes `PAIRING_AUTHORIZE`. Only after `authorized=true` is
   `pairing_tokens.used_at` written and the device token returned once.
   Authorization failure revokes the grant just written.
5. A used token is rejected forever with `used_at` retained. Every failed step
   consumes one unit of the configured failure budget.

Access restart between HELLO and FINISH loses the in-memory claim; the client
restarts pairing from HELLO. No authorization exists before that point.

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
./repo check --project access
./repo test --project access
./repo test --project access --coverage
```

The [repo reference](../tools/repo/README.md) defines project selection and
coverage gates; native Go tools remain available for specialized debugging.

Database and MQTT integration tests run in the single Linux test container:

```bash
./repo test --suite integration --group storage
./repo test --suite integration --group transport
```

Pytest owns PostgreSQL, EMQX, fixture callbacks, and native driver execution.
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
