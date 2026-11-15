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


def finish(
    module,
    tmp_path,
    collect=False,
    description=None,
    parameter_description=None,
    groups=(),
    shard=None,
    items=None,
    group=None,
    only=None,
    exitstatus=0,
):
    def function():
        return None

    function.__doc__ = description
    item = SimpleNamespace(
        nodeid="case.one",
        iter_markers=lambda: [SimpleNamespace(name=group) for group in groups],
        function=function,
        get_closest_marker=lambda name: (
            SimpleNamespace(args=(parameter_description,))
            if parameter_description is not None
            else None
        ),
    )
    config = SimpleNamespace(
        option=SimpleNamespace(collectonly=collect, shard=shard),
        getoption=lambda name: group if name == "--group" else only if name == "--only" else None,
    )
    session = SimpleNamespace(items=[item] if items is None else items, config=config, exitstatus=0)
    module.pytest_sessionfinish(session, exitstatus)
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


def test_failure_reports_locations_without_source_or_values(reporting):
    private = "synthetic-private-payload"
    entries = [
        SimpleNamespace(reprfileloc=SimpleNamespace(path=path, lineno=line, message=private))
        for path, line in [("cases/test_native_dsh.py", 1237), ("environment.py", 39)]
    ]
    reporting.pytest_runtest_logreport(
        SimpleNamespace(
            nodeid="case.one",
            when="call",
            passed=False,
            failed=True,
            skipped=False,
            longrepr=SimpleNamespace(reprtraceback=SimpleNamespace(reprentries=entries)),
            longreprtext=private,
            capstdout=private,
            capstderr=private,
        )
    )
    diagnostic = reporting._RESULTS["case.one"]["diagnostics"][0]["message"]
    assert "test_native_dsh.py:1237" in diagnostic
    assert "environment.py:39" in diagnostic
    assert private not in diagnostic


def test_failure_locations_are_bounded_and_filter_unexpected_paths(reporting):
    entries = [
        SimpleNamespace(reprfileloc=SimpleNamespace(path=path, lineno=line))
        for path, line in [("private-payload", 1), ("test_case.py", "private")]
        + [("test_case.py", n) for n in range(1, 21)]
    ]
    diagnostic = reporting.failure_message(
        SimpleNamespace(
            longrepr=SimpleNamespace(reprtraceback=SimpleNamespace(reprentries=entries)),
        )
    )
    assert "private" not in diagnostic
    assert diagnostic.count("test_case.py:") == 8
    assert "test_case.py:20" in diagnostic


def test_missing_teardown_is_not_success(reporting, tmp_path):
    phase(reporting, "setup")
    phase(reporting, "call")
    session, document = finish(reporting, tmp_path)
    assert session.exitstatus == 1
    assert [c["status"] for c in document["scope"]["tests"]] == ["interrupted"]
    assert document["scope"]["completeness"] == "incomplete"


def test_collection_does_not_claim_execution(reporting, tmp_path):
    session, document = finish(reporting, tmp_path, collect=True)
    assert session.exitstatus == 0
    assert [c["status"] for c in document["scope"]["tests"]] == ["not_run"]
    assert document["scope"]["completeness"] == "complete"


def test_interrupted_session_is_incomplete_even_after_last_case(reporting, tmp_path):
    for when in ("setup", "call", "teardown"):
        phase(reporting, when)
    original = reporting.pytest_sessionfinish

    def interrupted(session, exitstatus):
        session.exitstatus = 2
        return original(session, 2)

    reporting.pytest_sessionfinish = interrupted
    session, document = finish(reporting, tmp_path)
    assert session.exitstatus == 2
    assert document["scope"]["tests"][0]["status"] == "passed"
    assert document["scope"]["completeness"] == "incomplete"


def test_unexecuted_case_makes_execution_incomplete(reporting, tmp_path):
    session, document = finish(reporting, tmp_path)
    assert session.exitstatus == 1
    assert document["scope"]["tests"][0]["status"] == "not_run"
    assert document["scope"]["completeness"] == "incomplete"


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


def test_native_group_survives_export(reporting, tmp_path):
    for when in ("setup", "call", "teardown"):
        phase(reporting, when)
    _, document = finish(reporting, tmp_path, groups=("native-dsh", "description"))
    assert document["scope"]["tests"][0]["groups"] == ["native-dsh"]


@pytest.mark.parametrize("collect", [False, True])
def test_native_selection_excludes_unmatched_bodies(tmp_path, collect):
    import os
    import subprocess
    import sys

    directory = Path(__file__).resolve().parents[3] / "tests/integration"
    (tmp_path / "conftest.py").write_text(
        (directory / "conftest.py").read_text()
        + '\n@pytest.fixture(scope="session", autouse=True)\ndef container_only():\n    pass\n'
    )
    (tmp_path / "test_selection.py").write_text(
        "from pathlib import Path\nimport pytest\n"
        '@pytest.mark.__getattr__("native-dsh")\n'
        "def test_native_selected():\n"
        '    """When selected, the native case executes its own body."""\n'
        '    Path("executed").touch()\n'
        '@pytest.mark.__getattr__("native-dsh")\n'
        "def test_native_excluded():\n"
        '    """When excluded by a name filter, the native body never runs."""\n'
        '    raise AssertionError("excluded body ran")\n'
        "@pytest.mark.chain\n"
        "def test_other_selected():\n"
        '    """When excluded by group, the chain body never runs."""\n'
        '    raise AssertionError("other group ran")\n'
    )
    result = subprocess.run(
        [
            sys.executable,
            "-m",
            "pytest",
            "-q",
            "--group",
            "native-dsh",
            "--only",
            "selected",
            *(["--collect-only"] if collect else []),
        ],
        cwd=tmp_path,
        env={
            **os.environ,
            "PYTHONPATH": os.pathsep.join(
                [str(directory), str(directory.parents[1] / "tools/repo/src")]
            ),
            "WHALEREIN_INTEGRATION_CONTAINER": "0",
        },
        capture_output=True,
        text=True,
        timeout=20,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    assert (tmp_path / "executed").exists() is not collect
    if collect:
        assert "2 deselected" in result.stdout
    else:
        assert '"total": 1' in result.stdout


def test_empty_shard_exports_a_complete_empty_scope(reporting, tmp_path):
    session, document = finish(reporting, tmp_path, shard="1/2", items=[], exitstatus=5)
    assert session.exitstatus == 0
    scope = document["scope"]
    assert scope["tests"] == []
    assert scope["completeness"] == "complete"
    assert scope["exit_code"] == 0
    assert scope["shard"] == "1/2"
    assert scope["selection"] == "full"


def test_sharded_group_selection_remains_partial(reporting, tmp_path):
    for when in ("setup", "call", "teardown"):
        phase(reporting, when)
    _, document = finish(reporting, tmp_path, shard="0/2", group="chain")
    assert document["scope"]["selection"] == "partial"


def test_nonsharded_deselection_keeps_the_existing_selection_semantics(reporting, tmp_path):
    for when in ("setup", "call", "teardown"):
        phase(reporting, when)
    reporting._DESELECTED = True
    _, document = finish(reporting, tmp_path)
    assert document["scope"]["selection"] == "partial"


def test_shard_collection_partitions_every_case_exactly_once(tmp_path):
    import os
    import subprocess
    import sys

    directory = Path(__file__).resolve().parents[3] / "tests/integration"
    (tmp_path / "conftest.py").write_text(
        (directory / "conftest.py").read_text()
        + '\n@pytest.fixture(scope="session", autouse=True)\ndef container_only():\n    pass\n'
    )
    (tmp_path / "test_shard.py").write_text(
        "from pathlib import Path\n"
        "import pytest\n"
        "@pytest.mark.chain\n"
        "def test_one():\n"
        '    """When sharded, the first case runs in exactly one shard."""\n'
        '    Path("one").touch()\n'
        "@pytest.mark.chain\n"
        "def test_two():\n"
        '    """When sharded, the second case runs in exactly one shard."""\n'
        '    Path("two").touch()\n'
        "@pytest.mark.chain\n"
        "def test_three():\n"
        '    """When sharded, the third case runs in exactly one shard."""\n'
        '    Path("three").touch()\n'
        "@pytest.mark.chain\n"
        "def test_four():\n"
        '    """When sharded, the fourth case runs in exactly one shard."""\n'
        '    Path("four").touch()\n'
    )
    environment = {
        **os.environ,
        "PYTHONPATH": os.pathsep.join(
            [str(directory), str(directory.parents[1] / "tools/repo/src")]
        ),
        "NO_COLOR": "1",
        "WHALEREIN_INTEGRATION_CONTAINER": "0",
    }
    for shard in ("0/2", "1/2"):
        result = subprocess.run(
            [sys.executable, "-m", "pytest", "-q", "--shard", shard, str(tmp_path)],
            cwd=tmp_path,
            env=environment,
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert result.returncode in (0, 5), result.stdout + result.stderr
        assert "No integration cases match" not in result.stderr
    assert all((tmp_path / name).exists() for name in ("one", "two", "three", "four"))


def test_invalid_shard_option_fails_collection(tmp_path):
    import os
    import subprocess
    import sys

    directory = Path(__file__).resolve().parents[3] / "tests/integration"
    (tmp_path / "conftest.py").write_text(
        (directory / "conftest.py").read_text()
        + '\n@pytest.fixture(scope="session", autouse=True)\ndef container_only():\n    pass\n'
    )
    (tmp_path / "test_one.py").write_text(
        "def test_one():\n"
        '    """When selected, one case exists for shard validation."""\n'
        "    pass\n"
    )
    environment = {
        **os.environ,
        "PYTHONPATH": os.pathsep.join(
            [str(directory), str(directory.parents[1] / "tools/repo/src")]
        ),
        "WHALEREIN_INTEGRATION_CONTAINER": "0",
    }
    result = subprocess.run(
        [sys.executable, "-m", "pytest", "-q", "--shard", "2/2", str(tmp_path)],
        cwd=tmp_path,
        env=environment,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 4, result.stdout + result.stderr
    assert "--shard INDEX" in result.stdout + result.stderr
