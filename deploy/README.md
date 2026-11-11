# WhaleRein deployment assets

Official and self-hosted deployments use the **same programs and the same five
PostgreSQL tables**. This directory contains the Docker Compose stack only.
Core, Access and the DSH plugin do not read Compose settings.

This is the current standalone stack, not a verified multi-instance deployment.
The proposed [Access redesign](../docs/architecture/access-evolution.md) defines
future regional roles and automation separately from these working instructions.

```text
Client (whalerein CLI)
  │ HTTPS + MQTT 5
  ▼
WhaleRein Access ── PostgreSQL
  │ MQTT 5
  ▼
EMQX 5.8 ── DSH host (whalerein-dsh plugin + DSH)
```

## Layout

```text
deploy/
├─ compose/stack.yaml               # PostgreSQL 18 + EMQX 5.8 + Access
├─ compose/access.yaml              # Access config mounted into the stack
├─ compose/emqx/emqx.conf           # EMQX auth/ACL callback config
├─ compose/emqx/api-keys.csv        # EMQX API key bootstrap
├─ docker/access.Dockerfile         # Access server image
└─ postgres/init/01-test-db.sql     # creates whalerein_test on a fresh volume
```

## One-command stack

```bash
docker compose -f deploy/compose/stack.yaml up -d --build
docker compose -f deploy/compose/stack.yaml ps
curl -sS http://127.0.0.1:8080/readyz
```

Compose owns server-stack lifecycle. Run `up -d --build` to check current image
inputs through Docker's build cache. For native Access debugging, start only its
dependencies with `docker compose -f deploy/compose/stack.yaml up -d postgres emqx`
and follow [Access development](../access/README.md). Project processes are not
started by the repository task tool.

The published ports are loopback-only:

```text
Access HTTP      http://127.0.0.1:8080
Access internal  127.0.0.1:8081
PostgreSQL 18    127.0.0.1:55432
EMQX MQTT        127.0.0.1:1883
EMQX dashboard   127.0.0.1:18083
```

Stop the stack while preserving PostgreSQL and EMQX data:

```bash
docker compose -f deploy/compose/stack.yaml down
```

## PostgreSQL

Access owns the schema (`access/internal/store/migrations`, five tables:
`hosts`, `devices`, `grants`, `pairing_tokens`, `requests`). The Compose
service uses `postgres:18-alpine` with trust authentication on the Docker
network and creates `whalerein_dev`. `postgres/init/01-test-db.sql` creates
`whalerein_test` on a fresh data volume. Access applies `001_init.sql` at
startup before `/readyz` turns green.

```bash
docker compose -f deploy/compose/stack.yaml up -d postgres
docker compose -f deploy/compose/stack.yaml stop postgres
```

## EMQX

`deploy/compose/emqx/emqx.conf` configures:

- MQTT 5 plaintext listener on 1883. TLS and WebSocket listeners are disabled.
- HTTP authentication to `POST /internal/broker/auth`.
- HTTP ACL to `POST /internal/broker/acl`, fail-closed (`no_match = deny`).
- API key bootstrap from `deploy/compose/emqx/api-keys.csv`. Access uses it to
  disconnect a revoked device.

The callback token in that file must match `WHALEREIN_INTERNAL_TOKEN` on the
Access service. The Compose file sets both.

## Access

The image is built from `deploy/docker/access.Dockerfile`. Its config is
`deploy/compose/access.yaml`. Secrets come from the Compose environment:

| Variable | Purpose |
|---|---|
| `WHALEREIN_DATABASE_URL` | PostgreSQL URL |
| `WHALEREIN_ADMIN_TOKEN` | Administrator bearer token for invitations |
| `WHALEREIN_HOST_PROVISIONING_TOKEN` | Pre-shared token a host uses for broker auth |
| `WHALEREIN_BROKER_PASSWORD` | Access's own broker credential |
| `WHALEREIN_INTERNAL_TOKEN` | Fixed header shared with EMQX callbacks |
| `WHALEREIN_EMQX_API_KEY` / `WHALEREIN_EMQX_API_SECRET` | REST disconnect credentials |

Development defaults are in `deploy/compose/stack.yaml`. Replace them before
sharing the stack.

## Smoke tests

```bash
curl --fail --silent --show-error http://127.0.0.1:8080/healthz
curl --fail --silent --show-error http://127.0.0.1:8080/readyz
pg_isready -h 127.0.0.1 -p 55432
```

`/healthz` reports process liveness. `/readyz` additionally requires PostgreSQL
and an established, subscribed MQTT connection after startup migration.

## MQTT packet budget

The committed EMQX ceiling is 1 MiB. Current client/relay source uses packets
below 64 KiB to carry logical envelopes up to 16 MiB. The broker ceiling,
frame size, and reconstructed-envelope size are different limits. See the
[wire budget](../proto/v1/WIRE.md#transport-size-budget) for exact values and ACK
routing. After changing broker/client limits, reconnect clients to renegotiate.
Verify interoperability across Access, plugin, and clients before rollout.

Upgrade Access, the plugin, and clients together before lowering any broker
override. Reconnect clients after changing negotiated transport limits.
