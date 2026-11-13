# Tasks

Implementation is not started. Read [Decision Review](design.md#decision-review) before applying. Pending decisions block only the dependent tasks and acceptance branches listed below. A checked task is not a passing AC.

## 1. Resolve bounded implementation prerequisites

- [ ] 1.1 Reconcile manifests, `tools/repo` contracts, current source revision and existing workflows against the proposal, including current source-built integration and local-vs-CI verification policy; verify by saving a read-only inventory and any drift under `.local-docs/plans/automate-monorepo-releases/` (AC-01,02,12,17,20).
- [ ] 1.2 Investigate D-05 with an isolated Cargo/pnpm fixture and official version-engine interfaces: test independent versions, RC constraints, dependency propagation, fixed DSH group, lockfiles and explicit channel selection; verify by recording concrete outputs and recommending one engine/version-source map without registry writes (AC-01–05,07,08).
- [ ] 1.3 Present D-05 findings and record the user's engine decision before engine-specific implementation; verify that the decision basis and adapter contract are explicit. This decision task can proceed, but tasks dependent on D-05 remain BLOCKED until resolved.
- [ ] 1.4 Read available non-secret hosting/destination metadata and propose D-06 namespace, permissions, platform/DSH matrix, Core distribution and sandbox choices; obtain decisions before external writes. Verify by a documented destination matrix and permitted native rehearsal procedure (AC-09–11,13–16).
- [ ] 1.5 Record D-07 as deferred onboarding while apps are absent; verify that no placeholder mobile products are registered and that mobile implementation/acceptance remains visibly BLOCKED (AC-01,03,04,13–15).

## 2. Establish independent versions and deterministic preparation

- [ ] 2.1 BLOCKED by D-05: separate Core/CLI version sources in `Cargo.toml`, `core/Cargo.toml`, `apps/cli/Cargo.toml`; preserve local Core path plus version constraint and lockfile integrity. Verify joint native build and deliberate mismatch rejection (AC-01,02,05).
- [ ] 2.2 BLOCKED by D-05: declare Access identity and the fixed Host/DSH group using native version sources and existing IDs from `tools/repo/projects.toml`; add registry-reference validation without a second project catalog. Verify Access-only and Host-only fixture plans (AC-01,05).
- [ ] 2.3 BLOCKED by D-05: add repository-owned release plan/prepare/check entry points through `tools/repo/src/repo/cli.py` and focused release modules; calculate independent baselines, affected-consumer closure, required release closure and embedded-source fingerprints. Verify deterministic plans, source/constraint propagation and unchanged-unit exclusion (AC-03–05).
- [ ] 2.4 BLOCKED by D-05: adapt the selected engine to generate versions, lockfiles, release notes and a selected-channel release-MR payload; preserve Cargo paths and pnpm workspace source manifests. Verify RC numbering, direct stable, repeated preparation and conflict rejection with native fixtures (AC-02,03,05,07,08).
- [ ] 2.5 Land meaningful release-plan/CLI tests under `tools/repo/tests/`, including invalid input and no-dispatch help behavior; run `./repo check --project repo` and `./repo test --project repo --coverage`. Document only implemented command contracts in `tools/repo/README.md` and verify examples (AC-01–05,07,08,17).

## 3. Automate ordinary CI and release-MR lifecycle

- [ ] 3.1 Add actual applicable Node static checks to both package manifests and keep `projects.py` capability handling accurate; verify affected Node checks run rather than being silently skipped, then run relevant Host/DSH unit coverage checks (AC-12).
- [ ] 3.2 Add fast ordinary `.github/workflows/` CI using `./repo`, frozen dependencies and configured Actions Artifact retention; verify feature/doc/tool event fixtures do not automatically start full integration or cause publication, tag, release or channel side effects. Actual GitHub observation is BLOCKED by D-06 (AC-06,12,17).
- [ ] 3.3 BLOCKED by D-05 for engine adapter and D-06 for live GitHub writes: wire explicit components/channel selection to release MR creation/update, preserve selection across main changes and reject overlap; verify lifecycle fixtures, no per-update automatic full integration, and an authorized sandbox MR procedure (AC-07,08,12).
- [ ] 3.4 Add trusted merged-release-MR recognition, frozen revision binding, input-based evidence invalidation and separate publication permissions; verify spoofed-title/label, feature-MR, stale-evidence, later-main and recursive-event negatives. Live permission proof is BLOCKED by D-06 (AC-09).
- [ ] 3.5 Land workflow orchestration tests with each lifecycle change; update verified development/release usage docs for ordinary versus release events, and verify the documented event paths in sandbox before calling them operational (AC-06–09,12,17).
- [ ] 3.6 Add the D-09 daily/manual integration workflow (`17 3 * * *`, `Asia/Shanghai`) and repository-owned selection checks; freeze main SHA, detect relevant unverified inputs and explain docs-only/already-verified skips. Verify timezone, no-change, product/protocol/lock/build/test change, unknown-impact, delayed start and advancing-main fixtures; native observation is BLOCKED by D-06 (AC-12,18).
- [ ] 3.7 Add durable passed-baseline receipts and shared heavy-integration serialization for daily/manual/release requests; verify failure/cancellation/invalid-export/cleanup failures never advance the baseline, queued inputs stay correct and manual repair does not wait until the next day. Land meaningful baseline/concurrency tests with this work (AC-19,20).
- [ ] 3.8 Add schedule/reuse tests under `tools/repo/tests/` and run `./repo check --project repo` plus `./repo test --project repo --coverage`; reconcile implemented CI timing in `docs/testing.md`, integration README and developer/release docs while retaining local verification obligations. Verify docs distinguish delayed integration from success and explain missed-schedule/manual recovery (AC-12,17–20).
- [ ] 3.9 Add repository-owned local plan/decision commands and fixtures for CI conditions; add the pinned `actionlint` static check to local docs and CI, and provide an optional local runner for compatible jobs. Verify selection, release planning and skip/error logic require neither GitHub credentials nor a push, and unsupported GitHub-only features are explicitly surfaced (AC-21).
- [ ] 3.10 Run a bounded GitHub smoke workflow after all local checks: verify actual event payload, token permissions, runner/artifact behavior, schedule/manual dispatch and non-publishing release plan. For failures, verify `gh run rerun --failed --debug` or the UI rerun preserves the original SHA, and inspect diagnostics for secret/private-data leakage. Live run is BLOCKED by D-06 (AC-06,07,09,18,21).

## 4. Build and verify distributed artifacts

- [ ] 4.1 Add `./repo` release build/check adapters for CLI binaries, Access image/version metadata and pnpm tarballs; freeze source SHA and collect digests and embedded Core inputs. Final supported matrix and Core registry mode are BLOCKED by D-06 (AC-04,13,14).
- [ ] 4.2 Implement isolated Host/DSH tarball inspection/install and smoke procedures outside the checkout; verify required descriptor/runtime files, exact Host dependency and source-manifest preservation. Add missing-file and `workspace:`/`file:`/`link:` failure cases; no public registry writes are required for fixture tests (AC-02,13).
- [ ] 4.3 Add manifest validation with exact artifact identities, retained component references, tested combinations, evidence status and upgrade constraints; verify mutated digest, unknown target and stale evidence rejection (AC-04,05,14,15).
- [ ] 4.4 Add migration-aware release checks using `access/internal/store` migrations, Host state and applicable client state; verify new install, retained-state upgrade and unknown-schema downgrade rejection in isolated fixtures. Historical-restore native rehearsal is BLOCKED by D-06 procedure approval (AC-16).
- [ ] 4.5 Land package/migration/evidence tests alongside adapters; run relevant repository/unit coverage and `./repo test --suite integration`. Update verified operator packaging/upgrade guidance where behavior is implemented (AC-12–16).
- [ ] 4.6 Add input-bound integration evidence lookup with conservative exact-revision reuse and actual-artifact acceptance receipts; verify same-input successful reuse, absent/expired/unreadable/failed/partial result rerun, and invalidation on source/dependency/toolchain/configuration/scope changes. Verify source-built integration cannot replace final tarball/binary acceptance; require new acceptance for changed artifact digests (AC-13,20).

## 5. Publish and recover safely

- [ ] 5.1 Add publication planning and fixture destination adapters with per-artifact receipts, availability verification, dependency order and staged channel promotion; verify Host-before-DSH, delayed visibility and no stable promotion on partial failure. Real adapter destinations remain BLOCKED by D-06 (AC-10,11).
- [ ] 5.2 Implement overlap serialization, cancellation/incomplete-state retention and retry of missing steps only; verify simultaneous releases, duplicate successful uploads, digest conflicts and interrupted promotion with fault injection (AC-04,11).
- [ ] 5.3 BLOCKED by D-06: wire configured credentials and native publication adapters; after release-MR merge build fixed-revision candidates and run or validly reuse full integration plus final-artifact acceptance without waiting for the daily sweep. Verify uploads cannot precede success, test versions land on npm `next` and GitHub Pre-release, and rebuilt/verified stable versions promote `latest` and regular Releases only after cohort success (AC-08–11,14,20).
- [ ] 5.4 BLOCKED by D-06: bind and execute the approved GitHub/registry/platform SecretStore/TLS/DSH procedures; verify actual scheduled/manual workflows, baseline/failure handling, independent release triggering and target evidence before advertising support; missing environments stay blocked (AC-06,07,09–11,13–15,18–20).
- [ ] 5.5 Land publisher/recovery tests and verified retry/channel instructions with these changes; run tooling checks/coverage and relevant full integration before enabling the publication workflow (AC-04,08–12,14,17).

## 6. Future mobile onboarding and combined acceptance

- [ ] 6.1 BLOCKED by D-07: when actual Android/iOS projects exist, register their build adapters and version sources, identify binding/native-library contract and platform build numbers, and verify matching and deliberately mismatched pairs plus platform credential/lifecycle procedures (AC-01,03,04,13–15). Do not implement mobile apps as part of this release-tooling change.
- [ ] 6.2 Bind every mandatory case below to an exact test or native procedure, with revision, setup, action, assertions, command, result path and subcase status; verify no branch is represented solely by a suite summary (AC-01–21).
- [ ] 6.3 Run fresh `./repo check`, `./repo build`, `./repo proto --check`, `./repo test --coverage`, and `./repo test --suite integration`; verify required native measurements and actual reports, then reconcile AC-12 and limitations. Mobile adapters require their explicitly onboarded checks as well.
- [ ] 6.4 BLOCKED by applicable D-05/D-06: execute ordinary merges -> daily unchanged/changed/failure/manual-repair checks -> explicit RC MR -> installation -> stable MR -> interrupted-upload recovery for configured current components; verify scheduling independence, matching-evidence reuse, receipts, channels, local workflow debug and Actions/Release separation against AC-01–21, with D-07 branches still distinct.
- [ ] 6.5 Assess architecture deviations and the need for maintained docs from final verified behavior; verify `docs/development.md`, `docs/releasing.md`, `docs/README.md`, tool reference and deployment guidance agree where applicable, or record a reason no update is needed. Keep dated results and planning local (AC-17).
- [ ] 6.6 Update the Acceptance Results and capability gates from fresh per-case evidence; verify no FAIL/BLOCKED/NOT RUN branch is counted as a pass, and report current-component delivery separately from deferred mobile and whole-change acceptance (AC-01–21).

## AC-to-case matrix

Case IDs below are planned binding identities, not existing test names. Use prefix `automate-monorepo-releases/` followed by the listed AC/subcase, for example `automate-monorepo-releases/AC-02.rust-local`. Before execution, map each to an exact implemented test selector or native procedure in `.local-docs/acceptance/automate-monorepo-releases/cases.md`. All listed branches are mandatory for their applicable gate.

| AC | Mandatory subcases | Layers | Owning tasks |
|---|---|---|---|
| AC-01 | `.access-only`, `.dsh-fixed`, `.mobile-absent`, `.mobile-onboarded` (D-07) | Plan fixtures; native manifests | 1.5,2.1,2.2,2.5,6.1 |
| AC-02 | `.rust-local`, `.rust-mismatch`, `.node-local`, `.pack-preserves-source` | Native Cargo/pnpm | 2.1,2.4,2.5,4.2 |
| AC-03 | `.repeatable`, `.core-consumers`, `.host-group`, `.lockfile`, `.unrelated-excluded`, `.mobile-consumer` (D-07) | Graph fixtures; native lockfiles | 2.3–2.5,6.1 |
| AC-04 | `.changed-core`, `.identical-reuse`, `.digest-conflict`, `.mobile-embedded` (D-07) | Input fixtures; actual artifact metadata | 2.3,4.1,4.3,5.2,6.1 |
| AC-05 | `.manifest-drift`, `.constraint-drift`, `.lockfile-drift`, `.tag-drift`, `.one-authority` | Unit/CLI with destination-write sentinel | 2.1–2.5,4.3 |
| AC-06 | `.feature`, `.docs`, `.tooling`, `.failed-build`, `.no-per-push-integration`, `.actions-only` (D-06) | Workflow fixtures; actual Actions run | 3.2,3.5,5.4 |
| AC-07 | `.explicit-selection`, `.update`, `.channel-preserved`, `.overlap`, `.no-implicit-channel`, `.no-per-update-integration` | Fixtures; native MR lifecycle (D-05/D-06) | 2.4,3.3,3.5,5.4 |
| AC-08 | `.rc-first`, `.rc-next`, `.direct-stable`, `.reject-other`, `.stable-rebuild` | Version tests; actual pack/classification | 2.4,3.3,5.3 |
| AC-09 | `.trusted-merge`, `.spoofed-label`, `.untrusted-mr`, `.main-advances`, `.stale-evidence`, `.no-recursion` | Event tests; native permissions (D-06) | 3.4,5.4 |
| AC-10 | `.host-first`, `.visibility-delay`, `.next`, `.latest-after-success`, `.partial-no-promotion`, `.registry-install` | Registry fixtures; native destination (D-06) | 5.1,5.3,5.4 |
| AC-11 | `.concurrent`, `.partial-upload`, `.identical-retry`, `.conflict`, `.cancel`, `.promotion-resume` | Fault fixtures; concurrent native runs (D-06) | 5.1–5.5 |
| AC-12 | `.consumer-closure`, `.node-static`, `.coverage-missing`, `.coverage-low`, `.integration-required`, `.integration-deferred`, `.cancel-cleanup`, `.required-skip` | Fast-check/trigger fixtures; daily/release full integration | 2.5,3.1–3.3,3.6,3.8,4.5,5.5,6.3 |
| AC-13 | `.isolated-js`, `.exact-dependency`, `.descriptor`, `.missing-file`, `.local-reference`, `.binary`, `.binding-match`, `.binding-mismatch` (last two D-07) | Actual tarball/binary; native mobile (D-07) | 4.1,4.2,4.5,5.4,6.1 |
| AC-14 | `.single-unit`, `.coordinated`, `.retained-identity`, `.mutated-artifact`, `.unsupported-combination`, `.stale-evidence` | Manifest tests; native artifact/receipt evidence | 4.1,4.3,5.3,5.4 |
| AC-15 | `.secretstore`, `.tls`, `.dsh`, `.missing-target`, `.pinned-linux-limit`, `.mobile-target` (D-07) | Gate fixtures; declared native targets (D-06/D-07) | 4.3,5.4,6.1 |
| AC-16 | `.new-install`, `.pg-upgrade`, `.host-upgrade`, `.client-upgrade`, `.unsafe-downgrade`, `.historical-restore` | Isolated state tests; coordinated restore (D-06) | 4.4,4.5 |
| AC-17 | `.no-openspec`, `.ignored-plans`, `.artifact-exclusion`, `.diagnostics`, `.docs-match` | Clean checkout; synthetic payload inspection; docs review | 3.2,3.5,5.5,6.5 |
| AC-18 | `.timezone`, `.multiple-merges`, `.docs-only`, `.already-verified`, `.product`, `.protocol`, `.dependency`, `.build-test-input`, `.unknown-impact`, `.late-start`, `.main-advances`, `.manual`, `.no-publication` | Input/scheduler fixtures; actual scheduled/manual Actions (D-06) | 3.6,3.8,5.4,6.4 |
| AC-19 | `.failure`, `.cancel`, `.invalid-export`, `.cleanup-failure`, `.baseline-retained`, `.overlap`, `.repair-manual`, `.queued-release-input` | Baseline/concurrency fixtures; native workflow rehearsal (D-06) | 3.7,3.8,5.4,6.4 |
| AC-20 | `.matching-reuse`, `.missing-receipt`, `.expired-unreadable`, `.failed-partial`, `.source-change`, `.dependency-change`, `.toolchain-change`, `.config-scope-change`, `.source-image-insufficient`, `.artifact-digest-change`, `.before-upload`, `.schedule-independent`, `.rc-stable-separate` | Evidence fault tests; native final-artifact/release rehearsal (D-06) | 3.7,3.8,4.6,5.3,5.4,6.4 |
| AC-21 | `.local-plan`, `.fixture-selection`, `.no-credentials`, `.workflow-static-lint`, `.local-compatible-job`, `.github-event`, `.token-scope`, `.schedule`, `.artifact`, `.original-sha-rerun`, `.diagnostic-redaction` | Repository tests; workflow linter/local runner; actual GitHub smoke (D-06) | 3.9,3.10,6.2,6.4 |

For each native procedure, record exact versions, allowed environment and observed outcomes. Use only synthetic sessions/data. Never log real messages, tool parameters, private keys or pairing tokens. Any subcase failure makes the parent FAIL; otherwise unresolved prerequisites yield BLOCKED, an unexecuted mandatory branch yields NOT RUN, and only all-PASS yields PASS. Future-mobile branches are not silently waived to complete current-component delivery.

## Acceptance Results

No implementation or acceptance checks have run. OpenSpec validation only verifies planning structure, not runtime behavior. Evidence entries below must be replaced with fresh case bindings/results during implementation.

| AC | Status | Evidence | Limitations |
|---|---|---|---|
| AC-01 | NOT RUN | None | D-05; mobile branch requires D-07. |
| AC-02 | NOT RUN | None | Native fixture execution pending D-05. |
| AC-03 | NOT RUN | None | D-05; mobile branch requires D-07. |
| AC-04 | NOT RUN | None | Input identity and receipts not implemented; D-07 mobile branch. |
| AC-05 | NOT RUN | None | Engine and validators not implemented. |
| AC-06 | NOT RUN | None | No CI workflows; native hosting requires D-06. |
| AC-07 | NOT RUN | None | D-05/D-06 unresolved. |
| AC-08 | NOT RUN | None | Version and pack/channel tests not executed. |
| AC-09 | NOT RUN | None | No permission/event binding implementation; D-06 native proof. |
| AC-10 | NOT RUN | None | D-06 destinations and rehearsal authorization unresolved. |
| AC-11 | NOT RUN | None | Fault injection and native concurrency not executed. |
| AC-12 | NOT RUN | None | No change-specific native/unit/integration checks run. |
| AC-13 | NOT RUN | None | Packaging adapters pending; D-06 target and D-07 mobile branches. |
| AC-14 | NOT RUN | None | Release manifest and evidence checks not implemented. |
| AC-15 | NOT RUN | None | D-06 target matrix and D-07 mobile prerequisites unresolved. |
| AC-16 | NOT RUN | None | Migration/restore procedures not executed; D-06 native scope. |
| AC-17 | NOT RUN | None | Runtime clean-checkout and synthetic exclusion tests not executed. |
| AC-18 | NOT RUN | None | Schedule, change selection and manual procedures not implemented; native hosting requires D-06. |
| AC-19 | NOT RUN | None | Baseline retention, heavy-run serialization and repair procedures not executed. |
| AC-20 | NOT RUN | None | Input-equivalence reuse and independent final-artifact gate not implemented or exercised. |
| AC-21 | NOT RUN | None | Local workflow tooling, static validation and GitHub platform smoke are not implemented or run. |

## Workflow follow-up

- Review the architecture, Decision Review and Acceptance Review before implementation; resolve pending choices for dependent work.
- After implementation, verify against fresh per-case evidence and report partial delivery honestly before considering archive.
- Archive only on a separate user request. Keep archived OpenSpec artifacts ignored and update tracked docs only from verified durable facts.
