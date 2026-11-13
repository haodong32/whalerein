"""Safety-bounded reproduction of the intermittent GX-03 native chain failure."""
import json
from pathlib import Path

import pytest
from test_gx_workflows import test_gx03_native_queue_edit_remove_promote_interrupt_and_wait as run_gx03

pytestmark = pytest.mark.__getattr__("native-dsh")


def test_gx03_native_repeated_same_fixture(scenario, paired_device):
    """Repeat the GX-03 native chain with stage and status-only diagnostics."""
    events = []
    report = Path("/reports/gx03-stress.json")
    original = paired_device.invoke

    def persist():
        report.write_text(json.dumps({"events":events[-100:], "count":len(events)}))

    def traced(*args, **kwargs):
        expected_success = kwargs.get("success", True)
        result = original(*args, success=False)
        code, envelopes = result
        last = envelopes[-1] if envelopes else {}
        error = last.get("error", {})
        events.append({"kind":"cli", "command":args[0], "exit":code, "error_code":error.get("code"), "status":last.get("data",{}).get("status")})
        persist()
        if expected_success and code != 0:
            raise AssertionError("GX-03 fixture command returned a failure status")
        return result

    paired_device.invoke = traced
    for iteration in range(12):
        events.append({"kind":"iteration_start", "iteration":iteration})
        persist()
        try:
            run_gx03(scenario, paired_device)
        except Exception as error:
            stage_file = Path("/reports/gx03-stage.json")
            stage = json.loads(stage_file.read_text())["stage"] if stage_file.exists() else "before_stage"
            events.append({"kind":"failure", "iteration":iteration, "stage":stage, "error_type":type(error).__name__})
            persist()
            raise
        events.append({"kind":"iteration_pass", "iteration":iteration})
        persist()
