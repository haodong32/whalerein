# lifecycle-outcome Specification

## Purpose
Define the explicit creation/fork receipt contract so callers can distinguish
proven success, partial success with identity, proven failure, and unresolved
outcomes that need reconciliation.

## Requirements

### Requirement: Explicit creation/fork outcome
Creation and fork SHALL return `READY`, `READY_WITH_WARNING`, `FAILED`, or
`UNKNOWN`. `READY_WITH_WARNING` SHALL require an observed identity and a warning
code. `FAILED` SHALL be a proven no-effect failure. `UNKNOWN` SHALL NOT claim an
identity and SHALL carry a reconciliation key.

#### Scenario: Workspace attach fails after creation
- **WHEN** native creation returns an observed identity and workspace attachment fails
- **THEN** the receipt is `READY_WITH_WARNING` with that identity
- **AND** the session is not deleted or recreated

#### Scenario: Native outcome is ambiguous
- **WHEN** native creation entered but no identity can be proven
- **THEN** the receipt is `UNKNOWN` with an operation or preferred-id reconcile key
- **AND** no automatic retry occurs

#### Scenario: Empty fork source
- **WHEN** the source has no completed turn
- **THEN** the receipt is `FAILED` before any native effect
- **AND** no child session is created
