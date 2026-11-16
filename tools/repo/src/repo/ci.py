"""Select affected CI projects from the repository inventory and native dependencies."""

from __future__ import annotations

import argparse
import json
import subprocess
import tomllib
from pathlib import Path


def plan(root: Path, paths: list[str]) -> dict:
    entries = tomllib.loads((root / "tools/repo/projects.toml").read_text())["projects"]
    ids = {p["id"] for p in entries}
    selected = set()
    for path in paths:
        if path.endswith(".md") or path.startswith(
            ("docs/", ".local-docs/", "apps/ios/", "tools/ios-stack/")
        ):
            # The iOS application and its local stack have their own macOS workflow.
            continue
        owners = {
            p["id"]
            for p in entries
            if any(
                path == s or path.startswith(s + "/")
                for s in (p.get("sources") or [p["path"]])
                if s != "."
            )
        }
        if path.startswith(("proto/", "tests/fixtures/")):
            owners |= ids
        elif path in ("Cargo.toml", "Cargo.lock"):
            owners |= {p["id"] for p in entries if p["kind"] == "rust"}
        elif path in ("go.mod", "go.sum"):
            owners.add("access")
        elif path in ("package.json", "pnpm-lock.yaml", "pnpm-workspace.yaml"):
            owners |= {p["id"] for p in entries if p["kind"] == "node"}
        elif path.startswith(("tools/repo/", ".github/", "tests/integration/", "deploy/")):
            owners |= ids
        elif not owners:
            # Unknown build/configuration inputs must not silently skip validation.
            owners |= ids
        selected |= owners
    while True:
        expanded = selected | {
            p["id"] for p in entries if selected.intersection(p.get("depends_on", []))
        }
        if expanded == selected:
            break
        selected = expanded
    groups = []
    for kind in ("rust", "go", "node", "python"):
        projects = [p["id"] for p in entries if p["kind"] == kind and p["id"] in selected]
        if projects:
            groups.append({"kind": kind, "projects": projects})
    return {"include": groups}


def changed_paths(root: Path, base: str, head: str) -> list[str]:
    if not base or set(base) == {"0"}:
        return [".github/workflows/ci.yml"]  # New branch: conservative full validation.
    result = subprocess.run(
        ["git", "diff", "--name-only", "--no-renames", "-z", base, head, "--"],
        cwd=root,
        check=True,
        capture_output=True,
    )
    return result.stdout.decode().rstrip("\0").split("\0") if result.stdout else []


def run_projects(root: Path, projects: list[str]) -> None:
    from .projects import load_projects, select_projects

    selected = select_projects(load_projects(root), projects)
    if not projects:
        raise ValueError("CI requires explicit projects")
    for operation in ("check", "build", "test"):
        supported = [p.id for p in selected if operation in p.capabilities]
        if supported:
            command = [str(root / "repo"), operation]
            for identity in supported:
                command += ["--project", identity]
            if operation == "test":
                command += ["--coverage"]
            subprocess.run(command, cwd=root, check=True)


def native_commands(root: Path, matrix: dict) -> list[list[str]]:
    """Native unit commands for a non-Linux runner.

    The macOS and Windows jobs cannot use ``./repo`` for execution: its
    streaming process runner relies on POSIX process groups and selector
    support for pipes. They still honor the Linux job's affected-project
    selection, so each selected project maps to its native unit command. These
    jobs intentionally do not measure coverage; Linux remains the coverage
    gate and the platform jobs prove OS-gated behavior.
    """
    from .projects import go_test_paths, load_projects, select_projects

    groups = matrix.get("include", [])
    identities = [identity for group in groups for identity in group.get("projects", [])]
    if not identities:
        return []
    selected = select_projects(load_projects(root), identities)
    commands: list[list[str]] = []
    for project in selected:
        if project.kind == "rust":
            commands.append(["cargo", "test", "--package", project.name])
        elif project.kind == "go":
            commands.append(["go", "test", *go_test_paths(project)])
        elif project.kind == "node":
            commands.append(["pnpm", "--filter", project.name, "test"])
        elif project.kind == "python":
            commands.append(["uv", "run", "--project", "tools/repo", "pytest", "tools/repo/tests"])
    return commands


def run_native(root: Path, matrix: dict) -> None:
    """Run every selected project's native unit tests without the repo runner."""
    commands = native_commands(root, matrix)
    if not commands:
        return
    for command in commands:
        subprocess.run(command, cwd=root, check=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("plan", "run", "native"))
    parser.add_argument("--base", default="")
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--projects", default="[]")
    parser.add_argument("--matrix", default='{"include": []}')
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[4]
    if args.operation == "plan":
        print(json.dumps(plan(root, changed_paths(root, args.base, args.head))))
    elif args.operation == "run":
        run_projects(root, json.loads(args.projects))
    else:
        run_native(root, json.loads(args.matrix))


if __name__ == "__main__":
    main()
