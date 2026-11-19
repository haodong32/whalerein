# Linux integration tests

The repository runner builds selected product artifacts with pinned Linux tools
and runs each pytest shard in a disposable Docker container. Product processes and
fixtures share its loopback network. The container receives no Docker socket
and publishes no host ports. Use a native Linux host with uv and Docker BuildKit.
See the [testing policy](../../docs/testing.md).

## Run

Select the smallest target that exercises the changed behavior. Use
[staged validation](../../docs/testing.md#choose-checks-for-a-change) when
expanding scope or repeating a run.

```bash
./repo test protocol-vectors --only observation_artifact
./repo test protocol-http
./repo test storage --parallel 2
./repo test chain --only test_send
./repo test native-dsh --only test_native_browser
./repo test protocol
./repo test protocol-vectors --list
./repo test integration --integration-timeout 10800
```

The [Linux target catalog](../../tools/repo/src/repo/integration_targets.py)
assigns every test module to one leaf and defines the product artifacts it needs.
Pytest receives only the selected collection paths. `protocol` covers both vector
and live HTTP cases. `protocol-vectors` does not start product services.
`integration` covers every leaf. A case filter applies within a specific leaf
after collection. An unmatched filter stops before product services start.

The storage target compiles tagged Go test binaries and publishes the native
case inventories and authored descriptions before importing its Python module.
Tests already present in the unit binaries are excluded from the inventory.
The Rust MQTT receiver is an example executable used by a Go integration case.
Node's large-message regression is executed by the transport target.

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
./repo test chain
WHALEREIN_INTEGRATION_SHARED_BROKER=0 ./repo test chain  # per-case broker
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

`native-dsh` uses the Linux runner with the native tools and browser image. It runs the actual pinned
DSH `0.2.2-rc.1`, the real WhaleRein connector, CLI/Core and Access, with Playwright
Chromium for settings and browser ownership checks. Backend and browser cases are
one group; `--only` selects individual journeys. `--list` discovers them without
starting DSH, Chromium, PostgreSQL or the broker.

Each native case gives the CLI its own explicit default TOML configuration;
the DSH connector reads only its profile and never initializes the CLI file.
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

`protocol-vectors` covers protocol compatibility checks. Existing fixed fixtures
remain in `tests/fixtures`; their Node/Rust drivers are in `drivers/noise` and
`drivers/protobuf`. Each implementation/vector is a separate pytest case, using
its stable `caseId`. Descriptor and field-contract checks are separate cases.
Current vectors cover Rust/Node consistency; Go protobuf and historical-version
interoperability remain additional coverage work, not claimed existing behavior.

Regenerate intentional protocol changes with `./repo proto` and the relevant
Node driver's `--write` option, review the fixture/schema diff, and rerun the
`protocol` target. Fixed private keys are public fixture data and must never be used
outside tests.

## Parallel shards

`./repo test integration --parallel N` accepts 1–8 (default 1) and cannot
combine with `--list`. One platform image and one set of published artifacts
serve the selected suite. Each shard runs with its own export directory
(`container/0` through `container/N-1`) with `--shard i/N`. Collection applies
the target paths and `--only` first, then partitions the remaining cases by
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
passes only when every shard exited successfully and reported no errors. Startup and
cleanup failures remain execution errors rather than test cases. Required
skipped cases also fail the integration gate.

For output paths, JSON/HTML schemas, offline rendering and cancellation behavior,
see [report contracts](../../tools/repo/README.md#coverage-and-report-facts).
Runtime credentials, process logs and captured product payloads stay in private
fixture state and are not exported. The report records the expected image identity, case inventory, and verified cleanup.

Tooling regressions run with `./repo test repo`. Separate invocations
own distinct containers and output directories. Release-only behaviors still
require their actual environments; see the testing policy.

## Case descriptions

Write the condition and expected outcome in English beside the assertions. Use
`pytest.mark.description` on each parameter when the decision, process, language,
or vector changes. The reporter prefers that parameter description over a shared
docstring, and keeps the technical node ID in Details. Missing/shared descriptions
fail collection. Native Go scenarios use their adjacent source comments; the
builder exports those comments alongside its compiled case inventory.

## Build cache

The default run deadline is 7200 seconds. Use `--integration-timeout` to
set a different deadline. The report retains completed case observations if
execution is interrupted or loses the final session export. The runner gives
owned container cleanup a separate deadline.

The platform images contain pinned Linux tools, stable fixtures, and locked
Node dependencies. The build reads a frozen source tree. A per-worktree build
lock protects the native Go and Cargo caches and stable compiler paths. Compiler
outputs and required native inventories are published into the run directory
and included in its artifact manifest. Each shard mounts the source and artifacts
read-only and keeps mutable runtime state in its own temporary space.

The launcher uses BuildKit directly and reuses local Docker image tags
identified by the platform context's literal file allowlist, target and CPU
architecture. It verifies the input label and records the loaded image ID.
Changes to product and test source do not change platform tags. Update
`Dockerfile.dockerignore` when the platform requires a new input file.

Inspect disk use with `docker system df`
and `docker buildx du`. The launcher does not prune user images, containers,
or caches.
