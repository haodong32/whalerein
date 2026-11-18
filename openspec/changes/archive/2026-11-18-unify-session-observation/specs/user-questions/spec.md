# User Questions Delta

## ADDED Requirements

### Requirement: Discovery is independent of answer ownership

Automatic observation and explicit question reads SHALL preserve normalized native decision context without acquiring a native answer claim or maintaining a submitted answer's liveness. Editing a local answer SHALL NOT reserve a question.

#### Scenario: AC-19 Reading and editing do not own or renew an answer

- **WHEN** questions are observed, listed, fetched in detail, or edited while another answer request is pending
- **THEN** those activities acquire no native claim, submit no answer, and do not renew that pending execution
- **AND** the original question/detail/header, labels/descriptions, intent, selected labels, and optional custom text remain faithful to native data

### Requirement: Observed state does not grant submission authority

An answer SHALL revalidate its exact authorized Host/session/batch, native owner, epoch, version, digest, phase, and deadline at invocation. Collection freshness SHALL NOT replace these guards, and an open answer SHALL NOT silently switch to the continued path.

#### Scenario: AC-20 Changed owner or phase rejects stale submission

- **WHEN** the displayed batch changes owner, epoch, version, digest, or phase before native acceptance, or local and remote answerers compete
- **THEN** stale or losing submissions fail without fallback messages or a second native settlement; unchanged valid submissions remain usable
- **AND** a fresh current batch can be answered after explicit user review

### Requirement: Answer presentation preserves native outcomes

Observed questions and submitted answers SHALL retain the distinction between open settlement, continued reply admission, unavailable content, and offline state. Normalized user input SHALL follow existing verified native validation rather than invented selection or free-text restrictions.

#### Scenario: AC-21 Supported answers retain exact meaning

- **WHEN** a valid explicit open/continued answer is submitted, or the user attempts offline/unavailable/stale submission
- **THEN** accepted open answers report settlement, accepted continued replies report reply_queued, unsupported/offline/stale attempts fail explicitly, and no ordinary chat message is manufactured
- **AND** accepted native labels/custom values are forwarded exactly while invalid question IDs/types remain rejected

### Requirement: Native acceptance evidence survives caller expiry

Confirmed native acceptance SHALL remain confirmed evidence despite later caller loss or bookkeeping delay. Possible acceptance without proof SHALL remain unknown and recoverable with the original command identity. Cancellation delivery SHALL NOT imply instantaneous distributed rollback.

#### Scenario: AC-23 Cancellation races native acceptance

- **WHEN** Host observes cancellation before native entry, or native acceptance wins while cancellation travels or completion is recorded after the deadline
- **THEN** proven pre-effect cancellation performs no native settlement, while a winning confirmed acceptance retains its receipt or conservative unknown if durable proof is unavailable
- **AND** no path reports a proven no-effect cancellation merely from an Access expiry timestamp or automatically repeats the answer

### Requirement: Recovery is independent of the conversation observer

Answer admission and recovery identity SHALL survive observation close/replacement. Recovery SHALL query the original command under its original principal and SHALL NOT reopen an expired execution, recreate a native handle, or infer success from question disappearance.

#### Scenario: AC-24 Lost receipt and observer replacement preserve one invocation

- **WHEN** an answer's receipt or terminal journal write is lost and its conversation observer closes or switches sessions
- **THEN** the operation remains attributed to its original session/principal, recovery returns its supported recorded outcome without another native invocation, and the new observer receives no misattributed result
- **AND** same-command changed-payload reuse remains rejected and question disappearance alone is not treated as acceptance

## MODIFIED Requirements

### Requirement: Release open-question claims

Cancellation of the matching answer execution, its native claim expiry, or loss of its applicable authorization SHALL release the exact open-answer claim through the native lifecycle without manufacturing an answer. Observation disconnect alone SHALL NOT cancel an independently live HTTP answer. An old handle SHALL NOT claim later requests.

#### Scenario: AC-14 Release open-question claims

- **WHEN** a remote open-answer execution loses its caller liveness, cancels, or loses its claim before answering, including a late submission after release
- **THEN** release or expire the exact claim; preserve native local/provider handling of still-active or subsequent questions, reject stale submissions without a second settlement, and prevent the old handle from claiming later requests
- **AND** keep remote open capability disabled if this cannot be verified; an observation-only disconnect does not establish loss of an independently live HTTP execution

This existing capability-scoped AC-14 is retained for synchronization identity and binds to this change's AC-22.baseline-release-scenario. It is distinct from session-observation/AC-14.

#### Scenario: AC-22 Exact native cleanup does not affect another execution

- **WHEN** an answer is cancelled, its native claim expires, or its applicable authorization is revoked, including a late submission after release
- **THEN** the exact native claim is released or expires, stale submissions cannot settle twice, and native local/provider handling of other active or subsequent questions is preserved
- **AND** an old cleanup callback cannot release a newer attempt's claim; an unchanged valid native path and a later fresh answer remain usable
- **AND** remote open capability stays disabled if native ownership, arbitration, and cleanup cannot be verified
