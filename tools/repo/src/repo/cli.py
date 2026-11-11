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
        raise UsageError(message)


def common(suppress=False):
    parser = argparse.ArgumentParser(add_help=False)
    default = argparse.SUPPRESS if suppress else False
    parser.add_argument("--json", dest="json_mode", action="store_true", default=default)
    parser.add_argument("-v", "--verbose", action="store_true", default=default)
    return parser


def build_parser():
    parser = Parser(
        prog="repo", description="WhaleRein repository engineering tasks", parents=[common()]
    )
    parser.add_argument("--version", action="version", version=f"repo {__version__}")
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("projects", "doctor", "fmt", "check", "build", "test", "report", "proto"):
        sub = commands.add_parser(name, parents=[common(True)])
        if name in ("fmt", "check", "build", "test"):
            sub.add_argument("--project", action="append", metavar="ID")
        if name == "doctor":
            sub.add_argument(
                "--for",
                dest="task",
                choices=("check", "build", "test", "coverage", "integration", "proto"),
            )
        if name == "test":
            sub.add_argument("--suite", choices=("unit", "integration", "all"), default="unit")
            sub.add_argument("--group", choices=("protocol", "storage", "transport", "chain"))
            sub.add_argument("--only")
            sub.add_argument("--coverage", action="store_true")
            sub.add_argument("--list", dest="list_only", action="store_true")
            sub.add_argument("--output", type=Path)
        if name == "report":
            sub.add_argument("input", type=Path)
            sub.add_argument("--output", type=Path, required=True)
        if name == "proto":
            sub.add_argument("--check", action="store_true")
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
        args = build_parser().parse_args(arguments)
        command = args.command
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
