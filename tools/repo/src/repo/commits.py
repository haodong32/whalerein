"""Checks for the commit convention in docs/commits.md."""

from __future__ import annotations

import re
import subprocess
from pathlib import Path, PurePosixPath

from .errors import CommandFailedError, RepoError

TYPES = ("feat", "fix", "docs", "test", "refactor", "chore")
SCOPES = ("cli", "core", "access", "dsh", "proto", "deploy", "repo", "docs")
SUBJECT_LIMIT = 72
BODY_LIMIT = 72

_HEADER = re.compile(rf"^({'|'.join(TYPES)})(?:\(({'|'.join(SCOPES)})\))?: (?P<subject>\S.*)$")
_GENERATED = re.compile(r"^(?:Merge |Revert \"|fixup! |squash! )")
_TRAILER = re.compile(r"^(?:[A-Za-z0-9]+(?:-[A-Za-z0-9]+)*): \S")
_PEM = re.compile(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----")
_SCISSORS = "# ------------------------ >8 ------------------------"
_SECRET_SUFFIXES = frozenset({".pem", ".key", ".crt", ".csr", ".srl"})
_SECRET_NAMES = frozenset({"id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"})


def message_errors(text: str) -> list[str]:
    """Return format and private-key problems in a commit message."""
    errors: list[str] = []
    if _PEM.search(text):
        errors.append("commit message contains a private key")
    lines = _normalize(text)
    if not lines:
        errors.append("commit message is empty")
        return errors
    if _GENERATED.match(lines[0]):
        return errors
    match = _HEADER.match(lines[0])
    if match is None:
        errors.append(
            "subject must match `type(scope): subject`. "
            f"type is {', '.join(TYPES)}. "
            f"scope is {', '.join(SCOPES)}, or omit the parentheses"
        )
        return errors
    subject = match.group("subject").strip()
    if subject.endswith("."):
        errors.append("subject must not end with a period")
    if len(subject) > SUBJECT_LIMIT:
        errors.append(f"subject is {len(subject)} characters; the limit is {SUBJECT_LIMIT}")
    body = lines[1:]
    if not body:
        return errors
    if body[0] != "":
        errors.append("separate the body from the subject with a blank line")
    for line in body:
        if line == "":
            continue
        if line.startswith("BREAKING CHANGE:"):
            if not line.removeprefix("BREAKING CHANGE:").strip():
                errors.append("BREAKING CHANGE needs a description")
            continue
        if _TRAILER.match(line) or " " not in line:
            continue
        if len(line) > BODY_LIMIT:
            errors.append(f"body line exceeds {BODY_LIMIT} characters: {line[:40]}")
    return errors


def staged_errors(paths: list[str], patch: str) -> list[str]:
    """Return secret-path and private-key problems in a staged diff."""
    errors = [problem for rel in paths if (problem := _path_error(rel))]
    added = [
        line
        for line in patch.splitlines()
        if line.startswith("+") and not line.startswith("+++") and _PEM.search(line)
    ]
    if added:
        errors.append("staged diff contains a private key")
    return errors


def read_staged(root: Path) -> tuple[list[str], str]:
    """Read staged paths and the staged patch from git."""
    names = _git(
        root,
        ["diff", "--cached", "--name-only", "--diff-filter=ACMRT", "-z"],
    )
    patch = _git(root, ["diff", "--cached", "--diff-filter=ACMRT", "--unified=0"])
    return [item for item in names.split("\0") if item], patch


def _normalize(text: str) -> list[str]:
    raw: list[str] = []
    for line in text.splitlines():
        if line.startswith(_SCISSORS):
            break
        if line.startswith("#"):
            continue
        raw.append(line.rstrip())
    while raw and raw[0] == "":
        raw.pop(0)
    while raw and raw[-1] == "":
        raw.pop()
    return raw


def _path_error(rel: str) -> str | None:
    path = PurePosixPath(rel.replace("\\", "/"))
    if "certs" in path.parts:
        return f"staged path is certificate material: {rel}"
    name = path.name
    if name in _SECRET_NAMES:
        return f"staged path is a private key: {rel}"
    if name.startswith(".env") and not name.endswith((".example", ".template")):
        return f"staged path is an env file: {rel}"
    if path.suffix.lower() in _SECRET_SUFFIXES:
        return f"staged path is key or certificate material: {rel}"
    return None


def _git(root: Path, args: list[str]) -> str:
    try:
        completed = subprocess.run(
            ["git", "--no-pager", *args],
            cwd=root,
            check=False,
            capture_output=True,
            text=True,
        )
    except FileNotFoundError as exc:
        raise CommandFailedError("git is not on PATH") from exc
    if completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip() or "git failed"
        raise CommandFailedError(detail)
    return completed.stdout


def main(argv=None):
    import sys

    from .output import Output
    from .paths import find_root

    arguments = sys.argv[1:] if argv is None else argv
    out = Output()
    try:
        if len(arguments) != 1:
            raise RepoError("expected a commit message file")
        errors = message_errors(Path(arguments[0]).read_text())
        paths, patch = read_staged(find_root())
        errors.extend(staged_errors(paths, patch))
        for error in errors:
            out.error(error)
        return int(bool(errors))
    except (RepoError, OSError) as exc:
        out.error(str(exc))
        return 1


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
