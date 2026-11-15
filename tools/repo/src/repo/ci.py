"""Select affected CI projects from the repository inventory and native dependencies."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tomllib
from pathlib import Path


def plan(root: Path, paths: list[str]) -> dict:
    entries = tomllib.loads((root / "tools/repo/projects.toml").read_text())["projects"]
    ids = {p["id"] for p in entries}
    selected = set()
    for path in paths:
        if path.endswith(".md") or path.startswith(("docs/", ".local-docs/")):
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


def native_plan(root: Path, paths: list[str]) -> dict:
    rust = next((g["projects"] for g in plan(root, paths)["include"] if g["kind"] == "rust"), [])
    return {
        "include": [{"os": os, "projects": rust} for os in ("macos-15", "windows-2025") if rust]
    }


def run_projects(root: Path, projects: list[str]) -> None:
    from .projects import load_projects, select_projects

    selected = select_projects(load_projects(root), projects)
    if not projects:
        raise ValueError("CI requires explicit projects")
    for operation in ("check", "build", "test"):
        supported = [p.id for p in selected if operation in p.capabilities]
        if supported:
            command = [sys.executable, "-m", "repo", operation]
            for identity in supported:
                command += ["--project", identity]
            if operation == "test":
                command += ["--coverage"]
            subprocess.run(command, cwd=root, check=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("plan", "native-plan", "run"))
    parser.add_argument("--base", default="")
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--projects", default="[]")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[4]
    if args.operation in ("plan", "native-plan"):
        planner = plan if args.operation == "plan" else native_plan
        print(json.dumps(planner(root, changed_paths(root, args.base, args.head))))
    else:
        run_projects(root, json.loads(args.projects))


if __name__ == "__main__":
    main()
