# iOS client architecture

The client is a SwiftUI app that talks to Core through the `WhaleReinMobile`
UniFFI binding. This document is the map an agent or reviewer reads before
changing it. Rules for day-to-day edits live in
[`apps/ios/AGENTS.md`](../apps/ios/AGENTS.md).

## Layer map

```
App/            WhaleReinApp, AppEnvironment, AppRuntime, AppRouter
Features/       vertical slices: State, Model, View, transitions table
Domain/         protocols, FeatureSet, shared values, DTOs, errors, Scenario
Platform/       Core/UniFFI adapters, secret stores, storage locations, logging
DesignSystem/   stateless view primitives
Debug/          ScenarioCatalog, StateGallery (Debug only)
Fixtures/       deterministic services driven by a Scenario
```

Dependencies point down only:

```
App -> Features -> Domain -> Platform
```

- The app shell may import Features, Domain and Platform.
- A feature may import Domain, DesignSystem and its own files.
- A feature must not import another feature's Model, View or State.
- A view must not reference a service protocol, call a service, or use `as?`.
- `as? any Service` exists only while `FeatureSet` is assembled.

Boundaries are enforced by `apps/ios/scripts/architecture_gate.py` in CI, not by
separate Swift modules. The project uses file-system synchronized groups, so a
new file under `WhaleRein/` is part of the app target automatically.

## State ownership

| State | Owner | Lifetime |
| --- | --- | --- |
| Activation, instance switching, generation fence | `AppRuntime` | process |
| Navigation, presented route | `AppRouter` | process |
| Capabilities | `FeatureSet`, built per runtime | runtime group |
| Business state per screen | `Features/<X>/<X>State` | feature model |
| Core truth (queue order, approvals, authorization) | Core | process |

One screen has exactly one `Equatable` state value. Every dimension is an enum,
so illegal combinations cannot be constructed. There is no second
DomainState/ScreenState layer: Core is authoritative and the screens are 1:1, so
a second layer would add reconciliation without a consumer.

State changes only through a pure reducer:

```swift
state = ConversationReduce.reduce(state, .sendSubmitted(command))
```

Models translate intents and service results into actions; they never mutate
state properties directly. Reducers perform no IO, read no clock and use no
randomness, so state tests run without a service or UI.

## Feature shape

```
Features/Conversation/
  ConversationState.swift       one primary state type
  ConversationValues.swift      supporting value types
  ConversationReduce.swift      pure transitions
  ConversationModel.swift       intent + service orchestration
  ConversationView.swift        renders state only
  Conversation+Transitions.md   event -> state table
```

File budgets: `*View.swift` ≤ 300 lines, `*Model.swift` ≤ 500 lines.

## Conversation history and viewport

Opening a conversation starts a paged Core observation. The typed snapshot
supplies recent rows, their provenance and an optional older-page continuation.
The session service preserves the producer's watch scope through polling.
The conversation model applies snapshots and live updates and makes independent
older-page reads when the reader approaches the older edge. It fences those
requests by traversal so a replaced window cannot receive an old page.

The persistent transcript reports geometry and reader intent. It follows the
bottom while the reader is there and restores a visible row at its previous
viewport offset while reading earlier messages. SwiftUI lays out the rows already loaded by the history pager in one stable
stack. Offset correction reads the current native scroll position alongside
the measured row. Send receipts, question changes,
history prepends and keyboard changes share this viewport policy.
Command status has its own identity. A committed message settles a pending
presentation row only when the command has an authoritative association.

## App shell

- `AppEnvironment` is the only assembly point: service factory, preferences
  factory, configuration store, clock, UUID source, app version, scenario.
  Feature models receive their dependencies explicitly.
- `AppRuntime` owns the current instance, the monotonic `activationGeneration`
  fence, scene transitions and the runtime group. A rebuild replaces the group
  and increments the fence, so a late result from the old group is dropped.
- `AppRouter` owns navigation only.
- Cross-feature coordination goes through `AppRuntime` events, never through
  feature-to-feature references or shared mutable singletons.

## Scenario workflow

`Scenario` is the single definition of a debuggable state: launch overrides,
fixture behavior, presentation overrides and destination entry.

- UI tests launch with `-ui-scenario <id>`.
- The Debug state gallery (conversation toolbar → grid icon) lists the catalog,
  searches it, applies a scenario by rebuilding the runtime group, and opens its
  entry.
- SwiftUI previews build the same scenario through `TestSupport`.
- Ids are stable. Changing an id is a breaking test change.

Adding a scenario: add one entry to `Debug/Scenario/ScenarioCatalog.swift` with
a unique id, a category, notes that say what it verifies, and an owner. Do not
add a second list of states in a test.

Cleanup: scenarios without a referencing test or a human owner are removed each
quarter; the catalog is the only list to edit.

## Logging and diagnostics

One facade, `Platform/Logging/AppLog.swift`, wraps `os.Logger` per category and
the persistent `AppLogStore` (2 MB memory ring, 5 MB current + 5 MB rotated
JSONL, 7-day bound, complete file protection, backup exclusion). Records are
redacted before they are written: only allowlisted field names, identifiers
hashed or truncated, no message contents or payloads.

Core already owns a bounded redacted diagnostic journal. The binding exposes it
as a flat `DiagnosticEventView`; `CoreDiagnosticBridge` forwards it into the same
store with the Core trace id. The binding addition is the only Core-facing
change; `core/` is untouched.

Export is explicit: Settings writes a redacted snapshot to an app-private staging
file and opens the system share sheet. Nothing is uploaded automatically.

## Test layers

| Layer | Scope | Does not depend on |
| --- | --- | --- |
| State | every reducer transition and invariant | service, UI, Core |
| Model | intent orchestration, error mapping, activation/fencing | UI, Core |
| Contract | same script against Live and Fixture projections | UI |
| UI | one flow per scenario through `-ui-scenario` | fixtures only, no network |
| Integration | real Core and the local Cloud stack | production |

`TestSupport` provides shared fakes so tests, previews and the gallery use one
construction path.

## Where does this belong?

| Change | Place |
| --- | --- |
| A new screen or a screen state | `Features/<Name>/` |
| A rule two or more features share | `Domain/Values/` |
| A service contract | `Domain/Protocols/` |
| A Core/UniFFI adapter | `Platform/` |
| A reusable button, row, empty state | `DesignSystem/` |
| A debuggable state | `Debug/Scenario/ScenarioCatalog.swift` |
| A new log event | call `AppLog` with an allowlisted field set |

## Commands

```bash
./repo test ios-unit --only ConversationReduceTests
./repo test ios-unit --only AppLogStoreTests
./repo test ios-ui --only StateGalleryUITests
python3 apps/ios/scripts/architecture_gate.py
apps/ios/scripts/release-surface-scan.sh
```

Focused tests first; full plans only for acceptance.

## Review checklist

- [ ] State is one value, `Equatable`, with enums instead of parallel booleans.
- [ ] Transitions go through `reduce`; the model has no direct state mutation.
- [ ] The view renders state and sends intents only.
- [ ] No feature-to-feature import; no service reference in a view.
- [ ] A new state is reachable from the scenario catalog.
- [ ] Log records use allowlisted fields and carry a correlation id.
- [ ] Debug-only code is under `Debug/` or `#if DEBUG`.
- [ ] Focused tests cover the new transition and pass.
