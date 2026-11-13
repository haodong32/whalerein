# Spec Delta

## Purpose

Keep independently delivered WhaleRein components identifiable and reproducible while allowing a feature to change multiple consumers in one workspace MR.

## ADDED Requirements

### Requirement: Independent release identities

The workflow SHALL independently version Core, CLI, Access and the DSH group. The DSH plugin and Host runtime MUST share one version. Mobile clients SHALL become independent release units when introduced, with Core bindings and native libraries following the Core SDK version.

#### Scenario: AC-01 Release units follow delivery boundaries
- **WHEN** a release is prepared for changed Access or DSH sources
- **THEN** an Access-only change does not force unrelated units to increase versions, while a change to either DSH package updates both DSH packages to the same version
- **AND** absent Android/iOS projects are reported as not onboarded rather than successfully built or published

### Requirement: Workspace development uses current source

The workflow SHALL preserve Rust path dependencies with version constraints and pnpm workspace dependencies in source MRs. Multiple components MUST be jointly editable and testable without a prior registry publication or manual dependency-source switch.

#### Scenario: AC-02 Joint changes use local dependencies
- **WHEN** a feature changes Core and CLI together, or Host and DSH together
- **THEN** builds use the current checkout, a Rust version-constraint mismatch fails, and no registry upload is required to validate the feature
- **AND** source manifests retain their local dependency declarations after packaging

### Requirement: Release preparation propagates version changes

The workflow SHALL generate release versions, internal dependency constraints and lockfile updates automatically. It MUST distinguish affected validation consumers from selected publication units, include required dependency releases, and reject stale or inconsistent release metadata.

#### Scenario: AC-03 Dependency propagation is deterministic
- **WHEN** unreleased Core or Host changes are included in a selected release
- **THEN** repeated preparation of identical inputs produces the same version plan, updates required consumer constraints and lockfiles, and includes the fixed DSH group
- **AND** consumer validation includes dependents even when their directories are unchanged, while unrelated units are not automatically published

### Requirement: Released versions identify immutable source

Every published component SHALL record its version, source revision and artifact digest. A changed embedded Core MUST NOT be represented as the unchanged previously published Core solely because its development manifest version remained unchanged.

#### Scenario: AC-04 Changed source cannot masquerade as an old release
- **WHEN** a client release contains Core inputs different from the last published Core identity
- **THEN** preparation requires an appropriate Core release identity and records the actual embedded Core source
- **AND** publishing different bytes under an existing artifact identity is rejected, while identical verified artifacts can be reused during recovery

### Requirement: Release metadata has one authority

Version automation SHALL use one authoritative release plan per selected source revision and channel, and one declared version source per release unit. It MUST reject disagreement between generated native manifests, release metadata and tags.

#### Scenario: AC-05 Conflicting metadata fails before publication
- **WHEN** a generated package version, internal constraint, lockfile or tag disagrees with the release plan
- **THEN** validation fails before any publication or channel update
- **AND** no second independent JS or Rust version engine silently overrides the plan
