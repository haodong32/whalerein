"""Single test execution flow; reports are values, not command dispatch state."""

from __future__ import annotations

import json
from dataclasses import asdict, dataclass
from datetime import UTC, datetime
from pathlib import Path
from time import monotonic
from uuid import uuid4

from .diagnostics import sanitize
from .errors import RepoError, UsageError
from .reports.html import render
from .reports.model import VERSION, atomic_write, summary, validate, validate_scope
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

    def validate(self, explicit=False):
        if self.suite not in ("unit", "integration", "all"):
            raise UsageError("invalid test suite")
        if explicit and self.suite != "unit":
            raise UsageError("--project requires the unit suite")
        if self.group and self.suite != "integration":
            raise UsageError("--group requires the integration suite")
        if self.parallel is not None:
            if self.suite != "integration":
                raise UsageError("--parallel requires the integration suite")
            if self.list_only:
                raise UsageError("--parallel cannot combine with --list")
            if not 1 <= self.parallel <= 8:
                raise UsageError("--parallel must be between 1 and 8")
        if self.integration_timeout is not None:
            if self.suite not in ("integration", "all"):
                raise UsageError("--integration-timeout requires the integration or all suite")
            if self.integration_timeout <= 0:
                raise UsageError("--integration-timeout must be a positive number of seconds")
        if self.coverage and (
            self.only is not None or self.list_only or self.suite == "integration"
        ):
            raise UsageError("coverage requires unfiltered unit execution")
        if self.only is not None and (not self.only.strip() or self.suite == "all"):
            raise UsageError("--only requires nonempty text and one suite")
        if self.only is not None and self.suite == "unit" and len(self.projects) != 1:
            raise UsageError("unit --only requires one project")


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


def run(root, projects, runner, request, output=None):
    if (
        request.list_only
        and request.suite in ("unit", "all")
        and any(p.kind == "node" for p in projects)
    ):
        raise UsageError("Node static case listing is unsupported; select a different project")
    directory = output or root / ".data/runs" / (
        datetime.now(UTC).strftime("%Y%m%dT%H%M%S") + "-" + uuid4().hex[:8]
    )
    directory = Path(directory).resolve()
    directory.mkdir(parents=True, exist_ok=True)
    if any((directory / name).exists() for name in ("report.json", "report.html")):
        raise UsageError("output directory already contains a report; choose a new directory")
    started = monotonic()
    scopes = []
    interrupted = False
    from .progress import Display

    display = None if request.list_only else Display(verbose=runner.out.verbose)
    runner.progress = display
    try:
        if request.suite in ("unit", "all"):
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
                            runner.out.info(
                                f"Coverage measurement (separate execution): {project.id}"
                            )
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
                scope.update(
                    kind="unit",
                    language=project.kind,
                    path=project.rel,
                    module=project.id,
                    duration_ms=round((monotonic() - began) * 1000, 3),
                )
                scopes.append(scope)
        if request.suite in ("integration", "all"):
            scope = (
                failure_scope("integration", "integration", "Not executed after interruption")
                if interrupted
                else integration.collect(root, runner, request, directory)
            )
            scope["kind"] = "integration"
            scopes.append(scope)
            interrupted |= scope.get("exit_code") == 130
    except BaseException:
        if display:
            display.close()
        raise
    finally:
        runner.progress = None
    if request.only and not any(request.only in c["id"] for s in scopes for c in s["tests"]):
        scopes[0].setdefault("errors", []).append("No test cases match --only")
    configuration = asdict(request)
    configuration["parallel"] = request.parallel or 1
    if request.suite in ("integration", "all"):
        configuration["integration_timeout"] = (
            request.integration_timeout or integration.DEFAULT_EXECUTION_TIMEOUT_SECONDS
        )
    document = {
        "schema_version": VERSION,
        "generated_at": datetime.now(UTC).isoformat(),
        "configuration": configuration,
        "scope": "partial" if request.only or request.group or request.suite != "all" else "full",
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
    if display:
        display.result(document, 130 if interrupted else int(failed))
        display.close()
    atomic_write(directory / "report.html", render(document))
    if request.list_only:
        for scope in scopes:
            for case in scope["tests"]:
                runner.out.info(case["id"])
    runner.out.info("Report: " + str(directory / "report.html"))
    return {
        "summary": document["summary"],
        "json": str(directory / "report.json"),
        "html": str(directory / "report.html"),
    }, 130 if interrupted else int(failed)
