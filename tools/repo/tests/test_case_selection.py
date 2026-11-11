"""Selection happens before native test bodies execute."""

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest
from conftest import collect

from repo.output import Output
from repo.process import Runner
from repo.projects import Project


def test_node_selection_does_not_execute_unselected_bodies(tmp_path):
    if not shutil.which("node"):
        pytest.skip("node unavailable")
    directory = tmp_path / "test"
    directory.mkdir()
    (directory / "one.test.js").write_text(
        "const {test}=require('node:test');"
        "test('chosen [literal]',()=>{});"
        "test('other',()=>{throw Error('must not execute')});"
    )
    (directory / "two.test.js").write_text(
        "const {test}=require('node:test');"
        "test('unselected',()=>{throw Error('must not execute')});"
    )
    project = Project("sample", "node", tmp_path, tmp_path / "package.json", tmp_path)
    scope = collect(project, Runner(Output(quiet=True)), True, only="chosen [literal]")
    assert scope["exit_code"] == 0
    assert scope["completeness"] == "complete"
    assert len(scope["tests"]) == 1
    assert scope["tests"][0]["status"] == "passed"
    assert "chosen [literal]" in scope["tests"][0]["id"]
    empty = collect(project, Runner(Output(quiet=True)), True, only="missing")
    assert empty["tests"] == []


def test_pytest_selection_excludes_unselected_fixture(tmp_path):
    (tmp_path / "test_sample.py").write_text(
        "import pytest\n"
        "@pytest.fixture\ndef forbidden():\n    raise AssertionError('must not execute')\n"
        "def test_other(forbidden): pass\n"
        "def test_chosen(): pass\n"
    )
    destination = tmp_path / "report.json"
    source = Path(__file__).resolve().parents[1] / "src"
    result = subprocess.run(
        [sys.executable, "-m", "pytest", "-p", "repo.pytest_report", "-q", str(tmp_path)],
        cwd=tmp_path,
        env={
            **os.environ,
            "PYTHONPATH": str(source),
            "WHALEREIN_TEST_ONLY": "test_chosen",
            "WHALEREIN_TEST_REPORT": str(destination),
        },
        capture_output=True,
        text=True,
        timeout=20,
    )
    assert result.returncode == 0, result.stderr
    cases = json.loads(destination.read_text())["tests"]
    assert len(cases) == 1
    assert cases[0]["id"].endswith("::test_chosen")
    assert cases[0]["status"] == "passed"


@pytest.mark.parametrize(
    "only",
    ["TestChosen", "test.local/sample::TestParent/chosen"],
    ids=["top-level-name", "fully-qualified-child"],
)
def test_go_selection_excludes_other_tests(tmp_path, only):
    if not shutil.which("go"):
        pytest.skip("Go unavailable")
    (tmp_path / "go.mod").write_text("module test.local/sample\n\ngo 1.23\n")
    (tmp_path / "sample_test.go").write_text(
        'package sample\nimport "testing"\n'
        "func TestChosen(t *testing.T) {}\n"
        'func TestOther(t *testing.T) { t.Fatal("must not execute") }\n'
        "func TestParent(t *testing.T) {\n"
        't.Run("chosen",func(t *testing.T) {});\n'
        't.Run("other",func(t *testing.T) {t.Fatal("must not execute")})\n}\n'
    )
    project = Project("sample", "go", tmp_path, tmp_path / "go.mod", tmp_path)
    scope = collect(project, Runner(Output(quiet=True)), True, only=only)
    assert scope["exit_code"] == 0
    assert all(case["status"] == "passed" for case in scope["tests"])
    assert any(only in case["id"] for case in scope["tests"])
    assert not any("Other" in case["id"] or "/other" in case["id"] for case in scope["tests"])


def test_rust_selection_does_not_execute_unselected_tests(tmp_path):
    if not shutil.which("cargo"):
        pytest.skip("Cargo unavailable")
    from repo.runners.rust import collect as collect_rust

    (tmp_path / "Cargo.toml").write_text(
        '[package]\nname="selection-probe"\nversion="0.1.0"\nedition="2021"\n[lib]\ndoctest=false\n'
    )
    (tmp_path / "src").mkdir()
    (tmp_path / "src/lib.rs").write_text(
        "#[test] fn chosen_one() {}\n#[test] fn chosen_two() {}\n"
        '#[test] fn other() {panic!("must not execute");}\n'
    )
    project = Project("sample", "rust", tmp_path, tmp_path / "Cargo.toml", tmp_path)
    scope = collect_rust(project, Runner(Output(quiet=True)), True, only="chosen_")
    assert scope["exit_code"] == 0
    assert scope["completeness"] == "complete"
    assert len(scope["tests"]) == 2
    assert all(case["status"] == "passed" for case in scope["tests"])


def test_node_identity_survives_line_changes_and_names_nested_cases(tmp_path):
    from repo.runners.node import collect

    directory = tmp_path / "test"
    directory.mkdir()
    source = directory / "stable.test.js"
    body = "const {describe,it}=require('node:test');describe('parent',()=>{it('child',()=>{})});"
    source.write_text(body)
    project = Project("sample", "node", tmp_path, tmp_path / "package.json", tmp_path)
    runner = Runner(Output(quiet=True))
    first = collect(project, runner, True)
    source.write_text("\n\n" + body)
    second = collect(project, runner, True)
    assert [c["id"] for c in first["tests"]] == [c["id"] for c in second["tests"]]
    assert any("parent/child" in c["id"] for c in first["tests"])
    identity = next(c["id"] for c in first["tests"] if "parent/child" in c["id"])
    selected = collect(project, runner, True, only=identity)
    assert selected["exit_code"] == 0
    assert any(c["id"] == identity for c in selected["tests"])
