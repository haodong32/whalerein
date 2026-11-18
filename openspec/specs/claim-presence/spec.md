# claim-presence Specification

## Purpose
Define how a device that stops serving a request is detected and how its
question claim is released without adding a second ownership registry.

## Requirements

### Requirement: HTTP polling lease for one-shot commands

One-shot commands SHALL maintain liveness for their exact authenticated device/request pair. Only that request's valid polling or explicit heartbeat SHALL renew its live unexpired lease, bounded by its original deadline. Expiry SHALL be irreversible and restart-safe, with retryable exact cancellation delivery; no second native question-ownership registry SHALL be introduced.

#### Scenario: Killed answer process

- **WHEN** an answer process is killed and stops polling or heartbeating its request
- **THEN** Access publishes request_stale after the lease window and Host releases the native claim before the request deadline on the supported healthy path
- **AND** a later device can answer the same still-active batch once

This retained scenario binds to AC-28.baseline-killed-process in this change's acceptance matrix.

#### Scenario: AC-26 Unrelated same-device traffic cannot renew an answer

- **WHEN** another request is submitted, polled, or heartbeated using the same device while answer R receives no renewal
- **THEN** only the addressed request is affected and R expires after its existing 10-second silent lease rather than being kept alive by unrelated traffic
- **AND** wrong-device renewal is rejected and legitimate renewal of its own R succeeds

#### Scenario: AC-27 Original answer heartbeat survives follow-up queries

- **WHEN** Core waits through answer submission/acceptance and terminal result queries using other request IDs
- **THEN** Core maintains the original R explicitly within its existing lease, follow-up queries do not substitute for renewal, and completion/cancellation stops that renewal without a detached heartbeat
- **AND** result/deadline progress is not blocked by heartbeat I/O

#### Scenario: AC-28 Killed answer releases despite unrelated live activity

- **WHEN** an answer process dies while a sibling process keeps polling or a session observer remains connected
- **THEN** R becomes stale after the lease window, exact cancellation releases its supported native claim before the hard deadline on a healthy delivery path, and another explicit answer can subsequently settle the same still-active batch once
- **AND** the sibling process and observer remain usable

#### Scenario: AC-29 Renewal and expiry have one monotonic winner

- **WHEN** renewal races the expiry transaction, or a heartbeat arrives after the lease has expired
- **THEN** renewal that validly wins keeps R live, expiry that wins closes it permanently, and late polling can recover outcomes but cannot revive execution or reset the original deadline

#### Scenario: AC-30 Restart and publication failure preserve expiry facts

- **WHEN** Access restarts with live/expired requests or the first cancellation publication fails
- **THEN** original lease deadlines and irreversible closure survive, exact cancellation can be retried from retained metadata without reopening execution, and duplicate delivery cannot cancel a fresh request R2
- **AND** no stale question/answer content is persisted to implement liveness

### Requirement: Host-scoped presence

Access SHALL publish authorized, bounded presence/cancellation metadata only to the exact owning Host presence topic. Device wills SHALL remain limited to their publishing device. Cancellation SHALL identify the exact existing execution scope and SHALL NOT expose question/answer content or cancel a foreign request.

#### Scenario: Spoofed device event

- **WHEN** another device publishes an offline event for a different device
- **THEN** ACL denies it and Host does not abort that device's scope

This retained scenario binds to AC-34.baseline-spoofed-device in this change's acceptance matrix.

#### Scenario: AC-34 Spoofed or foreign presence cannot cancel another scope

- **WHEN** a device spoofs another device's will, or a cancellation carries a foreign Host/device/request/command binding
- **THEN** authorization or exact-scope validation rejects it without exposing question data or cancelling the foreign execution
- **AND** the corresponding valid exact-scope event remains effective and no wildcard ownership is inferred

### Requirement: Observation disconnect is independent of HTTP execution

Losing or closing a session observer SHALL stop its observation resources without cancelling an independently live HTTP answer. Secure-channel replacement alone SHALL NOT be interpreted as authorization revocation or proof that the answer caller died.

#### Scenario: AC-25 MQTT observation ends while the HTTP answer remains live

- **WHEN** a tail/GUI observation disconnects or its channel is replaced while answer request R continues valid HTTP renewal
- **THEN** observation ends or reconnects as appropriate, R can still complete or recover through its original command identity, and no unrelated native claim is released solely by that transport event
- **AND** stopping R while observation remains connected still expires only R

### Requirement: Cancellation fences delayed admission

A closed execution SHALL NOT be revived by delayed delivery or renewal. Matching cancellation received before Host admission SHALL fence that request's later execution while preserving any already proven native outcome. A new explicit request SHALL remain independent.

#### Scenario: AC-31 Cancellation precedes request admission

- **WHEN** exact cancellation arrives before the encrypted request is registered, is duplicated, or is followed by delayed delivery and renewal of that request
- **THEN** the closed request never invokes the native answer, duplicate cancellation is harmless, and a fresh request R2 remains unaffected
- **AND** cancellation after admission cannot overwrite a proven accepted result

### Requirement: Expiry notification is not instantaneous rollback

Request-lease expiry SHALL close renewal/forwarding authority at Access and initiate exact Host cancellation. Native acceptance racing that notification SHALL retain its confirmed or unknown outcome; the expiry timestamp alone SHALL NOT prove that no answer occurred.

#### Scenario: AC-32 Native acceptance wins during cancellation transit

- **WHEN** Access expires R while cancellation is in flight and the native source accepts R before Host can prevent entry
- **THEN** the original receipt or conservative unknown remains recoverable, no fabricated no-effect result is returned, and the answer is not automatically invoked again

### Requirement: Authorization remains stronger than observation liveness

Applicable authorization revocation SHALL fence every affected observation and execution regardless of a fresh request heartbeat. Transport disappearance SHALL NOT substitute for this authorization event or weaken its scope.

#### Scenario: AC-33 Revocation stops the applicable live scopes

- **WHEN** a Host-scoped or device-wide revoke affects an active observer and answer R
- **THEN** the affected authority cannot expose question content or admit another native answer, its active scopes are cancelled according to native outcome semantics, and unrelated valid Host authority remains usable

### Requirement: Hard deadlines remain an independent fallback

When request liveness cancellation cannot be delivered or native cancellation cannot finish, the existing hard request deadline SHALL bound the supported execution scope and degradation SHALL be explicit. Publishing an expiry event SHALL NOT itself be reported as proof of native claim release.

#### Scenario: AC-35 Dependency failure uses an honest bounded fallback

- **WHEN** Access, PostgreSQL, MQTT, or native cancellation is unavailable during lease expiry
- **THEN** no false renewal or release success is reported, the independent existing request deadline/verified native lifecycle remains the fallback, and later supported recovery preserves outcomes without manufacturing an answer
- **AND** evidence distinguishes event publication, Host cancellation observation, actual native cleanup, and any unverified native limitation
