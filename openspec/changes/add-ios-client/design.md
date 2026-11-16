# Design

## Context

The reference baseline is upstream `origin/main` at `858e4f16`. It contains
the CLI, Core's typed conversation and recovery, and explicit
`send_message_with_delivery` with Source capability reporting. The mobile
app and its Swift binding are new. Host enrollment and an iOS account
sign-in, discovery, and Device authorization journey have separate acceptance
needs. See the [Core architecture](../../../docs/architecture/core.md),
[Harness contract](../../../docs/architecture/harness-contract.md), and
[Access evolution](../../../docs/architecture/access-evolution.md).

## Goals / Non-Goals

- Pair an iPhone, open an existing session, and operate it through loss of
  connectivity. Deliver the conversation UI first with fixture data.
- Preserve Core behavior across the CLI and mobile clients and reuse the
  current Access TLS transport and DSH ownership model.
- Deliver iOS independently. Android, HarmonyOS, hosted login, and mobile
  push integration have separate delivery work.

## Decisions

### Architecture review

| Owner | Responsibility |
| --- | --- |
| `apps/ios/` | SwiftUI App, NavigationStack and Observation, screen and keyboard state, launch and notification routing as capabilities are added, native controls, Keychain adapter, String Catalogs, and accessibility. The iOS deployment target is 18. |
| `apps/mobile-bindings/` | UniFFI exports, type conversion, cancellation and resource lifetime. Package device and Simulator slices as an XCFramework with Swift bindings produced from the same pinned Rust revision. |
| `core/` | Device identity, trust, asynchronous pairing confirmation, typed session observation, encrypted cache and drafts, command routing/recovery, delivery preference and diagnostics. Reuse the existing SecretStore interface. |
| Access and Host | Access enforces grants and relays encrypted content. The Host and Source own execution, receipt and authoritative session state. |

Dependencies run from iOS to bindings to Core to the existing public protocol.
Swift consumes typed state and forwards explicit actions. It owns rendering,
scroll, selection and focus. Use typed, Host-qualified session identity
throughout. Reuse the existing approval and command paths. Core owns snapshot
and watch handoff, and leaves an uncertain operation recoverable when a view
closes. No new network schema is expected for the baseline journey.

Use an iOS sandbox and device-local Keychain storage for secrets. Protect
durable drafts and cached history, exclude sensitive migration material from
backups, and keep profile, Host, and session identities scoped. At confirmed
revocation, clear the affected local view and credential material through Core.
Exclude content, invitations and credentials from exported evidence.

For the conversation, use native controls and lazy rows with stable IDs.
Generate live previews as system text and render committed assistant Markdown
at the SwiftUI presentation layer. Try [Textual](https://github.com/gonzalezreal/textual)
with the same fixtures on iOS 18 before pinning it. Use native views for tools
and approvals. Keep the composer and individual row state independent of live
updates elsewhere in the transcript.

### Decision Review

| ID | Decision and options | Status and basis | Affected |
| --- | --- | --- | --- |
| D-01 | iOS 18 with SwiftUI and a shared Rust/UniFFI binding. A shared UI runtime would reduce UI duplication across future ports and conflicts with the agreed native direction. | CONFIRMED. iOS-only first version and native UI, minimum OS and Core ownership agreed. | AC-01, AC-12, AC-13 |
| D-02 | Default iOS running sends to Steer, with explicit Queue and idle Send. Keep existing CLI defaults by sending a mode from iOS. | CONFIRMED for mobile. Capability gating follows the existing upstream contract. | AC-06, AC-07 |
| D-03 | Unit tests, stack integration and native platform evidence are distinct. Use Swift Testing for Swift units and XCTest/XCUIAutomation for UI automation. | CONFIRMED for unit and integration scope, CONSTRAINED by the test policy for Rust coverage and OS-specific evidence. | AC-01–AC-13 |
| D-04 | Recommend first delivering an invited Standalone iOS beta, then the official hosted beta. An alternative is to wait and release the first public App Store build with both paths, which ties it to public account integration and APNs. | PENDING for whether a Standalone-only build may go public on the App Store. The Standalone beta and its independent test scope can be reviewed now. | Store promotion only |
| D-05 | Verify Textual package/toolchain compatibility, Swift/native TLS trust and ALPN, and real DSH approval descriptions. Use the native text baseline in fixtures and collect frame times, memory and input latency with long history. | FEASIBILITY. No iOS app has been built. Record the tested setup, measurements and any blocking gap before adopting a dependency or setting a numeric release target. | AC-01, AC-03, AC-05, AC-08, AC-12, AC-13 |

### Verification and delivery gates

- **Unit.** Rust Core and binding tests cover state, profile and Host scope,
  operation recovery and cancellation using isolated fixtures. Use the
  repository's 90% line coverage requirement for affected Rust crates.
  Swift Testing covers presentation state, localized error/status selection,
  flow wiring, and using the server-confirmed outcome. Collect Swift app
  coverage for review.
- **iOS integration.** On macOS, use Xcode test plans. The stack plan runs a
  signed-for-Simulator app target with the real UniFFI library, a disposable
  Keychain namespace, and isolated stock Access, PostgreSQL, Mosquitto and
  a deterministic Host fixture. Use separate TLS fixture trust with explicit
  negative cases. The UI plan drives SwiftUI with deterministic input and
  tests page transitions and retained context through XCTest/XCUIAutomation.
- **Upstream integration.** Run the existing full Linux integration suite on
  the exact candidate for affected cross-component changes. Use the pinned
  native DSH cases to recheck delivery and approval semantics. The Linux
  suite and iOS stack plan cover different runtime surfaces. Review
  [testing policy](../../../docs/testing.md) and
  [native fixture scope](../../../tests/integration/README.md#native-dsh-and-browser-journeys).
- **iPhone acceptance.** Run the signed device artifact on iOS 18 and a
  current supported version. Check Keychain and backup behavior, invitation
  dispatch, HTTPS and native MQTT TLS/ALPN, background/foreground recovery,
  accessibility, and scrolling under load. Record app build, Core revision,
  backend versions and OS. If an environment is unavailable, retain the
  result as outstanding.
- **Standalone beta.** Device cohort and candidate artifact require all
  applicable AC below. D-04 governs public promotion. The later official
  beta has a separate end-to-end gate for system-browser Passkey, Device
  proof and revocation, and APNs lock-screen routing and stale notifications.

### Acceptance Review

| ID | Observable outcome | Method | Scenario |
| --- | --- | --- | --- |
| AC-01 | Device and Simulator artifacts load the recorded Core revision. | Swift integration smoke test and physical iPhone launch | [AC-01](specs/ios-session-client/spec.md#scenario-ac-01-device-and-simulator-builds-use-the-same-core-revision) |
| AC-02 | Open or paste an invite, confirm the Host, and reach the list. Resume a lost result or show expiry. | Rust unit, Xcode stack integration, and physical iPhone pairing | [AC-02](specs/ios-session-client/spec.md#scenario-ac-02-pair-or-resume-the-same-invitation) |
| AC-03 | Decline, cancellation, late consent, bad fingerprint and invalid certificate leave grants unchanged. | Rust unit and Xcode stack integration | [AC-03](specs/ios-session-client/spec.md#scenario-ac-03-decline-or-reject-invalid-pairing) |
| AC-04 | Switch Hosts, restore the last selection, and keep old-watch updates out of the visible session. | Rust unit and XCTest UI tests over Simulator | [AC-04](specs/ios-session-client/spec.md#scenario-ac-04-navigate-across-hosts-and-sessions) |
| AC-05 | Load history, stream live output, recover a lost connection and resume after suspension without duplicates. | Rust unit, Xcode stack integration, native DSH and iPhone | [AC-05](specs/ios-session-client/spec.md#scenario-ac-05-observe-and-recover-the-transcript) |
| AC-06 | Idle send starts work. Running send defaults to Steer, explicit Queue is honored, and unsupported modes retain the draft. | Rust and Swift unit, Xcode stack integration, native DSH | [AC-06](specs/ios-session-client/spec.md#scenario-ac-06-send-while-idle-or-choose-a-supported-running-delivery-mode) |
| AC-07 | A lost send or stop reply is reconciled once. Show stop settlement when the Source confirms it. | Rust unit, Xcode stack integration, native DSH | [AC-07](specs/ios-session-client/spec.md#scenario-ac-07-recover-an-uncertain-send-or-stop) |
| AC-08 | Allow Once and Reject use the reviewed version and update the original card. | Rust and Swift unit, Xcode stack integration, native DSH and iPhone | [AC-08](specs/ios-session-client/spec.md#scenario-ac-08-resolve-a-current-approval) |
| AC-09 | Expired, changed, and already-resolved requests refresh safely. | Rust unit, Xcode stack integration and native DSH | [AC-09](specs/ios-session-client/spec.md#scenario-ac-09-handle-an-expired-changed-or-resolved-approval) |
| AC-10 | Restart preserves local identity and draft. Scope changes, revocation, and backup restore enforce isolation. | Rust unit, Xcode stack integration and physical iPhone | [AC-10](specs/ios-session-client/spec.md#scenario-ac-10-preserve-only-authorized-data-across-the-device-lifecycle) |
| AC-11 | Diagnose an MQTT failure with HTTPS still available, and share only confirmed, redacted output. | Rust unit, Swift unit and Xcode stack integration | [AC-11](specs/ios-session-client/spec.md#scenario-ac-11-export-connection-details-with-user-confirmation) |
| AC-12 | Keep the selected reading and editing state while streaming, and support copying and jumping to the latest output. | XCTest UI tests and iPhone Instruments run | [AC-12](specs/ios-session-client/spec.md#scenario-ac-12-read-a-long-conversation-while-new-output-arrives) |
| AC-13 | Both languages and appearances support essential flows with VoiceOver and large text. | Swift unit, XCTest UI tests and physical iPhone walkthrough | [AC-13](specs/ios-session-client/spec.md#scenario-ac-13-complete-the-core-flows-with-accessibility-settings) |

## Risks / Trade-offs

iOS cross-compilation, actual native certificate loading and Keychain behavior
are feasibility checks. Keep the desktop keyring selection intact and verify
device/simulator separation. Use an explicit test fixture and storage scope;
the phone must still validate the server certificate and Host identity.

Current approval metadata may be too sparse for some native requests. Review
representative real DSH approvals. If a case needs a clearer authoritative
summary, extend the shared approval model through a reviewed public change
before offering resolution for that case.

## Migration Plan

The first iOS app has no existing mobile data to migrate. Use an isolated
staging application identity. Preserve valid identity, grants, drafts and
recoverable operation IDs when exercising an in-place upgrade between beta
builds. Verify the existing CLI and service combination before advancing.
