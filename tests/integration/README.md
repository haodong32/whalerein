# Linux integration tests

All cases, pytest, product processes, PostgreSQL 18, and Mosquitto 2.1.2 with
the WhaleRein plugin run in one Docker container. The host Python/uv runner uses Docker buildx to build and start
the container, collect its export and verify cleanup. The container receives no
Docker socket and publishes no host ports; product builds do not use host native
compilers or development services. See the [testing policy](../../docs/testing.md).

## Run

The full container suite is slow and runs only when explicitly selected.
Routine iteration should use focused unit tests.

```bash
./repo test --suite integration
./repo test --suite integration --integration-timeout 10800
./repo test --suite integration --group protocol
./repo test --suite integration --group storage
./repo test --suite integration --group storage --parallel 2
./repo test --suite integration --group native-dsh
./repo test --suite integration --group chain --parallel 3
./repo test --suite integration --group native-dsh --only test_native_browser
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
Access, plugin state, a file SecretStore, and a separate FakeDSH process. One warm
Mosquitto line is shared per container by default; transport cases temporarily
stop it and own a case-scoped broker plus the deferred CONNECT policy callback,
so those configurations cannot mix. Access's service account authenticates
locally against the plugin password file, so its startup never waits on its own
HTTP callback. No case depends on a preceding case's pairing, session, or topic
namespace.
The default runner is sequential; `--parallel N` starts N independent containers
with deterministic `--shard INDEX/COUNT` hash partitions. PostgreSQL, ports,
processes and schemas stay container-local, so per-case isolation is unchanged
and sharding excludes no case from the selected suite. With the default shared
broker, each shard container starts and reuses its own warm broker.

A warm broker is shared by default for local/CI speed:

```bash
./repo test --suite integration --group chain
WHALEREIN_INTEGRATION_SHARED_BROKER=0 ./repo test --suite integration --group chain  # per-case broker
```

It starts one Mosquitto broker per container and reuses it across cases. MQTT
clients use clean-start sessions and publications are non-retained, so no case
inherits subscriptions or retained state; the transport cases stop the warm
broker and start their case-scoped broker per case. Use the `=0` opt-out while
investigating broker-state isolation.

Access authenticates locally against the plugin password file and uses a
case-unique host/client-id namespace, so a shared broker does not couple Access
startup to its own HTTP callback and cannot reuse another case's session. A
broker that exits between cases is restarted by the fixture supervisor; its
plugin state and data directory live on the container tmpfs.

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

## Native DSH and browser journeys

`native-dsh` uses the same integration image and runner. It runs the actual pinned
DSH `0.2.2-rc.1`, the real WhaleRein connector, CLI/Core and Access, with Playwright
Chromium for settings and browser ownership checks. Backend and browser cases are
one group; `--only` selects individual journeys. `--list` discovers them without
starting DSH, Chromium, PostgreSQL or the broker.

Each case owns a fresh DSH home, working directory, native process and loopback
Messages provider. The provider returns deterministic text or a test-only native
tool that asks the real Approval service before incrementing a private counter.
This controls streaming, decisions and cancellation without a paid model account.
The native PluginManager installs the local connector bundle; native Settings
seeds configuration so browser saves exercise actual persistence. No personal DSH
profile or host DSH installation is used. Runtime networking remains `none`;
loopback services stay within the container. Image construction needs network to
install the locked DSH/browser dependencies in `native/package-lock.json`.

The cases cover pairing/revocation, large-message prefix/final consistency,
fixed-cut history during growth, cold DSH recovery and Access restart, approval
accept/reject, late resolution after cancellation, stream interruption, settings
save/reload, read-only connection drafts, browser invitations and device revoke.
Approval tests preserve claim semantics: remote ownership preempts the native UI,
including when a browser is already observing the Session. They do not introduce
simultaneous local and remote answers or establish delegated-mode behavior.

Native logs and launch credentials remain private to the disposable case. Browser
automation exports bounded stage codes only, with no traces, screenshots, raw
browser errors, model request bodies or launch URLs. Readiness and event waits are
bounded; no test automatically retries an uncertain control operation. This group
uses file SecretStore and headless Linux Chromium. Keychain, desktop shell,
production TLS, external model quality and other DSH versions remain release work.

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

## Parallel shards

`./repo test --suite integration --parallel N` accepts 1-8 (default 1). It is
valid only with the integration suite and cannot combine with `--list`. The
image is built once and each container runs its own export directory
(`container/0` through `container/N-1`) with `--shard i/N`. Collection applies
`--group`/`--only` first, then partitions the remaining cases by
`CRC32(nodeid) % N`. The union of all shards equals the full selection exactly;
an empty shard is valid when N exceeds the selected case count.

N is a resource tradeoff, not a promise of linear scaling. The acceptance VM is
4 vCPU/8 GB; two to three containers is the recommended range there. Each shard
still starts PostgreSQL, Mosquitto, DSH and browser tooling, so CPU, memory and image
build cache contention can dominate and additional shards may not reduce wall
time. Case durations are uneven, so the slowest shard bounds merged wall time.

## Results

Each container writes `container/<shard>/integration.json`, a version 1 scope
export. The host validates every export and merges them into one integration
scope: tests, errors and checks are combined, duplicate case identities are
rejected, `duration_ms` is the parallel wall time (not a sum), and completeness
is `complete` only when every shard export is valid and complete. A shard that
is missing, invalid or incomplete keeps the merged report incomplete; the run
passes only when every shard exited 0 or 5 and reported no errors. Startup and
cleanup failures remain execution errors rather than test cases. Required
skipped cases also fail the integration gate.

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

Integration execution defaults to 7200 seconds. Set `--integration-timeout`
to a positive number of seconds for a longer or shorter execution deadline;
the image build has a separate 1800-second deadline. When execution times out,
the container sends SIGINT to pytest; the host runner allows a short shutdown
grace period, retains any exported partial report, and verifies cleanup. A timeout
is a failed, incomplete run. Individual test and fixture deadlines are unchanged.

The launcher requires Docker buildx/BuildKit and uses one build path:
`docker buildx build --load --progress=plain --iidfile FILE`. The runner starts
the immutable ID from that file for every shard; parallel worktrees and
containers do not share a mutable tag. It still checks current sources
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
