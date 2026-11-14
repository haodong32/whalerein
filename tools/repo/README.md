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
| `doctor [--for TASK]` | Probe basic tools or check/build/test/coverage/integration/proto prerequisites. |
| `fmt [--project ID]` | Write formatting changes. |
| `check [--project ID]` | Check formatting and run static analysis without modifying source. |
| `build [--project ID]` | Build applicable projects; Access writes `.data/bin/whalerein-access`. |
| `test` | Run native unit tests by default; see selection below. |
| `report FILE --output FILE` | Render saved JSON facts to offline HTML. |
| `proto [--check]` | Generate committed Go/JS bindings, or check their consistency. |
| `bench` | Run the opt-in streaming-reply product benchmark in an isolated container. |
| `bench-compare -b DIR -c DIR` | Compare saved benchmark runs under the predeclared protocol. |

`--project` accepts exact IDs and is repeatable. Defaults select all applicable
registered projects. Explicitly selecting an unsupported operation fails before
native execution. Inspect [projects.toml](projects.toml) or `./repo projects`;
paths and package names are not alternate selectors.

Rust formatting targets selected packages. Node format/check/build operations
require manifest scripts (`fmt`, `fmt:check`/`lint`, `build`). Python supports
format/check/test. `check` does not execute tests, start services or generate
bindings. Run `proto --check` explicitly for protocol consistency.

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

CI runs selected Rust, Go, Node and Python groups in parallel. Projects within
each group run sequentially; Rust coverage cleanup must not race another Rust
project in the same checkout. Each group installs only its required toolchains,
checks supported operations and saves its own coverage reports as an artifact.
The `CI gate` job requires all selected groups to pass, including on retries.

Inspect selection locally without running tests or contacting GitHub:

```bash
python3 tools/repo/src/repo/ci.py plan --base BASE_SHA --head HEAD_SHA
uv run --project tools/repo python -m repo.ci run --projects '["core", "cli"]'
```

The second command executes selected projects through `./repo check`, applicable
`./repo build` operations and `./repo test --coverage`. Rust registry/build caches
and the pinned coverage tool are cached separately. Reports remain downloadable
Artifacts with seven-day retention; no Summary or Pages publishing is configured.
The Go artifact also includes the native coverage profile when available, even
when the run fails its coverage gate.

## Integration CI

The separate Integration workflow provides daily (03:17 Asia/Shanghai), manual
and reusable release-gate execution. See [CI integration timing](../../docs/testing.md#ci-integration-timing)
for trigger, evidence, concurrency and report rules. It calls the existing
`./repo test --suite integration`; it does not require local server services.

Inspect its content fingerprint locally:

```bash
python3 tools/repo/src/repo/integration_ci.py key --revision HEAD
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
| 2 | Invalid CLI request. |
| 3 | Missing prerequisite reported by doctor or a prerequisite error. |
| 4 | Unknown project ID. |
| 130 | Interruption. |

There is no global dry-run or native argument passthrough. Use native tools for
specialized options that the repository interface does not expose.

## Product benchmark

`./repo bench` measures one client-observable workflow: a user sends a message
while the conversation is open, and the benchmark records how long the first
visible reply text and the correct final reply take. It runs in the same
isolated container image as the integration suite, through the real CLI, Access,
Host and plugin against FakeDSH. It is opt-in and never runs as part of
`./repo test`.

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

1. creates a fresh session and syncs it,
2. starts `whalerein --json tail` and waits for the watch-ready marker,
3. arms the fixture's scripted reply, then invokes `whalerein send`,
4. reads tail output and records milestones.

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
./repo test --project core
./repo test --project core --only reconnect
./repo test --project core --list
./repo test --project repo --coverage
./repo test --suite integration --group protocol
./repo test --suite integration --group chain --parallel 2
./repo test --suite integration --only test_send
./repo test --suite integration --list
./repo test --suite all
```

`--suite` is unit (default), integration or all. `--only` matches case IDs by
literal, case-sensitive substring; no matches fail. Supported combinations are:

| Option | Suites | Additional constraints |
|---|---|---|
| `--project ID` | unit | Repeatable; exact registered IDs. |
| `--group GROUP` | integration | One of protocol/storage/transport/chain/native-dsh. |
| `--integration-timeout SECONDS` | integration, all | Positive execution deadline; defaults to 7200 seconds. Image build has a separate 1800-second limit. |
| `--parallel N` | integration only | 1-8 independent containers, each with a deterministic hash shard of the selected cases. Defaults to 1 (today's single-container path). Incompatible with `--list`. |
| `--only TEXT` | unit, integration | Nonempty; unit requires one project; incompatible with coverage. |
| `--coverage` | unit, all | No `--only` or `--list`; all measures only its unit portion. |
| `--list` | unit, integration, all | No coverage; Node static listing is unsupported. |

In particular, `--suite all --only TEXT`, `--suite integration --coverage` and
`--parallel` with the unit/all suites or `--list` are invalid requests.
`--parallel N` is an integration execution mode, not a selector: the selected
case set is unchanged and shards are distributed by a stable CRC32 of each
pytest node ID. The image is built once per invocation and every shard runs that
immutable image ID in its own container, bind mount and fixed-port namespace.

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
./repo test --project host --only 'parent~1child'
./repo test --project host --only 'host::test/example.test.js::parent~01child'
```

The first selects the slash name; the second selects the literal tilde name.
The example file/names illustrate syntax; use actual IDs from your saved report.

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
cargo-llvm-cov execution after resetting stale workspace coverage artifacts,
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

Runs default to unique directories under `.data/runs/`. `--output DIR` selects a
fresh directory; one already containing a report is rejected. Saved version 3
`report.json` and `report.html` use the same in-memory facts and atomic writes.
When execution fails or is interrupted, the tool saves facts it can assemble;
JSON is saved before rendering HTML. Older report schemas are rejected.

```bash
./repo test --project repo --output .data/runs/my-check
./repo report .data/runs/my-check/report.json --output /tmp/test-report.html
```

Rendering requires neither native test tools nor network access. Diagnostic
redaction is bounded and best effort; tests must not print secrets or private
business contents.

## Architecture

[projects.toml](projects.toml) is the sole project inventory. It registers stable
IDs, paths, execution kinds, roles and necessary target configuration. Names and
scripts come from native manifests; new projects require explicit registration.

| Module | Owns |
|---|---|
| `cli.py` | Request parsing, dispatch, final output and exit status. |
| `projects.py` | Registration validation and exact ID selection. |
| `toolchain.py` | Sequential formatting/check/build coordination. |
| `testing.py` | Immutable test requests, execution and result aggregation. |
| `runners/` | Ecosystem-specific commands, discovery and coverage. |
| `process.py` | Child processes, deadlines, process-group cancellation and capture. |
| `reports/model.py` | Versioned facts, validation and atomic persistence. |
| `reports/html.py` | Offline HTML presentation. |
| `doctor.py`, `proto.py`, `commits.py` | Independent repository workflows. |

The CLI does not mutate its arguments to dispatch another command. Execution is
sequential. There is no dynamic plugin registry, second discovery catalog or
implicit development-service startup. The host integration runner owns its
container and pins it to the built image ID, independently of parallel worktrees; each run mounts a private tmpfs at `/tmp`, discarded with the container. With
`--parallel N`, the same runner builds one image, starts N containers concurrently
with separate exports, stops and verifies every launched container, then merges
their version 1 scopes into one report. An interrupted or invalid shard keeps the
merged result incomplete; duplicate case identities are rejected. The container exports one version 1 scope for host validation and
aggregation. The container enforces the selected execution deadline and sends
SIGINT to pytest; the host runner allows a short shutdown grace period, verifies
container cleanup and retains any exported partial report. A timed-out run is
marked incomplete and never passes. See [integration implementation](../../tests/integration/README.md).

## Maintaining the tool

```bash
./repo fmt --project repo
./repo check --project repo
./repo test --project repo --coverage
```

Read [commit rules](../../docs/commits.md) before committing. Git hooks invoke
`repo.commits` internally; their installation instructions live in that guide.
