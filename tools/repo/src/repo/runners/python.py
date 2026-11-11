"""Native test execution and measurement."""

from __future__ import annotations

import json
import tempfile
from pathlib import Path

from ..diagnostics import redact, sanitize
from .coverage import CoverageResult


def collect(project, runner, execute=False, only=None, coverage=False):
    scope = {
        "project": project.name,
        "tests": [],
        "coverage": [],
        "completeness": "incomplete",
        "note": "",
        "exit_code": None,
    }
    with tempfile.TemporaryDirectory(prefix="whalerein-tests-") as tmp:
        destination = Path(tmp) / "pytest.json"
        argv = ["uv", "run", "--project", str(project.path), "pytest", "-p", "repo.pytest_report"]
        coverage_path = Path(tmp) / "coverage.json"
        if coverage:
            argv += [
                "--cov=" + project.coverage_source,
                f"--cov-report=json:{coverage_path}",
                "--cov-fail-under=0",
            ]
        if not execute:
            argv.append("--collect-only")
        result = runner.run(
            argv,
            project.path,
            quiet=True,
            timeout=1800,
            env={
                "WHALEREIN_TEST_REPORT": str(destination),
                **({"WHALEREIN_TEST_ONLY": only} if only else {}),
            },
        )
        scope["exit_code"] = result.returncode
        if not result.ok:
            scope["errors"] = [sanitize(result.tail()) or "Native test execution failed"]
        if destination.exists():
            data = json.loads(destination.read_text())
            scope["tests"] = [
                {
                    **c,
                    "id": redact(c["id"]),
                    **(
                        {"behavior": {k: redact(v) for k, v in c["behavior"].items()}}
                        if "behavior" in c
                        else {}
                    ),
                }
                for c in data["tests"]
            ]
            complete = (
                data["collection_errors"] == 0
                and len(scope["tests"]) == data["collected"]
                and (result.returncode in (0, 1, 5))
                and (not execute or all(c["status"] != "not_run" for c in scope["tests"]))
            )
            scope["completeness"] = "complete" if complete else "incomplete"
        if coverage:
            measured = CoverageResult(project.name, "python", project.rel)
            try:
                totals = json.loads(coverage_path.read_text())["totals"]
                measured.percent = float(totals["percent_covered"])
                measured.covered = int(totals["covered_lines"])
                measured.total = int(totals["num_statements"])
                if not result.ok:
                    measured.error = "Coverage execution failed"
            except (OSError, ValueError, KeyError, TypeError):
                measured.error = "Missing or invalid pytest coverage output"
            entry = measured.as_dict()
            entry.update(unit="lines", phase="test_execution")
            scope["coverage"].append(entry)
    scope["note"] = "pytest items under the current configuration; subtest plugins not supported."
    return scope


def steps(project, action):
    from ..toolchain import Step

    prefix = ("uv", "run", "--project", str(project.path), "ruff")
    commands = (
        [(*prefix, "format", "src", "tests")]
        if action == "fmt"
        else [(*prefix, "format", "--check", "src", "tests"), (*prefix, "check", "src", "tests")]
    )
    return [Step(f"{project.id}: ruff", command, project.path) for command in commands]
