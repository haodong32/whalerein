"""One command boundary for repository operations and structured results."""

from __future__ import annotations

import argparse
import signal
import sys
from collections.abc import Sequence
from pathlib import Path

from . import __version__, doctor, proto, testing, toolchain
from .errors import RepoError, UsageError
from .output import Output
from .paths import find_root
from .process import Runner
from .projects import load_projects, select_projects
from .reports.html import render
from .reports.model import atomic_write, read_report


class Parser(argparse.ArgumentParser):
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


def build_parser():
    parser = Parser(
        prog="./repo",
        description="WhaleRein repository engineering tasks. Run from the repository root.",
        parents=[common()],
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""Examples:
  ./repo check --project core
  ./repo test --project repo --coverage
  ./repo test --suite integration --group protocol
  ./repo doctor --for test

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
        "report": "Render saved JSON test results as offline HTML without running tests.",
        "proto": "Generate committed Go/JS protocol bindings, or check their consistency.",
    }
    examples = {
        "projects": "  ./repo projects\n  ./repo projects --json",
        "doctor": "  ./repo doctor\n  ./repo doctor --for coverage",
        "fmt": "  ./repo fmt\n  ./repo fmt --project core --project cli",
        "check": "  ./repo check\n  ./repo check --project repo",
        "build": "  ./repo build\n  ./repo build --project access",
        "test": (
            "  ./repo test --project core\n"
            "  ./repo test --project core --only reconnect\n"
            "  ./repo test --project core --list\n"
            "  ./repo test --project repo --coverage\n"
            "  ./repo test --suite integration --group protocol\n"
            "  ./repo test --suite all"
        ),
        "report": "  ./repo report .data/runs/my-check/report.json --output /tmp/test-report.html",
        "proto": "  ./repo proto\n  ./repo proto --check",
    }
    for name, description in descriptions.items():
        notes = ""
        if name == "test":
            notes = (
                "Selection rules:\n"
                "  --project is unit-only; --group is integration-only.\n"
                "  Unit --only requires exactly one project; --only cannot use suite all.\n"
                "  --coverage cannot combine with --only, --list or suite integration.\n"
                "  With suite all, coverage measures only the unit portion.\n"
                "  --list may compile tests or build an image;\n"
                "  Node static listing is unsupported.\n\n"
            )
        elif name == "doctor":
            notes = (
                "Without --for, probe basic tools. This does not verify package installation,\n"
                "daemon readiness or deployment behavior.\n\n"
            )
        sub = commands.add_parser(
            name,
            parents=[common(True)],
            help=description,
            description=description,
            formatter_class=argparse.RawDescriptionHelpFormatter,
            epilog=notes
            + "Examples:\n"
            + examples[name]
            + "\n\nFull reference: tools/repo/README.md",
        )
        if name in ("fmt", "check", "build", "test"):
            sub.add_argument(
                "--project",
                action="append",
                metavar="ID",
                help="exact project ID (repeatable); default: all applicable projects; "
                "see ./repo projects",
            )
        if name == "doctor":
            sub.add_argument(
                "--for",
                dest="task",
                choices=("check", "build", "test", "coverage", "integration", "proto"),
                help="check prerequisites for this task (default: basic tools)",
            )
        if name == "test":
            sub.add_argument(
                "--suite",
                choices=("unit", "integration", "all"),
                default="unit",
                help="test suite to run (default: unit)",
            )
            sub.add_argument(
                "--group",
                choices=("protocol", "storage", "transport", "chain", "native-dsh"),
                help="select one integration group (default: all groups)",
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
                "--output",
                type=Path,
                metavar="DIR",
                help="save report.json and report.html (default: unique .data/runs/ directory); "
                "must not already contain a report",
            )
        if name == "report":
            sub.add_argument(
                "input",
                type=Path,
                metavar="REPORT_JSON",
                help="saved report.json from a test run",
            )
            sub.add_argument(
                "--output",
                type=Path,
                required=True,
                metavar="HTML_FILE",
                help="destination HTML file (required)",
            )
        if name == "proto":
            sub.add_argument(
                "--check",
                action="store_true",
                help="check binding consistency instead of writing generated files",
            )
    return parser


def dispatch(args, root, runner):
    if args.command == "report":
        atomic_write(args.output, render(read_report(args.input)))
        return {"html": str(args.output)}, 0
    if args.command == "proto":
        return proto.run(runner, runner.out, root, check=args.check), 0
    if args.command == "doctor":
        data = doctor.run_doctor(runner, doctor.specs_for(args.task))
        runner.out.info(doctor.format_doctor(data))
        return data, 0 if data["ok"] else 3
    projects = [] if args.command == "test" and args.suite == "integration" else load_projects(root)
    if args.command == "projects":
        data = [p.as_dict() for p in projects]
        for p in data:
            runner.out.info(
                f"{p['id']:<12} {p['kind']:<8} {p['path']:<26} {', '.join(p['operations'])}"
            )
        return {"projects": data}, 0
    selected = select_projects(projects, args.project)
    if args.command == "test":
        request = testing.TestRequest(
            args.suite,
            tuple(p.id for p in selected),
            args.only,
            args.group,
            args.coverage,
            args.list_only,
        )
        request.validate(bool(args.project))
        return testing.run(root, selected, runner, request, args.output)
    return toolchain.run(selected, runner, args.command, explicit=bool(args.project))


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
        parser = build_parser()
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
            error={"code": "command_failed", "message": "operation failed; inspect results"}
            if code
            else None,
        )
        return code
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
