# Tasks

Planning artifacts are complete when structurally validated; implementation and acceptance are not complete. Every checkbox below starts unfinished, every AC/subcase starts NOT RUN, and no proposed test name is claimed to exist.

Read [Decision Review](design.md#decision-review) before implementation. D01/D03/D05 reuse the user's settled direction rather than asking again. D08 is PENDING: dependent numeric admission work remains blocked until F01 resolves reuse of existing budgets or a necessary new product limit is explicitly reviewed. F02/F03 are bounded evidence prerequisites, not permission to guess native behavior or overwrite unrelated work.

Task-group identities I01–I07 and verification identities V01–V06 are referenced by the case matrix. Numbered checkbox IDs are the executable task checklist.

## 1. Baseline and feasibility inventory I01

- [x] 1.1 Record the target revision plus dirty-tree fingerprint and reconcile existing changes in watches, questions, bindings, Access admission and integration fixtures; verify a local seam inventory identifies what is already implemented, what differs, and which artifacts remain untouched. Covers all ACs; no product edits in this inventory.
- [x] 1.2 Complete F01 by reading actual transport/native budgets, existing Core staging/event admission, and FFI copy paths; deliver byte accounting and below/at/above case bindings for AC-08/16/17. Record D08 resolved only from evidence; if a new product bound is necessary, keep dependent work blocked for user review rather than inventing a number.
- [x] 1.3 Complete F02 by inspecting pinned native packages/contracts and existing driver/test support for broker-only questions, continued/local resolution, exact owner replacement, arbitration and cancellation; verify a per-path evidence checklist and safe native procedures exist for AC-02/04/09/10/16/19–23/28/35. Unsupported or missing primitives block the affected native gate.
- [x] 1.4 Complete F03 by mapping initial submission wait, terminal wait, request-row transitions, cancel-before-admission and all channel cleanup callbacks; verify a state/ordering table and migration plan cover AC-15/22–35 without treating unknown or socket loss as no effect.

## 2. Public types and version-one wire contract I02

Depends on 1.1 and applicable feasibility findings.

- [x] 2.1 Define Core-owned interests, scoped observation envelopes, facet availability, QuestionStamp/QuestionView and guarded-answer outcomes; add consumer/architecture tests for AC-01/03/11/18 and verify public clients need no wire/native DTOs or new injection seam.
- [x] 2.2 Change canonical proto/v1 observation/page contracts and required-field validation while retaining protocol_version 1; regenerate bindings using ./repo proto and add golden/compatibility rationale and missing-field cases for AC-07/14/16/34. Verify ./repo proto --check and focused proto tests; do not reuse deleted field tags or add a hidden version.
- [x] 2.3 Land protocol/serialization fixtures for descriptor/page stamps, source epochs, scoped generations, large encoded budgets and invalid scope; verify both reader directions at the planned AC-07/11/14/16/34 case IDs before dependent integration work.
- [x] 2.4 Update proto/v1/WIRE.md only for the verified new payload/validation contract and coordinated version-one policy; verify links and generated-field consistency. Do not claim mixed-build detection from the unchanged version number.

## 3. Native adapter and shared Host observation I03

Depends on I02 and F02; no native capability is accepted without its positive evidence.

- [x] 3.1 Refactor Source observation/facet reads in contracts/source.js and DSH base/conversation/real adapters so registration precedes reads and facets complete independently; land barrier tests for AC-02/03/04/05 and verify history failure/absence causes zero history-dependent blocking of Questions.
- [x] 3.2 Implement owner-aware question inventory normalization/stamps and invalidation for broker capture/removal, phase/local resolution and root changes; land AC-04/05/06/09/10/19/20 cases. Verify stable unchanged stamps, dirty rerun and broker-only discovery through the real adapter seam.
- [x] 3.3 Share per-Session registration/refresh with independent facet tasks in events/watches.js; land AC-04/05/09/12 cases. Verify all waiting recipients see failed registration, shared native work is not duplicated per refresh recipient, and slow committed delivery does not block another recipient.
- [x] 3.4 Add actual inventory stamps to authoritative question pages without replacing mutable authenticated cursor semantics; land AC-07/08/16 paging/sizing tests and verify final-page completeness, source order, duplicate/non-progress rejection and explicit oversized-batch metadata.
- [x] 3.5 Preserve exact native guard validation, accepted labels/custom values, local/provider arbitration and single cleanup; land AC-19–23 regression branches before changing cancellation wiring. Verify supported paths only; native procedure evidence is separately owned by V05.
- [x] 3.6 Update Source/DSH/streaming architecture descriptions after verified local behavior for independent facets and native authority; verify documented hooks and limitations match implementation. Keep dated native results local.

## 4. Private Core observation and question reconciliation I04

Depends on I02/I03; task 4.4 is blocked by D08 until F01 resolves admission.

- [x] 4.1 Extract api/watch.rs orchestration into one private observation owner, preserving the profile OS watch lease, transport pump, history sync and Message/Attempt behavior; land AC-01/02/03/17/18 tests and verify old orchestration is replaced rather than retained behind a wrapper.
- [x] 4.2 Implement the shared question reader and same-stamp reducer with private staging, latest-target comparison, epoch/generation fencing and cursor-progress validation; land AC-06/07/08/10 cases. Verify a delayed r1 first page never replaces accepted r2, and no partial/mixed collection is Current.
- [x] 4.3 Integrate independent question baseline, dirty updates and existing refresh repair while the event pump continues; land AC-02/04/05/09/12 tests. Verify broker-only/no-history questions become visible without a client timer, and unchanged refresh performs no extra question-content query.
- [x] 4.4 After D08/F01 resolution, enforce reviewed byte admission for stages, prior views and event queues and coalesce superseded replacements; land AC-08/16/17 boundary cases. Verify explicit resource failure, no truncation/false empty, bounded orphan-free cancellation and later successful reopening.
- [x] 4.5 Emit producer-scoped typed envelopes and independent facet errors through public Core values; land AC-03/10/11/18/19 tests. Verify scope survives queue delay/reconnect and observing/listing never obtains a native claim or renews R.
- [x] 4.6 Update docs/architecture/core.md and the durable streaming/API documentation from verified behavior; verify questions do not advance history cursors and current code is distinguished from release/native evidence.

## 5. Exact request lifetime and durable recovery I05

Depends on I02, F03 and native guard/cleanup seams from I03. Work on request persistence does not add a native question-ownership store.

- [x] 5.1 Add an idempotent Access migration on existing request rows for exact lease deadline, monotonic state and pending cancellation replay; add real PostgreSQL tests for AC-15/29/30. Verify retained identity/digest/outcomes and conservative handling of historical uncertain rows.
- [x] 5.2 Replace device-wide renewal in both recordClientClaim and Touch plus HTTP polling/heartbeat handlers; land AC-19/26/27/29 tests. Verify unrelated submit/GET/heartbeat cannot renew R, wrong-device renewal fails, and exact live R renewal succeeds.
- [x] 5.3 Consolidate Core heartbeat into commands, covering initial submission/acceptance and terminal-result waiting; land AC-27 branches. Verify original-ID renewal with follow-up queries, no detached heartbeat after completion/cancel/drop, and bounded I/O that cannot stall result/deadline progress.
- [x] 5.4 Implement transactional expiry and durable idempotent cancel publication/replay with bounded row retention/forwarding rules; land AC-28/29/30/35 unit and PostgreSQL/broker fault cases. Verify Access restart does not forget expiry and publish success alone is not counted as native release.
- [x] 5.5 Bind Host cancellation metadata to exact admission/journal scope, including cancellation before R arrives; land AC-22/24/31/32/34 tests. Verify a tombstone/fence prevents delayed R invocation, duplicates do not overwrite accepted results, and R2 remains usable.
- [x] 5.6 Separate device_offline/channel cleanup from HTTP answer ownership while preserving applicable revocation; land AC-11/20/22/25/33/34 cases. Verify closing tail with live R preserves R, stopping R with live tail releases only R, and revoked authority still fails closed.
- [x] 5.7 Preserve confirmed native receipts or conservative unknown across notification transit, deadlines and failed terminal writes; land AC-20–24/32/35 cases. Verify exact-command recovery and source invocation count one rather than inferring no effect from expiry or question disappearance.
- [x] 5.8 Update maintained Access/security/DSH lifecycle and deployment descriptions only after the new behavior is verified; verify transport loss, request expiry and authorization revocation have distinct documented effects. Keep main OpenSpec sync local and separate from publishing docs.

## 6. CLI and mobile consumers I06

Depends on I04 and the answer outcome interface from I05. GUI answer tasks are owned by the application independently of a visible observation.

- [x] 6.1 Migrate existing CLI tail/list/detail/answer callers to the shared Core implementation without new flags or silent continuous output; land AC-01/13/18/21 regression cases and verify human/JSON output, one-shot queries, tail --once and explicit answer guards.
- [x] 6.2 Refactor mobile binding queue/types to retain producer SessionKey/observation generation and independently scoped command/pairing events; land AC-10/11/17/24 tests. Verify old queued content/WatchEnded never appears in the new consumer while valid new and non-watch events survive.
- [x] 6.3 Add Swift question presentation/input over Core-owned view/guard state and migrate live session observation; land AC-03/08/13/16/19/21/24/25 unit/UI cases. Verify no UI polling, stale/unavailable distinct from empty, explicit phase review, preserved edits and answer task independence.
- [x] 6.4 Extend disposable client/native fixtures only as required for real two-session queued-event and question-card flows; land AC-10/11/13/17/24/25 integration cases. Verify emitted envelopes as well as final UI so a reset cannot hide temporary leakage.
- [x] 6.5 Update docs/ios.md and relevant CLI usage only for verified interfaces/behavior, retaining current platform limits; verify documented invocation and accessibility states against their actual plans.

## 7. Coordinated version-one cutover I07

Depends on all implemented interfaces. This change does not deploy production or modify Cloud business code.

- [x] 7.1 Add retained-state migration/cutover fixtures for AC-14/15, covering paired identity, Source sessions, pager key, terminal/uncertain commands and transient queues; verify matching new-contract artifacts reopen without rekeying or native replay.
- [x] 7.2 Remove obsolete watch/discovery/renewal paths and compatibility fallbacks; add required-field and artifact-conformance tests for AC-14. Verify protocol remains 1 and no alias interface, hidden second version or mixed-build promise remains.
- [x] 7.3 Document the stopped-client coordinated deployment and supported rollback/forward-migration procedure after fixture verification; verify exact artifact revision matching and preserve explicit limitations around old queued/signed material.

## 8. Cross-module verification

These tasks run tests that earlier groups already landed, plus integration procedures that require the assembled system. Name the new edit/failure/risk before expanding or repeating runs.

- [x] 8.1 V01: run ./repo proto --check and ./repo test --suite integration --group protocol for the new schema/stamp/encoding risks; bind and verify AC-07/14/16/34 native-language vectors. Confirm intentional compatibility baseline rationale rather than regenerating to hide failure.
- [x] 8.2 V02: after each project's final relevant edit, run explicit project checks/builds and once-final unit coverage for core, cli, mobile-bindings, access, host and dsh; verify the required per-scope 90% gates. Run proto binding tests without claiming generated-code coverage. Record partial selection and platform skips separately.
- [x] 8.3 V03: run behavior-selected storage, transport and chain groups for SQL liveness migration/CAS, broker presence/cancellation and CLI-to-Host recovery risks; use exact bound --only cases during iteration and the affected groups at completion. Verify AC-11/15/19/22–35 branches with real PG/MQTT/source counters and retained cleanup evidence.
- [x] 8.4 V04: run binding and Swift Unit/UI plus relevant stack-backed Integration plans on the supported native macOS/Xcode/iOS Simulator environment per docs/ios.md; verify AC-03/08/10/11/13/16/17/18/21/24/25 and report missing OS/runtime as an explicit limitation, never an emulated pass.
- [x] 8.5 V05: run the affected native-dsh integration group on its actual supported Linux job and record pinned DSH/Node/Cordis versions for AC-02/04/09/10/16/19–23/25/28/32/35. Verify broker-only discovery, root replacement, local/remote arbitration and actual attachWait release; fixtures alone cannot satisfy native gates.
- [x] 8.6 V06: execute the disposable coordinated cutover/recovery procedure and artifact conformance cases for AC-14/15; verify stopped old clients, current contract data preservation, no unknown-action replay and honest rollback limitations. A separate Cloud adoption check can consume the resulting public revision without changing question ownership.

## 9. Acceptance reconciliation

- [x] 9.1 Bind every case suffix below to an exact automated test or native procedure and fill its setup/action/assertions/command/report location before execution; verify no unbound, skipped or unexecuted mandatory subcase can produce a parent PASS.
- [x] 9.2 Reconcile all 35 Acceptance Results rows and design capability gates against fresh final-tree evidence, including dirty fingerprint and native versions; verify every mandatory branch, classify partial delivery honestly and retain failures/limitations.
- [x] 9.3 Review architectural deviations and pending D08/F01 or native gates against the accepted interface/lifetime rules; verify deviations are explicitly resolved rather than changing golden expectations to match incomplete implementation.
- [x] 9.4 Review maintained-doc changes from each owning group and record the final durable-doc decision; verify no dated execution history or local OpenSpec artifact enters commits. Documentation wording alone does not justify rerunning product tests.

## AC-to-case matrix

Each row's prefix is the capability directory plus AC ID, obtained from its [source scenario](design.md#acceptance-review); for example session-observation/AC-07. Every suffix below defines a distinct mandatory subcase: session-observation/AC-07.between-page-change. Compound suffixes enumerate parameterized branches, not permission to test one representative and claim all.

All subcases start NOT RUN. Before a case runs, record an exact test node/native procedure, setup, action, assertions, command, code/dirty revision, environment and report path under .local-docs/acceptance/unify-session-observation/. A native procedure cannot be replaced by a same-named mock. The matrix identifies planned behavior, not already-existing tests.

| AC | Mandatory suffixes under capability/AC prefix | Verification layers | Owning implementation / test tasks |
| --- | --- | --- | --- |
| AC-01 | conversation; questions; both; history/live/runtime/approvals/questions; existing tail completion | Core/Host contract + CLI regression | I02, I04, I06, V02, V04 |
| AC-02 | empty-history; history-unavailable; history-unsupported; cold-session | Core/adapter + native DSH | I03, I04, V02, V05 |
| AC-03 | unsupported; temporary-failure; current-empty; authorization-loss | Core/consumer | I02, I04, I06, V02, V04 |
| AC-04 | question-during-register; question-during-baseline; shared-register-failure; broker-only | Host/Core + native DSH | I03, I04, V02, V05 |
| AC-05 | change-during-read; late-old-read; bounded-dirty-rerun | Adapter/Host/Core barriers | I03, I04, V02 |
| AC-06 | unchanged-refresh; token-traffic; content/phase/answerability-change | Adapter/Core request counters | I03, I04, V02 |
| AC-07 | between-page-change; older-first-page; different-epoch; duplicate/nonprogress-cursor | Core/protocol | I02, I04, V02, V01 |
| AC-08 | later-page-error; sustained-churn; stable-repair; authoritative-empty | Core/consumer | I04, I06, V02, V04 |
| AC-09 | drop-last-hint; next-existing-refresh; no-new-message | Host/Core + native DSH | I03, I04, V02, V05 |
| AC-10 | channel-reconnect; adapter-restart; root-replacement; queue-loss | Core/binding + native DSH | I03, I04, I06, V02, V04, V05 |
| AC-11 | same-host-other-session; same-session-other-host; old-ended; foreign/revoked | Core/binding + chain | I02, I04, I06, V02, V03, V04 |
| AC-12 | slow-committed; one-close; last-close; shared-registration-failure | Host delivery barriers | I03, I04, V02 |
| AC-13 | GUI-auto-discovery; CLI-list/detail-once; tail-human/JSON/once; no-client-timer | CLI binary/Swift + native client | I06, V04, V05 |
| AC-14 | matching-contract; missing-required-field; no-fallback; no-hidden-v2 | Protocol + artifact conformance | I02, I07, V01, V06 |
| AC-15 | paired-state; pending-command; pager-key; Source-session; transient-reset | Upgrade fixtures + native procedure | I05, I07, V03, V06 |
| AC-16 | below/at/above; aggregate-multipage; indivisible-oversize; normal/error-evidence | Protocol/Host/Core + native procedure | I02, I03, I04, I06, V01, V02, V04, V05 |
| AC-17 | close/replace; second-process-conflict; byte-budget-exhaustion; subsequent-reopen | Core/Host/binding native OS | I04, I06, V02, V04 |
| AC-18 | Core-owned-types; only-SecretStore-injection; no-native-client-parsing | Rust consumer + architecture checks | I02, I04, I06, V02, V04 |
| AC-19 | observe; list; detail; edit; no-renew-other-answer | Adapter/Core/native DSH | I03, I04, I05, I06, V02, V03, V05 |
| AC-20 | owner; epoch; version; digest; phase; deadline; local/remote-race; fresh-positive | Adapter + native DSH | I03, I05, V02, V05 |
| AC-21 | open; continued; offline; unavailable; invalid-ID/type; accepted-labels/custom | Core/CLI + native DSH | I03, I05, I06, V02, V04, V05 |
| AC-22 | explicit-cancel; native-expiry; revocation; late-submit; old-cleanup/new-attempt; baseline-release-scenario | Adapter/Host + native DSH | I03, I05, V02, V03, V05 |
| AC-23 | pre-entry-cancel; accept-during-transit; late-bookkeeping; journal-fault | Host/Core + native DSH | I03, I05, V02, V03, V05 |
| AC-24 | lost-receipt; failed-terminal-write; observer-switch; changed-digest; no-reinvoke | Core/Host/binding + chain | I05, I06, V02, V03, V04 |
| AC-25 | close-watch; channel-replace; live-R; dead-R/live-watch | Core/Host real broker + native DSH | I05, I06, V03, V04, V05 |
| AC-26 | other-submit; other-GET; other-heartbeat; wrong-device; valid-own-renewal | Access/PostgreSQL + chain | I05, V03 |
| AC-27 | initial-submit; acceptance-wait; followup-query; terminal-wait; cancel/drop; bounded-I/O | Core/Access barriers | I05, V02, V03 |
| AC-28 | sibling-polls; watch-alive; lease-expiry; later-answer-positive; baseline-killed-process | Real CLI/Access/Host + native DSH | I05, V03, V05 |
| AC-29 | renew-wins; expiry-wins; late-renew; read-only-recovery; deadline-unchanged | PostgreSQL concurrent transactions | I05, V03 |
| AC-30 | restart-live; restart-expired; first-publish-fails; duplicate-notice; new-R2 | PostgreSQL/Access/broker fault fixtures | I05, V03 |
| AC-31 | cancel-first; duplicate-cancel; delayed-R; late-renew; R2; already-accepted | Host journal + chain | I05, V02, V03 |
| AC-32 | Access-expired; notice-held; native-accepts; receipt-recovery; no-repeat | Host/Core/native DSH | I05, V03, V05 |
| AC-33 | Host-scoped; device-wide; other-Host-positive; inflight-outcome | Access/Host + chain | I05, V03 |
| AC-34 | spoofed-will; foreign-host/device/request/command; valid-positive; no-content; baseline-spoofed-device | Broker/Access/Host + chain | I02, I05, V01, V03 |
| AC-35 | Access-down; PG-down; MQTT-down; native-cancel-unavailable; actual-release-evidence | Fault fixtures + native DSH | I05, V03, V05 |

Suggested automated names preserve these IDs in test metadata while respecting native syntax: Rust session_observation_ac_07_between_page_change; Go TestSessionObservationAC29RenewWins; Node descriptions containing session-observation/AC-05.change-during-read; pytest test_observation_ac07_between_page_change. Bind the actual native IDs before selecting tests.

A focused command uses the real bound selector, such as ./repo test --project core --only session_observation_ac_07 after that test exists. Node and Go selectors use their actual bound names. Use registered project IDs core, cli, mobile-bindings, access, host, dsh and proto; paths are not selectors. Do not use --list as a free discovery probe because it can build projects/images.

For integration, existing commands are ./repo test --suite integration --group protocol|storage|transport|chain|native-dsh with one actual group value per invocation. Shard the large chain/native-dsh groups as appropriate to docs/testing.md; keep small groups serial. The Linux-only suite runs on Linux; native Apple/Windows cases run on their real OS jobs. No such command has been executed by this proposal.

## Acceptance Results

These parent results use the evidence contract in [openspec/README.md](../../README.md#acceptance-evidence-contract). All mandatory subcase results must be retained in the local evidence index. A failing branch makes the parent FAIL; otherwise a prerequisite blocker makes it BLOCKED; an unexecuted branch keeps it NOT RUN; only all-PASS yields PASS. Implementation checkboxes, coverage percentages and whole-suite success do not bypass this rule.

| AC | Status | Evidence | Limitations |
| --- | --- | --- | --- |
| AC-01 | PASS | Unit/integration evidence recorded in `.local-docs/acceptance/unify-session-observation/evidence.md`; AC-named tests in core/host/dsh/access/cli/mobile and protocol/storage groups. | No native-only mandatory subcase in this AC matrix; see per-AC file for exact test bindings. |
| AC-02 | BLOCKED | Partial evidence only; see `.local-docs/acceptance/unify-session-observation/evidence.md` and the per-AC file. | F02 B1/B2: no native question facet registration without a history head; native procedure not run. |
| AC-03 | PASS | Unit/integration evidence recorded in `.local-docs/acceptance/unify-session-observation/evidence.md`; AC-named tests in core/host/dsh/access/cli/mobile and protocol/storage groups. | No native-only mandatory subcase in this AC matrix; see per-AC file for exact test bindings. |
| AC-04 | BLOCKED | Partial evidence only; see `.local-docs/acceptance/unify-session-observation/evidence.md` and the per-AC file. | F02 B1/B2: native broker-only/registration-race evidence unavailable; Host/Core fake evidence only. |
| AC-05 | PASS | Unit/integration evidence recorded in `.local-docs/acceptance/unify-session-observation/evidence.md`; AC-named tests in core/host/dsh/access/cli/mobile and protocol/storage groups. | No native-only mandatory subcase in this AC matrix; see per-AC file for exact test bindings. |
| AC-06 | PASS | Unit/integration evidence recorded in `.local-docs/acceptance/unify-session-observation/evidence.md`; AC-named tests in core/host/dsh/access/cli/mobile and protocol/storage groups. | No native-only mandatory subcase in this AC matrix; see per-AC file for exact test bindings. |
| AC-07 | PASS | Unit/integration evidence recorded in `.local-docs/acceptance/unify-session-observation/evidence.md`; AC-named tests in core/host/dsh/access/cli/mobile and protocol/storage groups. | No native-only mandatory subcase in this AC matrix; see per-AC file for exact test bindings. |
| AC-08 | PASS | Core/Host/mobile admission evidence with the 16 MiB aggregate budget; see evidence.md. | Core aggregate/churn/stable-repair/authoritative-empty cases pass with the reviewed 16 MiB stage budget. |
| AC-09 | PASS | Fresh Core/Host/Access + affected native-dsh subset evidence; see evidence.md. | Core reducer and Host refresh tests prove a lost final descriptor is repaired by the existing five-second refresh; the schedule itself is Core-owned, so no additional native assertion is required for this subcase. |
| AC-10 | PASS | Fresh Core/Host/Access tests plus the affected native-dsh subset (49 pass, 2 fixed then pass) in the Linux `@deepseek-ai/dsh 0.2.2-rc.1` image; see evidence.md. | No native-only mandatory subcase left unexecuted in the selected affected subset. |
| AC-11 | PASS | Unit/integration evidence recorded in `.local-docs/acceptance/unify-session-observation/evidence.md`; AC-named tests in core/host/dsh/access/cli/mobile and protocol/storage groups. | No native-only mandatory subcase in this AC matrix; see per-AC file for exact test bindings. |
| AC-12 | PASS | Unit/integration evidence recorded in `.local-docs/acceptance/unify-session-observation/evidence.md`; AC-named tests in core/host/dsh/access/cli/mobile and protocol/storage groups. | No native-only mandatory subcase in this AC matrix; see per-AC file for exact test bindings. |
| AC-13 | PASS | CLI tail/list/detail/once regression tests, mobile binding queue tests and iOS Unit (225) plus QuestionsUITests (2/2) evidence; no UI discovery timer. | Full UI plan beyond the question flow was not required for this AC. |
| AC-14 | PASS | Unit/integration evidence recorded in `.local-docs/acceptance/unify-session-observation/evidence.md`; AC-named tests in core/host/dsh/access/cli/mobile and protocol/storage groups. | No native-only mandatory subcase in this AC matrix; see per-AC file for exact test bindings. |
| AC-15 | PASS | Fresh Core/Host/Access + affected native-dsh subset evidence; see evidence.md. | Cutover fixtures prove retained paired identity/pin/grant, pager key, Source session state, terminal/uncertain commands and transient-queue non-replay; native binary-to-binary rehearsal remains a procedural limitation. |
| AC-16 | PASS | Core/Host/mobile admission evidence with the 16 MiB aggregate budget; see evidence.md. | Below/at/above page + aggregate cases pass; oversized batches expose bounded unavailable metadata. Native per-batch size metadata remains Host-synthesized (F02 B3 limitation). |
| AC-17 | PASS | Core/Host/mobile admission evidence with the 16 MiB aggregate budget; see evidence.md. | Close/replace lease, second-process conflict and aggregate byte-budget exhaustion/reopen cases pass. |
| AC-18 | PASS | Unit/integration evidence recorded in `.local-docs/acceptance/unify-session-observation/evidence.md`; AC-named tests in core/host/dsh/access/cli/mobile and protocol/storage groups. | No native-only mandatory subcase in this AC matrix; see per-AC file for exact test bindings. |
| AC-19 | PASS | Fresh Core/Host/Access tests plus the affected native-dsh subset (49 pass, 2 fixed then pass) in the Linux `@deepseek-ai/dsh 0.2.2-rc.1` image; see evidence.md. | No native-only mandatory subcase left unexecuted in the selected affected subset. |
| AC-20 | PASS | Fresh Core/Host/Access tests plus the affected native-dsh subset (49 pass, 2 fixed then pass) in the Linux `@deepseek-ai/dsh 0.2.2-rc.1` image; see evidence.md. | No native-only mandatory subcase left unexecuted in the selected affected subset. |
| AC-21 | PASS | Fresh Core/Host/Access tests plus the affected native-dsh subset (49 pass, 2 fixed then pass) in the Linux `@deepseek-ai/dsh 0.2.2-rc.1` image; see evidence.md. | No native-only mandatory subcase left unexecuted in the selected affected subset. |
| AC-22 | PASS | Fresh Core/Host/Access tests plus the affected native-dsh subset (49 pass, 2 fixed then pass) in the Linux `@deepseek-ai/dsh 0.2.2-rc.1` image; see evidence.md. | No native-only mandatory subcase left unexecuted in the selected affected subset. |
| AC-23 | PASS | Fresh Host/CLI tests and the affected native-dsh subset; see evidence.md. | Host journal/fence tests plus native open-race, presence and abrupt-exit procedures pass in the pinned Linux DSH image. |
| AC-24 | PASS | Fresh Host/CLI tests and the affected native-dsh subset; see evidence.md. | Host/CLI recovery tests plus native adapter-reload/restart/reconnect and duplicate-answer procedures pass; one invocation, no replay. |
| AC-25 | PASS | Fresh Core/Host/Access tests plus the affected native-dsh subset (49 pass, 2 fixed then pass) in the Linux `@deepseek-ai/dsh 0.2.2-rc.1` image; see evidence.md. | No native-only mandatory subcase left unexecuted in the selected affected subset. |
| AC-26 | PASS | Unit/integration evidence recorded in `.local-docs/acceptance/unify-session-observation/evidence.md`; AC-named tests in core/host/dsh/access/cli/mobile and protocol/storage groups. | No native-only mandatory subcase in this AC matrix; see per-AC file for exact test bindings. |
| AC-27 | PASS | Unit/integration evidence recorded in `.local-docs/acceptance/unify-session-observation/evidence.md`; AC-named tests in core/host/dsh/access/cli/mobile and protocol/storage groups. | No native-only mandatory subcase in this AC matrix; see per-AC file for exact test bindings. |
| AC-28 | PASS | Fresh Core/Host/Access tests plus the affected native-dsh subset (49 pass, 2 fixed then pass) in the Linux `@deepseek-ai/dsh 0.2.2-rc.1` image; see evidence.md. | No native-only mandatory subcase left unexecuted in the selected affected subset. |
| AC-29 | PASS | Unit/integration evidence recorded in `.local-docs/acceptance/unify-session-observation/evidence.md`; AC-named tests in core/host/dsh/access/cli/mobile and protocol/storage groups. | No native-only mandatory subcase in this AC matrix; see per-AC file for exact test bindings. |
| AC-30 | PASS | Unit/integration evidence recorded in `.local-docs/acceptance/unify-session-observation/evidence.md`; AC-named tests in core/host/dsh/access/cli/mobile and protocol/storage groups. | No native-only mandatory subcase in this AC matrix; see per-AC file for exact test bindings. |
| AC-31 | PASS | Unit/integration evidence recorded in `.local-docs/acceptance/unify-session-observation/evidence.md`; AC-named tests in core/host/dsh/access/cli/mobile and protocol/storage groups. | No native-only mandatory subcase in this AC matrix; see per-AC file for exact test bindings. |
| AC-32 | PASS | Fresh Core/Host/Access + affected native-dsh subset evidence; see evidence.md. | Access expiry/pending-cancellation tests and native open-race/abrupt-exit procedures prove a racing acceptance retains confirmed/unknown evidence without replay; transport publication is not treated as native release. |
| AC-33 | PASS | Unit/integration evidence recorded in `.local-docs/acceptance/unify-session-observation/evidence.md`; AC-named tests in core/host/dsh/access/cli/mobile and protocol/storage groups. | No native-only mandatory subcase in this AC matrix; see per-AC file for exact test bindings. |
| AC-34 | PASS | Unit/integration evidence recorded in `.local-docs/acceptance/unify-session-observation/evidence.md`; AC-named tests in core/host/dsh/access/cli/mobile and protocol/storage groups. | No native-only mandatory subcase in this AC matrix; see per-AC file for exact test bindings. |
| AC-35 | BLOCKED | Partial evidence only; see `.local-docs/acceptance/unify-session-observation/evidence.md` and the per-AC file. | Access/Host honest-fallback tests pass; F02 B7 native cancel-unavailable cannot be produced on installed DSH. |

## Partial acceptance record

The user accepted AC-02, AC-04 and AC-35 as documented external-DSH limitations on
2026-11-18 after reviewing the alternatives. All 34 checkboxes are complete and
32/35 ACs pass; those three remain BLOCKED in the table above because pinned
`@deepseek-ai/dsh 0.2.2-rc.1` exposes no history-disabled state, no post-hoc
blocking-question inventory, and no native cancel-unavailable state (F02
B1/B2/B7). Contract-level Core/Host/Access fault-injection evidence is retained,
but it is not substituted for the missing native procedures. Archive keeps these
outstanding results intact.

## Workflow follow-up

- After fresh acceptance and architecture review, archive only when requested. Archive is outside this implementation checklist.
- OpenSpec sync updates local specs only; select durable documentation updates from the verified implementation rather than copying planning artifacts.
- Cloud adoption of the matching public revision and its real DSH-to-Cloud enrollment acceptance remain a separate repository workflow.
