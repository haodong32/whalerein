"""Current-version benchmark case facts, derivation and report documents.

The fixture and outer runner persist raw observations only. This module is the
single owner of the business verdicts: attempt outcomes, deadline snapshots,
terminal integrity, slot accounting, populations, percentiles, mixed-load
exposure and paired effects, recovery catch-up, attribution intervals, trace
coverage, quality and exemplar selection. `derive` is a pure function of the
raw facts, so online reporting and offline replay use exactly the same code.

Only case schema 2, suite schema 2 and workload version 8 are accepted. Retired
and future versions fail with an explicit unsupported-version error; there are
no fallback readers and no scenario-version branches.
"""

from __future__ import annotations

import copy
import json
import math
import re
from bisect import bisect_right
from collections import Counter
from itertools import accumulate, pairwise
from pathlib import Path
from statistics import median

from ..errors import RepoError
from . import analysis

SCHEMA_VERSION = 2
SUITE_SCHEMA_VERSION = 2
SCENARIO_VERSION = 8
SCENARIO_ID = "streaming-reply"
OBSERVATION_SURFACE = "cli-stdout"
OBSERVATION_ENDPOINT = "cli_stdout"
NETWORK_PROBE_COUNT = 5
BURST_WINDOW_MS = 50
DEFAULT_ATTEMPTS = 30
DEFAULT_WARMUPS = 4

# Mixed-load arms. A neutral watch/recovery attempt uses ARM_WATCH, which is
# not a background-load arm and never enters the paired comparison.
ARMS = ("idle", "bulk")
ARM_WATCH = "watch"
SLOT_ARMS = (ARM_WATCH, *ARMS)
PHASES = ("warmup", "measured")
WORKFLOWS = ("watch", "recovery", "mixed-load")
NETWORK_PROFILES = {
    "normal": (0, 0, 0.0),
    "high-rtt": (280, 0, 0.0),
    "jitter": (280, 40, 0.0),
    "loss": (280, 0, 1.0),
}
NETWORK_LINKS = ("host", "client", "both")
DEFAULT_CASES = (
    ("watch", "normal", "host"),
    ("watch", "high-rtt", "host"),
    ("watch", "jitter", "host"),
    ("watch", "loss", "host"),
    ("recovery", "normal", "client"),
    ("mixed-load", "normal", "host"),
)
ATTACHMENT_BYTES = 1 << 20
ATTACHMENT_SEED = 7
ATTACHMENT_NAME = "benchmark-attachment.bin"
ATTACHMENT_MEDIA_TYPE = "application/octet-stream"
UPDATES = tuple(f"Fixture segment {index:02d}. " for index in range(1, 33))

OUTCOMES = (
    "completed",
    "product_error",
    "timeout_before_text",
    "timeout_after_text",
    "late_reply",
    "incorrect_content",
    "duplicate_final",
    "not_started",
    "setup_failed",
    "interrupted",
)
FAILURE_OUTCOMES = tuple(item for item in OUTCOMES if item != "completed")
FAILURE_ATTRIBUTIONS = ("product", "fixture", "unknown")
STATUSES = ("complete", "incomplete", "invalid")
SLOT_STATES = (
    "planned",
    "started",
    "measurement_entered",
    "finished",
    "setup_failed",
    "interrupted",
    "not_started",
)
CLEANUP_RESULTS = ("verified", "unverified")
OBSERVATION_MODES = ("off", "aggregate", "trace")
EARLY_STOPS = ("none", "deadline", "terminal_window", "error", "interrupted")
OUTPUT_KINDS = ("preview", "replace", "reset", "terminal", "duplicate", "wrong", "hidden")
READ_OUTCOMES = ("completed", "timeout", "incorrect_content", "product_error")
READ_ROUTES = ("inline", "reference")
RECOVERY_MILESTONES = (
    "blackout_start",
    "link_restored",
    "subscribed",
    "channel_ready",
    "watch_ready",
    "first_update",
    "first_visible",
    "caught_up",
    "timeout",
)
MILESTONE_STATES = ("observed", "missed", "inapplicable")

# A tail is only reported when its sample floor is met. Above the floor the
# nearest-rank estimate is exact for the observed samples; below it the estimate
# is labelled unavailable rather than presented as evidence.
TAILS = (("median", 1), ("p95", 20), ("p99", 100))
TAIL_MINIMUM = dict(TAILS)
QUANTILE_FRACTIONS = {"median": 0.50, "p95": 0.95, "p99": 0.99}
UNAVAILABLE_TAIL = "insufficient_samples"

STREAMING_METRICS = {
    "max_display_delay_ms": ("maximum display delay", "ms"),
    "longest_pending_stall_ms": ("longest pending stall", "ms"),
    "peak_backlog_bytes": ("peak backlog", "bytes"),
    "pending_bytes_at_cutoff": ("pending bytes at cutoff", "bytes"),
}
PAIR_STREAMING_METRICS = (
    "max_display_delay_ms",
    "longest_pending_stall_ms",
    "largest_output_share",
    "final_output_share",
)

# Metric catalog: each public metric declares its unit, boundaries, sample
# unit, population, denominator, observation mode, missingness and weighting.
# Watch-weighted and segment-weighted identifiers are deliberately distinct.
METRIC_CATALOG = {
    "first_text_ms": {
        "unit": "ms",
        "boundaries": "first source emission -> first positive verified visible prefix",
        "sample_unit": "measured attempt",
        "population": "measured attempts that reached first correct text, including later timeouts",
        "denominator": "measured attempts",
        "mode": "off|aggregate|trace",
        "missingness": "unavailable when first text was never observed",
        "weighting": "watch",
    },
    "final_ms": {
        "unit": "ms",
        "boundaries": "first source emission -> on-time correct terminal visibility",
        "sample_unit": "measured attempt",
        "population": "on-time correct completions with passing terminal integrity",
        "denominator": "measured attempts",
        "mode": "off|aggregate|trace",
        "missingness": "unavailable without an on-time correct completion",
        "weighting": "watch",
    },
    "max_display_delay_ms": {
        "unit": "ms",
        "boundaries": "each emitted increment -> first visible prefix covering it",
        "sample_unit": "measured attempt",
        "population": "measured attempts with reconstructable output",
        "denominator": "measured attempts with reconstructable output",
        "mode": "off|aggregate|trace",
        "missingness": "unavailable without output observations",
        "weighting": "watch",
    },
    "segment_display_delay_ms": {
        "unit": "ms",
        "boundaries": "one emitted increment -> first visible prefix covering it",
        "sample_unit": "emitted segment",
        "population": "segments that became visible on measured attempts",
        "denominator": "measured attempts",
        "mode": "off|aggregate|trace",
        "missingness": "unavailable without visible segments",
        "weighting": "segment",
    },
    "longest_pending_stall_ms": {
        "unit": "ms",
        "boundaries": "outstanding emitted text -> next actual prefix advance or cutoff",
        "sample_unit": "measured attempt",
        "population": "measured attempts with outstanding text",
        "denominator": "measured attempts",
        "mode": "off|aggregate|trace",
        "missingness": "unavailable when text was never outstanding",
        "weighting": "watch",
    },
    "peak_backlog_bytes": {
        "unit": "bytes",
        "boundaries": "source emission -> cutoff snapshot",
        "sample_unit": "measured attempt",
        "population": "measured attempts with emitted text",
        "denominator": "measured attempts",
        "mode": "off|aggregate|trace",
        "missingness": "unavailable without emitted text",
        "weighting": "watch",
    },
    "pending_bytes_at_cutoff": {
        "unit": "bytes",
        "boundaries": "source emission -> cutoff snapshot",
        "sample_unit": "measured attempt",
        "population": "measured attempts with emitted text",
        "denominator": "measured attempts",
        "mode": "off|aggregate|trace",
        "missingness": "unavailable without emitted text",
        "weighting": "watch",
    },
    "host_queue_ms": {
        "unit": "ms",
        "boundaries": "host_enqueue -> host_dequeue",
        "sample_unit": "matched frame",
        "population": "matched sampled events",
        "denominator": "sampled attempts eligible for the path",
        "mode": "trace",
        "missingness": "unavailable when the join is missing, reversed or ambiguous",
        "weighting": "stage",
    },
    "access_forward_ms": {
        "unit": "ms",
        "boundaries": "access_query admit -> reply",
        "sample_unit": "matched request attempt",
        "population": "matched sampled query events",
        "denominator": "sampled attempts eligible for the path",
        "mode": "trace",
        "missingness": "unavailable when the join is missing, reversed or ambiguous",
        "weighting": "stage",
    },
    "core_delivery_ms": {
        "unit": "ms",
        "boundaries": "core_receipt -> core_delivery",
        "sample_unit": "matched frame",
        "population": "matched sampled frame events",
        "denominator": "sampled attempts eligible for the path",
        "mode": "trace",
        "missingness": "unavailable when the join is missing, reversed or ambiguous",
        "weighting": "stage",
    },
    "cli_terminal_ms": {
        "unit": "ms",
        "boundaries": "cli_request invoke -> terminal",
        "sample_unit": "matched request attempt",
        "population": "matched sampled CLI requests",
        "denominator": "sampled attempts eligible for the path",
        "mode": "trace",
        "missingness": "unavailable when the join is missing, reversed or ambiguous",
        "weighting": "stage",
    },
}

COVERAGE_PATHS = {
    "preview": (
        "fixture_emit",
        "dsh_callback",
        "dsh_handoff",
        "host_enqueue",
        "host_dequeue",
        "host_publish",
        "core_receipt",
        "core_enqueue",
        "core_dequeue",
        "core_delivery",
        "cli_write",
        "fixture_observe",
    ),
    "query": ("cli_request", "access_query", "host_command", "core_command", "cli_request"),
    "command": ("cli_request", "access_command", "host_command", "core_command", "cli_request"),
    "recovery": ("fixture_recovery", "core_join", "fixture_observe"),
    "bulk": ("cli_request", "access_query", "core_sync", "cli_request"),
    "object": ("cli_request", "access_relay", "core_command", "cli_request"),
}

EVENT_PRODUCERS = {
    "fixture_clock_check": ("fixture",),
    "fixture_slot": ("fixture",),
    "fixture_socket_sample": ("fixture",),
    "fixture_observe": ("fixture",),
    "fixture_emit": ("fixture",),
    "fixture_recovery": ("fixture",),
    "dsh_callback": ("dsh",),
    "dsh_handoff": ("dsh",),
    "host_enqueue": ("host",),
    "host_coalesce": ("host",),
    "host_dequeue": ("host",),
    "host_publish": ("host",),
    "host_command": ("host",),
    "host_subscribe": ("host",),
    "core_receipt": ("core",),
    "core_enqueue": ("core",),
    "core_dequeue": ("core",),
    "core_delivery": ("core",),
    "core_join": ("core",),
    "core_sync": ("core",),
    "core_command": ("core",),
    "cli_write": ("core", "cli"),
    "cli_request": ("cli",),
    "access_query": ("access",),
    "access_command": ("access",),
    "access_relay": ("access",),
    "access_notice": ("access",),
    "access_transport": ("access",),
}

FORBIDDEN_FIELDS = (
    "text",
    "payload",
    "body",
    "ciphertext",
    "token",
    "secret",
    "key",
    "cursor",
    "host_id",
    "session_id",
    "device_id",
    "approval",
    "path",
)

# Typed event pairs used for attribution. The key fields are the contract's
# join scope; a missing, reversed or ambiguous match stays unavailable.
ATTRIBUTION_PAIRS = {
    "host_queue_ms": ("host_enqueue", "host_dequeue", ("stream", "block", "seq_start", "seq_end")),
    "access_forward_ms": ("access_query", "access_query", ("request", "attempt")),
    "core_delivery_ms": ("core_receipt", "core_delivery", ("frame",)),
    "cli_terminal_ms": ("cli_request", "cli_request", ("request", "attempt", "kind")),
}
_ACCESS_STAGE = ("admit", "reply")
_CLI_STAGE = ("invoke", "terminal")

# Per-increment causal edges. These are evaluated per attempt for the waterfall
# and for edge-completeness coverage; the global attribution above remains the
# bounded aggregate view.
EDGE_PAIRS = (
    ("source_to_callback_ms", "fixture_emit", "dsh_callback",
     ("stream", "block", "seq_start", "seq_end")),
    ("callback_to_handoff_ms", "dsh_callback", "dsh_handoff",
     ("stream", "block", "seq_start", "seq_end", "batch")),
    ("handoff_to_enqueue_ms", "dsh_handoff", "host_enqueue",
     ("stream", "block", "seq_start", "seq_end", "batch")),
    ("enqueue_to_publish_ms", "host_enqueue", "host_publish",
     ("stream", "seq_start", "seq_end")),
    ("publish_to_receipt_ms", "host_publish", "core_receipt",
     ("stream", "seq_start", "seq_end")),
    # Core stamps core_dequeue after handling; the delivery therefore precedes
    # it. Join receipt->delivery and keep wait_ns as the queue-wait fact instead
    # of inventing a reversed dequeue->delivery edge.
    ("receipt_to_delivery_ms", "core_receipt", "core_delivery", ("frame",)),
    ("delivery_to_write_ms", "core_delivery", "cli_write", ("delivery",)),
)
PREVIEW_EDGES = tuple(name for name, *_ in EDGE_PAIRS) + ("write_to_observe_ms",)


def invalid(message):
    raise RepoError("invalid benchmark case: " + message)


def unsupported(message):
    raise RepoError("unsupported benchmark " + message)


def number(value):
    return type(value) in (int, float) and math.isfinite(value) and value >= 0


def count(value):
    return type(value) is int and value >= 0


def text(value):
    return isinstance(value, str) and bool(value)


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


def sample_entry(values):
    """Summarize sorted samples using the shared nearest-rank floors."""
    size = len(values)
    observed = quantiles(values)
    return {
        "count": size,
        "quantiles": {name: observed[name] for name, floor in TAILS if size >= floor},
        "unavailable": {name: UNAVAILABLE_TAIL for name, floor in TAILS if 0 < size < floor},
        "floors": {name: floor for name, floor in TAILS},
        "reason": None if size else "no_samples",
    }


def seed_parity(seed):
    value = str(seed).strip()
    if value.lstrip("-").isdigit():
        return int(value) % 2
    return sum(ord(char) for char in value) % 2


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


def workload_controls(workflow, cadence_ms, *, paired=False):
    if workflow == "recovery":
        return {"outage": {"start_ms": 8 * cadence_ms, "duration_ms": min(1000, 10 * cadence_ms)}}
    if workflow == "mixed-load":
        controls = {
            "history": {
                "start_ms": [index * cadence_ms for index in (4, 7, 10, 13)],
                "messages": 8,
                "bytes_per_message": 8192,
            },
            "exposure": {"required_ms": max(1, cadence_ms)},
        }
        if paired:
            controls["attachment"] = {
                "start_ms": [5 * cadence_ms],
                "bytes": ATTACHMENT_BYTES,
                "seed": ATTACHMENT_SEED,
                "name": ATTACHMENT_NAME,
                "media_type": ATTACHMENT_MEDIA_TYPE,
            }
            controls["pairing"] = {
                "arms": list(ARMS),
                "rule": "alternating",
                "first": "seed-parity",
            }
        return controls
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
            **workload_controls(workflow, gap, paired=True),
        },
        "response": {
            "start_ms": 0,
            "final_ms": final,
            "delay_ms": configuration.get("delay_ms", 0),
            "duplicate_final_ms": final + 300,
            "final_text": "".join(UPDATES),
        },
    }


def source_sizes(scenario):
    return [len(chunk.encode()) for chunk in scenario["workload"]["updates"]]


def observed_extra_rtt(probe):
    return median(probe["host_broker_ms"]) - median(probe["reference_ms"])


def network_probe_matches(probe, expected):
    return abs(observed_extra_rtt(probe) - expected) <= max(10, expected * 0.10)


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


# ---------------------------------------------------------------------------
# Raw case validation
# ---------------------------------------------------------------------------


def validate_case(facts):
    """Strict structural validation of raw case facts, without deriving verdicts."""
    if not isinstance(facts, dict):
        invalid("document is not an object")
    schema = facts.get("schema_version")
    if schema != SCHEMA_VERSION:
        unsupported(f"case schema {schema!r}; expected {SCHEMA_VERSION}")
    kind = facts.get("kind")
    if kind == "benchmark-suite":
        unsupported("mixed document kind benchmark-suite for a case reader")
    if kind is not None and kind != "benchmark-case":
        invalid("unknown document kind")
    if "derived" in facts:
        invalid("raw case facts must not contain derived sections")
    for key in ("run", "scenario", "environment", "schedule", "attempts", "trace", "status"):
        if key not in facts:
            invalid("missing section " + key)
    _run(facts["run"])
    _scenario(facts["scenario"], facts["run"]["configuration"])
    _environment(facts["environment"])
    _schedule(facts["schedule"], facts["run"]["configuration"])
    _attempts(
        facts["attempts"],
        facts["schedule"],
        facts["run"]["configuration"],
        facts["scenario"],
    )
    _trace(facts["trace"])
    _events(facts)
    _status(facts["status"])
    return facts


def _run(run):
    if not isinstance(run, dict):
        invalid("run is not an object")
    for key in ("id", "started_at", "surface", "endpoint", "observation_mode", "configuration"):
        if key not in run:
            invalid("run is missing " + key)
    for key in ("id", "started_at"):
        if not text(run.get(key)):
            invalid("run " + key + " must be text")
    if run["surface"] != OBSERVATION_SURFACE:
        invalid("unknown observation surface")
    if run["endpoint"] != OBSERVATION_ENDPOINT:
        invalid("unknown observation endpoint")
    if run["observation_mode"] not in OBSERVATION_MODES:
        invalid("unknown observation mode")
    configuration = run["configuration"]
    if not isinstance(configuration, dict):
        invalid("run configuration is not an object")
    for key in (
        "attempts",
        "warmup_attempts",
        "concurrency",
        "profile",
        "workflow",
        "network",
        "deadline_ms",
        "observation_window_ms",
        "seed",
        "observation_mode",
        "cadence_ms",
    ):
        if key not in configuration:
            invalid("run configuration is missing " + key)
    if configuration["concurrency"] != 1:
        invalid("only sequential execution is supported")
    if not count(configuration["attempts"]) or configuration["attempts"] < 1:
        invalid("run configuration attempts must be a positive integer")
    if not count(configuration["warmup_attempts"]):
        invalid("run configuration warmup_attempts must be a non-negative integer")
    if not text(configuration["profile"]):
        invalid("run configuration profile must be text")
    if configuration["workflow"] not in WORKFLOWS:
        invalid("unknown workflow")
    if configuration["observation_mode"] not in OBSERVATION_MODES:
        invalid("unknown configuration observation mode")
    if configuration["observation_mode"] != run["observation_mode"]:
        invalid("run observation mode disagrees with its configuration")
    if not number(configuration["deadline_ms"]) or configuration["deadline_ms"] <= 0:
        invalid("run deadline_ms must be a positive number")
    if not number(configuration["observation_window_ms"]):
        invalid("run observation_window_ms must be a number")
    if not count(configuration["cadence_ms"]) or configuration["cadence_ms"] < 1:
        invalid("run cadence_ms must be a positive integer")
    if not isinstance(configuration["seed"], (int, str)):
        invalid("run seed must be an integer or text")
    try:
        expected_network = network_settings(
            configuration["network"]["profile"], configuration["network"]["link"]
        )
    except (KeyError, TypeError, ValueError):
        invalid("unknown benchmark network settings")
    if configuration["network"] != expected_network:
        invalid("run network settings are not canonical")
    fingerprint = run.get("fingerprint")
    if not isinstance(fingerprint, dict):
        invalid("run fingerprint is not an object")
    build = fingerprint.get("build")
    if not isinstance(build, dict) or not text(build.get("revision")) or not text(
        build.get("fingerprint")
    ):
        invalid("run fingerprint requires a build identity")
    schemas = fingerprint.get("schemas")
    if not isinstance(schemas, dict):
        invalid("run fingerprint requires schemas")
    if schemas.get("case") != SCHEMA_VERSION or schemas.get("workload") != SCENARIO_VERSION:
        unsupported(
            f"fingerprint schemas {schemas.get('case')!r}/{schemas.get('workload')!r}; "
            f"expected {SCHEMA_VERSION}/{SCENARIO_VERSION}"
        )
    for key in ("fixture", "platform", "limits", "clock", "sampling"):
        if not isinstance(fingerprint.get(key), dict):
            invalid("run fingerprint " + key + " is not an object")
    for key in ("source", "unit", "verified"):
        if key not in fingerprint["clock"]:
            invalid("run fingerprint clock is missing " + key)


def _scenario(scenario, configuration):
    if not isinstance(scenario, dict):
        invalid("scenario is not an object")
    if scenario.get("id") != SCENARIO_ID:
        invalid("unknown scenario identity")
    version = scenario.get("version")
    if version != SCENARIO_VERSION:
        unsupported(f"scenario version {version!r}; expected {SCENARIO_VERSION}")
    if scenario.get("seed") != str(configuration["seed"]):
        invalid("scenario seed disagrees with the run configuration")
    if scenario.get("deadline_ms") != configuration["deadline_ms"]:
        invalid("scenario deadline_ms disagrees with the run configuration")
    if scenario.get("observation_window_ms") != configuration["observation_window_ms"]:
        invalid("scenario observation_window_ms disagrees with the run configuration")
    if scenario.get("network") != configuration["network"]:
        invalid("scenario network disagrees with the run configuration")
    workload = scenario.get("workload")
    if not isinstance(workload, dict):
        invalid("scenario workload is not an object")
    for key in ("updates", "gap_ms", "delivery", "workflow"):
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
    if workload["delivery"] != "immediate":
        invalid("the current source-driven scenario requires immediate delivery")
    if workload["workflow"] != configuration["workflow"]:
        invalid("scenario workflow disagrees with the run configuration")
    if "controls" in workload and not isinstance(workload["controls"], dict):
        invalid("scenario workload controls must be an object")
    response = scenario.get("response")
    if not isinstance(response, dict):
        invalid("scenario response is not an object")
    for key in ("start_ms", "final_ms", "delay_ms", "duplicate_final_ms", "final_text"):
        if key not in response:
            invalid("scenario response is missing " + key)
    for key in ("start_ms", "final_ms", "delay_ms", "duplicate_final_ms"):
        if not number(response[key]):
            invalid("scenario response " + key + " must be a number")
    if not text(response["final_text"]):
        invalid("scenario response final_text must be text")
    if response["final_text"] != "".join(updates):
        invalid("scenario response final_text must equal the scripted updates")
    last_update = response["start_ms"] + workload["gap_ms"] * (len(updates) - 1)
    if response["final_ms"] < last_update:
        invalid("scenario response final_ms precedes its last update")


def _environment(environment):
    if not isinstance(environment, dict):
        invalid("environment is not an object")
    for key in ("artifacts", "topology", "limits", "versions", "fixture", "clock"):
        if key not in environment:
            invalid("environment is missing " + key)
    for key in ("artifacts", "topology", "limits", "versions", "fixture"):
        if not isinstance(environment[key], dict):
            invalid("environment " + key + " is not an object")
    artifacts = environment["artifacts"]
    if not artifacts or not all(text(key) and text(value) for key, value in artifacts.items()):
        invalid("environment artifacts must map names to identifiers")
    topology = environment["topology"]
    if topology.get("kind") != "single-machine":
        unsupported("topology: only a single-machine capture is supported")
    if topology.get("clock") != "CLOCK_MONOTONIC":
        unsupported("topology clock; one CLOCK_MONOTONIC timeline is required")
    clock = environment["clock"]
    for key in ("source", "unit", "boot_id_verified", "namespace_verified", "producers"):
        if key not in clock:
            invalid("environment clock is missing " + key)
    if clock["source"] != "CLOCK_MONOTONIC":
        unsupported("clock source; only CLOCK_MONOTONIC is supported")
    if clock["unit"] != "ns":
        unsupported("clock unit; only integer nanoseconds are supported")
    if type(clock["boot_id_verified"]) is not bool or type(clock["namespace_verified"]) is not bool:
        invalid("clock verification flags must be booleans")
    producers = clock["producers"]
    if not isinstance(producers, list):
        invalid("clock producers must be a list")
    lifetimes = set()
    for producer in producers:
        if not isinstance(producer, dict) or not text(producer.get("producer")):
            invalid("clock producer entries need a producer role")
        if not text(producer.get("lifetime")):
            invalid("clock producer entries need a lifetime")
        if type(producer.get("verified")) is not bool:
            invalid("clock producer verified must be a boolean")
        if producer.get("clock") not in (None, clock["source"]):
            unsupported("clock source: a capture accepts one clock")
        if producer.get("unit") not in (None, clock["unit"]):
            unsupported("clock unit: a capture accepts one unit")
        if producer["lifetime"] in lifetimes:
            invalid("clock producer lifetime is duplicated")
        lifetimes.add(producer["lifetime"])
        if "reason" in producer and not isinstance(producer["reason"], str):
            invalid("clock producer reason must be text")
    if "network_probe" in environment:
        _network_probe(environment["network_probe"])
    if "network_counters" in environment and not isinstance(environment["network_counters"], dict):
        invalid("environment network_counters must be an object")
    for key in ("sampler", "resources"):
        if key in environment and not isinstance(environment[key], dict):
            invalid("environment " + key + " must be an object")
    origin = environment.get("capture_origin_ns")
    if origin is not None and not count(origin):
        invalid("capture_origin_ns must be a non-negative integer")


def _network_probe(probe):
    if not isinstance(probe, dict):
        invalid("network probe is not an object")
    for key in ("reference_ms", "host_ms", "client_ms"):
        samples = probe.get(key)
        if samples is None:
            continue
        if (
            not isinstance(samples, list)
            or len(samples) != NETWORK_PROBE_COUNT
            or not all(number(sample) and sample > 0 for sample in samples)
        ):
            invalid("network probe requires positive TCP RTT samples for " + key)


def _schedule(schedule, configuration):
    if not isinstance(schedule, dict):
        invalid("schedule is not an object")
    for key in ("seed", "rule", "slots"):
        if key not in schedule:
            invalid("schedule is missing " + key)
    if schedule["seed"] != str(configuration["seed"]):
        invalid("schedule seed disagrees with the run configuration")
    if schedule["rule"] != "alternating":
        invalid("schedule rule must be alternating")
    slots = schedule["slots"]
    if not isinstance(slots, list) or not slots:
        invalid("schedule slots must be a nonempty list")
    seen = set()
    for slot in slots:
        if not isinstance(slot, dict):
            invalid("schedule slot is not an object")
        for key in ("slot", "phase", "arm", "pair", "order", "planned_start_ms"):
            if key not in slot:
                invalid("schedule slot is missing " + key)
        if not count(slot["slot"]):
            invalid("schedule slot ordinal must be a non-negative integer")
        if slot["slot"] in seen:
            invalid("schedule slot ordinal is duplicated")
        seen.add(slot["slot"])
        if slot["phase"] not in PHASES:
            invalid("schedule slot has an unknown phase")
        if slot["arm"] not in SLOT_ARMS:
            invalid("schedule slot has an unknown arm")
        if not text(slot["pair"]):
            invalid("schedule slot pair must be text")
        if slot["order"] not in (0, 1):
            invalid("schedule slot order must be 0 or 1")
        if not number(slot["planned_start_ms"]):
            invalid("schedule slot planned_start_ms must be a number")
    measured = [slot for slot in slots if slot["phase"] == "measured"]
    warmup = [slot for slot in slots if slot["phase"] == "warmup"]
    if len(measured) != configuration["attempts"]:
        invalid("schedule measured slots disagree with the run configuration attempts")
    if len(warmup) != configuration["warmup_attempts"]:
        invalid("schedule warmup slots disagree with the run configuration warmup_attempts")
    workflow = configuration["workflow"]
    if workflow == "mixed-load":
        for phase in PHASES:
            phase_slots = [slot for slot in slots if slot["phase"] == phase]
            if len(phase_slots) % 2:
                invalid("mixed load requires an even number of slots in each phase")
            _counterbalanced(phase_slots, configuration["seed"])
        measured_arms = Counter(slot["arm"] for slot in measured)
        if measured_arms["idle"] != measured_arms["bulk"]:
            invalid("mixed load arms must have equal measured slot counts")
        if _per_arm_p95(configuration):
            if len(measured) < 40:
                invalid("per-arm P95 requires at least 40 planned measured slots")
            for arm in ARMS:
                if measured_arms[arm] < 20:
                    invalid("per-arm P95 requires at least 20 planned slots per arm")
    else:
        if any(slot["arm"] != ARM_WATCH for slot in slots):
            invalid("watch and recovery cases use the watch arm")
        if len({slot["arm"] for slot in slots}) != 1:
            invalid("non-mixed cases use a single watch arm")


def _per_arm_p95(configuration):
    return configuration.get("per_arm_p95") is True or configuration.get("p95_scope") == "per-arm"


def _counterbalanced(slots, seed):
    groups = {}
    for slot in slots:
        groups.setdefault(slot["pair"], []).append(slot)
    ordered = sorted(groups.items(), key=lambda item: min(entry["slot"] for entry in item[1]))
    first = "idle" if seed_parity(seed) == 0 else "bulk"
    other = "bulk" if first == "idle" else "idle"
    for index, (_pair, members) in enumerate(ordered):
        if len(members) != 2:
            invalid("mixed load requires exactly two slots per pair")
        expected = (first, other) if index % 2 == 0 else (other, first)
        by_order = sorted(members, key=lambda entry: entry["order"])
        if [entry["order"] for entry in by_order] != [0, 1]:
            invalid("mixed load pair slots need order 0 and 1")
        if tuple(entry["arm"] for entry in by_order) != expected:
            invalid("mixed load pair order is not counterbalanced")


def _attempts(attempts, schedule, configuration, scenario):
    if not isinstance(attempts, list):
        invalid("attempts must be a list")
    slots = {slot["slot"]: slot for slot in schedule["slots"]}
    identities = set()
    seen_slots = set()
    for attempt in attempts:
        if not isinstance(attempt, dict):
            invalid("attempt is not an object")
        for key in (
            "id",
            "slot",
            "phase",
            "arm",
            "status",
            "failure",
            "setup",
            "source",
            "outputs",
            "observation",
        ):
            if key not in attempt:
                invalid("attempt is missing " + key)
        if not text(attempt["id"]) or attempt["id"] in identities:
            invalid("attempt identity is missing or duplicated")
        identities.add(attempt["id"])
        if not count(attempt["slot"]) or attempt["slot"] not in slots:
            invalid("attempt slot is not in the persisted schedule")
        if attempt["slot"] in seen_slots:
            invalid("attempt slot is duplicated")
        seen_slots.add(attempt["slot"])
        slot = slots[attempt["slot"]]
        if (
            attempt["phase"] != slot["phase"]
            or attempt["arm"] != slot["arm"]
            or attempt.get("pair_id") != slot["pair"]
        ):
            invalid("attempt identity disagrees with its scheduled slot")
        if attempt["status"] not in SLOT_STATES:
            invalid("unknown attempt status")
        _attempt(attempt, configuration)
    if len(attempts) > len(slots):
        invalid("more attempts than scheduled slots")
    workflow = configuration["workflow"]
    if workflow == "mixed-load":
        expected = _expected_reads(scenario)
        for attempt in attempts:
            if attempt["arm"] != "bulk" or attempt["status"] not in (
                "measurement_entered",
                "finished",
            ):
                continue
            _attempt_reads(attempt, expected)


def slot_phase_status(status):
    order = {
        "planned": 0,
        "not_started": 0,
        "started": 1,
        "setup_failed": 1,
        "measurement_entered": 2,
        "finished": 3,
        "interrupted": 2,
    }
    return order[status]


def _expected_reads(scenario):
    """Scheduled read counts declared in the workload before execution.

    The current case-facts shape puts `history`, `attachment`, `pairing` and
    `exposure` directly on `scenario.workload` (that is what `scenario()`
    produces). The fixture mirrors those declarations under `controls` as a
    migration aid, so both locations are accepted with the top level winning.
    """
    workload = scenario["workload"]
    controls = workload.get("controls") or {}
    history = workload.get("history") or controls.get("history") or {}
    attachment = workload.get("attachment") or controls.get("attachment") or {}
    return {
        "history": len(history.get("start_ms") or []),
        "attachment": len(attachment.get("start_ms") or []),
    }


def _attempt(attempt, configuration):
    if not isinstance(attempt["failure"], dict):
        invalid("attempt failure must be an object")
    attribution = attempt["failure"].get("attribution")
    if attribution not in (None, *FAILURE_ATTRIBUTIONS):
        invalid("unknown failure attribution")
    if "reason" in attempt["failure"] and not isinstance(attempt["failure"]["reason"], str):
        invalid("failure reason must be text")
    if not isinstance(attempt["setup"], dict) or not isinstance(
        attempt["setup"].get("errors"), list
    ):
        invalid("attempt setup requires an errors list")
    if any(not isinstance(item, str) for item in attempt["setup"]["errors"]):
        invalid("setup errors must be text")
    _source(attempt["source"])
    _outputs(attempt["outputs"], attempt["source"])
    _observation(attempt["observation"], configuration, attempt["status"])
    if "recovery" in attempt:
        _recovery(attempt["recovery"])
    if "reads" in attempt:
        _reads(attempt["reads"])
    if "trace" in attempt:
        _attempt_trace(attempt["trace"])
    if attempt["status"] in ("finished", "measurement_entered") and not attempt["outputs"]:
        # A measured attempt may legitimately have no visible output; the
        # observation window, not an invented record, carries that fact.
        pass


def _source(source):
    if not isinstance(source, dict):
        invalid("attempt source is not an object")
    times = source.get("times_ms")
    sizes = source.get("sizes")
    if not isinstance(times, list) or not all(number(value) for value in times):
        invalid("attempt source times_ms must be numbers")
    if any(later < earlier for earlier, later in pairwise(times)):
        invalid("attempt source times must be ordered")
    if not isinstance(sizes, list) or not all(count(value) for value in sizes):
        invalid("attempt source sizes must be non-negative integers")
    if len(times) != len(sizes):
        invalid("attempt source times and sizes must have equal length")


def _outputs(outputs, source):
    if not isinstance(outputs, list):
        invalid("attempt outputs must be a list")
    previous = (-math.inf, -1)
    sizes = source["sizes"]
    times = source["times_ms"]
    cumulative = list(accumulate(sizes))
    for event in outputs:
        if not isinstance(event, dict):
            invalid("attempt output is not an object")
        for key in ("at_ms", "ordinal", "kind", "bytes", "prefix_bytes", "correct"):
            if key not in event:
                invalid("attempt output is missing " + key)
        if not number(event["at_ms"]):
            invalid("attempt output at_ms must be a number")
        if not count(event["ordinal"]):
            invalid("attempt output ordinal must be a non-negative integer")
        if event["kind"] not in OUTPUT_KINDS:
            invalid("unknown output kind")
        if not count(event["bytes"]):
            invalid("attempt output bytes must be a non-negative integer")
        if not count(event["prefix_bytes"]):
            invalid("attempt output prefix_bytes must be a non-negative integer")
        if type(event["correct"]) is not bool:
            invalid("attempt output correct must be a boolean")
        if "block" in event and not text(event["block"]):
            invalid("attempt output block must be text")
        order = (event["at_ms"], event["ordinal"])
        if order <= previous:
            invalid("attempt outputs must be ordered by time and ordinal")
        previous = order
        index = bisect_right(times, event["at_ms"])
        emitted = cumulative[index - 1] if index else 0
        if event["prefix_bytes"] > emitted:
            invalid("visible prefix exceeds the emitted source bytes")


def _observation(observation, configuration, status):
    if not isinstance(observation, dict):
        invalid("attempt observation is not an object")
    for key in ("started_ms", "ended_ms", "cutoff_ms", "early_stop"):
        if key not in observation:
            invalid("attempt observation is missing " + key)
    for key in ("started_ns", "ended_ns", "cutoff_ns"):
        if key in observation and observation[key] is not None and not count(observation[key]):
            invalid("attempt observation " + key + " must be a non-negative integer or null")
    if observation.get("started_ns") is not None and observation.get("ended_ns") is not None:
        if observation["ended_ns"] < observation["started_ns"]:
            invalid("attempt absolute observation end precedes its start")
    if observation.get("cutoff_ns") is not None:
        if (
            observation.get("started_ns") is not None
            and observation["cutoff_ns"] < observation["started_ns"]
        ):
            invalid("attempt absolute cutoff precedes its start")
        if (
            observation.get("ended_ns") is not None
            and observation["cutoff_ns"] > observation["ended_ns"]
        ):
            invalid("attempt absolute cutoff exceeds its observation end")
    for key in ("started_ms", "ended_ms", "cutoff_ms"):
        value = observation[key]
        if value is None:
            if status not in ("planned", "not_started"):
                invalid("attempt observation " + key + " is required after start")
            continue
        if not number(value):
            invalid("attempt observation " + key + " must be a number or null")
    started = observation.get("started_ms")
    ended = observation.get("ended_ms")
    cutoff = observation.get("cutoff_ms")
    if started is not None:
        if ended is not None and ended < started:
            invalid("attempt observation end precedes its start")
        if cutoff is not None:
            if cutoff < started:
                invalid("attempt cutoff precedes its start")
            if cutoff > configuration["deadline_ms"]:
                invalid("attempt cutoff must not exceed the deadline")
    if observation["early_stop"] not in EARLY_STOPS:
        invalid("unknown observation early_stop")


def _recovery(recovery):
    if not isinstance(recovery, dict):
        invalid("attempt recovery is not an object")
    for key in ("blackout_start_ms", "restored_ms", "milestones"):
        if key not in recovery:
            invalid("attempt recovery is missing " + key)
    if not number(recovery["blackout_start_ms"]) or not number(recovery["restored_ms"]):
        invalid("recovery boundaries must be numbers")
    if recovery["restored_ms"] <= recovery["blackout_start_ms"]:
        invalid("recovery restoration must follow its blackout start")
    milestones = recovery["milestones"]
    if not isinstance(milestones, dict):
        invalid("recovery milestones must be an object")
    for name, milestone in milestones.items():
        if name not in RECOVERY_MILESTONES:
            invalid("unknown recovery milestone " + name)
        if not isinstance(milestone, dict) or milestone.get("state") not in MILESTONE_STATES:
            invalid("recovery milestone needs a known state")
        if "at_ms" in milestone and not number(milestone["at_ms"]):
            invalid("recovery milestone at_ms must be a number")
        if milestone["state"] == "observed" and "at_ms" not in milestone:
            invalid("an observed recovery milestone needs at_ms")
        if "via" in milestone and milestone["via"] not in ("preview", "terminal", "history"):
            invalid("unknown recovery catch-up path")


def _reads(reads):
    if not isinstance(reads, list):
        invalid("attempt reads must be a list")
    identities = set()
    for read in reads:
        if not isinstance(read, dict):
            invalid("read observation is not an object")
        for key in (
            "id",
            "op",
            "scheduled_at_ms",
            "started_at_ms",
            "finished_at_ms",
            "deadline_ms",
            "pages",
            "messages",
            "bytes_expected",
            "bytes_verified",
            "physical_bytes",
            "route",
            "outcome",
            "verified",
            "token_repeats",
            "corrupt",
        ):
            if key not in read:
                invalid("read observation is missing " + key)
        if not text(read["id"]) or read["id"] in identities:
            invalid("read identity is missing or duplicated")
        identities.add(read["id"])
        if read["op"] not in ("history", "attachment"):
            invalid("unknown read operation")
        for key in ("scheduled_at_ms", "started_at_ms", "finished_at_ms", "deadline_ms"):
            value = read[key]
            if value is not None and not number(value):
                invalid("read " + key + " must be a number or null")
        if read["started_at_ms"] is not None and read["started_at_ms"] < read["scheduled_at_ms"]:
            invalid("a read cannot start before its scheduled time")
        if (
            read["started_at_ms"] is not None
            and read["finished_at_ms"] is not None
            and read["finished_at_ms"] < read["started_at_ms"]
        ):
            invalid("a read cannot finish before it starts")
        for key in ("pages", "messages", "bytes_expected", "bytes_verified", "physical_bytes"):
            if not count(read[key]):
                invalid("read " + key + " must be a non-negative integer")
        if read["route"] not in READ_ROUTES:
            invalid("unknown read route")
        if read["outcome"] not in READ_OUTCOMES:
            invalid("unknown read outcome")
        if type(read["verified"]) is not bool or type(read["corrupt"]) is not bool:
            invalid("read verified and corrupt must be booleans")
        if not count(read["token_repeats"]):
            invalid("read token_repeats must be a non-negative integer")
        if read["outcome"] == "completed" and read["bytes_verified"] != read["bytes_expected"]:
            invalid("a completed read must verify the expected bytes")
        code = read.get("error_code")
        if code is not None and (read["outcome"] != "product_error" or not valid_error_code(code)):
            invalid("invalid read error code")


def _attempt_reads(attempt, expected):
    reads = attempt.get("reads")
    if not isinstance(reads, list):
        invalid("a mixed-load bulk attempt requires read observations")
    if expected is None:
        return
    history = [read for read in reads if read["op"] == "history"]
    attachments = [read for read in reads if read["op"] == "attachment"]
    if len(history) != expected["history"]:
        invalid("mixed load requires an outcome for every scheduled history read")
    if len(attachments) != expected["attachment"]:
        invalid("mixed load requires an outcome for every scheduled attachment read")


def _attempt_trace(trace):
    if not isinstance(trace, dict):
        invalid("attempt trace is not an object")
    if "coverage" in trace:
        coverage = trace["coverage"]
        if not isinstance(coverage, dict):
            invalid("attempt trace coverage must be an object")
        for name, entry in coverage.items():
            if name not in COVERAGE_PATHS:
                invalid("unknown coverage path " + name)
            if not isinstance(entry, dict):
                invalid("coverage path entry must be an object")
            for key in ("required", "observed"):
                values = entry.get(key)
                if not isinstance(values, list) or any(not text(item) for item in values):
                    invalid("coverage " + key + " must be a list of event names")
            if type(entry.get("complete")) is not bool:
                invalid("coverage complete must be a boolean")
            if "inapplicable" in entry:
                inapplicable = entry["inapplicable"]
                if not isinstance(inapplicable, dict) or any(
                    not text(stage) or not text(reason) for stage, reason in inapplicable.items()
                ):
                    invalid("inapplicable stages require an evidence reason")
    if "loss" in trace and not isinstance(trace["loss"], dict):
        invalid("attempt trace loss must be an object")


def _trace(trace):
    if not isinstance(trace, dict):
        invalid("trace is not an object")
    if "complete" not in trace or type(trace["complete"]) is not bool:
        invalid("trace complete must be a boolean")
    for key in ("path", "checksum", "events_path"):
        if trace.get(key) is not None and not isinstance(trace[key], str):
            invalid("trace " + key + " must be text or null")
    for key in ("bytes", "cap_bytes"):
        if trace.get(key) is not None and not count(trace[key]):
            invalid("trace " + key + " must be a non-negative integer")
    if "losses" in trace:
        losses = trace["losses"]
        if not isinstance(losses, dict):
            invalid("trace losses must be an object")
        for key, value in losses.items():
            if not text(key) or not count(value):
                invalid("trace loss counters must be named non-negative integers")
    if "coverage" in trace and not isinstance(trace["coverage"], dict):
        invalid("trace coverage must be an object")


def _events(facts):
    events = facts.get("events")
    if events is None:
        trace = facts["trace"]
        events = trace.get("events") if isinstance(trace, dict) else None
    if events is None:
        return
    if not isinstance(events, list):
        invalid("typed events must be a list")
    for event in events:
        if not isinstance(event, dict):
            invalid("typed event is not an object")
        for key in ("producer", "event", "ts", "data"):
            if key not in event:
                invalid("typed event is missing " + key)
        if not text(event["producer"]):
            invalid("typed event producer must be text")
        if event["event"] not in EVENT_PRODUCERS:
            invalid("unknown typed event " + str(event["event"]))
        if event["producer"] not in EVENT_PRODUCERS[event["event"]]:
            invalid("typed event producer does not own its boundary")
        if not count(event["ts"]):
            invalid("typed event ts must be a non-negative integer")
        if not isinstance(event["data"], dict):
            invalid("typed event data must be an object")
        for field in event["data"]:
            if field in FORBIDDEN_FIELDS:
                invalid("typed event carries a forbidden field")
        if "v" in event and event["v"] != 1:
            unsupported(f"event schema {event['v']!r}; expected 1")


def _status(status):
    if not isinstance(status, dict) or status.get("state") not in STATUSES:
        invalid("status needs a known state")
    reasons = status.get("reasons")
    if not isinstance(reasons, list) or any(not isinstance(item, str) for item in reasons):
        invalid("status reasons must be text")
    if status["state"] != "complete" and not reasons:
        invalid("incomplete or invalid status must state its reasons")
    cleanup = status.get("cleanup")
    if not isinstance(cleanup, dict) or cleanup.get("result") not in CLEANUP_RESULTS:
        invalid("cleanup needs a verified or unverified result")
    if cleanup["result"] != "verified" and not text(cleanup.get("reason")):
        invalid("unverified cleanup must state a reason")


# ---------------------------------------------------------------------------
# Derivation
# ---------------------------------------------------------------------------


def derive(facts):
    """Pure derivation of every business row, aggregate and verdict."""
    facts = copy.deepcopy(facts)
    clock = _clock_quality(facts["environment"]["clock"])
    events = _event_list(facts)
    coverage = _coverage(facts, events)
    trace = _trace_quality(facts["trace"], coverage)
    scenario = facts["scenario"]
    slots = {slot["slot"]: slot for slot in facts["schedule"]["slots"]}
    raw_by_slot = {attempt["slot"]: attempt for attempt in facts["attempts"]}
    attempts = []
    for ordinal in sorted(slots):
        slot = slots[ordinal]
        raw = raw_by_slot.get(ordinal)
        attempts.append(
            _derive_attempt(
                raw,
                slot,
                scenario=scenario,
                clock_verified=clock["verified"],
                events=events,
                attempts=facts["attempts"],
            )
        )
    measured = [item for item in attempts if item["phase"] == "measured"]
    by_outcome = Counter(item["outcome"] for item in measured)
    outcomes = {
        "counts": {name: by_outcome.get(name, 0) for name in OUTCOMES},
        "measured": len(measured),
        "correct_by_deadline": by_outcome.get("completed", 0),
        "success_share": (
            by_outcome.get("completed", 0) / len(measured) if measured else None
        ),
        "warmup": {
            "attempts": sum(item["phase"] == "warmup" for item in attempts),
            "completed": sum(
                item["phase"] == "warmup" and item["outcome"] == "completed"
                for item in attempts
            ),
        },
    }
    populations = _populations(attempts, measured)
    latency = _latency(measured, facts)
    streaming = _streaming(measured, scenario)
    diagnostics = _diagnostics(measured, scenario)
    workflow = _workflow(attempts, measured, facts)
    attribution = _attribution(events, clock, facts)
    quality = _quality(facts, clock, coverage, trace)
    exemplars = _exemplars(measured, facts, trace, coverage, quality)
    for item in attempts:
        for key in ("_raw", "_outputs", "_source_times", "_recovery"):
            item.pop(key, None)
    return {
        "attempts": attempts,
        "outcomes": outcomes,
        "populations": populations,
        "latency": latency,
        "streaming": streaming,
        "diagnostics": diagnostics,
        "workflow_results": workflow,
        "attribution": attribution,
        "coverage": coverage,
        "trace": trace,
        "quality": quality,
        "trace_index": exemplars,
        "metric_catalog": METRIC_CATALOG,
    }


def _clock_quality(clock):
    reasons = []
    verified = clock["boot_id_verified"] and clock["namespace_verified"]
    if not clock["boot_id_verified"]:
        reasons.append("boot_id_unverified")
    if not clock["namespace_verified"]:
        reasons.append("namespace_unverified")
    if not clock["producers"]:
        reasons.append("no_producer_lifetimes")
        verified = False
    unverified = [item for item in clock["producers"] if not item["verified"]]
    if unverified:
        reasons.append("producer_unverified")
        verified = False
    return {
        "verified": verified,
        "source": clock["source"],
        "unit": clock["unit"],
        "producers": [
            {
                "producer": item["producer"],
                "lifetime": item["lifetime"],
                "verified": item["verified"],
                "reason": item.get("reason"),
            }
            for item in clock["producers"]
        ],
        "reason": reasons[0] if reasons else None,
        "reasons": reasons,
        "restarted": len(
            {(item["producer"], item["lifetime"]) for item in clock["producers"]}
        )
        > 1
        and len({item["lifetime"] for item in clock["producers"]}) > 1,
    }


def _derive_attempt(raw, slot, *, scenario, clock_verified, events, attempts=()):
    if raw is None:
        return _empty_attempt(slot, implicit=True)
    status = raw["status"]
    observation = raw["observation"]
    started = observation.get("started_ms")
    ended = observation.get("ended_ms")
    cutoff = observation.get("cutoff_ms")
    deadline = scenario["deadline_ms"]
    window = scenario["observation_window_ms"]
    if started is None:
        return _empty_attempt(slot, raw=raw)
    if cutoff is None:
        cutoff = min(deadline, ended if ended is not None else deadline)
    snapshot = _stream_snapshot(raw, cutoff)
    first_text_ms = _first_text(raw["outputs"], ended)
    integrity = _terminal_integrity(raw["outputs"], ended, cutoff, window)
    deadline_visible = integrity["terminal_ms"] is not None
    outcome, failure_class = _classify(
        raw,
        snapshot,
        first_text_ms,
        integrity,
        started=started,
        ended=ended,
        cutoff=cutoff,
        deadline=deadline,
    )
    correct = outcome == "completed" and deadline_visible and integrity["verdict"] == "pass"
    cutoff_ns = observation.get("cutoff_ns")
    evidence = _pending_evidence(events, raw["id"], cutoff_ns, attempts)
    attribution = _attempt_attribution(events, raw["id"], attempts)
    return {
        "id": raw["id"],
        "slot": slot["slot"],
        "phase": slot["phase"],
        "arm": slot["arm"],
        "pair_id": slot["pair"],
        "status": status,
        "implicit": False,
        "outcome": outcome,
        "failure_class": failure_class,
        "first_text_ms": first_text_ms,
        "terminal_ms": integrity["terminal_ms"],
        "final_ms": integrity["terminal_ms"],
        "late_terminal_ms": integrity.get("late_terminal_ms"),
        "visible_bytes": snapshot["visible_bytes"],
        "historical_visible_bytes": snapshot["historical_visible_bytes"],
        "emitted_bytes": snapshot["emitted_bytes"],
        "pending_bytes": snapshot["pending_bytes"],
        "peak_backlog_bytes": snapshot["peak_backlog_bytes"],
        "longest_pending_stall_ms": snapshot["longest_pending_stall_ms"],
        "since_last_advance_ms": snapshot["since_last_advance_ms"],
        "last_advance_ms": snapshot["last_advance_ms"],
        "snapshot_state": snapshot["state"],
        "deadline_visible": deadline_visible,
        "terminal_integrity": integrity["verdict"],
        "terminal_integrity_reason": integrity["reason"],
        "terminal_window_ms": window,
        "observed_through_ms": ended,
        "observed_first_text_ms": first_text_ms,
        "correct_by_deadline": correct,
        "clock_verified": clock_verified and raw.get("status") != "not_started",
        "last_evidenced_stage": evidence["stage"],
        "last_evidenced_owner": evidence.get("owner"),
        "pending_range": evidence["range"],
        "pending_evidence_reason": evidence.get("reason"),
        "attribution": attribution,
        "waterfall": _waterfall(events, raw["id"], cutoff_ns),
        "reads": _derive_reads(raw.get("reads", [])),
        "trace_coverage": _attempt_coverage(raw.get("trace", {})),
        "_raw": raw,
        "_outputs": raw["outputs"],
        "_source_times": raw["source"]["times_ms"],
        "_recovery": raw.get("recovery"),
    }


def _empty_attempt(slot, *, raw=None, implicit=False):
    return {
        "id": raw["id"] if raw else f"slot-{slot['slot']}",
        "slot": slot["slot"],
        "phase": slot["phase"],
        "arm": slot["arm"],
        "pair_id": slot["pair"],
        "status": raw["status"] if raw else "not_started",
        "implicit": implicit,
        "outcome": "not_started",
        "failure_class": None,
        "first_text_ms": None,
        "terminal_ms": None,
        "final_ms": None,
        "late_terminal_ms": None,
        "visible_bytes": None,
        "historical_visible_bytes": None,
        "emitted_bytes": None,
        "pending_bytes": None,
        "peak_backlog_bytes": None,
        "longest_pending_stall_ms": None,
        "since_last_advance_ms": None,
        "last_advance_ms": None,
        "snapshot_state": "inapplicable",
        "deadline_visible": False,
        "terminal_integrity": "missing",
        "terminal_integrity_reason": "attempt_not_started",
        "terminal_window_ms": None,
        "observed_through_ms": None,
        "observed_first_text_ms": None,
        "correct_by_deadline": False,
        "clock_verified": False,
        "last_evidenced_stage": "unavailable",
        "last_evidenced_owner": None,
        "pending_range": None,
        "pending_evidence_reason": "attempt_not_started",
        "attribution": {"intervals": [], "last_stage": "unavailable"},
        "reads": [],
        "trace_coverage": {},
    }


def _stream_snapshot(raw, limit):
    source = raw["source"]
    outputs = raw["outputs"]
    cutoff = limit
    if cutoff is None:
        return _snapshot_result(None, None, None, None, None, None, None, None, "unknown")
    times = source["times_ms"]
    sizes = source["sizes"]
    emissions = list(zip(times, sizes, strict=True))
    timeline = []
    for at_ms, size in emissions:
        if at_ms <= cutoff:
            timeline.append((at_ms, 0, "emit", size, None))
    for event in outputs:
        if event["at_ms"] <= cutoff:
            timeline.append((event["at_ms"], 1, "output", 0, event))
    timeline.sort(key=lambda item: (item[0], item[1], item[4]["ordinal"] if item[4] else 0))
    hidden = 0
    emitted = 0
    states = {}
    historical = 0
    last_advance = None
    first_text = None
    backlog = 0
    peak = 0
    stalled_since = None
    longest = None
    for at_ms, _, kind, size, event in timeline:
        if kind == "emit":
            emitted += size
        elif event["kind"] == "hidden":
            hidden += event["bytes"]
        elif event["kind"] == "reset":
            block = event.get("block") or "__stream__"
            states[block] = 0
        elif event["kind"] in ("preview", "replace", "terminal") and event["correct"]:
            block = event.get("block") or "__stream__"
            before = sum(states.values())
            states[block] = event["prefix_bytes"]
            after = sum(states.values())
            if after > before:
                last_advance = at_ms
                if stalled_since is not None:
                    longest = max(longest or 0, at_ms - stalled_since)
                    stalled_since = None
            if after > historical:
                historical = after
            if after > 0 and first_text is None:
                first_text = at_ms
        effective_emitted = max(0, emitted - hidden)
        backlog = max(0, effective_emitted - sum(states.values()))
        peak = max(peak, backlog)
        if backlog > 0 and stalled_since is None:
            stalled_since = at_ms
        if backlog > 0 and stalled_since is not None:
            longest = max(longest or 0, at_ms - stalled_since)
    effective_emitted = max(0, emitted - hidden)
    visible_total = sum(states.values())
    backlog = max(0, effective_emitted - visible_total)
    peak = max(peak, backlog)
    if stalled_since is not None and backlog > 0:
        longest = max(longest or 0, cutoff - stalled_since)
    state = "measured"
    if effective_emitted == 0 and visible_total == 0:
        state = "inapplicable" if not times else "not_yet_visible"
    elif effective_emitted == 0:
        state = "unknown"
    elif visible_total == 0:
        state = "not_yet_visible"
    return _snapshot_result(
        visible_total,
        historical,
        effective_emitted,
        backlog,
        peak,
        longest,
        cutoff - last_advance if last_advance is not None else None,
        last_advance,
        state,
    )


def _snapshot_result(
    visible,
    historical,
    emitted,
    pending,
    peak,
    longest,
    since,
    last_advance,
    state,
):
    return {
        "visible_bytes": visible,
        "historical_visible_bytes": historical,
        "emitted_bytes": emitted,
        "pending_bytes": pending,
        "peak_backlog_bytes": peak,
        "longest_pending_stall_ms": None if longest is None else round(longest, 3),
        "since_last_advance_ms": None if since is None else round(since, 3),
        "last_advance_ms": last_advance,
        "state": state,
    }


def _first_text(outputs, ended):
    if ended is None:
        return None
    for event in outputs:
        if event["at_ms"] > ended:
            break
        if (
            event["kind"] in ("preview", "replace", "terminal")
            and event["correct"]
            and event["prefix_bytes"] > 0
        ):
            return event["at_ms"]
    return None


def _terminal_integrity(outputs, ended, cutoff, window):
    result = {
        "verdict": "unverified",
        "reason": None,
        "terminal_ms": None,
        "late_terminal_ms": None,
        "observed_through_ms": ended,
        "window_ms": window,
    }
    if ended is None:
        result["verdict"] = "missing"
        result["reason"] = "attempt_not_started"
        return result
    terminals = [
        event
        for event in outputs
        if event["kind"] == "terminal" and event["correct"] and event["at_ms"] <= ended
    ]
    bad_terminal = [
        event
        for event in outputs
        if event["kind"] == "terminal" and not event["correct"] and event["at_ms"] <= ended
    ]
    if not terminals:
        horizon = (cutoff or 0) + window
        result["verdict"] = "missing" if ended >= horizon else "unverified"
        result["reason"] = (
            "no_terminal_within_horizon" if ended >= horizon else "horizon_incomplete"
        )
        if bad_terminal:
            result["reason"] = "incorrect_terminal"
        return result
    first = terminals[0]
    if first["at_ms"] <= cutoff:
        result["terminal_ms"] = first["at_ms"]
    else:
        result["late_terminal_ms"] = first["at_ms"]
    window_end = first["at_ms"] + window
    violations = []
    for event in outputs:
        if event is first or event["at_ms"] > ended:
            continue
        after = (event["at_ms"], event["ordinal"]) > (first["at_ms"], first["ordinal"])
        if not after:
            continue
        if event["at_ms"] > window_end:
            continue
        if event["kind"] in ("preview", "replace", "reset", "terminal", "duplicate"):
            violations.append(event)
    if bad_terminal and (bad_terminal[0]["at_ms"], bad_terminal[0]["ordinal"]) > (
        first["at_ms"],
        first["ordinal"],
    ):
        violations.append(bad_terminal[0])
    if violations:
        kinds = {event["kind"] for event in violations}
        result["verdict"] = "fail"
        result["reason"] = (
            "duplicate_terminal"
            if kinds & {"terminal", "duplicate"}
            else "post_terminal_output"
        )
        return result
    if ended >= window_end:
        result["verdict"] = "pass"
        result["reason"] = "complete_integrity_window"
    else:
        result["verdict"] = "unverified"
        result["reason"] = "window_incomplete"
    return result


def _classify(raw, snapshot, first_text, integrity, *, started, ended, cutoff, deadline):
    status = raw["status"]
    attribution = raw["failure"].get("attribution")
    if status in ("planned", "not_started"):
        return "not_started", None
    if status == "setup_failed":
        return "setup_failed", attribution or "unknown"
    if status == "interrupted":
        return "interrupted", attribution or "unknown"
    if attribution == "product":
        return "product_error", "product"
    if integrity.get("terminal_ms") is not None:
        if integrity["verdict"] == "fail":
            if integrity["reason"] == "duplicate_terminal":
                return "duplicate_final", attribution or "unknown"
            return "incorrect_content", attribution or "unknown"
        if integrity["verdict"] == "unverified":
            return "interrupted", attribution or "unknown"
        return "completed", None
    if integrity.get("late_terminal_ms") is not None:
        return "late_reply", attribution or "unknown"
    if ended is not None and cutoff is not None and ended < cutoff:
        return "interrupted", attribution or "unknown"
    # An incorrect terminal that was not visible by the deadline is still an
    # incorrect product result rather than a timeout.
    if any(
        event["kind"] == "terminal"
        and not event["correct"]
        and event["at_ms"] <= (ended if ended is not None else cutoff)
        for event in raw["outputs"]
    ):
        return "incorrect_content", attribution or "unknown"
    visible_before = _first_text(raw["outputs"], cutoff)
    if visible_before is not None:
        return "timeout_after_text", attribution or "unknown"
    if snapshot["emitted_bytes"]:
        return "timeout_before_text", attribution or "unknown"
    return "product_error", attribution or "unknown"


def _populations(attempts, measured):
    def bucket(items):
        return {
            "total": len(items),
            "warmup": sum(item["phase"] == "warmup" for item in items),
            "measured": sum(item["phase"] == "measured" for item in items),
        }

    started = [item for item in attempts if slot_phase_status(item["status"]) >= 1]
    entered = [item for item in attempts if slot_phase_status(item["status"]) >= 2]
    finished = [item for item in attempts if item["status"] == "finished"]
    correct = [item for item in measured if item["correct_by_deadline"]]
    measured_entered = [item for item in entered if item["phase"] == "measured"]
    denominator = len(measured)
    return {
        "planned": bucket(attempts),
        "started": bucket(started),
        "measurement_entered": bucket(entered),
        "finished": bucket(finished),
        "on_time_correct": bucket(correct),
        "denominators": {
            "on_time_correct_over_planned": (
                len(correct) / denominator if denominator else None
            ),
            "on_time_correct_over_measurement_entered": (
                len(correct) / len(measured_entered) if measured_entered else None
            ),
        },
        "setup_failures": dict(
            sorted(
                Counter(
                    item["failure_class"] or "unknown"
                    for item in measured
                    if item["outcome"] == "setup_failed"
                ).items()
            )
        ),
        "not_started": sum(item["outcome"] == "not_started" for item in measured),
        "interrupted": sum(item["outcome"] == "interrupted" for item in measured),
    }


def _latency(measured, facts):
    first = sorted(
        float(item["first_text_ms"])
        for item in measured
        if item["first_text_ms"] is not None
    )
    final = sorted(
        float(item["terminal_ms"])
        for item in measured
        if item["outcome"] == "completed" and item["terminal_ms"] is not None
    )
    samples = {"first_text_ms": sample_entry(first), "final_ms": sample_entry(final)}
    result = {
        "samples": samples,
        "analysis": analysis.run_checks(
            {
                "outcomes": _outcomes_for_analysis(measured),
                "latency": {"samples": samples},
            }
        ),
    }
    if facts["run"]["configuration"]["workflow"] == "mixed-load":
        arms = {}
        for arm in ARMS:
            selected = [item for item in measured if item["arm"] == arm]
            arms[arm] = {
                "measured": len(selected),
                "samples": {
                    "first_text_ms": sample_entry(
                        sorted(
                            float(item["first_text_ms"])
                            for item in selected
                            if item["first_text_ms"] is not None
                        )
                    ),
                    "final_ms": sample_entry(
                        sorted(
                            float(item["terminal_ms"])
                            for item in selected
                            if item["outcome"] == "completed"
                            and item["terminal_ms"] is not None
                        )
                    ),
                },
            }
        result["arms"] = arms
        if _per_arm_p95(facts["run"]["configuration"]):
            result["per_arm_p95"] = {
                arm: (entry["samples"]["first_text_ms"]["quantiles"].get("p95"))
                is not None
                for arm, entry in arms.items()
            }
    return result


def _outcomes_for_analysis(measured):
    by_outcome = Counter(item["outcome"] for item in measured)
    return {
        "measured": len(measured),
        "correct_by_deadline": by_outcome.get("completed", 0),
        "success_share": (
            by_outcome.get("completed", 0) / len(measured) if measured else None
        ),
    }


def _segment_values(attempt, sizes):
    """Times from each emitted increment to the prefix that first covers it."""
    times = attempt.get("_source_times") or []
    values = []
    cumulative = list(accumulate(sizes))
    for end_bytes, emitted_at in zip(cumulative, times, strict=False):
        for event in attempt["_outputs"]:
            if (
                event["kind"] in ("preview", "replace", "terminal")
                and event["correct"]
                and event["prefix_bytes"] >= end_bytes
            ):
                values.append(round(event["at_ms"] - emitted_at, 3))
                break
    return values


def _streaming(measured, scenario):
    sizes = source_sizes(scenario)
    final_bytes = max(1, sum(sizes))
    rows = []
    for item in measured:
        outputs = item.get("_outputs")
        times = item.get("_source_times")
        if not outputs or times is None:
            continue
        segments = _segment_values(
            {"_outputs": outputs, "_source_times": times}, sizes
        )
        rows.append(
            {
                "max_display_delay_ms": max(segments) if segments else None,
                "longest_pending_stall_ms": item["longest_pending_stall_ms"],
                "peak_backlog_bytes": item["peak_backlog_bytes"],
                "pending_bytes_at_cutoff": item["pending_bytes"],
                "largest_output_share": max(event["bytes"] for event in outputs)
                / final_bytes,
                "final_output_share": outputs[-1]["bytes"] / final_bytes,
                "largest_burst_share": _largest_burst(outputs, BURST_WINDOW_MS)
                / final_bytes,
                "zero_preview": len(outputs) == 1,
            }
        )
    result = {}
    for metric in (*STREAMING_METRICS, "largest_output_share", "final_output_share"):
        values = sorted(
            float(row[metric]) for row in rows if row.get(metric) is not None
        )
        result[metric] = sample_entry(values)
    result["largest_burst_share"] = sample_entry(
        sorted(float(row["largest_burst_share"]) for row in rows)
    )
    result["zero_preview"] = sum(row["zero_preview"] for row in rows)
    result["attempts"] = len(rows)
    return result

def _diagnostics(measured, scenario):
    sizes = source_sizes(scenario)
    values = []
    for item in measured:
        if "_outputs" not in item:
            continue
        attempt = {"_outputs": item["_outputs"], "_source_times": item["_source_times"]}
        values.extend(_segment_values(attempt, sizes))
    return {"segment_display_delay_ms": sample_entry(sorted(values))}


def _largest_burst(outputs, window_ms):
    first = total = largest = 0
    for event in outputs:
        while first < len(outputs) and event["at_ms"] - outputs[first]["at_ms"] >= window_ms:
            total -= outputs[first]["bytes"]
            first += 1
        total += event["bytes"]
        largest = max(largest, total)
    return largest


def largest_burst(outputs, window_ms):
    return _largest_burst(outputs, window_ms)


def _workflow(attempts, measured, facts):
    workflow = facts["run"]["configuration"]["workflow"]
    if workflow == "recovery":
        return _recovery_summary(measured)
    if workflow == "mixed-load":
        return _mixed_summary(attempts, measured, facts)
    return {}


def _recovery_summary(measured):
    catch_ups = []
    counts = Counter()
    for item in measured:
        recovery = item.get("_recovery")
        if recovery is None:
            counts["unassessable"] += 1
            continue
        restored = recovery["restored_ms"]
        milestones = recovery["milestones"]
        caught = milestones.get("caught_up")
        timeout = milestones.get("timeout")
        first_update = milestones.get("first_update")
        if (
            first_update
            and first_update.get("at_ms") is not None
            and first_update["at_ms"] <= restored
            and not caught
        ):
            counts["already_current"] += 1
            continue
        if caught and caught.get("state") == "observed":
            catch_ups.append(round(caught["at_ms"] - restored, 3))
            counts["assessable"] += 1
            if caught.get("via") == "terminal":
                counts["terminal_first"] += 1
            elif caught.get("via") in ("preview", "history"):
                counts["other_first"] += 1
            else:
                counts["unknown_path"] += 1
        elif timeout and timeout.get("state") == "observed":
            counts["assessable"] += 1
            counts["verified_timeouts"] += 1
        elif caught is None and timeout is None:
            counts["unassessable"] += 1
        else:
            counts["unassessable"] += 1
    assessable = counts["assessable"]
    return {
        "catch_up_ms": sample_entry(sorted(catch_ups)),
        "assessable": assessable,
        "terminal_first": counts["terminal_first"],
        "terminal_first_share": (
            counts["terminal_first"] / assessable if assessable else None
        ),
        "verified_timeouts": counts["verified_timeouts"],
        "already_current": counts["already_current"],
        "never_restored": counts["never_restored"],
        "unassessable": counts["unassessable"],
        "other_first": counts["other_first"],
        "unknown_path": counts["unknown_path"],
    }


def _watcher_interval(item, scenario):
    raw = item.get("_raw")
    if raw is None or not raw["source"]["times_ms"]:
        return None
    start = raw["source"]["times_ms"][0]
    end = item["terminal_ms"] if item["deadline_visible"] else scenario["deadline_ms"]
    end = min(end if end is not None else scenario["deadline_ms"], scenario["deadline_ms"])
    return (start, end)


def _merge_union(intervals):
    if not intervals:
        return 0.0
    ordered = sorted(intervals)
    total = 0.0
    start, end = ordered[0]
    for next_start, next_end in ordered[1:]:
        if next_start > end:
            total += end - start
            start, end = next_start, next_end
        else:
            end = max(end, next_end)
    total += end - start
    return round(total, 3)


def _mixed_summary(attempts, measured, facts):
    scenario = facts["scenario"]
    workload = scenario["workload"]
    controls = workload.get("controls") or {}
    exposure_decl = workload.get("exposure") or controls.get("exposure") or {}
    required_exposure = exposure_decl.get("required_ms")
    rows = [item for item in measured if item["arm"] in ARMS]
    watch_interval = {}
    for item in rows:
        watch_interval[item["pair_id"]] = _watcher_interval(item, scenario)
    exposure = {
        "required_exposure_ms": required_exposure,
        "scheduled_reads": 0,
        "started_reads": 0,
        "completed_reads": 0,
        "overlapping_reads": 0,
        "unknown_reads": 0,
        "summed_overlap_ms": 0.0,
        "union_ms": 0.0,
        "sufficient": False,
        "reason": None,
        "attempts": [],
    }
    for item in rows:
        reads = item["reads"]
        interval = watch_interval.get(item["pair_id"])
        overlaps = []
        for read in reads:
            exposure["scheduled_reads"] += 1
            if read["started_ms"] is None:
                exposure["unknown_reads"] += 1
                continue
            exposure["started_reads"] += 1
            if read["outcome"] == "completed":
                exposure["completed_reads"] += 1
            if interval is None:
                exposure["unknown_reads"] += 1
                continue
            if read["finished_ms"] is None:
                exposure["unknown_reads"] += 1
                continue
            clipped_start = max(read["started_ms"], interval[0])
            clipped_end = min(read["finished_ms"], interval[1])
            overlap = max(0.0, clipped_end - clipped_start)
            if overlap > 0:
                exposure["overlapping_reads"] += 1
                overlaps.append((clipped_start, clipped_end))
            exposure["summed_overlap_ms"] += overlap
        union = _merge_union(overlaps)
        exposure["union_ms"] += union
        exposure["attempts"].append(
            {
                "id": item["id"],
                "pair_id": item["pair_id"],
                "arm": item["arm"],
                "scheduled_reads": len(reads),
                "overlapping_reads": sum(
                    1
                    for read in reads
                    if read["started_ms"] is not None
                    and read["finished_ms"] is not None
                    and interval is not None
                    and max(
                        0.0,
                        min(read["finished_ms"], interval[1])
                        - max(read["started_ms"], interval[0]),
                    )
                    > 0
                ),
                "union_ms": union,
            }
        )
    exposure["summed_overlap_ms"] = round(exposure["summed_overlap_ms"], 3)
    exposure["union_ms"] = round(exposure["union_ms"], 3)
    required_ok = all(
        entry["overlapping_reads"] > 0
        and (
            required_exposure is None
            or entry["union_ms"] >= required_exposure
        )
        for entry in exposure["attempts"]
        if entry["scheduled_reads"]
    )
    if not rows:
        exposure["reason"] = "no_mixed_attempts"
    elif exposure["overlapping_reads"] == 0:
        exposure["reason"] = "zero_overlap"
    elif exposure["unknown_reads"]:
        exposure["reason"] = "unknown_overlap"
    elif not required_ok:
        exposure["reason"] = "insufficient_exposure"
    else:
        exposure["sufficient"] = True
        exposure["reason"] = None
    pairs = _pairs(rows, scenario)
    read_buckets = _read_buckets(rows)
    return {
        "pairs": pairs,
        "pair_counts": {"pairs": len(pairs), "idle_first": sum(
            pair["order"] == ["idle", "bulk"] for pair in pairs
        ), "bulk_first": sum(pair["order"] == ["bulk", "idle"] for pair in pairs)},
        "deltas": _pair_deltas(pairs),
        "exposure": exposure,
        "reads": read_buckets,
        "bulk_read_gate": analysis.bulk_read_gate(read_buckets["bulk"]),
    }


def _pairs(rows, scenario):
    groups = {}
    for item in rows:
        groups.setdefault(item["pair_id"], []).append(item)
    result = []
    for pair_id in sorted(groups):
        members = groups[pair_id]
        by_arm = {item["arm"]: item for item in members}
        idle, bulk = by_arm.get("idle"), by_arm.get("bulk")
        order = sorted(members, key=lambda item: item["slot"])
        values = None
        reason = None
        if idle is None or bulk is None:
            reason = "incomplete_pair"
        elif idle["outcome"] != "completed" or bulk["outcome"] != "completed":
            reason = "incomplete_outcome"
        else:
            values = {
                "idle": _pair_values(idle, scenario),
                "bulk": _pair_values(bulk, scenario),
            }
        result.append(
            {
                "pair_id": pair_id,
                "order": [item["arm"] for item in order],
                "idle_outcome": idle["outcome"] if idle else None,
                "bulk_outcome": bulk["outcome"] if bulk else None,
                "values": values,
                "deltas": _pair_delta_values(values),
                "reason": reason,
            }
        )
    return result


def _pair_delta_values(values):
    """Signed bulk-minus-idle deltas; a missing arm value stays unknown."""
    deltas = {}
    for metric in PAIR_STREAMING_METRICS:
        if not values:
            deltas[metric] = None
            continue
        idle = values["idle"].get(metric)
        bulk = values["bulk"].get(metric)
        deltas[metric] = (
            round(bulk - idle, 6) if idle is not None and bulk is not None else None
        )
    return deltas


def _pair_values(item, scenario):
    sizes = source_sizes(scenario)
    outputs = item["_outputs"]
    values = {
        "max_display_delay_ms": max(
            _segment_values(
                {"_outputs": outputs, "_source_times": item["_source_times"]}, sizes
            ),
            default=None,
        ),
        "longest_pending_stall_ms": item["longest_pending_stall_ms"],
        "largest_output_share": max(event["bytes"] for event in outputs)
        / max(1, sum(sizes)),
        "final_output_share": outputs[-1]["bytes"] / max(1, sum(sizes)),
    }
    values["max_display_delay_ms"] = (
        None if values["max_display_delay_ms"] is None else values["max_display_delay_ms"]
    )
    return values


def _pair_deltas(pairs):
    result = {"by_order": {"idle_first": {}, "bulk_first": {}}, "pooled": {}}
    for metric in PAIR_STREAMING_METRICS:
        idle_first = [
            pair["deltas"][metric]
            for pair in pairs
            if pair["deltas"][metric] is not None and pair["order"] == ["idle", "bulk"]
        ]
        bulk_first = [
            pair["deltas"][metric]
            for pair in pairs
            if pair["deltas"][metric] is not None and pair["order"] == ["bulk", "idle"]
        ]
        pooled = idle_first + bulk_first
        result["by_order"]["idle_first"][metric] = _delta_entry(idle_first)
        result["by_order"]["bulk_first"][metric] = _delta_entry(bulk_first)
        result["pooled"][metric] = _delta_entry(pooled)
    return result


def _delta_entry(values):
    return {
        "count": len(values),
        "mean": analysis.mean(values),
        "values": values,
    }


def _read_buckets(rows):
    buckets = {}
    for arm in ARMS:
        selected = [
            read
            for item in rows
            if item["arm"] == arm
            for read in item["reads"]
        ]
        completed = [read for read in selected if read["outcome"] == "completed"]
        by_op = Counter(read["op"] for read in selected)
        errors = Counter(
            read["error_code"] for read in selected if read.get("error_code")
        )
        buckets[arm] = {
            "expected_reads": len(selected),
            "successful_reads": len(completed),
            "bytes_expected": sum(read["bytes_expected"] for read in selected),
            "bytes_verified": sum(read["bytes_verified"] for read in completed),
            "errors": dict(sorted(errors.items())),
            "ops": dict(sorted(by_op.items())),
            "pages": sum(read["pages"] for read in completed),
            "physical_bytes": sum(read["physical_bytes"] for read in selected),
            "verified": sum(read["verified"] for read in completed),
        }
    return buckets


def _derive_reads(reads):
    result = []
    for read in reads:
        result.append(
            {
                "id": read["id"],
                "op": read["op"],
                "scheduled_at_ms": read["scheduled_at_ms"],
                "started_ms": read["started_at_ms"],
                "finished_ms": read["finished_at_ms"],
                "deadline_ms": read["deadline_ms"],
                "pages": read["pages"],
                "messages": read["messages"],
                "bytes_expected": read["bytes_expected"],
                "bytes_verified": read["bytes_verified"],
                "physical_bytes": read["physical_bytes"],
                "route": read["route"],
                "outcome": read["outcome"],
                "verified": read["verified"],
                "token_repeats": read["token_repeats"],
                "corrupt": read["corrupt"],
                "error_code": read.get("error_code"),
            }
        )
    return result


# ---------------------------------------------------------------------------
# Typed-event attribution
# ---------------------------------------------------------------------------


def _event_list(facts):
    events = facts.get("events")
    if events is None:
        events = facts["trace"].get("events")
    return list(events or [])


def _event_key(event, fields):
    data = event["data"]
    return tuple(data.get(field) for field in fields)


def _pair_events(events, start_name, end_name, fields, *, stage=None):
    starts = [event for event in events if event["event"] == start_name]
    ends = [event for event in events if event["event"] == end_name]
    if stage is not None:
        starts = [
            event
            for event in starts
            if event["data"].get("stage") == stage[0]
        ]
        ends = [
            event
            for event in ends
            if event["data"].get("stage") == stage[1]
        ]
    intervals = []
    unresolved = []
    for start in starts:
        key = _event_key(start, fields)
        matches = [event for event in ends if _event_key(event, fields) == key]
        if len(matches) != 1:
            unresolved.append(
                {
                    "key": list(key),
                    "reason": "ambiguous_join" if matches else "missing_boundary",
                }
            )
            continue
        end = matches[0]
        if end["ts"] < start["ts"]:
            unresolved.append({"key": list(key), "reason": "reversed_boundary"})
            continue
        intervals.append(
            {
                "key": list(key),
                "start_ns": start["ts"],
                "end_ns": end["ts"],
                "duration_ms": round((end["ts"] - start["ts"]) / 1_000_000, 6),
                "owners": [start["producer"], end["producer"]],
                "attempt": start["data"].get("attempt") or end["data"].get("attempt"),
            }
        )
    return intervals, unresolved


def _attribution(events, clock, facts):
    mode = facts["run"]["observation_mode"]
    available = clock["verified"] and mode == "trace"
    if not available:
        reason = "clock_unverified" if not clock["verified"] else "mode_" + mode
    else:
        reason = None
    intervals = []
    unresolved = []
    if available and events:
        for name, (start, end, fields) in ATTRIBUTION_PAIRS.items():
            stage = None
            if name == "access_forward_ms":
                stage = _ACCESS_STAGE
            elif name == "cli_terminal_ms":
                stage = _CLI_STAGE
            found, missing = _pair_events(events, start, end, fields, stage=stage)
            for interval in found:
                interval["name"] = name
            intervals.extend(found)
            for item in missing:
                item["name"] = name
            unresolved.extend(missing)
    if available and not events:
        reason = "no_events"
    intervals.sort(key=lambda item: (item["start_ns"], item["end_ns"]))
    summed = sum(item["duration_ms"] for item in intervals)
    merged = _merge_union([(item["start_ns"], item["end_ns"]) for item in intervals])
    cardinality = {
        "events": len(events),
        "streams": len(
            {event["data"].get("stream") for event in events if event["data"].get("stream")}
        ),
        "blocks": len(
            {event["data"].get("block") for event in events if event["data"].get("block")}
        ),
        "requests": len(
            {event["data"].get("request") for event in events if event["data"].get("request")}
        ),
        "watches": len(
            {event["data"].get("watch") for event in events if event["data"].get("watch")}
        ),
    }
    return {
        "available": available,
        "reason": reason,
        "mode": facts["run"]["observation_mode"],
        "transport": _transport_summary(events),
        "cardinality": cardinality,
        "metrics": list(METRIC_CATALOG),
        "intervals": intervals,
        "unresolved": unresolved,
        "summed_ms": round(summed, 6),
        "union_ms": round(merged / 1_000_000, 6) if merged else 0.0,
        "overlap_ms": round(summed - (merged / 1_000_000 if merged else 0.0), 6),
        "critical_path_ms": round(merged / 1_000_000, 6) if merged else 0.0,
        "critical_path_rule": "overlapping intervals are unioned, never summed",
    }


def _stream_attempts(events):
    """Map a stream token to its attempt token from fixture-owned emission.

    Product producers see the stream but not the fixture's attempt name; the
    fixture_emit row is the trusted place that binds the two run-local tokens.
    """
    mapping = {}
    for event in events:
        data = event["data"]
        stream = data.get("stream")
        attempt = data.get("attempt")
        if isinstance(stream, str) and stream and isinstance(attempt, str) and attempt:
            mapping.setdefault(stream, attempt)
    return mapping


def _join_index(events, attempts=()):
    """Run-local joins: stream->attempt, frame->stream, delivery->frame.

    Product facts name the transport frame and the CLI delivery ordinal but do
    not see the fixture's attempt name. The trusted fixture_emit row and the
    Core receipt/delivery rows provide the bridge; neither invents a timestamp.
    """
    stream_attempts = _stream_attempts(events)
    frame_streams = {}
    delivery_frames = {}
    for event in events:
        data = event["data"]
        frame = data.get("frame")
        stream = data.get("stream")
        if isinstance(frame, str) and frame and isinstance(stream, str) and stream:
            frame_streams.setdefault(frame, stream)
        delivery = data.get("delivery")
        if isinstance(delivery, int) and isinstance(frame, str) and frame:
            # Delivery ordinals restart per CLI process; the lifetime is part
            # of the join key.
            delivery_frames.setdefault((event.get("lifetime"), delivery), frame)
    windows = []
    for attempt in attempts:
        observation = attempt.get("observation") or {}
        start, end = observation.get("started_ns"), observation.get("ended_ns")
        if isinstance(start, int) and isinstance(end, int):
            windows.append((attempt["id"], start, end))
    return {
        "stream_attempts": stream_attempts,
        "frame_streams": frame_streams,
        "delivery_frames": delivery_frames,
        "windows": windows,
    }


def _event_attempt(event, index):
    if not isinstance(index, dict):
        index = {
            "stream_attempts": index,
            "frame_streams": {},
            "delivery_frames": {},
            "windows": [],
        }
    data = event["data"]
    attempt = data.get("attempt")
    if isinstance(attempt, str) and attempt:
        return attempt
    stream = data.get("stream")
    if not isinstance(stream, str) or not stream:
        frame = data.get("frame")
        if isinstance(frame, str) and frame:
            stream = index["frame_streams"].get(frame)
        else:
            delivery = data.get("delivery")
            if isinstance(delivery, int):
                frame = index["delivery_frames"].get((event.get("lifetime"), delivery))
                stream = index["frame_streams"].get(frame) if frame else None
    if isinstance(stream, str) and stream:
        mapped = index["stream_attempts"].get(stream)
        if mapped:
            return mapped
    ts = event.get("ts")
    matches = [
        attempt_id
        for attempt_id, start, end in index["windows"]
        if isinstance(ts, int) and start <= ts <= end
    ]
    return matches[0] if len(matches) == 1 else None


def _transport_summary(events):
    """Access broker forwarding facts. They carry no stream/sequence identity,
    so they describe client-bound transport cost without pretending to join a
    preview to one increment."""
    selected = [event for event in events if event["event"] == "access_transport"]
    counts = Counter(
        (
            event["data"].get("role"),
            event["data"].get("direction"),
            event["data"].get("outcome"),
        )
        for event in selected
    )
    return {
        "events": len(selected),
        "connections": len({
            event["data"].get("connection") for event in selected
            if event["data"].get("connection")
        }),
        "bytes": sum(
            event["data"].get("bytes", 0) for event in selected
            if isinstance(event["data"].get("bytes"), int)
        ),
        "counts": [
            {"role": role, "direction": direction, "outcome": outcome, "count": count}
            for (role, direction, outcome), count in sorted(counts.items())
        ],
        "per_stream_join": "unavailable",
        "reason": "transport carries no stream/sequence identity",
    }


def _pending_evidence(events, attempt_id, cutoff_ns=None, attempts=()):
    index = _join_index(events, attempts)
    relevant = [
        event
        for event in events
        if _event_attempt(event, index) == attempt_id
        and (cutoff_ns is None or event["ts"] <= cutoff_ns)
    ]
    if not relevant:
        return {
            "stage": "unavailable",
            "range": None,
            "owner": None,
            "reason": "no_attempt_events",
        }
    latest = max(relevant, key=lambda event: (event["ts"], event.get("seq", 0)))
    data = latest["data"]
    range_info = None
    if "seq_start" in data or "seq_end" in data:
        range_info = {
            "block": data.get("block"),
            "seq_start": data.get("seq_start"),
            "seq_end": data.get("seq_end"),
        }
    return {
        "stage": latest["event"],
        "range": range_info,
        "owner": latest["producer"],
        "reason": None if data.get("stream") else "no_stream_scope",
    }


def _attempt_attribution(events, attempt_id, attempts=()):
    index = _join_index(events, attempts)
    relevant = [
        event
        for event in events
        if _event_attempt(event, index) == attempt_id
    ]
    relevant.sort(key=lambda event: event["ts"])
    last = relevant[-1] if relevant else None
    return {
        "intervals": [],
        "event_count": len(relevant),
        "last_stage": last["event"] if last else "unavailable",
        "coverage": None,
    }


def _edge_key(event, fields):
    data = event["data"]
    key = []
    for field in fields:
        value = data.get(field)
        if isinstance(value, int):
            key.append(value)
        elif isinstance(value, str) and value:
            key.append(value)
        else:
            return None
    return tuple(key)


def _pair_edge(events, start_name, end_name, fields):
    starts = [event for event in events if event["event"] == start_name]
    ends = [event for event in events if event["event"] == end_name]
    intervals = []
    unresolved = []
    for start in starts:
        key = _edge_key(start, fields)
        if key is None:
            if start_name == "dsh_callback" and start["data"].get("disposition") == "suppressed":
                unresolved.append({
                    "name": start_name + "->" + end_name,
                    "key": [],
                    "reason": "inapplicable",
                })
            continue
        matches = [event for event in ends if _edge_key(event, fields) == key]
        if len(matches) != 1:
            reason = "ambiguous_join" if matches else "missing_boundary"
            if start_name == "host_publish" and start["data"].get("outcome") == "dropped":
                reason = "dropped"
            elif start_name == "dsh_callback" and start["data"].get("disposition") == "suppressed":
                reason = "inapplicable"
            unresolved.append({
                "name": start_name + "->" + end_name,
                "key": list(key),
                "reason": reason,
            })
            continue
        end = matches[0]
        if end["ts"] < start["ts"]:
            unresolved.append({
                "name": start_name + "->" + end_name,
                "key": list(key),
                "reason": "reversed_boundary",
            })
            continue
        intervals.append({
            "name": start_name + "->" + end_name,
            "start_ns": start["ts"],
            "end_ns": end["ts"],
            "duration_ms": round((end["ts"] - start["ts"]) / 1_000_000, 6),
            "owners": [start["producer"], end["producer"]],
            "key": list(key),
        })
    return intervals, unresolved


def _pair_handoff_enqueue(events):
    """Join per-frame handoffs to the coalesced Host enqueue that contains them.

    LiveBuffer may merge several adapter frames into one Host batch; the merged
    batch token then legitimately differs from each per-frame handoff batch.
    Containment by stream/block/sequence range is the documented join, and the
    edge runs from the last handoff to the enqueue so the coalescing window is
    counted once.
    """
    handoffs = [event for event in events if event["event"] == "dsh_handoff"]
    enqueues = [event for event in events if event["event"] == "host_enqueue"]
    intervals = []
    unresolved = []
    for enqueue in enqueues:
        data = enqueue["data"]
        required = (
            data.get("stream"),
            data.get("block"),
            data.get("seq_start"),
            data.get("seq_end"),
        )
        if any(value is None or value == "" for value in required):
            continue
        stream, block, start, end = required
        if start == 1 and end == 1:
            # Host reserves sequence 1 for the start frame; it is not a text
            # increment and has no source predecessor to join.
            continue
        members = [
            event for event in handoffs
            if event["data"].get("stream") == stream
            and event["data"].get("block") == block
            and isinstance(event["data"].get("seq_start"), int)
            and isinstance(event["data"].get("seq_end"), int)
            and event["data"]["seq_start"] >= start
            and event["data"]["seq_end"] <= end
            and event["ts"] <= enqueue["ts"]
        ]
        if not members:
            unresolved.append({
                "name": "handoff_to_enqueue_ms",
                "key": [stream, block, start, end],
                "reason": "missing_boundary",
            })
            continue
        last = max(members, key=lambda event: event["ts"])
        intervals.append({
            "name": "handoff_to_enqueue_ms",
            "start_ns": last["ts"],
            "end_ns": enqueue["ts"],
            "duration_ms": round((enqueue["ts"] - last["ts"]) / 1_000_000, 6),
            "owners": [last["producer"], enqueue["producer"]],
            "key": [stream, block, start, end],
            "coalesced_frames": len(members),
        })
    return intervals, unresolved


def _pair_receipt_delivery(events):
    """Only preview deliveries name a text increment; terminal/message
    deliveries from the same frame are not missing receipt edges."""
    receipts = [event for event in events if event["event"] == "core_receipt"]
    deliveries = [
        event for event in events
        if event["event"] == "core_delivery"
        and event["data"].get("kind") == "preview"
    ]
    intervals = []
    unresolved = []
    for delivery in deliveries:
        frame = delivery["data"].get("frame")
        if not frame:
            continue
        matches = [
            event for event in receipts
            if event["data"].get("frame") == frame and event["ts"] <= delivery["ts"]
        ]
        if not matches:
            unresolved.append({
                "name": "receipt_to_delivery_ms",
                "key": [frame],
                "reason": "missing_boundary",
            })
            continue
        receipt = max(matches, key=lambda event: event["ts"])
        intervals.append({
            "name": "receipt_to_delivery_ms",
            "start_ns": receipt["ts"],
            "end_ns": delivery["ts"],
            "duration_ms": round((delivery["ts"] - receipt["ts"]) / 1_000_000, 6),
            "owners": [receipt["producer"], delivery["producer"]],
            "key": [frame],
        })
    return intervals, unresolved


def _write_observe_edge(events, index):
    writes = [event for event in events if event["event"] == "cli_write"]
    observes = [event for event in events if event["event"] == "fixture_observe"]
    intervals = []
    unresolved = []
    for write in writes:
        attempt = _event_attempt(write, index)
        ordinal = write["data"].get("ordinal")
        if not attempt or not isinstance(ordinal, int):
            continue
        matches = [
            event for event in observes
            if _event_attempt(event, index) == attempt
            and event["data"].get("record", event["data"].get("ordinal")) == ordinal
        ]
        if len(matches) != 1:
            unresolved.append({
                "name": "cli_write->fixture_observe",
                "key": [attempt, ordinal],
                "reason": "ambiguous_join" if matches else "missing_boundary",
            })
            continue
        observe = matches[0]
        if observe["ts"] < write["ts"]:
            # The reader can consume the bytes after write(2) but before the
            # CLI's post-flush stamp. Keep the pair and report a zero-length
            # reordered edge instead of losing a real record.
            intervals.append({
                "name": "write_to_observe_ms",
                "start_ns": write["ts"],
                "end_ns": write["ts"],
                "duration_ms": 0.0,
                "owners": [write["producer"], observe["producer"]],
                "key": [attempt, ordinal],
                "reordered": True,
            })
            continue
        intervals.append({
            "name": "write_to_observe_ms",
            "start_ns": write["ts"],
            "end_ns": observe["ts"],
            "duration_ms": round((observe["ts"] - write["ts"]) / 1_000_000, 6),
            "owners": [write["producer"], observe["producer"]],
            "key": [attempt, ordinal],
        })
    return intervals, unresolved


def _waterfall(events, attempt_id, cutoff_ns=None):
    """One row per Host preview batch with explicit residual time.

    A batch is the unit Host actually publishes, so its source range, frame and
    records are unambiguous. Wall-clock attempt spans contain source cadence and
    are not used; each batch span starts at its first source increment and ends
    at its last observed record. Matched edges are unioned inside that span.
    """
    index = _join_index(events)
    relevant = [
        event
        for event in events
        if _event_attempt(event, index) == attempt_id
        and (cutoff_ns is None or event["ts"] <= cutoff_ns)
    ]
    relevant.sort(key=lambda event: (event["ts"], event.get("seq", 0)))
    enqueues = [event for event in relevant if event["event"] == "host_enqueue"]
    edges = []
    unresolved = []
    batches = []
    total_ns = 0
    matched_ns = 0
    for enqueue in enqueues:
        data = enqueue["data"]
        stream = data.get("stream")
        start_seq = data.get("seq_start")
        end_seq = data.get("seq_end")
        batch = data.get("batch")
        if not stream or start_seq is None or end_seq is None:
            continue
        emits = [
            event for event in relevant
            if event["event"] == "fixture_emit"
            and event["data"].get("stream") == stream
            and isinstance(event["data"].get("seq_start"), int)
            and start_seq <= event["data"]["seq_start"] <= end_seq
        ]
        if not emits:
            continue
        start_ns = min(event["ts"] for event in emits)
        deliveries = [
            event for event in relevant
            if event["event"] == "core_delivery"
            and event["data"].get("stream") == stream
            and event["data"].get("seq_start") is not None
            and start_seq <= event["data"]["seq_start"] <= end_seq
        ]
        writes = []
        observes = []
        for delivery in deliveries:
            ordinal = delivery["data"].get("delivery")
            for write in relevant:
                if write["event"] == "cli_write" and write["data"].get("delivery") == ordinal:
                    writes.append(write)
                    for observe in relevant:
                        if observe["event"] == "fixture_observe" and observe["data"].get(
                            "record", observe["data"].get("ordinal")
                        ) == write["data"].get("ordinal"):
                            observes.append(observe)
        endpoints = [event["ts"] for event in (*writes, *observes)]
        if not endpoints:
            continue
        end_ns = max(endpoints)
        # Keep source/adapter events from the batch range, but never let a
        # neighbouring batch's publish/receipt/delivery leak into this window.
        window = [
            event for event in relevant
            if start_ns <= event["ts"] <= end_ns
            and (
                event["event"] not in (
                    "host_enqueue", "host_dequeue", "host_publish",
                    "core_receipt", "core_delivery",
                )
                or event["data"].get("batch") == batch
            )
        ]
        matched = []
        for name, start_name, end_name, fields in EDGE_PAIRS:
            if name == "handoff_to_enqueue_ms":
                found, missing = _pair_handoff_enqueue(window)
            elif name == "receipt_to_delivery_ms":
                found, missing = _pair_receipt_delivery(window)
            else:
                found, missing = _pair_edge(window, start_name, end_name, fields)
            for interval in found:
                interval["name"] = name
                interval["batch"] = batch
            matched.extend(found)
            for item in missing:
                item["name"] = name
                unresolved.append(item)
        found, missing = _write_observe_edge(window, index)
        for interval in found:
            interval["batch"] = batch
        matched.extend(found)
        unresolved.extend(missing)
        covered = _merge_union([(edge["start_ns"], edge["end_ns"]) for edge in matched])
        edges.extend(matched)
        total_ns += end_ns - start_ns
        matched_ns += covered
        batches.append({
            "batch": batch,
            "start_ns": start_ns,
            "end_ns": end_ns,
            "total_ms": round((end_ns - start_ns) / 1_000_000, 6),
            "matched_ms": round(covered / 1_000_000, 6),
            "residual_ms": round(max(0, end_ns - start_ns - covered) / 1_000_000, 6),
            "records": len(observes),
        })
    residual = round(max(0, total_ns - matched_ns) / 1_000_000, 6)
    last = max(edges, key=lambda edge: edge["end_ns"]) if edges else None
    return {
        "attempt": attempt_id,
        "batches": batches,
        "edges": edges,
        "unresolved": unresolved,
        "total_ms": round(total_ns / 1_000_000, 6),
        "matched_ms": round(matched_ns / 1_000_000, 6),
        "residual_ms": residual,
        "last_evidenced_edge": last["name"] if last else "unavailable",
        "complete": (
            bool(batches)
            and not [
                item for item in unresolved
                if item["reason"] not in ("inapplicable", "dropped")
            ]
            and residual <= 0.001
        ),
    }


# ---------------------------------------------------------------------------
# Coverage, trace quality and quality verdicts
# ---------------------------------------------------------------------------


def _attempt_coverage(trace):
    coverage = trace.get("coverage") if isinstance(trace, dict) else None
    if not isinstance(coverage, dict):
        return {}
    result = {}
    for name, entry in coverage.items():
        result[name] = {
            "required": list(entry.get("required", [])),
            "observed": list(entry.get("observed", [])),
            "inapplicable": dict(entry.get("inapplicable", {})),
            "complete": entry.get("complete") is True,
        }
    return result


APPLICABLE_PATHS = {
    "watch": ("preview",),
    "recovery": ("preview", "recovery"),
    # The fixture's attachment download uses the protected payload fetch route;
    # it does not traverse relay.Service.Fetch, so the object interval is
    # inapplicable here. Access unit tests cover the reference object lifecycle.
    "mixed-load": ("preview", "bulk"),
}


def _coverage(facts, events=()):
    """Required boundary coverage for the paths this workload actually runs.

    A watch case does not exercise the query, command, recovery, bulk or object
    paths, so demanding those boundaries would make every quality verdict
    incomplete by construction. Observed boundaries are matched per attempt
    through the trusted fixture stream-to-attempt join, not by trusting a
    producer-local attempt field (the product does not know the fixture name).
    """
    attempts = facts["attempts"]
    eligible = [
        attempt
        for attempt in attempts
        if slot_phase_status(attempt["status"]) >= 2 and attempt["phase"] == "measured"
    ]
    floor = facts["run"]["configuration"].get("coverage_floor", 1.0)
    workflow = facts["scenario"]["workload"]["workflow"]
    paths = APPLICABLE_PATHS.get(workflow, ("preview",))
    index = _join_index(events, attempts)
    observed_by_attempt = {}
    for event in events:
        attempt = _event_attempt(event, index)
        if attempt:
            observed_by_attempt.setdefault(attempt, set()).add(event["event"])
    result = {
        "paths": {},
        "complete": True,
        "edge_complete": True,
        "floor": floor,
        "reasons": [],
    }
    for path in paths:
        default_required = COVERAGE_PATHS[path]
        # A bulk/object path is only exercised by the bulk arm; the idle arm is
        # deliberately exempt rather than counted as a missing boundary.
        path_attempts = (
            [attempt for attempt in eligible if attempt["arm"] == "bulk"]
            if path in ("bulk", "object")
            else eligible
        )
        covered = 0
        denominator = len(path_attempts)
        missing = Counter()
        for attempt in path_attempts:
            entry = (attempt.get("trace") or {}).get("coverage", {}).get(path) or {}
            observed = observed_by_attempt.get(attempt["id"], set()) | set(
                entry.get("observed", [])
            )
            if not entry and not observed:
                missing["whole_trace_loss"] += 1
                continue
            required = list(entry.get("required") or default_required)
            inapplicable = {
                stage: reason
                for stage, reason in (entry.get("inapplicable") or {}).items()
                if text(reason)
            }
            remaining = [stage for stage in required if stage not in inapplicable]
            lacking = [stage for stage in remaining if stage not in observed]
            if lacking:
                missing.update({stage: 1 for stage in lacking})
                continue
            covered += 1
        share = covered / denominator if denominator else None
        complete = denominator > 0 and share >= floor
        path_entry = {
            "required": list(default_required),
            "eligible": denominator,
            "covered": covered,
            "share": share,
            "complete": complete,
            "missing": dict(sorted(missing.items())),
            "reason": None if complete else (
                "no_eligible_attempts" if not denominator else "missing_required_boundary"
            ),
        }
        if path == "preview":
            required_edges = PREVIEW_EDGES
            edge_covered = 0
            edge_missing = Counter()
            waterfalls = [
                _waterfall(events, attempt["id"]) for attempt in path_attempts
            ]
            for waterfall in waterfalls:
                observed = {edge["name"] for edge in waterfall["edges"]}
                lacking = [name for name in required_edges if name not in observed]
                lacking.extend(
                    item["name"] for item in waterfall["unresolved"]
                    if item["name"] in required_edges
                    and item["reason"] not in ("inapplicable", "dropped")
                )
                if lacking:
                    edge_missing.update({name: 1 for name in lacking})
                    continue
                edge_covered += 1
            edge_complete = denominator > 0 and edge_covered == denominator
            inapplicable = sum(
                1 for waterfall in waterfalls
                for item in waterfall["unresolved"]
                if item["reason"] in ("inapplicable", "dropped")
            )
            path_entry["edges"] = {
                "required": list(required_edges),
                "eligible": denominator,
                "covered": edge_covered,
                "complete": edge_complete,
                "inapplicable": inapplicable,
                "missing": dict(sorted(edge_missing.items())),
                "reason": None if edge_complete else (
                    "no_eligible_attempts" if not denominator else "incomplete_edge_chain"
                ),
            }
            if not edge_complete:
                result["edge_complete"] = False
                result["reasons"].append("preview:" + path_entry["edges"]["reason"])
        result["paths"][path] = path_entry
        if not complete:
            result["complete"] = False
            result["reasons"].append(path + ":" + path_entry["reason"])
    return result


def _trace_quality(trace, coverage):
    complete = trace.get("complete") is True
    bytes_ = trace.get("bytes")
    cap = trace.get("cap_bytes")
    truncated = bool(cap is not None and bytes_ is not None and bytes_ >= cap)
    losses = trace.get("losses") or {}
    total_loss = sum(losses.values()) if losses else 0
    reasons = []
    if not complete:
        reasons.append("trace_incomplete")
    if truncated:
        reasons.append("trace_byte_cap")
    if total_loss:
        reasons.append("trace_loss")
    return {
        "path": trace.get("path"),
        "checksum": trace.get("checksum"),
        "bytes": bytes_,
        "cap_bytes": cap,
        "complete": complete,
        "truncated": truncated,
        "losses": dict(losses),
        "total_loss": total_loss,
        "reasons": reasons,
    }


def _quality(facts, clock, coverage, trace):
    status = facts["status"]
    mode = facts["run"]["observation_mode"]
    reasons = list(status["reasons"])
    limitations = []
    if not clock["verified"]:
        reasons.append("clock_unverified")
    if status["state"] != "complete":
        reasons.append("status_" + status["state"])
    if mode == "trace":
        if not trace["complete"]:
            reasons.extend(trace["reasons"])
        if not coverage["complete"]:
            reasons.append("coverage_incomplete")
    else:
        limitations.append("attribution_unavailable_" + mode)
    if trace["truncated"]:
        reasons.append("trace_byte_cap")
    attribution_available = (
        mode == "trace" and clock["verified"] and trace["complete"] and not trace["truncated"]
    )
    diagnostic_pass = (
        status["state"] == "complete"
        and clock["verified"]
        and attribution_available
        and coverage["complete"]
    )
    reasons = sorted(set(reasons))
    return {
        "state": "complete" if not reasons else "incomplete",
        "reasons": reasons,
        "limitations": sorted(set(limitations)),
        "diagnostic_pass": diagnostic_pass,
        "clock": clock,
        "capture": {
            "mode": mode,
            "trace_complete": trace["complete"],
            "truncated": trace["truncated"],
            "losses": trace["losses"],
            "attribution_available": attribution_available,
        },
        "coverage_complete": coverage["complete"],
        "edge_coverage_complete": coverage.get("edge_complete"),
    }

# ---------------------------------------------------------------------------
# Exemplars
# ---------------------------------------------------------------------------


def _exemplars(measured, facts, trace, coverage, quality):
    rows = [
        item
        for item in measured
        if item["snapshot_state"] not in ("inapplicable",) and item["status"] != "not_started"
    ]
    exemplars = []
    typical = _typical_row(rows)
    if typical is not None:
        exemplars.append(
            _exemplar(
                "typical",
                "first_text_ms",
                typical,
                facts,
                trace,
                coverage,
                quality,
                "nearest_median",
            )
        )
    for metric in (
        "first_text_ms",
        "terminal_ms",
        "longest_pending_stall_ms",
        "peak_backlog_bytes",
    ):
        candidates = [item for item in rows if item.get(metric) is not None]
        if not candidates:
            continue
        candidates.sort(key=lambda item: (item[metric], item["slot"]))
        values = [item[metric] for item in candidates]
        if len(values) >= TAIL_MINIMUM["p95"]:
            index = max(1, math.ceil(0.95 * len(values))) - 1
            method = "nearest_rank_p95"
        else:
            index = len(values) - 1
            method = "observed_max"
        chosen = candidates[index]
        exemplars.append(
            _exemplar("tail", metric, chosen, facts, trace, coverage, quality, method)
        )
    failed = next(
        (
            item
            for item in sorted(rows, key=lambda item: item["slot"])
            if item["outcome"] in FAILURE_OUTCOMES and item["outcome"] != "not_started"
        ),
        None,
    )
    if failed is not None:
        exemplars.append(
            _exemplar("failed", "outcome", failed, facts, trace, coverage, quality,
                      "first_failed_slot")
        )
    return {
        "exemplars": exemplars,
        "trace_path": trace["path"],
        "coverage_complete": coverage["complete"],
        "edge_coverage_complete": coverage.get("edge_complete"),
        "quality": quality["state"],
    }


def _typical_row(rows):
    candidates = [item for item in rows if item.get("first_text_ms") is not None]
    if not candidates:
        return None
    middle = median(item["first_text_ms"] for item in candidates)
    return min(candidates, key=lambda item: (abs(item["first_text_ms"] - middle), item["slot"]))


def _exemplar(kind, metric, item, facts, trace, coverage, quality, selection):
    raw = item.get("_raw")
    observation = raw["observation"] if raw else {}
    start_ms = observation.get("started_ms")
    end_ms = observation.get("ended_ms")
    start_ns = observation.get("started_ns")
    end_ns = observation.get("ended_ns")
    trace_range = None
    range_reason = None
    if not trace["complete"] or not trace["path"]:
        range_reason = "trace_missing" if not trace["path"] else "trace_truncated"
    elif start_ns is None or end_ns is None:
        range_reason = "trace_clock_unavailable"
    else:
        trace_range = {
            "path": trace["path"],
            "start_ns": start_ns,
            "end_ns": end_ns,
            "complete": True,
        }
    return {
        "kind": kind,
        "metric": metric,
        "value": item.get(metric) if metric in item else item.get("outcome"),
        "attempt_id": item["id"],
        "slot": item["slot"],
        "phase": item["phase"],
        "selection": selection,
        "reason": (
            "nearest_rank"
            if selection in ("nearest_rank_p95", "observed_max")
            else selection
        ),
        "attempt_range_ms": {"start_ms": start_ms, "end_ms": end_ms},
        "coverage": {
            "complete": coverage["complete"],
            "quality": quality["state"],
        },
        "trace_range": trace_range,
        "range_reason": range_reason,
    }


# ---------------------------------------------------------------------------
# Documents, replay validation and readers
# ---------------------------------------------------------------------------


def case_document(facts):
    """Validate raw facts and publish them with their derived business sections."""
    checked = validate_case(facts)
    document = copy.deepcopy(checked)
    document["derived"] = derive(checked)
    return document


def validate_report(report):
    """Recompute derived sections from embedded facts and require equality."""
    if not isinstance(report, dict) or "derived" not in report:
        invalid("published report is missing derived sections")
    facts = {key: value for key, value in report.items() if key != "derived"}
    checked = validate_case(facts)
    expected = derive(checked)
    if report["derived"] != expected:
        raise RepoError("invalid benchmark report: derived sections are stale or tampered")
    return report


def suite_document(cases, *, suite_id="suite", started_at=None, configuration=None, status=None):
    """Build a current suite document with a stable per-case index."""
    entries = []
    for index, case in enumerate(cases):
        source = case if isinstance(case, dict) else {}
        run = source.get("run", {})
        derived = source.get("derived", {})
        entry = {
            "index": index,
            "case": source.get("case") or run.get("id") or f"case-{index}",
            "run_id": run.get("id"),
            "workflow": (run.get("configuration") or {}).get("workflow"),
            "network": (run.get("configuration") or {}).get("network"),
            "path": case.get("path") if isinstance(case, dict) else None,
            "status": (
                "complete"
                if source.get("status", {}).get("state") == "complete"
                else "incomplete"
            )
            if source
            else "planned",
            "case_schema": SCHEMA_VERSION,
            "outcomes": derived.get("outcomes"),
        }
        entries.append(entry)
    if status is None:
        complete = all(entry["status"] == "complete" for entry in entries)
        status = {
            "state": "complete" if complete else "incomplete",
            "reasons": [] if complete else ["incomplete_cases"],
            "pending": [],
            "interruption": None,
        }
    return {
        "schema_version": SUITE_SCHEMA_VERSION,
        "kind": "benchmark-suite",
        "id": suite_id,
        "started_at": started_at,
        "configuration": configuration or {},
        "cases": entries,
        "status": status,
    }


def validate_suite(suite):
    """Strict suite schema 2 validation, including interruption/pending facts."""
    if not isinstance(suite, dict):
        invalid("suite is not an object")
    if suite.get("schema_version") != SUITE_SCHEMA_VERSION:
        unsupported(
            f"suite schema {suite.get('schema_version')!r}; "
            f"expected {SUITE_SCHEMA_VERSION}"
        )
    if suite.get("kind") != "benchmark-suite":
        unsupported("suite kind; expected benchmark-suite")
    if not text(suite.get("id")):
        invalid("suite id must be text")
    cases = suite.get("cases")
    if not isinstance(cases, list):
        invalid("suite cases must be a list")
    indexes = []
    identities = set()
    for case in cases:
        if not isinstance(case, dict):
            invalid("suite case entry is not an object")
        if not count(case.get("index")):
            invalid("suite case index must be a non-negative integer")
        indexes.append(case["index"])
        identity = case.get("case") or case.get("run_id")
        if identity in identities:
            invalid("suite case identity is duplicated")
        identities.add(identity)
        if case.get("status") not in (
            "complete",
            "incomplete",
            "planned",
            "interrupted",
            "invalid",
        ):
            invalid("suite case has an unknown status")
        if case.get("case_schema") != SCHEMA_VERSION:
            unsupported(
                f"suite case schema {case.get('case_schema')!r}; "
                f"expected {SCHEMA_VERSION}"
            )
    if indexes != list(range(len(cases))):
        invalid("suite case indexes must be contiguous and ordered")
    status = suite.get("status")
    if not isinstance(status, dict) or status.get("state") not in (
        "complete",
        "incomplete",
        "interrupted",
    ):
        invalid("suite status needs a known state")
    reasons = status.get("reasons")
    if not isinstance(reasons, list) or any(not isinstance(item, str) for item in reasons):
        invalid("suite status reasons must be text")
    pending = status.get("pending")
    if not isinstance(pending, list) or any(not count(item) for item in pending):
        invalid("suite status pending must list case indexes")
    if any(item >= len(cases) for item in pending):
        invalid("suite pending index is outside the case index")
    if status["state"] == "interrupted" and not pending:
        invalid("an interrupted suite must list pending cases")
    if status["state"] != "complete" and not reasons:
        invalid("an incomplete or interrupted suite must state its reasons")
    return suite


def read(path):
    """Read only the current case or suite schema; reject every other version."""
    try:
        document = json.loads(Path(path).read_text())
    except (OSError, ValueError) as exc:
        raise RepoError("invalid benchmark document: unreadable file") from exc
    if not isinstance(document, dict):
        invalid("document is not an object")
    version = document.get("schema_version")
    if version != SCHEMA_VERSION:
        unsupported(f"schema version {version!r}; expected {SCHEMA_VERSION}")
    kind = document.get("kind")
    if kind == "benchmark-suite":
        return validate_suite(document)
    if kind not in (None, "benchmark-case"):
        unsupported(f"document kind {kind!r}")
    if "derived" in document:
        return validate_report(document)
    return case_document(document)


# ---------------------------------------------------------------------------
# Rendering-independent helper aliases kept for fixture/runner callers
# ---------------------------------------------------------------------------


def longest_pending_gap(outputs, source_times, sizes):
    raw = {
        "source": {"times_ms": list(source_times), "sizes": list(sizes)},
        "outputs": [
            {
                "at_ms": event["at_ms"],
                "ordinal": index,
                "kind": event.get("kind", "preview"),
                "bytes": event.get("bytes", 0),
                "prefix_bytes": event.get("prefix_bytes", 0),
                "correct": event.get("correct", True),
                "block": event.get("block"),
            }
            for index, event in enumerate(outputs)
        ],
    }
    last_source = source_times[-1] if source_times else 0
    last_output = outputs[-1]["at_ms"] if outputs else last_source
    snapshot = _stream_snapshot(raw, max(last_source, last_output))
    return snapshot["longest_pending_stall_ms"] or 0


def segment_display_delays(outputs, source_times, sizes):
    attempt = {"_outputs": outputs, "_source_times": source_times}
    return iter(_segment_values(attempt, sizes))


def max_display_delay(outputs, source_times, sizes):
    return max(segment_display_delays(outputs, source_times, sizes), default=0)


def streaming_values(
    attempt, final_bytes, sizes=None, burst_window_ms=None, *, display_delay=False
):
    outputs = attempt["outputs"]
    values = {
        "largest_output_share": max(event["bytes"] for event in outputs) / final_bytes,
        "final_output_share": outputs[-1]["bytes"] / final_bytes,
        "zero_preview": len(outputs) == 1,
    }
    if burst_window_ms is not None:
        values["largest_burst_share"] = _largest_burst(outputs, burst_window_ms) / final_bytes
    if display_delay and sizes is not None:
        values["max_display_delay_ms"] = round(
            max(
                _segment_values(
                    {
                        "_outputs": outputs,
                        "_source_times": attempt["source_times_ms"],
                    },
                    sizes,
                ),
                default=0,
            ),
            3,
        )
    return values


def streaming_summary(
    attempts, final_bytes, sizes=None, burst_window_ms=None, *, display_delay=False
):
    measured = [item for item in attempts if item.get("phase") == "measured"]
    rows = [
        streaming_values(item, final_bytes, sizes, burst_window_ms, display_delay=display_delay)
        for item in measured
        if item.get("outputs")
    ]
    metrics = list(PAIR_STREAMING_METRICS)
    if burst_window_ms is not None:
        metrics.append("largest_burst_share")
    return {
        "samples": {
            metric: sample_entry(sorted(row[metric] for row in rows if metric in row))
            for metric in metrics
        },
        "zero_preview": sum(row["zero_preview"] for row in rows),
    }


def diagnostic_summary(attempts, sizes):
    values = []
    for item in attempts:
        if item.get("phase") != "measured" or not item.get("outputs"):
            continue
        values.extend(
            _segment_values(
                {"_outputs": item["outputs"], "_source_times": item.get("source_times_ms", [])},
                sizes,
            )
        )
    return {"segment_display_delay_ms": sample_entry(sorted(values))}
