"""Run the benchmark in one owned container and publish validated facts.

The integration runner owns image building and container lifecycle; this
runner reuses that lifecycle for a separate benchmark entry point inside the
same image. It consumes only the versioned `benchmark.json` export, validates
it, records cleanup, and writes the report atomically.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path
from time import monotonic

from ..diagnostics import sanitize
from ..errors import RepoError, ToolMissingError, UsageError
from ..reports.model import atomic_write
from ..runners.integration import _container, prepare
from ..sources import SourceError, SourceSelection, capture_sources
from ..verification import (
    Invocation,
    RunContext,
    VerificationError,
    interrupt_on_termination,
    run_lock,
)
from . import model

DEFAULT_EXECUTION_TIMEOUT_SECONDS = 900

EXIT_OK = 0
EXIT_FAILED_ATTEMPTS = 1
EXIT_USAGE = 2
EXIT_INTERRUPTED = 130

# The streaming scenario emits this many visible updates; the container entry
# point owns the same list. Kept here so a cadence/deadline combination that
# cannot settle before the deadline fails as a usage error instead of wasting a
# container run.
SCENARIO_UPDATES = 5


@dataclass(frozen=True)
class BenchRequest:
    attempts: int = 30
    warmup: int = 3
    delay_ms: int = 0
    cadence_ms: int = 100
    deadline_ms: int = 5000
    observation_window_ms: int = 1000
    seed: int = 0
    concurrency: int = 1
    timeout: int | None = None

    def validate(self):
        if not 1 <= self.attempts <= 10000:
            raise UsageError("--attempts must be between 1 and 10000")
        if not 0 <= self.warmup <= 10000:
            raise UsageError("--warmup must be between 0 and 10000")
        if not 0 <= self.delay_ms <= 60000:
            raise UsageError("--delay must be between 0 and 60000 milliseconds")
        if not 10 <= self.cadence_ms <= 60000:
            raise UsageError("--cadence must be between 10 and 60000 milliseconds")
        if not 100 <= self.deadline_ms <= 600000:
            raise UsageError("--deadline must be between 100 and 600000 milliseconds")
        # The scripted reply settles one cadence step after the last update, so a
        # slow cadence against a short deadline can never be observed in time.
        if self.cadence_ms * SCENARIO_UPDATES >= self.deadline_ms:
            raise UsageError(
                f"--cadence {self.cadence_ms} ms needs a deadline above "
                f"{self.cadence_ms * SCENARIO_UPDATES} ms to observe the scripted reply"
            )
        if not 0 <= self.observation_window_ms <= 60000:
            raise UsageError("--observation-window must be between 0 and 60000 milliseconds")
        if self.concurrency != 1:
            raise UsageError("--concurrency must be 1; sequential execution is the only scenario")
        if self.timeout is not None and self.timeout <= 0:
            raise UsageError("--benchmark-timeout must be a positive number of seconds")

    def as_configuration(self):
        return {
            "attempts": self.attempts,
            "warmup_attempts": self.warmup,
            "concurrency": self.concurrency,
            "profile": "warm-client",
            "delay_ms": self.delay_ms,
            "cadence_ms": self.cadence_ms,
            "deadline_ms": self.deadline_ms,
            "observation_window_ms": self.observation_window_ms,
            "seed": self.seed,
        }


@dataclass
class BenchResult:
    document: dict
    exit_code: int
    path: Path | None
    errors: list = field(default_factory=list)


def run(root, runner, request, directory):
    """Execute one benchmark run; never raises for measurement or cleanup faults."""
    directory = Path(directory).resolve()
    request.validate()
    try:
        directory.mkdir(mode=0o700, parents=True)
    except FileExistsError as exc:
        raise UsageError("output directory already exists; choose a new directory") from exc
    with run_lock(directory), interrupt_on_termination():
        return _run_in_directory(root, runner, request, directory)


def _run_in_directory(root, runner, request, directory):
    export = directory / "container"
    export.mkdir()
    timeout = request.timeout or DEFAULT_EXECUTION_TIMEOUT_SECONDS
    errors = []
    exit_code = EXIT_FAILED_ATTEMPTS
    document = None
    cleanup = {"result": "unverified", "reason": "container_not_launched"}
    interrupted = False
    started = monotonic()
    context = RunContext(
        Invocation("benchmark", "protocol-http", (SourceSelection("public", root),), timeout=3600),
        directory,
    )
    try:
        with context.phase("source-capture"):
            context.sources = capture_sources(context.invocation.sources, directory / "sources")
        image, workspace = prepare(context, runner, "protocol-http")
        container = _container(context, runner, workspace, image, "benchmark")
        container.create(
            _container_options(workspace, export, timeout, request),
            [
                "timeout",
                "--signal=INT",
                "--kill-after=10s",
                f"{timeout}s",
                "python3",
                "tests/integration/benchmark/main.py",
            ],
        )
        result = container.execute()
        exit_code = result.returncode
        if exit_code == 130:
            raise KeyboardInterrupt
        if exit_code == 124:
            errors.append(f"Benchmark execution timed out after {timeout} seconds")
        elif exit_code not in (0, EXIT_FAILED_ATTEMPTS):
            errors.append("Benchmark entry point exited " + str(exit_code))
    except KeyboardInterrupt:
        interrupted = True
        exit_code = EXIT_INTERRUPTED
        errors.append("Benchmark run was interrupted")
        runner.interrupted = True
    except ToolMissingError:
        raise
    except UsageError:
        raise
    except (VerificationError, SourceError) as exc:
        errors.append(str(exc))
        if isinstance(exc, VerificationError) and exc.exit_code == 3:
            raise ToolMissingError(str(exc)) from exc
    except (OSError, ValueError) as exc:
        errors.append(sanitize(exc))
    finally:
        context.cleanup()
        if context.cleanups:
            cleanup = (
                {"result": "unverified", "reason": "container_cleanup_not_verified"}
                if context.cleanup_errors
                else {"result": "verified", "reason": None}
            )
        document = _load(export / "benchmark.json", errors, interrupted)
    if document is None:
        document = _placeholder(request, errors)
        exit_code = exit_code if interrupted else EXIT_FAILED_ATTEMPTS
    else:
        document["status"]["cleanup"] = cleanup
        if cleanup["result"] != "verified":
            document["status"]["reasons"] = [
                *document["status"].get("reasons", []),
                "cleanup_unverified",
            ]
            document["status"]["state"] = "invalid"
            errors.append("Could not verify owned container cleanup")
        if errors:
            document["status"]["reasons"] = [
                *document["status"].get("reasons", []),
                *[entry for entry in errors if entry not in document["status"].get("reasons", [])],
            ]
        if errors and not interrupted:
            document["status"]["state"] = "invalid"
        document["duration_ms"] = round((monotonic() - started) * 1000, 3)
    path = None
    try:
        atomic_write(
            directory / "benchmark.json",
            json.dumps(document, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        )
        path = directory / "benchmark.json"
    except OSError as exc:
        errors.append(sanitize(exc))
    if interrupted:
        exit_code = EXIT_INTERRUPTED
    elif errors and exit_code == EXIT_OK:
        exit_code = EXIT_FAILED_ATTEMPTS
    elif document["outcomes"]["counts"].get("completed") != document["outcomes"].get("measured"):
        exit_code = EXIT_FAILED_ATTEMPTS
    if document["status"]["state"] != "complete":
        exit_code = exit_code or EXIT_FAILED_ATTEMPTS
    # The command contract is 0/1/130; a native container status is not a
    # user-facing exit code.
    if exit_code not in (EXIT_OK, EXIT_FAILED_ATTEMPTS, EXIT_INTERRUPTED):
        exit_code = EXIT_FAILED_ATTEMPTS
    return BenchResult(document, exit_code, path, errors)


def _container_options(workspace, export, timeout, request):
    return [
        "--network",
        "none",
        "--tmpfs",
        "/tmp:rw,exec,size=1024m,mode=1777",
        "--env",
        "WHALEREIN_PROGRESS=events",
        "--env",
        f"WHALEREIN_BENCHMARK_TIMEOUT={timeout}",
        "--env",
        "WHALEREIN_BENCHMARK_CONFIG="
        + json.dumps(request.as_configuration(), separators=(",", ":")),
        "--mount",
        f"type=bind,src={workspace},dst=/workspace,readonly",
        "--mount",
        f"type=bind,src={export},dst=/reports",
    ]


def _load(path, errors, interrupted):
    if not path.exists():
        errors.append("Missing benchmark export")
        return None
    try:
        document = model.read(path)
    except RepoError as exc:
        errors.append(str(exc))
        return None
    if interrupted and document["status"]["state"] == "complete":
        document["status"]["state"] = "incomplete"
        document["status"]["reasons"] = ["run_interrupted"]
    return document


def _placeholder(request, errors):
    """Facts that make an unsuccessful run explicit without inventing attempts."""
    return {
        "schema_version": model.SCHEMA_VERSION,
        "scenario": {
            "id": model.SCENARIO_ID,
            "version": model.SCENARIO_VERSION,
            "seed": str(request.seed),
            "deadline_ms": request.deadline_ms,
            "observation_window_ms": request.observation_window_ms,
            "workload": {"updates": ["placeholder"], "gap_ms": 0, "delivery": "immediate"},
            "response": {
                "start_ms": 0,
                "final_ms": 0,
                "delay_ms": 0,
                "duplicate_final_ms": 0,
                "final_text": "placeholder",
            },
        },
        "run": {
            "id": "unavailable",
            "started_at": datetime.now(UTC).isoformat(),
            "surface": model.OBSERVATION_SURFACE,
            "configuration": request.as_configuration(),
        },
        "environment": {
            "artifacts": {"benchmark": "unavailable"},
            "topology": {},
            "limits": {},
            "versions": {},
            "fixture": {},
        },
        "attempts": [
            {
                "id": "not-run",
                "phase": "measured",
                "outcome": "product_error",
                "reason": "run_not_executed",
            }
        ],
        "outcomes": {
            "counts": {name: int(name == "product_error") for name in model.OUTCOMES},
            "measured": 1,
            "correct_by_deadline": 0,
            "success_share": 0.0,
            "warmup": {"attempts": 0, "completed": 0},
        },
        "latency": {
            "samples": {
                "first_text_ms": {"count": 0, "reason": "no_samples", "quantiles": {}},
                "final_ms": {"count": 0, "reason": "no_samples", "quantiles": {}},
                "update_gaps_ms": {"count": 0, "reason": "no_samples", "quantiles": {}},
            },
            "tails": [{"name": "p95", "state": "unavailable", "reason": "run_not_executed"}],
        },
        "status": {
            "state": "invalid",
            "reasons": errors or ["run_not_executed"],
            "cleanup": {"result": "unverified", "reason": "run_not_executed"},
        },
    }
