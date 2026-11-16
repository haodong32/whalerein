"""Destructive CI storage preparation must reject local and shared machines."""

import os
import shlex
import shutil
import subprocess
from pathlib import Path

import pytest


@pytest.mark.parametrize(
    "environment",
    [
        {},
        {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "self-hosted", "RUNNER_OS": "Linux"},
        {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted", "RUNNER_OS": "macOS"},
    ],
)
def test_storage_cleanup_refuses_unowned_environments_before_any_commands(tmp_path, environment):
    bash = shutil.which("bash")
    if not bash:
        pytest.skip("requires Bash to verify the CI-only storage guard")
    marker = tmp_path / "invoked"
    for name in ("docker", "sudo", "df"):
        tool = tmp_path / name
        tool.write_text("#!/bin/sh\nprintf invoked >> " + shlex.quote(str(marker)) + "\nexit 1\n")
        tool.chmod(0o755)
    script = Path(__file__).resolve().parents[1] / "prepare-integration-storage.sh"
    inherited = {
        k: v
        for k, v in os.environ.items()
        if k not in ("GITHUB_ACTIONS", "RUNNER_ENVIRONMENT", "RUNNER_OS")
    }
    result = subprocess.run(
        [bash, str(script)],
        env={**inherited, **environment, "PATH": str(tmp_path)},
        capture_output=True,
        text=True,
        check=False,
        timeout=10,
    )
    assert result.returncode == 1
    assert "requires a GitHub-hosted Linux runner" in result.stderr
    assert not marker.exists()
