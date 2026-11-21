"""Compare saved benchmark runs under the predeclared protocol.

Each saved `benchmark.json` is one run. Runs are paired in the order given
(baseline run *i* with candidate run *i*), so alternating or randomized order
stays the caller's responsibility, as the protocol requires.
"""

from __future__ import annotations

import json
from pathlib import Path

from ..errors import RepoError, UsageError
from . import analysis, model


def load(directory):
    """Load and validate one saved run from a report directory or file."""
    path = Path(directory)
    if path.is_dir():
        path = path / "benchmark.json"
    if not path.exists():
        raise UsageError("no benchmark.json at " + str(path))
    document = model.read(path)
    measured = [item for item in document["attempts"] if item["phase"] == "measured"]
    return {
        "path": str(path),
        "run": document["run"]["id"],
        "attempts": measured,
        "outcomes": document["outcomes"],
        "latency": document["latency"],
        "streaming": document.get("streaming"),
        "artifacts": document["environment"]["artifacts"],
        "fixture": document["environment"]["fixture"],
        "workflow_results": document.get("workflow_results", {}),
        "conditions": {
            "scenario": document["scenario"],
            "surface": document["run"]["surface"],
            "configuration": document["run"]["configuration"],
            "topology": document["environment"]["topology"],
            "limits": document["environment"]["limits"],
            "runtime_versions": {
                key: value
                for key, value in document["environment"]["versions"].items()
                if key in ("platform", "python", "node")
            },
        },
        "status": document["status"],
    }


def _samples(run, metric):
    return analysis.samples(run["attempts"], metric)


def _comparable(run):
    return run["status"]["state"] == "complete"


def compare(baselines, candidates, *, vary="build"):
    if not baselines or not candidates:
        raise UsageError("compare requires at least one baseline and one candidate run")
    if len(baselines) != len(candidates):
        counts = f"{len(baselines)} baseline and {len(candidates)} candidate runs"
        raise UsageError("compare requires matched counts: " + counts)
    invalid = [run["path"] for run in (*baselines, *candidates) if not _comparable(run)]
    if invalid:
        raise RepoError("invalid benchmark runs cannot be compared: " + ", ".join(invalid))
    all_runs = (*baselines, *candidates)
    if all(run["conditions"]["scenario"]["version"] >= 5 for run in all_runs):
        return _compare_links(baselines, candidates, vary)
    if vary == "network":
        raise UsageError("network comparison requires link-performance benchmark reports")
    conditions = baselines[0]["conditions"]
    if any(run["conditions"] != conditions for run in all_runs):
        raise UsageError("comparison requires identical benchmark conditions")
    result = {
        "pairs": len(baselines),
        "required_pairs": analysis.DECLARATION["sample_floor"]["run_pairs"],
        "metrics": {},
        "verdict": "inconclusive",
        "reasons": [],
        "runs": {
            "baseline": [run["run"] for run in baselines],
            "candidate": [run["run"] for run in candidates],
        },
    }
    checks = [_declared(run) for run in (*baselines, *candidates)]
    failed = [
        {"run": check["run"], "name": item["name"]}
        for check in checks
        for item in check["checks"]
        if not item["ok"]
    ]
    result["checks"] = checks
    if len(baselines) < result["required_pairs"]:
        result["reasons"].append("insufficient_pairs")
    if failed:
        result["reasons"].append("declared_minimums_not_met")
    shared = analysis.compare(
        [run["attempts"] for run in baselines],
        [run["attempts"] for run in candidates],
    )
    for metric, entry in shared["metrics"].items():
        if len(baselines) < result["required_pairs"] or failed:
            entry = {**entry, "verdict": "inconclusive", "reason": "insufficient_pairs"}
        result["metrics"][metric] = {
            **entry,
            "samples": {
                "baseline": [len(_samples(run, metric)) for run in baselines],
                "candidate": [len(_samples(run, metric)) for run in candidates],
            },
        }
    verdicts = {entry["verdict"] for entry in result["metrics"].values()}
    if verdicts == {"within_tolerance"}:
        result["verdict"] = "within_tolerance"
    elif verdicts == {"improvement"}:
        result["verdict"] = "improvement"
    elif verdicts == {"regression"}:
        result["verdict"] = "regression"
    else:
        result["verdict"] = "inconclusive"
    if result["verdict"] == "inconclusive" and not result["reasons"]:
        result["reasons"].append("metrics_disagree_or_cross_tolerance")
    if baselines[0]["streaming"] is not None:
        result["streaming"] = _streaming(baselines, candidates)
    return result


def _link_conditions(run, vary):
    conditions = run["conditions"]
    if vary == "build":
        return conditions

    def without_profile(settings):
        return {
            **{key: value for key, value in settings.items() if key != "network"},
            "link": settings["network"]["link"],
        }

    return {
        **conditions,
        "scenario": without_profile(conditions["scenario"]),
        "configuration": without_profile(conditions["configuration"]),
    }


def _compare_links(baselines, candidates, vary):
    all_runs = (*baselines, *candidates)
    expected = _link_conditions(baselines[0], vary)
    if any(_link_conditions(run, vary) != expected for run in all_runs):
        raise UsageError(
            "link comparison requires identical conditions outside the declared variable"
        )
    harness = baselines[0]["artifacts"].get("harness")
    if not harness or any(
        run["artifacts"].get("harness") != harness or run["fixture"] != baselines[0]["fixture"]
        for run in all_runs
    ):
        raise UsageError("link comparison requires the same fixture and benchmark harness")
    for arm in (baselines, candidates):
        if any(run["artifacts"] != arm[0]["artifacts"] for run in arm):
            raise UsageError("each comparison arm must use one build")
        if vary == "network" and any(
            run["conditions"]["scenario"]["network"] != arm[0]["conditions"]["scenario"]["network"]
            for run in arm
        ):
            raise UsageError("each network comparison arm must use one network profile")
    if vary == "network" and baselines[0]["artifacts"] != candidates[0]["artifacts"]:
        raise UsageError("network comparison requires the same product build")

    version = baselines[0]["conditions"]["scenario"]["version"]
    metrics = (
        model.PRIMARY_STREAMING_METRICS
        if version >= 6
        else ("longest_output_gap_ms", "largest_burst_share")
    )
    streaming = _streaming(baselines, candidates, metrics=metrics)
    outcomes = {
        label: {
            "correct": sum(run["outcomes"]["correct_by_deadline"] for run in arm),
            "measured": sum(run["outcomes"]["measured"] for run in arm),
        }
        for label, arm in (("baseline", baselines), ("candidate", candidates))
    }
    reasons = sorted({entry["reason"] for entry in streaming.values() if entry["reason"]})
    return {
        "vary": vary,
        "pairs": len(baselines),
        "required_pairs": analysis.DECLARATION["sample_floor"]["run_pairs"],
        "verdict": "inconclusive" if reasons else "measured",
        "reasons": reasons,
        "metrics": {},
        "streaming": streaming,
        "outcomes": outcomes,
        "runs": {
            "baseline": [run["run"] for run in baselines],
            "candidate": [run["run"] for run in candidates],
        },
    }


def _streaming(baselines, candidates, metrics=model.STREAMING_METRICS):
    # Each run contributes one empirical p95, regardless of how many previews
    # it emitted. Keep this descriptive until an A/A pilot sets tolerances.
    def p95(run, metric):
        return run["streaming"]["samples"][metric]["quantiles"].get("p95")

    results = {}
    required = analysis.DECLARATION["sample_floor"]["run_pairs"]
    correct = all(_correct_streaming_run(run) for run in (*baselines, *candidates))
    for metric in metrics:
        before = [p95(run, metric) for run in baselines]
        after = [p95(run, metric) for run in candidates]
        differences = [
            b - a for a, b in zip(before, after, strict=True) if a is not None and b is not None
        ]
        reason = None
        if not correct:
            reason = "correctness_below_declared_floor"
        elif len(before) < required:
            reason = "insufficient_pairs"
        elif len(differences) != len(before):
            reason = "p95_unavailable"
        estimate, low, high, _ = analysis.paired(differences)
        results[metric] = {
            "baseline_p95": before,
            "candidate_p95": after,
            "mean_paired_p95_delta": estimate if reason is None else None,
            "interval_95": [low, high] if reason is None else None,
            "reason": reason,
        }
    return results


def _correct_streaming_run(run):
    threshold = analysis.DECLARATION["correctness_floor"]
    share = run["outcomes"]["success_share"]
    if share is None or share < threshold:
        return False
    workflow = run.get("workflow_results", {})
    if "history_measured" in workflow:
        count = workflow["history_measured"]
        return count > 0 and workflow["history_correct"] / count >= threshold
    return True


def _declared(run):
    checks = analysis.run_checks({"outcomes": run["outcomes"], "latency": run["latency"]})
    return {"run": run["run"], **checks}


def save(path, result):
    from ..reports.model import atomic_write

    atomic_write(
        Path(path),
        json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
    )
