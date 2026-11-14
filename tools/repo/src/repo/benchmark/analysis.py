"""Predeclared comparison protocol: paired run-level differences and 95% intervals.

Each matched pair contributes one difference per metric. The interval is a
two-sided t interval over the paired differences, so the resampling unit is the
matched run pair rather than an individual attempt. Classification only uses
the predeclared tolerance; overlapping boundaries and insufficient samples stay
inconclusive.

A run may carry an `analysis` section that applies the same rules to its own
attempt samples. That only decides whether the run meets the declared minimums;
it never compares releases.
"""

from __future__ import annotations

import math

# Declared once, before measuring candidates. A/A pilot evidence sets the
# sample floors; the tolerances are the smallest change worth reporting.
DECLARATION = {
    "primary_metrics": ("first_text_ms", "final_ms"),
    "tolerance_ms": {"first_text_ms": 25.0, "final_ms": 25.0},
    "sample_floor": {"attempts_per_run": 20, "run_pairs": 5},
    "repeats": 24,
    "confidence": 0.95,
    "correctness_floor": 0.95,
    "statistic": "paired mean difference, two-sided t interval",
}

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
    """Latency samples for one run: completed attempts only."""
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
    samples = run.get("latency", {}).get("samples", {})
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
        entry = samples.get(metric) or {}
        tail = (entry.get("quantiles") or {}).get("p95")
        ok = tail is not None
        checks.append(
            {
                "name": metric + "_p95_available",
                "required": DECLARATION["sample_floor"]["attempts_per_run"],
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


def compare(baseline, candidate):
    """Compare matched runs.

    Each argument is a sequence of runs, and each run is that run's list of
    measured attempt facts. Only correctly completed attempts contribute
    latency samples, and the matched run pair is the resampling unit.
    """
    pair_count = min(len(baseline), len(candidate))
    result = {
        "pairs": pair_count,
        "required_pairs": DECLARATION["sample_floor"]["run_pairs"],
        "metrics": {},
        "verdict": "inconclusive",
        "reasons": [],
    }
    if pair_count < DECLARATION["sample_floor"]["run_pairs"]:
        result["reasons"].append("insufficient_pairs")
    for metric in DECLARATION["primary_metrics"]:
        differences = []
        for index in range(pair_count):
            before = mean(samples(baseline[index], metric))
            after = mean(samples(candidate[index], metric))
            if before is None or after is None:
                continue
            differences.append(after - before)
        estimate, low, high, error = paired(differences)
        verdict, reason = classify(estimate, low, high, tolerance(metric))
        if pair_count < DECLARATION["sample_floor"]["run_pairs"]:
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
