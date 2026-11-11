# Linux integration tests

All cases, pytest, product processes, PostgreSQL 18, and EMQX 5.8.8 run in one
Docker container. The host Python/uv runner uses Docker buildx to build and start
the container, collect its export and verify cleanup. The container receives no
Docker socket and publishes no host ports; product builds do not use host native
compilers or development services. See the [testing policy](../../docs/testing.md).

## Run

The full container suite is slow and runs only when explicitly selected.
Routine iteration should use focused unit tests.

```bash
./repo test --suite integration
./repo test --suite integration --group protocol
./repo test --suite integration --group storage
./repo test --suite integration --only test_send
./repo test --suite integration --list
```

The launcher always checks the image build; unchanged layers are cached. Native
executables and language dependencies are prepared during the image build.
The host Python runner requires uv and Docker buildx. It owns image preparation,
container cancellation and verified cleanup. Pytest discovery, selection and all
fixture code execute inside the container. Protocol-only selection starts no PostgreSQL or broker.

Pytest discovers `cases/test_*.py`. Group markers are filters, not another catalog.
Native PostgreSQL case IDs are generated from compiled Go test binaries during
image construction, excluding tests already selected by the unit build.
The Rust MQTT receiver is an example executable used by its Go integration driver,
not an additional unit case. Go integration assertions use the `integration`
build tag and are invoked individually by pytest. Node's large-message regression
also lives here rather than in the package unit glob.

## Isolation and fixtures

PostgreSQL is acquired once per run when required. Each chain case owns a schema,
Access, a broker, plugin state, a file SecretStore, and a separate FakeDSH process.
Broker instances are case-scoped so authenticated and transport-only configurations
cannot leak into each other. No case depends on a preceding case's pairing.
The initial runner is sequential; separate containers provide parallel isolation.

`scenario` supplies an unpaired environment without sessions or approvals.
`session_id` creates a unique session; `pending_approval` explicitly prepares a
running session and pending approval. `paired_device` pairs through the actual
CLI/Core. Its methods expose business operations such as `send`, `sessions`,
`resolve`, and `revoke`, hiding arguments, credentials, and envelopes. FakeDSH
control uses the plugin-owned DshAdapter seam over a private Unix socket. The
FakeDSH authority survives plugin restart, preserving sessions and credentials.
Recovery cases retain durable state within their own case only.

Fixtures stop owned processes and drop their schemas in `finally` blocks, including
partial setup failures. Readiness waits are bounded and detect exited children;
commands and native drivers have deadlines. Business controls are never retried
implicitly. An unavailable required facility fails setup instead of skipping.

## Protocol

Protocol compatibility is a group of integration cases. Existing fixed fixtures
remain in `tests/fixtures`; their Node/Rust drivers are in `drivers/noise` and
`drivers/protobuf`. Each implementation/vector is a separate pytest case, using
its stable `caseId`. Descriptor and field-contract checks are separate cases.
Current vectors cover Rust/Node consistency; Go protobuf and historical-version
interoperability remain additional coverage work, not claimed existing behavior.

Regenerate intentional protocol changes with `./repo proto` and the relevant
Node driver's `--write` option, review the fixture/schema diff, and rerun the
protocol group. Fixed private keys are public fixture data and must never be used
outside tests.

## Results

The container writes `container/integration.json`, a version 1 scope export.
The host validates it and creates the final run report, or merges it with unit
results for `--suite all`. A container that exits without a valid export fails;
startup and cleanup failures remain execution errors rather than test cases.
Required skipped cases also fail the integration gate.

For output paths, JSON/HTML schemas, offline rendering and cancellation behavior,
see [report contracts](../../tools/repo/README.md#coverage-and-report-facts).
Runtime credentials, process logs and captured product payloads stay in private
fixture state and are not exported. No test containers should remain after a run.

Tooling regressions run with `./repo test --project repo`. Separate invocations
own distinct containers and output directories. Release-only behaviors still
require their actual environments; see the testing policy.

## Case descriptions

Write the condition and expected outcome in English beside the assertions. Use
`pytest.mark.description` on each parameter when the decision, process, language,
or vector changes. The reporter prefers that parameter description over a shared
docstring, and keeps the technical node ID in Details. Missing/shared descriptions
fail collection. Native Go scenarios use their adjacent source comments; the
image exports those comments alongside its compiled case inventory.

## Build cache

The launcher requires Docker buildx/BuildKit and uses one build path:
`docker buildx build --load --progress=plain --iidfile FILE`. The runner starts
the immutable ID from that file; parallel worktrees do not share a mutable tag. It still checks current sources
on every invocation; there is no skip-build or stale-image shortcut.

Rust, Go and Node are independent build stages. Rust caches its registry, git
sources and target directory; Go caches modules and compiler output; Node caches
the pnpm store. Cache mounts use locked sharing, and architecture-sensitive
caches include the target architecture. Rust executables are copied out before
the target cache is unmounted. Only runtime dependencies and exported artifacts
reach the final test image.

Go module download and Node dependency installation precede source COPY steps.
The Node stage installs the root pnpm workspace with its shared lockfile and
copies the root virtual store alongside both packages into the runtime image.
Python cases, fixtures and the shared timing helper are copied into the runtime
last.
Editing these files does not invalidate language build stages. Native Go case
descriptions are Go source comments, so editing them still invalidates the Go
stage; the compiler cache can reuse unaffected work. Protocol/schema changes
are intentionally shared inputs where applicable.

Inspect storage with `docker system df` and BuildKit cache with
`docker buildx du`. Caches consume disk space; the launcher does not prune user
images, containers, or caches automatically. No legacy-builder fallback exists.
