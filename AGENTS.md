# WhaleRein Guidelines

- Use English for code, comments, and developer docs; `WhaleRein` for display names, `whalerein` for packages, paths, and keys.
- Keep changes focused and architecture simple; add dependencies or abstractions only for clear requirements.
- `.local-docs/plans/` and `.local-docs/acceptance/` hold Git-ignored working material. Keep project facts and durable decisions in tracked `docs/` or clear code comments.
- Use `./repo` for monorepo tasks (`tools/repo/`, Python + uv). For task selection or runner changes, use [whalerein-repo](.agents/skills/whalerein-repo/SKILL.md).
- DSH owns sessions; access its APIs only through the plugin adapter.
- Keep Core self-contained in Rust; `SecretStore` is its only injection boundary.
- Use PostgreSQL for server state and internal tasks; review additional infrastructure before introducing it.
- Never commit secrets or expose message contents, tool parameters, private keys, or pairing tokens in logs or tests.
- Run relevant build/lint/tests; require ≥90% unit-test line coverage per Rust crate, Go package, and Node.js package.
- `./repo test` runs unit tests only. Full integration is slow and opt-in via `./repo test --suite integration` or `./repo test --suite all`; use it for cross-component, protocol, recovery, or release changes.
- Claim only verified API, integration, and deployment behavior.
- Before committing, read [commit rules](docs/commits.md).

## Architecture

For testing, troubleshooting, security, and acceptance, use the [docs index](docs/README.md).

Before changing behavior or boundaries, read the relevant guide:
[overview](docs/architecture.md), [Core](docs/architecture/core.md),
[CLI](docs/architecture/cli.md), [Access](docs/architecture/access.md),
[DSH plugin](docs/architecture/dsh-plugin.md).
