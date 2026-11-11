"""Own the integration container; consume only a versioned scope export."""

import json
from uuid import uuid4

from ..diagnostics import sanitize
from ..errors import RepoError
from ..reports.model import validate_scope


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
    container = "whalerein-test-" + uuid4().hex[:12]
    image_file = directory / "image.id"
    image = None
    export = directory / "container"
    export.mkdir()
    launched = False
    try:
        probe = runner.run(["docker", "buildx", "version"], root, timeout=15, quiet=True)
        if not probe.ok:
            scope["errors"].append("Docker buildx is required")
            return scope
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
            return scope
        image = image_file.read_text().strip()
        if not image.startswith("sha256:") or len(image) != 71:
            raise ValueError("Invalid built image ID")
        arguments = [
            "docker",
            "run",
            "--init",
            "--sig-proxy=false",
            "--network",
            "none",
            "--name",
            container,
            "--env",
            "WHALEREIN_PROGRESS=events",
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
        launched = True
        result = runner.run(arguments, root, quiet=True, timeout=1800)
        scope["exit_code"] = result.returncode
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
            if not stopped.ok and scope["exit_code"] == 130:
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
    except (RepoError, OSError, ValueError, KeyError, TypeError) as exc:
        scope["errors"].append("Missing or invalid integration export: " + sanitize(exc))
    if scope["exit_code"] not in (0, 5):
        scope["errors"].append("Integration command exited " + str(scope["exit_code"]))
    return scope
