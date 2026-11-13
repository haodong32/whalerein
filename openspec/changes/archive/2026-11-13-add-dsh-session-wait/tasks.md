# Native session wait tasks

Read [Decision Review](design.md#decision-review), especially D-REL-01, D-WAIT-01, D-WAIT-02 and D-LIMITS-01. This is a plan only; implementation and runtime checks have not run.

## Implementation and verification

- [x] 1.1 Verify native cold/live/maintenance and input stopping semantics in an isolated fixture, including claimed approval/question handles. Record versions, exact timeout limits and transport lifetime evidence locally before enabling waiting. (AC-01–AC-04, AC-09, AC-10).
- [x] 1.2 Define additive source-neutral wait/result/cancellation messages and source methods. Bind wait identity and absolute expiry; keep native cursors/output private. Generate bindings with ./repo proto and verify ./repo proto --check. (AC-01, AC-03, AC-05–AC-10).
- [x] 2.1 Implement Host read-only query dispatch, cancellation and cleanup on request expiry, revocation and close. Add tests proving one cancellation cannot affect another observation and abandoned clients cannot accumulate waits. (AC-05–AC-08).
- [x] 2.2 Implement adapter-native wait, outer cancellation covering output assembly, normalization and claimed-input reconciliation. Test cold queue=null idle, native maintenance/whenIdle, no first state, output-read failure, and no content leakage. (AC-01–AC-05, AC-07, AC-09, AC-10).
- [x] 2.3 Add Core overall monotonic deadline and scoped read-only slice/recovery handling. Derive per-query budgets from actual request/poll expiry with response allowance. Allow independent waits and tail, preserving stale-result rejection and cancellation isolation. (AC-02–AC-08).
- [x] 3.1 Add CLI parsing and human/JSON formatting. Test omitted timeout=30000 ms, positive integer-ms bounds up to 2147483647, exact seconds conversion, default exit 0 versus --fail-on-timeout exit 7, and transport errors distinct from timeout observations. (AC-03, AC-05–AC-10).
- [x] 3.2 Update only verified public API/CLI/adapter docs. Explicitly explain native idle, claimed-input adaptation, state-only output, timeout policy, cancellation and lack of a task-success guarantee. (AC-01–AC-12).
- [x] 4.1 Run applicable ./repo check --project core --project cli, affected package unit tests with --coverage (at least 90%), ./repo proto --check, protocol integration, then ./repo test --suite integration. Include delayed forwarding beyond a slice and lifecycle/recovery faults. Host/DSH have no check/build operation. (AC-01–AC-12).
- [x] 4.2 Run dedicated real DSH probes for cold/live/maintenance, pending input and cancellation. Compare results with native wait and verify bounded cleanup; FakeDSH and code inspection are not native acceptance. (AC-01–AC-12).
- [x] 4.3 Reconcile every acceptance row against fresh evidence; preserve failures/blockers/unexecuted results. Assess architectural deviations and document the maintained-docs decision. (AC-01–AC-12).
- [x] 4.4 Review independent capability rollout and scoped cleanup on disable/rollback. Report unverified operations as disabled, not passed; run OpenSpec strict validation. (AC-01–AC-12).


## Acceptance case matrix

Follow the [acceptance evidence contract](../../README.md#acceptance-evidence-contract). Case-family IDs below are planned: expand each semicolon-separated variant into a stable subcase ID, bind it to an exact automated test selector and (where required) a native procedure before acceptance, and attach every subresult to its parent AC row. Existing feature tasks own implementation; task 6.1 owns full matrix verification. Slashes name distinct required alternatives, not permission to test only one. Tests belong in the affected paths already identified above.

| AC | Planned case family | Mandatory variants | Required verification | Owner |
|---|---|---|---|---|
| AC-01 | `session-wait/AC-01.*` | live/cold-state; claim/delegate-input; settled/expired-handle-removal | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-02 | `session-wait/AC-02.*` | cold-null-queue-idle; live-maintenance-running-false; native-whenIdle; claimed-input-priority | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-03 | `session-wait/AC-03.*` | default-30000ms; minimum/maximum/invalid-ms; exact-seconds-conversion; overall-timeout-exit-0/7; no-first-state-error | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-04 | `session-wait/AC-04.*` | silent-native-input; claimed-request-open/settle/expire; native-refresh | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-05 | `session-wait/AC-05.*` | explicit-cancel; channel-close; request-expiry; native-output-abort; no-Agent-cancel | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-06 | `session-wait/AC-06.*` | wait-plus-wait; wait-plus-tail; independent-cancel; no-extra-MQTT-consumer | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-07 | `session-wait/AC-07.*` | revoked; foreign-wait/session/channel-result; source-error-distinct-from-revocation | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-08 | `session-wait/AC-08.*` | reconnect-budget-remaining; reconnect-at-deadline; old-channel-result; no-deadline-extension | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-09 | `session-wait/AC-09.*` | missing-wait-capability; disabled-native-wait; offline-cached-idle | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-10 | `session-wait/AC-10.*` | output-discarded; output-read-failure; encoded-budget-boundaries; synthetic-content-redaction | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-11 | `session-wait/AC-11.*` | one-slice-timeout; multiple-slice-timeouts; later-idle; later-input; overall-deadline-after-slices | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-12 | `session-wait/AC-12.*` | forwarding-delay; result-encoding-delay; polling-expiry; output-read-overrun; late-result; scoped-cleanup | Automated fixtures/integration | 6.1 + existing AC-linked tasks |

- [x] 6.1 Implement the planned matrix subcases in the existing protocol/adapter/Host/Core/CLI test paths as applicable; record exact selectors, commands, assertions and native procedures. Run focused ./repo checks and full integration as required above, plus the mapped native procedures. Reconcile every subcase before setting a parent PASS; unresolved bounds or unavailable required native procedures remain BLOCKED. Assess capability gates and report partial delivery separately.

## Acceptance Results

| AC ID | Status | Evidence | Limitations |
|---|---|---|---|
| AC-01 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-02 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-03 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-04 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-05 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-06 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-07 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-08 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-09 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-10 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-11 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-12 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
