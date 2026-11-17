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

Write the title and description in English for human reviewers. Keep the title
in Conventional Commits format, following [commit rules](commits.md), with a
short, plain-language subject that describes the resulting change.

Use the four headings in the [PR template](../.github/pull_request_template.md),
in order, including when creating a PR/MR through a CLI or API:

- **Background:** explain the problem or reason for the change; link a related
  issue when available.
- **Changes:** summarize the main changes and resulting behavior. Include
  implementation details only when they help the reviewer assess the change.
- **Key test changes:** list only important test cases added, updated or removed,
  with a brief before-and-after comparison of their assertions or coverage.
  For a new case, state the previous coverage gap and what it now verifies;
  for a removed case, explain what replaces it or why it is obsolete. If no
  cases changed, write `None`. Keep commands, pass counts, coverage percentages,
  execution results and environment limits in CI or test reports, not this section.
- **Compatibility and breaking changes:** explain the impact on existing users
  and consumers, including API, protocol, configuration or state changes. Include
  migration, upgrade order and rollback limits when relevant; write `None` when
  there is no compatibility impact.

Keep each section to a few sentences or short bullets, scaled to the change.
Keep compatibility impact separate from the change summary so reviewers can
quickly find upgrade requirements.

Update the description when scope changes. Keep secrets, pairing links and
private message/tool contents out of the PR and attachments.

## Validation and merge

Use `./repo` and follow [testing policy](testing.md). Report only verified
results for the relevant revision; partial, skipped or cancelled checks are not
full passes. Retain execution evidence in CI or saved test reports without
committing acceptance histories. Do not rely on workflows that are not implemented.

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
