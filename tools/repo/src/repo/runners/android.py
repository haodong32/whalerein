"""Explicit Android device suites backed by Gradle.

These targets never participate in ordinary unit selection. A missing SDK,
device or requested local stack is recorded as unavailable evidence with a
prerequisite exit status instead of a silent skip or a passing empty suite.
"""

from __future__ import annotations

import os
import shutil
from pathlib import Path

from ..sources import SourceSelection
from ..verification import Invocation, VerificationError, run_suite
from .kotlin import parse_junit_files

SUITES = {
    "android-ui": {
        "task": ":app:connectedNoGmsDebugAndroidTest",
        "description": "Compose fixture instrumentation",
    },
    "android-integration": {
        "task": ":app:connectedNoGmsDebugAndroidTest",
        "description": "native bridge, Keystore and TLS device checks",
    },
    "android-performance": {
        "task": ":benchmark:connectedNoGmsReleaseAndroidTest",
        "description": "Macrobenchmark on a physical device",
    },
}
TASK_OVERRIDES = {
    "android-integration": "WHALEREIN_ANDROID_INTEGRATION_TASK",
    "android-performance": "WHALEREIN_ANDROID_PERFORMANCE_TASK",
}


def task_for(target: str) -> str:
    override = os.environ.get(TASK_OVERRIDES.get(target, ""), "")
    return override or SUITES[target]["task"]


def _sdk(root: Path) -> Path | None:
    for name in ("ANDROID_HOME", "ANDROID_SDK_ROOT"):
        value = os.environ.get(name)
        if value and Path(value).is_dir():
            return Path(value)
    local = root / "apps/android/local.properties"
    try:
        for line in local.read_text().splitlines():
            key, _, value = line.partition("=")
            if key.strip() == "sdk.dir" and value.strip() and Path(value.strip()).is_dir():
                return Path(value.strip())
    except OSError:
        pass
    return None


def _devices(runner, root: Path) -> list[str]:
    if runner.which("adb") is None:
        raise VerificationError("android-adb-unavailable", 3)
    result = runner.run(["adb", "devices", "-l"], root, quiet=True, timeout=30)
    if not result.ok:
        raise VerificationError("android-adb-unavailable", 3)
    devices = []
    for line in result.stdout.splitlines()[1:]:
        fields = line.split()
        if len(fields) >= 2 and fields[1] == "device":
            devices.append(fields[0])
    return devices


def _selected_device(runner, root: Path) -> str:
    devices = _devices(runner, root)
    requested = os.environ.get("ANDROID_SERIAL", "")
    if requested:
        if requested not in devices:
            raise VerificationError("android-device-unavailable", 3)
        return requested
    if not devices:
        raise VerificationError("android-device-unavailable", 3)
    if len(devices) > 1:
        raise VerificationError("android-device-selection-required", 3)
    return devices[0]


def _property(runner, root: Path, serial: str, name: str) -> str:
    result = runner.run(
        ["adb", "-s", serial, "shell", "getprop", name], root, quiet=True, timeout=30
    )
    return result.stdout.strip() if result.ok else ""


def _device_facts(runner, root: Path, serial: str) -> dict:
    page = runner.run(
        ["adb", "-s", serial, "shell", "getconf", "PAGE_SIZE"], root, quiet=True, timeout=30
    )
    page_size = page.stdout.strip() if page.ok else ""
    facts = {
        "serial": serial,
        "api": _property(runner, root, serial, "ro.build.version.sdk"),
        "release": _property(runner, root, serial, "ro.build.version.release"),
        "abi": _property(runner, root, serial, "ro.product.cpu.abi"),
        "page_size": page_size,
        "emulator": _property(runner, root, serial, "ro.kernel.qemu") == "1",
    }
    return {key: value for key, value in facts.items() if value not in ("", None)}


def _result_files(root: Path) -> list[Path]:
    base = root / "apps/android"
    return sorted(
        path
        for path in base.rglob("*.xml")
        if "androidTest-results" in path.parts and path.is_file()
    )


def _clean_results(root: Path):
    for directory in (root / "apps/android").rglob("androidTest-results"):
        if directory.is_dir():
            shutil.rmtree(directory, ignore_errors=True)


def _gradle_command(root: Path, target: str, *, list_only: bool) -> tuple[list[str], Path]:
    wrapper = root / ("apps/android/gradlew.bat" if os.name == "nt" else "apps/android/gradlew")
    if not wrapper.exists():
        raise VerificationError("android-gradle-unavailable", 3)
    argv = [str(wrapper), task_for(target), "--console=plain"]
    if list_only:
        argv.append("-Pandroid.testInstrumentationRunnerArguments.log=true")
    return argv, root / "apps/android"


def _local_stack() -> str | None:
    """An explicitly requested Cloud/Standalone stack must exist."""
    if os.environ.get("WHALEREIN_ANDROID_LOCAL_STACK") != "1":
        return None
    if not os.environ.get("WHALEREIN_ANDROID_ACCESS_URL"):
        raise VerificationError("android-local-stack-unavailable", 3)
    return os.environ["WHALEREIN_ANDROID_ACCESS_URL"]


def _run_platform(ctx, runner, root: Path, target: str, case_filter: str | None, list_only: bool):
    with ctx.phase("platform-preparation"):
        if _sdk(root) is None:
            raise VerificationError("android-sdk-unavailable", 3)
        serial = _selected_device(runner, root)
        facts = _device_facts(runner, root, serial)
        if target == "android-performance" and facts.get("emulator"):
            raise VerificationError("android-physical-device-required", 3)
        stack = _local_stack()
        ctx.record_artifact("android-device", facts)
        ctx.record_artifact("android-task", {"name": task_for(target), "serial": serial})
        if stack:
            ctx.record_artifact("android-local-stack", {"access_url": stack})
        argv, cwd = _gradle_command(root, target, list_only=list_only)
        _clean_results(root)
    environment = {"ANDROID_SERIAL": serial}

    if list_only:
        with ctx.phase("collection"):
            result = ctx.run(runner, argv, cwd, env=environment)
            cases = parse_junit_files(_result_files(root))
            selected = [
                case["id"] for case in cases if case_filter is None or case_filter in case["id"]
            ]
            if case_filter is not None and not selected:
                raise VerificationError("unmatched-case-filter", 2)
            if not selected:
                raise VerificationError("android-inventory-unavailable", 1)
            ctx.record_collection(selected)
            ctx.record_scope(
                {
                    "suite_id": target,
                    "project": target,
                    "kind": "android",
                    "language": "kotlin",
                    "completeness": "complete",
                    "exit_code": result.returncode,
                    "tests": [{"id": case, "status": "todo"} for case in selected],
                }
            )
            return

    with ctx.phase("execution"):
        result = ctx.run(runner, argv, cwd, env=environment)
        cases = parse_junit_files(_result_files(root))
        selected = [case for case in cases if case_filter is None or case_filter in case["id"]]
        if case_filter is not None and not selected:
            ctx.record_collection([])
            raise VerificationError("unmatched-case-filter", 2)
        if not selected:
            raise VerificationError("android-no-tests", 1)
        ctx.record_collection([case["id"] for case in selected])
        for case in selected:
            ctx.record_case(case["id"], case["status"])
        complete = result.returncode in (0, 1) and all(
            case["status"] in {"passed", "failed", "skipped"} for case in selected
        )
        ctx.record_scope(
            {
                "suite_id": target,
                "project": target,
                "kind": "android",
                "language": "kotlin",
                "completeness": "complete" if complete else "incomplete",
                "exit_code": result.returncode,
                "tests": [
                    {
                        "id": case["id"],
                        "status": case["status"],
                        **({"diagnostics": case["diagnostics"]} if case.get("diagnostics") else {}),
                    }
                    for case in selected
                ],
            }
        )
        if result.returncode == 130:
            raise KeyboardInterrupt
        if not complete:
            raise VerificationError("incomplete-android-result")
        if result.returncode != 0:
            raise VerificationError("android-tests-failed")
        if not any(case["status"] == "passed" for case in selected):
            raise VerificationError("no-android-case-passed")


def run_android(
    runner,
    root: Path,
    target: str,
    *,
    case_filter: str | None = None,
    output: Path | None = None,
    list_only: bool = False,
):
    ci = os.environ.get("GITHUB_ACTIONS") == "true"
    timeout = 5400 if target == "android-performance" else 3600
    return run_suite(
        Invocation(
            "ci" if ci else "local",
            target,
            (SourceSelection("public", root, revision="HEAD" if ci else None),),
            case_filter=case_filter,
            output=output,
            timeout=timeout,
            configuration={"list_only": list_only, "task": task_for(target)},
        ),
        lambda ctx: _run_platform(ctx, runner, root, target, case_filter, list_only),
    )
