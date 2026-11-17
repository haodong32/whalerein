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

## Build

Requirements: macOS with Xcode 16.4 or later, Rust with the
`aarch64-apple-ios` and `aarch64-apple-ios-sim` targets. Mac Catalyst also needs
`aarch64-apple-ios-macabi`
(`rustup target add aarch64-apple-ios-macabi`).

```bash
bash apps/mobile-bindings/scripts/build-xcframework.sh
```

The script builds the Rust library for device and Simulator with
`--no-default-features` (the app injects the Keychain `SecretStore`), generates
Swift bindings from the same crate revision, and writes
`apps/ios/Vendor/WhaleReinMobile.xcframework` together with a recorded revision.
Generated artifacts stay untracked.

The project compiles the generated `WhaleReinMobile.swift` and
`WhaleReinMobileError.swift` into a dedicated `WhaleReinMobile` framework target
and links the XCFramework there; the app and test targets import the resulting
`WhaleReinMobile` module. The generated binding defines `HostSummary` and
`DeliveryChoice`, so a separate module keeps them from colliding with the app's
domain models. A "Check Vendor" build phase fails with an explicit message when
`apps/ios/Vendor/` is missing; generated files are never vendored into git.

The deployment target is iOS 18.0. The project targets both iPhone and iPad
(`TARGETED_DEVICE_FAMILY = "1,2"`) and enables Mac Catalyst for the app and test
targets. `DEVELOPER_DIR` must point at a full Xcode installation; a
CommandLineTools-only selection cannot build or test the app.

The shared XCFramework build produces iOS device and Simulator slices only.
Mac Catalyst needs a separate Rust slice:

```bash
bash apps/mobile-bindings/scripts/build-xcframework.sh
bash apps/ios/scripts/build-catalyst-slice.sh
```

The second script builds `aarch64-apple-ios-macabi`, adds a Mac Catalyst slice
to the generated XCFramework, and reuses the generated Swift binding. Re-run it
after rebuilding the binding.

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

A paired launch is conversation-first: `AppModel.bootstrapConversation()` loads
the authorized Hosts, picks the remembered `lastHostId` when it is still
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
| `Unit` | `xcodebuild test -project apps/ios/WhaleRein.xcodeproj -scheme WhaleRein -testPlan Unit -destination 'platform=iOS Simulator,name=iPhone 17,OS=26.4.1'` | Swift Testing units for conversation-first restore, switcher state, the 720 pt adaptive breakpoint, presentation state, delivery choice, approvals, drafts, diagnostics and localization. |
| `UI` | same with `-testPlan UI` | XCUITest flows over deterministic fixtures: pairing into a conversation, switcher Host/session changes, iPhone landscape with the persistent sidebar and rotation back, streaming retention, composer, approvals, diagnostics, both languages, accessibility sizes and appearances. |
| `Integration` | `apps/ios/scripts/test-integration.sh` | Stack-backed XCTest journeys over the real Swift-to-UniFFI-to-Core path against a disposable Access, PostgreSQL, Mosquitto and Host fixture. |

`apps/ios/scripts/test.sh` wraps the Unit and UI invocations and records result
bundles under `.local-docs/acceptance/add-ios-client/xcode/`. The dedicated
`test-integration.sh` starts or reuses the stack, mints a fresh invitation,
builds, injects `WHALEREIN_IOS_STACK_*` into the generated `.xctestrun`, and runs
`test-without-building`; it writes `Integration.result`. Test plans and schemes
are shared in the project. UI and unit fixtures are selected with launch
arguments (`-ui-fixtures`, `-integration-fixtures`, `-ui-test-reset`). The
recorded local acceptance run is
`.local-docs/acceptance/add-ios-client/integration-ios.md` (Integration 21/21,
Unit 123/123, UI 19/19).

## Local integration stack

`tools/ios-stack/` starts the disposable backend and writes a state contract
(invitation URI, Host, session) for the `Integration` plan. It exposes a
token-protected loopback control API for fresh invitations, fixture events,
Access restarts, and separate broker stop/start so transport loss can be tested
while HTTPS stays reachable. See its README for the exact environment variables,
lifecycle and limitations. It is intentionally local-only: GitHub macOS runners
cannot run the Docker service stack.

The session switcher is sessions-first. The current Host is a single compact
picker row (a native `Menu`) above the list; sessions are grouped by workspace
(`cwd`'s last path component) with folder section headers, and each row is one
line: running state, title, and a last-active time rendered by the system
(`Text(date, style: .relative)`, or a short absolute date after seven days).
Grouping is skipped when a Host has only one workspace. The compact switcher
uses a plain list for density; the wide sidebar keeps the native sidebar list
style.

Service selection: `-ui-fixtures` keeps the deterministic `FixtureSessionService`;
`-integration-fixtures` and a normal launch select `LiveSessionService`, which
uses the app data directory plus the device-only Keychain namespace
(`com.whalerein.ios.secrets`, staging `.staging`). If Core cannot initialize,
the app shows the localized unavailable state instead of crashing.

`LiveSessionService` restores the encrypted local transcript with
`MobileCore.cachedMessages` on each watch reset, deduplicates committed message
ids against live updates, maps Source `deliveryModes`
(`start_if_idle`/`steer`/`enqueue_after_run`) and `interrupt` without inventing
support, and treats Core's authoritative `state` event as the online signal.
The requested commands are:

```bash
apps/ios/scripts/test.sh Unit     # 123 Swift Testing cases
apps/ios/scripts/test-integration.sh  # 21 stack-backed XCTest cases
apps/ios/scripts/test.sh UI       # 19 XCUITest cases
```

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
the app container. Drafts and cached conversation data are encrypted by Core and
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
- `build-xcframework.sh` does not emit a Catalyst slice; run
  `apps/ios/scripts/build-catalyst-slice.sh` after it. The generated slice is
  gitignored.
- The loopback stack is plaintext HTTP/MQTT with a FakeDSH fixture and one
  session. It does not establish TLS, multi-Host or real source persistence.
- Textual is not adopted in the first baseline. Committed assistant text uses
  native `AttributedString` inline Markdown plus a native block renderer for
  headings, paragraphs, ordered and unordered lists, block quotes, rules and
  fenced code with copy; the package and performance evaluation remains a
  feasibility item.
- Hosted Passkey sign-in, automatic pairing and lock-screen approval alerts are
  outside this client milestone.
