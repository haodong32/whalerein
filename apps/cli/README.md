# whalerein CLI

Internal design: [CLI architecture](../../docs/architecture/cli.md).

Thin CLI over `whalerein-core`. It parses arguments, renders stable output and
maps errors to WIRE.md exit codes; it does not parse HTTP DTOs, MQTT topics or
Noise messages.

## Commands

```bash
whalerein pair [--name <display-name>] <pairing-uri> [--yes] [--fingerprint <hex>]
whalerein pair-resume
whalerein pairing-history
whalerein hosts
whalerein sessions --host <host-id>
whalerein models --host <host-id>
whalerein model get --session <session-id>
whalerein model set --session <session-id> --provider <provider> --model <model> [--effort <effort>]
whalerein session-create --host <host-id> [--cwd <host-path> | --workspace <id>] [--preset <id>] [--session-id <preferred-id>]
whalerein session-fork --session <session-id>
whalerein commands-pending
whalerein command-recover --request <request-id>
whalerein tail --session <session-id> [--once] [--timeout <seconds>]
whalerein wait --session <session-id> [--timeout <seconds>] [--fail-on-timeout]
whalerein sync --session <session-id>
whalerein send --session <session-id> [--delivery enqueue-after-run|steer|start-if-idle] <text>
whalerein approvals --session <session-id>
whalerein approvals --request <approval-id>
whalerein approve --request <approval-id> --digest <hex> --version <n>
whalerein reject --request <approval-id> --digest <hex> --version <n>
whalerein interrupt --session <session-id>
whalerein questions --session <session-id> [--cursor <cursor>]
whalerein answer --session <session-id> --batch <batch-id> --epoch <epoch> --version <n> --digest <hex> --phase <phase> --input <file-or-dash> [--command-id <id>]
whalerein question-cancel --session <session-id> --command <command-id>
whalerein queue list --session <session-id>
whalerein queue edit --session <session-id> --item <item-id> <text>
whalerein queue remove --session <session-id> --item <item-id>
whalerein queue steer --session <session-id> --item <item-id>
whalerein profile list [--include-staging]
whalerein profile current
whalerein profile add NAME [--staging --enable-testing | --api-origin URL --issuer URL --client-id ID [--access-url URL]]
whalerein profile use NAME
whalerein profile remove NAME
whalerein revoke
whalerein forget [--local-only] --yes
whalerein doctor
whalerein doctor --export <file>
```

Global flags: `--json`, `--yes`, `--data-dir <path>`. Destructive commands
require `--yes` when standard input is not a terminal.

To select a Host for a session operation, put `--host HOST_ID` before the
command, for example `whalerein --host HOST_ID send --session SESSION_ID text`.
An unqualified session ID is resolved against the current authorized Hosts. If
more than one Host has the same ID, or a Host cannot be checked, specify the
Host explicitly. The single-Host form needs no extra flag. Pair additional Hosts
through the same Access endpoint with their own invitations; each confirmed
Host fingerprint is retained separately.

`send` defaults to enqueue-after-run and waits for the host's terminal command
result. A delivery ID confirms acceptance, not execution or a committed reply.
The adapter must advertise the selected mode. The real DSH adapter advertises
queue and steer whenever its native prompt service is callable; the exact
runtime allowlist is no longer the advertisement gate. Strong queue/steer
evidence exists only for the standard DSH 0.2.2-rc.1 CLI runtime with Cordis
4.1.0 on Linux or macOS arm64 with Node 25.9.0. Other platforms and versions
are open but unverified; the CLI reports availability from the Host
advertisement and claims no fallback delivery mode for them. An unsupported
mode is an explicit error that suggests checking Host runtime compatibility.
Explicit `start-if-idle` checks a sampled state before submission and cannot
exclude a concurrent local start. `tail` prints session output text for `Output`
events; `session-create` checks the Host's advertised creation capability
first. The real DSH adapter advertises creation when the native controller
provides it. When supported, omitted location uses the Host default; cwd is
interpreted on the Host and may be created by DSH. A preferred session ID may
reuse an eligible blank session; use the returned ID as the actual identity.
Creation never sends a first message. The outcome is one of `READY`,
`READY_WITH_WARNING`, `FAILED` or `UNKNOWN`. `FAILED` is a proven preflight
failure (missing workspace or preset, an unsupported combination, or a fork
source without a completed turn); it reports a stable reason and whether a
retry is safe. `READY_WITH_WARNING` retains the session identity when workspace
attachment failed after creation; do not retry or delete it. `UNKNOWN` never
claims identity; it reports an operation id and a reconcile mode, and the CLI
never retries automatically.

`whalerein wait --session SESSION_ID --timeout <seconds> [--fail-on-timeout]`
reports a typed idle/running/input-required/timeout observation without activating
a cold Agent. The default timeout exit is 0; `--fail-on-timeout` exits 7 only for
a timeout observation, while transport failures keep their own error codes.

`pairing-history` shows redacted Host and claim identities for pending pairings
removed by global revocation, and for pairings known to have committed whose original grants are no longer available or whose Host
confirmation cannot be recovered. If Host confirmation is unknown after Access
cleanup, the original pending FINISH is retained; an unconfirmed Host outcome
cannot be completed automatically. A previously recorded Host COMMIT confirmation
can be recovered while the original grant remains active. The record does not retain the pairing token or device credential. It is kept until the local
device identity is explicitly forgotten. A global revocation removes any pending
pairing credential and retains only its redacted identity with status
`unconfirmed_revoked`; that record requires manual verification and cannot
resubmit the original FINISH.

Use `commands-pending` to find uncertain command identities and
`command-recover --request <request-id>` to read a prior result. Recovery does
not repeat the original action. After a device grant is revoked, `commands-pending`
also lists opaque request identities retained for manual verification. These
identities do not contain the original payload or credentials and cannot currently
be passed to `command-recover`, including after re-pairing. Revocation reports
`revoked_retention_unverified` when Core cannot confirm whether all unconfirmed
command or pairing identities were retained; an absent cache may be either unused or lost.

`send` waits for the host's terminal command result and prints the message id
when one is returned. `tail` prints session output text for `Output` events;
it never prints raw event JSON, tool arguments, keys or tokens. `doctor` prints
the paired Access address and how many local commands are unfinished or have an
unknown result. It does not open a channel, so it does not report connection,
reconnect, protocol, or sync state.

Question operations require the corresponding Source capability. Submit a
complete JSON object `{"answers":[{"id":"...","selected":["..."],"custom":null}]}`
from a file or `--input -` for standard input, using the current batch guard.
An open-question receipt reports settlement; a continued-question receipt
reports only that the reply was queued. The CLI does not answer automatically.

For an open answer, Ctrl-C requests cancellation over the authenticated channel.
The command identity is included when the outcome remains unknown. To cancel
from another invocation, supply a unique `--command-id` when answering and use
`question-cancel` with that ID, or inspect `commands-pending`. Cancellation is
scoped to the device and session. A `false` cancellation receipt means no
matching operation was active when checked; it does not prove whether the answer
completed or has yet to arrive. Use `commands-pending` to find its request ID and
`command-recover` before retrying.

Model, pending-queue (`queue list/edit/remove/steer`), `session-create`,
`session-fork`, `wait` and question commands are production entry points. Each
checks the Host's advertised capability and is usable when the plugin advertises
the command; otherwise it fails with an explicit unsupported-capability error.
Advertisement depends on native service support, not on the CLI version.

## Output

`--json` emits one stable object:

```json
{"schema":"whalerein/v1","command":"hosts","data":{}}
{"schema":"whalerein/v1","command":"hosts","error":{"code":"unauthorized","message":"..."}}
```

Exit codes: `0` success, `1` internal, `2` usage/input, `3` not found,
`4` unauthorized/revoked, `5` host offline/unavailable, `6` host rejected,
`7` timeout/unknown, `8` conflict, `9` unsupported/incompatible.

## Profiles

Profiles isolate local device identity, Host pins, cache, pending operations and
service authority. `profile current` shows the active selection and its
environment; a fresh directory reports official production as the ordinary
account default while invitation pairing remains available and no account
service is contacted.

`profile add` never switches the selection. Official staging is hidden from
ordinary setup: adding it requires both `--staging` and `--enable-testing`, and
it stays visibly labeled `Staging (testing only)` whenever active. This is a
local testing choice, not server admission control. A custom profile requires an
explicit account API origin, OIDC issuer and public native client ID; the client
does not infer endpoints from hostnames.

`profile use` fails with `conflict` while another client process is active, so
old traffic stops before the new selection is published. Removing a managed
profile deletes only its local state and never claims remote revocation. Older
binaries ignore the profile registry; a managed selection therefore reports a
rollback warning and those clients must be stopped before downgrading.

| Command | Result shape |
|---|---|
| `profile list` | `{"profiles":[...]}` |
| `profile current` | selection with `active`, `default_service`, `standalone_available` |
| `profile add` | `{"profile":...,"current":false}` |
| `profile use` | activation outcome with optional `rollback_warning` |
| `profile remove` | removal outcome and `remote_revocation:"not_attempted"` |

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

### Model operations

`whalerein models --host HOST_ID` reads the current source catalog.
`whalerein model get --session SESSION_ID` reads the recorded next and
last-used choices along with the source fallback. `pending_known` states whether
the native projection establishes if a distinct pending record exists. `whalerein model set --session SESSION_ID --provider PROVIDER --model MODEL
[--effort EFFORT]` requests a
selection and reports the source-default attempt and unconfirmed persistence.
The real DSH adapter advertises catalog, read and selection when the bound
native controller exposes the corresponding services. Unsupported operations
return an explicit error. The fake adapter can opt in for tests.
`whalerein session-fork --session SESSION_ID` forks the latest complete turn.
The child shares the source working directory; it does not create a worktree or
start the child. Availability depends on the Host's advertised fork capability,
which the real adapter reports when the native controller provides it. A fork
outcome follows the same four dispositions: a missing source or a source with
no completed turn fails at preflight, a verified child attach failure keeps the
parent and child identities with a warning, and an unresolved native phase is
`UNKNOWN` with a parent-id or manual reconcile hint.
