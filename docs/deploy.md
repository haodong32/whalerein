# Deployment operations and capacity

This guide covers the operational behavior of the Compose stack in
[deploy/README.md](../deploy/README.md): shutdown, log growth, retention,
PostgreSQL failure modes, backup/restore validation, and the upgrade window. It
does not replace environment-specific release validation for production TLS,
platform credential stores or a multi-instance deployment.

## Graceful shutdown

Access handles `SIGTERM` by stopping its public and internal HTTP servers, then
cancelling the broker, forwarder, line and cleanup workers. `runServices` gives
those components a 10 s budget. A clean stop is an exit code of `0` and the
`access stopped` log record.

Docker's default stop timeout is 10 s, which would race the application budget
and can end in `SIGKILL` (exit code `137`). The Compose service therefore sets:

```yaml
stop_grace_period: 20s
```

Use the container stop path, not a bare `docker kill`:

```bash
docker stop --time 20 whalerein-access
docker start whalerein-access
```

Acceptance evidence: `TestGracefulShutdownAndLogRotation` inserts an in-flight
durable request, polls it while `SIGTERM` arrives, asserts the stop completes
inside the 10 s budget, the container exits `0` (not `137`), the shutdown log
record exists, and the in-flight row is neither settled nor cleared. It also
asserts that the Access log does not contain the admin, Host provisioning,
device, broker or internal tokens.

## Log growth and rotation

Every Compose service uses the Docker `json-file` driver with a bounded
configuration:

| Service | `max-size` | `max-file` | Upper bound per container |
|---|---|---|---|
| `access` | 10 MiB | 3 | 30 MiB |
| `mosquitto` | 10 MiB | 3 | 30 MiB |
| `postgres` | 10 MiB | 3 | 30 MiB |

Verify the effective configuration after `up`:

```bash
docker inspect --format '{{json .HostConfig.LogConfig}}' whalerein-access
```

The configuration is part of `deploy/compose/stack.yaml`; changing it is a
deployment change and should be recorded with the size rationale. The Access
Compose profile logs at `debug` for local diagnosis. A production deployment
should lower `log.level` and ship logs to an operator-owned collector that has
its own retention; the container bound prevents disk exhaustion but does not
preserve a long audit history.

Logs must never carry message contents, tool parameters, private keys, pairing
or device tokens. Acceptance checks the common credential values and export
redaction; that is a guard, not permission to log payloads.

## Client address in logs

Access never records a plaintext client address. The public ingress writes
`X-Whalerein-Client-IP` (overwriting any inbound value), and Access reads it only
from the CIDRs in `http.client_address.trusted_proxies`. The access record then
carries:

| Field | Meaning |
|---|---|
| `client_ref` | `c1_` + a 128-bit windowed HMAC of the address |
| `client_ref_window` | UTC window, `2026-11` (monthly) or `2026-11-20` (daily) |
| `client_addr_status` | `forwarded`, `direct`, or `unavailable` |

Generate the 256-bit master key once per environment with
`openssl rand -base64 32`, put it in the deployment secret store, and set
`http.client_reference.key` from `${WHALEREIN_CLIENT_REF_KEY}`. Put it in
`http.client_reference.window` (`monthly` or `daily`). The window changes
automatically; the master key changes only on compromise or an explicit annual
rotation. Never reuse a key across environments.

To relate an operator-held IP (for example from a firewall or load-balancer
log) to a record, compute the same reference in Python 3:

```python
import base64, hashlib, hmac, sys
key = base64.b64decode(sys.argv[1])
window, address = sys.argv[2], sys.argv[3]
digest = hmac.new(key, f"whalerein/client/v1\n{window}\n{address}".encode(),
                  hashlib.sha256).hexdigest()
print("c1_" + digest[:32])
```

Run it with the key, the `client_ref_window` from the record, and the candidate
address; search the Access logs for the printed value. The raw address never
appears in those logs.

## Request metadata retention and capacity

Access runs cleanup every `cleanup.interval` (Compose: one minute) and once at
startup. The policy is intentionally asymmetric:

| Record | Policy | Rationale |
|---|---|---|
| Expired request bodies | Cleared on the first cleanup pass after `expires_at`. | Large `bytea` payloads must not stay resident. |
| Query/handshake metadata (`command_id = ''`, kind 1/2) | Deleted after the 24 h window, in batches of at most 2000 rows per pass. | Bounded disposable history. |
| Command deduplication rows (`command_id <> ''`) | Retained after expiry; only their bodies are cleared. | Deleting them could let a retried or restored command execute twice. |
| Exact client-lease and cancellation metadata | Rides the request identity; pending cancellation rows are excluded from cleanup until published (bounded retries). | Lease expiry and cancellation replay must survive an Access restart. |

Capacity consequence: the `requests` table grows with the number of unique
control commands, not with message payloads. Each retained row is small
(identifiers, digest, status and timestamps). Monitor it:

```sql
SELECT pg_total_relation_size('requests') AS table_and_index_bytes,
       count(*) FILTER (WHERE command_id <> '') AS command_rows,
       count(*) FILTER (WHERE command_id = '') AS bodyless_query_rows,
       count(*) FILTER (WHERE request_body <> ''::bytea OR result_body <> ''::bytea) AS rows_with_bodies,
       count(*) FILTER (WHERE cancel_pending) AS pending_cancellations,
       count(*) FILTER (WHERE client_lease_state = 'expired') AS expired_client_leases
FROM requests;
```

A pending cancellation means Access has closed its forwarding/renewal authority
but has not yet confirmed publication to the owning Host. It must not be reported
as native claim release; it replays after broker or service recovery until
published or until the bounded retention rule removes it.

Choose an installation-specific retention/re-architecture decision before the
command table becomes a meaningful share of the database. Do not delete command
deduplication rows on a live installation without an explicit provenance
review: a restore or client retry can otherwise repeat an execution. Run normal
PostgreSQL autovacuum; clearing bodies creates dead tuples that must be
reclaimed. Acceptance evidence: `TestRequestMetadataRetentionPolicy` proves an
expired query row is deleted, the expired command bodies are cleared, and the
command deduplication row survives exactly once.

## PostgreSQL restart, read-only storage and disk-full

Access connects through a pool and reconnects after a PostgreSQL restart. An
in-flight request keeps its durable status and body; it is not settled just
because the connection failed. On a write failure (read-only database, full disk
or failed storage), a revoke returns an error instead of reporting success. The
revocation fence is persisted before it is acknowledged, survives an Access
restart, and blocks the old device credential.

Acceptance evidence: `TestPostgresRestartAndReadOnlyFence` restarts PostgreSQL
with a durable `host_accepted` request and verifies the status/body survive. It
then sets `default_transaction_read_only` on the database, forces Access to
reconnect, verifies the revoke fails and no `revoked_at` is recorded, restores
writes, revokes successfully, verifies a non-zero revocation generation, and
verifies the credential is rejected after another Access restart.

A real disk-full run is an environment-specific acceptance item: PostgreSQL has
no portable per-test disk quota. The read-only path exercises the same failing
write and durable-fence behavior; a production drill should additionally verify
that the storage alert fires, writes resume after space is freed, and no
request was reported as accepted while the write failed.

Operational checks:

```bash
curl --fail --silent --show-error http://127.0.0.1:8080/healthz
curl --fail --silent --show-error http://127.0.0.1:8080/readyz
pg_isready -h 127.0.0.1 -p 55432
```

`/readyz` requires PostgreSQL, the subscribed MQTT session and broker policy
reconciliation. It does not prove that a historical restore or a read-only
storage event was handled; use the acceptance cases for those semantics.

## Backup and restore validation

The conservative restore procedure and the exact invalidation SQL are in
[Backup and restore](../deploy/README.md#backup-and-restore). Run that SQL only
against the isolated restored database, never an active installation.

`TestBackupRestoreCannotResurrectGrantOrDuplicateCommand` dumps the live
database, restores it into a scratch database, and proves:

1. the untouched restore still contains the old active grant and credential
   (the hazard is real, not hypothetical);
2. the documented invalidation leaves zero active grants, deletes device
   credentials and pairing operations, marks previously received/host-accepted
   requests `unknown`, and sets `forward_state = 'none'`;
3. the completed command deduplication row is preserved (status intact) and a
   duplicate `command_id` insert is still rejected, so a restore cannot
   re-execute a command.

The test creates and drops a uniquely named scratch database and does not touch
the live data beyond a consistent `pg_dump` read. It needs the PostgreSQL
container (`WHALEREIN_ACCEPTANCE_POSTGRES_CONTAINER`, default
`whalerein-postgres`) with `pg_dump`, `pg_restore`, `createdb` and `dropdb`.

A restore is not proof that a control never executed. Preserve uncertain command
records for explicit source-side review, re-enroll clients with fresh identities,
and never lower a Host revocation generation to make an old identity work.

## Rolling upgrade matrix

WhaleRein is forward-migration only. At startup Access applies missing embedded
migrations in one transaction under an advisory lock. A binary that finds a
migration name it does not know refuses to start with
`database contains a newer migration; upgrade Access`. There is no downgrade
path and no "roll back the binary against a newer schema" support.

| From / to | Status | Notes |
|---|---|---|
| Same build, restart | Supported | Restart recovery keeps durable state; see the PostgreSQL and shutdown cases. |
| Adjacent versions with no new migration | Supported as stop/start | Database schema is unchanged; stop the old process before starting the new one. |
| Adjacent versions with a new migration | Supported as a short maintenance stop | Stop old Access, start the new image so migrations apply, then start the Host plugin and clients. Do not run old and new Access concurrently. |
| New version to an older binary | Not supported | The old binary rejects the newer migration ledger. Restore a coordinated backup with the explicit re-enrollment procedure instead. |
| Skip versions (N-2 or older) | Not supported without rehearsal | The repository does not publish an N-2 compatibility promise. Rehearse the exact path in an isolated installation. |
| Mixed Access versions behind one database | Not supported | One Access process per standalone installation is the current design. |
| Access, plugin and clients across the wire-version boundary | Not supported | Upgrade Access, the broker plugin and clients together before changing negotiated transport limits. Reconnect clients after limit changes. |

Run the adjacent-version matrix against a throwaway PostgreSQL instance:

```bash
OLD_REF=<older-git-ref> NEW_REF=<newer-git-ref> \
  deploy/tests/rolling-upgrade-matrix.sh
```

or pass prebuilt images with `OLD_IMAGE=... NEW_IMAGE=...`. The script builds
both Access images from `git archive`, starts a disposable `postgres:18-alpine`
on loopback port 55490, applies the old schema, applies the new schema, asserts
that the old binary rejects the newer ledger, and confirms the new binary is
still usable.

Current limitations, recorded honestly:

- the matrix is a manual, Docker-dependent script; it is **not** wired into the
  scheduled CI job because the repository publishes no adjacent-version images
  and the runner does not retain release artifacts;
- it validates Access schema compatibility only. It does not establish
  compatibility for the Mosquitto plugin, clients, mobile bindings or a
  multi-instance deployment;
- there is no zero-downtime promise when a release adds a migration. Plan a
  maintenance stop and rehearse the exact upgrade in an isolated installation;
- an actual production rollout also needs TLS/trust, credential-store and
  broker-policy reconciliation checks that are environment-specific.

## Coordinated version-one cutover

The session-observation contract is a coordinated replacement inside application
protocol version 1. The version number does not identify every older build, so
mixed Access/Host/Core/CLI/mobile sets are unsupported and must not run
concurrently. Stop all old clients and connectors, record the exact Access image
digest plus Host/CLI/mobile revisions, take a coordinated backup, stop the old
Access process, and only then start the matching new set. Access applies its
migrations and starts; start the Host plugin and clients afterward and rebuild
transient observations from current authority.

Retained device/Host keys, pins, grants, invitations, Source sessions, pager keys
and terminal/uncertain command identities survive. A retained non-terminal
command is never forwarded automatically by the new Access: a row that was
already published keeps its conservative status but stops retrying, and a row
that never left `pending` becomes `unknown`. Historical rows without an exact
client lease keep a NULL deadline and are never treated as expired. Old question
pager cursors are reported as `snapshot_required`, never reinterpreted.

Do not resend old signed pairing FINISH material or old queued observation
events; replace them from current authority. An uncertain outcome remains
`unknown` and recoverable by its original command identity; it is not proof that
no native action occurred. Rollback is a stopped-stack restore of the
coordinated backup plus matching older artifacts (then the documented restore
invalidation/re-enrollment), never a reset of keys, pins or revocation
generation.

## Deployment acceptance in CI

The CI `Deployment acceptance` job builds and starts the Compose stack on a
Linux runner and runs the `acceptance`-tagged Go suite with explicit restart,
stop and start commands. Missing configuration fails the run; it never skips a
restart or shutdown case. See
[Deployment acceptance](testing.md#deployment-acceptance) for the full
environment contract, case list, and the rule that OS-specific tests run only on
their matching runner. The Linux container integration suite remains separate
and is not a substitute for macOS or Windows validation.
