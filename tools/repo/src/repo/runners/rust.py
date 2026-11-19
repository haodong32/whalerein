"""Collect Rust test binaries independently to preserve target identities."""

import json
import re
import tomllib

from ..diagnostics import sanitize
from .coverage import CoverageResult, extract_json, parse_llvm_cov_json


def _execution_case_name(target, name):
    if target == "doctest":
        # Rustdoc decorates compile-only outcomes but omits the suffix in --list.
        return re.sub(r"(?<=\)) - compile(?: fail)?$", "", name)
    return name


def _execution_filters(target, names, selected):
    if len(names) == len(selected):
        return []
    if target != "doctest":
        return ["--exact", *selected]
    # Rustdoc splits its test arguments on whitespace. Use only native tokens
    # that select a listed case without also matching any unselected example.
    excluded = set(names) - selected.keys()
    filters = []
    for name in selected:
        token = next(
            (
                part
                for part in name.split()
                if not part.startswith("-") and not any(part in other for other in excluded)
            ),
            None,
        )
        if token is None:
            return None
        filters.append(token)
    return list(dict.fromkeys(filters))


def collect(project, runner, execute, only=None):
    scope = {
        "project": project.name,
        "tests": [],
        "completeness": "incomplete",
        "exit_code": None,
        "note": "Compiled test targets and enabled library doctests; current configuration only.",
    }
    build = runner.run(
        [
            "cargo",
            "test",
            "--manifest-path",
            str(project.manifest),
            "--no-run",
            "--message-format=json",
        ],
        project.path,
        quiet=True,
        timeout=1800,
    )
    scope["exit_code"] = build.returncode
    if not build.ok:
        scope["errors"] = [sanitize(build.tail()) or "Rust test compilation failed"]
        return scope
    complete = True
    try:
        artifacts = [json.loads(line) for line in build.stdout.splitlines()]
        binaries = sorted(
            {
                (e["target"]["name"], e["executable"])
                for e in artifacts
                if e.get("reason") == "compiler-artifact"
                and e.get("profile", {}).get("test")
                and e.get("executable")
            }
        )
    except (ValueError, KeyError, TypeError):
        scope["errors"] = ["Malformed Cargo test artifact stream"]
        return scope
    targets = [(target, [binary], [binary]) for target, binary in binaries]
    manifest = tomllib.loads(project.manifest.read_text())
    library = manifest.get("lib", {})
    if library.get("doctest", True) and (
        (project.path / library.get("path", "src/lib.rs")).exists()
    ):
        command = ["cargo", "test", "--manifest-path", str(project.manifest), "--doc", "--"]
        targets.append(("doctest", command, command))
    for index, (target, list_command, run_command) in enumerate(targets):
        listed = runner.run(
            [*list_command, "--list", "--format=terse"], project.path, quiet=True, timeout=1800
        )
        names = [line[:-6] for line in listed.stdout.splitlines() if line.endswith(": test")]
        if not listed.ok or len(names) != len(set(names)):
            complete = False
        cases = {name: {"id": target + "::" + name, "status": "not_run"} for name in names}
        if only:
            cases = {name: case for name, case in cases.items() if only in case["id"]}
            if not cases and listed.returncode != 130:
                continue
        if listed.returncode == 130:
            scope["exit_code"] = 130
            scope["tests"].extend(cases.values())
            scope["not_run_targets"] = [t[0] for t in targets[index:]]
            break
        if execute:
            filters = _execution_filters(target, names, cases)
            if filters is None:
                scope["errors"] = [
                    "Rustdoc cannot isolate this selection; run the complete doctest target"
                ]
                scope["tests"].extend(cases.values())
                complete = False
                break
            if getattr(runner, "progress", None):
                runner.progress.event(
                    {
                        "event": "detail",
                        "description": f"Rust target · {target}",
                        "total": len(cases),
                    }
                )
            result = runner.run(
                [
                    *run_command,
                    "--test-threads=1",
                    "--format=pretty",
                    *filters,
                ],
                project.path,
                quiet=True,
                timeout=1800,
            )
            scope["exit_code"] = result.returncode or scope["exit_code"]
            for line in result.stdout.splitlines():
                match = re.fullmatch(r"test (.+) \.\.\. (ok|FAILED|ignored)(?:, .*)?", line)
                if match and (name := _execution_case_name(target, match[1])) in cases:
                    cases[name]["status"] = {
                        "ok": "passed",
                        "FAILED": "failed",
                        "ignored": "skipped",
                    }[match[2]]
            # libtest emits one captured-output block per failed test.
            blocks = re.finditer(
                r"^---- (.+) stdout ----\n(.*?)"
                r"(?=^---- .+ stdout ----|^failures:\s*$|^test result:|\Z)",
                result.stdout,
                re.MULTILINE | re.DOTALL,
            )
            for block in blocks:
                name = _execution_case_name(target, block[1])
                if name in cases and cases[name]["status"] == "failed":
                    cases[name]["diagnostics"] = [
                        {"phase": "test", "message": sanitize(block[2].strip())}
                    ]
            # Panic headers can be on stderr without a captured stdout block.
            # Preserve only source locations in that case, never assertion values.
            for panic in re.finditer(
                r"thread '([^'\n]+)'(?: \([^\n)]*\))? panicked at "
                r"([A-Za-z0-9_./\\-]+\.rs:\d+(?::\d+)?):",
                result.output,
            ):
                if panic[1] in cases and cases[panic[1]]["status"] == "failed":
                    cases[panic[1]].setdefault(
                        "diagnostics", [{"phase": "test", "message": "Rust panic at " + panic[2]}]
                    )
            summary = re.search(
                r"test result: .*? (\d+) passed; (\d+) failed; (\d+) ignored;", result.stdout
            )
            if (
                not summary
                or sum(map(int, summary.groups())) != len(cases)
                or any(c["status"] == "not_run" for c in cases.values())
            ):
                complete = False
        scope["tests"].extend(cases.values())
        if execute and result.returncode == 130:
            scope["not_run_targets"] = [t[0] for t in targets[index + 1 :]]
            complete = False
            break
    scope["completeness"] = "complete" if complete else "incomplete"
    scope["collection_complete"] = complete
    scope["exclusions"] = ["benchmarks", "fuzz campaigns", "other feature/platform configurations"]
    return scope


def measure(project, runner):
    result = CoverageResult(name=project.name, kind="rust", path=project.rel)
    probe = runner.run(["cargo", "llvm-cov", "--version"], project.root, timeout=30, quiet=True)
    if probe.missing:
        result.error = (
            "cargo-llvm-cov is not installed; install it with `cargo install cargo-llvm-cov`"
        )
        return result
    if not probe.ok:
        result.error = "cargo llvm-cov is unavailable: " + (probe.tail(300) or "unknown error")
        return result
    # Keep llvm-cov's package cleanup enabled so it measures only current binaries.
    # Match the ordinary Rust runner's serial test execution.
    run = runner.run(
        [
            "cargo",
            "llvm-cov",
            "--package",
            project.name,
            "--json",
            "--summary-only",
            "--",
            "--test-threads=1",
        ],
        project.root,
        timeout=1800,
    )
    if not run.ok:
        failures = sorted(
            set(
                re.findall(
                    r"(?m)^test ([A-Za-z_][A-Za-z0-9_:]*) \.\.\. FAILED(?:,.*)?$", run.output
                )
            )
        )
        if failures:
            detail = "failed tests: " + ", ".join(failures[:20])
        else:
            # libtest prints captured test output after failures; exposing a raw
            # tail here could leak message contents. Report only the stage.
            detail = (
                "failed test names unavailable (compile or runner failure); "
                "captured output suppressed"
            )
        result.error = (
            f"cargo llvm-cov exited {run.returncode}; coverage is not trustworthy: {detail}"
        )
        return result
    try:
        covered, total, percent = parse_llvm_cov_json(extract_json(run.stdout))
    except ValueError as exc:
        result.error = f"could not parse cargo llvm-cov output: {exc}; {run.tail(500)}"
        return result
    result.covered, result.total, result.percent = (covered, total, percent)
    return result


def steps(project, action):
    from ..toolchain import Step

    base = ("cargo",)
    package = ("--manifest-path", str(project.manifest), "--package", project.name)
    if action == "fmt":
        commands = [(*base, "fmt", *package)]
    elif action in ("check", "quick"):
        commands = [(*base, "fmt", *package, "--", "--check")]
        if action == "check":
            commands.append((*base, "clippy", *package, "--all-targets", "--", "-D", "warnings"))
    else:
        commands = [(*base, "build", *package)]
    return [Step(f"{project.id}: {command[1]}", command, project.path) for command in commands]
