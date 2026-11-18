# Session Observation Delta

## Purpose

Provide one Host-qualified session observation that exposes conversation and current pending questions without client polling loops, coupling question discovery to history, or acquiring native answer ownership.

## ADDED Requirements

### Requirement: One scoped observation with explicit interests

Core SHALL offer one session observation selecting Conversation, Questions, or both, using one device transport owner. Conversation covers committed history, live Attempts, runtime, and Approvals. Control events SHALL NOT create additional subscriptions.

#### Scenario: AC-01 One observation carries five business event families

- **WHEN** a caller observes both interests for an authorized SessionKey
- **THEN** committed history, live Attempts, runtime, Approvals, and Questions are delivered through one scoped observation without a second MQTT consumer
- **AND** unchanged Message, Attempt, approval, and tail completion semantics remain intact

#### Scenario: AC-02 Questions-only observation has no history dependency

- **WHEN** only Questions is selected and history is empty, unavailable, or unsupported
- **THEN** supported pending questions are observed without reading history, requiring a history head, or activating a cold native Agent merely to inspect questions

### Requirement: Independent facet availability

Each selected facet SHALL expose its own readiness and failure. Question views SHALL distinguish Loading, Current, Stale, Unavailable, and Unsupported. Only a successfully completed current collection SHALL establish that no pending questions exist.

#### Scenario: AC-03 Unsupported or failing Questions does not end Conversation

- **WHEN** question capability is absent or its authoritative read fails while Conversation remains available
- **THEN** Conversation continues and Questions reports Unsupported or Unavailable rather than an empty current collection
- **AND** loss of authorization still stops all affected facets

#### Scenario: AC-04 Initial question discovery closes the subscription race

- **WHEN** a question appears during initial subscription or baseline reading, including a shared registration that fails
- **THEN** a successful observer receives the current question through its baseline or subsequent reconciliation, or receives explicit failure/recovery rather than a permanently inert success
- **AND** a question outside committed history remains discoverable

### Requirement: Authoritative semantic question versions

Question observations SHALL carry a Source-owned epoch and monotonically increasing revision covering ordered membership, normalized content, guards, phase, answerability, and unavailable-content metadata. Unrelated text output and unchanged reads SHALL NOT advance that revision.

#### Scenario: AC-05 A change during an in-flight read is not swallowed

- **WHEN** native question state changes while its previous read is awaiting completion
- **THEN** observation performs a subsequent authoritative reconciliation and converges to the latest still-pending state or explicitly reports that reconciliation is unavailable
- **AND** a late old read cannot regress the accepted observation

#### Scenario: AC-06 Unchanged inventory does not retransmit question content

- **WHEN** token output, runtime updates, or periodic refresh occur without a normalized question change
- **THEN** the question stamp remains stable and no additional question-content page query is needed after the installed baseline
- **AND** native reconciliation cost is reported separately from wire-content transfer

### Requirement: Complete same-version question views

Automatic question observation SHALL publish a complete collection only when all pages certify the same current Source epoch/revision. It SHALL reject older pages, stale observer generations, mixed revisions, duplicates, and non-progressing continuation. Explicit mutable list cursors SHALL remain continuation cursors rather than future-event cursors.

#### Scenario: AC-07 Pagination cannot replace a newer target with old or mixed pages

- **WHEN** inventory changes between pages, descriptor revision r2 precedes delayed page r1, or a page belongs to another epoch
- **THEN** no partial or regressed collection becomes Current; reconciliation restarts from the first page against the current authoritative target or reports explicit stale/unavailable state
- **AND** a different epoch is not ordered by its opaque token value

#### Scenario: AC-08 Partial failure or sustained churn retains honest stale state

- **WHEN** a later page fails, a cursor cannot progress, or inventory churn exhausts bounded reconciliation work
- **THEN** the previous complete view remains explicitly stale and the incomplete stage is not presented as Current or empty
- **AND** a later successful stable read installs the complete new collection, including a genuinely empty collection

### Requirement: Reconciliation repairs missed observation

Core SHALL register observation before reading its baseline and reconcile question state after missed notifications, reconnect, or queue loss. Existing watch refresh SHALL repair a lost final change without waiting for another Message or user action.

#### Scenario: AC-09 Lost final notification is repaired by existing refresh

- **WHEN** the final question-change hint is dropped while authoritative question state remains readable
- **THEN** the next successful existing watch-refresh reconciliation detects the changed stamp and installs the current collection without a UI timer or new history output
- **AND** the existing five-second refresh schedule is tested with a controlled clock, without claiming a new end-to-end SLA during dependency failure

#### Scenario: AC-10 Reconnect and Source replacement discard obsolete observation

- **WHEN** a channel/watch reconnects, the native owner is replaced, or an event queue reports loss
- **THEN** affected question state is marked stale and rebuilt from current authority; obsolete epoch/generation data cannot restore answerability
- **AND** cached text never recreates a vanished native open handle

### Requirement: Scope and recipient isolation

Every observed value SHALL retain its original Host, Session, observation identity, and generation through buffering and client bindings. Authorization SHALL precede source access and delivery. One recipient's cancellation or backpressure SHALL NOT corrupt another valid recipient's observation.

#### Scenario: AC-11 Old queued content and foreign scope never enter a new session

- **WHEN** old events are drained after switching sessions or Hosts, or a foreign/revoked scope attempts access
- **THEN** neither emitted consumer events nor final views expose foreign content or apply the old watch-end to the new observation
- **AND** valid events for the new authorized scope continue to arrive

#### Scenario: AC-12 Shared observation isolates slow and closing recipients

- **WHEN** two devices observe one Source session and one recipient's committed delivery stalls or its observer closes
- **THEN** the other still receives valid ordered committed/question state, and closing the last recipient releases the native observation resources
- **AND** shared registration failure remains explicit for every affected recipient

### Requirement: Clients consume Core-owned observation

Clients SHALL consume typed Core domain state and explicit user actions. Automatic question discovery, paging, refresh, and transport recovery SHALL remain inside Core and its existing Host/adapter seams. Native structures and wire DTOs SHALL NOT become consumer responsibilities.

#### Scenario: AC-13 Existing consumers migrate without independent polling

- **WHEN** a GUI opens a question-capable conversation and CLI list/detail/tail commands use the new Core
- **THEN** the GUI receives structured question updates without its own discovery loop, and CLI one-shot commands share the authoritative reader while existing tail output/completion behavior remains correct
- **AND** one-shot list/detail remain one-shot; this change does not silently introduce new CLI flags or continuous output

#### Scenario: AC-18 Core remains self-contained and native-neutral

- **WHEN** a client uses the public observation or answer interface
- **THEN** only Core-owned values and stable errors cross the interface, native DSH access stays behind the Host adapter, and SecretStore remains Core's only external injection interface

### Requirement: Coordinated version-one contract replacement

The application protocol SHALL remain version 1 with only the new observation contract supported. Matching artifacts SHALL be deployed together without an old-contract runtime path, hidden second version, or discovery fallback. Unchanged version numbers SHALL NOT be represented as proof of mixed-build compatibility.

#### Scenario: AC-14 New version-one artifacts enforce the new contract

- **WHEN** matching new-contract artifacts communicate or an observation omits required new fields
- **THEN** matching artifacts operate with protocol_version 1, detectable invalid/missing fields fail explicitly rather than becoming empty Questions, and no compatibility reader/fallback is invoked
- **AND** deployment checks reject a mismatched artifact set without promising that the version number detects every older build

#### Scenario: AC-15 Hard cut preserves identity and recovery facts

- **WHEN** old clients/connectors are stopped and a coordinated new-contract set starts against retained state
- **THEN** keys, pins, grants, Source sessions, pager keys, and command recovery identities survive; transient observation is rebuilt and possibly executed actions are never automatically replayed

### Requirement: Bounded private question observation

Question content SHALL remain encrypted in transit and excluded from diagnostics, plaintext operational storage, and history cursors. Pagination SHALL respect actual encoded protocol budgets including overhead. Resource pressure SHALL be explicit rather than hidden truncation or false completeness; new product limits require separate evidence and review.

#### Scenario: AC-16 Large or private collections retain their correct meaning

- **WHEN** a collection spans envelopes, an indivisible batch exceeds its effective budget, or normal/failing observation exports evidence
- **THEN** pages preserve source order and complete-view rules, oversized batches expose bounded unavailable metadata with explicit detail-read failure, and evidence contains no question/answer text or secrets
- **AND** below/at/above tests use verified encoded limits including stamp and envelope overhead

### Requirement: Observation resources follow their owner

Observation SHALL retain the existing one-active-session/profile constraint and release its tasks, staged pages, and scoped delivery on close or replacement. A selected facet SHALL NOT leak work after its owner ends. Resource exhaustion SHALL leave other valid work intact.

#### Scenario: AC-17 Owner close and resource pressure remain bounded

- **WHEN** an observation is closed/replaced, another process competes for the same profile watch, or staging/event admission exhausts its reviewed byte budget
- **THEN** old work stops with no orphan refresh, competing ownership fails deterministically, and resource failure is explicit without a partial Current view or damage to unrelated valid operations
- **AND** a new valid observation can subsequently proceed
