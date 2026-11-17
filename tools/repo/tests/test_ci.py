"""Affected selection must never schedule unrelated project bodies."""

import json
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest

from repo import ci

ROOT = Path(__file__).resolve().parents[3]


@pytest.mark.parametrize(
    ("paths", "expected"),
    [
        ([], set()),
        (["docs/testing.md", "README.md", ".local-docs/acceptance/result"], set()),
        (["apps/ios/WhaleRein/App/WhaleReinApp.swift"], set()),
        (["tools/ios-stack/ios_stack.py"], set()),
        (["core/src/api.rs"], {"core", "cli", "noise-peer", "mobile-bindings"}),
        (["apps/cli/src/main.rs"], {"cli"}),
        (["apps/mobile-bindings/src/lib.rs"], {"mobile-bindings"}),
        (["host/runtime/src/host.js"], {"host", "dsh"}),
        (["host/dsh/client.js"], {"dsh"}),
        (["access/internal/config/config.go"], {"access"}),
        (["go.sum"], {"access"}),
        (
            ["contracts/client-environment/catalog.json"],
            {"core", "cli", "noise-peer", "mobile-bindings", "host", "dsh"},
        ),
        (["Cargo.lock"], {"core", "cli", "proto", "noise-peer", "mobile-bindings"}),
        (["pnpm-lock.yaml"], {"host", "dsh"}),
    ],
)
def test_plan_selects_only_affected_consumers(paths, expected):
    matrix = ci.plan(ROOT, paths)
    assert {p for job in matrix["include"] for p in job["projects"]} == expected


@pytest.mark.parametrize(
    "path",
    [
        "proto/v1/whalerein.proto",
        "tests/fixtures/proto/fields.json",
        "tools/repo/src/repo/testing.py",
        ".github/workflows/ci.yml",
        "tests/integration/cases/test_chain.py",
        "deploy/docker/access.Dockerfile",
        "new-build-config",
    ],
)
def test_shared_and_unknown_inputs_select_every_project(path):
    matrix = ci.plan(ROOT, [path])
    assert sum(len(job["projects"]) for job in matrix["include"]) == 9


def test_diff_preserves_deleted_and_renamed_paths(monkeypatch):
    calls = []

    def run(command, **kwargs):
        calls.append(command)
        return SimpleNamespace(stdout=b"core/old.rs\0apps/cli/new.rs\0")

    monkeypatch.setattr(ci.subprocess, "run", run)
    assert ci.changed_paths(ROOT, "base", "head") == ["core/old.rs", "apps/cli/new.rs"]
    assert "--no-renames" in calls[0]
    monkeypatch.setattr(ci.subprocess, "run", lambda *a, **k: SimpleNamespace(stdout=b""))
    assert ci.changed_paths(ROOT, "base", "head") == []
    assert ci.changed_paths(ROOT, "", "head") == [".github/workflows/ci.yml"]
    assert ci.changed_paths(ROOT, "0000", "head") == [".github/workflows/ci.yml"]


def test_execution_excludes_unselected_projects_and_unsupported_build(monkeypatch):
    calls = []
    monkeypatch.setattr(ci.subprocess, "run", lambda cmd, **kwargs: calls.append(cmd))
    ci.run_projects(ROOT, ["dsh"])
    assert calls == [
        [sys.executable, "-m", "repo", "check", "--project", "dsh"],
        [sys.executable, "-m", "repo", "test", "--project", "dsh", "--coverage"],
    ]
    calls.clear()
    ci.run_projects(ROOT, ["cli"])
    assert [cmd[3] for cmd in calls] == ["check", "build", "test"]
    with pytest.raises(ValueError, match="explicit"):
        ci.run_projects(ROOT, [])


def test_cli_plan_and_run(monkeypatch, capsys):
    monkeypatch.setattr("sys.argv", ["ci", "plan", "--base", "base"])
    monkeypatch.setattr(ci, "changed_paths", lambda *args: ["apps/cli/src/main.rs"])
    ci.main()
    assert json.loads(capsys.readouterr().out)["include"][0]["projects"] == ["cli"]
    calls = []
    monkeypatch.setattr("sys.argv", ["ci", "run", "--projects", '["cli"]'])
    monkeypatch.setattr(ci, "run_projects", lambda *args: calls.append(args[1]))
    ci.main()
    assert calls == [["cli"]]


def test_native_commands_follow_the_affected_selection():
    matrix = ci.plan(ROOT, ["core/src/api.rs", "access/internal/config/config.go"])
    commands = ci.native_commands(ROOT, matrix)
    assert ["cargo", "test", "--package", "whalerein-core"] in commands
    assert ["cargo", "test", "--package", "whalerein-cli"] in commands
    assert ["cargo", "test", "--package", "noise-peer"] in commands
    assert ["cargo", "test", "--package", "whalerein-mobile"] in commands
    assert ["go", "test", "./access/...", "./proto/v1/bindings/go/..."] in commands


def test_native_commands_cover_node_and_python_projects():
    node = ci.plan(ROOT, ["host/runtime/src/host.js"])
    assert ci.native_commands(ROOT, node) == [
        ["pnpm", "--filter", "whalerein-host", "test"],
        ["pnpm", "--filter", "whalerein-dsh", "test"],
    ]
    python = ci.plan(ROOT, ["tools/repo/src/repo/testing.py"])
    assert [
        "uv",
        "run",
        "--project",
        "tools/repo",
        "pytest",
        "tools/repo/tests",
    ] in ci.native_commands(ROOT, python)


def test_native_runs_selected_commands_without_coverage(monkeypatch):
    calls = []
    monkeypatch.setattr(ci.subprocess, "run", lambda cmd, **kwargs: calls.append((cmd, kwargs)))
    ci.run_native(ROOT, {"include": [{"kind": "rust", "projects": ["cli"]}]})
    assert calls == [
        (["cargo", "test", "--package", "whalerein-cli"], {"cwd": ROOT, "check": True})
    ]
    assert all("--coverage" not in command for command, _ in calls)
    calls.clear()
    ci.run_native(ROOT, {"include": []})
    assert calls == []


def test_cli_native_passes_the_selected_matrix(monkeypatch):
    calls = []
    monkeypatch.setattr(
        "sys.argv",
        ["ci", "native", "--matrix", '{"include": [{"kind": "go", "projects": ["access"]}]}'],
    )
    monkeypatch.setattr(ci, "run_native", lambda root, matrix: calls.append(matrix))
    ci.main()
    assert calls == [{"include": [{"kind": "go", "projects": ["access"]}]}]


def test_native_plan_schedules_matching_rust_consumers_on_both_systems():
    jobs = ci.native_plan(ROOT, ["core/src/api.rs"])["include"]
    assert {job["os"] for job in jobs} == {"macos-15", "windows-2025"}
    assert all(set(job["projects"]) == {"core", "cli", "noise-peer"} for job in jobs)
    assert ci.native_plan(ROOT, ["host/dsh/client.js"]) == {"include": []}
    assert ci.native_plan(ROOT, ["docs/testing.md"]) == {"include": []}
    assert all(
        "proto" in job["projects"]
        for job in ci.native_plan(ROOT, [".github/workflows/ci.yml"])["include"]
    )


def test_quick_checks_never_compile_rust_or_collect_coverage(monkeypatch):
    calls = []
    monkeypatch.setattr(ci.subprocess, "run", lambda cmd, **kwargs: calls.append(cmd))
    ci.run_quick(ROOT, ["core", "cli", "dsh"])
    rust = [cmd for cmd in calls if cmd[0] == "cargo"]
    assert len(rust) == 2
    assert all(cmd[1] == "fmt" and cmd[-2:] == ["--", "--check"] for cmd in rust)
    assert [cmd[3] for cmd in calls if cmd[0] == sys.executable] == ["check", "test"]
    assert all("--coverage" not in cmd for cmd in calls)
    with pytest.raises(ValueError, match="explicit"):
        ci.run_quick(ROOT, [])


def test_quick_failure_stops_subsequent_checks(monkeypatch):
    import subprocess

    calls = []

    def fail(cmd, **kwargs):
        calls.append(cmd)
        raise subprocess.CalledProcessError(1, cmd)

    monkeypatch.setattr(ci.subprocess, "run", fail)
    with pytest.raises(subprocess.CalledProcessError):
        ci.run_quick(ROOT, ["core", "dsh"])
    assert len(calls) == 1


@pytest.mark.parametrize("kind,selected", [("rust", "true"), ("node", "false")])
def test_quick_plan_emits_single_line_outputs(monkeypatch, capsys, kind, selected):
    monkeypatch.setattr("sys.argv", ["ci", "quick-plan", "--kind", kind])
    monkeypatch.setattr(ci, "changed_paths", lambda *args: ["apps/cli/src/main.rs"])
    ci.main()
    lines = capsys.readouterr().out.splitlines()
    assert lines[0] == "selected=" + selected
    assert json.loads(lines[1].removeprefix("projects=")) == (["cli"] if kind == "rust" else [])


def test_quick_plan_requires_kind(monkeypatch):
    monkeypatch.setattr("sys.argv", ["ci", "quick-plan"])
    with pytest.raises(SystemExit) as error:
        ci.main()
    assert error.value.code == 2


def test_quick_cli_dispatch(monkeypatch):
    calls = []
    monkeypatch.setattr("sys.argv", ["ci", "quick", "--projects", '["cli"]'])
    monkeypatch.setattr(ci, "run_quick", lambda root, projects: calls.append(projects))
    ci.main()
    assert calls == [["cli"]]
