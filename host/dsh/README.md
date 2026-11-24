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

The current DSH profile's `whalerein-dsh` entry in `cordis.patch.yml` is the
only plugin configuration source. The settings page writes that same namespace
atomically at the revision it read. After saving, reload the plugin to apply a
connection change. A stale save preserves the draft and offers to load the latest
saved values. Ordinary starts preserve the entry.

```yaml
- id: whalerein-dsh
  name: whalerein-dsh
  config:
    service: whalerein_cloud
    cloud_environment: production
    adapter: real
    state_root: /absolute/path/to/instance-state
```

| Key | Default | Meaning |
| --- | --- | --- |
| `service` | `whalerein_cloud` | `whalerein_cloud`, `custom`, or `standalone` |
| `cloud_environment` | `production` | Internal-only `production`, `staging`, or `local`; hidden from the normal settings form |
| `custom` | absent | Custom account-service definition, described below |
| `state_root` | `~/.whalerein/dsh` | Existing Host registry and per-identity state root; `state_dir` independently selects the Standalone SQLite directory |
| `state_dir` | `state_root` | Standalone SQLite directory |
| `adapter` | `auto` | Real DSH inside the plugin; `real` and `fake` are also supported |
| `log_level` | `info` | Runtime logging level |
| `pending_queue_enabled` | `true` | Advertise supported native pending-queue operations |
| `enable_open_question_broker` | `true` | Enable the native open-question broker |
| `enrollment_issuer_public_key` | empty | Optional pinned base64url Ed25519 issuer public key |
| `enrollment_issuer` | empty | Optional expected authorization issuer |

Cloud definitions come from the shared, packaged official catalog. DSH does not
read `WHALEREIN_CONFIG` or `~/.whalerein/config.toml` during ordinary startup,
status polling or settings saves. The registry retains credentials, registration,
pairing, recovery and pause state; its active pointer cannot override the profile.
Cloud environments have separate identities, and returning to an environment
reuses its identity. An invalid target fails closed without connecting to a
previous environment. Starting another owner of the same state root fails the
existing activation lock; give independent DSH instances independent state roots.

Custom services use the same browser-authorization flow as Cloud:

```yaml
service: custom
custom:
  label: My service
  api_origin: https://app.example.com
  issuer: https://auth.example.com/oidc
  native_client_id: whalerein-cli
  tls:
    ca_provider: system
```

Only `label`, `api_origin`, `issuer`, `native_client_id`, and `tls` are accepted
inside `custom`. The label is optional. API origin, issuer and the service's
registered native client ID are required. API origin must be an HTTPS origin
without a path, credentials, query or fragment; issuer must be HTTPS on a distinct
origin. `tls` accepts only `ca_provider` (`system` or `file`, default `system`) and
`ca_file` (an absolute path, required only for `file`). Unknown fields are errors.
Access/MQTT routes and enrollment credentials are supplied by the approved service,
not entered in the custom block. Changing authority selects an isolated identity;
changing a label reuses the identity. Managed profiles also accept an optional
`host_label` (at most 64 characters). It is display metadata, included in the
approval name as `hostname · host_label`, and never changes the Host id or keys.

`service: standalone` uses the existing manual fields: `server`, `access_port`,
`mqtt_port`, `host_id`, `host_token`, `admin_token`, `ca_file`, and, for explicit
connections without `server`, `access_url`/`mqtt_url`. Host IDs are generated once
when missing. Tokens remain write-only in the UI. Profile `!!js process.env`
expressions require the operator to export those variables; the launcher injects
no development token defaults. `allow_insecure_mqtt` is only for explicit local
plaintext development. Existing runtime tuning keys remain supported.

The configuration contract is a hard cut. Removed selectors (`environment`,
`config_file`, `connection_mode`, `profile_id`, `developer_tools_enabled`) are
unsupported and rejected. There is no TOML import, registry-based configuration
inference, startup rewrite or editor repair of old settings. Configure the new
fields explicitly. Shared CLI/Core TOML behavior is unchanged.

## Run real DSH

```bash
cd host/dsh
pnpm start
pnpm start --profile other --port 3100
pnpm start --reinstall
pnpm start --dry-run
# Internal development only: saves the Cloud deployment in this DSH profile.
pnpm start --cloud-environment staging
pnpm start --cloud-environment local
```

The launcher creates the dedicated `whalerein-debug` profile when missing,
installs this checkout, registers the bundle and starts DSH with `--no-open`.
Fresh profiles use Cloud production and a profile-specific state directory under
`.data/dsh/<profile>`. Existing entries and paths are preserved. An explicit
`--cloud-environment` updates only that scalar; it never rewrites custom fields,
secrets or sibling entries. Unrecognized entry layouts are preserved on ordinary
starts and refused for scalar overrides; duplicate target entries are rejected.
The Cloud override does not change a selected custom/Standalone
service. Dry-run validates and prints only non-secret connection scalars, with no
filesystem writes. The normal `web` profile is untouched. The launcher does not
start Access, PostgreSQL or Mosquitto.

## Settings page

The connection selector exposes exactly **WhaleRein Cloud**, **Custom service**
and **Standalone**. All connection fields are saved in the DSH namespace; there
is no profile picker, Staging enable action or shared-default editor. Internal
Cloud instances show a fixed deployment badge without an environment switch.
The custom service form shows name and API origin first, with required account
and optional trust parameters under Advanced.

Loading settings never begins enrollment or opens a browser. The primary Connect
action reserves a tab during the click and navigates it only to the validated
approval URL. The page handles browser blocking with a manual link. Account login
stays in the browser. Status and details are allowlisted projections. Connection
details show the current Host ID and omit credentials, private routes, and inactive identities. The DSH remote surface
exposes lifecycle actions, read-only details, connection probes and device pairing;
configuration edits use the existing revisioned DSH settings writer only.

Disconnect is a local pause that retains the cloud registration. **Remove this
computer** opens the verified browser confirmation without marking removal
pending. The Host runtime, not the page, resolves the scoped Cloud/Access
removal state at cold start, before a user connect/resume, and after a
definitive authentication refusal. Only an active `removing` state is polled
to completion; a healthy connected profile has no background removal poll.
A blocked or abandoned browser handoff leaves the original connection policy
intact. Once removal is terminal the page shows a fresh-approval **Reconnect**
action; reconnecting keeps the DSH profile Host id and starts a new browser
approval for the next enrollment generation. Access re-enables the same Host id
only for a strictly higher revision with identical workspace and keys; old
credentials, grants and revocation fences remain scoped to the removed
generation. A rejected removal keeps the live binding usable. Inactive identity deletion is not exposed by the settings page. Managed profiles hide device invitation/revocation
unless the selected service advertises pairing support; Standalone profiles keep
the existing pairing, invitation, revocation, and connection-test behavior.
Managed settings list paired Devices and mark those with an active Session
observation as connected. A one-shot command leaves the Device paired.

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
UI through its Client half. In Standalone mode the connection form asks for a server host, a host
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
fields are saved to the profile user layer, and clearing a field restores its
profile base or schema default. The host token is write-only: its row shows
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
./repo test dsh
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
- **Channels** support independent CLI processes for the same Device. The Host
  accepts up to 64 channels per Device and 512 in total. Idle channels expire
  after two minutes and their pending ciphertext is cleared. A Host MQTT
  disconnect or reconnect clears all channels and requires fresh handshakes.
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
# Repository gate: ./repo test dsh --coverage
```

Coverage thresholds in `package.json` are 90% line, 85% branch and 90%
function. Repository reports retain the measured values for each run.

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
- Pending approvals are current state and are queried on watch/reconnect/version check;
  historical snapshots do not invent old pending actions.
- Delegated approval ownership and interactive profile HMR still require target-environment acceptance.
- Current Core permits one active Watch per instance. Other Harness connectors,
  multi-session client multiplexing and generic provider integration are not shipped.

## Native DSH validation and remaining limits

The reproducible `native-dsh` integration group runs in the shared Docker image:

```bash
./repo test native-dsh
./repo test native-dsh --only test_native_browser
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
- Very large cold histories and production memory/throughput limits. Pages bound
  protocol output, but a synchronization snapshot may still stage the complete
  authorized range; a bounded source read does not bound DSH's native
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
`./repo test transport` from the repository root. The test
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

### Internal Cloud deployments

Only `cloud_environment` in the DSH profile selects official staging or local
services. Local uses the packaged catalog's account authority and scoped mkcert
trust. The setting is hidden from ordinary forms; there is no development toggle
that changes selection. A missing local CA or unavailable service reports an error
without changing the saved deployment or disabling TLS validation.
