"""Execute workflow decision scripts to guard timing tiers and failure gates."""

import json
import os
import re
import shutil
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[3]
pytestmark = pytest.mark.skipif(
    os.name == "nt" or not shutil.which("bash"),
    reason="requires the POSIX Bash used by the Linux GitHub Actions decision jobs",
)


def script(workflow, step):
    text = (ROOT / ".github/workflows" / workflow).read_text()
    block = text.split("      - name: " + step + "\n", 1)[1]
    match = re.search(r"        run: \|\n((?:          .*\n|\n)+)", block)
    if match is None:
        inline = re.search(r"        run: (.+)", block)
        assert inline, step
        return inline[1]
    return "\n".join(line[10:] for line in match[1].splitlines())


def execute(tmp_path, workflow, step, **env):
    output = tmp_path / "output"
    output.write_text("")
    result = subprocess.run(
        ["bash", "-e", "-c", script(workflow, step)],
        cwd=ROOT,
        env={**os.environ, "GITHUB_OUTPUT": str(output), **env},
        capture_output=True,
        text=True,
        timeout=5,
    )
    return result.returncode, dict(line.split("=", 1) for line in output.read_text().splitlines())


GATE = {
    "CHANGES_RESULT": "success",
    "TEST_RESULT": "success",
    "NATIVE_RESULT": "skipped",
    "NATIVE_MATRIX": '{"include": []}',
    "CONTRACT_RESULT": "skipped",
    "DEPLOYMENT_RESULT": "skipped",
    "ANDROID_RESULT": "skipped",
    "ANDROID_BRIDGE_RESULT": "skipped",
    "ANDROID_MATRIX": '{"include":[]}',
    "ANDROID_PERFORMANCE_RESULT": "skipped",
    "ANDROID_PERFORMANCE": "false",
    "MATRIX": '{"include": [{"projects": ["dsh"]}]}',
}


@pytest.mark.parametrize("action,label", [("synchronize", "run-full-ci"), ("labeled", "other")])
def test_ordinary_pr_never_checks_out_or_builds_ios(tmp_path, action, label):
    code, outputs = execute(
        tmp_path,
        "ios.yml",
        "Compare the tested revision",
        EVENT="pull_request",
        ACTION=action,
        LABEL=label,
    )
    assert code == 0
    assert outputs == {
        "build": "false",
        "integration": "false",
        "matrix": '{"include":[]}',
    }


def test_requested_ios_detects_core_without_requiring_swift_changes(tmp_path):
    git = tmp_path / "git"
    git.write_text("#!/bin/sh\nprintf 'core/src/lib.rs\\n'\n")
    git.chmod(0o755)
    code, outputs = execute(
        tmp_path,
        "ios.yml",
        "Compare the tested revision",
        EVENT="pull_request",
        ACTION="labeled",
        LABEL="run-full-ci",
        BASE_SHA="base",
        HEAD_SHA="head",
        PATH=str(tmp_path) + os.pathsep + os.environ["PATH"],
    )
    assert code == 0
    assert outputs["build"] == "true"
    jobs = json.loads(outputs["matrix"])["include"]
    assert [(job["target"], job["shard"]) for job in jobs] == [
        ("ios-unit", ""),
        ("ios-ui", "1/4"),
        ("ios-ui", "2/4"),
        ("ios-ui", "3/4"),
        ("ios-ui", "4/4"),
    ]
    assert len({job["name"] for job in jobs}) == len(jobs)
    assert outputs["integration"] == "false"


@pytest.mark.parametrize(
    ("suite", "targets"),
    [
        ("unit", ["ios-unit"]),
        ("smoke", ["ios-unit", "ios-ui-smoke"]),
        ("integration", []),
    ],
)
def test_ios_dispatch_respects_selected_suite(tmp_path, suite, targets):
    code, outputs = execute(
        tmp_path,
        "ios.yml",
        "Compare the tested revision",
        EVENT="workflow_dispatch",
        SUITE=suite,
    )
    assert code == 0
    assert [job["target"] for job in json.loads(outputs["matrix"])["include"]] == targets
    assert outputs["build"] == ("false" if suite == "integration" else "true")
    assert outputs["integration"] == ("true" if suite == "integration" else "false")


def test_ios_schedule_requests_stack_integration(tmp_path):
    code, outputs = execute(tmp_path, "ios.yml", "Compare the tested revision", EVENT="schedule")
    assert code == 0
    assert outputs["build"] == "true"
    assert outputs["integration"] == "true"
    assert [job["target"] for job in json.loads(outputs["matrix"])["include"]] == [
        "ios-unit",
        "ios-ui",
        "ios-ui",
        "ios-ui",
        "ios-ui",
    ]


@pytest.mark.parametrize("result", ["success", "failure", "cancelled", "skipped"])
def test_ios_gate_requires_selected_stack_integration(tmp_path, result):
    code, _ = execute(
        tmp_path,
        "ios.yml",
        "Check required results",
        CHANGES_RESULT="success",
        NATIVE_REQUIRED="false",
        NATIVE_RESULT="skipped",
        INTEGRATION_REQUIRED="true",
        INTEGRATION_RESULT=result,
        ARCHITECTURE_RESULT="success",
    )
    assert (code == 0) == (result == "success")


@pytest.mark.parametrize("label,expected", [("run-integration", "true"), ("run-full-ci", "false")])
def test_integration_only_responds_to_its_own_label(tmp_path, label, expected):
    code, outputs = execute(
        tmp_path,
        "integration.yml",
        "Decide whether to run",
        EVENT="pull_request",
        ACTION="labeled",
        LABEL=label,
        REVISION="current",
    )
    assert code == 0
    assert outputs["run"] == expected


@pytest.mark.parametrize("result", ["failure", "cancelled", "skipped", "success"])
def test_quick_gate_cannot_hide_unsuccessful_jobs(tmp_path, result):
    code, _ = execute(tmp_path, "ci.yml", "Require every quick check", RESULT=result)
    assert (code == 0) == (result == "success")


@pytest.mark.parametrize("changes", ["failure", "cancelled", "success"])
def test_ios_gate_requires_successful_selection_even_when_build_is_skipped(tmp_path, changes):
    code, _ = execute(
        tmp_path,
        "ios.yml",
        "Check required results",
        CHANGES_RESULT=changes,
        NATIVE_REQUIRED="false",
        NATIVE_RESULT="skipped",
        INTEGRATION_REQUIRED="false",
        INTEGRATION_RESULT="skipped",
        ARCHITECTURE_RESULT="success",
    )
    assert (code == 0) == (changes == "success")


@pytest.mark.parametrize("result", ["failure", "cancelled", "skipped", "success"])
def test_requested_ios_full_ui_failure_blocks_gate(tmp_path, result):
    code, _ = execute(
        tmp_path,
        "ios.yml",
        "Check required results",
        CHANGES_RESULT="success",
        NATIVE_REQUIRED="true",
        NATIVE_RESULT=result,
        INTEGRATION_REQUIRED="false",
        INTEGRATION_RESULT="skipped",
        ARCHITECTURE_RESULT="success",
    )
    assert (code == 0) == (result == "success")


@pytest.mark.parametrize("result", ["failure", "cancelled", "skipped", "success"])
def test_ios_architecture_gate_blocks_the_gate(tmp_path, result):
    code, _ = execute(
        tmp_path,
        "ios.yml",
        "Check required results",
        CHANGES_RESULT="success",
        NATIVE_REQUIRED="false",
        NATIVE_RESULT="skipped",
        INTEGRATION_REQUIRED="false",
        INTEGRATION_RESULT="skipped",
        ARCHITECTURE_RESULT=result,
    )
    assert (code == 0) == (result == "success")


def gate(tmp_path, **overrides):
    return execute(
        tmp_path,
        "full-ci.yml",
        "Check required results",
        **{**GATE, **overrides},
    )


@pytest.mark.parametrize("proto", [True, False])
@pytest.mark.parametrize("contract", ["success", "skipped", "failure"])
def test_full_gate_only_accepts_expected_contract_result(tmp_path, proto, contract):
    code, _ = gate(
        tmp_path,
        CONTRACT_RESULT=contract,
        MATRIX='{"include": [{"projects": ["%s"]}]}' % ("proto" if proto else "dsh"),
    )
    assert (code == 0) == (contract == ("success" if proto else "skipped"))


@pytest.mark.parametrize("access", [True, False])
@pytest.mark.parametrize("deployment", ["success", "skipped", "failure", "cancelled"])
def test_full_gate_requires_deployment_only_for_access_consumers(tmp_path, access, deployment):
    code, _ = gate(
        tmp_path,
        DEPLOYMENT_RESULT=deployment,
        MATRIX='{"include": [{"projects": ["%s"]}]}' % ("access" if access else "dsh"),
    )
    assert (code == 0) == (deployment == ("success" if access else "skipped"))


@pytest.mark.parametrize("result", ["success", "failure", "cancelled", "skipped"])
def test_full_gate_requires_selected_android_suites(tmp_path, result):
    code, _ = gate(
        tmp_path,
        ANDROID_RESULT=result,
        ANDROID_BRIDGE_RESULT="success",
        ANDROID_MATRIX='{"include":[{"target":"android-ui"}]}',
    )
    assert (code == 0) == (result == "success")


@pytest.mark.parametrize("result", ["success", "failure", "cancelled", "skipped"])
def test_full_gate_requires_selected_bridge_jvm_suite(tmp_path, result):
    code, _ = gate(
        tmp_path,
        ANDROID_RESULT="success",
        ANDROID_BRIDGE_RESULT=result,
        ANDROID_MATRIX='{"include":[{"target":"android-ui"}]}',
    )
    assert (code == 0) == (result == "success")


@pytest.mark.parametrize("result", ["success", "failure", "cancelled", "skipped"])
def test_full_gate_requires_requested_performance_run(tmp_path, result):
    code, _ = gate(
        tmp_path,
        ANDROID_PERFORMANCE="true",
        ANDROID_PERFORMANCE_RESULT=result,
    )
    assert (code == 0) == (result == "success")


@pytest.mark.parametrize(
    ("action", "label", "expected"),
    [
        ("synchronize", "run-full-ci", False),
        ("labeled", "other", False),
        ("opened", "run-full-ci", False),
    ],
)
def test_android_heavy_work_never_runs_on_ordinary_pr_events(tmp_path, action, label, expected):
    code, outputs = execute(
        tmp_path,
        "full-ci.yml",
        "Select Android work",
        EVENT="pull_request",
        ACTION=action,
        LABEL=label,
    )
    assert code == 0
    assert outputs["matrix"] == '{"include":[]}'
    assert outputs["performance"] == "false"
    assert expected is False


def test_android_heavy_work_selects_exact_revision_on_label_event(tmp_path):
    git = tmp_path / "git"
    git.write_text("#!/bin/sh\nprintf 'apps/android/app/build.gradle.kts\\n'\n")
    git.chmod(0o755)
    code, outputs = execute(
        tmp_path,
        "full-ci.yml",
        "Select Android work",
        EVENT="pull_request",
        ACTION="labeled",
        LABEL="run-full-ci",
        BASE_SHA="base",
        HEAD_SHA="head",
        PATH=str(tmp_path) + os.pathsep + os.environ["PATH"],
    )
    assert code == 0
    assert [job["target"] for job in json.loads(outputs["matrix"])["include"]] == [
        "android-ui",
        "android-integration",
    ]
    assert outputs["performance"] == "false"


def test_android_heavy_work_skips_unrelated_labeled_changes(tmp_path):
    git = tmp_path / "git"
    git.write_text("#!/bin/sh\nprintf 'docs/testing.md\\n'\n")
    git.chmod(0o755)
    code, outputs = execute(
        tmp_path,
        "full-ci.yml",
        "Select Android work",
        EVENT="pull_request",
        ACTION="labeled",
        LABEL="run-full-ci",
        BASE_SHA="base",
        HEAD_SHA="head",
        PATH=str(tmp_path) + os.pathsep + os.environ["PATH"],
    )
    assert code == 0
    assert outputs["matrix"] == '{"include":[]}'


@pytest.mark.parametrize(
    "path",
    [
        "core/src/api.rs",
        "apps/mobile-bindings/src/client.rs",
        "proto/v1/whalerein.proto",
    ],
)
def test_android_label_filter_includes_shared_native_inputs(tmp_path, path):
    git = tmp_path / "git"
    git.write_text(f"#!/bin/sh\nprintf '{path}\\n'\n")
    git.chmod(0o755)
    code, outputs = execute(
        tmp_path,
        "full-ci.yml",
        "Select Android work",
        EVENT="pull_request",
        ACTION="labeled",
        LABEL="run-full-ci",
        BASE_SHA="base",
        HEAD_SHA="head",
        PATH=str(tmp_path) + os.pathsep + os.environ["PATH"],
    )
    assert code == 0
    assert [job["target"] for job in json.loads(outputs["matrix"])["include"]] == [
        "android-ui",
        "android-integration",
    ]


def test_manual_android_dispatch_can_request_performance(tmp_path):
    code, outputs = execute(
        tmp_path,
        "full-ci.yml",
        "Select Android work",
        EVENT="workflow_dispatch",
        PERFORMANCE="true",
    )
    assert code == 0
    assert outputs["performance"] == "true"
    assert [job["target"] for job in json.loads(outputs["matrix"])["include"]] == [
        "android-ui",
        "android-integration",
    ]


@pytest.mark.parametrize("elapsed,warning", [(179, False), (180, False), (181, True), (500, True)])
def test_slow_quick_checks_warn_without_failing(tmp_path, elapsed, warning):
    date = tmp_path / "date"
    date.write_text("#!/bin/sh\nprintf '1000\\n'\n")
    date.chmod(0o755)
    result = subprocess.run(
        ["bash", "-e", "-c", script("ci.yml", "Report quick-check duration")],
        env={
            **os.environ,
            "PATH": str(tmp_path) + os.pathsep + os.environ["PATH"],
            "QUICK_CI_STARTED_AT": str(1000 - elapsed),
        },
        capture_output=True,
        text=True,
        timeout=5,
    )
    assert result.returncode == 0
    assert ("::warning::" in result.stdout) == warning
    assert f"{elapsed}s" in result.stdout
