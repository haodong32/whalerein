"""Explicit project registration and exact selection."""

from __future__ import annotations

import json
import tomllib
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path

from .errors import ProjectNotFoundError, RepoError
from .paths import is_skipped

MANIFESTS = {
    "rust": "Cargo.toml",
    "go": "go.mod",
    "node": "package.json",
    "python": "pyproject.toml",
    "kotlin": "build.gradle.kts",
}


@dataclass(frozen=True)
class Project:
    name: str
    kind: str
    path: Path
    manifest: Path
    root: Path
    id: str = ""
    role: str = "product"
    sources: tuple[str, ...] = ()
    binary: str = ""
    coverage_source: str = ""

    @property
    def rel(self) -> str:
        return self.path.relative_to(self.root).as_posix()

    @property
    def capabilities(self) -> list[str]:
        if self.kind == "kotlin":
            # The standalone state build compiles Kotlin and runs JUnit. It has
            # no formatter or packaging step, and coverage is not measured.
            return ["check", "test"]
        if self.kind == "node":
            scripts = json.loads(self.manifest.read_text()).get("scripts", {})
            return [
                op
                for op in ("fmt", "check", "build", "test")
                if (
                    op == "test"
                    or (op == "check" and ("lint" in scripts or "fmt:check" in scripts))
                    or op in scripts
                )
            ]
        return ["fmt", "check", "test"] + ([] if self.kind == "python" else ["build"])

    def as_dict(self) -> dict:
        return {
            "id": self.id,
            "name": self.name,
            "kind": self.kind,
            "path": self.rel,
            "role": self.role,
            "operations": self.capabilities,
        }


def load_projects(root: Path) -> list[Project]:
    try:
        entries = tomllib.loads((root / "tools/repo/projects.toml").read_text())["projects"]
        if not isinstance(entries, list) or not entries:
            raise ValueError("projects must be a nonempty array")
        projects = []
        identities = set()
        locations = set()
        for entry in entries:
            identity, kind = entry["id"], entry["kind"]
            if not isinstance(identity, str) or not identity or identity in identities:
                raise ValueError("project IDs must be nonempty and unique")
            path = (root / entry["path"]).resolve()
            if not path.is_relative_to(root.resolve()):
                raise ValueError("project path escapes repository")
            if (kind, path) in locations:
                raise ValueError("duplicate project location")
            manifest = path / MANIFESTS[kind]
            content = manifest.read_text()
            if kind == "go":
                name = next(
                    line.split()[1] for line in content.splitlines() if line.startswith("module ")
                )
            elif kind == "node":
                name = json.loads(content)["name"]
            elif kind == "kotlin":
                # A Gradle build has no package name in its build script.
                name = identity
            else:
                name = tomllib.loads(content)["package" if kind == "rust" else "project"]["name"]
            role = entry.get("role", "product")
            if role not in ("product", "tool", "bindings"):
                raise ValueError("invalid project role")
            sources = entry.get("sources", [])
            if not isinstance(sources, list) or any(
                not isinstance(s, str)
                or not (path / s).resolve().is_relative_to(path)
                or not (path / s).is_dir()
                for s in sources
            ):
                raise ValueError("invalid source directories")
            if kind == "python" and not entry.get("coverage_source"):
                raise ValueError("Python projects require coverage_source")
            projects.append(
                Project(
                    name,
                    kind,
                    path,
                    manifest,
                    root,
                    identity,
                    role,
                    tuple(sources),
                    entry.get("binary", ""),
                    entry.get("coverage_source", ""),
                )
            )
            identities.add(identity)
            locations.add((kind, path))
        return projects
    except (OSError, ValueError, KeyError, TypeError, StopIteration) as exc:
        raise RepoError("invalid project registry: " + str(exc)) from exc


def select_projects(projects: Sequence[Project], ids: Sequence[str] | None) -> list[Project]:
    if not ids:
        return list(projects)
    missing = set(ids) - {p.id for p in projects}
    if missing:
        raise ProjectNotFoundError("unknown project IDs: " + ", ".join(sorted(missing)))
    return [p for p in projects if p.id in ids]


def go_test_paths(project: Project) -> list[str]:
    return ["./" + source + "/..." for source in project.sources] or ["./..."]


def source_files(project: Project, pattern: str):
    for source in project.sources or (".",):
        for path in sorted((project.path / source).rglob(pattern)):
            if not is_skipped(path, project.path):
                yield path
