"""Advisory test scope and comparable local wall-time observations."""

import math
import platform
import statistics
import sys
from dataclasses import asdict

from .errors import RepoError
from .reports.model import read_report

HISTORY_LIMIT = 100
SAMPLE_LIMIT = 5
MAX_REPORT_BYTES = 8 * 1024 * 1024
QUICK_TARGET_MS = 180_000


def environment():
    return {"platform": sys.platform, "machine": platform.machine()}


def selection(configuration):
    """Execution limits do not change selection; parallelism changes wall time."""
    suite = configuration.get("suite", "unit")
    target = configuration.get("group") or suite
    return (
        target,
        tuple(sorted(configuration.get("projects", []))),
        configuration.get("only"),
        configuration.get("coverage", False),
        configuration.get("list_only", False),
        configuration.get("parallel") or 1,
    )


def historical_duration(root, request):
    """Read at most 100 recent reports and use up to five successful peers."""
    samples = []
    try:
        candidates = sorted(
            (root / ".data/runs").glob("*/report.json"),
            key=lambda path: path.stat().st_mtime,
            reverse=True,
        )[:HISTORY_LIMIT]
        wanted = selection(asdict(request))
        for path in candidates:
            try:
                if path.stat().st_size > MAX_REPORT_BYTES:
                    continue
                report = read_report(path)
                if (
                    report.get("timing_environment") != environment()
                    or selection(report["configuration"]) != wanted
                    or report.get("completeness") != "complete"
                    or report.get("duration_complete") is not True
                    or not report["scopes"]
                    or not any(
                        case["status"] == "passed"
                        for scope in report["scopes"]
                        for case in scope["tests"]
                    )
                ):
                    continue
                if any(
                    scope.get("exit_code") != 0
                    or scope.get("errors")
                    or not scope["tests"]
                    or any(case["status"] not in ("passed", "skipped") for case in scope["tests"])
                    or any(not item["ok"] for item in scope.get("coverage", []))
                    for scope in report["scopes"]
                ):
                    continue
                duration = report.get("duration_ms")
                if (
                    type(duration) not in (int, float)
                    or not math.isfinite(duration)
                    or duration <= 0
                ):
                    continue
                samples.append(duration)
                if len(samples) == SAMPLE_LIMIT:
                    break
            except (OSError, RepoError, ValueError, TypeError, KeyError):
                continue
    except OSError:
        pass  # History is optional and must not prevent test execution.
    return {"samples": len(samples), "median_ms": statistics.median(samples) if samples else None}


def announce(root, request, out):
    """Describe the request before discovery, compilation or test execution."""
    scope = (
        f"target: {request.group or 'integration'}"
        if request.suite == "integration"
        else f"unit projects: {', '.join(request.projects)}"
    )
    out.info(f"Test selection: {scope}")
    out.info(
        f"Mode: {'discovery' if request.list_only else 'execution'}; "
        f"case filter={'on' if request.only else 'off'}; "
        f"coverage={'on' if request.coverage else 'off'}; parallel={request.parallel or 1}"
    )
    history = historical_duration(root, request)
    median = history["median_ms"]
    if median is None:
        out.info("Historical wall time: unknown (no comparable complete successful local runs).")
    else:
        out.info(
            f"Historical wall time: median {median / 1000:.1f}s from {history['samples']} local "
            "run(s); compilation/cache state may differ."
        )
        if median > QUICK_TARGET_MS:
            out.warn("Historical duration exceeds the three-minute quick-feedback target.")
    out.info("History is advisory; every selected test will execute.")
    # Agent subprocesses use pipes: publish the notice before native work starts.
    out.stdout.flush()
    out.stderr.flush()
    return history
