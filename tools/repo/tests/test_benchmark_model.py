"""Benchmark facts: strict validation, quantiles, and the comparison protocol."""

from __future__ import annotations

import json
import runpy
from pathlib import Path
from types import SimpleNamespace

import pytest

from repo.benchmark import analysis, model, runner, summary
from repo.errors import RepoError


def attempt(identity="a-1", outcome="completed", phase="measured", **extra):
    record = {
        "id": identity,
        "phase": phase,
        "outcome": outcome,
        "deadline_ms": 5000,
        "first_text_ms": 120.0,
        "final_ms": 260.0,
        "send_exit_ms": 300.0,
        "update_gaps_ms": [100.0, 100.0],
        "update_count": 3,
        "duplicate_finals": 0,
    }
    record.update(extra)
    return record


def document(attempts=None, **changes):
    attempts = attempts if attempts is not None else [attempt()]
    measured = [item for item in attempts if item["phase"] == "measured"]
    completed = [item for item in measured if item["outcome"] == "completed"]
    counts = {name: sum(item["outcome"] == name for item in measured) for name in model.OUTCOMES}
    gaps = sorted(
        float(gap)
        for item in completed
        for gap in item["update_gaps_ms"]
        if isinstance(gap, (int, float))
    )
    document = {
        "schema_version": model.SCHEMA_VERSION,
        "scenario": {
            "id": model.SCENARIO_ID,
            "version": 1,
            "seed": "0",
            "deadline_ms": 5000,
            "observation_window_ms": 1000,
            "workload": {"updates": ["a", "b"], "gap_ms": 100, "delivery": "accepted"},
            "response": {
                "start_ms": 0,
                "final_ms": 200,
                "delay_ms": 0,
                "duplicate_final_ms": 500,
                "final_text": "ab",
            },
        },
        "run": {
            "id": "run-1",
            "started_at": "2026-11-14T00:00:00Z",
            "surface": model.OBSERVATION_SURFACE,
            "configuration": {
                "attempts": len(measured),
                "warmup_attempts": len([item for item in attempts if item["phase"] == "warmup"]),
                "concurrency": 1,
                "profile": "warm-client",
            },
        },
        "environment": {
            "artifacts": {"cli": "sha256:" + "0" * 64},
            "topology": {"containers": 1},
            "limits": {},
            "versions": {},
            "fixture": {},
        },
        "attempts": attempts,
        "outcomes": {
            "counts": counts,
            "measured": len(measured),
            "correct_by_deadline": counts["completed"],
            "success_share": (counts["completed"] / len(measured)) if measured else None,
            "warmup": {
                "attempts": len([item for item in attempts if item["phase"] == "warmup"]),
                "completed": sum(
                    item["outcome"] == "completed" for item in attempts if item["phase"] == "warmup"
                ),
            },
        },
        "latency": {
            "samples": {
                "first_text_ms": sample(times(completed, "first_text_ms")),
                "final_ms": sample(times(completed, "final_ms")),
                "update_gaps_ms": sample(gaps),
            },
        },
        "status": {"state": "complete", "reasons": [], "cleanup": {"result": "verified"}},
    }
    document.update(changes)
    return document


def streaming_document(count=1, extra_rtt=0, *, with_previews=True, version=model.SCENARIO_VERSION):
    profile = "high-rtt" if extra_rtt else "normal"
    configuration = runner.BenchRequest(
        attempts=count, warmup=0, cadence_ms=10, network_profile=profile
    ).as_configuration()
    workload = model.scenario(configuration)
    workload["version"] = version
    if version < 6:
        configuration.pop("workflow")
        workload["workload"].pop("workflow")
    if version < 4:
        workload["workload"]["delivery"] = "accepted"
    if version < 5:
        configuration.pop("network")
        workload.pop("network")
        configuration["host_broker_extra_rtt_ms"] = extra_rtt
        workload["host_broker_extra_rtt_ms"] = extra_rtt
    sizes = model.source_sizes(workload) if version >= 4 else None
    final_bytes = len(workload["response"]["final_text"].encode())
    previews = (
        [
            {"at_ms": 100, "bytes": final_bytes // 10, "final": False},
            {"at_ms": 180, "bytes": final_bytes // 10, "final": False},
        ]
        if with_previews
        else []
    )
    outputs = [
        *previews,
        {
            "at_ms": 600,
            "bytes": final_bytes - sum(event["bytes"] for event in previews),
            "final": True,
        },
    ]
    if sizes is not None:
        prefix = 0
        for event in outputs:
            prefix += event["bytes"]
            event["prefix_bytes"] = prefix
    attempts = [
        attempt(
            str(index),
            first_text_ms=100 if with_previews else 600 if sizes is not None else None,
            final_ms=600,
            update_count=len(previews),
            update_gaps_ms=[80] if with_previews else [],
            source_times_ms=[
                (0 if sizes is not None else 50) + index * 10 for index in range(len(model.UPDATES))
            ],
            outputs=outputs,
        )
        for index in range(count)
    ]
    facts = document(attempts, scenario=workload)
    facts["run"]["configuration"] = configuration
    probe = {
        "reference_ms": [0.25] * model.NETWORK_PROBE_COUNT,
        "host_broker_ms": [extra_rtt + 0.25] * model.NETWORK_PROBE_COUNT,
    }
    burst_window = model.BURST_WINDOW_MS if version >= 5 else None
    if burst_window is not None:
        probe["host_ms"] = probe.pop("host_broker_ms")
        probe["client_ms"] = probe["reference_ms"]
        facts["environment"]["network_counters"] = {"packets": 42, "dropped": 0}
        facts["environment"]["artifacts"]["harness"] = "sha256:" + "b" * 64
    facts["environment"]["network_probe"] = probe
    facts["streaming"] = model.streaming_summary(
        attempts, final_bytes, sizes, burst_window, display_delay=version >= 6
    )
    if version >= 6:
        facts["workflow_results"] = {}
    return facts


@pytest.mark.parametrize(
    ("with_previews", "expected_gap", "expected_share"),
    [(True, 420, 0.8), (False, 600, 1.0)],
)
def test_streaming_includes_pending_text_until_the_final_output(
    with_previews,
    expected_gap,
    expected_share,
):
    facts = model.validate(streaming_document(with_previews=with_previews))
    streaming = facts["streaming"]
    assert streaming["zero_preview"] == (0 if with_previews else 1)
    samples = streaming["samples"]
    assert samples["longest_output_gap_ms"]["quantiles"]["median"] == expected_gap
    assert samples["largest_output_share"]["quantiles"]["median"] == expected_share
    assert samples["final_output_share"]["quantiles"]["median"] == expected_share
    assert f"median={expected_share * 100:.1f}%" in summary.render(facts)


def test_streaming_can_have_its_largest_output_before_the_final():
    final_bytes = 100
    record = {
        "source_times_ms": [10],
        "outputs": [
            {"at_ms": 70, "bytes": 80, "final": False},
            {"at_ms": 90, "bytes": 20, "final": True},
        ],
    }
    assert model.streaming_values(record, final_bytes) == {
        "longest_output_gap_ms": 60,
        "largest_output_share": 0.8,
        "final_output_share": 0.2,
    }


def test_streaming_counts_the_settlement_gap_with_a_zero_byte_final():
    record = {
        "source_times_ms": [10],
        "outputs": [
            {"at_ms": 30, "bytes": 100, "final": False},
            {"at_ms": 300, "bytes": 0, "final": True},
        ],
    }
    assert model.streaming_values(record, 100) == {
        "longest_output_gap_ms": 270,
        "largest_output_share": 1,
        "final_output_share": 0,
    }


def test_pending_gap_excludes_source_idle_and_visible_text_free_final():
    record = {
        "source_times_ms": [0, 100],
        "outputs": [
            {"at_ms": 20, "bytes": 10, "prefix_bytes": 10, "final": False},
            {"at_ms": 125, "bytes": 10, "prefix_bytes": 20, "final": False},
            {"at_ms": 400, "bytes": 0, "prefix_bytes": 20, "final": True},
        ],
    }
    assert model.streaming_values(record, 20, [10, 10]) == {
        "longest_output_gap_ms": 25,
        "largest_output_share": 0.5,
        "final_output_share": 0,
    }


def test_pending_wait_continues_while_a_preview_only_repeats_shown_bytes():
    outputs = [
        {"at_ms": 20, "prefix_bytes": 10},
        {"at_ms": 50, "prefix_bytes": 10},
        {"at_ms": 100, "prefix_bytes": 20},
    ]
    assert model.longest_pending_gap(outputs, [0, 10], [10, 10]) == 80


def test_legacy_netem_report_keeps_its_original_gap_definition():
    facts = streaming_document(version=3, with_previews=False)
    samples = model.validate(facts)["streaming"]["samples"]
    assert samples["longest_output_gap_ms"]["quantiles"]["median"] == 550


def test_streaming_observer_counts_cli_text_kind_and_only_the_final_suffix(monkeypatch):
    integration = Path(__file__).resolve().parents[3] / "tests" / "integration"
    monkeypatch.syspath_prepend(str(integration))
    observer = runpy.run_path(str(integration / "benchmark" / "main.py"))["Observer"]
    monkeypatch.setitem(observer.observe.__globals__, "FINAL_BYTES", "éx".encode())
    state = SimpleNamespace(stream="attempt", finals=0, outputs=[], preview=b"")
    observer.observe(
        state, {"type": "preview", "attempt_id": "attempt", "kind": "TEXT", "text": "é"}, 20
    )
    observer.observe(
        state,
        {
            "type": "message",
            "role": "ASSISTANT",
            "origin": {"attempt_id": "attempt"},
            "blocks": [{"type": "text", "text": "éx"}],
            "preview_prefix": {"text_bytes": 2},
        },
        30,
    )
    assert state.outputs == [
        {"at_ns": 20, "bytes": 2, "prefix_bytes": 2, "final": False},
        {"at_ns": 30, "bytes": 1, "prefix_bytes": 3, "final": True},
    ]


def test_streaming_rejects_false_summaries_and_invalid_source_times():
    facts = streaming_document()
    facts["streaming"]["zero_preview"] = 1
    with pytest.raises(RepoError, match="streaming summary disagrees"):
        model.validate(facts)
    facts = streaming_document()
    facts["attempts"][0]["source_times_ms"][0] = 999
    with pytest.raises(RepoError, match="ordered source emission times"):
        model.validate(facts)


def test_display_delay_detects_backlog_even_when_output_keeps_arriving():
    outputs = [
        {"at_ms": time, "prefix_bytes": offset}
        for time, offset in ((30, 10), (80, 20), (130, 30), (180, 40))
    ]
    assert model.longest_pending_gap(outputs, [0, 10, 20, 30], [10] * 4) == 50
    assert model.max_display_delay(outputs, [0, 10, 20, 30], [10] * 4) == 150


def test_display_diagnostics_use_each_increment_while_excluding_warmups_and_failures():
    facts = streaming_document(with_previews=False)
    sizes = model.source_sizes(facts["scenario"])
    attempts = [
        *facts["attempts"],
        attempt("warm", phase="warmup"),
        attempt("failed", outcome="timeout_before_text"),
    ]
    diagnostics = model.diagnostic_summary(attempts, sizes)
    sample = diagnostics["segment_display_delay_ms"]
    assert sample["count"] == len(model.UPDATES)
    assert sample["quantiles"] == {"median": 440, "p95": 590}
    facts["diagnostics"] = diagnostics
    text = summary.render(model.validate(facts))
    assert "Diagnostics\nfirst text: n=1 median=600.0ms" in text
    assert "per-segment display delay: n=32 median=440.0ms p95=590.0ms" in text

    sample["quantiles"]["p95"] = 589
    with pytest.raises(RepoError, match="diagnostic summary disagrees"):
        model.validate(facts)


def mixed_load_document(pairs=2, *, bulk_failure=None, count=None):
    """A paired v7 mixed-load document built from completed Watch attempts."""
    pair_count = count if count is not None else pairs * 2
    facts = streaming_document(count=pair_count)
    configuration = facts["run"]["configuration"]
    configuration["workflow"] = "mixed-load"
    facts["scenario"] = model.scenario(configuration)
    workload = facts["scenario"]["workload"]
    history_expected = sum(len(text.encode()) for text in model.history_texts(workload["history"]))
    attachment_expected = workload["attachment"]["bytes"]
    for index, attempt in enumerate(facts["attempts"]):
        attempt["arm"] = model.ARMS[index % 2]
        attempt["pair_id"] = f"pair-{index // 2:04d}"
        if attempt["arm"] != "bulk":
            continue
        bulk_ordinal = index // 2
        attempt["history_reads"] = [
            read_observation(index, history_expected, history_expected)
            for index in range(len(workload["history"]["start_ms"]))
        ]
        attempt["attachment_reads"] = [
            read_observation(
                index,
                attachment_expected,
                attachment_expected,
                failure=bulk_failure if bulk_ordinal == 0 and index == 0 else None,
            )
            for index in range(len(workload["attachment"]["start_ms"]))
        ]
    facts["workflow_results"] = model.workflow_summary(
        facts["attempts"],
        "mixed-load",
        final_bytes=len(facts["scenario"]["response"]["final_text"].encode()),
        sizes=model.source_sizes(facts["scenario"]),
        paired=True,
        burst_window_ms=model.BURST_WINDOW_MS,
    )
    return facts


def read_observation(index, expected_bytes, success_bytes, failure=None):
    observation = {
        "scheduled_at_ms": index * 100.0,
        "started_at_ms": index * 100.0,
        "at_ms": index * 100.0,
        "duration_ms": 80.0,
        "outcome": "completed",
        "bytes": success_bytes,
        "expected_bytes": expected_bytes,
    }
    if failure:
        observation.update(outcome="product_error", bytes=0, error_code=failure)
    return observation


def test_mixed_load_preserves_read_failures_separately_from_watch():
    facts = mixed_load_document(pairs=2, bulk_failure="rate_limited")
    measured = model.validate(facts)
    workflow = measured["workflow_results"]
    # Two bulk attempts each schedule four history reads; the first attachment
    # read fails, the second verifies its bytes.
    assert workflow["history_correct"] == 8
    assert workflow["history_measured"] == 8
    assert workflow["attachment_correct"] == 1
    assert workflow["attachment_measured"] == 2
    assert workflow["attachment_errors"] == {"rate_limited": 1}
    assert workflow["bulk"]["expected_reads"] == 10
    assert workflow["bulk"]["successful_reads"] == 9
    assert workflow["bulk"]["errors"] == {"rate_limited": 1}
    assert workflow["bulk"]["gate"]["state"] == "incomplete"
    assert "bulk_read_errors" in workflow["bulk"]["gate"]["reasons"]
    assert workflow["arms"]["idle"]["correct"] == 2
    assert workflow["arms"]["bulk"]["correct"] == 2
    assert len(workflow["paired"]["pairs"]) == 2
    assert all(pair["deltas"]["longest_output_gap_ms"] == 0 for pair in workflow["paired"]["pairs"])
    # Two idle samples are one A/A difference; the gate says so instead of passing.
    assert workflow["paired"]["gate"]["state"] == "incomplete"
    assert "insufficient_aa_noise" in workflow["paired"]["gate"]["reasons"]
    text = summary.render(measured)
    assert "Background history reads: 8/8" in text
    assert "Background attachment reads: 1/2" in text
    assert "Bulk read gate: incomplete" in text
    assert "idle arm: 2/2 correct" in text
    assert "Idle/bulk gate: incomplete" in text
    facts["attempts"][1]["attachment_reads"].pop()
    with pytest.raises(RepoError, match="every scheduled attachment read"):
        model.validate(facts)


def test_mixed_load_requires_alternating_complete_pairs():
    facts = mixed_load_document(pairs=2)
    facts["attempts"][1]["pair_id"] = "pair-0001"
    with pytest.raises(RepoError, match="one bulk attempt per idle pair"):
        model.validate(facts)
    facts = mixed_load_document(pairs=2)
    facts["attempts"][0]["arm"] = "bulk"
    facts["attempts"][1]["arm"] = "idle"
    with pytest.raises(RepoError, match="alternate idle then bulk"):
        model.validate(facts)
    facts = mixed_load_document(pairs=2)
    facts["attempts"][0]["history_reads"] = facts["attempts"][1]["history_reads"]
    with pytest.raises(RepoError, match="idle arm must not perform bulk reads"):
        model.validate(facts)


@pytest.mark.parametrize(
    ("code", "expected"), [("rate_limited", "rate_limited"), ("invalid/code", None)]
)
def test_history_read_retains_only_a_safe_public_code(monkeypatch, code, expected):
    integration = Path(__file__).resolve().parents[3] / "tests" / "integration"
    monkeypatch.syspath_prepend(str(integration))
    workflow = runpy.run_path(str(integration / "benchmark" / "workflow.py"))
    response = {
        "schema": "whalerein/client/v1",
        "command": "session.history",
        "error": {"code": code, "message": "A detailed failure message"},
    }
    monkeypatch.setattr(
        workflow["subprocess"],
        "run",
        lambda *args, **kwargs: SimpleNamespace(returncode=1, stdout=json.dumps(response)),
    )
    reader = SimpleNamespace(args=lambda *args: args, env={}, envelope=json.loads)
    observation = workflow["HistoryLoad"](reader, "host", "session", ["expected"]).read(0, 1000)
    assert observation["outcome"] == "product_error"
    assert observation.get("error_code") == expected
    assert "message" not in observation


def test_legacy_v6_mixed_load_report_keeps_its_history_only_schema():
    facts = streaming_document(version=6)
    configuration = facts["run"]["configuration"]
    configuration["workflow"] = "mixed-load"
    facts["scenario"] = model.scenario(configuration)
    facts["scenario"]["version"] = 6
    facts["scenario"]["workload"].pop("attachment")
    facts["scenario"]["workload"].pop("pairing")
    facts["attempts"][0]["history_reads"] = [read_observation(index, 64, 64) for index in range(4)]
    facts["workflow_results"] = model.workflow_summary(facts["attempts"], "mixed-load")
    measured = model.validate(facts)
    workflow = measured["workflow_results"]
    assert "attachment_measured" not in workflow
    assert "arms" not in workflow
    assert "paired" not in workflow
    assert workflow["history_correct"] == 4


def test_recovery_requires_source_activity_during_the_blackout():
    facts = streaming_document()
    configuration = facts["run"]["configuration"]
    configuration.update(workflow="recovery", network=model.network_settings("normal", "client"))
    facts["scenario"] = model.scenario(configuration)
    timing = {"started_ms": 80, "restored_ms": 180, "catch_up_ms": 420}
    facts["attempts"][0]["recovery"] = timing
    facts["workflow_results"] = model.workflow_summary(facts["attempts"], "recovery")
    assert model.validate(facts)["workflow_results"]["catch_up_ms"]["quantiles"]["median"] == 420
    timing.update(started_ms=350, restored_ms=450)
    with pytest.raises(RepoError, match="source must continue"):
        model.validate(facts)


def test_burst_counts_records_arriving_within_the_same_window():
    outputs = [
        {"at_ms": 5, "bytes": 4},
        {"at_ms": 30, "bytes": 6},
        {"at_ms": 55, "bytes": 2},
        {"at_ms": 150, "bytes": 0},
    ]
    assert model.largest_burst(outputs, 50) == 10


def test_network_probe_detects_shaping_on_the_wrong_link():
    facts = streaming_document(extra_rtt=280)
    probe = facts["environment"]["network_probe"]
    probe["host_ms"], probe["client_ms"] = probe["client_ms"], probe["host_ms"]
    with pytest.raises(RepoError, match="observed RTT"):
        model.validate(facts)


def test_network_probe_checks_the_requested_delay_and_rejects_unshaped_measurement():
    facts = streaming_document(extra_rtt=280)
    assert model.validate(facts) == facts
    assert "host 280.0 ms, client 0.0 ms" in summary.render(facts)
    facts["environment"]["network_probe"]["host_ms"] = [1.0] * model.NETWORK_PROBE_COUNT
    with pytest.raises(RepoError, match="observed RTT"):
        model.validate(facts)
    facts["environment"].pop("network_probe")
    with pytest.raises(RepoError, match="requires network samples"):
        model.validate(facts)


def test_legacy_ack_floor_reports_remain_readable_under_their_own_scenario():
    facts = streaming_document(version=2)
    facts["scenario"]["event_ack_floor_ms"] = 280
    facts["run"]["configuration"]["event_ack_floor_ms"] = 280
    facts["environment"].pop("network_probe")
    assert "Host event completion floor: 280 ms" in summary.render(model.validate(facts))


def times(items, key):
    """Only well-formed times feed the sample summary; malformed ones are the subject."""
    return sorted(
        float(item[key])
        for item in items
        if isinstance(item.get(key), (int, float)) and item.get(key) is not None
    )


def sample(values):
    if not values:
        return {"count": 0, "quantiles": {}, "unavailable": {}, "reason": "no_samples"}
    return {
        "count": len(values),
        "quantiles": supported(values),
        "unavailable": {
            name: model.UNAVAILABLE_TAIL for name, minimum in model.TAILS if len(values) < minimum
        },
        "reason": None,
    }


def supported(values):
    computed = model.quantiles(values)
    return {
        name: computed[name]
        for name, minimum in model.TAILS
        if len(values) >= minimum and name in computed
    }


def test_nearest_rank_quantiles_match_the_recorded_sample_count():
    values = list(range(1, 21))
    assert model.quantiles(values) == {"median": 10, "p95": 19, "p99": 20}


def test_quantiles_are_exact_for_single_sample():
    assert model.quantiles([4.5]) == {"median": 4.5, "p95": 4.5, "p99": 4.5}


def test_quantiles_are_empty_without_samples():
    assert model.quantiles([]) == {}


def test_valid_document_round_trips_through_json(tmp_path):
    path = tmp_path / "benchmark.json"
    path.write_text(json.dumps(document()))
    assert model.read(path)["run"]["id"] == "run-1"


@pytest.mark.parametrize(
    ("change", "message"),
    [
        ({"schema_version": 2}, "unsupported schema_version"),
        ({"attempts": []}, "run configuration attempts must be a positive integer"),
        ({"outcomes": []}, "outcomes is not an object"),
        ({"status": {"cleanup": {"result": "verified"}}}, "status needs a state"),
    ],
)
def test_document_sections_are_required(change, message):
    with pytest.raises(RepoError, match=message):
        model.validate(document(**change))


@pytest.mark.parametrize(
    "attempts",
    [
        [attempt(identity="a-1"), attempt(identity="a-1")],
        [attempt(outcome="unknown", first_text_ms=None, final_ms=None)],
        [attempt(phase="setup")],
        [attempt(outcome="completed", final_ms=None)],
        [attempt(outcome="timeout_before_text", first_text_ms=10.0)],
        [attempt(outcome="product_error", final_ms=200.0)],
        [attempt(first_text_ms=200.0, final_ms=100.0)],
        [attempt(first_text_ms=260.0, final_ms=5200.0)],
        [attempt(update_gaps_ms=["slow"], update_count=3)],
    ],
)
def test_attempt_records_are_validated(attempts):
    with pytest.raises(RepoError):
        model.validate(document(attempts))


def test_outcome_counts_must_match_measured_attempts():
    attempts = [attempt("a-1"), attempt("a-2", outcome="timeout_after_text")]
    facts = document(attempts)
    facts["outcomes"]["counts"]["completed"] = 2
    with pytest.raises(RepoError, match="outcome counts disagree"):
        model.validate(facts)


def test_a_late_reply_is_never_counted_as_a_completion():
    """A reply past its deadline keeps the failure outcome and its observed milestones."""
    late = attempt("a-1", outcome="late_reply", final_ms=None, deadline_ms=5000)
    facts = document([late])
    assert facts["outcomes"]["correct_by_deadline"] == 0
    assert facts["attempts"][0]["first_text_ms"] == 120.0
    model.validate(facts)
    # A recorded final time would make it look measured within the deadline.
    facts["attempts"][0]["final_ms"] = 1200.0
    with pytest.raises(RepoError, match="late_reply cannot record a final reply time"):
        model.validate(facts)


def test_warmup_attempts_are_excluded_from_measured_counts():
    attempts = [attempt("w-1", phase="warmup"), attempt("a-1")]
    facts = document(attempts)
    assert facts["outcomes"]["measured"] == 1
    assert facts["outcomes"]["warmup"] == {"attempts": 1, "completed": 1}
    model.validate(facts)
    facts["outcomes"]["warmup"]["attempts"] = 0
    with pytest.raises(RepoError, match="warmup counts disagree"):
        model.validate(facts)


def test_quantiles_must_agree_with_the_attempt_samples():
    facts = document([attempt("a-1"), attempt("a-2", first_text_ms=200.0)])
    facts["latency"]["samples"]["first_text_ms"]["quantiles"]["median"] = 999.0
    with pytest.raises(RepoError, match="disagrees with samples"):
        model.validate(facts)


def test_unavailable_quantiles_stay_unavailable_without_samples():
    facts = document([attempt("a-1", outcome="product_error", first_text_ms=None, final_ms=None)])
    facts["latency"]["samples"]["final_ms"]["quantiles"] = {"median": 1.0}
    with pytest.raises(RepoError, match="empty without samples"):
        model.validate(facts)


def test_cleanup_and_status_reasons_are_required_when_not_complete():
    facts = document()
    facts["status"] = {"state": "invalid", "reasons": [], "cleanup": {"result": "verified"}}
    with pytest.raises(RepoError, match="must state its reasons"):
        model.validate(facts)
    facts["status"] = {"state": "complete", "reasons": [], "cleanup": {"result": "unverified"}}
    with pytest.raises(RepoError, match="must state a reason"):
        model.validate(facts)


def test_recorded_cadence_must_match_the_workload():
    facts = document()
    facts["run"]["configuration"]["cadence_ms"] = 20
    with pytest.raises(RepoError, match="gap_ms disagrees with the recorded cadence"):
        model.validate(facts)
    facts["scenario"]["workload"]["gap_ms"] = 20
    model.validate(facts)
    facts["run"]["configuration"]["cadence_ms"] = 0
    with pytest.raises(RepoError, match="cadence_ms must be a positive integer"):
        model.validate(facts)


def test_recorded_deadline_must_precede_scripted_settlement():
    facts = document()
    facts["scenario"]["deadline_ms"] = 100
    with pytest.raises(RepoError, match="precede the deadline"):
        model.validate(facts)


def test_scripted_final_text_must_match_the_updates():
    facts = document()
    facts["scenario"]["response"]["final_text"] = "ax"
    with pytest.raises(RepoError, match="must equal the scripted updates"):
        model.validate(facts)


def test_unreadable_export_is_reported_as_invalid(tmp_path):
    path = tmp_path / "benchmark.json"
    path.write_text("{")
    with pytest.raises(RepoError, match="unreadable document"):
        model.read(path)


def test_summary_reports_the_same_counts_and_labels_unavailable_tails():
    facts = document(
        [
            attempt("a-1"),
            attempt("a-2", outcome="timeout_before_text", first_text_ms=None, final_ms=None),
        ]
    )
    text = summary.render(facts)
    assert "Measured attempts: 2  correct by deadline: 1 (50.0%)" in text
    assert "timeout_before_text=1" in text
    assert "Status: complete" in text
    assert "Cleanup: verified" in text


def test_summary_labels_unavailable_tails_with_their_reason():
    facts = document([attempt("a-1")])
    text = summary.render(facts)
    # One sample cannot support p95 or p99; they are labelled, not invented.
    assert "p95=unavailable(n>=20,insufficient_samples)" in text
    assert "p99=unavailable(n>=100,insufficient_samples)" in text
    assert "median=" in text


def test_paired_interval_is_computed_over_run_differences():
    def run(value):
        return [
            {"outcome": "completed", "first_text_ms": value, "final_ms": value} for _ in range(20)
        ]

    baseline = [run(100.0) for _ in range(5)]
    candidate = [run(150.0) for _ in range(5)]
    result = analysis.compare(baseline, candidate)
    entry = result["metrics"]["first_text_ms"]
    assert entry["estimate_ms"] == pytest.approx(50.0)
    assert entry["verdict"] == "regression"
    assert entry["interval_ms"][0] > entry["tolerance_ms"]


def test_insufficient_pairs_are_inconclusive():
    baseline = [[{"outcome": "completed", "first_text_ms": 100.0}]]
    candidate = [[{"outcome": "completed", "first_text_ms": 400.0}]]
    result = analysis.compare(baseline, candidate)
    assert result["pairs"] == 1
    assert all(entry["verdict"] == "inconclusive" for entry in result["metrics"].values())
    assert "insufficient_pairs" in result["reasons"]


def test_interval_crossing_the_tolerance_is_inconclusive():
    class Stub:
        @staticmethod
        def interval(low, high, estimate=0.0):
            return estimate, low, high

    assert analysis.classify(0.0, -30.0, 5.0, 25.0) == (
        "inconclusive",
        "interval_crosses_tolerance",
    )
    assert analysis.classify(0.0, -10.0, 10.0, 25.0) == (
        "within_tolerance",
        "interval_within_tolerance",
    )
    assert analysis.classify(-50.0, -70.0, -30.0, 25.0) == (
        "improvement",
        "interval_beyond_lower_tolerance",
    )


def test_run_checks_mark_insufficient_samples_incomplete():
    facts = document([attempt("a-1")])
    checks = analysis.run_checks(facts)
    assert checks["state"] == "incomplete"
    assert any(item["name"] == "attempts_below_declared_floor" for item in checks["reasons"])


def paired_arm(identity, gap, share, *, delay=10.0, final=0.2):
    return {
        "max_display_delay_ms": delay,
        "longest_output_gap_ms": gap,
        "largest_output_share": share,
        "final_output_share": final,
    }


def paired_arm_pairs(idle_gaps, bulk_gaps, idle_shares=None, bulk_shares=None):
    idle_shares = idle_shares or [0.1] * len(idle_gaps)
    bulk_shares = bulk_shares or idle_shares
    return [
        {
            "pair_id": f"pair-{index:04d}",
            "values": {
                "idle": paired_arm("i", idle_gaps[index], idle_shares[index]),
                "bulk": paired_arm("b", bulk_gaps[index], bulk_shares[index]),
            },
            "reason": None,
        }
        for index in range(len(idle_gaps))
    ]


def test_paired_arms_derive_tolerance_from_idle_aa_noise():
    # Idle changes by at most 2 ms, so the declared 2x noise tolerance is 4 ms.
    pairs = paired_arm_pairs([100, 102, 101, 103, 102, 104], [103] * 6)
    result = analysis.paired_arm_comparison(pairs)
    entry = result["gate"]["metrics"]["longest_output_gap_ms"]
    assert entry["aa_noise"] == 2
    assert entry["aa_differences"] == 5
    assert entry["tolerance"] == 4
    assert entry["mean_delta"] == 1
    assert entry["verdict"] == "within_tolerance"
    assert result["gate"]["state"] == "complete"
    assert [pair["deltas"]["longest_output_gap_ms"] for pair in result["pairs"]] == [
        3,
        1,
        2,
        0,
        1,
        -1,
    ]

    excessive = paired_arm_pairs([100, 102, 101, 103, 102, 104], [107] * 6)
    gate = analysis.paired_arm_comparison(excessive)["gate"]
    assert gate["metrics"]["longest_output_gap_ms"]["mean_delta"] == 5
    assert gate["metrics"]["longest_output_gap_ms"]["verdict"] == "regression"
    assert gate["state"] == "failed"
    assert "bulk_degradation" in gate["reasons"]


def test_paired_arms_gate_share_and_require_aa_samples():
    pairs = paired_arm_pairs(
        [100] * 6,
        [100] * 6,
        idle_shares=[0.10, 0.11, 0.10, 0.11, 0.10, 0.11],
        bulk_shares=[0.15] * 6,
    )
    gate = analysis.paired_arm_comparison(pairs)["gate"]
    share = gate["metrics"]["largest_output_share"]
    assert share["aa_noise"] == pytest.approx(0.01)
    assert share["tolerance"] == pytest.approx(0.02)
    assert share["verdict"] == "regression"

    short = analysis.paired_arm_comparison(paired_arm_pairs([100, 101], [101, 102]))
    assert short["gate"]["state"] == "incomplete"
    assert short["gate"]["reasons"] == ["insufficient_aa_noise"]
    assert short["gate"]["metrics"]["longest_output_gap_ms"]["tolerance"] is None


def test_paired_arms_report_incomplete_pairs_without_inventing_deltas():
    pairs = [
        {
            "pair_id": "pair-0000",
            "values": {
                "idle": paired_arm("i", 100, 0.1),
                "bulk": paired_arm("b", 120, 0.2),
            },
            "reason": None,
        },
        {"pair_id": "pair-0001", "values": None, "reason": "incomplete_pair"},
    ]
    result = analysis.paired_arm_comparison(pairs)
    assert result["pairs"][1] == {
        "pair_id": "pair-0001",
        "deltas": {metric: None for metric in model.PAIR_STREAMING_METRICS},
        "reason": "incomplete_pair",
    }


def test_bulk_read_gate_requires_every_read_and_verified_byte():
    complete = {
        "expected_reads": 5,
        "successful_reads": 5,
        "bytes_expected": 1024,
        "bytes_verified": 1024,
        "errors": {},
    }
    assert analysis.bulk_read_gate(complete)["state"] == "complete"
    partial = {**complete, "successful_reads": 4, "errors": {"rate_limited": 1}}
    gate = analysis.bulk_read_gate(partial)
    assert gate["state"] == "incomplete"
    assert gate["reasons"] == ["bulk_reads_not_successful", "bulk_read_errors"]
    assert analysis.bulk_read_gate({**complete, "bytes_verified": 1023})["reasons"] == [
        "bulk_bytes_not_verified"
    ]


def test_attachment_read_retains_only_a_safe_public_code(monkeypatch):
    integration = Path(__file__).resolve().parents[3] / "tests" / "integration"
    monkeypatch.syspath_prepend(str(integration))
    workflow = runpy.run_path(str(integration / "benchmark" / "workflow.py"))
    response = {
        "schema": "whalerein/client/v1",
        "command": "session.download",
        "error": {"code": "not_found", "message": "A detailed failure message"},
    }
    monkeypatch.setattr(
        workflow["subprocess"],
        "run",
        lambda *args, **kwargs: SimpleNamespace(returncode=3, stdout=json.dumps(response)),
    )
    reader = SimpleNamespace(args=lambda *args: args, env={}, envelope=json.loads)
    load = workflow["AttachmentLoad"](
        reader, "host", "session", "0" * 64, 4, "0" * 64, "fixture.bin"
    )
    observation = load.read(0, 1000)
    assert observation["outcome"] == "product_error"
    assert observation["error_code"] == "not_found"
    assert observation["bytes"] == 0
    assert observation["expected_bytes"] == 4
    assert "message" not in observation


def test_benchmark_cli_reports_result_and_compares_saved_evidence(monkeypatch, capsys, tmp_path):
    from repo import cli
    from repo.benchmark import runner

    baseline = tmp_path / "baseline"
    comparison = tmp_path / "comparison.json"

    def measure(root, process, request, directory):
        assert request.attempts == 1
        assert request.warmup == 0
        assert directory == baseline
        result = document()
        directory.mkdir()
        path = directory / "benchmark.json"
        path.write_text(json.dumps(result))
        return runner.BenchResult(result, 0, path)

    monkeypatch.setattr(cli.benchmark, "run", measure)
    assert (
        cli.main(
            [
                "bench",
                "--network",
                "normal",
                "--attempts",
                "1",
                "--warmup",
                "0",
                "--output",
                str(baseline),
                "--json",
            ]
        )
        == 0
    )
    measured = json.loads(capsys.readouterr().out)
    assert measured["data"]["report"] == str(baseline / "benchmark.json")
    assert measured["data"]["outcomes"]["correct_by_deadline"] == 1
    assert (
        cli.main(
            [
                "bench-compare",
                "-b",
                str(baseline),
                "-c",
                str(baseline),
                "--output",
                str(comparison),
                "--json",
            ]
        )
        == 0
    )
    compared = json.loads(capsys.readouterr().out)
    assert compared["data"]["comparison"]["verdict"] == "inconclusive"
    assert json.loads(comparison.read_text()) == compared["data"]["comparison"]
