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

See the [repo command reference](../tools/repo/README.md#test-selection) for exact
project IDs, case discovery, selector syntax and unsupported combinations.
The [integration guide](../tests/integration/README.md) describes container and
fixture ownership. Development services are not prerequisites for integration.

## CI integration timing

Ordinary PR updates and main pushes run affected-project fast checks. Full
integration is scheduled separately by `.github/workflows/integration.yml` at
03:17 Asia/Shanghai each day. GitHub scheduling is best-effort; manual dispatch
can validate a repair or high-risk change without waiting for the next schedule.

The workflow freezes one main commit and fingerprints all tracked inputs except
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
