"""Timing facts remain optional and interrupted observations stay partial."""

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

from repo.reports.html import duration_label


@pytest.mark.parametrize("interrupted", [False, True])
def test_native_pytest_phases_and_interruption(tmp_path, interrupted):
    (tmp_path / "test_probe.py").write_text(
        "def test_probe():\n" + ("    raise KeyboardInterrupt\n" if interrupted else "    pass\n")
    )
    destination = tmp_path / "timing.json"
    source = Path(__file__).resolve().parents[1] / "src"
    environment = {
        **os.environ,
        "PYTHONPATH": str(source),
        "WHALEREIN_TEST_REPORT": str(destination),
        "WHALEREIN_TEST_ONLY": "test_probe",
    }
    result = subprocess.run(
        [sys.executable, "-m", "pytest", "-p", "repo.pytest_report", str(tmp_path)],
        cwd=tmp_path,
        env=environment,
        capture_output=True,
        text=True,
        timeout=20,
    )
    assert result.returncode == (2 if interrupted else 0), result.stderr
    case = json.loads(destination.read_text())["tests"][0]
    assert case["duration_ms"] >= 0
    assert case["duration_complete"] is not interrupted
    if interrupted:
        assert case["status"] == "interrupted"
    else:
        assert set(case["phase_durations_ms"]) == {"setup", "call", "teardown"}
        assert case["duration_ms"] == pytest.approx(
            sum(case["phase_durations_ms"].values()), abs=0.001
        )


def test_missing_timing_is_not_zero():
    assert duration_label(None) == "—"
    assert duration_label(0) == "0.0ms"
