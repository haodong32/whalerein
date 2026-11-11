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
Select exact IDs with `--project`; directory paths are not selectors.

| Intent | Entry point |
|---|---|
| Write formatting / check format and static issues | `./repo fmt` / `./repo check` |
| Compile registered projects | `./repo build` |
| Native tests / coverage | `./repo test` / `./repo test --coverage` |
| Find cases without executing their bodies | `./repo test --project ID --list` |
| Run a matching unit case | `./repo test --project ID --only TEXT` |
| Check or regenerate protocol bindings | `./repo proto --check` / `./repo proto` |
| Render saved facts without executing tests | `./repo report FILE --output FILE` |
| Diagnose missing tools | `./repo doctor --for TASK` |

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

For implementation changes, run `./repo check --project repo` and
`./repo test --project repo --coverage`, plus the product/integration checks
required by the changed behavior. Keep the command reference synchronized with
changes to CLI or report contracts.
