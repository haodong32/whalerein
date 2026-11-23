# WhaleRein iOS client

Native SwiftUI client for the OpenSpec change `add-ios-client`. The app ships
both a deterministic fixture service for UI/unit tests and `LiveSessionService`,
which talks to the real Rust Core through the UniFFI `WhaleReinMobile` module.
A normal launch and `-integration-fixtures` use the live service; `-ui-fixtures`
keeps the fixture path.

- Product/app target: `WhaleRein`
- Bundle identifier: `com.whalerein.ios` (staging `com.whalerein.ios.staging`)
- Deployment target: iOS 18.0, Swift 5 language mode
- Device family: iPhone and iPad (`TARGETED_DEVICE_FAMILY = "1,2"`); Mac
  Catalyst is enabled for the app and test targets
- No third-party Swift packages; Apple frameworks only

## Requirements and environment

- macOS with Xcode 27.2 at `/Applications/Xcode.app`.
- Rust targets `aarch64-apple-ios`, `aarch64-apple-ios-sim` and
  `aarch64-apple-ios-macabi` (the last only for Mac Catalyst builds).
- `xcode-select` points at CommandLineTools on the reference host, so every
  Xcode command must run with:

  ```sh
  export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer
  ```

  `scripts/build.sh` uses the default installation when available.
- Simulator runtime installed on the reference host: iOS 26.4.1
  (`iPhone 17`, `iPad Pro 13-inch (M5)`), plus iOS 27.2. There is no iOS 18
  runtime installed.
- Mac Catalyst builds use the same project and bundle identifier with signing
  disabled for verification; no Mac App Store signing identity is required.
- The documented destination is `platform=iOS Simulator,name=iPhone 17,OS=26.4`,
  but the installed runtime reports `26.4.1`, so `OS=26.4` does not match. Use
  `OS=26.4.1` (or omit `OS`). This is an environment difference, not a project
  setting: the deployment target remains 18.0.
- No physical iPhone or iPad is attached; all device evidence is Simulator
  evidence.
- Mac Catalyst is built with signing disabled. There is no signed Catalyst app
  or Mac App Store package.

## Build and test

The generated XCFramework and Swift binding are prerequisites:

```sh
bash apps/mobile-bindings/scripts/build-xcframework.sh
# Mac Catalyst needs a fourth Rust slice; run this after the command above:
bash apps/ios/scripts/build-catalyst-slice.sh
```

The first command produces the iOS device and Simulator slices. The Catalyst
script rebuilds the Rust binding for `aarch64-apple-ios-macabi` and adds a
Mac Catalyst slice to the same generated XCFramework without changing the
generated Swift binding. Re-run it after any `build-xcframework.sh` run or
Rust binding change.

The project has a `WhaleReinMobile` framework target that compiles the generated
`WhaleReinMobile.swift`/`WhaleReinMobileError.swift` and links
`Vendor/WhaleReinMobile.xcframework`. The app and integration test targets link
that framework and `import WhaleReinMobile`. A dedicated module is required
because the generated binding also defines `HostSummary` and `DeliveryChoice`,
which would collide with the app's domain models in the same module. A
"Check Vendor" build phase fails with a clear message when `apps/ios/Vendor/` is
missing; generated artifacts remain gitignored.

```sh
export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer

# Build for iPhone
xcodebuild -project apps/ios/WhaleRein.xcodeproj -scheme WhaleRein \
  -destination 'platform=iOS Simulator,name=iPhone 17,OS=26.4.1' build

# Build for iPad
xcodebuild -project apps/ios/WhaleRein.xcodeproj -scheme WhaleRein \
  -destination 'platform=iOS Simulator,name=iPad Pro 13-inch (M5),OS=26.4.1' \
  -derivedDataPath .data/ios-derived/ipad CODE_SIGNING_ALLOWED=NO build

# Build for Mac Catalyst (compile/link verification, no signing identity)
xcodebuild -project apps/ios/WhaleRein.xcodeproj -scheme WhaleRein \
  -destination 'platform=macOS,variant=Mac Catalyst' \
  -derivedDataPath .data/ios-derived/catalyst CODE_SIGNING_ALLOWED=NO build

# Run a specific local plan
./repo test ios-unit
./repo test ios-integration
./repo test ios-ui

# Wide-layout UI evidence on a selected iPad Simulator
./repo test ios-ui --simulator "$IPAD_UDID" --only WideLayoutUITests
```

Run the commands from the repository root. Results and native result bundles
are scoped to each run under `.data/runs/`. Identify devices with
`xcrun simctl list devices available`.

### Schemes and test plans

| Scheme | Test plan | Target | Purpose |
| --- | --- | --- | --- |
| `WhaleRein` | `Unit` (default), `Integration`, `UI` | app + all test targets | Build and `-testPlan` selection |
| `WhaleReinTests` | `Unit` | `WhaleReinTests` | Focused Swift Testing runs |
| `WhaleReinUITests` | `UI` | `WhaleReinUITests` | Focused XCTest/XCUIAutomation runs |

The `WhaleRein` scheme includes the `WhaleReinIntegrationTests` target in the
`Integration` test plan.

Test plans live in `apps/ios/TestPlans/`. The Integration plan requires the
stack environment; when it is absent every case skips with a message naming
`python3 tools/ios-stack/ios_stack.py up`. A skip is not a pass.

## Architecture

```
WhaleRein/
  App/            App entry, composition root, launch options, preferences
  Domain/         Domain models, SessionService protocol, live service, Keychain
  Fixtures/       Deterministic FixtureSessionService and catalog
  Presentation/   @Observable models, delivery policy, Markdown parser, L10n
  Views/          SwiftUI screens and reusable rows
  Resources/      Localizable.xcstrings, Assets.xcassets
WhaleReinTests/         Swift Testing unit tests
WhaleReinUITests/       XCTest UI tests
WhaleReinIntegrationTests/ Stack-backed Integration suite
Config/WhaleRein-Info.plist
TestPlans/{Unit,Integration,UI}.xctestplan
scripts/{build,test,test-integration}.sh
```

The project uses `PBXFileSystemSynchronizedRootGroup` (Xcode 16+) so files added
to the source folders join their target automatically.

Controls use native `.glass`/`.glassProminent` styles and the composer and
approval card use `glassEffect` on iOS 26, with material fallbacks on iOS 18.
Message content and code blocks stay on content surfaces. The navigation bar is transparent and uses the iOS 26 soft scroll-edge blur so
the conversation scrolls beneath the title and glass toolbar controls.
Connection, running and watch-ended state sit with the composer instead of a
header row under the title bar.

### Service boundary

`SessionService` (`Domain/SessionService.swift`) is the only interface screens
and presentation models depend on. It exposes pairing, Hosts, Host-scoped
sessions, capabilities, watch start/stop, a per-consumer
`AsyncStream<SessionEventEnvelope>` event stream, capability-aware send, stop,
approval listing/resolution with version and digest, draft save/load/clear,
diagnostics preview/export, connection state, forget/revoke, app version and
Core revision.

`FixtureSessionService` implements it deterministically. `LiveSessionService`
owns one `MobileCore`, runs every binding call off the main thread, drains
`pollEvents` in a background pump, and broadcasts ordered envelopes to all
current `AsyncStream` consumers. It maps binding errors to `AppErrorCode`
without forwarding raw Core text, maps `SessionSummaryView` and
`SessionCapabilitiesView` (including Source delivery modes and interrupt), and
uses the Keychain `SecretStore`. On every watch start it loads
`MobileCore.cachedMessages` and emits the encrypted local transcript with the
reset before live updates, deduplicating immutable committed message ids.
Capabilities come from `SessionCapabilitiesView`: Source `deliveryModes`
(`start_if_idle`, `steer`, `enqueue_after_run`) and `interrupt`; missing
capabilities are never defaulted. A `state` event marks the watch online because
Core emits it only after the authoritative snapshot/watch handoff, while later
`connection` events still report degraded, reconnecting and offline. Presentation
models filter every event by `(hostId, sessionId)` and monotonic sequence, so a
late event from a previous watch cannot enter a new conversation.

Domain types mirror the plan's binding contract: `HostSummary`,
`SessionSummary`, `PairingPrompt`, `Approval`, `ConversationMessage` /
`ConversationBlock`, `DeliveryChoice`, `ConnectionStatus`, `DiagnosticsReport`,
`CommandReceipt`, and `MessageEvent`
(reset/message/replace/preview/attemptEnded/state/connection/capabilities/
approvals/approvalChanged/commandStatus/error/watchEnded).

### Launch arguments

| Argument | Behavior |
| --- | --- |
| `-ui-fixtures` | Select the deterministic fixture service |
| `-ui-test-reset` | Wipe `Application Support/WhaleRein` before launch |
| `-ui-fixture-paired` | Seed a paired fixture Device and restore the default conversation. Applies when both `-ui-fixtures` and `-ui-test-reset` are present. |
| `-integration-fixtures` | Select `LiveSessionService` (the default without `-ui-fixtures`) |
| `-ui-large-text` | Force Dynamic Type `.accessibility1` for UI tests |
| `-ui-stream-on-open` | Start a scripted live preview stream when a conversation opens |
| `-ui-approval-state current\|expired\|changed` | Select the approval fixture variant |
| `-ui-late-event` | Emit a late event for the previous watch after a Host switch |
| `-ui-send-uncertain` | Return an uncertain send receipt and settle it later |
| `-ui-send-echo` | Echo the committed user message with a Core id (dedupe fixture) |
| `-ui-approval-state current\|expired\|changed\|fast-clock\|short-lived\|unavailable` | Select the approval fixture variant |
| `-ui-approval-refresh-after-decision` | Emit a pending-only approval refresh after a decision |
| `-ui-revoked-on-open` | Emit a remote revocation after the first watch reset |
| `-ui-watch-ended` | End the first watch after it starts |
| `-ui-offline` | Report the Host offline after the watch reset |
| `-ui-incompatible-protocol` | Report Host protocol major version 2 |
| `-ui-legacy-protocol` | Report a Host without a usable protocol version |
| `-ui-slow-pairing` | Delay pairing so a queued deep link can be exercised |
| `-ui-diagnostics-failure` | Make the diagnostics preview fail |
| `-ui-staging` | Show the staging indicator |
| `-AppleLanguages (zh-Hans)`, `-AppleInterfaceStyle Dark` | Handled by the system |

Without `-ui-fixtures` the app uses the live Core service. If the binding or
Keychain cannot initialize, the app runs with a localized unavailable error
state instead of crashing. Integration tests construct `LiveSessionService`
directly with a temporary data directory and an isolated Keychain service.

### Storage

- Authoritative configuration: `Application Support/WhaleRein/client/app.toml`
  with `[app] active_instance`/`reasoning_visible` and typed
  `[instances.<id>]` tables. It is created only when absent and contains Cloud
  production and Standalone; legacy root `preferences.json`/`core/` are never
  read, migrated or deleted.
- Per-instance state: materialized Core config at
  `client/instances/<id>/instance.toml`, Core data at
  `client/instances/<id>/data`, imported CA at
  `client/instances/<id>/ca.pem`, and last-selection UI state beside them.
- Secrets use `KeychainSecretStore` with per-instance service
  `com.whalerein.ios.secrets.<instanceId>` (staging
  `com.whalerein.ios.staging.secrets.<instanceId>`) and
  `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`. No secrets are written to
  UserDefaults or logs. Integration tests use a disposable service name per case.
- Drafts and cached history are owned by Core, scoped to profile, Host and
  session, and cleared by a confirmed revocation.

### Screens

1. Invitation: service selector for Cloud production, Standalone and (Debug
   only) Staging with explicit switch confirmation, `onOpenURL` for
   `whalerein://pair?...`, paste field, localized validation, explicit Host
   fingerprint prompt with confirm/decline, expiry and lost-result copy. The
   unpaired launch state; pairing success goes straight to the conversation.
2. Conversation root: after pairing, launch restores the last-opened session
   (`lastHostId` / `lastSessionId` / title) instead of showing a Host list. If
   the remembered session is gone it opens the most recent session of the
   remembered Host, or of the first authorized Host (preferring an online Host)
   when the remembered Host is no longer authorized. Restoring, no-Host,
   no-session and error states are explicit and offer a retry or a "Choose
   session" action.
3. Adaptive Host/session switcher: compact widths keep the bottom sheet opened
   from the conversation toolbar (`conversation.switcher`) with medium/large
   detents. At 720 pt and wider (iPhone landscape, iPad, wide Mac Catalyst
   windows) the switcher becomes a persistent 300 pt left sidebar beside the
   conversation, separated by a `Divider`, and the toolbar button is hidden to
   avoid duplicate controls. Both presentations use the same
   `SessionSwitcherContent`: every authorized Host with text online state and a
   selected checkmark, then the selected Host's sessions sorted by recent
   activity with cached/loading/empty/error states and a checkmark on the open
   conversation. Tapping a Host loads its sessions in place; tapping a session
   replaces the root conversation (and dismisses the sheet when it is open).
   There is no separate Host or session page and no back button to one.
4. Conversation: stable transcripts with lazy rows for long histories and eager
   rows for short or accessibility-size transcripts, plain-text streaming
   preview, native Markdown for committed assistant text (AttributedString inline + fenced code parser),
   typed tool/notice/question/approval rows, copy confirmation, approval card
   with Allow Once/Reject bound to version+digest, composer with draft,
   capability-aware Send/Steer/Queue, Stop and uncertain/pending status,
   jump-to-latest while scrolled up, and a cached/offline banner when the open
   session came from cached sessions.
5. Diagnostics: HTTPS/MQTT stages per Host, versions, correlation IDs, redacted
   preview, explicit Cancel/Share confirmation, system share sheet with the
   exported file only after confirmation. Reachable in one tap from the
   conversation toolbar (`conversation.diagnostics`).
6. Settings: Core revision, language note (follows system), data location,
   staging/fixture indicators, service selector with explicit switch
   confirmation, Standalone private-CA import/removal with validation errors,
   forget/revoke with confirmation. Reachable in one tap from the conversation
   toolbar (`conversation.settings`). Forget and revoke return to the invitation
   flow and clear switcher state. The App-section reasoning flag is readable and
   writable on the model but has no reasoning UI in this change.

### Fixtures and determinism

`FixtureCatalog` generates a 320-row transcript with Markdown, fenced code,
tool results, notices, questions, an approval, and a final code block. Content
is derived from a SplitMix64 seed, never the wall clock or system RNG. Multiple
Hosts and sessions are included, `travel-laptop` is offline and exercises the
cached path. A scripted stream emits deterministic `LIVE-STREAM-CHUNK` previews
and a final message. Clock and random generator are injected into the fixture
service for tests.

### Localization and accessibility

- `WhaleRein/Resources/Localizable.xcstrings` is a String Catalog with `en` and
  `zh-Hans` entries for all 171 keys in `L10nKey`. Views use `L10nKey` /
  `Text(LocalizedStringKey)`; no user-facing English is hardcoded in views.
  `LocalizationTests` fails if either language misses a key or loses a format
  specifier.
- Stable accessibility identifiers include `invitation.*`, `pairing.*`,
  `conversation.switcher`, `conversation.diagnostics`, `conversation.settings`,
  `conversation.restoring`, `conversation.chooseSession`, `conversation.cached`,
  `switcher.close`, `session.sidebar`, `host.row.<hostId>`, `session.row.<sessionId>`,
  `sessions.cached`, `sessions.loading`, `sessions.error`, `sessions.empty`,
  `sessions.retry`, `message.<id>`, `tool.expand.<blockId>`,
  `code.copy.<block>-<index>`, `approval.card.<id>`, `approval.allowOnce`,
  `approval.reject`, `conversation.jumpToLatest`,
  `conversation.connectionStatus`, `composer.field`, `composer.send`,
  `composer.stop`, `diagnostics.preview`, `settings.coreRevision`.
- State is conveyed by text and shape as well as color; semantic system colors
  support light and dark appearance; Dynamic Type is allowed through
  `.accessibility5`. The persistent sidebar uses a native sidebar list over
  `.regularMaterial`, not a hardcoded light color, and selection is carried by
  checkmarks and `Selected`/`Last opened` text as well as color.
- At accessibility sizes and for transcripts of 50 messages or fewer the
  transcript uses an eager `VStack` because `LazyVStack` did not reliably
  realize rows after a large text-size change or an adaptive session
  replacement; longer histories remain lazy.
- The app supports iPhone portrait and both landscape orientations; iPhone
  upside-down remains disabled. iPad supports all four orientations. Mac
  Catalyst windows are freely resizable and can cross the 720 pt breakpoint.

### Session switcher

Sessions are the primary switcher content. The current Host is one compact
`Menu` picker row; sessions are grouped by workspace derived from `cwd` and
shown as one-line rows (running state, title, system-rendered last-active time).
A single workspace renders as a flat list. The sheet uses a plain list for
density; the wide sidebar keeps `.sidebar` styling.

### Diagnostics visibility

`conversation.diagnostics` is compiled behind `#if DEBUG`. Unit and UI plans run
Debug builds, so the flow stays covered; Release builds omit the toolbar entry
point.

## Tests

- Unit (`WhaleReinTests`, Swift Testing): 234 tests covering invitation parsing,
  pairing state machine and queued deep links, conversation-first launch
  restoration, background/foreground suspend and resume, preference migration and
  corrupt-payload recovery, remote revocation and re-pair routes, switcher
  Host/session selection and error/empty states, Host/session scoping and
  stale-watch filtering, delivery policy, optimistic-send echo deduplication,
  send/stop uncertainty reconciliation, clock-driven approval expiry with a
  server-authoritative fast-clock guard, approval refresh merge rules
  (decided/terminal cards survive pending-only lists), offline/concurrent
  approval actionability, draft lifecycle and flush on close/background,
  diagnostics redaction/preview/export, localization completeness and plurals,
  Chinese error/empty states, protocol compatibility including legacy peers, recoverable locked/unavailable
  Keychain launches,
  scroll anchor/jump logic, the 720 pt adaptive breakpoint (below/at/above and
  representative device widths including Split View widths), Markdown tables and
  the documented link/image policy, the parsed-block cache, and fixture
  determinism.
- UI (`WhaleReinUITests`, XCTest/XCUIAutomation): 43 tests covering the pairing
  walkthrough into the restored conversation, background resume with draft
  retention, remote revocation pair-again/forget, watch-ended reconnect, uncertain
  send, echoed-send dedupe, offline/fast-clock/short-lived/unavailable approvals,
  decided-card survival across a pending-only refresh, legacy protocol upgrade
  guidance, add-Host from settings, revoke confirm/cancel, diagnostics failure
  retry, locked-Keychain recovery guidance, reading older rows while streaming
  with anchor retention, send while
  idle and running, tool expansion, code copy confirmation, jump-to-latest,
  approval allow/reject/changed/expired, Host and session switching through the
  switcher sheet with a late event, iPhone landscape with the persistent sidebar,
  iPad wide sidebar, diagnostics cancel, forget device, English and Simplified
  Chinese, large text with dark appearance, and accessibility
  labels/values including unread-count meaning and Host-qualified session rows.
  `setUp` forces portrait; landscape and iPad scenarios rotate explicitly. The
  iPad scenario skips with an explicit reason on an iPhone destination; run it
  with an iPad destination (see below).
- Integration (`WhaleReinIntegrationTests`): 21 XCTest cases named for
  AC-01..AC-11 that exercise real pairing, Host/session listing, cached history
  replay, preview then final, send/steer/queue, approvals, drafts,
  restart/revocation, diagnostics redaction and MQTT loss with HTTPS available.
  Each case skips with an explicit reason when the stack environment is absent.
  The recorded run passed 21/21 with 0 skipped; the bundle is
  `.local-docs/acceptance/add-ios-client/xcode/Integration.result`.
  CI owns this plan through the nightly schedule and the `integration` manual
  dispatch suite. Hosted macOS runners have no Docker daemon, so the job reads
  `vars.WHALEREIN_IOS_STACK_RUNNER` for a self-hosted macOS runner with Docker
  and Xcode. The Unit plan's `ContractParityTests/testLocalStackParity` remains
  an explicit skip; the live contract comparison is this stack-backed plan, not
  the placeholder.

## Known limitations

- No iOS 18 simulator runtime is installed, so Simulator evidence is from iOS
  26.4.1 against a deployment target of 18.0. No physical-device or iOS 18
  evidence exists.
- The required destination string `OS=26.4` does not match the installed
  `26.4.1` runtime; verification commands use `OS=26.4.1`.
- The system share sheet is presented but the UI plan cancels the confirmation
  instead of completing a share; export content is verified by unit and
  integration tests.
- The local stack is loopback plaintext HTTP/MQTT with FakeDSH and one fixture
  session. It does not establish TLS, multi-Host or real DSH persistence.
  Cross-session watch switching cannot be exercised with one session; the watch
  replacement/ordering variant is covered by this suite and the cross-session
  variant by Core and UI fixture evidence.
- Paste-board reading is implemented but not exercised by UI tests because iOS
  may show a system paste prompt. Link opening is covered by unit tests and the
  `onOpenURL` wiring; the UI plan types the invitation into the field.
- App icon artwork is an empty placeholder set; this does not affect tests.
- Textual was not adopted; the app uses a small native Markdown renderer
  (`MarkdownParser` + `MarkdownView`).
- UI test execution adds `-AppleLanguages (en)` for English runs so results do
  not depend on the host simulator language; Simplified Chinese runs pass
  `-AppleLanguages (zh-Hans)`.
- No push notifications are implemented. A remote approval or a watch ending
  while the app is backgrounded is only visible after the app returns to the
  foreground and resyncs; there is no notification permission, APNs token or
  background delivery. The in-app `onOpenURL` path and the queued-link behavior
  are covered by unit tests; the UI plan cannot inject an OS URL delivery, so
  the deep-link delivery itself has no UI-automation evidence.
- iPad wide-layout evidence requires an iPad destination; the iPhone UI plan
  skips `WideLayoutUITests` with an explicit device reason. The sidebar uses the
  same 720 pt breakpoint as iPad Split View and Stage Manager windows; a 500 pt
  Split View column falls back to the compact switcher by design.
- Link policy: only `http`, `https` and `mailto` links are tappable. Other
  schemes (`javascript:`, `file:`, custom schemes) are rendered as plain text.
  Remote images are replaced with their alt text and never fetched. Raw HTML is
  displayed literally and not rendered. Pipe tables render as a native grid.
- Approval expiry is server-authoritative. The local timer asks the Host to
  refresh at the deadline; a Host that still reports the request pending wins
  over a fast local clock. Offline, the locally expired request is shown as
  expired and decisions stay disabled.
- The authoritative app document is not authoritative for pairing state; Core
  still owns the device identity. Legacy root `preferences.json`/`core/` are
  deliberately ignored by the new layout, so an upgrade enters fresh Cloud
  onboarding without reading or deleting them.
- A locked Keychain cannot be reproduced on the Simulator (its Keychain is
  always unlocked). The locked/unavailable classification, the recoverable
  service wrapper and the UI state are covered by injected failures; a real
  reboot-before-first-unlock pass needs a physical device and is an
  environmental limitation, not a skipped release check.
- Mac Catalyst has build/link, launch, screenshot and resize evidence (a 1000 pt
  wide window shows the sidebar; a 550 pt window falls back to the compact
  switcher). The Unit and UI plans are executed on the iOS Simulator; no Catalyst
  test plan or signed Mac distribution is part of this milestone.
- `build-xcframework.sh` does not emit a Catalyst slice by itself. Run
  `apps/ios/scripts/build-catalyst-slice.sh` after it before building Catalyst;
  the slice is generated and gitignored like the other binding artifacts.
- No explicit minimum Catalyst window size is imposed because a hard minimum
  would block resizing below the breakpoint. The default window opens wide
  enough for the sidebar and can be resized narrower to exercise the sheet.
