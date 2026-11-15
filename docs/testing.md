# Testing

## Validation scope

| Scope | Covers | Environment |
|---|---|---|
| Unit | Module behavior, validation, state transitions and errors. | Native toolchains and isolated fixtures. |
| Integration | Protocol compatibility, PostgreSQL, broker transport, CLI-to-FakeDSH and pinned native DSH/browser journeys. | Independent Linux Docker container. |
| Release | Target DSH versions/configurations, platform SecretStore, production TLS and deployment. | Actual target environments. |

FakeDSH does not verify real DSH APIs or browser behavior. The `native-dsh`
integration group adds actual DSH services and headless Chromium with a local
deterministic model; its pinned Linux configuration does not establish behavior
for every DSH version, external model, desktop shell or platform SecretStore. An integration or tooling-harness pass is not release validation.

## Platform-specific tests

The local host is not a substitute for every target OS. When a test requires an
operating system the current host cannot provide, gate it and skip it with an
explicit platform reason. Do not emulate another OS or run an OS-specific test
through a different-OS container, and never treat a skip as a pass. The Linux
container integration suite is intentionally Linux-only, not a substitute for
macOS or Windows validation.

- Rust: `#[cfg(unix)]`, `#[cfg(windows)]`, `#[cfg(target_os = "...")]`, or an
  explicit ignored/manual test with the reason in the attribute.
- Go: build tags or `t.Skip("requires ...")`.
- Node.js and Python: skip with a platform-specific reason
  (`pytest.mark.skipif`, `test.skip`).
- The Linux container integration suite remains Linux-only; it does not establish
  macOS or Windows behavior.
- Cross-OS verification belongs to the GitHub Actions matrix: Linux, macOS and
  Windows VMs run their native unit/platform tests. macOS SecretStore/keychain
  checks and Windows file-lock/sharing checks run only in their matching jobs.
  The Linux job owns the container integration suite.
- Platform-specific tests and the jobs that cover them should be named in the
  change record; a missing local OS is an environmental limitation, not a reason
  to fail or silently pass the local run.

## Choose checks for a change

Run commands from the repository root:

```bash
./repo check --project core
./repo test --project core
./repo test --project core --only reconnect
./repo test --project core --coverage
```

- During iteration, run focused unit tests and the affected project's checks.
- Protocol changes require `./repo proto --check` and
  `./repo test --suite integration --group protocol`. Regenerate intentional
  binding changes with `./repo proto` and review the generated diff.
- Cross-component, recovery and release changes require the full integration
  suite: `./repo test --suite integration`.
- `./repo test` runs all registered unit projects. Integration is slow and opt-in.
  Use `./repo test --suite all` for one report containing both suites.

For the Mosquitto line, two opt-in fixtures back the replacement acceptance
record. The native broker fixture needs the pinned Mosquitto 2.1.2 headers and
broker binary:

```bash
MOSQUITTO_INCLUDE=$(brew --prefix)/include \
  go test -tags mosquitto ./deploy/mosquitto/test -run TestNativeBrokerCapabilities -v
```

The stack acceptance suite runs against the Compose stack and needs its
restart commands so it can exercise restarts:

```bash
WHALEREIN_ACCEPTANCE=1 \
WHALEREIN_ACCEPTANCE_RESTART_ACCESS='docker restart whalerein-access' \
WHALEREIN_ACCEPTANCE_RESTART_BROKER='docker restart whalerein-mosquitto' \
  go test -tags acceptance ./tests/acceptance -run TestMosquittoLineAcceptance -v
```

The target-line benchmark is part of the same acceptance binary and needs the
TLS listener and seeded fixture:

```bash
WHALEREIN_BENCH=1 WHALEREIN_BENCH_DEVICES=2000 \
WHALEREIN_BENCH_MQTT=<host>:8883 WHALEREIN_BENCH_ACCESS=http://127.0.0.1:8080 \
  go test -tags acceptance ./tests/acceptance -run TestMosquittoLineBenchmark -v
```

Both fixtures stay outside the default unit suite; record their output under
`.local-docs/acceptance/`. See the [repo command reference](../tools/repo/README.md#test-selection) for exact
project IDs, case discovery, selector syntax and unsupported combinations.
The [integration guide](../tests/integration/README.md) describes container and
fixture ownership. Development services are not prerequisites for integration.

## Local integration timing

The Mosquitto broker binds in well under a second, and a full stack start
(pytest, Access, broker, FakeDSH) takes roughly 2–3 seconds once per session.
Wall time is dominated by per-case pairing setup (about 0.5 s), real command
round trips, and deliberate deadline waits.

Measured on the development host for the `chain` group (83 cases):

| Invocation | Wall time |
|---|---|
| `./repo test --suite integration --group chain` | ~200 s |
| `./repo test --suite integration --group chain --parallel 4` | ~64 s |

Shard large groups (`chain`, `native-dsh`) with `--parallel 4`. Keep small,
startup-dominated groups (`protocol`, `transport`) serial. `--parallel 8`
oversubscribes a 4–6 vCPU host and can fail timing-sensitive cases, so raise
the shard count only with more CPU. Readiness polling runs at 20 ms, which
roughly halves the wall time spent in fixture waits.

## CI integration timing

Ordinary PR updates and main pushes run affected-project fast checks. Full
integration is scheduled separately by `.github/workflows/integration.yml` at
03:17 Asia/Shanghai each day. GitHub scheduling is best-effort; manual dispatch
can validate a repair or high-risk change without waiting for the next schedule.

For a PR, manually add the `run-integration` label to start the **PR integration /
Full integration (manual)** check. The dedicated `.github/workflows/pr-integration.yml`
workflow runs the complete suite on Linux against the PR merge revision captured
by that label event. Opening a PR, pushing commits, or leaving the label attached
does not start another full run. After pushing a repair, remove and re-add the
label to test the new revision. Use **Re-run jobs** to retry the original revision.
The label must exist in the repository; create it once through GitHub's Labels
page if needed. JSON/HTML reports are available in the run's
`pr-integration-<pr-number>-<run-id>-<attempt>` Artifact for seven days.
This optional check has its own per-PR queue, does not block the fast CI gate,
and does not update the scheduled main-branch success baseline. It should not be
configured as a required branch-protection check because it only runs on request.

The scheduled workflow freezes one main commit and fingerprints all tracked inputs except
Markdown and the documentation/local-planning directories. A scheduled run skips
heavy execution only when an exact successful input receipt exists. The initial
run, changed inputs, unknown/missing evidence or evicted cache cause a full run.
Skipped runs identify the original verified revision; intermediate commits are
not represented as individually tested. Failed, partial or interrupted runs and
cleanup errors never establish a passed baseline.

Manual dispatch always executes the full suite, with an optional main revision.
The same workflow is reusable through `workflow_call` with an exact `revision`
and returns `tested_revision` only after success. Release callers always run the
suite, require that output to match the intended release commit, and make upload
jobs depend on the successful integration job. Publication automation is not yet
implemented; this is its callable integration gate, not a complete release flow.
Final package installation acceptance remains a separate release requirement.

Scheduled, manual and reusable invocations share a concurrency group and do not
cancel an active run. GitHub may replace an older pending run when several queue;
cancelled callers must never be treated as successful publication gates.
JSON/HTML reports and successful-input receipts are uploaded as
`integration-<run-id>-<attempt>` Artifacts with seven-day retention. The cache is
an optimization, not a permanent evidence archive. No Summary or Pages is used.

These CI triggers do not waive the local change-validation requirements above.

## Question claim liveness

One-shot CLI commands use the Access HTTP API, not a persistent device MQTT
session. Access records a non-terminal request claim, renews it on result
polling and on the CLI `POST /v1/requests/{id}/heartbeat`, and publishes a
bounded `request_stale` event to the Host presence topic after a 10s silent
lease. Host aborts the request scope; the adapter `finally` releases the
native question claim. The CLI heartbeat runs every 1s while a terminal
command is awaited. MQTT will/keepalive remains only for clients that actually
hold an MQTT connection. Unit tests cover leases, heartbeat, ACL and Host
routes; targeted integration covers kill, half-open and reconnect-window
behavior.(feat: add production DSH capabilities and claim lifecycle)

## Coverage and evidence

Require at least 90% per Rust crate, Go package and Node package. Rust/Node use
line coverage; Go uses its native statement coverage. Integration results do not
replace unit coverage. The Repo Tooling project also enforces 90% Python lines.
Generated protocol bindings are excluded from coverage; their tests still run.

Passing test cases and satisfying coverage are separate outcomes. Missing tools,
invalid measurements and below-threshold values fail the coverage gate. See
[measurement and report contracts](../tools/repo/README.md#coverage-and-report-facts)
for native execution phases, totals, cancellation and timing.

Record local validation under `.local-docs/acceptance/`, including commands,
artifact locations, failures, skips and environmental limitations. Keep dated
case counts and performance measurements there, not in maintained usage guides.
Historical runs do not establish current verification.

## Author tests

- Write names and descriptions in English. State the condition and expected
  result, such as `rejects an expired approval without calling DSH`. Name each
  parameterized case and keep setup, action and assertions distinct.
- Assert the expected behavior without printing secrets or private business
  contents. Diagnostic redaction is best effort, not permission to log payloads.
- Cover applicable failure cases: duplicates, expiry, uncertain outcomes, event
  gaps, reconnect/revocation, stale approvals, tampering and resource limits.
- For execution tooling, verify that selectors exclude unmatched bodies and
  cancellation stops further scheduling while retaining observed results.

Use the integration guide's [case description rules](../tests/integration/README.md#case-descriptions)
when adding fixture-backed cases or native drivers. A selected run remains
partial validation even when every selected case passes.
