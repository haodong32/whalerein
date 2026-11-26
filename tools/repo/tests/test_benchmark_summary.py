"""Terminal summary renders model-derived rows without recomputing them."""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

from repo.benchmark import model, summary

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "tests" / "integration"))

from benchmark import facts, slots, source  # noqa: E402


def configuration(workflow="watch", observation="trace"):
    return {
        "attempts": 2,
        "warmup_attempts": 0,
        "concurrency": 1,
        "profile": "warm-client",
        "delay_ms": 0,
        "cadence_ms": 100,
        "network": model.network_settings("normal", "host"),
        "workflow": workflow,
        "deadline_ms": 10000,
        "observation_window_ms": 1000,
        "seed": 0,
        "observation_mode": observation,
    }


def report(workflow="watch", observation="trace"):
    configuration_ = configuration(workflow, observation)
    scenario = facts.scenario_controls(model.scenario(configuration_))
    schedule = slots.plan(configuration_)

    class Capture:
        run_id = "run-summary"
        mode = observation

    run = facts.run_block(
        capture=Capture(),
        configuration=configuration_,
        started_at="2026-01-01T00:00:00Z",
        fingerprint={
            "build": {"revision": "synthetic", "fingerprint": "sha256:synthetic"},
            "fixture": {"name": "FakeDSH", "fingerprint": "sha256:fixture"},
            "schemas": {"case": 2, "event": 1, "workload": 8},
            "platform": {"os": "linux", "arch": "x86_64", "kernel": "test"},
            "limits": {"cpus": 1, "memory_bytes": 1024},
            "clock": {"source": "CLOCK_MONOTONIC", "unit": "ns", "verified": True},
            "sampling": {"socket_hz": 1, "runtime_hz": 1},
        },
    )
    environment = facts.environment_block(
        artifacts={"cli": "sha256:cli"},
        limits={"cpus": 1, "memory_bytes": 1024},
        versions={"python": "3.13"},
        fixture={"name": "FakeDSH", "fingerprint": "sha256:fixture"},
        clock={
            "source": "CLOCK_MONOTONIC",
            "unit": "ns",
            "boot_id_verified": True,
            "namespace_verified": True,
            "producers": [{"producer": "core", "lifetime": "lt-1", "verified": True}],
        },
        capture_origin_ns=1000,
    )
    attempts = []
    for slot in schedule["slots"]:
        reads = None
        if slot["arm"] == "bulk":
            reads = [
                {
                    "id": f"history-{index}",
                    "op": "history",
                    "scheduled_at_ms": 0.0,
                    "started_at_ms": 1.0,
                    "finished_at_ms": 2.0,
                    "deadline_ms": 10000,
                    "pages": 1,
                    "messages": 1,
                    "bytes_expected": 4,
                    "bytes_verified": 4,
                    "physical_bytes": 4,
                    "route": "inline",
                    "outcome": "completed",
                    "verified": True,
                    "token_repeats": 0,
                    "corrupt": False,
                }
                for index in range(4)
            ] + [
                {
                    "id": "attachment-0",
                    "op": "attachment",
                    "scheduled_at_ms": 0.0,
                    "started_at_ms": 1.0,
                    "finished_at_ms": 2.0,
                    "deadline_ms": 10000,
                    "pages": 0,
                    "messages": 0,
                    "bytes_expected": 8,
                    "bytes_verified": 8,
                    "physical_bytes": 8,
                    "route": "reference",
                    "outcome": "completed",
                    "verified": True,
                    "token_repeats": 0,
                    "corrupt": False,
                }
            ]
        attempts.append(
            facts.attempt(
                slot=slot["slot"],
                phase=slot["phase"],
                arm=slot["arm"],
                pair_id=slot["pair"],
                status="finished",
                source={"times_ms": [0.0, 100.0], "sizes": [4, 4]},
                outputs=[
                    facts.output_fact(
                        at_ms=0.0,
                        ordinal=1,
                        kind="preview",
                        bytes_=4,
                        prefix_bytes=4,
                        correct=True,
                    ),
                    facts.output_fact(
                        at_ms=150.0,
                        ordinal=2,
                        kind="terminal",
                        bytes_=4,
                        prefix_bytes=8,
                        correct=True,
                    ),
                ],
                observation=facts.observation_block(
                    started_ns=1_000,
                    ended_ns=2_000_000_000,
                    cutoff_ns=1_000_000_000,
                    origin_ns=1_000,
                    origin=source.FIRST_EMISSION,
                ),
                reads=reads,
            )
        )
    raw = facts.document(
        run=run,
        scenario=scenario,
        environment=environment,
        schedule=schedule,
        attempts=attempts,
        trace=facts.trace_block(mode=observation),
        status=facts.status_block(state="complete"),
    )
    document = model.case_document(raw)
    document["execution"] = {
        "duration_ms": 1234.0,
        "errors": ["container cleanup warning"],
    }
    return document


def test_render_reports_deadline_progress_and_capture_quality():
    document = report()
    text = summary.render(document)
    assert "Benchmark: streaming-reply v8" in text
    assert "observation trace" in text
    assert "Measured attempts: 2" in text
    assert "Populations: planned/started/entered/finished" in text
    assert "Visible progress" in text
    assert "Deadline snapshots" in text
    assert "deadline_visible=True" in text
    assert "Attribution: trace available=" in text
    assert "Capture: mode=trace" in text
    assert "Status: complete" in text
    assert "Cleanup: verified" in text
    assert "Orchestration duration: 1.2s" in text
    assert "Execution: container cleanup warning" in text


def test_render_mixed_load_shows_exposure_and_pairs():
    document = report("mixed-load")
    text = summary.render(document)
    assert "Bulk exposure:" in text
    assert "Mixed pairs:" in text
    assert "bulk-minus-idle" in text
    assert "idle arm:" in text and "bulk arm:" in text


def test_render_off_mode_marks_attribution_unavailable():
    text = summary.render(report(observation="off"))
    assert "Attribution: off available=False" in text
    assert "Capture: mode=off" in text


def test_render_rejects_a_report_without_derived_rows():
    with pytest.raises(ValueError):
        summary.render({"derived": {}})


def test_comparison_and_method_render_declared_dimensions():
    result = {
        "pairs": 5,
        "required_pairs": 5,
        "vary": "observation",
        "verdict": "inconclusive",
        "declaration": {
            "versions": {"case": 2, "suite": 2, "workload": 8},
            "budget_revision": "budget-1",
            "conditions": {"mismatches": ["network.link"]},
            "observation_mode": {"baseline": ["off"], "candidate": ["trace"]},
        },
        "metrics": {
            "first_text_ms": {
                "estimate_ms": 0.5,
                "upper_bound_ms": 1.5,
                "tolerance_ms": 2.0,
                "verdict": "inconclusive",
                "reason": "straddled_boundary",
            }
        },
        "verdicts": {
            "business": {"state": "pass", "reasons": []},
            "mode_overhead": {"state": "inconclusive", "reasons": ["noise"]},
            "predecessor": {"state": "blocked", "reasons": ["unmatched"]},
        },
        "budget": {
            "revision": "budget-1",
            "state": "frozen",
            "metrics": {
                "first_text_ms": {
                    "upper_bound_observed": 1.2,
                    "upper_bound": 2.0,
                    "unit": "ms",
                    "verdict": "within",
                    "reason": None,
                }
            },
        },
        "reasons": ["metrics_disagree"],
    }
    text = summary.comparison(result)
    assert "vary=observation" in text
    assert "Budget revision: budget-1" in text
    assert "Condition mismatches: network.link" in text
    assert "Mode Overhead: inconclusive" in text
    assert "Reasons: metrics_disagree" in text
    protocol = summary.method()
    assert "observation: same build" in protocol
    assert "predecessor:" in protocol

def test_comparison_renders_a_predecessor_revision_string():
    result = {
        "pairs": 5,
        "required_pairs": 5,
        "vary": "predecessor",
        "verdict": "within_tolerance",
        "declaration": {
            "versions": {"case": 2, "suite": 2, "workload": 8},
            "budget_revision": "budget-1",
            "observation_mode": {"baseline": ["off"], "candidate": ["off"]},
        },
        "metrics": {},
        "verdicts": {"predecessor": {"state": "within", "reasons": []}},
        "budget": "budget-1",
    }
    text = summary.comparison(result)
    assert "Predecessor: within" in text
    assert "Budget: revision=budget-1" in text
