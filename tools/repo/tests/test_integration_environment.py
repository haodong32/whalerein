import importlib.util
import sys
from pathlib import Path

import pytest


@pytest.fixture
def environment():
    path = Path(__file__).resolve().parents[3] / "tests/integration/environment.py"
    spec = importlib.util.spec_from_file_location("integration_environment", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_process_cleanup_terminates_owned_child(environment, tmp_path):
    child = environment.Process([sys.executable, "-c", "import time; time.sleep(60)"], tmp_path)
    child.close()
    assert child.child.poll() is not None


def test_dead_process_fails_readiness_immediately(environment, tmp_path):
    child = environment.Process([sys.executable, "-c", "raise SystemExit(1)"], tmp_path)
    try:
        child.child.wait(timeout=5)
        with pytest.raises(AssertionError, match="exited before readiness"):
            environment.wait(lambda: False, process=child)
    finally:
        child.close()


def test_unobserved_condition_times_out(environment):
    with pytest.raises(TimeoutError):
        environment.wait(lambda: False, timeout=0)


def test_readiness_retries_a_slow_probe_before_the_overall_deadline(environment):
    import subprocess

    attempts = 0

    def probe():
        nonlocal attempts
        attempts += 1
        if attempts == 1:
            raise subprocess.TimeoutExpired("fixture-probe", timeout=0.01)
        return True

    environment.wait(probe, timeout=1)
    assert attempts == 2
