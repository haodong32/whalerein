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
The canonical client-environment catalog is packaged with Host; no generated
Cloud descriptor or checkout-only path is used.

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

The running DSH profile is the only connection-settings source. Settings → WhaleRein writes `$DSH_HOME/profiles/<profile>/cordis.patch.yml` (entry id `whalerein-dsh`). The plugin reads that document after Cordis has evaluated any `!!js` expression stored in it. Managed profiles reference the per-device TOML file through `config_file` (Host default `~/.whalerein/config.toml`, or `WHALEREIN_CONFIG`); the plugin keeps no environment catalog, endpoint table or TOML writer of its own.

Every documented key below is part of the shipped `Config` schema and is read from the profile. Saving applies all changed fields in one atomic namespace mutation at the revision the draft was read from; a rejected save keeps the draft, and clearing a field restores inheritance. If the stored configuration cannot resolve at startup, the plugin keeps the settings page available and names the invalid field instead of looking like an unpaired host. Clearing `server` is blocked while it is the only source of `mqtt_url`; save a legacy `mqtt_url` first or keep the server address.

A legacy Standalone secret may be stored in that file as an `!!js process.env` expression so the value stays out of the file. Managed profiles carry no endpoints, trust material or tokens; `pnpm start` seeds only `connection_mode: managed`, `adapter: real` and its state directory.

| Key | Default | Meaning |
|---|---|---|
| `connection_mode` | `auto` | `auto`, `managed`, or `standalone`. Auto keeps every profile with existing `server`/`mqtt_url`/`host_token` material standalone and treats a profile without connection material as the managed WhaleRein Cloud flow |
| `config_file` | empty | Explicit per-device TOML path passed to the Host configuration module. Empty uses `WHALEREIN_CONFIG`, then `~/.whalerein/config.toml` |
| `environment` | empty | Optional per-instance environment override. It is validated by Host resolution, never persisted into the file and never creates a second default |
| `state_root` | `~/.whalerein/dsh` | Managed state root passed to the Host connection manager. It is exactly the caller value; it is never derived from a configuration descriptor. `state_dir` remains the legacy alias for Standalone profiles |
| `profile_id` | none | Selected managed profile. Opaque identifier only; it is never a display name or host ID |
| `developer_tools_enabled` | `false` | Enables the staging/Developer tools affordance in the managed settings page. Enabling staging still requires an explicit profile selection |
| `enrollment_issuer_public_key` | none | One base64url Ed25519 public key. When set, the managed flow verifies the account service's portable authorization signature before sending anything. Access verifies the same document independently |
| `enrollment_issuer` | none | Absolute origin the signed authorization must name. Checked together with the pinned key |
| `server` | none | host name or IPv4 address. When set, the plugin derives the Access origin and the MQTT URL and ignores `mqtt_url` / `access_url` |
| `admin_token` | none | Not shown in settings. The plugin does not use it. A saved value stays in the profile. Host-signed requests mint this host’s pairing link and revoke this host’s grant. The Access admin token remains the operator credential for invitations for any host and for `DELETE /v1/devices/{id}` |
| `access_port` | `443`, or `8080` when plaintext is allowed | Access port. `443` and plaintext `80` are omitted from the URL |
| `mqtt_port` | `8883`, or `1883` when plaintext is allowed | MQTT port |
| `mqtt_url` | required when `server` is empty | legacy `mqtts://...`; `mqtt://` only when `allow_insecure_mqtt` is true |
| `host_id` | generated once from the computer name when the profile has none | topic-safe host id. A saved value stays. Clearing the settings field keeps the saved value. Changing it requires confirmation and creates a new host identity: paired devices must pair again. The Host credential record key stays stable so a rename does not brick the next start |
| `host_token` | required | broker provisioning token (never logged) |
| `ca_file` | none | Legacy Standalone TLS CA bundle. Managed trust comes only from the selected environment's scoped `tls` resolution |
| `state_dir` | `~/.whalerein/dsh` | Not shown in settings. SQLite directory |
| `adapter` | `auto` | Not shown in settings. Inside DSH, `auto` uses the real adapter. `fake` and `real` stay for tests and standalone runs |
| `pending_queue_enabled` | `true` | Not shown in settings. When false, pending-queue read/edit/remove/steer are not advertised |
| `enable_open_question_broker` | `true` | Not shown in settings. When false, the open-question broker is not installed and open-question answer is not advertised |
| `access_url` | none | legacy Access origin, used when `server` is empty |
| `proto_path` | repo descriptor | protobuf JSON descriptor |
| `approval_mode` | `claim` | `claim` answers `approval/request`; `delegate` calls `next()` |
| `approval_ttl_ms` | `300000` | pending approval lifetime |
| `event_batch_ms` | `20` | ordinary-output batch window |
| `outbox_per_device_limit` | `100` | pending ciphertext rows per device |
| `outbox_total_limit` | `1000` | pending ciphertext rows overall |
| `snapshot_max_events` | `5000` | DSH snapshot read bound |
| `reconnect_min_ms` / `reconnect_max_ms` | `500` / `30000` | MQTT backoff |
| `shutdown_timeout_ms` | `5000` | wait for in-flight requests on stop |
| `log_level` | `info` | `debug`, `info`, `warn`, `error`, or `silent` |

`allow_insecure_mqtt` defaults to false and is not shown in settings. Turn it on in the profile only for a trusted plaintext broker.

`pending_queue_enabled` and `enable_open_question_broker` default to true and are not shown in settings. Setting either false in the profile withdraws that capability group from advertisement; native service support still determines the remaining groups.

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
`WHALEREIN_DSH_PORT` selects the port. `WHALEREIN_CONFIG` selects the
per-device TOML file; no Cloud descriptor or host/admin token is injected.
The normal `web` profile on 3080 is untouched.

Loading the Host plugin as a bundle (rather than a `--patch` launch overlay)
is what lets the settings page save into the profile user layer.

## Settings page

A fresh profile without manual connection material loads the managed WhaleRein
Cloud flow: a title, a short explanation, and one primary **Connect to WhaleRein
Cloud** action. No server, port, Host ID, token, region or fingerprint input is
required, and loading the page never starts enrollment or opens a browser. The
server-side settings service projects the frozen `ConnectionManager` API
(`snapshot`, `details`, `startConnect`, `cancel`, `retry`, `disconnect`,
`reconnect`, `remove`, `switchProfile`, `enableStaging`, `addCustomService`,
`deleteProfile`) through allowlisted snapshots. Poll credentials, authorization
documents, keys, host ids, routes and raw approval locators never cross the
browser RPC boundary or enter logs; the approval destination is a validated
credential-free http(s) URL used only for the explicit browser handoff.

Managed states are `idle`, `requesting`, `awaiting_approval`, `enrolling`,
`recovering`, `connecting`, `connected`, `reconnecting`, `disconnected`,
`denied`, `expired`, `action_required`, `removing`, and `removed`. Connected is
shown only from runtime readiness. The status region announces changes through
`role=status`/`aria-live`, focus moves to it on state changes, and the layout
wraps from 320 CSS px upward.

Browser handoff reserves a tab during the explicit Connect click and falls back
to a visible **Open browser** link when the browser blocks it. The approval URL
carries the active `en`/`zh-CN` locale. Connection details are collapsed by
default and expose only non-secret identity and redacted diagnostics. Custom
services live under Advanced, while Developer tools enables staging and then
requires an explicit staging profile selection; enabling staging does not
switch the computer. An active Staging label stays visible when details and
Developer tools are hidden, and switching back to production remains reachable.

Disconnect is a local pause that retains the cloud registration. **Remove this
computer** is browser-authorized and stays `removing` until the service enforces
removal. Deleting an inactive local profile is separate and explains that the
remote registration remains. Managed profiles hide device invitation/revocation
unless the selected service advertises pairing support; Standalone profiles keep
the existing pairing, invitation, revocation, and connection-test behavior.

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
native coverage is scoped as described under [Native DSH validation and
remaining limits](#native-dsh-validation-and-remaining-limits), so treat
adapter failures as the primary debugging target.

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

Latest run (Node 25.9.0): **304 tests pass**, line **96.0%**, branch **85.3%**,
function **91.7%**. Thresholds in `package.json` are 90% line, 85% branch, 90%
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
- Delegated approval ownership and interactive profile HMR still require target-environment acceptance.
- Current Core permits one active Watch per instance. Other Harness connectors,
  multi-session client multiplexing and generic provider integration are not shipped.

## Native DSH validation and remaining limits

The reproducible `native-dsh` integration group runs in the shared Docker image:

```bash
./repo test --suite integration --group native-dsh
./repo test --suite integration --group native-dsh --only test_native_browser
```

See the [integration guide](../../tests/integration/README.md#native-dsh-and-browser-journeys)
for isolation, pinned dependencies, local model behavior and case selection.
The group uses actual native APIs and Chromium; FakeDSH cases remain a separate
source of controlled fault/recovery coverage.

The native DSH `0.2.2-rc.1` runtime has been exercised in an isolated CLI Web
profile, using a deterministic loopback Messages provider rather than a paid
model account. The real Cordis services, Session persistence, credentials,
SessionController, approval waterfall and WhaleRein plugin were used; FakeDSH
was not part of that native journey.

Verified paths include invitation pairing, native Session discovery, user prompt
admission, live text plus final Message delivery (including an 80 KB body),
fixed history-cut paging while the native log grows, cold history cursor/message
identity after process restart, allowed-once approval, interruption during model
streaming, late approval rejection after cancellation, Access restart/reconnect,
and revocation/re-enrollment without reviving an old transport credential.

The automated browser journeys additionally cover masked credentials, connection
checking with valid/rejected drafts, save/reload persistence, invitation creation,
paired-device revocation, and exclusive remote claim ownership while the native
UI observes a UI-created Session. They use the actual installed settings page and
headless Chromium, with no screenshot or trace export.

Claim mode registers a global, prepended approval answerer: native Agent carriers
filter unrelated contexts, and the UI Remote bridge can otherwise hold the
waterfall before the remote adapter sees it. Delegate mode forwards immediately.
The native journey and real-Cordis routing regression cover this ownership rule.

Still requiring target-environment acceptance:

- Real external model-provider/account behavior and long-running production logs.
- Delegated local-UI approval ownership and Cordis configuration HMR in an
  interactive user profile. Automated claim-mode tests preserve exclusive remote
  ownership; simultaneous local and remote answers are not a shipped capability.
- Production MQTT/HTTPS TLS, CA/ACL deployment, and platform credential-store policy.
- Historical restoration of actual endpoint journals and model/provider failures
  beyond deterministic loopback fixtures.
- Very large cold histories beyond the configured snapshot bound and production
  memory/throughput limits. A bounded source read does not bound DSH's native
  full-snapshot allocation.

Private acceptance records live under `.local-docs/acceptance/`; passing isolated
native tests does not establish these remaining deployment properties.

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

The real DSH connector advertises model catalog/read/select, pending-queue
read/edit/remove/steer, creation, fork, wait, question read/open/continued and
delivery queue/steer by default when the bound native services provide them.
No test-only switch is required: the entry points open from feature detection,
and `pending_queue_enabled` / `enable_open_question_broker` can only opt out.
Test fixtures may opt in independently.

The only currently verified queue/steer and native entry-point evidence is the
standard DSH CLI runtime on Linux or macOS arm64 with Node 25.9.0, DSH
launcher/session-controller/app-boot `0.2.2-rc.1`, and Cordis `4.1.0`, as
recorded in [the DSH architecture
guide](../../docs/architecture/dsh-plugin.md#production-capabilities). Other
DSH versions, operating systems, CPU architectures, and Node versions are open
but unverified; advertisement there is feature detection, not a claim that the
operation works. Windows native behavior and a repeatable macOS runner remain
unverified.

### Official local account service

The official `local` environment uses the managed enrollment flow, alongside
staging and production, and is selected only by the per-device TOML file
(`environment = "local"` or an explicit settings/instance selection). Its
service identity, API/issuer origins, native registration and mkcert trust
defaults come from the public catalog and Host configuration module. No
Cloud-generated environment document is required.

The DSH profile keeps only `connection_mode: managed`, `adapter: real`, its
explicit `state_dir` and an optional `config_file`/`environment` reference; no
broker token, server, CA path or state root is copied from the environment
definition. Editing the TOML file changes desired settings for a subsequent
open only. A running authority stays pinned until an explicit switch; a changed
authority under the same name requires a new Host profile. Missing configuration
fails closed, and the development CA is scoped to the selected local service
without disabling validation or touching global Node trust.
