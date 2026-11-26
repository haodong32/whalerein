"""Raw case-facts builders.

Every value produced here is an observation. Classification, durations,
percentiles, exemplar selection and verdicts are derived by
``repo.benchmark.model`` and never computed in the fixture.

Attempt observation keeps both representations agreed with the model owner:

* ``started_ms`` / ``ended_ms`` / ``cutoff_ms`` are attempt-relative for
  metric derivation and satisfy the model's deadline constraint;
* ``started_ns`` / ``ended_ns`` / ``cutoff_ns`` are absolute
  ``CLOCK_MONOTONIC`` values for trace ranges;
* ``origin`` names the measurement cut and ``origin_ns`` is its absolute base.

``environment.capture_origin_ns`` is the single absolute base for the merged
typed-event capture.
"""

from __future__ import annotations

import json
import os

SCHEMA_VERSION = 2
KIND = "benchmark-case"
SCENARIO_VERSION = 8
SCENARIO_ID = "streaming-reply"
SURFACE = "cli-stdout"
ENDPOINT = "cli_stdout"
EARLY_STOPS = ("none", "deadline", "terminal_window", "error", "interrupted")


def milliseconds(value_ns, origin_ns):
    return round((value_ns - origin_ns) / 1_000_000, 3)


def scenario_controls(scenario):
    """Expose workload controls under the documented ``workload.controls`` key.

    ``model.scenario`` currently spreads the same declarations at the workload
    top level while its raw-fact validator reads ``workload.controls``. The
    fixture mirrors them so the persisted scenario is self-consistent for
    either reader; the values are not duplicated in meaning, only addressable
    through both keys.
    """
    workload = scenario.setdefault("workload", {})
    controls = workload.get("controls")
    if not isinstance(controls, dict):
        controls = {}
    for key in ("history", "attachment", "exposure", "pairing", "outage"):
        if key in workload and key not in controls:
            controls[key] = workload[key]
    if controls:
        workload["controls"] = controls
    return scenario


def attempt_id(slot):
    return f"attempt-{slot:04d}"


def observation_block(
    *,
    started_ns,
    ended_ns,
    cutoff_ns,
    origin_ns,
    origin,
    early_stop="none",
    cut=None,
):
    """Attempt observation in attempt-relative ms and absolute ns.

    All attempt-relative values share the first-emission origin because the
    model requires non-negative times. Workflow-specific cuts that would
    otherwise move the origin (recovery) are named additively in ``cut``.
    """
    if early_stop not in EARLY_STOPS:
        raise ValueError("unknown early stop " + str(early_stop))
    block = {
        "started_ms": milliseconds(started_ns, origin_ns),
        "ended_ms": milliseconds(ended_ns, origin_ns),
        "cutoff_ms": milliseconds(cutoff_ns, origin_ns),
        "started_ns": int(started_ns),
        "ended_ns": int(ended_ns),
        "cutoff_ns": int(cutoff_ns),
        "origin_ns": int(origin_ns),
        "origin": origin,
        "early_stop": early_stop,
    }
    if cut is not None:
        block["cut"] = dict(cut)
    return block


def output_fact(*, at_ms, ordinal, kind, bytes_, prefix_bytes, correct, block=None):
    event = {
        "at_ms": round(float(at_ms), 3),
        "ordinal": int(ordinal),
        "kind": kind,
        "bytes": int(bytes_),
        "prefix_bytes": int(prefix_bytes),
        "correct": bool(correct),
    }
    if block is not None:
        event["block"] = block
    return event


def attempt(
    *,
    slot,
    phase,
    arm,
    pair_id,
    status,
    source=None,
    outputs=None,
    observation=None,
    failure=None,
    setup_errors=None,
    recovery=None,
    reads=None,
    trace=None,
    identity=None,
):
    """One raw attempt fact; missing sections are explicit, never invented."""
    fact = {
        "id": identity or attempt_id(slot),
        "slot": slot,
        "phase": phase,
        "arm": arm,
        "pair_id": pair_id,
        "status": status,
        "failure": failure or {"attribution": None},
        "setup": {"errors": list(setup_errors or [])},
        "source": source or {"times_ms": [], "sizes": []},
        "outputs": list(outputs or []),
        "observation": observation
        or {
            "started_ms": None,
            "ended_ms": None,
            "cutoff_ms": None,
            "early_stop": "none",
        },
    }
    if recovery is not None:
        fact["recovery"] = recovery
    if reads is not None:
        fact["reads"] = reads
    if trace is not None:
        fact["trace"] = trace
    return fact


def not_started(slot):
    """An explicitly unstarted planned slot (reconciles every slot)."""
    return attempt(
        slot=slot["slot"],
        phase=slot["phase"],
        arm=slot["arm"],
        pair_id=slot["pair"],
        status="not_started",
    )


def run_block(*, capture, configuration, started_at, fingerprint):
    return {
        "id": capture.run_id,
        "started_at": started_at,
        "surface": SURFACE,
        "endpoint": ENDPOINT,
        "observation_mode": capture.mode,
        "configuration": configuration,
        "fingerprint": fingerprint,
    }


def environment_block(
    *,
    artifacts,
    limits,
    versions,
    fixture,
    clock,
    capture_origin_ns,
    network_probe=None,
    network_counters=None,
    sampler=None,
    resources=None,
):
    environment = {
        "artifacts": artifacts,
        "topology": {"kind": "single-machine", "clock": "CLOCK_MONOTONIC"},
        "limits": limits,
        "versions": versions,
        "fixture": fixture,
        "clock": clock,
        "capture_origin_ns": int(capture_origin_ns),
    }
    if network_probe is not None:
        environment["network_probe"] = network_probe
    if network_counters is not None:
        environment["network_counters"] = network_counters
    if sampler is not None:
        environment["sampler"] = sampler
    if resources is not None:
        environment["resources"] = resources
    return environment


def trace_block(
    *,
    path=None,
    checksum=None,
    bytes_=None,
    cap_bytes=None,
    complete=False,
    losses=None,
    coverage=None,
    events_path=None,
    outlets=None,
    mode="off",
):
    trace = {
        "mode": mode,
        "path": path,
        "checksum": checksum,
        "bytes": bytes_,
        "cap_bytes": cap_bytes,
        "complete": bool(complete),
        "losses": dict(losses or {}),
        "coverage": dict(coverage or {}),
        "events_path": events_path,
    }
    if outlets is not None:
        trace["outlets"] = outlets
    return trace


def status_block(*, state, reasons=None, cleanup=None):
    return {
        "state": state,
        "reasons": list(reasons or []),
        "cleanup": cleanup or {"result": "verified", "reason": None},
    }


def document(*, run, scenario, environment, schedule, attempts, trace, status):
    return {
        "schema_version": SCHEMA_VERSION,
        "kind": KIND,
        "run": run,
        "scenario": scenario,
        "environment": environment,
        "schedule": schedule,
        "attempts": attempts,
        "trace": trace,
        "status": status,
    }


def publish(path, document_):
    """Atomically publish JSON facts with private permissions.

    Facts known at exit are always written; a publish failure is reported by
    the caller as a cleanup/status reason instead of being swallowed.
    """
    temporary = str(path) + ".tmp"
    directory = os.path.dirname(str(path))
    if directory:
        os.makedirs(directory, exist_ok=True)
    with open(temporary, "w", encoding="utf-8") as handle:
        handle.write(
            json.dumps(document_, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
        )
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(temporary, path)
    _private(path)
    return path


def _private(path):
    # Reports must remain readable by the outer runner that mounts the case
    # directory; the private capture staging (outlet files) stays owner-only.
    try:
        os.chmod(path, 0o644)
    except OSError:
        pass
