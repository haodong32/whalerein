"""Private pytest adapter with sanitized native failure representations."""

import inspect
import json
import os
from pathlib import Path
from time import monotonic

import pytest

from repo.diagnostics import sanitize
from repo.test_timing import finish_interrupted, record_phase

_cases = {}
_errors = 0
_progress_enabled = False
_started = {}
_completed = set()


def pytest_configure(config):
    global _progress_enabled
    _progress_enabled = os.environ.get("WHALEREIN_PROGRESS") == "events" and hasattr(
        config, "pluginmanager"
    )
    _cases.clear()
    _started.clear()
    _completed.clear()
    global _errors
    _errors = 0
    config.addinivalue_line(
        "markers", "behavior(id, ensures): stable behavior identity and guarantee"
    )


def pytest_collection_finish(session):
    if _progress_enabled and not session.config.option.collectonly:
        from repo.progress import emit

        emit({"event": "detail", "description": "Python unit tests", "total": len(session.items)})
    for item in session.items:
        case = {
            "id": item.nodeid,
            "status": "not_run",
            "source": str(item.location[0]),
            "line": item.location[1] + 1,
        }
        try:
            doc = inspect.getdoc(item.obj)
            if doc:
                case["description"] = doc
                case["description_source"] = "docstring"
        except (OSError, TypeError, AttributeError):
            pass
        marker = item.get_closest_marker("behavior")
        if marker:
            if len(marker.args) == 2 and all(isinstance(v, str) and v for v in marker.args):
                case["behavior"] = {"id": marker.args[0], "ensures": marker.args[1]}
                chinese = getattr(marker, "kwargs", {}).get("description_zh")
                if isinstance(chinese, str) and chinese.strip():
                    case["description_zh"] = chinese.strip()
            else:
                global _errors
                _errors += 1
        from repo.test_descriptions import describe

        describe(case)
        _cases[item.nodeid] = case


def pytest_collectreport(report):
    global _errors
    if report.failed:
        _errors += 1


def pytest_runtest_logstart(nodeid, location):
    _started[nodeid] = monotonic()
    if _progress_enabled:
        from repo.progress import emit

        emit(
            {
                "event": "start",
                "id": nodeid,
                "description": _cases.get(nodeid, {}).get("description", nodeid),
            }
        )


def pytest_runtest_logreport(report):
    case = _cases.setdefault(report.nodeid, {"id": report.nodeid, "status": "not_run"})
    if hasattr(report, "duration"):
        record_phase(case, report)
    if report.when == "teardown":
        _completed.add(report.nodeid)
    if report.failed:
        case["status"] = "failed"
        # longrepr excludes captured stdout/stderr sections and local variables by default.
        case.setdefault("diagnostics", []).append(
            {
                "phase": report.when,
                "message": sanitize(
                    getattr(report, "longreprtext", "") or getattr(report, "longrepr", "")
                ),
            }
        )
    elif case["status"] != "failed":
        if report.skipped:
            case["status"] = "skipped"
            case["skip_reason"] = sanitize(getattr(report, "longrepr", ""))
        elif report.when == "call":
            case["status"] = "passed"

    if report.when == "teardown" and _progress_enabled:
        from repo.progress import emit

        emit(
            {
                "event": "finish",
                "id": report.nodeid,
                "description": case.get("description", report.nodeid),
                "status": case["status"],
            }
        )


@pytest.hookimpl(trylast=True)
def pytest_sessionfinish(session, exitstatus):
    finish_interrupted(_cases, _started, monotonic(), _completed)
    destination = os.environ.get("WHALEREIN_TEST_REPORT")
    if destination:
        Path(destination).write_text(
            json.dumps(
                {
                    "tests": list(_cases.values()),
                    "collected": session.testscollected,
                    "collection_errors": _errors,
                    "exit_code": int(exitstatus),
                }
            )
        )


def pytest_collection_modifyitems(config, items):
    only = os.environ.get("WHALEREIN_TEST_ONLY")
    if only:
        deselected = [item for item in items if only not in item.nodeid]
        items[:] = [item for item in items if only in item.nodeid]
        config.hook.pytest_deselected(items=deselected)
