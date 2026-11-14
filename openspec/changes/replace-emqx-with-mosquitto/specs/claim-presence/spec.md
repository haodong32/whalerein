## ADDED Requirements

### Requirement: Presence remains coherent on the Mosquitto line
Access SHALL preserve Host presence transitions and claim release after the
broker replacement. Presence SHALL correspond to the current authenticated
connection and applicable committed grant.

#### Scenario: AC-10 Disconnect and revocation update the current connection
- **WHEN** an authenticated Host connects, disconnects, times out, is revoked or reconnects with the same client ID
- **THEN** watch and claim handling follow the current connection and grant, and any delayed notification for a prior connection leaves the current one intact
- **AND** the existing heartbeat and claim expiry behavior remains available when a broker notification is lost

#### Scenario: AC-11 Access restarts while authenticated Hosts remain connected
- **WHEN** Access reconnects to Mosquitto and rebuilds its presence view while Host sessions remain connected
- **THEN** Access reconciles broker connection state and later notifications without accepting stale online entries or overriding a newer connection
- **AND** affected presence is reported as unknown or unavailable until current state is established
