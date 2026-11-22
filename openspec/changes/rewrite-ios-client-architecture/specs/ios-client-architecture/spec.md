# iOS client architecture specification

## Purpose

Define the internal architecture of the rewritten iOS client: feature-owned business
state, explicit dependencies and capability declaration, one scenario definition for
debug/preview/tests, a Debug-only gallery, independently runnable test layers, and
agent-navigable structure. Core and UniFFI contracts and user-visible behavior stay
unchanged.

## ADDED Requirements

### Requirement: Feature-owned business state

The iOS client SHALL keep each screen's business behavior in one feature-owned
`Equatable` value state with business phases, reduced by pure transition functions.
The state SHALL answer command lifecycle, queue, wait, approval, history and
provenance questions without a view, and SHALL NOT use independent booleans or
parallel error fields for the same dimension.

#### Scenario: AC-01 Business state is expressible without UI

- **WHEN** a state-transition test drives a conversation through submitted, accepted,
  delivered, uncertain and rejected command phases while queue, wait and approval
  change independently
- **THEN** every phase and the allowed user intents are asserted from the feature
  state and reducer alone, with no view, service or Core dependency

#### Scenario: AC-02 Illegal combinations are unrepresentable

- **WHEN** a state attempts to represent a command in two phases at once, two errors
  for one dimension, or a delivered command without a receipt
- **THEN** the type system rejects the value or an invariant test fails, and the
  rewritten screens expose one enum and one error per dimension

### Requirement: View renders state only

iOS views SHALL render feature state and call feature-model intents only. Views SHALL
NOT reference service protocols, call services, probe capabilities or perform business
transitions.

#### Scenario: AC-03 Views render only state

- **WHEN** the source tree is scanned and a scenario is driven through a UI flow
- **THEN** no view references a service protocol or `as?`, and asynchronous events
  update the view only through the feature model's state

### Requirement: Feature boundaries and explicit dependencies

Feature slices SHALL NOT import another feature's model, view or state. Feature
models SHALL receive dependencies from the app shell explicitly, and the SwiftUI
environment SHALL NOT be used to reach the app model. Service capabilities SHALL be
declared once in a `FeatureSet`; runtime capability probes are permitted only at
assembly.

#### Scenario: AC-04 Feature boundaries are enforced

- **WHEN** a change introduces a feature-to-feature import or a view-to-domain service
  dependency
- **THEN** the CI boundary gate rejects it, and compliant changes depend only on
  Domain protocols, shared values and DesignSystem

#### Scenario: AC-05 Dependencies are explicit

- **WHEN** a feature model, preview or focused test is constructed
- **THEN** its dependencies are passed by `AppEnvironment` or the test support code,
  with no `@Environment(AppModel)` access anywhere in the client

#### Scenario: AC-06 Capabilities are declared once

- **WHEN** a service lacks Cloud account, session management, history, attachments,
  instance status or operation recovery
- **THEN** `FeatureSet` reports the capability as unavailable, the feature receives an
  explicit unavailable implementation, and no feature performs an ad hoc capability
  probe

### Requirement: One scenario definition for debug, preview and tests

The client SHALL define debuggable states in one `ScenarioCatalog` with stable
identifiers, categories, notes and function coverage. UI tests, previews and the Debug
State Gallery SHALL consume that catalog. Launch-time state selection SHALL use
`-ui-scenario <id>`; the former `-ui-*` boolean flags SHALL be removed.

#### Scenario: AC-07 One scenario definition drives debug, preview and tests

- **WHEN** a scenario is added, renamed or removed
- **THEN** the gallery, previews and UI tests reference the same catalog entry, stable
  identifier and notes, and duplicate identifiers or orphaned references fail the
  catalog test

#### Scenario: AC-08 Debug gallery applies scenarios at runtime

- **WHEN** a developer opens the Debug State Gallery, selects a scenario and applies it,
  and then searches the source for legacy `-ui-` flags
- **THEN** the app rebuilds the current runtime group through `AppRuntime`, opens the
  scenario entry, fences late events, and no legacy flag remains in the client

### Requirement: Debug surface is isolated from Release

Scenario, gallery, preview and debug-only fixture code SHALL be compiled and shipped
only in Debug builds. Release builds SHALL contain no gallery entry point, scenario
identifier or debug menu.

#### Scenario: AC-09 Debug surface is absent from release

- **WHEN** a Release-configured build is produced and scanned
- **THEN** it contains no State Gallery, Scenario Catalog entry points or `-ui-scenario`
  handling, and the scan result is recorded

### Requirement: Independently runnable test layers

The client SHALL provide state, model, contract and UI test layers that can run
independently. Contract tests SHALL drive the same scenario against the Live and
Fixture implementations and fail when their projections disagree. Shared fakes and
scenario builders SHALL live in `TestSupport`, not in individual test files.

#### Scenario: AC-10 Test layers are independently runnable

- **WHEN** a focused state test, model test, contract parity test or scenario UI test is
  run
- **THEN** each layer runs without requiring the others or a network, and a divergent
  Live/Fixture projection fails the contract layer

### Requirement: Agent-navigable structure

The client SHALL provide an architecture map, per-feature state transition tables,
documented focused test commands, shared test support and CI gates for boundaries,
forbidden view dependencies, file budgets and debug isolation. File and naming rules
SHALL be documented in `apps/ios/AGENTS.md`.

#### Scenario: AC-11 Agent navigation is deterministic

- **WHEN** an agent receives a behavior change for a feature
- **THEN** it can locate the feature state and transition table from the architecture
  map, identify the owning tests, run the documented focused commands, and touch no
  more than the feature and its tests without hidden dependencies

### Requirement: Behavior preservation

The rewritten client SHALL preserve the user-visible behavior and visuals of the
current client. The complete pre-rewrite UI flow list SHALL pass on the rewritten
client, including error, cancellation, offline, accessibility-size and Cloud/Standalone
variants.

#### Scenario: AC-12 User-visible behavior is unchanged

- **WHEN** the baseline UI flow suite is executed against the rewritten client
- **THEN** every recorded flow passes with the same user-visible outcomes and no visual
  redesign is introduced

### Requirement: Accessibility and localization preservation

The rewritten client SHALL preserve the existing accessibility identifiers and
localized strings used by the UI flows, and SHALL continue to support Dynamic Type,
VoiceOver traversal and the existing locale catalog.

#### Scenario: AC-14 Accessibility and localization are preserved

- **WHEN** the baseline accessibility and localization checks run against the rewritten
  screens at the largest supported Dynamic Type size and with VoiceOver enabled
- **THEN** every recorded identifier and localized string resolves, VoiceOver reaches
  the same controls in the same order, and no untranslated key is introduced

### Requirement: Nonfunctional performance budget

The rewrite SHALL NOT regress cold launch or first conversation rendering beyond the
measured baseline budgets recorded before implementation.

#### Scenario: AC-15 Launch and conversation rendering stay within budget

- **WHEN** cold launch to the first interactive screen and conversation open to first
  rendered content are measured on the same Simulator device and build configuration
  used for the baseline
- **THEN** cold launch is no more than 10 percent slower and conversation first render
  is no more than 100 milliseconds slower than the recorded baseline

### Requirement: Existing install upgrade behavior

Because compatibility is not required, an existing installation upgraded to the
rewritten client SHALL start from the same explicit onboarding path as a fresh install
and SHALL NOT crash, silently reuse stale in-memory assumptions, or delete user data
outside the app container.

#### Scenario: AC-16 Upgrade starts from explicit onboarding

- **WHEN** a build with existing configuration, preferences and Keychain items is
  replaced by the rewritten build and launched
- **THEN** the app presents onboarding with a clean official Cloud instance, leaves
  unrelated files and Keychain items untouched, and never crashes on stale state

### Requirement: Scenario data is synthetic and secret-free

Scenarios, gallery entries, previews, fixtures and debug logs SHALL use only synthetic
identifiers and content. They SHALL NOT contain real accounts, pairing links, message
contents, credentials, tokens or keys.

#### Scenario: AC-17 Scenario and debug data contain no private material

- **WHEN** scenarios, previews, gallery entries, fixtures and debug logs are inspected,
  including a Release-configured build scan
- **THEN** only synthetic identifiers and content are present, and no real account,
  pairing link, message content, credential, token or key appears in the catalog,
  fixtures, previews, logs or packaged debug payloads

### Requirement: Structured app logging

The iOS client SHALL log through one structured facade with per-feature categories and
levels. Logs SHALL NOT contain message contents, tool parameters, pairing links,
credentials, tokens or keys; identifiers SHALL be hashed or truncated and sensitive
interpolations SHALL be private by default.

#### Scenario: AC-18 App logs are structured and redacted

- **WHEN** features log a successful operation, a typed failure and a retry, and a
  synthetic sensitive marker is present in inputs
- **THEN** each record carries a category, level, correlation fields and a redacted
  message, and no marker appears in os.Logger output, captured logs or debug console

### Requirement: Persistent bounded app logs and on-demand export

The client SHALL persist redacted app logs to an app-private file with size and age
bounds, rotation, complete file protection and backup exclusion. Logs SHALL be redacted
before they are written, SHALL survive relaunch, SHALL be exportable on demand through
an explicit user action, and SHALL be cleared on device reset or forget. Logs SHALL NOT
be uploaded automatically. Core keeps its existing bounded diagnostic history.

#### Scenario: AC-19 Logs persist, stay bounded and export on request

- **WHEN** the app relaunches after a logged session, logs exceed the size or age bound,
  a device reset or forget occurs, or a user exports logs or diagnostics at any time
- **THEN** previously written redacted records are present, oldest segments are rotated
  or pruned first, reset clears the app log files and calls Core's existing
  `clear_diagnostics`, and the exported file contains only redacted records with no
  automatic upload

### Requirement: Performance signposts

The client SHALL emit `OSSignposter` intervals for cold launch and first conversation
content, and the acceptance measurement SHALL reuse those signposts.

#### Scenario: AC-20 Launch and render signposts are reproducible

- **WHEN** the app is cold-launched and a conversation is opened on the acceptance
  Simulator configuration
- **THEN** signposts for launch and first content appear with stable names, and the
  recorded durations feed AC-15

### Requirement: Core diagnostic events reach the app log

The binding SHALL expose Core's existing structured, redacted diagnostic event stream
through an additive sink; Core SHALL NOT gain a second logging system. The iOS client
SHALL record those events in the same bounded timeline with source, level, trace id and
event name, and a failing or slow sink SHALL NOT change Core operation results or block
Core work.

#### Scenario: AC-21 Core diagnostic events are bridged with correlation and isolation

- **WHEN** a Core operation emits info, debug and error diagnostic events, the sink is
  saturated, and the sink implementation fails
- **THEN** the app timeline records the Core source, level, trace id and event name,
  drops excess records without blocking, and the Core operation still returns its
  normal result

### Requirement: Core and binding behavioral contracts unchanged

The rewrite SHALL NOT change Rust Core behavior, protocol messages, UniFFI data types
or the mobile service contract. Additive logging instrumentation is permitted: Core
MAY emit structured log records and the binding MAY expose a log sink, provided no
existing operation, DTO, error or authorization result changes. The existing Core and
binding suites SHALL remain green.

#### Scenario: AC-13 Core and binding behavioral contracts are unchanged

- **WHEN** the rewrite diff and the Core/binding test suites are inspected
- **THEN** no operation, protocol message, DTO, error or authorization result changes;
  the only permitted binding addition is the diagnostic event sink and its event view,
  and the existing Core and binding suites pass
