# Design

## Context

See [proposal](proposal.md) for observed state and scope. Existing commands in `tools/repo` coordinate native tools and saved facts, not releases. The default registry contains Core, CLI, Access, Host, DSH, bindings and tools; mobile applications are not present. Both Rust consumers inherit a shared version today, so independent release identities require manifest changes.

This design is a local plan, not evidence that any publication or deployment already works. The implementation and account/platform prerequisites below remain unverified.

The current integration launcher builds its own isolated source image; it does not consume the final release binaries or npm tarballs. Current `docs/testing.md` requires full integration for relevant change categories but does not implement automatic scheduling. D-09 specifies the newly agreed CI timing; local verification and complete test scope remain applicable.

## Goals / Non-Goals

**Goals:** One release decision authority; reproducible component selection; workspace-first development; explicit test/stable release MRs; machine-enforced artifact and evidence checks; independently reportable capability gates.

**Non-Goals:** New product features, mobile scaffolding, mandatory public Core crate publication, automated production deployment, compulsory weekly releases, or copying application state into release infrastructure.

## Decisions

### Architecture review

| Responsibility | Owner and boundary |
|---|---|
| Project inventory | Existing `tools/repo/projects.toml`; release metadata references registered IDs instead of rediscovering project paths. Future mobile kinds require explicit onboarding. |
| Native package versions | One declared source per release unit; generated changes update manifests, constraints and lockfiles together. Exact version-file placement is part of D-05. |
| Version calculations | One selected version engine behind `./repo`; no competing Cargo and Node authorities. Human input states semantic intent and channel, not file edits. |
| Local release orchestration | `tools/repo` owns deterministic plans, validation, native packager invocation and receipt interpretation. Proposed `release plan/prepare/check/build/publish` names are not existing commands. |
| CI hosting and credentials | GitHub Actions owns event identity, workflow permissions, concurrency and configured publication secrets. Feature-MR workflows do not receive publication authority. |
| Release state | Tracked finalized version metadata plus durable workflow receipts/artifacts; record source SHA, input fingerprints, per-artifact digest and destination outcome. Do not add an application database or queue for CI. |
| Main integration state | Daily/manual workflow records a frozen tested revision, complete input fingerprint, observed outcome and last successful baseline. Failed/incomplete runs never advance that baseline; CI state is not product state. |
| Product state | Unchanged: Core owns client behavior, DSH remains behind the adapter, Access owns PostgreSQL state. Release code does not create another Core injection boundary. |
| Human review | Select components/channel and review semantic changes and acceptance. Merge the generated release MR to initiate publication. |
| CI authoring | Keep business decisions and change selection in local `./repo` commands with fixture coverage. Keep workflow YAML as thin event/permission/job orchestration; lint it statically and simulate compatible steps locally. |

Dependency direction is `feature changes -> affected-consumer calculation -> version plan -> generated manifests -> artifacts -> acceptance -> publication receipts -> final manifest/channel promotion`. A CLI source build can compile Core directly from the locked checkout; it does not require a prior registry upload. Registry dependency ordering applies to packages such as Host and DSH.

### Decision Review

| ID | Options and tradeoffs | Recommendation | Status | Basis | Affected ACs |
|---|---|---|---|---|---|
| D-01 | One product version is simpler; independent delivery units avoid forcing mobile/client/server releases together. | Independent Core, CLI, Access, Android, iOS; fixed DSH/Host group. | CONFIRMED | User requested mobile consumers and asked to record the resulting summarized plan. | 01,03,04,14 |
| D-02 | Registry-only development adds prerequisite publishes; local sources allow atomic cross-component MRs. | Preserve Rust path+version and pnpm workspace dependencies; transform packed output automatically. | CONFIRMED | User discussion of Rust and JS MR dependencies, carried into the requested summary. | 02,03,05,13 |
| D-03 | Publish every merge produces noisy versions; release MRs separate CI from distribution. | Ordinary main -> Actions Artifacts only; explicit test/stable release MR -> publication. | CONFIRMED | User accepted the simplified Actions/Release split; later asked to record the summary. | 06–09 |
| D-04 | Per-ecosystem/manual scripts create drift; one plan with automated adapters keeps updates reviewable. | One version authority, dependency propagation, provenance, checks and resumable publication. | CONFIRMED | User requested automation instead of relying on people; this is in the final summary. | 03–05,10–14 |
| D-05 | Release Please reuses commit-driven multi-language version MRs; Covector handles Rust/JS but uses change files; custom tooling adds maintenance. | Run a bounded Release Please feasibility spike first, then confirm one engine and native version sources. | PENDING | No engine has been selected or verified against this repository; earlier tool recommendations were not user approval. | 01–05,07,08 |
| D-06 | Public vs private registries, namespaces, supported targets and channel permissions alter publication side effects. | Confirm actual GitHub repo, npm/image namespaces, Core distribution choice, platform matrix and test destination before native publishing. | PENDING | Accounts, permissions, target list and actual destination behavior have not been inspected/approved. | 09–11,13–16 |
| D-07 | Build mobile infrastructure now vs onboard actual apps when available. | Reserve independent identities now; define build numbers, bindings and store adapters when apps exist. | PENDING for adapters | User confirmed planned Android/iOS; repository currently has neither app. No mobile technology or distribution contract selected. | 01,03,04,13–15 |
| D-08 | Tool-specific runtime workflow vs repository-owned commands. | Keep engineering entry points in `./repo`, planning ignored, secrets/private payloads excluded. | CONSTRAINED | AGENTS.md, repository skill, docs/testing.md and local OpenSpec evidence contract. | 12,15–17 |
| D-09 | Full integration on every push is expensive; release-only integration delays regression discovery. | Fast checks per commit, conditional daily main sweep at 03:17 Asia/Shanghai plus manual dispatch, fixed-revision integration and actual-artifact verification before RC/stable publication. Reuse only equivalent successful evidence. | CONFIRMED | User accepted the GitHub scheduled-task proposal and requested this OpenSpec update after the Flutter comparison. | 06,07,09,12,13,18–20 |
| D-10 | Push every workflow edit to GitHub for feedback; local simulation alone cannot reproduce GitHub events, permissions or hosted runners. | Put decisions in `./repo`; statically lint Actions YAML; run compatible steps locally; use a small number of actual runs for GitHub-only behavior and use original-SHA re-runs for failures. | CONFIRMED | User agreed with this debug approach and asked whether the planning artifacts needed updating. | 06,07,09,12,18–21 |

No pending item changes the confirmed two-channel release behavior. D-05 blocks engine-specific implementation; D-06 blocks real publication/target claims; D-07 blocks mobile-specific implementation. D-10 settles the debugging strategy but the concrete local runner and workflow linter versions still require implementation-time toolchain selection. Read-only feasibility and planning can continue independently. A request to record or later apply the plan is not implicit approval of these choices.

### Release-unit metadata and source identity

Separate Core and CLI native versions instead of continuing shared `version.workspace = true`. Keep root edition/license/toolchain defaults shared. Preserve a local Core path plus a synchronized version constraint. Bindings/tools need not acquire independent public release channels.

The version engine updates DSH and Host together; source retains `workspace:*`. Pack through the pinned pnpm version and inspect tarballs for the exact expected Host dependency and shipped descriptor. Source manifests must not be rewritten into publication-only forms.

Compute component input identities over relevant source, build configuration, generated inputs and applicable dependency/lockfile inputs. A whole-monorepo commit change alone must not force every component to bump. Conversely, an unchanged development version cannot hide changed Core inputs in a released client. Record both component identity and whole source revision.

Prepare from the selected release baseline to a frozen head, expand required dependency releases, and list validation-only dependents separately. Retry RC numbering against finalized identities; a failed immutable upload can be reused only if identical, otherwise require a new release identity. Keep wire and schema versions separate from component SemVer.

### Lifecycle and authorization

1. Ordinary PR/commit and main events run fast CI, update the unreleased-change inventory as applicable and upload successful outputs to Actions Artifacts under a configured retention policy. They do not automatically schedule full integration. No registry, release tag or GitHub Release is produced.
2. An explicit component/channel selection creates a release MR. The selected engine computes generated changes. Later relevant merges update the active MR and invalidate affected evidence; overlapping component selections share or reject the active plan. These updates rerun fast checks, not the entire integration suite.
3. The merged release MR is recognized from trusted event/MR provenance and validated metadata, never just its title or label. Its merge revision is frozen. Any premerge evidence is reused only after checking relevant input identity; otherwise rerun.
4. After merge, build final-version candidates in a clean checkout. Require full integration for the fixed release inputs, reusing only a successful complete matching evidence record, and separately run isolated installs and target procedures on the actual artifact digests. This gate runs immediately without waiting for daily scheduling. `rc.N` and stable are different builds with distinct validation; never promote RC bytes by relabeling.
5. Persist per-artifact results, upload in dependency order and verify availability. Publish npm packages with a staging approach that avoids accidental `latest` changes before the required stable cohort is complete. Exact mechanism depends on D-06.
6. Finalize component tags, GitHub Pre-release/Release, immutable manifest and intended package channels after the required gate succeeds. A partial upload is an explicit incomplete release; there is no cross-registry atomic transaction.

Serializing overlapping component publications prevents races. Receipts are keyed by component/version/source/channel and retain uploads, verification and promotion outcomes. Retries verify existing artifact identities and continue missing steps; conflicts stop. Credentials, pairing URIs, private message/tool payloads and personal OpenSpec material never enter receipts or artifacts.

### Integration timing and evidence reuse

| Trigger | Fast checks | Full integration | Distribution acceptance |
|---|---|---|---|
| Ordinary PR update / main merge | Static analysis, applicable builds, unit coverage, lightweight protocol consistency where relevant. | Not automatically triggered per push. | Ordinary successful build outputs only; no release claim. |
| Release MR creation / bot update | Version/dependency/lockfile checks plus applicable fast checks. | Not automatically triggered per update. | Packaging checks can run without publishing. |
| Daily main sweep | Check whether relevant unverified inputs exist. | One frozen revision, only when needed. | Does not establish package acceptance. |
| Manual integration dispatch | Validate explicit scope and main revision. | Can verify a repair or investigate a high-risk change early. | No publication authority. |
| Merged RC/stable release MR | Validate trusted release inputs and build final candidates. | Mandatory gate, executed or satisfied by proven equivalent complete successful evidence. | Always require actual-artifact acceptance before uploads. |

Configure GitHub `schedule` for `17 3 * * *` in `Asia/Shanghai`, plus `workflow_dispatch`. The workflow file must be on the default branch, which is expected to be main; do not silently test another default branch. GitHub scheduling is best-effort and may be delayed/missed, and inactive public repositories can have schedules disabled. Manual dispatch remains the recovery path; no exact-time SLA or mandatory release depends on the schedule.

At execution start, freeze main's SHA and derive relevant inputs from product sources, protocol/generated inputs, applicable dependency locks, toolchains, build/container inputs, integration runner/cases/fixtures and configuration. Compare with durable successful evidence, not the latest run regardless of status. Docs-only changes can skip; unknown impact or missing/expired/unreadable receipts conservatively rerun. Record the reason for every skip. Test one accumulated revision rather than pretending every intermediate commit was exercised. Later main commits wait for the next eligible run.

Serialize heavy integration across scheduled, manual and release-triggered execution. An overlapping request queues or coalesces only when input-equivalent; it must not cancel a needed release check or substitute another revision's pass. Once the execution slot is available, recheck for newly available matching evidence before running. Never schedule a second periodic run solely to catch up a missed timeslot.

A complete successful run may advance the passed baseline; failure, cancellation, skipped required cases, failed cleanup or invalid exports leave it unchanged and visible. Main remains integration-unverified until applicable success exists; green unit checks do not imply integrated health. Repairs can be checked immediately via manual dispatch.

Reuse requires the same validated input fingerprint, test implementation/scope, toolchain/environment configuration and complete successful outcome. Start conservatively with the exact source SHA plus those inputs; only allow broader equivalence after tests prove excluded metadata cannot affect execution. RC success is not proof of stable artifact acceptance. A failed main run is not globally permanent: a repaired release revision can pass its own direct gate.

The Flutter comparison motivates split early/late checks rather than release-only testing: [Flutter CI](https://github.com/flutter/flutter/blob/main/.ci.yaml) has path-triggered and postsubmit integration tasks, and [Cocoon](https://github.com/flutter/cocoon/blob/main/CI_YAML.md#postsubmit-features) describes batching under queue pressure. The once-daily choice is WhaleRein's approved cost tradeoff, not a claim that Flutter uses this exact schedule. [GitHub schedule documentation](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#schedule) establishes trigger behavior, not deployed verification of this repository.

### Local CI debugging

Do not put component selection, change impact, release version calculation, integration skip logic or evidence equivalence only in YAML. Implement them as repository-owned commands that accept an explicit revision and fixture data, emit a bounded result, and can run without GitHub credentials. Unit tests cover branch conditions and errors. A workflow lint catches YAML/schema/expression mistakes; a local Actions runner may exercise compatible container jobs and environment wiring.

Local simulation is not authoritative for GitHub event payloads, `GITHUB_TOKEN` scopes, protected environments, OIDC, hosted-runner images, schedules, artifact retention or actual Release/registry behavior. Enumerate those as small real-platform acceptance runs after the workflow logic passes locally. For a failed platform run, inspect normal logs first, then enable GitHub debug logging and rerun the failed job on its original SHA; do not edit and push repeatedly just to inspect a step. Keep all diagnostic output free of secrets and private product data.

### Validation and independent acceptance gates

Reuse current `./repo check`, `build`, `proto --check`, `test --coverage` and full integration. Add actual Node static checks rather than treating unsupported operations as coverage. Preserve native measurement semantics: Rust/Node/Python lines and Go statements, existing thresholds and exclusions.

This schedule does not remove integration scenarios or authorize a local agent to disregard applicable repository verification instructions. During implementation, reconcile `docs/testing.md` and integration/CI usage docs to distinguish local change verification from automatic CI timing. Do not redefine a deferred integration check as a pass.

- **Versioning gate:** AC-01 through AC-05; native Cargo and pnpm source/pack exercises, plus engine feasibility resolved under D-05.
- **Lifecycle gate:** AC-06 through AC-11 and AC-18/19; local decision/job simulation plus real GitHub event/Artifacts and schedule/manual observation, plus explicitly authorized test-destination publication after D-06. Fixture tests alone establish only modeled behavior.
- **Distribution gate:** AC-12 through AC-17 and AC-20/21; isolated installs and declared native environments, migration fixtures and authorized coordinated-restore procedure. Missing target evidence blocks the target.
- **Combined workflow gate:** all ACs required for the configured current components, including ordinary merge -> daily no-change/change/failure/repair checks -> explicit RC release MR -> isolated consumption -> stable release MR -> recovery from an interrupted upload. Verify release independence from scheduled execution and identical-input reuse separately. Bind every mandatory subcase before execution.
- **Mobile gate:** D-07 adapter branches remain BLOCKED until actual apps and native procedures exist. Current-component delivery can be accepted independently; it is not a mobile acceptance claim or unconditional whole-plan completion.

### Acceptance Review

| AC | Scenario and expected outcome | Verification method | Source scenario |
|---|---|---|---|
| AC-01 | Independent Access; fixed DSH group; absent mobile reported honestly. | Release-plan fixtures and native manifest checks. | [AC-01](specs/component-versioning/spec.md#scenario-ac-01-release-units-follow-delivery-boundaries) |
| AC-02 | Joint source edits compile locally; mismatch fails; packaging preserves source. | Native Cargo/pnpm checkout and pack procedures. | [AC-02](specs/component-versioning/spec.md#scenario-ac-02-joint-changes-use-local-dependencies) |
| AC-03 | Deterministic propagation and consumer closure without unrelated publication. | Synthetic dependency graph and native lockfile checks. | [AC-03](specs/component-versioning/spec.md#scenario-ac-03-dependency-propagation-is-deterministic) |
| AC-04 | Changed Core gets a new identity; existing conflicting uploads fail. | Input-change fixtures and receipt/digest faults. | [AC-04](specs/component-versioning/spec.md#scenario-ac-04-changed-source-cannot-masquerade-as-an-old-release) |
| AC-05 | Version, lockfile or tag drift fails before writes. | Deliberately inconsistent release-plan cases. | [AC-05](specs/component-versioning/spec.md#scenario-ac-05-conflicting-metadata-fails-before-publication) |
| AC-06 | Ordinary merges retain only applicable Actions outputs. | Workflow tests and actual GitHub run observation. | [AC-06](specs/release-lifecycle/spec.md#scenario-ac-06-ordinary-main-updates-create-only-development-evidence) |
| AC-07 | Explicit selection maintains one fresh, channel-preserving MR. | Event fixtures and sandbox MR create/update procedure. | [AC-07](specs/release-lifecycle/spec.md#scenario-ac-07-selection-creates-or-updates-one-release-plan) |
| AC-08 | RC sequence, direct stable and stable rebuild work; other channels fail. | Version fixtures, pack metadata and GitHub classification. | [AC-08](specs/release-lifecycle/spec.md#scenario-ac-08-channel-and-version-agree) |
| AC-09 | Only trusted merged release MR publishes its frozen revision. | Spoofed/untrusted events, stale evidence and advancing-main cases. | [AC-09](specs/release-lifecycle/spec.md#scenario-ac-09-merge-authorization-and-source-remain-bound) |
| AC-10 | Host availability precedes DSH; channels promote only after cohort success. | Fault-injected registry fixture and authorized destination install. | [AC-10](specs/release-lifecycle/spec.md#scenario-ac-10-package-consumers-can-resolve-published-dependencies) |
| AC-11 | Overlap serialized; interrupted upload resumes without conflict. | Concurrent workflows and partial-upload/digest/visibility faults. | [AC-11](specs/release-lifecycle/spec.md#scenario-ac-11-resume-and-concurrency-cannot-corrupt-a-release) |
| AC-12 | Fast checks follow consumers; integration is explicitly deferred or gated, never silently passed. | Trigger/selection/failure fixtures plus repository/full integration commands. | [AC-12](specs/release-verification/spec.md#scenario-ac-12-required-checks-cannot-silently-disappear) |
| AC-13 | Actual packages install without workspace; missing files/ABI pairs fail. | Isolated tarball/binary installs; future native mobile procedure. | [AC-13](specs/release-verification/spec.md#scenario-ac-13-distribution-does-not-rely-on-workspace-links) |
| AC-14 | Manifest digests and declared combinations match evidence. | Manifest schema, mutation and combination tests. | [AC-14](specs/release-verification/spec.md#scenario-ac-14-evidence-and-provenance-match-the-advertised-combination) |
| AC-15 | Untested SecretStore/TLS/DSH targets remain incomplete. | Native target procedures and missing-evidence rejection. | [AC-15](specs/release-verification/spec.md#scenario-ac-15-target-environment-limits-remain-visible) |
| AC-16 | New/upgrade state verified; unsafe downgrade rejected; restore qualified. | Isolated PostgreSQL/Host/client state and restore exercises. | [AC-16](specs/release-verification/spec.md#scenario-ac-16-migration-risk-is-a-release-constraint) |
| AC-17 | No SDD dependency or leaked local/private material. | Clean checkout plus synthetic artifact/diagnostic inspection. | [AC-17](specs/release-verification/spec.md#scenario-ac-17-planning-data-is-not-a-release-prerequisite) |
| AC-18 | Daily/manual check freezes main, skips verified/docs-only inputs, and handles late execution honestly. | Trigger/timezone/input fixtures plus actual GitHub schedule/manual observation. | [AC-18](specs/release-lifecycle/spec.md#scenario-ac-18-daily-checks-select-one-unverified-input-set) |
| AC-19 | Failure preserves passed baseline; overlap serialized; repair can run manually. | Failure/cancel/cleanup/concurrency fixtures and authorized workflow rehearsal. | [AC-19](specs/release-lifecycle/spec.md#scenario-ac-19-failed-or-overlapping-checks-remain-recoverable) |
| AC-20 | Release runs or reuses exact integration evidence and independently accepts final artifacts before upload. | Input mutation/reuse tests and RC/stable release candidate rehearsal. | [AC-20](specs/release-verification/spec.md#scenario-ac-20-reuse-cannot-bypass-publication-acceptance) |
| AC-21 | Decisions/fixtures run locally, YAML is statically checked, compatible jobs are simulated, platform-only behavior gets real verification and safe reruns. | Local command tests, workflow linter/local runner, then bounded GitHub run and original-SHA debug rerun. | [AC-21](specs/release-verification/spec.md#scenario-ac-21-workflow-changes-are-debugged-before-platform-validation) |

## Risks / Trade-offs

- Independent versions increase combinations -> advertise only combinations with applicable evidence; do not infer compatibility from SemVer alone.
- Version engines may mishandle workspace prereleases or nested paths -> resolve D-05 with an isolated fixture before production integration.
- Main may contain unready features -> a release includes its fixed checkout; keep changes releasable, explicitly gate features or defer merging. Do not silently cherry-pick files during publication.
- Registry uploads cannot be atomic -> stage channel promotion, retain receipts and report partial outcomes; existing versions are immutable.
- RC and stable binaries differ -> rebuild and verify stable artifacts, retaining provenance separately.
- Core registry publication may require additional crate dependency work -> it remains optional under D-06; CLI path builds do not require it.
- External Actions/registry accounts and mobile stores are not verified -> dependent delivery gates remain blocked, not assumed configured.
- Daily batching increases regression discovery latency -> expose the tested revision and failed/unverified state, allow manual early checks, and gate every publication independently.
- Local Actions simulation can differ from GitHub -> keep decision logic in native scripts and explicitly verify event, permission and release boundaries on GitHub.
- Scheduler delay or disabled schedules can leave main unchecked -> retain manual dispatch and visible baseline; never use the clock as evidence of success.
- Overbroad fingerprints waste runs; narrow ones can hide changes -> start with exact-revision reuse, test every exclusion, and fail closed when equivalence cannot be demonstrated.

## Migration Plan

1. Complete read-only/isolated engine and hosting feasibility, then confirm D-05 and D-06 before their dependent implementation. No production registry writes are part of planning.
2. Introduce independent manifest identities without changing product APIs. Keep applicable native package constraints and lockfiles synchronized. Add repository-owned release planning/checking and Node static checks.
3. Exercise version PR generation and packing against fixtures; install fast ordinary CI, conditional daily/manual integration and independent release verification. Reconcile documented automatic timing and test baseline/failure/reuse behavior before enabling the schedule. Run local workflow lint/simulation before a bounded real GitHub smoke run. D-06 determines actual host settings and permissions.
4. Rehearse RC publication, stable rebuild and interrupted-upload recovery at authorized test destinations. Add only the tested targets to the manifest.
5. Enable stable publishing after acceptance. Add mobile adapters through D-07 once projects exist; do not make current workflows depend on placeholder apps.

Workflow rollback disables publication triggers and preserves immutable receipts/uploads; it does not undo application migrations. Access rejects unknown migration ledger entries. Follow [deployment recovery](../../../deploy/README.md#backup-and-restore) for coordinated state restoration, not deleting migration rows or automatically retrying uncertain commands.

## Agent execution details

Relevant existing seams: `tools/repo/src/repo/cli.py`, `projects.py`, native runners, `process.py`, reports and tests including `test_architecture.py`, `test_cli_help.py`, `test_execution_regressions.py` and `test_coverage.py`. Keep native ecosystem commands in their adapters and treat CI credentials as orchestration inputs.

Read-only feasibility must cover exact version sources, RC constraints with local Cargo dependencies, pnpm pack transformations, dependency closure and fixed DSH groups, lockfile regeneration, merged-MR event binding and permission model. D-06 additionally selects platform/DSH/TLS procedures, registry namespaces and allowed verification writes. D-10 leaves the particular action linter and local runner choice for implementation, where availability, Docker compatibility, secrets policy and runner fidelity can be checked. Do not install a new dependency, modify repository settings or create external test MRs merely to finish this planning task.

Maintained docs are updated from implemented and verified behavior only. Local plans, dated evidence and task history stay ignored. Ordinary workflow operation must work in a clean checkout with no `openspec/` directory.
