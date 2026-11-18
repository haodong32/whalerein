"""Shared lifecycle for environment-backed native verification."""

from __future__ import annotations

import json
import os
import re
import signal
import threading
import time
import uuid
from collections.abc import Callable
from contextlib import contextmanager
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path

from .errors import ToolMissingError
from .reports.html import render
from .reports.model import STATUSES, VERSION, atomic_write, summary, validate_scope
from .sources import CapturedSource, SourceError, SourceSelection, capture_sources
from .test_cost import environment


class VerificationError(Exception):
    def __init__(self, code: str, exit_code: int = 1):
        if not re.fullmatch(r"[a-z][a-z0-9-]{0,79}", code):
            raise ValueError("invalid verification error code")
        if exit_code not in (1, 2, 3, 130):
            raise ValueError("invalid verification status")
        super().__init__(code)
        self.exit_code = exit_code


@dataclass(frozen=True)
class Invocation:
    purpose: str
    suite: str
    sources: tuple[SourceSelection, ...]
    case_filter: str | None = None
    timeout: float = 1800
    output: Path | None = None
    configuration: dict = field(default_factory=dict)


@dataclass(frozen=True)
class RunResult:
    exit_code: int
    report_directory: Path


class RunContext:
    def __init__(self, invocation: Invocation, directory: Path):
        self.run_directory = directory
        self.invocation = invocation
        self.started = time.monotonic()
        self.deadline = self.started + invocation.timeout
        self.sources: dict[str, CapturedSource] = {}
        self.phases = []
        self.scopes = {}
        self.cases = {}
        self.collected = None
        self.artifacts = {}
        self.identities = {}
        self.resources = {}
        self.cleanups = []
        self.cleanup_errors = []
        self.cleanup_outcome = "pending"

    def remaining(self) -> float:
        return _remaining(self.deadline)

    def run(self, runner, argv, cwd, *, quiet=True, **options):
        result = runner.run(argv, cwd, timeout=self.remaining(), quiet=quiet, **options)
        if result.returncode == 130:
            raise KeyboardInterrupt
        if result.missing:
            raise VerificationError("missing-tool", 3)
        return result

    @contextmanager
    def phase(self, name):
        record = {"name": name, "status": "running"}
        self.phases.append(record)
        start = time.monotonic()
        self.checkpoint()
        try:
            self.remaining()
            yield
        except BaseException:
            record["status"] = "failed"
            raise
        else:
            record["status"] = "complete"
        finally:
            record["duration_ms"] = round((time.monotonic() - start) * 1000)
            self.checkpoint()

    def record_collection(self, case_ids):
        self.collected = sorted(case_ids)
        self.checkpoint()

    def record_case(self, case_id, status):
        if status not in STATUSES or not case_id:
            raise ValueError("invalid case observation")
        self.cases[case_id] = {"id": case_id, "status": status}
        self.checkpoint()

    def record_scope(self, scope):
        validate_scope(scope)
        self.scopes[scope["suite_id"]] = scope
        self.checkpoint()

    def record_artifact(self, name, identity):
        self.artifacts[name] = identity
        self.checkpoint()

    def record_identity(self, name, expected, actual):
        self.identities[name] = {"expected": expected, "actual": actual}
        self.checkpoint()
        if expected != actual:
            raise VerificationError("runtime-identity-mismatch")

    def record_resource(self, name, identity):
        """Persist allowlisted resource metadata before and after acquisition."""
        self.resources[name] = identity
        self.checkpoint()

    def register_cleanup(self, name, callback: Callable[[float], None]):
        self.cleanups.append((name, callback))

    def checkpoint(self):
        atomic_write(
            self.run_directory / "checkpoint.json",
            json.dumps({**self.facts(), "observed_cases": list(self.cases.values())}),
        )

    def facts(self):
        return {
            "purpose": self.invocation.purpose,
            "suite": self.invocation.suite,
            "case_filter": self.invocation.case_filter,
            "collected_cases": self.collected,
            "sources": {
                name: {
                    "revision": source.revision,
                    "fingerprint": source.fingerprint,
                    "mode": source.mode,
                    "path": str(source.root.relative_to(self.run_directory)),
                }
                for name, source in self.sources.items()
            },
            "phases": self.phases,
            "last_completed_phase": next(
                (
                    phase["name"]
                    for phase in reversed(self.phases)
                    if phase["status"] == "complete" and phase["name"] != "cleanup"
                ),
                None,
            ),
            "artifacts": self.artifacts,
            "identities": self.identities,
            "resources": self.resources,
            "cleanup": {"status": self.cleanup_outcome, "errors": self.cleanup_errors},
        }

    def cleanup(self):
        deadline = time.monotonic() + 90
        start = time.monotonic()
        for name, callback in reversed(self.cleanups):
            try:
                _remaining(deadline)
                callback(deadline)
            except KeyboardInterrupt:
                self.cleanup_errors.append({"resource": name, "code": "cleanup-interrupted"})
            except Exception as exc:
                code = str(exc) if isinstance(exc, VerificationError) else "cleanup-failed"
                self.cleanup_errors.append({"resource": name, "code": code})
        self.cleanup_outcome = "failed" if self.cleanup_errors else "complete"
        self.phases.append(
            {
                "name": "cleanup",
                "status": self.cleanup_outcome,
                "duration_ms": round((time.monotonic() - start) * 1000),
            }
        )
        self.checkpoint()


def _remaining(deadline):
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise VerificationError("deadline-exceeded")
    return remaining


def _result_scopes(context, error):
    scopes = list(context.scopes.values())
    exported = {case["id"] for scope in scopes for case in scope["tests"]}
    partial = [case for key, case in context.cases.items() if key not in exported]
    if partial or not scopes:
        scopes.append(
            {
                "suite_id": f"{context.invocation.suite}/observations",
                "project": context.invocation.suite,
                "kind": "integration",
                "completeness": "incomplete",
                "tests": partial,
                "errors": [error] if error else [],
            }
        )
    return scopes


@contextmanager
def interrupt_on_termination():
    if threading.current_thread() is not threading.main_thread():
        yield
        return

    def interrupted(signum, frame):
        raise KeyboardInterrupt

    previous = signal.signal(signal.SIGTERM, interrupted)
    try:
        yield
    finally:
        signal.signal(signal.SIGTERM, previous)


@contextmanager
def run_lock(directory: Path):
    """Hold a kernel lease while a run owns or recovers its resources."""
    if not directory.is_dir():
        raise VerificationError("unknown-run-directory", 2)
    with (directory / "owner.lock").open("a+b") as handle:
        try:
            if os.name == "nt":
                import msvcrt

                msvcrt.locking(handle.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl

                fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as exc:
            raise VerificationError("run-in-use", 3) from exc
        yield


def run_suite(invocation: Invocation, run_platform: Callable[[RunContext], None]) -> RunResult:
    root = invocation.sources[0].root if invocation.sources else Path.cwd()
    if invocation.output is None:
        stamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%S")
        directory = root / ".data" / "runs" / f"{stamp}-{uuid.uuid4().hex[:8]}"
    else:
        directory = Path(invocation.output)
    directory = directory.resolve()
    try:
        directory.mkdir(mode=0o700, parents=True)
    except FileExistsError as exc:
        raise VerificationError("output-already-exists", 2) from exc
    with run_lock(directory), interrupt_on_termination():
        return _execute(RunContext(invocation, directory), run_platform)


def _execute(context, run_platform):
    invocation = context.invocation
    directory = context.run_directory
    exit_code, error = 0, None
    try:
        with context.phase("source-capture"):
            context.sources = capture_sources(invocation.sources, directory / "sources")
        run_platform(context)
        if not context.scopes or any(
            scope["completeness"] != "complete" for scope in context.scopes.values()
        ):
            raise VerificationError("missing-native-result")
        if any(
            scope.get("exit_code") not in (0, None)
            or any(case["status"] not in {"passed", "skipped", "todo"} for case in scope["tests"])
            for scope in context.scopes.values()
        ):
            raise VerificationError("native-test-failed")
    except VerificationError as exc:
        exit_code, error = exc.exit_code, str(exc)
    except ToolMissingError:
        exit_code, error = 3, "missing-tool"
    except SourceError as exc:
        exit_code, error = 1, str(exc)
    except KeyboardInterrupt:
        exit_code, error = 130, "interrupted"
    except Exception:
        exit_code, error = 1, "platform-execution-error"
    finally:
        context.cleanup()

    if not exit_code and context.cleanup_errors:
        interrupted = any(item["code"] == "cleanup-interrupted" for item in context.cleanup_errors)
        exit_code, error = (130, "interrupted") if interrupted else (1, "cleanup-failed")
    scopes = _result_scopes(context, error)
    _write_report(
        directory,
        {**invocation.configuration, "suite": invocation.suite},
        scopes,
        {**context.facts(), "exit_code": exit_code, "error_code": error},
        duration_ms=round((time.monotonic() - context.started) * 1000),
        duration_complete=exit_code == 0,
    )
    return RunResult(exit_code, directory)


def _write_report(directory, configuration, scopes, facts, **timing):
    report = {
        "schema_version": VERSION,
        "generated_at": datetime.now(UTC).isoformat(),
        "timing_environment": environment(),
        "configuration": configuration,
        "completeness": (
            "complete"
            if all(scope["completeness"] == "complete" for scope in scopes)
            else "incomplete"
        ),
        "scopes": scopes,
        "summary": summary(scopes),
        "verification": facts,
        **timing,
    }
    atomic_write(directory / "report.json", json.dumps(report, indent=2) + "\n")
    atomic_write(directory / "report.html", render(report))


def recover_run(directory: Path, callbacks: dict) -> dict:
    """Recover a checkpoint only after acquiring the original owner's lease."""
    directory = directory.resolve()
    with run_lock(directory):
        try:
            checkpoint = json.loads((directory / "checkpoint.json").read_text())
            resources = checkpoint["resources"]
            cleanup = checkpoint["cleanup"]
        except (OSError, ValueError, KeyError, TypeError) as exc:
            raise VerificationError("invalid-run-checkpoint", 2) from exc
        if cleanup.get("status") == "complete":
            _finalize_recovery(directory, checkpoint, cleanup)
            return cleanup
        deadline = time.monotonic() + 90
        errors = []
        for name, ownership in reversed(tuple(resources.items())):
            try:
                _remaining(deadline)
                callback = callbacks.get(ownership.get("kind"))
                if callback is None:
                    raise VerificationError("unsupported-resource-recovery")
                callback(name, ownership, deadline)
            except Exception as exc:
                code = str(exc) if isinstance(exc, VerificationError) else "recovery-failed"
                errors.append({"resource": name, "code": code})
        outcome = {"status": "failed" if errors else "complete", "errors": errors}
        atomic_write(directory / "recovery.json", json.dumps(outcome, indent=2) + "\n")
        _finalize_recovery(directory, checkpoint, outcome)
        return outcome


def _finalize_recovery(directory, checkpoint, cleanup):
    suite = checkpoint.get("suite")
    if not suite or (directory / "report.json").exists():
        return
    observations = {case["id"]: case for case in checkpoint.pop("observed_cases", [])}
    for identifier in checkpoint.get("collected_cases") or ():
        observations.setdefault(identifier, {"id": identifier, "status": "not_run"})
    scope = {
        "suite_id": f"{suite}/observations",
        "project": suite,
        "kind": "integration",
        "completeness": "incomplete",
        "tests": list(observations.values()),
        "errors": ["abandoned-run"],
        "exit_code": 1,
    }
    _write_report(
        directory,
        {"suite": suite},
        [scope],
        {**checkpoint, "cleanup": cleanup, "exit_code": 1, "error_code": "abandoned-run"},
        duration_complete=False,
    )
