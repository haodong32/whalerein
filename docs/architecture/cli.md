# WhaleRein CLI Architecture

The CLI selects a configuration path and SecretStore, then opens Core and
formats typed outcomes. The system keyring is the default. On Unix
`--secrets-dir` selects owner-only local files for that invocation. Core owns
TOML validation, endpoint selection, authority binding, credential storage and
the client instance lifecycle.

| File | Responsibility |
| --- | --- |
| `cli.rs` | Grouped command grammar and argument validation |
| `run.rs` | Dispatch, history/send recovery, attachment download, interactive prompts and cancellation |
| `output.rs` | Human and machine output, reasoning policy, download progress and shell-safe guidance |
| `main.rs` | Streams and process exit codes |
| `diagnostics.rs` | CLI diagnostic receiver, level filtering, and stderr formatting |

Core resolves one Host and Session before sending a request. The resolved key
travels with the request and its observation. A durable send record binds the
operation ID to the original target and intent before transmission. Recovery
uses that record's pinned Host identity. The CLI renders typed statuses and
includes the operation ID when an outcome is uncertain. Detached send reports
the receipt available at submission. Foreground send follows the durable
receipt, resolves the confirmed canonical message through Core's public history
read, and renders its typed blocks in the original command under one remaining
deadline.

`host status` is the explicit connection probe: it opens the Host
connection, waits a bounded time for a usable state, and reports the
authoritative `state`, terminal `code` and `since_unix_ms`. It needs no
session and never reads a cached value; a Host that cannot be reached keeps
its current state instead of failing the command.

`session show` stays the finite metadata summary. `session history` is the
independent committed-history entry point. It performs one bounded Core page
read with no watch lease, MQTT subscription or cache write, starts with the
latest page and walks older pages only through the opaque Core continuation.
`--cache` selects the eligible encrypted local view explicitly. `--message`
resolves one scoped committed-message reference through the same history module.
Pages identify their source, keep messages chronological within the page and
expose a usable continuation. A stale, expired or wrong-scope token is an
actionable `snapshot_required` failure that restarts traversal; it is never
silently retried. Raw cursors and continuations are user-facing output only and
never enter diagnostics.

Reasoning is a typed block, not answer text. A global `--show-reasoning` option
is off by default and applies to live observation, online and cached history,
and a confirmed send reply. Human output labels the block; JSON preserves the
`reasoning` type. Hidden reasoning is absent from default JSON and does not
satisfy `session watch --once`; Core reports answer and reasoning preview
prefixes separately, so a hidden prefix never offsets visible text. This is a
presentation policy only and never changes authorization.

Human tool-result text is bounded (8 KiB per result) with an explicit hidden-byte
marker; a global `--full` disables the cap for human output. JSON always carries
complete values. An attachment without a Source name is shown and saved under a
media-derived default, unavailable rows explain why no save action exists, and
duplicate suggested names receive distinct `--name` arguments in printed save
commands.

Send renders one typed outcome. `Committed` reads the canonical message through
history and shows the reply. A later content-read failure preserves the
confirmation and prints the exact `session history --message` command;
`snapshot_required` additionally explains that the reference is stale and must
be refreshed. Pending input shows only the evidenced approval/question
inspection commands scoped to the original Host and session. Pending and
uncertain outcomes show neutral next steps. Guidance uses shell-safe quoting,
and content unavailability never resubmits a send. JSON keeps the typed reply,
input kinds, message and read error.

The Host adapter translates native attempt evidence into a scoped result.
Watch output contains normalized typed conversation events. Finite machine
commands write exactly one `whalerein/client/v1` JSON envelope on stdout.
Progress and diagnostics stay on stderr; in `--json` mode each diagnostic is one
structured JSON event. The CLI opens Core with an instance-local diagnostic
receiver whose bounded queue makes delivery best effort. `--verbose` enables
informational and debug events alongside warnings and errors, including the
Core history projection/page/continuation and send confirmation/input/read
stages. `session download` reads one authorized attachment reference from
history, requires the owning session and an explicit destination directory, and
delegates verification and the atomic local save to Core. Progress is throttled
on stderr (structured events in JSON mode); exactly one result envelope is
written after a verified commit. Failures state whether existing data was
preserved and how to start a fresh transfer. The CLI never reads bytes for an
unsupported, oversized or unauthorized read, and never displays success before
the save commits.

See the [CLI usage guide](../../apps/cli/README.md) and the [Core architecture](core.md).
