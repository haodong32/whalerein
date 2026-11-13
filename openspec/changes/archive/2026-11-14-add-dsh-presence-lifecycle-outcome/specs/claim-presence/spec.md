# claim-presence

## ADDED Requirements

### Requirement: Device offline releases the exact question scope
When Access observes a device disconnect, Host SHALL abort the existing request
scope for that device. The adapter's single `finally` SHALL release the native
question claim; no second claim registry or lease SHALL be introduced.

#### Scenario: Hard disconnect
- **WHEN** a remote open-question claimant is killed while holding its native claim
- **THEN** Host releases the claim through the device-offline path, without waiting for the request deadline
- **AND** a later device can answer the same batch once

#### Scenario: Presence unavailable
- **WHEN** broker presence cannot be observed
- **THEN** the request deadline remains the fallback
- **AND** the limitation is reported, not hidden

### Requirement: HTTP polling lease for one-shot commands
One-shot CLI commands use the Access HTTP API rather than a persistent MQTT
device session. Access SHALL treat a non-terminal request's HTTP result polling
as the client liveness signal. When polling stops for the lease window, Access
SHALL publish a bounded `request_stale` event to the owning Host's exact
presence topic. Host SHALL abort the request scope; the adapter `finally`
remains the only native claim release.

#### Scenario: Killed answer process
- **WHEN** an answer process is killed and stops polling its request
- **THEN** Access publishes `request_stale` after the lease window
- **AND** Host releases the native claim before the request deadline
- **AND** a later answer can settle the same batch once

### Requirement: Host-scoped presence
Access SHALL publish one `device_offline` event per authorized host on
`whalerein/v1/h/<host_id>/presence`. Host SHALL subscribe only to its exact
presence topic. Device wills SHALL be accepted only for the publishing device.

#### Scenario: Spoofed device event
- **WHEN** another device publishes an offline event for a different device
- **THEN** ACL denies it and Host does not abort that device's scope
