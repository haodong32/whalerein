# Testing

## Validation scope

| Scope | Covers | Environment |
|---|---|---|
| Unit | Module behavior, validation, state transitions and errors. | Native toolchains and isolated fixtures. |
| Integration | Protocol compatibility, PostgreSQL, broker transport and CLI-to-FakeDSH behavior. | Independent Linux Docker container. |
| Release | Real DSH, platform SecretStore, production TLS and deployment. | Actual target environments. |

FakeDSH does not verify real DSH APIs, credentials, approval atomicity or reload
behavior. An integration or tooling-harness pass is not release validation.

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
