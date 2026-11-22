"""Render the terminal summary from the validated facts, never recomputing them."""

from __future__ import annotations

from statistics import median

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
    version = document["scenario"]["version"]
    if version < 6:
        lines.extend(
            _samples(
                document["latency"]["samples"],
                {
                    "first_text_ms": (
                        "first text" if document["scenario"]["version"] >= 4 else "first preview",
                        "ms",
                    ),
                    "final_ms": ("final reply", "ms"),
                    "update_gaps_ms": ("preview gap", "ms"),
                },
            )
        )
    if "streaming" in document:
        streaming = document["streaming"]
        scenario = document["scenario"]
        if scenario["version"] == 2:
            lines.append(f"Host event completion floor: {scenario['event_ack_floor_ms']} ms")
        elif scenario["version"] >= 5:
            network = scenario["network"]
            lines.append(
                f"Network: {network['profile']} on {network['link']}, "
                f"{network['extra_rtt_ms']} ms extra RTT, "
                f"{network['jitter_ms_per_direction']} ms jitter and "
                f"{network['loss_percent_per_direction']:g}% loss per direction"
            )
            probe = document["environment"].get("network_probe")
            if probe:
                reference = median(probe["reference_ms"])
                lines.append(
                    "Observed extra TCP RTT: "
                    + ", ".join(
                        f"{link} {max(0, median(probe[link + '_ms']) - reference):.1f} ms"
                        for link in ("host", "client")
                    )
                )
            counters = document["environment"].get("network_counters")
            if counters:
                lines.append(
                    f"Netem workload: {counters['packets']} forwarded packets, "
                    f"{counters['dropped']} dropped"
                )
        else:
            requested = scenario["host_broker_extra_rtt_ms"]
            probe = document["environment"].get("network_probe")
            observed = (
                "unavailable" if probe is None else f"{model.observed_extra_rtt(probe):.1f} ms"
            )
            lines.append(
                f"Host–broker extra TCP RTT: {requested} ms requested, {observed} observed"
            )
        metrics = model.STREAMING_METRICS
        if scenario["version"] >= 6:
            metrics = model.PRIMARY_STREAMING_METRICS
        elif scenario["version"] == 5:
            metrics = {**metrics, **model.LINK_STREAMING_METRICS}
        lines.extend(_samples(streaming["samples"], metrics))
        lines.append(
            f"Completed without previews: {streaming['zero_preview']}/{counts['completed']}"
        )
    if version >= 6:
        lines.append("Diagnostics")
        lines.extend(
            _samples(document["latency"]["samples"], {"first_text_ms": ("first text", "ms")})
        )
        if "diagnostics" in document:
            lines.extend(
                _samples(
                    document["diagnostics"],
                    {"segment_display_delay_ms": ("per-segment display delay", "ms")},
                )
            )
    workflow = document.get("workflow_results", {})
    if "history_measured" in workflow:
        lines.append(
            f"Background history reads: {workflow['history_correct']}/"
            f"{workflow['history_measured']} correct by deadline"
        )
        lines.extend(_samples(workflow, {"history_read_ms": ("history read time", "ms")}))
        if errors := workflow.get("history_errors"):
            lines.append(
                "History errors: " + ", ".join(f"{code}={n}" for code, n in errors.items())
            )
    if "catch_up_ms" in workflow:
        lines.extend(_samples(workflow, {"catch_up_ms": ("catch-up after network restore", "ms")}))
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


def _samples(samples, metrics):
    lines = []
    for metric, (label, unit) in metrics.items():
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
                scale = 100 if unit == "%" else 1
                parts.append(f"{name}={value * scale:.1f}{unit}")
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
    if "vary" in result:
        lines.append("Comparison variable: " + result["vary"])
        for arm, count in result["outcomes"].items():
            lines.append(f"{arm} correct: {count['correct']}/{count['measured']}")
    streaming_metrics = {
        **model.STREAMING_METRICS,
        **model.PRIMARY_STREAMING_METRICS,
    }
    for metric, entry in result.get("streaming", {}).items():
        label, unit = streaming_metrics[metric]
        scale = 100 if unit == "%" else 1
        unit = "pp" if unit == "%" else unit
        delta = entry["mean_paired_p95_delta"]
        if delta is None:
            lines.append(f"{label} p95 change: unavailable ({entry['reason']})")
            continue
        low, high = entry["interval_95"]
        lines.append(
            f"{label} p95 change: {delta * scale:+.1f} {unit}"
            f" [{low * scale:+.1f}, {high * scale:+.1f}] {unit} (95% interval)"
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
