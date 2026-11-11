# WhaleRein Host

This directory contains the host-side implementation: the shared protocol runtime
and the connector that embeds it in DSH.

| Directory | Responsibility | npm package |
| --- | --- | --- |
| [runtime](runtime/README.md) | Source-independent networking, security, command execution tracking, watched publication and durable Host state. | `whalerein-host` |
| [dsh](dsh/README.md) | Cordis lifecycle, DSH source adapter, credentials, settings and local launchers. | `whalerein-dsh` |

The DSH connector depends on the runtime through the root pnpm workspace. The
runtime depends only on the normalized Source contract and never imports DSH.
Directory names describe subsystem roles; npm package names remain unchanged.

Run `pnpm install --frozen-lockfile` from the repository root. Use `./repo` for
project discovery, testing and coverage; the project paths are `host/runtime`
and `host/dsh`. See the [architecture overview](../docs/architecture.md) for the
relationship with Core and Access.
