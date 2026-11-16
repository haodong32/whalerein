# Proposal

## Why

WhaleRein users need to follow and operate an existing Host session from an
iPhone. The conversation is the primary screen. It should preserve reading and
editing context while output streams and while the connection recovers.

## What Changes

- Deliver a native SwiftUI app for iOS 18 and later, using the public Rust Core
  through a shared UniFFI crate. Build the conversation first with synthetic
  fixtures, then connect it to a real self-hosted session.
- Support Standalone invitation pairing, Host and session selection, history and
  live output, sending, interruption, and approval review and resolution. While
  a run is active, default to Steer and offer Queue where the Source supports
  the chosen mode.
- Preserve encrypted drafts and recoverable local state through app restarts.
  Provide connection details and a previewable, redacted diagnostics export.
- Ship Simplified Chinese and English, VoiceOver support, Dynamic Type, and
  automatic system appearance. Use the ChatGPT iPhone conversation experience
  as a layout reference and implement it with native controls.
- Add Rust and Swift unit tests, iOS Simulator integration and UI tests, real
  DSH integration, and physical iPhone release evidence.

The proposed first milestone is an independently usable Standalone iOS beta.
Official Passkey sign-in, automatic pairing and lock-screen approval alerts
belong to the official iOS beta and depend on the public external-account
contract and hosted integration. Their timing relative to public App Store
release is recorded in [D-04](design.md#decision-review). Android, HarmonyOS,
mobile session creation, attachments, and queue management are outside this
change.

## Capabilities

### New Capabilities

- `ios-session-client`: Pair an iPhone with a Standalone Host and operate its
  existing sessions with native conversation, recovery, and accessibility.

### Modified Capabilities

None. The iOS composer selects an explicit delivery mode through Core. Existing
CLI and wire defaults keep their established behavior.

## Impact

- `apps/ios/` for the Xcode app, native system adapters, resources and tests.
- `apps/mobile-bindings/` for the reusable Rust/Swift interface and tests.
- `core/` for shared pairing, observation, draft and diagnostic behavior
  required by the app.
- Existing CI and integration tooling for macOS/Xcode checks and the isolated
  Linux service suite. The iOS app uses stock Access and the DSH connector.

See [Core architecture](../../../docs/architecture/core.md),
[Harness contract](../../../docs/architecture/harness-contract.md), and
[testing policy](../../../docs/testing.md).
