"""Compare saved current-version benchmark cases under one declared protocol.

One comparison is schema 2. It declares the versions, configuration
fingerprint, single-machine topology, shared-clock policy, artifact
provenance, observation mode and budget revision it was computed under, then
applies the shared run-level paired statistics from `repo.benchmark.analysis`.

The comparison dimension is one-way:

* ``build``: the build may vary; everything else must match.
* ``network``: only the declared network setting may vary on the same build.
* ``observation``: only the observation mode may vary on the same build.
* ``aa``: neither the build nor any condition may vary.
* ``predecessor``: a frozen pre-instrumentation off build against the
  candidate off build under the same external harness.

Retired readers and fallback branches do not exist.
"""

from __future__ import annotations

import json
from pathlib import Path

from ..errors import RepoError, UsageError
from . import analysis, model

COMPARISON_SCHEMA = 2
COMPARISON_METRICS = ("first_text_ms", "final_ms")
VARY_CHOICES = ("build", "network", "observation", "aa", "predecessor")


def load(directory):
    """Load and validate one current saved case from a directory or file."""
    path = Path(directory)
    if path.is_dir():
        path = path / "benchmark.json"
    if not path.exists():
        raise UsageError("no benchmark.json at " + str(path))
    document = model.read(path)
    if document.get("kind") == "benchmark-suite":
        raise UsageError("compare requires case reports, not a suite index")
    derived = document["derived"]
    return {
        "path": str(path),
        "run": document["run"]["id"],
        "document": document,
        "identity": analysis.identity(document),
        "conditions": analysis.conditions(document),
        "network": document["run"]["configuration"]["network"],
        "observation_mode": document["run"]["observation_mode"],
        "artifacts": document["environment"]["artifacts"],
        "fixture": document["environment"]["fixture"],
        "attempts": derived["attempts"],
        "outcomes": derived["outcomes"],
        "latency": derived["latency"],
        "workflow_results": derived.get("workflow_results") or {},
        "coverage": derived.get("coverage") or {},
        "status": document["status"],
        "endpoint": document["run"]["endpoint"],
    }


def compare(baselines, candidates, *, vary="build", budget=None, now=None):
    if vary not in VARY_CHOICES:
        raise UsageError("--vary must be build, network, observation, aa, or predecessor")
    if not baselines or not candidates:
        raise UsageError("compare requires at least one baseline and one candidate run")
    if len(baselines) != len(candidates):
        counts = f"{len(baselines)} baseline and {len(candidates)} candidate runs"
        raise UsageError("compare requires matched counts: " + counts)
    invalid = [
        run["path"]
        for run in (*baselines, *candidates)
        if run["status"]["state"] == "invalid"
    ]
    if invalid:
        raise RepoError("invalid benchmark runs cannot be compared: " + ", ".join(invalid))
    all_runs = (*baselines, *candidates)
    try:
        _check_dimension(vary, baselines, candidates, all_runs)
    except UsageError as exc:
        if vary != "predecessor":
            raise
        return _inconclusive_predecessor(exc, baselines, candidates, budget)
    if budget is not None:
        validity = analysis.validate_budget(budget)
        if validity["state"] != "current":
            raise UsageError("benchmark budget declaration is stale or invalid")
    declaration = _declaration(vary, baselines, candidates, budget)
    required_pairs = analysis.DECLARATION["sample_floor"]["run_pairs"]
    result = {
        "schema_version": COMPARISON_SCHEMA,
        "kind": "benchmark-comparison",
        "vary": vary,
        "pairs": len(baselines),
        "required_pairs": required_pairs,
        "declaration": declaration,
        "conditions": declaration["conditions"],
        "provenance": declaration["artifact_provenance"],
        "runs": {
            "baseline": [run["run"] for run in baselines],
            "candidate": [run["run"] for run in candidates],
        },
        "metrics": {},
        "verdicts": {},
        "verdict": "inconclusive",
        "reasons": [],
        "method": analysis.DECLARATION["statistic"],
    }
    if len(baselines) < required_pairs:
        result["reasons"].append("insufficient_pairs")
    correctness = _correctness(baselines, candidates)
    result["verdicts"]["business"] = correctness
    if correctness["state"] != "complete":
        result["reasons"].extend(correctness["reasons"])
    result["metrics"] = _metrics(baselines, candidates, result["reasons"])
    latency_state = _latency_state(result["metrics"])
    result["verdicts"]["business"] = {
        **correctness,
        "latency": latency_state,
        "state": (
            "complete"
            if correctness["state"] == "complete" and latency_state == "within_tolerance"
            else "regression"
            if latency_state == "regression"
            else "inconclusive"
        ),
    }
    if vary == "aa":
        result["aa"] = _aa(baselines, candidates, required_pairs)
        result["verdict"] = result["aa"]["state"]
    elif vary == "observation":
        overhead = _budget_section("mode_overhead", baselines, candidates, budget, now)
        result["verdicts"]["mode_overhead"] = overhead
        result["budget"] = overhead.get("budget")
        result["verdict"] = overhead["state"]
    elif vary == "predecessor":
        release = _predecessor_section(baselines, candidates, budget, now)
        result["verdicts"]["predecessor"] = release
        result["budget"] = release.get("budget")
        result["verdict"] = release["state"]
    else:
        result["verdict"] = _overall(result["metrics"])
    if result["verdict"] == "inconclusive" and not result["reasons"]:
        result["reasons"].append("inconclusive_evidence")
    _apply_bulk_gate(result, all_runs)
    return result


def _inconclusive_predecessor(exc, baselines, candidates, budget):
    declaration = _declaration("predecessor", baselines, candidates, budget)
    reason = (
        "unmatched_predecessor"
        if "unmatched predecessor" in str(exc)
        else "incompatible_baseline"
    )
    result = {
        "schema_version": COMPARISON_SCHEMA,
        "kind": "benchmark-comparison",
        "vary": "predecessor",
        "pairs": len(baselines),
        "required_pairs": analysis.DECLARATION["sample_floor"]["run_pairs"],
        "declaration": declaration,
        "conditions": declaration["conditions"],
        "provenance": declaration["artifact_provenance"],
        "runs": {
            "baseline": [run["run"] for run in baselines],
            "candidate": [run["run"] for run in candidates],
        },
        "metrics": {},
        "verdicts": {
            "predecessor": {
                "state": "inconclusive",
                "reasons": [reason],
                "detail": str(exc),
                "baseline": baselines[0]["identity"] if baselines else None,
                "candidate": candidates[0]["identity"] if candidates else None,
                "budget": budget.get("revision") if isinstance(budget, dict) else None,
            }
        },
        "verdict": "inconclusive",
        "reasons": [reason],
        "method": analysis.DECLARATION["statistic"],
    }
    return result


def _declaration(vary, baselines, candidates, budget):
    all_runs = (*baselines, *candidates)
    first = all_runs[0]["document"]
    conditions = analysis.compare_conditions(first, all_runs[-1]["document"])
    return {
        "schema_version": COMPARISON_SCHEMA,
        "vary": vary,
        "versions": {
            "comparison": COMPARISON_SCHEMA,
            "case": model.SCHEMA_VERSION,
            "suite": model.SUITE_SCHEMA_VERSION,
            "workload": model.SCENARIO_VERSION,
        },
        "configuration_fingerprint": {
            arm: [run["document"]["run"].get("fingerprint") for run in runs]
            for arm, runs in (("baseline", baselines), ("candidate", candidates))
        },
        "topology": {
            "kind": (first["environment"].get("topology") or {}).get("kind"),
            "clock": (first["environment"].get("topology") or {}).get("clock"),
        },
        "clock_policy": analysis.conditions(first)["clock"],
        "observation_mode": {
            "baseline": sorted({run["observation_mode"] for run in baselines}),
            "candidate": sorted({run["observation_mode"] for run in candidates}),
        },
        "artifact_provenance": {
            arm: [
                {
                    "run": run["run"],
                    "revision": run["identity"].get("revision"),
                    "fingerprint": run["identity"].get("fingerprint"),
                    "fixture": run["identity"].get("fixture"),
                    "artifacts": run["artifacts"],
                }
                for run in runs
            ]
            for arm, runs in (("baseline", baselines), ("candidate", candidates))
        },
        "budget_revision": budget.get("revision") if isinstance(budget, dict) else None,
        "conditions": conditions,
        "endpoint": sorted({run["endpoint"] for run in all_runs}),
        "sample_floor": analysis.DECLARATION["sample_floor"],
    }


def _check_dimension(vary, baselines, candidates, all_runs):
    builds = {
        (run["identity"].get("revision"), run["identity"].get("fingerprint"))
        for run in all_runs
    }
    modes = {run["observation_mode"] for run in all_runs}
    networks = {_canonical(run["network"]) for run in all_runs}
    fixtures = {
        (_canonical(run["fixture"]), _canonical(run["conditions"].get("fingerprint")))
        for run in all_runs
    }
    non_network = {_without_network(run["conditions"]) for run in all_runs}
    if vary == "build":
        _require(len(modes) == 1, "build comparison requires the same observation mode")
        _require(len(networks) == 1, "build comparison requires the same network setting")
        _require(len(fixtures) == 1, "build comparison requires the same fixture and platform")
        _require(
            len({_without_build(run["conditions"]) for run in all_runs}) == 1,
            "build comparison may vary only the product build",
        )
        _require(
            len({_canonical(run["identity"].get("revision")) for run in baselines}) == 1
            and len({_canonical(run["identity"].get("revision")) for run in candidates}) == 1,
            "each build comparison arm must use one build",
        )
        _require(
            {
                (
                    _canonical(run["identity"].get("revision")),
                    _canonical(run["identity"].get("fingerprint")),
                )
                for run in baselines
            }
            != {
                (
                    _canonical(run["identity"].get("revision")),
                    _canonical(run["identity"].get("fingerprint")),
                )
                for run in candidates
            },
            "vary build requires a build difference; use --vary aa for the same build",
        )
    elif vary == "network":
        _require(len(builds) == 1, "network comparison requires the same product build")
        _require(len(modes) == 1, "network comparison requires the same observation mode")
        _require(
            len(non_network) == 1,
            "network comparison may vary only the declared network setting",
        )
        _require(
            len({_canonical(run["network"]) for run in baselines}) == 1
            and len({_canonical(run["network"]) for run in candidates}) == 1,
            "each network comparison arm must use one network setting",
        )
        _require(
            {_canonical(run["network"]) for run in baselines}
            != {_canonical(run["network"]) for run in candidates},
            "vary network requires a network difference",
        )
    elif vary == "observation":
        _require(len(builds) == 1, "observation comparison requires the same product build")
        _require(len(networks) == 1, "observation comparison requires the same network setting")
        _require(
            len(_without_mode(all_runs)) == 1,
            "observation comparison may vary only the observation mode",
        )
        _require(
            len({run["observation_mode"] for run in baselines}) == 1
            and len({run["observation_mode"] for run in candidates}) == 1,
            "each observation comparison arm must use one mode",
        )
        _require(
            {run["observation_mode"] for run in baselines}
            != {run["observation_mode"] for run in candidates},
            "vary observation requires an observation-mode difference",
        )
    elif vary == "aa":
        _require(len(builds) == 1, "A/A requires the same product build")
        _require(len(modes) == 1, "A/A requires the same observation mode")
        _require(len(networks) == 1, "A/A requires the same network setting")
        _require(len(fixtures) == 1, "A/A requires the same fixture and platform")
        _require(
            len({_canonical(run["conditions"]) for run in all_runs}) == 1,
            "A/A requires identical benchmark conditions",
        )
    elif vary == "predecessor":
        _require(modes == {"off"}, "the predecessor comparison requires off-mode runs")
        _require(len(networks) == 1, "the predecessor comparison requires the same network setting")
        _require(
            len({_without_build(run["conditions"]) for run in all_runs}) == 1,
            "the predecessor comparison may vary only the product build",
        )
        _require(
            len({_canonical(run["identity"].get("revision")) for run in baselines}) == 1
            and len({_canonical(run["identity"].get("revision")) for run in candidates}) == 1,
            "each predecessor arm must use one build",
        )
        baseline_revisions = {run["identity"].get("revision") for run in baselines}
        candidate_revisions = {run["identity"].get("revision") for run in candidates}
        _require(
            None not in baseline_revisions and None not in candidate_revisions,
            "unmatched predecessor: both arms need a recorded revision",
        )
        _require(
            baseline_revisions != candidate_revisions,
            "unmatched predecessor: baseline and candidate revisions are identical",
        )
        _require(
            all(run["artifacts"] for run in all_runs),
            "predecessor artifacts must be recorded",
        )


def _require(condition, message):
    if not condition:
        raise UsageError(message)


def _canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), default=str)


def _without_network(conditions):
    value = json.loads(json.dumps(conditions, default=str))
    configuration = value.get("configuration") or {}
    configuration.pop("network", None)
    return _canonical(value)


def _without_build(conditions):
    value = json.loads(json.dumps(conditions, default=str))
    (value.get("fingerprint") or {}).pop("build", None)
    return _canonical(value)


def _without_mode(runs):
    values = set()
    for run in runs:
        conditions = json.loads(json.dumps(run["conditions"], default=str))
        configuration = conditions.get("configuration") or {}
        configuration.pop("observation_mode", None)
        values.add(_canonical(conditions))
    return values


def _correctness(baselines, candidates):
    threshold = analysis.DECLARATION["correctness_floor"]
    arms = {}
    reasons = []
    for label, runs in (("baseline", baselines), ("candidate", candidates)):
        correct = sum(run["outcomes"]["correct_by_deadline"] for run in runs)
        measured = sum(run["outcomes"]["measured"] for run in runs)
        share = correct / measured if measured else None
        arms[label] = {"correct": correct, "measured": measured, "share": share}
        if share is None or share < threshold:
            reasons.append(label + ":correctness_below_declared_floor")
    return {
        "state": "complete" if not reasons else "inconclusive",
        "threshold": threshold,
        "arms": arms,
        "reasons": reasons,
    }


def _metric_samples(run, metric):
    return analysis.samples(run["attempts"], metric)


def _metrics(baselines, candidates, reasons):
    floor = analysis.DECLARATION["sample_floor"]
    required_pairs = floor["run_pairs"]
    required_samples = floor["attempts_per_run"]
    metrics = {}
    for metric in COMPARISON_METRICS:
        pairs = []
        sample_counts = {"baseline": [], "candidate": []}
        short = False
        for before, after in zip(baselines, candidates, strict=True):
            before_samples = _metric_samples(before, metric)
            after_samples = _metric_samples(after, metric)
            sample_counts["baseline"].append(len(before_samples))
            sample_counts["candidate"].append(len(after_samples))
            if len(before_samples) < required_samples or len(after_samples) < required_samples:
                short = True
                continue
            pairs.append(
                {
                    "baseline": analysis.mean(before_samples),
                    "candidate": analysis.mean(after_samples),
                }
            )
        differences = [pair["candidate"] - pair["baseline"] for pair in pairs]
        estimate, upper, error = analysis.paired_upper_bound(differences)
        tolerance = analysis.tolerance(metric)
        entry = {
            "estimate_ms": estimate,
            "upper_bound_ms": upper,
            "standard_error_ms": error,
            "tolerance_ms": tolerance,
            "pairs": len(pairs),
            "required_pairs": required_pairs,
            "required_samples": required_samples,
            "samples": sample_counts,
            "verdict": "inconclusive",
            "reason": None,
        }
        if len(baselines) < required_pairs:
            entry["reason"] = "insufficient_pairs"
        elif short:
            entry["reason"] = "insufficient_samples"
        elif len(pairs) < required_pairs or upper is None:
            entry["reason"] = "insufficient_pairs"
        elif estimate > tolerance:
            entry["verdict"] = "regression"
            entry["reason"] = "estimate_beyond_upper_tolerance"
        elif upper > tolerance:
            entry["verdict"] = "inconclusive"
            entry["reason"] = "interval_crosses_tolerance"
        elif upper <= 0 and estimate < 0:
            entry["verdict"] = "improvement"
            entry["reason"] = "upper_bound_below_zero"
        else:
            entry["verdict"] = "within_tolerance"
            entry["reason"] = "upper_bound_within_tolerance"
        metrics[metric] = entry
        if entry["verdict"] == "inconclusive" and entry["reason"] not in reasons:
            reasons.append(entry["reason"])
    return metrics


def _latency_state(metrics):
    verdicts = {entry["verdict"] for entry in metrics.values()}
    if "regression" in verdicts:
        return "regression"
    if verdicts == {"within_tolerance"} or verdicts == {"improvement"}:
        return "within_tolerance"
    return "inconclusive"


def _overall(metrics):
    verdicts = {entry["verdict"] for entry in metrics.values()}
    if verdicts == {"regression"}:
        return "regression"
    if verdicts == {"improvement"}:
        return "improvement"
    if verdicts == {"within_tolerance"}:
        return "within_tolerance"
    return "inconclusive"


def _metric_evidence(baselines, candidates):
    evidence = {}
    required_samples = analysis.DECLARATION["sample_floor"]["attempts_per_run"]
    for metric in COMPARISON_METRICS:
        pairs = []
        baseline_successes = []
        candidate_successes = []
        for before, after in zip(baselines, candidates, strict=True):
            before_samples = _metric_samples(before, metric)
            after_samples = _metric_samples(after, metric)
            baseline_successes.append(len(before_samples))
            candidate_successes.append(len(after_samples))
            if before_samples and after_samples:
                pairs.append(
                    {
                        "baseline": analysis.mean(before_samples),
                        "candidate": analysis.mean(after_samples),
                    }
                )
        evidence[metric] = {
            "pairs": pairs,
            "baseline_successes": min(baseline_successes) if baseline_successes else 0,
            "candidate_successes": min(candidate_successes) if candidate_successes else 0,
            "coverage": {
                "baseline": _coverage_share(baselines),
                "candidate": _coverage_share(candidates),
            },
            "required_samples": required_samples,
        }
    return evidence


def _coverage_share(runs):
    shares = []
    for run in runs:
        coverage = run["coverage"]
        if coverage.get("complete") is True:
            shares.append(1.0)
        elif coverage.get("complete") is False:
            shares.append(0.0)
    return min(shares) if shares else 1.0


def _aa(baselines, candidates, required_pairs):
    evidence = _metric_evidence(baselines, candidates)
    pairs = evidence["first_text_ms"]["pairs"]
    tolerance = analysis.tolerance("first_text_ms")
    result = analysis.aa_comparison(pairs, tolerance_ms=tolerance, required_pairs=required_pairs)
    result["tolerance_ms"] = tolerance
    result["interpretation"] = "same-build A/A measurement precision"
    return result


def _budget_section(name, baselines, candidates, budget, now):
    evidence = _metric_evidence(baselines, candidates)
    section = {
        "state": "inconclusive",
        "reasons": [],
        "budget": None,
    }
    if budget is None:
        section["reasons"].append("budget_not_declared")
        return section
    evaluated = analysis.evaluate_budget(budget, evidence, now=now)
    if not evaluated["metrics"]:
        section["reasons"].append("budget_metrics_unmatched")
        return section
    section.update(
        {
            "state": evaluated["state"],
            "reasons": evaluated["reasons"],
            "budget": evaluated,
            "revision": evaluated.get("revision"),
        }
    )
    return section


def _predecessor_section(baselines, candidates, budget, now):
    evidence = _metric_evidence(baselines, candidates)
    baseline_identity = baselines[0]["identity"]
    candidate_identity = candidates[0]["identity"]
    if budget is None:
        return {
            "state": "inconclusive",
            "reasons": ["budget_not_declared"],
            "baseline": baseline_identity,
            "candidate": candidate_identity,
            "budget": None,
        }
    release = analysis.release_comparison(
        baseline_identity, candidate_identity, budget, evidence
    )
    release["baseline"] = baseline_identity
    release["candidate"] = candidate_identity
    return release


def _bulk_gate(run):
    workflow = run.get("workflow_results") or {}
    gate = workflow.get("bulk_read_gate")
    if gate is None:
        return None
    return gate


def _apply_bulk_gate(result, runs):
    entries = [{"run": run["run"], "gate": _bulk_gate(run)} for run in runs]
    gates = [entry["gate"] for entry in entries if entry["gate"] is not None]
    if not gates:
        return
    if any(gate.get("state") != "complete" for gate in gates):
        result["bulk"] = {"state": "failed", "runs": entries}
        result["reasons"].append("bulk_gate_failed")
        if result["verdict"] not in ("regression",):
            result["verdict"] = "inconclusive"
    else:
        result["bulk"] = {"state": "complete", "runs": entries}


def save(path, result):
    from ..reports.model import atomic_write

    atomic_write(
        Path(path),
        json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
    )