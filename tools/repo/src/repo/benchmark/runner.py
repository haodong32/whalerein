"""Run one current-version benchmark case and publish validated facts.

The integration runner owns image building and container lifecycle; this
runner reuses that lifecycle for the benchmark entry point. The container
fixture writes raw case facts plus an optional typed-event sidecar into a
private case directory. This module validates those facts with
`repo.benchmark.model`, optionally exports one native Perfetto trace, and
publishes the report atomically. It never derives business verdicts itself.

Case schema 2, suite schema 2 and workload version 8 are the only accepted
contracts. Retired or future artifacts fail explicitly; there is no fallback
reader.
"""

from __future__ import annotations

import json
import secrets
from dataclasses import dataclass, field, replace
from datetime import UTC, datetime
from pathlib import Path
from time import monotonic
from uuid import uuid4

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
from . import model, trace

DEFAULT_EXECUTION_TIMEOUT_SECONDS = 900
DEFAULT_SCHEDULE_FILE = "schedule.json"
TRACE_FILENAME = "case.pftrace"

EXIT_OK = 0
EXIT_FAILED_ATTEMPTS = 1
EXIT_USAGE = 2
EXIT_INTERRUPTED = 130


@dataclass(frozen=True)
class BenchRequest:
    attempts: int = model.DEFAULT_ATTEMPTS
    warmup: int = model.DEFAULT_WARMUPS
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
    observation: str = "off"
    cpu_profile: bool = False
    p95_scope: str = "pooled"

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
        if self.observation not in model.OBSERVATION_MODES:
            raise UsageError("--observation must be off, aggregate, or trace")
        if self.p95_scope not in ("pooled", "per-arm"):
            raise UsageError("--p95-scope must be pooled or per-arm")
        if self.workflow == "mixed-load":
            # Full idle/bulk pairs are required in every phase. Reject the plan
            # before any build artifact or container exists.
            if self.attempts % 2:
                raise UsageError("mixed load requires an even measured --attempts count")
            if self.warmup % 2:
                raise UsageError("mixed load requires an even --warmup count")
            if self.p95_scope == "per-arm" and self.attempts < 40:
                raise UsageError(
                    "per-arm P95 requires at least 40 measured attempts "
                    "(20 eligible per arm)"
                )
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
            "observation_mode": self.observation,
            "cpu_profile": self.cpu_profile,
            "p95_scope": self.p95_scope,
        }


@dataclass
class BenchResult:
    document: dict
    exit_code: int
    path: Path | None
    errors: list = field(default_factory=list)


def slot_plan(request):
    """The seeded, counterbalanced slot plan persisted before fixture startup.

    This mirrors the fixture's pure planner so the plan written by the runner
    and the plan the fixture persists are identical. It is planning, not
    business classification: the model validates the plan and derives every
    outcome from the attempts that actually started.
    """
    cycle_ms = 15000.0
    slots = []
    ordinal = 0
    for phase in model.PHASES:
        count = request.warmup if phase == "warmup" else request.attempts
        if request.workflow == "mixed-load":
            first = "idle" if model.seed_parity(request.seed) == 0 else "bulk"
            other = "bulk" if first == "idle" else "idle"
            prefix = "warmup" if phase == "warmup" else "pair"
            for index in range(count // 2):
                arms = (first, other) if index % 2 == 0 else (other, first)
                for order, arm in enumerate(arms):
                    slots.append(
                        {
                            "slot": ordinal,
                            "phase": phase,
                            "arm": arm,
                            "pair": f"{prefix}-{index:04d}",
                            "order": order,
                            "planned_start_ms": round(cycle_ms * len(slots), 3),
                        }
                    )
                    ordinal += 1
        else:
            prefix = "warmup" if phase == "warmup" else "slot"
            for index in range(count):
                slots.append(
                    {
                        "slot": ordinal,
                        "phase": phase,
                        "arm": model.ARM_WATCH,
                        "pair": f"{prefix}-{index:04d}",
                        "order": 0,
                        "planned_start_ms": round(cycle_ms * len(slots), 3),
                    }
                )
                ordinal += 1
    return {"seed": str(request.seed), "rule": "alternating", "slots": slots}



def _create_output(directory):
    directory = Path(directory).resolve()
    try:
        directory.mkdir(mode=0o700, parents=True)
    except FileExistsError as exc:
        raise UsageError("output directory already exists; choose a new directory") from exc
    return directory


def _private_case(directory):
    case = Path(directory) / "case"
    case.mkdir(mode=0o700)
    return case


def run(root, runner, request, directory, *, prepared=None):
    """Execute one benchmark run; never raises for measurement or cleanup faults."""
    request.validate()
    directory = _create_output(directory)
    with run_lock(directory), interrupt_on_termination():
        return _run_in_directory(root, runner, request, directory, prepared)


def run_matrix(root, runner, request, directory):
    """Run all declared workloads in fresh containers with one build."""
    request.validate()
    directory = _create_output(directory)
    report = directory / "suite.json"
    cases = _planned_cases(request)
    document = _suite_document(request, cases, status="running")
    with run_lock(directory), interrupt_on_termination():
        return _run_matrix(directory, report, document, root, runner, request)


def _planned_cases(request):
    cases = []
    for index, (workflow, profile, link) in enumerate(model.DEFAULT_CASES):
        name = f"{workflow}-{profile}-{link}"
        cases.append(
            {
                "index": index,
                "case": name,
                "run_id": None,
                "workflow": workflow,
                "network": model.network_settings(profile, link),
                "link": link,
                "path": None,
                "status": "planned",
                "case_schema": model.SCHEMA_VERSION,
                "slot_plan": f"{name}/{DEFAULT_SCHEDULE_FILE}",
                "observation_mode": request.observation,
                "endpoint": model.OBSERVATION_ENDPOINT,
                "capture_quality": None,
                "artifacts": None,
                "exit_code": None,
            }
        )
    return cases


def _suite_document(request, cases, *, status):
    pending = [case["index"] for case in cases if case["status"] in ("planned", "running")]
    if status == "running":
        suite_status = {
            "state": "incomplete",
            "reasons": ["suite_in_progress"],
            "pending": pending,
            "interruption": None,
        }
    elif status == "interrupted":
        suite_status = {
            "state": "interrupted",
            "reasons": ["benchmark_suite_interrupted"],
            "pending": pending,
            "interruption": {"pending": pending},
        }
    elif status == "complete":
        suite_status = {
            "state": "complete",
            "reasons": [],
            "pending": [],
            "interruption": None,
        }
    else:
        suite_status = {
            "state": "incomplete",
            "reasons": ["suite_incomplete"],
            "pending": pending,
            "interruption": None,
        }
    document = model.suite_document(
        [],
        suite_id="benchmark-suite",
        started_at=datetime.now(UTC).isoformat(),
        configuration=request.as_configuration(),
        status=suite_status,
    )
    document["cases"] = cases
    return document


def _write_matrix(report, document):
    model.validate_suite(document)
    atomic_write(report, json.dumps(document, ensure_ascii=False, indent=2) + "\n")


def _run_matrix(directory, report, document, root, runner, request):
    cases = document["cases"]
    exit_code = EXIT_OK
    active = None
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
            name = case["case"]
            workflow, network = case["workflow"], case["network"]
            active = case["index"]
            _write_matrix(report, document)
            runner.out.info(f"Benchmark {index}/{len(cases)}: {workflow}, {network['profile']}")
            scenario = replace(
                request,
                workflow=workflow,
                network_profile=network["profile"],
                link=network["link"],
                # The recovery workload owns the client link; every declared
                # case keeps the observation mode selected for the suite.
            )
            result = run(root, runner, scenario, directory / name, prepared=prepared)
            active = None
            _index_case(case, result, directory)
            if result.exit_code == EXIT_INTERRUPTED:
                case["status"] = "interrupted"
                _write_matrix(report, document)
                raise KeyboardInterrupt
            _write_matrix(report, document)
            runner.out.info(_case_status(case))
            if result.exit_code != EXIT_OK:
                exit_code = EXIT_FAILED_ATTEMPTS
        incomplete = [case["case"] for case in cases if case["status"] != "complete"]
        document["status"] = {
            "state": "complete" if not incomplete else "incomplete",
            "reasons": [] if not incomplete else ["suite_cases_incomplete"],
            "pending": [],
            "interruption": None,
        }
        if incomplete:
            exit_code = exit_code or EXIT_FAILED_ATTEMPTS
    except KeyboardInterrupt:
        runner.interrupted = True
        exit_code = EXIT_INTERRUPTED
        document["status"] = {
            "state": "interrupted",
            "reasons": ["benchmark_suite_interrupted"],
            "pending": [
                case["index"]
                for case in cases
                if case["status"] in ("planned", "interrupted", "incomplete")
            ],
            "interruption": {"reason": "signal"},
        }
        for case in cases:
            if case["index"] == active and case["status"] == "planned":
                case["status"] = "interrupted"
    except (ToolMissingError, VerificationError, SourceError, OSError, ValueError) as exc:
        exit_code = (
            3
            if isinstance(exc, ToolMissingError)
            else getattr(exc, "exit_code", EXIT_FAILED_ATTEMPTS)
        )
        document["status"] = {
            "state": "incomplete",
            "reasons": [sanitize(exc)],
            "pending": [
                case["index"]
                for case in cases
                if case["status"] in ("planned", "interrupted")
            ],
            "interruption": None,
        }
    else:
        unreachable = [case for case in cases if case["status"] not in ("complete", "incomplete")]
        if unreachable:
            document["status"] = {
                "state": "incomplete",
                "reasons": ["suite_cases_not_executed"],
                "pending": [case["index"] for case in unreachable],
                "interruption": None,
            }
            exit_code = exit_code or EXIT_FAILED_ATTEMPTS
    _write_matrix(report, document)
    return BenchResult(document, exit_code, report, list(document["status"]["reasons"]))


def _index_case(case, result, directory):
    document = result.document
    derived = document.get("derived", {})
    quality = derived.get("quality", {})
    trace_metadata = derived.get("trace", {})
    outcomes = derived.get("outcomes", {})
    case.update(
        run_id=document["run"]["id"],
        status="complete" if document["status"]["state"] == "complete" else "incomplete",
        path=f"{case['case']}/benchmark.json",
        outcomes=outcomes,
        correct=outcomes.get("correct_by_deadline"),
        measured=outcomes.get("measured"),
        exit_code=result.exit_code,
        observation_mode=document["run"]["observation_mode"],
        endpoint=document["run"]["endpoint"],
        capture_quality=quality.get("state"),
        artifacts=document["environment"].get("artifacts"),
        trace={
            "path": trace_metadata.get("path"),
            "checksum": trace_metadata.get("checksum"),
            "complete": trace_metadata.get("complete"),
            "losses": trace_metadata.get("losses"),
        }
        if document["run"]["observation_mode"] == "trace"
        else None,
    )


def _case_status(case):
    status = f"{case['case']}: {case['status']}, Watch {case['correct']}/{case['measured']} correct"
    workflow = (case.get("outcomes") or {}).get("workflow")
    if workflow is None:
        return status
    return status


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
    case_dir = _private_case(directory)
    schedule = slot_plan(request)
    _write_json(case_dir / DEFAULT_SCHEDULE_FILE, schedule)
    timeout = request.timeout or DEFAULT_EXECUTION_TIMEOUT_SECONDS
    errors = []
    raw = None
    cleanup = {"result": "unverified", "reason": "container_not_launched"}
    interrupted = False
    container_code = None
    started = monotonic()
    context = _context(root, directory)
    key = secrets.token_hex(32)
    env_file = directory / ".container.env"
    try:
        image, workspace = prepared if prepared is not None else _prepare(context, runner)
        if prepared is None and context.cleanup_errors:
            raise VerificationError("benchmark-build-cleanup-unverified")
        source = context.sources.get("public")
        revision = getattr(source, "revision", None) or "unavailable"
        run_id = "run-" + uuid4().hex
        _write_env(env_file, key)
        container = _container(context, runner, workspace, image, "benchmark")
        container.create(
            _container_options(
                workspace, case_dir, timeout, request, env_file, run_id, revision
            ),
            [
                "timeout",
                "--signal=INT",
                "--kill-after=10s",
                f"{timeout}s",
                "python3",
                "tests/integration/benchmark/main.py",
            ],
        )
        env_file.unlink(missing_ok=True)
        result = container.execute()
        container_code = result.returncode
        if container_code == 130:
            raise KeyboardInterrupt
        if container_code == 124:
            errors.append(f"Benchmark execution timed out after {timeout} seconds")
        elif container_code not in (0, EXIT_FAILED_ATTEMPTS):
            errors.append("Benchmark entry point exited " + str(container_code))
    except KeyboardInterrupt:
        interrupted = True
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
        env_file.unlink(missing_ok=True)
        context.cleanup()
        if context.cleanups:
            cleanup = (
                {"result": "unverified", "reason": "container_cleanup_not_verified"}
                if context.cleanup_errors
                else {"result": "verified", "reason": None}
            )
        raw = _load(case_dir / "benchmark.json", request, case_dir, errors, interrupted)
    return _publish(
        directory,
        raw,
        request,
        errors=errors,
        interrupted=interrupted,
        cleanup=cleanup,
        container_code=container_code,
        started=started,
        schedule=schedule,
    )


def _write_env(path, key):
    # Only the ephemeral capture key needs the private file.
    path.write_text("WHALEREIN_BENCH_CAPTURE_KEY=" + key + "\n")
    path.chmod(0o600)


def _write_json(path, document):
    atomic_write(path, json.dumps(document, ensure_ascii=False, indent=2) + "\n")


def _container_options(
    workspace, case_dir, timeout, request, env_file, run_id, revision
):
    # The ephemeral capture key travels in a 0600 env file, never on argv.
    return [
        "--network",
        "none",
        "--cap-add",
        "NET_ADMIN",
        "--tmpfs",
        "/tmp:rw,exec,size=1024m,mode=1777",
        "--env-file",
        str(env_file),
        "--env",
        "WHALEREIN_PROGRESS=events",
        "--env",
        f"WHALEREIN_BENCHMARK_TIMEOUT={timeout}",
        "--env",
        "WHALEREIN_BENCHMARK_CONFIG="
        + json.dumps(request.as_configuration(), separators=(",", ":")),
        "--env",
        "WHALEREIN_BENCH_MODE=" + request.observation,
        "--env",
        "WHALEREIN_BENCH_OBSERVATION=" + request.observation,
        "--env",
        "WHALEREIN_BENCHMARK_OBSERVATION=" + request.observation,
        "--env",
        "WHALEREIN_BENCH_RUN_ID=" + run_id,
        "--env",
        "WHALEREIN_BENCH_OUTLET=/reports/outlet",
        "--env",
        "WHALEREIN_BENCH_SCHEDULE=/reports/" + DEFAULT_SCHEDULE_FILE,
        "--env",
        "WHALEREIN_BUILD_REVISION=" + revision,
        "--mount",
        f"type=bind,src={workspace},dst=/workspace,readonly",
        "--mount",
        f"type=bind,src={case_dir},dst=/reports",
    ]


def _load(path, request, case_dir, errors, interrupted):
    if not path.exists():
        errors.append("Missing benchmark export")
        return None
    try:
        facts = json.loads(path.read_text())
        model.validate_case(facts)
        if facts["run"]["observation_mode"] != request.observation:
            errors.append("Benchmark export observation mode disagrees with the request")
            return None
        if interrupted and facts["status"]["state"] == "complete":
            facts["status"]["state"] = "incomplete"
            facts["status"]["reasons"] = [
                *facts["status"]["reasons"],
                "run_interrupted",
            ]
        if request.observation == "trace":
            _merge_events(facts, case_dir, errors)
        return facts
    except (OSError, ValueError) as exc:
        errors.append("Invalid benchmark export: " + sanitize(exc))
        return None
    except RepoError as exc:
        errors.append(str(exc))
        return None


def _merge_events(facts, case_dir, errors):
    """Merge the fixture's typed-event sidecar before model validation/export."""
    if facts.get("events") is not None:
        return
    declared = (facts.get("trace") or {}).get("events_path")
    if not declared:
        return
    candidates = [
        case_dir / declared,
        case_dir / Path(declared).name,
        case_dir / "outlet" / Path(declared).name,
    ]
    path = next((item for item in candidates if item.is_file()), None)
    if path is None:
        errors.append("Missing benchmark event sidecar")
        return
    events = []
    try:
        for line in path.read_text().splitlines():
            if not line.strip():
                continue
            event = json.loads(line)
            if not isinstance(event, dict):
                raise ValueError("event row is not an object")
            events.append(event)
    except (OSError, ValueError) as exc:
        errors.append("Invalid benchmark event sidecar: " + sanitize(exc))
        return
    facts["events"] = events


def _publish(
    directory,
    facts,
    request,
    *,
    errors,
    interrupted,
    cleanup,
    container_code,
    started,
    schedule,
):
    if facts is None:
        facts = _placeholder(request, errors, schedule, interrupted)
    facts = dict(facts)
    facts["status"] = dict(facts["status"])
    facts["status"]["cleanup"] = cleanup
    execution = {
        "duration_ms": round((monotonic() - started) * 1000, 3),
        "container_exit_code": container_code,
        "interrupted": interrupted,
        "errors": list(errors),
        "cleanup": cleanup,
        "trace_export": {"state": "not_requested"},
    }
    trace_failed = False
    had_facts = bool(facts)
    if request.observation == "trace" and had_facts and facts["status"]["state"] != "invalid":
        try:
            metadata = _export_trace(directory, facts)
        except Exception as exc:  # noqa: BLE001 - export failure must not drop facts
            trace_failed = True
            errors.append("Benchmark trace export failed: " + sanitize(exc))
            execution["trace_export"] = {
                "state": "failed",
                "reason": sanitize(exc),
            }
        else:
            facts["trace"] = {
                **facts["trace"],
                "path": TRACE_FILENAME,
                "checksum": metadata["checksum"],
                "bytes": metadata["bytes"],
                "complete": True,
            }
            execution["trace_export"] = {
                "state": "complete",
                "path": TRACE_FILENAME,
                "checksum": metadata["checksum"],
                "bytes": metadata["bytes"],
            }
    document = model.case_document(facts)
    document["execution"] = {**execution, "errors": list(errors)}
    path = None
    try:
        _write_json(directory / "benchmark.json", document)
        path = directory / "benchmark.json"
    except OSError as exc:
        errors.append(sanitize(exc))
    outcomes = document["derived"]["outcomes"]
    measured = outcomes["measured"]
    complete = document["status"]["state"] == "complete"
    if interrupted:
        exit_code = EXIT_INTERRUPTED
    elif trace_failed or errors or cleanup["result"] != "verified":
        exit_code = EXIT_FAILED_ATTEMPTS
    elif container_code == EXIT_FAILED_ATTEMPTS:
        exit_code = EXIT_FAILED_ATTEMPTS
    elif not complete:
        exit_code = EXIT_FAILED_ATTEMPTS
    elif measured and outcomes["correct_by_deadline"] != measured:
        exit_code = EXIT_FAILED_ATTEMPTS
    else:
        exit_code = EXIT_OK
    if exit_code not in (EXIT_OK, EXIT_FAILED_ATTEMPTS, EXIT_INTERRUPTED):
        exit_code = EXIT_FAILED_ATTEMPTS
    return BenchResult(document, exit_code, path, errors)


def _export_trace(directory, facts):
    tier = facts["derived"] if "derived" in facts else None
    if tier is None:
        # Derivation is pure; use the same facts the report will publish.
        derived = model.derive(facts)
    else:  # pragma: no cover - defensive for callers that pass a report
        derived = tier
    events = facts.get("events") or []
    origin = facts.get("environment", {}).get("capture_origin_ns")
    exemplars = []
    for exemplar in derived.get("trace_index", {}).get("exemplars", []):
        trace_range = exemplar.get("trace_range")
        if not trace_range:
            continue
        if "start_ns" in trace_range and "end_ns" in trace_range:
            start_ns = int(trace_range["start_ns"])
            end_ns = int(trace_range["end_ns"])
        else:
            if origin is None:
                raise RepoError("trace export requires capture_origin_ns")
            start_ns = int(origin) + int(round(trace_range["start_ms"] * 1_000_000))
            end_ns = int(origin) + int(round(trace_range["end_ms"] * 1_000_000))
        exemplars.append(
            {
                "attempt": exemplar["attempt_id"],
                "metric": exemplar["metric"],
                "reason": exemplar.get("selection"),
                "coverage": (exemplar.get("coverage") or {}).get("complete"),
                "start_ns": start_ns,
                "end_ns": end_ns,
            }
        )
    if events and origin is None:
        raise RepoError("trace export requires capture_origin_ns")
    staging = directory / ".case.pftrace.tmp"
    try:
        metadata = trace.export(
            events,
            exemplars,
            staging,
            origin_ns=int(origin) if origin is not None else None,
            run_id=facts["run"]["id"],
        )
        staging.replace(directory / TRACE_FILENAME)
    finally:
        staging.unlink(missing_ok=True)
    return metadata


def _placeholder(request, errors, schedule, interrupted):
    """Facts that make an unsuccessful run explicit without inventing attempts."""
    configuration = request.as_configuration()
    scenario = model.scenario(configuration)
    state = "incomplete"
    reasons = [sanitize(error) for error in errors] or ["run_not_executed"]
    return {
        "schema_version": model.SCHEMA_VERSION,
        "kind": "benchmark-case",
        "scenario": scenario,
        "run": {
            "id": "benchmark-" + uuid4().hex[:12],
            "started_at": datetime.now(UTC).isoformat(),
            "surface": model.OBSERVATION_SURFACE,
            "endpoint": model.OBSERVATION_ENDPOINT,
            "observation_mode": request.observation,
            "configuration": configuration,
            "fingerprint": {
                "build": {"revision": "unavailable", "fingerprint": "unavailable"},
                "fixture": {"name": "unavailable", "fingerprint": "unavailable"},
                "schemas": {
                    "case": model.SCHEMA_VERSION,
                    "event": 1,
                    "workload": model.SCENARIO_VERSION,
                },
                "platform": {"os": "unavailable", "arch": "unavailable", "kernel": "unavailable"},
                "limits": {"cpus": 0, "memory_bytes": 0},
                "clock": {
                    "source": "CLOCK_MONOTONIC",
                    "unit": "ns",
                    "verified": False,
                },
                "sampling": {"socket_hz": 0, "runtime_hz": 0},
            },
        },
        "environment": {
            "artifacts": {"benchmark": "unavailable"},
            "topology": {"kind": "single-machine", "clock": "CLOCK_MONOTONIC"},
            "limits": {},
            "versions": {},
            "fixture": {"name": "unavailable", "fingerprint": "unavailable"},
            "clock": {
                "source": "CLOCK_MONOTONIC",
                "unit": "ns",
                "boot_id_verified": False,
                "namespace_verified": False,
                "producers": [],
            },
        },
        "schedule": schedule,
        "attempts": [],
        "trace": {"complete": request.observation != "trace"},
        "status": {
            "state": state,
            "reasons": reasons,
            "cleanup": {"result": "unverified", "reason": "run_not_executed"},
        },
    }


def replay(directory, *, output=None):
    """Re-derive a current report from saved facts without any container.

    The published facts must re-derive byte-identically. Nothing in this path
    contacts Docker, the fixture or the network, and the live report is never
    modified.
    """
    source = Path(directory)
    if source.is_dir():
        source = source / "benchmark.json"
    if not source.exists():
        raise UsageError("no benchmark.json at " + str(source))
    report = model.read(source)
    facts = {key: value for key, value in report.items() if key not in ("derived", "execution")}
    derived = model.derive(facts)
    if derived != report["derived"]:
        raise RepoError("invalid benchmark report: offline replay disagrees with saved facts")
    document = {
        **facts,
        "derived": derived,
        "execution": {
            **(report.get("execution") or {}),
            "replay": True,
            "duration_ms": 0,
        },
    }
    path = None
    if output is not None:
        path = Path(output)
        if path.suffix != ".json":
            path = path / "benchmark.json"
        _write_json(path, document)
    outcomes = derived["outcomes"]
    passed = (
        document["status"]["state"] == "complete"
        and outcomes["measured"] > 0
        and outcomes["correct_by_deadline"] == outcomes["measured"]
    )
    return BenchResult(document, EXIT_OK if passed else EXIT_FAILED_ATTEMPTS, path, [])