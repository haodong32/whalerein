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

## Test selection

[Test policy](../../docs/testing.md) defines when unit, integration and release
validation are required. The command accepts:

```bash
./repo test --project core
./repo test --project core --only reconnect
./repo test --project core --list
./repo test --project repo --coverage
./repo test --suite integration --group protocol
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
| `--only TEXT` | unit, integration | Nonempty; unit requires one project; incompatible with coverage. |
| `--coverage` | unit, all | No `--only` or `--list`; all measures only its unit portion. |
| `--list` | unit, integration, all | No coverage; Node static listing is unsupported. |

In particular, `--suite all --only TEXT` and
`--suite integration --coverage` are invalid requests.

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
container and pins it to the built image ID, independently of parallel worktrees; the container exports one version 1 scope for host validation and
aggregation. See [integration implementation](../../tests/integration/README.md).

## Maintaining the tool

```bash
./repo fmt --project repo
./repo check --project repo
./repo test --project repo --coverage
```

Read [commit rules](../../docs/commits.md) before committing. Git hooks invoke
`repo.commits` internally; their installation instructions live in that guide.
