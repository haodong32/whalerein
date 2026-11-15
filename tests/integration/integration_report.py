"""Thin pytest adapter to the repository report format."""

import json
import os
import re
from pathlib import Path
from time import monotonic

import pytest
from repo.test_timing import finish_interrupted, record_phase

_RESULTS = {}
_PHASES = {}
_COLLECTION_ERRORS = False
_DESELECTED = False


def describe_item(item):
    parameter = getattr(item, "get_closest_marker", lambda name: None)("description")
    if parameter and len(parameter.args) != 1:
        raise ValueError(f"Description requires one text argument: {item.nodeid}")
    authored = (
        parameter.args[0]
        if parameter
        else getattr(getattr(item, "function", None), "__doc__", None)
    )
    if not isinstance(authored, str) or not authored.strip():
        return None, None
    return " ".join(authored.split()), "behavior" if parameter else "docstring"


def validate_descriptions(items):
    seen = {}
    for item in items:
        description, _ = describe_item(item)
        if not description or description == item.nodeid:
            raise ValueError(f"Missing authored integration description: {item.nodeid}")
        if description in seen:
            raise ValueError(
                f"Shared integration description: {seen[description]} and {item.nodeid}"
            )
        seen[description] = item.nodeid


def pytest_collectreport(report):
    global _COLLECTION_ERRORS
    _COLLECTION_ERRORS |= report.failed


def pytest_deselected(items):
    global _DESELECTED
    _DESELECTED |= bool(items)


def failure_message(report):
    # Export source locations only. Pytest assertion text, source lines and
    # captured output can contain decrypted messages or credentials.
    traceback = getattr(getattr(report, "longrepr", None), "reprtraceback", None)
    locations = []
    for entry in getattr(traceback, "reprentries", []):
        location = getattr(entry, "reprfileloc", None)
        filename = str(getattr(location, "path", "")).replace("\\", "/").rsplit("/", 1)[-1]
        line = getattr(location, "lineno", None)
        if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*\.py", filename) and type(line) is int and line > 0:
            locations.append(f"{filename}:{line}")
    if locations:
        return "Integration failure at " + " -> ".join(locations[-8:])
    return "Integration assertion or fixture failed; source location unavailable."


def pytest_runtest_logreport(report):
    _PHASES.setdefault(report.nodeid, set()).add(report.when)
    record = _RESULTS.setdefault(
        report.nodeid,
        {
            "id": report.nodeid,
            "name": report.nodeid,
            "kind": "integration",
            "node_role": "case",
            "status": "interrupted",
        },
    )
    if hasattr(report, "duration"):
        record_phase(record, report)
    if report.when == "call" and report.passed:
        record["status"] = "passed"
    if report.failed:
        record["status"] = "failed"
        record.setdefault("diagnostics", []).append(
            {
                "phase": report.when,
                "message": failure_message(report),
            }
        )
    elif report.skipped and record["status"] != "failed":
        record["status"] = "skipped"

    if report.when == "teardown":
        progress(
            {
                "event": "finish",
                "id": report.nodeid,
                "description": _DESCRIPTIONS.get(report.nodeid, report.nodeid),
                "status": record["status"],
                "phase": {"call": "action", "teardown": "cleanup"}.get(
                    record.get("diagnostics", [{}])[0].get("phase", "test"),
                    record.get("diagnostics", [{}])[0].get("phase", "test"),
                ),
            }
        )


@pytest.hookimpl(trylast=True)
def pytest_sessionfinish(session, exitstatus):
    if os.environ.get("WHALEREIN_INTEGRATION_CONTAINER") != "1":
        return
    shard = getattr(session.config.option, "shard", None)
    if shard and not session.items and session.exitstatus == 5:
        # An intentional empty shard is a successful (empty) selection, not
        # pytest's "no tests collected" failure.
        session.exitstatus = 0
    for item in session.items:
        _RESULTS.setdefault(
            item.nodeid,
            {
                "id": item.nodeid,
                "name": item.nodeid,
                "status": "interrupted" if item.nodeid in _STARTED else "not_run",
                "kind": "integration",
                "node_role": "case",
            },
        )
    finish_interrupted(_RESULTS, _CASE_STARTED, monotonic(), {identity for identity, phases in _PHASES.items() if "teardown" in phases})
    for item in session.items:
        record = _RESULTS[item.nodeid]
        authored, source = describe_item(item)
        if authored:
            record["description"] = authored
            record["description_source"] = source
        record["groups"] = sorted(
            {m.name for m in item.iter_markers()}
            & {"chain", "protocol", "storage", "transport", "native-dsh"}
        )
        if record["status"] == "passed" and "teardown" not in _PHASES.get(
            item.nodeid, set()
        ):
            record["status"] = "interrupted"
    if (
        not session.config.option.collectonly
        and any(r["status"] != "passed" for r in _RESULTS.values())
        and session.exitstatus == 0
    ):
        session.exitstatus = 1
    # With --shard, deselecting the other shards is distribution rather than a
    # narrowed user selection. --group/--only still make the selection partial.
    partial = (
        bool(session.config.getoption("--group") or session.config.getoption("--only"))
        if shard
        else _DESELECTED
    )
    scope = {
        "project": "integration",
        "kind": "integration",
        "suite_id": "integration",
        "tests": list(_RESULTS.values()),
        "coverage": [],
        "checks": [],
        "completeness": "complete"
        if (session.items or shard)
        and not _COLLECTION_ERRORS
        and session.exitstatus in (0, 1)
        and (session.config.option.collectonly or all(
            r["status"] not in ("interrupted", "not_run") for r in _RESULTS.values()
        ))
        else "incomplete",
        "exit_code": int(session.exitstatus),
        "selection": "partial" if partial else "full",
    }
    if shard:
        # Sharding distributes an existing selection; it does not narrow it.
        scope["shard"] = shard
    Path("/reports").mkdir(exist_ok=True)
    tests = scope["tests"]
    for case in tests:
        case.setdefault("description", case["name"])
        case.setdefault("description_source", "native_name")
    if not session.config.option.collectonly and _SESSION_STARTED is not None:
        scope["duration_ms"] = round((monotonic() - _SESSION_STARTED) * 1000, 3)
    scope["errors"] = ["Integration collection failed"] if _COLLECTION_ERRORS else []
    Path("/reports/integration.json").write_text(json.dumps({"version": 1, "scope": scope}))
    if _PROGRESS_STREAM is not None:
        _PROGRESS_STREAM.close()


_PROGRESS_STREAM = None
_DESCRIPTIONS = {}
_STARTED = set()
_CASE_STARTED = {}
_SESSION_STARTED = None


@pytest.hookimpl(trylast=True)
def pytest_configure(config):
    if not config.option.collectonly:
        terminal = config.pluginmanager.get_plugin("terminalreporter")
        if terminal:
            config.pluginmanager.unregister(terminal)


def pytest_sessionstart(session):
    global _PROGRESS_STREAM, _SESSION_STARTED
    _SESSION_STARTED = monotonic()
    if not session.config.option.collectonly:
        _PROGRESS_STREAM = os.fdopen(os.dup(1), "w", buffering=1)


def progress(event):
    if _PROGRESS_STREAM:
        print("WHALEREIN_PROGRESS " + json.dumps(event), file=_PROGRESS_STREAM, flush=True)


def pytest_collection_finish(session):
    for item in session.items:
        _DESCRIPTIONS[item.nodeid] = describe_item(item)[0] or item.nodeid
    if not session.config.option.collectonly:
        progress(
            {
                "event": "phase",
                "description": f"Integration · {len(session.items)} cases",
                "total": len(session.items),
            }
        )


def pytest_runtest_logstart(nodeid, location):
    _STARTED.add(nodeid)
    _CASE_STARTED[nodeid] = monotonic()
    progress(
        {
            "event": "start",
            "id": nodeid,
            "description": _DESCRIPTIONS.get(nodeid, nodeid),
        }
    )
