"""Single test execution flow; reports are values, not command dispatch state."""

from __future__ import annotations

import json
from dataclasses import asdict, dataclass
from datetime import UTC, datetime
from pathlib import Path
from time import monotonic
from uuid import uuid4

from . import test_cost
from .diagnostics import sanitize
from .errors import RepoError, ToolMissingError, UsageError
from .progress import Display
from .reports.html import render
from .reports.model import VERSION, atomic_write, read_report, summary, validate, validate_scope
from .runners import integration
from .test_descriptions import describe
from .toolchain import RUNNERS


@dataclass(frozen=True)
class TestRequest:
    suite: str = "unit"
    projects: tuple[str, ...] = ()
    only: str | None = None
    group: str | None = None
    coverage: bool = False
    list_only: bool = False
    integration_timeout: int | None = None
    parallel: int | None = None

    def validate(self):
        if self.suite not in ("unit", "integration"):
            raise UsageError("invalid test suite")
        if self.projects and self.suite != "unit":
            raise UsageError("a unit project cannot select integration testing")
        if self.group and self.suite != "integration":
            raise UsageError("an integration scope requires integration testing")
        if self.parallel is not None:
            if self.suite != "integration":
                raise UsageError("--parallel requires the integration suite")
            if self.list_only:
                raise UsageError("--parallel cannot combine with --list")
            if not 1 <= self.parallel <= 8:
                raise UsageError("--parallel must be between 1 and 8")
        if self.integration_timeout is not None:
            if self.suite != "integration":
                raise UsageError("--integration-timeout requires a Linux integration target")
            if self.integration_timeout <= 0:
                raise UsageError("--integration-timeout must be a positive number of seconds")
        if self.coverage and (
            self.only is not None or self.list_only or self.suite == "integration"
        ):
            raise UsageError("coverage requires unfiltered unit execution")
        if self.only is not None and not self.only.strip():
            raise UsageError("--only requires nonempty text")
        if self.only is not None and self.suite == "unit" and len(self.projects) != 1:
            raise UsageError("unit --only requires a leaf target")


def failure_scope(identity, name, message):
    return {
        "suite_id": identity,
        "project": name,
        "tests": [],
        "coverage": [],
        "completeness": "incomplete",
        "errors": [sanitize(message)],
        "exit_code": 1,
    }


def saved_test_result(result, out, *, list_only=False, history=None, display=None):
    directory = result.report_directory
    source = directory / "report.json"
    destination = directory / "report.html"
    report = read_report(source)
    if display is not None:
        display.result(report, result.exit_code)
    if list_only:
        for scope in report["scopes"]:
            for case in scope["tests"]:
                out.info(case["id"])
    out.info("Report: " + str(destination))
    return {
        "summary": report["summary"],
        "historical_duration": history,
        "json": str(source),
        "html": str(destination),
        "verification_error": report.get("verification", {}).get("error_code"),
    }, result.exit_code


def run(root, projects, runner, request, output=None):
    if request.suite == "integration":
        history = test_cost.announce(root, request, runner.out)
        display = None if request.list_only else Display(verbose=runner.out.verbose)
        runner.progress = display
        try:
            result = integration.collect(root, runner, request, output)
            return saved_test_result(
                result, runner.out, list_only=request.list_only, history=history, display=display
            )
        finally:
            runner.progress = None
            if display:
                display.close()
    if request.list_only and any(p.kind == "node" for p in projects):
        raise UsageError("Node static case listing is unsupported; select a different project")
    directory = output or root / ".data/runs" / (
        datetime.now(UTC).strftime("%Y%m%dT%H%M%S") + "-" + uuid4().hex[:8]
    )
    directory = Path(directory).resolve()
    try:
        directory.mkdir(mode=0o700, parents=True)
    except FileExistsError as exc:
        raise UsageError("output directory exists; choose a new directory") from exc
    history = test_cost.announce(root, request, runner.out)
    started = monotonic()
    scopes = []
    interrupted = False
    blocked = False
    display = None if request.list_only else Display(verbose=runner.out.verbose)
    runner.progress = display
    try:
        for project in projects:
            began = monotonic()
            runner.out.info(f"Unit: {project.id}")
            if display:
                display.event({"event": "phase", "description": "Unit · " + project.id})
            if interrupted:
                scope = failure_scope(
                    "unit:" + project.id, project.id, "Not executed after interruption"
                )
            else:
                try:
                    native_only = request.only
                    if native_only:
                        prefix = project.id + "::"
                        for overlap in range(min(len(prefix), len(native_only)), 0, -1):
                            if prefix.endswith(native_only[:overlap]):
                                native_only = native_only[overlap:] or None
                                break
                    options = {}
                    if project.kind != "rust":
                        options["coverage"] = request.coverage
                    if project.kind == "go" and request.coverage:
                        options["coverage_output"] = (
                            directory / f"coverage-{project.id}.coverprofile"
                        )
                    scope = RUNNERS[project.kind].collect(
                        project,
                        runner,
                        not request.list_only,
                        only=native_only,
                        **options,
                    )
                    scope.update(suite_id="unit:" + project.id, project=project.id)
                    scope.setdefault("coverage", [])
                    scope.setdefault("errors", [])
                    if scope.get("exit_code") not in (None, 0, 5):
                        scope["errors"].append(
                            "Native test command exited " + str(scope["exit_code"])
                        )
                    interrupted = scope.get("exit_code") == 130
                    blocked |= scope.get("exit_code") == 127
                    for case in scope["tests"]:
                        case["id"] = project.id + "::" + case["id"]
                        case["kind"] = "unit"
                        describe(case)
                    if (
                        request.coverage
                        and not interrupted
                        and project.role != "bindings"
                        and project.kind == "rust"
                    ):
                        measure_started = monotonic()
                        if display:
                            display.event(
                                {
                                    "event": "phase",
                                    "description": "Coverage · " + project.id,
                                    "count_cases": False,
                                }
                            )
                        runner.out.info(f"Coverage measurement (separate execution): {project.id}")
                        try:
                            measured = RUNNERS[project.kind].measure(project, runner)
                        except (
                            RepoError,
                            OSError,
                            ValueError,
                            KeyError,
                            TypeError,
                            KeyboardInterrupt,
                        ) as exc:
                            from .runners.coverage import CoverageResult

                            if isinstance(exc, KeyboardInterrupt):
                                interrupted = True
                                scope["exit_code"] = 130
                            measured = CoverageResult(
                                project.name, project.kind, project.rel, error=sanitize(exc)
                            )
                        if getattr(runner, "interrupted", False):
                            interrupted = True
                            scope["exit_code"] = 130
                        measured = measured if isinstance(measured, list) else [measured]
                        for value in measured:
                            entry = value.as_dict()
                            entry.update(
                                unit="statements" if project.kind == "go" else "lines",
                                phase="separate_measurement",
                            )
                            if entry.get("error"):
                                entry["error"] = sanitize(entry["error"])
                            scope["coverage"].append(entry)
                        scope["coverage_duration_ms"] = round(
                            (monotonic() - measure_started) * 1000, 3
                        )
                    validate_scope(scope)
                except KeyboardInterrupt:
                    interrupted = True
                    scope = failure_scope("unit:" + project.id, project.id, "Interrupted")
                    scope["exit_code"] = 130
                except (RepoError, OSError, ValueError, KeyError, TypeError) as exc:
                    scope = failure_scope("unit:" + project.id, project.id, exc)
                    if isinstance(exc, ToolMissingError):
                        scope["exit_code"] = 3
                        blocked = True
            scope.update(
                kind="unit",
                language=project.kind,
                path=project.rel,
                module=project.id,
                duration_ms=round((monotonic() - began) * 1000, 3),
            )
            scopes.append(scope)
    except BaseException:
        if display:
            display.close()
        raise
    finally:
        runner.progress = None
    unmatched = (
        request.only
        and all(
            s["completeness"] == "complete" and s.get("exit_code") in (None, 0, 5) for s in scopes
        )
        and not any(request.only in c["id"] for s in scopes for c in s["tests"])
    )
    if unmatched:
        scopes[0].setdefault("errors", []).append("No test cases match --only")
    configuration = asdict(request)
    configuration["parallel"] = request.parallel or 1
    document = {
        "schema_version": VERSION,
        "generated_at": datetime.now(UTC).isoformat(),
        "configuration": configuration,
        "timing_environment": test_cost.environment(),
        "scope": "partial" if request.only or request.list_only else "full",
        "completeness": "complete"
        if all(s["completeness"] == "complete" for s in scopes)
        else "incomplete",
        "scopes": scopes,
        "summary": summary(scopes),
        "duration_ms": round((monotonic() - started) * 1000, 3),
        "duration_complete": not interrupted
        and all(
            scope.get("exit_code") not in (2, 124, 137, 143)
            and not any(
                case["status"] in ("interrupted", "cancelled", "not_run") for case in scope["tests"]
            )
            for scope in scopes
        ),
    }
    validate(document)
    atomic_write(
        directory / "report.json", json.dumps(document, ensure_ascii=False, indent=2) + "\n"
    )
    failed = any(
        s["completeness"] != "complete"
        or s.get("errors")
        or s.get("exit_code") not in (None, 0, 5)
        or any(
            c["status"] in ("failed", "blocked", "interrupted", "cancelled")
            or (not request.list_only and c["status"] == "not_run")
            for c in s["tests"]
        )
        or any(not c["ok"] for c in s.get("coverage", []))
        for s in scopes
    )
    code = 130 if interrupted else 2 if unmatched else 3 if blocked else int(failed)
    if display:
        display.result(document, code)
        display.close()
    atomic_write(directory / "report.html", render(document))
    if request.list_only:
        for scope in scopes:
            for case in scope["tests"]:
                runner.out.info(case["id"])
    if document["duration_ms"] > test_cost.QUICK_TARGET_MS:
        runner.out.warn(
            "Test execution exceeded the three-minute quick-feedback target; "
            "review scope before repeating. Test results determine success."
        )
    runner.out.info("Report: " + str(directory / "report.html"))
    return {
        "summary": document["summary"],
        "historical_duration": history,
        "json": str(directory / "report.json"),
        "html": str(directory / "report.html"),
    }, code
