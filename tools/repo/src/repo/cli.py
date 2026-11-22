"""One command boundary for repository operations and structured results."""

from __future__ import annotations

import argparse
import re
import signal
import sys
from collections.abc import Sequence
from datetime import UTC, datetime
from pathlib import Path
from uuid import uuid4

from . import __version__, doctor, proto, targets, testing, toolchain
from .benchmark import runner as benchmark
from .diagnostics import sanitize
from .errors import RepoError, UsageError
from .output import Output
from .paths import find_root
from .process import Runner
from .projects import load_projects
from .reports.html import render
from .reports.model import atomic_write, read_report
from .verification import VerificationError


class Parser(argparse.ArgumentParser):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, allow_abbrev=False, **kwargs)

    def error(self, message):
        raise UsageError(f"{message}\nRun '{self.prog} --help' for usage.")


def common(suppress=False):
    parser = argparse.ArgumentParser(add_help=False)
    default = argparse.SUPPRESS if suppress else False
    parser.add_argument(
        "--json",
        dest="json_mode",
        action="store_true",
        default=default,
        help="write one JSON result to stdout; progress and diagnostics go to stderr",
    )
    parser.add_argument(
        "-v",
        "--verbose",
        action="store_true",
        default=default,
        help="show additional bounded, sanitized diagnostics",
    )
    return parser


def build_parser(projects=()):
    parser = Parser(
        prog="./repo",
        description="WhaleRein repository engineering tasks. Run from the repository root.",
        parents=[common()],
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""Examples:
  ./repo check core
  ./repo test core --only reconnect
  ./repo test protocol-vectors
  ./repo doctor integration

Run './repo COMMAND --help' for command options.
Run './repo projects' for project IDs and supported operations.
Full reference: tools/repo/README.md""",
    )
    parser.add_argument("--version", action="version", version=f"repo {__version__}")
    commands = parser.add_subparsers(
        dest="command", required=True, title="commands", metavar="COMMAND"
    )
    descriptions = {
        "projects": "List registered project IDs, paths, languages and supported operations.",
        "doctor": "Check required tools and versions without installing them.",
        "fmt": "Format source files in place for applicable projects.",
        "check": "Check formatting and static analysis without modifying source or running tests.",
        "build": "Build applicable projects.",
        "test": "Run native unit tests by default; integration tests are opt-in.",
        "bench": "Measure the plugin–Access–client path under controlled network conditions.",
        "bench-compare": "Compare saved benchmark runs under the predeclared protocol.",
        "report": "Render saved JSON test results as offline HTML without running tests.",
        "recover": "Clean up resources still owned by an abandoned verification run.",
        "proto": "Generate committed Go/JS protobuf bindings and Go/Node/Rust transport profiles.",
    }
    examples = {
        "projects": "  ./repo projects\n  ./repo projects --json",
        "doctor": "  ./repo doctor\n  ./repo doctor integration",
        "fmt": "  ./repo fmt\n  ./repo fmt core",
        "check": "  ./repo check\n  ./repo check repo\n  ./repo check proto",
        "build": "  ./repo build\n  ./repo build access",
        "test": (
            "  ./repo test core --only reconnect\n"
            "  ./repo test core --coverage\n"
            "  ./repo test core --list\n"
            "  ./repo test android-state --only AC-24.focused\n"
            "  ./repo test --project android-state --json\n"
            "  ./repo test android-ui --only AC-12.rich\n"
            "  ./repo test protocol-vectors\n"
            "  ./repo test chain --parallel 2\n"
            "  ./repo test integration"
        ),
        "report": "  ./repo report .data/runs/my-check",
        "recover": "  ./repo recover .data/runs/interrupted-check",
        "proto": "  ./repo proto",
        "bench": (
            "  ./repo bench\n"
            "  ./repo bench --attempts 3 --warmup 0 --output .data/benchmarks/smoke-suite\n"
            "  ./repo bench --network jitter --link client\n"
            "  ./repo bench --scenario recovery\n"
            "  ./repo bench --scenario mixed-load"
        ),
        "bench-compare": (
            "  ./repo bench-compare --baseline .data/benchmarks/a --candidate .data/benchmarks/b\n"
            "  ./repo bench-compare -b a1 -b a2 -c b1 -c b2 --json"
        ),
    }
    for name, description in descriptions.items():
        notes = ""
        if name == "test":
            notes = (
                "Bare test selects unit targets. Integration targets are explicit.\n"
                "Select a leaf for --only or --list. Use --coverage for unfiltered unit tests.\n"
                "Use --parallel for Linux integration execution (1–8 shards).\n\n"
            )
        elif name == "doctor":
            notes = "Without a target, probe the basic local toolchain.\n\n"
        sub = commands.add_parser(
            name,
            parents=[common(True)],
            help=description,
            description=description,
            formatter_class=argparse.RawDescriptionHelpFormatter,
            epilog=notes
            + targets.describe(name, projects)
            + "Examples:\n"
            + examples[name]
            + "\n\nFull reference: tools/repo/README.md",
        )
        if name in ("fmt", "check", "build", "test", "doctor"):
            sub.add_argument("target", nargs="?", metavar="TARGET", help="scope to select")
        if name == "test":
            sub.add_argument(
                "--project",
                metavar="ID",
                help="unit project ID; equivalent to the TARGET positional argument",
            )
            sub.add_argument(
                "--integration-timeout",
                type=int,
                metavar="SECONDS",
                help="Linux integration run deadline in seconds (default: 7200)",
            )
            sub.add_argument(
                "--only",
                metavar="TEXT",
                help="literal, case-sensitive substring of case IDs; no matches fail",
            )
            sub.add_argument(
                "--coverage",
                action="store_true",
                help="measure unit coverage and enforce the 90%% gate per first-party scope",
            )
            sub.add_argument(
                "--list",
                dest="list_only",
                action="store_true",
                help="discover case IDs without executing test bodies",
            )
            sub.add_argument(
                "--parallel",
                type=int,
                default=None,
                metavar="N",
                help="integration only: run N independent containers with deterministic "
                "hash shards (1-8, default 1); incompatible with --list",
            )
            sub.add_argument(
                "--output",
                type=Path,
                metavar="DIR",
                help="save report.json and report.html (default: unique .data/runs/ directory); "
                "must be a fresh run directory",
            )
            sub.add_argument(
                "--shard",
                metavar="INDEX/COUNT",
                help="run one 1-based shard of the full ios-ui plan (2–8 shards)",
            )
            sub.add_argument(
                "--simulator",
                metavar="UDID",
                help="select a native iOS simulator by UDID",
            )
        if name in {"report", "recover"}:
            sub.add_argument(
                "directory",
                type=Path,
                metavar="RUN_DIRECTORY",
                help="verification run directory",
            )
        if name == "bench":
            sub.add_argument(
                "--attempts",
                type=int,
                default=30,
                metavar="N",
                help="measured attempts after warmup (default: 30)",
            )
            sub.add_argument(
                "--warmup",
                type=int,
                default=3,
                metavar="N",
                help="warmup attempts excluded from results (default: 3)",
            )
            sub.add_argument(
                "--delay",
                type=int,
                default=0,
                metavar="MS",
                help="fixture delay before the first source increment (default: 0 ms)",
            )
            sub.add_argument(
                "--cadence",
                type=int,
                default=100,
                metavar="MS",
                help="spacing between scripted visible updates in milliseconds "
                "(default: 100); values at or below the product's 20 ms coalescing "
                "window expose batching",
            )
            sub.add_argument(
                "--scenario",
                choices=("watch", "recovery", "mixed-load"),
                help="run an individual workload instead of the default suite",
            )
            sub.add_argument(
                "--network",
                choices=("normal", "high-rtt", "jitter", "loss"),
                help="override the selected workload network profile",
            )
            sub.add_argument(
                "--link",
                choices=("host", "client", "both"),
                help="endpoint link for network shaping (default: host, client for recovery)",
            )
            sub.add_argument(
                "--deadline",
                type=int,
                default=10000,
                metavar="MS",
                help="per-attempt deadline from the first source emission (default: 10000 ms)",
            )
            sub.add_argument(
                "--observation-window",
                type=int,
                default=1000,
                metavar="MS",
                help="window after the final reply used to detect duplicates (default: 1000)",
            )
            sub.add_argument(
                "--seed",
                type=int,
                default=0,
                metavar="N",
                help="scenario seed recorded in the report (default: 0)",
            )
            sub.add_argument(
                "--concurrency",
                type=int,
                default=1,
                metavar="N",
                help="active conversations; only the sequential value 1 is supported",
            )
            sub.add_argument(
                "--benchmark-timeout",
                type=int,
                metavar="SECONDS",
                help="per-case execution deadline in seconds (default: 900; build is separate)",
            )
            sub.add_argument(
                "--output",
                type=Path,
                metavar="DIR",
                help="save suite.json or benchmark.json under a new directory "
                "(default: unique .data/benchmarks/ directory)",
            )
        if name == "bench-compare":
            sub.add_argument(
                "--vary",
                choices=("build", "network"),
                default="build",
                help="the only variable allowed to differ between arms (default: build)",
            )
            sub.add_argument(
                "-b",
                "--baseline",
                action="append",
                type=Path,
                required=True,
                metavar="DIR",
                help="saved baseline run directory or benchmark.json (repeatable, ordered)",
            )
            sub.add_argument(
                "-c",
                "--candidate",
                action="append",
                type=Path,
                required=True,
                metavar="DIR",
                help="saved candidate run directory or benchmark.json (repeatable, ordered)",
            )
            sub.add_argument(
                "--output",
                type=Path,
                metavar="FILE",
                help="also save the comparison as JSON at this path",
            )
    return parser


def dispatch(args, root, runner):
    if args.command == "recover":
        from .runners.integration import recover_container
        from .runners.ios import recovery_callbacks
        from .verification import recover_run

        directory = args.directory.resolve()
        callbacks = {
            "docker-container": lambda name, ownership, deadline: recover_container(
                runner, root, name, ownership, deadline
            ),
            **recovery_callbacks(runner, directory),
        }
        outcome = recover_run(directory, callbacks)
        runner.out.info("Recovery: " + outcome["status"])
        return outcome, int(outcome["status"] != "complete")
    if args.command == "report":
        directory = args.directory.resolve()
        source = directory / "report.json"
        if not source.is_file():
            raise UsageError(f"expected a run directory containing report.json: {directory}")
        destination = directory / "report.html"
        atomic_write(destination, render(read_report(source)))
        runner.out.info("Report: " + str(destination))
        return {"json": str(source), "html": str(destination)}, 0
    if args.command == "bench-compare":
        from .benchmark import compare as benchmark_compare
        from .benchmark import summary as benchmark_summary

        result = benchmark_compare.compare(
            [benchmark_compare.load(path) for path in args.baseline],
            [benchmark_compare.load(path) for path in args.candidate],
            vary=args.vary,
        )
        for line in benchmark_summary.comparison(result).splitlines():
            runner.out.info(line)
        if args.output:
            benchmark_compare.save(args.output, result)
            runner.out.info("Comparison: " + str(args.output))
        return {"comparison": result, "output": str(args.output) if args.output else None}, 0
    if args.command == "bench":
        from .benchmark import summary as benchmark_summary

        request = benchmark.BenchRequest(
            attempts=args.attempts,
            warmup=args.warmup,
            delay_ms=args.delay,
            cadence_ms=args.cadence,
            workflow=args.scenario or "watch",
            network_profile=args.network or "normal",
            link=args.link or ("client" if args.scenario == "recovery" else "host"),
            deadline_ms=args.deadline,
            observation_window_ms=args.observation_window,
            seed=args.seed,
            concurrency=args.concurrency,
            timeout=args.benchmark_timeout,
        )
        directory = args.output or root / ".data/benchmarks" / (
            datetime.now(UTC).strftime("%Y%m%dT%H%M%S") + "-" + uuid4().hex[:8]
        )
        if args.scenario is None and args.network is None and args.link is None:
            runner.out.info("Benchmark: preparing one build for all workloads")
            result = benchmark.run_matrix(root, runner, request, directory)
            cases = result.document["cases"]
            passed = sum(case.get("exit_code") == 0 for case in cases)
            runner.out.info(
                f"Benchmark suite: {result.document['status']}, {passed}/{len(cases)} cases passed"
            )
            for error in result.errors:
                runner.out.info(f"Benchmark suite: {error}")
            runner.out.info("Report: " + str(result.path))
            return {**result.document, "report": str(result.path)}, result.exit_code
        runner.out.info("Benchmark: preparing isolated container")
        result = benchmark.run(root, runner, request, directory)
        text = benchmark_summary.render(result.document)
        for line in text.splitlines():
            runner.out.info(line)
        if result.path:
            runner.out.info("Report: " + str(result.path))
        return {
            "report": str(result.path) if result.path else None,
            "status": result.document["status"]["state"],
            "outcomes": result.document["outcomes"],
            "latency": result.document["latency"]["samples"],
            "streaming": result.document.get("streaming"),
            "diagnostics": result.document.get("diagnostics", {}),
            "network_probe": result.document["environment"].get("network_probe"),
            "snapshot": result.document["environment"]["artifacts"],
            "workflow_results": result.document.get("workflow_results", {}),
            "errors": [sanitize(entry) for entry in result.errors],
        }, result.exit_code
    if args.command == "proto" or (args.command == "check" and args.target == "proto"):
        return proto.run(runner, runner.out, root, check=args.command == "check"), 0
    projects = load_projects(root)
    if args.command == "projects":
        data = [p.as_dict() for p in projects]
        for p in data:
            runner.out.info(
                f"{p['id']:<12} {p['kind']:<8} {p['path']:<26} {', '.join(p['operations'])}"
            )
        return {"projects": data}, 0
    if args.command == "test" and args.project:
        if args.target:
            raise UsageError("select the test target positionally or with --project, not both")
        args.target = args.project
    target = targets.resolve(args.command, args.target, projects)
    if args.command == "doctor":
        data = doctor.run_doctor(runner, doctor.specs_for_target(target))
        runner.out.info(doctor.format_doctor(data))
        return data, 0 if data["ok"] else 3
    if args.command == "test":
        if (args.only is not None or args.list_only) and (target is None or not target.leaf):
            raise UsageError("--only and --list require a leaf target")
        if args.list_only and target and target.project and target.project.kind == "node":
            raise UsageError("Node static case listing is unsupported; select a different target")
        if args.only is not None and not args.only.strip():
            raise UsageError("--only requires nonempty text")
        kind = target.kind if target else "unit"
        if args.simulator is not None and kind != "ios":
            raise UsageError("--simulator requires an iOS target")
        shard = None
        if args.shard is not None:
            if kind != "ios" or target.name != "ios-ui":
                raise UsageError("--shard requires the ios-ui target")
            if args.only is not None:
                raise UsageError("--shard cannot combine with --only")
            match = re.fullmatch(r"([1-8])/([2-8])", args.shard)
            if match is None or int(match[1]) > int(match[2]):
                raise UsageError(
                    "--shard requires INDEX/COUNT with 1 <= INDEX <= COUNT and 2 <= COUNT <= 8"
                )
            shard = (int(match[1]), int(match[2]))
        if kind == "android":
            if any(
                (
                    args.coverage,
                    args.parallel is not None,
                    args.integration_timeout is not None,
                )
            ):
                raise UsageError("Android device suites support --only, --list and --output")
            from .runners.android import run_android

            result = run_android(
                runner,
                root,
                target.name,
                case_filter=args.only,
                output=args.output,
                list_only=args.list_only,
            )
            return testing.saved_test_result(result, runner.out)
        if kind == "ios":
            if any(
                (
                    args.coverage,
                    args.parallel is not None,
                    args.integration_timeout is not None,
                )
            ):
                raise UsageError("iOS targets support --only, --list, --output, and --simulator")
            from .runners.ios import run_ios

            result = run_ios(
                runner,
                root,
                target.name,
                case_filter=args.only,
                output=args.output,
                simulator=args.simulator,
                list_only=args.list_only,
                shard=shard,
            )
            return testing.saved_test_result(result, runner.out)
        selected = [target.project] if target and target.project else projects
        request = testing.TestRequest(
            suite=kind,
            projects=tuple(p.id for p in selected) if kind == "unit" else (),
            only=args.only,
            group=target.name if kind == "integration" and target.name != "integration" else None,
            coverage=args.coverage,
            list_only=args.list_only,
            integration_timeout=args.integration_timeout,
            parallel=args.parallel,
        )
        request.validate()
        return testing.run(root, selected, runner, request, args.output)
    selected = [target.project] if target else projects
    return toolchain.run(selected, runner, args.command, explicit=target is not None)


def main(argv: Sequence[str] | None = None) -> int:
    arguments = list(sys.argv[1:] if argv is None else argv)
    out = Output(
        json_mode="--json" in arguments, verbose="--verbose" in arguments or "-v" in arguments
    )
    command = "repo"

    def terminate(signum, frame):
        raise KeyboardInterrupt

    previous_term = signal.signal(signal.SIGTERM, terminate)
    try:
        parser = build_parser(load_projects(find_root()) if "--help" in arguments else ())
        if not arguments:
            parser.print_help()
            return 0
        args, unknown = parser.parse_known_args(arguments)
        command = args.command
        if unknown:
            raise UsageError(
                f"unrecognized arguments: {' '.join(unknown)}\n"
                f"Run '{parser.prog} {command} --help' for usage."
            )
        out.json_mode, out.verbose = args.json_mode, args.verbose
        data, code = dispatch(args, find_root(), Runner(out=out))
        out.result(
            command,
            ok=code == 0,
            data=data,
            error={
                "code": {2: "usage_error", 3: "tool_missing"}.get(code, "command_failed"),
                "message": data.get("verification_error")
                or {
                    2: "No test cases match --only",
                    3: "Blocked by prerequisites; inspect results",
                    130: "Interrupted; inspect results",
                }.get(code, "Operation failed; inspect results"),
            }
            if code
            else None,
        )
        return code
    except VerificationError as exc:
        out.failure(command, UsageError(str(exc)) if exc.exit_code == 2 else RepoError(str(exc)))
        return exc.exit_code
    except RepoError as exc:
        out.failure(command, exc)
        return exc.exit_code
    except KeyboardInterrupt:
        out.result(command, ok=False, error={"code": "command_failed", "message": "interrupted"})
        return 130
    except Exception as exc:
        out.failure(command, exc)
        return 1
    finally:
        signal.signal(signal.SIGTERM, previous_term)


if __name__ == "__main__":  # pragma: no cover
    sys.exit(main())
