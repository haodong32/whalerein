# WhaleRein for DSH (`whalerein-dsh`)

Internal design: [DSH plugin architecture](../../docs/architecture/dsh-plugin.md).

The host-side protocol endpoint for WhaleRein. It:

- opens **one outbound MQTT 5/TLS connection** and **no HTTP port**;
- does **not** link Rust or import Rust Crates;
- performs Noise XX (X25519 / ChaCha20-Poly1305 / SHA-256) pairing and secure channels;
- calls DSH only through the adapter in `src/adapter`;
- normalizes watched Messages and live Attempts, encrypts per device, and keeps only a
  bounded ciphertext outbox;
- keeps its own state in SQLite: host public key reference, peer keys, applied
  revocations, command dedupe/status/results, sequence bases, outbox, meta.

The host private key lives only in the DSH adapter credential store; nothing in
SQLite, configuration, or logs contains it, message bodies, tool arguments,
tokens, or ciphertext plaintext.

## Layout

The DSH entry, native source adapter, credentials and settings live here. Reusable
Host networking, encryption, commands, watched publication and state now live in
[`host/runtime`](../../host/runtime/README.md). The package depends on that local
Host package through `workspace:*` and shares the root `pnpm-lock.yaml`. Run
`pnpm install --frozen-lockfile` from the repository root to install both packages.
The generated descriptor is packaged with Host rather than resolved
through a checkout-only path.

```text
host/dsh/
  index.js, cordis.patch.yml    Cordis lifecycle and configuration
  client.js, locale/, icon.svg  DSH settings
  src/config.js                DSH profile configuration
  src/credentials.js           DSH Host-key credential binding
  src/adapter/                 native APIs, projection, opaque history, live frames
  src/settings.js              settings and allowlisted diagnostics
  src/pairing.js               signed Access administration
  src/connection-test.js       bounded connection tests
  test/                        connector and native-context tests
host/runtime/
  src/contracts/               normalized Source interface/validation
  src/commands/, events/       deduplication, watched updates and recovery
  src/access/, security/      transport and cryptography
  src/state/                   operational state and instance lock
  src/generated/               protobuf descriptor from proto/v1
  test/                        shared runtime tests
```

## Configuration

The running DSH profile is the only connection config. Settings → WhaleRein writes `$DSH_HOME/profiles/<profile>/cordis.patch.yml` (entry id `whalerein-dsh`). The plugin reads that document after Cordis has evaluated any `!!js` expression stored in it. It does not consult `WHALEREIN_*` environment variables on its own.

A secret may be stored in that file as `!!js process.env.WHALEREIN_HOST_TOKEN` so the value stays out of the file. The process still has to export that variable. `pnpm start` does this for the host token and the administrator token.

| Key | Default | Meaning |
|---|---|---|
| `server` | none | host name or IPv4 address. When set, the plugin derives the Access origin and the MQTT URL and ignores `mqtt_url` / `access_url` |
| `admin_token` | none | Not shown in settings. The plugin does not use it. A saved value stays in the profile. Host-signed requests mint this host’s pairing link and revoke this host’s grant. The Access admin token remains the operator credential for invitations for any host and for `DELETE /v1/devices/{id}` |
| `access_port` | `443`, or `8080` when plaintext is allowed | Access port. `443` and plaintext `80` are omitted from the URL |
| `mqtt_port` | `8883`, or `1883` when plaintext is allowed | MQTT port |
| `mqtt_url` | required when `server` is empty | legacy `mqtts://...`; `mqtt://` only when `allow_insecure_mqtt` is true |
| `host_id` | generated once from the computer name when the profile has none | topic-safe host id. A saved value stays. Clearing the settings field keeps the saved value |
| `host_token` | required | broker provisioning token (never logged) |
| `ca_file` | none | TLS CA bundle. Empty uses the system trust store |
| `state_dir` | `~/.whalerein/dsh` | Not shown in settings. SQLite directory |
| `adapter` | `auto` | Not shown in settings. Inside DSH, `auto` uses the real adapter. `fake` and `real` stay for tests and standalone runs |
| `access_url` | none | legacy Access origin, used when `server` is empty |
| `proto_path` | repo descriptor | protobuf JSON descriptor |
| `approval_mode` | `claim` | `claim` answers `approval/request`; `delegate` calls `next()` |
| `approval_ttl_ms` | `300000` | pending approval lifetime |
| `event_batch_ms` | `5` | ordinary-output batch window |
| `outbox_per_device_limit` | `100` | pending ciphertext rows per device |
| `outbox_total_limit` | `1000` | pending ciphertext rows overall |
| `snapshot_max_events` | `5000` | DSH snapshot read bound |
| `reconnect_min_ms` / `reconnect_max_ms` | `500` / `30000` | MQTT backoff |
| `shutdown_timeout_ms` | `5000` | wait for in-flight requests on stop |
| `log_level` | `info` | `debug`, `info`, `warn`, `error`, or `silent` |

`allow_insecure_mqtt` defaults to false and is not shown in settings. Turn it on in the profile only for a trusted plaintext broker.

MQTT identity is fixed by WIRE.md:

```text
client id  whalerein-host-{host_id}
username   host:{host_id}
```

The plugin subscribes only `whalerein/v1/h/{host_id}/request` and publishes only
its own `result` and `event` topics. After subscribing it publishes a
`HostResult{status=succeeded, result_body=HostReady}` readiness signal.

## One-command real-DSH debug

```bash
cd host/dsh
pnpm start
```

`pnpm start` invokes `bin/start-real-dsh.sh`, which creates the
`whalerein-debug` DSH profile if needed, installs this package with
`dsh plugin` (pnpm), and registers `whalerein-dsh` as a profile bundle before running real DSH in
the foreground on port 3099. The first launch writes local Compose defaults into
that profile's `cordis.patch.yml`. Later launches leave the file unchanged.
DSH prints its tokenized UI URL. Press Ctrl-C to stop. It only ensures the plugin
is loaded by DSH; it does **not** start or check Access/PostgreSQL/EMQX.
`WHALEREIN_DSH_PORT` selects the port. `WHALEREIN_HOST_TOKEN` and
`WHALEREIN_ADMIN_TOKEN` are exported for profile values that reference them.
The normal `web` profile on 3080 is untouched.

Loading the Host plugin as a bundle (rather than a `--patch` launch overlay)
is what lets the settings page save into the profile user layer.

## Settings page

The page reuses the host's `@deepseek-ai/dsh-client-ui-primitives` module for
buttons, inputs, disclosure rows, the log-level menu,
status dots, tags, icons, and clipboard writes. The package declares that module
in `dsh.client.external`; DSH supplies it without another React or CSS runtime.
This internal component contract was inspected in DSH `0.2.2-rc.1`. Unit tests
check composition and callbacks using opaque component handles, not DSH rendering.
After upgrading DSH, verify the page and plugin configuration view in an
authenticated browser, including light/dark themes and menu keyboard behavior.
Fields follow the General settings layout: title and subtitle on the left,
official Input or Menu on the right, stacking on narrow screens. Each control
references its subtitle for assistive technology. Token presence is shown in an
official Tag beside its title; the password input never receives the saved secret.

**Test connection** sits beside Save and validates the current draft without
writing configuration. The Host resolves hidden settings and keeps its saved
host token when the token field is empty. It checks Access `/readyz` (HTTP 200,
no redirects) and performs one MQTT 5 login using `probe:<host_id>` with a random
`whalerein-probe-<32 lowercase hex digits>` client ID. The connection uses a clean
session, zero session expiry, no Will, no retries, and closes after CONNACK. It
never subscribes, publishes, or sends HostReady. Each check has a five-second
deadline with bounded client cleanup; TLS verifies the configured CA and server
identity. Only one test runs at once, with at least ten seconds between starts
per plugin service.

Access authenticates the probe against the same host provisioning credential,
without registering a host. All probe topic operations are denied and probe
lifecycle events are excluded from presence. The probe client ID cannot replace
the normal host client ID. **Access and the DSH plugin must both be updated**;
older Access deployments reject the new identity convention.

Feedback stays to one sentence: a successful check displays “Connection test
passed”; rejected credentials and network failures receive a short actionable
message. Protocol names remain in developer diagnostics. Success verifies service
readiness and credential acceptance, not host topic permissions or message
execution. The Host explicitly marks authenticated results; older network-only
results are rejected by the new UI. Editing the draft discards previous and in-flight results. The action
footer composes official Buttons because the official SettingsForm footer is
fixed. Completing a test refreshes live status; a failed readiness or draft check
does not itself mark an established host connection offline.

The same `whalerein-dsh` package adds **Settings → WhaleRein** to the DSH Web
UI through its Client half. The connection form asks for a server host, a host
ID, and a host token. Ports, the CA bundle path, and the log level live under
Advanced. Empty ports use 443 and 8883, or 8080 and 1883 when plaintext
is allowed. Saving a server clears any saved `mqtt_url` and `access_url` so
the derived endpoints win.

**Generate pairing link** asks Access for one `whalerein://pair?...` URI. The
page does not mint one on open. The connection card places status and its action
in the header, followed by the grouped host fingerprint while connected. A
generated URI appears in a separate block below, with its expiry hint and copy
action. The fingerprint is for the client's confirm step. A separate list shows device names and IDs, OS
labels when known, pairing times, and live channel status.

`snapshot` stays free of tokens. `invite` returns the pairing URI, its expiry,
and the fingerprint. The URI embeds the one-time invitation token. The plugin
does not log that URI or the invitation token, and it does not return the token
as its own field. Pairing and revocation use host-signed Access requests.

The page edits this plugin's `Config` section through DSH `configForms`: saved
fields override the profile/environment values, and clearing a field inherits
the environment/default again. The host token is write-only: its row shows
whether it is configured and accepts a replacement; leaving it empty
preserves the saved token. The administrator token is not exposed by the page.
The plugin must be loaded as a profile
bundle (not a launch `--patch` overlay) for the user layer to win; `pnpm start`
does that. **Runtime diagnostics** appears only while DSH **Developer tools** is
enabled. It shows the running host ID, DSH/identity readiness, revocation-sync
state, MQTT reconnect/publish-failure counters, pending ciphertext count, commands
with unknown outcomes, and sessions requiring resync. MQTT counters cover the
current host run; the queue and recovery counts describe current state.
Unavailable values render as `—`. The snapshot explicitly selects these fields
from the Host diagnostic summary; it never forwards raw adapter summaries.

## Install into a DSH profile (debug)

Use a **dedicated profile**, not the running `web` profile:

```bash
# Create a copy of the shipped web profile.
dsh --profile whalerein-debug --from-default-profile web --dump-config

# Install this package as a profile dependency.
dsh plugin --profile whalerein-debug add "$PWD/host/dsh"
```

The package declares its bundle patch in `package.json`:

```json
"dsh": { "bundle": { "patch": "./cordis.patch.yml" } }
```

Add the bundle to the profile's `dsh.profile.bundles` list in
`$DSH_HOME/profiles/whalerein-debug/package.json`:

```json
"dsh": {
  "profile": {
    "bundles": [
      "@deepseek-ai/dsh-base",
      "@deepseek-ai/dsh-web-app",
      "whalerein-dsh"
    ]
  }
}
```

Then verify the profile document:

```bash
dsh --profile whalerein-debug --dump-config | grep -A20 whalerein-dsh
```

Start that profile on a separate port (the normal Web UI may already use 3080).
Export a token variable only when the profile value is a `!!js process.env` reference:

```bash
WHALEREIN_HOST_TOKEN=dev-host-token \
dsh --profile whalerein-debug web --port 3099 --no-open
```

`WHALEREIN_ADAPTER=fake` is useful to isolate protocol/plugin bugs from DSH API
bugs; `real` exercises `src/adapter/real.js`. The real DSH APIs are internal and
were only verified against fake contexts, so treat adapter failures as the
primary debugging target.

## Local run with FakeDSH

### In-process harness (no broker, no DSH)

```bash
pnpm install --frozen-lockfile
./repo test --project dsh
```

The repository-only harness composes FakeDSH with the shared Host test harness:
an in-process MQTT broker, simulated Access and device wire clients. Test support
is excluded from the published package:

```js
import { startHarness } from './test/support/harness.js'

const harness = await startHarness({
  sessions: [{ sessionId: 's1', title: 'Demo', cwd: '/tmp', running: false }],
  behavior: { approvalOnFirstMessage: true, longRunning: false },
  config: { reconnect_min_ms: 50, reconnect_max_ms: 100 },
})
try {
  const device = await harness.newPairedDevice('device-1')
  const list = await device.query({ list_sessions: {} })
  const sent = await device.command(
    { send_message: { session_id: 's1', text: 'hello' } },
    { commandId: 'cmd-1' },
  )
  const events = await device.events()
  console.log(list.status, sent.status, events.length)
} finally {
  await harness.stop()
}
```

`startHarness()` returns `{ host, adapter, broker, access, codec, config,
stateDir, tokenHash, createDevice, pairDevice, newPairedDevice, stop,
diagnostics }`. This is the integration mode for `tests/integration`: it runs
the real plugin code (security, commands, events, SQLite, MQTT client) with
FakeDSH and no external services.

### `pnpm dev` (watch mode)

```bash
pnpm install --frozen-lockfile
pnpm --filter whalerein-dsh dev
```

`pnpm dev` runs `node --watch bin/dev.js`: it starts the standalone FakeDSH host
with local Compose-stack defaults, checks Access `/readyz`, and restarts on
source changes. This is restart-on-change, not Vite-style browser HMR; the
in-memory secure channels are recreated on each restart while SQLite state
survives. Real DSH debugging still requires a DSH profile, not this command.

### Standalone process with FakeDSH + real Access/broker

```bash
WHALEREIN_MQTT_URL=mqtts://access.example:8883 \
WHALEREIN_HOST_ID=host-1 \
WHALEREIN_HOST_TOKEN=... \
WHALEREIN_CA_FILE=./ca.pem \
WHALEREIN_STATE_DIR=./.state \
WHALEREIN_FAKE_SEED=./seed.json \
node bin/run-fake.js
# Local EMQX without TLS (test only):
# WHALEREIN_MQTT_URL=mqtt://127.0.0.1:1883 WHALEREIN_ALLOW_INSECURE_MQTT=1 ...
```

`seed.json`:

```json
{
  "sessions": [
    { "sessionId": "s1", "title": "Demo", "cwd": "/tmp", "running": false }
  ],
  "behavior": {
    "approvalOnFirstMessage": true,
    "longRunning": false,
    "emitOutputOnMessage": true,
    "approvalTtlMs": 300000
  }
}
```

Env flags: `WHALEREIN_FAKE_APPROVAL_ON_FIRST_MESSAGE=1`,
`WHALEREIN_FAKE_LONG_RUNNING=1`, `WHALEREIN_FAKE_OUTPUT_ON_MESSAGE=0`.
The process prints one JSON line after starting; it has no HTTP surface and
stops on SIGINT/SIGTERM.

The in-process unit/component suites never require a real broker. EMQX against
a live Access is only exercised by `tests/integration` and the standalone
process above.

## Behaviour decisions

- **Channel id**: the client-chosen random `HostRequest.channel_id` is the
  secure channel id. It is authenticated through the Noise prologue; no second
  id is derived from the handshake hash.
- **Request AAD**: `{kind}` in the AAD is the proto enum name
  (`REQUEST_KIND_QUERY` / `REQUEST_KIND_COMMAND`). Result ciphertext uses the
  same request AAD as its request; WIRE.md does not define a separate result
  AAD.
- **Frames**: `EncryptedFrame` with 12-byte random nonce and
  ChaCha20-Poly1305; wrong channel, MAC, or AAD is rejected without side
  effects.
- **Pairing**: HELLO/FINISH only learn the device key. `PAIRING_AUTHORIZE` is
  the only path that persists a peer key, and only after Access reports the
  grants transaction is done. A public-key reference in SQLite without the
  credential is a hard startup error (no silent identity rotation).
- **Grant state**: the plugin cannot query Access grants (it has no HTTP and
  Access has no host-facing grant API). A persisted peer key (written only by
  `PAIRING_AUTHORIZE`, after Access wrote grants) plus the applied-revocation
  cache is the local proof that a device is authorized. Revocation deletes the
  peer key; Access remains authoritative. This is the only form of "verify
  device grant state" available on the host.
- **Revocations**: each MQTT connect clears channels and sets
  `revocations_pending`; new pairing/channel handshakes are rejected until a
  `RevocationNotice{final=true}` batch is applied. Access is authoritative on
  conflict; the plugin never deletes an Access ACL.
- **Channels**: one live channel per device, replaced by a completed handshake;
  old-channel ciphertext is deleted. MQTT disconnect/reconnect clears all
  channels, so clients always run a fresh handshake.
- **Commands**: `(device_id, command_id)` plus `SHA-256(body)` are persisted.
  The accepted row is written and `host_accepted` is published **before** DSH is
  called. Same id + same digest replays the stored terminal result; same id +
  different digest is rejected. Unfinished accepted rows become `unknown` at
  the next start and are never auto-executed.
- **Approvals**: digest/version/expiry/pending/session state are checked before
  DSH is asked, and again atomically by the adapter. Late/mismatched requests
  return `approval_expired`, `approval_version_mismatch`,
  `approval_already_resolved`, or `approval_unavailable`, and fail closed.
- **Conversations**: Source-owned opaque history cursors and stable Messages replace
  numeric client event positions. Watch publication is recipient-scoped; transient
  text and committed changes have separate bounded lanes. Refresh reconciles a
  lost last notification from authoritative source state.
- **History**: fixed-cut pages stage atomically in Core. Reads preserve Message
  IDs. Unavailable or incomplete history is explicit; no second plaintext log is
  created. See [streaming](../../docs/streaming.md).
- **Storage**: `<state_dir>/state.sqlite` preserves peers, commands, revocations
  and metadata. A separate SQLite instance lock prevents concurrent persistent
  runtimes without keeping the operational database write-locked.

## Tests and coverage

```bash
cd host/dsh
pnpm test          # node:test with coverage thresholds
# Repository gate: ./repo test --project dsh --coverage
```

Latest run (Node 25.9.0): **117 tests pass**, line **96.1%**, branch **85.9%**,
function **93.2%**. Thresholds in `package.json` are 90% line, 85% branch, 90%
function.

Test suites cover: Noise XX roundtrip/MAC/prologue/AAD/channel keys; protobuf
binary fixture (`proto/v1/fixtures/host_request.bin`); pairing cancel/mismatch/
authorize ordering; channel replacement and revocation replay; command dedupe
across restart, digest conflict, accepted→terminal, crash→unknown, expiry;
approval expiry/version/digest/already-resolved/atomic consumption; event
sequencing restart, duplicate/gap/snapshot, batching, outbox bounds, ephemeral
non-persistence, per-device encryption; FakeDSH behaviour; real-adapter mapping
against a fake Cordis context; MQTT client; host lifecycle and reload.

## Known limitations

- DSH owns session content. The native snapshot API may allocate its full source
  view; paged protocol output does not remove that native memory cost.
- Native titles and queued prompts are decoded only in the adapter. Unknown
  source metadata remains unavailable rather than inferred.
- Pending approvals are current state and are queried on watch/reconnect/refresh;
  historical snapshots do not invent old pending actions.
- Claim/delegate mode and local/remote approval races require actual DSH acceptance.
- Current Core permits one active Watch per instance. Other Harness connectors,
  multi-session client multiplexing and generic provider integration are not shipped.

## What is not verified against real DSH

No real DSH process was run for this deliverable. The real adapter is written
against the internal DSH `0.2.2-rc.1` typings and is exercised only with a fake
Cordis context in `test/real-adapter.test.js`. The following remain unverified:

1. Cold-session coverage of `ctx.sessionController.list({}, signal)`. Its value is `{ items }`.
2. `ctx.sessionController.prompt/cancel/state` signatures, error codes, and
   `queue`/`steer` semantics.
3. `ctx.sessionController.events({address, beforeSeq, maxMessages})` cold paging
   and `hasMore`/`cursor` semantics.
4. `ctx.sessions.get(id).snapshotEvents()` full-log ordering and the deprecated
   synchronous read contract; live-session coverage is assumed complete unless
   `snapshot_max_events` truncates it.
5. `ctx.on('session/event')` committed event types, sequence stability across
   DSH restart, and whether any actual event is ephemeral.
6. `ctx.on('approval/request')` waterfall registration, audit-id matching
   (`approval/asked`), signal abort, timeout, and coexistence with a UI
   answerer. `approval_mode=claim` answers every approval; this may hide local
   prompts.
7. `ctx.credentials.readRecord/modifyRecord` with a `GrantRecord` payload and
   the `whalerein-dsh/host-key-*` record key.
8. Session `active`/`running` state as the approval fail-closed gate.
9. Cordis `ctx.effect` reload/stop behavior inside a real DSH profile.
10. Real MQTT 5/TLS against the selected broker (CA, ACLs, client-id rules,
    reconnect) and the Access HTTP/MQTT provisioning flow.
11. Crash recovery on a real OS process boundary (the tests simulate it with a
    second SQLite connection).
12. DSH's own session sequence restart behavior; a regression triggers a
    snapshot but has not been observed on a real DSH restart.

Do not claim real DSH integration works until those are run.

### Correlating a CLI failure

Use the CLI's Diagnostic ID to search Access and plugin `trace_id` fields.
Locally hashed request/device IDs deliberately differ between services. Plugin
`request_frame_rejected` records channel scope and a bounded reason; this does
not by itself mean a device grant was revoked. `revocation_applied` includes the
notice's revocation timestamp and whether it came from Access or local settings.

MQTT disconnect records include connection `generation`, `reasonCode`, and the
last publish's `kind` and `bytes`. A reason code of `-1` means no broker reason
was received. These are diagnostic clues, not proof of an oversized message.
No payload, topic, token or broker free-text reason is included.

### Snapshot and MQTT size budget

Current source fragments logical envelopes with capability checks, bounded
reassembly, and receipts. It no longer uses the former 4 MiB single-envelope
limit. See the [wire budget](../../proto/v1/WIRE.md#transport-size-budget) for
limits and ACK routing. Oversized snapshots fail explicitly rather than returning
successful partial content. Reconnect clients after changing transport limits.

The large-message regression lives in
`tests/integration/drivers/transport/mqtt-large-message.test.js`. Run
`./repo test --suite integration --group transport` from the repository root. The test
container owns its private broker and synthetic two MiB body; no host broker
credentials are required and no user devices are paired or revoked.

### Identity and transport-v2 rollout

See [the security guide](../../docs/security.md) and [wire contract](../../proto/v1/WIRE.md)
for identity lifetime, staged pairing/COMMIT, transport receipts and migration.
Plugin SQLite development schemas upgrade to internal schema 3; stage metadata survives reload but is
not an active device. Revocation removes staged and active peers. No message
contents or private keys are added to this SQLite state.

Access, plugin and CLI must be upgraded together for new pairing. Existing
paired IDs/keys are retained. Reload the running plugin to load new source;
a symlinked package alone does not reload it. The real-broker transport test runs inside the isolated test container and
does not touch production grants or broker configuration. Its test-only auth-free config must not be used for deployment.


## Current normalized conversation contract

The runtime uses the unreleased v1 typed [Harness contract](../../docs/architecture/harness-contract.md).
HistoryRead replaces GetEvents; Messages use stable IDs and opaque
Source cursors. Snapshot reads preserve IDs; pages install atomically. Source
runtime/approval state is separate from committed history. Live text is an Attempt
overlay and has no persistent event number. Core owns provisional sequencing and
final precedence; CLI consumes typed ViewUpdates.

Current code paths, ownership and native verification limits are documented in
[the DSH architecture guide](../../docs/architecture/dsh-plugin.md). No other
Harness connector or Pi package is shipped. Update Access, Host and CLI local
builds together; no published-version compatibility path is retained.
