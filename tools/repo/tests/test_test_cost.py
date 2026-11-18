"""Cost notices inform selection without changing execution or success."""

import io
import json
import os
from dataclasses import asdict
from pathlib import Path

import pytest
from conftest import report_facts

from repo import cli, test_cost, testing
from repo.output import Output
from repo.runners import rust


@pytest.fixture
def sample_request():
    return testing.TestRequest(projects=("core",), only="regression")


@pytest.fixture
def registered_tree(repo_tree):
    (repo_tree / "tools/repo/projects.toml").write_text(
        '[[projects]]\nid="core"\npath="core"\nkind="rust"\n'
    )
    return repo_tree


def save(root, sample_request, duration=1000, change=None, name="previous"):
    scope = {
        "suite_id": "unit:core",
        "project": "core",
        "completeness": "complete",
        "tests": [{"id": "case", "status": "passed"}],
        "exit_code": 0,
    }
    report = {
        "scopes": [scope],
        "configuration": asdict(sample_request),
        "duration_ms": duration,
        "duration_complete": True,
        "timing_environment": test_cost.environment(),
    }
    if change:
        change(report)
    report_facts(report)
    path = root / ".data/runs" / name / "report.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(report))
    return path


def test_missing_history_is_unknown(tmp_path, sample_request):
    assert test_cost.historical_duration(tmp_path, sample_request) == {
        "samples": 0,
        "median_ms": None,
    }


def test_median_uses_complete_matching_run_wall_time(tmp_path, sample_request):
    for index, duration in enumerate((1000, 3000, 9000)):
        save(tmp_path, sample_request, duration, name=str(index))
    assert test_cost.historical_duration(tmp_path, sample_request) == {
        "samples": 3,
        "median_ms": 3000,
    }


@pytest.mark.parametrize(
    "field,value",
    [
        ("suite", "integration"),
        ("projects", ["cli"]),
        ("only", "different"),
        ("group", "protocol"),
        ("coverage", True),
        ("list_only", True),
        ("parallel", 2),
    ],
)
def test_different_selection_is_not_a_comparable_sample(tmp_path, sample_request, field, value):
    save(tmp_path, sample_request, change=lambda r: r["configuration"].update({field: value}))
    assert test_cost.historical_duration(tmp_path, sample_request)["samples"] == 0


@pytest.mark.parametrize(
    "change",
    [
        lambda r: r.pop("timing_environment"),
        lambda r: r.update(timing_environment={"platform": "different", "machine": "different"}),
        lambda r: r.update(duration_complete=False),
        lambda r: r.pop("duration_ms"),
        lambda r: r.update(duration_ms=0),
        lambda r: r.update(duration_ms=True),
        lambda r: r.update(duration_ms=float("nan")),
        lambda r: r["scopes"][0].update(exit_code=1),
        lambda r: r["scopes"][0].update(errors=["failed cleanup"]),
        lambda r: r["scopes"][0].update(completeness="incomplete"),
        lambda r: r["scopes"][0].update(tests=[]),
        lambda r: r["scopes"][0]["tests"][0].update(status="failed"),
        lambda r: r["scopes"][0]["tests"][0].update(status="skipped"),
        lambda r: r["scopes"][0].update(coverage=[{"percent": 89, "ok": False}]),
        lambda r: r["configuration"].update(projects=None),
    ],
)
def test_partial_failed_or_unknown_measurement_is_ignored(tmp_path, sample_request, change):
    save(tmp_path, sample_request, change=change)
    assert test_cost.historical_duration(tmp_path, sample_request)["median_ms"] is None


def test_corrupt_old_and_oversized_reports_do_not_block_history(
    tmp_path, sample_request, monkeypatch
):
    bad = save(tmp_path, sample_request, name="bad")
    bad.write_text("private invalid json")
    old = save(tmp_path, sample_request, name="old")
    raw = json.loads(old.read_text())
    raw["schema_version"] = 1
    old.write_text(json.dumps(raw))
    big = save(tmp_path, sample_request, name="big")
    big.write_text(" " * 2000)
    monkeypatch.setattr(test_cost, "MAX_REPORT_BYTES", 1000)
    save(tmp_path, sample_request, 1234)
    assert test_cost.historical_duration(tmp_path, sample_request) == {
        "samples": 1,
        "median_ms": 1234,
    }


def test_scan_and_sample_counts_are_bounded(tmp_path, sample_request, monkeypatch):
    for index in range(5):
        path = save(tmp_path, sample_request, (index + 1) * 1000, name=str(index))
        os.utime(path, (index + 1, index + 1))
    monkeypatch.setattr(test_cost, "HISTORY_LIMIT", 3)
    monkeypatch.setattr(test_cost, "SAMPLE_LIMIT", 2)
    assert test_cost.historical_duration(tmp_path, sample_request) == {
        "samples": 2,
        "median_ms": 4500,
    }


def test_history_read_failure_is_advisory(tmp_path, sample_request, monkeypatch):
    def denied(*args, **kwargs):
        raise PermissionError("private path")

    monkeypatch.setattr(Path, "glob", denied)
    assert test_cost.historical_duration(tmp_path, sample_request)["median_ms"] is None


def test_project_order_and_default_parallelism_do_not_change_selection(tmp_path):
    sample_request = testing.TestRequest(projects=("core", "cli"))
    save(
        tmp_path,
        sample_request,
        change=lambda r: r["configuration"].update(
            projects=["cli", "core"],
            parallel=1,
        ),
    )
    assert test_cost.historical_duration(tmp_path, sample_request)["samples"] == 1


@pytest.mark.parametrize(
    "suite,group,expected",
    [
        ("unit", None, "unit projects: core"),
        ("integration", "protocol-vectors", "target: protocol-vectors"),
        ("integration", None, "target: integration"),
    ],
)
def test_notice_describes_scope_without_printing_filter(tmp_path, suite, group, expected):
    sample_request = testing.TestRequest(
        suite=suite, projects=("core",), group=group, only="private-filter"
    )
    stdout, stderr = io.StringIO(), io.StringIO()
    out = Output(stdout=stdout, stderr=stderr)
    test_cost.announce(tmp_path, sample_request, out)
    text = stdout.getvalue()
    assert expected in text
    assert "case filter=on; coverage=off" in text
    assert "unknown" in text
    assert "private-filter" not in text + stderr.getvalue()


@pytest.mark.parametrize("duration,warn", [(180_000, False), (180_001, True)])
def test_expensive_history_only_warns(tmp_path, sample_request, duration, warn):
    save(tmp_path, sample_request, duration)
    stdout, stderr = io.StringIO(), io.StringIO()
    history = test_cost.announce(tmp_path, sample_request, Output(stdout=stdout, stderr=stderr))
    assert history["median_ms"] == duration
    assert ("warning:" in stderr.getvalue()) == warn
    assert "compilation/cache state may differ" in stdout.getvalue()


def test_notice_flushes_piped_output_before_native_work(tmp_path, sample_request):
    class Stream(io.StringIO):
        flushed = False

        def flush(self):
            self.flushed = True
            super().flush()

    stdout, stderr = Stream(), Stream()
    test_cost.announce(tmp_path, sample_request, Output(stdout=stdout, stderr=stderr))
    assert stdout.flushed and stderr.flushed


def test_cli_prints_notice_before_execution_and_preserves_json(
    registered_tree, monkeypatch, capsys
):
    monkeypatch.setenv("WHALEREIN_ROOT", str(registered_tree))
    observed = []

    def collect(*args, **kwargs):
        captured = capsys.readouterr()
        assert captured.out == ""
        assert "Test selection:" in captured.err and "coverage=off" in captured.err
        observed.append(True)
        return {
            "project": "core",
            "completeness": "complete",
            "exit_code": 0,
            "tests": [{"id": "regression", "status": "passed"}],
        }

    monkeypatch.setattr(rust, "collect", collect)
    args = ["--json", "test", "core", "--only", "regression"]
    assert cli.main(args) == 0
    result = json.loads(capsys.readouterr().out)
    assert result["ok"] and result["data"]["historical_duration"]["samples"] == 0
    # A matching successful history informs the second run; it never suppresses execution.
    assert cli.main(args) == 0
    result = json.loads(capsys.readouterr().out)
    assert result["data"]["historical_duration"]["samples"] == 1
    assert observed == [True, True]


def test_actual_slow_execution_warns_but_remains_successful(registered_tree, monkeypatch, out):
    from conftest import FakeRunner

    from repo.projects import load_projects

    runner = FakeRunner()
    runner.out = out
    monkeypatch.setattr(
        rust,
        "collect",
        lambda *a, **k: {
            "project": "core",
            "completeness": "complete",
            "exit_code": 0,
            "tests": [{"id": "regression", "status": "passed"}],
        },
    )
    monkeypatch.setattr(test_cost, "QUICK_TARGET_MS", -1)
    data, code = testing.run(
        registered_tree,
        load_projects(registered_tree)[:1],
        runner,
        testing.TestRequest(projects=("core",)),
    )
    assert code == 0
    assert "review scope before repeating" in out.stderr.getvalue()
    assert (
        json.loads(Path(data["json"]).read_text())["timing_environment"] == test_cost.environment()
    )
