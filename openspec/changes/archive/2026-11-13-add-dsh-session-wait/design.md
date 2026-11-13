# Native-backed session wait design

## Decision Review

| ID | Decision | Status and basis | Affected ACs |
|---|---|---|---|
| D-REL-01 | Deliver verified capabilities independently; keep unverified ones disabled and report them. | CONFIRMED by user; dependencies still apply. | All |
| D-WAIT-01 | Default timeout exits 0; --fail-on-timeout exits 7. Both retain the typed timeout result. | CONFIRMED by user. | AC-03 |
| D-WAIT-02 | Delegate stopping to native wait through the adapter; Core handles transport and the overall deadline. | Selected after the user requested a recommendation: native whenIdle and identity checks cannot be reconstructed from sampled state. | AC-01–AC-12 |
| D-LIMITS-01 | Reuse native/protocol limits; justify necessary additional bounds separately. | CONFIRMED by user; native default and timer bounds were inspected. | AC-03, AC-10 |

## Context and inspected evidence

Installed `@deepseek-ai/dsh-api-session-controller` 0.2.2-rc.1 `lib/index.js:2199-2395` defines the 30000 ms default, positive integer timeout checks, `observeStates(1000)`, pending-input priority, cold `active=false` idle, and live `whenIdle()` plus revision/Agent-identity checks. `@deepseek-ai/dsh-timeout/lib/index.js` defines a maximum timer delay of 2147483647 ms. Native wait reads assistant output after observing its stopping state and errors if no state arrived before timeout. These are source facts, not runtime acceptance.

`core/src/commands/mod.rs` currently gives requests a 30000 ms TTL; `core/src/access/http.rs` also has a 30-second result-poll deadline. Thus forwarding one 30-second wait unchanged is unsafe: forwarding, output assembly and response delivery need budget too.

## Architecture Review

| Boundary | Responsibility |
|---|---|
| CLI → Core | Parse `wait --session ID [--timeout SECONDS] [--fail-on-timeout]`; preserve millisecond precision and format typed observations. |
| Core → Host | Submit independent, read-only scoped waits through existing encrypted queries; retain one monotonic overall deadline across slices/recovery. Do not use the watch/tail admission guard to decide native quiescence. |
| Host → SourceAdapter | Add a source-neutral wait method and observation-only cancellation. Bind each live wait to device, channel, session, wait ID and absolute request expiry. Abort on revocation/close/expiry and release the in-memory record in all outcomes. No durable scheduler or control-command log for observations. |
| DSH adapter → DSH | Call native wait with a cancellation signal. Preserve native stopping/identity semantics. Race/check still-active adapter-claimed input handles before returning idle, since claimed requests may be absent from native pending Remote events. Only the adapter handles any native output cursor and discards assistant records before public serialization. |
| Access | Relay encrypted requests/results under existing TTL and authorization rules. No business wait state. |

### Deadline and cancellation contract

Compute each native slice from the remaining user deadline and actual request/poll lifetimes, reserving time for result encoding/delivery. The Host clamps the slice again against the received absolute expiry and applies an outer abort deadline to observation AND native assistant-output assembly. Native timeout ends only that slice when the overall budget remains; Core may issue the next read-only slice. A request/poll expiry or source error stays an error, not a fabricated successful timeout.

Provide typed observation cancellation keyed to the exact device/channel/session/wait identity. Cancel/reconnect/disconnect uses bounded best-effort remote cancellation; expiry is the cleanup fallback and prevents abandoned waits accumulating. No cancellation path invokes Agent interruption. Wait IDs are ephemeral observation identities, not command IDs or content-bearing history cursors. Establish lifecycle tests before enabling wait; current query handling has no such signal propagation yet.

### Native parity and deliberate adaptations

- Cold `queue=null` remains unavailable for queue editing, but does not prevent native wait from returning idle.
- A live `running=false` sample is insufficient for idle: native `whenIdle()` also waits for maintenance and driver settlement.
- WhaleRein adds transport authorization, scoped cancellation and claimed-input reconciliation; it does not invent queue-empty or single-observer admission conditions.
- The public API remains state-only. Calling native wait still incurs its output read; output failures, cost and cancellation remain real. Do not claim a pure-state native RPC or constant-cost read.
- Shares normalized input types with questions, without requiring questions or their UI to ship first.

## Acceptance Review

| AC ID | Scenario and expected result | Verification | Source |
|---|---|---|---|
| AC-01 | Passive scoped state: Report active/running state, queue availability and normalized input reasons without content; combine native pending events with still-active claimed handles and remove settled or expired handles | Adapter/Host/Core/CLI fixtures and targeted native checks for source behavior | [Scenario](specs/session-wait/spec.md#ac-01) |
| AC-02 | Native stopping conditions: The cold session may return native idle without activation; the live session waits for native whenIdle and current-agent identity checks. An active claimed human-input request takes precedence over reporting idle | Adapter/Host/Core/CLI fixtures and targeted native checks for source behavior | [Scenario](specs/session-wait/spec.md#ac-02) |
| AC-03 | Deadline and CLI policy: Only the overall deadline produces terminal timeout and the last valid state, with exit 0 or explicit exit 7. A successful slice timeout with budget remaining continues under AC-11; no-first-state and transport failures remain errors | Adapter/Host/Core/CLI fixtures and targeted native checks for source behavior | [Scenario](specs/session-wait/spec.md#ac-03) |
| AC-04 | Silent input changes: Retain native one-second refresh behavior and promptly reconcile claimed handles. Do not promise the former synthetic six-second SLA or scan conversation text to infer pending input | Adapter/Host/Core/CLI fixtures and targeted native checks for source behavior | [Scenario](specs/session-wait/spec.md#ac-04) |
| AC-05 | Cancel observation only: Abort the native wait, including its output read, release or expire the exact scoped observation, and never call native Agent cancel, answer an input request, or remove queued work | Adapter/Host/Core/CLI fixtures and targeted native checks for source behavior | [Scenario](specs/session-wait/spec.md#ac-05) |
| AC-06 | Independent read-only waits: Keep their identities and cancellation independent without opening a second MQTT consumer for waiting. Apply only existing transport/resource constraints, not an invented one-wait-per-Core rule; one cancellation leaves the other operation intact | Adapter/Host/Core/CLI fixtures and targeted native checks for source behavior | [Scenario](specs/session-wait/spec.md#ac-06) |
| AC-07 | Authorization and scope: Reject or discard the result appropriately, cancel the affected observation, and retain typed errors without leaking foreign state or interpreting source failure as authorization revocation | Adapter/Host/Core/CLI fixtures and targeted native checks for source behavior | [Scenario](specs/session-wait/spec.md#ac-07) |
| AC-08 | Recovery without resetting the deadline: Bound or cancel the old observation, repeat only a read-only wait with the remaining budget, discard old-channel results, and never extend the user deadline. Request expiry or transport failure is not silently converted to a business timeout | Adapter/Host/Core/CLI fixtures and targeted native checks for source behavior | [Scenario](specs/session-wait/spec.md#ac-08) |
| AC-09 | Capabilities and offline behavior: Return unsupported_capability or the existing offline/unavailable error; leave normal history, approvals and tail behavior available | Adapter/Host/Core/CLI fixtures and targeted native checks for source behavior | [Scenario](specs/session-wait/spec.md#ac-09) |
| AC-10 | Native work and privacy boundaries: Discard native output before constructing the typed state-only result, preserve output-read errors and cancellation, account for envelope size, and keep logs/evidence free of message or tool content | Adapter/Host/Core/CLI fixtures and targeted native checks for source behavior | [Scenario](specs/session-wait/spec.md#ac-10) |
| AC-11 | Continue completed native wait slices: continue read-only waiting with the remaining total budget; return the later stopping result or one terminal overall timeout, never reset the deadline, start Agent work, or interpret the first successful slice timeout as final | Core simulated-clock cases, Host deadline fixtures and controlled native multi-slice waiting | [Scenario](specs/session-wait/spec.md#ac-11) |
| AC-12 | Separate transport expiry from business timeout: return the actual transport/source error, bound or abort its exact observation, reject late/stale results, and do not report successful business timeout; verify the per-slice budget reserved room for encoding and delivery | Delay/fault injection across Core/Host/result delivery and native output cancellation | [Scenario](specs/session-wait/spec.md#ac-12) |

## Risks, rollout and evidence gaps

Native output reads can outlive the stopping timer; an outer request-expiry signal must cover them. Claimed input providers are not automatically visible to native pending-input sampling; their actual lifecycle must be integrated and tested. Test slow forwarding/encoding, maintenance with running=false, cold state, no first state, claimed approval/question changes, overlapping waits/tail, revocation and abandoned-client cleanup. Keep wait disabled if these contracts cannot be established; other verified features may still ship.

Use additive typed protocol/capability fields and the existing encrypted route. Roll back by withdrawing the wait capability and cancelling scoped observations; retain all Agent work. Maintain docs only for verified public contracts/usage; keep dated results local. No native runtime acceptance has been performed during this review.

## Capability acceptance gates

These are logical operation gates, not mandatory new wire flags. Follow the [evidence contract](../../README.md#acceptance-evidence-contract): all listed AC variants need current evidence, and source behavior needs the mapped native procedures. A flag covering several operations is enabled only when all its operation gates pass. Negative unsupported-capability tests passing do not enable an unverified operation. Existing shipped operations remain governed by their existing support; report any regression separately.

| Operation | Required ACs (all mandatory variants) | Required native evidence subset |
|---|---|---|
| Native-backed waiting | AC-01–AC-12 | AC-01, AC-02, AC-04, AC-05, AC-09–AC-11 |

Record each operation as enabled or disabled, its gate evidence, blockers and tested native version in the delivery summary. Keep any disabled path unavailable and retain FAIL/BLOCKED/NOT RUN results. A partial rollout is not full acceptance. The [combined workflow cases](../add-dsh-session-creation/design.md#combined-workflow-acceptance) qualify the integrated feature set separately and do not block independent delivery of verified operations.
