# WhaleRein

WhaleRein remotely operates **DeepSeek Harness (DSH)** sessions. It is not a
remote desktop, shell, or file manager: it lets an operator view sessions and
live output, send messages into an existing session, approve or reject tool
calls, interrupt a task, and recover after disconnects.

First version scope: one host, one CLI, one Access, one PostgreSQL, one MQTT
broker. No control plane, user table, or multi-tenant model.

Documentation: [index](docs/README.md), [testing](docs/testing.md),
[troubleshooting](docs/troubleshooting.md), [security](docs/security.md).

## Architecture

See [the architecture overview](docs/architecture.md) for component boundaries,
state ownership, request flows, security, and recovery behavior.
Detailed guides cover [Core](docs/architecture/core.md),
[CLI](docs/architecture/cli.md), [Access](docs/architecture/access.md), and
[the DSH plugin](docs/architecture/dsh-plugin.md).

```text
whalerein CLI (apps/cli)
  │  HTTPS + native MQTT 5
  ▼
WhaleRein Core (Rust library)
  │
WhaleRein Access (Go monolith) ── PostgreSQL (five tables)
  │  MQTT 5
  ▼
Mosquitto 2.1.4 ── WhaleRein for DSH plugin (Node.js) ── DSH / FakeDSH
```

- `proto/v1/whalerein.proto` is the only wire contract. HTTPS uses proto3 JSON;
  MQTT uses binary protobuf. There is no second OpenAPI or JSON Schema.
- After pairing, handshakes, queries, controls and sync all use
  `POST /v1/hosts/{host_id}/requests` and results are read only by
  `request_id`. There is no HTTP route for sessions, messages or approvals.
- Online status is the broker's connection fact. Access owns transport grants
  and request copies; the shared Host owns execution records; source cursors stay opaque and DSH
  owns session content; Core runtime owns in-memory client connection state.
- Noise XX with X25519/ChaCha20-Poly1305/SHA-256 protects everything after
  pairing. Access and the broker only see routing and transport metadata.

## Repository layout

```text
repo                     # engineering tasks (bash -> uv -> tools/repo)
apps/cli/                # whalerein product CLI
core/                    # Rust client core
access/                  # Go Access service and PostgreSQL migrations
host/                    # host-side implementation
  runtime/               # shared Node.js host runtime
  dsh/                   # WhaleRein for DSH connector
proto/v1/                # wire schema, Go/Rust bindings and fixtures
deploy/                  # Docker Compose stack for Access, Mosquitto, and Postgres
tests/integration/       # isolated Docker/pytest suite with FakeDSH
tests/integration/drivers/ # Native protocol and transport integration drivers
tests/fixtures/          # fixed compatibility fixtures
tools/repo/              # Python monorepo manager (uv)
.local-docs/plans/        # local proposals (git-ignored)
.local-docs/acceptance/   # local validation records (git-ignored)
```

The Compose stack lives only in `deploy/`. Core, Access and the plugin do not
read it.

The two Node.js packages share a pnpm workspace and root lockfile. Run
`pnpm install --frozen-lockfile` at the repository root; DSH resolves Host through
`workspace:*`. The private root `package.json` coordinates installation and pins
pnpm; it is not a product package or a test suite. Use `./repo` for build and test.

## Server stack

[Docker Compose](deploy/README.md) runs Access, PostgreSQL and Mosquitto. The DSH
plugin runs separately on its host.

```bash
docker compose -f deploy/compose/stack.yaml up -d --build
docker compose -f deploy/compose/stack.yaml ps
```

See the deployment guide for ports, development configuration, readiness probes
and shutdown. Repository engineering commands do not start the server stack.

## Plugin debug

```bash
cd host/dsh

# FakeDSH + watch
pnpm dev

# Real DSH + plugin, foreground until Ctrl-C
pnpm start
```

`pnpm dev` starts the standalone FakeDSH host and restarts it on source changes.
`pnpm start` creates/uses the `whalerein-debug` DSH profile, installs the plugin
with pnpm, and runs real DSH on port 3099. Neither command starts or checks
Access; the plugin reconnects when the control plane appears. The normal 3080
DSH instance is untouched.

The same `whalerein-dsh` plugin also adds **Settings → WhaleRein** to the DSH
Web UI. It edits the host plugin's `configForms` section; saved values override
the profile defaults, and clearing a field inherits the environment/default
again. The form takes a server host, an administrator token, and the host
identity. **Generate pairing link** mints one `whalerein://pair` URI on demand.
There is no second plugin to install.

## Prerequisites

- CI toolchains: Rust 1.100.0 (via rustup), Go 1.27.4, Node.js 26.13.1 LTS,
  pnpm 12.21.0, Python 3.15.2, and uv 0.13.9.
- Build tools: Git, Git LFS, Bash, a C/C++ compiler, protoc 36.2, and
  protoc-gen-go 1.36.12.
- Local server stack: Docker with Compose. Integration tests: Docker with
  buildx/BuildKit. PostgreSQL and Mosquitto run in containers.
- Optional: cargo-llvm-cov and llvm-tools-preview for Rust coverage;
  DSH for real-host plugin development.

Run `./repo doctor` to check the basic toolchains. Use `./repo doctor integration`
or `./repo doctor proto` for target-specific prerequisites.

## Quickstart (self-hosted)

```bash
# 1. Toolchain report (never modifies the system)
./repo doctor

# 2. Local stack: PostgreSQL 18 on 55432, Mosquitto on 1883, and Access on 8080.
#    Docker checks image inputs and reuses cached layers.
docker compose -f deploy/compose/stack.yaml up -d --build
curl -sS http://127.0.0.1:8080/readyz

# 3. Build the native CLI and install plugin workspace dependencies.
./repo build cli
pnpm install --frozen-lockfile

# 4. In another terminal, start the plugin against real DSH.
(cd host/dsh && pnpm start)

# 5. Create the CLI's selected Standalone instance configuration.
cat > client.toml <<'TOML'
version = 1
service = "standalone"
data_dir = "./client-instance"
TOML

# 6. Read a Host invitation from a private file, then send to a Session.
./target/debug/whalerein --config client.toml host connect --invite-stdin < invitation.txt
./target/debug/whalerein --config client.toml session list
./target/debug/whalerein --config client.toml session send <session-id> <text>
./target/debug/whalerein --config client.toml doctor
```

See the [CLI guide](apps/cli/README.md) for configuration, Cloud sign-in,
command help, machine output, and recovery.

See [deploy/README.md](deploy/README.md) for the Compose layout and smoke tests.

## Build and test

```bash
./repo projects                     # Project IDs and available operations
./repo fmt core                     # Write formatting changes
./repo check core                   # Format checks and static analysis
./repo build                        # Applicable native builds
./repo test                         # Unit tests only
./repo test repo --coverage
./repo test integration             # Explicit, slower container suite
```

Use the [repo command reference](tools/repo/README.md) for project selection,
protocol generation, coverage and reports. The [testing policy](docs/testing.md)
defines validation required by each change; the
[integration guide](tests/integration/README.md) describes fixture isolation.
For Agent task selection, use
[whalerein-repo](.agents/skills/whalerein-repo/SKILL.md).

## Contributing

Follow the [pull request rules](docs/pull-requests.md) and
[commit rules](docs/commits.md).
Run `git config core.hooksPath tools/hooks` once to enable the `commit-msg` check.

## Security notes

- Message bodies, tool parameters, private keys and pairing tokens are never
  logged, never placed in fixtures and never committed. The only printed token
  is the one-time pairing token the user supplies.
- TLS is mandatory for public HTTPS and MQTT. The development plaintext MQTT
  listener is loopback-only; disable it (`bind = 0`) in production.
- The Mosquitto plugin is built into the pinned broker image. Access
  uses a bootstrapped API key for REST disconnect; the key file is mode 0600
  and outside Git.
- Access and the broker plugin share a fixed internal callback token that must differ from
  the admin token. All deployment secrets live in mode-0600 environment files.
- Development certificates are generated into `.data/certs` (gitignored) and
  must never be used in production.
- See [AGENTS.md](AGENTS.md) for repository-wide rules.

## Release validation

Real DSH, target-platform SecretStore, production TLS, and deployment behavior
require validation in their target environments. See [testing](docs/testing.md)
for the validation layers and commands.

Shared Host modules live in [`host/runtime`](host/runtime/README.md). The current DSH connector implements the typed [Harness conversation contract](docs/architecture/harness-contract.md); no Pi connector is included.

## License

WhaleRein is licensed under the GNU Affero General Public License, version 3
only (`AGPL-3.0-only`). See [LICENSE](LICENSE) for the full terms.

Commercial use is permitted. Distribution of covered software requires compliance
with the license's corresponding-source obligations. If you modify WhaleRein and
allow users to interact with that version remotely over a network, you must
prominently offer those users access to its Corresponding Source as required by
Section 13.

Third-party components retain their respective licenses and notices.
