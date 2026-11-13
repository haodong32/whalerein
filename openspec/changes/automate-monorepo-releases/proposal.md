# Proposal

## Why

WhaleRein needs a repeatable development and release workflow that does not depend on developers manually synchronizing versions, replacing local dependencies, or remembering publication order. Upcoming Android and iOS consumers make Core an independently versioned SDK rather than a private part of the CLI.

## What Changes

- Use `main` with short-lived feature branches; one feature MR may modify multiple components. Create maintenance branches only when an older release needs support.
- Independently version Core, CLI, Access, and the DSH group. Version the DSH plugin and shared Host together. Reserve independent Android/iOS release units for their actual introduction; do not create placeholder apps in this change.
- Preserve Rust path dependencies with version constraints and pnpm `workspace:*` during development and in merged source. Automate version, dependency and lockfile updates in release MRs.
- On ordinary commits and merges, run fast CI (static checks, unit tests/coverage and applicable builds) and retain downloadable build outputs in GitHub Actions Artifacts. Do not automatically run full integration for every push or publish snapshot packages, tags or GitHub Releases.
- Add a daily main integration sweep scheduled for 03:17 Asia/Shanghai, with manual dispatch. Run only when relevant inputs have not already passed equivalent full integration, freeze one source revision, serialize integration execution and never advance the passed baseline on failure.
- Automatically accumulate unreleased changes. An explicit component/channel selection creates a release MR; subsequent updates retain that selection and require fresh validation. Only merging that release MR authorizes its publication workflow.
- Offer only test releases (`x.y.z-rc.N`, GitHub Pre-release) and stable releases (`x.y.z`, regular GitHub Release). Test releases are optional. Do not introduce alpha, beta or snapshot release channels.
- Publish validated packages and binaries through configured destinations, enforce dependency order, and produce immutable checksums and a component-combination manifest. Test npm packages use `next`; stable npm packages use `latest` only after the required publication set succeeds.
- Enforce affected-consumer validation, isolated package installation, migration limitations, and resumable failure handling through `./repo` and CI.
- Make CI behavior locally debuggable: keep release decisions and selectors in `./repo`, statically lint workflow definitions, use a local Actions runner for compatible jobs, and reserve real GitHub runs for event, permission, artifact and release integration.
- After a test/stable release MR merges, build its fixed-revision candidate artifacts, require full integration evidence and verify the actual distribution artifacts before publication. Do not wait for the daily schedule; reuse existing integration evidence only for identical verified inputs and scope, and never let a source-built integration image substitute for final package acceptance.

## Capabilities

### New Capabilities

- `component-versioning`: Independent release units, local workspace dependencies, automated version propagation and source provenance.
- `release-lifecycle`: Ordinary builds, daily/manual integration scheduling, explicit release-MR selection, test/stable publication, concurrency and failure recovery.
- `release-verification`: Fast versus full validation timing, evidence reuse, packed-product acceptance, compatibility combinations, migration constraints and evidence.

### Modified Capabilities

None. `openspec list --specs` currently reports no main specs.

## Impact

### Observed repository state

- `Cargo.toml`, `core/Cargo.toml` and `apps/cli/Cargo.toml` currently share a workspace version; the Core workspace dependency is path-only.
- `host/dsh/package.json` depends on `whalerein-host` via `workspace:*`; both packages currently declare `0.1.0`. `pnpm-workspace.yaml` includes `host/*` and the private root pins pnpm.
- `tools/repo/projects.toml` is the existing project inventory. `tools/repo/src/repo/projects.py` derives available operations from native manifests. No release commands or tracked GitHub workflows were found.
- Only `apps/cli` currently exists under tracked application paths; Android/iOS implementation and their build contracts are future work.
- `docs/testing.md` defines unit, isolated integration including pinned native DSH/browser, and target-environment release validation. Node manifests do not currently expose lint/build scripts.
- The current integration runner builds its own Docker image from source. Its success is not proof that separately packed release artifacts install correctly. Existing guidance requires full integration for cross-component/recovery/release work; the newly agreed CI schedule changes automatic timing, not test scope or local verification obligations. Reconcile maintained timing guidance when implemented.
- `access/internal/store/store.go` rejects a database containing migrations unknown to the binary; direct binary downgrade after a new migration cannot be assumed safe.

### Intended touchpoints

Root and component manifests/lockfiles, `tools/repo` CLI and tests, release metadata referencing existing project IDs, new `.github/workflows/`, Access version reporting/image metadata, native package/binary packaging, and verified developer/operator documentation.

Preserve [architecture](../../../docs/architecture.md), coverage/evidence rules in the [testing policy](../../../docs/testing.md), [commit rules](../../../docs/commits.md) and [deployment recovery](../../../deploy/README.md). Update verified CI timing guidance consistently during implementation. Release orchestration does not change product protocol or application ownership boundaries.

### Exclusions and unresolved implementation choices

No application feature implementation, mobile app scaffolding, public compatibility promises without tests, automatic production deployment, scheduled mandatory releases, or mandatory SDD-tool dependency. Recording this proposal does not authorize registry writes, repository setting changes or application-store submissions.

The final version engine, destination namespaces/credentials, supported release platform matrix and mobile packaging adapters remain explicit implementation prerequisites (D-05 through D-07 in the design). Existing conversation decisions establish behavior, not approval of a specific third-party engine or unverified platform support.
