# WhaleRein CLI Architecture

[Overview](../architecture.md) · [Core](core.md) · [Usage](../../apps/cli/README.md)

The CLI constructs Rust Core, dispatches commands and formats normalized outcomes.
It has no DSH/Pi payload paths, history cursor arithmetic or preview protocol reducer.

| File | Responsibility |
| --- | --- |
| `cli.rs` | Arguments and conflicts. |
| `run.rs` | Dispatch, prompts, secret backend and visible-output stopping. |
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
only after the acting process holds the exclusive activation lease. Staging is
available only with an explicit testing flag, stays hidden from ordinary list
output unless active or requested, and is labeled as staging in every human and
JSON result. `profile remove` deletes only managed local state and never claims a
remote grant or account-session revocation. See [usage](../../apps/cli/README.md#profiles).

`tail --once` waits for visible text or a committed Message. Reset/state/start
metadata does not satisfy it, and a marker-only deadline returns timeout. Commands
and approvals retain stable IDs and digest/version guards. Pairing and diagnostic
export prompts belong here; synchronization, reconnection and authorization
settlement stay in Core.

JSON uses `whalerein/v1`; current HTTP routes and MQTT topic names stay v1. This is
an unreleased in-place contract replacement with no old-event compatibility path.
