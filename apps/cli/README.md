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

```sh
whalerein status
whalerein login
whalerein host list
whalerein host connect <host-id>
whalerein session list
whalerein session create
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

- `status` inspects selected paths, service, sign-in and Device access.
- `logout` clears Cloud sign-in. `device revoke` applies Device revocation.
  `device forget` clears local pairing identity. Review prompts or supply
  `--yes` for non-interactive destructive commands.
- `session show <id>`, `session fork <id>`, `session watch <id>`,
  `session wait <id>` and `session interrupt <id>` operate on one Session.
- `session model`, `session queue`, `session approval` and `session question`
  expose advanced operations and their verification guards.
- `session send <id> <text>` follows the submitted message's reply.
  `--detach` returns the submission receipt. `--timeout <seconds>` limits
  observation. Use `--` before text that could be parsed as a flag.
- `operation list` displays retained operations. `operation recover <id>`
  checks the original operation using its recorded target. Recover uncertain
  sends before submitting another message.
- `doctor` shows local diagnostics. Use `doctor --export <file>` with explicit
  confirmation for a redacted report.

## Machine output

`--json` on a finite command writes one newline-terminated
`whalerein/client/v1` JSON envelope with a command and typed data or an error.
Errors contain a stable code and message. An uncertain outcome includes the
operation ID when available and uses exit status 7. Explicit `session watch`
emits scoped JSON events. Sign-in prompts and diagnostics use stderr.

## Replace an existing installation

On the old client, settle pending sends and connections while their original
authority and credentials are available. Stop old client processes. Choose a
new data directory, open the new TOML, then sign in and pair a fresh Device.
Keep the old directory and credentials for inspection. Revoking an old grant
is a separate authenticated action.
