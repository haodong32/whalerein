"""Own the integration container(s); consume only versioned scope exports."""

import json
import os
import tempfile
import threading
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field
from time import monotonic
from uuid import uuid4

from ..diagnostics import sanitize
from ..errors import RepoError
from ..reports.model import validate_scope

DEFAULT_EXECUTION_TIMEOUT_SECONDS = 7200
CONTAINER_TIMEOUT_GRACE_SECONDS = 40


def collect(root, runner, request, directory):
    scope = {
        "suite_id": "integration",
        "project": "integration",
        "tests": [],
        "coverage": [],
        "completeness": "incomplete",
        "errors": [],
        "exit_code": 1,
    }
    parallel = getattr(request, "parallel", None) or 1
    if parallel > 1:
        return _collect_parallel(root, runner, request, directory, scope, parallel)
    container = "whalerein-test-" + uuid4().hex[:12]
    export = directory / "container"
    export.mkdir()
    launched = False
    try:
        image = _build_image(root, runner, directory, scope)
        if image is None:
            return scope
        execution_timeout = request.integration_timeout or DEFAULT_EXECUTION_TIMEOUT_SECONDS
        arguments = _arguments(
            container,
            image,
            export,
            execution_timeout,
            request,
        )
        launched = True
        result = runner.run(
            arguments,
            root,
            quiet=True,
            timeout=execution_timeout + CONTAINER_TIMEOUT_GRACE_SECONDS,
        )
        scope["exit_code"] = result.returncode
        if result.returncode == 124:
            scope["errors"].append(
                f"Integration execution timed out after {execution_timeout} seconds"
            )
    except KeyboardInterrupt:
        scope["exit_code"] = 130
    except (OSError, ValueError) as exc:
        scope["errors"].append(sanitize(exc))
    finally:
        if launched:
            # The name belongs exclusively to this invocation; never stop shared services.
            stopped = runner.run(
                ["docker", "stop", "--signal", "SIGINT", "--timeout", "35", container],
                root,
                timeout=45,
                quiet=True,
            )
            if not stopped.ok and scope["exit_code"] in (124, 130):
                scope["errors"].append("Could not gracefully stop integration container")
            removed = runner.run(
                ["docker", "rm", "--force", container], root, timeout=30, quiet=True
            )
            verified = runner.run(
                ["docker", "ps", "-aq", "--filter", f"name=^/{container}$"],
                root,
                timeout=15,
                quiet=True,
            )
            if not removed.ok or not verified.ok or verified.stdout.strip():
                scope["errors"].append("Could not verify owned container cleanup")
    try:
        payload = json.loads((export / "integration.json").read_text())
        if payload.get("version") != 1:
            raise ValueError("unsupported integration export")
        native = payload["scope"]
        validate_scope(native)
        errors, code = scope["errors"], scope["exit_code"]
        scope = native
        scope["errors"] = [*native.get("errors", []), *errors]
        scope["exit_code"] = code
        if code == 124:
            scope["completeness"] = "incomplete"
    except (RepoError, OSError, ValueError, KeyError, TypeError) as exc:
        scope["errors"].append("Missing or invalid integration export: " + sanitize(exc))
    if scope["exit_code"] not in (0, 5):
        scope["errors"].append("Integration command exited " + str(scope["exit_code"]))
    return scope


def _build_image(root, runner, directory, scope):
    """Build the immutable image once, recording failures on the scope."""
    image_file = directory / "image.id"
    try:
        probe = runner.run(["docker", "buildx", "version"], root, timeout=15, quiet=True)
        if not probe.ok:
            scope["errors"].append("Docker buildx is required")
            return None
        # Release reserved physical space before serializing failure evidence.
        # A full Docker disk must not hide the build error behind report I/O.
        with tempfile.TemporaryFile(dir=directory) as reserve:
            for _ in range(32):
                reserve.write(b"\0" * (1024 * 1024))
            reserve.flush()
            built = runner.run(
                [
                    "docker",
                    "buildx",
                    "build",
                    "--load",
                    "--progress=plain",
                    "-f",
                    str(root / "tests/integration/Dockerfile"),
                    "--iidfile",
                    str(image_file),
                    str(root),
                ],
                root,
                timeout=1800,
                quiet=True,
            )
        if not built.ok:
            scope["errors"].append("Integration image build failed: " + sanitize(built.tail()))
            scope["exit_code"] = built.returncode
            return None
        image = image_file.read_text().strip()
        if not image.startswith("sha256:") or len(image) != 71:
            scope["errors"].append("Invalid built image ID")
            return None
        return image
    except (OSError, ValueError) as exc:
        scope["errors"].append(sanitize(exc))
        return None


def _arguments(container, image, export, execution_timeout, request, shard=None):
    arguments = [
        "docker",
        "run",
        "--init",
        "--sig-proxy=false",
        "--network",
        "none",
        "--tmpfs",
        "/tmp:rw,exec,size=1024m,mode=1777",
        "--name",
        container,
        "--env",
        "WHALEREIN_PROGRESS=events",
        "--env",
        f"WHALEREIN_INTEGRATION_TIMEOUT={execution_timeout}",
        *(
            [
                "--env",
                f"WHALEREIN_INTEGRATION_SHARED_BROKER={os.environ['WHALEREIN_INTEGRATION_SHARED_BROKER']}",
            ]
            if os.environ.get("WHALEREIN_INTEGRATION_SHARED_BROKER") is not None
            else []
        ),
        "--mount",
        f"type=bind,src={export},dst=/reports",
        image,
    ]
    if request.list_only:
        arguments += ["--collect-only", "-q"]
    if request.only:
        arguments += ["--only", request.only]
    if request.group:
        arguments += ["--group", request.group]
    if shard is not None:
        arguments += ["--shard", shard]
    return arguments


@dataclass
class _Shard:
    index: int
    export: object
    name: str = ""
    code: object = None
    launched: bool = False
    verified: bool = False
    cleanup_error: bool = False
    errors: list = field(default_factory=list)


def _collect_parallel(root, runner, request, directory, scope, parallel):
    export_root = directory / "container"
    export_root.mkdir(exist_ok=True)
    exports = []
    for index in range(parallel):
        export = export_root / str(index)
        export.mkdir()
        exports.append(export)
    image = _build_image(root, runner, directory, scope)
    if image is None:
        return scope
    execution_timeout = request.integration_timeout or DEFAULT_EXECUTION_TIMEOUT_SECONDS
    states = [
        _Shard(index=index, export=exports[index], name="whalerein-test-" + uuid4().hex[:12])
        for index in range(parallel)
    ]
    started = monotonic()
    lock = threading.Lock()
    launched = set()
    stop = threading.Event()
    interrupted = False
    display = getattr(runner, "progress", None)
    # Per-shard status lines replace the shared live display, which is not
    # thread-safe. The containers' own progress events are suppressed.
    runner.progress = None
    try:
        with ThreadPoolExecutor(max_workers=parallel) as executor:
            futures = {}
            for state in states:
                runner.out.info(f"Shard {state.index}/{parallel}: starting ({state.name})")
                futures[
                    executor.submit(
                        _run_shard,
                        root,
                        runner,
                        image,
                        request,
                        state,
                        execution_timeout,
                        parallel,
                        lock,
                        launched,
                        stop,
                    )
                ] = state
            try:
                for future in as_completed(futures):
                    try:
                        future.result()
                    except KeyboardInterrupt:
                        raise
                    except Exception as exc:  # pragma: no cover - defensive boundary
                        futures[future].errors.append("Shard failed: " + sanitize(exc))
            except KeyboardInterrupt:
                interrupted = True
                runner.interrupted = True
                _stop_launched(runner, root, lock, launched, stop)
                executor.shutdown(wait=True, cancel_futures=True)
    finally:
        runner.progress = display
    for state in states:
        _verify_cleanup(root, runner, state, parallel)
        if state.code is None:
            state.code = 130 if interrupted else 1
            state.errors.append(f"Shard {state.index}/{parallel}: did not execute")
        if state.code not in (0, 5):
            state.errors.append(
                f"Shard {state.index}/{parallel}: integration command exited {state.code}"
            )
    merged = _merge(states, parallel, interrupted, started)
    for state in states:
        label = (
            "passed" if state.code in (0, 5) else "interrupted" if state.code == 130 else "failed"
        )
        runner.out.info(f"Shard {state.index}/{parallel}: {label} (exit {state.code})")
    return merged


def _run_shard(
    root,
    runner,
    image,
    request,
    state,
    execution_timeout,
    parallel,
    lock,
    launched,
    stop,
):
    try:
        with lock:
            if stop.is_set():
                state.code = 130
                return
            launched.add(state.name)
        state.launched = True
        arguments = _arguments(
            state.name,
            image,
            state.export,
            execution_timeout,
            request,
            shard=f"{state.index}/{parallel}",
        )
        result = runner.run(
            arguments,
            root,
            quiet=True,
            timeout=execution_timeout + CONTAINER_TIMEOUT_GRACE_SECONDS,
        )
        state.code = result.returncode
        if result.returncode == 124:
            state.errors.append(
                f"Shard {state.index}/{parallel}: "
                f"integration execution timed out after {execution_timeout} seconds"
            )
    except KeyboardInterrupt:
        state.code = 130
        raise
    except (OSError, ValueError) as exc:
        state.errors.append(f"Shard {state.index}/{parallel}: " + sanitize(exc))
    finally:
        if state.launched:
            _cleanup(runner, root, state, parallel)


def _cleanup(runner, root, state, parallel):
    stopped = runner.run(
        ["docker", "stop", "--signal", "SIGINT", "--timeout", "35", state.name],
        root,
        timeout=45,
        quiet=True,
    )
    if not stopped.ok and state.code in (124, 130):
        state.errors.append(
            f"Shard {state.index}/{parallel}: could not gracefully stop integration container"
        )
    runner.run(["docker", "rm", "--force", state.name], root, timeout=30, quiet=True)
    _record_verified(runner, root, state, parallel)


def _verify_cleanup(root, runner, state, parallel):
    """Re-verify every launched container after all workers have stopped."""
    if not state.launched or state.verified:
        return
    runner.run(["docker", "rm", "--force", state.name], root, timeout=30, quiet=True)
    _record_verified(runner, root, state, parallel)


def _record_verified(runner, root, state, parallel):
    verified = runner.run(
        ["docker", "ps", "-aq", "--filter", f"name=^/{state.name}$"],
        root,
        timeout=15,
        quiet=True,
    )
    clean = verified.ok and not verified.stdout.strip()
    state.verified = bool(clean)
    if not clean and not state.cleanup_error:
        state.cleanup_error = True
        state.errors.append(
            f"Shard {state.index}/{parallel}: could not verify owned container cleanup"
        )


def _stop_launched(runner, root, lock, launched, stop):
    # Mark the stop before releasing the lock so no shard can launch afterwards.
    with lock:
        stop.set()
        names = list(launched)
    for name in names:
        # Workers still own removal; stopping first lets them finish promptly.
        runner.run(
            ["docker", "stop", "--signal", "SIGINT", "--timeout", "35", name],
            root,
            timeout=45,
            quiet=True,
        )


def _merge(states, parallel, interrupted, started):
    tests, checks, coverage, errors = [], [], [], []
    seen = set()
    complete = True
    partial = False
    duplicate = False
    for state in states:
        native = _read_shard(state, parallel)
        errors.extend(state.errors)
        if native is None:
            complete = False
            continue
        if native.get("completeness") != "complete":
            complete = False
        if native.get("selection") == "partial":
            partial = True
        checks.extend(native.get("checks", []))
        coverage.extend(native.get("coverage", []))
        for case in native["tests"]:
            identity = case["id"]
            if identity in seen:
                duplicate = True
                continue
            seen.add(identity)
            tests.append(case)
    if duplicate:
        errors.append("Duplicate integration case identity across shards")
        complete = False
    if interrupted:
        code = 130
    elif any(state.code == 130 for state in states):
        code = 130
    elif any(state.code == 124 for state in states):
        code = 124
    elif any(state.code not in (0, 5) for state in states) or errors or not complete:
        code = 1
    else:
        code = 0
    if code not in (0, 5):
        complete = False
    merged = {
        "suite_id": "integration",
        "project": "integration",
        "tests": tests,
        "coverage": coverage,
        "checks": checks,
        "selection": "partial" if partial else "full",
        "completeness": "complete" if complete and not interrupted else "incomplete",
        "errors": errors,
        "exit_code": code,
        "duration_ms": round((monotonic() - started) * 1000, 3),
    }
    validate_scope(merged)
    return merged


def _read_shard(state, parallel):
    try:
        payload = json.loads((state.export / "integration.json").read_text())
        if payload.get("version") != 1:
            raise ValueError("unsupported integration export")
        native = payload["scope"]
        validate_scope(native)
        shard = native.get("shard")
        if shard is not None and shard != f"{state.index}/{parallel}":
            raise ValueError("unexpected shard identity " + str(shard))
        return native
    except (RepoError, OSError, ValueError, KeyError, TypeError) as exc:
        state.errors.append(
            f"Shard {state.index}: Missing or invalid integration export: " + sanitize(exc)
        )
        return None
