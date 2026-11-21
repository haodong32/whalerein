# WhaleRein mobile bindings

UniFFI 0.29 binding between WhaleRein Core and the iOS app. The crate is
`whalerein-mobile` (library `whalerein_mobile`); Swift imports the module
`WhaleReinMobile`.

The crate projects Core's transport-neutral state into flat, typed values. It
does not implement protocol, pairing, cache, draft or command logic: those stay
in `core/`. The app implements the bindings' `SecretStore` with the Keychain.

## Layout

```
apps/mobile-bindings/
  build.rs                    revision/Core-version provenance
  uniffi.toml                 Swift module names
  src/lib.rs                  uniffi::setup_scaffolding!("WhaleReinMobile")
  src/secret.rs               foreign SecretStore + MemorySecretStore
  src/client.rs               MobileCore, runtime, pairing/watch, event queue
  src/app_config.rs           typed app document, instance materialization, CA staging
  src/types.rs                exported records/enums and total conversions
  src/error.rs                flat MobileError and Core error mapping
  src/bin/uniffi-bindgen.rs   bindings generator entry point
  src/bin/revision.rs         prints the embedded revision
  swift/WhaleReinMobileError.swift   stable `code`/`message` accessors
  scripts/build-xcframework.sh       XCFramework + Swift binding build
  tests/mobile_core.rs               fixture-backed behavior tests
```

## Build

```bash
# Rust checks
cargo fmt --all -- --check
cargo clippy -p whalerein-mobile --all-targets -- -D warnings
cargo test -p whalerein-mobile

# Generated iOS artifacts (requires Xcode; DEVELOPER_DIR is set automatically
# to /Applications/Xcode.app/Contents/Developer when present)
bash apps/mobile-bindings/scripts/build-xcframework.sh

# Optional Intel Simulator slice
SIM_TARGETS="aarch64-apple-ios-sim x86_64-apple-ios" \
  bash apps/mobile-bindings/scripts/build-xcframework.sh
```

The script builds device (`aarch64-apple-ios`) and Simulator
(`aarch64-apple-ios-sim` by default) static libraries with
`--no-default-features`, generates Swift bindings from a host library built in
the same run, assembles `WhaleReinMobileFFI.framework` per platform, and runs
`xcodebuild -create-xcframework`. Output paths:

- `apps/ios/Vendor/WhaleReinMobile.xcframework` (contains
  `WhaleReinMobileFFI.framework`)
- `apps/ios/Vendor/WhaleReinMobile/WhaleReinMobile.swift`
- `apps/ios/Vendor/WhaleReinMobile/WhaleReinMobileError.swift`
- `apps/ios/Vendor/WhaleReinMobile/WhaleReinMobile.revision`
- `apps/ios/Vendor/revision.txt`

`apps/ios/Vendor/` is generated and gitignored. Link
`WhaleReinMobile.xcframework` and add both Swift files from
`apps/ios/Vendor/WhaleReinMobile/` to the app target; the C module comes from
the framework. Core's `keyring` feature stays off; `test-support` is enabled
only by dev-dependencies in Rust tests.

## Provenance

`coreRevision()` returns a build-time value from `build.rs`:

```
git:<12-char HEAD>[-dirty] core:<Core package version>
```

`git:unknown core:<version>` is the fallback when git metadata is unavailable.
The same string is written to `WhaleReinMobile.revision` and `revision.txt`.
`DiagnosticsReportView.coreVersion` is the Core package version, independent of
the git marker.

## Swift API surface

Module `WhaleReinMobile`; C module `WhaleReinMobileFFI`. Errors are
`MobileError` (or `SecretStoreError` from a secret store implementation).

```swift
// Errors: MobileError has `code: String` and `message: String` from the
// companion source file. Cases: InvalidInput, NotPaired, NotFound, Revoked,
// HostOffline, Unavailable, Conflict, Timeout, Cancelled, Security, Internal.

func coreRevision() -> String

// Authoritative app document at Application Support/WhaleRein/client/app.toml.
// Mutators return the new AppConfigView; every write is atomic and a failed
// save/bootstrapped selection leaves the previous document in place.
func appConfigBootstrap(path: String) throws -> AppConfigView
func appConfigRead(path: String) throws -> AppConfigView
func appConfigValidateInstance(path: String, input: InstanceInput) throws -> AppConfigView
func appConfigSaveInstance(path: String, input: InstanceInput) throws -> AppConfigView
func appConfigSetActive(path: String, id: String) throws -> AppConfigView
func appConfigRemoveInstance(path: String, id: String) throws -> AppConfigView
func appConfigSetReasoningVisible(path: String, visible: Bool) throws -> AppConfigView
func appConfigRemoveCA(path: String, id: String) throws -> AppConfigView

final class MobileCore {
    convenience init(dataDir: String, secretStore: SecretStore, appVersion: String) throws
    static func newWithMemorySecretStore(dataDir: String, appVersion: String) throws -> MobileCore
    func coreRevision() -> String

    // Pairing
    func pairingStart(invitation: String) throws -> PairingPromptView
    func pairingDecide(approved: Bool, expectedFingerprint: String?) throws
    func pairingCancel()

    // Hosts, sessions, connection
    func hosts() throws -> [HostSummary]
    func sessions(hostId: String) throws -> [SessionSummaryView]
    func cachedSessions(hostId: String) throws -> [SessionSummaryView]
    func cachedMessages(hostId: String, sessionId: String) throws -> [MessageView]
    func capabilities(hostId: String, sessionId: String) throws -> SessionCapabilitiesView
    func connectionState(hostId: String) -> String

    // Observation
    func startWatch(hostId: String, sessionId: String) throws
    func stopWatch()
    func pollEvents(timeoutMs: UInt64) -> [ConversationEvent]

    // Commands and approvals
    func send(hostId: String, sessionId: String, text: String,
              delivery: DeliveryChoice) throws -> CommandView
    func stop(hostId: String, sessionId: String) throws -> CommandView
    func approvals(hostId: String, sessionId: String) throws -> [ApprovalView]
    func resolveApproval(hostId: String, sessionId: String, approvalId: String,
                         version: Int64, digestHex: String, allow: Bool) throws -> CommandView

    // Drafts
    func saveDraft(hostId: String, sessionId: String, text: String) throws
    func loadDraft(hostId: String, sessionId: String) throws -> String?
    func clearDraft(hostId: String, sessionId: String) throws

    // Diagnostics and lifecycle
    func diagnosticsPreview() throws -> DiagnosticsReportView
    func exportDiagnostics(path: String) throws -> DiagnosticsReportView
    func forgetDevice() throws
    func revoke() throws
}

protocol SecretStore: AnyObject, Sendable {
    func load(key: String) throws -> Data?
    func save(key: String, value: Data) throws
    func delete(key: String) throws
}

final class MemorySecretStore {          // Rust object; process-local, test only
    convenience init()
    func count() -> UInt64
    func load(key: String) throws -> Data?
    func save(key: String, value: Data) throws
    func delete(key: String) throws
}
```

App configuration records: `AppSettingsView { activeInstanceId, reasoningVisible }`,
`InstanceView { id, service, environment, dataDir, configPath, hasStandaloneCA,
standaloneCAPath }`, `AppConfigView { path, version, app, instances }`,
`InstanceInput { id, service, environment, dataDir, standaloneCAPEM }`.

Records: `HostSummary { hostId, online, fingerprint, protocolVersion }`,
`SessionSummaryView { sessionId, title, updatedAtUnixMs, cwd, running,
capabilities: SessionCapabilitiesView? }`,
`SessionCapabilitiesView { history, resumableHistory, liveText, interrupt,
approvals, liveReasoning, toolActivity, conversationNotices, attachments,
historyPage, historyExact, sendFollowup, deliveryModes: [String],
modelSelection, pendingQueueRead, pendingQueueEdit, pendingQueueRemove,
pendingQueueSteer, questionsRead, questionsAnswerOpen,
questionsAnswerContinued, sessionFork }`,
`PairingPromptView { hostId, accessUrl, fingerprint, fingerprintDisplay }`,
`ApprovalView { approvalId, sessionId, toolName, callId, reason, version,
expiresAtUnixMs, digestHex, state }`, `MessageView { messageId, role, blocks,
createdAtUnixMs }`,
`BlockView { blockId, kind, text, toolName, toolActivity: ToolActivityView?,
notice: NoticeView? }`, `ToolActivityView { callId, name, summary, state,
stages: [String], progress, failureCode, result: [ToolResultPartView],
callMissing }`, `ToolResultPartView { text | attachment(mediaType, name, size,
available) }`, `NoticeView { category, state, summary }`,
`CommandView { requestId, commandId, status, messageId, deliveryId }`,
`SessionKeyView { hostId, sessionId }`, `MessageReferenceView { session,
messageId, projectionId }`, `DiagnosticsReportView { json, redactionFields,
lastFailureCode, appVersion, coreVersion }`.

Enums: `DeliveryChoice { steer, queue, send }`;
`MessageReplyView { pending, committed(reference: MessageReferenceView),
inputRequired(kinds: [String], operation, session: SessionKeyView), uncertain }`;
`ConversationEvent { state(runPhase:running:revision:approvals:), historyReset,
message(message:), preview(attemptId:kind:text:replace:incomplete:),
toolActivity(activity:), attemptEnded(attemptId:outcome:),
connection(hostId:status:), pairingPrompt(prompt:),
pairingFinished(hostId:code:), commandStatus(command:),
approvalChanged(approval:), questions(view:), error(code:message:),
droppedEvents(count:), watchEnded }`.

Semantics:

- `appConfigBootstrap` creates `cloud-production` (official production) and
  `standalone` only when the document is absent; an existing document is never
  overwritten and legacy `preferences.json`/`core/` files are never read,
  migrated or deleted. `InstanceInput.dataDir` and the CA reference are relative
  to the app document; `InstanceView.dataDir`, `configPath` and
  `standaloneCAPath` are absolute for Swift.
- `appConfigSetActive` writes the materialized
  `client/instances/<id>/instance.toml` first and then commits
  `app.active_instance`, so a failed write leaves the previous selection. A
  valid Standalone PEM is validated through Core trust, staged under the
  instance directory, and replaces `ca.pem` only after the document commits;
  invalid PEM changes nothing. Editing one field preserves `transport` and every
  other instance entry.
- `send` maps `send` to start-if-idle, `queue` to enqueue-after-run and `steer`
  to steer. Delivery capability remains a Source decision; the app chooses the
  mode.
- `resolveApproval` submits exactly the reviewed `version` and `digestHex`.
  `digestHex` is the 32-byte approval digest rendered as hex.
- `cachedMessages` reads Core's encrypted local cache only (no channel), in
  committed order. An empty cache returns `[]`; an unpaired profile throws
  `not_paired`. On `historyReset`, replay these before live updates and dedupe
  by `messageId`. It shares the exact `MessageView`/`BlockView` projection used
  by live `message` events.
- `capabilities` resolves the session through the Host's session list; an
  absent session throws `not_found` and a Source that reported no capabilities
  throws `unavailable`. `deliveryModes` contains stable lowercase values
  (`start_if_idle`, `enqueue_after_run`, `steer`) and preserves unknown modes in
  lowercase. `state.running` is true while `runPhase` is `running` or
  `stopping`; `runPhase` distinguishes them.
- `pollEvents` blocks the calling thread up to `timeoutMs` (capped at 30 s) and
  returns an empty array on timeout; call it off the main thread.
- `send`, `stop` and `resolveApproval` return their `CommandView` and also queue
  a `commandStatus` event. The queue is bounded at 512 events; overflow drops the
  oldest and reports `droppedEvents(count:)` before the retained events.
- `startWatch` replaces an earlier watch; stale updates are discarded.
  `stopWatch` aborts the Rust task and emits `watchEnded`.
- `pairingStart` waits up to 20 s for Core's Noise-verified prompt and returns
  it (also queued as `pairingPrompt`). A timeout is typed; the task continues
  and can still deliver `pairingPrompt`/`pairingFinished`.
- `revoke` blocks until Core confirms revocation, then stops the watch.
  `forgetDevice` also stops the watch; Core requires the profile to be revoked
  (or unpaired) first, so a paired unrevoked profile returns `conflict`.
- `role` values are `user`, `assistant`, `tool`, `notice`, `context`,
  `unknown`. `context` and `unknown` never enter the user lane. `kind` values are
  `text`, `reasoning`, `tool`, `notice`, `attachment`, `unknown`; reasoning keeps
  its own block identity and is never merged into an answer block. `runPhase`,
  `outcome`, tool `state`/`stages` and approval `state` use `unknown` for
  unrecognized protocol values.
- Tool blocks carry the canonical typed activity in `toolActivity`; Core has
  already merged repeated call/result occurrences by `callId`. `text` is the
  settled result text (or the call summary/progress before settlement) so a flat
  renderer keeps working. Tool `state` values are `preparing`, `running`,
  `succeeded`, `failed`, `interrupted`, `abandoned`, `unknown`; stages are
  `preparation`, `execution`, `progress`, `settlement`, `unknown`.
- Notice blocks carry only `summary` as their display body plus typed
  `category` (`retry`, `max_output`, `compaction`, `command`, `context`,
  `omission`, `unknown`) and `state` (`scheduled`, `started`, `cancelled`,
  `completed`, `failed`, `reached`, `unknown`). A missing summary stays empty;
  the binding never invents notice text.
- `preview.kind` is `text` or `reasoning`; callers must not append a reasoning
  preview to answer text. `toolActivity` events report one material change to a
  canonical activity; `message` events remain the committed projection.
- `MessageReplyView` mirrors Core `SendReply`: `pending`, `committed` with a
  scoped `MessageReferenceView`, `inputRequired` with evidenced lowercase
  `kinds` (`approval`, `question`, `other`), or `uncertain`. A committed reply
  names the canonical message; its body stays in history.
- `DiagnosticsReportView.json` uses Core's redacted preview/export. It excludes
  message and draft text, tool arguments, credentials, invitation/device tokens,
  private keys, full ciphertext and raw Host/device ids.

`MobileError` codes: `invalid_input`, `not_paired`, `not_found`, `revoked`,
`host_offline`, `unavailable`, `conflict`, `timeout`, `cancelled`, `security`,
`internal`.

## Contract notes

- UniFFI 0.29 requires trait arguments to be `Arc<dyn SecretStore>`; Swift still
  sees the `SecretStore` protocol, so the `Box` in the plan is an FFI detail.
- `ApprovalView` exposes `digestHex` in addition to the plan's fields because
  `resolveApproval` must bind the reviewed digest.
- `pairingPrompt`, `pairingFinished`, `state`, and `commandStatus` carry their
  payloads; `droppedEvents` is the explicit loss counter required for a bounded
  queue.
- `MemorySecretStore` is a UniFFI object, not a foreign trait implementation, so
  Swift cannot pass it directly as `SecretStore`; use
  `newWithMemorySecretStore` or wrap its methods in a `SecretStore`
  conformance. No `unwrap`/panic path exists on FFI input paths.
- The typed conversation contract is a hard cut. `BlockView` gained
  `toolActivity` and `notice`, `ConversationEvent` gained `toolActivity` and a
  `kind` on `preview`, and `MessageReplyView` mirrors Core `SendReply`; a client
  compiled against the previous contract must be rebuilt. Independent history
  pages (`HistoryPage`) are not exposed by these bindings.
- iOS feature and UI work remains deferred for this release (design D-12). This
  crate only performs workspace compile maintenance, so the app must not be
  treated as validated against the new typed values until that work resumes.

## Security

No logs are emitted. No `unwrap` is used on FFI inputs; ids, text, invitation
and fingerprint lengths are validated before use. `MobileError` messages are
fixed neutral strings and never include Core error text.
