# Rewrite iOS client architecture proposal

## Why

The iOS client has a sound protocol boundary but a fragile internal shape. `AppModel`
(1,066 lines) and `ConversationModel` (1,768 lines) each combine lifecycle, feature
state, error mapping and view composition. Conversation state is expressed as
thirteen independent booleans plus parallel error fields, so illegal combinations
are representable and a behavior cannot be tested without a view. Thirty-two
`-ui-*` launch flags form a second, undocumented state model that fixtures, UI tests
and developers each interpret differently.

The result is expensive to test, hard for agents to modify without context, and prone
to Live/Fixture drift: every capability is implemented twice and nothing asserts both
implementations agree.

Compatibility with the current Swift internals is explicitly not required. The
Rust Core, UniFFI contracts, Cloud and CLI stay unchanged, so the Swift client can be
replaced instead of incrementally patched.

## What Changes

- Replace the `AppModel`/`ConversationModel` monoliths with an app shell
  (`AppEnvironment`, `AppRuntime`, `AppRouter`) and vertical feature slices that each
  own one business state, one observable model and one view.
- Model each screen as one business-flavored value state (command lifecycle, queue,
  wait, approval, provenance) reduced by pure functions, instead of UI booleans. Do
  not introduce a separate DomainState/ScreenState two-tier model.
- Inject dependencies explicitly through `AppEnvironment`; remove
  `@Environment(AppModel.self)` from feature views. Declare service capabilities once
  in `FeatureSet`; feature code no longer probes with `as? any`.
- Delete all `-ui-*` launch flags. Add a `ScenarioCatalog` as the single definition of
  debug, preview, UI-test and QA states, plus a Debug-only State Gallery that applies
  a scenario through the existing activation path.
- Rewrite the test suite around state transitions, model orchestration, Live/Fixture
  contract tests and scenario-driven UI flows. Add shared `TestSupport` fakes and a
  unified command entry for focused runs.
- Add agent-navigation artifacts: an architecture map per feature, state transition
  tables, boundary and size gates in CI, and naming rules in `apps/ios/AGENTS.md`.
- Add one structured logging and observability layer: an app log facade with categories,
  redaction and bounded persistent storage with on-demand export, performance signposts used by acceptance, and a
  bridge for Core's existing redacted diagnostic journal so Core events appear in the
  same correlated timeline without adding a second Rust logging system or changing
  Core behavior.
- Remove the legacy types, flags, fixtures and tests rather than keeping compatibility
  shims. No migration of existing iOS installs is required or attempted.
- Keep user-visible behavior and visuals as they are. This change is an internal
  rewrite, not a redesign.

## Capabilities

### New Capabilities

- `ios-client-architecture` defines the internal architecture, state ownership,
  scenario/debug surface, test layers and agent-facing structure of the iOS client.

### Modified Capabilities

None. Every behavior specified by the existing iOS changes remains the behavioral
baseline and is re-verified through the rewritten UI flow suite.

## Impact

- Rewritten: `apps/ios/WhaleRein/App`, `Domain` protocols and value types,
  `Presentation`, `Views`, `Fixtures`, `WhaleReinTests`, `WhaleReinUITests` and
  integration tests. The verified Live DTO/error mapping is retained and re-homed
  under Platform rather than reimplemented.
- Added: `Debug/Scenario`, `Debug/StateGallery`, `Features/` slices,
  `DesignSystem/`, `docs/ios-architecture.md`, `apps/ios/AGENTS.md` and CI gates.
- Updated: `docs/ios.md` where it describes the client composition, and the iOS
  workflow in `.github/workflows`.
- Additive only: `apps/mobile-bindings` gains a diagnostic event sink and view. Core
  operations, protocol messages, DTOs, errors, authorization results and the existing
  `core/src/diagnostics` journal stay unchanged; Cloud, CLI, Host adapters and DSH
  execution are untouched.
