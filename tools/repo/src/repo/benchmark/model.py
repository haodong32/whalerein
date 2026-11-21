"""Versioned benchmark facts: one attempt outcome each, timings and run status.

The container produces this document; the host validates it before it is
published, and the terminal summary is rendered from the same facts. Validation
is strict so a malformed export can never be presented as a measurement.
"""

from __future__ import annotations

import json
import math
import re
from bisect import bisect_right
from collections import Counter
from itertools import accumulate, pairwise
from pathlib import Path
from statistics import median

from ..errors import RepoError

SCHEMA_VERSION = 1
SCENARIO_ID = "streaming-reply"
SCENARIO_VERSION = 6
OBSERVATION_SURFACE = "cli-stdout"
MAX_HOST_BROKER_EXTRA_RTT_MS = 1000
NETWORK_PROBE_COUNT = 5
BURST_WINDOW_MS = 50
NETWORK_PROFILES = {
    "normal": (0, 0, 0.0),
    "high-rtt": (280, 0, 0.0),
    "jitter": (280, 40, 0.0),
    "loss": (280, 0, 1.0),
}
NETWORK_LINKS = ("host", "client", "both")
WORKFLOWS = ("watch", "recovery", "mixed-load")
DEFAULT_CASES = (
    ("watch", "normal", "host"),
    ("watch", "high-rtt", "host"),
    ("watch", "jitter", "host"),
    ("watch", "loss", "host"),
    ("recovery", "normal", "client"),
    ("mixed-load", "normal", "host"),
)
LINK_STREAMING_METRICS = {
    "largest_burst_share": (f"largest {BURST_WINDOW_MS} ms output share", "%"),
}
PRIMARY_STREAMING_METRICS = {
    "max_display_delay_ms": ("maximum display delay", "ms"),
    **LINK_STREAMING_METRICS,
}
UPDATES = tuple(f"Fixture segment {index:02d}. " for index in range(1, 33))
STREAMING_METRICS = {
    "longest_output_gap_ms": ("longest output gap", "ms"),
    "largest_output_share": ("largest output share", "%"),
    "final_output_share": ("final output share", "%"),
}

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


def valid_error_code(value):
    return isinstance(value, str) and re.fullmatch(r"[a-z][a-z0-9_]{0,63}", value) is not None


def quantiles(observed):
    """Exact nearest-rank percentiles for sorted samples."""
    if not observed:
        return {}
    size = len(observed)
    values = {}
    for name, fraction in QUANTILE_FRACTIONS.items():
        values[name] = observed[max(1, math.ceil(fraction * size)) - 1]
    return values


def network_settings(profile, link):
    rtt, jitter, loss = NETWORK_PROFILES[profile]
    if link not in NETWORK_LINKS:
        raise ValueError("unknown benchmark network link")
    return {
        "profile": profile,
        "link": link,
        "extra_rtt_ms": rtt,
        "jitter_ms_per_direction": jitter,
        "loss_percent_per_direction": loss,
    }


def shaped_links(network):
    return ("host", "client") if network["link"] == "both" else (network["link"],)


def workload_controls(workflow, cadence_ms):
    if workflow == "recovery":
        return {"outage": {"start_ms": 8 * cadence_ms, "duration_ms": min(1000, 10 * cadence_ms)}}
    if workflow == "mixed-load":
        return {
            "history": {
                "start_ms": [index * cadence_ms for index in (4, 7, 10, 13)],
                "messages": 8,
                "bytes_per_message": 8192,
            }
        }
    return {}


def history_texts(history):
    return [
        f"History item {index:02d}. ".ljust(history["bytes_per_message"], "x")
        for index in range(history["messages"])
    ]


def scenario(configuration):
    gap = configuration["cadence_ms"]
    final = gap * len(UPDATES)
    workflow = configuration["workflow"]
    return {
        "id": SCENARIO_ID,
        "version": SCENARIO_VERSION,
        "seed": str(configuration["seed"]),
        "deadline_ms": configuration["deadline_ms"],
        "observation_window_ms": configuration["observation_window_ms"],
        "network": configuration["network"],
        "workload": {
            "updates": list(UPDATES),
            "gap_ms": gap,
            "delivery": "immediate",
            "workflow": workflow,
            **workload_controls(workflow, gap),
        },
        "response": {
            "start_ms": 0,
            "final_ms": final,
            "delay_ms": configuration["delay_ms"],
            "duplicate_final_ms": final + 300,
            "final_text": "".join(UPDATES),
        },
    }


def sample_entry(values):
    """Summarize sorted samples using the shared nearest-rank floors."""
    count = len(values)
    observed = quantiles(values)
    return {
        "count": count,
        "quantiles": {name: observed[name] for name, floor in TAILS if count >= floor},
        "unavailable": {name: UNAVAILABLE_TAIL for name, floor in TAILS if 0 < count < floor},
        "reason": None if count else "no_samples",
    }


def source_sizes(scenario):
    return [len(chunk.encode()) for chunk in scenario["workload"]["updates"]]


def longest_pending_gap(outputs, source_times, sizes):
    """Largest unseen-text wait between increases of the correct visible prefix."""
    emitted_bytes = list(accumulate(sizes))
    visible_bytes = 0
    last_output = source_times[0]
    longest = 0
    for event in outputs:
        prefix_bytes = event["prefix_bytes"]
        if prefix_bytes <= visible_bytes:
            continue
        pending = bisect_right(emitted_bytes, visible_bytes)
        if pending >= len(source_times):
            break
        started = max(last_output, source_times[pending])
        longest = max(longest, event["at_ms"] - started)
        visible_bytes = prefix_bytes
        last_output = event["at_ms"]
    return longest


def segment_display_delays(outputs, source_times, sizes):
    """Times from each emitted increment to its first correct visible prefix."""
    source = iter(zip(accumulate(sizes), source_times, strict=True))
    milestone = next(source, None)
    for event in outputs:
        while milestone is not None and event["prefix_bytes"] >= milestone[0]:
            yield event["at_ms"] - milestone[1]
            milestone = next(source, None)


def max_display_delay(outputs, source_times, sizes):
    return max(segment_display_delays(outputs, source_times, sizes), default=0)


def diagnostic_summary(attempts, sizes):
    delays = sorted(
        round(delay, 3)
        for attempt in _measured(attempts)
        if attempt["outcome"] == "completed"
        for delay in segment_display_delays(attempt["outputs"], attempt["source_times_ms"], sizes)
    )
    return {"segment_display_delay_ms": sample_entry(delays)}


def largest_burst(outputs, window_ms):
    """Bulk output in the busiest half-open time window."""
    first = total = largest = 0
    for event in outputs:
        while first < len(outputs) and event["at_ms"] - outputs[first]["at_ms"] >= window_ms:
            total -= outputs[first]["bytes"]
            first += 1
        total += event["bytes"]
        largest = max(largest, total)
    return largest


def streaming_values(
    attempt, final_bytes, sizes=None, burst_window_ms=None, *, display_delay=False
):
    outputs = attempt["outputs"]
    source = attempt["source_times_ms"]
    if sizes is None:
        times = [source[0], *(event["at_ms"] for event in outputs)]
        longest = max(b - a for a, b in pairwise(times))
    else:
        longest = longest_pending_gap(outputs, source, sizes)
    values = {
        "longest_output_gap_ms": round(longest, 3),
        "largest_output_share": max(event["bytes"] for event in outputs) / final_bytes,
        "final_output_share": outputs[-1]["bytes"] / final_bytes,
    }
    if burst_window_ms is not None:
        values["largest_burst_share"] = largest_burst(outputs, burst_window_ms) / final_bytes
    if display_delay:
        values["max_display_delay_ms"] = round(max_display_delay(outputs, source, sizes), 3)
    return values


def streaming_summary(
    attempts, final_bytes, sizes=None, burst_window_ms=None, *, display_delay=False
):
    completed = [item for item in _measured(attempts) if item["outcome"] == "completed"]
    samples = [
        streaming_values(item, final_bytes, sizes, burst_window_ms, display_delay=display_delay)
        for item in completed
    ]
    metrics = (
        (*STREAMING_METRICS, *LINK_STREAMING_METRICS)
        if burst_window_ms is not None
        else STREAMING_METRICS
    )
    if display_delay:
        metrics = (*metrics, "max_display_delay_ms")
    return {
        "samples": {
            metric: sample_entry(sorted(row[metric] for row in samples)) for metric in metrics
        },
        "zero_preview": sum(len(item["outputs"]) == 1 for item in completed),
    }


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
    _cadence(document)
    _environment(document["environment"])
    _status(document["status"])
    attempts = _attempts(document["attempts"], complete=document["status"]["state"] == "complete")
    _outcomes(document["outcomes"], attempts)
    _latency(document["latency"], attempts)
    version = document["scenario"]["version"]
    if version >= 2:
        _streaming(document, attempts)
    if version >= 5:
        _link_network(document)
    elif version >= 3:
        _network(document)
    if version >= 6:
        _workflow(document, attempts)
    return document


def _scenario(scenario):
    if not isinstance(scenario, dict):
        invalid("scenario is not an object")
    if scenario.get("id") != SCENARIO_ID:
        invalid("unknown scenario identity")
    if scenario.get("version") not in (1, 2, 3, 4, 5, SCENARIO_VERSION):
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
    if scenario["version"] >= 4 and workload["delivery"] != "immediate":
        invalid("the source-driven scenario requires immediate fixture delivery")
    _response(scenario["response"], scenario["deadline_ms"], workload, scenario["version"])
    if scenario["version"] == 2 and not count(scenario.get("event_ack_floor_ms")):
        invalid("event_ack_floor_ms must be a non-negative integer")
    if scenario["version"] in (3, 4):
        delay = scenario.get("host_broker_extra_rtt_ms")
        if not count(delay) or delay > MAX_HOST_BROKER_EXTRA_RTT_MS:
            invalid("host_broker_extra_rtt_ms must be in the supported range")


def observed_extra_rtt(probe):
    return median(probe["host_broker_ms"]) - median(probe["reference_ms"])


def network_probe_matches(probe, expected):
    # RTT calibration has its own tolerance. It does not classify a product
    # performance result. Compare medians to suppress isolated scheduler noise.
    return abs(observed_extra_rtt(probe) - expected) <= max(10, expected * 0.10)


def _network(document):
    expected = document["scenario"]["host_broker_extra_rtt_ms"]
    if document["run"]["configuration"].get("host_broker_extra_rtt_ms") != expected:
        invalid("scenario Host–broker RTT disagrees with the run configuration")
    probe = document["environment"].get("network_probe")
    if probe is None and document["status"]["state"] != "complete":
        return
    if not isinstance(probe, dict):
        invalid("a complete run requires a network probe")
    for key in ("reference_ms", "host_broker_ms"):
        samples = probe.get(key)
        if (
            not isinstance(samples, list)
            or len(samples) != NETWORK_PROBE_COUNT
            or not all(number(sample) and sample > 0 for sample in samples)
        ):
            invalid("network probe requires positive TCP RTT samples")
    if document["status"]["state"] == "complete" and not network_probe_matches(probe, expected):
        invalid("observed Host–broker RTT does not match the network profile")


def link_probe_matches(probe, network):
    reference = median(probe["reference_ms"])
    for link in ("host", "client"):
        shaped = link in shaped_links(network)
        extra = network["extra_rtt_ms"] if shaped else 0
        jitter = network["jitter_ms_per_direction"] if shaped else 0
        observed = median(probe[link + "_ms"]) - reference
        if abs(observed - extra) > max(10, extra * 0.10, jitter * 2):
            return False
    return True


def _link_network(document):
    network = document["scenario"].get("network")
    try:
        expected = network_settings(network["profile"], network["link"])
    except (KeyError, TypeError, ValueError):
        invalid("unknown benchmark network settings")
    if network != expected or document["run"]["configuration"].get("network") != network:
        invalid("scenario network settings disagree with the run configuration")
    if document["status"]["state"] != "complete":
        return
    probe = document["environment"].get("network_probe")
    if not isinstance(probe, dict):
        invalid("a complete run requires network samples for all links")
    for key in ("reference_ms", "host_ms", "client_ms"):
        samples = probe.get(key)
        if not (
            isinstance(samples, list)
            and len(samples) == NETWORK_PROBE_COUNT
            and all(number(sample) and sample > 0 for sample in samples)
        ):
            invalid("network probe requires positive TCP RTT samples for all links")
    if not link_probe_matches(probe, network):
        invalid("observed RTT does not match the selected network link and profile")
    counters = document["environment"].get("network_counters")
    if not isinstance(counters, dict) or not all(
        count(counters.get(key)) for key in ("packets", "dropped")
    ):
        invalid("a complete run requires netem packet and drop counters")


def workflow_summary(attempts, workflow):
    measured = list(_measured(attempts))
    if workflow == "recovery":
        delays = [
            attempt["recovery"]["catch_up_ms"]
            for attempt in measured
            if attempt["outcome"] == "completed"
            and attempt.get("recovery")
            and attempt["recovery"].get("catch_up_ms") is not None
        ]
        return {"catch_up_ms": sample_entry(sorted(delays))}
    if workflow == "mixed-load":
        reads = [read for attempt in measured for read in attempt.get("history_reads", [])]
        completed = [read["duration_ms"] for read in reads if read["outcome"] == "completed"]
        result = {
            "history_correct": len(completed),
            "history_measured": len(reads),
            "history_read_ms": sample_entry(sorted(completed)),
        }
        errors = Counter(read["error_code"] for read in reads if "error_code" in read)
        if errors:
            result["history_errors"] = dict(sorted(errors.items()))
        return result
    return {}


def _workflow(document, attempts):
    workload = document["scenario"]["workload"]
    configuration = document["run"]["configuration"]
    workflow = workload.get("workflow")
    if workflow not in WORKFLOWS or configuration.get("workflow") != workflow:
        invalid("scenario workflow disagrees with the run configuration")
    if any(
        workload.get(key) != expected
        for key, expected in workload_controls(workflow, configuration["cadence_ms"]).items()
    ):
        invalid("scenario controls disagree with the declared workflow")
    if workflow == "recovery" and document["scenario"]["network"]["link"] != "client":
        invalid("recovery must control the client link")
    for attempt in attempts:
        if workflow == "recovery":
            recovery = attempt.get("recovery")
            if (
                not isinstance(recovery, dict)
                or not all(number(recovery.get(key)) for key in ("started_ms", "restored_ms"))
                or recovery["restored_ms"] <= recovery["started_ms"]
            ):
                invalid("recovery requires an observed blackout and restoration")
            catch_up = recovery.get("catch_up_ms")
            if not (catch_up is None or number(catch_up)):
                invalid("recovery catch-up time must be non-negative")
            if attempt["outcome"] == "completed" and catch_up is None:
                invalid("a completed recovery requires an observed catch-up")
            if attempt["outcome"] == "completed" and not any(
                recovery["started_ms"] <= at_ms < recovery["restored_ms"]
                for at_ms in attempt["source_times_ms"]
            ):
                invalid("the source must continue through the recovery blackout")
        if workflow == "mixed-load":
            reads = attempt.get("history_reads")
            if not isinstance(reads, list) or len(reads) != len(workload["history"]["start_ms"]):
                invalid("mixed load requires an outcome for every scheduled read")
            for read in reads:
                if (
                    not isinstance(read, dict)
                    or not number(read.get("at_ms"))
                    or not number(read.get("duration_ms"))
                    or read.get("outcome")
                    not in ("completed", "timeout", "incorrect_content", "product_error")
                ):
                    invalid("invalid history read observation")
                if "error_code" in read and (
                    read["outcome"] != "product_error" or not valid_error_code(read["error_code"])
                ):
                    invalid("invalid history read error code")
    if document.get("workflow_results") != workflow_summary(attempts, workflow):
        invalid("workflow results disagree with attempt observations")


def _streaming(document, attempts):
    scenario = document["scenario"]
    config = document["run"]["configuration"]
    if (
        scenario["version"] == 2
        and config.get("event_ack_floor_ms") != scenario["event_ack_floor_ms"]
    ):
        invalid("scenario ack floor disagrees with the run configuration")
    final_bytes = len(scenario["response"]["final_text"].encode())
    sizes = source_sizes(scenario) if scenario["version"] >= 4 else None
    source_prefixes = [0, *accumulate(sizes)] if sizes is not None else None
    for attempt in attempts:
        if attempt["outcome"] != "completed":
            continue
        source, outputs = attempt.get("source_times_ms"), attempt.get("outputs")
        if (
            not isinstance(source, list)
            or len(source) != len(scenario["workload"]["updates"])
            or (not all(number(time) for time in source) or any(a > b for a, b in pairwise(source)))
        ):
            invalid("a completed attempt requires ordered source emission times")
        if sizes is not None and source[0] != 0:
            invalid("source-driven timelines must start at the first source emission")
        if not isinstance(outputs, list) or not outputs:
            invalid("a completed attempt requires a streaming timeline")
        previous = source[0]
        for event in outputs:
            if not isinstance(event, dict) or not number(event.get("at_ms")):
                invalid("invalid streaming output time")
            if event["at_ms"] < previous:
                invalid("streaming output times must be ordered after source start")
            if not count(event.get("bytes")) or event["bytes"] > final_bytes:
                invalid("invalid streaming output byte count")
            if type(event.get("final")) is not bool:
                invalid("invalid streaming output kind")
            if source_prefixes is not None:
                available = source_prefixes[bisect_right(source, event["at_ms"])]
                prefix = event.get("prefix_bytes")
                if not count(prefix) or prefix > available:
                    invalid("visible prefix exceeds the emitted source bytes")
            previous = event["at_ms"]
        previews, final = outputs[:-1], outputs[-1]
        if any(event["bytes"] == 0 for event in previews):
            invalid("empty previews cannot be streaming outputs")
        if (
            not final["final"]
            or final["at_ms"] != attempt["final_ms"]
            or any(event["final"] for event in previews)
        ):
            invalid("last streaming output must be the committed reply")
        if len(previews) != attempt.get("update_count"):
            invalid("streaming preview count disagrees with the attempt")
        if sizes is not None and final["prefix_bytes"] != final_bytes:
            invalid("the final must settle the complete correct prefix")
        first_output = (
            next((event["at_ms"] for event in outputs if event["bytes"]), None)
            if sizes is not None
            else previews[0]["at_ms"]
            if previews
            else None
        )
        if attempt.get("first_text_ms") != first_output:
            invalid("streaming first text time disagrees with the attempt")
        if final["at_ms"] < source[-1]:
            invalid("final output precedes the last source emission")
        if not previews and final["bytes"] != final_bytes:
            invalid("a reply without previews must render the entire final")
    burst_window = BURST_WINDOW_MS if scenario["version"] >= 5 else None
    if document.get("streaming") != streaming_summary(
        attempts, final_bytes, sizes, burst_window, display_delay=scenario["version"] >= 6
    ):
        invalid("streaming summary disagrees with attempt timelines")
    if scenario["version"] >= 6 and "diagnostics" in document:
        if document["diagnostics"] != diagnostic_summary(attempts, sizes):
            invalid("diagnostic summary disagrees with attempt timelines")


def _response(response, deadline_ms, workload, version):
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
    scripted_settle = response["final_ms"] - response["start_ms"]
    if version < 4:
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


def _cadence(document):
    """The recorded cadence must match the workload it produced."""
    configuration = document["run"]["configuration"]
    if "cadence_ms" not in configuration:
        return
    cadence = configuration["cadence_ms"]
    if not count(cadence) or cadence < 1:
        invalid("run configuration cadence_ms must be a positive integer")
    gap = document["scenario"]["workload"]["gap_ms"]
    if gap != cadence:
        invalid("scenario gap_ms disagrees with the recorded cadence")


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


def _attempts(attempts, *, complete):
    if not isinstance(attempts, list) or (complete and not attempts):
        invalid("a complete run requires a nonempty attempt list")
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
    if attempt["outcome"] == "timeout_after_text" and first is None:
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
