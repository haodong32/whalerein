# Proposal

## Why

Access currently queries PostgreSQL for each relayed event and for device
subscription and receipt authorization. Replacing EMQX with Mosquitto provides
a lighter independent line, provided both authorization paths can make local
decisions after loading the current grant.

## What Changes

- Give Access one bounded Otter v2.4.0 grant cache for event admission and
  device MQTT authorization. PostgreSQL remains authoritative.
- Keep Mosquitto publish, subscription, delivery and Will ACL decisions local.
  Reuse upstream authentication, ACL and management capabilities wherever they
  cover the existing contract. Verify native feasibility before selecting the
  minimum extension.
- Project committed grants to Mosquitto and reconcile them after failure.
  Distinguish committed revocation from confirmed broker enforcement.
- **BREAKING** Replace EMQX and its operator configuration in one release.
  The planned restart disconnects existing MQTT sessions. Existing credentials,
  URLs, TLS identity, topic names and MQTT 5 wire behavior carry forward.
- Keep one active Access per line. Central assignment, line migration,
  multiple active relays and preview protocol changes are outside this change.

## Capabilities

### New Capabilities

- `access-grant-admission` — local authorization for realtime forwarding with
  durable pairing and revocation semantics.
- `mosquitto-line-transport` — local broker ACLs, lifecycle, compatibility,
  hard cut and target-line validation.

### Modified Capabilities

- `claim-presence` — preserve the existing requirements and add acceptance
  coverage for presence recovery and claim release on the replacement broker.

## Impact

Access auth, events, pairing, persistence, MQTT integration and startup wiring
change. Compose, integration fixtures, operator configuration and maintained
architecture documentation follow the new deployment. The application contract
is defined in [Access architecture](../../../docs/architecture/access.md),
[streaming](../../../docs/streaming.md) and the
[wire contract](../../../proto/v1/WIRE.md). Target-line validation records
latency and resource use without a latency threshold, as confirmed in D-06. Broker integration remains subject to the
native feasibility checks in D-05.
