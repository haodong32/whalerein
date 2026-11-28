# WhaleRein CLI

Choose one TOML file for each client instance. Save it at
`~/.whalerein/config.toml` or pass `--config <file>` on each invocation.

```toml
version = 1
service = "whalerein_cloud"
data_dir = "./cloud"
```

A relative `data_dir` resolves beside the selected file. Use a fresh directory
for first sign-in and Device pairing. Official production is the default.
Developers can select the official `local` or `staging` catalog entry with
`cloud_environment`. Local development requires the trusted development CA.

### Optional local credential storage

On macOS and other Unix platforms, pass `--secrets-dir PATH` to store CLI
credentials in a local directory and avoid system keyring prompts. The
directory is created on the first secret write with mode `0700`. Credential
files use mode `0600` and are unencrypted on disk. Choose a private location
under your home directory outside the instance's `data_dir`.

Use a separate TOML file with a fresh `data_dir`, then sign in and connect
again. System keyring entries are not migrated. Pass the same absolute
secrets path on each invocation for that instance.

```sh
whalerein --config ~/.whalerein/staging-local.toml --secrets-dir ~/.whalerein/staging-secrets login
whalerein --config ~/.whalerein/staging-local.toml --secrets-dir ~/.whalerein/staging-secrets host connect <host-id>
```

### Cloud commands

```sh
whalerein status
whalerein login
whalerein host list
whalerein host connect <host-id>
whalerein session list            # archived sessions are hidden
whalerein session list --all      # include archived sessions
whalerein session create
whalerein session history <session-id>
whalerein session send <session-id> "Hello"
```

Open the verification URL shown by `login` and enter the code in the browser.
A Cloud Host can appear in the directory before this Device has access. Connect
an enrolled Host to activate its regional grant and select it as the default.
Use `host default <host-id>` to select a different authorized Host explicitly.

For self-hosted Standalone, use `service = "standalone"` and a separate data
directory. If the server uses a private CA, set
`standalone_ca_certificate = "./ca.pem"`. Read the invitation from a private
file or an interactive pipe.

```sh
whalerein host connect --invite-stdin < invitation.txt
whalerein session list
```

Compare the fingerprint with a value obtained from the Host. For
non-interactive pairing, pass the verified `--fingerprint <hex>` or `--yes`.
Use `--help` on each leaf command to see its arguments. Where supported,
`--host <id>` follows the leaf command and overrides its Host selection.
Session IDs belong to the selected Host.

## Commands

- `host list` reports the Access/Broker-derived directory presence. `host
  status` reports this device's own MQTT connection and access state: a
  device-only network loss can make the local status reconnect while the Host
  remains online in the directory.
- `status` inspects selected paths, service, sign-in and Device access.
- `logout` signs this instance out immediately and reports a pending remote
  sign-out when Cloud or Access cannot yet confirm it. Run `status` or
  `logout` again to retry pending remote sign-out.
- `account sessions` lists Cloud App sessions. `account revoke-session ID`
  signs out an individual App session, and `account revoke-all-sessions`
  signs out all App sessions in the selected Cloud environment.
- `device revoke` applies Device revocation. `--host <id>` is Cloud-only; a
  Standalone profile rejects it instead of ignoring the named Host.
  `device forget` clears local pairing identity. Review prompts or supply
  `--yes` for non-interactive destructive commands.
- `session show <id>` remains a finite metadata summary. `session fork <id>`,
  `session watch <id>`, `session wait <id>` and `session interrupt <id>` operate
  on one Session.
- `session history <id>` reads one bounded committed-history page, newest first,
  without creating a watch lease or changing the cache. Messages are
  chronological inside the page. `--continuation <token>` walks toward older
  history; an expired, stale or wrong-scope token fails with `snapshot_required`
  and asks you to restart without `--continuation`. The human page prints the
  exact command for that token. `--cache` reads the eligible encrypted local
  cache explicitly (an empty cache says so); a cache-sourced continuation must
  also be read with `--cache` (the printed command includes it),
  `--message <reference>` resolves
  one exact committed message, and `--max-messages` / `--max-bytes` bound the
  page.
- `session download <reference> --to <dir> --session <id>` saves one verified
  attachment. Use the `save_command` shown by `session history` (or its human
  `Save:` line); the reference is opaque and belongs to that Host and session.
  `--name <name>` overrides the Source-suggested file name, `--force` explicitly
  replaces an existing final file, and `--timeout <seconds>` bounds the whole
  transfer. Without `--force` a collision preserves the existing file. A
  verified file appears only after the local save commits; a failure removes
  temporary data and leaves existing files unchanged. Success is reported after
  that commit, never before. Recovery is always a fresh transfer, not a resume.
  An attachment without a Source name is shown and saved under a media-derived
  name (`attachment.png`, `attachment.bin`, ...). Rows the Source cannot expose
  explain why no save action is available. When two rows share a suggested
  name, later `Save:` commands add a distinct `--name`. Large images show an
  estimated decoding memory note before you download them.
- `session model`, `session queue`, `session approval` and `session question`
  expose advanced operations and their verification guards.
- `session approval approve` submits the Allow resolution with the requested
  `--digest` and `--version`. It does not pre-read the approval or withhold
  Allow when review facts are incomplete; the pinned DSH exposes only the tool
  name, call id and reason, so remote approval must stay available. The Host
  remains authoritative for the digest, version and approval state guards.
  `session approval list` and `list --request <id>` still render `review:` plus
  any supplied action, target and impact, and state an explicit limitation when
  the review is incomplete, unavailable, or labeled complete with an empty
  essential fact. No value is invented for unavailable facts; inspect with
  `session approval list --request <id>` before deciding, and reject with
  `session approval reject` when the request should not proceed.
- `session send <id> <text>` follows the submitted message's reply and renders
  the confirmed committed message in the original command. `--detach` returns
  the submission receipt. `--timeout <seconds>` limits observation. Pending
  approval or question input shows the scoped inspection command. If the later
  content read fails, the confirmation stays and the exact
  `session history --message` command is shown; never resend because content was
  unavailable. Use `--` before text that could be parsed as a flag.
- `--show-reasoning` renders typed reasoning blocks in live, `session history`
  and confirmed send output. It is off by default and never changes
  authorization. Hidden reasoning does not satisfy `session watch --once` and
  does not appear in default JSON.
- Human tool results are capped at 8 KiB with a hidden-byte note; `--full`
  shows the complete text. JSON always carries complete values.
- `operation list` displays retained operations. `operation recover <id>`
  checks the original operation using its recorded target. Recover uncertain
  sends before submitting another message.
- `doctor` shows a bounded summary of the current redacted capture; expanding it
  reads only local state and writes nothing. `doctor --export <file>` writes that
  same captured report. A new path is created with 0600 permissions in one action,
  in terminal or noninteractive use, without `--yes`. An existing regular file
  needs an interactive overwrite answer or `--yes` in noninteractive use; a
  refusal, symlink or directory leaves it untouched. The report is never uploaded.

## Machine output

`--json` on a finite command writes exactly one newline-terminated
`whalerein/client/v1` JSON envelope on stdout with a command and typed data or
an error. Errors contain a stable code and message. An uncertain outcome
includes the operation ID when available and uses exit status 7. Explicit
`session watch` is the only streaming stdout command and emits scoped JSON
events. Sign-in prompts, progress and diagnostics use stderr.

`session history` JSON data contains `target`, `source` (`online` or `cache`),
`projection`, `complete`, chronological `messages`, folded `activities`,
`attachments` with `reference` and `save_command`, and a usable `continuation`
or `null`. `--message` returns the same typed `message` plus its `reference`.
`session send` JSON keeps `reply.state`, the confirmed `reference` (and the
nested `reply.reference`), typed `input_kinds` when applicable, the typed
`message` when its read succeeds, and a stable `read_error` when it does not. Reasoning blocks appear as
`{"type":"reasoning"}` only with `--show-reasoning`.

`session download` JSON data contains `target`, the opaque `reference`, the
written `path`, `name`, `bytes`, `sha256`, `media_type` and `verified`. Progress
never enters stdout. In `--json` mode each progress update is one
`{"event":"download_progress","received":N,"total":N}` object on stderr; human
mode uses a throttled `Progress: received/total bytes (percent%)` line. The
envelope is written only after Core verified the bytes and committed the file.

## Detailed diagnostics

Pass `-v` or `--verbose` before or after a subcommand to include informational
and debug events. The default log level includes warnings and errors. When
combined with `--json`, diagnostic events on stderr use one JSON object per
line. Use the `event` and `trace_id` fields to locate request and connection
stages. `command_completed` includes the actual `exit_code` and the
`outcome_exit_code` selected by command execution before writing its result.
Interactive prompts can also appear on stderr. Diagnostic delivery is
best effort when stderr falls behind. The CLI limits how long it waits for
queued diagnostics after writing the command result.

```sh
whalerein --verbose --json host list > result.json 2> diagnostics.log
```

The same shape applies to history and send recovery. stdout stays one command
envelope (or watch events); stderr is one JSON event per line when `--json` is
also set. Correlate both streams with `trace_id`.

```sh
# One history page on stdout, redacted stages on stderr.
whalerein --verbose --json session history <session-id> > page.json 2> history.log

# One verified save on stdout, progress and download stages on stderr.
whalerein --verbose --json session download <reference> --host <host> --session <id> --to ./downloads > save.json 2> download.log
jq -c 'select(.event == "download_progress" or (.stage // "" | startswith("download_")))' download.log
jq '.data.path, .data.sha256, .data.verified' save.json

# Find the bounded history/send stages without touching stdout.
jq -c 'select(.stage | startswith("history_") or startswith("send_"))' history.log

# A later content-read failure remains a successful confirmation.
whalerein --verbose --json session send <session-id> "Hello" > send.json 2> send.log
jq -c 'select(.stage | startswith("send_"))' send.log
jq '.data.reply, .data.read_error' send.json
```

Stable diagnostic stages include `history_page`, `history_projection`,
`history_continuation`, `send_reply_confirmed`, `send_input`,
`send_reply_read`, `download_open`, `download_admission`, `download_part`,
`download_verify`, `download_commit` and `download_cancel`. A failed download
stage is a bounded warning diagnostic even without `--verbose`; verbose adds the
successful transfer stages. Events exclude message and reasoning bodies, tool
parameters, attachment names and contents, local paths, raw cursors and
credentials. Diagnostic delivery is bounded and best effort; the command result
is written before the CLI waits briefly for queued diagnostics.

## Replace an existing installation

On the old client, settle pending sends and connections while their original
authority and credentials are available. Stop old client processes. Choose a
new data directory, open the new TOML, then sign in and pair a fresh Device.
Keep the old directory and credentials for inspection. Revoking an old grant
is a separate authenticated action.
