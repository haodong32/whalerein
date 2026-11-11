"""Repository root discovery and shared path constants."""

from __future__ import annotations

import os
from pathlib import Path

from .errors import RepoError

ROOT_ENV = "WHALEREIN_ROOT"

# Directories that never contain first-party projects or are generated output.
SKIP_DIR_NAMES = frozenset(
    {
        ".git",
        ".agents",
        ".claude",
        ".hg",
        ".svn",
        ".data",
        ".venv",
        ".uv",
        "__pycache__",
        "node_modules",
        "target",
        "vendor",
        "dist",
        "build",
        "coverage",
        ".pytest_cache",
        ".ruff_cache",
        ".mypy_cache",
    }
)


def find_root(start: Path | None = None) -> Path:
    """Return the repository root.

    ``WHALEREIN_ROOT`` wins when set; otherwise walk up from ``start`` (the
    package directory by default) looking for the repository markers.
    """
    env = os.environ.get(ROOT_ENV)
    if env:
        candidate = Path(env).expanduser().resolve()
        if not candidate.is_dir():
            raise RepoError(f"{ROOT_ENV} does not point to a directory: {candidate}")
        return candidate

    origin = (start or Path(__file__)).resolve()
    for candidate in (origin, *origin.parents):
        if (candidate / "AGENTS.md").is_file() and (candidate / "tools" / "repo").is_dir():
            return candidate
    raise RepoError("could not locate the repository root; set WHALEREIN_ROOT")


def is_skipped(path: Path, root: Path) -> bool:
    """Whether a path lives under a skipped directory or outside the root."""
    try:
        rel = path.relative_to(root)
    except ValueError:
        return True
    return any(part in SKIP_DIR_NAMES for part in rel.parts)
