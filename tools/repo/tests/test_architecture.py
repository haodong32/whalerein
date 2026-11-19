"""Public contracts for the hard-cut repository task interface."""

import json
from dataclasses import FrozenInstanceError
from pathlib import Path
from types import SimpleNamespace

import pytest
from conftest import FakeRunner, ok, report_facts

from repo import cli, testing, toolchain
from repo.errors import RepoError, UsageError
from repo.projects import Project, load_projects, select_projects
from repo.reports.model import atomic_write, read_report, validate
from repo.runners import rust


@pytest.fixture
def registry(repo_tree):
    (repo_tree / "tools/repo/projects.toml").write_text("""
[[projects]]
id = "core"
path = "core"
kind = "rust"
[[projects]]
id = "access"
path = "access"
kind = "go"
[[projects]]
id = "dsh"
path = "host/dsh"
kind = "node"
[[projects]]
id = "repo"
path = "tools/repo"
kind = "python"
coverage_source = "repo"
""")
    return repo_tree


def scope(status="passed", code=0):
    return {
        "project": "sample",
        "suite_id": "unit:sample",
        "completeness": "complete",
        "tests": [{"id": "native::chosen", "status": status}],
        "coverage": [],
        "exit_code": code,
        "errors": [],
    }


def test_registry_exact_ids_and_capabilities(registry):
    projects = load_projects(registry)
    assert [p.id for p in select_projects(projects, ["core", "repo"])] == ["core", "repo"]
    assert select_projects(projects, None) == projects
    with pytest.raises(RepoError):
        select_projects(projects, ["tools/repo"])
    assert projects[-1].as_dict()["operations"] == ["fmt", "check", "test"]


@pytest.mark.parametrize(
    "change",
    [
        'id = "core"',
        'kind = "unknown"',
        'path = "../escape"',
        'role = "wrong"',
        'sources = ["missing"]',
    ],
)
def test_registry_rejects_invalid_entries(registry, change):
    path = registry / "tools/repo/projects.toml"
    original = path.read_text()
    if change.startswith("id"):
        original = original.replace('id = "access"', change)
    elif change.startswith("kind"):
        original = original.replace('kind = "go"', change)
    elif change.startswith("path"):
        original = original.replace('path = "access"', change)
    else:
        original = original.replace('id = "access"', 'id = "access"\n' + change)
    path.write_text(original)
    with pytest.raises(RepoError):
        load_projects(registry)


@pytest.mark.parametrize(
    "arguments",
    [
        ["list"],
        ["lint"],
        ["dev", "up"],
        ["env", "up"],
        ["hooks", "install"],
        ["commit-msg", "message"],
        ["test", "unit"],
        ["test", "all"],
        ["test", "--suite", "integration"],
        ["test", "protocol", "--group", "protocol"],
        ["check", "--project", "core"],
        ["doctor", "--for", "integration"],
        ["proto", "--check"],
        ["report", "report.json", "--output", "report.html"],
        ["--dry-run", "build"],
        ["fmt", "--check"],
        ["test", "--baseline-report", "old.json"],
    ],
)
def test_removed_surface_returns_one_json_error(arguments, capsys):
    assert cli.main(["--json", *arguments]) == 2
    payload = json.loads(capsys.readouterr().out)
    assert payload["ok"] is False
    assert payload["error"]["code"] == "usage_error"


def test_json_projects_and_errors(registry, monkeypatch, capsys):
    monkeypatch.setenv("WHALEREIN_ROOT", str(registry))
    assert cli.main(["projects", "--json"]) == 0
    assert len(json.loads(capsys.readouterr().out)["data"]["projects"]) == 4
    assert cli.main(["--json", "build", "missing"]) == 2
    assert json.loads(capsys.readouterr().out)["error"]["code"] == "usage_error"
    monkeypatch.setattr(cli, "dispatch", lambda *a: (_ for _ in ()).throw(RuntimeError("private")))
    assert cli.main(["--json", "projects"]) == 1
    assert json.loads(capsys.readouterr().out)["error"]["message"] == "internal error"


@pytest.mark.parametrize(
    "test_request",
    [
        testing.TestRequest(suite="bad"),
        testing.TestRequest(suite="integration", projects=("core",)),
        testing.TestRequest(group="protocol"),
        testing.TestRequest(integration_timeout=5),
        testing.TestRequest(suite="integration", integration_timeout=0),
        testing.TestRequest(suite="integration", integration_timeout=-1),
        testing.TestRequest(coverage=True, only="case"),
        testing.TestRequest(coverage=True, list_only=True),
        testing.TestRequest(coverage=True, suite="integration"),
        testing.TestRequest(only=""),
        testing.TestRequest(only="case", suite="all"),
        testing.TestRequest(only="case"),
    ],
)
def test_invalid_test_requests_fail_before_execution(test_request):
    with pytest.raises(UsageError):
        test_request.validate()


def test_request_is_immutable():
    with pytest.raises(FrozenInstanceError):
        testing.TestRequest().suite = "all"


@pytest.mark.parametrize(
    "status,code,expected",
    [
        ("passed", 0, 0),
        ("failed", 1, 1),
        ("not_run", 0, 1),
        ("interrupted", 130, 130),
    ],
)
def test_execution_persists_facts_and_json_envelope(
    registry, monkeypatch, capsys, status, code, expected
):
    monkeypatch.setenv("WHALEREIN_ROOT", str(registry))
    monkeypatch.setattr(rust, "collect", lambda *a, **kw: scope(status, code))
    destination = registry / "result"
    assert cli.main(["--json", "test", "core", "--output", str(destination)]) == expected
    result = json.loads(capsys.readouterr().out)
    assert result["ok"] == (expected == 0)
    document = read_report(destination / "report.json")
    assert document["summary"]["cases"] == 1
    assert document["scopes"][0]["tests"][0]["id"] == "core::native::chosen"
    assert (destination / "report.html").exists()
    assert cli.main(["--json", "test", "core", "--output", str(destination)]) == 2
    capsys.readouterr()


def test_list_and_offline_render_share_facts(registry, monkeypatch, capsys):
    monkeypatch.setenv("WHALEREIN_ROOT", str(registry))
    monkeypatch.setattr(rust, "collect", lambda *a, **k: scope("not_run"))
    assert cli.main(["--json", "test", "core", "--list", "--output", str(registry / "listed")]) == 0
    payload = json.loads(capsys.readouterr().out)
    assert (
        cli.main(
            [
                "--json",
                "report",
                str(Path(payload["data"]["json"]).parent),
            ]
        )
        == 0
    )
    assert (
        Path(json.loads(capsys.readouterr().out)["data"]["html"])
        == registry / "listed" / "report.html"
    )


@pytest.mark.parametrize("failure", [RepoError("native failed"), KeyboardInterrupt()])
def test_execution_failure_preserves_partial_report(registry, monkeypatch, out, failure):
    project = load_projects(registry)[0]

    def fail(*a, **k):
        raise failure

    monkeypatch.setattr(rust, "collect", fail)
    data, code = testing.run(
        registry,
        [project],
        SimpleNamespace(out=out),
        testing.TestRequest(projects=("core",)),
        registry / "failed",
    )
    document = read_report(Path(data["json"]))
    assert code != 0
    assert document["completeness"] == "incomplete"
    assert document["scopes"][0]["errors"]


def test_coverage_failure_does_not_erase_cases(registry, monkeypatch, out):
    from repo.runners.coverage import CoverageResult

    project = load_projects(registry)[0]
    monkeypatch.setattr(rust, "collect", lambda *a, **k: scope())
    monkeypatch.setattr(
        rust, "measure", lambda *a: CoverageResult("core", "rust", "core", percent=89)
    )
    data, code = testing.run(
        registry,
        [project],
        SimpleNamespace(out=out),
        testing.TestRequest(projects=("core",), coverage=True),
        registry / "covered",
    )
    assert code == 1
    document = read_report(Path(data["json"]))
    assert document["summary"]["statuses"] == {"passed": 1}
    assert document["scopes"][0]["coverage"][0]["phase"] == "separate_measurement"


def test_unsupported_listing_and_empty_selection_fail(registry, monkeypatch, out):
    projects = load_projects(registry)
    with pytest.raises(UsageError):
        testing.run(
            registry, projects, SimpleNamespace(out=out), testing.TestRequest(list_only=True)
        )
    monkeypatch.setattr(rust, "collect", lambda *a, **k: {**scope(), "tests": []})
    data, code = testing.run(
        registry,
        projects[:1],
        SimpleNamespace(out=out),
        testing.TestRequest(only="missing"),
        registry / "empty",
    )
    assert code == 2
    assert "No test cases" in read_report(Path(data["json"]))["scopes"][0]["errors"][0]


def test_toolchain_preflights_and_limits_rust_scope(registry, out):
    projects = load_projects(registry)
    commands = rust.steps(projects[0], "fmt")
    assert "--all" not in commands[0].argv
    assert ("--package", "whalerein-core") == commands[0].argv[-2:]
    runner = FakeRunner()
    runner.out = out
    with pytest.raises(RepoError):
        toolchain.run([projects[-1]], runner, "build", True)
    assert runner.calls == []
    data, code = toolchain.run(projects, runner, "build")
    assert code == 0
    assert {s["project"] for s in data["not_applicable"]} == {"dsh", "repo"}
    (projects[1].path / "main.go").write_text("package main")
    runner.responses = [ok("difference.go"), ok()]
    _, code = toolchain.run([projects[1]], runner, "check")
    assert code == 1


@pytest.mark.parametrize(
    "mutate",
    [
        lambda d: d.update(schema_version=2),
        lambda d: d["scopes"][0].update(completeness="bogus"),
        lambda d: d["scopes"][0]["tests"].append(dict(d["scopes"][0]["tests"][0])),
        lambda d: d["scopes"].append(dict(d["scopes"][0])),
        lambda d: d["summary"].update(cases=99),
        lambda d: d.update(duration_ms=float("nan")),
        lambda d: d["scopes"][0]["tests"][0].update(status="bogus"),
        lambda d: d["scopes"][0]["tests"][0].update(diagnostics=[{}]),
        lambda d: d["scopes"][0].update(coverage=[{"name": "x", "percent": 101}]),
    ],
)
def test_report_rejects_invalid_facts(mutate):
    data = report_facts({"scopes": [scope()]})
    mutate(data)
    with pytest.raises(RepoError):
        validate(data)


def test_atomic_report_does_not_replace_good_file_on_failure(tmp_path, monkeypatch):
    target = tmp_path / "report.json"
    atomic_write(target, "old")
    monkeypatch.setattr(
        "repo.reports.model.os.replace", lambda *a: (_ for _ in ()).throw(OSError("disk failure"))
    )
    with pytest.raises(OSError):
        atomic_write(target, "new")
    assert target.read_text() == "old"
    assert list(tmp_path.iterdir()) == [target]


@pytest.mark.parametrize("kind", ["go", "python", "node"])
@pytest.mark.parametrize("mode", ["success", "missing", "failed"])
def test_combined_coverage_has_one_native_execution(tmp_path, out, kind, mode):
    raw_profile = "mode: set\nsample/pkg/a.go:1.1,1.2 1 1\n"
    manifest = tmp_path / (
        "package.json" if kind == "node" else "pyproject.toml" if kind == "python" else "go.mod"
    )
    manifest.write_text(
        json.dumps(
            {
                "scripts": {
                    "test": "node --test --experimental-test-coverage --test-coverage-lines=90"
                }
            }
        )
        if kind == "node"
        else ""
    )
    if kind == "node":
        (tmp_path / "test").mkdir()
        (tmp_path / "test/one.test.js").write_text("")
    project = Project(
        "sample", kind, tmp_path, manifest, tmp_path, "sample", coverage_source="sample"
    )

    class Native(FakeRunner):
        def run(self, argv, cwd, **kwargs):
            self.calls.append((tuple(argv), str(cwd)))
            code = 1 if mode == "failed" else 0
            if kind == "python":
                Path(kwargs["env"]["WHALEREIN_TEST_REPORT"]).write_text(
                    json.dumps(
                        {
                            "tests": [{"id": "one", "status": "passed"}],
                            "collected": 1,
                            "collection_errors": 0,
                        }
                    )
                )
                flag = next(a for a in argv if a.startswith("--cov-report="))
                if mode != "missing":
                    Path(flag.split("json:", 1)[1]).write_text(
                        json.dumps(
                            {
                                "totals": {
                                    "percent_covered": 95,
                                    "covered_lines": 19,
                                    "num_statements": 20,
                                }
                            }
                        )
                    )
                return ok(returncode=code)
            if kind == "go":
                flag = next(a for a in argv if a.startswith("-coverprofile="))
                if mode != "missing":
                    Path(flag.split("=", 1)[1]).write_text(raw_profile)
                return ok(
                    "\n".join(
                        json.dumps(e)
                        for e in [
                            {"Package": "sample/pkg", "Action": "run", "Test": "TestOne"},
                            {"Package": "sample/pkg", "Action": "pass", "Test": "TestOne"},
                            {"Package": "sample/pkg", "Action": "pass"},
                        ]
                    ),
                    returncode=code,
                )
            events = [
                {"file": "test/one.test.js", "name": "one", "status": "passed"},
                {"summary": True, "counts": {"tests": 1, "passed": 1}},
            ]
            if mode != "missing":
                events.append(
                    {
                        "coverage": {
                            "coveredLinePercent": 95,
                            "coveredLineCount": 19,
                            "totalLineCount": 20,
                        }
                    }
                )
            return ok("\n".join(json.dumps(e) for e in events), returncode=code)

    runner = Native()
    runner.out = out
    if kind == "go":
        output = tmp_path / "report"
        data, exit_code = testing.run(
            tmp_path, [project], runner, testing.TestRequest(coverage=True), output
        )
        result = read_report(Path(data["json"]))["scopes"][0]
        profile = output / "coverage-sample.coverprofile"
        assert exit_code == (mode != "success")
        if mode == "missing":
            assert not profile.exists()
        else:
            assert profile.read_text() == raw_profile
    else:
        result = toolchain.RUNNERS[kind].collect(project, runner, True, coverage=True)
    assert len(runner.calls) == 1
    assert result["coverage"][0]["ok"] == (mode == "success")
    assert result["tests"][0]["status"] == "passed"
    assert result["coverage"][0].get("phase", "test_execution") == "test_execution"


def test_rust_coverage_uses_an_empty_build_directory(tmp_path):
    project = Project("sample", "rust", tmp_path, tmp_path / "Cargo.toml", tmp_path, "sample")
    targets = []

    class CoverageRunner(FakeRunner):
        def run(self, argv, cwd, *, env=None, **kwargs):
            if argv[-1] != "--version":
                target = Path(env["CARGO_LLVM_COV_TARGET_DIR"])
                assert env["CARGO_LLVM_COV_BUILD_DIR"] == str(target)
                assert list(target.iterdir()) == []
                (target / "instrumented-binary").touch()
                targets.append(target)
            return super().run(argv, cwd, env=env, **kwargs)

    runner = CoverageRunner(default_returncode=0, default_stdout='{"data":[{"totals":{}}]}')
    for _ in range(2):
        assert rust.measure(project, runner).ok
    first, second = targets
    assert first != second
    assert not first.exists()
    assert not second.exists()


@pytest.mark.parametrize(
    "probe,output,code",
    [
        (127, "", 1),
        (1, "", 1),
        (0, "invalid", 0),
        (
            0,
            json.dumps(
                {"data": [{"totals": {"lines": {"covered": 9, "count": 10, "percent": 90}}}]}
            ),
            0,
        ),
        (
            0,
            json.dumps(
                {"data": [{"totals": {"lines": {"covered": 9, "count": 10, "percent": 90}}}]}
            ),
            1,
        ),
    ],
)
def test_rust_coverage_requires_native_success(tmp_path, probe, output, code):
    project = Project("sample", "rust", tmp_path, tmp_path / "Cargo.toml", tmp_path, "sample")
    runner = FakeRunner(responses=[ok(returncode=probe), ok(output, returncode=code)])
    if probe == 127:
        from conftest import missing

        runner.responses[0] = missing()
    measured = rust.measure(project, runner)
    assert measured.ok == (probe == 0 and code == 0 and output != "invalid")
    if probe == 0:
        command = runner.calls[1][0]
        assert command[:4] == ("cargo", "llvm-cov", "--package", "sample")
        assert "--no-clean" not in command
        assert command[-2:] == ("--", "--test-threads=1")


def test_coverage_exception_retains_successful_cases(registry, monkeypatch, out):
    project = load_projects(registry)[0]
    monkeypatch.setattr(rust, "collect", lambda *a, **k: scope())

    def fail(*a):
        raise RepoError("measurement missing")

    monkeypatch.setattr(rust, "measure", fail)
    data, code = testing.run(
        registry,
        [project],
        SimpleNamespace(out=out),
        testing.TestRequest(coverage=True),
        registry / "missing-cover",
    )
    assert code == 1
    assert read_report(Path(data["json"]))["summary"]["statuses"] == {"passed": 1}


def test_internal_hook_and_staged_reader(tmp_path, monkeypatch, capsys):
    from repo import commits

    monkeypatch.setenv("WHALEREIN_ROOT", str(tmp_path))
    message = tmp_path / "message"
    message.write_text("feat(core): retain the cursor\n")
    monkeypatch.setattr(commits, "read_staged", lambda root: ([], ""))
    assert commits.main([str(message)]) == 0
    message.write_text("bad")
    assert commits.main([str(message)]) == 1
    assert commits.main([]) == 1
    assert commits.main([str(tmp_path / "missing")]) == 1
    monkeypatch.undo()
    import subprocess

    subprocess.run(["git", "init", "-q", str(tmp_path)], check=True)
    assert commits.read_staged(tmp_path) == ([], "")
    assert commits._git(tmp_path, ["status", "--porcelain"])
    with pytest.raises(RepoError):
        commits._git(tmp_path, ["not-a-command"])


def test_doctor_and_proto_dispatch(registry, monkeypatch, capsys):
    from repo import doctor, proto

    monkeypatch.setenv("WHALEREIN_ROOT", str(registry))
    monkeypatch.setattr(
        doctor,
        "run_doctor",
        lambda *a: {
            "ok": True,
            "tools": [],
            "missing_required": [],
            "outdated": [],
            "missing_optional": [],
        },
    )
    assert cli.main(["--json", "doctor", "integration"]) == 0
    assert json.loads(capsys.readouterr().out)["ok"]
    assert any(s.version_args == ("buildx", "version") for s in doctor.specs_for("integration"))
    assert any(s.version_args == ("llvm-cov", "--version") for s in doctor.specs_for("coverage"))
    monkeypatch.setattr(proto, "run", lambda *a, **k: {"checked": k["check"]})
    assert cli.main(["--json", "check", "proto"]) == 0
    assert json.loads(capsys.readouterr().out)["data"]["checked"]
    monkeypatch.setattr(cli, "dispatch", lambda *a: (_ for _ in ()).throw(KeyboardInterrupt()))
    assert cli.main(["--json", "projects"]) == 130
    assert json.loads(capsys.readouterr().out)["ok"] is False


def test_pytest_phase_export_retains_failures_and_interruption(monkeypatch, tmp_path):
    import importlib.util

    import repo.pytest_report

    spec = importlib.util.spec_from_file_location(
        "isolated_pytest_report", repo.pytest_report.__file__
    )
    plugin = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(plugin)
    from repo.test_timing import finish_interrupted, record_phase

    config = SimpleNamespace(addinivalue_line=lambda *a: None)
    plugin.pytest_configure(config)
    item = SimpleNamespace(
        nodeid="test::one",
        location=("test.py", 0),
        obj=lambda: None,
        get_closest_marker=lambda name: None,
    )
    session = SimpleNamespace(items=[item], testscollected=1)
    plugin.pytest_collection_finish(session)
    plugin.pytest_runtest_logstart("test::one", None)
    plugin.pytest_runtest_logreport(
        SimpleNamespace(
            nodeid="test::one", when="setup", duration=0.01, failed=False, skipped=False
        )
    )
    plugin.pytest_runtest_logreport(
        SimpleNamespace(
            nodeid="test::one",
            when="call",
            duration=0.01,
            failed=True,
            skipped=False,
            longreprtext="safe failure",
        )
    )
    destination = tmp_path / "export.json"
    monkeypatch.setenv("WHALEREIN_TEST_REPORT", str(destination))
    plugin.pytest_sessionfinish(session, 1)
    case = json.loads(destination.read_text())["tests"][0]
    assert case["status"] == "failed" and case["duration_complete"] is False
    assert case["diagnostics"][0]["message"] == "safe failure"
    case = {"status": "passed"}
    record_phase(case, SimpleNamespace(when="teardown", duration=0.1))
    assert case["duration_complete"]
    finish_interrupted({"one": case}, {"one": 1}, 2, set())
    assert case["status"] == "interrupted"


def test_listed_project_qualified_id_can_be_selected(registry, monkeypatch, out):
    project = load_projects(registry)[0]

    def selected(p, runner, execute, only):
        assert only == "native::chosen"
        return scope()

    monkeypatch.setattr(rust, "collect", selected)
    data, code = testing.run(
        registry,
        [project],
        SimpleNamespace(out=out),
        testing.TestRequest(projects=("core",), only="core::native::chosen"),
        registry / "qualified",
    )
    assert code == 0
    assert read_report(Path(data["json"]))["summary"]["cases"] == 1


@pytest.mark.parametrize(
    "field,value", [("module", []), ("note", {}), ("exit_code", "bad"), ("checks", [{}])]
)
def test_report_rejects_invalid_scope_presentation(field, value):
    data = report_facts({"scopes": [scope()]})
    data["scopes"][0][field] = value
    with pytest.raises(RepoError):
        validate(data)


def test_coverage_facts_keep_precision_near_gate():
    from repo.runners.coverage import CoverageResult

    measured = CoverageResult("sample", "node", ".", percent=89.99999)
    data = report_facts({"scopes": [scope()]})
    data["scopes"][0]["coverage"] = [{**measured.as_dict(), "unit": "lines"}]
    validate(data)
    assert data["scopes"][0]["coverage"][0]["percent"] < 90


def test_measurement_interruption_preserves_prior_case_results(registry, monkeypatch, out):
    project = load_projects(registry)[0]
    monkeypatch.setattr(rust, "collect", lambda *a, **k: scope())

    def interrupt(*a):
        raise KeyboardInterrupt

    monkeypatch.setattr(rust, "measure", interrupt)
    data, code = testing.run(
        registry,
        [project],
        SimpleNamespace(out=out),
        testing.TestRequest(coverage=True),
        registry / "cancel-measure",
    )
    assert code == 130
    report = read_report(Path(data["json"]))
    assert report["summary"]["statuses"] == {"passed": 1}
    assert report["duration_complete"] is False


def test_cli_sigterm_uses_normal_cancellation_path(tmp_path):
    import os
    import signal
    import subprocess
    import sys
    import time

    marker = tmp_path / "ready"
    script = f"""from pathlib import Path
import time
from repo import cli
def wait(*args):
    Path({str(marker)!r}).touch()
    time.sleep(30)
cli.dispatch = wait
raise SystemExit(cli.main(['--json','projects']))
"""
    process = subprocess.Popen(
        [sys.executable, "-c", script], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True
    )
    try:
        deadline = time.monotonic() + 5
        while not marker.exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        assert marker.exists()
        os.kill(process.pid, signal.SIGTERM)
        stdout, stderr = process.communicate(timeout=5)
        assert process.returncode == 130
        assert json.loads(stdout)["error"]["message"] == "interrupted"
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()


@pytest.mark.parametrize("failure_stream", ["stdout", "stderr"])
def test_rust_coverage_failure_reports_failed_test_without_exposing_output(
    tmp_path, failure_stream
):
    project = Project("sample", "rust", tmp_path, tmp_path / "Cargo.toml", tmp_path, "sample")
    failure = "test run::tests::connect_profile ... FAILED\nmessage_content=synthetic-private\n"
    stdout = failure if failure_stream == "stdout" else ""
    stderr = failure + "x" * 1200 if failure_stream == "stderr" else "x" * 1200
    runner = FakeRunner(responses=[ok(), ok(stdout, returncode=101, stderr=stderr)])
    measured = rust.measure(project, runner)
    assert not measured.ok
    assert "run::tests::connect_profile" in measured.error
    assert "synthetic-private" not in measured.error


def test_rust_coverage_failure_fallback_suppresses_captured_output(tmp_path):
    project = Project("sample", "rust", tmp_path, tmp_path / "Cargo.toml", tmp_path, "sample")
    runner = FakeRunner(responses=[ok(), ok("message_content=synthetic-private\n", returncode=101)])
    measured = rust.measure(project, runner)
    assert not measured.ok
    assert "101" in measured.error
    assert "synthetic-private" not in measured.error
