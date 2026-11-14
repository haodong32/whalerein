# Access Grant Admission

## Purpose

Keep authorization for realtime events and device MQTT operations responsive
while preserving durable grant and revocation semantics across restarts.

## ADDED Requirements

### Requirement: Local checks on the realtime path
Access SHALL enforce the current Host and device grant on event forwarding and
device MQTT authorization. Once the grant is loaded, these decisions SHALL use
local state.

#### Scenario: AC-01 Warm grants avoid per-message database reads
- **WHEN** a warmed and still-authorized device receives repeated live and committed events or sends transport receipts
- **THEN** authorization performs zero per-event or per-receipt PostgreSQL grant reads and zero broker-to-Access per-message authorization requests
- **AND** unrelated warmed grants continue forwarding during a stalled cold lookup or grant write

#### Scenario: AC-02 Cold grants load once per key and fail closed
- **WHEN** a device subscribes for the first time or concurrent callers miss the same grant after restart, expiry or eviction
- **THEN** at most one grant lookup per key is in flight, unrelated grants remain independent, and absent grants or lookup failures grant no access

### Requirement: Durable and ordered grant changes
PostgreSQL SHALL own the committed grant. Access and the broker SHALL enforce
only the applicable grant revision and report revocation as complete only
after durable commit and confirmed broker enforcement.

#### Scenario: AC-03 Pairing becomes usable after both authorization checks are ready
- **WHEN** pairing or re-pairing is committed
- **THEN** the new grant becomes usable after the broker policy and Access admission are ready
- **AND** retry after an interrupted update converges without duplicating enrollment or restoring an older grant

#### Scenario: AC-04 Revocation stops forwarding and survives restart
- **WHEN** a Host-scoped or device-wide revocation starts during forwarding, cold loading, or re-pairing
- **THEN** affected new dispatches stop immediately at Access and older lookups or updates cannot reopen admission
- **AND** existing grants outside the revoked scope remain valid
- **AND** completion requires a durable commit, broker enforcement and closure or confirmed absence of the superseded connection
- **AND** rolled-back or ambiguous writes and missing broker confirmation are distinguishable from completion

#### Scenario: AC-05 Broker permission recovery excludes stale authority
- **WHEN** Access or Mosquitto restarts, a policy update is missed, or a retire-and-rejoin race occurs
- **THEN** policy converges from committed PostgreSQL state and obsolete updates cannot undo a revocation or a newer pairing
- **AND** startup with uncertain policy denies affected delivery and reconnect, and a running broker with a lost control path denies them after its bounded local policy validity expires
- **AND** a restart after a completed revocation cannot re-enable it
