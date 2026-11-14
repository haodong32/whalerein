"""Versioned benchmark facts: one attempt outcome each, timings and run status.

The container produces this document; the host validates it before it is
published, and the terminal summary is rendered from the same facts. Validation
is strict so a malformed export can never be presented as a measurement.
"""

from __future__ import annotations

import json
import math
from collections import Counter
from pathlib import Path

from ..errors import RepoError

SCHEMA_VERSION = 1
SCENARIO_ID = "streaming-reply"
SCENARIO_VERSION = 1
OBSERVATION_SURFACE = "cli-stdout"

# One terminal outcome per measured attempt. A reply that misses the deadline,
# carries wrong content or settles twice is never a successful completion.
OUTCOMES = (
    "completed",
    "product_error",
    "timeout_before_text",
    "timeout_after_text",
    "late_reply",
    "incorrect_content",
    "duplicate_final",
)
FAILURE_OUTCOMES = tuple(item for item in OUTCOMES if item != "completed")

STATUSES = ("complete", "incomplete", "invalid")
ANALYSIS_STATES = ("complete", "incomplete")
CLEANUP_RESULTS = ("verified", "unverified")
PHASES = ("warmup", "measured")
MILESTONES = ("first_text_ms", "final_ms")
ATTEMPT_TIMINGS = ("first_text_ms", "final_ms", "send_exit_ms")
# A tail is only reported when its sample floor is met. Above the floor the
# nearest-rank estimate is exact for the observed samples; below it the estimate
# is labelled unavailable rather than presented as evidence.
TAILS = (("median", 1), ("p95", 20), ("p99", 100))
TAIL_MINIMUM = dict(TAILS)
QUANTILE_FRACTIONS = {"median": 0.50, "p95": 0.95, "p99": 0.99}
UNAVAILABLE_TAIL = "insufficient_samples"


def invalid(message):
    raise RepoError("invalid benchmark export: " + message)


def number(value):
    return type(value) in (int, float) and math.isfinite(value) and value >= 0


def count(value):
    return type(value) is int and value >= 0


def quantiles(observed):
    """Exact nearest-rank percentiles for sorted samples."""
    if not observed:
        return {}
    size = len(observed)
    values = {}
    for name, fraction in QUANTILE_FRACTIONS.items():
        values[name] = observed[max(1, math.ceil(fraction * size)) - 1]
    return values


def validate(document):
    if not isinstance(document, dict):
        invalid("document is not an object")
    if document.get("schema_version") != SCHEMA_VERSION:
        invalid("unsupported schema_version")
    for key in ("scenario", "run", "environment", "attempts", "outcomes", "latency", "status"):
        if key not in document:
            invalid("missing section " + key)
    _scenario(document["scenario"])
    _run(document["run"])
    _environment(document["environment"])
    attempts = _attempts(document["attempts"])
    _outcomes(document["outcomes"], attempts)
    _latency(document["latency"], attempts)
    _status(document["status"])
    return document


def _scenario(scenario):
    if not isinstance(scenario, dict):
        invalid("scenario is not an object")
    if scenario.get("id") != SCENARIO_ID:
        invalid("unknown scenario identity")
    if scenario.get("version") != SCENARIO_VERSION:
        invalid("unknown scenario version")
    for key in ("seed", "deadline_ms", "observation_window_ms", "response", "workload"):
        if key not in scenario:
            invalid("scenario is missing " + key)
    if not number(scenario["deadline_ms"]) or scenario["deadline_ms"] <= 0:
        invalid("deadline_ms must be a positive number")
    if not number(scenario["observation_window_ms"]):
        invalid("observation_window_ms must be a number")
    workload = scenario["workload"]
    if not isinstance(workload, dict):
        invalid("scenario workload is not an object")
    for key in ("updates", "gap_ms"):
        if key not in workload:
            invalid("scenario workload is missing " + key)
    updates = workload["updates"]
    if (
        not isinstance(updates, list)
        or not updates
        or not all(isinstance(part, str) for part in updates)
    ):
        invalid("scenario workload updates must be a nonempty list of text")
    if not number(workload["gap_ms"]):
        invalid("scenario workload gap_ms must be a number")
    if workload.get("delivery") not in ("accepted", "immediate"):
        invalid("scenario workload delivery must be accepted or immediate")
    _response(scenario["response"], scenario["deadline_ms"], workload)


def _response(response, deadline_ms, workload):
    if not isinstance(response, dict):
        invalid("scenario response is not an object")
    for key in ("start_ms", "final_ms", "delay_ms", "duplicate_final_ms", "final_text"):
        if key not in response:
            invalid("scenario response is missing " + key)
    for key in ("start_ms", "final_ms", "delay_ms", "duplicate_final_ms"):
        if not number(response[key]):
            invalid("scenario response " + key + " must be a number")
    if not isinstance(response["final_text"], str) or not response["final_text"]:
        invalid("scenario response final_text must be text")
    updates = workload["updates"]
    last_update = response["start_ms"] + workload["gap_ms"] * (len(updates) - 1)
    if response["final_ms"] < last_update:
        invalid("scenario response final_ms precedes its last update")
    scripted_settle = response["final_ms"] + response["delay_ms"]
    if scripted_settle >= deadline_ms:
        invalid("scripted settlement must precede the deadline")
    expected_final = "".join(updates)
    if response["final_text"] != expected_final:
        invalid("scenario response final_text must equal the scripted updates")


def _run(run):
    if not isinstance(run, dict):
        invalid("run is not an object")
    for key in ("id", "started_at", "surface"):
        if not isinstance(run.get(key), str) or not run[key]:
            invalid("run is missing text " + key)
    if run["surface"] != OBSERVATION_SURFACE:
        invalid("unknown observation surface")
    configuration = run.get("configuration")
    if not isinstance(configuration, dict):
        invalid("run configuration is not an object")
    for key in ("attempts", "warmup_attempts", "concurrency", "profile"):
        if key not in configuration:
            invalid("run configuration is missing " + key)
    if configuration["concurrency"] != 1:
        invalid("only sequential execution is supported")
    if not count(configuration["attempts"]) or configuration["attempts"] < 1:
        invalid("run configuration attempts must be a positive integer")
    if not count(configuration["warmup_attempts"]):
        invalid("run configuration warmup_attempts must be a non-negative integer")
    if not isinstance(configuration["profile"], str) or not configuration["profile"]:
        invalid("run configuration profile must be text")


def _environment(environment):
    if not isinstance(environment, dict):
        invalid("environment is not an object")
    for key in ("artifacts", "topology", "limits", "versions", "fixture"):
        if not isinstance(environment.get(key), dict):
            invalid("environment " + key + " is not an object")
    artifacts = environment["artifacts"]
    if not artifacts or not all(
        isinstance(key, str) and isinstance(value, str) and value
        for key, value in artifacts.items()
    ):
        invalid("environment artifacts must map names to identifiers")


def _attempts(attempts):
    if not isinstance(attempts, list) or not attempts:
        invalid("attempts must be a nonempty list")
    identities = set()
    for attempt in attempts:
        if not isinstance(attempt, dict):
            invalid("attempt is not an object")
        for key in ("id", "outcome", "phase"):
            if not isinstance(attempt.get(key), str):
                invalid("attempt is missing text " + key)
        if not attempt["id"] or attempt["id"] in identities:
            invalid("attempt identity is missing or duplicated")
        identities.add(attempt["id"])
        if attempt["outcome"] not in OUTCOMES:
            invalid("unknown attempt outcome")
        if attempt["phase"] not in PHASES:
            invalid("unknown attempt phase")
        for key in ("session_id", "host_id"):
            if key in attempt and not isinstance(attempt[key], str):
                invalid("attempt " + key + " must be text")
        _attempt(attempt)
    return attempts


def _attempt(attempt):
    for key in ATTEMPT_TIMINGS:
        value = attempt.get(key)
        if value is not None and not number(value):
            invalid("attempt timing " + key + " must be a number or null")
    gaps = attempt.get("update_gaps_ms")
    if gaps is not None and (not isinstance(gaps, list) or any(not number(gap) for gap in gaps)):
        invalid("attempt update_gaps_ms must be a list of numbers")
    if attempt.get("update_count") is not None and not count(attempt["update_count"]):
        invalid("attempt update_count must be a non-negative integer")
    duplicates = attempt.get("duplicate_finals")
    if duplicates is not None and not count(duplicates):
        invalid("attempt duplicate_finals must be a non-negative integer")
    for key in ("first_text_ms", "final_ms"):
        value = attempt.get(key)
        if value is not None and value > attempt.get("deadline_ms", math.inf):
            invalid("attempt " + key + " exceeds its deadline")
    first, final = attempt.get("first_text_ms"), attempt.get("final_ms")
    if first is not None and final is not None and final < first:
        invalid("attempt final_ms precedes first_text_ms")
    if attempt["outcome"] == "completed" and final is None:
        invalid("a completed attempt requires final_ms")
    if attempt["outcome"] in ("timeout_after_text", "incorrect_content") and first is None:
        invalid(attempt["outcome"] + " requires an observed first text")
    if attempt["outcome"] == "timeout_before_text" and first is not None:
        invalid("timeout_before_text cannot record a first text")
    # A late reply records the first text it did see but never a final reply time:
    # a past-deadline reply is a timeout, not a completion.
    if attempt["outcome"] == "late_reply" and final is not None:
        invalid("late_reply cannot record a final reply time")
    if attempt["outcome"] in ("product_error", "duplicate_final") and final is not None:
        invalid(attempt["outcome"] + " cannot record a final reply")


def _measured(attempts):
    return [attempt for attempt in attempts if attempt["phase"] == "measured"]


def _outcomes(outcomes, attempts):
    if not isinstance(outcomes, dict):
        invalid("outcomes is not an object")
    measured = _measured(attempts)
    counts = Counter(attempt["outcome"] for attempt in measured)
    expected = {name: counts.get(name, 0) for name in OUTCOMES}
    if outcomes.get("counts") != expected:
        invalid("outcome counts disagree with measured attempts")
    if outcomes.get("measured") != len(measured):
        invalid("measured attempt count disagrees with attempts")
    if outcomes.get("correct_by_deadline") != expected["completed"]:
        invalid("correct_by_deadline disagrees with completed attempts")
    share = outcomes.get("success_share")
    expected_share = expected["completed"] / len(measured) if measured else None
    if expected_share is None:
        if share is not None:
            invalid("success_share must be null without measured attempts")
    elif not number(share) or abs(float(share) - expected_share) > 1e-9:
        invalid("success_share disagrees with outcome counts")
    warmup = outcomes.get("warmup")
    if not isinstance(warmup, dict):
        invalid("outcomes warmup is not an object")
    warmup_attempts = [attempt for attempt in attempts if attempt["phase"] == "warmup"]
    expected_warmup = {
        "attempts": len(warmup_attempts),
        "completed": sum(attempt["outcome"] == "completed" for attempt in warmup_attempts),
    }
    if warmup != expected_warmup:
        invalid("warmup counts disagree with warmup attempts")
    return outcomes


def _latency(latency, attempts):
    if not isinstance(latency, dict):
        invalid("latency is not an object")
    samples = latency.get("samples")
    if not isinstance(samples, dict):
        invalid("latency samples is not an object")
    completed = [attempt for attempt in _measured(attempts) if attempt["outcome"] == "completed"]
    for milestone in MILESTONES:
        entry = samples.get(milestone)
        if not isinstance(entry, dict):
            invalid("latency samples for " + milestone + " is not an object")
        observed = sorted(
            float(attempt[milestone]) for attempt in completed if attempt.get(milestone) is not None
        )
        if entry.get("count") != len(observed):
            invalid("latency sample count for " + milestone + " disagrees with attempts")
        _quantile_entry(entry, observed)
    gaps = samples.get("update_gaps_ms")
    if not isinstance(gaps, dict):
        invalid("latency update gap samples is not an object")
    observed_gaps = sorted(
        float(gap) for attempt in completed for gap in (attempt.get("update_gaps_ms") or [])
    )
    if gaps.get("count") != len(observed_gaps):
        invalid("latency update gap count disagrees with attempts")
    _quantile_entry(gaps, observed_gaps)
    _analysis(latency.get("analysis"))


def _quantile_entry(entry, observed):
    decision = entry.get("quantiles")
    if not isinstance(decision, dict):
        invalid("latency quantiles is not an object")
    unavailable = entry.get("unavailable")
    if unavailable is None:
        unavailable = {}
    if not isinstance(unavailable, dict) or any(
        not isinstance(key, str) or value != UNAVAILABLE_TAIL for key, value in unavailable.items()
    ):
        invalid("latency unavailable tails need a stated reason")
    if not observed:
        if entry.get("reason") not in (None, "no_samples"):
            invalid("empty samples require the no_samples reason")
        if decision or unavailable:
            invalid("latency quantiles must be empty without samples")
        return
    expected = quantiles(observed)
    count = len(observed)
    for name, minimum in TAILS:
        if count >= minimum:
            if name not in decision:
                invalid("missing latency quantile " + name)
            value = decision[name]
            if not number(value) or abs(float(value) - expected[name]) > 1e-6:
                invalid("latency quantile " + name + " disagrees with samples")
            if name in unavailable:
                invalid("an available latency tail cannot also be unavailable")
        else:
            if name in decision:
                invalid("latency quantile " + name + " needs at least " + str(minimum) + " samples")
            if unavailable.get(name) != UNAVAILABLE_TAIL:
                invalid("latency quantile " + name + " must be labelled unavailable")
    extra = (set(decision) | set(unavailable)) - {name for name, _ in TAILS}
    if extra:
        invalid("unexpected latency tail")


def _analysis(analysis):
    if analysis is None:
        return
    if not isinstance(analysis, dict) or analysis.get("state") not in ANALYSIS_STATES:
        invalid("analysis needs a state")
    for key in ("checks", "reasons"):
        items = analysis.get(key)
        if not isinstance(items, list):
            invalid("analysis " + key + " must be a list")
        if any(
            not isinstance(item, dict) or not isinstance(item.get("name"), str) for item in items
        ):
            invalid("analysis " + key + " is malformed")
    if analysis["state"] != "complete" and not analysis["reasons"]:
        invalid("incomplete analysis must state its reasons")


def _status(status):
    if not isinstance(status, dict) or status.get("state") not in STATUSES:
        invalid("status needs a state")
    reasons = status.get("reasons")
    if not isinstance(reasons, list) or any(not isinstance(item, str) for item in reasons):
        invalid("status reasons must be text")
    if status["state"] != "complete" and not reasons:
        invalid("incomplete or invalid status must state its reasons")
    cleanup = status.get("cleanup")
    if not isinstance(cleanup, dict) or cleanup.get("result") not in CLEANUP_RESULTS:
        invalid("cleanup needs a result")
    if cleanup["result"] != "verified" and not isinstance(cleanup.get("reason"), str):
        invalid("unverified cleanup must state a reason")


def read(path):
    try:
        document = json.loads(Path(path).read_text())
    except (OSError, ValueError) as exc:
        raise RepoError("invalid benchmark export: unreadable document") from exc
    try:
        return validate(document)
    except RepoError:
        raise
    except Exception as exc:  # noqa: BLE001 - any structural surprise is an invalid export
        raise RepoError("invalid benchmark export: malformed document") from exc
