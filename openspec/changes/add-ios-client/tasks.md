# Tasks

Use [Decision Review](design.md#decision-review) and
[Acceptance Review](design.md#acceptance-review) before implementation.
Task checks track completed work. Acceptance results track observed behavior.

## 1. Parallel feasibility and conversation slice

- [ ] 1.1 In `apps/mobile-bindings/` and `apps/ios/`, prove that one Core
  revision builds and loads on Simulator and iPhone; exercise Keychain and
  verified HTTPS/MQTT over TLS through a minimal native probe (AC-01, AC-03,
  AC-05, AC-10). Record toolchain, linked slices, and target evidence.
- [x] 1.2 In `apps/ios/`, build the native conversation from shared synthetic
  fixtures, including long history, live output, composer and approval views.
  Evaluate Textual and exercise reading position, keyboard, selection,
  Markdown copying, and locale/accessibility (AC-04, AC-05, AC-12, AC-13).
  Run the fixture UI plan with `xcodebuild test`.

## 2. Standalone end-to-end baseline

- [x] 2.1 After 1.1, package the XCFramework and UniFFI Swift bindings and
  keep iOS SecretStore behind the existing Core interface (AC-01, AC-10).
  Add platform smoke and isolated Keychain checks.
- [x] 2.2 In `core/` and the binding crate, support asynchronous invitation
  confirmation and a cancellable, Host-qualified session watch with ordered
  snapshot handoff. Reuse pairing and history recovery. Verify cancellation,
  stale confirmation, reconnect and watch replacement with unit tests
  (AC-02–AC-05).
- [x] 2.3 After 2.1 and 2.2, connect the invitation, Host list, session list and
  conversation UI to real Core. Run a Simulator walkthrough with stock Access
  and the isolated Host fixture (AC-02–AC-05, AC-13).

## 3. Control and recovery

These tasks can proceed in parallel after the shared interfaces in section 2.

- [x] 3.1 In Core/bindings and the composer, expose capability-aware send,
  running Steer and Queue, stop settlement and original-request recovery.
  Run supported and unsupported cases and keep the draft available when
  delivery is unconfirmed (AC-06, AC-07, AC-12).
- [x] 3.2 In Core/bindings and the native approval view, resolve only the
  reviewed request variant. Test races using fixtures and clear action
  descriptions using the pinned native DSH path (AC-08, AC-09, AC-13).
- [x] 3.3 Add shared encrypted draft lifecycle and reuse redacted connection
  diagnostics through Core. Verify restart, scope, revocation, and explicit
  preview/share with tests in Core and the app (AC-10, AC-11).

## 4. Automated verification

- [x] 4.1 Register changed Rust crates in existing tooling as needed, wire an
  affected iOS macOS job, and document the shared test schemes and destinations.
  Run `./repo check`, affected Rust unit coverage, and the Swift unit plan.
  Collect an Xcode result bundle and confirm the iOS job detects changes to
  its shared Rust inputs. If tooling changes, also run its checks and coverage.
  Completed: `./repo check`, Core 91.38% and binding 93.01% line coverage, Unit
  plan 123/123, UI plan 19/19, Integration plan 21/21, and `.github/workflows/ios.yml`.
- [x] 4.2 Implement and run the Xcode `Integration` and `UI` test plans using
  a disposable service stack and synthetic data. Bind every planned case
  below to a concrete test or native procedure and retain result bundles.
  Test end to end over the actual Swift-to-Rust interface (AC-01–AC-13).
- [ ] 4.3 Run `./repo test --suite integration` against the candidate baseline,
  including native DSH evidence for delivery, interrupt, recovery and approvals
  (AC-05–AC-09). If the protocol changes, also run `./repo proto --check` and
  the protocol integration group.
  Completed with outstanding results: 489/494 cases passed. Two load-sensitive
  failures passed on isolated rerun; three persistent failures remain in
  unchanged Access storage/server and native-DSH concurrency cases. No protocol
  change was made, so `./repo proto --check` was not required.

## 5. Physical devices and review

- [ ] 5.1 Run the signed candidate on iOS 18 and a current supported iOS version.
  Exercise the physical-device cases in the matrix. Record low-end-device
  scrolling and input observations with the same synthetic transcript, and
  verify iOS beta upgrade and staging separation. Treat a Simulator run as
  Simulator evidence.
- [ ] 5.2 Reconcile current AC evidence and architectural deviations. Review
  whether maintained architecture, build/test, security or usage docs need
  updates based on verified implementation. Keep unexecuted items visible.
  Record the user decision on D-04 before public App Store promotion.

## AC-to-case matrix

Each suffix below is a planned case ID under `ios-session-client/AC-NN`.
All subcases start as NOT RUN. Before execution, bind IDs to an exact test
selector or documented device procedure, record the actual command/device,
and link the evidence from Acceptance Results. Approve an AC only after all
its required cases pass. See the
[acceptance evidence contract](../../README.md#acceptance-evidence-contract).

| AC | Mandatory case suffixes | Verification layers | Owning tasks |
| --- | --- | --- | --- |
| AC-01 | simulator-load, device-load, provenance-match | Swift integration smoke test and physical iPhone launch | 1.1, 2.1, 5.1 |
| AC-02 | open-link, paste, confirm, lost-reply-relaunch, expired-claim | Rust unit, Xcode stack integration, and physical iPhone pairing | 2.2, 2.3, 4.2, 5.1 |
| AC-03 | decline, leave-before-confirm, late-confirm, fingerprint, invalid-tls | Rust unit and Xcode stack integration | 2.2, 2.3, 4.2 |
| AC-04 | sort-and-scope, cold-resume, switch-with-late-event | Rust unit and XCTest UI tests over Simulator | 1.2, 2.2, 2.3, 4.2 |
| AC-05 | history-live-cut, mqtt-loss-https-ok, history-repair, foreground, process-restart | Rust unit, Xcode stack integration, native DSH and iPhone | 1.2, 2.2, 2.3, 4.2, 4.3, 5.1 |
| AC-06 | idle-send, default-steer, explicit-queue, accepted-queue, unsupported-mode | Rust and Swift unit, Xcode stack integration, native DSH | 3.1, 4.2, 4.3 |
| AC-07 | lost-send, lost-stop, runtime-settlement, no-duplicate | Rust unit, Xcode stack integration, native DSH | 3.1, 4.2, 4.3 |
| AC-08 | clear-action, allow-once, reject, original-card | Rust and Swift unit, Xcode stack integration, native DSH and iPhone | 3.2, 4.2, 4.3, 5.1 |
| AC-09 | expiry, changed-version, other-device, fresh-confirmation | Rust unit, Xcode stack integration and native DSH | 3.2, 4.2, 4.3 |
| AC-10 | same-device-restart, profile-host-session-isolation, scoped-revoke, global-revoke, backup-nonmigration | Rust unit, Xcode stack integration and physical iPhone | 2.1, 3.3, 4.2, 5.1 |
| AC-11 | mqtt-https-distinction, redaction, preview, cancel, confirm | Rust unit, Swift unit and Xcode stack integration | 3.3, 4.2 |
| AC-12 | history-prepend, scroll-anchor, expand-and-select, keyboard-and-draft, appearance, copy, jump, performance-baseline | XCTest UI tests and iPhone Instruments run | 1.2, 3.1, 4.2, 5.1 |
| AC-13 | zh-Hans, en, voiceover, accessibility-size, light-and-dark | Swift unit, XCTest UI tests and physical iPhone walkthrough | 1.2, 2.3, 3.2, 4.2, 5.1 |

## Acceptance Results

Evidence record: `.local-docs/acceptance/add-ios-client/acceptance.md`
(Git-ignored). Status follows the evidence contract: only an AC whose mandatory
cases all pass is PASS; an unresolved prerequisite is BLOCKED.

| AC | Status | Evidence | Limitations |
| --- | --- | --- | --- |
| AC-01 | BLOCKED | Integration `testAC01_provenance`; Xcode build/launch; both XCFramework slices record `git:0faad5246f11-dirty core:0.1.0`. | No physical iPhone, so `device-load` is unexecuted. |
| AC-02 | BLOCKED | Integration `testAC02_confirm`, `testAC02_invalidInvitation`; UI pairing flow; upstream lost-reply recovery; Core unit recovery. | Physical iPhone pairing and an executed expired-invitation case are outstanding. |
| AC-03 | PASS | Integration `testAC03_decline`, `testAC03_fingerprint`, `testAC03_invalidInvitation`; binding/Core cancel and late-confirmation tests; CLI invalid-TLS procedure. | Core/Simulator evidence; native device TLS remains under AC-01/AC-05. |
| AC-04 | PASS | Swift Host/session scope and cold-resume wiring tests; UI `testSwitchingHostsDropsLateEventsFromThePreviousWatch`; Integration watch replacement. | Simulator/model evidence; no full process-relaunch UI automation. |
| AC-05 | BLOCKED | Integration `testAC05_historyLiveCut`, `testAC05_00_previewThenFinal`, `testAC05_mqttLossHttpsOk`, `testAC05_processRestart`; upstream lost-final repair. | Explicit foreground case and physical iPhone unexecuted; one native concurrent-watch case failed. |
| AC-06 | PASS | Integration `testAC06_idleSend`, `testAC06_defaultSteer`, `testAC06_explicitQueue`; Swift delivery policy units; native DSH delivery recovery 6/6. | Source capability values are normalized by the binding. |
| AC-07 | BLOCKED | Rust command recovery units; native DSH delivery/interrupt recovery and duplicate/lost-final cases; Swift uncertain-send/stop units. | The Xcode stack has no mid-response fault injection, so its layer is unexecuted. |
| AC-08 | BLOCKED | Integration `testAC08_allowOnce`, `testAC08_reject`, `testAC08_originalCard`; UI approval flows; native DSH approval cases. | Physical iPhone case is unexecuted. |
| AC-09 | PASS | Integration `testAC09_changedVersion`; Swift/UI expired, changed and resolved cases; native DSH stale/expired guards. | Other-device staleness is Core/upstream evidence, not a second iPhone. |
| AC-10 | BLOCKED | Integration `testAC10_sameDeviceRestart`; Core draft/revocation and upstream global-revoke tests; Keychain ThisDeviceOnly adapter. | Backup-restore-to-another-phone and physical iPhone are unexecuted. |
| AC-11 | PASS | Integration `testAC11_mqttHttpsDistinction`, `testAC11_redaction`; Core redaction tests; UI diagnostics cancel/preview; Swift confirmed-export test. | Loopback HTTP/MQTT path, not production TLS. |
| AC-12 | BLOCKED | UI scroll-anchor, tool expansion, code copy, jump, large-text/dark cases; Swift scroll/anchor units. | No physical iPhone Instruments run; no release performance baseline. |
| AC-13 | BLOCKED | UI zh-Hans, en, VoiceOver grouping/labels, accessibility size, light/dark; localization completeness units. | Physical iPhone walkthrough remains unexecuted. |
