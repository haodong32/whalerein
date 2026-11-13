# Tasks

## 1. Presence

- [x] 1.1 Add device MQTT keepalive and last will.
- [x] 1.2 Add Access host-scoped presence topic, ACL and store fan-out.
- [x] 1.3 Add Host exact presence subscription, device channel clear and scope abort.
- [x] 1.4 Add Access/Host unit tests and a Host presence integration case.
- [x] 1.5 Release HTTP-held claims through Access polling leases. The original MQTT-will premise does not cover one-shot CLI commands; HTTP `GET /v1/requests/{id}` polling is the liveness signal.
- [x] 1.6 Add Access client-poll lease sweep, Host `request_stale` cancellation, and a killed-client integration case.
- [x] 1.7 Half-open SIGSTOP release and duplicate/offline dedupe are covered.
- [x] 1.8 Reconnect-window verified: SIGSTOP 5s then SIGCONT keeps the claim, and a later kill releases within 15s.

## 2. LifecycleOutcome

- [x] 2.1 Add proto dispositions/fields and core mapping.
- [x] 2.2 Add adapter native facts and Host preflight/persisted operation.
- [x] 2.3 Add six targeted creation/fork integration cases.
- [x] 2.4 Add four-state CLI rendering and reconcile hints.
- [x] 2.5 Add the core `unknown`-status + UNKNOWN-receipt recovery test; the journal payload now surfaces operation_id/reconcile fields. A full crash-after-native integration case remains a documented gap.
- [x] 2.6 Legacy direct-adapter cases updated to the fact-only contract; `--only test_native_adapter` is 5/5.

## 3. Observability

- [x] 3.1 Access presence metrics/logs.
- [x] 3.2 Host lifecycle/presence structured logs.
- [x] 3.3 Host lifecycle/presence structured logs + Access Prometheus counters (`request_client_stale_total`, presence metrics) are wired. Host has no Prometheus backend in this change; that is documented as a residual.
