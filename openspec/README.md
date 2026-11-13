# Personal OpenSpec workspace

This workspace and its generated skills are excluded through `.git/info/exclude`.
They stay on this machine. Current behavior comes from code and reproducible
observations; approved change specs define the intended outcome for that change.
Project-specific rules live in [config.yaml](config.yaml).

## Use in Codex

1. Select `openspec-propose` and describe a business change. The agent inspects
   the repository and prepares the default planning artifacts.
2. Resolve consequential choices in the Decision Review table, then review the
   architecture and Acceptance Review table in `design.md`. Pending decisions
   block dependent implementation; previously written plans are not approval.
3. Select `openspec-apply-change` to implement the approved change.
4. Select `openspec-verify-change` to check the implementation against the plan.
   Reconcile every AC row in `tasks.md` with fresh verification evidence.
5. Review acceptance failures, blockers, unverified items, architectural
   deviations, and the decision about maintained documentation.
6. Select `openspec-archive-change` when ready to retain the local record.

Skills can also be invoked by `$<skill-name>` in Codex environments that support
that syntax. Reload the project or start a new chat if newly installed skills
are not discovered by the current session.

`openspec-explore` helps investigate an unclear request; `openspec-update-change`
revises an existing plan. Use ordinary repository workflows for trivial changes.

## Completion and documentation

Acceptance rows use stable AC identifiers and separate implementation progress
from PASS, FAIL, BLOCKED, or NOT RUN results. A pass requires relevant evidence.
OpenSpec's structural validation and agent verification do not enforce a CI gate
or prove every runtime assertion. Record the limits of each actual check.

Keep plans, test-run history, and acceptance records local. Update tracked `docs/`
only for durable information supported by the implementation. OpenSpec `sync`
updates `openspec/specs/`; it does not publish maintained documentation.

## Acceptance evidence contract

An AC passes only when every required variant has current evidence. The case
matrix in each change's tasks names the variants; use stable IDs such as
`session-creation/AC-14.blank` in test names or the evidence index. Before running
acceptance, bind each ID to a concrete automated test or native procedure and
record its setup, action, assertions, command and result location. These IDs are
planned verification identities, not claims that tests already exist.

Each subcase records PASS, FAIL, BLOCKED or NOT RUN. A failed subcase makes its
parent FAIL; otherwise an unresolved prerequisite makes it BLOCKED, an unexecuted
required subcase leaves it NOT RUN, and only all-PASS makes the parent PASS.
Keep those subresults in the local evidence record and link them from the AC row.
Do not infer parent success from a whole-suite exit code, coverage, a task check,
or an unsupported-capability test passing.

Evidence identifies the tested code revision plus any dirty-tree fingerprint,
native package version, configuration assumptions, executed case IDs, exit/result,
artifact locations and limitations. Record metadata and synthetic fixture IDs;
keep message contents, tool parameters and secrets out of logs and evidence.
For bounds, record the source constant/configuration and actual encoded budget,
including envelope overhead; test below, at and above the effective boundary.
Native behavior assertions need native procedure evidence. Deterministic fault
injection and wire/CLI assertions may use isolated automated fixtures, with their
scope clearly identified. A missing safe native procedure is BLOCKED, not PASS.

Capability gates in each design list the required ACs and native checks. Treat
them as logical delivery gates, not a requirement to add new protocol flags. If
one flag covers several operations, all its operation gates must pass. Report
enabled/disabled operations, blockers and the tested scope explicitly. Partial
delivery is permitted, but unfinished ACs remain visible and the whole change
is not reported as fully accepted. Archive may retain a partial record only with
its outstanding results intact. These are agent review rules, not a CI gate.

The combined workflow cases live in the existing
[creation design](changes/add-dsh-session-creation/design.md#combined-workflow-acceptance).
They establish integration readiness for the combined feature set. They do not
force all seven features to ship together; unavailable positive-path variants
remain BLOCKED and cannot be counted as completed group acceptance.

## Maintenance

OpenSpec CLI 1.17.0 was installed through npm under the active Node installation.
Switching Node versions through nvm may require reinstalling the CLI.
The user-level OpenSpec profile includes the six core workflows plus `verify`.
Run `openspec update` after upgrading the CLI to refresh the upstream skills;
the project rules remain in `config.yaml`. Review generated paths against local
Git exclusions after an upgrade.
