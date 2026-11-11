# Commits

Follow this document when creating a git commit.

## Message

Use Conventional Commits:

```text
type(scope): subject

Optional body that explains why.

BREAKING CHANGE: what callers must do differently.
```

- **type** is `feat`, `fix`, `docs`, `test`, `refactor`, or `chore`.
- **scope** is `cli`, `core`, `access`, `dsh`, `proto`, `deploy`, `repo`, or `docs`. Omit the parentheses when one change spans several areas. Use the area the change is about; a test for that area keeps the same scope.
- **subject** is imperative English, at most 72 characters, with no trailing period.
- Add a **body** when the subject does not say why the change exists. Wrap the body at 72 characters. Leave the body out when the subject is the whole story.
- Add **BREAKING CHANGE** when a caller, a wire field, or a stored row must change. Put the migration in that footer.

`feat` adds behavior. `fix` corrects a defect. `docs` changes documentation only. `test` changes tests only. `refactor` keeps behavior and changes structure. `chore` covers tooling, dependencies, and repository maintenance.

## One change

Each commit is one change. Generated files and lockfiles that this change requires go in the same commit. Regenerated bindings belong in the same commit as the `.proto` edit that produced them.

## Before committing

- The message matches the format above.
- The diff is one change.
- The commit adds no secrets, message contents, tool parameters, private keys, or pairing tokens.
- The message reports only checks that were actually run.

## Hook

From the repository root, point Git at the committed hooks:

```bash
git config core.hooksPath tools/hooks
```

The `commit-msg` hook invokes the internal `repo.commits` module through uv and
rejects the commit when its check fails. It is not a public `./repo` command.

The hook enforces:

- the message format above, including the 72-character subject, the blank line before a body, and body lines wrapped at 72 characters
- a staged path that is an env file, a certificate, or a private key
- a private-key block in the message or in an added diff line

A body line with no whitespace may exceed 72 characters. Git-generated `Merge `, `Revert "`, `fixup! `, and `squash! ` subjects skip the format check. `git commit --no-verify` skips the hook.

The hook leaves these checks to the author: one change per commit, imperative English, pairing tokens, message contents, tool parameters, and whether the message reports a check that was actually run.

## Examples

```text
feat(core): persist the session cursor across restart

The cursor is the sync resume point. Losing it on process exit
replays the session log from the beginning.
```

```text
fix(access): reject a digest conflict before the host runs
```

```text
docs: record the commit message convention
```
