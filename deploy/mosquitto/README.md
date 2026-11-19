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

`/readyz` on Access reports these conditions. A broker that never receives a
policy confirmation does not authorize external clients.

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
- Schema compatibility: this change needs no schema migration beyond the
  existing migration ledger; the rollback window keeps the same PostgreSQL
  schema and grant rows.

## Rollback

Rollback is a whole-release operation:

1. Stop Access and Mosquitto to end external admissions.
2. Start the previous release against the same PostgreSQL state. Its broker
   configuration and credentials are unchanged, so clients reconnect without
   re-pairing.
3. Verify policy readiness, revoked grants, live commands and presence before
   reopening traffic.

A stale PostgreSQL snapshot cannot be used: current revocations and unfinished
command state must survive the rollback.

## Limits

- One active Access and one broker per line.
- Connection policy: 1024 ACL entries, 128 KiB control request body.
- The line does not add broker clustering or automatic line migration.
