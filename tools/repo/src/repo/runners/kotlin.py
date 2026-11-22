"""JDK-only Kotlin/JVM execution through the standalone Android state build.

The state module is an included Gradle build under ``apps/android/state``. It
never applies an Android plugin, so these commands compile and test with a JDK
alone. The build registers a JUnit condition that selects a literal case
substring before any test body runs and can disable every body for discovery.
Case identity comes from Gradle's JUnit XML: ``classname::display name``.
"""

from __future__ import annotations

import os
import subprocess
import xml.etree.ElementTree as ET
from pathlib import Path

from ..diagnostics import redact, sanitize
from .coverage import THRESHOLD, CoverageResult

DOM_WRAPPER = "state"
FILTER_PROPERTY = "-PcaseFilter="
LIST_ARGUMENT = "-PcaseList=true"
JACOCO_XML = Path("build/reports/jacoco/test/jacocoTestReport.xml")


def _revision(root: Path) -> str:
    try:
        result = subprocess.run(
            ["git", "-C", str(root), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError):
        return ""
    return result.stdout.strip()


def _gradle(project) -> tuple[str, Path]:
    wrapper = project.path.parent / ("gradlew.bat" if os.name == "nt" else "gradlew")
    return str(wrapper), project.path.parent


def parse_junit_files(paths) -> list[dict]:
    """Parse JUnit XML files, including a wrapped ``testsuites`` document."""
    cases = {}
    for path in sorted(Path(path) for path in paths):
        try:
            root = ET.parse(path).getroot()
        except (OSError, ET.ParseError):
            continue
        suites = []
        if root.tag == "testsuite":
            suites.append(root)
        suites.extend(root.iter("testsuite"))
        for suite in suites:
            fallback = suite.get("name") or ""
            for case in suite.iter("testcase"):
                name = case.get("name") or ""
                classname = case.get("classname") or fallback
                if not name:
                    continue
                identity = f"{classname}::{name}" if classname else name
                outcome = "passed"
                message = None
                node = case.find("failure")
                if node is None:
                    node = case.find("error")
                if node is not None:
                    outcome = "failed"
                    message = node.get("message") or node.get("type")
                elif case.find("skipped") is not None:
                    outcome = "skipped"
                duration = case.get("time")
                entry = {"id": identity, "status": outcome}
                if duration is not None:
                    try:
                        entry["duration_ms"] = round(float(duration) * 1000, 3)
                    except ValueError:
                        pass
                if message:
                    entry["diagnostics"] = [{"message": sanitize(message)}]
                cases.setdefault(identity, entry)
    return [cases[key] for key in sorted(cases)]


def parse_junit_results(directory: Path) -> list[dict]:
    return parse_junit_files(Path(directory).rglob("*.xml"))


def parse_jacoco_packages(path: Path) -> list[dict]:
    """Return per-package JaCoCo line counters from the XML report."""
    root = ET.parse(path).getroot()
    packages = []
    for package in root.iter("package"):
        name = package.get("name") or ""
        counter = next(
            (item for item in package.findall("counter") if item.get("type") == "LINE"),
            None,
        )
        if counter is None:
            continue
        covered = int(counter.get("covered") or 0)
        missed = int(counter.get("missed") or 0)
        packages.append(
            {
                "name": name.replace("/", "."),
                "covered": covered,
                "total": covered + missed,
            }
        )
    return sorted(packages, key=lambda entry: entry["name"])


def collect(project, runner, execute=False, only=None, coverage=False):
    scope = {
        "project": project.name,
        "tests": [],
        "coverage": [],
        "completeness": "incomplete",
        "note": "JDK-only Gradle test; case IDs come from JUnit XML.",
        "exit_code": None,
        "revision": _revision(project.root),
    }
    results = project.path / "build" / "test-results" / "test"
    # Remove only previous result documents. Gradle's binary working files are
    # part of the task output and must stay owned by the test task.
    if results.is_dir():
        for stale in results.rglob("*.xml"):
            stale.unlink(missing_ok=True)
    jacoco = project.path / JACOCO_XML
    jacoco.unlink(missing_ok=True)
    wrapper, cwd = _gradle(project)
    # The repository contract executes every selected case. cleanTest also
    # prevents a stale or partially written prior result from being reused.
    argv = [wrapper, "-p", DOM_WRAPPER, "cleanTest", "test", "--console=plain"]
    if only is not None and execute:
        argv.append(FILTER_PROPERTY + only)
    if not execute:
        argv.append(LIST_ARGUMENT)
    elif coverage:
        argv.append("jacocoTestCoverageVerification")
    result = runner.run(argv, cwd, quiet=True, timeout=1800)
    scope["exit_code"] = result.returncode
    cases = parse_junit_results(results)
    if not execute:
        if only is not None:
            cases = [case for case in cases if only in case["id"]]
        for case in cases:
            case["status"] = "not_run"
        if cases:
            for case in cases:
                case["id"] = redact(case["id"])
            scope["tests"] = cases
            scope["completeness"] = "complete" if result.returncode == 0 else "incomplete"
        elif only is not None and result.returncode == 0:
            # Discovery succeeded but no case matched; the generic unit flow
            # reports the unmatched selector as invalid usage.
            scope["completeness"] = "complete"
        else:
            scope["errors"] = [sanitize(result.tail()) or "No JUnit cases discovered"]
        return _coverage(scope, project, coverage)
    if only is not None:
        # The condition disables non-matching cases before execution. Skipped
        # cases from that condition are not selected evidence; only observed
        # (non-skipped) cases count. Genuinely skipped selected cases therefore
        # require a full unfiltered run to report their skip.
        cases = [case for case in cases if case["status"] != "skipped"]
        if not cases and result.returncode == 0:
            scope["completeness"] = "complete"
            return _coverage(scope, project, coverage)
    if not result.ok and not cases:
        scope["errors"] = [sanitize(result.tail()) or "Native test execution failed"]
    for case in cases:
        case["id"] = redact(case["id"])
    if cases:
        scope["tests"] = cases
        scope["completeness"] = (
            "complete"
            if result.returncode in (0, 1)
            and all(case["status"] in {"passed", "failed", "skipped"} for case in cases)
            else "incomplete"
        )
    elif result.ok:
        scope["errors"] = ["No JUnit test cases found"]
    return _coverage(scope, project, coverage)


def _coverage(scope, project, coverage):
    if not coverage:
        return scope
    report = project.path / JACOCO_XML
    try:
        packages = parse_jacoco_packages(report)
    except (OSError, ET.ParseError, ValueError):
        packages = []
    if not packages:
        measured = CoverageResult(
            project.name,
            "kotlin",
            project.rel,
            error="Missing or invalid JaCoCo XML report",
        )
        entry = measured.as_dict()
        entry.update(unit="lines", phase="test_execution")
        scope["coverage"].append(entry)
        return scope
    for package in packages:
        total = package["total"]
        percent = package["covered"] / total * 100.0 if total else 100.0
        measured = CoverageResult(
            package["name"],
            "kotlin",
            project.rel,
            covered=package["covered"],
            total=total,
            percent=percent,
            threshold=THRESHOLD,
        )
        entry = measured.as_dict()
        entry.update(unit="lines", phase="test_execution")
        scope["coverage"].append(entry)
    return scope


def steps(project, action):
    from ..toolchain import Step

    if action != "check":
        return []
    wrapper, cwd = _gradle(project)
    return [
        Step(
            f"{project.id}: testClasses",
            (wrapper, "-p", DOM_WRAPPER, "testClasses", "--console=plain"),
            cwd,
        )
    ]
