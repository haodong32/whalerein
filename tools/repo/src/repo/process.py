"""Subprocess execution without a shell.

All commands are argument arrays. Environment overrides are merged for the
child only and are never logged.
"""

from __future__ import annotations

import codecs
import os
import selectors
import shlex
import shutil
import signal
import subprocess
import time
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path

from .errors import ToolMissingError
from .output import Output


@dataclass(frozen=True)
class ProcessResult:
    argv: tuple[str, ...]
    cwd: str
    returncode: int
    stdout: str = ""
    stderr: str = ""
    duration_s: float = 0.0
    missing: bool = False

    @property
    def ok(self) -> bool:
        return self.returncode == 0 and not self.missing

    @property
    def output(self) -> str:
        return "\n".join(part for part in (self.stdout, self.stderr) if part).strip()

    def tail(self, limit: int = 4000) -> str:
        text = self.output
        return text[-limit:] if len(text) > limit else text


@dataclass
class Runner:
    out: Output
    environ: Mapping[str, str] = field(default_factory=lambda: os.environ)
    progress: object = None
    interrupted: bool = False

    def which(self, tool: str) -> str | None:
        return shutil.which(tool, path=self.environ.get("PATH", os.defpath))

    def require(self, tool: str, hint: str = "") -> str:
        path = self.which(tool)
        if path is None:
            message = f"required tool not found on PATH: {tool}"
            if hint:
                message += f" ({hint})"
            raise ToolMissingError(message)
        return path

    def run(
        self,
        argv: Sequence[str],
        cwd: Path | str,
        *,
        env: Mapping[str, str] | None = None,
        timeout: float | None = None,
        check: bool = False,
        quiet: bool = False,
    ) -> ProcessResult:
        args = tuple(str(part) for part in argv)
        cwd_str = str(cwd)
        display = shlex.join(args)
        if not quiet:
            self.out.detail(f"$ {display}  (cwd={cwd_str})")

        child_env = dict(self.environ)
        if env is not None:
            child_env = dict(self.environ)
            child_env.update({key: str(value) for key, value in env.items()})

        if getattr(self, "progress", None):
            child_env = {**child_env, "WHALEREIN_PROGRESS": "events"}
        started = time.monotonic()
        try:
            # Windows selectors cannot watch anonymous subprocess pipes.
            execute = self._capture if os.name == "nt" else self._stream
            completed = execute(
                list(args),
                cwd=cwd_str,
                env=child_env,
                capture_output=True,
                stdin=subprocess.DEVNULL,
                text=True,
                timeout=timeout,
                check=False,
            )
            result = ProcessResult(
                argv=args,
                cwd=cwd_str,
                returncode=completed.returncode,
                stdout=completed.stdout or "",
                stderr=completed.stderr or "",
                duration_s=round(time.monotonic() - started, 3),
            )
        except KeyboardInterrupt as exc:
            self.interrupted = True
            result = ProcessResult(
                argv=args,
                cwd=cwd_str,
                returncode=130,
                stdout=_as_text(getattr(exc, "stdout", None)),
                stderr=_as_text(getattr(exc, "stderr", None)),
                duration_s=round(time.monotonic() - started, 3),
            )
        except FileNotFoundError:
            result = ProcessResult(
                argv=args,
                cwd=cwd_str,
                returncode=127,
                stderr=f"executable not found: {args[0]}",
                duration_s=round(time.monotonic() - started, 3),
                missing=True,
            )
        except subprocess.TimeoutExpired as exc:
            stdout = _as_text(exc.stdout)
            stderr = _as_text(exc.stderr)
            result = ProcessResult(
                argv=args,
                cwd=cwd_str,
                returncode=124,
                stdout=stdout,
                stderr=(stderr + f"\ncommand timed out after {timeout}s").strip(),
                duration_s=round(time.monotonic() - started, 3),
            )

        if not quiet and (result.stdout or result.stderr):
            self.out.detail(result.output)
        if check and not result.ok:
            raise ToolMissingError(result.stderr) if result.missing else _failed(result)
        return result

    def _capture(self, argv, *, cwd, env, timeout, **kwargs):
        """Portable buffered capture; Windows uses taskkill to stop the child tree."""
        with subprocess.Popen(
            argv,
            cwd=cwd,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            errors="replace",
        ) as process:
            try:
                stdout, stderr = process.communicate(timeout=timeout)
            except (subprocess.TimeoutExpired, KeyboardInterrupt) as failure:
                if os.name == "nt":
                    subprocess.run(
                        ["taskkill", "/PID", str(process.pid), "/T", "/F"],
                        capture_output=True,
                        check=False,
                        timeout=15,
                    )
                else:
                    process.terminate()
                stdout, stderr = process.communicate(timeout=45)
                failure.stdout, failure.stderr = stdout, stderr
                raise
        return subprocess.CompletedProcess(argv, process.returncode, stdout, stderr)

    def _stream(self, argv, *, cwd, env, timeout, **kwargs):
        from .progress import parse

        output = {"stdout": [], "stderr": []}
        decoders = {name: codecs.getincrementaldecoder("utf-8")("replace") for name in output}
        pending = {name: "" for name in output}
        announced = set()
        started = time.monotonic()
        with subprocess.Popen(
            argv,
            cwd=cwd,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
        ) as process:
            selector = selectors.DefaultSelector()
            for name in output:
                selector.register(getattr(process, name), selectors.EVENT_READ, name)
            try:
                while selector.get_map():
                    now = time.monotonic()
                    if timeout is not None and now - started > timeout:
                        raise subprocess.TimeoutExpired(argv, timeout)
                    for key, _ in selector.select(0.2):
                        chunk = os.read(key.fd, 65536)
                        text = decoders[key.data].decode(chunk, final=not chunk)
                        output[key.data].append(text)
                        pending[key.data] += text
                        if not chunk:
                            selector.unregister(key.fileobj)
                            pending[key.data] += "\n"
                        while "\n" in pending[key.data]:
                            line, pending[key.data] = pending[key.data].split("\n", 1)
                            event = parse(line)
                            if event and self.progress:
                                self.progress.event(event)
                        if key.data == "stdout" and "--format=pretty" in argv:
                            from .progress import rust_start

                            event = rust_start(pending["stdout"])
                            if event and self.progress and event["id"] not in announced:
                                announced.add(event["id"])
                                self.progress.event(event)
                remaining_timeout = (
                    None if timeout is None else max(0, timeout - (time.monotonic() - started))
                )
                process.wait(timeout=remaining_timeout)
            except BaseException as failure:
                try:
                    os.killpg(
                        process.pid,
                        signal.SIGINT if isinstance(failure, KeyboardInterrupt) else signal.SIGTERM,
                    )
                except ProcessLookupError:
                    pass
                try:
                    # Let launchers stop owned containers and write final reports.
                    remaining = process.communicate(timeout=45)
                    for name, value in zip(output, remaining, strict=True):
                        output[name].append(value or "")
                except (subprocess.TimeoutExpired, KeyboardInterrupt):
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    process.wait()
                if isinstance(failure, (subprocess.TimeoutExpired, KeyboardInterrupt)):
                    failure.stdout = "".join(output["stdout"])
                    failure.stderr = "".join(output["stderr"])
                raise
            finally:
                selector.close()
        return subprocess.CompletedProcess(
            argv, process.returncode, "".join(output["stdout"]), "".join(output["stderr"])
        )


def _failed(result: ProcessResult) -> Exception:
    from .errors import CommandFailedError

    return CommandFailedError(
        f"command failed with exit code {result.returncode}: {shlex.join(result.argv)}"
    )


def _as_text(value: bytes | str | None) -> str:
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode("utf-8", "replace")
    return value
