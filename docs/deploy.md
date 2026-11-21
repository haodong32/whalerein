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

## Temporary ciphertext capacity

Oversized relay payloads and attachment parts use one quota directory per Access
node (`storage.root_dir`), backed in Compose by the named
`whalerein-access-payloads` volume. PostgreSQL records every reservation; the
files are disposable. Capacity is charged from reservation until file deletion,
so partial, ready and expired ciphertext all count against the configured
object/host/node limits plus the object and active-upload counts.

The Compose values are development defaults, not a sizing plan:

| Key | Value | Operational consequence |
|---|---|---|
| `max_object_bytes` | 16 MiB | One object; keep it at or below the profile envelope (16 MiB). |
| `max_host_bytes` / `max_node_bytes` | 256 MiB / 1 GiB | Charged ciphertext bytes per Host and node. |
| `max_objects` | 1024 | Reserved, ready and expired metadata objects. |
| `max_active_uploads` | 16 | Concurrent upload bodies for the node. |
| `free_space_reserve_bytes` | 1 GiB | Admission is refused below this free-space headroom. |
| `retention` | 24 h | Hard lifetime cap; parent deadlines can only shorten it. |
| `upload_timeout` / `read_timeout` | 2 min / 60 s | One complete upload or fetch. |

Size the volume for `max_node_bytes` plus `free_space_reserve_bytes` plus
filesystem overhead, and keep it on one dedicated filesystem. Do not share the
directory with PostgreSQL or Host state. A new Compose named volume inherits the
image directory ownership; a pre-existing volume or bind mount must be chowned
to the unprivileged Access user before startup or `/readyz` fails the storage
check. Startup reconciliation repairs stale
reservations, orphan files and missing ready files in bounded batches; cleanup
failures stay charged and retry with bounded backoff.

`/readyz` adds a `storage` check when storage is enabled. It proves the temp
directory is writable and that free space is at least the reserve. It does not
prove that the volume is large enough for future growth, that cleanup has
reclaimed everything, or that no reserved writer is stalled. Watch these bounded
metrics:

```text
payload_free_bytes
payload_headroom_bytes
payload_ready_bytes
payload_reserved_bytes
payload_cleanup_reclaimed_bytes_total
payload_admission_refusals_total
payload_expired_backlog_seconds
```

When the reserve is reached, admission fails closed (`429 quota_exceeded` or
`503 storage_pressure`); MQTT delivery is not paused by a full payload volume.
Disabling storage removes the `/readyz` storage check but also disables reference
resolution, so keep it enabled whenever the envelope can exceed the packet
budget. Access does not currently validate `max_object_bytes <= envelope_bytes`
at startup; treat that relation as an operator invariant.
A real disk-full drill is an environment-specific acceptance item. Do not point
the volume at a network filesystem or delete files behind Access's back.

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
reconciliation; when storage is enabled it also requires a writable payload
directory with headroom. It does not prove that a historical restore, a
read-only storage event or a matching Host/client relay epoch was handled; use
the acceptance cases and the cutover gate for those semantics.

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
| Access, plugin and clients across the relay-epoch boundary | Not supported | Relay epoch 3 is a hard cut: upgrade Access, the broker plugin/policy, Host runtime, Core, CLI, mobile bindings and clients together. Reconnect clients after the cut so both directions renegotiate. |
| Epoch-2 Access or client against epoch-3 Access | Not supported | Rejected with `protocol_version_incompatible`; there is no fallback, fragment path or ACK topic. |
| Older binary against the epoch-3 migration ledger | Not supported | Restore a coordinated pre-cut backup with matching older artifacts, or use an explicitly compatible binary. |

The static preflight runs without Docker:

```bash
deploy/tests/rolling-upgrade-matrix.sh --static
```

It parses the Compose Access config and stack, asserts the payload volume is
mounted and writable by design, checks `storage.enabled` and the byte/count
limits, asserts the broker ceiling equals the packaged
`brokerPacketLimitBytes` and every profile packet budget is within it, and
checks the epoch-3/relay-topic prerequisites. It does not start a container.

Run the full adjacent-version matrix against a throwaway PostgreSQL instance:

```bash
OLD_REF=<older-git-ref> NEW_REF=<newer-git-ref> \
  deploy/tests/rolling-upgrade-matrix.sh
```

or pass prebuilt images with `OLD_IMAGE=... NEW_IMAGE=...`. The script builds
both Access images from `git archive`, starts a disposable `postgres:18-alpine`
on loopback port 55490, applies the old schema, applies the new schema, asserts
that the old binary rejects the newer ledger, and confirms the new binary is
still usable. Its static preflight (`--static`) needs neither Docker nor a
running stack.

Current limitations, recorded honestly:

- the full matrix is a manual, Docker-dependent script; it is **not** wired into
  the scheduled CI job because the repository publishes no adjacent-version
  images and the runner does not retain release artifacts. Only the static
  preflight is environment-independent;
- it validates Access schema compatibility and the static deployment inputs. It
  does not establish compatibility for the Mosquitto plugin, clients, mobile
  bindings, TLS ingress or a multi-instance deployment;
- there is no zero-downtime promise when a release adds a migration. Plan a
  maintenance stop and rehearse the exact upgrade in an isolated installation;
- an actual production rollout also needs TLS/trust, credential-store and
  broker-policy reconciliation checks that are environment-specific.

## Coordinated relay epoch-3 cutover

The MQTT relay is replaced in one coordinated hard cut. Application protocol
stays 1; the MQTT relay epoch is 3. There is no v2 acceptance, unframed
fallback, fragment/reassembly path, transfer receipt or device ACK topic.
Access, the broker plugin/policy, Host runtime/DSH plugin, Core, CLI, mobile
bindings, iOS builds and every supported client must come from one release
revision. A mixed artifact set is not a supported release.

### Prepare

1. Provision the node payload volume and filesystem from
   [Temporary ciphertext capacity](#temporary-ciphertext-capacity). Confirm
   permissions, free space above `max_node_bytes` plus the reserve, and that
   only Access writes the directory.
2. Configure public TLS for the Access origin and the five relay routes, plus
   broker TLS/hostnames. Set ingress body limits to at least
   `max(http.max_body_bytes, storage.max_object_bytes)` and request/response
   timeouts to at least `storage.upload_timeout` / `storage.read_timeout` (and
   the profile `transfer_timeout`). Ingress manifests are operator inputs; this
   repository contains only the loopback Compose stack.
3. Select the same profile or explicit overrides on Access, Host and clients.
   Every packet budget must stay at or below the rendered broker
   `max_packet_size` and the packaged `brokerPacketLimitBytes` (1 MiB).
4. Deploy the matching broker config/plugin and confirm the six relay topics
   and the denied `ack` topic. Access computes ACLs from the authenticated
   grant; do not duplicate policy in ad-hoc broker files.
5. Record artifact digests and take a coordinated PostgreSQL backup. Preserve
   Host command-deduplication records and identity with it.
6. Export unresolved operations before stopping anything: `request_id`,
   `command_id`, `payload_digest`, status and whether native execution is
   possible. Never resend one with a new identity.

### Cut over

1. **Quiesce old admissions.** Stop new client submissions at the ingress or by
   stopping clients. Let old in-flight requests reach terminal state or expire to
   `unknown`. The v2 path has no durable fragment directory to migrate; drain or
   expire its transient state. Preserve grants, revocations, pins, keys, pairing
   records, command identities and proved/unknown outcomes.
2. **Stop the old set.** Stop old clients and Host, then old Access. Keep
   PostgreSQL and its migration ledger.
3. **Deploy the matching set.** Start PostgreSQL, broker, Access (wait for
   `/readyz`), Host runtime/DSH plugin, then clients. The order prevents a Host
   from registering against an incomplete broker.
4. **Gate readiness on epoch 3.**
   - `/readyz` is green: database write probe, MQTT session,
     presence/policy reconciliation, and storage writability/headroom.
   - Access accepted the Host's `HostReady` with `relay_version=3` and a valid
     receive advertisement.
   - Each client connected with epoch 3, subscribed its own `event` and
     `notice` topics, and published `ready` on its own topic; Access registered
     the current connection generation.
   - An old/mixed artifact fails with `protocol_version_incompatible` before
     business delivery.
   - The broker shows the six topics and no `ack` acceptance.
   - Payload free space remains above the reserve.
5. **Reattach and recover.** Reconnect Host and clients, publish fresh `ready`
   advertisements, rerun the original watch/history reads. Recover an
   interrupted foreground command by its original identity with
   `whalerein operation recover <request-id>` (or Core `GetCommandResult`).
   Treat a missing body as the recorded terminal status, never as success, and
   preserve `unknown` when execution is possible.

`/readyz` is necessary but not sufficient for the epoch gate. This repository
has no endpoint that reports a connected Host's or client's relay epoch, so the
operator must record the Host/client epoch evidence and broker policy evidence
separately.

### Rollback

A live mixed-contract rollback is unsupported. Rollback requires, in order:

1. fenced admission so no new work crosses the boundary;
2. quiesced receivers (stop Host and clients);
3. matching old artifacts and broker policy for the whole set;
4. a verified recovery procedure for commands admitted under the new release.

Epoch 3 applies new migrations, and the old binary refuses a newer migration
ledger. Rollback is therefore a coordinated pre-cut restore with matching older
artifacts plus the documented invalidation/re-enrollment, or a compatible binary
built for the new schema. It is not an in-place binary swap, and it must not
lower keys, pins or revocation generations. Commands executed after the backup
cannot be proved by the restored database; keep their original identity and
report `unknown` rather than claiming they did not run.

### Verified versus operator procedure

Verified by repository tests: storage/transport config validation and readiness,
storage headroom behavior, relay epoch rejection, broker topic/ACL policy, the
static deployment preflight, and the migration-adjacency gate. The Docker matrix
proves an old binary rejects a newer ledger.

Operator procedure that this repository cannot prove: production TLS/DNS,
multi-node owner paths, ingress body/timeout limits, real volume pressure and
reclaim, broker HA, coordinated live backup/restore, Cloud adoption inputs, and
the AC-19 netem measurement. Rehearse the exact procedure on a disposable native
Linux installation and record the result with the release evidence.

## Deployment acceptance in CI

The CI `Deployment acceptance` job builds and starts the Compose stack on a
Linux runner and runs the `acceptance`-tagged Go suite with explicit restart,
stop and start commands. Missing configuration fails the run; it never skips a
restart or shutdown case. See
[Deployment acceptance](testing.md#deployment-acceptance) for the full
environment contract, case list, and the rule that OS-specific tests run only on
their matching runner. The Linux container integration suite remains separate
and is not a substitute for macOS or Windows validation.
