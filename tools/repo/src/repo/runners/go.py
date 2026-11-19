"""Native test execution and measurement."""

from __future__ import annotations

import json
import re
import tempfile
from collections import Counter
from dataclasses import replace
from pathlib import Path

from ..diagnostics import redact, sanitize
from ..projects import go_test_paths
from ..reports.model import atomic_write
from .coverage import CoverageResult, parse_go_coverprofile


def case_id(package: str, name: str) -> str:
    """Redact native components before adding the report's structural separator."""
    return f"{redact(package)}::{redact(name)}"


def collect(project, runner, execute=False, only=None, coverage=False, coverage_output=None):
    if coverage:
        with tempfile.TemporaryDirectory(prefix="whalerein-go-cover-") as tmp:
            profile = Path(tmp) / "cover.out"
            scope = _collect(project, runner, execute, only, profile)
            try:
                raw_profile = profile.read_text()
                parsed = parse_go_coverprofile(raw_profile)
                if not parsed:
                    raise ValueError("empty coverage profile")
                if coverage_output is not None:
                    try:
                        atomic_write(coverage_output, raw_profile)
                    except OSError:
                        scope.setdefault("errors", []).append("Could not save Go coverage profile")
                for name, values in parsed.items():
                    if "/proto/" in name:
                        continue
                    measured = CoverageResult(
                        name, "go", project.rel, covered=values["covered"], total=values["total"]
                    )
                    measured.percent = (
                        values["covered"] / values["total"] * 100 if values["total"] else None
                    )
                    if scope["exit_code"] != 0:
                        measured.error = "Coverage execution failed"
                    scope["coverage"].append(
                        {**measured.as_dict(), "unit": "statements", "phase": "test_execution"}
                    )
            except (OSError, ValueError):
                scope["coverage"].append(
                    {
                        "name": project.id,
                        "percent": None,
                        "ok": False,
                        "unit": "statements",
                        "error": "Missing or invalid Go coverage profile",
                    }
                )
            return scope
    return _collect(project, runner, execute, only)


def _collect(project, runner, execute=False, only=None, profile=None):
    scope = {
        "project": project.name,
        "tests": [],
        "coverage": [],
        "completeness": "incomplete",
        "note": "",
        "exit_code": None,
    }
    argv = ["go", "test", "-json", "-count=1"]
    if profile is not None:
        argv += ["-covermode=set", f"-coverprofile={profile}"]
    if not execute:
        argv += ["-list", "."]
    if only:
        inventory = collect(project, runner, execute=False)
        if inventory.get("exit_code") == 130:
            return inventory
        selected = [case["id"] for case in inventory["tests"] if only in case["id"]]
        if not selected and "::" in only and ("/" in only.split("::", 1)[1]):
            package, child = only.split("::", 1)
            parent = package + "::" + child.split("/", 1)[0]
            if any(case["id"] == parent for case in inventory["tests"]):
                selected = [only]
        if not execute:
            inventory["tests"] = [case for case in inventory["tests"] if case["id"] in selected]
            return inventory
        if not selected:
            inventory["tests"] = []
            return inventory
        grouped = {}
        for identity in selected:
            package, name = identity.split("::", 1)
            grouped.setdefault(package, []).append(name)
        results = []
        unexecuted = []
        for index, (package, names) in enumerate(grouped.items()):
            patterns = [
                "/".join("^" + re.escape(part) + "$" for part in name.split("/")) for name in names
            ]
            pattern = (
                patterns[0]
                if len(patterns) == 1
                else "^(?:" + "|".join(re.escape(name) for name in names) + ")$"
            )
            results.append(
                runner.run(
                    [*argv, "-run", pattern, package], project.path, quiet=True, timeout=1800
                )
            )
            if results[-1].returncode == 130:
                unexecuted = [
                    (package, name)
                    for package in list(grouped)[index + 1 :]
                    for name in grouped[package]
                ]
                break
        result = replace(
            results[0],
            stdout="\n".join(r.stdout for r in results),
            stderr="\n".join(r.stderr for r in results),
            returncode=130
            if any(r.returncode == 130 for r in results)
            else next((r.returncode for r in results if r.returncode), 0),
        )
    else:
        unexecuted = []
        result = runner.run(
            [*argv, *go_test_paths(project)], project.path, quiet=True, timeout=1800
        )
    scope["exit_code"] = result.returncode
    if not result.ok:
        scope["errors"] = [sanitize(result.tail()) or "Native test execution failed"]
    cases = dict.fromkeys(unexecuted, "not_run")
    messages = {}
    durations = {}
    malformed = False
    packages = set()
    finished = set()
    for line in result.stdout.splitlines():
        if not line.strip():
            continue
        try:
            event = json.loads(line)
            package = event.get("Package", "")
            name = event.get("Test")
            if package:
                packages.add(package)
            if not name and event.get("Action") in ("pass", "fail", "skip"):
                finished.add(package)
            if not execute and event.get("Action") == "output":
                candidate = event.get("Output", "").strip()
                if re.fullmatch("(Test|Example|Fuzz)\\w+", candidate):
                    cases[package, candidate] = "not_run"
            if execute and name:
                key = package, name
                if event.get("Action") == "output":
                    messages[key] = sanitize(messages.get(key, "") + event.get("Output", ""))
                status = {
                    "run": "interrupted",
                    "pass": "passed",
                    "fail": "failed",
                    "skip": "skipped",
                }.get(event.get("Action"))
                if status:
                    cases[key] = status
                    if "Elapsed" in event:
                        durations[key] = round(event["Elapsed"] * 1000, 3)
        except (ValueError, AttributeError):
            malformed = True
    identities = {key: case_id(*key) for key in cases}
    counts = Counter(identities.values())
    ambiguous = any(count > 1 for count in counts.values())
    if ambiguous:
        scope.setdefault("errors", []).append("Go case IDs collide after redaction")
    for key, status in sorted(cases.items()):
        identity = identities[key]
        if counts[identity] != 1:
            continue
        case = {"id": identity, "status": status}
        scope["tests"].append(case)
        if key in durations:
            case["duration_ms"] = durations[key]
        if case["status"] == "failed" and messages.get(key):
            case["diagnostics"] = [{"phase": "test", "message": messages[key]}]
        if case["status"] == "skipped" and messages.get(key):
            case["skip_reason"] = messages[key]
    scope["note"] = (
        "Native Go events; dynamic children require execution. "
        "Fuzz campaigns and build matrices are excluded."
    )
    if (
        result.returncode != 130
        and packages
        and packages == finished
        and not (malformed or ambiguous)
    ):
        if all(c["status"] != "interrupted" for c in scope["tests"]):
            scope["completeness"] = "complete"
            scope["note"] = (
                "Native Go test nodes under the current configuration; parent tests are included. "
                "Fuzz campaigns are excluded."
            )
    if malformed:
        scope["note"] += " Malformed native event stream."
    return scope


def steps(project, action):
    from ..projects import source_files
    from ..toolchain import Step

    if action in ("fmt", "check"):
        files = tuple(str(p) for p in source_files(project, "*.go"))
        result = (
            [
                Step(
                    f"{project.id}: gofmt",
                    ("gofmt", "-w" if action == "fmt" else "-l", *files),
                    project.path,
                    action == "check",
                )
            ]
            if files
            else []
        )
        if action == "check":
            result.append(
                Step(f"{project.id}: vet", ("go", "vet", *go_test_paths(project)), project.path)
            )
        return result
    result = [Step(f"{project.id}: build", ("go", "build", *go_test_paths(project)), project.path)]
    if project.binary:
        output = project.root / ".data/bin" / ("whalerein-" + project.id)
        output.parent.mkdir(parents=True, exist_ok=True)
        result.append(
            Step(
                f"{project.id}: binary",
                ("go", "build", "-o", str(output), project.binary),
                project.path,
            )
        )
    return result
