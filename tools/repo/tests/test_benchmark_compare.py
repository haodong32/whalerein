"""Comparison of saved runs: pairing, declared minimums, verdicts and persistence."""

from __future__ import annotations

import json

import pytest
from test_benchmark_model import attempt, document

from repo.benchmark import compare, summary
from repo.errors import RepoError, UsageError


def write_run(directory, first=200.0, final=None, count=20, outcome="completed"):
    """Write a valid benchmark.json with `count` identical measured attempts."""
    final = first * 3 if final is None else final
    directory.mkdir(parents=True, exist_ok=True)
    attempts = [
        attempt(
            f"attempt-{index:04d}",
            outcome=outcome,
            first_text_ms=first,
            final_ms=final if outcome == "completed" else None,
        )
        for index in range(count)
    ]
    facts = document(attempts)
    (directory / "benchmark.json").write_text(json.dumps(facts))
    return directory


def pair(tmp_path, first, count=5, attempts=20):
    return [
        write_run(tmp_path / f"run-{index}", first=first, count=attempts) for index in range(count)
    ]


def test_load_accepts_a_directory_or_a_report_file(tmp_path):
    run = write_run(tmp_path / "run")
    from_directory = compare.load(run)
    from_file = compare.load(run / "benchmark.json")
    assert from_directory["run"] == from_file["run"] == "run-1"
    assert len(from_directory["attempts"]) == 20


def test_load_rejects_a_missing_run(tmp_path):
    with pytest.raises(UsageError, match="no benchmark.json"):
        compare.load(tmp_path / "absent")


def test_matched_pair_counts_are_required(tmp_path):
    baselines = [compare.load(write_run(tmp_path / "a"))]
    candidates = [
        compare.load(write_run(tmp_path / "b1")),
        compare.load(write_run(tmp_path / "b2")),
    ]
    with pytest.raises(UsageError, match="matched counts"):
        compare.compare(baselines, candidates)


def test_empty_selection_is_a_usage_error():
    with pytest.raises(UsageError, match="at least one baseline"):
        compare.compare([], [])


def test_invalid_runs_are_never_compared(tmp_path):
    broken = write_run(tmp_path / "broken")
    facts = json.loads((broken / "benchmark.json").read_text())
    facts["status"] = {
        "state": "invalid",
        "reasons": ["cleanup_unverified"],
        "cleanup": {"result": "unverified", "reason": "cleanup_unverified"},
    }
    (broken / "benchmark.json").write_text(json.dumps(facts))
    with pytest.raises(RepoError, match="invalid benchmark runs cannot be compared"):
        compare.compare([compare.load(broken)], [compare.load(write_run(tmp_path / "ok"))])


def test_insufficient_pairs_are_inconclusive(tmp_path):
    result = compare.compare(
        [compare.load(run) for run in pair(tmp_path / "base", 200.0, count=2)],
        [compare.load(run) for run in pair(tmp_path / "cand", 400.0, count=2)],
    )
    assert result["pairs"] == 2
    assert result["verdict"] == "inconclusive"
    assert "insufficient_pairs" in result["reasons"]
    assert all(entry["reason"] == "insufficient_pairs" for entry in result["metrics"].values())


def test_a_known_delay_is_detected_as_a_regression(tmp_path):
    result = compare.compare(
        [compare.load(run) for run in pair(tmp_path / "base", 200.0)],
        [compare.load(run) for run in pair(tmp_path / "cand", 400.0)],
    )
    assert result["pairs"] == 5
    # A 400 ms attempt against a 200 ms one: first text shifts by 200 ms and the
    # final reply, at three times first text, shifts by 600 ms.
    for metric, shift in (("first_text_ms", 200.0), ("final_ms", 600.0)):
        entry = result["metrics"][metric]
        assert entry["verdict"] == "regression"
        assert entry["estimate_ms"] == pytest.approx(shift)
        assert entry["interval_ms"][0] > entry["tolerance_ms"]
    assert result["verdict"] == "regression"


def test_an_identical_pair_is_within_tolerance(tmp_path):
    result = compare.compare(
        [compare.load(run) for run in pair(tmp_path / "base", 200.0)],
        [compare.load(run) for run in pair(tmp_path / "cand", 200.0)],
    )
    assert result["verdict"] == "within_tolerance"
    for entry in result["metrics"].values():
        assert entry["interval_ms"] == [0.0, 0.0]


def test_runs_below_the_declared_attempt_floor_are_inconclusive(tmp_path):
    result = compare.compare(
        [compare.load(run) for run in pair(tmp_path / "base", 200.0, attempts=3)],
        [compare.load(run) for run in pair(tmp_path / "cand", 400.0, attempts=3)],
    )
    assert "declared_minimums_not_met" in result["reasons"]
    assert all(entry["verdict"] == "inconclusive" for entry in result["metrics"].values())
    failed = [item for check in result["checks"] for item in check["checks"] if not item["ok"]]
    assert any(item["name"] == "attempts_per_run" for item in failed)


def test_a_noise_level_difference_is_inconclusive_not_a_verdict(tmp_path):
    """Paired runs whose intervals span the tolerance must not claim a change."""
    baselines, candidates = [], []
    for index in range(5):
        low = write_run(tmp_path / f"b{index}", first=200.0 + index)
        high = write_run(tmp_path / f"c{index}", first=200.0 - index)
        baselines.append(compare.load(low))
        candidates.append(compare.load(high))
    result = compare.compare(baselines, candidates)
    assert result["verdict"] in ("inconclusive", "within_tolerance")
    for entry in result["metrics"].values():
        assert entry["verdict"] != "regression"


def test_comparison_is_rendered_and_saved(tmp_path):
    baselines = [compare.load(run) for run in pair(tmp_path / "base", 200.0)]
    candidates = [compare.load(run) for run in pair(tmp_path / "cand", 400.0)]
    result = compare.compare(baselines, candidates)
    text = summary.comparison(result)
    assert "5 matched pair(s)" in text
    assert "regression" in text
    destination = tmp_path / "comparison.json"
    compare.save(destination, result)
    saved = json.loads(destination.read_text())
    assert saved["metrics"]["first_text_ms"]["verdict"] == "regression"
    assert saved["runs"]["baseline"] == ["run-1"] * 5


def test_method_text_states_the_declaration():
    text = summary.method()
    assert "first_text_ms" in text
    assert "tolerance" in text or "tolerances" in text
    assert "paired" in text.lower()
