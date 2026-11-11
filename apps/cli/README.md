# whalerein CLI

Internal design: [CLI architecture](../../docs/architecture/cli.md).

Thin CLI over `whalerein-core`. It parses arguments, renders stable output and
maps errors to WIRE.md exit codes; it does not parse HTTP DTOs, MQTT topics or
Noise messages.

## Commands

```bash
whalerein pair [--name <display-name>] <pairing-uri> [--yes] [--fingerprint <hex>]
whalerein hosts
whalerein sessions --host <host-id>
whalerein tail --session <session-id> [--once] [--timeout <seconds>]
whalerein send --session <session-id> <text>
whalerein approve --request <approval-id> --digest <hex> --version <n>
whalerein reject --request <approval-id> --digest <hex> --version <n>
whalerein interrupt --session <session-id>
whalerein revoke
whalerein doctor
whalerein doctor --export <file>
```

Global flags: `--json`, `--yes`, `--data-dir <path>`.

`send` waits for the host's terminal command result and prints the message id
when one is returned. `tail` prints session output text for `Output` events;
it never prints raw event JSON, tool arguments, keys or tokens. `doctor` prints
the paired Access address and how many local commands are unfinished or have an
unknown result. It does not open a channel, so it does not report connection,
reconnect, protocol, or sync state.

## Output

`--json` emits one stable object:

```json
{"schema":"whalerein/v1","command":"hosts","data":{}}
{"schema":"whalerein/v1","command":"hosts","error":{"code":"unauthorized","message":"..."}}
```

Exit codes: `0` success, `1` internal, `2` usage/input, `3` not found,
`4` unauthorized/revoked, `5` host offline/unavailable, `6` host rejected,
`7` timeout/unknown, `8` conflict, `9` unsupported/incompatible.

## Environment

| Variable | Meaning |
|---|---|
| `WHALEREIN_DATA_DIR` | Local config/cache directory |
| `WHALEREIN_SECRET_BACKEND` | `keyring` (default), `memory` (tests), `file:<dir>` (test harness only) |
| `WHALEREIN_MQTT_URL` | Override the broker endpoint derived from the Access URL |
| `WHALEREIN_CA_FILE` | Private CA PEM for HTTPS and MQTT |
| `WHALEREIN_LOG` / `RUST_LOG` | Log filter written to stderr |

The `file:<dir>` and `memory` secret backends provide no OS keyring protection;
use them only for tests and the integration harness.

## Resume standalone pairing

If a confirmed pairing loses its FINISH response, run `whalerein pair-resume`
with the same data directory and SecretStore. The original approved Host identity
and request are retained securely; this command does not ask for a new invitation
or create another grant. Recovery is bounded by the Access pairing deadline.
Expired or lost local secrets require explicit Host/admin revocation and fresh
pairing. See [standalone recovery](../../access/README.md#pairing-and-recovery).

For live output from separate CLI invocations, send the prompt and then attach
`tail`. Separate Core processes share a device identity but not in-memory channel
keys; starting another command may replace the active channel and require watch
history recovery. Concurrent multi-process channel multiplexing is not provided.
