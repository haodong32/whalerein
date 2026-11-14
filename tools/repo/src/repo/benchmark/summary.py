"""Render the terminal summary from the validated facts, never recomputing them."""

from __future__ import annotations

from . import analysis, model


def render(document):
    lines = [
        f"Benchmark: {document['scenario']['id']} v{document['scenario']['version']}"
        f" (surface {document['run']['surface']})",
        f"Run: {document['run']['id']}  scenario seed {document['scenario']['seed']}"
        f"  profile {document['run']['configuration']['profile']}",
    ]
    artifacts = document["environment"].get("artifacts", {})
    if artifacts:
        lines.append(
            "Artifacts: "
            + ", ".join(f"{name}={value}" for name, value in sorted(artifacts.items()))
        )
    outcomes = document["outcomes"]
    counts = outcomes["counts"]
    total = outcomes["measured"]
    share = outcomes["success_share"]
    fraction = "n/a" if share is None else f"{share * 100:.1f}%"
    lines.append(
        f"Measured attempts: {total}  correct by deadline: "
        f"{counts.get('completed', 0)} ({fraction})"
    )
    failures = [f"{name}={counts[name]}" for name in model.FAILURE_OUTCOMES if counts.get(name)]
    lines.append("Failures: " + (", ".join(failures) if failures else "none"))
    warmup = outcomes["warmup"]
    if warmup["attempts"]:
        lines.append(
            f"Warmup (excluded): {warmup['attempts']} attempts, {warmup['completed']} completed"
        )
    lines.extend(_samples(document["latency"]["samples"]))
    for entry in document["latency"].get("analysis", {}).get("reasons", []):
        lines.append(f"analysis: incomplete ({entry['name']})")
    status = document["status"]
    detail = f" ({'; '.join(status['reasons'])})" if status.get("reasons") else ""
    lines.append(f"Status: {status['state']}{detail}")
    cleanup = status["cleanup"]
    lines.append(
        "Cleanup: verified"
        if cleanup["result"] == "verified"
        else f"Cleanup: {cleanup['result']} ({cleanup.get('reason')})"
    )
    if "duration_ms" in document:
        lines.append(f"Duration: {document['duration_ms'] / 1000:.1f}s")
    return "\n".join(lines)


def _samples(samples):
    lines = []
    for metric, label in (
        ("first_text_ms", "first text"),
        ("final_ms", "final reply"),
        ("update_gaps_ms", "update gap"),
    ):
        entry = samples.get(metric) or {}
        quantiles = entry.get("quantiles") or {}
        if not entry.get("count"):
            lines.append(f"{label}: no samples ({entry.get('reason') or 'unavailable'})")
            continue
        parts = [f"n={entry['count']}"]
        unavailable = entry.get("unavailable") or {}
        for name, minimum in model.TAILS:
            value = quantiles.get(name)
            if value is None:
                reason = unavailable.get(name, model.UNAVAILABLE_TAIL)
                parts.append(f"{name}=unavailable(n>={minimum},{reason})")
            else:
                parts.append(f"{name}={value:.1f}ms")
        lines.append(f"{label}: " + " ".join(parts))
    return lines


def comparison(result):
    lines = [f"Comparison: {result['pairs']} matched pair(s), requires {result['required_pairs']}"]
    for metric, entry in sorted(result["metrics"].items()):
        interval = entry["interval_ms"]
        bounds = (
            "insufficient pairs"
            if interval[0] is None
            else f"[{interval[0]:.1f}, {interval[1]:.1f}] ms"
        )
        estimate = entry["estimate_ms"]
        shown = "n/a" if estimate is None else f"{estimate:+.1f} ms"
        lines.append(
            f"{metric}: {shown} {bounds} against +/-{entry['tolerance_ms']:.1f} ms"
            f" -> {entry['verdict']} ({entry['reason']})"
        )
    if result["reasons"]:
        lines.append("Reasons: " + ", ".join(result["reasons"]))
    return "\n".join(lines)


def method():
    declaration = analysis.DECLARATION
    lines = [
        "Comparison protocol (declared before measuring candidates):",
        "  primary metrics: " + ", ".join(declaration["primary_metrics"]),
        "  tolerances: " + ", ".join(f"{k}={v}ms" for k, v in declaration["tolerance_ms"].items()),
        "  sample floor: " + ", ".join(f"{k}={v}" for k, v in declaration["sample_floor"].items()),
        f"  repeats: {declaration['repeats']}  confidence: {declaration['confidence']}",
        f"  statistic: {declaration['statistic']}",
        "  correctness floor: " + str(declaration["correctness_floor"]),
    ]
    return "\n".join(lines)
