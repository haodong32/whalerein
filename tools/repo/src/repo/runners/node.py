"""Native test execution and measurement."""

from __future__ import annotations

import json
import re
import shlex
from collections import Counter
from pathlib import Path

from ..diagnostics import redact, sanitize
from .coverage import CoverageResult


def name_pattern(selector, anchored=False):
    """Match encoded ID substrings against native, unescaped full names.

    An unqualified substring can start/end inside a two-character escape:
    `1child` also matches `/child`, and a trailing `~` matches `~` or `/`.
    """

    def decode(text):
        parts = []
        index = 0
        while index < len(text):
            char = text[index]
            if char == "~":
                if index + 1 == len(text):
                    parts.append("[~/]")
                elif text[index + 1] in "01":
                    parts.append(re.escape("~" if text[index + 1] == "0" else "/"))
                    index += 1
                else:
                    return None
            else:
                parts.append(re.escape(" " if char == "/" else char))
            index += 1
        return "".join(parts)

    candidates = [decode(selector)]
    if not anchored and selector.startswith(("0", "1")):
        suffix = decode(selector[1:])
        if suffix is not None:
            candidates.append(re.escape("~" if selector[0] == "0" else "/") + suffix)
    alternatives = [pattern for pattern in candidates if pattern is not None]
    if not alternatives:
        return "(?!)"
    return ("^" if anchored else "") + "(?:" + "|".join(alternatives) + ")"


def collect(project, runner, execute=False, only=None, coverage=False):
    scope = {
        "project": project.name,
        "tests": [],
        "coverage": [],
        "completeness": "incomplete",
        "note": "",
        "exit_code": None,
    }
    if not execute:
        scope["note"] = "Node static case listing is unsupported; tests were not executed."
        return scope
    reporter = Path(__file__).parent.parent / "assets" / "node-reporter.mjs"
    files = sorted(
        {
            p
            for directory in ("test", "tests")
            for pattern in ("*.test.js", "*.test.cjs", "*.test.mjs")
            for p in (project.path / directory).rglob(pattern)
        }
    )
    if not files:
        scope["note"] = "No supported *.test.js entry points; scope not collected."
        return scope
    name_filter = None
    if only:
        if "::" in only:
            location, name_filter = only.split("::", 1)
            files = [path for path in files if location in str(path)]
        elif any(only in str(path) for path in files):
            files = [path for path in files if only in str(path)]
        if not files:
            scope["completeness"] = "complete"
            return scope
    if name_filter is not None:
        # Native full names use spaces; canonical IDs encode each path segment.
        name_filter = name_pattern(name_filter, anchored=True)
    elif only and not any(only in str(path) for path in files):
        name_filter = name_pattern(only)
    flags = []
    if coverage:
        script = json.loads(project.manifest.read_text()).get("scripts", {}).get("test", "")
        flags = [flag for flag in shlex.split(script) if flag.startswith("--test-coverage-")]
        flags = ["--experimental-test-coverage", *flags]
    result = runner.run(
        [
            "node",
            "--test",
            *flags,
            *(["--test-name-pattern", name_filter] if name_filter else []),
            "--test-reporter",
            str(reporter),
            *map(str, files),
        ],
        project.path,
        quiet=True,
        timeout=1800,
    )
    scope["exit_code"] = result.returncode
    if not result.ok:
        scope["errors"] = [sanitize(result.tail()) or "Native test execution failed"]
    events = []
    try:
        events = [json.loads(line) for line in result.stdout.splitlines()]
        summaries = [e for e in events if e.get("summary") and (not e.get("file"))]
        previous_nesting = {}
        for index, event in enumerate(events):
            file = event.get("file")
            nesting = event.get("nesting", 0) or 0
            parent = previous_nesting.get(file, -1) > nesting
            if not event.get("summary"):
                previous_nesting[file] = nesting
            if (
                not event.get("summary")
                and not event.get("coverage")
                and event.get("type") != "suite"
            ):
                scope["tests"].append(
                    {
                        "id": redact(f"{event.get('file')}:{event.get('line')}::{event['name']}"),
                        "status": event["status"],
                        **(
                            {"duration_ms": event["duration_ms"]}
                            if isinstance(event.get("duration_ms"), (int, float))
                            else {}
                        ),
                        "nesting": event.get("nesting"),
                        "native_type": event.get("type"),
                        "event_index": index,
                        "node_role": "case",
                        "has_children": parent,
                        **(
                            {
                                "diagnostics": [
                                    {"phase": "test", "message": sanitize(event["error"])}
                                ]
                            }
                            if event.get("error")
                            else {}
                        ),
                        **(
                            {"skip_reason": sanitize(event["skip_reason"])}
                            if event.get("skip_reason")
                            else {}
                        ),
                    }
                )
        if len(summaries) == 1:
            counts = summaries[0]["counts"]
            observed = Counter(c["status"] for c in scope["tests"])
            if counts["tests"] == len(scope["tests"]) and all(
                counts.get(key, 0) == observed[key]
                for key in ("passed", "failed", "skipped", "todo", "cancelled")
            ):
                scope["completeness"] = "complete"
    except (ValueError, KeyError, AttributeError, TypeError):
        scope["note"] = "Malformed Node reporter stream. "
    if only:
        file_nodes = {
            f"{event.get('file')}:{event.get('line')}::{event.get('name')}"
            for event in events
            if isinstance(event, dict)
            and event.get("name") == event.get("file")
            and (event.get("status") == "passed")
        }
        scope["tests"] = [case for case in scope["tests"] if case["id"] not in file_nodes]
        scope["tests"] = [
            case
            for case in scope["tests"]
            if not (
                case["status"] == "skipped"
                and "test name does not match pattern" in case.get("skip_reason", "")
            )
        ]
    for case in scope["tests"]:
        index = case["event_index"]
        event = events[index]
        parents = []
        level = event.get("nesting", 0) or 0
        for later in events[index + 1 :]:
            if (
                later.get("file") == event.get("file")
                and not later.get("summary")
                and not later.get("coverage")
                and (later.get("nesting", 0) or 0) < level
            ):
                parents.insert(0, later["name"])
                level = later.get("nesting", 0) or 0
        file = event.get("file") or "unknown"
        path = Path(file)
        if path.is_absolute() and path.is_relative_to(project.path):
            file = path.relative_to(project.path).as_posix()
        case["name"] = " ".join([*parents, event["name"]])
        case["source"] = file
        case["line"] = event.get("line")
        segments = [
            part.replace("~", "~0").replace("/", "~1") for part in [*parents, event["name"]]
        ]
        case["id"] = file + "::" + "/".join(segments)
    if len({case["id"] for case in scope["tests"]}) != len(scope["tests"]):
        scope["completeness"] = "incomplete"
        scope["errors"] = ["Duplicate Node test identity; use unique names within each parent"]
    if coverage:
        measured = CoverageResult(project.name, "node", project.rel)
        measurements = [event["coverage"] for event in events if "coverage" in event]
        if len(measurements) == 1:
            measured.percent = measurements[0]["coveredLinePercent"]
            measured.covered = measurements[0]["coveredLineCount"]
            measured.total = measurements[0]["totalLineCount"]
            if not result.ok:
                measured.error = "Coverage execution failed"
        else:
            measured.error = "Missing native Node coverage event"
        scope["coverage"].append({**measured.as_dict(), "unit": "lines", "phase": "test_execution"})
    scope["note"] += "Native Node test nodes (including parent tests), suites excluded."
    return scope


def steps(project, action):
    from ..toolchain import Step

    scripts = json.loads(project.manifest.read_text()).get("scripts", {})
    names = (
        [name for name in ("fmt:check", "lint") if name in scripts]
        if action == "check"
        else [action]
    )
    return [Step(f"{project.id}: {name}", ("pnpm", "run", name), project.path) for name in names]
