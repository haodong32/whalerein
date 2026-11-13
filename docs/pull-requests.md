# Pull requests

These rules also apply to merge requests (MRs).

## Scope

- Target `main` with a short-lived branch. One PR delivers one coherent change;
  it can span projects when they implement the same behavior.
- Include affected consumers, tests, generated bindings and lockfiles together.
  Separate unrelated cleanup and dependency updates.
- Keep intermediate plans and acceptance histories Git-ignored. Review must not
  depend on personal OpenSpec files or another SDD tool.

## Title and description

Write in English. The title follows [commit rules](commits.md).
Keep the description proportional to the change and explain:

- **Behavior:** the problem and resulting behavior; link the related issue.
- **Validation:** actual commands or CI runs and outcomes. Mark checks not run
  with the reason; identify affected consumers when relevant.
- **Compatibility and rollout:** API, protocol, configuration or state changes,
  consumer updates, migration and rollback limits, or `None`.

Update the description when scope changes. Keep secrets, pairing links and
private message/tool contents out of the PR and attachments.

## Validation and merge

Use `./repo` and follow [testing policy](testing.md). Report only verified
results for the relevant revision; partial, skipped or cancelled checks are not
full passes. Link CI reports and summarize local results without committing
acceptance histories. Do not rely on workflows that are not implemented.

Use a draft while the change is incomplete. Before merge, resolve blocking
feedback, satisfy applicable checks, and ensure consumers, generated files and
lockfiles agree. The final commit message must follow the commit rules.

## Release PRs

List selected components, versions, test (`x.y.z-rc.N`) or stable (`x.y.z`)
channel, included changes and upgrade limits. Keep product fixes in ordinary PRs.

Preserve Rust path/version and pnpm workspace dependencies in source. Version
preparation synchronizes versions, constraints and lockfiles; packaging converts
local dependencies for distribution. Publication follows the implemented release
workflow and its verification gates.
