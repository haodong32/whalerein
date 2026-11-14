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
        "status": document["status"],
    }


def _samples(run, metric):
    return analysis.samples(run["attempts"], metric)


def _comparable(run):
    return run["status"]["state"] == "complete"


def compare(baselines, candidates):
    if not baselines or not candidates:
        raise UsageError("compare requires at least one baseline and one candidate run")
    if len(baselines) != len(candidates):
        counts = f"{len(baselines)} baseline and {len(candidates)} candidate runs"
        raise UsageError("compare requires matched counts: " + counts)
    invalid = [run["path"] for run in (*baselines, *candidates) if not _comparable(run)]
    if invalid:
        raise RepoError("invalid benchmark runs cannot be compared: " + ", ".join(invalid))
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
    return result


def _declared(run):
    checks = analysis.run_checks({"outcomes": run["outcomes"], "latency": run["latency"]})
    return {"run": run["run"], **checks}


def save(path, result):
    from ..reports.model import atomic_write

    atomic_write(
        Path(path),
        json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
    )
