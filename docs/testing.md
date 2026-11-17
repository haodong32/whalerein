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
- The CI workflow runs affected Rust projects' checks, builds, unit tests and
  per-crate coverage on Linux, macOS 15 and Windows Server 2025. Linux also owns
  the Go, Node.js and Python groups and the separate container integration suite.
  All selected native matrix jobs must pass the `CI gate`.
- Windows runs the Core file-lock/sharing and reparse-point cases natively,
  plus focused repository-runner tests for Windows pipes and process-tree
  termination. macOS additionally invokes the ignored SecretStore Keychain
  round-trip explicitly against a temporary unlocked keychain. The helper
  restores the prior default/search list and deletes its keychain, including
  on test failure; failed cleanup or zero executed cases fails the job.
  Native reports are uploaded as `ci-native-<os>-<sha>-<attempt>` Artifacts.
- Platform-specific tests and the jobs that cover them should be named in the
  change record; a missing local OS is an environmental limitation, not a reason
  to fail or silently pass the local run.

## CI operating-system matrix

`.github/workflows/ci.yml` keeps the affected-project selection in the `changes`
job and adds two consumers:

| Job | Runner | What it proves |
|---|---|---|
| Checks and coverage (`fast`) | `ubuntu-24.04` | Static checks, Linux unit coverage, CLI diagnostics acceptance. |
| Native unit tests (macOS/Windows) | `macos-15`, `windows-2025` | The same selected Rust, Go and Node projects run their native unit tests on the matching OS. |
| Deployment acceptance | `ubuntu-24.04` | Compose stack, restart/in-flight recovery, graceful shutdown, log rotation, backup/restore and retention. |

The native jobs reuse `tools/repo/src/repo/ci.py plan`; they do not select a
different project set and they never run the Linux container integration suite.
They invoke native unit commands instead of `./repo test` because the streaming
process runner depends on POSIX process groups. Rust coverage, Go statement
coverage and Node line coverage remain the Linux gate; the OS jobs prove that
platform-gated tests actually execute. A skipped platform test in another OS job
is not evidence for the skipped platform.

Named platform-gated tests include
`windows_sharing_violation_is_retryable_admission_contention`,
`windows_directory_handle_blocks_renaming_until_released`,
`windows_reparse_points_are_rejected_without_touching_target`,
`unix_epipe_is_not_retryable_admission_contention`, and the macOS-only
`scutil` display-name path in the CLI. The Windows reparse-point test needs
Developer Mode or symlink privilege on the runner; a failure there is a runner
capability failure, not permission to skip the case in another job.

## Contract checks

The `Contract checks` job runs when the affected set includes the `proto`
project. It is offline and runs three checks:

```bash
./repo proto --check
python3 proto/v1/bindings/swift/check_swift_surface.py
python3 tests/integration/drivers/protobuf/proto_compat.py --check
```

The first compares committed Go/JavaScript bindings with the proto source and
needs `protoc`, `protoc-gen-go` and the Node workspace. The Swift check compares
`proto/v1/error_codes.json` with the recorded iOS surface and the hand-written
`MobileError` mapping; a new wire code or a changed mapping fails. The
compatibility check compares the compiled descriptor with the immutable
baseline and the exact field snapshot; a removed field/tag reuse fails, and an
intentional breaking change requires an explicit baseline update with a
rationale. A failure in either script prints an actionable `::error::` message
naming the file to review; do not regenerate a snapshot just to make it pass.

## Client environment contract checks

The shared contract at `contracts/client-environment/` is the only schema source.
Rust and Node parity tests read `cases.json` and must produce the same
normalized document or stable error code for every fixture and resolution entry;
neither runtime may relax the corpus to match its implementation. A Node test
compares `catalog.json` and `cases.json` against the canonical files byte for
byte; regenerate only with
`host/runtime/scripts/sync-client-environment-contract.mjs`.

Rust embeds the catalog and corpus with `include_str!`; the Node package ships
generated copies under `src/configuration/` and the integration Dockerfile
copies `contracts/client-environment/`, so installed artifacts resolve built-ins
without a checkout-relative path. Packaging and clean-room probes (embedded
catalog from a moved source tree, `npm pack` tarball, Docker context copy) are
recorded evidence, not a substitute for a full Linux image build or a release
installation.

Both runtimes implement the same writer protocol: the sidecar
`<config>.lock.sqlite` with table `whalerein_config_lock`, `BEGIN IMMEDIATE`,
a revision recheck under the lock, and temp + fsync + rename + directory fsync.
Unit tests cover stale revisions, changed targets, validation before write,
fault injection, crash lock release and concurrent writers per runtime. The
cross-runtime lock and reader matrix is recorded macOS evidence only; Linux,
Windows, NFS/SMB, power-loss durability and an automated cross-runtime CI case
are not run.

Platform and native gates remain honest limitations, not implied passes:
two-device isolation, real Cordis/DSH browser lifecycle, macOS Keychain and real
DSH credential write/restart, live enrollment and TLS/MQTT ALPN against real
ingress stay BLOCKED where no prerequisite exists. Fixture certificates,
loopback handshakes, synthetic legacy layouts and file secret stores do not
establish those claims. Record per-AC status and prerequisites under
`.local-docs/acceptance/`; a skipped platform test is not evidence for that
platform.

## Deployment acceptance

`tests/acceptance` is an opt-in Go suite with the `acceptance` build tag. It
drives the Compose stack from [deploy/README.md](../deploy/README.md) over real
MQTT, PostgreSQL and the Access HTTP API. Restart commands are **required**: a
missing command fails the case with a configuration error instead of skipping
the restart scenario. The CI `Deployment acceptance` job supplies the explicit
commands and starts the stack before running the suite.

| Variable | Required | Purpose |
|---|---|---|
| `WHALEREIN_ACCEPTANCE` | yes | Must be `1`; without it the suite skips as not requested. |
| `WHALEREIN_ACCEPTANCE_RESTART_ACCESS` | yes | Restart command, for example `docker compose -f deploy/compose/stack.yaml restart access`. |
| `WHALEREIN_ACCEPTANCE_RESTART_BROKER` | yes | Restart command for `mosquitto`. |
| `WHALEREIN_ACCEPTANCE_RESTART_POSTGRES` | deployment cases | PostgreSQL restart command. |
| `WHALEREIN_ACCEPTANCE_STOP_ACCESS` | shutdown case | SIGTERM stop command, for example `docker stop --time 20 whalerein-access`. |
| `WHALEREIN_ACCEPTANCE_START_ACCESS` | shutdown case | Start command paired with the stop command. |
| `WHALEREIN_ACCEPTANCE_ACCESS_CONTAINER` | no | Access container name for log/rotation inspection; defaults to `whalerein-access`. |
| `WHALEREIN_ACCEPTANCE_POSTGRES_CONTAINER` | no | PostgreSQL container name for `pg_dump`/`pg_restore`; defaults to `whalerein-postgres`. |
| `WHALEREIN_ACCEPTANCE_DATABASE_URL` | no | Database URL; defaults to the Compose loopback URL. |
| `WHALEREIN_ACCEPTANCE_MQTT` / `_ACCESS` | no | Published endpoints; default to `127.0.0.1:1883` and `http://127.0.0.1:8080`. |
| `WHALEREIN_HOST_PROVISIONING_TOKEN` / `WHALEREIN_ADMIN_TOKEN` | no | Compose development tokens; override for a non-default stack. |

```bash
WHALEREIN_ACCEPTANCE=1 \
WHALEREIN_ACCEPTANCE_RESTART_ACCESS='docker compose -f deploy/compose/stack.yaml restart access' \
WHALEREIN_ACCEPTANCE_RESTART_BROKER='docker compose -f deploy/compose/stack.yaml restart mosquitto' \
WHALEREIN_ACCEPTANCE_RESTART_POSTGRES='docker compose -f deploy/compose/stack.yaml restart postgres' \
WHALEREIN_ACCEPTANCE_STOP_ACCESS='docker stop --time 20 whalerein-access' \
WHALEREIN_ACCEPTANCE_START_ACCESS='docker start whalerein-access' \
  go test -tags acceptance ./tests/acceptance -v
```

The CLI diagnostics acceptance case (`TestCLIDoctorDegradedEnvironment`) does
not need the stack. CI runs it in the Linux Rust group after the CLI is built,
with `WHALEREIN_ACCEPTANCE_CLI=target/debug/whalerein`; without that variable the
case builds the CLI with Cargo and fails if the toolchain is unavailable.

Deployment acceptance cases mutate restart state and some create temporary
databases. Run them serially against the stack; do not run them concurrently
with another agent's Compose run.

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
restart commands so it can exercise restarts. Missing restart configuration
fails the run; it does not skip the restart cases (see
[Deployment acceptance](#deployment-acceptance) for the full contract):

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

## CI test tiers

Pull requests, the merge queue, main pushes and the nightly schedule run
deliberately different amounts of work. Cheap tiers keep PR feedback fast; the
queue, main and nightly tiers prove the exact trees that land.

### Pull request

`.github/workflows/ci.yml` runs the affected-project fast checks, contract
checks and deployment acceptance. The iOS workflow builds the XCFramework and
test products once, then runs the Unit plan and a small UI smoke selection; the
full 43-case UI plan does not run on every push. The workflow has no path filter
so every revision reports `iOS gate`, but the heavy jobs skip when no
iOS-facing build input changed. `CI gate` and `iOS gate` are the required checks
on main.

`Full integration` reports a result on every PR revision but only runs the
suite when the `run-integration` label is attached to that exact event. Opening
or updating a PR, or leaving the label attached, does not start the heavy suite.
After pushing a repair, remove and re-add the label to test the new revision.
The label must exist in the repository; create it once through GitHub's Labels
page if needed. JSON/HTML reports are available in the run's
`integration-<run-id>-<attempt>` Artifact for seven days. A complete run builds
the integration image and executes the whole isolated suite, so it takes about
90 minutes; it is therefore reported but not a required check on this
repository.

### Merge queue (not available here)

The workflows accept `merge_group` and run `CI gate`, the complete integration
suite and the full iOS UI plan against the exact merge-group tree. The merge
queue itself is a repository ruleset feature that GitHub currently rejects for
this personal public repository (`Invalid rule 'merge_queue'`), so the tier is
inactive here. No workflow change is needed if the queue becomes available
later, for example after moving the repository into an organization with the
feature. Enabling the ruleset with required `CI gate` and `iOS gate` checks is
already done; the merge-queue rule can be added to it unchanged.

### Main and nightly

A main push reruns the full iOS UI plan for the landed tree. Full integration
remains scheduled by `.github/workflows/integration.yml` at 03:17 Asia/Shanghai
and the iOS full plan at 02:47 Asia/Shanghai (18:47 UTC); GitHub scheduling is
best-effort, and manual dispatch can validate a repair or high-risk change
without waiting.
iOS dispatch accepts `unit`, `smoke` or `full`; choose the PR branch and `full`
to run the complete UI plan before merging.

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

Pull-request and merge-queue integration runs queue per revision. Scheduled,
manual and reusable invocations share one global group and never cancel an
active run. GitHub may replace an older pending run when several queue;
cancelled callers must never be treated as successful publication gates.
JSON/HTML reports and successful-input receipts are uploaded as
`integration-<run-id>-<attempt>` Artifacts with seven-day retention. The cache is
an optimization, not a permanent evidence archive. No Summary or Pages is used.

The manual iOS dispatch accepts `unit`, `smoke` or `full`, so a flaky UI case can
be retried without pushing a commit that would supersede the run.

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
