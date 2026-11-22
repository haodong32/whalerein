"""Android device suites fail loudly when their prerequisites are absent."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest
from conftest import FakeRunner, ok

from repo import cli, targets
from repo.projects import load_projects
from repo.runners import android
from repo.verification import Invocation, run_suite

ROOT = Path(__file__).resolve().parents[3]
SERIAL = "emulator-5554"


def checkout(tmp_path: Path) -> Path:
    root = tmp_path / "checkout"
    root.mkdir()
    (root / "input").write_text("test only")
    (root / "apps/android").mkdir(parents=True)
    wrapper = root / "apps/android/gradlew"
    wrapper.write_text("#!/bin/sh\n")
    wrapper.chmod(0o755)
    for command in [
        ["init", "-q"],
        ["-c", "user.name=Test", "-c", "user.email=test@example.invalid", "add", "input"],
        ["-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-m", "base"],
    ]:
        subprocess.run(["git", "-C", root, *command], check=True, capture_output=True)
    return root


def sdk(tmp_path: Path) -> Path:
    directory = tmp_path / "sdk"
    directory.mkdir()
    return directory


def xml(cases: str) -> str:
    return (
        '<?xml version="1.0" encoding="UTF-8"?>'
        '<testsuite name="com.whalerein.android.AndroidTest" tests="1">' + cases + "</testsuite>"
    )


class Device(FakeRunner):
    def __init__(
        self,
        *,
        devices: str = f"List of devices attached\n{SERIAL}\tdevice product:fixture\n",
        emulator: bool = False,
        body: str | None = None,
        gradle_code: int = 0,
    ):
        super().__init__()
        self.devices = devices
        self.emulator = emulator
        self.body = body
        self.gradle_code = gradle_code
        self.which_map["adb"] = "/sdk/platform-tools/adb"

    def run(self, argv, cwd, *, env=None, timeout=None, check=False, quiet=False):
        argv = tuple(str(part) for part in argv)
        self.calls.append((argv, str(cwd)))
        if argv[:2] == ("adb", "devices"):
            return ok(self.devices)
        if argv[:2] == ("adb", "-s") and argv[-3:-1] == ("shell", "getprop"):
            prop = argv[-1]
            values = {
                "ro.build.version.sdk": "36",
                "ro.build.version.release": "16",
                "ro.product.cpu.abi": "arm64-v8a",
                "ro.kernel.qemu": "1" if self.emulator else "0",
            }
            return ok(values.get(prop, ""))
        if argv[:2] == ("adb", "-s") and argv[-2:] == ("getconf", "PAGE_SIZE"):
            return ok("4096\n")
        if argv[0].endswith(("gradlew", "gradlew.bat")):
            if self.body is not None:
                directory = Path(cwd) / "app/build/outputs/androidTest-results/connected"
                directory.mkdir(parents=True, exist_ok=True)
                (directory / "TEST-sample.xml").write_text(self.body)
            return ok(returncode=self.gradle_code)
        raise AssertionError(f"unexpected invocation: {argv}")


@pytest.fixture
def env(tmp_path, monkeypatch):
    root = checkout(tmp_path)
    monkeypatch.setenv("ANDROID_HOME", str(sdk(tmp_path)))
    monkeypatch.delenv("ANDROID_SERIAL", raising=False)
    monkeypatch.delenv("WHALEREIN_ANDROID_LOCAL_STACK", raising=False)
    monkeypatch.delenv("GITHUB_ACTIONS", raising=False)
    return root


def report(output: Path) -> dict:
    return json.loads((output / "report.json").read_text())


def test_missing_adb_reports_unavailable_evidence(env, tmp_path):
    runner = Device()
    runner.which_map.clear()
    output = tmp_path / "missing-adb"
    result = android.run_android(runner, env, "android-ui", output=output)
    assert result.exit_code == 3
    verification = report(output)["verification"]
    assert verification["error_code"] == "android-adb-unavailable"
    assert runner.calls == []


def test_missing_sdk_reports_unavailable_evidence(env, tmp_path, monkeypatch):
    monkeypatch.delenv("ANDROID_HOME")
    monkeypatch.delenv("ANDROID_SDK_ROOT", raising=False)
    runner = Device()
    output = tmp_path / "missing-sdk"
    result = android.run_android(runner, env, "android-ui", output=output)
    assert result.exit_code == 3
    assert report(output)["verification"]["error_code"] == "android-sdk-unavailable"


def test_missing_device_reports_unavailable_evidence(env, tmp_path):
    runner = Device(devices="List of devices attached\n")
    output = tmp_path / "missing-device"
    result = android.run_android(runner, env, "android-ui", output=output)
    assert result.exit_code == 3
    assert report(output)["verification"]["error_code"] == "android-device-unavailable"


def test_execution_saves_device_facts_and_case_statuses(env, tmp_path):
    runner = Device(
        body=xml(
            '<testcase name="passes" classname="com.whalerein.android.AndroidTest"/>'
            '<testcase name="fails" classname="com.whalerein.android.AndroidTest">'
            '<failure message="fixture failure"/></testcase>'
            '<testcase name="skips" classname="com.whalerein.android.AndroidTest">'
            "<skipped/></testcase>"
        ),
        gradle_code=1,
    )
    output = tmp_path / "connected"
    result = android.run_android(runner, env, "android-ui", output=output)
    assert result.exit_code == 1
    document = report(output)
    facts = document["verification"]
    assert facts["error_code"] == "android-tests-failed"
    assert facts["artifacts"]["android-device"]["api"] == "36"
    assert facts["artifacts"]["android-device"]["abi"] == "arm64-v8a"
    assert facts["artifacts"]["android-device"]["page_size"] == "4096"
    assert facts["artifacts"]["android-task"]["name"] == ":app:connectedNoGmsDebugAndroidTest"
    assert document["configuration"]["suite"] == "android-ui"
    assert [case["status"] for case in document["scopes"][0]["tests"]] == [
        "failed",
        "passed",
        "skipped",
    ]
    assert document["scopes"][0]["kind"] == "android"


def test_passing_device_suite_reports_success(env, tmp_path):
    runner = Device(
        body=xml('<testcase name="passes" classname="com.whalerein.android.AndroidTest"/>')
    )
    output = tmp_path / "passing"
    result = android.run_android(runner, env, "android-ui", output=output)
    assert result.exit_code == 0
    assert report(output)["scopes"][0]["tests"] == [
        {"id": "com.whalerein.android.AndroidTest::passes", "status": "passed"}
    ]


def test_only_filters_parsed_cases_and_unmatched_is_usage(env, tmp_path):
    body = xml(
        '<testcase name="AC-13.adaptive" classname="com.whalerein.android.AdaptiveTest"/>'
        '<testcase name="AC-13.locale" classname="com.whalerein.android.AdaptiveTest"/>'
    )
    selected = tmp_path / "selected"
    result = android.run_android(
        Device(body=body), env, "android-ui", case_filter="AC-13.adaptive", output=selected
    )
    assert result.exit_code == 0
    assert [case["id"] for case in report(selected)["scopes"][0]["tests"]] == [
        "com.whalerein.android.AdaptiveTest::AC-13.adaptive"
    ]
    unmatched = tmp_path / "unmatched"
    result = android.run_android(
        Device(body=body), env, "android-ui", case_filter="missing", output=unmatched
    )
    assert result.exit_code == 2
    assert report(unmatched)["verification"]["error_code"] == "unmatched-case-filter"


def test_list_mode_uses_instrumentation_log_arguments(env, tmp_path):
    runner = Device(
        body=xml('<testcase name="listed" classname="com.whalerein.android.AndroidTest"/>')
    )
    output = tmp_path / "listed"
    result = android.run_android(runner, env, "android-ui", output=output, list_only=True)
    assert result.exit_code == 0
    command = next(call for call in runner.calls if call[0][0].endswith("gradlew"))[0]
    assert "-Pandroid.testInstrumentationRunnerArguments.log=true" in command
    assert report(output)["scopes"][0]["tests"] == [
        {"id": "com.whalerein.android.AndroidTest::listed", "status": "todo"}
    ]


def test_performance_requires_a_physical_device(env, tmp_path):
    runner = Device(emulator=True, body=xml(""))
    output = tmp_path / "performance"
    result = android.run_android(runner, env, "android-performance", output=output)
    assert result.exit_code == 3
    assert report(output)["verification"]["error_code"] == "android-physical-device-required"
    assert not any(call[0][0].endswith("gradlew") for call in runner.calls)


def test_no_discovered_cases_is_a_failure_not_a_pass(env, tmp_path):
    runner = Device(body=xml(""))
    output = tmp_path / "empty"
    result = android.run_android(runner, env, "android-ui", output=output)
    assert result.exit_code == 1
    assert report(output)["verification"]["error_code"] == "android-no-tests"


def test_requested_local_stack_must_be_available(env, tmp_path, monkeypatch):
    monkeypatch.setenv("WHALEREIN_ANDROID_LOCAL_STACK", "1")
    monkeypatch.delenv("WHALEREIN_ANDROID_ACCESS_URL", raising=False)
    output = tmp_path / "stack"
    result = android.run_android(Device(), env, "android-integration", output=output)
    assert result.exit_code == 3
    assert report(output)["verification"]["error_code"] == "android-local-stack-unavailable"


def test_task_overrides_keep_device_suites_configurable(monkeypatch):
    monkeypatch.setenv("WHALEREIN_ANDROID_INTEGRATION_TASK", ":integration:connectedCheck")
    monkeypatch.setenv("WHALEREIN_ANDROID_PERFORMANCE_TASK", ":benchmark:connectedCheck")
    assert android.task_for("android-integration") == ":integration:connectedCheck"
    assert android.task_for("android-performance") == ":benchmark:connectedCheck"
    monkeypatch.delenv("WHALEREIN_ANDROID_INTEGRATION_TASK")
    assert android.task_for("android-integration") == ":app:connectedNoGmsDebugAndroidTest"


def test_targets_expose_explicit_device_suites():
    project = load_projects(ROOT)
    assert targets.resolve("test", "android-ui", project).kind == "android"
    names = set(targets.available("test", project))
    assert {"android-ui", "android-integration", "android-performance"}.issubset(names)
    unit = {
        name for name, target in targets.available("test", project).items() if target.kind == "unit"
    }
    assert not ({"android-ui", "android-integration", "android-performance"} & unit)


def test_ci_run_binds_the_report_to_the_checked_out_commit(env, tmp_path, monkeypatch):
    monkeypatch.setenv("GITHUB_ACTIONS", "true")
    runner = Device(
        body=xml('<testcase name="passes" classname="com.whalerein.android.AndroidTest"/>')
    )
    output = tmp_path / "ci-revision"
    result = android.run_android(runner, env, "android-ui", output=output)
    assert result.exit_code == 0
    sources = report(output)["verification"]["sources"]["public"]
    revision = subprocess.check_output(
        ["git", "-C", str(env), "rev-parse", "HEAD"], text=True
    ).strip()
    assert sources["mode"] == "commit"
    assert sources["revision"] == revision
    assert report(output)["configuration"]["suite"] == "android-ui"


def test_cli_routes_android_targets_to_the_device_runner(monkeypatch, capsys, tmp_path):
    def fake(runner, root, target, *, case_filter, output, list_only):
        assert (target, case_filter, output, list_only) == (
            "android-ui",
            "case",
            tmp_path / "run",
            False,
        )

        def execute(context):
            context.record_scope(
                {
                    "suite_id": target,
                    "project": target,
                    "kind": "android",
                    "completeness": "complete",
                    "tests": [{"id": "case::chosen", "status": "passed"}],
                }
            )

        return run_suite(Invocation("local", target, (), output=output), execute)

    from repo.runners import android as runner

    monkeypatch.setattr(runner, "run_android", fake)
    assert (
        cli.main(
            [
                "test",
                "android-ui",
                "--only",
                "case",
                "--output",
                str(tmp_path / "run"),
                "--json",
            ]
        )
        == 0
    )
    payload = json.loads(capsys.readouterr().out)
    assert payload["data"]["summary"]["statuses"] == {"passed": 1}
