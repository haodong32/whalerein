"""Capture declared Git inputs before invoking native build tools."""

from __future__ import annotations

import hashlib
import os
import shutil
import stat
import subprocess
from dataclasses import dataclass
from pathlib import Path


class SourceError(Exception):
    """The requested input could not be frozen."""


@dataclass(frozen=True)
class SourceSelection:
    name: str
    root: Path
    destination: str = "."
    revision: str | None = None
    exclude: tuple[str, ...] = ()


@dataclass(frozen=True)
class CapturedSource:
    root: Path
    revision: str
    fingerprint: str
    mode: str


def _git(root, *args):
    try:
        return subprocess.run(["git", "-C", root, *args], check=True, capture_output=True).stdout
    except (OSError, subprocess.CalledProcessError) as exc:
        raise SourceError("git-input-unavailable") from exc


def _within(path, parent):
    return path == parent or parent in path.parents


def _allowed(relative, excluded):
    if relative == Path("access/config.yaml"):
        return False
    return not any(_within(relative, entry) for entry in excluded) and all(
        part not in {".git", ".data", ".local-docs", ".venv", "node_modules"}
        and (not part.startswith(".env") or part.endswith((".example", ".sample")))
        and not part.endswith((".key", ".pem", ".p12", ".pfx", ".env.local"))
        for part in relative.parts
    )


def _inventory(selection, revision, excluded):
    committed = _git(selection.root, "ls-tree", "-r", "-z", revision)
    gitlinks = [
        Path(os.fsdecode(record.split(b"\t", 1)[1]))
        for record in committed.split(b"\0")
        if record.startswith(b"160000 ")
    ]
    if any(_allowed(path, excluded) for path in gitlinks):
        raise SourceError("undeclared-source-gitlink")
    values = [record.split(b"\t", 1)[1] for record in committed.split(b"\0") if record]
    if selection.revision is None:
        values = _git(
            selection.root, "ls-files", "-z", "--cached", "--others", "--exclude-standard"
        ).split(b"\0")
    return sorted(
        {path for value in values if value and _allowed(path := Path(os.fsdecode(value)), excluded)}
    )


def _capture_local(source, destination):
    try:
        before = source.lstat()
    except FileNotFoundError:
        return
    if stat.S_ISLNK(before.st_mode):
        destination.symlink_to(source.readlink())
    elif stat.S_ISREG(before.st_mode):
        shutil.copyfile(source, destination, follow_symlinks=False)
        destination.chmod(stat.S_IMODE(before.st_mode) & 0o777)
    elif not source.is_dir():
        raise SourceError("unsupported-source-entry")
    if _signature(before) != _signature(source.lstat()):
        raise SourceError("source-changed-during-capture")


def _signature(info):
    return (
        info.st_dev,
        info.st_ino,
        info.st_mode,
        info.st_size,
        info.st_mtime_ns,
        info.st_ctime_ns,
    )


def _fingerprint(root, paths, layout):
    fingerprint = hashlib.sha256()
    for path in paths:
        file = root / path
        if file.is_symlink():
            target = file.readlink()
            try:
                resolved = (file.parent / target).resolve()
            except (OSError, RuntimeError) as exc:
                raise SourceError("invalid-source-link") from exc
            if target.is_absolute() or not _within(resolved, layout):
                raise SourceError("undeclared-source-link")
            identity = b"link:" + os.fsencode(target)
        elif file.is_file():
            with file.open("rb") as handle:
                identity = hashlib.file_digest(handle, "sha256").digest()
            identity = (b"exec:" if file.stat().st_mode & 0o111 else b"file:") + identity
        else:
            continue
        name = os.fsencode(path)
        fingerprint.update(len(name).to_bytes(8, "big") + name)
        fingerprint.update(len(identity).to_bytes(8, "big") + identity)
    return fingerprint.hexdigest()


def _capture_revision(selection, revision, root, excluded):
    # git archive streams blobs without consulting the dirty worktree.
    import io
    import tarfile

    archive = _git(selection.root, "archive", "--format=tar", revision)
    with tarfile.open(fileobj=io.BytesIO(archive)) as entries:
        for entry in entries:
            relative = Path(entry.name)
            if not _allowed(relative, excluded) or entry.isdir():
                continue
            if relative.is_absolute() or ".." in relative.parts:
                raise SourceError("invalid-source-entry")
            destination = root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            if entry.issym():
                destination.symlink_to(entry.linkname)
            elif entry.isfile():
                with entries.extractfile(entry) as source, destination.open("xb") as output:
                    shutil.copyfileobj(source, output)
                destination.chmod(entry.mode)
            else:
                raise SourceError("unsupported-source-entry")


def capture_sources(selections, layout):
    """Assemble the complete layout, then seal every declared input together."""
    layout.mkdir()
    pending = {}
    for selection in selections:
        destination = Path(selection.destination)
        if destination.is_absolute() or ".." in destination.parts or selection.name in pending:
            raise SourceError("invalid-source-layout")
        root = layout / destination
        if not _within(root.resolve(), layout):
            raise SourceError("invalid-source-layout")
        excluded = tuple(Path(entry) for entry in selection.exclude)
        checkout = selection.root.resolve()
        if checkout in layout.parent.parents:
            excluded = (*excluded, layout.parent.relative_to(checkout))
        revision = (
            _git(
                selection.root,
                "rev-parse",
                "--verify",
                "--end-of-options",
                f"{selection.revision or 'HEAD'}^{{commit}}",
            )
            .decode()
            .strip()
        )
        paths = _inventory(selection, revision, excluded)
        root.mkdir(parents=True, exist_ok=True)
        if selection.revision is not None:
            _capture_revision(selection, revision, root, excluded)
        else:
            originals = {}
            for path in paths:
                source = checkout / path
                if not _within(source.parent.resolve(), checkout):
                    raise SourceError("undeclared-source-link")
                if source.exists() or source.is_symlink():
                    originals[path] = _signature(source.lstat())
                output = root / path
                output.parent.mkdir(parents=True, exist_ok=True)
                _capture_local(source, output)
            if (
                paths != _inventory(selection, revision, excluded)
                or revision != _git(selection.root, "rev-parse", "HEAD").decode().strip()
                or any(
                    (source := checkout / path).exists() or source.is_symlink()
                    for path in paths
                    if path not in originals
                )
                or any(
                    _signature((checkout / path).lstat()) != signature
                    for path, signature in originals.items()
                )
            ):
                raise SourceError("source-changed-during-capture")
        pending[selection.name] = root, revision, paths, selection.revision is not None
    sources = {
        name: CapturedSource(
            root,
            revision,
            _fingerprint(root, paths, layout),
            "commit" if exact else "worktree",
        )
        for name, (root, revision, paths, exact) in pending.items()
    }
    for path in sorted(layout.rglob("*"), key=lambda path: -len(path.parts)):
        if not path.is_symlink():
            path.chmod(stat.S_IMODE(path.stat().st_mode) & ~0o222)
    layout.chmod(0o555)
    return sources
