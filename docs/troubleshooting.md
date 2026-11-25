# Troubleshooting

Start with the CLI error code and Diagnostic ID. Search Access/plugin logs by
that ID as `trace_id`; masked request/device IDs differ between services.

Add `--verbose` to include client configuration, request, connection, history,
send-recovery and attachment-download stages on stderr. Add `--json` to capture
diagnostic events as individual JSON lines and one command result on stdout. For
configuration errors, use `configuration_failed` and its `code` to identify the
failing stage.

## Verbose history, send and download workflow

Keep the two streams separate: stdout is the machine result (or watch events),
stderr is diagnostics. Both carry the same `trace_id`.

```sh
whalerein --verbose --json session history <session-id> > page.json 2> history.log
whalerein --verbose --json session send <session-id> "Hello" > send.json 2> send.log
whalerein --verbose --json session download <reference> --host <host> --session <id> --to ./downloads > save.json 2> download.log
```

History stages are `history_page`, `history_projection` and
`history_continuation`. Send stages are `send_reply_confirmed`, `send_input` and
`send_reply_read`. Download stages are `download_open`, `download_admission`,
`download_part`, `download_verify`, `download_commit` and `download_cancel`. A
`snapshot_required` history stage means the cut, projection or continuation
changed: restart with `session history` and no continuation (or omit `--message`
for a fresh reference). For send, `send_reply_read` may
fail after `send_reply_confirmed`; the confirmation stands and the CLI prints the
exact `session history --message` retry. Never resubmit the message.

```sh
jq -c 'select(.stage | startswith("history_") or startswith("send_") or startswith("download_"))' history.log
jq '.data.source, .data.continuation' page.json
jq '.data.reply, .data.read_error' send.json
jq '.data.path, .data.sha256, .data.verified' save.json
```

Download progress is separate from diagnostics: in `--json` mode stderr carries
one `{"event":"download_progress",...}` object per bounded update, and stdout
stays one result envelope written after the verified commit. A failed download
also emits a bounded warning `attachment_transfer` event without `--verbose`.

Stage events carry only stage/outcome/stable code, counts and correlation IDs.
They never include message or reasoning bodies, tool parameters, attachment
contents or names, local paths, raw cursors or credentials. Diagnostic delivery
is bounded and best effort so a slow stderr cannot change the command result.

`whalerein doctor` reads local diagnostics; `whalerein host list` checks authorization
and broker-derived presence. Export with `whalerein doctor --export <file>` after
review/confirmation. Follow the [diagnostic privacy rules](security.md#diagnostics).

## Symptoms

| Symptom | Check / action |
|---|---|
| Access unreachable | Verify advertised URL, listener and TLS/CA trust; probe `/healthz` and `/readyz`. |
| Liveness passes, readiness fails | Inspect PostgreSQL and Access MQTT connection/subscription. |
| Host offline | Check plugin host ID, MQTT credentials, subscription and HostReady; directory presence is Broker-derived and must recover from lifecycle events or a snapshot after Access restart. |
| `protocol_version_incompatible` / upgrade prompt | The peer uses a retired relay epoch (3 or 4) or a retired frame. Deploy the matching epoch-5 Access, plugin, Host, Core and client set; there is no mixed-epoch or fallback path. |
| Retired HTTP handshake/query path fails | Post-pairing Noise and session queries now use the MQTT 5 query lane. Upgrade the client; do not retry the old HTTPS route. |
| Query reply times out | Check that the Device is subscribed to its own `.../d/{device_id}/reply`, the fixed response topic and Correlation Data match, the connection reference is current, and the original deadline has not elapsed. Access restart drops transient waiters; retry a safe read within the caller deadline. |
| Live previews stop while control continues | Previews are QoS 0 and may drop. Check the direct `.../d/{device_id}/live` subscription and the Device grant. A lost preview is repaired by the committed control message or an authoritative history read; it is not a control failure. |
| Watch appears stale | The five-second `CheckVersions` read is read-only and repairs a lost final notification. Check Host readiness and source availability; reconnect creates a new connection generation and observation. |
| Channel rejected | Core retries read-only queries on a fresh channel; verify token rejection before re-pairing. |
| Device revoked | Confirm Access rejected the stored token, then pair again. |
| Command timeout/`unknown` | Reconcile the original command result; do not blindly repeat it with a new ID. |
| Tail gap/stall | Check channel/cursor recovery; run `whalerein session watch <id>`. |
| History continuation rejected | Restart with `session history <id>` and no `--continuation`; re-read `--cache` only when the cached view is known current. |
| Send confirmation but no content | Keep the confirmation; run the printed `session history <id> --message <ref>`. Never resend the message. |
| Attachment download fails | Keep existing destination files. Re-read the attachment row with `session history <id>` and retry a fresh download; add `--force` only to replace a colliding file and `--name` when the suggested name is unsafe. |
| Reasoning missing | Pass `--show-reasoning`; the default hides reasoning without changing authorization. |
| Approval rejected | Refresh pending approval state, digest, version and expiry. |
| Large snapshot fails | Check peer capability, broker ACLs, the direct live/query subscriptions and [transport/HTTP limits](../proto/v1/WIRE.md#transport-budgets-and-configuration). |
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
