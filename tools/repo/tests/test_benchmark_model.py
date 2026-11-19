"""Benchmark facts: strict validation, quantiles, and the comparison protocol."""

from __future__ import annotations

import json

import pytest

from repo.benchmark import analysis, model, summary
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
            "version": model.SCENARIO_VERSION,
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
        cli.main(["bench", "--attempts", "1", "--warmup", "0", "--output", str(baseline), "--json"])
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
