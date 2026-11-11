# WhaleRein deployment assets

Standalone self-hosting uses one open-source Access program, PostgreSQL and EMQX.
External account mode is later work in that same program. This directory contains the Docker Compose stack only.
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

## Standalone setup and credential loss

Set `mode: standalone` in the Access YAML. No account registration, official
service, login key or license server is required. Keep one Access process per
installation. External account mode is not implemented and is rejected rather
than falling back to local authorization.

For a deployment beyond the loopback development stack, create independent random
administrator, Host provisioning, broker and callback credentials using a secret
manager or `openssl rand -hex 32`. Store them outside Git, protect the configuration
files, configure TLS and replace the matching EMQX callback/bootstrap values.
The Host provisioning credential authorizes first registration in this local
installation. Confirm the Host's Noise fingerprint when pairing each client.

If a FINISH response is lost, keep the same client data directory and SecretStore
and run `whalerein pair-resume` before `pairing.claim_ttl` expires. This never needs
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
Use a compatible binary, or restore a coordinated backup with the explicit
re-enrollment procedure. Production TLS, historical backup recovery and platform
credential-store behavior require their own environment-specific acceptance.
