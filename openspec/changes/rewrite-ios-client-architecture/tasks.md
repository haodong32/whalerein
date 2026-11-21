# Tasks

## 1. Baseline and gates

- [x] 1.1 Record the pre-rewrite baseline in local acceptance evidence: every `WhaleReinUITests` flow with its launch state and expected outcome; the screen/state inventory expressed by `LaunchOptions`, `AppModel` and `ConversationModel`; cold-launch and first-conversation-render timings; and the accessibility identifier/string list (AC-12, AC-14, AC-15).
- [x] 1.2 Add the architecture gate script under `apps/ios/scripts/` and wire it into the iOS CI workflow. It rejects feature-to-feature Model/View/State imports, service references or `as?` in Views, `@Environment(AppModel)`, legacy `-ui-` flags, and files over the documented budget. Start with a shrinking allowlist for legacy files only; new files are never allowlisted (AC-03, AC-04, AC-05, AC-08).
- [x] 1.3 Add `apps/ios/AGENTS.md` with layer rules, naming, file budgets, the six invariants and the focused test commands (AC-11).

## 2. App shell and scenario foundation

- [x] 2.1 Introduce `App/AppEnvironment.swift` with the service factory, preferences factory, configuration store, clock, UUID source and app version; keep the legacy model temporarily driven from it (AC-05).
- [x] 2.2 Introduce `App/AppRuntime.swift` owning instance activation, switching, the activation-generation fence, background transitions and the current feature model group (AC-05).
- [x] 2.3 Introduce `App/AppRouter.swift` owning navigation only, and remove business/service state from navigation paths (AC-05).
- [x] 2.4 Introduce `Domain/FeatureSet.swift`; compute capabilities once per runtime; replace `as? any` probes with explicit adapters or unavailable implementations (AC-06).
- [x] 2.5 Introduce `Debug/Scenario/Scenario.swift` and `ScenarioCatalog.swift` with stable ids, categories, notes, owner, launch overrides, fixture scenario, presentation overrides and entry; seed the catalog from the 1.1 inventory; use synthetic identifiers and content only (AC-07, AC-17).
- [x] 2.6 Replace `LaunchOptions` state selection with `-ui-scenario <id>` and an applier that rebuilds through `AppRuntime`; keep legacy flags only until 6.4 (AC-07, AC-08).
- [x] 2.7 Add `DesignSystem/` primitives used by rewritten views (status row, empty/error state, primary button) with no business state (AC-03).
- [ ] 2.8 Add the runtime coordination events for cross-feature flows (instance switch, selected session, selected Host) and verify no feature imports another feature's types (AC-04, AC-05).
- [x] 2.9 Expose Core's existing `DiagnosticSink` through the binding: add a flat `DiagnosticEventView` (receipt time, trace id, level, stable event name, allowlisted status/elapsed/layer/stage/outcome/code map), a foreign sink callback, and a constructor that installs it before Core opens; install a sink on every runtime rebuild. Do not change `core/`; add binding tests for event mapping and failure isolation (AC-21, AC-13).
- [x] 2.10 Add one small Swift sink adapter that validates event level/name, applies the shared redactor, assigns receipt timestamps, appends to the shared ring buffer and preserves the Core trace id (AC-21).
- [x] 2.11 Add `Platform/Logging/AppLog.swift` and `AppLogStore`: one facade, a 2 MB in-memory ring buffer, a JSON-lines file at `Library/Logs/WhaleRein/app-log.jsonl` with schema version/app version/Core revision, category/level policy, injected-clock timestamps, redaction-before-write and reset clearing (AC-18, AC-19).
- [x] 2.13 Add rotation, age/size pruning, `completeFileProtection`, private permissions, backup exclusion, periodic flush, background flush, export-staging cleanup and write-failure handling for the log file; verify bounds, relaunch persistence and no crash while locked (AC-19).
- [x] 2.12 Verify early-event buffering and saturation: the sink adapter drops oldest records with a visible counter, never blocks Core, and a failing sink leaves operation results unchanged (AC-21).

## 3. Conversation rewrite

- [x] 3.1 Add `Features/Conversation/ConversationState.swift` with command, queue, wait, approval, history and provenance dimensions plus the transition table; remove parallel booleans and error fields (AC-01, AC-02).
- [x] 3.2 Implement pure `reduce` transitions and state tests for every event, including uncertain, rejected, stale approval, cancelled wait and cached-only history; prove illegal combinations fail (AC-01, AC-02).
- [x] 3.3 Rewrite `ConversationModel` as intent + service orchestration over the reducer; preserve activation fencing and late-event drops; add model tests with `FakeSessionService`, `FakeClock` and `FakeUUIDSource` (AC-03, AC-10).
- [x] 3.4 Split queue, wait, history, attachment and recovery controllers out of `ConversationModel`; keep a thin facade exposing state and intents (AC-01, AC-11).
- [x] 3.5 Rewrite `ConversationView` and subviews to switch on the state; enforce no service access, no `as?`, and the file budget; keep accessibility identifiers and localization keys (AC-03, AC-12).
- [x] 3.6 Migrate conversation UI tests to conversation scenarios via `-ui-scenario`; record scenario-to-flow mapping (AC-07, AC-12).

## 4. Remaining feature slices

- [x] 4.1 Rewrite Settings as feature sections with one state/model per section; remove the aggregated settings model and view (AC-03, AC-05).
- [x] 4.2 Rewrite Cloud sign-in and directory with typed state and explicit dependencies; preserve system-sheet, cancellation, empty, unavailable and revoked flows (AC-01, AC-12).
- [x] 4.3 Rewrite Account/device/diagnostics with typed state and explicit `FeatureSet` capabilities; preserve revoke, forget, scoped sign-out, status and redacted preview/share (AC-03, AC-06).
- [x] 4.4 Rewrite Pairing, Sessions, Hosts and Attachments with the same feature shape; remove legacy models and unavailable probing (AC-03, AC-06).
- [x] 4.5 Move shared transitions used by two or more features into `Domain/Values` (`CommandLifecycle`, `ApprovalAuthority`) and delete the per-feature duplicates (AC-04; design D-09).
- [x] 4.6 Migrate the remaining UI tests to scenarios and update accessibility-depth checks (AC-07, AC-12).

## 5. Debug gallery and previews

- [x] 5.1 Add `Debug/StateGallery/StateGalleryView.swift` and model: group by category, search, show notes/owner/entry, apply a scenario and open its entry (AC-08).
- [x] 5.2 Add the Debug-only Settings entry and the fallback gesture; verify it is absent from Release configuration (AC-08, AC-09).
- [x] 5.3 Add previews for the rewritten screens, each constructed from a scenario through `TestSupport`; no network, no real data (AC-07, AC-11).
- [x] 5.4 Add the Release surface scan to CI. Emit a Debug-only marker string/constant for the gallery and scenario entry, build a Release-configured app, scan symbols and strings for that marker and `-ui-scenario`, and record the artifact (AC-09).

## 6. Remove the legacy architecture

- [x] 6.1 Delete `AppModel`, `ConversationModel` and their extensions after every screen and flow has moved; delete `LaunchOptions` flags and the legacy fixtures' boolean construction (AC-01, AC-08).
- [ ] 6.2 Delete obsolete fixtures, stub services, tests and localization keys that no longer have a consumer; verify the catalog covers the removed states (AC-07, AC-12).
- [x] 6.3 Add `Fixtures/ScenarioBuilder` and `TestSupport` shared fakes; remove per-test mock duplication (AC-10, AC-11).
- [x] 6.4 Delete all `-ui-` flags and legacy scenario mapping; empty the 1.2 allowlist so the gate runs unconditionally (AC-07, AC-08, AC-03, AC-04, AC-05).
- [ ] 6.5 Implement the two-level contract parity suite for send, queue, wait, approval, history, attachment and recovery. The deterministic level compares projections from recorded envelopes; the local level runs against the local stack and records unavailable live variants as BLOCKED (AC-10).
- [ ] 6.6 Replace the remaining `print` in fixtures; add the explicit Settings log export (redacted snapshot to an app-private staging file + system share) and append the same snapshot to diagnostics export; verify reset deletes both log segments and calls Core's existing `clear_diagnostics` (AC-19).

## 7. Agent navigation and documentation

- [x] 7.1 Write `docs/ios-architecture.md`: layer map, feature map, state ownership, dependency rules, "where does this belong", scenario workflow, focused commands, catalog ownership/cleanup and review checklist (AC-11).
- [x] 7.2 Add one state transition table per feature next to its State, covering events, target states and invariants (AC-11).
- [x] 7.3 Add per-feature focused test commands to `docs/ios-architecture.md` and verify each command runs only the named layer (AC-10, AC-11).
- [x] 7.4 Update `docs/ios.md` composition sections to describe the rewritten client; record that no other maintained guide needs changes or update the ones that do (AC-11).

## 8. Acceptance verification

- [ ] 8.1 Run the complete state and model suites for every feature and record focused evidence per AC (AC-01, AC-02, AC-03, AC-10).
- [ ] 8.2 Run the parity suite against the local Core stack and record enabled/disabled capabilities and any environment blockers (AC-10, AC-13).
- [ ] 8.3 Run the complete scenario-driven UI suite and compare every flow with the 1.1 baseline, including error, cancel, offline, accessibility size, Cloud and Standalone variants (AC-12).
- [ ] 8.4 Run the Release-configured build, gallery absence scan, architecture gates and file-budget check; record artifacts (AC-09, AC-03, AC-04, AC-05, AC-08).
- [ ] 8.5 Run the existing Core and binding suites; verify `core/` is unchanged and the binding diff contains only the diagnostic sink and event view, with no operation, protocol, DTO, error or authorization change (AC-13).
- [ ] 8.6 Review the result against this design, record architectural deviations and the maintained-docs decision (AC-11, AC-12, AC-13).
- [ ] 8.7 Compare cold launch and conversation first render with the 1.1 baseline on the same Simulator device and build configuration; record both measurements and the threshold outcome (AC-15).
- [ ] 8.8 Replace a populated build with the rewrite on the Simulator and verify onboarding, absence of crashes and untouched unrelated files/Keychain items (AC-16).
- [ ] 8.9 Run the accessibility identifier/string checks and VoiceOver traversal at the largest Dynamic Type size; record the manual VoiceOver procedure and result against the 1.1 list (AC-14).
- [ ] 8.10 Inspect the scenario catalog, fixtures, previews, debug logs and the Release package for real accounts, pairing links, message contents, credentials, tokens or keys; record the scan result (AC-17).
- [ ] 8.11 Drive a synthetic successful operation, typed failure and retry with a sensitive marker; assert category, level, trace id and redaction in captured app and Core diagnostic records (AC-18, AC-21).
- [ ] 8.12 Exercise relaunch persistence, rotation/age pruning, memory/file bounds, device reset, direct log export, diagnostics sharing and no-upload behavior; record file sizes, retention and reset evidence (AC-19).
- [ ] 8.13 Capture cold-launch and first-content signposts on the acceptance Simulator configuration and reuse them for the AC-15 measurements (AC-20).

## Acceptance case matrix

Bind each case to its focused test or native procedure before execution. The linked
specification scenario supplies the assertion. Record command, setup, tested revision,
runtime version, result and artifact path in local evidence. Use synthetic data only.

| Planned case ID | Verification layer | Owning tasks | Status |
| --- | --- | --- | --- |
| AC-01.command-phases | State tests | 3.1, 3.2 | NOT RUN |
| AC-01.queue-wait-approval-independent | State tests | 3.2 | NOT RUN |
| AC-01.intent-allowance | State tests, model tests | 3.2, 3.3 | NOT RUN |
| AC-02.no-double-phase | Compile/type review, state tests | 3.1, 3.2 | NOT RUN |
| AC-02.no-parallel-error | State tests | 3.1, 3.2 | NOT RUN |
| AC-02.delivered-without-receipt | Compile/type review | 3.1 | NOT RUN |
| AC-03.no-service-in-view | CI gate | 1.2 | NOT RUN |
| AC-03.state-driven-update | Scenario UI | 3.5, 3.6 | NOT RUN |
| AC-04.feature-import-gate | CI gate | 1.2 | NOT RUN |
| AC-04.view-domain-gate | CI gate | 1.2 | NOT RUN |
| AC-05.no-environment-appmodel | CI gate | 1.2 | NOT RUN |
| AC-05.preview-without-environment | Preview compile | 5.3 | NOT RUN |
| AC-06.feature-set-fixture-and-live | Unit tests | 2.4 | NOT RUN |
| AC-06.unavailable-is-explicit | Unit tests, review | 2.4, 4.3 | NOT RUN |
| AC-07.catalog-unique-ids | Catalog tests | 2.5 | NOT RUN |
| AC-07.no-orphan-references | Catalog tests, UI compile | 2.5, 5.3 | NOT RUN |
| AC-07.notes-and-owner-present | Catalog test | 2.5 | NOT RUN |
| AC-08.gallery-list-and-apply | Simulator UI | 5.1 | NOT RUN |
| AC-08.runtime-fence-and-entry | Simulator UI, model tests | 2.6, 5.1 | NOT RUN |
| AC-08.no-legacy-flags | CI gate | 1.2, 6.4 | NOT RUN |
| AC-09.release-symbol-scan | Release build scan | 5.4 | NOT RUN |
| AC-10.state-layer-focused | Focused command | 8.1 | NOT RUN |
| AC-10.model-layer-focused | Focused command | 8.1 | NOT RUN |
| AC-10.model-fencing-and-cancellation | Model tests | 3.3 | NOT RUN |
| AC-10.parity-deterministic-envelopes | Contract tests | 6.5 | NOT RUN |
| AC-10.parity-local-stack | Local stack integration | 6.5, 8.2 | NOT RUN |
| AC-10.parity-mismatch-fails | Contract tests | 6.5 | NOT RUN |
| AC-10.ui-scenario-focused | Focused command | 8.3 | NOT RUN |
| AC-11.test-support-shared | Review, unit tests | 6.3 | NOT RUN |
| AC-11.map-and-transition-tables | Docs review | 7.1, 7.2 | NOT RUN |
| AC-11.focused-commands | Command runs | 7.3 | NOT RUN |
| AC-12.baseline-flow-parity | Full UI suite | 8.3 | NOT RUN |
| AC-12.error-cancel-offline-large-text | Full UI suite | 8.3 | NOT RUN |
| AC-12.cloud-and-standalone | Full UI suite, integration | 8.3 | NOT RUN |
| AC-13.no-core-binding-diff | Diff check | 8.5 | NOT RUN |
| AC-13.existing-suites-green | Core/binding suites | 8.5 | NOT RUN |
| AC-14.identifiers-and-strings | Accessibility checks | 8.9 | NOT RUN |
| AC-14.voiceover-order | Accessibility UI | 8.9 | NOT RUN |
| AC-14.largest-dynamic-type | Accessibility UI | 8.9 | NOT RUN |
| AC-15.cold-launch | Measurement | 8.7 | NOT RUN |
| AC-15.first-conversation-render | Measurement | 8.7 | NOT RUN |
| AC-16.upgrade-to-onboarding | Simulator upgrade run | 8.8 | NOT RUN |
| AC-16.no-unrelated-deletion | Simulator upgrade run | 8.8 | NOT RUN |
| AC-17.synthetic-catalog-and-fixtures | Fixture inspection | 2.5, 8.10 | NOT RUN |
| AC-17.no-secrets-in-logs-or-release | Log/Release scan | 8.10 | NOT RUN |
| AC-18.structured-redacted-record | Captured-sink test | 2.11, 8.11 | NOT RUN |
| AC-18.sensitive-marker-absent | Captured-sink test, CI scan | 2.11, 8.11 | NOT RUN |
| AC-19.persists-across-relaunch | Simulator relaunch | 2.13, 8.12 | NOT RUN |
| AC-19.bounds-drop-oldest | Unit test | 2.11, 2.13, 8.12 | NOT RUN |
| AC-19.rotation-and-age-pruning | Unit test | 2.13, 8.12 | NOT RUN |
| AC-19.reset-clears-files-and-core | Unit test, Simulator | 6.6, 8.12 | NOT RUN |
| AC-19.export-anytime | Simulator UI | 6.6, 8.12 | NOT RUN |
| AC-19.export-visible-in-release | Release-configured Simulator UI | 6.6, 8.12 | NOT RUN |
| AC-19.staging-cleanup | Unit test, Simulator | 2.13, 6.6 | NOT RUN |
| AC-19.redacted-on-disk | File inspection, unit test | 2.11, 8.12 | NOT RUN |
| AC-20.launch-and-first-content-signposts | Simulator instrumentation | 8.13 | NOT RUN |
| AC-21.core-diagnostic-correlation | Binding + captured-sink test | 2.9, 2.10, 2.12, 8.11 | NOT RUN |
| AC-21.saturation-isolation | Model test | 2.12, 8.11 | NOT RUN |
| AC-21.early-events-before-install | Binding/model test | 2.9, 2.12 | NOT RUN |

## Acceptance Results

| AC ID | Status | Evidence | Limitations |
| --- | --- | --- | --- |
| AC-01 | NOT RUN | | |
| AC-02 | NOT RUN | | |
| AC-03 | NOT RUN | | |
| AC-04 | NOT RUN | | |
| AC-05 | NOT RUN | | |
| AC-06 | NOT RUN | | |
| AC-07 | NOT RUN | | |
| AC-08 | NOT RUN | | |
| AC-09 | NOT RUN | | |
| AC-10 | NOT RUN | | |
| AC-11 | NOT RUN | | |
| AC-12 | NOT RUN | | |
| AC-13 | NOT RUN | | |
| AC-14 | NOT RUN | | |
| AC-15 | NOT RUN | | |
| AC-16 | NOT RUN | | |
| AC-17 | NOT RUN | | |
| AC-18 | NOT RUN | | |
| AC-19 | NOT RUN | | |
| AC-20 | NOT RUN | | |
| AC-21 | NOT RUN | | |
