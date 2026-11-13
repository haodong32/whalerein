"""Native lifecycle outcome contract for session creation and fork."""

import json

import pytest

pytestmark = pytest.mark.__getattr__("native-dsh")


def receipt_of(envelope):
    return envelope["data"]["lifecycle_receipt"]


def test_native_lifecycle_create_ready_reports_operation_contract(scenario, paired_device):
    """A successful native create returns READY with identity and a coarse phase."""
    created = paired_device.call("session-create", "--host", scenario.host)
    receipt = created["lifecycle_receipt"]
    assert receipt["operation"] == "create"
    assert receipt["disposition"] == "READY"
    assert receipt["session_id"] and receipt["phase"] == "COMPLETE"
    assert receipt["operation_id"]
    assert not receipt.get("warning_code")
    assert any(item["session_id"] == receipt["session_id"] for item in paired_device.sessions(scenario.host))
    assert scenario.model.calls == 0


def test_native_lifecycle_missing_workspace_fails_before_native_call(scenario, paired_device):
    """A missing workspace is a definitive PREFLIGHT failure with no created identity."""
    before = len(paired_device.sessions(scenario.host))
    exit_code, envelopes = paired_device.invoke(
        "session-create", "--host", scenario.host, "--workspace", "fixture-no-such-workspace", success=False
    )
    assert exit_code == 3
    receipt = receipt_of(envelopes[-1])
    assert receipt["disposition"] == "FAILED"
    assert receipt["reason_code"] == "WORKSPACE_NOT_FOUND"
    assert receipt["phase"] == "PREFLIGHT"
    assert receipt.get("retryable") is True
    assert receipt.get("session_id", "") == ""
    assert len(paired_device.sessions(scenario.host)) == before
    assert "fixture-no-such-workspace" not in json.dumps(receipt)
    assert scenario.model.calls == 0


def test_native_lifecycle_missing_preset_fails_before_native_call(scenario, paired_device):
    """A missing preset is a definitive PREFLIGHT failure with no created identity."""
    before = len(paired_device.sessions(scenario.host))
    exit_code, envelopes = paired_device.invoke(
        "session-create", "--host", scenario.host, "--preset", "fixture-nonexistent-preset", success=False
    )
    assert exit_code == 3
    receipt = receipt_of(envelopes[-1])
    assert receipt["disposition"] == "FAILED"
    assert receipt["reason_code"] == "PRESET_NOT_FOUND"
    assert receipt.get("session_id", "") == ""
    assert len(paired_device.sessions(scenario.host)) == before


def test_native_lifecycle_attach_failure_keeps_identity_with_warning(scenario, paired_device):
    """A verified attach failure after persistence is READY_WITH_WARNING, not a failure."""
    workspace = scenario.request("workspace-id")["workspaceId"]
    before = len(paired_device.sessions(scenario.host))
    assert scenario.request("test-fail-next-attachment")["armed"]
    created = paired_device.call("session-create", "--host", scenario.host, "--workspace", workspace)
    receipt = created["lifecycle_receipt"]
    assert receipt["disposition"] == "READY_WITH_WARNING"
    assert receipt["warning_code"] == "workspace_attach_failed"
    assert receipt["session_id"] and receipt["phase"] == "ATTACH"
    assert receipt.get("retryable") in (None, False)
    assert len(paired_device.sessions(scenario.host)) == before + 1
    assert any(item["session_id"] == receipt["session_id"] for item in paired_device.sessions(scenario.host))


def test_native_lifecycle_empty_fork_source_fails_before_native_call(scenario, paired_device):
    """A source with no completed turn is a definitive PREFLIGHT fork failure."""
    parent = scenario.create()
    before = {item["session_id"] for item in paired_device.sessions(scenario.host)}
    assert scenario.request("test-enable-fork")["enabled"]
    exit_code, envelopes = paired_device.invoke("session-fork", "--session", parent, success=False)
    assert exit_code == 8
    receipt = receipt_of(envelopes[-1])
    assert receipt["disposition"] == "FAILED"
    assert receipt["reason_code"] == "SOURCE_NO_COMPLETE_TURN"
    assert receipt["operation"] == "fork"
    assert receipt.get("session_id", "") == ""
    assert {item["session_id"] for item in paired_device.sessions(scenario.host)} == before


def test_native_lifecycle_fork_attach_failure_keeps_child_identity(scenario, paired_device):
    """A verified fork attach failure retains parent and child identities."""
    parent = scenario.create()
    scenario.model.plan()
    paired_device.send(parent, "synthetic lifecycle turn", delivery="start-if-idle")
    scenario.wait_idle(parent)
    assert scenario.request("test-enable-fork")["enabled"]
    assert scenario.request("arm-fork-attach-fault", sessionId=parent)["armed"]
    before = {item["session_id"] for item in paired_device.sessions(scenario.host)}
    forked = paired_device.call("session-fork", "--session", parent)
    receipt = forked["lifecycle_receipt"]
    assert receipt["disposition"] == "READY_WITH_WARNING"
    assert receipt["warning_code"] == "workspace_attach_failed"
    assert receipt["parent_session_id"] == parent
    assert receipt["session_id"] and receipt["session_id"] != parent
    assert receipt["phase"] == "ATTACH"
    after = {item["session_id"] for item in paired_device.sessions(scenario.host)}
    assert after - before == {receipt["session_id"]}
