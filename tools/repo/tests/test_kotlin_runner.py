"""The Kotlin/JVM runner is JDK-only and reports Gradle JUnit facts."""

from __future__ import annotations

import json
from pathlib import Path

from conftest import FakeRunner, ok

from repo.output import Output
from repo.projects import Project, load_projects
from repo.runners import kotlin
from repo.testing import TestRequest as Request
from repo.testing import run as run_tests

ROOT = Path(__file__).resolve().parents[3]


def state_project(tmp_path: Path) -> tuple[Path, Project]:
    root = tmp_path / "repo"
    state = root / "apps/android/state"
    (state / "src/test/kotlin/sample").mkdir(parents=True)
    (state / "build.gradle.kts").write_text("plugins {}\n")
    wrapper = root / "apps/android/gradlew"
    wrapper.write_text("#!/bin/sh\n")
    wrapper.chmod(0o755)
    project = Project(
        "android-state", "kotlin", state, state / "build.gradle.kts", root, "android-state"
    )
    return root, project


def body(cases: str) -> str:
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<testsuite name="sample.SampleTest" tests="3" failures="1" errors="0" skipped="1">'
        + cases
        + "</testsuite>"
    )


class Gradle(FakeRunner):
    def __init__(self, document: str | None, *, returncode: int = 0, output: str = ""):
        super().__init__()
        self.document = document
        self.code = returncode
        self.output = output

    def run(self, argv, cwd, *, env=None, timeout=None, check=False, quiet=False):
        self.calls.append((tuple(str(part) for part in argv), str(cwd)))
        if self.document is not None:
            results = Path(cwd) / "state/build/test-results/test"
            results.mkdir(parents=True, exist_ok=True)
            (results / "TEST-sample.xml").write_text(self.document)
        return ok(self.output, returncode=self.code)


class FakeRunnerShim:
    """``testing.run`` uses the runner's Output for progress messages."""

    def __init__(self, runner):
        self._runner = runner
        self.out = getattr(runner, "out", Output(quiet=True))
        self.progress = None
        self.environ = {}
        self.interrupted = False

    def which(self, tool):
        return self._runner.which(tool)

    def run(self, *args, **kwargs):
        return self._runner.run(*args, **kwargs)


def test_registration_declares_a_jdk_only_kotlin_project():
    project = next(p for p in load_projects(ROOT) if p.id == "android-state")
    assert project.kind == "kotlin"
    assert project.rel == "apps/android/state"
    assert project.capabilities == ["check", "test"]


def test_parse_junit_xml_reads_wrapped_suites_and_diagnostics(tmp_path):
    path = tmp_path / "TEST-sample.xml"
    path.write_text(
        '<testsuites><testsuite name="sample.SampleTest">'
        '<testcase name="passes()" classname="sample.SampleTest" time="0.01"/>'
        '<testcase name="fails()" classname="sample.SampleTest" time="0.02">'
        '<failure message="expected true">stack</failure></testcase>'
        '<testcase name="skips()" classname="sample.SampleTest">'
        '<skipped message="platform"/></testcase>'
        "</testsuite></testsuites>"
    )
    cases = kotlin.parse_junit_files([path])
    assert [(case["id"], case["status"]) for case in cases] == [
        ("sample.SampleTest::fails()", "failed"),
        ("sample.SampleTest::passes()", "passed"),
        ("sample.SampleTest::skips()", "skipped"),
    ]
    assert cases[0]["diagnostics"][0]["message"] == "expected true"
    assert cases[1]["duration_ms"] == 10.0


def test_list_uses_the_gradle_list_property_and_reports_display_names(tmp_path):
    _, project = state_project(tmp_path)
    runner = Gradle(
        body(
            '<testcase name="AC-24.focused confirmed send" classname="sample.AC-24.focused">'
            "<skipped/></testcase>"
            '<testcase name="state updates are observable()" classname="sample.SampleTest">'
            "<skipped/></testcase>"
        )
    )
    scope = kotlin.collect(project, runner, execute=False)
    command = runner.calls[0][0]
    assert "-PcaseList=true" in command
    assert not any(part == "--tests" for part in command)
    assert scope["completeness"] == "complete"
    assert [case["id"] for case in scope["tests"]] == [
        "sample.AC-24.focused::AC-24.focused confirmed send",
        "sample.SampleTest::state updates are observable()",
    ]
    assert all(case["status"] == "not_run" for case in scope["tests"])
    filtered = kotlin.collect(project, runner, execute=False, only="AC-24.focused")
    assert [case["id"] for case in filtered["tests"]] == [
        "sample.AC-24.focused::AC-24.focused confirmed send"
    ]


def test_execution_invokes_gradle_and_reports_cases(tmp_path, monkeypatch):
    _, project = state_project(tmp_path)
    monkeypatch.setattr(kotlin, "_revision", lambda root: "revision-1")
    runner = Gradle(
        body(
            '<testcase name="passes()" classname="sample.SampleTest"/>'
            '<testcase name="fails()" classname="sample.SampleTest">'
            '<failure message="boom"/></testcase>'
            '<testcase name="skips()" classname="sample.SampleTest"><skipped/></testcase>'
        )
    )
    scope = kotlin.collect(project, runner, execute=True)
    command, cwd = runner.calls[0]
    assert command[:5] == (
        str(project.path.parent / "gradlew"),
        "-p",
        "state",
        "cleanTest",
        "test",
    )
    assert Path(cwd) == project.path.parent
    assert not any(part.startswith("-Pcase") for part in command)
    assert scope["exit_code"] == 0
    assert scope["completeness"] == "complete"
    assert scope["revision"] == "revision-1"
    assert [case["status"] for case in scope["tests"]] == ["failed", "passed", "skipped"]


def test_only_passes_the_case_filter_before_execution(tmp_path):
    _, project = state_project(tmp_path)
    runner = Gradle(
        body(
            '<testcase name="AC-24.focused keeps one send()" classname="sample.SampleTest"/>'
            '<testcase name="other()" classname="sample.SampleTest"><skipped/></testcase>'
        )
    )
    scope = kotlin.collect(project, runner, execute=True, only="AC-24.focused")
    command = runner.calls[0][0]
    assert "-PcaseFilter=AC-24.focused" in command
    assert not any(part == "--tests" for part in command)
    assert [case["id"] for case in scope["tests"]] == [
        "sample.SampleTest::AC-24.focused keeps one send()"
    ]


def test_filter_skipped_cases_are_not_selected_evidence(tmp_path):
    _, project = state_project(tmp_path)
    runner = Gradle(
        body(
            '<testcase name="chosen()" classname="sample.SampleTest"><skipped/></testcase>'
            '<testcase name="other()" classname="sample.SampleTest"><skipped/></testcase>'
        )
    )
    scope = kotlin.collect(project, runner, execute=True, only="chosen")
    assert scope["completeness"] == "complete"
    assert scope["tests"] == []
    assert scope["exit_code"] == 0


def test_literal_wildcard_selector_stays_one_property_value(tmp_path):
    _, project = state_project(tmp_path)
    runner = Gradle(body('<testcase name="literal * case()" classname="sample.SampleTest"/>'))
    kotlin.collect(project, runner, execute=True, only="literal * case")
    command = runner.calls[0][0]
    assert "-PcaseFilter=literal * case" in command
    assert not any(part == "--tests" for part in command)


def test_unmatched_filter_is_invalid_usage_and_saved_report_has_facts(tmp_path, monkeypatch):
    root, project = state_project(tmp_path)
    monkeypatch.setattr(kotlin, "_revision", lambda value: "revision-2")
    runner = Gradle(
        body('<testcase name="other()" classname="sample.SampleTest"><skipped/></testcase>')
    )
    output = tmp_path / "run"
    data, code = run_tests(
        root,
        [project],
        FakeRunnerShim(runner),
        Request(projects=("android-state",), only="missing"),
        output,
    )
    assert code == 2
    report = json.loads((output / "report.json").read_text())
    assert report["scopes"][0]["project"] == "android-state"
    assert report["scopes"][0]["revision"] == "revision-2"
    assert "No test cases match --only" in report["scopes"][0]["errors"][0]
    assert json.loads(Path(data["json"]).read_text())["summary"]["cases"] == 0
    assert (output / "report.html").exists()


def test_full_execution_with_no_cases_is_not_a_silent_pass(tmp_path):
    _, project = state_project(tmp_path)
    runner = Gradle(body(""))
    scope = kotlin.collect(project, runner, execute=True)
    assert scope["completeness"] == "incomplete"
    assert scope["tests"] == []
    assert scope["errors"] == ["No JUnit test cases found"]


class CoverageGradle(Gradle):
    def __init__(self, document: str, jacoco: str | None, **kwargs):
        super().__init__(document, **kwargs)
        self.jacoco = jacoco

    def run(self, argv, cwd, **kwargs):
        result = super().run(argv, cwd, **kwargs)
        if self.jacoco is not None:
            report = Path(cwd) / "state" / kotlin.JACOCO_XML
            report.parent.mkdir(parents=True, exist_ok=True)
            report.write_text(self.jacoco)
        return result


JACOCO = (
    '<report name="state">'
    '<package name="com/whalerein/android/state/core">'
    '<counter type="LINE" missed="10" covered="90"/></package>'
    '<package name="com/whalerein/android/state/access">'
    '<counter type="LINE" missed="20" covered="80"/></package>'
    "</report>"
)


def test_coverage_runs_jacoco_and_reports_each_package(tmp_path):
    _, project = state_project(tmp_path)
    runner = CoverageGradle(
        body('<testcase name="passes()" classname="sample.SampleTest"/>'), JACOCO
    )
    scope = kotlin.collect(project, runner, execute=True, coverage=True)
    command = runner.calls[0][0]
    assert "test" in command and "jacocoTestCoverageVerification" in command
    coverage = {entry["name"]: entry for entry in scope["coverage"]}
    assert set(coverage) == {
        "com.whalerein.android.state.access",
        "com.whalerein.android.state.core",
    }
    assert coverage["com.whalerein.android.state.core"]["percent"] == 90.0
    assert coverage["com.whalerein.android.state.core"]["ok"] is True
    assert coverage["com.whalerein.android.state.access"]["percent"] == 80.0
    assert coverage["com.whalerein.android.state.access"]["ok"] is False
    assert all(entry["unit"] == "lines" for entry in coverage.values())


def test_coverage_missing_jacoco_xml_is_a_failure(tmp_path):
    _, project = state_project(tmp_path)
    runner = CoverageGradle(body('<testcase name="passes()" classname="sample.SampleTest"/>'), None)
    scope = kotlin.collect(project, runner, execute=True, coverage=True)
    assert len(scope["coverage"]) == 1
    assert scope["coverage"][0]["ok"] is False
    assert "JaCoCo" in scope["coverage"][0]["error"]


def test_coverage_below_threshold_fails_the_saved_report(tmp_path):
    root, project = state_project(tmp_path)
    runner = CoverageGradle(
        body('<testcase name="passes()" classname="sample.SampleTest"/>'), JACOCO
    )
    output = tmp_path / "covered"
    data, code = run_tests(
        root,
        [project],
        FakeRunnerShim(runner),
        Request(projects=("android-state",), coverage=True),
        output,
    )
    assert code == 1
    report = json.loads((output / "report.json").read_text())
    coverage = report["scopes"][0]["coverage"]
    assert any(entry["percent"] == 80.0 and entry["ok"] is False for entry in coverage)
    assert json.loads(Path(data["json"]).read_text())["summary"]["statuses"] == {"passed": 1}


def test_static_check_compiles_test_classes_without_running(tmp_path):
    _, project = state_project(tmp_path)
    steps = kotlin.steps(project, "check")
    assert len(steps) == 1
    assert steps[0].argv[-2:] == ("testClasses", "--console=plain")
    assert kotlin.steps(project, "build") == []
