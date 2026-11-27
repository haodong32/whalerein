"""Render terminal summaries from model-derived rows, never recomputing them."""

from __future__ import annotations

import statistics

from . import analysis, model

MAX_ATTEMPT_LINES = 8


def render(document):
    derived = document.get("derived") or {}
    if not derived:
        raise ValueError("benchmark report is missing derived rows")
    run = document["run"]
    lines = [
        f"Benchmark: {document['scenario']['id']} v{document['scenario']['version']}"
        f" (surface {run['surface']}, endpoint {run['endpoint']})",
        f"Run: {run['id']}  seed {document['scenario']['seed']}"
        f"  profile {run['configuration']['profile']}"
        f"  observation {run['observation_mode']}",
    ]
    artifacts = document["environment"].get("artifacts", {})
    if artifacts:
        lines.append(
            "Artifacts: "
            + ", ".join(f"{name}={value}" for name, value in sorted(artifacts.items()))
        )
    outcomes = derived["outcomes"]
    populations = derived.get("populations") or {}
    counts = outcomes["counts"]
    share = outcomes["success_share"]
    fraction = "n/a" if share is None else f"{share * 100:.1f}%"
    lines.append(
        f"Measured attempts: {outcomes['measured']}  correct by deadline: "
        f"{counts.get('completed', 0)} ({fraction})"
    )
    failures = [f"{name}={counts[name]}" for name in model.FAILURE_OUTCOMES if counts.get(name)]
    lines.append("Failures: " + (", ".join(failures) if failures else "none"))
    denominators = populations.get("denominators") or {}
    if denominators:
        lines.append(
            "Populations: planned/started/entered/finished "
            f"{_count(populations, 'planned')}/{_count(populations, 'started')}/"
            f"{_count(populations, 'measurement_entered')}/{_count(populations, 'finished')}"
            f"  on-time correct over planned "
            f"{_share(denominators.get('on_time_correct_over_planned'))}"
        )
    setup = populations.get("setup_failures") or {}
    if setup:
        lines.append("Setup failures: " + ", ".join(f"{name}={n}" for name, n in setup.items()))
    warmup = outcomes["warmup"]
    if warmup["attempts"]:
        lines.append(
            f"Warmup (excluded): {warmup['attempts']} attempts, {warmup['completed']} completed"
        )

    latency = derived.get("latency") or {}
    lines.append("Latency")
    lines.extend(
        _samples(
            latency.get("samples") or {},
            {"first_text_ms": ("first text", "ms"), "final_ms": ("terminal", "ms")},
        )
    )
    for arm, entry in (latency.get("arms") or {}).items():
        lines.append(f"  {arm} arm: {entry['measured']} measured")
        lines.extend(
            "  " + line
            for line in _samples(
                entry.get("samples") or {},
                {"first_text_ms": ("first text", "ms"), "final_ms": ("terminal", "ms")},
            )
        )

    streaming = derived.get("streaming") or {}
    if streaming:
        labels = {
            "max_display_delay_ms": ("maximum display delay", "ms"),
            "longest_pending_stall_ms": ("longest pending stall", "ms"),
            "peak_backlog_bytes": ("peak backlog", "B"),
            "pending_bytes_at_cutoff": ("pending at cutoff", "B"),
            "largest_burst_share": (f"largest {model.BURST_WINDOW_MS} ms output share", "%"),
            "largest_output_share": ("largest output share", "%"),
            "final_output_share": ("final output share", "%"),
        }
        lines.append("Visible progress")
        lines.extend(_samples(streaming, labels))
        lines.append(f"Completed without previews: {streaming.get('zero_preview', 0)}")

    workflow = derived.get("workflow_results") or {}
    lines.extend(_workflow(workflow))

    attempts = [item for item in derived.get("attempts") or [] if item["phase"] == "measured"]
    lines.append("Deadline snapshots")
    if not attempts:
        lines.append("  no measured attempts")
    for item in attempts[:MAX_ATTEMPT_LINES]:
        lines.append(
            f"  slot {item['slot']} {item['outcome']}: visible={_number(item['visible_bytes'])} B"
            f" pending={_number(item['pending_bytes'])} B"
            f" peak={_number(item['peak_backlog_bytes'])} B"
            f" stall={_number(item['longest_pending_stall_ms'])} ms"
            f" deadline_visible={item['deadline_visible']}"
            f" integrity={item['terminal_integrity']}"
            f" stage={item['last_evidenced_stage']}"
        )
    if len(attempts) > MAX_ATTEMPT_LINES:
        lines.append(f"  ... {len(attempts) - MAX_ATTEMPT_LINES} more measured attempts")

    waterfalls = [item["waterfall"] for item in attempts if item.get("waterfall")]
    if waterfalls:
        complete = sum(bool(item.get("complete")) for item in waterfalls)
        residuals = [
            item["residual_ms"] for item in waterfalls if item.get("residual_ms") is not None
        ]
        lines.append(
            f"Waterfall: complete={complete}/{len(waterfalls)}"
            f" median_residual={_number(statistics.median(residuals)) if residuals else 'n/a'} ms"
        )

    attribution = derived.get("attribution") or {}
    coverage = derived.get("coverage") or {}
    quality = derived.get("quality") or {}
    lines.append(
        f"Attribution: {run['observation_mode']} available={attribution.get('available')}"
        f" intervals={len(attribution.get('intervals') or [])}"
        f" unresolved={len(attribution.get('unresolved') or [])}"
        f" union={attribution.get('union_ms')} ms"
    )
    if coverage.get("paths"):
        missing = [name for name, entry in coverage["paths"].items() if not entry.get("complete")]
        if not attribution.get("available"):
            lines.append(
                "Coverage: unavailable ("
                f"{attribution.get('reason') or 'mode_' + run['observation_mode']})"
            )
        else:
            lines.append(
                f"Coverage: {coverage.get('complete')}"
                + (f" (incomplete: {', '.join(missing)})" if missing else "")
            )
    trace = derived.get("trace") or {}
    lines.append(
        f"Capture: mode={run['observation_mode']} quality={quality.get('state')}"
        f" trace={trace.get('path') or 'none'} bytes={trace.get('bytes')}"
    )
    index = derived.get("trace_index") or {}
    exemplars = index.get("exemplars") or []
    if exemplars:
        lines.append(
            "Trace exemplars: "
            + ", ".join(
                f"{item['kind']}:{item['attempt_id']}@{item['metric']}" for item in exemplars
            )
        )

    status = document["status"]
    detail = f" ({'; '.join(status['reasons'])})" if status.get("reasons") else ""
    lines.append(f"Status: {status['state']}{detail}")
    cleanup = status["cleanup"]
    lines.append(
        "Cleanup: verified"
        if cleanup["result"] == "verified"
        else f"Cleanup: {cleanup['result']} ({cleanup.get('reason')})"
    )
    execution = document.get("execution") or {}
    if execution.get("duration_ms") is not None:
        lines.append(f"Orchestration duration: {execution['duration_ms'] / 1000:.1f}s")
    for error in execution.get("errors") or []:
        lines.append(f"Execution: {error}")
    return "\n".join(lines)


def _count(populations, name):
    entry = populations.get(name) or {}
    return entry.get("measured", entry.get("total", 0))


def _share(value):
    return "n/a" if value is None else f"{value * 100:.1f}%"


def _number(value):
    return "n/a" if value is None else f"{value:.1f}"


def _samples(samples, metrics):
    lines = []
    for metric, (label, unit) in metrics.items():
        entry = samples.get(metric) or {}
        quantiles = entry.get("quantiles") or {}
        if not entry.get("count"):
            lines.append(f"  {label}: no samples ({entry.get('reason') or 'unavailable'})")
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
        lines.append(f"  {label}: " + " ".join(parts))
    return lines


def _workflow(workflow):
    lines = []
    if "history" in workflow:
        history = workflow["history"]
        lines.append(
            f"History reads: {history.get('successful', 0)}/{history.get('expected', 0)} "
            f"complete, {history.get('bytes_verified', 0)}/{history.get('bytes_expected', 0)} bytes"
        )
    if "attachment" in workflow:
        attachment = workflow["attachment"]
        lines.append(
            f"Attachment reads: {attachment.get('successful', 0)}/"
            f"{attachment.get('expected', 0)} verified"
        )
    if "exposure" in workflow:
        exposure = workflow["exposure"]
        detail = f" ({exposure.get('reason')})" if exposure.get("reason") else ""
        lines.append(
            f"Bulk exposure: {exposure.get('overlapping_reads', 0)}/"
            f"{exposure.get('scheduled_reads', 0)} overlapping, "
            f"union {exposure.get('union_ms', 0)} ms{detail}"
        )
    if "pair_counts" in workflow:
        counts = workflow["pair_counts"]
        lines.append(
            f"Mixed pairs: {counts.get('pairs', 0)} "
            f"(idle-first {counts.get('idle_first', 0)}, "
            f"bulk-first {counts.get('bulk_first', 0)})"
        )
    if "deltas" in workflow:
        pooled = (workflow["deltas"] or {}).get("pooled") or {}
        for metric, entry in pooled.items():
            if entry.get("mean") is None:
                lines.append(f"  bulk-minus-idle {metric}: unavailable")
            else:
                lines.append(
                    f"  bulk-minus-idle {metric}: {entry['mean']:+.3f} (n={entry.get('count', 0)})"
                )
    if "bulk_read_gate" in workflow:
        gate = workflow["bulk_read_gate"]
        detail = f" ({', '.join(gate.get('reasons', []))})" if gate.get("reasons") else ""
        lines.append(f"Bulk read gate: {gate.get('state')}{detail}")
    if "assessable" in workflow:
        lines.append(
            f"Recovery: {workflow.get('terminal_first', 0)}/"
            f"{workflow.get('assessable', 0)} assessable terminal-first, "
            f"{workflow.get('verified_timeouts', 0)} verified timeouts, "
            f"{workflow.get('unassessable', 0)} unassessable"
        )
    if "catch_up_ms" in workflow:
        lines.extend(
            _samples(
                {"catch_up_ms": workflow["catch_up_ms"]},
                {"catch_up_ms": ("catch-up after restore", "ms")},
            )
        )
    return lines


def comparison(result):
    declaration = result.get("declaration") or {}
    versions = declaration.get("versions") or {}
    lines = [
        f"Comparison: {result['pairs']} matched pair(s), requires "
        f"{result.get('required_pairs')}  vary={result.get('vary')}",
        "Versions: " + ", ".join(f"{name}={value}" for name, value in versions.items()),
        f"Observation: {_modes(result)}",
        f"Budget revision: {declaration.get('budget_revision') or 'none'}",
    ]
    conditions = declaration.get("conditions") or {}
    if conditions.get("mismatches"):
        lines.append("Condition mismatches: " + ", ".join(conditions["mismatches"]))
    lines.append(f"Verdict: {result['verdict']}")
    for metric, entry in sorted((result.get("metrics") or {}).items()):
        estimate = entry.get("estimate_ms")
        upper = entry.get("upper_bound_ms")
        shown = "n/a" if estimate is None else f"{estimate:+.3f} ms"
        bound = "n/a" if upper is None else f"{upper:+.3f} ms"
        lines.append(
            f"{metric}: {shown} one-sided upper {bound} against "
            f"+{entry.get('tolerance_ms')} ms -> {entry.get('verdict')}"
            f" ({entry.get('reason')})"
        )
    for name in ("business", "mode_overhead", "predecessor"):
        section = (result.get("verdicts") or {}).get(name)
        if section is None:
            continue
        detail = f" ({', '.join(section.get('reasons') or [])})" if section.get("reasons") else ""
        lines.append(f"{name.replace('_', ' ').title()}: {section.get('state')}{detail}")
    budget = result.get("budget")
    if isinstance(budget, dict) and budget:
        lines.append(f"Budget: revision={budget.get('revision')} state={budget.get('state')}")
        for metric, entry in (budget.get("metrics") or {}).items():
            lines.append(
                f"  {metric}: upper {entry.get('upper_bound_observed')} vs "
                f"{entry.get('upper_bound')} {entry.get('unit')} -> {entry.get('verdict')}"
                f" ({entry.get('reason')})"
            )
    elif isinstance(budget, str) and budget:
        # The predecessor section reports the budget revision it applied.
        lines.append(f"Budget: revision={budget}")
    if result.get("reasons"):
        lines.append("Reasons: " + ", ".join(result["reasons"]))
    return "\n".join(lines)


def _modes(result):
    declaration = result.get("declaration") or {}
    modes = declaration.get("observation_mode") or {}
    return (
        "baseline="
        + ",".join(map(str, modes.get("baseline") or []))
        + " candidate="
        + ",".join(map(str, modes.get("candidate") or []))
    )


def method():
    declaration = analysis.DECLARATION
    return "\n".join(
        [
            "Comparison protocol (declared before measuring candidates):",
            "  primary metrics: " + ", ".join(declaration["primary_metrics"]),
            "  tolerances: "
            + ", ".join(f"{key}={value}ms" for key, value in declaration["tolerance_ms"].items()),
            "  sample floor: "
            + ", ".join(f"{key}={value}" for key, value in declaration["sample_floor"].items()),
            f"  repeats: {declaration['repeats']}  confidence: {declaration['confidence']}",
            f"  statistic: {declaration['statistic']}",
            "  correctness floor: " + str(declaration["correctness_floor"]),
            "Comparison dimensions: build, network, observation, aa, predecessor.",
            "  build/network: one-way matched comparisons under shared conditions.",
            "  observation: same build, only the observation mode varies; budget required.",
            "  aa: neither build nor conditions vary; establishes measurement noise.",
            "  predecessor: frozen pre-instrumentation off build vs candidate off build.",
        ]
    )
