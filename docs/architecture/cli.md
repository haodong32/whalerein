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

Core's typed ViewUpdates contain Messages, provisional text/reset, Attempt
settlement and runtime/approval state. Core decides completion and prefix relation;
the terminal renderer prints a suffix or a corrected complete message. It retains
only line/active-Attempt formatting state. JSON exposes normalized block types and
correlation metadata, never raw native event JSON or tool argument streams.

`tail --once` waits for visible text or a committed Message. Reset/state/start
metadata does not satisfy it, and a marker-only deadline returns timeout. Commands
and approvals retain stable IDs and digest/version guards. Pairing and diagnostic
export prompts belong here; synchronization, reconnection and authorization
settlement stay in Core.

JSON uses `whalerein/v1`; current HTTP routes and MQTT topic names stay v1. This is
an unreleased in-place contract replacement with no old-event compatibility path.
