# Session wait specification delta

## Purpose

Provide remote native-backed stopping decisions without reimplementing DSH quiescence or changing Agent execution.

## ADDED Requirements

### Requirement: Passive scoped state

Native state and active input handles SHALL remain source-owned and typed. Passive reads SHALL NOT activate a cold Agent.

<a id="ac-01"></a>
#### Scenario: AC-01 Passive scoped state

- **WHEN** an authorized caller reads live or cold state, including input currently claimed by the adapter
- **THEN** report active/running state, queue availability and normalized input reasons without content; combine native pending events with still-active claimed handles and remove settled or expired handles

### Requirement: Native stopping conditions

The adapter SHALL preserve native wait semantics instead of deriving idle from a Core queue-empty rule.

<a id="ac-02"></a>
#### Scenario: AC-02 Native stopping conditions

- **WHEN** a cold session has active=false and queue=null, or a live session reports running=false while maintenance is unfinished
- **THEN** the cold session may return native idle without activation; the live session waits for native whenIdle and current-agent identity checks. An active claimed human-input request takes precedence over reporting idle

### Requirement: Deadline and CLI policy

Waiting SHALL default to 30000 ms and accept positive integer milliseconds up to the native timer limit 2147483647. CLI seconds SHALL convert exactly to supported milliseconds. Timeout SHALL preserve its typed result.

<a id="ac-03"></a>
#### Scenario: AC-03 Deadline and CLI policy

- **WHEN** the overall user deadline expires after a valid state, or the caller supplies invalid bounds
- **THEN** return terminal timeout and the last valid state with exit 0 by default or 7 with --fail-on-timeout; reject invalid bounds before observation. A successful native slice timeout with budget remaining follows AC-11 instead. A native no-first-state timeout or transport failure remains an error, not a fabricated observation

### Requirement: Silent input changes

Native wait SHALL remain authoritative for silent input changes; the adapter SHALL observe the lifecycle of its own claimed requests.

<a id="ac-04"></a>
#### Scenario: AC-04 Silent input changes

- **WHEN** native input changes without a history event, or an adapter-claimed request opens, settles or expires
- **THEN** retain native one-second refresh behavior and promptly reconcile claimed handles. Do not promise the former synthetic six-second SLA or scan conversation text to infer pending input

### Requirement: Cancel observation only

Cancelling a wait SHALL cancel only its native observation and cleanup its scoped resources.

<a id="ac-05"></a>
#### Scenario: AC-05 Cancel observation only

- **WHEN** the user cancels or the source/request/channel lifetime ends
- **THEN** abort the native wait, including its output read, release or expire the exact scoped observation, and never call native Agent cancel, answer an input request, or remove queued work

### Requirement: Independent read-only waits

Wait SHALL use read-only queries, not the tail/watch observation-admission guard.

<a id="ac-06"></a>
#### Scenario: AC-06 Independent read-only waits

- **WHEN** two valid waits overlap, or a wait overlaps an existing tail
- **THEN** keep their identities and cancellation independent without opening a second MQTT consumer for waiting. Apply only existing transport/resource constraints, not an invented one-wait-per-Core rule; one cancellation leaves the other operation intact

### Requirement: Authorization and scope

The existing authorization and channel/session scope SHALL apply before invocation and result delivery.

<a id="ac-07"></a>
#### Scenario: AC-07 Authorization and scope

- **WHEN** a device is revoked, a result belongs to another wait/session/channel, or native observation fails
- **THEN** reject or discard the result appropriately, cancel the affected observation, and retain typed errors without leaking foreign state or interpreting source failure as authorization revocation

### Requirement: Recovery without resetting the deadline

Reconnection SHALL retain the overall deadline and use a fresh scoped native observation.

<a id="ac-08"></a>
#### Scenario: AC-08 Recovery without resetting the deadline

- **WHEN** the channel changes while a wait is active
- **THEN** bound or cancel the old observation, repeat only a read-only wait with the remaining budget, discard old-channel results, and never extend the user deadline. Request expiry or transport failure is not silently converted to a business timeout

### Requirement: Capabilities and offline behavior

Only verified native-backed waiting SHALL be advertised. Cached idle state SHALL NOT satisfy a fresh wait.

<a id="ac-09"></a>
#### Scenario: AC-09 Capabilities and offline behavior

- **WHEN** the source lacks wait support or only offline cache is available
- **THEN** return unsupported_capability or the existing offline/unavailable error; leave normal history, approvals and tail behavior available

### Requirement: Native work and privacy boundaries

Only the adapter SHALL handle native wait cursors/output. Public waiting SHALL NOT expose assistant content or persist a second content log.

<a id="ac-10"></a>
#### Scenario: AC-10 Native work and privacy boundaries

- **WHEN** native wait assembles bounded assistant output after a stopping state or a result reaches the transport budget
- **THEN** discard native output before constructing the typed state-only result, preserve output-read errors and cancellation, account for envelope size, and keep logs/evidence free of message or tool content

### Requirement: Continue completed native wait slices

A successful native slice timeout SHALL NOT end waiting while the overall monotonic user deadline still has budget.

<a id="ac-11"></a>
#### Scenario: AC-11 Continue completed native wait slices

- **WHEN** one or more native slices return timeout with valid states, then a later slice returns idle/input-required or the overall deadline expires
- **THEN** continue read-only waiting with the remaining total budget; return the later stopping result or one terminal overall timeout, never reset the deadline, start Agent work, or interpret the first successful slice timeout as final

### Requirement: Separate transport expiry from business timeout

Request/poll expiry and native output-read failure SHALL retain error semantics and bounded observation cleanup.

<a id="ac-12"></a>
#### Scenario: AC-12 Separate transport expiry from business timeout

- **WHEN** forwarding, output assembly or result delivery exhausts the request budget while user time remains, or an expired slice replies late
- **THEN** return the actual transport/source error, bound or abort its exact observation, reject late/stale results, and do not report successful business timeout; verify the per-slice budget reserved room for encoding and delivery
