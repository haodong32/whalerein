# WhaleRein repository tasks

`./repo` coordinates native engineering tools through Python and uv. Project
processes belong to each project's README; the server stack belongs to
[deploy](../../deploy/README.md). The product executable `whalerein` is separate.
For Agent task selection, use the
[whalerein-repo skill](../../.agents/skills/whalerein-repo/SKILL.md).

## Commands

Run commands from the repository root. Run `./repo` or `./repo --help` for the
command overview and common examples. Use `./repo COMMAND --help` for parameter
meanings, defaults, selection constraints and command-specific examples. Invalid
arguments include a hint to the relevant help; a bare `./repo` exits successfully
without executing a task.

| Command | Behavior |
|---|---|
| `projects` | List registered IDs, paths, languages and supported operations. |
| `doctor [TARGET]` | Probe basic tools or prerequisites for a target. |
| `fmt [TARGET]` | Write formatting changes. |
| `check [TARGET]` | Check formatting and run static analysis without modifying source. |
| `build [TARGET]` | Build applicable projects; Access writes `.data/bin/whalerein-access`. |
| `generate ios` | Generate the Xcode project on macOS. Xcode prepares Rust bindings during builds. |
| `test [TARGET]` | Run native unit tests by default; see selection below. |
| `report RUN_DIRECTORY` | Render saved report.json as report.html in the run directory. |
| `recover RUN_DIRECTORY` | Clean up resources whose ownership is recorded in an abandoned run. |
| `proto` | Generate committed Go/JS bindings. Use `check proto` to verify consistency. |
| `bench` | Measure the plugin–Access–client path under controlled network conditions. |
| `bench-compare -b DIR -c DIR` | Compare saved benchmark runs under the predeclared protocol. |

Targets are exact IDs scoped to the action. Bare `check` selects local static
checks and bare `test` selects unit targets. Command help lists the valid targets
and their tools. Inspect [projects.toml](projects.toml) or `./repo projects` for the
registered native projects.

Rust formatting targets selected packages. Node format/check/build operations
require manifest scripts (`fmt`, `fmt:check`/`lint`, `build`). Python supports
format/check/test. `check` does not execute tests, start services or generate
bindings. `check proto` verifies the generated Go/JS bindings. `check proto-rust`
selects only the Rust binding project's static checks.

`doctor` probes executables and versions without installing tools. Package
installation, daemon readiness and deployment behavior require separate checks.
Integration requires host Python/uv and Docker buildx; native compilers run in
the image. The server stack separately requires Compose.

## CI project selection

GitHub CI compares the PR base with the tested merge revision, or a main push's
previous revision with its new revision. Deleted/renamed paths participate in
selection. Documentation-only changes skip product jobs; unknown inputs select
all affected projects except the Android-only state project. Missing history
fails selection rather than silently skipping.

`projects.toml` remains the project inventory. Its `depends_on` relationships
expand affected consumers: Core changes include CLI and noise-peer; Host changes
include DSH. Rust/Node root manifests and lockfiles select their ecosystem;
protocol, fixtures, shared tooling, workflow, deployment and integration changes
select all projects except the Android-only project. Android state source and
shared Android Gradle inputs select only `android-state`; app and bridge changes
belong to the explicit Android workflow. Selection changes do not change local `./repo test` defaults.

The push workflow runs independent Rust, Go, Node, Kotlin and Python quick jobs
in parallel, without a preceding selection job. Each job selects affected
projects before installing toolchains. Rust only checks formatting; Go, Node and
Python run checks and unit tests without coverage or a separate build; Kotlin
runs the JDK-only `android-state` Gradle checks and tests with JDK 21 and no
Android SDK, NDK or device. These are partial
iteration checks, not evidence of Rust compilation, platform behavior or coverage.
Three minutes is the quick-job performance target, including setup and report
upload but excluding runner queueing. A final timing step emits a warning after
completion if a job exceeds the target; elapsed time alone does not change test
success. Quick jobs, the final gate and lightweight iOS decision/gate jobs have
ten-minute safety timeouts to stop stuck runs. Test failures and safety timeouts
still fail the checks. Superseded push runs are cancelled.

The separate `full-ci.yml` runs on the `run-full-ci` label event, manual dispatch,
main pushes, merge queue and nightly schedule. Leaving the label attached does
not rerun heavy jobs on later pushes. Full verification runs checks, supported
builds and unit coverage, plus deployment acceptance when `access` is selected,
protocol contracts when `proto` is selected, and the Android device and Core
bridge JVM suites when Android-facing paths changed. `core/`,
`apps/mobile-bindings/` and `proto/` count as native Android inputs on an
explicitly requested run. A manual dispatch can additionally request the
physical-device Macrobenchmark. Android device work is never part of the
ordinary quick `CI gate`. Protocol generation is checked
once in the contract job rather than again in the Node job. `Full CI gate` requires every selected job; it is distinct
from the required quick `CI gate`.

Affected Rust projects additionally run on `macos-15` and `windows-2025` native
runners, with checks, builds and per-crate coverage gates. The native matrix uses
the same project inventory and consumer expansion as Linux. Native failures
block `Full CI gate`. Client-environment contract files select Core and Host and
their consumers, without scheduling unrelated Access or repository tooling.
The macOS Core job explicitly runs the otherwise ignored Keychain round-trip
in a temporary keychain and verifies execution and cleanup. Windows also runs
focused repository-runner tests for its native pipes and process-tree deadlines.
The Windows runner buffers child output because Windows selectors cannot watch
subprocess pipes; timeout/interruption retains observed output and terminates
the child tree. CI invokes the repository module through its active Python
interpreter so Windows does not need to execute the Unix `./repo` launcher.
For the same entry on Windows, use
`uv run --project tools/repo python -m repo <command>`.
`repo.native_ci` installs checksummed native protoc 36.2 and owns the isolated
Keychain check; it does not start development services. macOS/Windows reports
are uploaded as `ci-native-<os>-<sha>-<attempt>` Artifacts.
Native dependency/build caches are also retained after failed verification so
debugging does not repeatedly rebuild the same toolchain. These caches contain
build artifacts, not successful-test receipts.

Inspect selection locally without running tests or contacting GitHub:

```bash
python3 tools/repo/src/repo/ci.py plan --base BASE_SHA --head HEAD_SHA
uv run --project tools/repo python -m repo.ci run --projects '["core", "cli"]'
```

For the lightweight push path:

```bash
python3 tools/repo/src/repo/ci.py quick-plan --base BASE_SHA --head HEAD_SHA --kind rust
uv run --project tools/repo python -m repo.ci quick --projects '["core", "cli"]'
```

The `run` command executes selected projects through `./repo check`, applicable
`./repo build` operations and `./repo test --coverage`. Rust registry/build caches
and the pinned coverage tool are cached separately. Full CI disables Rust
incremental compilation and uses line-table-only debug information to reduce
cache/build size. Profile-only cleanup preserves cached instrumented builds,
while removing old execution counts before each package measurement. Reports
remain downloadable Artifacts with seven-day retention; no Summary or Pages publishing is configured.
The Go artifact also includes the native coverage profile when available, even
when the run fails its coverage gate.

## Integration CI

The separate Integration workflow provides merge-queue, daily (03:17
Asia/Shanghai), manual and reusable release-gate execution. See
[CI test tiers](../../docs/testing.md#ci-test-tiers) for trigger, evidence,
concurrency and report rules. It calls `./repo test integration`, which
starts its own isolated test runtime.

The workflows already run the complete suite on a `merge_group` tree, but this
personal repository cannot enable GitHub's merge-queue ruleset, so that tier is
inactive here. On a pull request, add the `run-integration` label to request the
suite for that label event; later pushes do not rerun it, so remove and re-add
the label to verify a new revision. The check is reported but not required. PR and queue runs do not establish the
scheduled main-branch cache baseline. Reports are uploaded as
`integration-<run-id>-<attempt>` Artifacts.

Every run reclaims unused preinstalled images and SDKs on its disposable
GitHub-hosted Linux VM and requires 24 GiB free before image building. The
storage helper rejects local, self-hosted and non-Linux environments.
The runner preserves observed execution facts when a run fails.

Inspect its content fingerprint locally:

```bash
PYTHONPATH=tools/repo/src python3 -m repo.integration_ci key --revision HEAD
```

Only complete successful full-suite reports can create a cached receipt; errors,
partial selection and discovery-only runs cannot advance the baseline. Manual
and release callers always rerun; scheduled execution may skip identical inputs.
The GitHub workflow and receipt helper remain distinct from the native integration
runner, which owns image building, execution and verified cleanup.

## Output and exit status

Global `--json` writes one `whalerein/repo/v1` result envelope to stdout; progress
and diagnostics go to stderr. `--verbose` enables bounded, sanitized diagnostics.
Help and version requests produce informational text. Native subprocess exit
codes are retained in results; the CLI reports:

| Exit | Meaning |
|---|---|
| 0 | Success. |
| 1 | Operation, collection or coverage failure. |
| 2 | Invalid CLI request, unknown target or unmatched case filter. |
| 3 | Missing prerequisite reported by doctor or a prerequisite error. |
| 130 | Interruption. |

There is no global dry-run or native argument passthrough. Use native tools for
specialized options that the repository interface does not expose.

## Link performance benchmark

`./repo bench` measures delivery through the real Host plugin, broker, Access
and CLI in an isolated Linux container. Each Watch attaches before FakeDSH
emits a fixed 32-increment reply at an independent cadence. The first actual
source emission starts the clock. The fixture checks for zero native sends.

```sh
./repo bench
./repo bench --scenario recovery
./repo bench --scenario mixed-load
./repo bench --network jitter --link client
./repo bench-compare --vary network -b suite/watch-normal-host -c suite/watch-high-rtt-host
```

### Default suite

| Condition | Workload |
| --- | --- |
| Normal | One Watch with the default text cadence and no extra network impairment |
| Sustained weak network | The same Watch with added RTT, jitter, and random loss as three separate cases on the Host link |
| Brief outage | The same Watch during a client-link blackout. The fixture forces the existing Watch TCP connection down and restores the link after the scheduled interval |
| Mixed load | The same Watch alongside four online history reads scheduled independently at fixed offsets. Each read retrieves eight 8 KiB messages through a separate CLI Device |

The default suite runs six cases from one frozen source snapshot and build.
Each case owns its container and `benchmark.json`. The root `suite.json` indexes
case status, correctness, P95 streaming metrics and workflow observations.
Failed cases stay in the report while subsequent cases run. An interruption
leaves unstarted cases pending.

Set `--scenario watch`, `--scenario recovery`, or `--scenario mixed-load` for
one workload. Supplying `--network` or `--link` also selects a single case.
The recovery workload controls the client link so it can exercise Watch
reconnect and catch-up. Its default blackout begins at 800 ms and lasts 1 s.
The start follows the source cadence. The duration is capped at 1 s and
shortens for fast cadences so source text continues during recovery.

| Profile | Added RTT for each selected link | Jitter per direction | Random loss per direction |
| --- | ---: | ---: | ---: |
| `normal` | 0 ms | 0 ms | 0% |
| `high-rtt` | 280 ms | 0 ms | 0% |
| `jitter` | 280 ms | 40 ms, normal distribution | 0% |
| `loss` | 280 ms | 0 ms | 1% |

`--link host` shapes Host MQTT. `--link client` shapes the client's MQTT and
HTTP connections. `--link both` shapes both endpoints. The fixture probes both
endpoint listeners and a reference listener before measurement.

### Results

- **Correct by deadline** counts attempts that produce the exact final reply
  and settle once in the observation window. Failures remain in the denominator.
- **Maximum display delay** measures the slowest source increment in an attempt.
  A milestone completes when the corresponding correct prefix first appears in
  the public Watch output. An increment first shown in the final reply keeps
  its original source timestamp.
- **Largest 50 ms output share** counts the most visible answer bytes written
  in any 50 ms window as a fraction of final reply bytes. It includes reprints.

The observer timestamps public CLI records on the source's monotonic clock.
Recovery reports the observed blackout and time from restoration until the
correct prefix catches up with text emitted before restoration. Mixed load
reports history correctness and time to the exact, usable online page
separately from Watch. A failed history read makes the case exit nonzero.
The report keeps the CLI's structured error code when available.
The diagnostic section reports first-text latency and the distribution of
source-to-visible delays across individual text increments from correctly
completed measured attempts. The segment percentiles describe those increments.
Formal comparisons still use per-attempt maximum display delay and burst
share. Reports retain final reply and output-gap timelines for diagnosis.

Defaults are 30 measured attempts and three warmups per case, a 100 ms source
cadence, a 10 s deadline and a 1 s duplicate observation window. Use a small
attempt count to check execution before running the full suite.

### Comparisons

Use identical workload, fixture, network and resource settings to compare
builds. `--vary network` locks the build, workload and link while the profile
changes. Repeat the same profile and build for A/A. Pair independently run
reports in the supplied order.

V6 comparisons report paired per-run P95 differences for display delay and
burst share alongside correctness. Intervals require five matched pairs,
20 successful samples for each P95, and at least 95% correctness per run,
including background reads in mixed load. Legacy reports retain their original
metrics and can be compared within the same scenario version.

## Test selection

[Test policy](../../docs/testing.md) defines when unit, integration and release
validation are required. The command accepts:

```bash
./repo test core
./repo test --project android-state
./repo test core --only reconnect
./repo test core --list
./repo test repo --coverage
./repo test protocol
./repo test chain --parallel 2
./repo test chain --only test_send
./repo test protocol-vectors --list
./repo test integration
./repo test ios-integration --simulator SIMULATOR_UDID
```

`protocol` aggregates `protocol-vectors` and `protocol-http`. `integration` runs
both protocol targets and the `storage`, `transport`, `chain`, and `native-dsh`
targets. The iOS targets require a native macOS host. Use
`ios-unit`, `ios-ui`, and `ios-integration` for whole plans, or `ios-ui-smoke`
for the CI smoke selection. Bare test selects registered unit targets only.

The Android JDK-only project is `android-state` (Kotlin/JVM, Gradle included
build). It is a normal unit target and supports `check`, `test`, `--only` and
`--list`. Focused runs use the state build's `-PcaseFilter` JUnit condition, and
listing uses `-PcaseList=true`; neither executes unselected bodies. Filtered
runs count only observed non-skipped cases, so an all-skipped result is an
unmatched selector. Its `--coverage` runs JaCoCo verification and reports
per-package line ratios against the 90% gate; ordinary quick CI does not request
coverage. The explicit device targets are `android-ui`, `android-integration`
and `android-performance`; they require `adb`, the Android SDK and a compatible
target, while `android-performance` requires a physical device.
`android-integration` runs both the app instrumentation suite and the
`:core-bridge` instrumentation suite. `android-bridge-jvm` runs the Core bridge
adapter JVM cases with the Android SDK, Cargo and `protoc`, but no `adb` or
device. A missing prerequisite produces unavailable evidence with exit status 3.
Instrumentation case selection filters parsed case IDs, and an unmatched
`--only` is invalid usage. See
[apps/android/README.md](../../apps/android/README.md).

| Option | Targets | Additional constraints |
|---|---|---|
| `--project ID` | `test` only | Unit project ID; equivalent to the TARGET positional argument. Do not pass both. |
| `--only TEXT` | Leaf targets | Literal, case-sensitive case ID substring. An unmatched filter is invalid usage. Android device suites filter parsed instrumentation case IDs. |
| `--coverage` | Unit targets | No `--only` or `--list`. Kotlin/JVM `android-state` uses JaCoCo per-package lines; the Android device suites reject the option. |
| `--list` | Unit, Linux integration, iOS leaves, and Android device leaves | No coverage; Node static listing is unsupported. Kotlin listing runs Gradle list mode; Android device listing runs instrumentation in log mode and requires a device. `android-bridge-jvm` has no list mode. |
| `--parallel N` | Linux integration targets | 1–8 independent hash shards, default 1; incompatible with `--list`. |
| `--integration-timeout SECONDS` | Linux integration targets | Positive run deadline covering preparation through execution, defaults to 7200 seconds. |
| `--simulator UDID` | iOS targets | Exact Simulator device UDID. |
| `--shard INDEX/COUNT` | `ios-ui` | Run a 1-based shard of the sorted XCTest inventory, with 2–8 shards. Incompatible with `--only`. |

Run different iOS shards on separate simulators or runners. Cover all shards of
one revision and one test plan to verify the full UI suite.

`--parallel N` preserves the selected case set and distributes cases by a stable
CRC32 of each pytest node ID. Each shard has its own runtime and result export.

`--list` discovers cases without executing test bodies. Rust discovery can
compile tests and integration discovery can build its image. Node static listing
is unsupported and fails explicitly. Go lists top-level cases; dynamic children
require execution and can be selected by a full `package::TestParent/child` ID.
Necessary imports, fixtures and parent test code can still execute for a selected
child. Unselected cases do not inflate totals.

Rustdoc uses the case IDs shown by discovery, including for compile-only
examples. Select the complete documentation target with
`./repo test core --only doctest::`. A narrower selection runs only when
Rustdoc can isolate the selected examples from the rest of the inventory.

Node IDs use `file::parent/child` name paths. Each segment escapes `~` as `~0`
and `/` as `~1`: a root named `parent child`, a root named `parent/child` and
`child` under `parent` have distinct IDs. Line numbers are location metadata.
File-qualified name filters anchor at the beginning of the native full name
before execution. Node joins ancestor names with spaces for native matching;
use distinct native full names when targeting individual cases.

Selectors use the encoded ID spelling, including when only a name substring is
provided. For a root test named `parent/child`, the name portion of its ID is
`parent~1child`; for the literal name `parent~1child`, it is `parent~01child`.
For example, with project ID `host` and file `test/example.test.js`:

```bash
./repo test host --only 'parent~1child'
./repo test host --only 'host::test/example.test.js::parent~01child'
```

The first selects the slash name; the second selects the literal tilde name.
The example file/names illustrate syntax; use actual IDs from your saved report.

## Test scope and cost notices

Use [staged validation](../../docs/testing.md#choose-checks-for-a-change) to choose
iteration, final unit coverage and integration scope. During iteration, select a
target explicitly.

Before discovery, compilation or execution, public unit and Linux integration
tests print the selected projects or target, case-filter state, coverage state
and parallelism.
The filter text itself is not printed. JSON mode keeps notices on stderr and
adds `historical_duration` (`samples`, `median_ms`) to the result data.

History reads at most the 100 most recent `.data/runs/*/report.json` files, up to
8 MiB each, and takes the median of up to five complete successful runs with the
same target or project set, case filter, coverage/discovery mode, parallelism,
OS and CPU architecture. New reports record `timing_environment`;
older reports without it, corrupt/unreadable reports, failed/incomplete runs and
missing durations do not supply a cost estimate. Custom `--output` destinations
outside that directory remain valid reports but are not searched for history.

No comparable run means **unknown**, not zero. History includes recorded run wall
time (including test compilation), not a sum of case timings or a cold-build
prediction. Tool versions, input revisions, cache state and machine load may
vary; history neither proves current correctness nor prevents re-execution.
The three-minute quick-feedback target only produces warnings for expensive
history or an observed long run. Test results and existing execution deadlines
continue to determine success. There is no new deadline, approval, automatic
selection or result-skipping mechanism.

## Coverage and report facts

The repository enforces 90% per first-party scope. Go's native unit is statements
per package; Rust, Node and Python report lines. Generated protocol bindings are
excluded from coverage, but their tests still run. Missing tools, measurements
and below-threshold results fail the gate; passing cases alone do not satisfy it.

Python, Go, Node and Kotlin/JVM collect coverage with their case results in one
execution. For Go projects, use `go tool cover -func=<profile>` or `go tool cover
-html=<profile>` with the `coverage-<project>.coverprofile` in the report directory
to inspect the measured code blocks. Kotlin reports each JaCoCo package from
`apps/android/state/build/reports/jacoco/test/jacocoTestReport.xml`; the state
build also enforces its 90% per-package line rule.
Node keeps manifest coverage include/threshold flags. Rust uses a separate
serial cargo-llvm-cov execution in a temporary build directory so cached
binaries and profile counts do not enter the measurement. The report labels
it `separate_measurement` and excludes those cases from execution totals.

Reports distinguish selection, inventory completeness, observed outcomes,
execution errors and coverage. Build/service failures are errors, not invented
cases. Native parent tests with results count; pure suites do not. IDs must be
unique within a scope. Progress events are presentation, not authoritative facts.

Cancellation stops subsequent native targets/packages and retains observed
results, including captured output from process cleanup. Known unexecuted cases
remain `not_run`; undiscovered Rust targets appear in `not_run_targets` without
invented case identities. An incomplete inventory has no asserted total count.

Run and scope wall time are distinct from native case/pytest phase timings.
Shared fixture setup belongs to its first consumer. Case times must not be summed
to infer wall time; missing or interrupted observations are not invented. Rust
stable libtest has no reliable per-case timing in this collector.

Runs default to unique directories under `.data/runs/`. Use `--output DIR` to
select a new run directory. `report.json` is authoritative and the tool renders
`report.html` from those facts. Environment-backed verification reports include
source and artifact identities, runtime checks, measured phase outcomes and
cleanup. Interrupted runs retain available observations. The offline HTML shows
the run outcome separately from inventory and case outcomes.

```bash
./repo test repo --output .data/runs/my-check
./repo report .data/runs/my-check
```

Rendering uses only saved facts. Use `./repo recover RUN_DIRECTORY` after an
abandoned run to acquire its lease and retry cleanup of its recorded resources.
Diagnostic
redaction is bounded and best effort; tests must not print secrets or private
business contents.

## Architecture

[projects.toml](projects.toml) is the sole project inventory. It registers stable
IDs, paths, execution kinds, roles and necessary target configuration. Names and
scripts come from native manifests; new projects require explicit registration.

| Module | Owns |
|---|---|
| `cli.py` | Request parsing, dispatch, final output and exit status. |
| `targets.py` | Action target support, scope resolution, and prerequisite help. |
| `projects.py` | Registration validation and exact ID selection. |
| `toolchain.py` | Sequential formatting/check/build coordination. |
| `testing.py` | Public test routing, native unit execution and coverage policy. |
| `verification.py` | Shared run directory, phases, deadlines, cleanup and report. |
| `sources.py` | Frozen source inputs and their identities. |
| `integration_targets.py` | Linux integration scope, collection paths and artifact requirements. |
| `test_cost.py` | Advisory selection notices and comparable local wall-time history. |
| `runners/` | Ecosystem-specific commands, discovery and coverage. `runners/kotlin.py` owns the JDK-only Gradle state runner; `runners/android.py` owns explicit device suites and the Core bridge JVM suite. |
| `process.py` | Child processes, deadlines, process-group cancellation and capture. |
| `reports/model.py` | Versioned facts, validation and atomic persistence. |
| `reports/html.py` | Offline HTML presentation. |
| `doctor.py`, `proto.py`, `commits.py` | Independent repository workflows. |

The CLI resolves a target once and delegates to its native runner.
Linux integration uses a stable runtime image and compiles the selected product
artifacts from frozen inputs with native incremental caches. It verifies the
runtime identity and merged case inventory before finalizing its report.
`--parallel N` creates separate owned containers and exports, with a bounded
cleanup independent of the execution deadline. See the
[integration implementation](../../tests/integration/README.md).

## Maintaining the tool

```bash
./repo fmt repo
./repo check repo
./repo test repo --coverage
```

Read [commit rules](../../docs/commits.md) before committing. Git hooks invoke
`repo.commits` internally; their installation instructions live in that guide.
