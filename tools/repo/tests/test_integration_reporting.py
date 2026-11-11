"""Verify actual pytest report adaptation without a product environment."""

import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace

import pytest


@pytest.fixture
def reporting(tmp_path, monkeypatch):
    directory = Path(__file__).resolve().parents[3] / "tests/integration"
    monkeypatch.syspath_prepend(str(directory))
    spec = importlib.util.spec_from_file_location(
        "integration_reporting", directory / "integration_report.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    monkeypatch.setenv("WHALEREIN_INTEGRATION_CONTAINER", "1")
    monkeypatch.setattr(module, "Path", lambda name: tmp_path / name.lstrip("/"))
    return module


def finish(module, tmp_path, collect=False, description=None, parameter_description=None):
    def function():
        return None

    function.__doc__ = description
    item = SimpleNamespace(
        nodeid="case.one",
        iter_markers=lambda: [],
        function=function,
        get_closest_marker=lambda name: (
            SimpleNamespace(args=(parameter_description,))
            if parameter_description is not None
            else None
        ),
    )
    config = SimpleNamespace(
        option=SimpleNamespace(collectonly=collect), getoption=lambda name: None
    )
    session = SimpleNamespace(items=[item], config=config, exitstatus=0)
    module.pytest_sessionfinish(session, 0)
    return session, json.loads((tmp_path / "reports/integration.json").read_text())


def phase(module, when, outcome="passed"):
    module.pytest_runtest_logreport(
        SimpleNamespace(
            nodeid="case.one",
            when=when,
            passed=outcome == "passed",
            failed=outcome == "failed",
            skipped=outcome == "skipped",
        )
    )


def test_fixture_phases_do_not_inflate_case_count(reporting, tmp_path):
    for when in ["setup", "call", "teardown"]:
        phase(reporting, when)
    session, document = finish(reporting, tmp_path)
    assert session.exitstatus == 0
    assert len(document["scope"]["tests"]) == 1
    assert [c["status"] for c in document["scope"]["tests"]] == ["passed"]


def test_teardown_failure_preserves_failure(reporting, tmp_path):
    phase(reporting, "setup")
    phase(reporting, "call")
    phase(reporting, "teardown", "failed")
    session, document = finish(reporting, tmp_path)
    assert session.exitstatus == 1
    assert [c["status"] for c in document["scope"]["tests"]] == ["failed"]


def test_missing_teardown_is_not_success(reporting, tmp_path):
    phase(reporting, "setup")
    phase(reporting, "call")
    session, document = finish(reporting, tmp_path)
    assert session.exitstatus == 1
    assert [c["status"] for c in document["scope"]["tests"]] == ["interrupted"]


def test_collection_does_not_claim_execution(reporting, tmp_path):
    session, document = finish(reporting, tmp_path, collect=True)
    assert session.exitstatus == 0
    assert [c["status"] for c in document["scope"]["tests"]] == ["not_run"]


def test_required_skip_fails_gate(reporting, tmp_path):
    phase(reporting, "setup", "skipped")
    phase(reporting, "teardown")
    session, document = finish(reporting, tmp_path)
    assert session.exitstatus == 1
    assert [c["status"] for c in document["scope"]["tests"]] == ["skipped"]


def test_authored_description_is_preserved_without_changing_identity(reporting, tmp_path):
    for when in ("setup", "call", "teardown"):
        phase(reporting, when)
    _, document = finish(reporting, tmp_path, description="Rejects the expired approval.")
    case = document["scope"]["tests"][0]
    assert case["id"] == "case.one"
    assert case["description"] == "Rejects the expired approval."
    assert case["description_source"] == "docstring"


def test_parameter_description_overrides_shared_docstring_without_renaming_case(
    reporting, tmp_path
):
    for when in ("setup", "call", "teardown"):
        phase(reporting, when)
    _, document = finish(
        reporting,
        tmp_path,
        description="Shared wrapper description.",
        parameter_description=(
            "When a pending approval is rejected, DSH records the rejected decision."
        ),
    )
    case = document["scope"]["tests"][0]
    assert case["id"] == "case.one"
    assert (
        case["description"]
        == "When a pending approval is rejected, DSH records the rejected decision."
    )
    assert case["description_source"] == "behavior"


def test_multiline_source_description_is_displayed_as_one_paragraph(reporting, tmp_path):
    for when in ("setup", "call", "teardown"):
        phase(reporting, when)
    _, document = finish(
        reporting, tmp_path, description="When a device is revoked,\n    its token is rejected."
    )
    assert (
        document["scope"]["tests"][0]["description"]
        == "When a device is revoked, its token is rejected."
    )


def test_collection_rejects_missing_or_shared_descriptions(reporting):
    def item(identity, description):
        return SimpleNamespace(
            nodeid=identity, get_closest_marker=lambda name: SimpleNamespace(args=(description,))
        )

    with pytest.raises(ValueError, match="Missing authored"):
        reporting.validate_descriptions([item("case.missing", "")])
    with pytest.raises(ValueError, match="Shared integration"):
        reporting.validate_descriptions(
            [
                item("case.one", "When a pool closes, operations fail."),
                item("case.two", "When a pool closes, operations fail."),
            ]
        )
    reporting.validate_descriptions(
        [
            item("case.one", "When a pool closes, operations fail."),
            item("case.two", "When a device is revoked, its token is rejected."),
        ]
    )


@pytest.mark.parametrize("phase", ["setup", "call", "teardown"])
def test_interruption_cleans_temporary_paths_without_traceback(tmp_path, phase):
    import os
    import subprocess
    import sys

    directory = Path(__file__).resolve().parents[3] / "tests/integration"
    (tmp_path / "conftest.py").write_text(
        (directory / "conftest.py").read_text()
        + '\n@pytest.fixture(scope="session", autouse=True)\ndef container_only():\n    pass\n'
    )
    (tmp_path / "test_cancel.py").write_text(
        "import pytest\n"
        "@pytest.fixture\n"
        "def resource(tmp_path):\n"
        + ("    raise KeyboardInterrupt\n" if phase == "setup" else "")
        + "    yield tmp_path\n"
        + ("    raise KeyboardInterrupt\n" if phase == "teardown" else "")
        + "def test_cancel(resource):\n"
        '    """When interrupted, temporary resources close without secondary errors."""\n'
        + ("    raise KeyboardInterrupt\n" if phase == "call" else "    pass\n")
    )
    environment = {
        **os.environ,
        "PYTHONPATH": os.pathsep.join(
            [str(directory), str(directory.parents[1] / "tools/repo/src")]
        ),
        "NO_COLOR": "1",
        "WHALEREIN_INTEGRATION_CONTAINER": "0",
    }
    result = subprocess.run(
        [sys.executable, "-m", "pytest", "-q", str(tmp_path)],
        cwd=tmp_path,
        env=environment,
        capture_output=True,
        text=True,
        timeout=20,
    )
    assert result.returncode == 2, result.stdout + result.stderr
    assert "KeyError" not in result.stderr
    assert "Traceback" not in result.stderr
