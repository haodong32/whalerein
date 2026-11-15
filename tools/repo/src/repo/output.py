"""Human and JSON output helpers.

JSON mode writes exactly one envelope to stdout; diagnostics go to stderr so
scripts can parse stdout safely.
"""

from __future__ import annotations

import json
import sys
import traceback
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, TextIO

from .diagnostics import redact as redact
from .diagnostics import sanitize, sanitize_data
from .errors import RepoError

SCHEMA = "whalerein/repo/v1"
ERROR_CODES = frozenset(
    (
        "error",
        "internal_error",
        "repo_error",
        "usage_error",
        "tool_missing",
        "project_not_found",
        "command_failed",
        "coverage_below_threshold",
        "commit_rejected",
    )
)


@dataclass
class Output:
    json_mode: bool = False
    verbose: bool = False
    quiet: bool = False
    stdout: TextIO = field(default_factory=lambda: sys.stdout)
    stderr: TextIO = field(default_factory=lambda: sys.stderr)

    def info(self, message: str) -> None:
        if self.quiet:
            return
        print(message, file=self.stderr if self.json_mode else self.stdout)

    def detail(self, message: str) -> None:
        if self.verbose:
            self.info(sanitize(message))

    def warn(self, message: str) -> None:
        print(f"warning: {sanitize(message)}", file=self.stderr)

    def error(self, message: str) -> None:
        print(f"error: {sanitize(message)}", file=self.stderr)

    def failure(self, command: str, exc: Exception) -> None:
        """One boundary for intentional errors and unexpected failures in every mode."""
        message = exc.message if isinstance(exc, RepoError) else "internal error"
        code = exc.code if isinstance(exc, RepoError) else "internal_error"
        self.error(message)
        if self.verbose:
            self.diagnostic(exc)
        self.result(command, ok=False, error={"code": code, "message": message})

    def diagnostic(self, exc: Exception) -> None:
        """Show exception locations and chaining without source, locals, notes or payloads."""
        lines = ["Traceback (diagnostic locations; exception payloads omitted):"]
        seen = set()
        current = exc
        while current is not None and id(current) not in seen:
            seen.add(id(current))
            for frame, number in traceback.walk_tb(current.__traceback__):
                filename = Path(frame.f_code.co_filename).name
                lines.append(f'  File "{filename}", line {number}, in {frame.f_code.co_name}')
            label = type(current).__name__
            if isinstance(current, OSError) and type(current.errno) is int:
                label += f" (errno={current.errno})"
            lines.append(label)
            current = current.__cause__ or (
                current.__context__ if not current.__suppress_context__ else None
            )
            if current is not None:
                lines.append("Caused by:")
        print(sanitize("\n".join(lines)), file=self.stderr)

    def result(
        self,
        command: str,
        ok: bool,
        data: Any = None,
        error: dict[str, str] | None = None,
    ) -> None:
        if not self.json_mode:
            return
        envelope: dict[str, Any] = {"schema": SCHEMA, "command": command, "ok": ok}
        if ok:
            envelope["data"] = data if data is not None else {}
        else:
            if data is not None:
                envelope["data"] = sanitize_data(data)
            supplied = error or {"code": "error", "message": "unknown error"}
            code = supplied.get("code")
            envelope["command"] = sanitize(command)
            envelope["error"] = {
                "code": code if isinstance(code, str) and code in ERROR_CODES else "error",
                "message": sanitize(supplied.get("message", "unknown error")),
            }
        json.dump(envelope, self.stdout, indent=2, sort_keys=True)
        self.stdout.write("\n")
        self.stdout.flush()
