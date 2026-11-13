# Spec Delta

## Purpose

Ensure released artifacts and compatibility claims are supported by current, scoped evidence rather than by source-only builds or manually maintained release checklists.

## ADDED Requirements

### Requirement: Required validation follows affected consumers

The workflow SHALL apply fast static, build, unit and coverage checks to affected consumers on ordinary commits. Full integration SHALL cover batched main changes and each release gate, rather than every push or release-MR update. Manual early integration MUST remain available. Missing required measurements, interrupted checks and unsupported operations MUST NOT count as passes.

#### Scenario: AC-12 Required checks cannot silently disappear
- **WHEN** Core, shared protocol, Node packages or release tooling change
- **THEN** affected consumers receive fast checks, heavy integration is deferred to the daily/manual or fixed-revision release gate, and native per-scope coverage gates remain enforced
- **AND** missing Node static checks, absent coverage, failed cleanup, cancellation or skipped required checks block the relevant gate instead of producing a false pass
- **AND** deliberate integration deferral is reported as not yet verified, never as a passing integration result

### Requirement: Packed artifacts work outside the checkout

Release validation SHALL inspect and install the actual packed artifacts outside the source workspace. Published JS manifests MUST contain resolvable registry dependencies and no unresolved workspace or filesystem references. Binding and native-library identities MUST match when such SDK artifacts are onboarded.

#### Scenario: AC-13 Distribution does not rely on workspace links
- **WHEN** a DSH package cohort or configured SDK/client artifact is prepared
- **THEN** isolated installation and smoke checks run on the actual artifacts, required runtime files and protocol descriptors exist, and generated JS dependencies resolve to the intended versions
- **AND** missing files, residual local references and mismatched binding/native-library pairs fail validation; mobile branches remain blocked until D-07 onboarding

### Requirement: Release manifests describe verified combinations

A published release SHALL include selected component versions, source revisions, embedded Core identities, artifact digests, tested combinations, supported target environments and upgrade limitations. Unchanged components MAY be referenced from previous releases only when their exact identities and applicable evidence are retained.

#### Scenario: AC-14 Evidence and provenance match the advertised combination
- **WHEN** a coordinated or single-component release is finalized
- **THEN** its manifest identifies all relevant artifacts and retained component identities, and every supported combination points to current applicable evidence
- **AND** a missing target check, mutated artifact or unsupported combination prevents that claim from being marked verified

### Requirement: Native acceptance remains distinct from integration

Release validation SHALL exercise declared target-platform credential storage, configured TLS and supported DSH environments. Pinned native DSH/browser integration MUST NOT be treated as proof for untested platforms or versions. Missing required target evidence SHALL block the affected target without fabricating success.

#### Scenario: AC-15 Target environment limits remain visible
- **WHEN** Linux integration passes but a declared platform SecretStore, production TLS or DSH configuration has not been exercised
- **THEN** the affected release-target gate remains incomplete and the manifest does not claim that target is verified
- **AND** actual target-environment procedures and results are required before that target can be accepted, subject to D-06

### Requirement: State migration constrains upgrade and rollback

Release preparation SHALL identify PostgreSQL, Host and client state changes and require applicable upgrade and recovery evidence. A release MUST NOT promise direct binary downgrade against a newer incompatible schema or silently treat historical restoration as normal restart recovery.

#### Scenario: AC-16 Migration risk is a release constraint
- **WHEN** a release changes persisted state or declares a binary rollback path
- **THEN** new-install and retained-state upgrade checks run, declared rollback behavior is tested, and incompatible downgrades are rejected
- **AND** historical backup restoration follows the documented coordinated recovery and revocation/deduplication precautions rather than automatically reopening old state to clients

### Requirement: Repository workflows remain independent of planning tools

Release checks and commands SHALL be usable through the repository workflow without OpenSpec or another SDD tool. Intermediate plans and acceptance histories MUST remain Git-ignored; only verified durable usage, release metadata and operator guidance SHALL enter tracked documentation.

#### Scenario: AC-17 Planning data is not a release prerequisite
- **WHEN** a clean checkout without personal OpenSpec files runs release checks
- **THEN** checks use tracked repository configuration and declared inputs, do not need local plans, and do not upload planning artifacts, credentials or private payloads
- **AND** public diagnostics and retained evidence contain only allowed metadata and synthetic fixtures

### Requirement: Publication verifies fixed inputs and actual artifacts

After a release MR merges, publication SHALL build fixed-revision candidates and require successful full integration plus actual-artifact acceptance before uploads. Successful integration evidence MAY be reused only for identical validated inputs, configuration and complete scope. Release validation MUST proceed independently of daily scheduling.

#### Scenario: AC-20 Reuse cannot bypass publication acceptance
- **WHEN** a test/stable release reaches its verification gate or retries publication
- **THEN** it runs full integration unless equivalent successful evidence is available, rejects stale/partial/failed or unreadable evidence, and always requires acceptance bound to the actual artifact digests before upload
- **AND** changes to source, dependencies, toolchain, test configuration or scope invalidate affected evidence; unproven equivalence reruns integration
- **AND** a successful source-built test image does not substitute for installing final packages, release validation does not wait for a scheduled sweep, and stable candidates are rebuilt and verified separately from RC candidates

### Requirement: Workflow logic is locally diagnosable

CI decision logic SHALL be runnable through repository-owned commands with explicit inputs and synthetic fixtures. Workflow definitions SHALL receive local static validation, and compatible jobs SHOULD be locally simulatable. The workflow MUST identify GitHub-only behavior that requires a real run rather than claiming local simulation proves it.

#### Scenario: AC-21 Workflow changes are debugged before platform validation
- **WHEN** a workflow or its release/integration decision logic changes
- **THEN** the repository command and fixture reproduce the relevant selection, version plan or failure path locally, workflow syntax/configuration is statically checked, and compatible job steps can be exercised with a local runner
- **AND** event delivery, scheduled triggering, token permissions, GitHub-hosted runner differences, artifact retention and release publication are listed as platform checks for a small number of real GitHub runs
- **AND** rerunning a failed GitHub job uses the original run's source revision, and debug logs do not expose secrets or private product payloads
