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

Every native scenario records provenance before a case runs. The connector
driver reports the files it actually loaded and a content digest of
`host/runtime/src` and `host/dsh/src`; the harness compares those digests with
the mounted worktree and requires each loaded entry to resolve inside
`/workspace`. A cached, copied or registry bundle therefore fails the scenario
instead of producing results for code that was never edited. The driver also
writes transport counters (adapter events, live preview attempts and accepted
or rejected publishes, and native stream frame phases) next to the provenance
record so a failing case can separate "nothing was produced" from "production
was dropped".

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
  per-crate coverage on Linux, macOS 26 and Windows Server 2025. Linux also owns
  the Go, Node.js, Kotlin and Python groups and the separate container
  integration suite. All selected native matrix jobs must pass the
  `Full CI gate`.
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

The ordinary `.github/workflows/ci.yml` only runs quick iteration checks on Linux.
The opt-in `.github/workflows/full-ci.yml` owns the full platform matrix:

| Job | Runner | What it proves |
|---|---|---|
| Checks and coverage | `ubuntu-26.04` | Static checks, builds and affected-project unit coverage. |
| Native Rust checks and coverage | `macos-26`, `windows-2025` | Native checks, builds, unit tests and per-crate coverage. |
| Android device suites | `macos-26` emulator; physical device for performance | Compose fixtures, app and Core bridge native/Keystore/TLS checks and the explicit Macrobenchmark. |
| Android bridge JVM tests | `macos-26` | Core bridge adapter JVM cases without `adb` or a device. |
| iOS stack integration | self-hosted macOS (`WHALEREIN_IOS_STACK_RUNNER`) | Nightly or manual `ios-integration` against the disposable fixture stack. |
| Deployment acceptance | `ubuntu-24.04` | Compose restart/recovery, shutdown, backup/restore and retention. |

The full native jobs reuse the affected-project selection and invoke the repo
module through Python. Platform-gated tests execute on their matching OS; a skip
on another OS is not evidence. Every selected job is required by `Full CI gate`.

Named platform-gated tests include
`windows_sharing_violation_is_retryable_admission_contention`,
`windows_directory_handle_blocks_renaming_until_released`,
`windows_reparse_points_are_rejected_without_touching_target`,
`unix_epipe_is_not_retryable_admission_contention`, and the macOS-only
`scutil` display-name path in the CLI. The Windows reparse-point test needs
Developer Mode or symlink privilege on the runner; a failure there is a runner
capability failure, not permission to skip the case in another job.

## Android layers

The Android client has one JDK-only ordinary layer, one explicit host-side
bridge layer and three explicit device layers:

| Layer | Entry | Prerequisites | Ordinary PR |
|---|---|---|---|
| Kotlin/JVM state | `./repo test --project android-state` | JDK 21 and the Gradle wrapper only. No Android SDK, NDK, emulator or Rust compiler. | Yes, together with `./repo check android-state`. |
| Core bridge JVM | `./repo test android-bridge-jvm` | JDK 21, Android SDK, Cargo and `protoc`. No `adb` or device. | No. Full workflow, explicit label event or manual dispatch only. |
| Compose UI | `./repo test android-ui` | Android SDK, `adb`, a compatible target, and the fixture test APK. | No. Explicit label event, manual dispatch or full workflow only. |
| Native integration | `./repo test android-integration` | Android SDK, `adb`, a device, and the official local stack when `WHALEREIN_ANDROID_LOCAL_STACK=1` is requested. Runs the app suite and the `:core-bridge` instrumentation suite. | No. |
| Performance | `./repo test android-performance` | Physical device, Android SDK and a release-like Macrobenchmark package. | No. |

`android-state` is the only Android project registered in
`tools/repo/projects.toml`; bare `./repo test` selects it as a unit project and
never selects a device suite. Focused JVM runs pass the literal selector to the
state build's `-PcaseFilter` condition, which disables non-matching bodies
before execution; discovery uses `-PcaseList=true`, which disables every body.
Filter-skipped JVM cases are not selected evidence; a genuinely skipped
selected case is reported by an unfiltered run. Device targets accept
`--only` (a literal case-ID substring), `--list` and `--json`, and save the
common repository report with the selected suite, tasks, case IDs, statuses,
tested revision, and device ABI/API/page-size facts when `adb` can report them.
`--only` with no observed match is a usage error. `android-bridge-jvm` accepts
`--only` and `--json`; it has no body-free list mode.

A missing SDK, device, physical device or requested local stack is recorded as
unavailable evidence with prerequisite exit status 3. It is not a skip and not a
pass, and it cannot be used as an Android acceptance result. Kotlin/JVM
`--coverage` runs JaCoCo and enforces the per-package 90% line gate; it never
reports a passing coverage gate without measurements.

Android device work is bound to the tested revision. The `run-full-ci` label
event, manual dispatch and the full workflow may request it; a later push that
merely leaves the label attached stays on the quick path. The device path
filter treats `apps/android/`, `core/`, `apps/mobile-bindings/`, `proto/` and the
Android runner files as native build inputs, so a labeled change to a shared
input selects the device and bridge suites. It does not add device work to an
ordinary push. `CI gate` requires only the JDK-only Android quick checks on
ordinary pushes, and `Full CI gate` requires the selected device and bridge JVM
jobs. See
[apps/android/README.md](../apps/android/README.md) for build, signing and
device-suite commands.

## Contract checks

The `Contract checks` job runs when the affected set includes the `proto`
project. It is offline and runs three checks:

```bash
./repo check proto
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

Use the smallest scope that exercises the changed behavior, then finish with
complete affected-project validation. Run from the repository root:

| Stage | Selection | Completion |
|---|---|---|
| Iteration | `./repo check ID` and `./repo test ID --only TEXT` | The changed behavior and regression cases pass; coverage is deferred. |
| Final code validation | `./repo test ID --coverage` for affected projects, plus relevant checks/builds | Run once after the final relevant edit; each affected first-party scope meets the coverage gate. |
| Documentation only | Review links, command accuracy and `git diff --check` | Product tests are needed only if executable behavior also changed. |

Select project IDs explicitly for agent-driven format/check/build/unit commands.
Use known case names, source searches or saved reports to choose `--only`;
`--list` can compile Rust or build an integration image and is not a free probe.
A focused pass verifies only its selected behavior. It does not replace final
unit coverage, native platform validation, or a required integration run.

Before expanding or repeating tests, state the new relevant source/test/config
edit, observed failure, or specific uncovered risk that requires the run. Once
checks pass, document wording, PR descriptions and progress updates alone do not
justify repeating product tests. A base merge or dependency/toolchain change may
invalidate results and must be assessed. This is a selection rule, not automatic
result caching: the tool always executes the requested tests.

Choose integration scope by behavior, not the number of changed directories:

- Protocol schema, serialization or cross-language binding changes: run
  `./repo check proto` and `./repo test protocol`.
- Storage transactions, persistence or migrations: select `./repo test storage`.
- MQTT/TLS connections, transport framing or reconnect handling: select
  `./repo test transport`; add chain cases if end-to-end recovery is affected.
- CLI/Access/Host request flow, approval, deduplication or recovery: select
  relevant `./repo test chain` cases using `--only TEXT` during iteration, then the
  affected group at completion.
- Real DSH adapter, session or browser interactions: select `./repo test native-dsh`.
- Run the complete integration suite for release validation or shared changes
  with identified risks across multiple groups that narrower checks cannot
  establish. Name those risks before starting; crossing directories alone is
  insufficient. A full suite deferred to CI is reported as pending, not passed.

Bare `./repo test` selects all registered unit projects, including the JDK-only
`android-state`; it never selects an Android device suite. Use
`./repo test integration` for the complete Linux integration scope. State the
scope reason when running repository-wide validation.

Every test invocation displays its selected projects/group, whether a case filter
and coverage are enabled, and comparable historical wall time before compilation
or execution. History is a local observation, not a cold-build prediction or a
pass receipt. Three minutes is the quick-feedback target; a historical estimate
above it or a completed run exceeding it produces a warning, without cancelling
tests, changing results, asking for approval, or automatically changing scope.

For the Mosquitto line, two opt-in fixtures back the replacement acceptance
record. The native broker fixture needs the pinned Mosquitto 2.1.4 headers and
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

The stack acceptance suite stays outside the default unit suite. Record its
output under `.local-docs/acceptance/`. Use the isolated
[link performance benchmark](../tools/repo/README.md#link-performance-benchmark)
for repeatable network profiles.
See the [repo command reference](../tools/repo/README.md#test-selection) for exact
project IDs, case discovery, selector syntax and unsupported combinations.
The [integration guide](../tests/integration/README.md) describes container and
fixture ownership. Development services are not prerequisites for integration.

## Local integration timing

The Mosquitto broker binds in well under a second, and a full stack start
(pytest, Access, broker, FakeDSH) takes roughly 2–3 seconds once per session.
Wall time is dominated by per-case pairing setup (about 0.5 s), real command
round trips, and deliberate deadline waits.

Before the verification runner cutover, the development host measured 83
`chain` cases as follows:

| Invocation | Wall time |
|---|---|
| `./repo test chain` | ~200 s |
| `./repo test chain --parallel 4` | ~64 s |

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

`.github/workflows/ci.yml` runs parallel affected-project quick checks. Rust
checks formatting without compiling; Go, Node and Python run static checks and
unit tests without coverage or a separate build; Kotlin runs the JDK-only
`android-state` Gradle checks and tests without the Android SDK, NDK or a
device. Full Rust tests, coverage, platform matrices, Android device suites, iOS
builds and deployment acceptance do not run on ordinary PR pushes. Three minutes is a performance target for each quick job, including
setup and report upload but excluding runner queueing. A final timing step warns
if a job exceeds that target without changing its test result. Quick jobs and
lightweight gate/decision jobs have ten-minute safety timeouts to stop stuck
runs. Test failures and safety timeouts still fail; slow successful jobs pass.

Add `run-full-ci` to request full checks/builds/coverage, native Rust matrices,
contract checks when `proto` is affected, deployment acceptance when Access is
affected, and iOS Unit and full UI plans for that revision. The iOS job matrix
runs Unit and four UI shards on separate Xcode 27.2 runners, plus an iOS 18 Unit
job on Xcode 16.4. Each UI shard executes its
share of the sorted XCTest inventory and uploads its own report. The iOS gate
requires the entire matrix to succeed. Only that label event requests heavy
work on PRs. After a new push, remove and re-add the label to verify the new
tree. An unrelated label
must not trigger integration, even when `run-integration` is still attached.
The workflows keep `CI gate` and `iOS gate` names required by the main ruleset;
on ordinary pushes, `iOS gate` only confirms heavy work was not requested.
It is not evidence of a successful iOS build. Full native results use the
separate `Full CI gate` check.

Full verification is opt-in before merge under the current required-check rules.
Before merging a change that needs full validation, verify the final tested
merge revision matches the current PR head and base; an old successful run does
not validate a later push or base update. Main/nightly runs are additional
regression checks and do not replace pre-merge validation.

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

The workflows accept `merge_group` and run quick checks, full native verification,
the complete integration suite and the full iOS UI plan against the exact merge-group tree. The merge
queue itself is a repository ruleset feature that GitHub currently rejects for
this personal public repository (`Invalid rule 'merge_queue'`), so the tier is
inactive here. No workflow change is needed if the queue becomes available
later, for example after moving the repository into an organization with the
feature. Enabling the ruleset with required `CI gate` and `iOS gate` checks is
already done. If the queue is enabled, also require `Full CI gate` and
`Full integration` so merging waits for full verification.

### Main and nightly

A main push runs full native verification and the full iOS UI plan for the landed
tree. Full native CI also runs nightly at 02:17 Asia/Shanghai. Full integration
remains scheduled by `.github/workflows/integration.yml` at 03:17 Asia/Shanghai
and the iOS full plan at 02:47 Asia/Shanghai (18:47 UTC); GitHub scheduling is
best-effort, and manual dispatch can validate a repair or high-risk change
without waiting.
iOS dispatch accepts `unit`, `smoke`, `full` or `integration`; choose the PR
branch and `full` to run the complete UI plan before merging. The nightly and
`integration` dispatch paths run `./repo test ios-integration`, which starts the
fixture stack itself. Hosted macOS runners do not provide Docker, so that job
reads `vars.WHALEREIN_IOS_STACK_RUNNER` for a self-hosted macOS runner with
Docker and Xcode. Until the variable points at such a runner, the scheduled job
fails its Docker preflight; this is a repository-runner setting, not a secret.
The Unit plan's `ContractParityTests/testLocalStackParity` stays an explicit
skip; the live contract comparison is the stack-backed integration plan.

The scheduled Integration workflow freezes one main commit and fingerprints all tracked inputs except
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
The Kotlin/JVM `android-state` project uses JaCoCo line coverage and the same
90% gate per package. `--coverage` runs the Gradle verification task and reports
each package; ordinary push CI does not request coverage, while full CI does.

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
