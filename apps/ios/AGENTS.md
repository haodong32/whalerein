# iOS client rules

`apps/ios` is the SwiftUI client. It talks to Core through the
`WhaleReinMobile` UniFFI binding and must not reimplement Core policy.

Architecture and rationale: [docs/ios-architecture.md](../../docs/ios-architecture.md).

## Layers

```
App/          WhaleReinApp, AppEnvironment, AppRuntime, AppRouter
Features/     one vertical slice per screen: State, Model, View, transitions table
Domain/       protocols, FeatureSet, shared values, DTOs, errors
Platform/     Core/UniFFI adapters, secret stores, storage locations, logging
DesignSystem/ stateless view primitives
Debug/        Scenario catalog, State gallery (compiled only in Debug)
Fixtures/     deterministic services driven by a Scenario
```

- App shell may import Features, Domain and Platform.
- A feature may import Domain, DesignSystem and its own files only.
- A feature must not import another feature's Model, View or State.
- A view must not reference a service protocol, call a service or use `as?`.
- `as? any Service` is allowed only while `FeatureSet` is built.

## Six invariants

1. **One state value per screen.** `Features/<X>/<X>State.swift` is `Equatable`,
   business-flavored and has no UI or service types. One enum per dimension; no
   parallel booleans and no per-operation error fields.
2. **State changes only through `reduce`.** `reduce(state, action) -> state` is
   pure: no IO, no clock, no randomness. Models translate intents and service
   results into actions.
3. **Core is authoritative.** Swift records facts, pending intents and
   uncertainty, and labels provenance. Queue order, approvals, authorization and
   recovery policy stay in Core.
4. **Dependencies are explicit.** `AppEnvironment` is the only assembly point;
   models receive what they need. No `@Environment(AppModel.self)`.
5. **One scenario source.** Debug, previews and UI tests read `ScenarioCatalog`
   and pass `-ui-scenario <id>`. Do not add a second list of states.
6. **Debug stays out of Release.** Gallery, catalog and scenario parsing live
   under `Debug/`, `#if DEBUG` or both.

## File budgets

- `*View.swift` ≤ 300 lines.
- `*Model.swift` ≤ 500 lines.
- A State file holds one primary type.
- A feature directory holds no other feature's types.

New files are never allowlisted. The gate fails on allowlisted files that no
longer exist, so the list only shrinks.

## Naming

- Feature state: `<Feature>State`; reducer: `<Feature>Reduce`.
- Model: `<Feature>Model`, `@Observable @MainActor`, owns one state value.
- Scenario ids: `category.kebab-case` and stable; changing one is a breaking
  test change.
- Log events: stable snake_case names, never free-form text.

## Commands

```bash
./repo test ios-unit --only ConversationReduceTests
./repo test ios-ui --only <ScenarioTest>
python3 apps/ios/scripts/architecture_gate.py
python3 apps/ios/scripts/architecture_gate.py --report
```

Focused tests first. Full plans only for acceptance.
