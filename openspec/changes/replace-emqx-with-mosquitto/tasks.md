# Tasks

## 1. Verify native broker capabilities

- [x] 1.1 Resolve D-05 in an isolated native Mosquitto fixture. Compare upstream Dynamic Security and supported plugin hooks for deferred CONNECT authentication, internal bootstrap, local WRITE, SUBSCRIBE, outgoing READ and Will decisions, connection snapshots, notifications, and targeted disconnect. Bind AC-04 through AC-06 and AC-10 through AC-11 to native procedures. Verify allowed and denied operations with the existing MQTT 5 clients or protocol fixtures.
- [x] 1.2 Record the Mosquitto pin and evidence, the smallest extension needed, broker-local validity and reconciliation bounds, and the readiness proof. Exercise management loss and process restarts using synthetic credentials. Verify stale permissions expire locally and cannot return through a late message or restart. Tasks in sections 3 and 4 depend on closing D-05 with this evidence.

## 2. Move Access grant admission to local state

- [x] 2.1 Use Otter v2.4.0 in a shared admission service for `access/internal/auth` and `access/internal/events`. Wire it at startup and exercise concurrent cold loads, cancellation, bounds, expiry, eviction and denial in unit tests. Verify warm event and receipt authorization is free of grant queries and unrelated traffic progresses during a stalled lookup or write (AC-01, AC-02).
- [x] 2.2 Add scoped deny and revision handling around pairing and revocation in `access/internal/auth` and `access/internal/store`. Reuse transaction and operation identities. Verify concurrent cold load, queued dispatch, retry, new pairing, rollback and ambiguous commit with deterministic fault cases. Document authorization and durability boundaries in Access architecture (AC-03 through AC-05).

## 3. Integrate Access and Mosquitto

- [x] 3.1 After D-05, implement the private management interface and connect validation, integrate the long-lived MQTT client in `access/internal/broker`, and wire startup and configuration. Verify internal bootstrap and external ready-state gating, bounded auth latency, the versioned control interface and isolation from device topics in native tests (AC-05, AC-06).
- [x] 3.2 Project committed policy, acknowledge application, fence obsolete revisions, and reconcile startup and missed updates. Integrate broker enforcement and connection closure with revoke and usable pairing responses. Verify live delivery to existing subscriptions, PG and control failures, connection closure and idempotent retry with real PostgreSQL and Mosquitto. Add request-level metrics for policy freshness, revocation progress and reconciliation failures without collecting credentials or contents (AC-01, AC-03 through AC-06).
- [x] 3.3 Replace the EMQX presence hooks and disconnect client with the selected native integration. Verify snapshots and ordered connection identity, Host-scoped notifications, exact claim release and HTTP heartbeat fallback with integration tests. Document the management and presence boundary (AC-10, AC-11).

## 4. Ship the new deployment and preserve the wire contract

- [x] 4.1 After D-05, update `deploy/compose`, runtime config and integration fixtures to pin Mosquitto and remove retired EMQX runtime paths. Document readiness, compatible schema changes, rollback and operations. Verify one Access and one broker, external PG, matching TLS and endpoint, reconnect with existing credentials, and a clean old-to-new and new-to-old rehearsal (AC-06, AC-08).
- [x] 4.2 Exercise existing Core and Host releases with the new broker, covering live and committed events, MQTT QoS, receipts, framed size boundaries and recovery after reconnect. Use their existing integration paths and update maintained transport docs only where contracts or operation change. Run the applicable `./repo check --project access`, `./repo build --project access`, `./repo test --project access --coverage` and `./repo test --suite integration` checks (AC-06, AC-07, AC-10, AC-11).
- [x] 4.3 Rehearse the hard cut and rollback with representative state including a post-cutover revoke and unfinished commands. Record the PG schema and data compatibility evidence, the traffic reopening checks, and the rollback procedure (AC-08).

## 5. Record acceptance evidence

- [x] 5.1 Run D-06 on a 2 vCPU, 4 GiB line with separate PostgreSQL and 2,000 mostly idle TLS MQTT connections. Record setup, activity mix, durations and concurrent stream and reconnect workloads. Record observed connection counts, first-text and event latency p50/p95/p99, throughput, PG authorization queries, failures, drops, peak CPU, memory and bandwidth. Report observations without a latency pass threshold (AC-09).
- [x] 5.2 Recheck each case against fresh evidence, apply the acceptance gates, and review architectural deviations. Verify that maintained architecture, security and operator docs describe the implementation and that temporary evidence stays Git-ignored (AC-01 through AC-11).

## AC-to-case matrix

The planned IDs below bind cases to test or procedure manifests. Before
running acceptance, save those bindings under `.local-docs/acceptance/` with
each exact command or manual sequence, assertions and expected artifact
path. Record per-case configuration, revision, native versions and result
according to the [acceptance evidence contract](../../README.md#acceptance-evidence-contract).

| AC and owning tasks | Required cases and assertions |
| --- | --- |
| AC-01, 2.1 and 3.2 | `AC-01.stream` and `AC-01.receipt` instrument warmed live and committed fan-out and receipt authorization for zero SQL and broker RPC. `AC-01.isolation` stalls cold SQL and grant writes while a separate warmed grant forwards. |
| AC-02, 2.1 | `AC-02.concurrent` coalesces simultaneous misses using a blocked loader. `AC-02.lifecycle` repeats at first use, restart, expiry and eviction. `AC-02.deny` proves absence, timeout and DB errors do not grant access. |
| AC-03, 2.2 and 3.2 | `AC-03.initial` and `AC-03.repair` verify policy readiness and event admission with real broker and PG. `AC-03.retry` interrupts after commit and before final acknowledgement, then repeats the same operation. |
| AC-04, 1.1, 2.2 and 3.2 | `AC-04.scope` covers Host-only and device-wide revoke, a concurrent re-pair, an in-flight lookup, queued dispatch and unrelated grants. `AC-04.completion` verifies committed deny and closure or absence with a live broker. `AC-04.write` covers confirmed rollback and ambiguous commit. `AC-04.no-ack` withholds broker confirmation and verifies retry and completion timing. |
| AC-05, 1.2, 3.1 and 3.2 | `AC-05.boot` restarts Access and Mosquitto independently and together, including broker startup with PG unreachable. `AC-05.partition` checks broker-local expiry, no unauthorized renewals and recovery. `AC-05.order` replays missed or stale updates and retired connection events after a newer revision. |
| AC-06, 1.1, 3.1 and 4.1–4.2 | `AC-06.identity` covers Core, Host, Access, mismatched IDs, invalid credentials and private control operations. `AC-06.acl` covers WRITE, SUBSCRIBE, outgoing READ, foreign topics, wildcard, shared filters and Will policy. `AC-06.delivery` revokes an already subscribed client and checks broker call counts and closure. |
| AC-07, 4.2 | `AC-07.stream` exercises existing clients, encrypted event chunks and terminal messages. `AC-07.frames` checks framing boundaries below, at and above the configured limits with encoded overhead, QoS, receipts, duplicate and missing parts. `AC-07.recovery` reconnects during committed output and verifies history and recovery. |
| AC-08, 4.1 and 4.3 | `AC-08.cut` checks the same TLS endpoint, preserved credentials, policy readiness, revoked grants and ongoing commands. `AC-08.rollback` exercises post-cutover grants and revocations against current PG state and checks command recovery. |
| AC-09, 5.1 | `AC-09.steady`, `AC-09.stream` and `AC-09.reconnect` record target-line measurements for idle connections, active forwarding and concentrated reconnect with catch-up. Record load generator version, configuration and actual applied load for all three. |
| AC-10, 3.3 and 4.2 | `AC-10.offline` covers clean disconnect, killed process, broker timeout and revoke with active claims. `AC-10.race` reconnects with the same ID before a delayed disconnect. `AC-10.fallback` withholds broker notifications and tests heartbeat and claim expiration using the existing Host path. |
| AC-11, 3.3 | `AC-11.snapshot` restarts Access alone while Host sessions remain live. `AC-11.order` changes connections during the snapshot and checks unknown state, ordering and removal of stale entries with the real broker. |

## Acceptance Results

Evidence artifacts live under `.local-docs/acceptance/replace-emqx-with-mosquitto/`
(Git-ignored): `stack-acceptance.log`, `benchmark.md`/`benchmark.json`,
`cutover-rollback.md`, native plugin output and unit coverage reports.
Commands below run from the repository root.

| AC ID | Status | Evidence | Limitations |
| --- | --- | --- | --- |
| AC-01 | PASS | `WHALEREIN_ACCEPTANCE=1 go test -tags acceptance ./tests/acceptance` case `AC-01_warm_events_make_no_per-event_grant_loads`: 20 warmed deliveries, admission hits advanced, grant loads unchanged; receipts made no grant loads. Unit `access/internal/admission` covers cache-hit isolation and stalled-load independence. | Live events are dropped while a cold grant loads; durable recovery is separate. |
| AC-02 | PASS | `go test ./access/internal/admission`: coalesced concurrent misses (32 callers, one load), absence and lookup failure fail closed, TTL reload, stalled key does not block another grant, bounded cache. | Unit-level fake store; no injected database failure at container scale. |
| AC-03 | PASS | Unit `pairing`/`line` cover the commit -> admission ordering; stack case seeds the same durable grant and verifies policy readiness; integration `test_pair` (2 cases) runs real invitation/HELLO/FINISH pairing through the new line. | Broker projection after pairing is best effort: the device receives committed policy at CONNECT and reconcile retries a live update. |
| AC-04 | PASS | Stack case `AC-04_revocation_stops_forwarding_and_survives_restarts`: local deny before commit, broker deny + kick, no forwarding to the revoked device, credential reconnect denied, unrelated grant live, revocation survives Access restart. Unit tests cover in-flight load fencing, rollback/reconcile and ambiguous states. | Broker-enforcement failure (control path down at revoke time) returns an error and converges on retry; not exercised end-to-end. |
| AC-05 | PASS | Native plugin cases `policy_validity_expires_without_confirmation`, `access_outage_denies_new_external_connections`, `broker_restart_clears_policy_and_presence`, `late_policy_cannot_restore_revoked_authority`; stack cases restart Access and Mosquitto independently and re-check committed policy. | No real network partition; control loss is simulated by the auth source staying unavailable. |
| AC-06 | PASS | Native cases `acl_write_subscribe_read`, `device_subscribe_and_delivery`, `probe_denied_everywhere`, `unauthorized_will_is_denied`, `control_policy_update_and_kick`, `authorization_requests_only_on_connect` (one auth request per connection, zero per operation); stack case checks foreign topics, wildcards and cross-device topics. | Uses the composed line over loopback; production TLS identity not exercised. |
| AC-07 | PASS | `./repo test --suite integration --group transport` (4 cases: routes, logical 2 MiB framing, identity pairing/revocation, Node large message) passes repeatedly on the Linux container line; chain cases `test_pair`, `test_hosts`, `test_open_question` and `test_probe_login` pass. | Three consecutive full transport passes were recorded after the pairing fix; an earlier run showed one identity-case flake under the container harness and was not fully explained. |
| AC-08 | PASS | `cutover-rollback.md`: previous EMQX release image built from `HEAD`, same PostgreSQL state, stored credential reconnects on both lines, post-cutover revocation enforced after switching back, whole-release stop/start. | Loopback Compose rehearsal; no production TLS or live client traffic. |
| AC-09 | PASS | `benchmark.md`/`benchmark.json`: 2,000/2,000 TLS connections (0 failures) on a 2 vCPU / 4 GiB Docker VM, 2,947 events/s, p50/p95/p99 265/679/1235 µs, 500 concurrent reconnects in 0.66 s (0 failures), peak broker CPU 54.5 %, memory 99 MiB. | Load generator shared the VM; single streaming device; one snapshot timeout during the burst. |
| AC-10 | PASS | Stack case `AC-10_presence_follows_the_current_connection` (online, clean disconnect, reconnect with the same client ID, superseded disconnect ignored); native cases cover ordered notifications, snapshot and the device Will fallback; integration `test_open_question`/`test_continued_question` exercise claim release with the Host. | The stack run does not itself drive a CLI question claim. |
| AC-11 | PASS | Stack case `AC-11_Access_restart_preserves_live_host_presence`: Access restarted while the Host session stayed live, presence reconciled from the broker snapshot; native snapshot boundary/ordering cases cover races. | Presence is reported unavailable until the snapshot completes; no real slow-broker injection. |
