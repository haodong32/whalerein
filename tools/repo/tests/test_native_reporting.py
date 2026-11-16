import json
from pathlib import Path

import pytest
from conftest import FakeRunner, collect, render


@pytest.mark.behavior(
    "repo.report.escape-content",
    "Report content is escaped before HTML rendering.",
    description_zh="验证报告对测试名称和项目名称进行转义，防止内容注入脚本。",
)
def test_offline_report_escapes_untrusted_test_data():
    """Escape test names and project labels so report data cannot inject HTML scripts."""
    data = {
        "schema_version": 1,
        "completeness": "incomplete",
        "scopes": [
            {
                "project": "<script>alert(1)</script>",
                "note": "not enumerated",
                "tests": [{"id": "</td><script>bad()</script>", "status": "not_run"}],
            }
        ],
    }
    page = render(data)
    assert "<script>bad()" not in page
    assert "&lt;script&gt;alert(1)" in page
    assert "Not run" in page
    assert "<script src=" not in page
    assert "<link " not in page
    assert "MIT License" in page


@pytest.mark.parametrize(
    "kind,execute,stdout",
    [
        ("go", False, '{"Action":"output","Package":"p","Output":"TestOne\\n"}\n'),
        (
            "go",
            True,
            '{"Action":"run","Package":"p","Test":"TestOne"}\n'
            '{"Action":"pass","Package":"p","Test":"TestOne"}\n',
        ),
        ("go", True, "invalid\n"),
        ("node", False, ""),
        ("node", True, ""),
    ],
    ids=[
        "go-collection-marks-unexecuted-case-not-run",
        "go-unfinished-package-retains-passed-case-but-incomplete-inventory",
        "go-execution-marks-malformed-output-incomplete",
        "node-collection-marks-unexecuted-scope-incomplete",
        "node-execution-marks-empty-inventory-incomplete",
    ],
)
def test_native_collectors_report_coverage_limits_and_execution_metadata(
    tmp_path, kind, execute, stdout, out
):
    """Preserve observed outcomes without claiming complete inventory from insufficient evidence."""

    from conftest import collect

    from repo.projects import Project

    project = Project("sample", kind, tmp_path, tmp_path / "manifest", tmp_path)
    runner = FakeRunner(default_stdout=stdout)
    scope = collect(project, runner, execute)
    assert scope["completeness"] == "incomplete"
    if kind == "go" and stdout.startswith("{"):
        assert scope["tests"][0]["id"] == "p::TestOne"
        assert scope["tests"][0]["status"] == ("passed" if execute else "not_run")


@pytest.mark.parametrize("execute", [False, True], ids=["collect-only", "execute"])
def test_python_collector_without_artifact_cannot_claim_complete_inventory(tmp_path, execute):
    """A successful subprocess without its collection artifact supplies no case inventory."""

    from conftest import collect

    from repo.projects import Project

    project = Project("sample", "python", tmp_path, tmp_path / "pyproject.toml", tmp_path)
    scope = collect(project, FakeRunner(), execute)
    assert scope["exit_code"] == 0
    assert scope["tests"] == []
    assert scope["completeness"] == "incomplete"


@pytest.mark.parametrize(
    "execute,status,collection_errors,collected,exit_code,expected_completeness",
    [
        (False, "not_run", 0, 1, 0, "complete"),
        (True, "passed", 0, 1, 0, "complete"),
        (True, "not_run", 0, 1, 0, "incomplete"),
        (False, "not_run", 1, 1, 0, "incomplete"),
        (False, "not_run", 0, 2, 0, "incomplete"),
        (False, "not_run", 0, 1, 2, "incomplete"),
    ],
    ids=[
        "collected-not-run-inventory-is-complete",
        "executed-passing-inventory-is-complete",
        "execution-with-unobserved-case-is-incomplete",
        "collection-error-is-incomplete",
        "collection-count-mismatch-is-incomplete",
        "collection-process-error-is-incomplete",
    ],
)
def test_python_inventory_completeness_requires_reconciled_collection_evidence(
    tmp_path, execute, status, collection_errors, collected, exit_code, expected_completeness
):
    """Collect-only can prove full inventory; missing execution outcomes cannot prove a full run."""

    from conftest import FakeRunner, collect, ok

    from repo.projects import Project

    class ArtifactRunner(FakeRunner):
        def run(self, argv, cwd, **kwargs):
            assert ("--collect-only" in argv) is (not execute)
            Path(kwargs["env"]["WHALEREIN_TEST_REPORT"]).write_text(
                json.dumps(
                    {
                        "tests": [{"id": "test_sample.py::test_one", "status": status}],
                        "collection_errors": collection_errors,
                        "collected": collected,
                    }
                )
            )
            return ok(returncode=exit_code)

    project = Project("sample", "python", tmp_path, tmp_path / "pyproject.toml", tmp_path)
    scope = collect(project, ArtifactRunner(), execute)
    assert scope["completeness"] == expected_completeness
    assert scope["tests"] == [{"id": "test_sample.py::test_one", "status": status}]
    assert scope["exit_code"] == exit_code


def test_node_reporter_exports_test_inventory_and_outcomes(tmp_path, out):
    """Record both passing and skipped Node tests without exporting captured output."""
    import shutil

    from repo.process import Runner
    from repo.projects import Project

    if not shutil.which("node"):
        pytest.skip("node unavailable")
    directory = tmp_path / "test"
    directory.mkdir()
    (directory / "sample.test.js").write_text(
        "const {test}=require('node:test');test('passes',()=>{});test('skips',{skip:true},()=>{});"
    )
    project = Project("sample", "node", tmp_path, tmp_path / "package.json", tmp_path)
    scope = collect(project, Runner(out=out), True)
    assert {case["status"] for case in scope["tests"]} == {"passed", "skipped"}
    assert scope["exit_code"] == 0


def test_rust_report_uses_stable_target_identity_and_execution_results(tmp_path):
    """Reconcile a compiled Rust target without claiming coverage of other build configurations."""

    from conftest import FakeRunner, ok

    from repo.projects import Project
    from repo.runners.rust import collect as collect_rust

    (tmp_path / "Cargo.toml").write_text('[package]\nname="p"\nversion="0.1.0"\n')
    artifact = {
        "reason": "compiler-artifact",
        "target": {"name": "lib"},
        "profile": {"test": True},
        "executable": "/fake/test",
    }
    runner = FakeRunner(
        responses=[
            ok(json.dumps(artifact)),
            ok("checks: test\n"),
            ok("test checks ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n"),
        ]
    )
    result = collect_rust(
        Project("p", "rust", tmp_path, tmp_path / "Cargo.toml", tmp_path), runner, True
    )
    assert result["tests"] == [{"id": "lib::checks", "status": "passed"}]
    assert result["collection_complete"]
    assert result["completeness"] == "complete"
    assert "other feature/platform configurations" in result["exclusions"]


def test_pytest_plugin_records_case_metadata_and_outcomes(tmp_path, monkeypatch):
    """Export pytest behavior guarantees, authored descriptions and observed outcomes."""
    import importlib.util
    from types import SimpleNamespace

    from repo import pytest_report

    spec = importlib.util.spec_from_file_location("isolated_report_plugin", pytest_report.__file__)
    plugin = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(plugin)

    monkeypatch.setattr(plugin, "_cases", {})
    monkeypatch.setattr(plugin, "_errors", 0)
    config = SimpleNamespace(addinivalue_line=lambda *args: None)
    plugin.pytest_configure(config)
    marker = SimpleNamespace(args=("stable", "guarantee"))
    item = SimpleNamespace(
        nodeid="test.py::sample",
        location=("test.py", 2, "sample"),
        obj=test_pytest_plugin_records_case_metadata_and_outcomes,
        get_closest_marker=lambda name: marker,
    )
    session = SimpleNamespace(items=[item], testscollected=1)
    plugin.pytest_collection_finish(session)
    plugin.pytest_collectreport(SimpleNamespace(failed=False))
    plugin.pytest_runtest_logreport(
        SimpleNamespace(nodeid=item.nodeid, failed=False, skipped=False, when="call")
    )
    destination = tmp_path / "result.json"
    monkeypatch.setenv("WHALEREIN_TEST_REPORT", str(destination))
    plugin.pytest_sessionfinish(session, 0)
    data = json.loads(destination.read_text())
    assert data["tests"][0]["behavior"]["id"] == "stable"
    assert "implementation_hash" not in data["tests"][0]
    assert data["tests"][0]["status"] == "passed"


@pytest.mark.parametrize(
    "case,expected,origin",
    [
        (
            {"id": "path::test_rejects_invalid_token", "status": "passed"},
            "rejects invalid token",
            "native_name",
        ),
        (
            {"id": "pkg::TestRejectsExpiredApproval", "status": "passed"},
            "Rejects Expired Approval",
            "native_name",
        ),
        (
            {
                "id": "file::test_case",
                "description": "Reject expired decisions.",
                "description_source": "docstring",
            },
            "Reject expired decisions.",
            "docstring",
        ),
        (
            {"id": "test_case", "behavior": {"ensures": "Preserve command idempotency."}},
            "Preserve command idempotency.",
            "behavior",
        ),
    ],
    ids=[
        "pytest-name-expands-and-needs-review",
        "go-name-expands-and-needs-review",
        "docstring-is-preserved-as-authored",
        "behavior-is-preserved-as-authored",
    ],
)
def test_case_description_records_source_and_expected_provenance(case, expected, origin):
    """Distinguish authored guarantees from readable expansions of native names."""
    from repo.test_descriptions import describe

    result = describe(case)
    assert result["description"] == expected
    assert result["description_source"] == origin
    assert result["description_needs_review"] == (origin == "native_name")


@pytest.mark.parametrize("explicit_name", [False, True], ids=["infer-from-id", "native-name"])
def test_parameterized_description_preserves_action_before_embedded_identity(explicit_name):
    """A package::parent/child selector inside a parameter cannot truncate the test action."""
    from repo.test_descriptions import describe

    name = "test_go_selection_excludes_other_tests[test.local/sample::TestParent/chosen]"
    identity = "tests/test_case_selection.py::" + name
    case = {"id": identity, "status": "not_run"}
    if explicit_name:
        case["name"] = name
    result = describe(case)
    assert result["id"] == identity
    assert result["description"].startswith("go selection excludes other tests[")
    assert "test.local/sample::" in result["description"]
    assert result["description"].endswith("Parent/chosen]")


def test_report_template_renders_english_case_descriptions_from_json():
    """Render authored English descriptions without consulting test source files."""
    report = {
        "schema_version": 1,
        "completeness": "complete",
        "scopes": [
            {
                "project": "sample",
                "completeness": "complete",
                "tests": [
                    {
                        "id": "test_case",
                        "status": "passed",
                        "description": "Expired approvals are rejected.",
                    }
                ],
            }
        ],
    }
    page = render(report)
    assert "Expired approvals are rejected." in page
    assert ">Passed</span>" in page
    assert 'lang="en"' in page
    assert "0 missing" in page
    report["scopes"][0]["tests"][0].pop("description")
    assert "No description provided" in render(report)


def test_report_module_layout_and_coverage_match_collected_facts():
    report = {
        "schema_version": 1,
        "scopes": [
            {
                "module": "Access",
                "project": "go",
                "completeness": "complete",
                "tests": [{"id": "a", "status": "passed"}],
                "coverage": [
                    {
                        "name": "auth",
                        "percent": 92.5,
                        "unit": "statements",
                        "threshold": 90,
                        "measurement_scope": "native tests",
                    }
                ],
            },
            {
                "module": "System Integration",
                "project": "harness",
                "tests": [{"id": "pair", "status": "blocked", "node_role": "step"}],
                "completeness": "incomplete",
            },
        ],
    }
    page = render(report)
    assert "92.50% statements" in page
    assert "System Integration" in page and "Access" in page
    assert "Collection is incomplete" in page
    assert "module-filter" in page and "status-filter" in page
    assert report["scopes"][0]["tests"][0] == {"id": "a", "status": "passed"}


def test_node_reporter_keeps_following_test_outside_suite_container(tmp_path, out):
    import shutil

    from repo.process import Runner
    from repo.projects import Project

    if not shutil.which("node"):
        pytest.skip("node unavailable")
    tests = tmp_path / "test"
    tests.mkdir()
    (tests / "nested.test.cjs").write_text(
        "const {describe,it,test}=require('node:test');"
        "describe('suite',()=>{it('child',()=>{});});test('independent',()=>{});"
        "test('parent',async t=>{await t.test('nested',()=>{});});"
    )
    result = collect(
        Project("p", "node", tmp_path, tmp_path / "package.json", tmp_path), Runner(out=out), True
    )
    assert result["completeness"] == "complete"
    roles = {c["id"].split("::")[-1]: c["node_role"] for c in result["tests"]}
    assert roles["independent"] == "case"
    assert roles["parent"] == "case"


def test_report_prioritizes_failures_and_collapses_passed_cases():
    report = {
        "schema_version": 1,
        "scopes": [
            {
                "project": "module",
                "completeness": "complete",
                "tests": [
                    {"id": "passed-case", "description": "Successful case", "status": "passed"},
                    {"id": "skipped-case", "description": "Skipped case", "status": "skipped"},
                    {"id": "failed-case", "description": "Failure case", "status": "failed"},
                ],
            }
        ],
    }
    page = render(report)
    assert "Needs attention" in page
    assert "Reason not captured" in page
    assert page.index("Failure case") < page.index("Skipped case")
    assert "Passed tests (1)" in page
    assert 'data-filter="failed"' in page
    assert "data-case-link=" in page
    assert '<details class="passed-tests collapse collapse-arrow">' in page
    assert "Clear filters" in page and 'aria-live="polite"' in page


def test_report_separates_collection_checks_and_collapses_module_details():
    data = {
        "schema_version": 1,
        "scopes": [
            {"project": "Empty", "tests": [], "completeness": "incomplete"},
            {
                "project": "Failed",
                "tests": [{"id": "f", "status": "failed"}],
                "completeness": "incomplete",
            },
        ],
    }
    page = render(data)
    assert "Collection &amp; measurement checks" in page
    assert "No results" in page
    assert "Coverage &amp; collection details" in page
    assert 'class="module collapse collapse-arrow card-border"' in page
    assert "Other platforms" not in page


def test_diagnostics_redact_credentials_and_bound_output():
    from repo.diagnostics import sanitize

    text = 'AssertionError: token="fixture-value" password: fixture-value\nBearer fixture-value\n'
    result = sanitize(text)
    assert "AssertionError" in result
    assert "fixture-value" not in result
    assert "[diagnostic truncated]" in sanitize("x" * 9000)
    assert "PRIVATE KEY" not in sanitize(
        "-----BEGIN PRIVATE KEY-----\nfixture\n-----END PRIVATE KEY-----"
    )


def test_failed_node_case_exports_native_error(tmp_path, out):
    import shutil

    from repo.process import Runner
    from repo.projects import Project

    if not shutil.which("node"):
        pytest.skip("node unavailable")
    folder = tmp_path / "test"
    folder.mkdir()
    (folder / "error.test.cjs").write_text(
        "const {test}=require('node:test');test('broken',()=>{"
        "throw new Error('native failure token=fixture-value');});"
    )
    scope = collect(
        Project("p", "node", tmp_path, tmp_path / "package.json", tmp_path), Runner(out=out), True
    )
    case = scope["tests"][0]
    assert case["status"] == "failed"
    message = case["diagnostics"][0]["message"]
    assert "native failure" in message and "fixture-value" not in message


def test_failed_go_case_exports_associated_output(tmp_path):

    from conftest import FakeRunner, collect

    from repo.projects import Project

    events = [
        {"Action": "run", "Package": "p", "Test": "TestFailure"},
        {
            "Action": "output",
            "Package": "p",
            "Test": "TestFailure",
            "Output": "expected true token=fixture-value\n",
        },
        {"Action": "fail", "Package": "p", "Test": "TestFailure"},
        {"Action": "fail", "Package": "p"},
    ]
    scope = collect(
        Project("p", "go", tmp_path, tmp_path / "go.mod", tmp_path),
        FakeRunner(default_stdout="\n".join(map(json.dumps, events))),
        True,
    )
    diagnostic = scope["tests"][0]["diagnostics"][0]["message"]
    assert "expected true" in diagnostic and "fixture-value" not in diagnostic


def test_pytest_failure_export_contains_traceback_without_captured_output(tmp_path, out):
    import sys

    from repo.process import Runner

    source = tmp_path / "test_failure.py"
    source.write_text(
        "def test_failure():\n    print('private captured output')\n"
        "    assert False, 'native failure token=fixture-value'\n"
    )
    destination = tmp_path / "result.json"
    result = Runner(out=out).run(
        [
            sys.executable,
            "-m",
            "pytest",
            "-p",
            "repo.pytest_report",
            "--override-ini=addopts=",
            str(source),
        ],
        tmp_path,
        env={"WHALEREIN_TEST_REPORT": str(destination)},
        quiet=True,
    )
    assert result.returncode == 1
    case = json.loads(destination.read_text())["tests"][0]
    message = case["diagnostics"][0]["message"]
    assert "AssertionError" in message and "fixture-value" not in message
    assert "Captured stdout" not in message


def test_html_escapes_recorded_failure_diagnostics():
    data = {
        "schema_version": 1,
        "scopes": [
            {
                "project": "p",
                "tests": [
                    {
                        "id": "f",
                        "status": "failed",
                        "diagnostics": [
                            {"phase": "call", "message": "AssertionError: <script>bad()</script>"}
                        ],
                    }
                ],
            }
        ],
    }
    page = render(data)
    assert "&lt;script&gt;bad()&lt;/script&gt;" in page
    assert "<script>bad()" not in page


def test_failed_rust_case_exports_only_its_failure_block(tmp_path):

    from conftest import FakeRunner, ok

    from repo.projects import Project
    from repo.runners.rust import collect as collect_rust

    (tmp_path / "Cargo.toml").write_text('[package]\nname="p"\nversion="0.1.0"\n')
    artifact = {
        "reason": "compiler-artifact",
        "target": {"name": "p"},
        "profile": {"test": True},
        "executable": "/fake/test",
    }
    output = (
        "test broken ... FAILED\nfailures:\n\n---- broken stdout ----\n"
        "thread 'broken' panicked at src/lib.rs:4: expected true token=fixture-value\n"
        "\nfailures:\n    broken\ntest result: FAILED. 0 passed; 1 failed; 0 ignored;\n"
    )
    result = collect_rust(
        Project("p", "rust", tmp_path, tmp_path / "Cargo.toml", tmp_path),
        FakeRunner(responses=[ok(json.dumps(artifact)), ok("broken: test\n"), ok(output, 1)]),
        True,
    )
    message = result["tests"][0]["diagnostics"][0]["message"]
    assert "panicked at src/lib.rs:4" in message
    assert "fixture-value" not in message
    assert "test result:" not in message


def test_rust_stderr_panic_keeps_only_the_source_location(tmp_path):
    from conftest import ok

    from repo.projects import Project
    from repo.runners.rust import collect as collect_rust

    manifest = tmp_path / "Cargo.toml"
    manifest.write_text('[package]\nname="p"\nversion="0.1.0"\n[lib]\ndoctest=false\n')
    artifact = {
        "reason": "compiler-artifact",
        "target": {"name": "p"},
        "profile": {"test": True},
        "executable": "test.exe",
    }
    output = "test broken ... FAILED\ntest result: FAILED. 0 passed; 1 failed; 0 ignored;\n"
    error = (
        "thread 'broken' (123) panicked at core\\src\\sample.rs:42:9:\nsynthetic-private-value\n"
    )
    runner = FakeRunner(
        responses=[ok(json.dumps(artifact)), ok("broken: test\n"), ok(output, 1, stderr=error)]
    )
    result = collect_rust(Project("p", "rust", tmp_path, manifest, tmp_path), runner, True)
    assert result["tests"][0]["diagnostics"] == [
        {"phase": "test", "message": "Rust panic at core\\src\\sample.rs:42:9"}
    ]
