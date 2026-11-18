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
| `test [TARGET]` | Run native unit tests by default; see selection below. |
| `report RUN_DIRECTORY` | Render saved report.json as report.html in the run directory. |
| `recover RUN_DIRECTORY` | Clean up resources whose ownership is recorded in an abandoned run. |
| `proto` | Generate committed Go/JS bindings. Use `check proto` to verify consistency. |
| `bench` | Run the opt-in streaming-reply product benchmark in an isolated container. |
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
all projects. Missing history fails selection rather than silently skipping.

`projects.toml` remains the project inventory. Its `depends_on` relationships
expand affected consumers: Core changes include CLI and noise-peer; Host changes
include DSH. Rust/Node root manifests and lockfiles select their ecosystem;
protocol, fixtures, shared tooling, workflow, deployment and integration changes
select all projects. Selection changes do not change local `./repo test` defaults.

The push workflow runs independent Rust, Go, Node and Python quick jobs in
parallel, without a preceding selection job. Each job selects affected projects
before installing toolchains. Rust only checks formatting; Go, Node and Python
run checks and unit tests without coverage or a separate build. These are partial
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
builds and unit coverage, plus deployment acceptance when `access` is selected
and protocol contracts when `proto` is selected. Protocol generation is checked
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

## Product benchmark

`./repo bench` measures one client-observable workflow: a user sends a message
while the conversation is open, and the benchmark records how long the first
visible reply text and the correct final reply take. It uses the Linux
integration builder and the same artifacts for the real CLI, Access, Host and
plugin against FakeDSH. Run it explicitly with `./repo bench`.

```bash
./repo bench
./repo bench --attempts 50 --warmup 5 --output .data/benchmarks/baseline
./repo bench --attempts 20 --delay 50 --output .data/benchmarks/delayed-50
./repo bench-compare -b .data/benchmarks/baseline -c .data/benchmarks/delayed-50
```

| Option | Meaning |
|---|---|
| `--attempts N` | Measured attempts after warmup (default 30). |
| `--warmup N` | Setup-quality attempts excluded from every result (default 3). |
| `--delay MS` | Injected delay on the scripted reply, for calibration (default 0). |
| `--cadence MS` | Spacing between scripted visible updates (default 100). Values at or below the product's 20 ms coalescing window make that window observable. |
| `--deadline MS` | Per-attempt deadline measured from dispatch (default 5000). |
| `--observation-window MS` | Window after the final reply used to detect duplicates (default 1000). |
| `--seed N` | Recorded scenario seed (default 0). |
| `--concurrency N` | Only the sequential value `1` is supported. |
| `--benchmark-timeout SECONDS` | Container execution deadline (default 900). |
| `--output DIR` | Report directory; one that already holds `benchmark.json` is rejected. |

### Measurement surface

The observation surface is **CLI stdout**, recorded by one monotonic clock in
the driver process. Each attempt:

1. creates a fresh session and loads it with `whalerein session show`,
2. starts `whalerein --json session watch` and waits for its initial session state,
3. arms the fixture's scripted reply and invokes `whalerein session send --detach`,
4. reads watch output and records milestones.

Each benchmark device starts from a selected Standalone TOML and pairs with an
invitation passed on standard input.

Dispatch is taken immediately before the send process starts, so first-text and
final latency include send start-up. The fixture drives its reply schedule from
the *acceptance* of that send on fixed relative times; nothing reads CLI output
to decide when to emit, so the observer cannot advance or delay the schedule.

Two paired devices are used per run: the observer holds the live watch and the
sender performs the measured send. A single device cannot do both, because the
sending CLI displaces the watching CLI's live delivery.

The first-text milestone requires non-empty reply text from the current attempt.
Lifecycle markers, empty previews, and replies carrying another attempt's
identity never satisfy it. Attempt identity is unique per attempt, which is what
distinguishes a reply from a previous attempt's streams.

### Cadence and the coalescing window

Core coalesces adjacent deltas for up to 20 ms or an 8 KiB target before they
reach the client. A 100 ms cadence sits well above that window, so the measured
update gaps describe delivery jitter; a cadence at or below it turns the
coalescing window itself into the measurement. Use a short cadence to look for
batch-shaped gaps (multiples of the window) rather than smooth jitter:

```bash
./repo bench --cadence 20 --output .data/benchmarks/cadence-20
./repo bench --cadence 10 --output .data/benchmarks/cadence-10
```

The recorded `scenario.workload.gap_ms` and `run.configuration.cadence_ms` must
agree; a report whose cadence cannot settle before its deadline is rejected as
a usage error before any container starts.

### Outcomes

Every measured attempt records exactly one outcome:

| Outcome | Meaning |
|---|---|
| `completed` | Correct final content observed within the deadline. |
| `product_error` | The send command failed. |
| `timeout_before_text` | No reply text before the deadline. |
| `timeout_after_text` | Text appeared, but no correct final reply by the deadline. Partial timing is kept. |
| `incorrect_content` | A final reply arrived with the wrong content. |
| `duplicate_final` | A second final reply arrived inside the observation window. |
| `late_reply` | A final reply was observed only after the deadline; it is never a completion. |

The success share is `completed` over all measured attempts. Warmup attempts are
reported separately and excluded from every count and sample.

### Artifacts

`benchmark.json` is written atomically and validated before publication. It
carries the schema version, scenario identity and parameters, observation
surface, build and environment metadata, per-attempt timings and outcomes,
outcome counts, latency samples, and the run status with cleanup verification.

Timings are recorded from dispatch, so `first_text_ms` and `final_ms` include CLI
process start-up. `send_exit_ms` is when the send process exits after collecting
its result, not a start-up or reply time. The start-up share of an attempt can be
read back from the report's own constants: because the scripted final reply is
emitted `scenario.response.final_ms` after acceptance, `final_ms` minus that
constant is the dispatch-to-acceptance interval. For the default scenario that is
roughly 290 ms, against roughly 5 ms from acceptance to first visible text, so the
headline first-text number is dominated by process start-up rather than by
delivery.

Latency samples cover only correctly completed attempts and report nearest-rank
median, p95, and p99 with the actual sample count. A quantile that the sample
count cannot support is labelled unavailable with its reason instead of being
invented. Raw credentials, pairing material, and conversation contents are never
exported; the report holds sanitized timings and outcome labels.

A run is `complete` only when setup, execution, and cleanup all succeeded. On
interruption, failed setup, an invalid export, or unverified cleanup the runner
exits non-zero, keeps the observations it already holds, labels the evidence
`incomplete` or `invalid` with the reason, and removes only the container it
started.

Exit status for `bench`: `0` when the run completed and every measured attempt
succeeded; `1` for failed attempts, invalid execution, or an incomplete run;
`2` for usage errors; `3` for a missing prerequisite; `130` for interruption.

### Comparison protocol

Comparisons use the declaration in `repo/benchmark/analysis.py`: primary metrics
`first_text_ms` and `final_ms`, a ±25 ms tolerance, a floor of 20 attempts per
run and 5 matched run pairs, and a two-sided 95% t interval over the paired
run-level differences. Declare any change to these numbers before measuring
candidates.

`./repo bench-compare` pairs runs in the order given, so alternate or randomize
the order yourself and restore each run's declared initial state. It reports the
estimate and interval per metric and classifies it as `improvement`,
`regression`, `within_tolerance`, or `inconclusive`. Too few pairs, any run
below the declared minimums, or an interval crossing a tolerance boundary is
`inconclusive`; a latency improvement is only accepted when the correctness
floor also holds.

Run an A/A pilot (the same build in both arms) before trusting any tolerance.
On a shared 4 vCPU container host, five matched pairs of the same build showed
paired differences from about -45 ms to -130 ms, so the default ±25 ms tolerance
is tighter than that host's run-to-run noise. A five-pair comparison there
correctly refuses a verdict; raise the pair count or use a quieter, dedicated
host before treating a 25 ms tolerance as resolvable. The benchmark reports this
honestly as `inconclusive` rather than as a change.

Injected-delay calibration is monotone by construction: adding a known delay to
the scripted reply shifts both first-text and final latency by that amount plus
common send start-up. Use it to confirm the measurement path on a host before
comparing releases.

## Test selection

[Test policy](../../docs/testing.md) defines when unit, integration and release
validation are required. The command accepts:

```bash
./repo test core
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

| Option | Targets | Additional constraints |
|---|---|---|
| `--only TEXT` | Leaf targets | Literal, case-sensitive case ID substring. An unmatched filter is invalid usage. |
| `--coverage` | Unit targets | No `--only` or `--list`. |
| `--list` | Unit, Linux integration, and iOS leaves | No coverage; Node static listing is unsupported. |
| `--parallel N` | Linux integration targets | 1–8 independent hash shards, default 1; incompatible with `--list`. |
| `--integration-timeout SECONDS` | Linux integration targets | Positive run deadline covering preparation through execution, defaults to 7200 seconds. |
| `--simulator UDID` | iOS targets | Exact Simulator device UDID. |

`--parallel N` preserves the selected case set and distributes cases by a stable
CRC32 of each pytest node ID. Each shard has its own runtime and result export.

`--list` discovers cases without executing test bodies. Rust discovery can
compile tests and integration discovery can build its image. Node static listing
is unsupported and fails explicitly. Go lists top-level cases; dynamic children
require execution and can be selected by a full `package::TestParent/child` ID.
Necessary imports, fixtures and parent test code can still execute for a selected
child. Unselected cases do not inflate totals.

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

Python, Go and Node collect coverage with their case results in one execution.
For Go projects, use `go tool cover -func=<profile>` or `go tool cover
-html=<profile>` with the `coverage-<project>.coverprofile` in the report directory
to inspect the measured code blocks.
Node keeps manifest coverage include/threshold flags. Rust uses a separate
cargo-llvm-cov execution after clearing stale workspace profile counts while
retaining instrumented compilation outputs,
explicitly labeled `separate_measurement`; it does not
add cases to execution totals.

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
| `runners/` | Ecosystem-specific commands, discovery and coverage. |
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
