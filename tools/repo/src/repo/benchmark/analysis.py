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
    "statistic": "paired mean difference, two-sided t interval",
}

# AC-19's within-run control. The two Watch-primary metrics are gated against
# a tolerance derived from the run's own idle-arm A/A spread instead of a
# fixed allowance: idle attempts see the same cadence, deadline, profile and
# Device/session lifecycle, so their successive differences are measurement
# noise. The diagnostic metrics are reported but do not gate.
BULK_DECLARATION = {
    "reported_metrics": (
        "max_display_delay_ms",
        "longest_output_gap_ms",
        "largest_output_share",
        "final_output_share",
    ),
    "gated_metrics": ("longest_output_gap_ms", "largest_output_share"),
    "aa_noise_statistic": "max absolute change between successive idle-arm pairs",
    "aa_noise_multiplier": 2.0,
    "minimum_aa_differences": 5,
    "minimum_pairs": 2,
    "direction": "bulk-minus-idle mean paired difference",
    "decision": "the mean bulk-minus-idle difference must not exceed the A/A tolerance",
    "correctness": "every scheduled bulk read completes and verifies its expected bytes",
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
