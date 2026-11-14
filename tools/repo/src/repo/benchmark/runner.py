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
from uuid import uuid4

from ..diagnostics import sanitize
from ..errors import RepoError, ToolMissingError, UsageError
from ..reports.model import atomic_write
from ..runners.integration import (
    CONTAINER_TIMEOUT_GRACE_SECONDS,
    _build_image,
    _record_verified,
    _Shard,
)
from . import model

DEFAULT_EXECUTION_TIMEOUT_SECONDS = 900
ENTRY_POINT = "benchmark"

EXIT_OK = 0
EXIT_FAILED_ATTEMPTS = 1
EXIT_USAGE = 2
EXIT_INTERRUPTED = 130


@dataclass(frozen=True)
class BenchRequest:
    attempts: int = 30
    warmup: int = 3
    delay_ms: int = 0
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
        if not 100 <= self.deadline_ms <= 600000:
            raise UsageError("--deadline must be between 100 and 600000 milliseconds")
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
    if (directory / "benchmark.json").exists():
        raise UsageError(
            "output directory already contains a benchmark report; choose a new directory"
        )
    directory.mkdir(parents=True, exist_ok=True)
    export = directory / "container"
    export.mkdir()
    container = "whalerein-bench-" + uuid4().hex[:12]
    timeout = request.timeout or DEFAULT_EXECUTION_TIMEOUT_SECONDS
    errors = []
    exit_code = EXIT_FAILED_ATTEMPTS
    document = None
    cleanup = {"result": "unverified", "reason": "container_not_launched"}
    launched = False
    interrupted = False
    started = monotonic()
    try:
        probe = runner.run(["docker", "buildx", "version"], root, timeout=15, quiet=True)
        if not probe.ok:
            raise ToolMissingError("Docker buildx is required for ./repo bench")
        image_errors = []
        image = _build_image(root, runner, directory, {"errors": image_errors})
        if image is None:
            # A rejected build is invalid execution, not a crash: keep the
            # explicit failure and publish no invented attempts.
            errors.extend(image_errors)
            errors.append("Benchmark image build failed")
            exit_code = EXIT_FAILED_ATTEMPTS
        else:
            launched = True
            result = runner.run(
                _container_arguments(container, image, export, timeout, request),
                root,
                quiet=True,
                timeout=timeout + CONTAINER_TIMEOUT_GRACE_SECONDS,
            )
            exit_code = result.returncode
            if result.returncode == 124:
                errors.append(f"Benchmark execution timed out after {timeout} seconds")
            elif result.returncode not in (0, EXIT_FAILED_ATTEMPTS):
                # Exit 1 means "ran, with failed attempts" and the export carries
                # that outcome. Any other status means execution itself failed.
                errors.append("Benchmark entry point exited " + str(result.returncode))
    except KeyboardInterrupt:
        interrupted = True
        exit_code = EXIT_INTERRUPTED
        errors.append("Benchmark run was interrupted")
        runner.interrupted = True
    except ToolMissingError:
        raise
    except UsageError:
        raise
    except (OSError, ValueError) as exc:
        errors.append(sanitize(exc))
    finally:
        if launched:
            cleanup = _stop_and_verify(root, runner, container)
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


def _container_arguments(container, image, export, timeout, request):
    return [
        "docker",
        "run",
        "--init",
        "--sig-proxy=false",
        "--network",
        "none",
        "--tmpfs",
        "/tmp:rw,exec,size=1024m,mode=1777",
        "--name",
        container,
        "--env",
        "WHALEREIN_PROGRESS=events",
        "--env",
        f"WHALEREIN_BENCHMARK_TIMEOUT={timeout}",
        "--env",
        "WHALEREIN_BENCHMARK_CONFIG="
        + json.dumps(request.as_configuration(), separators=(",", ":")),
        "--mount",
        f"type=bind,src={export},dst=/reports",
        image,
        ENTRY_POINT,
    ]


def _stop_and_verify(root, runner, container):
    """Stop only this run's container and verify it is gone."""
    runner.run(
        ["docker", "stop", "--signal", "SIGINT", "--timeout", "10", container],
        root,
        timeout=25,
        quiet=True,
    )
    runner.run(["docker", "rm", "--force", container], root, timeout=30, quiet=True)
    state = _Shard(index=0, export=None, name=container, launched=True)
    _record_verified(runner, root, state, 1)
    if state.verified:
        return {"result": "verified", "reason": None}
    return {"result": "unverified", "reason": "container_cleanup_not_verified"}


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
