# WhaleRein CLI Architecture

The CLI selects a configuration path, opens Core with the platform SecretStore,
and formats typed outcomes. Core owns TOML validation, endpoint selection,
authority binding, credential storage and the client instance lifecycle.

| File | Responsibility |
| --- | --- |
| `cli.rs` | Grouped command grammar and argument validation |
| `run.rs` | Dispatch, interactive prompts and cancellation |
| `output.rs` | Human and machine output |
| `main.rs` | Streams, tracing and process exit codes |

Core resolves one Host and Session before sending a request. The resolved key
travels with the request and its observation. A durable send record binds the
operation ID to the original target and intent before transmission. Recovery
uses that record's pinned Host identity. The CLI renders typed statuses and
includes the operation ID when an outcome is uncertain.

The Host adapter translates native attempt evidence into a scoped result.
Watch output contains normalized conversation events. Finite machine commands
write one `whalerein/client/v1` JSON envelope. Log diagnostics use stderr.

See the [CLI usage guide](../../apps/cli/README.md) and the [Core architecture](core.md).
