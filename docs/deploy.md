# Deployment operations and capacity

This guide covers the operational behavior of the Compose stack in
[deploy/README.md](../deploy/README.md): shutdown, log growth, retention,
SQLite failure modes, backup/restore validation, and the upgrade window. It
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
SELECT (page_count * page_size) AS table_and_index_bytes,
       count(*) FILTER (WHERE command_id <> '') AS command_rows,
       count(*) FILTER (WHERE command_id = '') AS bodyless_query_rows,
       count(*) FILTER (WHERE request_body <> x'' OR result_body <> x'') AS rows_with_bodies,
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
SQLite WAL checkpoints; clearing bodies creates free pages that must be
reclaimed. Acceptance evidence: `TestRequestMetadataRetentionPolicy` proves an
expired query row is deleted, the expired command bodies are cleared, and the
command deduplication row survives exactly once.

## Temporary ciphertext capacity

Oversized relay payloads and attachment parts use one quota directory per Access
node (`storage.root_dir`), backed in Compose by the named
`whalerein-access-payloads` volume. SQLite records every reservation; the
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
directory with SQLite or Host state. A new Compose named volume inherits the
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

## SQLite restart, locked storage and disk-full

Access reopens its local SQLite file after a restart. A committed request keeps
its durable status and body; a request that failed to commit is retried or
reported as unavailable, never as accepted. A write failure (read-only file,
busy database, full disk or corruption) makes a revoke return an error instead
of reporting success. The revocation fence is committed before it is
acknowledged, survives an Access restart and blocks the old Device credential.

Acceptance evidence from this change:

- `TestSQLiteFreshInstallAndIdempotentMigration` opens a fresh file and applies
  the schema twice.
- `TestSQLiteRequestRoundTripPreservesTimestampsAndBytes` proves status, body,
  digest and exact millisecond expiry survive a round trip.
- `TestSQLiteRetainsUniqueCommandIdentity` proves a duplicate command identity
  cannot create a second row.
- `TestPayloadRestartAbandonsStaleReservation` and its siblings run against
  SQLite in `-tags integration` and cover reservation recovery, committed-file
  publication and cleanup retries.
- `TestCurrentTrustedBackupVerifies`, `TestStaleBackupIsRefused` and
  `TestMissingOrForgedBackupIsRefused` cover the Standalone restore gates.

A real disk-full drill remains an environment-specific acceptance item: neither
SQLite nor a container has a portable per-test disk quota. The storage layer maps
`database or disk is full`, `database is locked` and `database disk image is
malformed` to explicit retryable errors so no caller can mistake them for a
durable receipt.

Operational checks:

```bash
curl --fail --silent --show-error http://127.0.0.1:8080/healthz
curl --fail --silent --show-error http://127.0.0.1:8080/readyz
```

`/readyz` requires the SQLite write probe, the subscribed MQTT session and
broker policy reconciliation; when storage is enabled it also requires a
writable payload directory with headroom. It does not prove that a historical
restore, a read-only storage event or a matching Host/client relay epoch was
handled; use the acceptance cases and the cutover gate for those semantics.

## Backup and restore validation

The conservative restore procedure and the Standalone manifest rules are in
[Backup and restore](../deploy/README.md#backup-and-restore) and
`access/internal/standalone`. A trusted backup must be quiesced, signed by the
original installation credential and cover the final committed authorization
revision. A stale, unprovable or missing backup must start a fresh authority and
require explicit pairing; Central is never consulted.

An old backup can contain an active grant or credential that a later revocation
removed. The restore gate therefore verifies:

1. the untouched restore still contains the old active grant and credential
   (the hazard is real, not hypothetical);
2. a stale or unsigned backup is refused before Access serves;
3. the completed command deduplication row survives and a duplicate
   `command_id` insert is still rejected, so a restore cannot re-execute a
   command.

A restore is not proof that a control never executed. Preserve uncertain command
records for explicit source-side review, re-enroll clients with fresh identities,
and never lower a Host revocation generation to make an old identity work.

## Fresh deployment boundary

The reviewed architecture is a fresh release: no PostgreSQL data import, dual
backend, mixed-version rollout or compatibility API. A managed node starts from
an empty local SQLite file and obtains policy from Central. A Standalone node
starts fresh or restores only a trusted, current backup as described above.
Validate static deployment inputs with `./repo check` and run the affected
Access unit and integration cases; an adjacent-version database matrix is not
part of this release.
