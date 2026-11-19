"""GX-01 native creation placement and identity variants through explicit answering."""

import pytest
from test_gx_workflows import enable, select_model, answer

pytestmark = pytest.mark.__getattr__("native-dsh")


@pytest.mark.parametrize("placement", [
    pytest.param("cwd", marks=pytest.mark.description("A remote explicit directory participates in model, queue, wait and open-answer flow.")),
    pytest.param("workspace", marks=pytest.mark.description("A remote workspace and preset participate in model, queue, wait and open-answer flow.")),
    pytest.param("reuse", marks=pytest.mark.description("A reused blank identity participates in model, queue, wait and open-answer flow.")),
])
def test_gx01_native_creation_placement_open_workflow(placement, scenario, paired_device):
    """Carry an actual native session identity through all enabled GX-01 open-answer stages."""
    enable(scenario)
    args = ["session", "create", "--host", scenario.host]
    previous = None
    if placement == "cwd":
        directory = scenario.work / "gx-explicit-directory"
        args += ["--cwd", str(directory)]
    elif placement == "workspace":
        workspace = scenario.request("workspace-id")["workspaceId"]
        preset = scenario.request("default-preset-info")
        assert preset["available"]
        args += ["--workspace", workspace, "--preset", preset["presetId"]]
    else:
        previous = paired_device.call(*args)["lifecycle_receipt"]["session_id"]
        args += ["--session-id", previous]
    created = paired_device.call(*args)["lifecycle_receipt"]
    session = created["session_id"]
    assert created["disposition"] == "READY" and session
    if previous is not None:
        assert session == previous
    if placement == "cwd":
        assert directory.is_dir()
    if placement == "workspace":
        assert scenario.request("workspace-has-session", sessionId=session)["attached"]
        assert scenario.request("session-has-default-preset", sessionId=session)["matches"]
    assert any(row["session_id"] == session for row in paired_device.sessions(scenario.host))
    select_model(paired_device, scenario.host, session)
    scenario.model.plan()
    sent = paired_device.send_confirmed(session, "synthetic gx variant", delivery="enqueue-after-run")
    assert sent["status"] == "succeeded" and sent["delivery_id"]
    scenario.wait_idle(session)
    assert scenario.request("question-enable-remote-fixture")["enabled"]
    assert scenario.request("question-start-remote-fixture", sessionId=session)["started"]
    observation = paired_device.call("session", "wait", session, "--timeout", "3")
    assert observation["reason"] == "input_required" and "question" in observation["state"]["pending_input"]
    batch = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    assert batch["phase"] == "QUESTION_OPEN" and batch["answerable"]
    receipt = answer(paired_device, scenario.directory, session, batch)
    assert receipt["question_receipt"]["phase"] == "QUESTION_SETTLED"
    assert scenario.request("question-result-remote-fixture", sessionId=session) == {"selected":"Yes", "pending":False}
    assert paired_device.call("session", "wait", session, "--timeout", "3")["reason"] == "idle"
