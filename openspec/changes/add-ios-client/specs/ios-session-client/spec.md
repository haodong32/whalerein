# iOS Session Client

## ADDED Requirements

### Requirement: Native iPhone application

The client SHALL run on iOS 18 and later and use the same Rust Core behavior as
the upstream clients. It SHALL use SwiftUI native interaction and system
appearance throughout the primary flows.

#### Scenario: AC-01 Device and simulator builds use the same Core revision

- **WHEN** the application is built for an iPhone and for an iOS Simulator
- **THEN** the two builds SHALL load their target-specific Rust library and
  expose the same versioned behavior from the recorded Core source revision.

### Requirement: Standalone invitation and Host verification

The client SHALL accept an invitation by opening a link or pasting it and
present the authenticated Host fingerprint for explicit confirmation. Core
owns the enrollment and recovery state.

#### Scenario: AC-02 Pair or resume the same invitation

- **WHEN** the user confirms the fingerprint and the pairing finishes
- **THEN** the client SHALL open the authorized Host's session list.
- **AND WHEN** the response to a confirmed pairing is lost and the app restarts
- **THEN** Core SHALL resume the original attempt within its validity window
  and SHALL ask for a fresh invitation if that window expires.

#### Scenario: AC-03 Decline or reject invalid pairing

- **WHEN** the user declines, leaves before confirming, or a fingerprint or TLS
  identity check fails
- **THEN** the client SHALL show the applicable outcome and SHALL leave the
  device unpaired to that Host. A late confirmation SHALL have no effect.

### Requirement: Find and observe an existing session

The client SHALL scope session selection to the current profile and Host,
sort by recent activity, and offer to continue the last session.

#### Scenario: AC-04 Navigate across Hosts and sessions

- **WHEN** the user switches Hosts, reopens the app, or changes the visible
  session while an earlier watch has pending updates
- **THEN** the displayed session list and recent selection SHALL belong to the
  selected Host, and old-watch updates SHALL stay out of the new conversation.

#### Scenario: AC-05 Observe and recover the transcript

- **WHEN** an existing session is opened, emits output, and then loses its
  live connection or the app moves to the background
- **THEN** the client SHALL display history and incremental output in order,
  retain the last readable history with the appropriate connection status,
  and recover missing history and current state on return without duplicates.

### Requirement: Send, direct, and interrupt work

The client SHALL rely on Core and the Source for delivery capability and
command outcomes. It SHALL persist the draft until a successful submission
or an explicit user edit removes it.

#### Scenario: AC-06 Send while idle or choose a supported running delivery mode

- **WHEN** the user sends while idle, or sends during a run with Steer or Queue
- **THEN** idle Send SHALL start work and running Send SHALL use the selected
  mode, with Steer selected by default. Queue SHALL show acceptance only after
  the Source confirms it.
- **AND WHEN** the chosen mode is unavailable
- **THEN** the client SHALL keep the draft and offer the applicable explicit
  action.

#### Scenario: AC-07 Recover an uncertain send or stop

- **WHEN** a send or interrupt response is lost after submission
- **THEN** the client SHALL show an uncertain result, recover using the original
  request identity, and avoid duplicate execution. Stop SHALL remain pending
  until the app receives the relevant runtime settlement.

### Requirement: Review and resolve approvals

The client SHALL present pending approvals with enough information to
identify the requested action. Core SHALL bind the user's decision to the
approval version and digest they reviewed.

#### Scenario: AC-08 Resolve a current approval

- **WHEN** the user reviews a supported, current approval and selects Allow
  Once or Reject
- **THEN** the client SHALL submit the matching decision once and show the
  confirmed result on the original card.

#### Scenario: AC-09 Handle an expired, changed, or resolved approval

- **WHEN** an open approval expires, changes, or is resolved on another device
- **THEN** a stale decision SHALL be unable to authorize it. The app SHALL
  refresh the card and require a fresh decision for a changed pending request.

### Requirement: Protect device data and support recovery

The client SHALL keep secrets device-local, scope encrypted history and
drafts to their profile and Host, and use Core for redacted diagnostics.

#### Scenario: AC-10 Preserve only authorized data across the device lifecycle

- **WHEN** a device restarts, switches Hosts or profiles, or receives confirmed
  revocation
- **THEN** a restart SHALL restore the valid identity and session draft,
  switching SHALL show only the selected scope, and revocation SHALL clear
  the affected authorization, cached history and drafts while keeping the
  device identity. A backup restored to another phone SHALL not clone the
  original identity.

#### Scenario: AC-11 Export connection details with user confirmation

- **WHEN** HTTPS history remains reachable while the MQTT subscription fails
  and the user inspects and shares diagnostics
- **THEN** the report SHALL distinguish the failing stage, show version and
  correlation information, and exclude message content, tool parameters,
  credentials and invitations. Export or sharing SHALL follow user
  confirmation.

### Requirement: Read and compose using native accessible controls

The conversation SHALL use native text for live previews, Markdown for
committed assistant text, and typed native views for tool output and
approvals. The UI SHALL support Simplified Chinese and English, VoiceOver,
Dynamic Type, and system light and dark appearance.

#### Scenario: AC-12 Read a long conversation while new output arrives

- **WHEN** the user reads older messages, expands a tool result or selects
  code, edits a draft, or changes system appearance during streaming
- **THEN** the app SHALL preserve the relevant reading position, expansion,
  focus and draft while updating the current reply. The user SHALL be able
  to return to the latest output and copy a code block with native controls.

#### Scenario: AC-13 Complete the core flows with accessibility settings

- **WHEN** pairing, session selection, composition and approval are exercised
  in both languages with VoiceOver, an accessibility text size and each
  appearance
- **THEN** localized controls SHALL remain readable and operable, focus SHALL
  follow a useful order, and labels or state SHALL remain understandable
  independent of color.
