# Design

## Claim release

The request scope plus native `attachWait` handle is the only ownership source.
Access turns broker lifecycle evidence into one host-scoped `device_offline`
event. Host aborts the device's request/model/question/waits scopes. The adapter
`finally` releases the native handle. TTL remains the fallback when presence is
absent. Conditions that cannot be observed remain documented limitations, not
new ownership state.

## LifecycleOutcome

`READY` requires identity. `READY_WITH_WARNING` requires identity and a
workspace-attach warning. `FAILED` is a proven no-effect failure. `UNKNOWN` never
claims identity and carries a reconcile key (`operation_id`,
`preferred_session_id`, `parent_session_id`, or `MANUAL`). Preflight runs before
native entry. The existing command journal records `IN_FLIGHT` before native
entry; a stale record reads as `UNKNOWN`. No second registry, no automatic retry,
no deletion of observed sessions.
