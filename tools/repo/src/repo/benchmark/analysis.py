"""Predeclared comparison protocol for benchmark runs and observer-cost budgets.

The model owns the business rows. This module owns the statistics applied to
those rows: paired run-level differences, the frozen observer-cost budget
verdicts and the within-run mixed-load gate.

Every comparison carries the conditions it was computed under. A changed
workload, mode, clock policy or seed makes the comparison inconclusive instead
of silently comparing unlike runs. Only the predeclared tolerance and the
declared one-sided budget decide a verdict; missing samples or a stale budget
stay inconclusive.
"""

from __future__ import annotations

import json
import math
from itertools import pairwise

# Declared once, before measuring candidates. A/A pilot evidence sets the
# sample floors; the tolerances are the smallest change worth reporting.
DECLARATION = {
    "primary_metrics": ("first_text_ms", "final_ms"),
    "tolerance_ms": {"first_text_ms": 25.0, "final_ms": 25.0},
    "sample_floor": {"attempts_per_run": 20, "run_pairs": 5},
    "repeats": 24,
    "confidence": 0.95,
    "correctness_floor": 0.95,
    "statistic": "paired mean difference, one-sided upper t bound",
}

# The mixed-load within-run control. The tolerance is derived from the run's
# own idle-arm A/A spread instead of a fixed allowance: idle attempts see the
# same cadence, deadline, profile and Device/session lifecycle, so their
# successive differences are measurement noise. The reported metrics are
# always published; the gated metrics decide the load-impact verdict.
BULK_DECLARATION = {
    "reported_metrics": (
        "max_display_delay_ms",
        "longest_pending_stall_ms",
        "largest_output_share",
        "final_output_share",
    ),
    "gated_metrics": ("longest_pending_stall_ms", "largest_output_share"),
    "aa_noise_statistic": "max absolute change between successive idle-arm pairs",
    "aa_noise_multiplier": 2.0,
    "minimum_aa_differences": 5,
    "minimum_pairs": 2,
    "direction": "bulk-minus-idle mean paired difference",
    "decision": "the mean bulk-minus-idle difference must not exceed the A/A tolerance",
    "correctness": "every scheduled bulk read completes and verifies its expected bytes",
}

BUDGET_SCHEMA = 1
BUDGET_STATES = ("within", "breach", "inconclusive")
AA_STATES = ("calibrated", "inconclusive")
RELEASE_STATES = ("within", "breach", "inconclusive")

# Two-sided 95% Student t critical values for small degrees of freedom.
_T95 = {
    1: 12.706,
    2: 4.303,
    3: 3.182,
    4: 2.776,
    5: 2.571,
    6: 2.447,
    7: 2.365,
    8: 2.306,
    9: 2.262,
    10: 2.228,
    11: 2.201,
    12: 2.179,
    13: 2.160,
    14: 2.145,
    15: 2.131,
    16: 2.120,
    17: 2.110,
    18: 2.101,
    19: 2.093,
    20: 2.086,
    21: 2.080,
    22: 2.074,
    23: 2.069,
    24: 2.064,
    25: 2.060,
    26: 2.056,
    27: 2.052,
    28: 2.048,
    29: 2.045,
    30: 2.042,
}
INFERENCE = ("regression", "improvement", "within_tolerance", "inconclusive")


def samples(attempts, metric):
    """Latency samples for one run: correctly completed attempts only."""
    if metric == "update_gaps_ms":
        return [
            float(gap)
            for item in attempts
            if item.get("outcome") == "completed"
            for gap in (item.get("update_gaps_ms") or [])
        ]
    return [
        float(item[metric])
        for item in attempts
        if item.get("outcome") == "completed" and item.get(metric) is not None
    ]


def tolerance(metric):
    return DECLARATION["tolerance_ms"].get(metric, 25.0)


def t_critical(df):
    if df <= 0:
        return None
    return _T95.get(df, 1.96)


def mean(values):
    return sum(values) / len(values) if values else None


def paired(values):
    """Mean paired difference and its two-sided 95% interval."""
    size = len(values)
    centre = mean(values)
    if size < 2:
        return centre, None, None, None
    variance = sum((value - centre) ** 2 for value in values) / (size - 1)
    standard_error = math.sqrt(variance / size)
    critical = t_critical(size - 1)
    half = critical * standard_error
    return centre, centre - half, centre + half, standard_error


def paired_upper_bound(values):
    """One-sided 95% upper bound for the mean paired difference."""
    estimate = mean(values)
    size = len(values)
    if estimate is None:
        return None, None, None
    if size < 2:
        return estimate, None, None
    variance = sum((value - estimate) ** 2 for value in values) / (size - 1)
    standard_error = math.sqrt(variance / size)
    critical = t_critical(size - 1)
    return estimate, estimate + critical * standard_error, standard_error


def classify(estimate, low, high, limit):
    """Classify a difference interval against a predeclared tolerance."""
    if estimate is None or low is None or high is None:
        return "inconclusive", "insufficient_pairs"
    if high < -limit:
        return "improvement", "interval_beyond_lower_tolerance"
    if low > limit:
        return "regression", "interval_beyond_upper_tolerance"
    if low >= -limit and high <= limit:
        return "within_tolerance", "interval_within_tolerance"
    return "inconclusive", "interval_crosses_tolerance"


def run_checks(run):
    """Per-run checks against the declared minimums, without inventing evidence."""
    samples_by_name = run.get("latency", {}).get("samples", {})
    outcomes = run.get("outcomes", {})
    floor = DECLARATION["sample_floor"]
    checks = []
    reasons = []
    count = outcomes.get("measured") or 0
    enough = count >= floor["attempts_per_run"]
    checks.append(
        {
            "name": "attempts_per_run",
            "required": floor["attempts_per_run"],
            "observed": count,
            "ok": enough,
        }
    )
    if not enough:
        reasons.append("attempts_below_declared_floor")
    share = outcomes.get("success_share")
    correct = share is not None and share >= DECLARATION["correctness_floor"]
    checks.append(
        {
            "name": "correctness_floor",
            "required": DECLARATION["correctness_floor"],
            "observed": share,
            "ok": correct,
        }
    )
    if not correct:
        reasons.append("correctness_below_declared_floor")
    for metric in DECLARATION["primary_metrics"]:
        entry = samples_by_name.get(metric) or {}
        tail = (entry.get("quantiles") or {}).get("p95")
        ok = tail is not None
        checks.append(
            {
                "name": metric + "_p95_available",
                "required": floor["attempts_per_run"],
                "observed": entry.get("count") or 0,
                "ok": ok,
            }
        )
        if not ok:
            reasons.append(metric + "_p95_unavailable")
    return {
        "state": "complete" if not reasons else "incomplete",
        "method": DECLARATION["statistic"],
        "checks": checks,
        "reasons": [{"name": reason} for reason in reasons],
    }


def compare(baseline, candidate, declaration=None):
    """Compare matched runs.

    Each argument is a sequence of runs, and each run is that run's list of
    measured attempt facts. Only correctly completed attempts contribute
    latency samples, and the matched run pair is the resampling unit.
    """
    declaration = declaration or DECLARATION
    pair_count = min(len(baseline), len(candidate))
    result = {
        "pairs": pair_count,
        "required_pairs": declaration["sample_floor"]["run_pairs"],
        "metrics": {},
        "verdict": "inconclusive",
        "reasons": [],
    }
    if pair_count < declaration["sample_floor"]["run_pairs"]:
        result["reasons"].append("insufficient_pairs")
    for metric in declaration["primary_metrics"]:
        differences = []
        for index in range(pair_count):
            before = mean(samples(baseline[index], metric))
            after = mean(samples(candidate[index], metric))
            if before is None or after is None:
                continue
            differences.append(after - before)
        estimate, low, high, error = paired(differences)
        verdict, reason = classify(estimate, low, high, tolerance(metric))
        if pair_count < declaration["sample_floor"]["run_pairs"]:
            verdict, reason = "inconclusive", "insufficient_pairs"
        result["metrics"][metric] = {
            "estimate_ms": estimate,
            "interval_ms": [low, high],
            "standard_error_ms": error,
            "tolerance_ms": tolerance(metric),
            "verdict": verdict,
            "reason": reason,
        }
        if verdict not in INFERENCE:
            result["reasons"].append(metric + ":" + reason)
    return result


def _canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), default=str)


def conditions(document):
    """The run conditions that must match before two runs are compared."""
    run = document.get("run", document)
    environment = document.get("environment", {})
    configuration = dict(run.get("configuration") or {})
    configuration.pop("observation_mode", None)
    fingerprint = run.get("fingerprint") or {}
    schemas = fingerprint.get("schemas") or {}
    clock = environment.get("clock") or fingerprint.get("clock") or {}
    topology = environment.get("topology") or {}
    return {
        "configuration": configuration,
        "schemas": schemas,
        "clock": {"source": clock.get("source"), "unit": clock.get("unit")},
        "topology": {"kind": topology.get("kind"), "clock": topology.get("clock")},
        "fingerprint": {
            "fixture": fingerprint.get("fixture"),
            "platform": fingerprint.get("platform"),
        },
    }


def compare_conditions(baseline, candidate):
    """Return the condition keys that differ between two case documents."""
    before = conditions(baseline)
    after = conditions(candidate)
    mismatches = []
    for key in ("configuration", "schemas", "clock", "topology", "fingerprint"):
        if _canonical(before[key]) != _canonical(after[key]):
            mismatches.append(key)
    return {"match": not mismatches, "mismatches": mismatches}


def identity(document):
    """Stable provenance identity for a case or run document."""
    run = document.get("run", document)
    fingerprint = run.get("fingerprint") or {}
    build = fingerprint.get("build") or {}
    fixture = fingerprint.get("fixture") or run.get("fixture") or {}
    return {
        "run_id": run.get("id"),
        "revision": build.get("revision"),
        "fingerprint": build.get("fingerprint"),
        "fixture": fixture.get("fingerprint") if isinstance(fixture, dict) else fixture,
        "observation_mode": run.get("observation_mode"),
    }


def compare_runs(baseline, candidate, declaration=None):
    """Compare matched current-version case documents, preserving provenance."""
    declaration = declaration or DECLARATION
    result = {
        "pairs": min(len(baseline), len(candidate)),
        "required_pairs": declaration["sample_floor"]["run_pairs"],
        "provenance": {
            "baseline": [identity(item) for item in baseline],
            "candidate": [identity(item) for item in candidate],
        },
        "baseline": identity(baseline[0]) if baseline else None,
        "conditions": {"match": None, "mismatches": []},
        "clock_policy": conditions(baseline[0])["clock"] if baseline else None,
        "metrics": {},
        "verdict": "inconclusive",
        "reasons": [],
    }
    if not baseline or not candidate:
        result["reasons"].append("missing_run")
        return result
    result["conditions"] = compare_conditions(baseline[0], candidate[0])
    if not result["conditions"]["match"]:
        result["reasons"].append("conditions_mismatch")
    baseline_attempts = [
        item.get("derived", {}).get("attempts", []) for item in baseline
    ]
    candidate_attempts = [
        item.get("derived", {}).get("attempts", []) for item in candidate
    ]
    compared = compare(baseline_attempts, candidate_attempts, declaration)
    result["pairs"] = compared["pairs"]
    result["metrics"] = compared["metrics"]
    result["reasons"].extend(compared["reasons"])
    if result["reasons"]:
        result["verdict"] = "inconclusive"
    elif any(entry["verdict"] == "regression" for entry in result["metrics"].values()):
        result["verdict"] = "failed"
    elif all(
        entry["verdict"] in ("within_tolerance", "improvement")
        for entry in result["metrics"].values()
    ):
        result["verdict"] = "complete"
    else:
        result["verdict"] = "inconclusive"
    return result


def budget_declaration(
    metrics,
    *,
    revision,
    sample_floor=20,
    run_pairs=5,
    conditions_required=None,
    expires_at=None,
    scope="mode",
):
    """Freeze a versioned observer-cost budget before candidate validation.

    `metrics` maps a metric name to an upper bound or a dict with
    `upper_bound`, `unit`, `sample_floor`, `coverage_floor` and `scope`.
    """
    declared = {}
    for name, value in metrics.items():
        if isinstance(value, dict):
            entry = dict(value)
        else:
            entry = {"upper_bound": value}
        entry.setdefault("unit", "ms")
        entry.setdefault("direction", "upper")
        entry.setdefault("sample_floor", sample_floor)
        entry.setdefault("coverage_floor", 0.0)
        entry.setdefault("scope", scope)
        if not isinstance(entry["upper_bound"], (int, float)):
            raise ValueError("budget upper_bound must be numeric")
        declared[name] = entry
    return {
        "schema": BUDGET_SCHEMA,
        "revision": revision,
        "metrics": declared,
        "sample_floor": sample_floor,
        "run_pairs": run_pairs,
        "confidence": DECLARATION["confidence"],
        "conditions": conditions_required,
        "expires_at": expires_at,
        "scope": scope,
        "direction": "one-sided upper bound on candidate-minus-baseline",
    }


def validate_budget(budget, *, now=None, current_conditions=None):
    """Validate a frozen budget and detect stale declarations."""
    if not isinstance(budget, dict) or budget.get("schema") != BUDGET_SCHEMA:
        raise ValueError("unsupported budget declaration")
    if not isinstance(budget.get("revision"), str) or not budget["revision"]:
        raise ValueError("budget revision is required")
    metrics = budget.get("metrics")
    if not isinstance(metrics, dict) or not metrics:
        raise ValueError("budget metrics are required")
    for name, entry in metrics.items():
        if not isinstance(name, str) or not name:
            raise ValueError("budget metric names must be text")
        if not isinstance(entry, dict) or not isinstance(
            entry.get("upper_bound"), (int, float)
        ):
            raise ValueError("budget metric " + name + " needs a numeric upper bound")
        if not isinstance(entry.get("sample_floor", budget.get("sample_floor")), int):
            raise ValueError("budget metric " + name + " needs an integer sample floor")
    if expires_at := budget.get("expires_at"):
        if now is not None and str(now) > str(expires_at):
            return {"state": "stale", "reasons": ["budget_expired"]}
    required = budget.get("conditions")
    if required is not None and current_conditions is not None:
        if _canonical(required) != _canonical(current_conditions):
            return {"state": "stale", "reasons": ["conditions_changed"]}
    return {"state": "current", "reasons": []}


def evaluate_budget(
    budget,
    metric_samples,
    *,
    now=None,
    current_conditions=None,
):
    """Apply a frozen budget to matched, per-metric paired evidence.

    `metric_samples` maps each budgeted metric to `pairs` (baseline/candidate
    values), `baseline_successes`/`candidate_successes` and optional
    `coverage` fractions. Every verdict identifies its budget revision.
    """
    validity = validate_budget(budget, now=now, current_conditions=current_conditions)
    result = {
        "revision": budget.get("revision"),
        "state": "inconclusive",
        "metrics": {},
        "reasons": [],
        "validity": validity,
    }
    if validity["state"] != "current":
        result["reasons"].extend(validity["reasons"])
    for name, declared in budget["metrics"].items():
        evidence = metric_samples.get(name) or {}
        pairs = [
            (item.get("baseline"), item.get("candidate"))
            for item in evidence.get("pairs", [])
        ]
        usable = [
            (before, after)
            for before, after in pairs
            if isinstance(before, (int, float)) and isinstance(after, (int, float))
        ]
        differences = [after - before for before, after in usable]
        required_pairs = max(budget.get("run_pairs", 0), declared.get("run_pairs", 0))
        required_samples = max(
            budget.get("sample_floor", 0), declared.get("sample_floor", 0)
        )
        coverage_floor = declared.get("coverage_floor", 0.0)
        coverage = evidence.get("coverage") or {}
        entry = {
            "revision": budget.get("revision"),
            "unit": declared.get("unit"),
            "upper_bound": declared.get("upper_bound"),
            "direction": "upper",
            "scope": declared.get("scope"),
            "required_pairs": required_pairs,
            "required_samples": required_samples,
            "pairs": len(usable),
            "estimate": None,
            "upper_bound_observed": None,
            "verdict": "inconclusive",
            "reason": None,
        }
        if validity["state"] != "current":
            entry["reason"] = validity["reasons"][0]
        elif len(usable) < required_pairs:
            entry["reason"] = "insufficient_pairs"
        elif (
            evidence.get("baseline_successes", required_samples) < required_samples
            or evidence.get("candidate_successes", required_samples) < required_samples
        ):
            entry["reason"] = "insufficient_successes"
        elif coverage and min(
            coverage.get("baseline", 1.0), coverage.get("candidate", 1.0)
        ) < coverage_floor:
            entry["reason"] = "insufficient_coverage"
        elif len(differences) < 2:
            entry["reason"] = "insufficient_pairs"
        else:
            estimate, upper, error = paired_upper_bound(differences)
            entry["estimate"] = estimate
            entry["upper_bound_observed"] = upper
            entry["standard_error"] = error
            if upper is None:
                entry["reason"] = "insufficient_pairs"
            elif upper > declared["upper_bound"]:
                entry["verdict"] = "breach"
                entry["reason"] = "upper_bound_exceeds_budget"
            else:
                entry["verdict"] = "within"
                entry["reason"] = "upper_bound_within_budget"
        result["metrics"][name] = entry
        if entry["verdict"] == "inconclusive" and entry["reason"] not in result["reasons"]:
            result["reasons"].append(entry["reason"])
        if entry["verdict"] == "breach" and "budget_breach" not in result["reasons"]:
            result["reasons"].append("budget_breach")
    verdicts = [entry["verdict"] for entry in result["metrics"].values()]
    if "breach" in verdicts:
        result["state"] = "breach"
    elif any(verdict == "inconclusive" for verdict in verdicts):
        result["state"] = "inconclusive"
    else:
        result["state"] = "within"
    return result


def aa_comparison(pairs, *, tolerance_ms, required_pairs=5):
    """Same-build A/A calibration: neither side may differ beyond the noise bound."""
    usable = [
        (item.get("baseline"), item.get("candidate"))
        for item in pairs
        if isinstance(item.get("baseline"), (int, float))
        and isinstance(item.get("candidate"), (int, float))
    ]
    result = {
        "pairs": len(usable),
        "required_pairs": required_pairs,
        "tolerance_ms": tolerance_ms,
        "mean_difference_ms": None,
        "state": "inconclusive",
        "reason": None,
    }
    if len(usable) < required_pairs:
        result["reason"] = "insufficient_pairs"
        return result
    differences = [after - before for before, after in usable]
    result["mean_difference_ms"] = mean(differences)
    if abs(result["mean_difference_ms"]) <= tolerance_ms:
        result["state"] = "calibrated"
        result["reason"] = "within_noise_bound"
    else:
        result["reason"] = "noise_bound_exceeded"
    return result


def release_comparison(baseline_identity, candidate_identity, budget, metric_samples):
    """Compare the frozen predecessor off-run against the candidate off-run."""
    result = {
        "baseline": baseline_identity,
        "candidate": candidate_identity,
        "state": "inconclusive",
        "reasons": [],
        "budget": budget.get("revision") if isinstance(budget, dict) else None,
        "metrics": {},
    }
    if not baseline_identity or not baseline_identity.get("revision"):
        result["reasons"].append("unmatched_predecessor")
        return result
    if baseline_identity.get("revision") == (candidate_identity or {}).get("revision"):
        result["reasons"].append("baseline_is_not_a_predecessor")
        return result
    evaluated = evaluate_budget(budget, metric_samples)
    result.update(
        {
            "state": evaluated["state"],
            "metrics": evaluated["metrics"],
            "reasons": evaluated["reasons"],
            "budget": evaluated["revision"],
            "validity": evaluated["validity"],
        }
    )
    return result


def aa_noise(values):
    """A/A noise proxy: absolute change between successive idle observations."""
    return [abs(later - earlier) for earlier, later in pairwise(values)]


def paired_arm_comparison(pairs):
    """Within-run bulk-minus-idle deltas with an A/A-derived non-regression gate.

    `pairs` carries completed idle/bulk metric values per pair identity. The
    tolerance is twice the largest successive idle-arm change, which is the
    noise the same run exhibited without bulk reads. Too few idle pairs leave
    the gate incomplete instead of silently passing it.
    """
    metrics = BULK_DECLARATION["gated_metrics"]
    reported = BULK_DECLARATION["reported_metrics"]
    results = []
    for pair in pairs:
        values = pair.get("values")
        if values is None:
            delta = {metric: None for metric in reported}
            reason = pair.get("reason") or "incomplete_pair"
        else:
            idle, bulk = values["idle"], values["bulk"]
            delta = {metric: round(bulk[metric] - idle[metric], 6) for metric in reported}
            reason = None
        results.append({"pair_id": pair["pair_id"], "deltas": delta, "reason": reason})

    idle_series = {
        metric: [pair["values"]["idle"][metric] for pair in pairs if pair.get("values") is not None]
        for metric in metrics
    }
    gate_metrics = {}
    checks = []
    for metric in metrics:
        changes = aa_noise(idle_series[metric])
        tolerance = (
            BULK_DECLARATION["aa_noise_multiplier"] * max(changes)
            if len(changes) >= BULK_DECLARATION["minimum_aa_differences"]
            else None
        )
        deltas = [
            entry["deltas"][metric] for entry in results if entry["deltas"][metric] is not None
        ]
        if tolerance is None:
            verdict, reason = "inconclusive", "insufficient_aa_noise"
            mean_delta = max_delta = None
        elif len(deltas) < BULK_DECLARATION["minimum_pairs"]:
            verdict, reason = "inconclusive", "insufficient_pairs"
            mean_delta = mean(deltas)
            max_delta = max(deltas) if deltas else None
        else:
            mean_delta = mean(deltas)
            max_delta = max(deltas)
            if mean_delta <= tolerance:
                verdict, reason = "within_tolerance", "bulk_difference_within_aa_noise"
            else:
                verdict, reason = "regression", "bulk_difference_exceeds_aa_noise"
        gate_metrics[metric] = {
            "aa_noise": max(changes) if changes else None,
            "aa_differences": len(changes),
            "tolerance": tolerance,
            "mean_delta": mean_delta,
            "max_delta": max_delta,
            "verdict": verdict,
            "reason": reason,
        }
        checks.append(
            {
                "name": metric + "_within_tolerance",
                "required_aa_differences": BULK_DECLARATION["minimum_aa_differences"],
                "observed_aa_differences": len(changes),
                "ok": verdict == "within_tolerance",
            }
        )
    reasons = []
    for entry in gate_metrics.values():
        if entry["verdict"] == "regression" and "bulk_degradation" not in reasons:
            reasons.append("bulk_degradation")
        if entry["verdict"] == "inconclusive" and entry["reason"] not in reasons:
            reasons.append(entry["reason"])
    if any(entry["verdict"] == "regression" for entry in gate_metrics.values()):
        state = "failed"
    elif any(entry["verdict"] == "inconclusive" for entry in gate_metrics.values()):
        state = "incomplete"
    else:
        state = "complete"
    return {
        "pairs": results,
        "gate": {
            "state": state,
            "metrics": gate_metrics,
            "checks": checks,
            "reasons": reasons,
        },
    }


def bulk_read_gate(bulk):
    """The bulk arm's correctness gate: full count and full verified bytes."""
    expected = bulk.get("expected_reads") or 0
    successful = bulk.get("successful_reads") or 0
    expected_bytes = bulk.get("bytes_expected") or 0
    verified = bulk.get("bytes_verified") or 0
    errors = bulk.get("errors") or {}
    reasons = []
    if expected < 1:
        reasons.append("no_bulk_reads")
    if successful != expected:
        reasons.append("bulk_reads_not_successful")
    if verified != expected_bytes:
        reasons.append("bulk_bytes_not_verified")
    if errors:
        reasons.append("bulk_read_errors")
    return {
        "state": "complete" if not reasons else "incomplete",
        "expected_reads": expected,
        "successful_reads": successful,
        "bytes_expected": expected_bytes,
        "bytes_verified": verified,
        "errors": errors,
        "reasons": reasons,
    }