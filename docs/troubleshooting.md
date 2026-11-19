# Troubleshooting

Start with the CLI error code and Diagnostic ID. Search Access/plugin logs by
that ID as `trace_id`; masked request/device IDs differ between services.

Add `--verbose` to include client configuration, request, and connection stages
on stderr. Add `--json` to capture diagnostic events as individual JSON lines
and command results on stdout. For configuration errors, use `configuration_failed`
and its `code` to identify the failing stage.

`whalerein doctor` reads local diagnostics; `whalerein host list` checks authorization
and broker-derived presence. Export with `whalerein doctor --export <file>` after
review/confirmation. Follow the [diagnostic privacy rules](security.md#diagnostics).

## Symptoms

| Symptom | Check / action |
|---|---|
| Access unreachable | Verify advertised URL, listener and TLS/CA trust; probe `/healthz` and `/readyz`. |
| Liveness passes, readiness fails | Inspect PostgreSQL and Access MQTT connection/subscription. |
| Host offline | Check plugin host ID, MQTT credentials, subscription and HostReady; presence must recover after Access restart. |
| Channel rejected | Core retries read-only queries on a fresh channel; verify token rejection before re-pairing. |
| Device revoked | Confirm Access rejected the stored token, then pair again. |
| Command timeout/`unknown` | Reconcile the original command result; do not blindly repeat it with a new ID. |
| Tail gap/stall | Check channel/cursor recovery; run `whalerein session watch <id>`. |
| Approval rejected | Refresh pending approval state, digest, version and expiry. |
| Large snapshot fails | Check peer capability, ACK ACLs and [transport/HTTP limits](../proto/v1/WIRE.md#transport-size-budget). |
| Duplicate device names | Identify the obsolete device ID before revoking it. |

A disconnect alone proves neither revocation nor packet overflow. Preserve
pairing files during network/server diagnosis. Transport receipts and HTTP 200
do not prove command execution.

## Local stack

Use `docker compose -f deploy/compose/stack.yaml ps` for development services.
Default Access probes are `http://127.0.0.1:8080/healthz` and `/readyz`; see
[deployment](../deploy/README.md). Integration uses independent container-owned
services without published ports. Inspect its saved run report under `.data/runs/`
or the selected output directory; see [integration](../tests/integration/README.md).

`doctor --clear-diagnostics` removes diagnostic history, not pairing.
