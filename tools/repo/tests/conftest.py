from __future__ import annotations

import io
import sys
from dataclasses import dataclass, field
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from repo.output import Output  # noqa: E402
from repo.process import ProcessResult  # noqa: E402


@dataclass
class FakeRunner:
    which_map: dict[str, str] = field(default_factory=dict)
    responses: list[object] = field(default_factory=list)
    default_returncode: int = 0
    default_stdout: str = ""
    calls: list[tuple[tuple[str, ...], str]] = field(default_factory=list)

    def which(self, tool: str) -> str | None:
        return self.which_map.get(tool)

    def require(self, tool: str, hint: str = "") -> str:
        path = self.which(tool)
        if path is None:
            from repo.errors import ToolMissingError

            raise ToolMissingError(f"required tool not found on PATH: {tool}")
        return path

    def run(self, argv, cwd, *, env=None, timeout=None, check=False, quiet=False):
        args = tuple(str(part) for part in argv)
        self.calls.append((args, str(cwd)))
        spec = self.responses.pop(0) if self.responses else None
        if isinstance(spec, ProcessResult):
            return spec
        if isinstance(spec, Exception):
            raise spec
        return ProcessResult(
            argv=args,
            cwd=str(cwd),
            returncode=self.default_returncode,
            stdout=self.default_stdout,
        )


@pytest.fixture
def out():
    return Output(
        json_mode=False,
        verbose=False,
        stdout=io.StringIO(),
        stderr=io.StringIO(),
    )


def make_project_tree(root: Path) -> Path:
    (root / "AGENTS.md").write_text("# test\n")
    (root / "tools" / "repo").mkdir(parents=True)
    (root / "tools" / "repo" / "pyproject.toml").write_text(
        '[project]\nname = "whalerein-repo"\nversion = "0.1.0"\n'
    )
    (root / "Cargo.toml").write_text('[workspace]\nmembers = ["core"]\n')
    (root / "core").mkdir()
    (root / "core" / "Cargo.toml").write_text(
        '[package]\nname = "whalerein-core"\nversion = "0.1.0"\n'
    )
    (root / "access").mkdir()
    (root / "access" / "go.mod").write_text("module github.com/whalerein/access\n\ngo 1.25\n")
    (root / "host" / "dsh").mkdir(parents=True)
    (root / "host" / "dsh" / "package.json").write_text(
        '{"name":"whalerein-dsh","version":"0.1.0","scripts":{"test":"node --test test/"}}'
    )
    return root


@pytest.fixture
def repo_tree(tmp_path):
    return make_project_tree(tmp_path)


def ok(stdout: str = "", returncode: int = 0, stderr: str = "") -> ProcessResult:
    return ProcessResult(argv=(), cwd=".", returncode=returncode, stdout=stdout, stderr=stderr)


def missing() -> ProcessResult:
    return ProcessResult(argv=(), cwd=".", returncode=127, missing=True)


def result(argv, returncode=0, stdout="", stderr="", missing=False):
    return ProcessResult(
        argv=tuple(argv),
        cwd=".",
        returncode=returncode,
        stdout=stdout,
        stderr=stderr,
        missing=missing,
    )


def pytest_configure(config):
    config.addinivalue_line("markers", "behavior(id, ensures): stable behavior guarantee")


def collect(project, runner, execute=False, only=None):
    from repo.toolchain import RUNNERS

    return RUNNERS[project.kind].collect(project, runner, execute, only)


def report_facts(data):
    from repo.reports.model import VERSION, summary

    for index, scope in enumerate(data["scopes"]):
        scope.setdefault("suite_id", "test:" + str(index))
        scope.setdefault("project", "sample")
        scope.setdefault("completeness", "complete")
        for entry in scope.get("coverage", []):
            entry.setdefault("name", "sample")
            entry.setdefault("unit", "lines")
            entry.setdefault("ok", not entry.get("error") and entry.get("percent", 100) >= 90)
    data["schema_version"] = VERSION
    data["completeness"] = (
        "complete" if all(s["completeness"] == "complete" for s in data["scopes"]) else "incomplete"
    )
    data["summary"] = summary(data["scopes"])
    data.setdefault("configuration", {})
    return data


def render(data):
    from repo.reports.html import render as render_html

    return render_html(report_facts(data))
