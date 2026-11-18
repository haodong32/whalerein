# whalerein-core

Internal design: [Core architecture](../docs/architecture/core.md).

Self-contained Rust client core for WhaleRein. It owns HTTPS, MQTT 5, SQLite,
Noise XX, command projection, event sync and diagnostics. `SecretStore` is the
only injected capability.

The supported public modules are `api` and `conversation`. Secret stores,
connection state and diagnostic metric types are exported through `api`;
implementation modules are private.

## Modules

| Module | Responsibility |
|---|---|
| `api` | Stable snapshot/command API used by CLI and future bindings |
| `runtime` | Lifecycle, in-memory connection state, channel establishment, reconnect backoff |
| `pairing` | Pairing URI parsing and the Noise XX pairing state machine |
| `security` | SecretStore, device identity, Noise XX, channel keys, frames and AAD |
| `commands` | Request submission, polling, command projection |
| `sync` | Event cursor, gaps, snapshots, the only session/event cache writer |
| `cache` | SQLite with encrypted payload columns |
| `access` | Internal Access HTTPS and MQTT 5 transports |
| `diagnostics` | Redacted doctor report, detail mode, aggregate metrics |

## Security invariants

- Private keys and device tokens live only in `SecretStore`.
- SQLite never stores connection state, pairing tokens, private keys or unsent
  commands. Session summaries and event payloads are encrypted with a random
  cache key held by `SecretStore`.
- A reconnect always creates a new random `channel_id`; old ciphertext is
  rejected because it does not match the active channel.
- Pairing never trusts Access for the host fingerprint. The fingerprint is
  SHA-256 of the Noise remote static key, and a cancellation or mismatch never
  sends msg3.

## Local data

- Open Core with the platform-selected version 1 TOML and a SecretStore.
- The required `data_dir` resolves relative to the selected configuration.
  Its `instance.json` binds the stable identity and service authority.
- `state/regional.json` holds the verified regional endpoint and Host pins.
- `state/cache.sqlite3` holds encrypted Host, Session and conversation data.
- Recover retained operations with their original IDs under the same instance.

## Tests

```bash
cargo test -p whalerein-core
cargo clippy -p whalerein-core --all-targets -- -D warnings
cargo llvm-cov -p whalerein-core --summary-only
```

The `test-support` feature exposes a fake Access/host fixture for integration
tests. It is not enabled by default.

The separate `protocol-testing` feature exports low-level crypto and MQTT
drivers through `testing`. It is used by `tools/noise-peer` and the
`mqtt_transport_driver` example, not by client applications. The example requires
this feature explicitly.
