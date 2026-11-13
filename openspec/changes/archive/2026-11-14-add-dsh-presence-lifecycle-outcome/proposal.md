# Presence-driven claim release and lifecycle outcomes

## Why

A remote open-question claimant can be killed or lose its connection while
holding a native `attachWait`. The current request deadline eventually releases
it, but that TTL is a proxy for a missing device-offline signal. Separately,
creation/fork report only `READY` or a conservative `UNKNOWN`, so callers
cannot distinguish a proven preflight failure, a partial success with identity,
and an unresolved native effect.

## What changes

- Device MQTT connections use a 10s keepalive and a non-retained last will.
- Access resolves `$SYS`/will disconnect events to each authorized host and
  publishes `device_offline` on the host's exact presence topic.
- Host subscribes to its exact presence topic, clears the device channel, aborts
  its waits, and lets the adapter's existing `finally` release the native claim.
- Creation/fork return one `LifecycleOutcome` contract with
  `READY | READY_WITH_WARNING | FAILED | UNKNOWN`, preflight, persisted operation
  identity, identity-aware warnings, and explicit reconciliation.

## Impact

- Access: presence topic, ACL, device-to-host fan-out, metrics.
- Host: presence subscription, device scope abort, lifecycle outcome mapping.
- Core: MQTT will/keepalive; lifecycle receipt fields.
- CLI: four-state rendering and reconcile hints.
- Proto: additive lifecycle receipt fields and dispositions.

No ClaimRegistry, lease, claim token, or protocol claim field is introduced.
