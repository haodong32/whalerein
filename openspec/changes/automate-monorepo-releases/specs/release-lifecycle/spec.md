# Spec Delta

## Purpose

Separate ordinary development builds from explicit test and stable releases so maintainers control release timing while automation performs publication consistently.

## ADDED Requirements

### Requirement: Ordinary merges do not publish versions

An ordinary feature MR merged to main SHALL trigger fast CI and accumulate unreleased changes without automatically scheduling full integration. Downloadable build outputs SHALL be stored in GitHub Actions Artifacts with source provenance. Ordinary merges MUST NOT publish packages, create release tags or GitHub Releases, or update stable channels.

#### Scenario: AC-06 Ordinary main updates create only development evidence
- **WHEN** an ordinary feature, documentation or tooling MR merges to main
- **THEN** fast checks run and applicable build outputs are retained as Actions Artifacts, with no automatic full integration, package publication, release tag or GitHub Release
- **AND** documentation-only changes do not invent product release entries, and unsuccessful builds are not represented as downloadable successful products

### Requirement: A release MR carries an explicit selection

Release preparation SHALL require selected components and either the test or stable channel. Automation MUST create or update the corresponding release MR, preserve its selected channel, summarize included changes and reject overlapping active plans for the same release unit.

#### Scenario: AC-07 Selection creates or updates one release plan
- **WHEN** a maintainer selects components and a supported channel, then additional relevant changes arrive before merge
- **THEN** automation creates or updates the active release MR, recalculates its generated contents and reruns fast validation without automatically starting full integration on every update
- **AND** ordinary source merges alone do not select a channel or publish, while overlapping requests are rejected or attached to the existing plan

### Requirement: Only test and stable releases are supported

Test releases SHALL use `x.y.z-rc.N` and GitHub Pre-release. Stable releases SHALL use `x.y.z` and regular GitHub Releases. The workflow MUST support direct stable releases and reject alpha, beta and snapshot channel requests.

#### Scenario: AC-08 Channel and version agree
- **WHEN** a test or stable release is prepared
- **THEN** automation generates the matching version and release classification, advances test sequence numbers without reusing published identities, and permits a stable release without a preceding test release
- **AND** a stable release is rebuilt and verified with its own version metadata rather than relabeling RC bytes

### Requirement: Publication is bound to the release MR revision

Only merging a recognized, validated release MR SHALL initiate its publication workflow. Publication MUST use a fixed source revision, revalidate invalidated evidence and keep later main changes outside the release. An ordinary workflow or untrusted MR MUST NOT obtain publication authority.

#### Scenario: AC-09 Merge authorization and source remain bound
- **WHEN** a release MR is merged and main subsequently advances
- **THEN** publication builds only the recorded release revision and rejects evidence from incompatible inputs
- **AND** ordinary commits, spoofed titles or labels, and untrusted feature-MR runs do not authorize publication, and release commits do not recursively generate the same release again

### Requirement: Dependency publication precedes channel promotion

Publication SHALL respect required registry dependency order and verify destination availability. Test npm packages SHALL target `next`; stable packages SHALL update `latest` only after the required publication cohort succeeds. GitHub attachments MUST NOT substitute for required registry publication.

#### Scenario: AC-10 Package consumers can resolve published dependencies
- **WHEN** the DSH group is published to a configured destination after D-06 is resolved
- **THEN** the Host package is available before the plugin publication completes, packed dependencies resolve outside the workspace, and the npm channel matches the plan
- **AND** a partially failed cohort does not promote stable channels or mark the complete release successful

### Requirement: Concurrent and failed runs preserve immutable publication state

Publication SHALL serialize overlapping component releases and retain per-artifact outcomes. Retrying a failed run MUST reuse only verified identical artifacts, continue missing steps, and reject identity or digest conflicts. Failure MUST NOT overwrite versions or imply deployment rollback.

#### Scenario: AC-11 Resume and concurrency cannot corrupt a release
- **WHEN** two runs target the same unit or a run fails after some uploads
- **THEN** only one overlapping publication proceeds, a retry verifies existing uploads and completes missing steps, and conflicting bytes stop recovery
- **AND** cancellation, registry visibility delay and unresolved uploads remain explicit incomplete outcomes

### Requirement: Daily integration batches main changes

The workflow SHALL schedule one daily integration check at 03:17 Asia/Shanghai and provide manual dispatch. It MUST freeze main's revision at run start and run full integration only for relevant inputs lacking equivalent successful evidence. The schedule MUST NOT create a release or publish packages.

#### Scenario: AC-18 Daily checks select one unverified input set
- **WHEN** a scheduled check starts after multiple main merges, starts late, or is manually dispatched
- **THEN** it selects one fixed main revision, runs full integration for unverified product/protocol/dependency/build/test inputs, and leaves later commits for a subsequent check
- **AND** documentation-only or already-verified inputs skip heavy execution with an explicit reason, unknown impact conservatively runs integration, and no branch intermediate commits are falsely recorded as individually tested

### Requirement: Integration failure preserves the last passed baseline

The workflow SHALL serialize overlapping full integration execution, preserve the last successfully verified baseline on failure or interruption, and expose incomplete status. Manual execution MUST remain available when schedules are delayed or missed. A failed main result MUST NOT become successful merely because main advanced.

#### Scenario: AC-19 Failed or overlapping checks remain recoverable
- **WHEN** a scheduled/manual/release integration request overlaps an active run, or execution fails, is cancelled or lacks valid cleanup/results
- **THEN** heavy execution does not overlap, the failed input remains unverified, the prior passed baseline is retained, and status remains visible
- **AND** manual validation can check a repair without waiting for the next day, and a queued release verifies its own fixed input or reuses equivalent successful evidence rather than inheriting another revision's result
