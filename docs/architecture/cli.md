# WhaleRein CLI Architecture

[Overview](../architecture.md) · [Core](core.md) · [Usage](../../apps/cli/README.md)

The CLI constructs Rust Core, dispatches commands and formats normalized outcomes.
It has no DSH/Pi payload paths, history cursor arithmetic or preview protocol reducer.

| File | Responsibility |
| --- | --- |
| `cli.rs` | Arguments and conflicts. |
| `run.rs` | Dispatch, prompts, secret backend and visible-output stopping. |
| `config.rs` | Configuration/import adapter over Core's typed API. |
| `output.rs` | Stable human/JSON formatting and terminal line boundaries. |
| `main.rs` | Tracing, streams and process exit. |

The CLI resolves one explicit `SessionKey` per session command and passes it to
every Core call, so the Host cannot change between a lookup and its action. An
unqualified `--session` is resolved against the current authorized Hosts; an
explicit `--host` uses `scoped_session_key`. Approval commands resolve their Host
separately because an approval identity is Host-scoped, not session-scoped.

Core's typed ViewUpdates contain Messages, provisional text/reset, Attempt
settlement and runtime/approval state. Core decides completion and prefix relation;
the terminal renderer prints a suffix or a corrected complete message. It retains
only line/active-Attempt formatting state. JSON exposes normalized block types and
correlation metadata, never raw native event JSON or tool argument streams.

`profile` commands manage local selections before a Core instance is opened. They
read or mutate only the local registry and never contact an account or Access
service. `profile add` never activates; `profile use` publishes the selection
only after the acting process holds the exclusive activation lease. Staging and
local identities are testing-only: the active identity stays labeled, and an
inactive one remains hidden from ordinary list output unless
`--include-staging` is passed. `profile remove` deletes only managed local state
and never claims a remote grant or account-session revocation. See
[usage](../../apps/cli/README.md#profiles).

`tail --once` waits for visible text or a committed Message. Reset/state/start
metadata does not satisfy it, and a marker-only deadline returns timeout. Commands
and approvals retain stable IDs and digest/version guards. Pairing and diagnostic
export prompts belong here; synchronization, reconnection and authorization
settlement stay in Core.

JSON uses `whalerein/v1`; current HTTP routes and MQTT topic names stay v1. This is
an unreleased in-place contract replacement with no old-event compatibility path.

`config` commands are a thin adapter over Core's configuration API. Global
`--config` selects one file, with `WHALEREIN_CONFIG` and
`~/.whalerein/config.toml` as the fallbacks; `--environment NAME` overrides
selection for one invocation, accepts any valid environment name, is never
persisted, and fails instead of falling back. `config show` and `config check`
open read-only and explain path, revision, desired default, invocation override,
selection/definition provenance and field provenance; `check` exits non-zero
for invalid or unresolvable configuration and reports offline-only validation
with `live_readiness: not_checked`. `config set default`, `config set
environment`, `config set builtin` and `config remove environment` map to
Core's typed patches and revisioned writer. `config import` previews legacy
sources read-only and requires the reviewed preview id plus `--yes` to confirm;
it never moves state. The CLI has no TOML parser, catalog, path-precedence or
write policy of its own.

Profile commands are environment-scoped. `profile list`, `profile add` and
`profile use` operate in the resolved environment; `use` refuses an identity
from another environment. `--data-dir` remains the identity/state root and is
never derived from an environment or configuration path. Account transport is
still not implemented, so environment selection does not make login or
automatic pairing available.
