"""Run the benchmark in one owned container and publish validated facts.

The integration runner owns image building and container lifecycle; this
runner reuses that lifecycle for a separate benchmark entry point inside the
same image. It consumes only the versioned `benchmark.json` export, validates
it, records cleanup, and writes the report atomically.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field, replace
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


@dataclass(frozen=True)
class BenchRequest:
    attempts: int = 30
    warmup: int = 3
    delay_ms: int = 0
    cadence_ms: int = 100
    workflow: str = "watch"
    network_profile: str = "normal"
    link: str = "host"
    deadline_ms: int = 10000
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
        if self.workflow not in model.WORKFLOWS:
            raise UsageError("--scenario must be watch, recovery, or mixed-load")
        if self.workflow == "recovery" and self.link != "client":
            raise UsageError("the recovery scenario uses --link client")
        if self.network_profile not in model.NETWORK_PROFILES:
            raise UsageError("--network must be a named benchmark network profile")
        if self.link not in model.NETWORK_LINKS:
            raise UsageError("--link must be host, client, or both")
        if not 100 <= self.deadline_ms <= 600000:
            raise UsageError("--deadline must be between 100 and 600000 milliseconds")
        # The scripted reply settles one cadence step after the last update, so a
        # slow cadence against a short deadline can never be observed in time.
        final_ms = self.cadence_ms * len(model.UPDATES)
        if final_ms >= self.deadline_ms:
            raise UsageError(
                f"--cadence {self.cadence_ms} ms needs "
                f"a deadline above {final_ms} ms to observe the scripted reply"
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
            "workflow": self.workflow,
            "network": model.network_settings(self.network_profile, self.link),
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


def _create_output(directory):
    directory = Path(directory).resolve()
    try:
        directory.mkdir(mode=0o700, parents=True)
    except FileExistsError as exc:
        raise UsageError("output directory already exists; choose a new directory") from exc
    return directory


def run(root, runner, request, directory, *, prepared=None):
    """Execute one benchmark run; never raises for measurement or cleanup faults."""
    request.validate()
    directory = _create_output(directory)
    with run_lock(directory), interrupt_on_termination():
        return _run_in_directory(root, runner, request, directory, prepared)


def run_matrix(root, runner, request, directory):
    """Run all declared user workloads in fresh containers with one build."""
    request.validate()
    directory = _create_output(directory)
    report = directory / "suite.json"
    cases = [
        {"workflow": workflow, "network": profile, "link": link, "status": "pending"}
        for workflow, profile, link in model.DEFAULT_CASES
    ]
    document = {
        "schema_version": 1,
        "status": "running",
        "attempts_per_case": request.attempts,
        "warmup_per_case": request.warmup,
        "source": None,
        "cases": cases,
        "errors": [],
    }
    with run_lock(directory), interrupt_on_termination():
        return _run_matrix(directory, report, document, root, runner, request)


def _write_matrix(report, document):
    atomic_write(report, json.dumps(document, ensure_ascii=False, indent=2) + "\n")


def _run_matrix(directory, report, document, root, runner, request):
    cases = document["cases"]
    exit_code = EXIT_OK
    context = _context(root, directory, "benchmark-matrix")
    _write_matrix(report, document)
    try:
        try:
            prepared = _prepare(context, runner)
        finally:
            context.cleanup()
        if context.cleanup_errors:
            raise VerificationError("benchmark-build-cleanup-unverified")
        source = context.sources["public"]
        document["source"] = {"revision": source.revision, "fingerprint": source.fingerprint}
        for index, case in enumerate(cases, 1):
            workflow, profile, link = case["workflow"], case["network"], case["link"]
            name = f"{workflow}-{profile}-{link}"
            case["status"] = "running"
            _write_matrix(report, document)
            runner.out.info(f"Benchmark {index}/{len(cases)}: {workflow}, {profile} on {link}")
            scenario = replace(request, workflow=workflow, network_profile=profile, link=link)
            result = run(root, runner, scenario, directory / name, prepared=prepared)
            outcomes = result.document["outcomes"]
            workflow_results = result.document.get("workflow_results", {})
            samples = (result.document.get("streaming") or {}).get("samples", {})
            case.update(
                status=result.document["status"]["state"],
                exit_code=result.exit_code,
                report=str(result.path.relative_to(directory)) if result.path else None,
                measured=outcomes["measured"],
                correct=outcomes["correct_by_deadline"],
                workflow_results=workflow_results,
                streaming_p95={
                    metric: samples.get(metric, {}).get("quantiles", {}).get("p95")
                    for metric in model.PRIMARY_STREAMING_METRICS
                },
            )
            _write_matrix(report, document)
            status = f"{name}: {case['status']}, Watch {case['correct']}/{case['measured']} correct"
            if "history_measured" in workflow_results:
                correct = workflow_results["history_correct"]
                measured = workflow_results["history_measured"]
                status += f", history {correct}/{measured} correct"
            if "bulk" in workflow_results:
                bulk = workflow_results["bulk"]
                status += (
                    f", bulk {bulk['successful_reads']}/{bulk['expected_reads']} reads"
                    f" / {bulk['bytes_verified']}/{bulk['bytes_expected']} bytes"
                )
            runner.out.info(status)
            if result.exit_code == EXIT_INTERRUPTED:
                raise KeyboardInterrupt
            if result.exit_code != EXIT_OK:
                exit_code = EXIT_FAILED_ATTEMPTS
        document["status"] = "complete"
    except KeyboardInterrupt:
        runner.interrupted = True
        exit_code = EXIT_INTERRUPTED
        document["errors"].append("Benchmark suite was interrupted")
    except (ToolMissingError, VerificationError, SourceError, OSError, ValueError) as exc:
        exit_code = (
            3
            if isinstance(exc, ToolMissingError)
            else getattr(exc, "exit_code", EXIT_FAILED_ATTEMPTS)
        )
        document["errors"].append(sanitize(exc))
    if document["status"] == "running":
        document["status"] = "incomplete"
        for case in cases:
            if case["status"] == "running":
                case["status"] = "incomplete"
    _write_matrix(report, document)
    return BenchResult(document, exit_code, report, document["errors"])


def _context(root, directory, purpose="benchmark"):
    return RunContext(
        Invocation(purpose, "protocol-http", (SourceSelection("public", root),), timeout=3600),
        directory,
    )


def _prepare(context, runner):
    with context.phase("source-capture"):
        context.sources = capture_sources(
            context.invocation.sources, context.run_directory / "sources"
        )
    return prepare(context, runner, "protocol-http")


def _run_in_directory(root, runner, request, directory, prepared):
    export = directory / "container"
    export.mkdir()
    timeout = request.timeout or DEFAULT_EXECUTION_TIMEOUT_SECONDS
    errors = []
    exit_code = EXIT_FAILED_ATTEMPTS
    document = None
    cleanup = {"result": "unverified", "reason": "container_not_launched"}
    interrupted = False
    started = monotonic()
    context = _context(root, directory)
    try:
        image, workspace = prepared if prepared is not None else _prepare(context, runner)
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
        "--cap-add",
        "NET_ADMIN",
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
    configuration = request.as_configuration()
    scenario = model.scenario(configuration)
    final_bytes = len(scenario["response"]["final_text"].encode())
    return {
        "schema_version": model.SCHEMA_VERSION,
        "scenario": scenario,
        "run": {
            "id": "unavailable",
            "started_at": datetime.now(UTC).isoformat(),
            "surface": model.OBSERVATION_SURFACE,
            "configuration": configuration,
        },
        "environment": {
            "artifacts": {"benchmark": "unavailable"},
            "topology": {},
            "limits": {},
            "versions": {},
            "fixture": {},
        },
        "attempts": [],
        "outcomes": {
            "counts": {name: 0 for name in model.OUTCOMES},
            "measured": 0,
            "correct_by_deadline": 0,
            "success_share": None,
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
        "streaming": model.streaming_summary(
            [], final_bytes, burst_window_ms=model.BURST_WINDOW_MS, display_delay=True
        ),
        "workflow_results": model.workflow_summary(
            [],
            request.workflow,
            final_bytes=final_bytes,
            sizes=model.source_sizes(scenario),
            paired=scenario["version"] >= 7 and request.workflow == "mixed-load",
            burst_window_ms=model.BURST_WINDOW_MS,
        ),
        "status": {
            "state": "invalid",
            "reasons": errors or ["run_not_executed"],
            "cleanup": {"result": "unverified", "reason": "run_not_executed"},
        },
    }
