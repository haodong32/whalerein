"""Versioned execution facts, strict validation and atomic persistence."""

from __future__ import annotations

import json
import math
import os
import tempfile
from collections import Counter
from pathlib import Path

from ..errors import RepoError

VERSION = 3
STATUSES = {"passed", "failed", "skipped", "blocked", "not_run", "interrupted", "todo", "cancelled"}


def atomic_write(path: Path, content: str):
    path.parent.mkdir(parents=True, exist_ok=True)
    name = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w", encoding="utf-8", dir=path.parent, delete=False
        ) as handle:
            name = handle.name
            handle.write(content)
        os.replace(name, path)
    finally:
        if name and Path(name).exists():
            Path(name).unlink()


def summary(scopes):
    counts = Counter(case["status"] for scope in scopes for case in scope["tests"])
    cases = sum(counts.values())
    complete = all(s["completeness"] == "complete" for s in scopes)
    return {
        "cases": cases,
        "known_cases": cases,
        "total_cases": cases if complete else None,
        "containers": 0,
        "steps": 0,
        "statuses": dict(sorted(counts.items())),
    }


def number(value):
    return type(value) in (int, float) and math.isfinite(value) and value >= 0


def strings(value, keys):
    if any(key in value and not isinstance(value[key], str) for key in keys):
        raise RepoError("invalid report text field")


def validate_scope(scope):
    if not isinstance(scope, dict) or not all(
        isinstance(scope.get(k), str) and scope[k] for k in ("suite_id", "project")
    ):
        raise RepoError("invalid report scope identity")
    if scope.get("completeness") not in ("complete", "incomplete") or not isinstance(
        scope.get("tests"), list
    ):
        raise RepoError("invalid report scope completeness or tests")
    strings(scope, ("kind", "language", "path", "module", "note"))
    if (
        "exit_code" in scope
        and scope["exit_code"] is not None
        and type(scope["exit_code"]) is not int
    ):
        raise RepoError("invalid native exit code")
    checks = scope.get("checks", [])
    if not isinstance(checks, list) or any(
        not isinstance(c, dict) or not all(isinstance(c.get(k), str) for k in ("name", "status"))
        for c in checks
    ):
        raise RepoError("invalid execution checks")
    identities = set()
    for case in scope["tests"]:
        if (
            not isinstance(case, dict)
            or not isinstance(case.get("id"), str)
            or not case["id"]
            or case["id"] in identities
            or case.get("status") not in STATUSES
        ):
            raise RepoError("invalid or duplicate test identity/status")
        identities.add(case["id"])
        strings(
            case,
            (
                "name",
                "description",
                "description_source",
                "source",
                "submodule",
                "reason",
                "skip_reason",
                "node_role",
                "kind",
            ),
        )
        timing(case)
        if "diagnostics" in case and (
            not isinstance(case["diagnostics"], list)
            or any(
                not isinstance(d, dict) or not isinstance(d.get("message"), str)
                for d in case["diagnostics"]
            )
        ):
            raise RepoError("invalid test diagnostics")
    timing(scope)
    if not isinstance(scope.get("coverage", []), list):
        raise RepoError("invalid coverage array")
    for coverage in scope.get("coverage", []):
        if not isinstance(coverage, dict) or not isinstance(coverage.get("name"), str):
            raise RepoError("invalid coverage identity")
        strings(coverage, ("error", "note", "measurement_scope", "phase"))
        for key in ("percent", "threshold"):
            if coverage.get(key) is not None and (not number(coverage[key]) or coverage[key] > 100):
                raise RepoError("invalid coverage percentage")
        if not isinstance(coverage.get("ok"), bool) or coverage.get("unit") not in (
            "lines",
            "statements",
        ):
            raise RepoError("invalid coverage outcome or unit")
        if coverage.get("error") and coverage["ok"]:
            raise RepoError("coverage error cannot be successful")
        if (
            coverage.get("percent") is not None
            and coverage.get("threshold") is not None
            and not coverage.get("error")
        ):
            if coverage["ok"] != (coverage["percent"] + 1e-9 >= coverage["threshold"]):
                raise RepoError("coverage outcome disagrees with threshold")
        for key in ("covered", "total"):
            if key in coverage and not number(coverage[key]):
                raise RepoError("invalid coverage count")
        if coverage.get("covered", 0) > coverage.get("total", 0):
            raise RepoError("covered count exceeds total")
    if not isinstance(scope.get("errors", []), list) or any(
        not isinstance(e, str) for e in scope.get("errors", [])
    ):
        raise RepoError("invalid execution errors")


def timing(value):
    if "duration_ms" in value and not number(value["duration_ms"]):
        raise RepoError("invalid report duration")
    if "duration_complete" in value and not isinstance(value["duration_complete"], bool):
        raise RepoError("invalid duration completeness")
    if "phase_durations_ms" in value and (
        not isinstance(value["phase_durations_ms"], dict)
        or any(not number(v) for v in value["phase_durations_ms"].values())
    ):
        raise RepoError("invalid phase durations")


def validate(data):
    if not isinstance(data, dict) or data.get("schema_version") != VERSION:
        raise RepoError("unsupported test report schema_version")
    if not isinstance(data.get("scopes"), list) or not data["scopes"]:
        raise RepoError("report requires nonempty scopes")
    seen = set()
    for scope in data["scopes"]:
        validate_scope(scope)
        if scope["suite_id"] in seen:
            raise RepoError("duplicate report scope")
        seen.add(scope["suite_id"])
    timing(data)
    if data.get("summary") != summary(data["scopes"]):
        raise RepoError("report summary disagrees with execution facts")
    expected = (
        "complete" if all(s["completeness"] == "complete" for s in data["scopes"]) else "incomplete"
    )
    if data.get("completeness") != expected:
        raise RepoError("report completeness disagrees with scopes")
    if not isinstance(data.get("configuration"), dict):
        raise RepoError("missing report configuration")


def read_report(path):
    try:
        data = json.loads(path.read_text())
    except (OSError, ValueError) as exc:
        raise RepoError("cannot read test report") from exc
    validate(data)
    return data
