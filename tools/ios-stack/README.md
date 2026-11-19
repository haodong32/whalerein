# iOS loopback stack

A disposable, loopback-only WhaleRein stack for the iOS Simulator Integration
test plan. It starts the real software on one macOS host and prints a JSON
contract that an XCTest can read.

```
iOS Simulator / XCTest
  │  pair invitation, HTTP + MQTT over 127.0.0.1
  ▼
WhaleRein Access (Go, built from this worktree) ── PostgreSQL 18 (Docker)
  │  MQTT 5 + private $CONTROL
  ▼
Mosquitto 2.1.2 + repo broker plugin (Docker)
  │
  ▼
Host plugin runtime + FakeDSH fixture (Node.js, repo fixtures)
```

- Access is the stock server from `access/cmd/server`. It is built from the
  current worktree on every fresh `up`. Access itself has no SecretStore
  abstraction; the file SecretStore in the contract belongs to the repository
  CLI. The iOS app continues to use its Keychain-backed SecretStore.
- PostgreSQL runs from the pinned deployment image in a disposable container with a per-run database. A fresh
  `up` creates a new container and database; an idempotent `up` reuses the
  current one; `down` removes the container, which removes the database. Nothing
  is written to a host volume.
- Mosquitto runs the pinned `whalerein-mosquitto:local` image (Mosquitto 2.1.2
  with the repository plugin and loop-tick patch). The image is built from
  `deploy/docker/mosquitto.Dockerfile` if it is missing.
- The Host is the repository `tests/integration/fixtures/plugin.mjs` runtime
  talking to `tests/integration/fixtures/dsh.mjs` (FakeDSH over a private Unix
  socket). No file in `tests/integration/` is modified.
- Ports are chosen from free loopback ports at `up`; they are stable for the
  lifetime of the run and appear in the contract.

The stack is macOS-only in practice because it runs native Node/Go processes and
binds the broker callback to `host.docker.internal`. It does not provide TLS, a
physical-device path, or a production deployment.

## Quick start

Run from the repository root:

```bash
cargo build -p whalerein-cli          # the client used for end-to-end checks
python3 tools/ios-stack/ios_stack.py up
python3 tools/ios-stack/ios_stack.py status --json
python3 tools/ios-stack/ios_stack.py invitation --refresh
python3 tools/ios-stack/ios_stack.py seed-event stream
python3 tools/ios-stack/ios_stack.py down

# Equivalent without relying on the active virtual environment:
uv run --no-project tools/ios-stack/ios_stack.py up
```

The tool uses only the Python standard library, so system `python3` works too.

## Validation

`validate.py` starts a fresh stack, pairs the repository CLI through the real
Access/FakeDSH path, exercises history sync, idle/steer/queue sends, approval
resolution, stop, Access/broker recovery, and verified teardown, then writes
redacted evidence under `.local-docs/acceptance/add-ios-client/`:

```bash
cargo build -p whalerein-cli
python3 tools/ios-stack/validate.py
```

The report is `ios-stack-validation.md`; the raw command/output log is
`ios-stack-cli-output.log`. Neither contains invitation tokens, device tokens,
message text, digests, or control tokens.

`up` prints the JSON contract on stdout and progress on stderr. It is
idempotent: while the stack is healthy it prints the existing contract again.
Use `up --fresh` to replace a running stack. `down` stops the processes, removes
only the two labelled `whalerein-ios-stack-*` containers for this worktree, verifies the result,
and removes `.data/ios-stack/current`.

The stack emits the file path for a selected Standalone CLI instance.

```bash
STATE=.data/ios-stack/current/state.json
CLIENT_CONFIG="$(jq -r .cli.config "$STATE")"
python3 tools/ios-stack/ios_stack.py invitation --refresh \
  | target/debug/whalerein --config "$CLIENT_CONFIG" --yes host connect --invite-stdin
target/debug/whalerein --config "$CLIENT_CONFIG" --json host list
target/debug/whalerein --config "$CLIENT_CONFIG" --json session list
```

## State contract

All runtime state is under `.data/ios-stack/`, which is Git-ignored.

| Path | Contents |
|---|---|
| `current/state.json` | JSON contract, mode 0600 |
| `current/contract.env` | Same values as shell variables, mode 0600 |
| `current/invitation.txt` | Current invitation URI, mode 0600 |
| `current/secrets/` | Access/admin/broker/callback/control tokens, mode 0600 |
| `current/access.yaml`, `current/mosquitto.conf` | Generated per-run configuration |
| `current/logs` (`supervisor.log`, `access.log`, `dsh.log`, `plugin.log`) | Diagnostics, mode 0600; no secrets or payload contents |
| `current/client.toml`, `current/client/` | Selected CLI config and private instance data |
| `current/control.sock` | Supervisor control socket for the host CLI |
| `current/run.marker` | `teardown_marker` file; gone after a verified `down` |

`state.json` fields used by tests:

| Field | Meaning |
|---|---|
| `schema` | `whalerein/ios-stack/v1` |
| `ready` | Stack reached readiness when the contract was written |
| `access_url`, `mqtt_url` | Loopback endpoints |
| `invitation_uri` | Current pairing invitation (one-time token) |
| `invitation_expires_at_unix_ms` | Invitation expiry |
| `host_id`, `session_id` | Fixture Host and pre-existing session |
| `expected_transcript` | Ordered text of the seeded history (`fixture output` twice) |
| `expected_transcript_detail` | Same entries with `role` and `source` |
| `expected_message_count` | Initial committed message count |
| `stream_expectation` | Preview `x`, final `xy`, stream id `fixture-attempt` |
| `control_url`, `control_token` | Loopback fixture-control HTTP API for XCTest |
| `teardown_marker` | Run identity; also available as `current/run.marker` |
| `cleanup_command` | Absolute `down` command |
| `cli.data_dir`, `cli.secret_backend` | Ready CLI profile |
| `containers`, `ports` | Exact owned container names and allocated ports |
| `known_limitations` | Machine-readable limitation list |

`contract.env` exports:

```
WHALEREIN_IOS_STACK_STATE
WHALEREIN_IOS_STACK_ACCESS_URL
WHALEREIN_IOS_STACK_MQTT_URL
WHALEREIN_IOS_STACK_INVITATION_URI
WHALEREIN_IOS_STACK_CONTROL_URL
WHALEREIN_IOS_STACK_CONTROL_TOKEN
WHALEREIN_IOS_STACK_HOST_ID
WHALEREIN_IOS_STACK_SESSION_ID
WHALEREIN_IOS_STACK_TEARDOWN_MARKER
WHALEREIN_IOS_STACK_STATE_DIR
```

The invitation and control token are credentials. Do not print them in test
logs, paste them into bug reports, or commit the state directory. The state
files are mode 0600; keep the directory user-owned.

## Commands

| Command | Behavior |
|---|---|
| `up [--fresh] [--env] [--timeout S]` | Build Access, ensure images, start PostgreSQL/broker/Access/fixture, seed history, mint an invitation, write the contract. Prints JSON by default or the shell env contract with `--env`. Bounded waits; a failure prints a code and cleans up containers/processes it started. |
| `status [--json]` | Supervisor process/container/fixture readiness. Exit 1 when not ready. Never prints the invitation or tokens. |
| `invitation [--json] [--refresh]` | Print the current invitation, or mint a fresh one that the Host is online for. |
| `seed-event KIND` | Drive FakeDSH: `output` appends a committed message, `stream` emits preview `x`, holds `WHALEREIN_IOS_STACK_PREVIEW_PAUSE_MS` (default 600 ms), then commits `xy`, `preview` emits the preview `x` only, `final` commits `xy`, `approval` creates a pending approval and marks the session running, `running`/`idle` change runtime state. |
| `restart access\|broker` | Recovery check: restart that component and wait for readiness. |
| `down` | Send supervisor shutdown, bound-wait, remove only labelled containers, verify no owned processes/containers remain, then remove `current/`. Exit 1 and keep evidence if verification fails. |

All commands accept `--state <state.json|run-dir>` after the subcommand. The
default is `.data/ios-stack/current`.

Every readiness wait is bounded (90 s for containers/Access, 45 s for the Host
plugin, 300 s for a full `up` by default). Timeouts fail with an explicit code
instead of hanging.

## Pairing and operating a session

The fixture exposes one existing session with two committed assistant messages.
A test normally:

1. Reads the contract.
2. Calls `invitation --refresh` (the previous invitation may already be used).
3. Pairs through the app/Core with the invitation and confirms the Host
   fingerprint.
4. Lists Hosts and Sessions, then syncs the initial history and compares it to
   `expected_transcript`.
5. Drives the fixture for the scenario, performs App operations, and checks the
   resulting cache, approvals, and runtime state.

Invitations are single-use and expire after 30 minutes. The contract's
invitation is fresh at `up`, but a test that runs after another pairing must
request a new one.

The fixture session is `idle` with history after `up`. `seed-event approval`
sets it running and creates a pending approval. `seed-event stream` emits a
provisional preview followed by a committed `xy` message. `seed-event output`
appends another committed `fixture output` message. `seed-event running` lets a
test exercise steer/queue delivery; `seed-event idle` clears runtime state.

The CLI validates the same path:

```bash
whalerein session send "$SESSION" --detach --delivery start-if-idle "..."  # idle
whalerein session send "$SESSION" --detach --delivery steer "..."          # running
whalerein session send "$SESSION" --detach --delivery enqueue-after-run "..."
whalerein session approval list --session "$SESSION"
whalerein session approval approve --request ID --digest HEX --version N
whalerein session interrupt "$SESSION"
```

`start-if-idle` is a sampled check, not an atomic operation; while the fixture
is running it can return `conflict`. Use steer or enqueue-after-run there.

## XCTest Integration plan

### Environment

The contract is a host-side file, so use it in two places:

- Host-side scripts (Xcode pre/post actions, a wrapper around `xcodebuild`) read
  `WHALEREIN_IOS_STACK_STATE=/abs/path/to/repo/.data/ios-stack/current/state.json`
  and `contract.env`.
- The Simulator test runner receives the values it needs as environment
  variables injected by the test plan or a generated `.xctestrun`/wrapper:

```
WHALEREIN_IOS_STACK_ACCESS_URL=http://127.0.0.1:<access-port>
WHALEREIN_IOS_STACK_MQTT_URL=mqtt://127.0.0.1:<mqtt-port>
WHALEREIN_IOS_STACK_INVITATION_URI=<fresh invitation>
WHALEREIN_IOS_STACK_HOST_ID=<host-id>
WHALEREIN_IOS_STACK_SESSION_ID=<session-id>
WHALEREIN_IOS_STACK_CONTROL_URL=http://127.0.0.1:<control-port>
WHALEREIN_IOS_STACK_CONTROL_TOKEN=<control-token>
```

`WHALEREIN_IOS_STACK_STATE` is for the host side, not for a direct
`Data(contentsOf:)` read from the Simulator: the Simulator process is sandboxed
and must not be assumed to read arbitrary `/Users/...` host paths. Use a
host-side wrapper that reads the contract and exports/injects these variables
(for example by generating an `.xctestrun` from `xcodebuild
build-for-testing` and setting each test target's `EnvironmentVariables`).
The control URL and token change on every fresh run and must be injected, not
hard-coded in the test plan.

In the test code, read them from the process environment:

```swift
let environment = ProcessInfo.processInfo.environment
guard let controlURL = environment["WHALEREIN_IOS_STACK_CONTROL_URL"],
      let controlToken = environment["WHALEREIN_IOS_STACK_CONTROL_TOKEN"],
      let sessionID = environment["WHALEREIN_IOS_STACK_SESSION_ID"] else {
    throw XCTSkip("start tools/ios-stack and inject the contract")
}
```

If the App under test needs the invitation or session identifiers, pass them
through `XCUIApplication().launchEnvironment` as well; do not hard-code a
one-time invitation.

### Lifecycle

Use an Xcode scheme pre-action / test-plan setup script (or a wrapper around
`xcodebuild test`) to start the stack, and a post-action to stop it:

```bash
#!/bin/sh
set -eu
ROOT="$(git -C "$SRCROOT" rev-parse --show-toplevel)"
python3 "$ROOT/tools/ios-stack/ios_stack.py" up --fresh
```

Post-action:

```bash
#!/bin/sh
set -eu
ROOT="$(git -C "$SRCROOT" rev-parse --show-toplevel)"
python3 "$ROOT/tools/ios-stack/ios_stack.py" down || true
```

`up` prints the contract to stdout. The pre-action cannot inject arbitrary
environment variables into the test process, so also set
`WHALEREIN_IOS_STACK_STATE` statically in the test plan (absolute path) or use a
wrapper that reads the contract and exports the `WHALEREIN_IOS_STACK_*`
variables before invoking `xcodebuild test`.

Tests run on the Simulator share the host network stack. `127.0.0.1` in the
simulator reaches the Mac's loopback services, including the stack's JSON
control API. A physical device does not; see limitations.

### Driving fixture events from XCTest

The host CLI is not callable from the Simulator. Use the loopback control API
with `URLSession` (the test reads the URL and token from its environment):

```swift
func seedEvent(_ kind: String) async throws {
    var request = URLRequest(
        url: URL(string: controlURL + "/v1/seed-event")!
    )
    request.httpMethod = "POST"
    request.setValue("Bearer \(controlToken)", forHTTPHeaderField: "Authorization")
    request.setValue("application/json", forHTTPHeaderField: "Content-Type")
    request.httpBody = try JSONEncoder().encode(["kind": kind])
    let (data, response) = try await URLSession.shared.data(for: request)
    // Decode { "ok": true, "data": ... } or fail the test.
}
```

`GET /v1/status`, `GET /v1/invitation[?refresh=1]`, `POST /v1/seed-event`,
`POST /v1/restart` and `POST /v1/broker` use the same bearer token. `down` is
intentionally not exposed over HTTP. The control API is bound to `127.0.0.1`
only and is meant for the disposable test stack.

`POST /v1/broker` takes `{"action":"stop"}` or `{"action":"start"}`. Unlike
`POST /v1/restart {"target":"broker"}`, which stops and restarts in one blocking
call, `stop` leaves Mosquitto down so a test can observe the MQTT failure while
HTTPS remains reachable, inspect redacted diagnostics, and then restore the
broker explicitly.

For a stream timing test, call `seed-event stream` from the test process, then
observe preview and final events through the App/Core. For approvals, call
`seed-event approval`, list approvals through the App, and resolve with the
digest/version the Host returns.

### ATS and entitlements

Access and the control API are plain HTTP on loopback; MQTT is plain TCP on
loopback. ATS only governs `URLSession` HTTP loads. Add a local exception to the
App/Test host Info.plist:

```xml
<key>NSAppTransportSecurity</key>
<dict>
  <key>NSAllowsLocalNetworking</key>
  <true/>
</dict>
```

Do not add `NSAllowsArbitraryLoads` for this stack. No iOS network entitlement
is required for outbound connections. Loopback does not need
`NSLocalNetworkUsageDescription` or Bonjour; those apply to local-network
discovery, not `127.0.0.1`. If the App uses a custom socket for MQTT, ATS does
not apply, but the plaintext limitation still does.

### Isolation and cleanup

- One stack per test run. Use `up --fresh`; do not start two stacks (container
  names and the default state directory are fixed).
- Mint a fresh invitation per pairing test with `invitation --refresh`.
- Tests that share the one fixture session must run serially, or they must
  expect each other's messages in history. The stack does not create a session
  per test.
- Restart tests use `restart access` / `restart broker`; pairing credentials
  survive in PostgreSQL and the Host state directory.
- Cleanup is verified: `down` exits nonzero and keeps `current/` if a container
  or owned process remains. It never removes unrelated Docker containers or
  images.
- `down` is safe to call repeatedly and after a crash (it falls back to the
  recorded state and command-line markers).

## Security notes

- Secrets are generated per run, stored under `current/secrets/` with mode 0600,
  and passed to child processes through their environment (not argv/logs).
- `state.json`, `contract.env`, and `invitation.txt` are 0600.
- The Mosquitto container receives the Access/broker credentials through its
  entrypoint environment, matching the repository Compose stack. They are
  visible through `docker inspect` for that disposable container; `down`
  removes it.
- Logs contain readiness events and error codes, never invitation tokens,
  device tokens, message text, or control tokens.
- The loopback control endpoint uses a bearer token and must not be exposed
  beyond loopback.

## Known limitations

- Plain HTTP and plain MQTT on loopback only. There is no self-signed TLS option
  in this tool; production TLS behavior remains a separate release concern.
- Requires a reachable Docker engine with host-gateway/file-sharing support. The
  broker image is the pinned Mosquitto 2.1.2 + plugin build; a host Homebrew
  Mosquitto is not used.
- Access is built from the current worktree, including uncommitted changes. The
  tool does not build the repo CLI; build `whalerein-cli` separately.
- FakeDSH does not certify real DSH persistence, credentials, approval races,
  or reload behavior. It is the same fixture used by the Linux integration
  suite, driven standalone.
- One active stack at a time; container names and `.data/ios-stack/current` are
  fixed.
- No physical-device path: a device's `127.0.0.1` is itself, not this host.
- No multi-Host, external-account, or production deployment validation.
- The control API exposes fixture state changes, not arbitrary DSH operations.
- Tests that expect exact history must account for messages produced by earlier
  sends; `expected_transcript` describes the initial seeded history.

## Troubleshooting

- `status --json` reports process/container/fixture health and no secrets.
- `current/supervisor.log` has bounded readiness and cleanup events;
  `access.log`, `plugin.log`, and `dsh.log` have component diagnostics.
- `up` failure codes include `docker_unavailable`, `postgres_failed`,
  `broker_failed`, `access_failed`, `host_failed`, `invitation_failed`, and
  `timeout`. The stack cleans up what it started before returning.
- If a run is interrupted, `down` uses the recorded state and command markers;
  if the state directory is gone, it still removes only containers carrying the
  `com.whalerein.ios-stack=1` label.
- Docker mount errors usually mean the Docker engine cannot see the repository
  path. Keep the repository under a shared path such as `/Users`.
