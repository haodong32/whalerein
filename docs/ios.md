# iOS client

The iOS client is a native SwiftUI application for iOS 18 and later. It talks
to the same Rust Core as the CLI through a dedicated UniFFI binding. Access and
the Host remain unchanged: the app pairs through a Standalone invitation and
operates existing sessions over the existing encrypted transport.

| Layer | Path | Responsibility |
| --- | --- | --- |
| App | `apps/ios/` | SwiftUI screens, conversation presentation, composer, approvals, diagnostics, Keychain adapter, localization, accessibility, test plans. |
| Binding | `apps/mobile-bindings/` | `whalerein-mobile` crate, UniFFI exports, synchronous Swift facade, event pump, XCFramework assembly. |
| Core | `core/` | Pairing, watch orchestration, encrypted cache and drafts, command delivery and recovery, redacted diagnostics. |

Dependencies run from the app to the binding to Core. Swift owns rendering,
scroll position, selection and focus, and forwards explicit actions. Core owns
snapshot/watch handoff, order, delivery semantics, approval version binding and
local encrypted state. See [Core architecture](architecture/core.md) and the
[harness contract](architecture/harness-contract.md).

The presentation layer is being rewritten into per-screen MVI slices: one
business state value per screen, a pure reducer, and explicit dependencies from
`AppEnvironment`. The target structure, ownership rules and the "where does this
belong" table live in [iOS client architecture](ios-architecture.md); the rules
agents must follow live in [`apps/ios/AGENTS.md`](../apps/ios/AGENTS.md). During
the rewrite `AppRuntime` owns activation and the legacy `AppModel` is a temporary
bridge that is deleted once every screen has moved.

## Build

Requirements: macOS with Xcode 16.4 or later, Rust with the
`aarch64-apple-ios` and `aarch64-apple-ios-sim` targets. Mac Catalyst also needs
`aarch64-apple-ios-macabi`
(`rustup target add aarch64-apple-ios-macabi`).

```bash
./repo generate ios
open apps/ios/WhaleRein.xcodeproj
```

XcodeGen reads [project.yml](../apps/ios/project.yml) to generate the Xcode
project and installs the pinned XcodeGen release on first use. Regenerate the
project after editing that file.

The shared schemes prepare Rust bindings in a Build pre-action. Build or run
again after changing Rust or UniFFI. The hook checks the binding inputs and the
selected platform slice, then calls the binding scripts when needed. Rust
builds keep default Core features disabled because the app injects the
Keychain `SecretStore`. The hook writes
`apps/ios/Vendor/WhaleReinMobile.xcframework`, matching Swift bindings, and a
recorded revision.

The project compiles the generated `WhaleReinMobile.swift` and
`WhaleReinMobileError.swift` into a dedicated `WhaleReinMobile` framework target
and links the XCFramework there; the app and test targets import the resulting
`WhaleReinMobile` module. The generated binding defines `HostSummary` and
`DeliveryChoice`, so a separate module keeps them from colliding with the app's
domain models. The generated project and bindings stay untracked.

The deployment target is iOS 18.0. The project targets both iPhone and iPad
(`TARGETED_DEVICE_FAMILY = "1,2"`) and enables Mac Catalyst for the app and test
targets. `DEVELOPER_DIR` must point at a full Xcode installation; a
CommandLineTools-only selection cannot build or test the app.

```bash
# iPad
xcodebuild build -project apps/ios/WhaleRein.xcodeproj -scheme WhaleRein \
  -destination 'platform=iOS Simulator,name=iPad Pro 13-inch (M5),OS=26.4.1' \
  -derivedDataPath .data/ios-derived/ipad CODE_SIGNING_ALLOWED=NO

# Mac Catalyst (compile/link only; no signing identity required)
xcodebuild build -project apps/ios/WhaleRein.xcodeproj -scheme WhaleRein \
  -destination 'platform=macOS,variant=Mac Catalyst' \
  -derivedDataPath .data/ios-derived/catalyst CODE_SIGNING_ALLOWED=NO
```

## Launch and navigation

A paired launch is conversation-first: the runtime triggers launch restore,
which loads the authorized Hosts, picks the remembered `lastHostId` when it is still
authorized (otherwise the first authorized Host, preferring an online one),
loads that Host's sessions from the service or its cached sessions while
offline, and opens `lastSessionId` when it is still present. A missing session
falls back to the most recent session and is persisted as the new last session.
The root shows an explicit restoring, no-Host, no-session or error state instead
of a Host list; no-session and error states offer "Choose session" or retry.

Host and session switching is adaptive. Below 720 pt of available width the app
keeps the bottom-sheet switcher opened from the conversation toolbar
(`conversation.switcher`, medium/large presentation detents). At 720 pt and
wider it shows a persistent 300 pt left sidebar (`session.sidebar`) beside the
conversation in an `HStack`, separated by a `Divider`, and hides the toolbar
button. The breakpoint helper lives in `AdaptiveLayout`; iPhone portrait widths
stay compact, iPhone landscape, iPad, and wide Mac Catalyst windows show the
sidebar, and narrower Catalyst windows fall back to the sheet.

The sheet and sidebar share `SessionSwitcherContent`, so both list authorized
Hosts and the selected Host's sessions with the same states, selection
checkmarks and actions. Tapping a Host loads its sessions in place, and tapping
a session replaces the root conversation (and dismisses the sheet when open).
The sidebar uses a native sidebar list over `.regularMaterial` and has no Close
control; the sheet keeps its Close control. Settings
(`conversation.settings`) and diagnostics (`conversation.diagnostics`) remain
one-tap toolbar sheets and the unpaired invitation/onboarding flow is unchanged.
Forget and revoke return to the invitation flow and clear the conversation and
switcher state.

Orientations: iPhone supports portrait, landscape left and landscape right
(upside-down remains disabled); iPad supports all four. The app keeps automatic
light/dark appearance. Mac Catalyst windows resize freely, including across the
720 pt breakpoint; no hard minimum window size is imposed.

## Test plans

| Plan | Command | Covers |
| --- | --- | --- |
| `Unit` | `./repo test ios-unit` | Swift Testing units for conversation state, presentation, delivery, drafts, diagnostics, and localization. |
| `UI` | `./repo test ios-ui` | XCUITest journeys over deterministic fixtures, including navigation, layouts, and accessibility. |
| `Integration` | `./repo test ios-integration` | Stack-backed XCTest journeys over Swift, UniFFI, Rust Core, Access, and a disposable host fixture. |

Use `--simulator UDID` to select an available device. The command can use a
single available device or a single booted iPhone by default. Use `--only TEXT`
to select a literal substring of a native test ID. Each invocation builds from
its captured source, leases the device, and saves scoped verification facts
under `.data/runs/`. It builds before starting the integration fixture and
removes the private XCTest configuration during cleanup. The app chooses its fixture with a scenario
(`-ui-scenario <id>` from `Debug/Scenario/ScenarioCatalog.swift`) or, until the
legacy flags are removed, with `-ui-fixtures` and `-ui-test-reset`.

Focused entry points while the rewrite is in progress:

```bash
./repo test ios-unit --only ConversationReduceTests
./repo test ios-unit --only AppLogStoreTests
./repo test ios-unit --only CoreDiagnosticBridgeTests
./repo test ios-ui --only StateGalleryUITests
python3 apps/ios/scripts/architecture_gate.py
apps/ios/scripts/release-surface-scan.sh
```

The Debug state gallery (`conversation.gallery`) lists the same catalog and
rebuilds the runtime group when a scenario is applied, so a state is inspectable
on device without a UI test.

GitHub-hosted Unit and UI jobs use Xcode 27.2 and an iOS 27 simulator. Full
verification also runs the Unit plan on iOS 18 with Xcode 16.4 to check the
deployment baseline.

## Local integration stack

`tools/ios-stack/` starts the disposable backend and writes a state contract
(invitation URI, Host, session) for the `Integration` plan. It exposes a
token-protected loopback control API for fresh invitations, fixture events,
Access restarts, and separate broker stop/start so transport loss can be tested
while HTTPS stays reachable. See its README for the exact environment variables,
lifecycle and limitations. It is intentionally local-only: hosted GitHub macOS
runners cannot run the Docker service stack.

The `iOS` workflow schedules this plan nightly and exposes it as the
`integration` manual-dispatch suite; `./repo test ios-integration` starts and
tears down the fixture stack itself. The job uses
`vars.WHALEREIN_IOS_STACK_RUNNER` and defaults to `macos-15`, which has no Docker
daemon. Until a self-hosted macOS runner with Docker and Xcode is registered in
that repository variable, the scheduled integration job is expected to fail its
Docker preflight; that is a repository-runner setting, not a workflow secret.

`ContractParityTests/testLocalStackParity` remains a skip in the Unit plan even
when `WHALEREIN_LOCAL_STACK=1`, because it throws before doing any comparison.
The live contract comparison is instead owned by the `WhaleReinIntegrationTests`
cases in the `Integration` plan against the stack that `ios-integration`
provisions. Converting the placeholder into an executable comparison needs a
client-test change with injected stack state; it is not reachable from CI
configuration alone and is not counted as coverage here.

The session switcher is sessions-first. The current Host is a single compact
picker row (a native `Menu`) above the list; sessions are grouped by workspace
(`cwd`'s last path component) with folder section headers, and each row is one
line: running state, title, and a last-active time rendered by the system
(`Text(date, style: .relative)`, or a short absolute date after seven days).
Grouping is skipped when a Host has only one workspace. The compact switcher
uses a plain list for density; the wide sidebar keeps the native sidebar list
style.

## Client configuration

The app has one authoritative TOML at
`Application Support/WhaleRein/client/app.toml`. Its `[app]` section stores the
committed `active_instance` and `reasoning_visible`; each `[instances.<id>]`
table uses the typed Core `ClientConfig` fields with paths relative to the app
document. The binding creates a fresh document containing `cloud-production`
and `standalone` only when absent. Legacy root `preferences.json`/`core/` files
are never read, migrated or deleted.

Per-instance materialized Core configuration lives at
`client/instances/<id>/instance.toml`, Core state under
`client/instances/<id>/data`, and an imported Standalone private CA at
`client/instances/<id>/ca.pem`. All document writes are atomic; activation
writes and validates the materialized instance before committing
`app.active_instance`, so a failed write keeps the previous selection.
Cloud instances without an imported CA use the iOS platform verifier for both
HTTPS and MQTT; an imported CA replaces that trust for the instance.

The welcome screen and Settings offer the service selector. Production builds
show Cloud production and Standalone; Debug/test builds also show Staging.
Local stays hidden until iOS trust for the desktop mkcert root is verified.
Switching requires explicit confirmation, prepares the destination, rebuilds
`AppModel` and the service for the new instance, and fences callbacks and
events from the previous activation with a generation counter. Each instance
has its own data directory, draft/cache state and Keychain service
`com.whalerein.ios.secrets.<instanceId>` (staging
`com.whalerein.ios.staging.secrets.<instanceId>`). A failed switch returns an
error and keeps the old runtime.

`-ui-fixtures` keeps the deterministic `FixtureSessionService` with an
in-memory app configuration containing the same two instances.
`-integration-fixtures` and a normal launch compose `LiveSessionService` from
the active instance's materialized `config_path`. If Core cannot initialize,
the app shows the localized unavailable state instead of crashing.

A Cloud instance shows the official hosted sign-in as the primary welcome
action and keeps self-hosted invitation pairing as the secondary path. Swift
opens the URL returned by Core in `ASWebAuthenticationSession` with callback
scheme `whalerein`; it never renders credential fields, and Passkey/email are
handled by the hosted page. Core owns the attempt ID, PKCE S256 verifier,
Cloud-issued state, local credential/revocation material and exchange, all
outside the Swift boundary. Cancellation, expiry, replay, a mismatched state,
a wrong account/workspace scope and a callback for a replaced activation or
instance never establish a session. After sign-in the app shows the account
computer directory with empty, authorized-online, authorized-offline, eligible
connect and unavailable states; a session opens only after Core confirms the
computer grant. The account is restored on launch, Settings shows local
sign-out plus a pending remote sign-out, and `-ui-cloud-signed-in`,
`-ui-cloud-empty` and `-ui-cloud-unavailable` drive the deterministic Cloud
fixtures. Live Staging, Passkey, email and signed-iPhone behavior is not
verified in this environment.

`LiveSessionService` restores the encrypted local transcript with
`MobileCore.cachedMessages` on each watch reset, deduplicates committed message
ids against live updates, maps Source `deliveryModes`
(`start_if_idle`/`steer`/`enqueue_after_run`) and `interrupt` without inventing
support, and treats Core's authoritative `state` event as the online signal.
The requested commands are:

```bash
./repo test ios-unit
./repo test ios-integration
./repo test ios-ui
```

## Composer submissions

Send and Stop each represent one user intent per submission. While a Send or
Stop submission is pending, that pending state is set synchronously before the
service call, the affordance (`canSend`/`canStop`) is disabled, and repeated
taps are coalesced so they never issue a second command. A definite failure
clears the pending state and permits retry; an accepted or uncertain receipt
stays recoverable through the existing command reconciliation, and a late
settlement of a failed submission never re-arms or acts on a newer intent.

## Diagnostics visibility

The diagnostics entry point in the conversation toolbar is compiled only in
`DEBUG` builds. Release builds keep the Core-side redacted report and its
tests, but do not ship a toolbar shortcut for it; a release build should expose
support tooling through an explicit support channel instead.

## CI

`.github/workflows/ios.yml` reports a lightweight `iOS gate` on every ordinary
PR revision. Add `run-full-ci` to build and test affected iOS inputs for that
revision; only the label event requests this work, so remove and re-add it after
a new push. The build inputs include `apps/ios/`, the binding crate, Core,
protocol definitions and Rust manifests. Requested verification builds the
XCFramework and test products once, then runs the `Unit` and complete `UI` plans
on macOS and uploads result bundles. A lightweight gate success is not iOS test
evidence. Main pushes, merge queue and nightly runs also run full verification;
manual dispatch accepts `unit`, `smoke` or `full`.
See [CI test tiers](testing.md#ci-test-tiers) for timing and revision rules.

## Security and accessibility

Secrets live in the device-only Keychain namespace and never in UserDefaults or
the app container. The one exception is the Debug-only local Cloud instance
(`cloud-local`), which uses a 0700, complete-file-protection, backup-excluded
file store beside its materialized configuration because the local test stack is
used from Simulator builds where the Keychain identity may be absent. Every
other instance, including Staging and production, keeps the Keychain store.
Drafts and cached conversation data are encrypted by Core and
scoped to the profile, Host and session. A confirmed revocation clears the
affected authorization, cache and drafts while preserving the device identity.
Diagnostics are redacted by Core and shared only after an explicit user
confirmation.

Controls on iOS 26 use the native Liquid Glass button styles
(`.glass`/`.glassProminent`); the composer and approval surface use the system
`glassEffect` shape API, with a material fallback on older systems. Message
content, tool output and code blocks intentionally keep content surfaces rather
than glass, matching the platform guidance that glass belongs to controls and
navigation.

The navigation bar is transparent and uses the system soft scroll-edge blur on
iOS 26, so the conversation scrolls beneath the title and glass toolbar controls.
Connection, running and watch-ended state sit with the composer rather than in a
header row under the title bar.

The app supports English and Simplified Chinese through a String Catalog,
VoiceOver labels and values, Dynamic Type including accessibility sizes, and
system light and dark appearance. Status is never conveyed by color alone.

## Session questions

`MobileCore.startWatch` opens one Core observation selecting Conversation and
Questions on the same device transport. Each queued event retains its producer
Host/session, Core observation identity and binding generation; replacing the
watch drops only output from the replaced watch while pairing and command events
survive. Questions are `loading`, `current`, `stale`, `unavailable` or
`unsupported`, and only a complete current collection can mean that no pending
questions exist. A batch carries its session, source epoch, version, digest,
phase and answerable/content-unavailable flags; edits survive an unchanged guard
and are discarded when the guard changes. The UI requires an explicit review
before submitting and Core revalidates the guard at invocation. The answer task
is independent of the visible observation, so closing or replacing a view does
not cancel it, and there is no discovery polling loop.

One-shot `listQuestions`/`getQuestion` and guarded `answerQuestion`/
`cancelQuestionAnswer` are flat binding APIs; no wire or native DTO crosses into
Swift. The question fixture appears only when the app is launched with
`-ui-questions`.

## Known limitations

- Physical-iPhone acceptance, Keychain backup behavior, native MQTT TLS/ALPN on
  a device and Instruments measurements require a signed device and are release
  evidence, not Simulator evidence.
- The local development host may not have an iOS 18 Simulator runtime; the
  deployment target is still 18.0 while Simulator evidence records the installed
  runtime. There is no physical iPad or iPhone evidence.
- Mac Catalyst has compile/link, launch, screenshot and resize evidence with
  signing disabled. A 1000 pt wide window showed the sidebar and a 550 pt window
  fell back to the compact switcher. The Unit and UI plans run on the iOS
  Simulator, not Catalyst, and no signed Mac distribution exists.
- The loopback stack is plaintext HTTP/MQTT with a FakeDSH fixture and one
  session. It does not establish TLS, multi-Host or real source persistence.
- Textual is not adopted in the first baseline. Committed assistant text uses
  native `AttributedString` inline Markdown plus a native block renderer for
  headings, paragraphs, ordered and unordered lists, block quotes, rules and
  fenced code with copy; the package and performance evaluation remains a
  feasibility item.
- Hosted Passkey sign-in, automatic pairing and lock-screen approval alerts are
  outside this client milestone.
