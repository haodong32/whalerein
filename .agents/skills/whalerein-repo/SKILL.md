---
name: whalerein-repo
description: Runs and maintains WhaleRein repository engineering tasks through ./repo. Use when selecting build, format, static checks, tests, coverage, protocol generation or saved-report commands in this repository, or changing tools/repo runners and contracts. Service startup belongs to project or deployment instructions.
---

# WhaleRein repository tasks

Use `./repo` from the repository root. The product executable `whalerein` is a
separate CLI. Resolve the links below relative to this skill directory.

## Select the task

Use the quick entries below for ordinary tasks. Read only the relevant reference
section when the task needs additional flags or behavior. Reuse guidance already
read in this conversation. When the project ID is unknown, inspect
[projects.toml](../../../tools/repo/projects.toml) or run `./repo projects`.
Use `./repo COMMAND --help` to find the exact target for an operation.

| Intent | Entry point |
|---|---|
| Check the affected project | `./repo check ID` |
| Format or build the affected project | `./repo fmt ID` / `./repo build ID` |
| Final affected-project unit tests / coverage | `./repo test ID` / `./repo test ID --coverage` |
| Find cases without executing their bodies | `./repo test ID --list` |
| Run matching cases in a leaf target | `./repo test TARGET --only TEXT` |
| Run both protocol integration targets | `./repo test protocol` |
| Check or regenerate protocol bindings | `./repo check proto` / `./repo proto` |
| Render saved facts without executing tests | `./repo report RUN_DIRECTORY` |
| Diagnose missing tools | `./repo doctor TARGET` |

Iteration: choose explicit projects and matching `--only` cases without coverage.
Final validation: run affected-project unit coverage once after the final relevant
edit. Before widening or repeating a run, identify the new edit, failure or
uncovered risk. Read the startup scope and historical wall-time notice; unknown
history is not a cheap estimate. The three-minute quick-feedback target is
advisory. Pure document edits need document checks.

Pass the relevant project ID for focused format/check/build/unit operations.
An explicitly selected unsupported operation fails; do not silently substitute
an unrelated project or weaken a gate.

For validation scope, read [testing policy](../../../docs/testing.md#choose-checks-for-a-change)
when choosing between focused unit, protocol and full integration checks. Unit
is the default; integration does not require development services.

For server startup, use [Compose instructions](../../../deploy/README.md).
For a project's own process, use its README. Do not invent `repo dev`, `repo env`
or a repository-wide application-start command. Git hook installation belongs to
[commit instructions](../../../docs/commits.md).

## Execute and assess

For case selectors or combined test flags, read
[test selection](../../../tools/repo/README.md#test-selection), including Node
ID escaping and native matching limits. Node cannot statically list cases; use
known names or a saved run.

For coverage, saved results or offline rendering, read
[report contracts](../../../tools/repo/README.md#coverage-and-report-facts).

Use `--json` for machine consumption: stdout is one result envelope; diagnostics
and progress are on stderr. Report directories default to unique paths under
`.data/runs/`; an explicit `--output` must not reuse an existing report.

Inspect the exit status and saved facts together. Distinguish complete inventory,
passing execution, coverage success and partial selection. Missing measurement,
failed cleanup or interruption is not a pass. Report artifact paths and actual
verification limits. Keep credentials and business payloads out of diagnostics.

## Change the task tool

Use the [architecture map](../../../tools/repo/README.md#architecture) and inspect
the relevant runner plus its tests before editing. Keep native ecosystem logic
in its runner; the CLI owns parsing and final output, and reports consume facts.
Maintain project registration in `projects.toml`, not a second discovery catalog.

Cover the changed behavior at its actual execution seam. For scheduling changes,
check cancellation and observation retention; for selector changes, check that
unmatched test bodies do not execute.

For repo-tool implementation changes, iterate with
`./repo test repo --only TEXT`. At completion, run
`./repo check repo` and `./repo test repo --coverage` once,
plus the product/integration checks required by the changed behavior. Keep the
command reference synchronized with changes to CLI or report contracts.
