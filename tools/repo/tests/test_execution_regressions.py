"""Cancellation and selection must preserve execution intent and observed facts."""

import json
import sys

import pytest
from conftest import FakeRunner, ok

from repo.output import Output
from repo.process import Runner
from repo.projects import Project
from repo.runners import go, node, rust


def test_interruption_returns_observed_output_and_cleanup_output(tmp_path, out):
    class Interrupt:
        def event(self, event):
            if event.get("event") == "finish":
                raise KeyboardInterrupt

    script = """import json,signal,sys,time
def stop(*args):
    print('cleanup completed',file=sys.stderr,flush=True)
    raise SystemExit(0)
signal.signal(signal.SIGINT,stop)
print(json.dumps({'Action':'pass','Package':'sample','Test':'TestObserved'}),flush=True)
time.sleep(30)
"""
    runner = Runner(out, progress=Interrupt())
    result = runner.run([sys.executable, "-c", script], tmp_path, quiet=True)
    assert result.returncode == 130
    assert "TestObserved" in result.stdout
    assert "cleanup completed" in result.stderr


@pytest.mark.parametrize("phase", ["listing", "execution"])
def test_rust_cancellation_stops_target_scheduling(tmp_path, phase):
    manifest = tmp_path / "Cargo.toml"
    manifest.write_text('[package]\nname="sample"\nversion="0.1.0"\n[lib]\ndoctest=false\n')
    artifacts = "\n".join(
        json.dumps(
            {
                "reason": "compiler-artifact",
                "target": {"name": name},
                "profile": {"test": True},
                "executable": name,
            }
        )
        for name in ("first", "second")
    )
    listed = ok("observed: test\nremaining: test\n", returncode=130 if phase == "listing" else 0)
    runner = FakeRunner(
        responses=[ok(artifacts), listed, ok("test observed ... ok\n", returncode=130)]
    )
    project = Project("sample", "rust", tmp_path, manifest, tmp_path)
    scope = rust.collect(project, runner, True)
    assert scope["exit_code"] == 130
    assert scope["completeness"] == "incomplete"
    assert not any(args[0] == "second" for args, _ in runner.calls)
    if phase == "execution":
        assert {case["id"]: case["status"] for case in scope["tests"]} == {
            "first::observed": "passed",
            "first::remaining": "not_run",
        }
    else:
        assert len(runner.calls) == 2


def test_go_cancellation_stops_selected_package_scheduling(tmp_path):
    inventory = "\n".join(
        json.dumps(event)
        for package in ("sample/first", "sample/second")
        for event in (
            {"Package": package, "Action": "output", "Output": "TestChosen\n"},
            {"Package": package, "Action": "pass"},
        )
    )
    observed = json.dumps({"Package": "sample/first", "Action": "pass", "Test": "TestChosen"})
    runner = FakeRunner(responses=[ok(inventory), ok(observed, returncode=130)])
    project = Project("sample", "go", tmp_path, tmp_path / "go.mod", tmp_path)
    scope = go.collect(project, runner, True, only="TestChosen")
    assert scope["exit_code"] == 130
    assert len(runner.calls) == 2
    assert scope["completeness"] == "incomplete"
    assert scope["tests"][0]["status"] == "passed"
    assert any(
        case["id"] == "sample/second::TestChosen" and case["status"] == "not_run"
        for case in scope["tests"]
    )


def test_node_qualified_selector_does_not_execute_unmatched_body(tmp_path):
    directory = tmp_path / "test"
    directory.mkdir()
    marker = tmp_path / "unselected"
    (directory / "one.test.cjs").write_text(
        "const {test}=require('node:test');const fs=require('node:fs');"
        "test('wanted',()=>{});test('notwanted',()=>{fs.writeFileSync("
        + json.dumps(str(marker))
        + ",'ran')});"
    )
    project = Project("sample", "node", tmp_path, tmp_path / "package.json", tmp_path)
    scope = node.collect(
        project, Runner(Output(quiet=True)), True, only="test/one.test.cjs::wanted"
    )
    assert scope["exit_code"] == 0
    assert not marker.exists()
    assert [c["id"] for c in scope["tests"]] == ["test/one.test.cjs::wanted"]


def test_node_identity_distinguishes_name_paths(tmp_path):
    directory = tmp_path / "test"
    directory.mkdir()
    (directory / "one.test.cjs").write_text(
        "const {test,describe,it}=require('node:test');"
        "test('parent child',()=>{});test('parent/child',()=>{});"
        "test('parent~1child',()=>{});describe('parent',()=>{it('child',()=>{})});"
    )
    project = Project("sample", "node", tmp_path, tmp_path / "package.json", tmp_path)
    scope = node.collect(project, Runner(Output(quiet=True)), True)
    assert scope["completeness"] == "complete"
    assert len({case["id"] for case in scope["tests"]}) == 4
    assert not scope.get("errors")


def test_rust_cancelled_listing_with_no_match_does_not_continue(tmp_path):
    manifest = tmp_path / "Cargo.toml"
    manifest.write_text('[package]\nname="sample"\nversion="0.1.0"\n[lib]\ndoctest=false\n')
    artifacts = "\n".join(
        json.dumps(
            {
                "reason": "compiler-artifact",
                "target": {"name": name},
                "profile": {"test": True},
                "executable": name,
            }
        )
        for name in ("first", "second")
    )
    runner = FakeRunner(responses=[ok(artifacts), ok("", returncode=130)])
    project = Project("sample", "rust", tmp_path, manifest, tmp_path)
    scope = rust.collect(project, runner, True, only="missing")
    assert scope["exit_code"] == 130
    assert len(runner.calls) == 2


def test_go_cancellation_overrides_previous_package_failure(tmp_path):
    inventory = "\n".join(
        json.dumps(event)
        for package in ("sample/first", "sample/second")
        for event in (
            {"Package": package, "Action": "output", "Output": "TestChosen\n"},
            {"Package": package, "Action": "pass"},
        )
    )
    runner = FakeRunner(
        responses=[
            ok(inventory),
            ok(returncode=1, stderr="first failed"),
            ok(returncode=130, stderr="second cancelled"),
        ]
    )
    project = Project("sample", "go", tmp_path, tmp_path / "go.mod", tmp_path)
    scope = go.collect(project, runner, True, only="TestChosen")
    assert scope["exit_code"] == 130
    assert scope["completeness"] == "incomplete"
    assert "first failed" in scope["errors"][0]
    assert "second cancelled" in scope["errors"][0]


@pytest.mark.parametrize("name", ["parent/child", "parent~1child"])
def test_node_escaped_root_name_can_be_selected(tmp_path, name):
    directory = tmp_path / "test"
    directory.mkdir()
    (directory / "one.test.cjs").write_text(
        "const {test,describe,it}=require('node:test');"
        "test('parent/child',()=>{});test('parent~1child',()=>{});"
        "describe('parent',()=>{it('child',()=>{})});"
    )
    project = Project("sample", "node", tmp_path, tmp_path / "package.json", tmp_path)
    runner = Runner(Output(quiet=True))
    encoded = name.replace("~", "~0").replace("/", "~1")
    only = "test/one.test.cjs::" + encoded
    selected = node.collect(project, runner, True, only=only)
    assert selected["exit_code"] == 0
    assert [case["id"] for case in selected["tests"]] == [only]


def test_real_rust_cancel_preserves_observation_and_never_runs_next_target(tmp_path):
    import shutil

    if not shutil.which("cargo"):
        pytest.skip("Cargo unavailable")
    manifest = tmp_path / "Cargo.toml"
    manifest.write_text(
        '[package]\nname="cancel-probe"\nversion="0.1.0"\nedition="2021"\n[[test]]\nname="first"\npath="first.rs"\n[[test]]\nname="second"\npath="second.rs"\n'
    )
    (tmp_path / "first.rs").write_text(
        "#[test] fn observed() {}\n"
        "#[test] fn waiting() {std::thread::sleep(std::time::Duration::from_secs(30));}"
    )
    marker = tmp_path / "second-ran"
    (tmp_path / "second.rs").write_text(
        "#[test] fn unexecuted() {std::fs::write("
        + json.dumps(str(marker))
        + ', b"ran").unwrap();}'
    )

    class Interrupt:
        def event(self, event):
            if event.get("event") == "finish" and event.get("id") == "observed":
                raise KeyboardInterrupt

    project = Project("cancel-probe", "rust", tmp_path, manifest, tmp_path)
    result = rust.collect(project, Runner(Output(quiet=True), progress=Interrupt()), True)
    assert result["exit_code"] == 130
    assert not marker.exists()
    assert {case["id"]: case["status"] for case in result["tests"]} == {
        "first::observed": "passed",
        "first::waiting": "not_run",
    }
    assert result["not_run_targets"] == ["second"]


def test_go_successful_multi_package_selection_keeps_complete_inventory(tmp_path):
    packages = ("sample/first", "sample/second")
    inventory = (
        "\n".join(
            json.dumps(event)
            for package in packages
            for event in (
                {"Package": package, "Action": "output", "Output": "TestChosen\n"},
                {"Package": package, "Action": "pass"},
            )
        )
        + "\n"
    )
    responses = [ok(inventory)]
    for package in packages:
        responses.append(
            ok(
                "\n".join(
                    json.dumps(event)
                    for event in (
                        {"Package": package, "Action": "pass", "Test": "TestChosen"},
                        {"Package": package, "Action": "pass"},
                    )
                )
                + "\n"
            )
        )
    runner = FakeRunner(responses=responses)
    project = Project("sample", "go", tmp_path, tmp_path / "go.mod", tmp_path)
    scope = go.collect(project, runner, True, only="TestChosen")
    assert scope["completeness"] == "complete"
    assert len(scope["tests"]) == 2
    assert scope["exit_code"] == 0


@pytest.mark.parametrize(
    "selector", ["parent~1child", "parent~01child", "~1", "~0", "~", "1child", "0child"]
)
def test_node_unqualified_selector_matches_encoded_id_before_execution(tmp_path, selector):
    directory = tmp_path / "test"
    directory.mkdir()
    names = ["parent/child", "parent~1child", "parent~child", "parent1child"]
    expected = [name for name in names if selector in name.replace("~", "~0").replace("/", "~1")]
    marker = tmp_path / "unselected"
    declarations = []
    for name in names:
        body = (
            "" if name in expected else "fs.writeFileSync(" + json.dumps(str(marker)) + ',"ran");'
        )
        declarations.append("test(" + json.dumps(name) + ",()=>{" + body + "});")
    (directory / "one.test.cjs").write_text(
        "const {test}=require('node:test');const fs=require('node:fs');" + "".join(declarations)
    )
    project = Project("sample", "node", tmp_path, tmp_path / "package.json", tmp_path)
    result = node.collect(project, Runner(Output(quiet=True)), True, only=selector)
    assert not marker.exists()
    assert result["exit_code"] == 0
    assert {case["id"] for case in result["tests"]} == {
        "test/one.test.cjs::" + name.replace("~", "~0").replace("/", "~1") for name in expected
    }


def test_node_selector_pattern_preserves_partial_escape_substrings():
    import itertools
    import re

    names = [
        "".join(chars)
        for length in range(1, 4)
        for chars in itertools.product("a01~/", repeat=length)
    ]
    for name in names:
        encoded = name.replace("~", "~0").replace("/", "~1")
        for start in range(len(encoded)):
            for end in range(start + 1, len(encoded) + 1):
                selector = encoded[start:end]
                pattern = node.name_pattern(selector)
                for candidate in (name, "a", "~1", "/a", "0~"):
                    expected = selector in candidate.replace("~", "~0").replace("/", "~1")
                    assert bool(re.search(pattern, candidate)) == expected


def test_node_qualified_selector_pattern_requires_name_prefix():
    import re

    assert re.search(node.name_pattern("parent~1", anchored=True), "parent/child")
    assert not re.search(node.name_pattern("parent~1", anchored=True), "other parent/child")
    assert not re.search(node.name_pattern("1child", anchored=True), "/child")
    assert not re.search(node.name_pattern("~x"), "literal ~x")
