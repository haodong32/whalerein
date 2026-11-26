"""Schema-2 comparisons: one-way dimensions, paired statistics and verdicts."""

from __future__ import annotations

import json

import pytest
from test_benchmark_runner import facts_document, request

from repo.benchmark import analysis, compare, model, summary
from repo.errors import RepoError, UsageError

BASE = request(attempts=20, warmup=0)


def write_run(directory, *, shift=0.0, build=("rev-a", "build-a"), mode="off", network="normal"):
    directory.mkdir(parents=True, exist_ok=True)
    document = facts_document(
        request(
            attempts=20,
            warmup=0,
            observation=mode,
            network_profile=network,
            link="host",
        ),
        build=build,
        text_shift=shift,
    )
    report = model.case_document(document)
    (directory / "benchmark.json").write_text(json.dumps(report))
    return directory


def pair(base, prefix="run", *, shift=0.0, build=("rev-a", "build-a"), mode="off"):
    return [
        compare.load(write_run(base / f"{prefix}-{index}", shift=shift, build=build, mode=mode))
        for index in range(5)
    ]


def budget(**metrics):
    return analysis.budget_declaration(
        metrics,
        revision="budget-2026-11",
        sample_floor=20,
        run_pairs=5,
    )


def test_load_accepts_a_directory_or_report_file_and_rejects_a_suite(tmp_path):
    run = write_run(tmp_path / "run")
    from_directory = compare.load(run)
    from_file = compare.load(run / "benchmark.json")
    assert from_directory["run"] == from_file["run"] == "run-1"
    assert len(from_directory["attempts"]) == 20
    suite = model.suite_document(
        [], status={"state": "incomplete", "reasons": ["x"], "pending": []}
    )
    (run / "suite.json").write_text(json.dumps(suite))
    with pytest.raises(UsageError, match="not a suite index"):
        compare.load(run / "suite.json")


def test_load_rejects_a_missing_run(tmp_path):
    with pytest.raises(UsageError, match="no benchmark.json"):
        compare.load(tmp_path / "absent")


@pytest.mark.parametrize("version", [1, 3])
# latency/AC-08.old latency/AC-08.future
def test_retired_and_future_comparison_inputs_are_rejected(tmp_path, version):
    path = tmp_path / "old.json"
    document = model.case_document(facts_document(BASE))
    path.write_text(json.dumps({**document, "schema_version": version}))
    with pytest.raises(RepoError, match=f"schema version {version}"):
        compare.load(path)


# latency/AC-08.mixed
def test_mixed_document_kind_is_rejected(tmp_path):
    path = tmp_path / "mixed.json"
    document = model.case_document(facts_document(BASE))
    path.write_text(json.dumps({**document, "kind": "benchmark-suite"}))
    with pytest.raises(RepoError, match="suite"):
        compare.load(path)


def test_matched_counts_and_empty_selection_are_usage_errors(tmp_path):
    baseline = compare.load(write_run(tmp_path / "a"))
    candidate = compare.load(write_run(tmp_path / "b", shift=10, build=("rev-b", "build-b")))
    with pytest.raises(UsageError, match="matched counts"):
        compare.compare([baseline], [candidate, candidate])
    with pytest.raises(UsageError, match="at least one baseline"):
        compare.compare([], [])


# latency/AC-13.aa
def test_same_build_aa_is_calibrated(tmp_path):
    result = compare.compare(
        pair(tmp_path / "base", shift=0, build=("rev-a", "build-a")),
        pair(tmp_path / "cand", shift=0, build=("rev-a", "build-a")),
        vary="aa",
    )
    assert result["schema_version"] == 2
    assert result["kind"] == "benchmark-comparison"
    assert result["vary"] == "aa"
    assert result["verdict"] == "calibrated"
    assert result["aa"]["pairs"] == 5
    assert result["declaration"]["versions"] == {
        "comparison": 2,
        "case": 2,
        "suite": 2,
        "workload": 8,
    }
    assert result["declaration"]["topology"]["kind"] == "single-machine"
    assert result["declaration"]["clock_policy"]["source"] == "CLOCK_MONOTONIC"


def test_aa_rejects_a_build_difference(tmp_path):
    with pytest.raises(UsageError, match="A/A requires the same product build"):
        compare.compare(
            pair(tmp_path / "base", build=("rev-a", "build-a")),
            pair(tmp_path / "cand", build=("rev-b", "build-b")),
            vary="aa",
        )


def test_a_known_build_delay_is_a_regression(tmp_path):
    result = compare.compare(
        pair(tmp_path / "base", shift=0),
        pair(tmp_path / "cand", shift=200, build=("rev-b", "build-b")),
    )
    assert result["pairs"] == 5
    assert result["metrics"]["first_text_ms"]["verdict"] == "regression"
    assert result["metrics"]["first_text_ms"]["estimate_ms"] == pytest.approx(200)
    assert result["metrics"]["first_text_ms"]["upper_bound_ms"] > 25
    assert result["verdict"] == "regression"
    assert result["verdicts"]["business"]["state"] == "regression"


def test_identical_differences_stay_within_tolerance(tmp_path):
    result = compare.compare(
        pair(tmp_path / "base"),
        pair(tmp_path / "cand", build=("rev-b", "build-b")),
    )
    assert result["verdict"] == "within_tolerance"
    for entry in result["metrics"].values():
        assert entry["upper_bound_ms"] == pytest.approx(0.0)


def test_build_comparison_rejects_network_or_mode_differences(tmp_path):
    with pytest.raises(UsageError, match="same network setting"):
        compare.compare(
            pair(tmp_path / "base"),
            [
                compare.load(
                    write_run(
                        tmp_path / f"cand-{i}",
                        network="high-rtt",
                        build=("rev-b", "build-b"),
                    )
                )
                for i in range(5)
            ],
        )
    with pytest.raises(UsageError, match="same observation mode"):
        compare.compare(
            pair(tmp_path / "base"),
            [
                compare.load(
                    write_run(
                        tmp_path / f"cand-{i}",
                        mode="aggregate",
                        build=("rev-b", "build-b"),
                    )
                )
                for i in range(5)
            ],
        )
    with pytest.raises(UsageError, match="requires a build difference"):
        compare.compare(pair(tmp_path / "base"), pair(tmp_path / "cand"))


# latency/AC-13.paired
def test_network_comparison_varies_only_the_declared_network(tmp_path):
    baseline = pair(tmp_path / "base", build=("rev-a", "build-a"))
    candidate = [
        compare.load(
            write_run(tmp_path / f"cand-{i}", network="high-rtt", build=("rev-a", "build-a"))
        )
        for i in range(5)
    ]
    result = compare.compare(baseline, candidate, vary="network")
    assert result["vary"] == "network"
    assert result["declaration"]["artifact_provenance"]["baseline"][0]["revision"] == "rev-a"
    assert result["verdicts"]["business"]["state"] in ("complete", "inconclusive")
    with pytest.raises(UsageError, match="same product build"):
        compare.compare(
            baseline,
            [
                compare.load(
                    write_run(
                        tmp_path / f"other-{i}",
                        network="high-rtt",
                        build=("rev-b", "build-b"),
                    )
                )
                for i in range(5)
            ],
            vary="network",
        )
    with pytest.raises(UsageError, match="requires a network difference"):
        compare.compare(baseline, baseline, vary="network")


# latency/AC-13.mode
def test_observation_comparison_requires_the_same_build_and_a_budget(tmp_path):
    baseline = pair(tmp_path / "base", build=("rev-a", "build-a"), mode="off")
    candidate = pair(tmp_path / "cand", build=("rev-a", "build-a"), mode="aggregate")
    with_budget = compare.compare(
        baseline,
        candidate,
        vary="observation",
        budget=budget(first_text_ms=1.0),
    )
    assert with_budget["vary"] == "observation"
    assert with_budget["verdicts"]["mode_overhead"]["state"] == "within"
    assert with_budget["declaration"]["budget_revision"] == "budget-2026-11"
    assert with_budget["declaration"]["observation_mode"] == {
        "baseline": ["off"],
        "candidate": ["aggregate"],
    }
    without_budget = compare.compare(baseline, candidate, vary="observation")
    assert without_budget["verdict"] == "inconclusive"
    assert "budget_not_declared" in without_budget["verdicts"]["mode_overhead"]["reasons"]
    with pytest.raises(UsageError, match="same product build"):
        compare.compare(
            baseline,
            pair(tmp_path / "other", build=("rev-b", "build-b"), mode="aggregate"),
            vary="observation",
        )
    with pytest.raises(UsageError, match="requires an observation-mode difference"):
        compare.compare(baseline, baseline, vary="observation")


# latency/AC-13.breach latency/AC-13.stale
def test_observation_budget_breach_and_stale_budget(tmp_path):
    baseline = pair(tmp_path / "base", mode="off")
    candidate = pair(tmp_path / "cand", mode="aggregate")
    breached = compare.compare(
        baseline,
        candidate,
        vary="observation",
        budget=budget(first_text_ms=-1.0),
    )
    assert breached["verdict"] == "breach"
    expired = {
        **budget(first_text_ms=1.0),
        "expires_at": "2026-01-01T00:00:00Z",
    }
    stale = compare.compare(
        baseline,
        candidate,
        vary="observation",
        budget=expired,
        now="2026-02-01T00:00:00Z",
    )
    assert stale["verdict"] == "inconclusive"
    assert stale["verdicts"]["mode_overhead"]["reasons"][0] == "budget_expired"


# latency/AC-13.disabled-path-regression
def test_predecessor_comparison_records_both_identities(tmp_path):
    baseline = [
        compare.load(write_run(tmp_path / f"old-{i}", build=("rev-old", "build-old")))
        for i in range(5)
    ]
    candidate = [
        compare.load(write_run(tmp_path / f"new-{i}", build=("rev-new", "build-new")))
        for i in range(5)
    ]
    result = compare.compare(
        baseline,
        candidate,
        vary="predecessor",
        budget=budget(first_text_ms=1.0),
    )
    assert result["vary"] == "predecessor"
    assert result["verdicts"]["predecessor"]["baseline"]["revision"] == "rev-old"
    assert result["verdicts"]["predecessor"]["candidate"]["revision"] == "rev-new"
    assert result["verdict"] in ("within", "breach", "inconclusive")


def test_unmatched_predecessor_is_inconclusive(tmp_path):
    runs = pair(tmp_path / "old", build=("rev-same", "build-same"))
    unmatched = compare.compare(runs, runs, vary="predecessor")
    assert unmatched["verdict"] == "inconclusive"
    assert unmatched["verdicts"]["predecessor"]["reasons"] == ["unmatched_predecessor"]
    assert unmatched["verdicts"]["predecessor"]["baseline"]["revision"] == "rev-same"
    incompatible = compare.compare(
        pair(tmp_path / "a", build=("rev-old", "build-old"), mode="aggregate"),
        pair(tmp_path / "b", build=("rev-new", "build-new"), mode="aggregate"),
        vary="predecessor",
    )
    assert incompatible["verdict"] == "inconclusive"
    assert incompatible["verdicts"]["predecessor"]["reasons"] == ["incompatible_baseline"]


def test_predecessor_without_a_budget_is_inconclusive(tmp_path):
    result = compare.compare(
        pair(tmp_path / "old", build=("rev-old", "build-old")),
        pair(tmp_path / "new", build=("rev-new", "build-new")),
        vary="predecessor",
    )
    assert result["verdict"] == "inconclusive"
    assert "budget_not_declared" in result["verdicts"]["predecessor"]["reasons"]


# latency/AC-12.small-sample
def test_insufficient_pairs_are_inconclusive(tmp_path):
    result = compare.compare(
        pair(tmp_path / "base")[:2],
        pair(tmp_path / "cand", shift=200, build=("rev-b", "build-b"))[:2],
    )
    assert result["pairs"] == 2
    assert result["verdict"] == "inconclusive"
    assert "insufficient_pairs" in result["reasons"]
    assert all(entry["reason"] == "insufficient_pairs" for entry in result["metrics"].values())


def test_insufficient_usable_samples_are_inconclusive(tmp_path):
    baseline = []
    candidate = []
    for index in range(5):
        directory = tmp_path / f"base-{index}"
        directory.mkdir()
        document = facts_document(BASE, completed=19)
        (directory / "benchmark.json").write_text(json.dumps(model.case_document(document)))
        baseline.append(compare.load(directory))
        candidate.append(
            compare.load(
                write_run(tmp_path / f"cand-{index}", shift=200, build=("rev-b", "build-b"))
            )
        )
    result = compare.compare(baseline, candidate)
    assert result["verdict"] == "inconclusive"
    assert all(entry["reason"] == "insufficient_samples" for entry in result["metrics"].values())


# latency/AC-12.straddled
def test_a_straddled_bound_is_inconclusive_not_a_verdict(tmp_path):
    shifts = [0, 0, 0, 0, 50]
    baseline = pair(tmp_path / "base")
    candidate = [
        compare.load(write_run(tmp_path / f"cand-{index}", shift=shift, build=("rev-b", "build-b")))
        for index, shift in enumerate(shifts)
    ]
    result = compare.compare(baseline, candidate)
    entry = result["metrics"]["first_text_ms"]
    assert entry["estimate_ms"] == pytest.approx(10)
    assert entry["upper_bound_ms"] > entry["tolerance_ms"]
    assert entry["verdict"] == "inconclusive"
    assert entry["reason"] == "interval_crosses_tolerance"
    assert result["verdict"] == "inconclusive"


def test_comparison_is_rendered_and_saved(tmp_path):
    result = compare.compare(
        pair(tmp_path / "base"),
        pair(tmp_path / "cand", shift=200, build=("rev-b", "build-b")),
    )
    text = summary.comparison(result)
    assert "5 matched pair(s)" in text
    assert "regression" in text
    assert "budget revision: none" in text.lower()
    destination = tmp_path / "comparison.json"
    compare.save(destination, result)
    saved = json.loads(destination.read_text())
    assert saved["schema_version"] == 2
    assert saved["metrics"]["first_text_ms"]["verdict"] == "regression"
    assert saved["runs"]["baseline"] == ["run-1"] * 5


def test_method_text_states_the_declaration():
    text = summary.method()
    assert "first_text_ms" in text
    assert "tolerance" in text or "tolerances" in text
    assert "paired" in text.lower()
    assert "predecessor" in text
